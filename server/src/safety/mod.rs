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
//! ## 入队/记险的三条入口（**契约：只有这三条**）
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
//!    ℹ️ 入队之后的**出口**在 `admin_api::audit`：`world_event` 主体的裁决回写（migration 0047）。
//!    它是 `world_events.moderation` 上**唯一的放宽路径**，权限分两档（reviewer 推翻机器 /
//!    admin 推翻人审终判），判据取自 `audit_queue.status`。此前该主体没有回写分支，
//!    人审点「通过」是一次静默空操作，内容会永久停在 `pending`。
//!
//!    2b. `semantic`（子模块）：同一批运行时产出的 **§15 第 3 层**（语义分类）**异步**复核。
//!    它与 ② 是**同一条运行时链路的两段**，不是第四个入口：② 在 tick 事务内同步跑纯本地词表，
//!    `semantic` 在 **tick 提交之后、事务之外**跑网络调用 `check_text`，非 Approved 时把
//!    `world_events.moderation` 从 `approved` **收紧**（正文一个字节不改，§0.3）。
//!    留痕与入队一律复用本文件的 `record_risk` / `insert_runtime_audit`，**不另开写入路径**。
//!    🔴 provider 目前是 Dev 桩，故第 3 层**接通了但拦不住任何东西**——见该模块头。
//! 3. `queue_operator_recheck` / `queue_operator_recheck_image`：**已发布内容的运营再审**
//!    （人工发起，非机器判定）。①② 都是「内容刚产生时」的闸；已过审内容事后被举报出问题时，
//!    没有任何一条既有路径能把它送回人审队列——`admin_api::takedown` 需要的正是这条。
//!    它与 ① 跑同一套机审（让人审看到同样的机审上下文），但**无论裁决如何都入队**：入队理由是
//!    「有人举报」而不是「机器判定可疑」，机审直过恰恰是需要人来看的情形。
//!
//!    两个变体只差在跑哪一路机审：文本主体（角色卡 / 世界模板）跑 `check_text` + 注入检测；
//!    位图主体（立绘 / 世界封面）跑 `check_image`。入队、幂等、留痕三段**共用同一份实现**
//!    （`queue_recheck_inner`），所以位图接进来不会长出第二套口径。
//!
//!    🔴 它**不是**给 admin 开的侧门 —— 侧门的定义是绕过本模块直接 INSERT。这是同一扇门加宽一格：
//!    写入仍然只发生在本文件里，机审、留痕、字段口径与 ① 逐字一致。
//!
//! 换言之「唯一写入方」的旧契约现在读作：**静态内容走 ①，运行时产出走 ②，已发布内容的运营再审
//! 走 ③，此外任何模块都不得直接 INSERT audit_queue / risk_events**。
//!
//! ## 读取面的处置闸门：`disposal` 子模块
//!
//! 上面三条都是**入口**（内容怎么进人审）。`disposal` 是**出口**：一个已被下架的主体，它的
//! 名字还该不该从 `card_json` 里解引用出来摆给别人看。见该模块头注释。

use crate::app::AppState;
use crate::error::ApiError;
use crate::events::ProjectedEvent;
use crate::providers::ModerationVerdict;

pub mod disposal;
mod inject;
pub(crate) mod lexicon;
/// §15 **第 3 层**：语义分类异步复核（tick 提交后、事务之外）。见该模块头——
/// 🔴 它交付的是**管线**，不是防线：`ModerationProvider` 当前是 Dev 桩，一次真实语义分类都没发生。
pub mod semantic;
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
             VALUES ($1, $2, $3, $4, $5, 'open', $6)",
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

// ==================== 入口 ③：已发布内容的运营再审 ====================

/// 运营发起的再审留痕 kind（`risk_events.kind`）。与 ① 的 'moderation'/'injection' 分开：
/// 那两个是「机器判定」，这个是「人工调取」，混在一起会让风控面上的机审命中率算不准。
const RECHECK_RISK_KIND: &str = "content_recheck";

