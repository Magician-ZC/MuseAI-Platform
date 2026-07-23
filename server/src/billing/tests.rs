//! P4b 计费集成测试（sqlite::memory + axum oneshot，feature=billing）。
//! 覆盖：充值→账本双录+余额；同 key 幂等不双记（+异载荷 409）；退款逆向+状态机（含幂等重退/非 fulfilled 拒退/越权/不存在）；
//! 未成年拒充；账本恒等式 SUM(ledger)==balance；无提现/转账端点（路由不存在）；余额端点无行视为 0 + 鉴权守卫。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::any::AnyPoolOptions;
use tower::ServiceExt;

use crate::app::{build_router, AppState};
use crate::config::ServerConfig;
use crate::db::{new_id, now_ms};

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
        object_store_dir: std::env::temp_dir().join(new_id("muse-p4b")).to_string_lossy().into_owned(),
    }
}

async fn test_state() -> AppState {
    INIT.call_once(sqlx::any::install_default_drivers);
    let pool = AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, test_config())
}

fn token(state: &AppState, user_id: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, user_id, "user", 3600).unwrap()
}

/// 造用户，可控声明年龄（0 未声明 / 1 成年 / 2 未成年）。
async fn seed_user(state: &AppState, id: &str, age_declared: i64) {
    sqlx::query("INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) VALUES (?, '', ?, 'active', ?, ?)")
        .bind(id)
        .bind(age_declared)
        .bind(now_ms())
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
}

async fn post(app: &axum::Router, uri: &str, token: &str, idem: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = idem {
        b = b.header("Idempotency-Key", k);
    }
    let req = b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
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

// ---------- DB 断言辅助 ----------

async fn count(db: &sqlx::AnyPool, sql: &str, uid: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).bind(uid).fetch_one(db).await.unwrap()
}

async fn orders_count(db: &sqlx::AnyPool, uid: &str) -> i64 {
    count(db, "SELECT COUNT(*) FROM orders WHERE user_id = ?", uid).await
}

async fn ledger_count(db: &sqlx::AnyPool, uid: &str) -> i64 {
    count(db, "SELECT COUNT(*) FROM ledger_entries WHERE user_id = ?", uid).await
}

/// 账本聚合：SUM(ledger.delta_cents)，供恒等式断言。
async fn ledger_sum(db: &sqlx::AnyPool, uid: &str) -> i64 {
    count(db, "SELECT COALESCE(SUM(delta_cents), 0) FROM ledger_entries WHERE user_id = ?", uid).await
}

/// 权威余额（直读表，无行视为 0）。
async fn balance_row(db: &sqlx::AnyPool, uid: &str) -> i64 {
    let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM billing_balances WHERE user_id = ?")
        .bind(uid)
        .fetch_optional(db)
        .await
        .unwrap();
    row.map(|(b,)| b).unwrap_or(0)
}

async fn order_status(db: &sqlx::AnyPool, order_id: &str) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM orders WHERE id = ?")
        .bind(order_id)
        .fetch_one(db)
        .await
        .unwrap();
    row.0
}

/// 不变量：每个用户 SUM(ledger)==balance 恒成立。
async fn assert_double_entry(db: &sqlx::AnyPool, uid: &str) {
    assert_eq!(ledger_sum(db, uid).await, balance_row(db, uid).await, "账本双录恒等式被破坏: SUM(ledger) != balance");
}

// ---------- 测试 ----------

/// 充值 → 单事务落 orders(fulfilled)+ledger(+amount)+balance；账本双录，余额入账。
#[tokio::test]
async fn recharge_writes_double_entry_and_balance() {
    let state = test_state().await;
    seed_user(&state, "u1", 1).await;
    let app = build_router(state.clone());
    let tk = token(&state, "u1");

    let (st, body) = post(&app, "/api/billing/orders", &tk, Some("k1"), json!({"kind":"recharge","amountCents":5000})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["balanceCents"], 5000);
    let order_id = body["orderId"].as_str().unwrap().to_string();

    // 余额入账 + 端点读数一致
    assert_eq!(balance_row(&state.db, "u1").await, 5000);
    let (_, bal) = get(&app, "/api/billing/balance", Some(&tk)).await;
    assert_eq!(bal["balanceCents"], 5000);

    // 账本双录：恰一条 +5000，且订单已履约（fulfilled）
    assert_eq!(ledger_count(&state.db, "u1").await, 1);
    assert_eq!(ledger_sum(&state.db, "u1").await, 5000);
    assert_eq!(order_status(&state.db, &order_id).await, "fulfilled");
    assert_double_entry(&state.db, "u1").await;
}

