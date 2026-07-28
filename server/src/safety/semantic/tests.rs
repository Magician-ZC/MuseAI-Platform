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
    /// 每次 `check_text` 固定睡这么久（0 = 不睡）。
    delay_ms: u64,
}

impl ScriptedModeration {
    fn verdict(v: ModerationVerdict) -> Arc<Self> {
        Arc::new(Self { verdict: v, fail: false, stub: true, calls: AtomicUsize::new(0), delay_ms: 0 })
    }
    /// 每次调用**慢且失败**。用来断言「报错的调用照算耗时」——
    /// 用不慢的失败 provider 是断言不出来的（耗时本就 ~0，算不算都一样）。
    fn failing_slow(delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            verdict: ModerationVerdict::Approved,
            fail: true,
            stub: false,
            calls: AtomicUsize::new(0),
            delay_ms,
        })
    }
    fn failing() -> Arc<Self> {
        Arc::new(Self {
            verdict: ModerationVerdict::Approved,
            fail: true,
            stub: true,
            calls: AtomicUsize::new(0),
            delay_ms: 0,
        })
    }
    /// 「已接真实服务商」的形状：唯一区别是显式覆写 `is_dev_stub() == false`。
    fn production(v: ModerationVerdict) -> Arc<Self> {
        Arc::new(Self { verdict: v, fail: false, stub: false, calls: AtomicUsize::new(0), delay_ms: 0 })
    }
    /// 每次调用固定慢 `delay_ms`。用来断言 `provider_ms` 量的确实是 **provider 那段**——
    /// 桩的耗时恒为 ~0，用它是断言不出「计时范围对不对」的。
    fn slow(delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            verdict: ModerationVerdict::Approved,
            fail: false,
            stub: false,
            calls: AtomicUsize::new(0),
            delay_ms,
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ModerationProvider for ScriptedModeration {
    async fn check_text(&self, _text: &str) -> Result<ModerationVerdict, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }
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
/// 🔴 **绝不进事务**：本模块从不 `begin()`，也不接受 `Transaction` 参数。
/// 理由是 `check_text` 是网络调用——单连接池下事务持有唯一连接，调用期间再借连接必死锁。
///
/// 补偿轮询（`sweep.rs`）一并扫：它跑的是跨世界的扫描 + 队列写入，若把那一整段包进事务，
/// 单连接池下同样会在「扫描期间任何再借连接的操作」上死锁——而它是后台循环，
/// 死锁的表现是**悄悄不再补投**，比在请求路径上超时更难发现。
#[test]
fn red_line_never_opens_a_transaction() {
    for (file, code) in [
        ("mod.rs", strip_comments(include_str!("mod.rs"))),
        ("sweep.rs", strip_comments(include_str!("sweep.rs"))),
    ] {
        for forbidden in [".begin()", "Transaction<", "sqlx::Transaction"] {
            assert!(
                !code.contains(forbidden),
                "🔴 第 3 层（{file}）出现了事务用法「{forbidden}」——网络调用进事务 = 单连接池死锁 PoolTimedOut"
            );
        }
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

/// 🔴 **事实表只增不删**（§0.3 公共事实不可回滚）——上面那条棘轮的**另一半**。
///
/// 那条把 `UPDATE world_events` 逐条钉死了形状（正文零改写 / 单向棘轮 / 一条带守卫的放宽），
/// 但**删除这一支全仓没有任何红线**。而「回滚公共事实」最硬、最彻底的形态恰恰是删除：
/// 改一列还留着痕迹，删一行连痕迹都没有。
///
/// 本仓现有的处置手段全都是**状态标记**而非删除（内容处置写 `content_takedowns` +
/// 收紧 `moderation`；被拦事件仍落库留痕供人审/申诉，见 `clips` 那条用例）。
/// 也就是说这条不变式今天成立——本用例只是把它从「大家都这么做」变成「不这么做就红」。
///
/// 覆盖的是**只增不删的事实与台账**，不含那些删除本就合法的表
/// （幂等键、通知 outbox、运行时开关等有生命周期的行）。
#[test]
fn red_line_facts_are_never_deleted() {
    /// 只增不删：世界事实、拍台账、if 线拍、戏份账本、复式账本分录与流水、审计与风控留痕。
    const FACT_TABLES: &[&str] = &[
        "world_events",
        "world_ticks",
        "ifline_beats",
        "world_contributions",
        "ledger_postings",
        "ledger_journals",
        "audit_logs",
        "risk_events",
    ];

    let sources = production_sources();
    let mut offenders: Vec<String> = Vec::new();
    for (file, code) in &sources {
        for t in FACT_TABLES {
            if code.contains(&format!("DELETE FROM {t}")) {
                offenders.push(format!("{file} → DELETE FROM {t}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "🔴 事实表上出现了删除：{offenders:?}\n\
         §0.3「公共事实不可回滚」：改一列还留着痕迹，删一行连痕迹都没有。\n\
         本仓的处置手段一律是状态标记（`content_takedowns` + 收紧 `moderation`），被拦内容也留痕供申诉。\n\
         若确有合法的删除需求（如法定删除权），它需要**单独评审**并在此显式登记，不要只把断言改绿。"
    );
    // 扫描器失效 = 红线静默失效，故同时钉住「确实扫到了这些表」。
    let mentioned = FACT_TABLES
        .iter()
        .filter(|t| sources.iter().any(|(_, c)| c.contains(&format!("FROM {t}")) || c.contains(&format!("INTO {t}"))))
        .count();
    assert_eq!(
        mentioned,
        FACT_TABLES.len(),
        "🔴 有事实表在生产码里一次都没被扫到——要么表改名了，要么扫描器坏了（后者更危险）"
    );
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

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ⑩ 补偿轮询（`sweep`）：把内存队列丢掉的拍从**数据**里重新算出来
//
// 覆盖顺序按「写错了代价多大」排：
//   · 🔴 续号而不是重号 —— 写错只在**账单**上显形（每轮重烧一遍整拍）
//   · 🔴 无候选也要留终局行 —— 不留就永不收敛，同样只在账单上显形
//   · 三道边界（grace / lookback / batch）各自真的挡住了什么
//   · 覆盖上限被**如实量出来**（justOutsideWindow），而不是被掩盖
//   · 源码级：轮询对 world_events / world_ticks 只有 SELECT
// ═══════════════════════════════════════════════════════════════════════════

use super::sweep;

const MIN_MS: i64 = 60 * 1000;

/// 造一行「已落定」的 `world_ticks`（轮询的扫描面）。
async fn seed_done_tick(db: &AnyPool, world: &str, tick: i64, finished_at: i64) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, \
         started_at, finished_at, created_at) VALUES ($1, $2, $3, 0, 'done', 0, $4, $5, $6)",
    )
    .bind(new_id("wt"))
    .bind(world)
    .bind(tick)
    .bind(finished_at)
    .bind(finished_at)
    .bind(finished_at)
    .execute(db)
    .await
    .expect("seed world_ticks");
}

/// 造一行复核台账（轮询的判据面）。其余列走 DEFAULT —— 判据只看 outcome / attempt / created_at。
async fn seed_run_row(
    db: &AnyPool,
    world: &str,
    tick: i64,
    attempt: i64,
    outcome: &str,
    created_at: i64,
) {
    sqlx::query(
        "INSERT INTO safety_recheck_runs (id, world_id, tick_no, attempt, outcome, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(new_id("srr"))
    .bind(world)
    .bind(tick)
    .bind(attempt)
    .bind(outcome)
    .bind(created_at)
    .execute(db)
    .await
    .expect("seed safety_recheck_runs");
}

/// 一个「已落定 10 分钟、带一条 approved 公开事件、台账上一片空白」的拍 —— 缺口的标准形状。
async fn seed_lost_tick(state: &AppState, world: &str, tick: i64) {
    seed_tick(state, world, tick, &[pe(&format!("de-{world}-{tick}"), "甲", true)]).await;
    seed_done_tick(&state.db, world, tick, now_ms() - 10 * MIN_MS).await;
}

async fn run_sweep(state: &AppState) -> sweep::SweepReport {
    sweep::sweep_once(state, &mut sweep::InFlight::default()).await.expect("sweep")
}

/// 取一条补投的任务（超时即判为「根本没补投」，不挂死用例）。
async fn pop_job(state: &AppState) -> Option<RecheckJob> {
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        crate::queue::pop_json::<RecheckJob>(&*state.queue, TOPIC),
    )
    .await
    .ok()
    .flatten()
}

/// §0.1：轮询是这条链上唯一一处会「凭数据自发烧 token」的路径，默认必须是关的。
#[test]
fn sweep_flag_defaults_to_off() {
    assert!(
        !crate::flags::declared_default(sweep::ENV_SWEEP),
        "🔴 §0.1：补偿轮询不需要有人推进世界就能发起送审，默认开着等于让一次合并抬高成本曲线"
    );
}

/// 基本盘：队列把任务弄丢了（台账上一行都没有），轮询把这一拍算回来并补投。
#[tokio::test]
async fn sweep_requeues_a_tick_the_queue_lost() {
    let state = test_state().await;
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_lost_tick(&state, "w1", 7).await;

    let r = run_sweep(&state).await;
    assert_eq!((r.found, r.requeued), (1, 1), "{r:?}");

    let job = pop_job(&state).await.expect("🔴 补投的任务应当出现在复核队列里");
    assert_eq!((job.world_id.as_str(), job.tick_no), ("w1", 7));
    assert_eq!(job.attempt, 1, "台账为空 ⇒ 这是第 1 次尝试");
    assert!(job.retry_ids.is_empty(), "补偿路径重查整拍：那份 retry_ids 名单随丢失的任务一起没了");
}

/// 🔴 **续号，不是重号**。台账唯一键是 `(world_id, tick_no, attempt)` + `ON CONFLICT DO NOTHING`：
/// 补投若一律从 1 开始，跑完的结果会被冲突整行吞掉 → 台账永远没有终局行 →
/// 下一轮又当缺口补投 → **每隔一个 interval 重烧一遍整拍的 provider 调用**，
/// 而运营面上什么都看不出来（runs 不涨、缺口不降）。
#[tokio::test]
async fn sweep_resumes_the_attempt_chain_instead_of_restarting_at_one() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::verdict(ModerationVerdict::Approved);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_lost_tick(&state, "w1", 3).await;
    // 这条尝试链走到第 2 次就断在重试间隙（进程被重启带走了）。
    seed_run_row(&state.db, "w1", 3, 1, OUTCOME_RETRY, now_ms() - 12 * MIN_MS).await;
    seed_run_row(&state.db, "w1", 3, 2, OUTCOME_RETRY, now_ms() - 11 * MIN_MS).await;

    assert_eq!(run_sweep(&state).await.requeued, 1);
    let requeued = pop_job(&state).await.expect("应当补投");
    assert_eq!(requeued.attempt, 3, "🔴 必须接着 MAX(attempt) 往下走；重置成 1 会被唯一键吞掉结果");

    // ── 反证：若补投真用了 attempt=1，跑完也写不进台账 ────────────────────────
    let before = count(&state.db, "SELECT COUNT(*) FROM safety_recheck_runs").await;
    run_recheck(&state, &job("w1", 3, 1)).await.unwrap();
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM safety_recheck_runs").await,
        before,
        "🔴 重号的结果被 ON CONFLICT DO NOTHING 整行吞掉 —— 这正是「永不收敛地重烧 token」的机制"
    );
    assert_eq!(run_sweep(&state).await.found, 1, "重号跑完，缺口一点没少");

    // ── 续号跑完 ⇒ 留下终局行 ⇒ 缺口消失（收敛） ────────────────────────────
    run_recheck(&state, &job("w1", 3, 3)).await.unwrap();
    assert_eq!(run_sweep(&state).await.found, 0, "🔴 续号跑完后必须收敛");
}

/// 终局的三种 outcome 都算「查过了」，一律不再补投。
///
/// `skipped` 也是终局：它表达的是「看过了，这一拍没有可送审的内容」。把它排除在终局之外，
/// 会让正文全空的拍被无限重投。
#[tokio::test]
async fn sweep_ignores_ticks_that_already_have_a_terminal_run_row() {
    for outcome in [OUTCOME_DONE, OUTCOME_FAILED_CLOSED, OUTCOME_SKIPPED] {
        let state = test_state().await;
        seed_running_world(&state).await;
        enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
        seed_lost_tick(&state, "w1", 1).await;
        seed_run_row(&state.db, "w1", 1, 1, outcome, now_ms() - 9 * MIN_MS).await;

        assert_eq!(run_sweep(&state).await.found, 0, "🔴 outcome={outcome} 是终局，不该再被补投");
    }
}

/// 边界①（grace）：刚落定的拍留给正常路径自己跑完；台账上有 grace 以内的行也算「有人正在动它」。
#[tokio::test]
async fn sweep_leaves_alone_what_is_still_within_grace() {
    let state = test_state().await;
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;

    // ① 10 秒前刚落定 —— 正常路径多半还在队列里排着。
    seed_tick(&state, "w1", 1, &[pe("de-a", "甲", true)]).await;
    seed_done_tick(&state.db, "w1", 1, now_ms() - 10_000).await;
    // ② 落定够久，但台账上有一行刚写的 retry —— 有人正在重试，轮询不插手。
    seed_tick(&state, "w1", 2, &[pe("de-b", "乙", true)]).await;
    seed_done_tick(&state.db, "w1", 2, now_ms() - 10 * MIN_MS).await;
    seed_run_row(&state.db, "w1", 2, 1, OUTCOME_RETRY, now_ms() - 1_000).await;

    let r = run_sweep(&state).await;
    assert_eq!(r.found, 0, "🔴 与在飞任务重复送审 = 白烧一遍 token：{r:?}");
    assert!(pop_job(&state).await.is_none());
}

/// 边界②（lookback）：掉出回看窗口的拍**永远补不回来**——这是真实的覆盖上限。
/// 本用例同时钉住「它被如实量出来了」：`durability.justOutsideWindow > 0`
/// 就是「`MUSE_SAFETY_L3_SWEEP_LOOKBACK_MS` 配短了」的直接证据。
#[tokio::test]
async fn ticks_outside_the_lookback_window_are_lost_and_said_so() {
    let state = test_state().await;
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    // 默认 lookback 24h：30 小时前落定的拍已经掉出窗口。
    seed_tick(&state, "w1", 5, &[pe("de-old", "甲", true)]).await;
    seed_done_tick(&state.db, "w1", 5, now_ms() - 30 * 60 * MIN_MS).await;

    assert_eq!(run_sweep(&state).await.found, 0, "掉出窗口的拍不再被补投（这是上限，不是 bug）");

    let d = &sweep::gap_report(&state).await;
    assert_eq!(d["unresolvedInWindow"], 0);
    assert_eq!(
        d["justOutsideWindow"], 1,
        "🔴 覆盖上限必须被量出来，而不是悄悄吃掉：{d}"
    );
    let honesty = d["honesty"].as_array().expect("honesty[]");
    assert!(
        honesty.iter().any(|s| s.as_str().unwrap_or("").contains("永远补不回来")),
        "🔴 运营面必须直说这条链有补不回来的部分：{d}"
    );
}

/// 第 3 层对某个世界是关的 ⇒ 那个世界的拍不补投（不开就不查，本来就不该有复核行）。
/// 但它仍出现在 `unresolvedInWindow` 里 —— 两个数分开给，见 `gap_report` 的字段表。
#[tokio::test]
async fn sweep_skips_worlds_where_layer_three_is_off() {
    let state = test_state().await;
    seed_running_world(&state).await;
    seed_world(&state.db, "w2", 0, "running").await;
    // 只给 w1 开，w2 走全局默认（关）。
    enable_l3(&state.db, crate::flags::SCOPE_WORLD, "w1").await;
    seed_lost_tick(&state, "w1", 1).await;
    seed_lost_tick(&state, "w2", 1).await;

    let r = run_sweep(&state).await;
    assert_eq!((r.found, r.requeued, r.skipped_flag_off), (2, 1, 1), "{r:?}");
    let job = pop_job(&state).await.expect("w1 应当被补投");
    assert_eq!(job.world_id, "w1");
    assert!(pop_job(&state).await.is_none(), "🔴 关着第 3 层的世界不得被补投（那会绕开开关烧 token）");

    let d = sweep::gap_report(&state).await;
    assert_eq!(d["unresolvedInWindow"], 2, "缺口是事实：两拍都没有终局行");
    assert_eq!(d["enabledWorldTicks"], 1, "🔴 其中只有 1 拍是**真缺口**，另一拍是「压根没开」");
}

/// 补投后到台账落行之间有一段空窗（任务还在队列里排着）。若下一轮扫描落在这段空窗里，
/// 不做去重就会重复补投同一拍 —— 而重复的是**真实的 provider 调用**。
#[tokio::test]
async fn sweep_does_not_requeue_what_it_just_requeued() {
    let state = test_state().await;
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_lost_tick(&state, "w1", 1).await;

    // 同一个 InFlight 连扫两轮 = 同一个进程里两次相邻的扫描。
    let mut in_flight = sweep::InFlight::default();
    let first = sweep::sweep_once(&state, &mut in_flight).await.unwrap();
    let second = sweep::sweep_once(&state, &mut in_flight).await.unwrap();
    assert_eq!(first.requeued, 1);
    assert_eq!((second.found, second.requeued, second.skipped_in_flight), (1, 0, 1), "{second:?}");

    assert!(pop_job(&state).await.is_some());
    assert!(pop_job(&state).await.is_none(), "🔴 队列里只该有一份");
}

/// 🔴 收敛的前提：「看过了，这一拍没有可送审的内容」**要落账**。
///
/// 不落账时，一拍若有 approved 事件但正文全空，复核会一直 skip、一直不留行，
/// 于是每一轮扫描都把它当缺口重投一次，永不收敛。
#[tokio::test]
async fn a_tick_with_nothing_to_check_still_leaves_a_terminal_row() {
    let mut state = test_state().await;
    let provider = ScriptedModeration::verdict(ModerationVerdict::Rejected);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-blank", "   ", true)]).await;
    seed_done_tick(&state.db, "w1", 0, now_ms() - 10 * MIN_MS).await;

    assert_eq!(run_sweep(&state).await.requeued, 1, "台账为空 ⇒ 第一轮确实该补投它一次");

    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(r.outcome, OUTCOME_SKIPPED);
    assert_eq!(provider.calls(), 0, "🔴 无候选不得调用 provider");
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM safety_recheck_runs").await,
        1,
        "🔴 「看过了，没东西可看」必须入账"
    );
    let (chars, checked): (i64, i64) = sqlx::query(
        "SELECT chars_checked, public_checked FROM safety_recheck_runs",
    )
    .fetch_one(&state.db)
    .await
    .map(|r| (r.try_get("chars_checked").unwrap(), r.try_get("public_checked").unwrap()))
    .unwrap();
    assert_eq!((chars, checked), (0, 0), "这一行是零成本记录，不得污染 T5 成本口径");

    // 🔴 把这行台账的时间推到 grace 之外再扫。否则「下一轮找不到它」有两个可能的原因
    // （① skipped 算终局；② 那行是 grace 以内刚写的、被「有人正在动它」挡掉了），
    // 用例就证不出想证的那一个 —— 这是故障注入（把 skipped 移出终局集合）才暴露出来的弱点。
    sqlx::query("UPDATE safety_recheck_runs SET created_at = $1")
        .bind(now_ms() - 30 * MIN_MS)
        .execute(&state.db)
        .await
        .unwrap();
    assert_eq!(
        run_sweep(&state).await.found,
        0,
        "🔴 skipped 必须算终局：不算的话，正文全空的拍会被每一轮扫描无限重投"
    );
}

/// 开关关闭那一种 skip **仍然不落库** —— 与上一条的口径故意不同：
/// 关闭时的行为必须与接线前逐字节相同（`disabled_is_byte_identical_to_before_wiring`）。
#[tokio::test]
async fn the_disabled_skip_still_writes_nothing() {
    let state = test_state().await;
    seed_running_world(&state).await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "甲", true)]).await;

    assert_eq!(run_recheck(&state, &job("w1", 0, 1)).await.unwrap().outcome, OUTCOME_SKIPPED);
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM safety_recheck_runs").await,
        0,
        "🔴 开关关闭时不得留下任何痕迹（两种 skip 的落库口径是分开的）"
    );
}

