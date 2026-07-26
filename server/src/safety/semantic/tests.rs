//! §15 第 3 层（语义分类异步复核）用例。
//!
//! 覆盖清单，按重要性排：
//!   · 🔴 **正文逐字节不变**（只收紧 `moderation` 一列）——`red_line_recheck_never_rewrites_event_text`
//!   · 🔴 **单向棘轮**（不放宽、不覆盖更严裁决）——`red_line_tightening_is_one_way_only`
//!   · 🔴 **`world_events` 写入路径全仓盘点**（收紧只能从 approved 出发；放宽唯一且守卫齐全）
//!     ——`red_line_world_events_has_one_ratchet_and_one_guarded_relax`（migration 0047 后扩写）
//!   · 🔴 **provider 故障 fail-closed**（先重试、到顶收紧）——`red_line_provider_outage_fails_closed_not_open`
//!   · 🔴 **关闭时逐字节等同接线前**——`disabled_is_byte_identical_to_before_wiring`
//!   · 🔴 **不开事务 / 不越过 safety 既有入口写 risk_events、audit_queue**（源码级扫描）
//!   · 🔴 **「当前是桩」随数据走**——`stub_fact_travels_with_every_row_and_response`
//!   · 抽样确定性与参数化 · 直播缓冲窗口的 preemptive 度量 · 运营读数的成本口径与诚实边界

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::{AnyPool, Row};
use tower::ServiceExt;

use crate::app::build_router;
use crate::events::{persist_events, ProjectedEvent, MODERATION_APPROVED};
use crate::providers::ModerationProvider;
use crate::safety::testkit::{seed_member, seed_user, seed_world, test_state};

use super::*;

// ═══════════════════════════════════════════════════════════════════════════
// 夹具
// ═══════════════════════════════════════════════════════════════════════════

/// 剧本化机审 provider：定死裁决 / 定死报错，并如实报出自己是不是桩。
struct ScriptedModeration {
    verdict: ModerationVerdict,
    fail: bool,
    stub: bool,
    calls: AtomicUsize,
}

impl ScriptedModeration {
    fn verdict(v: ModerationVerdict) -> Arc<Self> {
        Arc::new(Self { verdict: v, fail: false, stub: true, calls: AtomicUsize::new(0) })
    }
    fn failing() -> Arc<Self> {
        Arc::new(Self {
            verdict: ModerationVerdict::Approved,
            fail: true,
            stub: true,
            calls: AtomicUsize::new(0),
        })
    }
    /// 「已接真实服务商」的形状：唯一区别是显式覆写 `is_dev_stub() == false`。
    fn production(v: ModerationVerdict) -> Arc<Self> {
        Arc::new(Self { verdict: v, fail: false, stub: false, calls: AtomicUsize::new(0) })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ModerationProvider for ScriptedModeration {
    async fn check_text(&self, _text: &str) -> Result<ModerationVerdict, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err("provider 不可用（用例注入）".into())
        } else {
            Ok(self.verdict)
        }
    }
    fn is_dev_stub(&self) -> bool {
        self.stub
    }
}

/// 打开第 3 层开关（写 `runtime_flags` 而不是设 env —— env 是进程级的，会与并发用例互踩）。
async fn enable_l3(db: &AnyPool, scope: &str, target: &str) {
    sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) \
         VALUES ($1, $2, $3, $4, 1, 0, 0, 'test', $5, 'test', $6)",
    )
    .bind(new_id("rf"))
    .bind(ENV_SEMANTIC_RECHECK)
    .bind(scope)
    .bind(target)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .expect("enable flag");
}

fn pe(domain_id: &str, summary: &str, public: bool) -> ProjectedEvent {
    ProjectedEvent {
        domain_event_id: domain_id.into(),
        event_type: "dialogue".into(),
        actor_ids: vec!["c1".into()],
        visibility: if public { "public".into() } else { "private".into() },
        audience_user_ids: if public { Vec::new() } else { vec!["u1".into()] },
        summary: summary.into(),
        arbiter_note: None,
        moderation: MODERATION_APPROVED.into(),
    }
}

async fn seed_tick(state: &AppState, world: &str, tick: i64, events: &[ProjectedEvent]) {
    persist_events(&state.db, world, tick, events).await.expect("seed events");
}

async fn seed_running_world(state: &AppState) {
    seed_user(&state.db, "u1").await;
    seed_world(&state.db, "w1", 0, "running").await;
    seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
}

fn job(world: &str, tick: i64, attempt: i64) -> RecheckJob {
    RecheckJob {
        world_id: world.into(),
        tick_no: tick,
        attempt,
        retry_ids: Vec::new(),
    }
}

/// `world_events` 的**整行**快照。正文不变的证明靠它：除 `moderation` 外每一列逐字节比对。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EventSnapshot {
    id: String,
    world_id: String,
    tick_no: i64,
    sequence: i64,
    domain_event_id: String,
    event_type: String,
    actors_json: String,
    visibility: String,
    audience_json: Option<String>,
    public_projection_json: Option<String>,
    private_projections_json: Option<String>,
    arbiter_note: Option<String>,
    moderation: String,
    ai_label: i64,
    occurred_at: i64,
}

impl EventSnapshot {
    /// 抹掉审核态之后的「正文部分」。两次快照的这一部分必须完全相等。
    fn body(&self) -> Self {
        Self { moderation: String::new(), ..self.clone() }
    }
}

