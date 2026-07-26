//! S2 集成测试：sqlite::memory + oneshot。覆盖 join 服务端校验、tick 幂等、
//! 预算熔断、DomainEvent→WorldEvent 受众投影隔离（查询层 + 推送层）。

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
use crate::events::{self, project_domain_events, ws_visible, ProjectionMember};
use crate::runtime::{self, TickStatus};
use crate::worlds::{create_world, load_world, CreateWorldParams};

use muse_engine::character::types::*;
use muse_engine::narrative::types::{DomainEvent, DomainEventType, EventVisibility};

// ---------- 脚手架 ----------

static INIT: std::sync::Once = std::sync::Once::new();

fn test_config() -> ServerConfig {
    ServerConfig {
        database_url: "sqlite::memory:".into(),
        bind_addr: "127.0.0.1:0".into(),
        jwt_secret: "test-secret".into(),
        access_ttl_secs: 3600,
        refresh_ttl_secs: 100_000,
        dev_mode: true,
        object_store_dir: std::env::temp_dir()
            .join(new_id("muse-test"))
            .to_string_lossy()
            .into_owned(),
    }
}

async fn test_state() -> AppState {
    INIT.call_once(|| sqlx::any::install_default_drivers());
    // 单连接内存库：跨查询保持同一 in-memory DB。
    let pool = AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, test_config())
}

fn token(state: &AppState, user_id: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, user_id, "user", 3600).unwrap()
}

fn sample_card_json(id: &str, name: &str) -> String {
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
    serde_json::to_string(&card).unwrap()
}

async fn seed_user(state: &AppState, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
         VALUES (?, '', 0, 'active', ?, ?)",
    )
    .bind(id)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_char(state: &AppState, id: &str, owner: &str, moderation: &str, withdrawn: i64) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES (?, ?, 'local', 1, ?, 'original', ?, ?, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(sample_card_json(id, "角色"))
    .bind(moderation)
    .bind(withdrawn)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn post_json(
    app: &axum::Router,
    uri: &str,
    token: &str,
    idem_key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = idem_key {
        b = b.header("Idempotency-Key", k);
    }
    let req = b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn get_json(app: &axum::Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

// ---------- 建房版本钉住 ----------

#[tokio::test]
async fn create_world_pins_versions_and_budget() {
    let state = test_state().await;
    let mut p = CreateWorldParams::official("tpl", 1, "测试世界");
    p.daily_token_budget = 500;
    let wid = create_world(&state.db, p).await.unwrap();

    let w = load_world(&state.db, &wid).await.unwrap();
    assert_eq!(w.engine_version, muse_engine::ENGINE_VERSION);
    assert_eq!(w.prompt_set_version, "dev-none"); // 无 active 版本 → 默认钉住占位
    assert_eq!(w.model_route_version, "dev-none");
    assert_eq!(w.status, "open");
    assert_eq!(w.visibility, "official");

    let budget: i64 = sqlx::query("SELECT daily_token_budget FROM world_budgets WHERE world_id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("daily_token_budget")
        .unwrap();
    assert_eq!(budget, 500);
}

// ---------- timeline_mode 落库（缺口①） ----------

#[tokio::test]
async fn create_world_defaults_timeline_mode_interval() {
    let state = test_state().await;
    // official() 默认 timeline_mode = interval，落库亦为 interval（向后兼容，老行为不变）。
    let p = CreateWorldParams::official("tpl", 1, "默认世界");
    assert_eq!(p.timeline_mode, "interval");
    let wid = create_world(&state.db, p).await.unwrap();

    let tm: String = sqlx::query("SELECT timeline_mode FROM worlds WHERE id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("timeline_mode")
        .unwrap();
    assert_eq!(tm, "interval");
}

#[tokio::test]
async fn create_world_persists_event_timeline_mode() {
    let state = test_state().await;
    let mut p = CreateWorldParams::official("tpl", 1, "放置世界");
    p.timeline_mode = "event".into();
    let wid = create_world(&state.db, p).await.unwrap();

    let tm: String = sqlx::query("SELECT timeline_mode FROM worlds WHERE id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("timeline_mode")
        .unwrap();
    assert_eq!(tm, "event");
}

#[tokio::test]
async fn create_world_normalizes_bogus_timeline_mode() {
    let state = test_state().await;
    // 防御式归一化：inner create_world 对非法值兜底为 interval（P4b 复用面）。
    let mut p = CreateWorldParams::official("tpl", 1, "非法值世界");
    p.timeline_mode = "bogus".into();
    let wid = create_world(&state.db, p).await.unwrap();

    let tm: String = sqlx::query("SELECT timeline_mode FROM worlds WHERE id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("timeline_mode")
        .unwrap();
    assert_eq!(tm, "interval");
}

// ---------- join 服务端权威 ----------

#[tokio::test]
async fn join_enforces_server_authority() {
    let state = test_state().await;
    let app = build_router(state.clone());

    seed_user(&state, "usrA").await;
    seed_user(&state, "usrB").await;
    seed_char(&state, "chA", "usrA", "approved", 0).await; // A 的合法角色
    seed_char(&state, "chPending", "usrA", "pending", 0).await; // 未审核
    seed_char(&state, "chWithdrawn", "usrA", "approved", 1).await; // 已撤回
    seed_char(&state, "chB", "usrB", "approved", 0).await; // 别人的角色

    let mut p = CreateWorldParams::official("tpl", 1, "世界");
    p.member_limit = 1;
    let wid = create_world(&state.db, p).await.unwrap();
    let ta = token(&state, "usrA");
    let uri = format!("/api/worlds/{wid}/join");

    // 成功：属本人 + approved + 未撤回。
    let (st, body) = post_json(&app, &uri, &ta, Some("k-ok"), json!({ "cloudCharacterId": "chA" })).await;
    assert_eq!(st, StatusCode::OK, "join 应成功: {body}");
    assert_eq!(body["status"], "active");
    let mid = body["membershipId"].as_str().unwrap().to_string();

    // 幂等重放：同 key 同载荷 → 相同响应。
    let (st2, body2) =
        post_json(&app, &uri, &ta, Some("k-ok"), json!({ "cloudCharacterId": "chA" })).await;
    assert_eq!(st2, StatusCode::OK);
    assert_eq!(body2["membershipId"].as_str().unwrap(), mid);

    // 人数上限（member_limit=1，已满）：另一 user 的首卡也进不来 → world_full。
    // （同 user 第二张卡在到达人数守卫前就被防自刷规则拦截，见下方专项测试。）
    let tb = token(&state, "usrB");
    let (st_full, body_full) =
        post_json(&app, &uri, &tb, None, json!({ "cloudCharacterId": "chB" })).await;
    assert_eq!(st_full, StatusCode::CONFLICT, "满员应 409");
    assert!(
        body_full["error"]["message"].as_str().unwrap_or("").contains("world_full"),
        "满员应命中 world_full 而非其他冲突: {body_full}"
    );

    // 非本人角色 → 403。
    let (st_forbidden, _) =
        post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chB" })).await;
    assert_eq!(st_forbidden, StatusCode::FORBIDDEN);

    // 未审核 → 409。
    let (st_pending, _) =
        post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chPending" })).await;
    assert_eq!(st_pending, StatusCode::CONFLICT);

    // 已撤回 → 409。
    let (st_withdrawn, _) =
        post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chWithdrawn" })).await;
    assert_eq!(st_withdrawn, StatusCode::CONFLICT);

    // 只有一条 active 成员记录。
    let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM world_members WHERE world_id=? AND status='active'")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn leave_marks_member_left() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_char(&state, "chA", "usrA", "approved", 0).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "世界")).await.unwrap();
    let ta = token(&state, "usrA");

    let (st, _) = post_json(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &ta,
        None,
        json!({ "cloudCharacterId": "chA" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st_leave, body) = post_json(
        &app,
        &format!("/api/worlds/{wid}/leave"),
        &ta,
        None,
        json!({ "cloudCharacterId": "chA" }),
    )
    .await;
    assert_eq!(st_leave, StatusCode::OK);
    assert_eq!(body["status"], "left");

    let status: String = sqlx::query("SELECT status FROM world_members WHERE world_id=? AND cloud_character_id='chA'")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("status")
        .unwrap();
    assert_eq!(status, "left");
}

// ---------- 防自刷：同一世界每位用户仅可投放一张角色卡 ----------

/// 同 user 第二张卡 join 同世界 → 409 固定文案；不同 user 一人一卡互不影响。
#[tokio::test]
async fn join_rejects_second_active_card_from_same_user() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_user(&state, "usrB").await;
    seed_char(&state, "chA1", "usrA", "approved", 0).await;
    seed_char(&state, "chA2", "usrA", "approved", 0).await;
    seed_char(&state, "chB", "usrB", "approved", 0).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");
    let ta = token(&state, "usrA");
    let tb = token(&state, "usrB");

    let (st, body) = post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chA1" })).await;
    assert_eq!(st, StatusCode::OK, "首卡应成功: {body}");

    // 第二张卡（防自刷抢隐藏任务钩子）→ 409 + 固定中文文案。
    let (st2, body2) = post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chA2" })).await;
    assert_eq!(st2, StatusCode::CONFLICT, "同 user 第二张卡应 409: {body2}");
    assert!(
        body2["error"]["message"].as_str().unwrap_or("").contains("同一世界每位用户仅可投放一张角色卡"),
        "文案应为「同一世界每位用户仅可投放一张角色卡」: {body2}"
    );

    // 不同 user 各投一张不受影响。
    let (st3, body3) = post_json(&app, &uri, &tb, None, json!({ "cloudCharacterId": "chB" })).await;
    assert_eq!(st3, StatusCode::OK, "不同 user 一人一卡不受影响: {body3}");

    // usrA 在库中仍只有一条 active 成员（chA2 未落行）。
    let n: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM world_members WHERE world_id=? AND user_id='usrA' AND status='active'",
    )
    .bind(&wid)
    .fetch_one(&state.db)
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(n, 1, "被拒的第二张卡不得落行");
}

/// 退出（left 不占名额）后换卡再进 → 成功；持有 active 卡期间复活旧卡 → 复活分支同样被拦。
#[tokio::test]
async fn join_allows_card_swap_after_leave() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_char(&state, "chA1", "usrA", "approved", 0).await;
    seed_char(&state, "chA2", "usrA", "approved", 0).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "世界")).await.unwrap();
    let ta = token(&state, "usrA");
    let join_uri = format!("/api/worlds/{wid}/join");
    let leave_uri = format!("/api/worlds/{wid}/leave");

    let (st, _) = post_json(&app, &join_uri, &ta, None, json!({ "cloudCharacterId": "chA1" })).await;
    assert_eq!(st, StatusCode::OK);
    let (st_leave, _) =
        post_json(&app, &leave_uri, &ta, None, json!({ "cloudCharacterId": "chA1" })).await;
    assert_eq!(st_leave, StatusCode::OK);

    // 已退出（非 active）不算占用 → 换卡再进成功。
    let (st2, body2) = post_json(&app, &join_uri, &ta, None, json!({ "cloudCharacterId": "chA2" })).await;
    assert_eq!(st2, StatusCode::OK, "退出后换卡应成功: {body2}");
    assert_eq!(body2["status"], "active");

    // chA2 active 期间复活 chA1（已有 left 行）→ 复活分支也被防自刷拦住。
    let (st3, body3) = post_json(&app, &join_uri, &ta, None, json!({ "cloudCharacterId": "chA1" })).await;
    assert_eq!(st3, StatusCode::CONFLICT, "持有 active 卡时复活旧卡应 409: {body3}");

    // 全程 usrA 至多一条 active，且是换入的 chA2。
    let active_char: String = sqlx::query(
        "SELECT cloud_character_id FROM world_members WHERE world_id=? AND user_id='usrA' AND status='active'",
    )
    .bind(&wid)
    .fetch_one(&state.db)
    .await
    .unwrap()
    .try_get("cloud_character_id")
    .unwrap();
    assert_eq!(active_char, "chA2");
}

/// 回归：同卡重复 join（无幂等键 → 直接走成员行幂等分支）不被防自刷拦截，返回同一 membership。
#[tokio::test]
async fn join_same_card_repeat_stays_idempotent() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_char(&state, "chA1", "usrA", "approved", 0).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "世界")).await.unwrap();
    let ta = token(&state, "usrA");
    let uri = format!("/api/worlds/{wid}/join");

    let (st, body) = post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chA1" })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let mid = body["membershipId"].as_str().unwrap().to_string();

    let (st2, body2) = post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chA1" })).await;
    assert_eq!(st2, StatusCode::OK, "同卡重复 join 应保持幂等成功: {body2}");
    assert_eq!(body2["membershipId"].as_str().unwrap(), mid, "应返回同一 membership");

    let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM world_members WHERE world_id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(n, 1, "重复 join 不得多落行");
}

// ---------- 波次 3：模板星级——join 历练准入 + 列表/详情 starRating 投影 ----------

/// 造一个指定星级的已过审模板（skeleton 为空对象：星级功能与骨架内容正交）。
async fn seed_star_template(state: &AppState, id: &str, star: i64) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, star_rating, created_at) \
         VALUES (?, '星级模板', 'chapter', '{}', '{\"mode\":\"open\"}', 1, 1, 'approved', ?, ?)",
    )
    .bind(id)
    .bind(star)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn set_mileage(state: &AppState, char_id: &str, mileage: i64) {
    sqlx::query("UPDATE cloud_characters SET mileage = ? WHERE id = ?")
        .bind(mileage)
        .bind(char_id)
        .execute(&state.db)
        .await
        .unwrap();
}

