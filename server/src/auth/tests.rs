//! 账号鉴权端到端测试（sqlx sqlite::memory + axum oneshot）。
//! 共享 helper（build_app / send / login_new_user）供 assets 集成测试复用。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::app::AppState;
use crate::config::ServerConfig;

/// 构建一个隔离的内存态 app（每个测试独立库）。返回 (router, state) 以便直接种数据。
pub(crate) async fn build_app() -> (Router, AppState) {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(sqlx::any::install_default_drivers);

    let pool = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .min_connections(1) // 保活单个内存连接，避免 :memory: 库被回收
        .connect("sqlite::memory:")
        .await
        .expect("connect sqlite memory");
    sqlx::migrate!("./migrations").run(&pool).await.expect("run migrations");

    let config = ServerConfig {
        database_url: "sqlite::memory:".into(),
        bind_addr: "127.0.0.1:0".into(),
        jwt_secret: "test-secret".into(),
        access_ttl_secs: 3600,
        refresh_ttl_secs: 2_592_000,
        dev_mode: true,
        object_store_dir: std::env::temp_dir().join("muse-test-objects").to_string_lossy().to_string(),
    };
    let state = AppState::new(pool, config);
    let router = crate::app::build_router(state.clone());
    (router, state)
}

/// 发一个请求，返回 (status, json body)。
pub(crate) async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    if let Some(k) = key {
        builder = builder.header("idempotency-key", k);
    }
    let req = if let Some(b) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let stat = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value: Value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
    (stat, value)
}

