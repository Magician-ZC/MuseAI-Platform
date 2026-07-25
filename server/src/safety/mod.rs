//! 安全与风控（S3）：角色卡/文本注入检测 + 分层机审管道 + 运行时敏感词库 + risk_events
//! （平台规格 §14；运行时内容安全五层漏斗见总规格 §15）。
//!
//! - `detect_injection(text)` / `card_scan_text(json)`：注入检测引擎，见 `inject` 子模块。
//!   归一化（去零宽/全角折叠/同形字映射/折叠空白）→ 语义拼接（卡片字段值，非序列化 JSON）→
//!   紧凑串匹配 + 句式判别（第二人称祈使 vs 第三人称叙述）。返回命中规则名 + 命中片段。
//!   短语黑名单仅作辅助信号，生产应叠加模型/分类器主闸（见 inject 模块头注释）。
//! - `lexicon::mask(text)`：§15 **第 2 层**运行时敏感词库——硬匹配 + 就地打码为 `*`，
//!   复用 `inject` 的同一条归一化管线（零宽/同形字/全角/标点绕过一并挡住）。纯本地、无 IO。
//! - `record_risk`：统一风控事件落库（其他模块复用；签名稳定）；`record_risk_tx` 为其事务内变体。
//!
//! ## 入队/记险的两条入口（**契约：只有这两条**）
//! 1. `moderate_and_queue`：**静态内容**（角色卡 / 世界模板 / 装配期钩子 / 入站托梦）的分层机审——
//!    provider 机审 + 注入检测，Approved 直过 / Pending（含注入命中）进 `audit_queue` / Rejected 直拒；
//!    命中或非过写 `risk_events`。调用方（assets/interventions/assembly）只取返回裁决，
//!    **不得**再自行 INSERT audit_queue / risk_events。
//! 2. `moderate_runtime_projection`：**运行时模型产出**（world_events 投影）的第 2 层闸——
//!    在 tick 事务内打码 + 置 `moderation='pending'` + `record_risk_tx` 留痕；
//!    **默认不写 `audit_queue`**，只有高危分类才入队（策略见 `MUSE_SAFETY_RUNTIME_AUDIT`）。
//!    理由：运行时每 tick 每事件都可能命中，若比照 ① 逐条入队，人审队列会被淹到不可用——
//!    「记险不入队」保证留痕完整、队列可用，人审仍可从 risk_events 反查。
//!
//! 换言之「唯一写入方」的旧契约现在读作：**静态内容走 ①，运行时产出走 ②，此外任何模块都不得
//! 直接 INSERT audit_queue / risk_events**。

use crate::app::AppState;
use crate::error::ApiError;
use crate::events::ProjectedEvent;
use crate::providers::ModerationVerdict;

mod inject;
mod lexicon;
pub use inject::{card_scan_text, detect_injection, InjectionHit};
pub use lexicon::{mask, Severity};

#[cfg(test)]
pub(crate) mod testkit;