/// 1-2★ 免检：零历练卡可进 2★ 世界；模板行缺失（历史/测试世界）按 1★ 兜底同样免检。
#[tokio::test]
async fn join_low_star_worlds_skip_mileage_gate() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrS").await;
    seed_char(&state, "chS", "usrS", "approved", 0).await; // mileage 默认 0
    let ts = token(&state, "usrS");

    seed_star_template(&state, "tpl_s2", 2).await;
    let w2 = create_world(&state.db, CreateWorldParams::official("tpl_s2", 1, "二星世界")).await.unwrap();
    let (st, body) =
        post_json(&app, &format!("/api/worlds/{w2}/join"), &ts, None, json!({ "cloudCharacterId": "chS" })).await;
    assert_eq!(st, StatusCode::OK, "2★ 应免历练检: {body}");

    // 模板行缺失 → 1★ 兜底免检（老世界零回归）。
    let w_ghost =
        create_world(&state.db, CreateWorldParams::official("tpl_ghost", 1, "无模板世界")).await.unwrap();
    let (st, body) =
        post_json(&app, &format!("/api/worlds/{w_ghost}/join"), &ts, None, json!({ "cloudCharacterId": "chS" })).await;
    assert_eq!(st, StatusCode::OK, "模板缺失应按 1★ 免检: {body}");
}

/// 3★ 门槛 300：投放卡 mileage 299 → 409 且文案含星级/门槛/当前值；300 → 过。
#[tokio::test]
async fn join_three_star_gates_on_card_mileage() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrS3").await;
    seed_char(&state, "chS3", "usrS3", "approved", 0).await;
    let ts = token(&state, "usrS3");
    seed_star_template(&state, "tpl_s3", 3).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl_s3", 1, "三星世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");

    set_mileage(&state, "chS3", 299).await;
    let (st, body) = post_json(&app, &uri, &ts, None, json!({ "cloudCharacterId": "chS3" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "历练不足应 409: {body}");
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("3 星副本"), "文案应含星级: {msg}");
    assert!(msg.contains("300"), "文案应含门槛数字: {msg}");
    assert!(msg.contains("299"), "文案应含当前历练: {msg}");

    // 被拒不落成员行。
    let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM world_members WHERE world_id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(n, 0, "历练不足不得落成员行");

    set_mileage(&state, "chS3", 300).await;
    let (st, body) = post_json(&app, &uri, &ts, None, json!({ "cloudCharacterId": "chS3" })).await;
    assert_eq!(st, StatusCode::OK, "达标应放行: {body}");
    assert_eq!(body["status"], "active");
}

/// 4★/5★ 阶梯：4★ 需 1000、5★ 需 3000（文案含对应门槛），达标即过。
#[tokio::test]
async fn join_high_star_thresholds_scale() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrS5").await;
    seed_char(&state, "chS5", "usrS5", "approved", 0).await;
    let ts = token(&state, "usrS5");
    seed_star_template(&state, "tpl_s4", 4).await;
    seed_star_template(&state, "tpl_s5", 5).await;
    let w4 = create_world(&state.db, CreateWorldParams::official("tpl_s4", 1, "四星世界")).await.unwrap();
    let w5 = create_world(&state.db, CreateWorldParams::official("tpl_s5", 1, "五星世界")).await.unwrap();

    set_mileage(&state, "chS5", 999).await;
    let (st, body) =
        post_json(&app, &format!("/api/worlds/{w4}/join"), &ts, None, json!({ "cloudCharacterId": "chS5" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]["message"].as_str().unwrap_or("").contains("1000"), "4★ 文案应含 1000: {body}");

    set_mileage(&state, "chS5", 2999).await;
    let (st, body) =
        post_json(&app, &format!("/api/worlds/{w5}/join"), &ts, None, json!({ "cloudCharacterId": "chS5" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]["message"].as_str().unwrap_or("").contains("3000"), "5★ 文案应含 3000: {body}");

    set_mileage(&state, "chS5", 3000).await;
    let (st, body) =
        post_json(&app, &format!("/api/worlds/{w5}/join"), &ts, None, json!({ "cloudCharacterId": "chS5" })).await;
    assert_eq!(st, StatusCode::OK, "3000 历练应进 5★: {body}");
}

/// 列表（new/hot）与详情投影 starRating；模板缺失兜底 1。
#[tokio::test]
async fn world_list_and_detail_project_star_rating() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrP").await;
    let tp = token(&state, "usrP");

    seed_star_template(&state, "tpl_s4p", 4).await;
    let w4 = create_world(&state.db, CreateWorldParams::official("tpl_s4p", 1, "四星投影世界")).await.unwrap();
    create_world(&state.db, CreateWorldParams::official("tpl_none", 1, "无模板投影世界")).await.unwrap();

    // 列表（sort=new 默认）：每项带 starRating；模板缺失 → 1。
    let (st, body) = get_json(&app, "/api/worlds", &tp).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let stars: std::collections::BTreeMap<String, i64> = body["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["title"].as_str().unwrap().to_string(), w["starRating"].as_i64().unwrap()))
        .collect();
    assert_eq!(stars["四星投影世界"], 4, "列表应投影模板星级");
    assert_eq!(stars["无模板投影世界"], 1, "模板缺失应兜底 1★");

    // sort=hot 同样带 starRating。
    let (st, hot) = get_json(&app, "/api/worlds?sort=hot", &tp).await;
    assert_eq!(st, StatusCode::OK, "{hot}");
    assert!(
        hot["worlds"].as_array().unwrap().iter().all(|w| w["starRating"].is_i64()),
        "hot 榜每项应带 starRating: {hot}"
    );

    // 详情：starRating=4。
    let (st, detail) = get_json(&app, &format!("/api/worlds/{w4}"), &tp).await;
    assert_eq!(st, StatusCode::OK, "{detail}");
    assert_eq!(detail["starRating"], 4, "详情应投影模板星级");
}

// ---------- R1：同源卡同世界唯一（规格 §7「这个世界只有一个唐三」） ----------

/// 造一张带同源两列的已过审卡；两列即发布路径（assets::source_identity）的物化结果。
async fn seed_source_char(
    state: &AppState,
    id: &str,
    owner: &str,
    name: &str,
    fingerprint: Option<&str>,
    pristine: i64,
) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, source_fingerprint, pristine, created_at) \
         VALUES (?, ?, ?, 1, ?, 'original', 'approved', 0, ?, ?, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(id)
    .bind(sample_card_json(id, name))
    .bind(fingerprint)
    .bind(pristine)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn active_member_count(state: &AppState, world_id: &str) -> i64 {
    sqlx::query("SELECT COUNT(*) AS n FROM world_members WHERE world_id=? AND status='active'")
        .bind(world_id)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
}

/// 同一提取源的第二张**原味卡** join 同世界 → 409 + 引导文案；占位者离场后名额释放。
#[tokio::test]
async fn join_rejects_second_pristine_card_from_same_source() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrT1").await;
    seed_user(&state, "usrT2").await;
    seed_source_char(&state, "chTang1", "usrT1", "唐三", Some("src-douluo"), 1).await;
    seed_source_char(&state, "chTang2", "usrT2", "唐三", Some("src-douluo"), 1).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "斗罗世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");
    let t1 = token(&state, "usrT1");
    let t2 = token(&state, "usrT2");

    let (st, body) = post_json(&app, &uri, &t1, None, json!({ "cloudCharacterId": "chTang1" })).await;
    assert_eq!(st, StatusCode::OK, "首张原味卡应成功: {body}");

    // 同指纹的第二张原味卡（别人的卡）→ 409 + 规格文案（含角色名）。
    let (st2, body2) = post_json(&app, &uri, &t2, None, json!({ "cloudCharacterId": "chTang2" })).await;
    assert_eq!(st2, StatusCode::CONFLICT, "同源原味卡应 409: {body2}");
    let msg = body2["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("这个世界已经有一个「唐三」了"), "文案应含角色名: {msg}");
    assert!(msg.contains("编辑出你自己的版本，或换一个世界实例"), "文案应给出两条出路: {msg}");
    assert_eq!(active_member_count(&state, &wid).await, 1, "被拒的同源卡不得落行");

    // 占位者离场（非 active 不占名额）→ 同源卡可以进。
    let (st_leave, _) = post_json(
        &app,
        &format!("/api/worlds/{wid}/leave"),
        &t1,
        None,
        json!({ "cloudCharacterId": "chTang1" }),
    )
    .await;
    assert_eq!(st_leave, StatusCode::OK);
    let (st3, body3) = post_json(&app, &uri, &t2, None, json!({ "cloudCharacterId": "chTang2" })).await;
    assert_eq!(st3, StatusCode::OK, "占位者离场后同源卡应放行: {body3}");
}

/// 硬约束：玩家**编辑过**的卡（pristine=0）即便同源也必须放行——撞卡压力转化为编辑创作激励。
#[tokio::test]
async fn join_allows_edited_card_from_same_source() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrE1").await;
    seed_user(&state, "usrE2").await;
    seed_source_char(&state, "chE1", "usrE1", "唐三", Some("src-douluo"), 1).await;
    seed_source_char(&state, "chE2", "usrE2", "唐三（我的版本）", Some("src-douluo"), 0).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "斗罗世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");

    let (st, body) =
        post_json(&app, &uri, &token(&state, "usrE1"), None, json!({ "cloudCharacterId": "chE1" })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st2, body2) =
        post_json(&app, &uri, &token(&state, "usrE2"), None, json!({ "cloudCharacterId": "chE2" })).await;
    assert_eq!(st2, StatusCode::OK, "编辑过的同源卡必须放行: {body2}");
    assert_eq!(active_member_count(&state, &wid).await, 2, "两张卡都应在场");
}