/// 同 Idempotency-Key 重复提交 → 不双扣不双记（1 order / 1 ledger / 余额只加一次）；异载荷同 key → 409。
#[tokio::test]
async fn idempotent_same_key_no_double_charge() {
    let state = test_state().await;
    seed_user(&state, "u2", 1).await;
    let app = build_router(state.clone());
    let tk = token(&state, "u2");

    let req = json!({"kind":"recharge","amountCents":3000});
    let (s1, b1) = post(&app, "/api/billing/orders", &tk, Some("dup"), req.clone()).await;
    let (s2, b2) = post(&app, "/api/billing/orders", &tk, Some("dup"), req.clone()).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    // 第二次返回缓存响应（同一 orderId），未再次记账
    assert_eq!(b1["orderId"], b2["orderId"]);
    assert_eq!(orders_count(&state.db, "u2").await, 1);
    assert_eq!(ledger_count(&state.db, "u2").await, 1);
    assert_eq!(balance_row(&state.db, "u2").await, 3000);
    assert_double_entry(&state.db, "u2").await;

    // 同 key 异载荷 → 幂等冲突 409（不覆盖不重记）
    let (s3, _) = post(&app, "/api/billing/orders", &tk, Some("dup"), json!({"kind":"recharge","amountCents":9999})).await;
    assert_eq!(s3, StatusCode::CONFLICT);
    assert_eq!(orders_count(&state.db, "u2").await, 1);
    assert_eq!(balance_row(&state.db, "u2").await, 3000);
}