async fn snapshot_events(db: &AnyPool, world: &str) -> Vec<EventSnapshot> {
    let rows = sqlx::query(
        "SELECT id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
         visibility, audience_json, public_projection_json, private_projections_json, \
         arbiter_note, moderation, CAST(ai_label AS BIGINT) AS ai_label, occurred_at FROM world_events \
         WHERE world_id = $1 ORDER BY sequence ASC, id ASC",
    )
    .bind(world)
    .fetch_all(db)
    .await
    .expect("snapshot");
    rows.into_iter()
        .map(|r| EventSnapshot {
            id: r.try_get("id").unwrap(),
            world_id: r.try_get("world_id").unwrap(),
            tick_no: r.try_get("tick_no").unwrap(),
            sequence: r.try_get("sequence").unwrap(),
            domain_event_id: r.try_get("domain_event_id").unwrap(),
            event_type: r.try_get("event_type").unwrap(),
            actors_json: r.try_get("actors_json").unwrap(),
            visibility: r.try_get("visibility").unwrap(),
            audience_json: r.try_get("audience_json").unwrap(),
            public_projection_json: r.try_get("public_projection_json").unwrap(),
            private_projections_json: r.try_get("private_projections_json").unwrap(),
            arbiter_note: r.try_get("arbiter_note").unwrap(),
            moderation: r.try_get("moderation").unwrap(),
            ai_label: r.try_get("ai_label").unwrap(),
            occurred_at: r.try_get("occurred_at").unwrap(),
        })
        .collect()
}

async fn count(db: &AnyPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(db).await.expect("count")
}

fn admin_token(state: &AppState, role: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, "adm1", role, 3600).unwrap()
}

async fn get_json(app: &axum::Router, uri: &str, tok: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// 去掉行注释后的源码（源码级红线用例专用）。
/// 本模块的 SQL 字面量里不含 `//`，故按首个 `//` 截断是安全的。范式抄 `livestage::tests`。
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 一段**故意难看**的正文：含中日文、零宽符、引号、换行与 emoji。
/// 用它是为了让「正文没被改写」这件事在字节层面有说服力——任何打码、归一化、
/// trim、重新序列化都会让它变形。
const NASTY: &str = "她冷笑：「你这个傻\u{200B}逼，滚。」\n——落款：某人 🗡 \u{FEFF}末尾还有空格   ";

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ① 关闭时与接线前逐字节一致（VALIDATION §0.1）
// ═══════════════════════════════════════════════════════════════════════════

/// 开关默认关闭：`commit_tick` 里那一行**什么都不产生**，复核任务也不会跑。
#[tokio::test]
async fn disabled_is_byte_identical_to_before_wiring() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::verdict(ModerationVerdict::Rejected);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    seed_tick(&state, "w1", 0, &[pe("de-1", NASTY, true), pe("de-2", NASTY, false)]).await;

    let before = snapshot_events(&state.db, "w1").await;

    // ① 接线点：关闭时不入队（等 200ms 都拿不到任务）。
    enqueue_after_commit(&state, "w1", 0).await;
    let popped = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        crate::queue::pop_json::<RecheckJob>(&*state.queue, TOPIC),
    )
    .await;
    assert!(popped.is_err(), "🔴 开关关闭时不得有任何复核任务入队");

    // ② 就算任务被别的途径塞进来，run 也必须原地返回。
    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(r.outcome, OUTCOME_SKIPPED);
    assert_eq!(provider.calls(), 0, "🔴 关闭时不得调用 provider（不得烧一分钱 token）");

    // ③ 数据面零变化。
    assert_eq!(snapshot_events(&state.db, "w1").await, before, "🔴 关闭时 world_events 必须逐字节不变");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM safety_recheck_runs").await, 0);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM risk_events").await, 0);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM audit_queue").await, 0);
}

/// 声明默认值与模块内常量同源（编译期已由 `const _: () = assert!(...)` 钉死，这里做运行期复述）。
#[test]
fn flag_defaults_to_off() {
    assert!(!DEFAULT_SEMANTIC_RECHECK_ENABLED, "🔴 §0.1：未验证功能默认关闭");
    assert_eq!(crate::flags::declared_default(ENV_SEMANTIC_RECHECK), DEFAULT_SEMANTIC_RECHECK_ENABLED);
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ② 正文逐字节不变（§0.3 公共事实不可回滚）
// ═══════════════════════════════════════════════════════════════════════════

/// 复核命中时**只有 `moderation` 一列变**，正文一个字节不动。
#[tokio::test]
async fn red_line_recheck_never_rewrites_event_text() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::verdict(ModerationVerdict::Rejected);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    // 公开 + 私有各一条，正文都用那段难看的字符串（含零宽符 / BOM / 尾随空格 / emoji）。
    seed_tick(&state, "w1", 0, &[pe("de-pub", NASTY, true), pe("de-priv", NASTY, false)]).await;

    let before = snapshot_events(&state.db, "w1").await;
    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert!(r.tightened >= 1, "至少公开那条必须被收紧：{r:?}");
    let after = snapshot_events(&state.db, "w1").await;

    assert_eq!(before.len(), after.len(), "🔴 复核不得增删事件行");
    for (b, a) in before.iter().zip(after.iter()) {
        // 除审核态外，整行逐字段相等（这一条断言同时覆盖了 sequence / occurred_at / ai_label）。
        assert_eq!(b.body(), a.body(), "🔴 第 3 层改写了世界事实的正文：\n前 {b:#?}\n后 {a:#?}");
    }
    // 再逐字节复述一遍最要紧的两列（把 assert 的失败信息落在字节上）。
    let pub_before = before[0].public_projection_json.clone().unwrap();
    let pub_after = after[0].public_projection_json.clone().unwrap();
    assert_eq!(pub_before.as_bytes(), pub_after.as_bytes(), "🔴 公共投影正文被改写了");
    assert!(pub_after.contains('\u{200B}'), "零宽符还在 = 没有被任何归一化管线碰过");
    let priv_after = after[1].private_projections_json.clone().unwrap();
    assert_eq!(before[1].private_projections_json.clone().unwrap().as_bytes(), priv_after.as_bytes());

    // 变的只有审核态，且方向是收紧。
    assert_eq!(after[0].moderation, "rejected");
    assert_ne!(before[0].moderation, after[0].moderation);
}