/// 迁移前老卡（source_fingerprint IS NULL）一律放行——不因缺字段拒绝入世。
#[tokio::test]
async fn join_allows_legacy_cards_without_fingerprint() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrL1").await;
    seed_user(&state, "usrL2").await;
    // 迁移回填态（NULL/0）与「原味但无提取源」（NULL/1，纯原创卡）两种都得放行。
    seed_source_char(&state, "chL1", "usrL1", "无名氏", None, 0).await;
    seed_source_char(&state, "chL2", "usrL2", "无名氏", None, 1).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "老世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");

    let (st, body) =
        post_json(&app, &uri, &token(&state, "usrL1"), None, json!({ "cloudCharacterId": "chL1" })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st2, body2) =
        post_json(&app, &uri, &token(&state, "usrL2"), None, json!({ "cloudCharacterId": "chL2" })).await;
    assert_eq!(st2, StatusCode::OK, "无指纹卡必须放行: {body2}");
    assert_eq!(active_member_count(&state, &wid).await, 2);
}

/// 不同提取源的原味卡互不影响（闸只按指纹匹配，不按角色名/卡数）。
#[tokio::test]
async fn join_allows_pristine_cards_from_different_sources() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrD1").await;
    seed_user(&state, "usrD2").await;
    seed_source_char(&state, "chD1", "usrD1", "唐三", Some("src-douluo"), 1).await;
    seed_source_char(&state, "chD2", "usrD2", "萧炎", Some("src-doupo"), 1).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "混搭世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");

    let (st, body) =
        post_json(&app, &uri, &token(&state, "usrD1"), None, json!({ "cloudCharacterId": "chD1" })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st2, body2) =
        post_json(&app, &uri, &token(&state, "usrD2"), None, json!({ "cloudCharacterId": "chD2" })).await;
    assert_eq!(st2, StatusCode::OK, "不同提取源应互不影响: {body2}");
    assert_eq!(active_member_count(&state, &wid).await, 2);
}

/// 回归：同一张原味卡重复 join 仍走成员行幂等分支，不被同源闸误伤（排除本卡自身）。
#[tokio::test]
async fn join_same_pristine_card_repeat_stays_idempotent() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrR1").await;
    seed_source_char(&state, "chR1", "usrR1", "唐三", Some("src-douluo"), 1).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "幂等世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");
    let t1 = token(&state, "usrR1");

    let (st, body) = post_json(&app, &uri, &t1, None, json!({ "cloudCharacterId": "chR1" })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let mid = body["membershipId"].as_str().unwrap().to_string();

    let (st2, body2) = post_json(&app, &uri, &t1, None, json!({ "cloudCharacterId": "chR1" })).await;
    assert_eq!(st2, StatusCode::OK, "同卡重复 join 不得被同源闸拦截: {body2}");
    assert_eq!(body2["membershipId"].as_str().unwrap(), mid, "应返回同一 membership");
    assert_eq!(active_member_count(&state, &wid).await, 1, "重复 join 不得多落行");
}

/// 发布路径物化两列：原味卡（draft + revision 0 + sourceWork）→ (指纹, 1)；
/// 改过 revision → pristine 0；无 sourceWork（纯原创）→ 指纹 NULL。
#[tokio::test]
async fn publish_materializes_source_fingerprint_and_pristine() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrPub").await;
    let tp = token(&state, "usrPub");

    let card = |lifecycle: &str, revision: i64, with_source: bool| {
        let mut c = json!({
            "schemaVersion": 2,
            "id": "local-card",
            "lifecycle": lifecycle,
            "revision": revision,
            "identity": { "name": "唐三" },
        });
        if with_source {
            c["identity"]["sourceWork"] =
                json!({ "sourceId": "hash-douluo", "title": "斗罗大陆" });
        }
        c
    };
    let publish = |local_id: &'static str, card: Value| {
        json!({ "localCardId": local_id, "cardJson": card, "rightsDeclaration": "original" })
    };

    for (local_id, body) in [
        ("card-pristine", publish("card-pristine", card("draft", 0, true))),
        ("card-edited", publish("card-edited", card("draft", 3, true))),
        ("card-original", publish("card-original", card("draft", 0, false))),
    ] {
        let (st, v) =
            post_json(&app, "/api/assets/characters", &tp, Some(local_id), body).await;
        assert_eq!(st, StatusCode::OK, "发布应成功: {v}");
    }

    let read = |local_id: &'static str| {
        let db = state.db.clone();
        async move {
            let row = sqlx::query(
                "SELECT source_fingerprint, pristine FROM cloud_characters WHERE local_card_id = ?",
            )
            .bind(local_id)
            .fetch_one(&db)
            .await
            .unwrap();
            let fp: Option<String> = row.try_get("source_fingerprint").unwrap();
            let pristine: i64 = row.try_get("pristine").unwrap();
            (fp, pristine)
        }
    };
    assert_eq!(read("card-pristine").await, (Some("hash-douluo".to_string()), 1), "原味卡应物化指纹 + pristine=1");
    assert_eq!(read("card-edited").await, (Some("hash-douluo".to_string()), 0), "改过 revision 即非原味");
    assert_eq!(read("card-original").await, (None, 1), "无 sourceWork → 指纹 NULL（不参与同源判定）");
}

/// 端到端：两个用户发布同一张原味卡 → 第二人 join 被同源闸拒（发布提取与 join 判定字段口径一致）。
#[tokio::test]
async fn published_identical_pristine_cards_collide_on_join() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrX1").await;
    seed_user(&state, "usrX2").await;
    let card = json!({
        "schemaVersion": 2,
        "id": "local-card",
        "lifecycle": "draft",
        "revision": 0,
        "identity": { "name": "唐三", "sourceWork": { "sourceId": "hash-douluo", "title": "斗罗大陆" } },
    });
    let body = json!({ "localCardId": "唐三卡", "cardJson": card, "rightsDeclaration": "original" });

    let mut ids = Vec::new();
    for (user, idem) in [("usrX1", "px1"), ("usrX2", "px2")] {
        let (st, v) =
            post_json(&app, "/api/assets/characters", &token(&state, user), Some(idem), body.clone()).await;
        assert_eq!(st, StatusCode::OK, "发布应成功: {v}");
        assert_eq!(v["moderation"], "approved", "机审 stub 直过（join 前置条件）: {v}");
        ids.push(v["id"].as_str().unwrap().to_string());
    }

    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "斗罗世界")).await.unwrap();
    let uri = format!("/api/worlds/{wid}/join");
    let (st, b) =
        post_json(&app, &uri, &token(&state, "usrX1"), None, json!({ "cloudCharacterId": ids[0] })).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st2, b2) =
        post_json(&app, &uri, &token(&state, "usrX2"), None, json!({ "cloudCharacterId": ids[1] })).await;
    assert_eq!(st2, StatusCode::CONFLICT, "同源原味卡应被拒: {b2}");
    assert!(
        b2["error"]["message"].as_str().unwrap_or("").contains("这个世界已经有一个「唐三」了"),
        "文案应含角色名: {b2}"
    );
}

// ==================== R1 生死契约三档（总规格 §11【拍板 24】）====================

mod lethality {
    //! 契约档三维正交的 server 侧接线：迁移默认 · join 契约签署 · 未成年红线 · 运营开关 · 引擎回灌。
    use super::*;
    use crate::worlds::{
        deathmatch_enabled, effective_lethality, lethality_label, DeathmatchSwitch,
        LETHALITY_CONSENT, LETHALITY_DEATHMATCH, LETHALITY_SANCTUARY,
    };
    use muse_engine::narrative::types::Lethality;

    /// 造用户并指定年龄声明（0 未声明 / 1 成年 / 2 未成年）——顶层 `seed_user` 恒落 0。
    async fn seed_user_age(state: &AppState, id: &str, age: i64) {
        sqlx::query(
            "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
             VALUES (?, '', ?, 'active', ?, ?)",
        )
        .bind(id)
        .bind(age)
        .bind(now_ms())
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
    }

    /// 建一个指定契约档的世界（走内部建房，不经 admin 开关校验——落库值即建房意图）。
    async fn seed_world_with(state: &AppState, title: &str, lethality: &str) -> String {
        let mut p = CreateWorldParams::official("tpl", 1, title);
        p.lethality = lethality.into();
        create_world(&state.db, p).await.unwrap()
    }

    async fn stored_lethality(state: &AppState, wid: &str) -> String {
        sqlx::query_scalar::<_, String>("SELECT lethality FROM worlds WHERE id = ?")
            .bind(wid)
            .fetch_one(&state.db)
            .await
            .unwrap()
    }

    async fn sign_audit_count(state: &AppState, wid: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_logs WHERE action='world.death_contract_signed' AND subject=?",
        )
        .bind(wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
    }

    // ---------- 迁移 0026：默认档 = 同意制，历史世界行为零变化 ----------

    #[tokio::test]
    async fn migration_defaults_existing_worlds_to_consent() {
        let state = test_state().await;
        let app = build_router(state.clone());

        // 模拟"迁移前就存在的世界行"：INSERT 显式不写 lethality 列，全靠迁移的 DEFAULT 兜。
        let wid = new_id("wld");
        let now = now_ms();
        sqlx::query(
            "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
             model_route_version, room_type, title, status, visibility, member_limit, tick_per_day, \
             state_revision, narrative_state_json, created_at, updated_at) \
             VALUES (?, 'tpl', 1, 'e', 'p', 'm', 'idle', '历史世界', 'open', 'official', 10, 3, 0, '{}', ?, ?)",
        )
        .bind(&wid)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await
        .unwrap();

        assert_eq!(stored_lethality(&state, &wid).await, LETHALITY_CONSENT, "历史行须落同意制");
        let world = load_world(&state.db, &wid).await.unwrap();
        assert_eq!(
            effective_lethality(&world.lethality),
            Lethality::Consent,
            "历史世界生效档 = 同意制（引擎侧行为与迁移前完全一致）"
        );