/// 机审 + 注入检测的分层管道（§14）。**静态内容**（角色卡 / 世界模板 / 装配期钩子 / 入站托梦）
/// 唯一的入队/记险方；运行时模型产出走平行入口 `moderate_runtime_projection`（见模块头注释）。
///
/// 组合判定：provider Rejected → Rejected（直拒）；provider Pending 或有注入命中 → Pending（进人审队列）；
/// 否则 Approved。副作用（单一写入点，调用方不得重复）：
/// - Pending → INSERT audit_queue（status='open'，带 machine_verdict/machine_hits）；
/// - 非 Approved 或有注入命中 → record_risk（有命中记 'injection'，否则 'moderation'）。
///
/// 返回的裁决已折叠注入命中（Approved+命中 → Pending），调用方据此写自己的领域态即可。
pub async fn moderate_and_queue(
    state: &AppState,
    subject_kind: &str,
    subject_id: &str,
    text: &str,
) -> Result<ModerationVerdict, ApiError> {
    let hits = detect_injection(text);
    let provider = state
        .moderation
        .check_text(text)
        .await
        .map_err(|e| ApiError::internal(std::io::Error::other(e)))?;

    let verdict = match provider {
        ModerationVerdict::Rejected => ModerationVerdict::Rejected,
        ModerationVerdict::Pending => ModerationVerdict::Pending,
        ModerationVerdict::Approved if !hits.is_empty() => ModerationVerdict::Pending,
        ModerationVerdict::Approved => ModerationVerdict::Approved,
    };

    if verdict == ModerationVerdict::Pending {
        sqlx::query(
            "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, machine_hits, status, created_at) \
             VALUES (?, ?, ?, ?, ?, 'open', ?)",
        )
        .bind(crate::db::new_id("aq"))
        .bind(subject_kind)
        .bind(subject_id)
        .bind(verdict_str(verdict))
        .bind(serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()))
        .bind(crate::db::now_ms())
        .execute(&state.db)
        .await?;
    }

    if verdict != ModerationVerdict::Approved || !hits.is_empty() {
        let kind = if hits.is_empty() { "moderation" } else { "injection" };
        record_risk(
            &state.db,
            None,
            None,
            kind,
            serde_json::json!({
                "subjectKind": subject_kind,
                "subjectId": subject_id,
                "verdict": verdict_str(verdict),
                "providerVerdict": verdict_str(provider),
                "hits": hits,
            }),
        )
        .await?;
    }

    Ok(verdict)
}

// ==================== 运行时产出闸（§15 第 2 层 + 第 3 层接口位） ====================

/// 运行时人审入队策略的 env 名（§0.2 参数化：阈值不写死在逻辑里）。
const ENV_RUNTIME_AUDIT: &str = "MUSE_SAFETY_RUNTIME_AUDIT";
/// 默认策略：仅**高危**命中进人审队列。低危（辱骂/引流等高频命中）只记险，防止淹没队列。
const DEFAULT_RUNTIME_AUDIT: &str = "high";
/// 运行时命中后写入 `world_events.moderation` 的裁决值（非 'approved' → 全部读取面过滤）。
const RUNTIME_BLOCK_VERDICT: ModerationVerdict = ModerationVerdict::Pending;

/// 该严重度是否进人审队列。策略取值：`high`（默认，仅高危）/ `all`（全部）/ `none`（都不进）。
fn runtime_audit_admits(sev: Severity) -> bool {
    let policy = std::env::var(ENV_RUNTIME_AUDIT).unwrap_or_else(|_| DEFAULT_RUNTIME_AUDIT.into());
    match policy.trim().to_ascii_lowercase().as_str() {
        "all" => true,
        "none" => false,
        // 含配错值在内一律回落默认（high）：配错不得静默放宽或收紧审核。
        _ => sev == Severity::High,
    }
}

