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
        "INSERT INTO billing_balances (user_id, balance_cents, updated_at) VALUES ($1, $2, $3) \
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
         VALUES ($1, 't', 'idle', '{}', '{\"mode\":\"open\"}', 0, 1, 'approved', $2, $3)",
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
         VALUES ($1, $2, 1, 'e1', 'p1', 'm1', 'idle', 'w', 'open', 'private', 10, 3, 0, '{}', $3, $4)",
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
    let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM ledger_accounts WHERE id = $1")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .unwrap();
    row.map(|(b,)| b).unwrap_or(0)
}

async fn billing_balance(db: &AnyPool, uid: &str) -> i64 {
    let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM billing_balances WHERE user_id = $1")
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

/// 🔴 **`withdrawable` 永不被置为非 0**（§0.5 无提现，红线⑤）。
///
/// `ledger::ensure_account` 的注释里写着「红线：`withdrawable` 恒 0」，而在此之前
/// **守着这条的只有那句注释**——对比 `memorial` 的 `withdrawn` 单向门，那个有源码级断言。
/// 一旦某处把它写成 1，`GET /me/earnings` 会立刻如实回 `withdrawable: true`
/// （`shop` 是**读库**而不是硬编码 false），于是红线在读取面上当场破掉，且没有任何用例会红。
///
/// 判据：生产码里凡出现 `withdrawable` 的 SQL，其取值只能是字面量 `0`；
/// 不接受绑定参数（`$N`）——绑定值意味着「取决于运行时」，那正是这条红线不允许的。
#[test]
fn red_line_withdrawable_is_never_written_nonzero() {
    let mut checked = 0usize;
    for (path, src) in crate::testkit::production_sources() {
        for lit in src.split('"').skip(1).step_by(2) {
            let sql: String = lit.split('\\').collect::<Vec<_>>().join(" ");
            let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            // 必须同时含表名：否则会误伤 JSON 键名 `"withdrawable"`（`shop` 的响应体里就有一个），
            // 那不是 SQL，判它等于让这条红线在第一次运行时就报假警。
            if !(sql.contains("withdrawable") && sql.contains("ledger_accounts")) {
                continue;
            }
            // 只读的 SELECT 不在管辖内。
            if sql.starts_with("SELECT") {
                continue;
            }
            checked += 1;
            assert!(
                sql.contains("VALUES") && sql.contains(", 0, 0,"),
                "🔴 {path} 的这条 SQL 触碰了 `withdrawable`，而它必须恒为字面量 0：\n  {sql}\n\
                 §0.5「无提现」：`GET /me/earnings` 是**读库**回这个标志的，写成 1 就等于\n\
                 在读取面上开了提现出口。若真要开（待牌照），那是产品与合规决定，需单独评审。"
            );
        }
    }
    assert_eq!(
        checked, 1,
        "🔴 触碰 `withdrawable` 的写语句应当**恰好一条**（`ledger::ensure_account` 的建户 INSERT）。\n\
         变多 = 多了一条能改这个标志的路径；变少 = 建户语句没了或扫描器坏了。"
    );
}

/// 🔴 **路由里不存在提现语义的路径**（§0.5）。
///
/// 原先那条用例试的是**五个猜出来的 URI**（`/me/earnings/withdraw` 等）——
/// 换个名字（`/me/wallet/cashout`、`/creator/payout-request`）就照样全绿。
/// 这一条改为扫**路由注册本身**：任何 `.route("…")` 的路径里都不得出现提现语义的词。
///
/// 保留原来那条按 URI 试 404 的用例：两者一个查「路由表里有没有」、
/// 一个查「打过去到底通不通」，都要。
#[test]
fn red_line_no_route_path_carries_withdrawal_semantics() {
    const BANNED: &[&str] = &["withdraw", "payout", "cashout", "cash-out", "remit", "encash"];

    /// ⚠️ **`withdraw` 在本仓是一词两义**，这里必须区分，否则这条红线要么误报要么形同虚设：
    /// - **资产义**：`cloud_characters.withdrawn` = 停止后续投放（下架一张已发布的卡 / 一个世界模板）。
    ///   与钱无关，`memorial` 封卷也复用它。
    /// - **金钱义**：`ledger_accounts.withdrawable` = 可提现。**这才是 §0.5 红线管的那个。**
    ///
    /// 下面按**完整路径**豁免资产义的那几条，不按词豁免——按词豁免等于把 `withdraw`
    /// 整个从黑名单里拿掉，那样 `/me/earnings/withdraw` 也会被放过。
    ///
    /// 🔵 这个歧义本身值得知道：有人为了核「无提现」去 grep `withdraw`，会先撞见一堆资产下架，
    /// 从而既可能误判成「有提现出口」，也可能因为「看起来都是资产的」而漏掉真的那个。
    const ASSET_SENSE_PATHS: &[&str] = &[
        "/assets/characters/{id}/withdraw",
        "/assets/worlds/{id}/withdraw",
    ];

    // 🔴 只看**真正的路由注册** `.route("…")`。
    //
    // 第一版按「以 `/` 开头的字面量」筛，当场误报了 `/assembly/payoutTable`——那是
    // `assembled_json` 里的 **JSON 指针**，`payout` 在那儿指「产出表」（战利品），不是付款。
    // 这是本条红线撞见的**第二个一词两义**（第一个是 `withdraw`），说明按词扫路径必须先
    // 确定「这个字面量真的是路由」，否则黑名单越全、误报越多，最后只会被加一堆豁免绕过去。
    let mut routes = 0usize;
    for (path, src) in crate::testkit::production_sources() {
        for seg in src.split(".route(\"").skip(1) {
            let Some(uri) = seg.split('"').next() else { continue };
            routes += 1;
            if ASSET_SENSE_PATHS.contains(&uri) {
                continue; // 资产下架，与钱无关（见上方注释）。
            }
            let low = uri.to_ascii_lowercase();
            for b in BANNED {
                assert!(
                    !low.contains(b),
                    "🔴 {path} 注册了一条带提现语义的路由 `{uri}`（命中 `{b}`）。\n\
                     §0.5「无提现」：creator_earnings 只作站内权益，提现默认永不开启（待牌照）。\n\
                     若确实要开，那是产品 + 合规决定，需单独评审——不要只把断言改绿。"
                );
            }
        }
    }
    assert!(
        routes > 80,
        "🔴 只扫到 {routes} 条路由注册，扫描口径疑似坏了（红线会静默失效）"
    );
}


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

// ---------- 分成比例的可见性（创作者有权知道自己被按什么条件付酬） ----------

/// 🔴 **创作者必须看得到自己被按什么比例结算**。
///
/// 此前 `/me/earnings` 只给余额与流水。而 `revenue_share_bps` 是**按模板可覆盖**的，
/// 于是创作者拿着自己模板挣的钱，却看不到自己的比例——他连「我这 700 分是不是算对了」
/// 都无从验证。告诉某人他正被按什么条件付酬不是新政策，**不说才是异常**。
#[tokio::test]
async fn a_creator_can_see_the_rate_they_are_being_paid_at() {
    let state = test_state().await;
    seed_user(&state.db, "creator").await;
    seed_template(&state.db, "tpl_default", "creator").await; // 未单独配 → 用平台默认
    // 另一个模板单独配 55%。
    seed_template(&state.db, "tpl_custom", "creator").await;
    sqlx::query("UPDATE world_templates SET revenue_share_bps = 5500 WHERE id = 'tpl_custom'")
        .execute(&state.db)
        .await
        .unwrap();

    let (s, v) = get_json(&state, "/api/me/earnings", Some(&token(&state, "creator"))).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    let rs = &v["revenueShare"];
    assert_eq!(
        rs["platformDefaultBps"].as_i64().unwrap(),
        crate::ledger::DEFAULT_REVENUE_SHARE_BPS,
        "平台默认必须与 ledger 同源: {rs}"
    );

    let tpls = rs["myTemplates"].as_array().expect("myTemplates");
    let by = |id: &str| tpls.iter().find(|t| t["templateId"] == id).unwrap_or_else(|| panic!("缺 {id}: {rs}"));

    // 🔴 「随平台默认浮动」与「给我单独定死」是两件不同的信息，必须分得开——
    // 合并成一个数，创作者就不知道自己的比例稳不稳。
    assert_eq!(by("tpl_default")["shareBps"], json!(crate::ledger::DEFAULT_REVENUE_SHARE_BPS));
    assert_eq!(by("tpl_default")["isPlatformDefault"], json!(true), "{rs}");
    assert_eq!(by("tpl_custom")["shareBps"], json!(5500), "{rs}");
    assert_eq!(by("tpl_custom")["isPlatformDefault"], json!(false), "🔴 单独配的不得被标成默认: {rs}");

    // ⚠️ 如实说明这是**当前**比例，不是历史流水各自结算时用的那个。
    assert!(
        rs["caveat"].as_str().unwrap_or("").contains("不是历史流水"),
        "比例改过之后旧流水仍按当时的比例结算过，这点必须说清: {rs}"
    );
}

/// 🔴 只列**本人拥有**的模板：他人的分成比例与他无关，露出来既无用又是信息泄露。
#[tokio::test]
async fn the_rate_list_never_shows_someone_elses_templates() {
    let state = test_state().await;
    seed_user(&state.db, "creator").await;
    seed_user(&state.db, "other").await;
    seed_template(&state.db, "tpl_mine", "creator").await;
    seed_template(&state.db, "tpl_theirs", "other").await;

    let (_s, v) = get_json(&state, "/api/me/earnings", Some(&token(&state, "creator"))).await;
    let raw = v["revenueShare"].to_string();
    assert!(raw.contains("tpl_mine"), "自己的要在: {raw}");
    assert!(
        !raw.contains("tpl_theirs") && !raw.contains("other"),
        "🔴 不得列出他人的模板或比例 —— 既无用又是信息泄露: {raw}"
    );
}