/// 入口 ③：把**已发布**的静态内容重新送回人审队列（`audit_queue`，status='open'）。
///
/// 与 ① `moderate_and_queue` 的差别只有一处，但很关键：**无论机审裁决如何都入队**。
/// ① 的语义是「机器觉得可疑才叫人来看」；③ 的语义是「有人举报，所以叫人来看」——
/// 机审直过在这里恰恰是最需要人眼的情形（能过机审的违规内容才是举报的主要来源）。
/// 机审仍然照跑，是为了把 `machine_hits` 一并摆到审核工作台上，让人审拿到与 ① 同样的上下文。
///
/// **幂等**：同一主体已有 `status='open'` 的队列行时不再插新行，原样返回既有那条
/// （`created=false`）。同一张卡被多人举报是常态，一举报一入队会让人审队列被同一个主体刷屏。
///
/// 副作用（本函数是这条路径上唯一的写入方）：
/// - 新建时 → INSERT `audit_queue`（`subject_kind` 必须是 `admin_api::audit::review` 认识的值，
///   否则人审裁决无处回写）；
/// - **每次调用**（含幂等命中）→ `record_risk(kind='content_recheck')`，detail 含发起人与理由。
///   幂等命中也记：那是一次真实发生的运营调取动作，不该因为队列里已有行就不留痕。
///
/// 返回 `(队列行 id, 机审裁决, 是否新建)`。
pub async fn queue_operator_recheck(
    state: &AppState,
    subject_kind: &str,
    subject_id: &str,
    text: &str,
    actor_id: &str,
    reason: &str,
) -> Result<(String, ModerationVerdict, bool), ApiError> {
    let hits = detect_injection(text);
    let provider = state
        .moderation
        .check_text(text)
        .await
        .map_err(|e| ApiError::internal(std::io::Error::other(e)))?;
    // 与 ① 同一条折叠规则：注入命中即便 provider 直过也算 Pending（保守阈值，§14 最高优先级威胁）。
    let verdict = match provider {
        ModerationVerdict::Rejected => ModerationVerdict::Rejected,
        ModerationVerdict::Pending => ModerationVerdict::Pending,
        ModerationVerdict::Approved if !hits.is_empty() => ModerationVerdict::Pending,
        ModerationVerdict::Approved => ModerationVerdict::Approved,
    };

    queue_recheck_inner(state, subject_kind, subject_id, verdict, provider, &hits, actor_id, reason)
        .await
}

/// 入口 ③ 的**位图**变体：把已发布的立绘 / 世界封面重新送回人审队列。
///
/// ## 补的是哪个缺口
///
/// `ModerationProvider::check_image` 自迁移 0016（立绘）/ 0027（封面）起就在**发布路径**上跑，
/// 但裁决落列之后**没有任何人审入队路径**——0027 的迁移注释把这件事记成了已知缺口：
/// 「人审改判的回写路径（`admin_api::audit` 的 `subject_kind` 分支）尚无 `world_cover` 分支，
/// 故本波次封面机审裁决不入 `audit_queue`（避免制造无法被改判的死队列项）」。
/// 于是位图主体一直只有「下架」没有「再审」，0044 的再审端点遇到位图只能 400 如实告知。
///
/// 本函数与 `admin_api::audit::review` 里新增的两个回写分支（`character_avatar` →
/// `cloud_characters.avatar_moderation`、`world_cover` → `worlds.cover_moderation`）是**一对**：
/// 🔴 少了任何一半，入队就重新变成 0027 警告过的那种「无法被改判的死队列项」。
///
/// ## 与文本变体的差别只有一处
///
/// 机审走 `check_image`，且 `machine_hits` **恒为空数组**——注入检测是文本管线（归一化 →
/// 紧凑串匹配），对图片字节跑它只会得到噪声。这里显式给空，而不是让它「碰巧没命中」：
/// 人审工作台上一个空的命中列表读作「图审没给出可展示的命中点」，那是真话。
///
/// 入队、幂等、留痕三段与文本变体**共用同一份实现**，口径逐字一致。
pub async fn queue_operator_recheck_image(
    state: &AppState,
    subject_kind: &str,
    subject_id: &str,
    bytes: &[u8],
    actor_id: &str,
    reason: &str,
) -> Result<(String, ModerationVerdict, bool), ApiError> {
    let provider = state
        .moderation
        .check_image(bytes)
        .await
        .map_err(|e| ApiError::internal(std::io::Error::other(e)))?;
    // 位图路径没有注入检测这一路信号，故裁决就是 provider 裁决（不做 ① 的折叠）。
    queue_recheck_inner(state, subject_kind, subject_id, provider, provider, &[], actor_id, reason)
        .await
}

