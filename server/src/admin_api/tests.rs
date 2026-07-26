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

// ---------------- 建房 lethality（R1 生死契约三档，总规格 §11【拍板 24】） ----------------

/// 建房参数须暴露契约档，且**星级不自动决定档位**（规格要求可分离、可配置）。
/// 生死状档另受运营开关把守：开关未开时建房直接 400（未验证功能默认关闭，VALIDATION.md §0.1）。
#[tokio::test]
async fn world_create_lethality_option() {
    use crate::worlds::DeathmatchSwitch;
    let _sw = DeathmatchSwitch::set(false);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    async fn stored(state: &AppState, wid: &str) -> String {
        sqlx::query("SELECT lethality FROM worlds WHERE id=?")
            .bind(wid)
            .fetch_one(&state.db)
            .await
            .unwrap()
            .try_get::<String, _>("lethality")
            .unwrap()
    }

    // 省略 lethality → consent（同意制，老行为不变）。
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "默认档", "roomType": "idle" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let wid = body["worldId"].as_str().unwrap().to_string();
    assert_eq!(stored(&state, &wid).await, "consent");
    assert_eq!(body["lethality"], "consent", "回执须回显契约档");

    // 显式 sanctuary（庇护场）落库；开关无关。
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "庇护场", "lethality": "sanctuary" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let wid_s = body["worldId"].as_str().unwrap().to_string();
    assert_eq!(stored(&state, &wid_s).await, "sanctuary");

    // 非法枚举 → 400。
    let (st, _) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "非法档", "lethality": "hardcore" }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 🔴 开关未开 → 生死场建不出来（前门拒绝）。
    let (st, err) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "生死场", "lethality": "deathmatch" }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "开关未开不得建生死场: {err}");
    assert!(
        err["error"]["message"].as_str().unwrap_or("").contains("生死状档尚未开启"),
        "文案须说明是运营开关未开: {err}"
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) AS n FROM worlds WHERE lethality='deathmatch'").await,
        0,
        "被拒不得落库"
    );
}