/// §15 **第 2 层**：运行时投影事件的敏感词库闸。**在 tick 事务内同步调用**（纯本地词表，≈0 成本，
/// 与状态 CAS 同成同败：状态回滚则打码与留痕一并回滚，不产生「事实已落但风控没记」的错位）。
///
/// 就地改写 `projected`：
/// - 命中 → `summary`/`arbiter_note` 替换为打码文本，`moderation` 置 `'pending'`；
///   **落库前改写，不是落库后回写**（§0.3 公共事实不可回滚：落库的即是最终事实）。
/// - 未命中 → 原样保持 `'approved'`，正常下发。
///
/// 副作用（本函数是运行时路径上唯一的写入方）：
/// - 每条命中 → `record_risk_tx(kind='lexicon')`，detail 含 worldId / domainEventId / 命中清单；
/// - 严重度过 `runtime_audit_admits` → `audit_queue`（默认仅高危）。
///
/// 返回被拦下的事件条数（可观测/测试用）。
///
/// ## 第 3 层（语义分类）的接口位——本轮**不实装**
/// `state.moderation.check_text()`（`ModerationProvider`）就是第 3 层的现成接口，公开投影全量、
/// 私有抽样。但它是**网络调用**，绝不能挪进这个事务：单连接池（测试/SQLite dev）下 tx 持有唯一连接，
/// 调用期间任何再借连接的操作都会死锁 PoolTimedOut（同 `record_risk_tx` 的注释）；且 tick 事务的
/// 持有时长会被外部 RTT 绑架。落地形态应为「事务外异步复核 + 命中后置 moderation」：
/// TODO(§15-L3)：tick 提交后另起任务对本 tick 的 public 投影跑 `check_text`，Pending/Rejected 时
/// UPDATE `world_events.moderation`（此为「未过审 → 不外发」的收紧方向，不改写已下发事实的内容），
/// 配合 §15 第 4 层「直播场延迟 1-2 拍缓冲」给这条异步链留出拦截窗口。
pub async fn moderate_runtime_projection(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    world_id: &str,
    projected: &mut [ProjectedEvent],
) -> Result<usize, ApiError> {
    if !lexicon::enabled() {
        return Ok(0);
    }
    let mut blocked = 0usize;
    for pe in projected.iter_mut() {
        let (masked_summary, mut hits) = mask(&pe.summary);
        let masked_note = match pe.arbiter_note.as_deref() {
            Some(note) => {
                let (m, mut h) = mask(note);
                hits.append(&mut h);
                Some(m)
            }
            None => None,
        };
        if hits.is_empty() {
            continue;
        }
        pe.summary = masked_summary;
        pe.arbiter_note = masked_note;
        pe.moderation = verdict_str(RUNTIME_BLOCK_VERDICT).to_string();
        blocked += 1;

        let severity = lexicon::max_severity(&hits).unwrap_or(Severity::Low);
        record_risk_tx(
            tx,
            None,
            Some(world_id),
            "lexicon",
            serde_json::json!({
                "layer": 2,
                "subjectKind": "world_event",
                "domainEventId": pe.domain_event_id,
                "visibility": pe.visibility,
                "verdict": verdict_str(RUNTIME_BLOCK_VERDICT),
                "severity": severity,
                "hits": hits,
            }),
        )
        .await?;

        if runtime_audit_admits(severity) {
            sqlx::query(
                "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, machine_hits, status, created_at) \
                 VALUES (?, ?, ?, ?, ?, 'open', ?)",
            )
            .bind(crate::db::new_id("aq"))
            .bind("world_event")
            .bind(&pe.domain_event_id)
            .bind(verdict_str(RUNTIME_BLOCK_VERDICT))
            .bind(serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()))
            .bind(crate::db::now_ms())
            .execute(&mut **tx)
            .await?;
        }
    }
    if blocked > 0 {
        tracing::warn!(world_id, blocked, "运行时投影命中敏感词库（§15 第 2 层），已打码并转人审");
    }
    Ok(blocked)
}

pub async fn record_risk(
    db: &sqlx::AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
    kind: &str,
    detail: serde_json::Value,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(crate::db::new_id("risk"))
    .bind(user_id)
    .bind(world_id)
    .bind(kind)
    .bind(detail.to_string())
    .bind(crate::db::now_ms())
    .execute(db)
    .await?;
    Ok(())
}

/// `record_risk` 的事务内变体：在调用方已开启的 tx 内写 `risk_events`，与业务副作用原子。
///
/// 用于扣费口 `ledger::charge` 的可疑交易留痕（未成年 owner 分成挂账 / 自打赏防刷 / 异常大额）——
/// 这些留痕必须与扣费在同一事务：要么随扣费一起提交，要么随回滚一起消失（绝不产生「记了险但没扣款」
/// 或反之的错位账）。且单连接池（测试/SQLite）下不可再向池借连接，必须复用 tx，否则死锁 PoolTimedOut。
///
/// 原先按 `any(billing, arena)` 门控（只有经济模块用）；§15 第 2 层的运行时闸
/// `moderate_runtime_projection` 在**所有 feature 组合**下都要在 tick 事务内记险，故取消门控。
pub async fn record_risk_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user_id: Option<&str>,
    world_id: Option<&str>,
    kind: &str,
    detail: serde_json::Value,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(crate::db::new_id("risk"))
    .bind(user_id)
    .bind(world_id)
    .bind(kind)
    .bind(detail.to_string())
    .bind(crate::db::now_ms())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub fn verdict_str(v: ModerationVerdict) -> &'static str {
    match v {
        ModerationVerdict::Approved => "approved",
        ModerationVerdict::Pending => "pending",
        ModerationVerdict::Rejected => "rejected",
    }
}

