//! livegate 集成测试（sqlite::memory + oneshot），#[cfg(all(test, feature="arena"))]。
//! 覆盖：礼物→arena_env_events(kind=gift_boon)、同 world+SKU 聚合合并、SKU 映射查表、未知 SKU、
//! 未知世界 404、签名校验（纯单元）、GET /arena/{worldId}/clips 列表。

use super::*;
use crate::safety::testkit::{count, seed_member, seed_user, seed_world, test_state, token};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// test_state + 容错建 arena_env_events（P6a 的 0007 并发期可能未落地；IF NOT EXISTS 落地后自动无操作）。
async fn arena_state() -> AppState {
    let state = test_state().await;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS arena_env_events (\
         id TEXT PRIMARY KEY, world_id TEXT NOT NULL, applied_tick INTEGER, \
         kind TEXT NOT NULL, payload_json TEXT NOT NULL, \
         aggregated_count INTEGER NOT NULL DEFAULT 1, created_at BIGINT NOT NULL)",
    )
    .execute(&state.db)
    .await
    .expect("create arena_env_events");
    state
}

async fn post_webhook(state: &AppState, body: Value) -> (StatusCode, Value) {
    let app = crate::app::build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/livegate/webhook")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

#[tokio::test]
async fn webhook_gift_writes_env_event() {
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;

    let (status, v) =
        post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 1})).await;
    assert_eq!(status, StatusCode::OK, "body={v}");
    assert_eq!(v["mapped"], true);

    // 写入 arena_env_events 专用通道，kind='gift_boon'。
    let n = count(
        &state.db,
        "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(n, 1, "礼物应写一条 gift_boon 到 arena_env_events");
    // 未写玩家 interventions（礼物是系统代投，不走玩家干预通道）。
    let ivs = count(&state.db, "SELECT COUNT(*) FROM interventions").await;
    assert_eq!(ivs, 0, "礼物不得进玩家 interventions");
    // gift_events 记账一条。
    let g = count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1' AND mapped=1").await;
    assert_eq!(g, 1);
}

#[tokio::test]
async fn same_sku_aggregates_count() {
    // 同 world+SKU 聚合：两次礼物合并为一行 gift_boon，aggregated_count 累加计数。
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;

    post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 3})).await;
    let (_s, v) = post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 2})).await;

    // 只有一行（防事件风暴），聚合计数 = 3 + 2。
    let rows = count(
        &state.db,
        "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(rows, 1, "同 world+SKU 应聚合为一行");
    assert_eq!(v["aggregatedCount"], 5, "aggregated_count 应累加");
    let agg = count(
        &state.db,
        "SELECT aggregated_count FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(agg, 5);
    // gift_events 仍逐笔记账（两条），聚合只发生在环境事件通道。
    let g = count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1'").await;
    assert_eq!(g, 2, "gift_events 应逐笔记账");
}

#[tokio::test]
async fn different_sku_separate_rows() {
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;
    post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 1})).await;
    post_webhook(&state, json!({"worldId": "w1", "giftSku": "rocket", "count": 1})).await;
    let rows = count(
        &state.db,
        "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(rows, 2, "不同 SKU 不应聚合");
}

#[tokio::test]
async fn sku_mapping_resolved() {
    // SKU→boon 映射查表：命中的 boon 进响应与 arena_env_events payload。
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;
    let (status, v) =
        post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 1})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["boon"]["effectTag"], "advantage:combat", "rose 应映射到过程增益 advantage:combat");
    assert_eq!(v["boon"]["kind"], "env_prop");

    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    let p: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(p["sku"], "rose");
    assert_eq!(p["boon"]["effectTag"], "advantage:combat");
}

#[tokio::test]
async fn unknown_sku_graceful() {
    // 未知 SKU：优雅处理——200 mapped=false，不写 arena_env_events，仍记 gift_events 账（对账用）。
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;
    let (status, v) =
        post_webhook(&state, json!({"worldId": "w1", "giftSku": "nonesuch", "count": 1})).await;
    assert_eq!(status, StatusCode::OK, "未知 SKU 不应报错");
    assert_eq!(v["mapped"], false);
    assert_eq!(v["boon"], Value::Null);

    let env = count(&state.db, "SELECT COUNT(*) FROM arena_env_events").await;
    assert_eq!(env, 0, "未知 SKU 不写环境事件");
    let g = count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE mapped=0").await;
    assert_eq!(g, 1, "未知 SKU 仍逐笔记账");
}