/// challenge + login 全流程，返回 (accessToken, refreshToken, userId)。
pub(crate) async fn login_new_user(app: &Router, phone: &str) -> (String, String, String) {
    let (st, chal) = send(app, "POST", "/api/auth/challenge", None, None, Some(json!({ "phone": phone }))).await;
    assert_eq!(st, StatusCode::OK, "challenge failed: {chal:?}");
    let code = chal["devCode"].as_str().expect("dev_mode 应返回 devCode").to_string();
    let (st, login) = send(app, "POST", "/api/auth/login", None, None, Some(json!({ "phone": phone, "code": code }))).await;
    assert_eq!(st, StatusCode::OK, "login failed: {login:?}");
    (
        login["accessToken"].as_str().unwrap().to_string(),
        login["refreshToken"].as_str().unwrap().to_string(),
        login["user"]["id"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn login_issues_tokens_and_creates_user() {
    let (app, _s) = build_app().await;
    let (access, refresh, uid) = login_new_user(&app, "13800000000").await;
    assert!(!access.is_empty());
    assert!(!refresh.is_empty());
    assert!(uid.starts_with("user_"), "uid = {uid}");
}

#[tokio::test]
async fn login_upserts_same_phone_to_same_user() {
    let (app, state) = build_app().await;
    let phone = "13800000010";
    let (_a, _r, uid1) = login_new_user(&app, phone).await;

    // 直接种第二条有效 challenge（绕过 60s 限频，仅测 upsert 语义）。
    let code2 = "654321";
    sqlx::query("INSERT INTO sms_challenges (id, phone, code_hash, expires_at, consumed, created_at) VALUES (?, ?, ?, ?, 0, ?)")
        .bind(crate::db::new_id("chal"))
        .bind(phone)
        .bind(super::sha256_hex(code2))
        .bind(crate::db::now_ms() + 300_000)
        .bind(crate::db::now_ms())
        .execute(&state.db)
        .await
        .unwrap();

    let (st, login) = send(&app, "POST", "/api/auth/login", None, None, Some(json!({ "phone": phone, "code": code2 }))).await;
    assert_eq!(st, StatusCode::OK, "{login:?}");
    assert_eq!(login["user"]["id"].as_str().unwrap(), uid1, "同手机号必须复用同一账号");
}

#[tokio::test]
async fn challenge_rate_limited_within_60s() {
    let (app, _s) = build_app().await;
    let phone = "13800000001";
    let (st1, _) = send(&app, "POST", "/api/auth/challenge", None, None, Some(json!({ "phone": phone }))).await;
    assert_eq!(st1, StatusCode::OK);
    let (st2, _) = send(&app, "POST", "/api/auth/challenge", None, None, Some(json!({ "phone": phone }))).await;
    assert_eq!(st2, StatusCode::CONFLICT, "60s 内重复发码应被限频");
}

#[tokio::test]
async fn login_rejects_wrong_code() {
    let (app, _s) = build_app().await;
    let phone = "13800000004";
    let (_st, chal) = send(&app, "POST", "/api/auth/challenge", None, None, Some(json!({ "phone": phone }))).await;
    let code = chal["devCode"].as_str().unwrap();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let (st, _) = send(&app, "POST", "/api/auth/login", None, None, Some(json!({ "phone": phone, "code": wrong }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn refresh_rotates_and_revokes_old() {
    let (app, _s) = build_app().await;
    let (_a, refresh, _u) = login_new_user(&app, "13800000002").await;
    let (st, r1) = send(&app, "POST", "/api/auth/refresh", None, None, Some(json!({ "refreshToken": refresh }))).await;
    assert_eq!(st, StatusCode::OK, "{r1:?}");
    let new_refresh = r1["refreshToken"].as_str().unwrap().to_string();
    assert_ne!(new_refresh, refresh, "旋转后必须是新 token");

    // 旧 refresh 已 revoke → 401
    let (st, _) = send(&app, "POST", "/api/auth/refresh", None, None, Some(json!({ "refreshToken": refresh }))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "旧 refresh 必须失效");
    // 新 refresh 可用
    let (st, _) = send(&app, "POST", "/api/auth/refresh", None, None, Some(json!({ "refreshToken": new_refresh }))).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn logout_revokes_all_refresh() {
    let (app, _s) = build_app().await;
    let (access, refresh, _u) = login_new_user(&app, "13800000003").await;
    let (st, _) = send(&app, "POST", "/api/auth/logout", Some(&access), None, None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, "POST", "/api/auth/refresh", None, None, Some(json!({ "refreshToken": refresh }))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "登出后所有 refresh 应失效");
}

#[tokio::test]
async fn logout_requires_auth() {
    let (app, _s) = build_app().await;
    let (st, _) = send(&app, "POST", "/api/auth/logout", None, None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn identity_verification_stores_reference_only() {
    let (app, state) = build_app().await;
    let (access, _r, uid) = login_new_user(&app, "13800000005").await;
    let (st, resp) = send(
        &app,
        "POST",
        "/api/identity/verification",
        Some(&access),
        None,
        Some(json!({ "provider": "aliyun", "referenceId": "ref-123", "status": "verified" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{resp:?}");
    assert_eq!(resp["status"], "verified");

    // 只落 provider/referenceId/status —— 不存证件原文。
    let row: (String, String, String) =
        sqlx::query_as("SELECT provider, reference_id, status FROM identity_verification_refs WHERE user_id = ?")
            .bind(&uid)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(row, ("aliyun".into(), "ref-123".into(), "verified".into()));
}

#[tokio::test]
async fn age_declaration_writes_adult_then_minor() {
    let (app, state) = build_app().await;
    let (access, _r, uid) = login_new_user(&app, "13800000007").await;

    // 注册默认未声明（age_declared=0）
    let age0 = sqlx::query_scalar::<_, i64>("SELECT age_declared FROM users WHERE id = ?")
        .bind(&uid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(age0, 0, "注册应默认未声明");

    // 声明成年 → age_declared=1
    let (st, resp) =
        send(&app, "POST", "/api/auth/age-declaration", Some(&access), None, Some(json!({ "isAdult": true }))).await;
    assert_eq!(st, StatusCode::OK, "{resp:?}");
    assert_eq!(resp["ageDeclared"], 1);
    let age1 = sqlx::query_scalar::<_, i64>("SELECT age_declared FROM users WHERE id = ?")
        .bind(&uid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(age1, 1, "声明成年应写入 1");

    // 改声明未成年 → age_declared=2（保守方向可回退）
    let (st, resp) =
        send(&app, "POST", "/api/auth/age-declaration", Some(&access), None, Some(json!({ "isAdult": false }))).await;
    assert_eq!(st, StatusCode::OK, "{resp:?}");
    assert_eq!(resp["ageDeclared"], 2);
    let age2 = sqlx::query_scalar::<_, i64>("SELECT age_declared FROM users WHERE id = ?")
        .bind(&uid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(age2, 2, "声明未成年应写入 2");
}

#[tokio::test]
async fn age_declaration_requires_auth() {
    let (app, _s) = build_app().await;
    let (st, _) =
        send(&app, "POST", "/api/auth/age-declaration", None, None, Some(json!({ "isAdult": true }))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_idempotency_key_returns_cached() {
    let (app, _s) = build_app().await;
    let phone = "13800000006";
    let (_st, chal) = send(&app, "POST", "/api/auth/challenge", None, None, Some(json!({ "phone": phone }))).await;
    let code = chal["devCode"].as_str().unwrap().to_string();
    let body = json!({ "phone": phone, "code": code });
    let (_st, a) = send(&app, "POST", "/api/auth/login", None, Some("login-key"), Some(body.clone())).await;
    let (st, b) = send(&app, "POST", "/api/auth/login", None, Some("login-key"), Some(body)).await;
    assert_eq!(st, StatusCode::OK);
    // 同键同载荷 → 返回同一缓存响应（同一 refresh，不重复消费/签发）。
    assert_eq!(a["refreshToken"], b["refreshToken"]);
}
