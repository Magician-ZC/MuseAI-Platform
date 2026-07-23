//! 角色资产上云端到端测试（复用 auth 测试 helper）。

use axum::http::StatusCode;
use serde_json::json;

use crate::auth::tests::{build_app, login_new_user, send};

fn sample_card(name: &str) -> serde_json::Value {
    json!({
        "schemaVersion": 2,
        "id": "local-card",
        "identity": { "name": name },
        "dramaticCore": { "coreContradiction": "忠诚与自由" }
    })
}

fn publish_body(local_card_id: &str, name: &str) -> serde_json::Value {
    json!({
        "localCardId": local_card_id,
        "cardJson": sample_card(name),
        "rightsDeclaration": "original"
    })
}

#[tokio::test]
async fn publish_assigns_server_version_and_moderation() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000001").await;
    // 客户端伪造 version/moderation —— 服务端必须忽略（§9.6 铁律）。
    let body = json!({
        "localCardId": "card-A",
        "cardJson": sample_card("孙悟空"),
        "rightsDeclaration": "original",
        "version": 999,
        "moderation": "approved"
    });
    let (st, v) = send(&app, "POST", "/api/assets/characters", Some(&access), Some("k1"), Some(body)).await;
    assert_eq!(st, StatusCode::OK, "{v:?}");
    assert_eq!(v["version"], 1, "服务端从 1 递增，忽略客户端 999");
    assert_eq!(v["moderation"], "approved", "当前机审 stub 直过");
    assert_eq!(v["withdrawn"], false);
}

#[tokio::test]
async fn publish_increments_version_per_local_card() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000002").await;
    let (_st, v1) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("v1"), Some(publish_body("card-B", "A"))).await;
    let (_st, v2) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("v2"), Some(publish_body("card-B", "A"))).await;
    assert_eq!(v1["version"], 1);
    assert_eq!(v2["version"], 2, "同 owner+localCardId 版本号服务端递增");
}