        // 行为零变化：join 不需要任何新字段，也不写签署留痕。
        seed_user(&state, "usrLegacy").await; // age_declared=0（未声明）——同意制世界不设年龄门
        seed_char(&state, "chLegacy", "usrLegacy", "approved", 0).await;
        let (st, body) = post_json(
            &app,
            &format!("/api/worlds/{wid}/join"),
            &token(&state, "usrLegacy"),
            None,
            json!({ "cloudCharacterId": "chLegacy" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "同意制世界 join 不受契约门影响: {body}");
        assert_eq!(body["lethality"], LETHALITY_CONSENT);
        assert_eq!(sign_audit_count(&state, &wid).await, 0, "非生死状世界不得留签署痕");
    }

    #[tokio::test]
    async fn create_world_defaults_and_persists_lethality() {
        let state = test_state().await;
        // 缺省 = 同意制。
        let default_wid =
            create_world(&state.db, CreateWorldParams::official("tpl", 1, "默认档世界")).await.unwrap();
        assert_eq!(stored_lethality(&state, &default_wid).await, LETHALITY_CONSENT);

        // 显式庇护档落库。
        let sanctuary = seed_world_with(&state, "庇护世界", LETHALITY_SANCTUARY).await;
        assert_eq!(stored_lethality(&state, &sanctuary).await, LETHALITY_SANCTUARY);

        // 非法值防御式归一为同意制（不落脏枚举）。
        let bogus = seed_world_with(&state, "非法档世界", "bogus").await;
        assert_eq!(stored_lethality(&state, &bogus).await, LETHALITY_CONSENT);
    }

    // ---------- 枚举归一 + 运营开关（VALIDATION.md §0.1 未验证功能默认关闭） ----------

    #[test]
    fn deathmatch_switch_defaults_to_off() {
        let _sw = DeathmatchSwitch::set(false);
        std::env::remove_var("MUSE_LETHALITY_DEATHMATCH");
        assert!(!deathmatch_enabled(), "生死状档必须默认关闭（未验证功能默认关闭）");
        // 配错的值不得静默开启高危档。
        std::env::set_var("MUSE_LETHALITY_DEATHMATCH", "maybe");
        assert!(!deathmatch_enabled(), "非法开关值须回落关闭");
        std::env::set_var("MUSE_LETHALITY_DEATHMATCH", "on");
        assert!(deathmatch_enabled(), "显式 on 应开启");
    }

    #[test]
    fn effective_lethality_degrades_conservatively() {
        {
            let _sw = DeathmatchSwitch::set(false);
            assert_eq!(
                effective_lethality(LETHALITY_DEATHMATCH),
                Lethality::Consent,
                "开关未开 → 生死状降级为同意制"
            );
            assert_eq!(lethality_label(LETHALITY_DEATHMATCH), LETHALITY_CONSENT, "投影同步降级");
        }
        {
            let _sw = DeathmatchSwitch::set(true);
            assert_eq!(effective_lethality(LETHALITY_DEATHMATCH), Lethality::Deathmatch);
            assert_eq!(lethality_label(LETHALITY_DEATHMATCH), LETHALITY_DEATHMATCH);
        }
        // 开关无关的两档 + 脏数据兜底。
        assert_eq!(effective_lethality(LETHALITY_SANCTUARY), Lethality::Sanctuary);
        assert_eq!(effective_lethality(LETHALITY_CONSENT), Lethality::Consent);
        assert_eq!(effective_lethality(""), Lethality::Consent, "空值 → 默认档");
        assert_eq!(effective_lethality("DEATHMATCH"), Lethality::Consent, "大小写不匹配 → 默认档");
        assert_eq!(effective_lethality("bogus"), Lethality::Consent, "未知值 → 默认档");
    }

    // ---------- join 契约签署：二次确认 ----------

    #[tokio::test]
    async fn join_deathmatch_requires_second_confirmation() {
        let _sw = DeathmatchSwitch::set(true);
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state, "usrAdult", 1).await;
        seed_char(&state, "chAdult", "usrAdult", "approved", 0).await;
        let wid = seed_world_with(&state, "生死场", LETHALITY_DEATHMATCH).await;
        let uri = format!("/api/worlds/{wid}/join");
        let tk = token(&state, "usrAdult");

        // join 前明示：详情页必须能看见档位与契约要求。
        let (st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &tk).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(detail["lethality"], LETHALITY_DEATHMATCH, "详情须明示契约档: {detail}");
        assert_eq!(detail["deathContractRequired"], true);

        // 缺二次确认 → 409 + 中文长句引导（绝不默认代签）。
        let (st, body) = post_json(&app, &uri, &tk, None, json!({ "cloudCharacterId": "chAdult" })).await;
        assert_eq!(st, StatusCode::CONFLICT, "缺确认应被拒: {body}");
        let msg = body["error"]["message"].as_str().unwrap_or("");
        assert!(msg.contains("生死状"), "文案须点明生死状: {body}");
        assert!(msg.contains("acceptDeathContract"), "文案须给出补救方式: {body}");
        // 被拒即零副作用：无成员行、无签署痕。
        assert_eq!(active_member_count(&state, &wid).await, 0);
        assert_eq!(sign_audit_count(&state, &wid).await, 0);

        // 显式 false 同样被拒（不接受"字段在但不确认"）。
        let (st, _) = post_json(
            &app,
            &uri,
            &tk,
            None,
            json!({ "cloudCharacterId": "chAdult", "acceptDeathContract": false }),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT);

        // 成年 + 确认 → 放行。
        let (st, body) = post_json(
            &app,
            &uri,
            &tk,
            None,
            json!({ "cloudCharacterId": "chAdult", "acceptDeathContract": true }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "成年 + 确认应可入: {body}");
        assert_eq!(body["status"], "active");
        assert_eq!(body["lethality"], LETHALITY_DEATHMATCH, "回执须明示签署的档位");
        assert_eq!(active_member_count(&state, &wid).await, 1);
    }

    // ---------- 🔴 真红线 §0.4：未成年禁入生死状（fail-closed） ----------

    #[tokio::test]
    async fn join_deathmatch_rejects_minor() {
        let _sw = DeathmatchSwitch::set(true);
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state, "usrMinor", 2).await; // 已声明未成年
        seed_char(&state, "chMinor", "usrMinor", "approved", 0).await;
        let wid = seed_world_with(&state, "生死场", LETHALITY_DEATHMATCH).await;

        // 即便带齐二次确认也一律拒——红线不因用户"愿意"而让路。
        let (st, body) = post_json(
            &app,
            &format!("/api/worlds/{wid}/join"),
            &token(&state, "usrMinor"),
            None,
            json!({ "cloudCharacterId": "chMinor", "acceptDeathContract": true }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "未成年禁入生死状（红线）: {body}");
        assert_eq!(active_member_count(&state, &wid).await, 0);
        assert_eq!(sign_audit_count(&state, &wid).await, 0, "被拒不得留签署痕");
    }

    #[tokio::test]
    async fn join_deathmatch_rejects_undeclared_age_fail_closed() {
        let _sw = DeathmatchSwitch::set(true);
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "usrUnknown").await; // age_declared = 0（未声明）
        seed_char(&state, "chUnknown", "usrUnknown", "approved", 0).await;
        let wid = seed_world_with(&state, "生死场", LETHALITY_DEATHMATCH).await;

        // fail-closed：年龄未知按未成年处理，绝不放行。
        let (st, body) = post_json(
            &app,
            &format!("/api/worlds/{wid}/join"),
            &token(&state, "usrUnknown"),
            None,
            json!({ "cloudCharacterId": "chUnknown", "acceptDeathContract": true }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "年龄未声明须按未成年拒绝: {body}");
        assert_eq!(active_member_count(&state, &wid).await, 0);

        // 同一世界的同意制对照：未声明年龄照常可入（年龄门只挂在生死状档上，不外溢）。
        let consent_wid = seed_world_with(&state, "同意制场", LETHALITY_CONSENT).await;
        let (st, body) = post_json(
            &app,
            &format!("/api/worlds/{consent_wid}/join"),
            &token(&state, "usrUnknown"),
            None,
            json!({ "cloudCharacterId": "chUnknown" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "年龄门不得外溢到同意制世界: {body}");
    }

    // ---------- 签署留痕（§0.2 全链审计） ----------

    #[tokio::test]
    async fn join_deathmatch_writes_signature_audit_log() {
        let _sw = DeathmatchSwitch::set(true);
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state, "usrSign", 1).await;
        seed_char(&state, "chSign", "usrSign", "approved", 0).await;
        let wid = seed_world_with(&state, "生死场", LETHALITY_DEATHMATCH).await;
        let uri = format!("/api/worlds/{wid}/join");
        let tk = token(&state, "usrSign");
        let body = json!({ "cloudCharacterId": "chSign", "acceptDeathContract": true });

        let (st, _) = post_json(&app, &uri, &tk, Some("k-sign"), body.clone()).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(sign_audit_count(&state, &wid).await, 1, "入场须留一条签署痕");

        // 痕内容：谁签的、哪张卡、什么档。
        let row = sqlx::query(
            "SELECT actor_id, actor_role, reason FROM audit_logs \
             WHERE action='world.death_contract_signed' AND subject=?",
        )
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(row.try_get::<String, _>("actor_id").unwrap(), "usrSign");
        assert_eq!(row.try_get::<String, _>("actor_role").unwrap(), "user");
        let reason: String = row.try_get("reason").unwrap();
        assert!(reason.contains("lethality=deathmatch"), "留痕须记档位: {reason}");
        assert!(reason.contains("character=chSign"), "留痕须记角色卡: {reason}");

        // 幂等重放 + 已在场重复 join：都不重复留痕（签署只认真实入场）。
        let (st, _) = post_json(&app, &uri, &tk, Some("k-sign"), body.clone()).await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = post_json(&app, &uri, &tk, None, body.clone()).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(sign_audit_count(&state, &wid).await, 1, "重放不得重复留痕");

        // 离场后复活 = 再次入场 → 再签一次。
        let (st, _) = post_json(
            &app,
            &format!("/api/worlds/{wid}/leave"),
            &tk,
            None,
            json!({ "cloudCharacterId": "chSign" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = post_json(&app, &uri, &tk, None, body).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(sign_audit_count(&state, &wid).await, 2, "复活入场须重新留痕");
    }

    // ---------- 运营开关关闭：生死状不生效（前后门双保险的"后门降级"） ----------

    #[tokio::test]
    async fn deathmatch_world_degrades_to_consent_when_switch_off() {
        let _sw = DeathmatchSwitch::set(false);
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "usrOff").await; // 未声明年龄
        seed_char(&state, "chOff", "usrOff", "approved", 0).await;
        // 落库仍是建房方的意图（deathmatch），但开关未开 → 生效档降级。
        let wid = seed_world_with(&state, "被降级的生死场", LETHALITY_DEATHMATCH).await;
        assert_eq!(stored_lethality(&state, &wid).await, LETHALITY_DEATHMATCH, "落库值保留意图");
        let world = load_world(&state.db, &wid).await.unwrap();
        assert_eq!(effective_lethality(&world.lethality), Lethality::Consent, "生效档降级为同意制");

        let tk = token(&state, "usrOff");
        // 详情页所见即所签：显示的是降级后的同意制，且不要求签署。
        let (_st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &tk).await;
        assert_eq!(detail["lethality"], LETHALITY_CONSENT);
        assert_eq!(detail["deathContractRequired"], false);

        // 不带确认、年龄未声明也能入（= 同意制世界的现行行为），且不留签署痕。
        let (st, body) = post_json(
            &app,
            &format!("/api/worlds/{wid}/join"),
            &tk,
            None,
            json!({ "cloudCharacterId": "chOff" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "开关未开时应按同意制放行: {body}");
        assert_eq!(body["lethality"], LETHALITY_CONSENT);
        assert_eq!(sign_audit_count(&state, &wid).await, 0, "未生效的生死状不得留签署痕");
    }

    // ---------- 引擎回灌：runtime 从世界行取档，而非恒传默认 ----------

    /// runtime 组装 RoundInput 时必须**从世界行回灌**契约档，且必须复用 `effective_lethality`
    /// （与 join 契约门同源）——否则会出现"玩家签的档"与"引擎跑的档"错配，或绕过运营开关降级。
    /// 采源码级断言（体例同 progression 的红线 grep 测试）：RoundInput 在 tick 事务深处组装，
    /// 端到端断言需要真实模型调用，成本不抵收益；而这条接线一旦被改回默认值，本断言立即红。
    #[test]
    fn runtime_backfills_lethality_from_world_row() {
        let runtime_src = include_str!("../runtime/mod.rs");
        assert!(
            runtime_src.contains("lethality: effective_lethality(&world.lethality)"),
            "runtime 组装 RoundInput 必须从 worlds.lethality 回灌生效档"
        );
        assert!(
            !runtime_src.contains("lethality: Lethality::default()"),
            "runtime 不得再恒传默认档——那会让生死状/庇护两档永不生效"
        );
    }
}

// ---------- 阵容头像按机审裁决过滤（Phase A 红线：未过审绝不下发） ----------

#[tokio::test]
async fn world_detail_roster_gates_avatar_on_moderation() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrAV").await;
    seed_char(&state, "chAV", "usrAV", "approved", 0).await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "头像世界")).await.unwrap();
    // 投放该角色（active 成员）。
    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, joined_at) VALUES (?, ?, 'usrAV', 'chAV', ?)",
    )
    .bind(new_id("wm"))
    .bind(&wid)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    let tk = token(&state, "usrAV");
    let uri = format!("/api/worlds/{wid}");

    // 无头像 → roster 不带 avatarUrl。
    let (st, body) = get_json(&app, &uri, &tk).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["roster"][0]["cloudCharacterId"], "chAV");
    assert!(body["roster"][0].get("avatarUrl").is_none(), "无头像不应带 avatarUrl");

