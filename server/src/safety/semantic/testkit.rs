//! 第 3 层复核的测试脚手架（仅 test 构建）。
//!
//! 抽出来是因为**第二个模块要用它**：`providers::http_moderation::tests` 需要跑一遍
//! 「真实 provider → 第 3 层 → 台账 → 运营面」的完整链路，才能证明 `is_dev_stub()` 翻面
//! 与成本比值这两件事是端到端成立的，而不只是 provider 自己的返回值对。
//!
//! ⚠️ 这里的播种口径必须与 `semantic::tests` 里的一致（同一批表、同一个世界 id），
//! 否则两边的用例会各自建一套「差不多」的夹具，然后在某次 schema 变更时只坏一边。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::AnyPool;
use tower::ServiceExt;

use crate::app::AppState;
use crate::db::{new_id, now_ms};
use crate::events::{persist_events, ProjectedEvent, MODERATION_APPROVED};

pub(crate) use super::RecheckJob;

/// T5 门槛（万分比整数）。给别的模块的用例断言用，避免它们各自抄一个 500。
pub(crate) fn threshold_bp() -> i64 {
    super::COST_THRESHOLD_BP
}

/// 「每 1000 单位 × 单价（分）」→ 分。转出来供别的模块的用例断言整数口径。
pub(crate) fn cost_cents(units: i64, price_per_1k: i64) -> i64 {
    super::cost_cents(units, price_per_1k)
}

pub(crate) fn job(world: &str, tick: i64, attempt: i64) -> RecheckJob {
    RecheckJob { world_id: world.into(), tick_no: tick, attempt, retry_ids: Vec::new() }
}

/// 一个 running 的世界 `w1` + 成员 `u1`/`c1`。
pub(crate) async fn seed_running_world(state: &AppState) {
    crate::safety::testkit::seed_user(&state.db, "u1").await;
    crate::safety::testkit::seed_world(&state.db, "w1", 0, "running").await;
    crate::safety::testkit::seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
}

/// 全局打开第 3 层开关。**写 `runtime_flags` 而不是设 env**——env 是进程级的，
/// 设了会与并发用例互踩。
pub(crate) async fn enable(db: &AnyPool) {
    sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) \
         VALUES ($1, $2, $3, $4, 1, 0, 0, 'test', $5, 'test', $6)",
    )
    .bind(new_id("rf"))
    .bind(super::ENV_SEMANTIC_RECHECK)
    .bind(crate::flags::SCOPE_GLOBAL)
    .bind("")
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .expect("enable flag");
}

/// 往一拍里塞一条公开投影事件（`moderation='approved'`，即第 3 层的候选形状）。
pub(crate) async fn seed_tick(state: &AppState, world: &str, tick: i64, summary: &str) {
    let e = ProjectedEvent {
        domain_event_id: format!("de-{tick}"),
        event_type: "dialogue".into(),
        actor_ids: vec!["c1".into()],
        visibility: "public".into(),
        audience_user_ids: Vec::new(),
        summary: summary.into(),
        arbiter_note: None,
        moderation: MODERATION_APPROVED.into(),
    };
    persist_events(&state.db, world, tick, &[e]).await.expect("seed events");
}

/// `GET /api/admin/safety/recheck` 的响应体（operator 档）。
pub(crate) async fn admin_recheck(state: &AppState) -> Value {
    let tok = crate::auth::issue_access(&state.config.jwt_secret, "adm1", "operator", 3600)
        .expect("issue token");
    let resp = crate::app::build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/safety/recheck")
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "运营面应当可读");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}