#[tokio::test]
async fn world_create_deathmatch_allowed_when_switch_on() {
    use crate::worlds::DeathmatchSwitch;
    let _sw = DeathmatchSwitch::set(true);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl1", "templateVersion": 1, "title": "生死场", "roomType": "idle", "lethality": "deathmatch" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "开关已开应可建生死场: {body}");
    let wid = body["worldId"].as_str().unwrap().to_string();
    assert_eq!(body["lethality"], "deathmatch");

    // 建房留痕带契约档（高风险配置须可溯）。
    let n = count(
        &state,
        &format!(
            "SELECT COUNT(*) AS n FROM audit_logs WHERE action='world.create' AND subject='{wid}' \
             AND reason LIKE '%lethality=deathmatch%'"
        ),
    )
    .await;
    assert_eq!(n, 1, "建生死场须在 audit_logs 记下档位");

    // 诊断视图同时给出落库值与生效档（运营据此判断急停阀是否在生效）。
    let (st, diag) = get(&app, &format!("/api/admin/worlds/{wid}/diagnostics"), Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(diag["world"]["lethality"], "deathmatch");
    assert_eq!(diag["world"]["effectiveLethality"], "deathmatch");
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

// ---------------- R1：Saga 归组字段（总规格 §3） ----------------

/// 建模板时的 Saga 归组校验：saga_id 与 stage_no 必须成对，且阶段序号有范围。
/// 不传两者 = 独立模板（默认路径），必须与本字段落地前行为完全一致。
#[tokio::test]
async fn create_template_saga_pairing_rules() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let create = |body: Value| {
        let app = app.clone();
        let admin = admin.clone();
        async move { post(&app, "/api/admin/world-templates", Some(&admin), body).await }
    };
    let skeleton = json!({ "mainlineNodes": [] });

    // 1) 完全不传 saga 字段 → 独立模板，落库为 ''/0（向后兼容，老调用方零改动）。
    let (st, body) = create(json!({
        "title": "独立模板", "roomType": "chapter", "skeletonJson": skeleton
    })).await;
    assert_eq!(st, StatusCode::OK, "不传 saga 字段应通过: {body}");
    let tpl_id = body["templateId"].as_str().unwrap().to_string();
    let row = sqlx::query("SELECT saga_id, stage_no FROM world_templates WHERE id = ?")
        .bind(&tpl_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(row.try_get::<String, _>("saga_id").unwrap(), "");
    assert_eq!(row.try_get::<i64, _>("stage_no").unwrap(), 0);

    // 2) 只给 stageNo 不给 sagaId → 400（孤儿阶段，阶段列表页无法归组）。
    let (st, _) = create(json!({
        "title": "孤儿阶段", "roomType": "chapter", "skeletonJson": skeleton, "stageNo": 3
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "只给 stageNo 应拒绝");

    // 3) 给 sagaId 但 stageNo 缺省（0）→ 400（有系列无阶段）。
    let (st, _) = create(json!({
        "title": "无阶段号", "roomType": "chapter", "skeletonJson": skeleton, "sagaId": "saga_dp"
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "有 sagaId 无 stageNo 应拒绝");

    // 4) stageNo 越界 → 400（防运营把字数误填成阶段号）。
    let (st, _) = create(json!({
        "title": "越界阶段", "roomType": "chapter", "skeletonJson": skeleton,
        "sagaId": "saga_dp", "stageNo": 1000
    })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "stageNo 超上限应拒绝");

    // 5) 成对且合法 → 通过。
    let (st, body) = create(json!({
        "title": "斗罗·史莱克篇", "roomType": "chapter", "skeletonJson": skeleton,
        "sagaId": "saga_dp", "stageNo": 2
    })).await;
    assert_eq!(st, StatusCode::OK, "成对合法应通过: {body}");
}

/// 阶段列表：按 sagaId 筛选时只返回该系列，且按 stage_no 升序（剧情顺序，不是录入时间）。
#[tokio::test]
async fn list_templates_by_saga_orders_by_stage() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let create = |body: Value| {
        let app = app.clone();
        let admin = admin.clone();
        async move { post(&app, "/api/admin/world-templates", Some(&admin), body).await }
    };
    let skeleton = json!({ "mainlineNodes": [] });

    // 故意逆序录入（阶段 3 先建、阶段 1 后建），验证排序不是按 created_at。
    for (title, stage) in [("第三篇", 3), ("第一篇", 1), ("第二篇", 2)] {
        let (st, _) = create(json!({
            "title": title, "roomType": "chapter", "skeletonJson": skeleton,
            "sagaId": "saga_a", "stageNo": stage
        })).await;
        assert_eq!(st, StatusCode::OK);
    }
    // 另一个系列 + 一个独立模板，验证筛选隔离。
    let (st, _) = create(json!({
        "title": "别的系列", "roomType": "chapter", "skeletonJson": skeleton,
        "sagaId": "saga_b", "stageNo": 1
    })).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = create(json!({
        "title": "独立的", "roomType": "chapter", "skeletonJson": skeleton
    })).await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = get(&app, "/api/admin/world-templates?sagaId=saga_a", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let items = body["templates"].as_array().unwrap();
    assert_eq!(items.len(), 3, "只应返回 saga_a 的三个阶段: {body}");
    let stages: Vec<i64> = items.iter().map(|t| t["stageNo"].as_i64().unwrap()).collect();
    assert_eq!(stages, vec![1, 2, 3], "阶段应按剧情顺序升序");
    let titles: Vec<&str> = items.iter().map(|t| t["title"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["第一篇", "第二篇", "第三篇"]);
    for t in items {
        assert_eq!(t["sagaId"], "saga_a");
    }
    // 阶段列表不分页（阶段数由剧情结构决定，量级十几个）。
    assert!(body["nextCursor"].is_null(), "阶段列表不应给游标");

    // 不传 sagaId → 全量列表，仍含独立模板与两个系列，且投影带上新字段。
    let (st, body) = get(&app, "/api/admin/world-templates", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let all = body["templates"].as_array().unwrap();
    assert_eq!(all.len(), 5, "全量应含 5 个模板");
    assert!(all.iter().any(|t| t["sagaId"] == "" && t["stageNo"] == 0), "独立模板应在全量列表里");
}

// ================= R1 成本仪表（总规格 §17【拍板 16】） =================
// 日界口径与后端一致：UTC 日界 + 固定 86_400_000ms 桶宽；种子取 now / now-1天 / now-2天，
// 恒落在窗口对应桶，无跨日抖动（同 metrics_trends 测试的做法）。

/// 建一个最小可列出的世界行（只填 NOT NULL 且无默认值的列，其余走建表默认）。
async fn ins_world(state: &AppState, id: &str, status: &str, created_at: i64) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, created_at, updated_at) \
         VALUES (?, 'tpl_cost', 1, 'e1', 'p1', 'm1', 'idle', '成本世界', ?, ?, ?)",
    )
    .bind(id)
    .bind(status)
    .bind(created_at)
    .bind(created_at)
    .execute(&state.db)
    .await
    .unwrap();
}

/// 带状态的 tick（成功率口径需要 done/failed/pending 三种）。
async fn ins_tick_st(
    state: &AppState,
    id: &str,
    world: &str,
    tick_no: i64,
    tokens: i64,
    status: &str,
    created_at: i64,
) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
         VALUES (?, ?, ?, 0, ?, ?, ?)",
    )
    .bind(id)
    .bind(world)
    .bind(tick_no)
    .bind(status)
    .bind(tokens)
    .bind(created_at)
    .execute(&state.db)
    .await
    .unwrap();
}

async fn ins_member(state: &AppState, id: &str, world: &str, user: &str, status: &str) {
    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, status, joined_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(world)
    .bind(user)
    .bind(format!("cc_{id}"))
    .bind(status)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 今日成本 + 近 N 日趋势：UTC 日界分桶、空天补零、跨日界不串账。
#[tokio::test]
async fn cost_meter_today_and_trend_respect_utc_day_boundary() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let now = now_ms();
    let day_a = now - 2 * DAY_MS; // 前天
                                  // 昨天故意不播种：验证空天补零

    // 前天：w_cost_a 花 1000；今天：w_cost_a 花 300 + w_cost_b 花 700。
    ins_tick_st(&state, "tc_a1", "w_cost_a", 0, 1000, "done", day_a).await;
    ins_tick_st(&state, "tc_a2", "w_cost_a", 1, 300, "done", now).await;
    ins_tick_st(&state, "tc_b1", "w_cost_b", 0, 700, "done", now).await;

    let (st, m) = get(&app, "/api/admin/metrics/overview?costDays=3", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let cost = &m["cost"];

    // 单价：与 runtime 熔断同源的 env 参数；未设 env 时为默认 2 分/千 token。
    let price = cost["centsPer1kTokens"].as_i64().unwrap();
    assert!(price > 0, "单价必须为正");
    if std::env::var_os("MUSE_TOKEN_CNY_CENTS_PER_1K").is_none() {
        assert_eq!(price, 2, "默认单价须与 runtime::DEFAULT_TOKEN_CNY_CENTS_PER_1K 同值");
    }

    // 今日成本：只含今天两笔（300+700），前天的 1000 不得串进来。
    assert_eq!(cost["today"]["day"], utc_day(now));
    assert_eq!(cost["today"]["tokens"], 1000);
    assert_eq!(cost["today"]["cents"], 1000 * price / 1000);

    // 趋势：3 天升序 [前天, 昨天(补零), 今天]。
    let trend = cost["trend"].as_array().unwrap();
    assert_eq!(trend.len(), 3, "costDays=3 应给 3 天");
    assert_eq!(cost["trendDays"], 3);
    assert_eq!(trend[0]["day"], utc_day(day_a));
    assert_eq!(trend[0]["tokens"], 1000);
    assert_eq!(trend[1]["tokens"], 0, "无数据的天须补零");
    assert_eq!(trend[2]["day"], utc_day(now));
    assert_eq!(trend[2]["tokens"], 1000);
    assert_eq!(trend[0]["cents"], 1000 * price / 1000);

    // 全量累计（不受趋势窗口影响）。
    assert_eq!(cost["total"]["tokens"], 2000);
    assert_eq!(cost["total"]["cents"], 2000 * price / 1000);
    assert_eq!(cost["total"]["worlds"], 2);

    // costDays 缺省 → 7 天；clamp 上下界。
    let (_, m) = get(&app, "/api/admin/metrics/overview", Some(&admin)).await;
    assert_eq!(m["cost"]["trendDays"], 7, "默认窗口 7 天");
    assert_eq!(m["cost"]["trend"].as_array().unwrap().len(), 7);
    let (_, m) = get(&app, "/api/admin/metrics/overview?costDays=999", Some(&admin)).await;
    assert_eq!(m["cost"]["trendDays"], 60, "上界 clamp 到 60");
    let (_, m) = get(&app, "/api/admin/metrics/overview?costDays=0", Some(&admin)).await;
    assert_eq!(m["cost"]["trendDays"], 1, "下界 clamp 到 1");
}

/// 每局成本 + 每玩家分摊：按 active 成员人均等分；无成员世界不除零（perPlayer 为 null）。
#[tokio::test]
async fn cost_meter_per_player_equal_split_and_no_divide_by_zero() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let now = now_ms();
    // w_pp_a：2_000_000 token，4 名 active + 1 名 left（left 不进分母）。
    ins_tick_st(&state, "tp_a1", "w_pp_a", 0, 1_500_000, "done", now).await;
    ins_tick_st(&state, "tp_a2", "w_pp_a", 1, 500_000, "done", now).await;
    for i in 0..4 {
        ins_member(&state, &format!("mem_a{i}"), "w_pp_a", &format!("u_a{i}"), "active").await;
    }
    ins_member(&state, "mem_a_left", "w_pp_a", "u_a_left", "left").await;
    // w_pp_b：1_000_000 token，0 名成员（全员退出/尚未有人加入）。
    ins_tick_st(&state, "tp_b1", "w_pp_b", 0, 1_000_000, "done", now).await;
    // w_pp_c：有成员但从未跑过 tick → 不进成本榜，其成员也不得稀释平台均值分母。
    ins_member(&state, "mem_c0", "w_pp_c", "u_c0", "active").await;

    let (st, m) = get(&app, "/api/admin/metrics/overview", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let cost = &m["cost"];
    let price = cost["centsPer1kTokens"].as_i64().unwrap();
    assert_eq!(cost["allocation"], "per_member_equal_split", "分摊口径须自述");

    // 榜单按累计 token 降序：a(2M) 在 b(1M) 前；从未跑过的 c 不在榜。
    let by_world = cost["byWorld"].as_array().unwrap();
    assert_eq!(by_world.len(), 2, "只有产生过 tick 的世界进成本榜: {cost}");
    assert_eq!(by_world[0]["worldId"], "w_pp_a");
    assert_eq!(by_world[1]["worldId"], "w_pp_b");

    // 每局成本 + 每玩家等分：2_000_000 / 4 = 500_000 token/人。
    let a_cents = 2_000_000 * price / 1000;
    assert_eq!(by_world[0]["tokens"], 2_000_000);
    assert_eq!(by_world[0]["cents"], a_cents);
    assert_eq!(by_world[0]["cny"].as_f64().unwrap(), a_cents as f64 / 100.0);
    assert_eq!(by_world[0]["activeMembers"], 4, "left 成员不计入在场");
    assert_eq!(by_world[0]["tokensPerPlayer"].as_f64().unwrap(), 500_000.0);
    assert_eq!(by_world[0]["centsPerPlayer"].as_f64().unwrap(), a_cents as f64 / 4.0);

    // 无成员世界：不除零、不编 0，perPlayer 为 null。
    assert_eq!(by_world[1]["activeMembers"], 0);
    assert!(by_world[1]["tokensPerPlayer"].is_null(), "0 成员不得除零");
    assert!(by_world[1]["centsPerPlayer"].is_null(), "0 成员不得除零");
    assert_eq!(by_world[1]["tokens"], 1_000_000, "成本本身照常统计");

    // 平台合计：分母只含有 tick 记账的世界（w_pp_c 的成员不进分母）。
    let total = &cost["total"];
    assert_eq!(total["tokens"], 3_000_000);
    assert_eq!(total["activeMembers"], 4, "从未跑过 tick 的世界成员不得稀释均值分母");
    assert_eq!(total["tokensPerPlayer"].as_f64().unwrap(), 750_000.0);
    let total_cents = 3_000_000 * price / 1000;
    assert_eq!(total["cents"], total_cents);
    assert_eq!(total["centsPerPlayer"].as_f64().unwrap(), total_cents as f64 / 4.0);

    // 兼容字段仍在，且与新字段同源。
    assert_eq!(m["tokenCostByWorld"][0]["worldId"], "w_pp_a");
    assert_eq!(m["tokenCostByWorld"][0]["tokens"], 2_000_000);
}

/// 世界监控列表补充列：在场人数 / 成功率 / 今日成本；moderationLatency 无数据源故不下发。
#[tokio::test]
async fn worlds_list_participant_success_rate_and_today_cost() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let now = now_ms();
    let yesterday = now - DAY_MS;
    ins_world(&state, "w_ops_1", "running", now).await;
    ins_world(&state, "w_ops_2", "open", now - 1000).await;

    // w_ops_1：3 active + 1 left；今天 3 done(各 200_000) + 1 failed(0) + 1 pending(0)，昨天 1 done(900_000)。
    for i in 0..3 {
        ins_member(&state, &format!("mem_o{i}"), "w_ops_1", &format!("u_o{i}"), "active").await;
    }
    ins_member(&state, "mem_o_left", "w_ops_1", "u_o_left", "left").await;
    ins_tick_st(&state, "to_1", "w_ops_1", 1, 200_000, "done", now).await;
    ins_tick_st(&state, "to_2", "w_ops_1", 2, 200_000, "done", now).await;
    ins_tick_st(&state, "to_3", "w_ops_1", 3, 200_000, "done", now).await;
    ins_tick_st(&state, "to_4", "w_ops_1", 4, 0, "failed", now).await;
    ins_tick_st(&state, "to_5", "w_ops_1", 5, 0, "pending", now).await;
    ins_tick_st(&state, "to_0", "w_ops_1", 0, 900_000, "done", yesterday).await;
    // w_ops_2：无成员、无 tick。

    let (st, body) = get(&app, "/api/admin/worlds", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let worlds = body["worlds"].as_array().unwrap();
    let w1 = worlds.iter().find(|w| w["id"] == "w_ops_1").unwrap();
    let w2 = worlds.iter().find(|w| w["id"] == "w_ops_2").unwrap();

    // 在场人数：只数 active。
    assert_eq!(w1["participantCount"], 3);
    assert_eq!(w2["participantCount"], 0, "无成员即 0（0 是真实答案，不是缺数据）");

    // 成功率：已终结 tick 中 done 占比 = 4/(4+1) = 0.8，取值 0..1；pending 不进分母。
    assert_eq!(w1["successRate"].as_f64().unwrap(), 0.8);
    assert!(w2["successRate"].is_null(), "无已终结 tick → null（暂无数据），不得当 0% 渲染");

    // 今日成本：只含今天 3 笔（600_000），昨天的 900_000 不得串进来。
    assert_eq!(w1["todayTokens"], 600_000);
    let (_, m) = get(&app, "/api/admin/metrics/overview", Some(&admin)).await;
    let price = m["cost"]["centsPer1kTokens"].as_i64().unwrap();
    let cents = 600_000 * price / 1000;
    assert_eq!(w1["todayCostCents"], cents, "今日成本须与 token 单价口径一致");
    assert_eq!(w1["todayCostCny"].as_f64().unwrap(), cents as f64 / 100.0);
    assert_eq!(w2["todayTokens"], 0);
    assert_eq!(w2["todayCostCents"], 0);

    // 风控延迟无数据源：不编造字段（前端 null 判定 → 显示 —）。
    assert!(w1.get("moderationLatency").is_none(), "无数据源的字段不得下发假值");
}

/// 补充列不改 RBAC：/admin/worlds 仍限 operator（+admin），/admin/metrics/* 仍限 operator/finance。
#[tokio::test]
async fn cost_meter_endpoints_keep_existing_rbac() {
    let state = test_state().await;
    let app = build_router(state.clone());
    ins_world(&state, "w_rbac", "running", now_ms()).await;

    let admin = admin_token(&state);
    let operator = role_token(&state, "operator");
    let finance = role_token(&state, "finance");
    let reviewer = role_token(&state, "reviewer");
    let support = role_token(&state, "support");
    let user = user_token(&state);

    // 世界监控（含新增成本列）：operator/admin 可读，其余后台角色 403，普通用户 403。
    assert_eq!(get(&app, "/api/admin/worlds", Some(&operator)).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/worlds", Some(&admin)).await.0, StatusCode::OK);
    assert_eq!(get(&app, "/api/admin/worlds", Some(&finance)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/worlds", Some(&reviewer)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/worlds", Some(&support)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/worlds", Some(&user)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/worlds", None).await.0, StatusCode::UNAUTHORIZED);

    // 诊断（含预算金额）：同上，operator/admin only。
    let diag = "/api/admin/worlds/w_rbac/diagnostics";
    assert_eq!(get(&app, diag, Some(&operator)).await.0, StatusCode::OK);
    assert_eq!(get(&app, diag, Some(&finance)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, diag, Some(&reviewer)).await.0, StatusCode::FORBIDDEN);

    // 成本仪表在 metrics/overview 上：operator/finance/admin 可读，reviewer/support 403。
    let ov = "/api/admin/metrics/overview";
    assert_eq!(get(&app, ov, Some(&operator)).await.0, StatusCode::OK);
    assert_eq!(get(&app, ov, Some(&finance)).await.0, StatusCode::OK);
    assert_eq!(get(&app, ov, Some(&admin)).await.0, StatusCode::OK);
    assert_eq!(get(&app, ov, Some(&reviewer)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, ov, Some(&support)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, ov, None).await.0, StatusCode::UNAUTHORIZED);
}

/// 诊断栏预算：金额换算 + 用量比下发（前端据此去掉硬编码的 ¥/百分比）；跨日陈旧计数器不当"今日"。
#[tokio::test]
async fn diagnostics_budget_exposes_cny_and_usage_ratio() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl_b", "templateVersion": 1, "title": "预算世界", "roomType": "idle",
                "dailyTokenBudget": 1_000_000, "dailyCnyBudgetCents": 4000 }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let wid = body["worldId"].as_str().unwrap().to_string();

    // 熔断计数器停在今天：花掉 400_000 token。
    let today = crate::runtime::day_string(now_ms());
    sqlx::query("UPDATE world_budgets SET spent_tokens_today = 400000, budget_day = ? WHERE world_id = ?")
        .bind(&today)
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();

    let (st, diag) = get(&app, &format!("/api/admin/worlds/{wid}/diagnostics"), Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let b = &diag["budget"];
    let price = b["centsPer1kTokens"].as_i64().unwrap();
    assert!(price > 0);
    let spent_cents = 400_000 * price / 1000;

    assert_eq!(b["dailyTokenBudget"], 1_000_000);
    assert_eq!(b["dailyCnyBudgetCents"], 4000);
    assert_eq!(b["dailyCnyBudget"].as_f64().unwrap(), 40.0);
    assert_eq!(b["spentTokensToday"], 400_000);
    assert_eq!(b["spentTokensTodayEffective"], 400_000);
    assert_eq!(b["spentCnyCents"], spent_cents);
    assert_eq!(b["spentCny"].as_f64().unwrap(), spent_cents as f64 / 100.0);
    assert_eq!(b["budgetDayIsToday"], true);
    // 用量比 0..1：token 40%，cny = spent_cents/4000；合并取较大者（先撞线的那条决定熔断）。
    assert_eq!(b["tokenUsageRatio"].as_f64().unwrap(), 0.4);
    let cny_ratio = spent_cents as f64 / 4000.0;
    assert_eq!(b["cnyUsageRatio"].as_f64().unwrap(), cny_ratio);
    assert_eq!(b["usageRatio"].as_f64().unwrap(), 0.4_f64.max(cny_ratio));

    // 计数器停留在过去某天 → 不得当"今日"：有效消耗归零，用量比归零。
    sqlx::query("UPDATE world_budgets SET budget_day = '1970-01-01' WHERE world_id = ?")
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    let (_, diag) = get(&app, &format!("/api/admin/worlds/{wid}/diagnostics"), Some(&admin)).await;
    let b = &diag["budget"];
    assert_eq!(b["budgetDayIsToday"], false);
    assert_eq!(b["spentTokensToday"], 400_000, "原始列照实回显");
    assert_eq!(b["spentTokensTodayEffective"], 0, "跨日后计数器属于过去某天，不算今日");
    assert_eq!(b["spentCnyCents"], 0);
    assert_eq!(b["usageRatio"].as_f64().unwrap(), 0.0);

    // 未设 cny 上限的世界：cnyUsageRatio 为 null（没有上限就没有"用了百分之多少"）。
    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl_b", "templateVersion": 1, "title": "无 cny 上限", "roomType": "idle",
                "dailyTokenBudget": 1000, "dailyCnyBudgetCents": 0 }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let wid2 = body["worldId"].as_str().unwrap();
    let (_, diag) = get(&app, &format!("/api/admin/worlds/{wid2}/diagnostics"), Some(&admin)).await;
    assert!(diag["budget"]["cnyUsageRatio"].is_null(), "无 cny 上限 → 用量比 null");
    assert_eq!(diag["budget"]["tokenUsageRatio"].as_f64().unwrap(), 0.0);
}

// ================= 叙事质量 SLO（VALIDATION §4.2 验证基建第二件） =================
// 落点：GET /admin/metrics/overview 的 `narrativeSlo` 顶层键（口径与聚合在 crate::slo）。
// 本节测的是**接进后台之后**的事：响应形态、RBAC、窗口参数、减负开关、
// 🔴 三项不可算指标是否如实标注为「无数据源」而不是显示 0。
// 聚合口径本身的正确性（基尼/无戏份/收尾率/二次入世率）在 slo::tests 里逐项断言。

async fn slo_ins_contribution(state: &AppState, world: &str, character: &str, score_milli: i64) {
    sqlx::query(
        "INSERT INTO world_contributions (world_id, character_id, score_milli, milestone_score_milli, \
         settled_at, updated_at) VALUES (?, ?, ?, 0, 0, ?)",
    )
    .bind(world)
    .bind(character)
    .bind(score_milli)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn slo_ins_event(state: &AppState, world: &str, tick_no: i64, seq: i64, kind: &str, actors: &[&str]) {
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, occurred_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'public', ?)",
    )
    .bind(format!("slo_ev_{world}_{tick_no}_{seq}"))
    .bind(world)
    .bind(tick_no)
    .bind(seq)
    .bind(format!("slo_dev_{world}_{tick_no}_{seq}"))
    .bind(kind)
    .bind(serde_json::to_string(actors).unwrap())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 四项可算指标进后台：数值正确、🔴 NPC 不污染基尼、门槛与窗口回显齐全。
#[tokio::test]
async fn metrics_overview_exposes_narrative_slo_four_computable_metrics() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let now = now_ms();

    // 世界 A（在跑）：三名玩家 8000/1000/1000 → 基尼 0.467 越 T2 门槛；另有一名 NPC 独占大头。
    ins_world(&state, "slo_w1", "running", now).await;
    for (mid, score) in [("p1", 8000i64), ("p2", 1000), ("p3", 1000)] {
        ins_member(&state, mid, "slo_w1", &format!("u_{mid}"), "active").await;
        slo_ins_contribution(&state, "slo_w1", &format!("cc_{mid}"), score).await;
    }
    slo_ins_contribution(&state, "slo_w1", "npc_slo", 90_000).await;

    // 拍域 3 拍；cc_p1 每拍有戏，cc_p2 只有第 0 拍，cc_p3 只被同意门拦下（不算戏份）。
    for t in 0..3 {
        ins_tick_st(&state, &format!("slo_tk_{t}"), "slo_w1", t, 100, "done", now).await;
        slo_ins_event(&state, "slo_w1", t, t * 10, "action", &["cc_p1"]).await;
        slo_ins_event(&state, "slo_w1", t, t * 10 + 2, "consent_request", &["cc_p3"]).await;
    }
    slo_ins_event(&state, "slo_w1", 0, 1, "dialogue", &["cc_p2"]).await;

    // 世界 B（已结束，强制收尾）+ 世界 C（已结束，自然收尾）。
    ins_world(&state, "slo_w2", "ended", now).await;
    ins_world(&state, "slo_w3", "ended", now).await;
    for (w, reason) in [("slo_w2", "time_cap|ending=none"), ("slo_w3", "mainline_complete|ending=peace")] {
        sqlx::query(
            "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
             VALUES (?, 'system', 'system', 'world.ended', ?, ?, ?)",
        )
        .bind(format!("slo_aud_{w}"))
        .bind(w)
        .bind(reason)
        .bind(now)
        .execute(&state.db)
        .await
        .unwrap();
    }

    // 二次入世：cc_p1 再进一个世界；两张未下架卡入库。
    ins_member(&state, "p1b", "slo_w2", "u_p1", "active").await;
    sqlx::query("UPDATE world_members SET cloud_character_id = 'cc_p1' WHERE id = 'p1b'")
        .execute(&state.db)
        .await
        .unwrap();
    seed_character(&state, "cc_p1", "u_p1", "沈砚", "approved", None).await;
    seed_character(&state, "cc_p2", "u_p2", "裴照", "approved", None).await;

    let (st, m) = get(&app, "/api/admin/metrics/overview", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let slo = &m["narrativeSlo"];
    assert_eq!(slo["status"], "ok", "{slo}");
    assert_eq!(slo["windowDays"], 30, "SLO 默认窗口 30 天");
    assert_eq!(slo["thresholds"]["attentionGiniMax"], 0.35, "T2 门槛必须回显在响应里");

    // ① 基尼：只统计 slo_w1（3 名玩家），NPC 已被 world_members 交集剔除。
    let g = &slo["metrics"]["attentionGini"];
    assert_eq!(g["status"], "ok");
    assert_eq!(g["worldsCounted"], 1);
    assert_eq!(g["worstWorlds"][0]["worldId"], "slo_w1");
    assert_eq!(g["worstWorlds"][0]["members"], 3, "🔴 NPC 不得进玩家公平度分母");
    let gini = g["worstWorlds"][0]["gini"].as_f64().unwrap();
    assert!((gini - 7000.0 / 15000.0).abs() < 1e-9, "8000/1000/1000 的基尼应为 0.4667，实测 {gini}");
    assert_eq!(g["worldsOverThreshold"], 1, "该世界越过 T2 门槛 0.35");

    // ② 最长连续无有效戏份：cc_p3 全程被同意门拦下 → 3 拍；cc_p2 → 2 拍；cc_p1 → 0。
    let s = &slo["metrics"]["silentStreak"];
    assert_eq!(s["status"], "ok");
    assert_eq!(s["ticksCounted"], 3);
    assert_eq!(s["maxStreak"], 3, "consent_request 不构成有效戏份");
    let worst = s["worstMembers"].as_array().unwrap();
    let streak = |cid: &str| worst.iter().find(|w| w["characterId"] == cid).unwrap()["streak"].as_i64().unwrap();
    assert_eq!(streak("cc_p3"), 3);
    assert_eq!(streak("cc_p2"), 2);
    assert_eq!(streak("cc_p1"), 0);

    // ③ 强制收尾率：两个已结束世界，一个 time_cap（强制）一个 mainline_complete（自然）。
    let f = &slo["metrics"]["forcedConclusionRate"];
    assert_eq!(f["endedWorlds"], 2);
    assert_eq!(f["forcedWorlds"], 1);
    assert_eq!(f["forcedRate"], 0.5);
    assert_eq!(f["byKind"]["natural"], 1);
    assert_eq!(f["byKind"]["forced"], 1);

    // ④ 同角色二次入世率：cc_p1 进过两个世界，分母是两张未下架卡。
    let r = &slo["metrics"]["repeatEntryRate"];
    assert_eq!(r["charactersTotal"], 2);
    assert_eq!(r["charactersTwoPlusWorlds"], 1);
    assert_eq!(r["repeatEntryRate"], 0.5);
}

/// 🔴 仍无数据源的 SLO 必须在后台响应里**如实标注**：status=no_data_source + value=null，
/// 并说清为什么算不了。后台显示 `—` 与显示 `0%` 是两个完全不同的经营判断。
///
/// 清单从三项缩到一项是**数据源逐个到位的预期结果**，不是断言被放宽：
/// `stateTextContradictionRate`（迁移 0030 CriticReport 落库）与
/// `oocAppealRate`（迁移 0037 OOC 注解权）先后转正，本用例对两者改为断言
/// 「不得再标无数据源」+「内容风控申诉仍不可冒充 OOC 申诉」。
#[tokio::test]
async fn narrative_slo_marks_remaining_metrics_as_no_data_source() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    // 造一条内容风控申诉：它**不是** OOC 申诉，绝不能被当成 OOC 申诉率的数据源。
    seed_character(&state, "ch_ap", "u_ap", "被驳回的卡", "rejected", None).await;
    seed_appeal(&state, "ap1", "ch_ap", "u_ap", "open", now_ms()).await;

    let (st, m) = get(&app, "/api/admin/metrics/overview", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let slo = &m["narrativeSlo"];

    let expected = ["plotRepetitionRate"];
    let listed: Vec<&str> =
        slo["unavailable"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(listed, expected);
    for key in expected {
        let x = &slo["metrics"][key];
        assert_eq!(x["status"], "no_data_source", "{key}: {x}");
        assert!(x["value"].is_null(), "{key} 必须是 null，不许是 0：{x}");
        assert!(!x["reason"].as_str().unwrap().is_empty(), "{key} 必须说清为什么算不了");
        assert!(!x["blockedBy"].as_str().unwrap().is_empty(), "{key} 必须说清补齐它需要什么");
    }

    // 🔴 内容风控申诉确实存在，但它**不是** OOC 申诉的数据源——两张表永远不可互相冒充。
    // 转正后这条命题的检验方式变了但强度没降：OOC 申诉率现在读 `ooc_appeals`，
    // 那张表里一条也没有，且注解权开关从未开过 → 必须报 `entry_not_open` + value=null，
    // 绝不能因为 `moderation_appeals` 里有一行就算出任何数。
    assert_eq!(
        count(&state, "SELECT COUNT(*) AS n FROM moderation_appeals").await,
        1,
        "前提：库里确实有一条内容风控申诉"
    );
    let ooc = &slo["metrics"]["oocAppealRate"];
    assert_ne!(ooc["status"], "no_data_source", "数据源已到位（迁移 0037），不得再标无数据源：{ooc}");
    assert_eq!(ooc["status"], "entry_not_open", "入口默认关闭 → 「没测过」而不是 0%：{ooc}");
    assert!(ooc["value"].is_null(), "🔴 内容风控申诉不得让 OOC 申诉率算出任何数：{ooc}");
    assert_eq!(
        count(&state, "SELECT COUNT(*) AS n FROM ooc_appeals").await,
        0,
        "OOC 申诉表与内容风控申诉表是两张互不相干的表"
    );
}

/// 空平台：SLO 段不除零、不报错、不 panic，四项可算指标一律 ok 且计数为 0。
#[tokio::test]
async fn narrative_slo_is_zero_safe_on_empty_platform() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let (st, m) = get(&app, "/api/admin/metrics/overview", Some(&admin_token(&state))).await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let slo = &m["narrativeSlo"];
    assert_eq!(slo["status"], "ok");
    assert_eq!(slo["metrics"]["attentionGini"]["worldsCounted"], 0);
    assert_eq!(slo["metrics"]["silentStreak"]["membersCounted"], 0);
    assert_eq!(slo["metrics"]["forcedConclusionRate"]["forcedRate"], 0.0);
    assert_eq!(slo["metrics"]["repeatEntryRate"]["repeatEntryRate"], 0.0);
    // 均值类无数据时给 null（"没数据"不是"均值为 0"）。
    assert!(slo["metrics"]["attentionGini"]["meanGini"].is_null());
    assert!(slo["metrics"]["silentStreak"]["meanStreak"].is_null());
}

/// 窗口参数 clamp 到 [1,365]；`?slo=0` 是给高频轮询的减负开关，且与"无数据源"是两种状态。
#[tokio::test]
async fn narrative_slo_window_clamps_and_can_be_skipped_for_polling() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let (_, m) = get(&app, "/api/admin/metrics/overview?sloDays=7", Some(&admin)).await;
    assert_eq!(m["narrativeSlo"]["windowDays"], 7);
    let (_, m) = get(&app, "/api/admin/metrics/overview?sloDays=0", Some(&admin)).await;
    assert_eq!(m["narrativeSlo"]["windowDays"], 1, "下限 clamp 到 1");
    let (_, m) = get(&app, "/api/admin/metrics/overview?sloDays=9999", Some(&admin)).await;
    assert_eq!(m["narrativeSlo"]["windowDays"], 365, "上限 clamp 到 365");

    // 减负开关：跳过态自成一档，仍如实列出剩余的无数据源指标。
    let (st, m) = get(&app, "/api/admin/metrics/overview?slo=0", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(m["narrativeSlo"]["status"], "skipped_by_request");
    assert_eq!(m["narrativeSlo"]["metrics"].as_object().unwrap().len(), 0);
    assert_eq!(m["narrativeSlo"]["unavailable"].as_array().unwrap().len(), 1);
    // 其余看板段不受影响。
    assert!(m["cost"]["centsPer1kTokens"].as_i64().unwrap() > 0);
}

/// RBAC 沿用 operator/finance（admin 直通），不放宽：reviewer/support 403、匿名 401。
#[tokio::test]
async fn narrative_slo_role_gate_operator_finance_admin_only() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let ov = "/api/admin/metrics/overview";

    for role in ["operator", "finance"] {
        let (st, m) = get(&app, ov, Some(&role_token(&state, role))).await;
        assert_eq!(st, StatusCode::OK, "{role} 应可读 SLO");
        assert_eq!(m["narrativeSlo"]["status"], "ok");
    }
    let (st, m) = get(&app, ov, Some(&admin_token(&state))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(m["narrativeSlo"]["status"], "ok");

    assert_eq!(get(&app, ov, Some(&role_token(&state, "reviewer"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, ov, Some(&role_token(&state, "support"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, ov, Some(&user_token(&state))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, ov, None).await.0, StatusCode::UNAUTHORIZED);
}

// ---------------- 世界封面（迁移 0027）：后台列表投影 + 官方建房带图 ----------------

/// 封面 base64 载荷（形态与 `POST /worlds/{id}/cover` 逐字一致）。
fn admin_cover_body(bytes: &[u8], mime: &str) -> Value {
    use base64::Engine as _;
    json!({
        "imageBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
        "mime": mime,
    })
}

/// 直接写封面三列（模拟已上传 + 指定机审裁决）。DevModeration::check_image 恒直过，
/// 从上传路径造不出 pending/rejected，范式同 worlds::tests 的 force_cover_moderation。
async fn set_world_cover(state: &AppState, world_id: &str, url: &str, moderation: &str) {
    sqlx::query(
        "UPDATE worlds SET cover_object_key = ?, cover_url = ?, cover_moderation = ? WHERE id = ?",
    )
    .bind(format!("covers/{world_id}.png"))
    .bind(url)
    .bind(moderation)
    .bind(world_id)
    .execute(&state.db)
    .await
    .unwrap();
}

fn find_admin_world<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id)
        .unwrap_or_else(|| panic!("列表里没有 {id}: {body}"))
}

/// 🔴 后台世界列表的 coverUrl 与玩家大厅同一个闸门（`worlds::visible_cover_url`）：
/// 只有 approved 才下发；pending/rejected/无封面/空白 URL 一律**键缺席**（不是空串、不是 null）。
#[tokio::test]
async fn admin_worlds_list_projects_only_approved_cover() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let now = now_ms();

    ins_world(&state, "w_cov_ok", "running", now).await;
    ins_world(&state, "w_cov_pending", "running", now - 1000).await;
    ins_world(&state, "w_cov_rejected", "running", now - 2000).await;
    ins_world(&state, "w_cov_blank", "running", now - 3000).await;
    ins_world(&state, "w_cov_none", "running", now - 4000).await;
    let ok_url = "/api/assets/objects/covers/w_cov_ok.png";
    set_world_cover(&state, "w_cov_ok", ok_url, "approved").await;
    set_world_cover(&state, "w_cov_pending", "/api/assets/objects/covers/w_cov_pending.png", "pending").await;
    set_world_cover(&state, "w_cov_rejected", "/api/assets/objects/covers/w_cov_rejected.png", "rejected").await;
    // 空白 URL + approved：闸门 trim 后归零，绝不下发空串。
    set_world_cover(&state, "w_cov_blank", "   ", "approved").await;

    let (st, body) = get(&app, "/api/admin/worlds", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK, "{body}");

    assert_eq!(find_admin_world(&body, "w_cov_ok")["coverUrl"], ok_url, "过审封面应下发");
    for id in ["w_cov_pending", "w_cov_rejected"] {
        let item = find_admin_world(&body, id);
        assert!(item.get("coverUrl").is_none(), "🔴 未过审封面不得出现在后台列表: {item}");
    }
    let blank = find_admin_world(&body, "w_cov_blank");
    assert!(blank.get("coverUrl").is_none(), "空白 URL 不得下发（更不得下发空串）: {blank}");
    let none = find_admin_world(&body, "w_cov_none");
    assert!(none.get("coverUrl").is_none(), "无封面世界须**键缺席**，让前端走确定性兜底图: {none}");
    assert_ne!(none.get("coverUrl").and_then(|v| v.as_str()), Some(""), "绝不下发空串");

    // 人审改判 approved 后无需重传即恢复下发（证明后台读的是同一个动态闸门，不是建房那一刻的快照）。
    set_world_cover(&state, "w_cov_pending", "/api/assets/objects/covers/w_cov_pending.png", "approved").await;
    let (_, body) = get(&app, "/api/admin/worlds", Some(&admin)).await;
    assert_eq!(
        find_admin_world(&body, "w_cov_pending")["coverUrl"],
        "/api/assets/objects/covers/w_cov_pending.png",
        "改判 approved 后应恢复下发"
    );
}

/// 官方建房可一次带封面：建房后内部复用 `worlds::upload_cover`（同一套图审与落库），
/// 回执给裁决与 URL，三列真落库、对象真写入，建房留痕带上封面裁决。
#[tokio::test]
async fn admin_create_world_accepts_cover_and_reuses_upload_pipeline() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let raw: &[u8] = b"\x89PNG\r\n\x1a\n-admin-cover-\x00\x01\xff";

    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({
            "templateId": "tpl_cover", "templateVersion": 1, "title": "带封面的官方世界",
            "roomType": "idle", "cover": admin_cover_body(raw, "image/png"),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let wid = body["worldId"].as_str().unwrap().to_string();
    assert_eq!(body["coverModeration"], "approved", "dev 图审直过");
    assert_eq!(body["coverUrl"], format!("/api/assets/objects/covers/{wid}.png"));

    // 三列落库，键以世界 id 命名（与单独调用上传端点完全同源）。
    let row = sqlx::query("SELECT cover_object_key, cover_url, cover_moderation FROM worlds WHERE id = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(row.try_get::<String, _>("cover_object_key").unwrap(), format!("covers/{wid}.png"));
    assert_eq!(row.try_get::<String, _>("cover_url").unwrap(), format!("/api/assets/objects/covers/{wid}.png"));
    assert_eq!(row.try_get::<String, _>("cover_moderation").unwrap(), "approved");
    // 对象真的写进了对象存储（不是只落了个 URL 字符串）。
    assert_eq!(state.objects.get(&format!("covers/{wid}.png")).unwrap(), raw);

    // 列表随即带上封面（同一个闸门）。
    let (_, list) = get(&app, "/api/admin/worlds", Some(&admin)).await;
    assert_eq!(
        find_admin_world(&list, &wid)["coverUrl"],
        format!("/api/assets/objects/covers/{wid}.png"),
    );

    // 建房留痕带上封面裁决（"谁在什么时候给哪个世界配了什么图"可溯）。
    let reason: String = sqlx::query("SELECT reason FROM audit_logs WHERE action = 'world.create' AND subject = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("reason")
        .unwrap();
    assert!(reason.contains("cover=approved"), "建房留痕须含封面裁决: {reason}");

    // 不带 cover：不凭空造封面键（前端据键缺席走兜底图）。
    let (st, plain) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({ "templateId": "tpl_cover", "templateVersion": 1, "title": "无封面官方世界" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{plain}");
    assert!(plain.get("coverUrl").is_none(), "未带封面不得下发 coverUrl: {plain}");
    assert!(plain.get("coverModeration").is_none(), "未带封面不得下发裁决: {plain}");
}

/// 封面非法（MIME 不在白名单）：**世界照建**且回执明示 coverError——
/// 建房已落库不可回滚，报 4xx 会诱导运营重试建出重复房；封面可事后单独重传。
#[tokio::test]
async fn admin_create_world_reports_cover_failure_without_failing_the_world() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let (st, body) = post(
        &app,
        "/api/admin/worlds",
        Some(&admin),
        json!({
            "templateId": "tpl_cover", "templateVersion": 1, "title": "坏封面世界",
            "cover": admin_cover_body(b"gif89a-not-allowed", "image/gif"),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let wid = body["worldId"].as_str().unwrap().to_string();
    assert_eq!(body["coverError"]["code"], "bad_request", "{body}");
    assert!(body.get("coverUrl").is_none(), "失败不得下发 URL: {body}");

    // 世界确实建成了，封面三列保持空（没有半吊子落库）。
    let row = sqlx::query("SELECT cover_url, cover_moderation FROM worlds WHERE id = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(row.try_get::<Option<String>, _>("cover_url").unwrap().is_none());
    assert!(row.try_get::<Option<String>, _>("cover_moderation").unwrap().is_none());

    // 失败同样进建房留痕。
    let reason: String = sqlx::query("SELECT reason FROM audit_logs WHERE action = 'world.create' AND subject = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("reason")
        .unwrap();
    assert!(reason.contains("cover_error=bad_request"), "封面失败须留痕: {reason}");
}

/// 封面不改 RBAC：读列表（含 coverUrl）与带封面建房仍限 operator（admin 直通），
/// 其余后台角色 403、普通用户 403、匿名 401，且被拒时不得建出世界、不得写对象存储。
#[tokio::test]
async fn admin_cover_paths_keep_existing_rbac() {
    let state = test_state().await;
    let app = build_router(state.clone());
    let now = now_ms();
    ins_world(&state, "w_cov_rbac", "running", now).await;
    set_world_cover(&state, "w_cov_rbac", "/api/assets/objects/covers/w_cov_rbac.png", "approved").await;

    let operator = role_token(&state, "operator");

    // 读：operator/admin 拿得到 coverUrl；其余角色连列表都进不来。
    let (st, body) = get(&app, "/api/admin/worlds", Some(&operator)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(find_admin_world(&body, "w_cov_rbac")["coverUrl"], "/api/assets/objects/covers/w_cov_rbac.png");
    for role in ["reviewer", "support", "finance"] {
        assert_eq!(
            get(&app, "/api/admin/worlds", Some(&role_token(&state, role))).await.0,
            StatusCode::FORBIDDEN,
            "{role} 不得读世界列表（含封面）"
        );
    }
    assert_eq!(get(&app, "/api/admin/worlds", Some(&user_token(&state))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, "/api/admin/worlds", None).await.0, StatusCode::UNAUTHORIZED);

    // 写：带封面建房仍只认 operator/admin。
    let body = json!({
        "templateId": "tpl_cover", "templateVersion": 1, "title": "越权带图建房",
        "cover": admin_cover_body(b"\x89PNG-rbac", "image/png"),
    });
    let before = count(&state, "SELECT COUNT(*) AS n FROM worlds").await;
    for role in ["reviewer", "support", "finance"] {
        assert_eq!(
            post(&app, "/api/admin/worlds", Some(&role_token(&state, role)), body.clone()).await.0,
            StatusCode::FORBIDDEN,
            "{role} 不得带封面建房"
        );
    }
    assert_eq!(post(&app, "/api/admin/worlds", Some(&user_token(&state)), body.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post(&app, "/api/admin/worlds", None, body.clone()).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM worlds").await, before, "越权请求不得建出世界");

    let (st, ok) = post(&app, "/api/admin/worlds", Some(&operator), body).await;
    assert_eq!(st, StatusCode::OK, "{ok}");
    assert_eq!(ok["coverModeration"], "approved");
}

// ---------------- 世界系列自动扩容：建房时登记（总规格 §5「世界系列自动扩容【新增】」） ----------------

/// 🔴 前门拒绝 + 参数校验 + 幂等登记（口径与生死状档的建房前门逐字一致）。
///
/// 「前门拒绝（登记不了）+ 读取侧降级（关阀即停扩容，见 worlds::tests）」两道都在，
/// `MUSE_WORLD_SERIES_AUTOSCALE` 才是真的急停阀而非一次性阉割。
#[tokio::test]
async fn world_create_series_enrollment() {
    use crate::worlds::SeriesSwitch;
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);
    let base =
        json!({ "templateId": "tpl_series", "templateVersion": 2, "title": "黑角域篇", "roomType": "idle" });

    // 🔴 开关未开 → 连登记都不允许（否则会攒下一堆"一开阀就同时开始扩容"的世界）。
    {
        let _sw = SeriesSwitch::set(false);
        let mut body = base.clone();
        body["series"] = json!({ "maxInstances": 3 });
        let (st, err) = post(&app, "/api/admin/worlds", Some(&admin), body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "开关未开不得登记系列: {err}");
        assert!(
            err["error"]["message"].as_str().unwrap_or("").contains("世界系列自动扩容尚未开启"),
            "文案须说明是运营开关未开: {err}"
        );
        assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM world_series").await, 0);
        // 不带 series 段的建房不受影响（默认行为零变化）。
        let (st_plain, plain) = post(&app, "/api/admin/worlds", Some(&admin), base.clone()).await;
        assert_eq!(st_plain, StatusCode::OK, "{plain}");
        assert!(plain.get("seriesId").is_none(), "不带 series 段就不该有系列: {plain}");
    }

    let _sw = SeriesSwitch::with(true, &[("MUSE_WORLD_SERIES_MAX_INSTANCES", "5")]);

    // 上限越界（0 / 超全局硬顶）→ 400，且不建出世界系列。
    for bad in [0, 6] {
        let mut body = base.clone();
        body["series"] = json!({ "maxInstances": bad });
        let (st, err) = post(&app, "/api/admin/worlds", Some(&admin), body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "maxInstances={bad} 应拒: {err}");
        assert!(err["error"]["message"].as_str().unwrap_or("").contains("maxInstances"), "{err}");
    }
    assert_eq!(count(&state, "SELECT COUNT(*) AS n FROM world_series").await, 0);

    // 合法登记：建房 + 登记为 1 号实例，回执带系列信息，audit 留痕含系列。
    let mut body = base.clone();
    body["series"] = json!({ "maxInstances": 3 });
    let (st, ok) = post(&app, "/api/admin/worlds", Some(&admin), body).await;
    assert_eq!(st, StatusCode::OK, "{ok}");
    let wid = ok["worldId"].as_str().unwrap().to_string();
    let sid = ok["seriesId"].as_str().expect("回执须带 seriesId").to_string();
    assert_eq!(ok["seriesInstanceNo"], json!(1));
    assert_eq!(ok["seriesMaxInstances"], json!(3));

    let origin: String = sqlx::query("SELECT origin_world_id FROM world_series WHERE id = ?")
        .bind(&sid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("origin_world_id")
        .unwrap();
    assert_eq!(origin, wid, "1 号实例即建房参数的复制源");
    let no: i64 = sqlx::query("SELECT instance_no FROM world_series_instances WHERE world_id = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("instance_no")
        .unwrap();
    assert_eq!(no, 1);
    let reason: String = sqlx::query("SELECT reason FROM audit_logs WHERE action='world.create' AND subject=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("reason")
        .unwrap();
    assert!(reason.contains("series="), "建房留痕须带系列信息: {reason}");

    // 运营诊断能看见系列（生效上限 = 系列自带 ∧ 全局硬顶，取小）。
    let (st_d, diag) = get(&app, &format!("/api/admin/worlds/{wid}/diagnostics"), Some(&admin)).await;
    assert_eq!(st_d, StatusCode::OK, "{diag}");
    assert_eq!(diag["series"]["seriesId"], json!(sid));
    assert_eq!(diag["series"]["instanceNo"], json!(1));
    assert_eq!(diag["series"]["instanceCount"], json!(1));
    assert_eq!(diag["series"]["maxInstancesConfigured"], json!(3));
    assert_eq!(diag["series"]["maxInstancesEffective"], json!(3));
    assert_eq!(diag["series"]["globalCap"], json!(5));
    assert_eq!(diag["series"]["autoscaleEnabled"], json!(true));
    assert_eq!(diag["series"]["status"], json!("active"));
    // 未崩塌 → 无 BE 传记。
    assert!(diag["beBiography"].is_null(), "未崩塌世界不应有传记: {diag}");

    // 普通世界（未登记）的诊断里 series 为 null。
    let (_st, plain) = post(&app, "/api/admin/worlds", Some(&admin), base.clone()).await;
    let plain_id = plain["worldId"].as_str().unwrap();
    let (_st2, diag2) = get(&app, &format!("/api/admin/worlds/{plain_id}/diagnostics"), Some(&admin)).await;
    assert!(diag2["series"].is_null(), "未登记世界的 series 段应为 null: {diag2}");
}

/// 运营诊断的 `series` 段**不受 env 开关门控**：急停阀一拉，运营更需要看得见这条队列
/// （开关状态另作 `autoscaleEnabled` 明示），否则整条队列在后台凭空消失。
#[tokio::test]
async fn diagnostics_series_visible_even_when_switch_off() {
    use crate::worlds::SeriesSwitch;
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = admin_token(&state);

    let wid = {
        let _sw = SeriesSwitch::set(true);
        let (st, ok) = post(
            &app,
            "/api/admin/worlds",
            Some(&admin),
            json!({ "templateId": "tpl_s", "title": "系列 1 号", "series": { "maxInstances": 2 } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{ok}");
        ok["worldId"].as_str().unwrap().to_string()
    };

    let _off = SeriesSwitch::set(false);
    let (st, diag) = get(&app, &format!("/api/admin/worlds/{wid}/diagnostics"), Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!diag["series"].is_null(), "关阀时后台仍须看得见系列: {diag}");
    assert_eq!(diag["series"]["autoscaleEnabled"], json!(false), "但要明示急停阀已拉下");
}