/// 未命中（provider 直过）：零改写、零留痕、零入队——不得因为接了第 3 层就给所有事件记险。
#[tokio::test]
async fn approved_verdict_leaves_no_trace() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::verdict(ModerationVerdict::Approved);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-ok", "她推门而入，屋内一片寂静。", true)]).await;

    let before = snapshot_events(&state.db, "w1").await;
    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();

    assert_eq!(r.outcome, OUTCOME_DONE);
    assert_eq!(r.public_checked, 1, "公开投影全量送审");
    assert_eq!(r.tightened, 0);
    assert_eq!(provider.calls(), 1);
    assert_eq!(snapshot_events(&state.db, "w1").await, before);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM risk_events").await, 0);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM audit_queue").await, 0);
    // 但**台账仍要记一行**：调用发生了就烧了钱，成本口径不能因为"没命中"而漏记。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM safety_recheck_runs").await, 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ③ 单向棘轮：只收紧，不放宽、不覆盖更严的裁决
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn red_line_tightening_is_one_way_only() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::verdict(ModerationVerdict::Pending);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-a", "甲", true), pe("de-b", "乙", true)]).await;

    // de-b 先被第 2 层（或人审）判成 rejected —— 比本次复核的 pending 更严。
    sqlx::query("UPDATE world_events SET moderation = 'rejected' WHERE domain_event_id = $1")
        .bind("de-b")
        .execute(&state.db)
        .await
        .unwrap();

    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(r.public_candidates, 1, "🔴 非 approved 的行根本不该进候选（幂等由数据本身保证）");
    assert_eq!(provider.calls(), 1, "🔴 已收紧的事件不得被重复送审（那是白烧 token）");

    let after = snapshot_events(&state.db, "w1").await;
    assert_eq!(after[0].moderation, "pending", "approved → pending");
    assert_eq!(after[1].moderation, "rejected", "🔴 更严的既有裁决不得被本层放宽回 pending");

    // 再跑一遍：两条都已非 approved → 无候选 → 不调 provider、不再记账。
    let r2 = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(r2.outcome, OUTCOME_SKIPPED);
    assert_eq!(provider.calls(), 1, "🔴 重放复核任务不得产生额外调用");
}

/// `tighten` 本身的契约：目标行不是 `approved` 就一行都不改（返回 false）。
#[tokio::test]
async fn tighten_is_a_no_op_on_non_approved_rows() {
    let state = test_state().await;
    seed_running_world(&state).await;
    seed_tick(&state, "w1", 0, &[pe("de-x", "丙", true)]).await;
    let id: String = sqlx::query_scalar("SELECT id FROM world_events WHERE domain_event_id = 'de-x'")
        .fetch_one(&state.db)
        .await
        .unwrap();

    assert!(tighten(&state.db, &id, ModerationVerdict::Pending).await.unwrap());
    assert!(
        !tighten(&state.db, &id, ModerationVerdict::Rejected).await.unwrap(),
        "🔴 第二次收紧必须落空：WHERE 钉着 approved，本层不做「越收越紧」的连续改判"
    );
    let m: String = sqlx::query_scalar("SELECT moderation FROM world_events WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(m, "pending");
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ④ provider 故障：先重试（窗口期）→ 到顶 fail-closed
// ═══════════════════════════════════════════════════════════════════════════

/// 重试窗口内**不收紧**（内容仍在外发），并按退避重排下一次尝试，只重查报错那几条。
#[tokio::test]
async fn provider_outage_retries_before_tightening() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::failing();
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "甲", true), pe("de-2", "乙", true)]).await;

    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(r.outcome, OUTCOME_RETRY);
    assert_eq!(r.provider_errors, 2);
    assert_eq!(r.tightened, 0, "🔴 重试窗口内不收紧：那段时间正是第 4 层的缓冲窗口");
    assert_eq!(r.failed_closed, 0);
    let after = snapshot_events(&state.db, "w1").await;
    assert!(after.iter().all(|e| e.moderation == "approved"), "窗口期内内容仍正常外发");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM risk_events").await, 0, "还没定性，不记险");

    // 重排的任务只带报错的那两条 id（不重查已有裁决的，避免抖动把整拍成本乘以尝试次数）。
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::queue::pop_json::<RecheckJob>(&*state.queue, TOPIC),
    )
    .await
    .expect("退避重排的任务应当到期")
    .expect("载荷可解析");
    assert_eq!(next.attempt, 2);
    assert_eq!(next.retry_ids.len(), 2);
    let mut sorted = next.retry_ids.clone();
    sorted.sort();
    assert_eq!(next.retry_ids, sorted, "重试批次的顺序必须确定（排过序）");
}

