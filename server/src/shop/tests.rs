//! P3 平台售卖集成测试（sqlite::memory + oneshot HTTP，feature=billing/arena）。
//! 覆盖：cloud_growth 扣费（全额平台不分成）+ 配额累加 + 余额不足零副作用；
//!       付费购道具走 grant_item_tx（单一写入路径）+ 幂等不双发货 + 余额不足零副作用；
//!       GET /me/earnings（余额+流水 + owner 隔离 + 明示不可提现）；无提现端点 404。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::AnyPool;
use tower::ServiceExt;

use crate::app::{build_router, AppState};
use crate::db::now_ms;
use crate::ledger::{post_journal, AccountRef, Posting};
use crate::safety::testkit::{seed_user, test_state, token};

// ---------- 脚手架 ----------

/// 充值钱包（镜像 billing 双写）：post_journal(user_wallet+amount / platform_recharge_source−amount) + billing_balances 物化。
/// 单连接池：两笔写必须同一 tx（不可再借连接，否则死锁）。保证起点 user_wallet == billing_balances 恒等。
async fn fund_wallet(db: &AnyPool, uid: &str, amount: i64) {
    let mut tx = db.begin().await.unwrap();
    post_journal(
        &mut tx,
        "recharge",
        "order",
        "seed",
        None,
        &[
            Posting { account: AccountRef::UserWallet(uid.to_string()), delta_cents: amount },
            Posting { account: AccountRef::PlatformRechargeSource, delta_cents: -amount },
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

/// 造创作者模板（official=0, owner=creator, 默认分成率）。
async fn seed_template(db: &AnyPool, id: &str, owner: &str) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, owner_id, created_at) \
         VALUES (?, 't', 'idle', '{}', '{\"mode\":\"open\"}', 0, 1, 'approved', ?, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 造世界实例（指向模板）。
async fn seed_world(db: &AnyPool, world_id: &str, template_id: &str) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, member_limit, tick_per_day, \
         state_revision, narrative_state_json, created_at, updated_at) \
         VALUES (?, ?, 1, 'e1', 'p1', 'm1', 'idle', 'w', 'open', 'private', 10, 3, 0, '{}', ?, ?)",
    )
    .bind(world_id)
    .bind(template_id)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
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

async fn scalar(db: &AnyPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(db).await.unwrap()
}

/// 红线不变量：每 journal SUM(postings)==0 且每账户 balance==SUM(postings)。
async fn assert_ledger_invariants(db: &AnyPool) {
    let unbalanced = scalar(
        db,
        "SELECT COUNT(*) FROM (SELECT journal_id FROM ledger_postings GROUP BY journal_id HAVING SUM(delta_cents) <> 0) t",
    )
    .await;
    assert_eq!(unbalanced, 0, "存在 SUM(postings)!=0 的 journal（账本红线被破坏）");
    let mismatched = scalar(
        db,
        "SELECT COUNT(*) FROM ledger_accounts a \
         WHERE a.balance_cents <> (SELECT COALESCE(SUM(p.delta_cents), 0) FROM ledger_postings p WHERE p.account_id = a.id)",
    )
    .await;
    assert_eq!(mismatched, 0, "存在 balance_cents != SUM(postings) 的账户");
}

async fn post_json(state: &AppState, uri: &str, bearer: Option<&str>, idem: Option<&str>, body: Value) -> (StatusCode, Value) {
    let app = build_router(state.clone());
    let mut builder = Request::builder().method("POST").uri(uri).header("content-type", "application/json");
    if let Some(tk) = bearer {
        builder = builder.header("authorization", format!("Bearer {tk}"));
    }
    if let Some(k) = idem {
        builder = builder.header("idempotency-key", k);
    }
    let resp = app.oneshot(builder.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let s = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

async fn get_json(state: &AppState, uri: &str, bearer: Option<&str>) -> (StatusCode, Value) {
    let app = build_router(state.clone());
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(tk) = bearer {
        builder = builder.header("authorization", format!("Bearer {tk}"));
    }
    let resp = app.oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
    let s = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

// ---------- 云成长（cloud_growth） ----------

/// cloud_growth 扣费：全额入平台（不分成）+ 落 user_entitlements 配额；钱包/余额扣减且恒等。
#[tokio::test]
async fn cloud_growth_charges_platform_and_grants_entitlement() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 3000).await;

    let (s, v) = post_json(&state, "/api/me/cloud-growth", Some(&token(&state, "u1")), None, json!({ "sku": "cloud_slot_1" })).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["entitlementKind"], "cloud_character_slot");
    assert_eq!(v["grantedQuantity"].as_i64().unwrap(), 1);
    assert_eq!(v["totalQuantity"].as_i64().unwrap(), 1);
    assert_eq!(v["chargedCents"].as_i64().unwrap(), 1000);
    assert_eq!(v["boundary"]["notPower"], true, "诚实边界：买配额不买战力");

    // 全额入平台，无创作者分成对手方（云成长不分成）。
    assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 1000);
    assert_eq!(billing_balance(&state.db, "u1").await, 2000, "钱包 3000 − 1000");
    assert_eq!(acct_balance(&state.db, "acct_wallet_u1").await, 2000, "user_wallet == billing_balances");
    // 配额落库。
    assert_eq!(
        scalar(&state.db, "SELECT quantity FROM user_entitlements WHERE user_id='u1' AND kind='cloud_character_slot'").await,
        1
    );
    assert_ledger_invariants(&state.db).await;
}

