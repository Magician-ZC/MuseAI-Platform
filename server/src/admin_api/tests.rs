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

use muse_engine::character::types::{CardLifecycle, CharacterCardV2, Identity};

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

/// 完整可解析的 CharacterCardV2 JSON（全字段用 Default 填充）；用于需卡真正解析的引用完整性校验用例。
fn full_card_json(id: &str, name: &str) -> Value {
    let card = CharacterCardV2 {
        schema_version: 2,
        id: id.into(),
        lifecycle: CardLifecycle::Ready,
        identity: Identity { name: name.into(), ..Default::default() },
        dramatic_core: Default::default(),
        decision_model: Default::default(),
        perception: Default::default(),
        emotion_dynamics: Default::default(),
        relation_grammar: Default::default(),
        expression_fingerprint: Default::default(),
        agency: Default::default(),
        growth_arc: Default::default(),
        world_adaptation: Default::default(),
        evidence_index: Default::default(),
        revision: 1,
        created_at: 0,
        updated_at: 0,
    };
    serde_json::to_value(card).unwrap()
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

// ---------------- Phase 3：建模板期引用完整性校验（reward_item_ref / connections / residentItems） ----------------

#[tokio::test]
async fn create_template_rejects_dangling_references() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let create = |body: Value| {
        let app = app.clone();
        let admin = admin.clone();
        async move { post(&app, "/api/admin/world-templates", Some(&admin), body).await }
    };

    // 1) 完整引用 → 通过（worldItems 目录 + 地点连通/驻留/gate + 世界角色携带全部可解引用）。
    let good = json!({
        "title": "完整引用模板", "roomType": "chapter",
        "skeletonJson": {
            "worldItems": [
                { "id": "wi_key", "narrative": "玉钥", "effectTags": ["access:secret"],
                  "origin": { "worldTemplateId": "t", "cosmology": ["myth"], "powerTier": 2 } }
            ],
            "locations": [
                { "id": "hall", "name": "前厅", "connections": ["secret"] },
                { "id": "secret", "name": "秘境", "connections": ["hall"], "isSecretRealm": true,
                  "gate": { "requiredItemIds": ["wi_key"], "requiredCosmologies": ["myth"], "maxPowerTier": 3 },
                  "residentItemIds": ["wi_key"] }
            ],
            "hiddenContentPool": [ { "id": "hc1", "themes": ["秘"], "rewardItemRef": "wi_key" } ]
        }
    });
    let (st, body) = create(good).await;
    assert_eq!(st, StatusCode::OK, "完整引用应通过: {body}");

    // 2) rewardItemRef 悬空（目录无此 id 且无内联 fallback）→ 400。
    let (st, _) = create(json!({
        "title": "悬空奖励引用", "roomType": "chapter",
        "skeletonJson": {
            "worldItems": [],
            "hiddenContentPool": [ { "id": "hc1", "themes": ["秘"], "rewardItemRef": "ghost_item" } ]
        }
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "悬空 rewardItemRef 应拒绝");

    // 2b) rewardItemRef 悬空但有内联 fallback → 通过（兼容期 fallback 合法）。
    let (st, _) = create(json!({
        "title": "悬空引用但有内联", "roomType": "chapter",
        "skeletonJson": {
            "hiddenContentPool": [ { "id": "hc1", "themes": ["秘"], "rewardItemRef": "ghost_item",
                "rewardItem": { "id": "inline", "narrative": "内联", "effectTags": [],
                  "origin": { "worldTemplateId": "t", "cosmology": ["myth"], "powerTier": 1 } } } ]
        }
    })).await;
    assert_eq!(st, StatusCode::OK, "有内联 fallback 的悬空 ref 应通过");

    // 3) connections 悬空（连向不存在地点）→ 400。
    let (st, _) = create(json!({
        "title": "悬空连通", "roomType": "chapter",
        "skeletonJson": { "locations": [ { "id": "hall", "connections": ["nowhere"] } ] }
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "悬空 connections 应拒绝");

    // 4) residentItemIds 悬空（引用不存在的 worldItems）→ 400。
    let (st, _) = create(json!({
        "title": "悬空驻留道具", "roomType": "chapter",
        "skeletonJson": {
            "worldItems": [],
            "locations": [ { "id": "hall", "residentItemIds": ["ghost_item"] } ]
        }
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "悬空 residentItemIds 应拒绝");

    // 5) gate.requiredCosmologies 非官方枚举 → 400。
    let (st, _) = create(json!({
        "title": "非法体系", "roomType": "chapter",
        "skeletonJson": {
            "locations": [ { "id": "secret", "gate": { "requiredCosmologies": ["warp"] } } ]
        }
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "非法体系标签应拒绝");

    // 6) 世界角色 carriedItemIds 悬空 → 400（卡须完整可解析，否则 Skeleton 解析失败会退化为不校验）。
    let npc_card = full_card_json("npc1", "反派");
    let (st, _) = create(json!({
        "title": "悬空携带道具", "roomType": "chapter",
        "skeletonJson": {
            "worldItems": [],
            "worldCharacters": [ { "card": npc_card, "carriedItemIds": ["ghost_item"] } ]
        }
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "世界角色悬空 carriedItemIds 应拒绝");
}

// ---------------- 波次 3：模板星级 curation（RBAC / 范围校验 / audit 留痕 / star_source 翻转） ----------------

/// 建一个模板并返回 id（admin 建，star_rating/star_source 走 0020 列默认值 1/'auto'）。
async fn seed_template_for_star(app: &axum::Router, state: &AppState) -> String {
    let (st, body) = post(
        app,
        "/api/admin/world-templates",
        Some(&admin_token(state)),
        json!({ "title": "星级候选模板", "roomType": "chapter", "skeletonJson": { "mainlineNodes": [] } }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body["templateId"].as_str().unwrap().to_string()
}

async fn star_row(state: &AppState, id: &str) -> (i64, String) {
    sqlx::query_as("SELECT star_rating, star_source FROM world_templates WHERE id = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn template_star_curation_rbac_and_source_flip() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let tpl_id = seed_template_for_star(&app, &state).await;
    assert_eq!(star_row(&state, &tpl_id).await, (1, "auto".into()), "新模板默认 1★/auto");
    let uri = format!("/api/admin/world-templates/{tpl_id}/star");

    // RBAC：reviewer/finance/support → 403（且不改星级、不落审计）。
    for role in ["reviewer", "finance", "support"] {
        let t = role_token(&state, role);
        let (st, _) = post(&app, &uri, Some(&t), json!({ "star": 4, "reason": "越权尝试" })).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{role} 应 403");
    }
    assert_eq!(star_row(&state, &tpl_id).await.0, 1, "越权请求不得改星级");
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM audit_logs WHERE action='template_star'").await, 0);

    // operator → 200：star_rating=4 + star_source 翻转为 curated + audit 留痕（action=template_star）。
    let operator = role_token(&state, "operator");
    let (st, v) =
        post(&app, &uri, Some(&operator), json!({ "star": 4, "reason": "结构厚度与完读数据达标，晋升四星" })).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["starRating"], 4);
    assert_eq!(v["starSource"], "curated");
    assert_eq!(star_row(&state, &tpl_id).await, (4, "curated".into()), "star_source 应翻转为 curated");
    let n = count(
        &state,
        &format!("SELECT COUNT(*) AS n FROM audit_logs WHERE action='template_star' AND subject='{tpl_id}'"),
    )
    .await;
    assert_eq!(n, 1, "curation 应恰好一条 audit_logs 留痕");

    // admin 直通：定 5★。
    let (st, v) = post(&app, &uri, Some(&admin_token(&state)), json!({ "star": 5, "reason": "周年运营位" })).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(star_row(&state, &tpl_id).await.0, 5);

    // admin 模板列表投影补 starRating/starSource。
    let (st, lst) = get(&app, "/api/admin/world-templates", Some(&admin_token(&state))).await;
    assert_eq!(st, StatusCode::OK);
    let item = lst["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == tpl_id.as_str())
        .expect("列表应含该模板")
        .clone();
    assert_eq!(item["starRating"], 5, "admin 模板列表应带 starRating");
    assert_eq!(item["starSource"], "curated");
}

#[tokio::test]
async fn template_star_rejects_bad_range_reason_and_missing() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let tpl_id = seed_template_for_star(&app, &state).await;
    let uri = format!("/api/admin/world-templates/{tpl_id}/star");

    // 范围非法：0 / 6 → 400。
    for bad in [0, 6] {
        let (st, _) = post(&app, &uri, Some(&admin), json!({ "star": bad, "reason": "范围试探" })).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "star={bad} 应 400");
    }
    // 理由非法：空 / 全空白 / 超 500 字符 → 400。
    let (st, _) = post(&app, &uri, Some(&admin), json!({ "star": 3, "reason": "" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "空 reason 应 400");
    let (st, _) = post(&app, &uri, Some(&admin), json!({ "star": 3, "reason": "   " })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "全空白 reason 应 400");
    let (st, _) = post(&app, &uri, Some(&admin), json!({ "star": 3, "reason": "长".repeat(501) })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "超长 reason 应 400");
    // 恰 500 字符 → 合法边界。
    let (st, _) = post(&app, &uri, Some(&admin), json!({ "star": 3, "reason": "长".repeat(500) })).await;
    assert_eq!(st, StatusCode::OK, "500 字符 reason 应通过");

    // 模板不存在 → 404。
    let (st, _) =
        post(&app, "/api/admin/world-templates/ghost/star", Some(&admin), json!({ "star": 3, "reason": "不存在" })).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 非法请求全程未污染星级（最终为上面 500 字符那次合法定档的 3★）。
    assert_eq!(star_row(&state, &tpl_id).await, (3, "curated".into()));
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

// ---------------- 建房 timelineMode（缺口①） ----------------

#[tokio::test]
async fn world_create_timeline_mode() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    // event 放置房：timelineMode=event + roomType=idle → 落库 event。
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "放置世界", "roomType": "idle", "timelineMode": "event" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let wid = body["worldId"].as_str().unwrap().to_string();
    let tm = sqlx::query("SELECT timeline_mode FROM worlds WHERE id=?")
        .bind(&wid).fetch_one(&state.db).await.unwrap().try_get::<String, _>("timeline_mode").unwrap();
    assert_eq!(tm, "event");

    // 省略 timelineMode → 默认 interval（向后兼容）。
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "默认世界", "roomType": "idle" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let wid2 = body["worldId"].as_str().unwrap().to_string();
    let tm2 = sqlx::query("SELECT timeline_mode FROM worlds WHERE id=?")
        .bind(&wid2).fetch_one(&state.db).await.unwrap().try_get::<String, _>("timeline_mode").unwrap();
    assert_eq!(tm2, "interval");

    // 非法值 → 400。
    let (st, _) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "非法", "roomType": "idle", "timelineMode": "foo" }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // P2 Stage3 建房闸放宽：event × chapter 现允许（引擎走 DES 地点碰撞，调度节奏由 chapter start 端点驱动）。
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "event章节房", "roomType": "chapter", "timelineMode": "event" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "event × chapter 应允许建房（Stage3 放宽）");
    let wid_chap = body["worldId"].as_str().unwrap().to_string();
    let tm_chap = sqlx::query("SELECT timeline_mode FROM worlds WHERE id=?")
        .bind(&wid_chap).fetch_one(&state.db).await.unwrap().try_get::<String, _>("timeline_mode").unwrap();
    assert_eq!(tm_chap, "event");

    // P2 Stage3：event × arena 亦允许（arena 需平台指派主播 hostUserId）。
    seed_user(&state, "host-arena", None, "user", "active").await;
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "event赛事房", "roomType": "arena", "timelineMode": "event", "hostUserId": "host-arena" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "event × arena 应允许建房（Stage3 放宽）");
    let wid_arena = body["worldId"].as_str().unwrap().to_string();
    let tm_arena = sqlx::query("SELECT timeline_mode FROM worlds WHERE id=?")
        .bind(&wid_arena).fetch_one(&state.db).await.unwrap().try_get::<String, _>("timeline_mode").unwrap();
    assert_eq!(tm_arena, "event");
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

// ---------------- 数据看板：按天趋势（GET /admin/metrics/trends） ----------------
// 分桶口径与后端一致：UTC 日界、固定 86_400_000ms 桶宽——种子取 now-2天/now，恒落在窗口首/末桶，无跨日抖动。

const DAY_MS: i64 = 86_400_000;

/// 与后端同口径的 UTC 日标签（趋势断言用，对齐 runtime::day_string）。
fn utc_day(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).unwrap().format("%Y-%m-%d").to_string()
}

async fn ins_user_at(state: &AppState, id: &str, created_at: i64) {
    sqlx::query(
        "INSERT INTO users (id, phone, nickname, age_declared, role, status, created_at, updated_at) \
         VALUES (?, NULL, '趋势用户', 1, 'user', 'active', ?, ?)",
    )
    .bind(id).bind(created_at).bind(created_at)
    .execute(&state.db).await.unwrap();
}

async fn ins_tick_at(state: &AppState, id: &str, world: &str, tick_no: i64, tokens: i64, created_at: i64) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
         VALUES (?, ?, ?, 0, 'done', ?, ?)",
    )
    .bind(id).bind(world).bind(tick_no).bind(tokens).bind(created_at)
    .execute(&state.db).await.unwrap();
}

async fn ins_event_at(state: &AppState, id: &str, occurred_at: i64) {
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, occurred_at) VALUES (?, 'w_tr', 0, 0, ?, 'social', '[]', 'public', ?)",
    )
    .bind(id).bind(id).bind(occurred_at)
    .execute(&state.db).await.unwrap();
}

async fn ins_gift_at(state: &AppState, id: &str, cnt: i64, created_at: i64) {
    sqlx::query(
        "INSERT INTO gift_events (id, world_id, sku, gift_count, mapped, created_at) \
         VALUES (?, 'w_tr', 'rose', ?, 1, ?)",
    )
    .bind(id).bind(cnt).bind(created_at)
    .execute(&state.db).await.unwrap();
}

/// 带业务时间的复式分录（趋势按 postings.created_at 分桶；账户/journal 复用对账测试的 ins_account/ins_journal）。
async fn ins_posting_at(state: &AppState, id: &str, journal_id: &str, account_id: &str, delta: i64, created_at: i64) {
    sqlx::query("INSERT INTO ledger_postings (id, journal_id, account_id, delta_cents, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(id).bind(journal_id).bind(account_id).bind(delta).bind(created_at)
        .execute(&state.db).await.unwrap();
}

#[tokio::test]
async fn metrics_trends_buckets_by_day_and_zero_fills() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let now = now_ms();
    let day_a = now - 2 * DAY_MS; // 前天
    let day_b = now - DAY_MS; // 昨天：不播种，验证空天补零

    // 收入科目 + 对手方钱包（贷方净增只数 platform_revenue 行，钱包行不得计入）。
    ins_account(&state, "acct_rev_tr", "platform_revenue", None, 320).await;
    ins_account(&state, "acct_wal_tr", "user_wallet", Some("u_tr_pay"), -320).await;
    ins_journal(&state, "j_tr_a", "gift").await;
    ins_journal(&state, "j_tr_c", "gift").await;

    // ---- 前天：2 用户 / 同一世界 2 tick（100+50）/ 1 事件 / 礼物 3 / 平台收入 +120 ----
    ins_user_at(&state, "u_tr_a1", day_a).await;
    ins_user_at(&state, "u_tr_a2", day_a).await;
    ins_tick_at(&state, "t_tr_a1", "w_tr_a", 0, 100, day_a).await;
    ins_tick_at(&state, "t_tr_a2", "w_tr_a", 1, 50, day_a).await;
    ins_event_at(&state, "ev_tr_a1", day_a).await;
    ins_gift_at(&state, "g_tr_a1", 3, day_a).await;
    ins_posting_at(&state, "p_tr_a1", "j_tr_a", "acct_rev_tr", 120, day_a).await;
    ins_posting_at(&state, "p_tr_a2", "j_tr_a", "acct_wal_tr", -120, day_a).await;

    // ---- 今天：1 用户 / 两世界各 1 tick（30+70）/ 2 事件 / 礼物 5 / 收入 +300−100=200（净增）----
    ins_user_at(&state, "u_tr_c1", now).await;
    ins_tick_at(&state, "t_tr_c1", "w_tr_a", 2, 30, now).await;
    ins_tick_at(&state, "t_tr_c2", "w_tr_b", 0, 70, now).await;
    ins_event_at(&state, "ev_tr_c1", now).await;
    ins_event_at(&state, "ev_tr_c2", now).await;
    ins_gift_at(&state, "g_tr_c1", 5, now).await;
    ins_posting_at(&state, "p_tr_c1", "j_tr_c", "acct_rev_tr", 300, now).await;
    ins_posting_at(&state, "p_tr_c2", "j_tr_c", "acct_rev_tr", -100, now).await;
    ins_posting_at(&state, "p_tr_c3", "j_tr_c", "acct_wal_tr", -200, now).await;

    let (st, body) = get(&app, "/api/admin/metrics/trends?days=3", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let arr = body["days"].as_array().unwrap();
    assert_eq!(arr.len(), 3);

    // 按天升序 + UTC 日标签，含今天。
    assert_eq!(arr[0]["day"], utc_day(day_a).as_str());
    assert_eq!(arr[1]["day"], utc_day(day_b).as_str());
    assert_eq!(arr[2]["day"], utc_day(now).as_str());

    // 前天分桶正确。
    assert_eq!(arr[0]["newUsers"], 2);
    assert_eq!(arr[0]["activeWorlds"], 1, "同一世界两次 tick 只计一个活跃世界");
    assert_eq!(arr[0]["events"], 1);
    assert_eq!(arr[0]["tickTokens"], 150);
    assert_eq!(arr[0]["giftCount"], 3);
    assert_eq!(arr[0]["revenueCents"], 120);

    // 昨天无数据 → 全部补零。
    for k in ["newUsers", "activeWorlds", "events", "tickTokens", "giftCount", "revenueCents"] {
        assert_eq!(arr[1][k], 0, "空天 {k} 应补零: {:?}", arr[1]);
    }

    // 今天分桶正确；revenueCents 为贷方净增（+300−100），钱包对手方行不计。
    assert_eq!(arr[2]["newUsers"], 1);
    assert_eq!(arr[2]["activeWorlds"], 2);
    assert_eq!(arr[2]["events"], 2);
    assert_eq!(arr[2]["tickTokens"], 100);
    assert_eq!(arr[2]["giftCount"], 5);
    assert_eq!(arr[2]["revenueCents"], 200);
}

#[tokio::test]
async fn metrics_trends_days_clamp_and_default() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    // 缺省 → 14 天。
    let (st, body) = get(&app, "/api/admin/metrics/trends", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["days"].as_array().unwrap().len(), 14);
    // 空库：每天各指标恒为 0（含 revenueCents 字段存在且为 0）。
    for d in body["days"].as_array().unwrap() {
        assert_eq!(d["newUsers"], 0);
        assert_eq!(d["revenueCents"], 0);
    }

    // 下限 clamp：0 → 1 天，且唯一一天即今天（UTC）。
    let (st, body) = get(&app, "/api/admin/metrics/trends?days=0", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let arr = body["days"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["day"], utc_day(now_ms()).as_str());

    // 上限 clamp：999 → 60 天。
    let (st, body) = get(&app, "/api/admin/metrics/trends?days=999", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["days"].as_array().unwrap().len(), 60);
}

#[tokio::test]
async fn metrics_trends_role_gate_operator_finance_admin() {
    // operator/finance/admin 放行；support/reviewer/user 越权 403；无 token 401（对齐 metrics_overview gate）。
    let state = test_state().await;
    let app = build_router(state.clone());
    assert_eq!(get(&app, "/api/admin/metrics/trends", Some(&admin_token(&state))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/metrics/trends", Some(&role_token(&state, "operator"))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/metrics/trends", Some(&role_token(&state, "finance"))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/metrics/trends", Some(&role_token(&state, "support"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/metrics/trends", Some(&role_token(&state, "reviewer"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/metrics/trends", Some(&user_token(&state))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/metrics/trends", None).await.0, StatusCode::UNAUTHORIZED);
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

// ---------------- 财务对账（P4：GET /admin/ledger/reconcile） ----------------
// 复式账本表恒存在（0013 迁移不随 feature 门控），故经 raw SQL 播种、直接核验对账 SQL，与 feature 无关。

async fn ins_account(state: &AppState, id: &str, kind: &str, owner: Option<&str>, balance: i64) {
    sqlx::query(
        "INSERT INTO ledger_accounts (id, kind, owner_id, scope_id, balance_cents, withdrawable, created_at, updated_at) \
         VALUES (?, ?, ?, NULL, ?, 0, ?, ?)",
    )
    .bind(id).bind(kind).bind(owner).bind(balance).bind(now_ms()).bind(now_ms())
    .execute(&state.db).await.unwrap();
}

async fn ins_journal(state: &AppState, id: &str, reason: &str) {
    sqlx::query("INSERT INTO ledger_journals (id, reason, ref_kind, ref_id, world_id, created_at) VALUES (?, ?, 'x', 'x', NULL, ?)")
        .bind(id).bind(reason).bind(now_ms()).execute(&state.db).await.unwrap();
}

async fn ins_posting(state: &AppState, id: &str, journal_id: &str, account_id: &str, delta: i64) {
    sqlx::query("INSERT INTO ledger_postings (id, journal_id, account_id, delta_cents, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(id).bind(journal_id).bind(account_id).bind(delta).bind(now_ms()).execute(&state.db).await.unwrap();
}

async fn ins_billing(state: &AppState, uid: &str, balance: i64) {
    sqlx::query("INSERT INTO billing_balances (user_id, balance_cents, updated_at) VALUES (?, ?, ?)")
        .bind(uid).bind(balance).bind(now_ms()).execute(&state.db).await.unwrap();
}

/// 播种一组平衡账本：recharge(wallet+2000/source-2000) + gift(wallet-1000/creator+700/platform+300)。
/// 账户物化余额与 postings 之和一致；user_wallet(u1)=1000 与 billing_balances(u1)=1000 恒等。
async fn seed_balanced_ledger(state: &AppState) {
    ins_account(state, "acct_wallet_u1", "user_wallet", Some("u1"), 1000).await; // 2000-1000
    ins_account(state, "acct_platform_recharge_source", "platform_recharge_source", None, -2000).await;
    ins_account(state, "acct_creator_c1", "creator_earnings", Some("c1"), 700).await;
    ins_account(state, "acct_platform_revenue", "platform_revenue", None, 300).await;
    ins_billing(state, "u1", 1000).await;

    ins_journal(state, "j_recharge", "recharge").await;
    ins_posting(state, "p1", "j_recharge", "acct_wallet_u1", 2000).await;
    ins_posting(state, "p2", "j_recharge", "acct_platform_recharge_source", -2000).await;

    ins_journal(state, "j_gift", "gift").await;
    ins_posting(state, "p3", "j_gift", "acct_wallet_u1", -1000).await;
    ins_posting(state, "p4", "j_gift", "acct_creator_c1", 700).await;
    ins_posting(state, "p5", "j_gift", "acct_platform_revenue", 300).await;
}

#[tokio::test]
async fn ledger_reconcile_role_gate_finance_and_admin_only() {
    // finance/admin 放行；operator/reviewer/support/user 越权 403；无 token 401（对齐 economy_overview gate）。
    let state = test_state().await;
    let app = build_router(state.clone());
    assert_eq!(get(&app, "/api/admin/ledger/reconcile", Some(&admin_token(&state))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/ledger/reconcile", Some(&role_token(&state, "finance"))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/ledger/reconcile", Some(&role_token(&state, "operator"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/ledger/reconcile", Some(&role_token(&state, "reviewer"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/ledger/reconcile", Some(&role_token(&state, "support"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/ledger/reconcile", Some(&user_token(&state))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/ledger/reconcile", None).await.0, StatusCode::UNAUTHORIZED);

    // 空账本 → 平衡，全账 SUM=0。
    let (st, body) = get(&app, "/api/admin/ledger/reconcile", Some(&role_token(&state, "finance"))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["balanced"], true);
    assert_eq!(body["globalPostingSumCents"], 0);
}

#[tokio::test]
async fn ledger_reconcile_verifies_sum_zero_and_detects_imbalance() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let finance = role_token(&state, "finance");

    // 平衡账本 → balanced=true：全账 SUM=0、无不平 journal、账户物化余额一致、wallet==billing。
    seed_balanced_ledger(&state).await;
    let (st, body) = get(&app, "/api/admin/ledger/reconcile", Some(&finance)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["balanced"], true, "平衡账本必须判为 balanced，body={body}");
    assert_eq!(body["globalPostingSumCents"], 0, "全账复式恒等：SUM(postings)=0");
    assert_eq!(body["journals"]["unbalanced"], 0);
    assert_eq!(body["accounts"]["mismatched"], 0);
    assert_eq!(body["walletBillingIdentity"]["mismatched"], 0, "user_wallet==billing_balances 恒等");

    // 注入一条不平 journal（单边 +50，不同步账户余额）→ 破坏两项不变量：
    //   全账 SUM≠0 且该 journal 不平；wallet 账户余额(1000) ≠ 其 postings 之和(1050)。
    ins_journal(&state, "j_bad", "gift").await;
    ins_posting(&state, "p_bad", "j_bad", "acct_wallet_u1", 50).await;

    let (st, body) = get(&app, "/api/admin/ledger/reconcile", Some(&finance)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["balanced"], false, "破坏后必须判为不平衡，body={body}");
    assert_eq!(body["globalPostingSumCents"], 50, "全账 SUM 应暴露 +50 缺口");
    assert_eq!(body["journals"]["unbalanced"], 1, "应检出 1 条不平 journal");
    assert!(
        body["journals"]["unbalancedIds"].as_array().unwrap().iter().any(|v| v == "j_bad"),
        "不平 journal 应列出 j_bad 供 triage"
    );
    assert_eq!(body["accounts"]["mismatched"], 1, "wallet 账户物化余额应与 postings 之和不符");
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

// ---------------- 内容风控申诉复审（moderation_appeals） ----------------

/// 播种云端角色（card_json 带 identity.name，供申诉列表主体摘要断言）。
async fn seed_character(
    state: &AppState,
    id: &str,
    owner: &str,
    name: &str,
    moderation: &str,
    avatar_moderation: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, avatar_moderation, created_at) \
         VALUES (?, ?, 'loc', 1, ?, 'original', ?, 0, ?, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(format!("{{\"identity\":{{\"name\":\"{name}\"}}}}"))
    .bind(moderation)
    .bind(avatar_moderation)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_appeal(state: &AppState, id: &str, subject_id: &str, owner: &str, status: &str, created_at: i64) {
    sqlx::query(
        "INSERT INTO moderation_appeals (id, subject_kind, subject_id, owner_id, appeal_text, status, created_at) \
         VALUES (?, 'character', ?, ?, '申诉正文', ?, ?)",
    )
    .bind(id)
    .bind(subject_id)
    .bind(owner)
    .bind(status)
    .bind(created_at)
    .execute(&state.db)
    .await
    .unwrap();
}

/// 人审 reject 的理由须同步落 audit_queue.reject_reason（用户侧回显用）；approve 保持 NULL。
#[tokio::test]
async fn review_reject_writes_reject_reason_to_queue_row() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    seed_character(&state, "ch_rr", "uRR", "被驳者", "pending", None).await;
    seed_character(&state, "ch_ok", "uRR", "过审者", "pending", None).await;
    for (aq, subject) in [("aq_rr", "ch_rr"), ("aq_ok", "ch_ok")] {
        sqlx::query(
            "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, status, created_at) \
             VALUES (?, 'character', ?, 'flagged', 'open', ?)",
        )
        .bind(aq).bind(subject).bind(now_ms()).execute(&state.db).await.unwrap();
    }

    // reject?reason=含违禁词（%E5%90%AB%E8%BF%9D%E7%A6%81%E8%AF%8D）→ 理由落队列行。
    let (st, _) = post(
        &app,
        "/api/admin/audit-queue/aq_rr/reject?reason=%E5%90%AB%E8%BF%9D%E7%A6%81%E8%AF%8D",
        Some(&admin),
        json!({}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let reason = sqlx::query("SELECT reject_reason FROM audit_queue WHERE id='aq_rr'")
        .fetch_one(&state.db).await.unwrap().try_get::<Option<String>, _>("reject_reason").unwrap();
    assert_eq!(reason.as_deref(), Some("含违禁词"), "reject 理由应落 audit_queue.reject_reason");

    // approve 不写 reject_reason（保持 NULL）；现有回写/留痕行为不回退。
    let (st, _) = post(&app, "/api/admin/audit-queue/aq_ok/approve?reason=ok", Some(&admin), json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let reason = sqlx::query("SELECT reject_reason FROM audit_queue WHERE id='aq_ok'")
        .fetch_one(&state.db).await.unwrap().try_get::<Option<String>, _>("reject_reason").unwrap();
    assert!(reason.is_none(), "approve 不得写 reject_reason");
    let m = sqlx::query("SELECT moderation FROM cloud_characters WHERE id='ch_rr'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "rejected", "reject 仍回写主体 moderation");
}

/// 列表：默认只出 pending；status 过滤 / all；含主体摘要（名字/moderation/avatar_moderation/owner）；非法 status → 400。
#[tokio::test]
async fn appeals_list_filters_status_and_returns_subject_summary() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let now = now_ms();

    seed_character(&state, "ch_ap1", "uA", "阿黎", "rejected", None).await;
    seed_character(&state, "ch_ap2", "uB", "沈镜", "approved", Some("rejected")).await;
    seed_appeal(&state, "apl_1", "ch_ap1", "uA", "pending", now).await;
    seed_appeal(&state, "apl_2", "ch_ap2", "uB", "upheld", now - 1000).await;

    // 默认 pending。
    let (st, body) = get(&app, "/api/admin/appeals", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "默认只列 pending: {items:?}");
    assert_eq!(items[0]["id"], "apl_1");
    assert_eq!(items[0]["subjectKind"], "character");
    assert_eq!(items[0]["appealText"], "申诉正文");
    // 主体摘要。
    assert_eq!(items[0]["subject"]["name"], "阿黎");
    assert_eq!(items[0]["subject"]["moderation"], "rejected");
    assert!(items[0]["subject"]["avatarModeration"].is_null());
    assert_eq!(items[0]["subject"]["ownerId"], "uA");

    // status=upheld 过滤。
    let (st, body) = get(&app, "/api/admin/appeals?status=upheld", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "apl_2");
    assert_eq!(items[0]["subject"]["avatarModeration"], "rejected");

    // all：两条，新在前。
    let (st, body) = get(&app, "/api/admin/appeals?status=all", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "apl_1");
    assert_eq!(items[1]["id"], "apl_2");

    // 非法 status → 400。
    let (st, _) = get(&app, "/api/admin/appeals?status=bogus", Some(&admin)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// overturn 只翻转「当时处于 rejected 的那个维度」：卡驳回改卡；仅头像驳回改头像；
/// 双驳回时卡优先、头像不顺带放行。留痕 + 申诉行落结论 + 重复 resolve → 409。
#[tokio::test]
async fn appeal_resolve_overturn_flips_only_rejected_dimension_and_audits() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let now = now_ms();

    // 场景一：卡 rejected → overturn → 卡 approved。
    seed_character(&state, "ch_ov1", "uO", "翻案者", "rejected", None).await;
    seed_appeal(&state, "apl_ov1", "ch_ov1", "uO", "pending", now).await;
    let (st, body) = post(
        &app,
        "/api/admin/appeals/apl_ov1/resolve",
        Some(&admin),
        json!({ "decision": "overturn", "reason": "复核为误判，改判通过。" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "overturned");
    assert_eq!(body["resolutionReason"], "复核为误判，改判通过。");
    assert_eq!(body["reviewerId"], "admin1");
    assert!(body["resolvedAt"].is_number());
    let m = sqlx::query("SELECT moderation FROM cloud_characters WHERE id='ch_ov1'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "approved", "overturn 应把 rejected 卡改为 approved");
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM audit_logs WHERE action='appeal_overturn'").await, 1);

    // 重复 resolve → 409（非 pending 不可再裁决）。
    let (st, _) = post(
        &app,
        "/api/admin/appeals/apl_ov1/resolve",
        Some(&admin),
        json!({ "decision": "uphold", "reason": "再裁一次" }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // 场景二：卡 approved、头像 rejected → overturn 只改头像维度。
    seed_character(&state, "ch_ov2", "uO", "头像翻案", "approved", Some("rejected")).await;
    seed_appeal(&state, "apl_ov2", "ch_ov2", "uO", "pending", now).await;
    let (st, _) = post(
        &app,
        "/api/admin/appeals/apl_ov2/resolve",
        Some(&admin),
        json!({ "decision": "overturn", "reason": "头像复核通过。" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let row = sqlx::query("SELECT moderation, avatar_moderation FROM cloud_characters WHERE id='ch_ov2'")
        .fetch_one(&state.db).await.unwrap();
    assert_eq!(row.try_get::<String, _>("moderation").unwrap(), "approved");
    assert_eq!(row.try_get::<Option<String>, _>("avatar_moderation").unwrap().as_deref(), Some("approved"));

    // 场景三：卡与头像同为 rejected → 只翻卡（卡优先），头像不顺带放行。
    seed_character(&state, "ch_ov3", "uO", "双驳者", "rejected", Some("rejected")).await;
    seed_appeal(&state, "apl_ov3", "ch_ov3", "uO", "pending", now).await;
    let (st, _) = post(
        &app,
        "/api/admin/appeals/apl_ov3/resolve",
        Some(&admin),
        json!({ "decision": "overturn", "reason": "卡文案复核通过。" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let row = sqlx::query("SELECT moderation, avatar_moderation FROM cloud_characters WHERE id='ch_ov3'")
        .fetch_one(&state.db).await.unwrap();
    assert_eq!(row.try_get::<String, _>("moderation").unwrap(), "approved", "卡维度翻转");
    assert_eq!(
        row.try_get::<Option<String>, _>("avatar_moderation").unwrap().as_deref(),
        Some("rejected"),
        "头像维度不得顺带放行"
    );
}

/// uphold 维持原判：moderation 不动、申诉行落 upheld、留痕。
#[tokio::test]
async fn appeal_resolve_uphold_keeps_moderation_and_audits() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    seed_character(&state, "ch_up", "uU", "维持者", "rejected", None).await;
    seed_appeal(&state, "apl_up", "ch_up", "uU", "pending", now_ms()).await;

    let (st, body) = post(
        &app,
        "/api/admin/appeals/apl_up/resolve",
        Some(&admin),
        json!({ "decision": "uphold", "reason": "复核确认违规，维持原判。" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "upheld");
    let m = sqlx::query("SELECT moderation FROM cloud_characters WHERE id='ch_up'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "rejected", "uphold 后 moderation 必须仍为 rejected");
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM audit_logs WHERE action='appeal_uphold'").await, 1);
}

/// resolve 入参校验：未知申诉 404；decision 非法 400；reason 空/超 500 字符 400（校验失败不落任何变更）。
#[tokio::test]
async fn appeal_resolve_validation_and_not_found() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    seed_character(&state, "ch_val", "uV", "校验者", "rejected", None).await;
    seed_appeal(&state, "apl_val", "ch_val", "uV", "pending", now_ms()).await;

    let (st, _) = post(&app, "/api/admin/appeals/nope/resolve", Some(&admin), json!({ "decision": "uphold", "reason": "x" })).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = post(&app, "/api/admin/appeals/apl_val/resolve", Some(&admin), json!({ "decision": "maybe", "reason": "x" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = post(&app, "/api/admin/appeals/apl_val/resolve", Some(&admin), json!({ "decision": "uphold", "reason": "  " })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = post(
        &app,
        "/api/admin/appeals/apl_val/resolve",
        Some(&admin),
        json!({ "decision": "uphold", "reason": "长".repeat(501) }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 全部被拒后：申诉仍 pending、主体 moderation 未动。
    let s = sqlx::query("SELECT status FROM moderation_appeals WHERE id='apl_val'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "pending");
    let m = sqlx::query("SELECT moderation FROM cloud_characters WHERE id='ch_val'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "rejected");
}

/// RBAC：申诉列表与 resolve 仅 reviewer/admin；support/operator/finance/user 越权 403，无 token 401。
#[tokio::test]
async fn appeals_rbac_reviewer_and_admin_only() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_character(&state, "ch_rb", "uR", "权限者", "rejected", None).await;
    seed_appeal(&state, "apl_rb", "ch_rb", "uR", "pending", now_ms()).await;

    // 列表。
    assert_eq!(get(&app, "/api/admin/appeals", Some(&admin_token(&state))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/appeals", Some(&role_token(&state, "reviewer"))).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/appeals", Some(&role_token(&state, "support"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/appeals", Some(&role_token(&state, "operator"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/appeals", Some(&role_token(&state, "finance"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/appeals", Some(&user_token(&state))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/appeals", None).await.0, StatusCode::UNAUTHORIZED);

    // resolve：越权角色带合法 body 仍 403（校验前先过角色门），且申诉保持 pending。
    let body = json!({ "decision": "overturn", "reason": "越权尝试" });
    for role in ["support", "operator", "finance"] {
        let (st, _) = post(&app, "/api/admin/appeals/apl_rb/resolve", Some(&role_token(&state, role)), body.clone()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{role} 不得 resolve");
    }
    assert_eq!(post(&app, "/api/admin/appeals/apl_rb/resolve", Some(&user_token(&state)), body.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post(&app, "/api/admin/appeals/apl_rb/resolve", None, body.clone()).await.0, StatusCode::UNAUTHORIZED);
    let s = sqlx::query("SELECT status FROM moderation_appeals WHERE id='apl_rb'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("status").unwrap();
    assert_eq!(s, "pending", "越权请求不得改动申诉");
    let m = sqlx::query("SELECT moderation FROM cloud_characters WHERE id='ch_rb'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "rejected", "越权请求不得改判");

    // reviewer 放行并成为唯一改判路径。
    let (st, ok) = post(&app, "/api/admin/appeals/apl_rb/resolve", Some(&role_token(&state, "reviewer")), json!({ "decision": "overturn", "reason": "复核通过" })).await;
    assert_eq!(st, StatusCode::OK, "{ok:?}");
    assert_eq!(ok["reviewerId"], "actor_reviewer");
    let m = sqlx::query("SELECT moderation FROM cloud_characters WHERE id='ch_rb'")
        .fetch_one(&state.db).await.unwrap().try_get::<String, _>("moderation").unwrap();
    assert_eq!(m, "approved");
}