/// 🔴 重试预算耗尽 → **fail-closed**：收紧为 pending + 记险 + **无条件**进人审队列。
///
/// 方向与 `MUSE_SAFETY_LEXICON` 的 fail-safe（默认「继续过滤」）自洽：审核链自身的故障
/// 绝不转化为放行，否则「打掉 provider」就成了绕过第 3 层的手段。
#[tokio::test]
async fn red_line_provider_outage_fails_closed_not_open() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::failing();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", NASTY, true)]).await;

    let before = snapshot_events(&state.db, "w1").await;
    // 直接跑到预算末尾那一次尝试。
    let r = run_recheck(&state, &job("w1", 0, max_attempts())).await.unwrap();

    assert_eq!(r.outcome, OUTCOME_FAILED_CLOSED);
    assert_eq!(r.failed_closed, 1);
    assert_eq!(r.tightened, 1);

    let after = snapshot_events(&state.db, "w1").await;
    assert_eq!(after[0].moderation, "pending", "🔴 给不出裁决 ≠ 放行");
    assert_ne!(after[0].moderation, "rejected", "「机器没能判定」不该被记成终判");
    // 🔴 正文照旧一个字节不动 —— fail-closed 也不是改写的理由。
    assert_eq!(before[0].body(), after[0].body());

    // 留痕带得出「是 provider 挂了」而不是「内容有问题」。
    let detail: String = sqlx::query_scalar(
        "SELECT detail_json FROM risk_events WHERE kind = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(RISK_KIND)
    .fetch_one(&state.db)
    .await
    .unwrap();
    let d: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(d["reason"], serde_json::json!(REASON_PROVIDER_UNAVAILABLE));
    assert_eq!(d["layer"], serde_json::json!(3));
    assert_eq!(d["bodyRewritten"], serde_json::json!(false));

    // 🔴 无条件进人审：入队理由是「机器没能判定」，那正是必须有人来看的情形。
    let subject: String =
        sqlx::query_scalar("SELECT subject_id FROM audit_queue WHERE subject_kind = 'world_event'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(subject, "de-1");
}

/// fail-closed 的**方向不参数化**：可配的只有阈值（重试次数 / 退避 / 超时）。
#[test]
fn fail_closed_direction_is_not_a_parameter() {
    let code = strip_comments(include_str!("mod.rs"));
    // 收紧动作只有两处调用点（正常裁决 + fail-closed），且 fail-closed 那处写死 Pending。
    assert!(
        code.contains("tighten(&state.db, &c.id, ModerationVerdict::Pending)"),
        "🔴 fail-closed 分支必须写死收紧，不得由 env 决定要不要收紧"
    );
    for banned in ["FAIL_OPEN", "fail_open", "ALLOW_ON_ERROR"] {
        assert!(!code.contains(banned), "🔴 出现了放行方向的开关：{banned}");
    }
    // 阈值该参数化的都参数化了。
    for env in [ENV_TIMEOUT_MS, ENV_MAX_ATTEMPTS, ENV_BACKOFF_MS, ENV_MAX_EVENTS] {
        assert!(env.starts_with("MUSE_"), "{env}");
    }
    assert!(max_attempts() >= 1, "配成 0/负数不得变成「一次都不查」");
    assert_eq!(backoff_ms(1), DEFAULT_BACKOFF_MS, "首次重试用退避基数");
    assert_eq!(backoff_ms(3), DEFAULT_BACKOFF_MS * 4, "指数退避");
    assert!(backoff_ms(999) < i64::MAX / 2, "退避必须封顶，不得溢出成天文数字");
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑤ 抽样：公开全量、私有抽样，且确定性
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn sampling_is_deterministic_and_parameterized() {
    // 边界：满档全收、零档全不收（配错值由 clamp_bp 夹回区间，不静默放宽/收紧）。
    assert!(sampled("w", 0, "de", SAMPLE_BP_FULL));
    assert!(!sampled("w", 0, "de", 0));
    assert_eq!(clamp_bp(-5), 0);
    assert_eq!(clamp_bp(99_999), SAMPLE_BP_FULL);
    assert_eq!(clamp_bp(500), 500);
    // 公开档默认全量（总规格 §15 第 3 层口径），私有档默认抽样。
    assert_eq!(DEFAULT_PUBLIC_SAMPLE_BP, SAMPLE_BP_FULL);
    assert!(DEFAULT_PRIVATE_SAMPLE_BP > 0 && DEFAULT_PRIVATE_SAMPLE_BP < SAMPLE_BP_FULL);

    // 确定性：同一输入恒等（重试拿到同一批样本，复盘算得回来）。
    for i in 0..200 {
        let id = format!("de-{i}");
        let a = sampled("w1", 7, &id, 5_000);
        let b = sampled("w1", 7, &id, 5_000);
        assert_eq!(a, b, "🔴 抽样必须可复现：{id}");
    }
    // 分布大致贴合万分比（SplitMix64 整数流，不掷系统随机数、不用浮点）。
    let hit = (0..2000).filter(|i| sampled("w1", 0, &format!("de-{i}"), 5_000)).count();
    assert!((600..1400).contains(&hit), "5000bp 的命中数偏离过大：{hit}/2000");
    // 换一个 tick 应当换一批样本（种子含 tick_no，不是按 id 一锤定音）。
    let hit_t1 = (0..2000).filter(|i| sampled("w1", 1, &format!("de-{i}"), 5_000)).count();
    assert_ne!(hit, hit_t1, "种子未把 tick_no 算进去");
}

/// 公开投影**全量**送审；私有投影按抽样率过筛，且过筛结果与 `sampled()` 逐条一致。
#[tokio::test]
async fn public_is_checked_in_full_private_is_sampled() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::verdict(ModerationVerdict::Approved);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_WORLD, "w1").await;

    let mut evts = Vec::new();
    for i in 0..40 {
        evts.push(pe(&format!("de-pub-{i}"), "公开正文", true));
        evts.push(pe(&format!("de-priv-{i}"), "私有正文", false));
    }
    seed_tick(&state, "w1", 3, &evts).await;

    let r = run_recheck(&state, &job("w1", 3, 1)).await.unwrap();
    assert_eq!(r.public_candidates, 40);
    assert_eq!(r.public_checked, 40, "🔴 公开投影全量（TODO 原文口径）");
    assert_eq!(r.private_candidates, 40);

    let expect_private: i64 = (0..40)
        .filter(|i| sampled("w1", 3, &format!("de-priv-{i}"), private_sample_bp()))
        .count() as i64;
    assert_eq!(r.private_checked, expect_private, "私有抽样条数必须与确定性判据一致");
    assert!(r.private_checked < r.private_candidates, "默认私有档是抽样，不是全量");
    assert_eq!(provider.calls() as i64, r.public_checked + r.private_checked);

    // 台账把当时生效的抽样率快照下来（事后复盘「那天为什么只查了三成」）。
    let row = sqlx::query(
        "SELECT public_sample_bp, private_sample_bp, chars_checked FROM safety_recheck_runs \
         WHERE world_id = $1 AND tick_no = $2 AND attempt = $3",
    )
    .bind("w1")
    .bind(3i64)
    .bind(1i64)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(row.try_get::<i64, _>("public_sample_bp").unwrap(), SAMPLE_BP_FULL);
    assert_eq!(row.try_get::<i64, _>("private_sample_bp").unwrap(), private_sample_bp());
    assert!(row.try_get::<i64, _>("chars_checked").unwrap() > 0, "送审字符数是成本口径的分子侧");
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑥ 与 §15 第 4 层（直播场延迟缓冲）的配合
// ═══════════════════════════════════════════════════════════════════════════

/// `interceptedBeforeBroadcast` 跟着直播播出水位线走：水位线已经越过该拍 → 不算 preemptive。
#[tokio::test]
async fn intercepted_before_broadcast_tracks_the_live_watermark() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::verdict(ModerationVerdict::Pending);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 5, &[pe("de-early", "缓冲期内拦下", true)]).await;
    seed_tick(&state, "w1", 9, &[pe("de-late", "已经播出去了", true)]).await;

    // 一场直播已经播到第 7 拍。
    sqlx::query(
        "INSERT INTO live_sessions (id, world_id, title, status, announce_at, starts_at, ends_at, \
         delay_ticks, published_high_tick, capacity, created_by, started_at, ended_at, created_at, updated_at) \
         VALUES ($1, 'w1', '场', 'live', 0, 0, 0, 2, 7, 0, 'op', 0, 0, $2, $3)",
    )
    .bind(new_id("ls"))
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();

    // 第 9 拍 > 水位线 7 → 观众还没看见 → preemptive。
    let late = run_recheck(&state, &job("w1", 9, 1)).await.unwrap();
    assert_eq!(late.tightened, 1);
    assert_eq!(late.intercepted_before_broadcast, 1);

    // 第 5 拍 ≤ 水位线 7 → 已经播出去了，收不回（只减少后续可见性）。
    let early = run_recheck(&state, &job("w1", 5, 1)).await.unwrap();
    assert_eq!(early.tightened, 1);
    assert_eq!(
        early.intercepted_before_broadcast, 0,
        "🔴 已经播出的不得被记成「缓冲拦下了」——那会让运营以为延迟拍数够用"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ⑦ 「当前是桩」随数据一起走
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn stub_fact_travels_with_every_row_and_response() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::verdict(ModerationVerdict::Pending);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "正文", true)]).await;
    run_recheck(&state, &job("w1", 0, 1)).await.unwrap();

    // ① 台账的一等列。
    // CAST：PG 下 INTEGER 是 int4，一律显式转 BIGINT 再读（双库可移植子集）。
    let stub: i64 =
        sqlx::query_scalar("SELECT CAST(provider_stub AS BIGINT) FROM safety_recheck_runs")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(stub, 1, "🔴 台账每一行都要带「这行数出自桩」");

    // ② 每一条风控留痕。
    let detail: String =
        sqlx::query_scalar("SELECT detail_json FROM risk_events WHERE kind = $1").bind(RISK_KIND)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&detail).unwrap()["providerStub"],
        serde_json::json!(true),
        "🔴 风控留痕要带 providerStub"
    );

    // ③ 人审工作台看到的 machine_hits。
    let hits: String = sqlx::query_scalar("SELECT machine_hits FROM audit_queue")
        .fetch_optional(&state.db)
        .await
        .unwrap()
        .unwrap_or_else(|| machine_hits(ModerationVerdict::Pending, None, true));
    assert!(hits.contains("providerStub"), "人审要看得见「这条是桩判的」：{hits}");

    // ④ 运营面响应：字段 + 诚实边界数组。
    let app = build_router(state.clone());
    let (st, body) = get_json(&app, "/api/admin/safety/recheck", &admin_token(&state, "operator")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["providerStub"], serde_json::json!(true));
    assert_eq!(body["source"], serde_json::json!("dev-stub"));
    let honesty = body["honesty"].as_array().unwrap();
    assert!(!honesty.is_empty());
    let joined = honesty.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("");
    assert!(joined.contains("桩"), "🔴 诚实边界必须明说这是桩：{joined}");
    assert!(joined.contains("拦不住"), "🔴 必须明说它当前拦不住任何东西：{joined}");
    assert!(joined.contains("五层漏斗已完整"), "🔴 必须显式否掉那句最危险的表述：{joined}");
}