/// 多次购买同一 kind → 配额累加（(user_id, kind) 唯一行 upsert 累加）。
#[tokio::test]
async fn cloud_growth_accumulates_quantity() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 5000).await;
    let tk = token(&state, "u1");

    for _ in 0..3 {
        let (s, _) = post_json(&state, "/api/me/cloud-growth", Some(&tk), None, json!({ "sku": "backpack_cap_10" })).await;
        assert_eq!(s, StatusCode::OK);
    }
    // 每份 +10，3 次 → 30；每次扣 500 → 平台 1500。
    assert_eq!(
        scalar(&state.db, "SELECT quantity FROM user_entitlements WHERE user_id='u1' AND kind='backpack_capacity'").await,
        30
    );
    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM user_entitlements WHERE user_id='u1'").await, 1, "同 kind 单行累加");
    assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 1500);
    assert_eq!(billing_balance(&state.db, "u1").await, 3500);
    assert_ledger_invariants(&state.db).await;
}

/// 余额不足 → 409 insufficient_balance，零副作用（无配额行 / 无 journal / 余额不动）。
#[tokio::test]
async fn cloud_growth_insufficient_balance_zero_side_effects() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 500).await; // < 1000
    let before_journals = scalar(&state.db, "SELECT COUNT(*) FROM ledger_journals").await;

    let (s, v) = post_json(&state, "/api/me/cloud-growth", Some(&token(&state, "u1")), None, json!({ "sku": "cloud_slot_1" })).await;
    assert_eq!(s, StatusCode::CONFLICT, "余额不足应 409");
    assert_eq!(v["error"]["code"], "conflict");
    assert!(v["error"]["message"].as_str().unwrap().contains("insufficient_balance"));

    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM user_entitlements").await, 0, "无配额落库");
    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM ledger_journals").await, before_journals, "无新 journal");
    assert_eq!(billing_balance(&state.db, "u1").await, 500, "余额不动");
    assert_ledger_invariants(&state.db).await;
}

/// 幂等：同 Idempotency-Key 重投 → 缓存返回，不双扣、配额不双加。
#[tokio::test]
async fn cloud_growth_idempotent_no_double_charge() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 3000).await;
    let tk = token(&state, "u1");

    let (s1, _) = post_json(&state, "/api/me/cloud-growth", Some(&tk), Some("idem-1"), json!({ "sku": "cloud_slot_1" })).await;
    let (s2, _) = post_json(&state, "/api/me/cloud-growth", Some(&tk), Some("idem-1"), json!({ "sku": "cloud_slot_1" })).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        scalar(&state.db, "SELECT quantity FROM user_entitlements WHERE user_id='u1' AND kind='cloud_character_slot'").await,
        1,
        "同 key 重投配额不双加"
    );
    assert_eq!(billing_balance(&state.db, "u1").await, 2000, "只扣一次");
    assert_ledger_invariants(&state.db).await;
}