    // 过审头像 → roster 带 avatarUrl。
    sqlx::query("UPDATE cloud_characters SET avatar_url = ?, avatar_moderation = 'approved' WHERE id = 'chAV'")
        .bind("/api/assets/objects/avatars/chAV.png")
        .execute(&state.db)
        .await
        .unwrap();
    let (_st, body) = get_json(&app, &uri, &tk).await;
    assert_eq!(
        body["roster"][0]["avatarUrl"], "/api/assets/objects/avatars/chAV.png",
        "过审头像应带 avatarUrl"
    );

    // 未过审（pending）→ roster 不带 avatarUrl（双过滤红线）。
    sqlx::query("UPDATE cloud_characters SET avatar_moderation = 'pending' WHERE id = 'chAV'")
        .execute(&state.db)
        .await
        .unwrap();
    let (_st, body) = get_json(&app, &uri, &tk).await;
    assert!(body["roster"][0].get("avatarUrl").is_none(), "未过审头像绝不下发");
}

// ---------- tick 幂等 ----------

#[tokio::test]
async fn tick_insertion_is_idempotent() {
    let state = test_state().await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "世界")).await.unwrap();

    assert!(runtime::insert_tick(&state.db, &wid, 0, 0).await.unwrap(), "首次插入应成功");
    assert!(!runtime::insert_tick(&state.db, &wid, 0, 0).await.unwrap(), "同 tick_no 重复插入应被拒");

    let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM world_ticks WHERE world_id=? AND tick_no=0")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(n, 1, "同 world_id+tick_no 只应有一行");
}

#[tokio::test]
async fn tick_skips_without_model_and_is_idempotent() {
    let state = test_state().await;
    let mut p = CreateWorldParams::official("tpl", 1, "世界");
    p.status = Some("running".into());
    let wid = create_world(&state.db, p).await.unwrap();

    assert!(runtime::insert_tick(&state.db, &wid, 0, 0).await.unwrap());
    // 无模型配置（model_route_version=dev-none 无匹配）→ 跳过、不 panic。
    let s1 = runtime::process_tick(&state, &wid, 0).await.unwrap();
    assert_eq!(s1, TickStatus::Skipped("no_model_config"));

    // tick 已 done → 再次处理幂等跳过。
    let s2 = runtime::process_tick(&state, &wid, 0).await.unwrap();
    assert_eq!(s2, TickStatus::Skipped("already_done"));

    let tstatus: String = sqlx::query("SELECT status FROM world_ticks WHERE world_id=? AND tick_no=0")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("status")
        .unwrap();
    assert_eq!(tstatus, "done");
}

// ---------- 预算熔断 ----------

#[tokio::test]
async fn budget_fuse_pauses_world() {
    let state = test_state().await;
    let mut p = CreateWorldParams::official("tpl", 1, "世界");
    p.status = Some("running".into());
    p.daily_token_budget = 100;
    let wid = create_world(&state.db, p).await.unwrap();

    // 今日已花满预算。
    let today = runtime::day_string(now_ms());
    sqlx::query("UPDATE world_budgets SET spent_tokens_today=100, budget_day=? WHERE world_id=?")
        .bind(&today)
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();

    runtime::insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    let status = runtime::process_tick(&state, &wid, 0).await.unwrap();
    assert_eq!(status, TickStatus::Fused);

    let w = load_world(&state.db, &wid).await.unwrap();
    assert_eq!(w.status, "paused", "熔断应暂停世界");

    let fused: i64 = sqlx::query("SELECT fused FROM world_budgets WHERE world_id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("fused")
        .unwrap();
    assert_eq!(fused, 1);
}

// ---------- 受众投影 ----------

fn domain_event(id: &str, vis: EventVisibility) -> DomainEvent {
    DomainEvent {
        schema_version: 1,
        id: id.into(),
        run_id: "r".into(),
        sequence: 0,
        timestamp: 0,
        event_type: DomainEventType::DialogueSpoken,
        actor_ids: vec!["chA".into()],
        target_ids: None,
        fact: json!({ "summary": "机密内容" }),
        state_patch_id: "sp".into(),
        caused_by: vec![],
        visibility: vis,
    }
}

#[test]
fn projection_maps_audience_characters_to_principals() {
    let members = vec![
        ProjectionMember { character_key: "chA".into(), user_id: "usrA".into() },
        ProjectionMember { character_key: "chB".into(), user_id: "usrB".into() },
    ];
    let events = vec![
        domain_event("de-pub", EventVisibility::Public),
        domain_event(
            "de-priv",
            EventVisibility::Private { audience_character_ids: vec!["chA".into()] },
        ),
    ];
    let projected = project_domain_events(&events, &members);
    assert_eq!(projected[0].visibility, "public");
    assert!(projected[0].audience_user_ids.is_empty());
    assert_eq!(projected[1].visibility, "private");
    // 受众角色 chA → principal usrA（非 usrB）。
    assert_eq!(projected[1].audience_user_ids, vec!["usrA".to_string()]);
}

#[test]
fn ws_fanout_filters_by_principal() {
    let audience = Some(vec!["usrA".to_string()]);
    assert!(ws_visible(&audience, "usrA"));
    assert!(!ws_visible(&audience, "usrB"));
    assert!(ws_visible(&None, "usrB")); // public 对所有连接可见
}

#[tokio::test]
async fn events_query_enforces_audience_isolation() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_user(&state, "usrB").await;
    // official 世界：A、B 都有观战资格，但私有事件仍按 principal 硬隔离。
    let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "世界")).await.unwrap();

    let members = vec![
        ProjectionMember { character_key: "chA".into(), user_id: "usrA".into() },
        ProjectionMember { character_key: "chB".into(), user_id: "usrB".into() },
    ];
    let events = vec![
        domain_event("de-pub", EventVisibility::Public),
        domain_event(
            "de-priv",
            EventVisibility::Private { audience_character_ids: vec!["chA".into()] },
        ),
    ];
    let projected = project_domain_events(&events, &members);
    events::persist_events(&state.db, &wid, 0, &projected).await.unwrap();

    let ta = token(&state, "usrA");
    let tb = token(&state, "usrB");
    let uri = format!("/api/worlds/{wid}/events");

    // A：看到 public + 自己的 private（共 2 条）。
    let (sta, ba) = get_json(&app, &uri, &ta).await;
    assert_eq!(sta, StatusCode::OK);
    let a_events = ba["events"].as_array().unwrap();
    assert_eq!(a_events.len(), 2, "A 应见 public + 自己的 private");
    assert!(a_events.iter().any(|e| e["visibility"] == "private"));

    // B：只看到 public（私有事件对 audience 之外硬隔离，查询层不可见）。
    let (stb, bb) = get_json(&app, &uri, &tb).await;
    assert_eq!(stb, StatusCode::OK);
    let b_events = bb["events"].as_array().unwrap();
    assert_eq!(b_events.len(), 1, "B 只应见 public");
    assert_eq!(b_events[0]["visibility"], "public");
}

// ---------- 世界发现：标题搜索 q + 热门排序 sort=hot（0017） ----------

mod discovery {
    //! GET /worlds 发现能力：q 大小写不敏感/通配符转义、sort=hot 热度分与快照语义、
    //! sort=new + q 组合分页、非法 sort 400、默认行为零回归。
    use super::*;
    use sqlx::AnyPool;