/// 换成「已接真实服务商」的实现（唯一区别是覆写 `is_dev_stub() == false`）：
/// 同一批字段自动翻面，领域代码一行不用改。
#[tokio::test]
async fn production_provider_flips_the_stub_fact_without_touching_domain_code() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::production(ModerationVerdict::Rejected);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "正文", true)]).await;
    run_recheck(&state, &job("w1", 0, 1)).await.unwrap();

    let stub: i64 =
        sqlx::query_scalar("SELECT CAST(provider_stub AS BIGINT) FROM safety_recheck_runs")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(stub, 0);

    let app = build_router(state.clone());
    let (_, body) = get_json(&app, "/api/admin/safety/recheck", &admin_token(&state, "admin")).await;
    assert_eq!(body["source"], serde_json::json!("production"));
    let joined = body["honesty"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("");
    assert!(joined.contains("不等于「已验证」"), "接上了也不等于验证过：{joined}");
}

/// Dev 桩（生产装配里的那个）默认就自认是桩——`is_dev_stub()` 的默认实现即为 `true`。
#[test]
fn dev_provider_admits_it_is_a_stub_by_default() {
    assert!(crate::providers::DevModeration::default().is_dev_stub());
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑧ 运营读数：调用量给得出、成本比值**明说给不出**
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn admin_readout_reports_call_volume_and_declines_the_cost_ratio() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::verdict(ModerationVerdict::Approved);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "甲乙丙", true), pe("de-2", "丁戊己", true)]).await;
    // 生成侧成本（分母）：一条已完成的拍。
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, \
         started_at, finished_at, created_at) VALUES ($1, 'w1', 0, 0, 'done', 1234, $2, $3, $4)",
    )
    .bind(new_id("wt"))
    .bind(now_ms())
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();

    run_recheck(&state, &job("w1", 0, 1)).await.unwrap();

    let app = build_router(state.clone());
    let (st, body) = get_json(&app, "/api/admin/safety/recheck", &admin_token(&state, "operator")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["layer"], serde_json::json!(3));
    assert_eq!(body["checks"], serde_json::json!(2));
    assert!(body["charsChecked"].as_i64().unwrap() > 0);
    assert_eq!(body["cost"]["moderationCallsInWindow"], serde_json::json!(2));
    assert_eq!(body["cost"]["generationTokensInWindow"], serde_json::json!(1234));
    // 🔴 比值明说算不出来（check_text 只回裁决、不回计价），不摆一个假的 5%。
    assert_eq!(body["cost"]["ratioAvailable"], serde_json::json!(false));
    assert!(body["cost"]["why"].as_str().unwrap().contains("5%"));
    assert!(!body["cost"].as_object().unwrap().contains_key("ratio"));
    // 与主看板的关系如实自述（范式同 `ifline` 的 dashboardIntegration）。
    assert_eq!(body["dashboardIntegration"]["mainDashboardIncludesL3"], serde_json::json!(false));
    // 失败方向也随响应走。
    assert_eq!(body["failClosedPolicy"]["direction"], serde_json::json!("fail-closed"));
    assert_eq!(body["failClosedPolicy"]["parameterized"], serde_json::json!(false));
    // 开关状态：本用例开了 global，故为 true；关着的世界里它会是 false。
    assert_eq!(body["flag"], serde_json::json!(ENV_SEMANTIC_RECHECK));
    assert_eq!(body["enabledGlobally"], serde_json::json!(true));
}