/// 未知/停用 SKU → 404，零副作用。
#[tokio::test]
async fn cloud_growth_unknown_sku_404() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 3000).await;
    let (s, _) = post_json(&state, "/api/me/cloud-growth", Some(&token(&state, "u1")), None, json!({ "sku": "nope" })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM user_entitlements").await, 0);
}

// ---------- 平台道具售卖（item_purchase 复用 grant_item_tx） ----------

/// 付费购道具：走 grant_item_tx 单一写入路径 → items 定义 + backpacks 归属行；全额入平台；钱包扣减且恒等。
#[tokio::test]
async fn item_purchase_charges_platform_and_grants_via_grant_item_tx() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 3000).await;

    let (s, v) = post_json(&state, "/api/shop/items/cosmetic_lantern/purchase", Some(&token(&state, "u1")), None, json!({})).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["itemId"], "item_sku_cosmetic_lantern");
    assert_eq!(v["chargedCents"].as_i64().unwrap(), 500);
    assert!(v["backpackId"].is_string(), "首次购买应发货，返回背包行 id");
    assert_eq!(v["boundary"]["notTradable"], true, "诚实边界：不可玩家间交易");

    // grant_item_tx 单一写入路径：items 定义 + backpacks 归属行各一。
    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM items WHERE id='item_sku_cosmetic_lantern'").await, 1);
    assert_eq!(
        scalar(&state.db, "SELECT COUNT(*) FROM backpacks WHERE user_id='u1' AND item_id='item_sku_cosmetic_lantern' AND status='owned'").await,
        1
    );
    // 平台单向售卖：全额入平台，无创作者对手方。
    assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 500);
    assert_eq!(billing_balance(&state.db, "u1").await, 2500);
    assert_eq!(acct_balance(&state.db, "acct_wallet_u1").await, 2500, "user_wallet == billing_balances");
    assert_ledger_invariants(&state.db).await;

    // 道具进入本人背包（GET /me/backpack 可见）。
    let (bs, bv) = get_json(&state, "/api/me/backpack", Some(&token(&state, "u1"))).await;
    assert_eq!(bs, StatusCode::OK);
    let items = bv["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["item"]["id"], "item_sku_cosmetic_lantern");
}

/// 余额不足 → 409，零副作用（无 items / 无 backpacks / 无 journal / 余额不动）。
#[tokio::test]
async fn item_purchase_insufficient_balance_zero_side_effects() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 100).await; // < 500
    let before_journals = scalar(&state.db, "SELECT COUNT(*) FROM ledger_journals").await;

    let (s, _) = post_json(&state, "/api/shop/items/cosmetic_lantern/purchase", Some(&token(&state, "u1")), None, json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);

    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM items WHERE id='item_sku_cosmetic_lantern'").await, 0, "无 item 定义");
    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM backpacks WHERE user_id='u1'").await, 0, "无发货");
    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM ledger_journals").await, before_journals, "无新 journal");
    assert_eq!(billing_balance(&state.db, "u1").await, 100, "余额不动");
    assert_ledger_invariants(&state.db).await;
}

/// 幂等：同 Idempotency-Key 重投 → 缓存返回，不双扣、不双发货（背包只一行）。
#[tokio::test]
async fn item_purchase_idempotent_no_double_grant() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 3000).await;
    let tk = token(&state, "u1");

    let (s1, _) = post_json(&state, "/api/shop/items/cosmetic_lantern/purchase", Some(&tk), Some("buy-1"), json!({})).await;
    let (s2, _) = post_json(&state, "/api/shop/items/cosmetic_lantern/purchase", Some(&tk), Some("buy-1"), json!({})).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        scalar(&state.db, "SELECT COUNT(*) FROM backpacks WHERE user_id='u1' AND item_id='item_sku_cosmetic_lantern'").await,
        1,
        "同 key 重投不双发货"
    );
    assert_eq!(billing_balance(&state.db, "u1").await, 2500, "只扣一次 500");
    assert_ledger_invariants(&state.db).await;
}