/// 缺口报告不依赖轮询是否跑过 / 是否开着：数字从两张既有表现算。
/// 若它读的是轮询自己的计数器，轮询一死，数字就冻结在健康的样子，而缺口在背后继续长。
#[tokio::test]
async fn gap_report_is_computed_from_data_not_from_the_sweepers_own_bookkeeping() {
    let state = test_state().await;
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_lost_tick(&state, "w1", 1).await;
    seed_lost_tick(&state, "w1", 2).await;

    // 一次都没扫过，开关也没开 —— 缺口照样报得出来。
    let d = sweep::gap_report(&state).await;
    assert_eq!(d["sweepEnabled"], false, "轮询开关默认关闭");
    assert_eq!(d["unresolvedInWindow"], 2);
    assert_eq!(d["enabledWorldTicks"], 2);

    // 也进了运营面（一处口径，不是第二套数）。
    let app = build_router(state.clone());
    let (st, body) = get_json(&app, "/api/admin/safety/recheck", &admin_token(&state, "operator")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["durability"]["unresolvedInWindow"], 2, "{body}");
    assert_eq!(body["durability"]["flag"], sweep::ENV_SWEEP);
}

/// 🔴 源码级：轮询对 `world_events` / `world_ticks` **只有 SELECT**。
/// 它是一个「重新算待办」的旁路，一旦长出写路径，`world_events` 的写入盘点
/// （`red_line_world_events_has_one_ratchet_and_one_guarded_relax`）就会多出一条没评审过的。
#[test]
fn red_line_sweep_only_reads_the_world_tables() {
    let code = strip_comments(include_str!("sweep.rs"));
    for verb in ["UPDATE", "INSERT INTO", "DELETE FROM"] {
        for table in ["world_events", "world_ticks", "safety_recheck_runs"] {
            let forbidden = format!("{verb} {table}");
            assert!(
                !code.contains(&forbidden),
                "🔴 补偿轮询出现了写入语句「{forbidden}」——它只应当重新算待办，不该自己改状态"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 ⑪ provider 侧真实耗时（migration 0049）
//
// 补的是 `admin_api::worlds_ops` 那条 `TODO(数据源缺失)` 的数据源。要点不在"记了一个数"，
// 而在**记的是哪一段**：混进 DB 往返的近似值系统性偏大，却看起来完全合理——
// 比 `audit_queue.reviewed_at`（人审周转，量级差几个数量级）那种一眼假的数更难识破。
// ═══════════════════════════════════════════════════════════════════════════

/// `provider_ms` 量的是 **provider 那一段**，不是整次尝试。
#[tokio::test]
async fn provider_ms_measures_the_provider_call_not_the_whole_attempt() {
    const DELAY: i64 = 40;
    let mut state = test_state().await;
    let provider = ScriptedModeration::slow(DELAY as u64);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "甲", true), pe("de-2", "乙", true)]).await;

    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!(provider.calls(), 2);
    assert!(
        r.provider_ms >= 2 * DELAY - 10,
        "🔴 两次各慢 {DELAY}ms 的调用必须被计进去，实测 {}ms",
        r.provider_ms
    );

    let (total, prov): (i64, i64) = sqlx::query(
        "SELECT latency_ms, provider_ms FROM safety_recheck_runs WHERE world_id='w1' AND tick_no=0",
    )
    .fetch_one(&state.db)
    .await
    .map(|row| (row.try_get("latency_ms").unwrap(), row.try_get("provider_ms").unwrap()))
    .unwrap();
    assert_eq!(prov, r.provider_ms, "台账落的就是报告里的那个数");
    assert!(
        prov <= total,
        "🔴 provider 段不可能超过全程：provider={prov}ms total={total}ms —— 超了说明计时范围写错了"
    );
}

/// 🔴 **报错与超时的调用照算**。剔掉它们会让「provider 开始抖动」在延迟曲线上反而变好看，
/// 而那正是最该被曲线报出来的时刻。
#[tokio::test]
async fn failing_calls_still_count_toward_provider_latency() {
    const DELAY: i64 = 40;
    let mut state = test_state().await;
    // 🔴 必须用**慢且失败**的 provider：不慢的失败 provider 耗时本就 ~0，
    // 「算不算它」两种实现给出的数一样，用例会假绿（这条是故障注入实测出来的：
    // 注入「只给成功的调用计时」后，原先写成 `provider_ms >= 0` 的断言照样过）。
    let provider = ScriptedModeration::failing_slow(DELAY as u64);
    state.moderation = provider.clone();
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "甲", true), pe("de-2", "乙", true)]).await;

    let r = run_recheck(&state, &job("w1", 0, 1)).await.unwrap();
    assert_eq!((r.provider_errors, provider.calls()), (2, 2));
    assert!(
        r.provider_ms >= 2 * DELAY - 10,
        "🔴 两次各慢 {DELAY}ms 的**失败**调用必须照计，实测 {}ms —— 把失败调用剔出延迟统计，\
         会让「provider 开始抖动」在曲线上反而变好看，而那正是最该被曲线报出来的时刻",
        r.provider_ms
    );

    // ── 源码级不变式：计时范围由**类型**保证，不由纪律保证 ──────────────────
    //
    // ⚠️ 为什么这里必须补一条源码断言，而不是再写一个行为用例：把 `provider_ms` 写成
    // 「整次尝试的耗时」——最容易犯、也最像对的那种错——在**内存 SQLite** 上与正确实现
    // 差不到 1ms，任何基于时间的断言都分不开。（这条是故障注入实测出来的：注入该错误后，
    // 上面那个 `provider_ms_measures_...` 照样绿。）真正能分开它们的是范围本身。
    let code = strip_comments(include_str!("mod.rs"));
    assert!(
        code.contains("async fn check_with_timeout(") && code.contains(") -> (Result<ModerationVerdict, String>, i64)"),
        "🔴 耗时必须**随裁决一起**从 check_with_timeout 返回。在调用点外面掐表的写法，\
         日后有人往两条 now_ms() 之间插一句 DB 操作时，编译器不会说什么，而延迟曲线会悄悄偏大"
    );
    assert!(
        code.contains("r.provider_ms += call_ms;"),
        "🔴 provider_ms 只能从 check_with_timeout 回报的那个值累加"
    );
    assert!(
        !code.contains("r.provider_ms ="),
        "🔴 provider_ms 只许 `+=`，不许赋值。一旦出现赋值，最可能的形态就是\
         `r.provider_ms = now_ms() - started`（拿整次尝试冒充 provider 段）—— 那个数\
         系统性偏大却看起来完全合理，比一眼假的数更难识破"
    );
}