/// RBAC：只读端点取 operator 档，reviewer / support 拿不到（与 `admin_api::require_role` 同语义）。
#[tokio::test]
async fn readout_requires_operator_role() {
    let state = test_state().await;
    let app = build_router(state.clone());
    for role in ["reviewer", "support", "finance"] {
        let (st, _) = get_json(&app, "/api/admin/safety/recheck", &admin_token(&state, role)).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{role} 不该看得到运营读数");
    }
    let (st, _) = get_json(&app, "/api/admin/safety/recheck", &admin_token(&state, "operator")).await;
    assert_eq!(st, StatusCode::OK);
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ⑨ 源码级红线
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **绝不进事务**：本模块从不 `begin()`，也不接受 `Transaction` 参数。
/// 理由是 `check_text` 是网络调用——单连接池下事务持有唯一连接，调用期间再借连接必死锁。
#[test]
fn red_line_never_opens_a_transaction() {
    let code = strip_comments(include_str!("mod.rs"));
    for forbidden in [".begin()", "Transaction<", "sqlx::Transaction"] {
        assert!(
            !code.contains(forbidden),
            "🔴 第 3 层出现了事务用法「{forbidden}」——网络调用进事务 = 单连接池死锁 PoolTimedOut"
        );
    }
}

/// 递归收集 `server/src` 下的**生产**源码，返回 `(相对路径, 去注释后的源码)`。
///
/// 跳过 `tests.rs` / `testkit.rs`，并在内联的 `mod tests {` 处截断（`mod tests;` 是外置
/// 文件的声明，不能截——那会把整个模块的生产代码一起丢掉，让扫描形同虚设）。
/// 遍历前排序：断言的失败信息不随文件系统顺序抖动。
fn production_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("读目录 {dir:?} 失败：{e}"))
            .map(|e| e.expect("目录项").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name == "tests.rs" || name == "testkit.rs" {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {path:?}：{e}"));
            let src = match src.find("\nmod tests {") {
                Some(i) => src[..i].to_string(),
                None => src,
            };
            let rel = path.strip_prefix(root).expect("相对路径").to_string_lossy().replace('\\', "/");
            out.push((rel, strip_comments(&src)));
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    assert!(out.len() > 50, "🔴 源码遍历只收到 {} 个文件，扫描口径坏了", out.len());
    out
}

/// 抽出一段源码里所有 `UPDATE world_events ...` 语句（到字符串字面量结束为止），
/// 并把跨行续写（`\` + 换行 + 缩进）折叠成单行，便于按形状断言。
fn world_event_updates(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(i) = code[from..].find("UPDATE world_events") {
        let start = from + i;
        let end = code[start..].find('"').map(|k| start + k).unwrap_or(code.len());
        out.push(code[start..end].replace('\\', " ").split_whitespace().collect::<Vec<_>>().join(" "));
        from = end.max(start + 1);
    }
    out
}

/// 🔴 **`world_events` 上允许存在的写入路径，逐条钉死形状**（原名
/// `red_line_only_write_to_world_events_is_the_moderation_ratchet`；migration 0047 之后
/// 这张表有了第二个方向，故一并改名与扩写，**不是**放宽了检查）。
///
/// 扫的是**全仓生产代码**，不再只扫第 3 层那一个文件——放宽路径落在 `admin_api::audit` 里，
/// 只扫本模块等于把新开的那条路径放到红线之外。三条不变式：
///
/// 1. **正文零改写**（§0.3）：每条语句的 `SET` 列表**只有 `moderation` 一列**。
/// 2. **收紧只能从 `'approved'` 出发**：非放宽语句的 `WHERE` 必须钉着 `moderation = 'approved'`
///    （单向棘轮：不覆盖更严的既有裁决，也不从中途状态乱跳）。
/// 3. **放宽全仓只有一条**，且带明确守卫：`SET` 写字面量 `'approved'`（不是绑定值）、
///    按主键 `id` 点名一行（不按跨世界重名的 `domain_event_id`）、CAS 到读到的当前态、
///    起点白名单 `IN ('pending','rejected')` 写死在 SQL 里（于是不会自我放宽、
///    也不会复活将来可能出现的哨兵值）。
///
/// 新增任何一条 `UPDATE world_events` 都会让本用例红——那正是它存在的意义：
/// 这张表上的写入路径必须逐条经过评审，不许悄悄长出第四条。
#[test]
fn red_line_world_events_has_one_ratchet_and_one_guarded_relax() {
    /// 第 3 层的机器棘轮（`safety::semantic::tighten`）。
    const RATCHET: &str =
        "UPDATE world_events SET moderation = $1 WHERE id = $2 AND moderation = 'approved'";
    /// 人审驳回一条仍在外发的事件（`admin_api::audit`）。形状与棘轮同类：从 approved 出发。
    const HUMAN_TIGHTEN: &str =
        "UPDATE world_events SET moderation = 'rejected' WHERE id = $1 AND moderation = 'approved'";
    /// 🔴 全仓唯一的放宽语句（`admin_api::audit`）。
    const RELAX: &str = "UPDATE world_events SET moderation = 'approved' WHERE id = $1 AND moderation = $2 AND moderation IN ('pending', 'rejected')";
    /// 放宽方向的判别式：`SET` 直接写 `'approved'` 字面量。
    const RELAX_HEAD: &str = "UPDATE world_events SET moderation = 'approved'";

    let sources = production_sources();
    let mut found: Vec<(String, String)> = Vec::new();
    for (file, code) in &sources {
        for sql in world_event_updates(code) {
            found.push((file.clone(), sql));
        }
    }

    // ── 全仓盘点：三条，一条不多 ────────────────────────────────────────────
    let inventory: Vec<String> = found.iter().map(|(f, s)| format!("\n  {f}: {s}")).collect();
    assert_eq!(
        found.len(),
        3,
        "🔴 `world_events` 的写入路径必须逐条评审。当前扫到 {} 条：{}",
        found.len(),
        inventory.join("")
    );

    // ── 不变式 ①②③ 逐条施加在每一条语句上 ──────────────────────────────────
    let mut relaxes: Vec<&(String, String)> = Vec::new();
    for entry in &found {
        let (file, sql) = entry;
        // ① SET 列表只有 moderation 一列 → 正文一个字节不动。
        let set = sql
            .split(" SET ")
            .nth(1)
            .and_then(|s| s.split(" WHERE ").next())
            .unwrap_or_else(|| panic!("🔴 {file} 的语句没有 SET/WHERE 结构：{sql}"));
        assert!(
            set.starts_with("moderation = ") && !set.contains(','),
            "🔴 {file} 改写了 moderation 之外的列（正文即世界事实，§0.3）：SET {set}"
        );
        // 按主键点名一行：`domain_event_id` 跨世界重名，按它写会误伤别的世界。
        assert!(sql.contains("WHERE id = $"), "🔴 {file} 未按主键定位要改的行：{sql}");

        if sql.starts_with(RELAX_HEAD) {
            relaxes.push(entry);
        } else {
            // ② 收紧只能从 approved 出发（单向棘轮不得被削弱）。
            assert!(
                sql.contains(" AND moderation = 'approved'"),
                "🔴 {file} 的收紧语句没有钉住起点 approved —— 它可能覆盖更严的既有裁决：{sql}"
            );
        }
    }

    // ── ③ 放宽有且只有一条，且守卫齐全 ──────────────────────────────────────
    assert_eq!(relaxes.len(), 1, "🔴 放宽路径必须全仓唯一，扫到 {} 条：{relaxes:?}", relaxes.len());
    let (relax_file, relax_sql) = relaxes[0];
    assert_eq!(relax_file, "admin_api/audit.rs", "🔴 放宽路径挪出了人审回写模块：{relax_file}");
    assert_eq!(
        relax_sql, RELAX,
        "🔴 放宽语句的守卫被改动了。它必须同时满足：SET 写字面量 'approved'（方向不可由绑定值决定）、\
         CAS 到读到的当前态（AND moderation = $2）、起点白名单写死在 SQL 里\
         （AND moderation IN ('pending', 'rejected') —— 白名单而非 `<> 'approved'` 黑名单，\
         后者在列为 NULL 时会静默命中 0 行，且会随将来新增的哨兵值失效）"
    );

    // ── 两条收紧语句按文件逐字复核（避免「形状对了但换了个地方」） ──────────
    for (file, sql) in [("safety/semantic/mod.rs", RATCHET), ("admin_api/audit.rs", HUMAN_TIGHTEN)] {
        assert!(
            found.iter().any(|(f, s)| f == file && s == sql),
            "🔴 {file} 里那条收紧语句不见了或被改写：应为 {sql}"
        );
    }

    // ── 第 3 层本模块：不写不删事件行，且**永不写 approved** ────────────────
    let l3 = strip_comments(include_str!("mod.rs"));
    for forbidden in ["INSERT INTO world_events", "DELETE FROM world_events"] {
        assert!(!l3.contains(forbidden), "🔴 第 3 层写/删了世界事实：{forbidden}");
    }
    assert!(
        l3.contains("debug_assert_ne!(verdict, ModerationVerdict::Approved"),
        "🔴 第 3 层的收紧口丢了「绝不写 approved」的断言 —— 它的 SET 是绑定值，方向只由调用方保证"
    );
    // 读是允许且必须的（候选装载要从这里取）。
    assert!(l3.contains("FROM world_events"));

    // ── 人审回写模块：只改可见性，不碰事件行本身 ──────────────────────────
    let review = strip_comments(include_str!("../../admin_api/audit.rs"));
    for forbidden in ["INSERT INTO world_events", "DELETE FROM world_events"] {
        assert!(!review.contains(forbidden), "🔴 人审回写写/删了世界事实：{forbidden}");
    }
}

/// 🔴 风控留痕与人审入队一律走 `safety` 既有入口，本模块不另开写入路径。
#[test]
fn red_line_risk_and_audit_go_through_safety_entries_only() {
    let code = strip_comments(include_str!("mod.rs"));
    for forbidden in ["INSERT INTO risk_events", "INSERT INTO audit_queue"] {
        assert!(
            !code.contains(forbidden),
            "🔴 第 3 层绕过 safety 的既有入口直接写「{forbidden}」——这条边界前面几批一直守着"
        );
    }
    assert!(code.contains("record_risk("), "留痕走 safety::record_risk");
    assert!(code.contains("super::insert_runtime_audit("), "入队走 safety 的共用写入语句");
    // 第 2 层与第 3 层共用同一条 audit_queue 语句 → 人审工作台只有一套字段口径。
    let safety_code = strip_comments(include_str!("../mod.rs"));
    assert_eq!(
        safety_code.matches("INSERT INTO audit_queue").count(),
        3,
        "🔴 safety 里的 audit_queue 写入点应为 3 处（静态内容 ① / 运营再审 ③ / 运行时 ②+2b 共用的那条）"
    );
}

/// 🔴 确定性契约：不用系统随机、不用浮点 RNG、不依赖 map 迭代序驱动 RNG。
#[test]
fn red_line_no_system_randomness_and_no_float_rng() {
    let code = strip_comments(include_str!("mod.rs"));
    for forbidden in ["thread_rng", "rand::", "SystemRandom", "f32", "f64", "HashMap", "BTreeMap"] {
        assert!(!code.contains(forbidden), "🔴 抽样引入了非确定性来源或浮点：{forbidden}");
    }
    assert!(code.contains("fnv1a_64"), "种子走 assembly::fnv1a_64");
    assert!(code.contains("assembly::Rng"), "整数流走 assembly::Rng（SplitMix64）");
    // 域常量已登记进 assembly 的唯一清单，且清单的「下一个可用」已续号。
    let assembly = include_str!("../../assembly/mod.rs");
    assert!(assembly.contains("0x5C：`safety::semantic`"), "🔴 域常量 0x5C 未登记进唯一清单");
    assert!(assembly.contains("**下一个可用域常量是 0x5D**"), "🔴 清单的下一个可用号未续");
}

/// 🔴 接线点必须落在**已提交**的位置（`runtime::commit_tick` 的 `tx.commit()` 之后），
/// 而不是任何一个还开着的事务里。判据是「它前面最近的一次 `tx.commit()` 晚于最近的一次 `begin()`」。
#[test]
fn red_line_enqueue_point_is_after_commit() {
    let runtime = include_str!("../../runtime/mod.rs");
    let hook = runtime
        .find("safety::semantic::enqueue_after_commit")
        .expect("commit_tick 里应有第 3 层接线点");
    let before = &runtime[..hook];
    let last_commit =
        before.rfind("tx.commit().await?;").expect("🔴 接线点之前找不到任何一次 tx.commit()");
    let last_begin = before.rfind(".begin().await?").unwrap_or(0);
    assert!(
        last_commit > last_begin,
        "🔴 第 3 层的接线点仍在一个未提交的事务里 —— 网络调用进事务 = 单连接池死锁 PoolTimedOut"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑩ 接线点本身
// ═══════════════════════════════════════════════════════════════════════════

/// 开着时才入队；载荷只带坐标（不夹带正文）。
#[tokio::test]
async fn enqueue_after_commit_is_gated_by_the_flag() {
    let state = test_state().await;
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_WORLD, "w1").await;

    enqueue_after_commit(&state, "w1", 12).await;
    let j = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        crate::queue::pop_json::<RecheckJob>(&*state.queue, TOPIC),
    )
    .await
    .expect("开着时应当入队")
    .expect("载荷可解析");
    assert_eq!(j.world_id, "w1");
    assert_eq!(j.tick_no, 12);
    assert_eq!(j.attempt, 1);
    assert!(j.retry_ids.is_empty());

    // 另一个世界没开灰度 → 不入队（按世界灰度是这条链路的开闸单位）。
    seed_world(&state.db, "w2", 0, "running").await;
    enqueue_after_commit(&state, "w2", 1).await;
    let none = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        crate::queue::pop_json::<RecheckJob>(&*state.queue, TOPIC),
    )
    .await;
    assert!(none.is_err(), "🔴 未开灰度的世界不得入队");
}