/// 未知/停用道具 SKU → 404，零副作用。
#[tokio::test]
async fn item_purchase_unknown_sku_404() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    fund_wallet(&state.db, "u1", 3000).await;
    let (s, _) = post_json(&state, "/api/shop/items/nope/purchase", Some(&token(&state, "u1")), None, json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(scalar(&state.db, "SELECT COUNT(*) FROM backpacks").await, 0);
}

// ---------- GET /me/earnings（创作者收益查询） ----------

/// 创作者收益：打赏分成入账后，owner 查得余额 + 流水；明示不可提现；他人查不到（owner 隔离）。
#[tokio::test]
async fn earnings_returns_balance_and_flow_isolated() {
    let state = test_state().await;
    seed_user(&state.db, "creator").await;
    seed_user(&state.db, "payer").await;
    seed_template(&state.db, "tpl", "creator").await; // 默认分成 70%
    seed_world(&state.db, "w1", "tpl").await;
    fund_wallet(&state.db, "payer", 2000).await;

    // 打赏 1000 → 创作者 700 + 平台 300（真实走 ledger::charge）。
    {
        let mut tx = state.db.begin().await.unwrap();
        crate::ledger::charge(&mut tx, "payer", 1000, "gift", "gift_event", "g1", Some("w1")).await.unwrap();
        tx.commit().await.unwrap();
    }

    // owner 查得 700 + 流水，withdrawable=false。
    let (s, v) = get_json(&state, "/api/me/earnings", Some(&token(&state, "creator"))).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["balanceCents"].as_i64().unwrap(), 700);
    assert_eq!(v["withdrawable"], false, "红线：站内可消费权益，不可提现");
    assert!(v["note"].as_str().unwrap().contains("不可提现"));
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "一条分成流水");
    assert_eq!(entries[0]["deltaCents"].as_i64().unwrap(), 700);
    assert_eq!(entries[0]["reason"], "gift");
    assert_eq!(entries[0]["worldId"], "w1", "溯源分成来源世界");

    // owner 隔离：payer（付费方）查自己的 earnings → 0，空流水。
    let (s2, v2) = get_json(&state, "/api/me/earnings", Some(&token(&state, "payer"))).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(v2["balanceCents"].as_i64().unwrap(), 0, "他人查不到创作者收益");
    assert!(v2["entries"].as_array().unwrap().is_empty());
}

/// 无 creator 账户的用户 → 余额 0、空流水、withdrawable=false（不报错）。
#[tokio::test]
async fn earnings_zero_when_no_account() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    let (s, v) = get_json(&state, "/api/me/earnings", Some(&token(&state, "u1"))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["balanceCents"].as_i64().unwrap(), 0);
    assert_eq!(v["withdrawable"], false);
    assert!(v["entries"].as_array().unwrap().is_empty());
}

/// 认证守卫：缺凭证 → 401。
#[tokio::test]
async fn earnings_requires_auth() {
    let state = test_state().await;
    let (s, _) = get_json(&state, "/api/me/earnings", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// ---------- 红线：无提现出口 ----------

/// 提现/转账/兑付端点一律不存在（404）——creator_earnings 站内可消费，本期绝不可提现。
#[tokio::test]
async fn no_withdraw_or_payout_endpoints() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    let tk = token(&state, "u1");
    for uri in [
        "/api/me/earnings/withdraw",
        "/api/me/earnings/payout",
        "/api/creator/withdraw",
        "/api/shop/withdraw",
        "/api/me/entitlements/withdraw",
    ] {
        let (s, _) = post_json(&state, uri, Some(&tk), None, json!({ "amountCents": 100 })).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "提现出口必须不存在：{uri}");
    }
}