/// 入口 ③ 的共用实现：入队（幂等）+ 留痕。两个变体只在**跑哪一路机审**上分叉，
/// 分叉之后的写入路径必须只有一条——否则位图与文本会各自长出一套 `audit_queue` 字段口径。
#[allow(clippy::too_many_arguments)]
async fn queue_recheck_inner(
    state: &AppState,
    subject_kind: &str,
    subject_id: &str,
    verdict: ModerationVerdict,
    provider: ModerationVerdict,
    hits: &[InjectionHit],
    actor_id: &str,
    reason: &str,
) -> Result<(String, ModerationVerdict, bool), ApiError> {
    // 幂等：已有 open 行就复用。全序取一行（`created_at DESC, id DESC`）——同毫秒并列时
    // 不加次级键会取到不确定的那一行，回执里的 queueId 就会在两次调用间跳变。
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM audit_queue WHERE subject_kind = $1 AND subject_id = $2 AND status = 'open' \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(subject_kind)
    .bind(subject_id)
    .fetch_optional(&state.db)
    .await?;

    let (queue_id, created) = match existing {
        Some(id) => (id, false),
        None => {
            let id = crate::db::new_id("aq");
            sqlx::query(
                "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, machine_hits, status, created_at) \
                 VALUES ($1, $2, $3, $4, $5, 'open', $6)",
            )
            .bind(&id)
            .bind(subject_kind)
            .bind(subject_id)
            .bind(verdict_str(verdict))
            .bind(serde_json::to_string(hits).unwrap_or_else(|_| "[]".into()))
            .bind(crate::db::now_ms())
            .execute(&state.db)
            .await?;
            (id, true)
        }
    };

    record_risk(
        &state.db,
        None,
        None,
        RECHECK_RISK_KIND,
        serde_json::json!({
            "subjectKind": subject_kind,
            "subjectId": subject_id,
            "queueId": queue_id,
            "created": created,
            "verdict": verdict_str(verdict),
            "providerVerdict": verdict_str(provider),
            "hits": hits,
            "actorId": actor_id,
            "reason": reason,
        }),
    )
    .await?;

    Ok((queue_id, verdict, created))
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
/// ## 第 3 层（语义分类）在哪 —— 已实装，但**在事务之外**
///
/// 原先此处挂着 `TODO(§15-L3)`，现由子模块 [`semantic`] 落地，形态与那条 TODO 逐字一致：
/// **tick 提交后另起任务跑 `check_text`，非 Approved 时收紧 `world_events.moderation`**
/// （公开投影全量、私有抽样，抽样率参数化且确定性），配合 §15 第 4 层「直播场延迟 1-2 拍缓冲」
/// 给这条异步链留出拦截窗口。
///
/// 🔴 **它绝不能挪进这个事务**，理由不变：`check_text` 是网络调用，单连接池（测试/SQLite dev）下
/// tx 持有唯一连接，调用期间任何再借连接的操作都会死锁 PoolTimedOut（同 `record_risk_tx` 的注释）；
/// 且 tick 事务的持有时长会被外部 RTT 绑架，而 `world_event_seq` 的行级排他锁就在同一事务里。
/// `semantic` 因此**从不 `begin()`**（源码级红线用例扫死），`runtime::commit_tick` 里也只在
/// `tx.commit()` **之后**调一次 `semantic::enqueue_after_commit`（纯内存入队，无 IO）。
///
/// ⚠️ 第 3 层默认**关闭**（`MUSE_SAFETY_SEMANTIC_RECHECK`），且 provider 目前是 Dev 桩——
/// 接通 ≠ 生效，详见 [`semantic`] 模块头。
pub async fn moderate_runtime_projection(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    world_id: &str,
    projected: &mut [ProjectedEvent],
    // 🔴 由调用方在**进事务之前**解析好传入（`runtime::commit_tick` 的 `db.begin()` 之前）。
    // 本函数在 tick 事务内，自己查库就是单连接池自锁——那正是 `MIGRATION_NOTES` 当初
    // 判定「这个开关干脆不迁」的头号理由，收 bool 把它解掉了。
    lexicon_on: bool,
) -> Result<usize, ApiError> {
    if !lexicon_on {
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
            insert_runtime_audit(
                &mut **tx,
                world_id,
                &pe.domain_event_id,
                RUNTIME_BLOCK_VERDICT,
                &serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()),
            )
            .await?;
        }
    }
    if blocked > 0 {
        tracing::warn!(world_id, blocked, "运行时投影命中敏感词库（§15 第 2 层），已打码并转人审");
    }
    Ok(blocked)
}