/// 运营面：桩的时候明说这个数**不可用于报警**；没调用过时给 `null` 而不是 `0`。
#[tokio::test]
async fn provider_latency_readout_refuses_to_look_like_a_fast_provider() {
    // ① 桩 + 有调用 —— 数出得来，但 usableForAlerting = false。
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::verdict(ModerationVerdict::Approved);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "甲", true)]).await;
    run_recheck(&state, &job("w1", 0, 1)).await.unwrap();

    let body = crate::safety::semantic::testkit::admin_recheck(&state).await;
    let pl = &body["providerLatency"];
    assert_eq!(pl["checks"], 1);
    assert_eq!(pl["usableForAlerting"], false, "🔴 桩下的耗时不得被当成可报警的 SLA：{pl}");
    assert!(
        pl["why"].as_str().unwrap_or("").contains("恒为 ~0"),
        "🔴 必须说清「恒 0」与「非常快」在看板上长得一样：{pl}"
    );

    // ② 一次调用都没有 —— avgMsPerCall 必须是 null，不是 0。
    let empty = test_state().await;
    let pl2 = crate::safety::semantic::testkit::admin_recheck(&empty).await;
    let pl2 = &pl2["providerLatency"];
    assert_eq!(pl2["checks"], 0);
    assert!(
        pl2["avgMsPerCall"].is_null(),
        "🔴 「没调用过」与「调用极快」必须分得开，给 0 就把两者混了：{pl2}"
    );
}

/// 真实 provider（`is_dev_stub() == false`）+ 有调用 ⇒ 这个数才开始可用于报警。
#[tokio::test]
async fn provider_latency_becomes_usable_once_the_stub_is_replaced() {
    let mut state = test_state().await;
    state.moderation = ScriptedModeration::slow(15);
    seed_running_world(&state).await;
    enable_l3(&state.db, crate::flags::SCOPE_GLOBAL, "").await;
    seed_tick(&state, "w1", 0, &[pe("de-1", "甲", true)]).await;
    run_recheck(&state, &job("w1", 0, 1)).await.unwrap();

    let body = crate::safety::semantic::testkit::admin_recheck(&state).await;
    let pl = &body["providerLatency"];
    assert_eq!(pl["usableForAlerting"], true, "{pl}");
    assert!(pl["avgMsPerCall"].as_i64().unwrap_or(0) >= 5, "{pl}");
    assert!(
        pl["why"].as_str().unwrap_or("").contains("只覆盖"),
        "🔴 就算可报警了，也要说清它只覆盖运行时投影这条链（静态审核不落本表）：{pl}"
    );
}