    /// 造 world_events 行（仅 NOT NULL 无默认列），occurred_at 由调用方指定以控 48h 热度窗。
    async fn seed_world_event(db: &AnyPool, world_id: &str, occurred_at: i64) {
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, visibility, occurred_at) \
             VALUES (?, ?, 0, 0, ?, 'dialogue_spoken', 'public', ?)",
        )
        .bind(new_id("we"))
        .bind(world_id)
        .bind(new_id("de"))
        .bind(occurred_at)
        .execute(db)
        .await
        .unwrap();
    }

    /// 造 gift_events 行，created_at 由调用方指定以控 7 天打赏窗。
    async fn seed_gift(db: &AnyPool, world_id: &str, gift_count: i64, created_at: i64) {
        sqlx::query(
            "INSERT INTO gift_events (id, world_id, sku, gift_count, created_at) VALUES (?, ?, 'rose', ?, ?)",
        )
        .bind(new_id("ge"))
        .bind(world_id)
        .bind(gift_count)
        .bind(created_at)
        .execute(db)
        .await
        .unwrap();
    }

    /// 造 active 成员行（status 默认 active；schema 无外键约束，直插即可）。
    async fn seed_member(db: &AnyPool, world_id: &str, character_id: &str) {
        sqlx::query(
            "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, joined_at) VALUES (?, ?, 'usrD', ?, ?)",
        )
        .bind(new_id("wm"))
        .bind(world_id)
        .bind(character_id)
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
    }

    fn titles(body: &Value) -> Vec<String> {
        body["worlds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["title"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn list_worlds_default_behavior_regression() {
        // 零回归：无 q/sort 时现行为不变——只出 open/running + official/public，
        // created_at DESC，现有字段齐全且不带 hotScore，type 过滤仍工作。
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "usrD").await;
        let tk = token(&state, "usrD");

        let wid = create_world(&state.db, CreateWorldParams::official("tpl", 1, "公开世界")).await.unwrap();
        let mut p = CreateWorldParams::official("tpl", 1, "私有世界");
        p.visibility = "private".into();
        create_world(&state.db, p).await.unwrap();

        let (st, body) = get_json(&app, "/api/worlds", &tk).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let ws = body["worlds"].as_array().unwrap();
        assert_eq!(ws.len(), 1, "私有世界不应出现在大厅");
        assert_eq!(ws[0]["id"], wid.as_str());
        for key in ["roomType", "title", "status", "visibility", "memberLimit", "memberCount", "tickPerDay", "aiLabel"] {
            assert!(ws[0].get(key).is_some(), "现有字段缺失: {key}");
        }
        assert!(ws[0].get("hotScore").is_none(), "默认（sort=new）不应带 hotScore");
        assert!(body["nextCursor"].is_null(), "不足一页 nextCursor 应为 null");

        // type 过滤照旧。
        let (st2, body2) = get_json(&app, "/api/worlds?type=arena", &tk).await;
        assert_eq!(st2, StatusCode::OK);
        assert!(body2["worlds"].as_array().unwrap().is_empty(), "idle 世界不应命中 type=arena");
    }

    #[tokio::test]
    async fn list_worlds_q_matches_case_insensitive_and_escapes_wildcards() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "usrD").await;
        let tk = token(&state, "usrD");

        create_world(&state.db, CreateWorldParams::official("tpl", 1, "魔法学院Alpha")).await.unwrap();
        create_world(&state.db, CreateWorldParams::official("tpl", 1, "剑与远征")).await.unwrap();
        create_world(&state.db, CreateWorldParams::official("tpl", 1, "折扣50%世界")).await.unwrap();
        create_world(&state.db, CreateWorldParams::official("tpl", 1, "under_score")).await.unwrap();

        // 命中：大小写不敏感（ALPHA 命中 Alpha）。
        let (st, body) = get_json(&app, "/api/worlds?q=ALPHA", &tk).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(titles(&body), vec!["魔法学院Alpha"]);

        // 不命中 → 空列表。
        let (_st, body) = get_json(&app, "/api/worlds?q=neverland", &tk).await;
        assert!(body["worlds"].as_array().unwrap().is_empty(), "不命中应返回空列表");

        // 转义：q='%'（URL 编码 %25）只命中标题真含 % 的世界，不得当通配符匹配全部。
        let (_st, body) = get_json(&app, "/api/worlds?q=%25", &tk).await;
        assert_eq!(titles(&body), vec!["折扣50%世界"], "'%' 不得通配误匹配");

        // 转义：q='_' 只命中标题真含下划线的世界（'_' 不得当单字符通配）。
        let (_st, body) = get_json(&app, "/api/worlds?q=_", &tk).await;
        assert_eq!(titles(&body), vec!["under_score"], "'_' 不得通配误匹配");

        // 空串 q 视为无搜索 → 全部可见世界。
        let (_st, body) = get_json(&app, "/api/worlds?q=", &tk).await;
        assert_eq!(body["worlds"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn list_worlds_hot_ranks_by_recent_activity_with_hot_score() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "usrD").await;
        let tk = token(&state, "usrD");
        let now = now_ms();
        let hour = 3600 * 1000_i64;

        let hot = create_world(&state.db, CreateWorldParams::official("tpl", 1, "火热世界")).await.unwrap();
        let cold = create_world(&state.db, CreateWorldParams::official("tpl", 1, "冷清世界")).await.unwrap();
        // 钉死 created_at：hot 更旧、cold 更新 → sort=new 下 cold 在前，热度榜必须逆转该顺序。
        sqlx::query("UPDATE worlds SET created_at=? WHERE id=?").bind(now - 10_000).bind(&hot).execute(&state.db).await.unwrap();
        sqlx::query("UPDATE worlds SET created_at=? WHERE id=?").bind(now).bind(&cold).execute(&state.db).await.unwrap();

        // 热世界：近 48h 事件 3 条 + 近 7 天打赏 gift_count 共 4 + active 成员 1 → 3×1 + 4×5 + 1×2 = 25。
        seed_world_event(&state.db, &hot, now - hour).await;
        seed_world_event(&state.db, &hot, now - 2 * hour).await;
        seed_world_event(&state.db, &hot, now - 47 * hour).await; // 贴近窗沿，仍在 48h 内
        seed_gift(&state.db, &hot, 3, now - 24 * hour).await;
        seed_gift(&state.db, &hot, 1, now - 6 * 24 * hour).await; // 仍在 7 天内
        seed_member(&state.db, &hot, "chHot1").await;

        // 冷世界：事件在 48h 窗外、打赏在 7 天窗外、无成员 → 热度 0（窗外活动不计分）。
        seed_world_event(&state.db, &cold, now - 72 * hour).await;
        seed_world_event(&state.db, &cold, now - 72 * hour).await;
        seed_gift(&state.db, &cold, 9, now - 8 * 24 * hour).await;

        // 基线（sort=new）：cold 更新在前。
        let (st_new, body_new) = get_json(&app, "/api/worlds", &tk).await;
        assert_eq!(st_new, StatusCode::OK, "{body_new}");
        assert_eq!(titles(&body_new), vec!["冷清世界", "火热世界"]);

        // sort=hot：热度逆转顺序，每项带 hotScore，快照榜 nextCursor 恒 null。
        let (st, body) = get_json(&app, "/api/worlds?sort=hot", &tk).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(titles(&body), vec!["火热世界", "冷清世界"]);
        let ws = body["worlds"].as_array().unwrap();
        assert_eq!(ws[0]["hotScore"], 25, "3事件×1 + 4打赏×5 + 1成员×2 = 25");
        assert_eq!(ws[0]["memberCount"], 1);
        assert_eq!(ws[1]["hotScore"], 0, "窗外事件/打赏不计分");
        assert!(body["nextCursor"].is_null(), "热度榜是快照，不返回游标");

        // hot 叠加 q：仍按热度出、带 hotScore。
        let (st_q, body_q) = get_json(&app, "/api/worlds?sort=hot&q=%E7%81%AB%E7%83%AD", &tk).await; // q=火热
        assert_eq!(st_q, StatusCode::OK, "{body_q}");
        assert_eq!(titles(&body_q), vec!["火热世界"]);
        assert_eq!(body_q["worlds"][0]["hotScore"], 25);
    }

    #[tokio::test]
    async fn list_worlds_new_with_q_paginates_with_cursor() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "usrD").await;
        let tk = token(&state, "usrD");

        for i in 1..=3 {
            create_world(&state.db, CreateWorldParams::official("tpl", 1, format!("搜索目标world{i}"))).await.unwrap();
        }
        create_world(&state.db, CreateWorldParams::official("tpl", 1, "无关世界")).await.unwrap();

        // 第一页：q + limit=2 → 2 条命中 + nextCursor。
        let (st, p1) = get_json(&app, "/api/worlds?q=world&limit=2", &tk).await;
        assert_eq!(st, StatusCode::OK, "{p1}");
        assert_eq!(p1["worlds"].as_array().unwrap().len(), 2);
        let cur = p1["nextCursor"].as_str().expect("第一页应有 nextCursor").to_string();

        // 第二页：cursor + 同 q → 剩余 1 条，翻页尽头 nextCursor=null。
        let (st2, p2) = get_json(&app, &format!("/api/worlds?q=world&limit=2&cursor={cur}"), &tk).await;
        assert_eq!(st2, StatusCode::OK, "{p2}");
        assert_eq!(p2["worlds"].as_array().unwrap().len(), 1);
        assert!(p2["nextCursor"].is_null());

        // 两页合计 3 条、无重复、全命中 q（"无关世界"不出现）。
        let mut all = titles(&p1);
        all.extend(titles(&p2));
        assert_eq!(all.len(), 3);
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 3, "翻页不得重复");
        assert!(all.iter().all(|t| t.contains("world")), "{all:?}");
    }

    #[tokio::test]
    async fn list_worlds_rejects_invalid_sort() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "usrD").await;
        let tk = token(&state, "usrD");

        let (st, body) = get_json(&app, "/api/worlds?sort=hottest", &tk).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "bad_request");

        // 显式 sort=new / sort=hot 均合法。
        let (st_new, _) = get_json(&app, "/api/worlds?sort=new", &tk).await;
        assert_eq!(st_new, StatusCode::OK);
        let (st_hot, _) = get_json(&app, "/api/worlds?sort=hot", &tk).await;
        assert_eq!(st_hot, StatusCode::OK);
    }
}

// ---------- 世界封面 coverUrl（迁移 0027）+ 下一拍 nextTickEstimatedAt ----------

mod cover_and_next_tick {
    //! 封面：上传 → 图审 → 下发全链路；🔴 未过审绝不下发；无封面不含该键（不是空串）；
    //! 对象回读路径穿越被拒；权限矩阵（官方房运营 / 创作者房房主）。
    //! 下一拍：仅 running 的 interval 世界可确定性推算，其余一律不下发。

    use super::*;
    use crate::worlds::{next_tick_estimated_at, tick_interval_ms, visible_cover_url};

    /// 带角色的 access token（默认 `token()` 恒为 "user"，官方房封面需运营角色）。
    fn token_with_role(state: &AppState, user_id: &str, role: &str) -> String {
        crate::auth::issue_access(&state.config.jwt_secret, user_id, role, 3600).unwrap()
    }

    /// 原始字节 GET（对象回读返回二进制，非 JSON，故不能用 `get_json`）。
    async fn get_raw(app: &axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
        let req = Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let stat = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        (stat, bytes)
    }

    /// 封面上传 body（base64 JSON，形态同角色立绘）。
    fn cover_body(bytes: &[u8], mime: &str) -> Value {
        use base64::Engine as _;
        json!({
            "imageBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "mime": mime,
        })
    }

    /// 创作者世界（public + host_user_id），封面归房主。
    async fn seed_host_world(state: &AppState, title: &str, host: &str) -> String {
        let mut p = CreateWorldParams::official("tpl_cover", 1, title);
        p.visibility = "public".into();
        p.host_user_id = Some(host.into());
        create_world(&state.db, p).await.unwrap()
    }

    /// 直接改库模拟机审给出非 approved 裁决（DevModeration::check_image 恒直过，
    /// 无法从上传路径造出 pending/rejected；范式同 assets::tests 对 avatar_moderation 的做法）。
    async fn force_cover_moderation(state: &AppState, world_id: &str, verdict: &str) {
        sqlx::query("UPDATE worlds SET cover_moderation = ? WHERE id = ?")
            .bind(verdict)
            .bind(world_id)
            .execute(&state.db)
            .await
            .unwrap();
    }

