//! 后台 API 集成测试（sqlite::memory + axum oneshot）。
//! 覆盖：AdminUser 守卫（401/403）、dev-login 换 admin token、审核 approve 回写、
//! prompt 激活互斥、模型路由回滚、看板聚合、ban/unban、模板审核流、风控/工单、audit_logs 留痕。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::any::AnyPoolOptions;
use sqlx::Row;
use tower::ServiceExt;

use crate::app::{build_router, AppState};
use crate::config::ServerConfig;
use crate::db::{new_id, now_ms};

static INIT: std::sync::Once = std::sync::Once::new();

fn test_config() -> ServerConfig {
    ServerConfig {
        database_url: "sqlite::memory:".into(),
        bind_addr: "127.0.0.1:0".into(),
        jwt_secret: "test-secret".into(),
        access_ttl_secs: 3600,
        refresh_ttl_secs: 100_000,
        dev_mode: true,
        object_store_dir: std::env::temp_dir().join(new_id("muse-admin-test")).to_string_lossy().into_owned(),
    }
}

async fn test_state() -> AppState {
    INIT.call_once(sqlx::any::install_default_drivers);
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, test_config())
}

fn admin_token(state: &AppState) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, "admin1", "admin", 3600).unwrap()
}

fn user_token(state: &AppState) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, "usr1", "user", 3600).unwrap()
}

fn role_token(state: &AppState, role: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, &format!("actor_{role}"), role, 3600).unwrap()
}

async fn get(app: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn post(app: &axum::Router, uri: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn seed_user(state: &AppState, id: &str, phone: Option<&str>, role: &str, status: &str) {
    sqlx::query(
        "INSERT INTO users (id, phone, nickname, age_declared, role, status, created_at, updated_at) \
         VALUES (?, ?, '昵称', 1, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(phone)
    .bind(role)
    .bind(status)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query(sql).fetch_one(&state.db).await.unwrap().try_get::<i64, _>("n").unwrap()
}

// ---------------- AdminUser 守卫 ----------------

#[tokio::test]
async fn guard_rejects_missing_and_non_admin() {
    let state = test_state().await;
    let app = build_router(state.clone());

    // 无 token → 401
    let (st, _) = get(&app, "/api/admin/users", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // user 角色 → 403
    let (st, _) = get(&app, "/api/admin/users", Some(&user_token(&state))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // admin 角色 → 200
    let (st, body) = get(&app, "/api/admin/users", Some(&admin_token(&state))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body["users"].is_array());
}

#[tokio::test]
async fn dev_login_issues_admin_token() {
    let state = test_state().await;
    let app = build_router(state.clone());

    // 错误 secret → 401
    let (st, _) = post(&app, "/api/admin/dev-login", None, json!({ "secret": "wrong" })).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // 正确 secret → 200 + admin token
    let (st, body) = post(&app, "/api/admin/dev-login", None, json!({ "secret": "muse-dev-admin" })).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["role"], "admin");
    let token = body["accessToken"].as_str().unwrap();

    // 用换来的 token 访问受保护端点 → 200
    let (st, _) = get(&app, "/api/admin/metrics/overview", Some(token)).await;
    assert_eq!(st, StatusCode::OK);
}

// ---------------- S-6：后台最小权限 role→action 矩阵 ----------------

#[tokio::test]
async fn role_matrix_enforces_least_privilege() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "u_ban", Some("13800000000"), "user", "active").await;

    let reviewer = role_token(&state, "reviewer");
    let finance = role_token(&state, "finance");
    let operator = role_token(&state, "operator");
    let support = role_token(&state, "support");
    let admin = admin_token(&state);

    // reviewer：内容审核队列可读；用户/经济/建房越权 403。
    assert_eq!(get(&app, "/api/admin/audit-queue", Some(&reviewer)).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/users", Some(&reviewer)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&reviewer)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        post(&app, "/api/admin/worlds", Some(&reviewer), json!({ "templateId": "t", "title": "x" })).await.0,
        StatusCode::FORBIDDEN
    );

    // finance：经济/看板只读；审核/用户越权 403。
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&finance)).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/metrics/overview", Some(&finance)).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/audit-queue", Some(&finance)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/users", Some(&finance)).await.0, StatusCode::FORBIDDEN);

    // operator：世界运营可；用户/治理写越权 403。
    assert_eq!(get(&app, "/api/admin/worlds", Some(&operator)).await.0, StatusCode::OK);
    assert_eq!(
        post(&app, "/api/admin/worlds", Some(&operator), json!({ "templateId": "t", "templateVersion": 1, "title": "x", "roomType": "idle" })).await.0,
        StatusCode::OK
    );
    assert_eq!(get(&app, "/api/admin/users", Some(&operator)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        post(&app, "/api/admin/prompts", Some(&operator), json!({ "scope": "director", "version": "v1", "content": "x" })).await.0,
        StatusCode::FORBIDDEN,
        "治理写操作仅 admin"
    );

    // support：用户管理/工单可；审核/建房越权 403。
    assert_eq!(get(&app, "/api/admin/users", Some(&support)).await.0, StatusCode::OK);
    assert_eq!(post(&app, "/api/admin/users/u_ban/ban", Some(&support), json!({})).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/data-requests", Some(&support)).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/audit-queue", Some(&support)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        post(&app, "/api/admin/worlds", Some(&support), json!({ "templateId": "t", "title": "x" })).await.0,
        StatusCode::FORBIDDEN
    );

    // admin：全权（抽查建房 / 审核 / 经济）。
    assert_eq!(
        post(&app, "/api/admin/worlds", Some(&admin), json!({ "templateId": "t", "templateVersion": 1, "title": "x", "roomType": "idle" })).await.0,
        StatusCode::OK
    );
    assert_eq!(get(&app, "/api/admin/audit-queue", Some(&admin)).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&admin)).await.0, StatusCode::OK);
}