/// 参数化自检（§0.2）：每个产品规则都有 env 名，且默认值保守。
#[test]
fn every_product_rule_is_a_parameter_with_a_conservative_default() {
    for name in [
        ENV_SEMANTIC_RECHECK,
        ENV_PUBLIC_SAMPLE_BP,
        ENV_PRIVATE_SAMPLE_BP,
        ENV_TIMEOUT_MS,
        ENV_MAX_ATTEMPTS,
        ENV_BACKOFF_MS,
        ENV_MAX_EVENTS,
        ENV_WORKERS,
    ] {
        assert!(name.starts_with("MUSE_"), "{name} 应以 MUSE_ 开头");
    }
    assert!(!DEFAULT_SEMANTIC_RECHECK_ENABLED, "未验证功能默认关闭");
    assert!(DEFAULT_MAX_EVENTS > 0, "单拍送审上限是成本闸，必须有限");
    assert!(DEFAULT_TIMEOUT_MS > 0 && DEFAULT_TIMEOUT_MS <= 60_000, "超时要有界且不至于卡死 worker");
    assert!(DEFAULT_MAX_ATTEMPTS >= 2, "至少给一次重试，否则 provider 抖一下就大面积 fail-closed");
    assert_eq!(DEFAULT_WORKERS, 1, "并发度默认最保守（它就是审核成本的闸）");
    assert_eq!(max_events().min(1), 1, "配成 0/负数不得变成「一条都不查」");
    assert_eq!(timeout_ms().max(1), timeout_ms());
}