    /// 从列表里挑出指定标题的世界项。
    fn find_world<'a>(body: &'a Value, title: &str) -> &'a Value {
        body["worlds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["title"].as_str() == Some(title))
            .unwrap_or_else(|| panic!("列表里没有「{title}」: {body}"))
    }

    // ===== 封面：全链路 =====

    /// 上传 → 过审 → 上传回执/详情/列表(new+hot) 都带 coverUrl → 对象回读拿到原始字节。
    #[tokio::test]
    async fn cover_upload_then_projected_everywhere_and_readable() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "coverHost").await;
        let th = token(&state, "coverHost");
        let wid = seed_host_world(&state, "有封面的世界", "coverHost").await;

        let raw: &[u8] = b"\x89PNG\r\n\x1a\n-fake-cover-bytes-\x00\x01\x02\xff";
        let (st, up) =
            post_json(&app, &format!("/api/worlds/{wid}/cover"), &th, None, cover_body(raw, "image/png")).await;
        assert_eq!(st, StatusCode::OK, "{up}");
        assert_eq!(up["moderation"], "approved", "dev 图审直过");
        let url = up["coverUrl"].as_str().expect("过审应回传 coverUrl");
        assert_eq!(url, format!("/api/assets/objects/covers/{wid}.png"), "对象键以世界 id 命名");

        // 详情投影。
        let (st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &th).await;
        assert_eq!(st, StatusCode::OK, "{detail}");
        assert_eq!(detail["coverUrl"].as_str(), Some(url), "详情应下发 coverUrl");

        // 列表（new + hot）投影。
        let (_st, list) = get_json(&app, "/api/worlds", &th).await;
        assert_eq!(find_world(&list, "有封面的世界")["coverUrl"].as_str(), Some(url), "sort=new 应下发 coverUrl");
        let (_st, hot) = get_json(&app, "/api/worlds?sort=hot", &th).await;
        assert_eq!(find_world(&hot, "有封面的世界")["coverUrl"].as_str(), Some(url), "sort=hot 应下发 coverUrl");

        // 对象回读：原样字节 + 正确 Content-Type 路径。
        let (st, bytes) = get_raw(&app, url).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(bytes, raw, "回读应得到上传的原始字节");
    }

    /// 🔴 红线：未过审（pending / rejected）封面在**任何读取面**都不下发。
    #[tokio::test]
    async fn unapproved_cover_is_never_projected() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "modHost").await;
        let th = token(&state, "modHost");
        let wid = seed_host_world(&state, "待审封面世界", "modHost").await;

        let (st, _up) = post_json(
            &app,
            &format!("/api/worlds/{wid}/cover"),
            &th,
            None,
            cover_body(b"\x89PNG-pending", "image/png"),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        for verdict in ["pending", "rejected"] {
            force_cover_moderation(&state, &wid, verdict).await;

            let (_st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &th).await;
            assert!(
                detail.get("coverUrl").is_none(),
                "🔴 {verdict} 封面不得出现在详情: {detail}"
            );

            let (_st, list) = get_json(&app, "/api/worlds", &th).await;
            assert!(
                find_world(&list, "待审封面世界").get("coverUrl").is_none(),
                "🔴 {verdict} 封面不得出现在 sort=new 列表: {list}"
            );

            let (_st, hot) = get_json(&app, "/api/worlds?sort=hot", &th).await;
            assert!(
                find_world(&hot, "待审封面世界").get("coverUrl").is_none(),
                "🔴 {verdict} 封面不得出现在 sort=hot 列表: {hot}"
            );
        }

        // 改判回 approved 后无需重传即恢复下发（cover_url 无论裁决都已落库）。
        force_cover_moderation(&state, &wid, "approved").await;
        let (_st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &th).await;
        assert!(detail["coverUrl"].as_str().is_some(), "改判 approved 后应恢复下发: {detail}");
    }

    /// 无封面世界：coverUrl **键缺席**（不是空串、不是 null），前端据此走确定性内置位图兜底。
    #[tokio::test]
    async fn world_without_cover_omits_cover_url_key() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "plainUser").await;
        let tk = token(&state, "plainUser");
        let wid = create_world(&state.db, CreateWorldParams::official("tpl_x", 1, "无封面世界")).await.unwrap();

        let (_st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &tk).await;
        assert!(detail.get("coverUrl").is_none(), "无封面详情不得含 coverUrl 键: {detail}");

        let (_st, list) = get_json(&app, "/api/worlds", &tk).await;
        let item = find_world(&list, "无封面世界");
        assert!(item.get("coverUrl").is_none(), "无封面列表项不得含 coverUrl 键: {item}");
        // 显式排除"下发空串"这条错误实现。
        assert_ne!(item.get("coverUrl").and_then(|v| v.as_str()), Some(""), "绝不下发空串");
    }

    /// 封面读取面过滤（纯函数）：只有 approved 且非空才给 URL。
    #[test]
    fn visible_cover_url_gates_on_approved() {
        let url = || Some("/api/assets/objects/covers/wld_x.png".to_string());
        assert_eq!(visible_cover_url(url(), Some("approved")), url(), "approved 才下发");
        assert_eq!(visible_cover_url(url(), Some("pending")), None, "🔴 pending 不下发");
        assert_eq!(visible_cover_url(url(), Some("rejected")), None, "🔴 rejected 不下发");
        assert_eq!(visible_cover_url(url(), None), None, "无裁决（从未上传）不下发");
        assert_eq!(visible_cover_url(None, Some("approved")), None, "无 URL 不下发");
        assert_eq!(visible_cover_url(Some("   ".into()), Some("approved")), None, "空白 URL 归零，不下发空串");
    }

    // ===== 封面：对象回读安全 =====

    /// 路径穿越硬防护：含 `..`、白名单外前缀、绝对路径一律 404。
    #[tokio::test]
    async fn cover_object_readback_rejects_path_traversal() {
        let state = test_state().await;
        let app = build_router(state.clone());

        for bad in [
            "/api/assets/objects/covers/../avatars/x.png",
            "/api/assets/objects/covers/../../etc/passwd",
            "/api/assets/objects/notcovers/x.png",
            "/api/assets/objects/x.png",
        ] {
            let (st, _) = get_raw(&app, bad).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "路径穿越/白名单外键必须 404: {bad}");
        }
        // 合法前缀但对象不存在 → 同样 404（不泄露存在性差异）。
        let (st, _) = get_raw(&app, "/api/assets/objects/covers/wld_nope.png").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    // ===== 封面：权限矩阵 =====

    /// 创作者世界：仅房主可设封面，他人 403；官方世界：仅运营角色可设，普通用户 403。
    #[tokio::test]
    async fn cover_upload_permission_matrix() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "owner").await;
        seed_user(&state, "stranger").await;
        seed_user(&state, "ops").await;
        let t_owner = token(&state, "owner");
        let t_stranger = token(&state, "stranger");
        let t_ops = token_with_role(&state, "ops", "operator");
        let t_reviewer = token_with_role(&state, "reviewer1", "reviewer");

        let creator_world = seed_host_world(&state, "创作者世界", "owner").await;
        let official_world =
            create_world(&state.db, CreateWorldParams::official("tpl_o", 1, "官方世界")).await.unwrap();
        let body = || cover_body(b"\x89PNG-perm", "image/png");

        // 创作者世界：房主 OK。
        let (st, _) = post_json(&app, &format!("/api/worlds/{creator_world}/cover"), &t_owner, None, body()).await;
        assert_eq!(st, StatusCode::OK, "房主可为自己的世界设封面");
        // 创作者世界：他人 403。
        let (st, _) = post_json(&app, &format!("/api/worlds/{creator_world}/cover"), &t_stranger, None, body()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "非房主不得设创作者世界封面");
        // 创作者世界：运营也不得替创作者换图（处置手段是审核态，不是换图）。
        let (st, _) = post_json(&app, &format!("/api/worlds/{creator_world}/cover"), &t_ops, None, body()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "运营不得覆盖创作者封面");

        // 官方世界：运营 OK。
        let (st, v) = post_json(&app, &format!("/api/worlds/{official_world}/cover"), &t_ops, None, body()).await;
        assert_eq!(st, StatusCode::OK, "运营可为官方世界设封面: {v}");
        // 官方世界：普通用户 403。
        let (st, _) = post_json(&app, &format!("/api/worlds/{official_world}/cover"), &t_owner, None, body()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "普通用户不得设官方世界封面");
        // 官方世界：reviewer（审核角色，非投放角色）403。
        let (st, _) = post_json(&app, &format!("/api/worlds/{official_world}/cover"), &t_reviewer, None, body()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "reviewer 不承担内容投放职责");

        // 不存在的世界 → 404。
        let (st, _) = post_json(&app, "/api/worlds/wld_nope/cover", &t_ops, None, body()).await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        // 未鉴权 → 401。
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/worlds/{creator_world}/cover"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body()).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 入参校验：MIME 白名单外、空数据、超 1MB 上限一律 400。
    #[tokio::test]
    async fn cover_upload_rejects_bad_mime_empty_and_oversize() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "valHost").await;
        let th = token(&state, "valHost");
        let wid = seed_host_world(&state, "校验世界", "valHost").await;
        let uri = format!("/api/worlds/{wid}/cover");

        let (st, _) = post_json(&app, &uri, &th, None, cover_body(b"GIF89a", "image/gif")).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "gif 不在白名单");

        let (st, _) = post_json(&app, &uri, &th, None, cover_body(b"", "image/png")).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "空数据应拒");

        let (st, _) =
            post_json(&app, &uri, &th, None, json!({ "imageBase64": "!!not-base64!!", "mime": "image/png" })).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "非法 base64 应拒");

        let too_big = vec![0u8; 1024 * 1024 + 1];
        let (st, _) = post_json(&app, &uri, &th, None, cover_body(&too_big, "image/png")).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "超 1MB 上限应拒");

        // 全程失败 → 世界仍无封面（不留半截状态）。
        let (_st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &th).await;
        assert!(detail.get("coverUrl").is_none(), "校验失败不得落封面: {detail}");
    }

    // ===== 下一拍 nextTickEstimatedAt =====

    /// 排拍间隔公式必须与 `runtime::schedule_due_ticks` 的
    /// `86_400_000 / tick_per_day.max(1)` 逐字一致——改调度器时这条会红。
    #[test]
    fn tick_interval_ms_mirrors_scheduler() {
        assert_eq!(tick_interval_ms(3), 86_400_000 / 3, "默认 3 拍/天 = 8 小时");
        assert_eq!(tick_interval_ms(1), 86_400_000);
        assert_eq!(tick_interval_ms(24), 3_600_000);
        // max(1) 防除零（脏数据 tick_per_day <= 0）。
        assert_eq!(tick_interval_ms(0), 86_400_000, "0 拍/天按 1 兜底，绝不 panic");
        assert_eq!(tick_interval_ms(-5), 86_400_000, "负值同样按 1 兜底");
    }

    /// running 的 interval 世界：下一拍 = 末拍入队时刻 + 间隔；从未有拍 → "现在"。
    #[test]
    fn next_tick_estimated_at_is_last_tick_plus_interval() {
        let interval = 8 * 3600 * 1000;
        let now = 1_700_000_000_000_i64;
        assert_eq!(
            next_tick_estimated_at("running", "interval", interval, Some(now - 1000), now),
            Some(now - 1000 + interval),
            "锚在末拍 created_at，不是 now——单拍跑慢/失败不会让排期整体漂移"
        );
        assert_eq!(
            next_tick_estimated_at("running", "interval", interval, None, now),
            Some(now),
            "无历史拍 → 调度器下一轮即排 → 下一拍就是现在"
        );
        // 已经过点（末拍很久以前）→ 给出过去时刻，表示"已到期待排"，不谎报未来。
        let overdue = next_tick_estimated_at("running", "interval", interval, Some(now - 10 * interval), now);
        assert_eq!(overdue, Some(now - 9 * interval));
        assert!(overdue.unwrap() < now, "过期世界诚实给过去时刻，不伪造未来时间");
    }

    /// 🔴 算不准就不给：非 running 状态、event 时间线一律 None。
    #[test]
    fn next_tick_estimated_at_none_when_not_computable() {
        let interval = 8 * 3600 * 1000;
        let now = 1_700_000_000_000_i64;
        for status in ["open", "paused", "ended"] {
            assert_eq!(
                next_tick_estimated_at(status, "interval", interval, Some(now), now),
                None,
                "{status} 世界不进调度器 WHERE status='running'，没有下一拍"
            );
        }
        assert_eq!(
            next_tick_estimated_at("running", "event", interval, Some(now), now),
            None,
            "event 房（背靠背 DES / 手动端点驱动）无墙钟表达式，宁可不给"
        );
    }

    /// 集成：running 的 interval 世界，详情与列表都带 nextTickEstimatedAt = 末拍 created_at + 间隔。
    #[tokio::test]
    async fn next_tick_projected_for_running_interval_world() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "tickUser").await;
        let tk = token(&state, "tickUser");
        let wid = create_world(&state.db, CreateWorldParams::official("tpl_t", 1, "跑动世界")).await.unwrap();
        sqlx::query("UPDATE worlds SET status='running' WHERE id=?")
            .bind(&wid)
            .execute(&state.db)
            .await
            .unwrap();

        // 末拍入队时刻钉死，便于精确断言。
        let last_tick_at = now_ms() - 60_000;
        sqlx::query(
            "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, created_at) \
             VALUES (?, ?, 0, 0, 'done', ?)",
        )
        .bind(new_id("tick"))
        .bind(&wid)
        .bind(last_tick_at)
        .execute(&state.db)
        .await
        .unwrap();

        let expected = last_tick_at + tick_interval_ms(3); // official() 默认 tick_per_day=3

        let (st, detail) = get_json(&app, &format!("/api/worlds/{wid}"), &tk).await;
        assert_eq!(st, StatusCode::OK, "{detail}");
        assert_eq!(detail["nextTickEstimatedAt"].as_i64(), Some(expected), "详情下一拍 = 末拍 + 间隔");

        let (_st, list) = get_json(&app, "/api/worlds", &tk).await;
        assert_eq!(
            find_world(&list, "跑动世界")["nextTickEstimatedAt"].as_i64(),
            Some(expected),
            "列表下一拍与详情同源"
        );
        let (_st, hot) = get_json(&app, "/api/worlds?sort=hot", &tk).await;
        assert_eq!(find_world(&hot, "跑动世界")["nextTickEstimatedAt"].as_i64(), Some(expected));
    }

    /// 集成 🔴：open 世界（大厅里最常见）与 event 世界一律**不含** nextTickEstimatedAt 键——
    /// 宁可不给，也不给一个会漂移的假时间（用户会按它安排回访）。
    #[tokio::test]
    async fn next_tick_absent_for_open_and_event_worlds() {
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user(&state, "absentUser").await;
        let tk = token(&state, "absentUser");

        // open（建房默认状态，未开跑）。
        create_world(&state.db, CreateWorldParams::official("tpl_a", 1, "未开跑世界")).await.unwrap();

        // running 但 event 时间线（放置房背靠背，无墙钟）。
        let mut p = CreateWorldParams::official("tpl_b", 1, "放置房世界");
        p.timeline_mode = "event".into();
        let ev = create_world(&state.db, p).await.unwrap();
        sqlx::query("UPDATE worlds SET status='running' WHERE id=?")
            .bind(&ev)
            .execute(&state.db)
            .await
            .unwrap();

        let (_st, list) = get_json(&app, "/api/worlds", &tk).await;
        for title in ["未开跑世界", "放置房世界"] {
            let item = find_world(&list, title);
            assert!(item.get("nextTickEstimatedAt").is_none(), "🔴 {title} 不得含 nextTickEstimatedAt: {item}");
        }
        let (_st, detail) = get_json(&app, &format!("/api/worlds/{ev}"), &tk).await;
        assert!(
            detail.get("nextTickEstimatedAt").is_none(),
            "🔴 event 房详情不得含 nextTickEstimatedAt: {detail}"
        );
    }
}