/// 退款：单事务逆向 ledger(-amount)+扣余额+orders.status=refunded；重退幂等；非 fulfilled 拒退；越权/不存在。
#[tokio::test]
async fn refund_reverses_and_enforces_state_machine() {
    let state = test_state().await;
    seed_user(&state, "u3", 1).await;
    seed_user(&state, "other", 1).await;
    let app = build_router(state.clone());
    let tk = token(&state, "u3");

    // 充值 5000 → 退款
    let (_, ord) = post(&app, "/api/billing/orders", &tk, Some("r-o1"), json!({"kind":"recharge","amountCents":5000})).await;
    let oid = ord["orderId"].as_str().unwrap().to_string();

    let (st, rb) = post(&app, "/api/billing/refunds", &tk, Some("r-f1"), json!({"orderId": oid})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(rb["status"], "refunded");
    assert_eq!(rb["balanceCents"], 0);
    // 逆向账本：+5000 与 -5000 两行，和为 0，余额 0，订单 refunded
    assert_eq!(ledger_count(&state.db, "u3").await, 2);
    assert_eq!(balance_row(&state.db, "u3").await, 0);
    assert_eq!(order_status(&state.db, &oid).await, "refunded");
    assert_double_entry(&state.db, "u3").await;

    // 重复退款（新幂等键，靠状态机）→ 幂等返回 refunded，不再逆向（ledger 仍 2 行）
    let (st2, rb2) = post(&app, "/api/billing/refunds", &tk, Some("r-f2"), json!({"orderId": oid})).await;
    assert_eq!(st2, StatusCode::OK);
    assert_eq!(rb2["status"], "refunded");
    assert_eq!(ledger_count(&state.db, "u3").await, 2);
    assert_eq!(balance_row(&state.db, "u3").await, 0);

    // 非 fulfilled 订单拒退：直插一条 paid 订单 → 409
    let paid_oid = new_id("order");
    sqlx::query("INSERT INTO orders (id, user_id, kind, amount_cents, status, created_at, updated_at) VALUES (?, 'u3', 'recharge', 3000, 'paid', ?, ?)")
        .bind(&paid_oid).bind(now_ms()).bind(now_ms()).execute(&state.db).await.unwrap();
    let (st3, _) = post(&app, "/api/billing/refunds", &tk, Some("r-f3"), json!({"orderId": paid_oid})).await;
    assert_eq!(st3, StatusCode::CONFLICT);

    // 越权退他人订单 → 403（先给 other 造一张 fulfilled 单）
    let other_oid = new_id("order");
    sqlx::query("INSERT INTO orders (id, user_id, kind, amount_cents, status, created_at, updated_at) VALUES (?, 'other', 'recharge', 1000, 'fulfilled', ?, ?)")
        .bind(&other_oid).bind(now_ms()).bind(now_ms()).execute(&state.db).await.unwrap();
    let (st4, _) = post(&app, "/api/billing/refunds", &tk, Some("r-f4"), json!({"orderId": other_oid})).await;
    assert_eq!(st4, StatusCode::FORBIDDEN);

    // 不存在订单 → 404
    let (st5, _) = post(&app, "/api/billing/refunds", &tk, Some("r-f5"), json!({"orderId":"order_does_not_exist"})).await;
    assert_eq!(st5, StatusCode::NOT_FOUND);
}

/// 红线：未成年（age_declared==2）拒充 → 403，且不产生任何账务副作用。
#[tokio::test]
async fn minor_recharge_forbidden() {
    let state = test_state().await;
    seed_user(&state, "kid", 2).await;
    let app = build_router(state.clone());
    let tk = token(&state, "kid");

    let (st, _) = post(&app, "/api/billing/orders", &tk, Some("m1"), json!({"kind":"recharge","amountCents":5000})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // 无订单 / 无账本 / 无余额行
    assert_eq!(orders_count(&state.db, "kid").await, 0);
    assert_eq!(ledger_count(&state.db, "kid").await, 0);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM billing_balances WHERE user_id = ?", "kid").await, 0);
}

/// 账本恒等式：多次充值 + 退款混合后，SUM(ledger.delta_cents) == balance_cents 恒成立。
#[tokio::test]
async fn ledger_sum_equals_balance_invariant() {
    let state = test_state().await;
    seed_user(&state, "u4", 1).await;
    let app = build_router(state.clone());
    let tk = token(&state, "u4");

    let (_, o1) = post(&app, "/api/billing/orders", &tk, Some("i-1"), json!({"kind":"recharge","amountCents":1000})).await;
    let oid1 = o1["orderId"].as_str().unwrap().to_string();
    post(&app, "/api/billing/orders", &tk, Some("i-2"), json!({"kind":"recharge","amountCents":2500})).await;
    // 退掉第一笔
    post(&app, "/api/billing/refunds", &tk, Some("i-3"), json!({"orderId": oid1})).await;

    // 1000 + 2500 - 1000 = 2500
    assert_eq!(balance_row(&state.db, "u4").await, 2500);
    assert_eq!(ledger_sum(&state.db, "u4").await, 2500);
    assert_eq!(ledger_count(&state.db, "u4").await, 3);
    assert_double_entry(&state.db, "u4").await;
}

/// 余额端点：无行视为 0；未鉴权 → 401（AuthUser 守卫）。
#[tokio::test]
async fn balance_zero_when_no_row_and_requires_auth() {
    let state = test_state().await;
    seed_user(&state, "u5", 1).await;
    let app = build_router(state.clone());
    let tk = token(&state, "u5");

    let (st, body) = get(&app, "/api/billing/balance", Some(&tk)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["balanceCents"], 0);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM billing_balances WHERE user_id = ?", "u5").await, 0);

    // 无 token → 401
    let (st_noauth, _) = get(&app, "/api/billing/balance", None).await;
    assert_eq!(st_noauth, StatusCode::UNAUTHORIZED);
}

/// 红线：绝不存在提现 / 转账端点——相关路由一律不注册 → 404。
#[tokio::test]
async fn no_withdraw_or_transfer_endpoints() {
    let state = test_state().await;
    seed_user(&state, "u6", 1).await;
    let app = build_router(state.clone());
    let tk = token(&state, "u6");

    for uri in ["/api/billing/withdraw", "/api/billing/transfer", "/api/billing/cashout", "/api/billing/withdrawals"] {
        let (ps, _) = post(&app, uri, &tk, None, json!({"amountCents":100})).await;
        assert_eq!(ps, StatusCode::NOT_FOUND, "POST {uri} 不应存在（余额不可提现/转账红线）");
        let (gs, _) = get(&app, uri, Some(&tk)).await;
        assert_eq!(gs, StatusCode::NOT_FOUND, "GET {uri} 不应存在（余额不可提现/转账红线）");
    }
}