/// 单拍送审条数上限真的在拦（成本闸不是摆设）。
#[tokio::test]
async fn per_tick_batch_is_capped() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::verdict(ModerationVerdict::Approved);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;

    let cap = max_events();
    let n = (cap + 5) as usize;
    let evts: Vec<_> = (0..n).map(|i| pe(&format!("de-{i}"), "正文", true)).collect();
    seed_tick(&state, "w1", 0, &evts).await;

    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(r.public_candidates, cap, "🔴 超出上限的部分不得进入本次送审");
    assert_eq!(provider.calls() as i64, cap);
}

/// 空文本事件不送审（省掉一次无意义的模型调用）。
#[tokio::test]
async fn blank_events_are_not_sent_for_review() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::verdict(ModerationVerdict::Rejected);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-blank", "   ", true)]).await;

    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(r.outcome, OUTCOME_SKIPPED);
    assert_eq!(provider.calls(), 0);
    let m: String = sqlx::query_scalar("SELECT moderation FROM world_events WHERE domain_event_id='de-blank'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(m, "approved", "空文本不该被 fail-closed 掉");
}

/// 送审文本覆盖 summary **与** arbiter_note —— 两层看的字段必须是同一套，
/// 否则会出现「词库查了但语义没查」的字段级缺口。
#[test]
fn composed_text_covers_the_same_fields_as_layer_two() {
    let public = Some(r#"{"summary":"公开摘要"}"#);
    let private = Some(r#"[{"audiencePrincipalIds":["u1"],"summary":"私有摘要"}]"#);
    let note = Some("仲裁备注");
    let t = compose_text(public, private, note);
    assert!(t.contains("公开摘要") && t.contains("私有摘要") && t.contains("仲裁备注"), "{t}");
    assert_eq!(compose_text(None, None, None), "");
    assert_eq!(compose_text(None, None, Some("   ")), "", "空白备注不算内容");
}