// ---------- P2 房主建房 POST /worlds + 开房费 charge（feature=billing/arena 才装配该端点） ----------

#[cfg(any(feature = "billing", feature = "arena"))]
mod room_open {
    //! 房主建房 + 开房费扣费：分成认 template.owner（创作者），自建自房归零，余额不足零副作用，免费房保留。
    use super::*;
    use sqlx::AnyPool;

    async fn seed_user_age(db: &AnyPool, id: &str, age: i64) {
        sqlx::query(
            "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) VALUES (?, '', ?, 'active', ?, ?)",
        )
        .bind(id)
        .bind(age)
        .bind(now_ms())
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
    }

    /// 造模板：owner=Some → 创作者模板（official=0）；None → 官方（official=1, owner NULL）。设开房费 + 可选分成率。
    /// moderation='approved'、withdrawn=0（可建房）。
    async fn seed_template(db: &AnyPool, id: &str, owner: Option<&str>, room_open_price: i64, bps: Option<i64>) {
        let official = if owner.is_some() { 0 } else { 1 };
        sqlx::query(
            "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, owner_id, revenue_share_bps, room_open_price_cents, withdrawn, created_at) \
             VALUES (?, '模板房', 'idle', '{}', '{\"mode\":\"open\"}', ?, 1, 'approved', ?, ?, ?, 0, ?)",
        )
        .bind(id)
        .bind(official)
        .bind(owner)
        .bind(bps)
        .bind(room_open_price)
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
    }

    /// 充值钱包（镜像 billing 双写），保证起点 user_wallet == billing_balances。
    async fn fund_wallet(db: &AnyPool, uid: &str, amount: i64) {
        let mut tx = db.begin().await.unwrap();
        crate::ledger::post_journal(
            &mut tx,
            "recharge",
            "order",
            "seed",
            None,
            &[
                crate::ledger::Posting {
                    account: crate::ledger::AccountRef::UserWallet(uid.to_string()),
                    delta_cents: amount,
                },
                crate::ledger::Posting {
                    account: crate::ledger::AccountRef::PlatformRechargeSource,
                    delta_cents: -amount,
                },
            ],
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO billing_balances (user_id, balance_cents, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT(user_id) DO UPDATE SET balance_cents = billing_balances.balance_cents + excluded.balance_cents, updated_at = excluded.updated_at",
        )
        .bind(uid)
        .bind(amount)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn acct_balance(db: &AnyPool, account_id: &str) -> i64 {
        let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM ledger_accounts WHERE id = ?")
            .bind(account_id)
            .fetch_optional(db)
            .await
            .unwrap();
        row.map(|(b,)| b).unwrap_or(0)
    }

    async fn billing_balance(db: &AnyPool, uid: &str) -> i64 {
        let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM billing_balances WHERE user_id = ?")
            .bind(uid)
            .fetch_optional(db)
            .await
            .unwrap();
        row.map(|(b,)| b).unwrap_or(0)
    }

    async fn count_sql(db: &AnyPool, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql).fetch_one(db).await.unwrap()
    }

    /// 红线不变量：每 journal SUM(postings)==0。返回不平衡 journal 数（应为 0）。
    async fn unbalanced_journals(db: &AnyPool) -> i64 {
        count_sql(
            db,
            "SELECT COUNT(*) FROM (SELECT journal_id FROM ledger_postings GROUP BY journal_id HAVING SUM(delta_cents) <> 0) t",
        )
        .await
    }

    #[tokio::test]
    async fn create_room_charges_open_fee_and_shares_to_owner() {
        // 房主用创作者模板建房：开房费 1000，默认分成 70% → 创作者 700 + 平台 300；世界落库归属房主。
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state.db, "creator", 1).await;
        seed_user_age(&state.db, "host", 1).await;
        seed_template(&state.db, "tpl", Some("creator"), 1000, None).await;
        fund_wallet(&state.db, "host", 2000).await;

        let th = token(&state, "host");
        let (s, v) = post_json(
            &app,
            "/api/worlds",
            &th,
            Some("k-room"),
            json!({ "templateId": "tpl", "title": "我的房", "visibility": "public" }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        let wid = v["worldId"].as_str().unwrap().to_string();
        assert_eq!(v["roomOpenPriceCents"], 1000);
        assert_eq!(v["charge"]["chargedCents"], 1000);
        assert_eq!(v["charge"]["creatorEarningsCents"], 700);
        assert_eq!(v["charge"]["platformRevenueCents"], 300);

        // 世界落库：归属房主、指向模板、可见性 public、room_type 取自模板。
        let w = load_world(&state.db, &wid).await.unwrap();
        assert_eq!(w.host_user_id.as_deref(), Some("host"));
        assert_eq!(w.template_id, "tpl");
        assert_eq!(w.visibility, "public");
        assert_eq!(w.room_type, "idle");
        assert_eq!(count_sql(&state.db, &format!("SELECT COUNT(*) FROM world_budgets WHERE world_id='{wid}'")).await, 1);

        // 扣费 + 分成：host 2000 − 1000 = 1000；creator 700；平台 300。分成认 template.owner（非房主）。
        assert_eq!(billing_balance(&state.db, "host").await, 1000);
        assert_eq!(acct_balance(&state.db, "acct_wallet_host").await, 1000);
        assert_eq!(acct_balance(&state.db, "acct_creator_creator").await, 700);
        assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 300);
        assert_eq!(unbalanced_journals(&state.db).await, 0);
        // journal reason=room_open，ref_id/world_id 均为新世界（审计溯源）。
        assert_eq!(
            count_sql(
                &state.db,
                &format!("SELECT COUNT(*) FROM ledger_journals WHERE reason='room_open' AND ref_id='{wid}' AND world_id='{wid}'")
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn create_room_self_owned_template_no_share() {
        // 自建自房防刷：host == 模板 owner → 分成归零，全额入平台。
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state.db, "host", 1).await;
        seed_template(&state.db, "tpl_self", Some("host"), 1000, None).await;
        fund_wallet(&state.db, "host", 2000).await;

        let th = token(&state, "host");
        let (s, v) = post_json(&app, "/api/worlds", &th, None, json!({ "templateId": "tpl_self" })).await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        assert_eq!(v["charge"]["creatorEarningsCents"], 0, "自建自房分成必须归零");
        assert_eq!(v["charge"]["platformRevenueCents"], 1000);
        assert_eq!(acct_balance(&state.db, "acct_creator_host").await, 0, "自建自房不得给自己产分成");
        assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 1000);
        assert_eq!(billing_balance(&state.db, "host").await, 1000);
        assert_eq!(unbalanced_journals(&state.db).await, 0);
    }

    #[tokio::test]
    async fn create_room_free_when_price_zero() {
        // 免费房：开房费 0 → charge no-op（不产 journal），保留免费开房能力；默认可见性 private。
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state.db, "host", 1).await;
        seed_template(&state.db, "tpl_free", None, 0, None).await; // 官方模板 owner NULL，开房费 0

        let th = token(&state, "host");
        let (s, v) = post_json(&app, "/api/worlds", &th, None, json!({ "templateId": "tpl_free" })).await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        let wid = v["worldId"].as_str().unwrap().to_string();
        assert_eq!(v["charge"]["chargedCents"], 0);
        assert_eq!(v["visibility"], "private", "未传可见性默认 private");
        assert_eq!(count_sql(&state.db, "SELECT COUNT(*) FROM ledger_journals WHERE reason='room_open'").await, 0, "免费开房不产 journal");
        assert_eq!(billing_balance(&state.db, "host").await, 0);
        assert!(load_world(&state.db, &wid).await.is_ok(), "免费房仍建成");
    }

    #[tokio::test]
    async fn create_room_insufficient_balance_rejected_zero_side_effects() {
        // 余额不足拒付 → 409，零副作用（无 world/budget/journal，钱包不动）。
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state.db, "creator", 1).await;
        seed_user_age(&state.db, "host", 1).await;
        seed_template(&state.db, "tpl", Some("creator"), 1000, None).await;
        fund_wallet(&state.db, "host", 500).await; // < 1000

        let th = token(&state, "host");
        let (s, _v) = post_json(&app, "/api/worlds", &th, None, json!({ "templateId": "tpl" })).await;
        assert_eq!(s, StatusCode::CONFLICT, "余额不足应 409");

        assert_eq!(count_sql(&state.db, "SELECT COUNT(*) FROM worlds WHERE host_user_id='host'").await, 0, "余额不足不得建房");
        assert_eq!(count_sql(&state.db, "SELECT COUNT(*) FROM ledger_journals WHERE reason='room_open'").await, 0, "余额不足不得产 journal");
        assert_eq!(billing_balance(&state.db, "host").await, 500, "钱包不动");
    }

    #[tokio::test]
    async fn create_room_rejects_missing_or_unavailable_template() {
        // 模板不存在 → 404；未审核/已撤回 → 409（均在扣费前，零账务副作用）。
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state.db, "host", 1).await;
        let th = token(&state, "host");

        let (s, _) = post_json(&app, "/api/worlds", &th, None, json!({ "templateId": "ghost" })).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        sqlx::query(
            "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, owner_id, room_open_price_cents, withdrawn, created_at) \
             VALUES ('tpl_pending','t','idle','{}','{\"mode\":\"open\"}',0,1,'pending','host',0,0,?)",
        )
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
        let (s2, _) = post_json(&app, "/api/worlds", &th, None, json!({ "templateId": "tpl_pending" })).await;
        assert_eq!(s2, StatusCode::CONFLICT, "未审核模板不得建房");

        sqlx::query(
            "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, owner_id, room_open_price_cents, withdrawn, created_at) \
             VALUES ('tpl_wd','t','idle','{}','{\"mode\":\"open\"}',0,1,'approved','host',0,1,?)",
        )
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
        let (s3, _) = post_json(&app, "/api/worlds", &th, None, json!({ "templateId": "tpl_wd" })).await;
        assert_eq!(s3, StatusCode::CONFLICT, "已撤回模板不得建房");
    }

    #[tokio::test]
    async fn create_room_idempotent_no_double_charge() {
        // 幂等：同 Idempotency-Key 重投 → 缓存返回，不双扣、不重复建房。
        let state = test_state().await;
        let app = build_router(state.clone());
        seed_user_age(&state.db, "creator", 1).await;
        seed_user_age(&state.db, "host", 1).await;
        seed_template(&state.db, "tpl", Some("creator"), 1000, None).await;
        fund_wallet(&state.db, "host", 2000).await;
        let th = token(&state, "host");

        let (s1, v1) = post_json(&app, "/api/worlds", &th, Some("k-1"), json!({ "templateId": "tpl", "visibility": "public" })).await;
        assert_eq!(s1, StatusCode::OK, "body={v1}");
        let (s2, v2) = post_json(&app, "/api/worlds", &th, Some("k-1"), json!({ "templateId": "tpl", "visibility": "public" })).await;
        assert_eq!(s2, StatusCode::OK, "body={v2}");
        assert_eq!(v1["worldId"], v2["worldId"], "同 key 重投返回同一世界");
        assert_eq!(billing_balance(&state.db, "host").await, 1000, "幂等重投不得双扣");
        assert_eq!(count_sql(&state.db, "SELECT COUNT(*) FROM ledger_journals WHERE reason='room_open'").await, 1, "幂等重投只产一笔 journal");
        assert_eq!(count_sql(&state.db, "SELECT COUNT(*) FROM worlds WHERE host_user_id='host'").await, 1, "幂等重投只建一个世界");
    }
}
