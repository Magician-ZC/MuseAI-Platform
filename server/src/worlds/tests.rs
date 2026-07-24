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