// ---------------- 用户管理 + audit_logs ----------------

#[tokio::test]
async fn ban_unban_desensitized_and_audited() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    seed_user(&state, "u_target", Some("13800001234"), "user", "active").await;

    // 列表脱敏：手机号打码。
    let (st, body) = get(&app, "/api/admin/users?query=13800001234", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let phone = body["users"][0]["phone"].as_str().unwrap();
    assert!(phone.contains("****"), "手机号应脱敏: {phone}");
    assert!(!phone.contains("00001"), "不应出现完整号段: {phone}");

    // ban
    let (st, body) = post(&app, "/api/admin/users/u_target/ban", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "banned");
    let s = sqlx::query("SELECT status FROM users WHERE id='u_target'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "banned");

    // unban
    let (st, _) = post(&app, "/api/admin/users/u_target/unban", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let s = sqlx::query("SELECT status FROM users WHERE id='u_target'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "active");

    // 未知用户 → 404
    let (st, _) = post(&app, "/api/admin/users/nope/ban", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // audit_logs 留痕：ban + unban 各一条。
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM audit_logs WHERE action='user.ban'").await, 1);
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM audit_logs WHERE action='user.unban'").await, 1);
}

// ---------------- 内容审核 approve 回写 ----------------

#[tokio::test]
async fn audit_approve_writes_back_character_moderation() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    // 待审角色（pending）+ 审核队列条目。
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES ('ch1','usr1','loc',1,'{}','original','pending',0,?)",
    )
    .bind(now_ms()).execute(&state.db).await.unwrap();
    sqlx::query(
        "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, status, created_at) \
         VALUES ('aq1','character','ch1','flagged','open',?)",
    )
    .bind(now_ms()).execute(&state.db).await.unwrap();

    // 队列可见。
    let (st, body) = get(&app, "/api/admin/audit-queue?status=open", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["items"][0]["subjectId"], "ch1");

    // approve → 队列 approved + 角色 moderation approved。
    let (st, body) = post(&app, "/api/admin/audit-queue/aq1/approve", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["moderation"], "approved");

    let q = sqlx::query("SELECT status FROM audit_queue WHERE id='aq1'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(q, "approved");
    let m = sqlx::query("SELECT moderation FROM cloud_characters WHERE id='ch1'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "approved");

    // 重复裁决 → 409。
    let (st, _) = post(&app, "/api/admin/audit-queue/aq1/approve", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT);

    // 留痕。
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM audit_logs WHERE action='audit.approved'").await, 1);
}

// ---------------- #10a 审核详情：卡片全文 + 同作者历史 ----------------

#[tokio::test]
async fn audit_detail_returns_card_full_text_manifest_and_author_history() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let now = now_ms();

    // 同一 owner 两张卡：chC 待审（当前主体），chH 历史；另有他人卡 chOther 不应出现在历史里。
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, manifest_json, created_at) \
         VALUES ('chH','ownerX','locH',2,'{\"identity\":{\"name\":\"历史卡\"}}','original','approved',0,'{\"fields\":[]}',?)",
    )
    .bind(now - 1000)
    .execute(&state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, manifest_json, created_at) \
         VALUES ('chC','ownerX','locC',1,'{\"identity\":{\"name\":\"当前卡\"},\"dramaticCore\":{\"coreContradiction\":\"忠诚与自由\"}}','original','pending',0,'{\"purpose\":\"叙事决策\"}',?)",
    )
    .bind(now)
    .execute(&state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES ('chOther','ownerY','locO',1,'{}','original','approved',0,?)",
    )
    .bind(now)
    .execute(&state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, machine_hits, status, created_at) \
         VALUES ('aqD','character','chC','flagged','[{\"rule\":\"imperative_override\"}]','open',?)",
    )
    .bind(now)
    .execute(&state.db)
    .await
    .unwrap();

    let (st, body) = get(&app, "/api/admin/audit-queue/aqD", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["subjectId"], "chC");
    assert_eq!(body["subjectKind"], "character");
    // 卡片全文（原文，非第三人称摘要）。
    assert_eq!(body["cardJson"]["identity"]["name"], "当前卡");
    assert_eq!(body["cardJson"]["dramaticCore"]["coreContradiction"], "忠诚与自由");
    // manifest 内联。
    assert_eq!(body["manifest"]["purpose"], "叙事决策");
    // 机审命中点透传。
    assert_eq!(body["machineHits"][0]["rule"], "imperative_override");
    // 同作者历史：含 chH，不含当前主体 chC，不含他人 chOther。
    let hist = body["authorHistory"].as_array().unwrap();
    assert_eq!(hist.len(), 1, "只出同作者其他卡: {hist:?}");
    assert_eq!(hist[0]["id"], "chH");
    assert_eq!(hist[0]["version"], 2);
    assert_eq!(hist[0]["moderation"], "approved");
    assert!(hist[0]["createdAt"].is_number());
}

