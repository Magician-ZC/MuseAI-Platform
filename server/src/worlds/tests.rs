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

    // 人数上限（member_limit=1，已满）。
    seed_char(&state, "chA2", "usrA", "approved", 0).await;
    let (st_full, _) =
        post_json(&app, &uri, &ta, None, json!({ "cloudCharacterId": "chA2" })).await;
    assert_eq!(st_full, StatusCode::CONFLICT, "满员应 409");

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
