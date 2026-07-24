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