#[tokio::test]
async fn unknown_world_not_found() {
    let state = arena_state().await;
    let (status, _v) =
        post_webhook(&state, json!({"worldId": "ghost", "giftSku": "rose", "count": 1})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn signature_matches_unit() {
    // 签名校验核心（生产开关关闭 dev 时启用）：正确签名通过，错误签名/篡改 body 拒绝。
    let secret = "s3cr3t";
    let body = br#"{"worldId":"w1","giftSku":"rose","count":1}"#;
    let good = sign_body(secret, body);
    assert!(signature_matches(secret, &good, body));
    assert!(!signature_matches(secret, "deadbeef", body));
    assert!(!signature_matches(secret, &good, b"tampered"), "篡改 body 应验签失败");
}

#[tokio::test]
async fn list_clips_route_returns_generated() {
    // GET /arena/{worldId}/clips：生成一条切片后列表可读（观战资格经 can_view_world）。
    let state = arena_state().await;
    seed_user(&state.db, "u1").await;
    seed_world(&state.db, "w1", 0, "running").await; // official → 可观战
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, public_projection_json, occurred_at) \
         VALUES ('e1','w1',0,1,'de1','action','[\"c1\"]','public','{\"summary\":\"高光\"}', 0)",
    )
    .execute(&state.db)
    .await
    .unwrap();
    crate::clips::generate_clip(&state, "w1", "e1").await.expect("generate_clip");

    let app = crate::app::build_router(state.clone());
    let tk = token(&state, "u1");
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/arena/w1/clips")
                .header("authorization", format!("Bearer {tk}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["clips"].as_array().unwrap().len(), 1, "列表应含一条切片");
    assert_eq!(v["clips"][0]["eventId"], "e1");
}

// ---------- POST /arena/{worldId}/gift：站内观众打赏（AuthUser + 观战资格 + via=in_app + 进流） ----------

/// 带鉴权（可选 Idempotency-Key）的站内打赏请求。
async fn post_gift(
    state: &AppState,
    world: &str,
    user: &str,
    body: Value,
    idem: Option<&str>,
) -> (StatusCode, Value) {
    let tk = token(state, user);
    let app = crate::app::build_router(state.clone());
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/arena/{world}/gift"))
        .header("authorization", format!("Bearer {tk}"))
        .header("content-type", "application/json");
    if let Some(k) = idem {
        builder = builder.header("idempotency-key", k);
    }
    let resp = app.oneshot(builder.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

#[tokio::test]
async fn spectator_gift_maps_to_env_and_stream() {
    let state = arena_state().await;
    seed_user(&state.db, "viewer").await;
    seed_world(&state.db, "w1", 0, "running").await; // official → 可观战

    let (s, v) = post_gift(&state, "w1", "viewer", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["mapped"], true);
    assert_eq!(v["boundary"]["notImmunity"], true);
    assert_eq!(v["boundary"]["notFinalVerdict"], true);

    // 与 webhook 同一系统频道：arena_env_events(gift_boon)。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'").await,
        1
    );
    // gift_events 记账且 via='in_app'（区分站内来源，供分成）。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1' AND via='in_app'").await,
        1
    );
    // 打赏进 public 流：arena_gift（audience NULL → 双硬隔离天然满足）。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w1' AND event_type='arena_gift' AND visibility='public' AND audience_json IS NULL").await,
        1
    );
    // 红线：不写玩家 interventions。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM interventions").await, 0);
}

#[tokio::test]
async fn spectator_gift_unmapped_sku_no_boon() {
    let state = arena_state().await;
    seed_user(&state.db, "viewer").await;
    seed_world(&state.db, "w1", 0, "running").await;

    let (s, v) = post_gift(&state, "w1", "viewer", json!({ "sku": "nonesuch", "count": 1 }), None).await;
    assert_eq!(s, StatusCode::OK, "未映射 SKU 不应报错");
    assert_eq!(v["mapped"], false);
    assert_eq!(v["boon"], Value::Null);

    // 未映射：不写 env、不进流；仍逐笔记 gift_events(via=in_app)（对账用，对齐 webhook 语义）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_env_events").await, 0, "未映射不写环境事件");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_events WHERE event_type='arena_gift'").await, 0, "未映射不进流");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE mapped=0 AND via='in_app'").await, 1);
}

#[tokio::test]
async fn spectator_gift_requires_view_permission() {
    let state = arena_state().await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "stranger").await;
    seed_world(&state.db, "w1", 0, "running").await;
    // 收敛为 private：仅成员/房主可观战 → 可打赏。
    sqlx::query("UPDATE worlds SET visibility='private' WHERE id='w1'").execute(&state.db).await.unwrap();
    seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;

    // 非成员 → 403（守卫挡在 apply_gift 之前，绝不记账）。
    let (s1, _) = post_gift(&state, "w1", "stranger", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s1, StatusCode::FORBIDDEN);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events").await, 0, "被拒绝的打赏不得记账");

    // 成员 → 200。
    let (s2, v2) = post_gift(&state, "w1", "u1", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s2, StatusCode::OK, "body={v2}");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE via='in_app'").await, 1);
}

#[tokio::test]
async fn spectator_gift_idempotent() {
    let state = arena_state().await;
    seed_user(&state.db, "viewer").await;
    seed_world(&state.db, "w1", 0, "running").await;

    // 同 Idempotency-Key 重投两次 → 第二次返回缓存，计数不翻倍。
    let (s1, v1) = post_gift(&state, "w1", "viewer", json!({ "sku": "rose", "count": 3 }), Some("k-1")).await;
    assert_eq!(s1, StatusCode::OK, "body={v1}");
    let (s2, v2) = post_gift(&state, "w1", "viewer", json!({ "sku": "rose", "count": 3 }), Some("k-1")).await;
    assert_eq!(s2, StatusCode::OK, "body={v2}");

    // gift_events 只一笔（幂等），聚合计数 = 3（非 6）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1'").await, 1, "幂等重投不得重复记账");
    assert_eq!(
        count(&state.db, "SELECT aggregated_count FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'").await,
        3,
        "幂等重投聚合计数不得翻倍"
    );
    // 进流也只一条 arena_gift。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w1' AND event_type='arena_gift'").await, 1);
}