/// 运行时产出（§15 第 2 层词库闸 + 第 3 层语义复核）**共用的唯一一条 `audit_queue` 写入语句**。
///
/// 两层的入队理由不同（一个是词表命中、一个是语义分类/机器没能判定），但落进人审工作台的
/// **字段口径必须只有一份**——否则 reviewer 会在同一个队列里看到两套 `subject_kind` /
/// `machine_hits` 形状，而这种漂移只有在人审真的点开时才暴露。
///
/// 泛型于 `Executor` 是为了同时服务两个调用点：第 2 层在 tick 事务内（`&mut **tx`），
/// 第 3 层在事务外（`&AnyPool`）。**能力边界不变**：写入仍然只发生在 `safety` 模块里。
///
/// 🔴 **`world_id` 不是可选的元信息，是回写路径的定位坐标**（migration 0047）。
/// `subject_id` 存的是 `domain_event_id`，而引擎按 `patch-{base_revision}-ev-{seq}` 生成它——
/// 确定性、不含世界维度，于是两个世界在同一 revision 上的事件 id 逐字相同。人审回写若只拿
/// `domain_event_id` 去 UPDATE，会把**别的世界里同名的、正被拦下的事件一并放行**。
/// 详见 `admin_api::audit::resolve_world_event`。
async fn insert_runtime_audit<'e, E>(
    exec: E,
    world_id: &str,
    domain_event_id: &str,
    verdict: ModerationVerdict,
    machine_hits_json: &str,
) -> Result<(), ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    // 占位符按 SQL 文本顺序发号、严格升序不复用，且与下面的 bind 顺序一一对应。
    sqlx::query(
        "INSERT INTO audit_queue (id, subject_kind, subject_id, subject_world_id, machine_verdict, machine_hits, status, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'open', $7)",
    )
    .bind(crate::db::new_id("aq"))
    .bind(WORLD_EVENT_SUBJECT)
    .bind(domain_event_id)
    .bind(world_id)
    .bind(verdict_str(verdict))
    .bind(machine_hits_json)
    .bind(crate::db::now_ms())
    .execute(exec)
    .await?;
    Ok(())
}

/// 运行时世界事件在人审队列里的 `subject_kind`。
///
/// 🔴 全仓唯一字面量：入队侧（本文件）与回写侧（`admin_api::audit`）共用它。两处各写一份的话，
/// 哪天改了其中一处，入队照旧而回写静默失配——表现正是本批次要修的那个缺陷「点了通过什么都没发生」。
pub const WORLD_EVENT_SUBJECT: &str = "world_event";

pub async fn record_risk(
    db: &sqlx::AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
    kind: &str,
    detail: serde_json::Value,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
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
        "INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
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
        // 🔴 开关**在进事务之前**解析（事务里查库 = 单连接池自锁）——与 `commit_tick` 同姿势。
        let lexicon_on = lexicon::enabled(&state.db).await;
        let mut tx = state.db.begin().await.unwrap();
        let blocked =
            moderate_runtime_projection(&mut tx, world, &mut projected, lexicon_on).await.unwrap();
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
            sqlx::query_scalar("SELECT content_json FROM daily_reports WHERE id = $1")
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
