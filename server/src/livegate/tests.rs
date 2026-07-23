//! livegate 集成测试（sqlite::memory + oneshot），#[cfg(all(test, feature="arena"))]。
//! 覆盖：礼物→arena_env_events(kind=gift_boon)、同 world+SKU 聚合合并、SKU 映射查表、未知 SKU、
//! 未知世界 404、签名校验（纯单元）、GET /arena/{worldId}/clips 列表。

use super::*;
use crate::safety::testkit::{count, seed_user, seed_world, test_state, token};
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