#[tokio::test]
async fn publish_requires_auth() {
    let (app, _s) = build_app().await;
    let (st, _) = send(&app, "POST", "/api/assets/characters", None, None, Some(publish_body("x", "A"))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn publish_rejects_bad_rights() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000009").await;
    let body = json!({ "localCardId": "x", "cardJson": sample_card("A"), "rightsDeclaration": "stolen" });
    let (st, _) = send(&app, "POST", "/api/assets/characters", Some(&access), None, Some(body)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mine_and_status_owner_scoped() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000003").await;
    let (_st, v) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("m1"), Some(publish_body("card-C", "X"))).await;
    let id = v["id"].as_str().unwrap().to_string();

    let (st, mine) = send(&app, "GET", "/api/assets/characters/mine", Some(&access), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(mine.as_array().unwrap().len(), 1);

    let (st, s) = send(&app, "GET", &format!("/api/assets/characters/{id}/status"), Some(&access), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s["moderation"], "approved");
    assert_eq!(s["version"], 1);

    // 他人访问 → 404（owner 硬隔离，不泄露存在性）。
    let (access2, _r, _u) = login_new_user(&app, "13900000099").await;
    let (st, _) = send(&app, "GET", &format!("/api/assets/characters/{id}/status"), Some(&access2), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_st, mine2) = send(&app, "GET", "/api/assets/characters/mine", Some(&access2), None, None).await;
    assert_eq!(mine2.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn withdraw_is_idempotent() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000004").await;
    let (_st, v) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("w1"), Some(publish_body("card-D", "Y"))).await;
    let id = v["id"].as_str().unwrap().to_string();

    let (st1, r1) = send(&app, "POST", &format!("/api/assets/characters/{id}/withdraw"), Some(&access), None, None).await;
    assert_eq!(st1, StatusCode::OK);
    assert_eq!(r1["withdrawn"], true);
    // 再次撤回（无幂等键也应自然幂等）。
    let (st2, r2) = send(&app, "POST", &format!("/api/assets/characters/{id}/withdraw"), Some(&access), None, None).await;
    assert_eq!(st2, StatusCode::OK);
    assert_eq!(r2["withdrawn"], true);

    let (_st, s) = send(&app, "GET", &format!("/api/assets/characters/{id}/status"), Some(&access), None, None).await;
    assert_eq!(s["withdrawn"], true);
}

#[tokio::test]
async fn delete_unplaced_is_immediate() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000005").await;
    let (_st, v) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("d1"), Some(publish_body("card-E", "Z"))).await;
    let id = v["id"].as_str().unwrap().to_string();

    let (st, r) = send(&app, "DELETE", &format!("/api/assets/characters/{id}"), Some(&access), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r["scope"], "immediate");
    assert_eq!(r["status"], "done");

    // 已删除 → status 404。
    let (st, _) = send(&app, "GET", &format!("/api/assets/characters/{id}/status"), Some(&access), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_placed_is_deferred() {
    let (app, state) = build_app().await;
    let (access, _r, uid) = login_new_user(&app, "13900000006").await;
    let (_st, v) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("d2"), Some(publish_body("card-F", "W"))).await;
    let id = v["id"].as_str().unwrap().to_string();

    // 种一条投放关系（world_members），模拟已投放。
    sqlx::query("INSERT INTO world_members (id, world_id, user_id, cloud_character_id, joined_at) VALUES (?, 'w_test', ?, ?, ?)")
        .bind(crate::db::new_id("wm"))
        .bind(&uid)
        .bind(&id)
        .bind(crate::db::now_ms())
        .execute(&state.db)
        .await
        .unwrap();

    let (st, r) = send(&app, "DELETE", &format!("/api/assets/characters/{id}"), Some(&access), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r["scope"], "deferred");
    assert_eq!(r["status"], "pending");

    // 已投放不立删（快照仍在），但停止后续投放（withdrawn=1）。
    let (st, s) = send(&app, "GET", &format!("/api/assets/characters/{id}/status"), Some(&access), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s["withdrawn"], true);
}

#[tokio::test]
async fn publish_idempotency_key_returns_cached() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000007").await;
    let body = publish_body("card-G", "Q");
    let (_st, a) = send(&app, "POST", "/api/assets/characters", Some(&access), Some("same"), Some(body.clone())).await;
    let (st, b) = send(&app, "POST", "/api/assets/characters", Some(&access), Some("same"), Some(body)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(a["id"], b["id"], "同键同载荷 → 同一响应");
    assert_eq!(a["version"], b["version"]);
    // 未重复发布 → /mine 只有一条。
    let (_st, mine) = send(&app, "GET", "/api/assets/characters/mine", Some(&access), None, None).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn publish_idempotency_mismatch_conflicts() {
    let (app, _s) = build_app().await;
    let (access, _r, _u) = login_new_user(&app, "13900000008").await;
    let (_st, _v) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("mk"), Some(publish_body("card-H", "A"))).await;
    // 同键异载荷 → 409。
    let (st, _v) =
        send(&app, "POST", "/api/assets/characters", Some(&access), Some("mk"), Some(publish_body("card-H2", "B"))).await;
    assert_eq!(st, StatusCode::CONFLICT);
}

#[test]
fn decide_moderation_maps_all_verdicts() {
    use super::decide_moderation;
    use crate::providers::ModerationVerdict::{Approved, Pending, Rejected};
    use crate::safety::InjectionHit;

    assert_eq!(decide_moderation(Approved, &[]), "approved");
    assert_eq!(decide_moderation(Pending, &[]), "pending");
    assert_eq!(decide_moderation(Rejected, &[]), "rejected");
    let hit = vec![InjectionHit { rule: "cmd".into(), excerpt: "忽略以上".into() }];
    assert_eq!(decide_moderation(Approved, &hit), "pending", "注入命中强制转人审");
}