#[tokio::test]
async fn audit_detail_role_gate_and_not_found() {
    let state = test_state().await;
    let app = build_router(state.clone());
    sqlx::query(
        "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, status, created_at) \
         VALUES ('aqR','character','x','ok','open',?)",
    )
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();

    // 无 token → 401；user → 403；finance 越权 → 403；reviewer/admin 放行。
    assert_eq!(get(&app, "/api/admin/audit-queue/aqR", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(get(&app, "/api/admin/audit-queue/aqR", Some(&user_token(&state))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        get(&app, "/api/admin/audit-queue/aqR", Some(&role_token(&state, "finance"))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&app, "/api/admin/audit-queue/aqR", Some(&role_token(&state, "reviewer"))).await.0,
        StatusCode::OK
    );
    // 不存在 → 404。
    assert_eq!(get(&app, "/api/admin/audit-queue/nope", Some(&admin_token(&state))).await.0, StatusCode::NOT_FOUND);
}

// ---------------- 模板创建 + 审核回写 ----------------

#[tokio::test]
async fn template_create_and_review_flow() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    // skeleton 非对象 → 400。
    let (st, _) = post(
        &app,
        "/api/admin/world-templates",
        Some(&admin),
        json!({ "title": "X", "roomType": "idle", "skeletonJson": "bad" }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 合法创建 → pending + 入审核队列。
    let (st, body) = post(
        &app,
        "/api/admin/world-templates",
        Some(&admin),
        json!({ "title": "官方模板", "roomType": "idle", "skeletonJson": { "mainNodes": [], "endings": [] } }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["moderation"], "pending");
    let tpl_id = body["templateId"].as_str().unwrap().to_string();

    // 审核队列里能找到该模板条目。
    let (_, q) = get(&app, "/api/admin/audit-queue?status=open", Some(&admin)).await;
    let aq_id = q["items"].as_array().unwrap().iter()
        .find(|i| i["subjectId"] == tpl_id.as_str())
        .and_then(|i| i["id"].as_str()).unwrap().to_string();

    // approve → 模板 moderation approved。
    let (st, _) = post(&app, &format!("/api/admin/audit-queue/{aq_id}/approve"), Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let m = sqlx::query("SELECT moderation FROM world_templates WHERE id=?")
        .bind(&tpl_id).fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "approved");
}

// ---------------- Prompt 版本化 / 激活互斥 / 灰度 ----------------

#[tokio::test]
async fn prompt_activation_is_mutually_exclusive_within_scope() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    // 非法 scope → 400。
    let (st, _) = post(&app, "/api/admin/prompts", Some(&admin), json!({ "scope": "bogus", "version": "v1", "content": "x" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 建两个同 scope 版本。
    let (_, a) = post(&app, "/api/admin/prompts", Some(&admin), json!({ "scope": "director", "version": "v1", "content": "A" })).await;
    let (_, b) = post(&app, "/api/admin/prompts", Some(&admin), json!({ "scope": "director", "version": "v2", "content": "B" })).await;
    let id_a = a["id"].as_str().unwrap().to_string();
    let id_b = b["id"].as_str().unwrap().to_string();

    // 激活 v1。
    let (st, _) = post(&app, &format!("/api/admin/prompts/{id_a}/activate"), Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM prompt_versions WHERE scope='director' AND active=1").await, 1);

    // 激活 v2 → 互斥：只剩 v2 active。
    let (st, _) = post(&app, &format!("/api/admin/prompts/{id_b}/activate"), Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM prompt_versions WHERE scope='director' AND active=1").await, 1);
    let active_id = sqlx::query("SELECT id FROM prompt_versions WHERE scope='director' AND active=1")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("id").unwrap();
    assert_eq!(active_id, id_b);

    // 一键回滚 = 再激活 v1。
    let (st, _) = post(&app, &format!("/api/admin/prompts/{id_a}/activate"), Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let active_id = sqlx::query("SELECT id FROM prompt_versions WHERE scope='director' AND active=1")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("id").unwrap();
    assert_eq!(active_id, id_a);

    // 灰度：写 canary_world_ids。
    let (st, cb) = post(&app, &format!("/api/admin/prompts/{id_b}/canary"), Some(&admin), json!({ "worldIds": ["w1", "w2"] })).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cb["canaryWorldIds"], json!(["w1", "w2"]));
    let raw = sqlx::query("SELECT canary_world_ids FROM prompt_versions WHERE id=?")
        .bind(&id_b).fetch_one(&state.db).await.unwrap().try_get::<String, _>("canary_world_ids").unwrap();
    assert_eq!(raw, "[\"w1\",\"w2\"]");
}

// ---------------- 模型路由激活 / 回滚 ----------------

#[tokio::test]
async fn model_route_activation_and_rollback() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let (_, a) = post(&app, "/api/admin/model-routes", Some(&admin), json!({ "version": "v1", "routesJson": { "decide": { "model": "m1" } } })).await;
    let (_, b) = post(&app, "/api/admin/model-routes", Some(&admin), json!({ "version": "v2", "routesJson": { "decide": { "model": "m2" } } })).await;
    let id_a = a["id"].as_str().unwrap().to_string();
    let id_b = b["id"].as_str().unwrap().to_string();

    post(&app, &format!("/api/admin/model-routes/{id_a}/activate"), Some(&admin), json!({})).await;
    post(&app, &format!("/api/admin/model-routes/{id_b}/activate"), Some(&admin), json!({})).await;
    // 全局单活跃。
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM model_routes WHERE active=1").await, 1);
    let active = sqlx::query("SELECT id FROM model_routes WHERE active=1")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("id").unwrap();
    assert_eq!(active, id_b);

    // routesJson 非对象 → 400。
    let (st, _) = post(&app, "/api/admin/model-routes", Some(&admin), json!({ "version": "v3", "routesJson": "bad" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

// ---------------- 世界运营 ----------------

#[tokio::test]
async fn world_create_pause_resume_and_diagnostics() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    // 官方建房。
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "官方放置世界", "roomType": "idle", "dailyTokenBudget": 1000 }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let wid = body["worldId"].as_str().unwrap().to_string();

    // 预算写入。
    let budget = sqlx::query("SELECT daily_token_budget FROM world_budgets WHERE world_id=?")
        .bind(&wid).fetch_one(&state.db).await.unwrap().try_get::<i64, _>("daily_token_budget").unwrap();
    assert_eq!(budget, 1000);

    // 诊断：脱敏视图，含 redactionNote，不含叙事内容。
    let (st, diag) = get(&app, &format!("/api/admin/worlds/{wid}/diagnostics"), Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(diag["world"]["id"], wid.as_str());
    assert!(diag["redactionNote"].is_string());
    assert!(diag["ticks"].is_array());

    // pause → paused。
    let (st, _) = post(&app, &format!("/api/admin/worlds/{wid}/pause"), Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let s = sqlx::query("SELECT status FROM worlds WHERE id=?")
        .bind(&wid).fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "paused");

    // resume → running。
    let (st, _) = post(&app, &format!("/api/admin/worlds/{wid}/resume"), Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let s = sqlx::query("SELECT status FROM worlds WHERE id=?")
        .bind(&wid).fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "running");

    // 已 running 再 resume → 409（非 paused）。
    let (st, _) = post(&app, &format!("/api/admin/worlds/{wid}/resume"), Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT);

    // 列表可见。
    let (st, list) = get(&app, "/api/admin/worlds?status=running", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list["worlds"].as_array().unwrap().iter().any(|w| w["id"] == wid.as_str()));

    // 未知世界诊断 → 404。
    let (st, _) = get(&app, "/api/admin/worlds/nope/diagnostics", Some(&admin)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

// ---------------- 数据看板聚合 ----------------

#[tokio::test]
async fn metrics_overview_aggregates() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    seed_user(&state, "u1", None, "user", "active").await;
    seed_user(&state, "u2", None, "user", "banned").await;

    // 日报：2 条，1 条已打开。
    sqlx::query("INSERT INTO daily_reports (id, world_id, user_id, character_id, report_day, content_json, opened_at, created_at) VALUES ('dr1','w1','u1','c1','2026-07-20','{}',?,?)")
        .bind(now_ms()).bind(now_ms()).execute(&state.db).await.unwrap();
    sqlx::query("INSERT INTO daily_reports (id, world_id, user_id, character_id, report_day, content_json, opened_at, created_at) VALUES ('dr2','w1','u1','c2','2026-07-20','{}',NULL,?)")
        .bind(now_ms()).execute(&state.db).await.unwrap();

    // tick：1 done(100) + 1 failed(50)。
    sqlx::query("INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) VALUES ('t1','w1',0,0,'done',100,?)")
        .bind(now_ms()).execute(&state.db).await.unwrap();
    sqlx::query("INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) VALUES ('t2','w1',1,0,'failed',50,?)")
        .bind(now_ms()).execute(&state.db).await.unwrap();

    // 审核积压：1 条 open。
    sqlx::query("INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, status, created_at) VALUES ('aq1','character','ch1','ok','open',?)")
        .bind(now_ms()).execute(&state.db).await.unwrap();

    let (st, m) = get(&app, "/api/admin/metrics/overview", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(m["users"]["total"], 2);
    assert_eq!(m["users"]["banned"], 1);
    assert_eq!(m["dailyReports"]["total"], 2);
    assert_eq!(m["dailyReports"]["opened"], 1);
    assert_eq!(m["dailyReports"]["openRate"], 0.5);
    assert_eq!(m["ticks"]["total"], 2);
    assert_eq!(m["ticks"]["done"], 1);
    assert_eq!(m["ticks"]["successRate"], 0.5);
    assert_eq!(m["auditBacklog"], 1);
    // token 成本按世界聚合。
    assert_eq!(m["tokenCostByWorld"][0]["worldId"], "w1");
    assert_eq!(m["tokenCostByWorld"][0]["tokens"], 150);
}

// ---------------- 经济运营：真实只读聚合 ----------------

#[tokio::test]
async fn economy_overview_empty_reports_zeros_and_disabled() {
    // 无任何计费/礼物数据时：全 0、billingEnabled=false（阶段由数据体现，非写死）。
    let state = test_state().await;
    let app = build_router(state.clone());
    let (st, e) = get(&app, "/api/admin/economy/overview", Some(&admin_token(&state))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(e["billingEnabled"], false);
    assert_eq!(e["recharge"]["totalCents"], 0);
    assert_eq!(e["refund"]["totalCents"], 0);
    assert_eq!(e["balance"]["totalCents"], 0);
    assert_eq!(e["orders"]["total"], 0);
    assert_eq!(e["gifts"]["events"], 0);
    // 订单状态桶恒含五态，缺省 0。
    assert_eq!(e["orders"]["byStatus"]["fulfilled"], 0);
    assert_eq!(e["orders"]["byStatus"]["refunded"], 0);
    // 创作者结算不在本聚合内（另一套账，§2.6）。
    assert_eq!(e["creatorSettlement"]["enabled"], false);
}

#[tokio::test]
async fn economy_overview_aggregates_orders_ledger_balances_gifts() {
    let state = test_state().await;
    let app = build_router(state.clone());

    // 两笔充值（100 + 300 = 400）各配一条 +ledger；其中一笔（300）后被退款：
    // orders: 1 fulfilled + 1 refunded；ledger: +100 recharge, +300 recharge, -300 refund；
    // billing_balances: 用户净额 100。恒等式 充值400 - 退款300 = 余额100。
    let now = now_ms();
    for (oid, amt, status) in [("o_keep", 100i64, "fulfilled"), ("o_ref", 300i64, "refunded")] {
        sqlx::query(
            "INSERT INTO orders (id, user_id, kind, amount_cents, status, created_at, updated_at) \
             VALUES (?, 'u_pay', 'recharge', ?, ?, ?, ?)",
        )
        .bind(oid).bind(amt).bind(status).bind(now).bind(now)
        .execute(&state.db).await.unwrap();
    }
    // 另加一笔 created 订单（未履约，进状态计数但不入账本）。
    sqlx::query(
        "INSERT INTO orders (id, user_id, kind, amount_cents, status, created_at, updated_at) \
         VALUES ('o_new', 'u_pay', 'recharge', 50, 'created', ?, ?)",
    )
    .bind(now).bind(now).execute(&state.db).await.unwrap();

    for (lid, oid, delta, reason) in [
        ("l1", "o_keep", 100i64, "recharge"),
        ("l2", "o_ref", 300i64, "recharge"),
        ("l3", "o_ref", -300i64, "refund"),
    ] {
        sqlx::query(
            "INSERT INTO ledger_entries (id, user_id, order_id, delta_cents, reason, created_at) \
             VALUES (?, 'u_pay', ?, ?, ?, ?)",
        )
        .bind(lid).bind(oid).bind(delta).bind(reason).bind(now)
        .execute(&state.db).await.unwrap();
    }
    sqlx::query("INSERT INTO billing_balances (user_id, balance_cents, updated_at) VALUES ('u_pay', 100, ?)")
        .bind(now).execute(&state.db).await.unwrap();

    // 礼物流水：两世界共 3 条事件、礼物量 1+2+5=8。
    for (gid, world, cnt) in [("g1", "w1", 1i64), ("g2", "w1", 2i64), ("g3", "w2", 5i64)] {
        sqlx::query(
            "INSERT INTO gift_events (id, world_id, sku, gift_count, mapped, created_at) \
             VALUES (?, ?, 'rose', ?, 1, ?)",
        )
        .bind(gid).bind(world).bind(cnt).bind(now)
        .execute(&state.db).await.unwrap();
    }

    let (st, e) = get(&app, "/api/admin/economy/overview", Some(&admin_token(&state))).await;
    assert_eq!(st, StatusCode::OK);

    // 有充值 → billingEnabled=true。
    assert_eq!(e["billingEnabled"], true);
    // 资金：充值 400 / 退款 300（正表示）/ 余额 100。
    assert_eq!(e["recharge"]["totalCents"], 400);
    assert_eq!(e["recharge"]["count"], 2);
    assert_eq!(e["refund"]["totalCents"], 300);
    assert_eq!(e["refund"]["count"], 1);
    assert_eq!(e["balance"]["totalCents"], 100);
    assert_eq!(e["balance"]["wallets"], 1);
    // 双录不变量：充值 - 退款 == 余额。
    assert_eq!(e["ledgerNetCents"], 100);
    assert_eq!(e["ledgerNetCents"].as_i64().unwrap(), e["balance"]["totalCents"].as_i64().unwrap());
    // 订单：3 总数，按状态计数正确。
    assert_eq!(e["orders"]["total"], 3);
    assert_eq!(e["orders"]["byStatus"]["fulfilled"], 1);
    assert_eq!(e["orders"]["byStatus"]["refunded"], 1);
    assert_eq!(e["orders"]["byStatus"]["created"], 1);
    assert_eq!(e["orders"]["byStatus"]["paid"], 0);
    // 礼物：3 事件 / 礼物量 8 / 覆盖 2 世界。
    assert_eq!(e["gifts"]["events"], 3);
    assert_eq!(e["gifts"]["giftCount"], 8);
    assert_eq!(e["gifts"]["worlds"], 2);
}

#[tokio::test]
async fn economy_overview_role_gate_finance_and_admin_only() {
    // finance/admin 放行；operator/reviewer/support/user 越权。
    let state = test_state().await;
    let app = build_router(state.clone());
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&admin_token(&state))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&role_token(&state, "finance"))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&role_token(&state, "operator"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&role_token(&state, "support"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/economy/overview", Some(&user_token(&state))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/economy/overview", None).await.0, StatusCode::UNAUTHORIZED);
}

// ---------------- 风控 + 工单 ----------------

#[tokio::test]
async fn risk_events_and_data_requests() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    sqlx::query("INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) VALUES ('r1','u1','w1','injection','{\"hit\":1}',?)")
        .bind(now_ms()).execute(&state.db).await.unwrap();
    sqlx::query("INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) VALUES ('r2','u1','w1','abuse','{}',?)")
        .bind(now_ms()).execute(&state.db).await.unwrap();

    // kind 过滤。
    let (st, body) = get(&app, "/api/admin/risk-events?kind=injection", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
    assert_eq!(body["events"][0]["kind"], "injection");

    // 工单：export pending → run → done + resultKey。
    sqlx::query("INSERT INTO data_requests (id, user_id, kind, status, created_at, updated_at) VALUES ('dq1','u1','export','pending',?,?)")
        .bind(now_ms()).bind(now_ms()).execute(&state.db).await.unwrap();

    let (st, body) = get(&app, "/api/admin/data-requests?status=pending", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["requests"][0]["id"], "dq1");

    let (st, body) = post(&app, "/api/admin/data-requests/dq1/run", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "done");
    assert!(body["resultKey"].as_str().unwrap().starts_with("export/"));

    let s = sqlx::query("SELECT status FROM data_requests WHERE id='dq1'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "done");

    // 幂等：再次 run → already_done。
    let (st, body) = post(&app, "/api/admin/data-requests/dq1/run", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["note"], "already_done");
}

// ---------------- Low：delete 工单在真实删除实现前保持 pending（合规，不谎报已删除） ----------------

#[tokio::test]
async fn delete_data_request_stays_pending_not_marked_done() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    sqlx::query("INSERT INTO data_requests (id, user_id, kind, status, created_at, updated_at) VALUES ('dq_del','u1','delete','pending',?,?)")
        .bind(now_ms()).bind(now_ms()).execute(&state.db).await.unwrap();

    let (st, body) = post(&app, "/api/admin/data-requests/dq_del/run", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "pending", "delete 工单在真实级联删除实现前不得标 done");
    assert_eq!(body["note"], "delete_execution_not_implemented");

    // 库内仍为 pending（未谎报完成）。
    let s = sqlx::query("SELECT status FROM data_requests WHERE id='dq_del'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "pending");
    // 但尝试有审计留痕。
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM audit_logs WHERE action='data_request.run_deferred'").await, 1);
}