// ==================== 测试：运行时闸的端到端契约 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{insert_events_tx, StoredEvent, MODERATION_APPROVED};
    use crate::safety::testkit::*;

    fn pe(domain_id: &str, summary: &str) -> ProjectedEvent {
        ProjectedEvent {
            domain_event_id: domain_id.into(),
            event_type: "dialogue".into(),
            actor_ids: vec!["c1".into()],
            visibility: "public".into(),
            audience_user_ids: Vec::new(),
            summary: summary.into(),
            arbiter_note: None,
            moderation: MODERATION_APPROVED.into(),
        }
    }

    /// 复刻 `runtime::commit_tick` 的真实次序：同一事务内「投影 → 第 2 层闸 → 落库」。
    async fn gate_and_store(
        state: &AppState,
        world: &str,
        mut projected: Vec<ProjectedEvent>,
    ) -> (usize, Vec<StoredEvent>, Vec<ProjectedEvent>) {
        let mut tx = state.db.begin().await.unwrap();
        let blocked = moderate_runtime_projection(&mut tx, world, &mut projected).await.unwrap();
        let stored = insert_events_tx(&mut tx, world, 0, &projected).await.unwrap();
        tx.commit().await.unwrap();
        (blocked, stored, projected)
    }

    async fn seed_running_world(state: &AppState) {
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
    }

    /// 命中 → 就地打码 + 置 pending + 记险；低危默认**不**进人审队列（平行入口的核心取舍）。
    #[tokio::test]
    async fn runtime_gate_masks_blocks_and_records_risk() {
        let state = test_state().await;
        seed_running_world(&state).await;

        let (blocked, stored, out) = gate_and_store(
            &state,
            "w1",
            vec![
                // 零宽字符绕过：归一化管线复用自 inject，照样命中。
                pe("de-dirty", "他冷笑一声：你这个傻\u{200B}逼，滚开。"),
                pe("de-clean", "两位大臣于烛下各怀心事，礼数周全。"),
            ],
        )
        .await;

        assert_eq!(blocked, 1, "只应拦下命中的那一条");
        assert_eq!(out[0].moderation, "pending", "命中事件置为待人审");
        assert!(!out[0].summary.contains('傻') && !out[0].summary.contains('逼'), "{}", out[0].summary);
        assert!(out[0].summary.contains("他冷笑一声"), "未命中片段保留：{}", out[0].summary);
        assert!(!out[0].summary.contains('\u{200B}'), "命中区间内的零宽符一并抹掉");

        assert_eq!(out[1].moderation, MODERATION_APPROVED, "未命中事件正常放行");
        assert_eq!(out[1].summary, "两位大臣于烛下各怀心事，礼数周全。", "未命中文本不得被改写");

        // 落库文本本身已是打码版（§0.3：落库前改写，不做事后回滚公开事实）。
        let db_summary: String = sqlx::query_scalar(
            "SELECT public_projection_json FROM world_events WHERE domain_event_id='de-dirty'",
        )
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert!(!db_summary.contains('傻'), "落库的即是打码后的最终事实：{db_summary}");

        // 推送层：未过审事件不进广播。
        assert_eq!(stored.len(), 1);
        assert!(stored[0].payload_json.contains("de-clean"));

        // 风控留痕：命中即记 risk_events（kind='lexicon'，带世界维度）。
        assert_eq!(
            count(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind='lexicon' AND world_id='w1'").await,
            1,
            "命中必须记险"
        );
        // 平行入口的取舍：低危（辱骂）只记险，不写 audit_queue——运行时逐 tick 入队会淹掉人审队列。
        assert_eq!(count(&state.db, "SELECT COUNT(*) FROM audit_queue").await, 0, "低危不得进人审队列");
    }

    /// 高危分类（默认策略 `high`）才进人审队列，subject 指回 domain_event_id 可反查。
    #[tokio::test]
    async fn high_severity_hits_enter_audit_queue() {
        let state = test_state().await;
        seed_running_world(&state).await;

        let (blocked, _, out) = gate_and_store(&state, "w1", vec![pe("de-h", "他从怀里掏出一包冰毒。")]).await;
        assert_eq!(blocked, 1);
        assert_eq!(out[0].moderation, "pending");

        assert_eq!(count(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind='lexicon'").await, 1);
        let subject: String = sqlx::query_scalar(
            "SELECT subject_id FROM audit_queue WHERE subject_kind='world_event'",
        )
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(subject, "de-h", "人审队列应指回 domain_event_id");
    }

    /// 全部未命中：零改写、零留痕、正常下发（不得因为接了闸就给所有事件记险）。
    #[tokio::test]
    async fn clean_projection_passes_through_without_side_effects() {
        let state = test_state().await;
        seed_running_world(&state).await;

        let (blocked, stored, out) =
            gate_and_store(&state, "w1", vec![pe("de-a", "她推门而入，屋内一片寂静。")]).await;
        assert_eq!(blocked, 0);
        assert_eq!(out[0].moderation, MODERATION_APPROVED);
        assert_eq!(out[0].summary, "她推门而入，屋内一片寂静。");
        assert_eq!(stored.len(), 1, "未命中事件正常进广播");
        assert_eq!(count(&state.db, "SELECT COUNT(*) FROM risk_events").await, 0);
        assert_eq!(count(&state.db, "SELECT COUNT(*) FROM audit_queue").await, 0);
    }

    /// 日报是第二条外泄路径（§2.5 高光聚合）：被拦事件绝不能从这里绕出去。
    #[tokio::test]
    async fn blocked_event_never_reaches_daily_report() {
        let state = test_state().await;
        seed_running_world(&state).await;

        gate_and_store(
            &state,
            "w1",
            vec![
                pe("de-bad", "他从怀里掏出一包冰毒，压低声音谈起交易。"),
                pe("de-good", "他把伞递过去，转身走进雨里。"),
            ],
        )
        .await;

        let day = crate::runtime::day_string(crate::db::now_ms());
        let rid = crate::reports::generate_report(&state, "w1", "u1", "c1", &day).await.unwrap();
        let content: String =
            sqlx::query_scalar("SELECT content_json FROM daily_reports WHERE id = ?")
                .bind(&rid)
                .fetch_one(&state.db)
                .await
                .unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let highlights = v["highlights"].as_array().unwrap();
        assert_eq!(highlights.len(), 1, "日报只应聚合过审事件：{content}");
        assert_eq!(highlights[0]["summary"], serde_json::json!("他把伞递过去，转身走进雨里。"));
        assert!(!content.contains("冰毒"), "被拦文本不得出现在日报：{content}");
        assert!(!content.contains("de-bad"), "被拦事件 id 也不得出现在日报");
    }

    /// 入队策略是参数（§0.2），且配错值一律回落默认（不静默放宽/收紧）。
    #[test]
    fn runtime_audit_policy_is_parameterized_with_safe_default() {
        assert_eq!(DEFAULT_RUNTIME_AUDIT, "high");
        assert_eq!(verdict_str(RUNTIME_BLOCK_VERDICT), "pending", "拦截态必须是非 approved 值");
        // 默认策略（无 env）：仅高危入队。
        assert!(runtime_audit_admits(Severity::High));
        assert!(!runtime_audit_admits(Severity::Low));
    }
}
