//! livegate 集成测试（sqlite::memory + oneshot），#[cfg(all(test, feature="arena"))]。
//! 覆盖：礼物→arena_env_events(kind=gift_boon)、同 world+SKU 聚合合并、SKU 映射查表、未知 SKU、
//! 未知世界 404、签名校验（纯单元）、GET /arena/{worldId}/clips 列表。

use super::*;
use crate::safety::testkit::{count, seed_member, seed_user, seed_world, test_state, token};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sqlx::AnyPool;
use tower::ServiceExt;

/// test_state + 容错建 arena_env_events（P6a 的 0007 并发期可能未落地；IF NOT EXISTS 落地后自动无操作）。
async fn arena_state() -> AppState {
    let state = test_state().await;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS arena_env_events (\
         id TEXT PRIMARY KEY, world_id TEXT NOT NULL, applied_tick INTEGER, \
         kind TEXT NOT NULL, payload_json TEXT NOT NULL, \
         aggregated_count INTEGER NOT NULL DEFAULT 1, created_at BIGINT NOT NULL)",
    )
    .execute(&state.db)
    .await
    .expect("create arena_env_events");
    state
}

async fn post_webhook(state: &AppState, body: Value) -> (StatusCode, Value) {
    let app = crate::app::build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/livegate/webhook")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

#[tokio::test]
async fn webhook_gift_writes_env_event() {
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;

    let (status, v) =
        post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 1})).await;
    assert_eq!(status, StatusCode::OK, "body={v}");
    assert_eq!(v["mapped"], true);

    // 写入 arena_env_events 专用通道，kind='gift_boon'。
    let n = count(
        &state.db,
        "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(n, 1, "礼物应写一条 gift_boon 到 arena_env_events");
    // 未写玩家 interventions（礼物是系统代投，不走玩家干预通道）。
    let ivs = count(&state.db, "SELECT COUNT(*) FROM interventions").await;
    assert_eq!(ivs, 0, "礼物不得进玩家 interventions");
    // gift_events 记账一条。
    let g = count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1' AND mapped=1").await;
    assert_eq!(g, 1);
}

#[tokio::test]
async fn same_sku_aggregates_count() {
    // 同 world+SKU 聚合：两次礼物合并为一行 gift_boon，aggregated_count 累加计数。
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;

    post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 3})).await;
    let (_s, v) = post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 2})).await;

    // 只有一行（防事件风暴），聚合计数 = 3 + 2。
    let rows = count(
        &state.db,
        "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(rows, 1, "同 world+SKU 应聚合为一行");
    assert_eq!(v["aggregatedCount"], 5, "aggregated_count 应累加");
    let agg = count(
        &state.db,
        "SELECT aggregated_count FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(agg, 5);
    // gift_events 仍逐笔记账（两条），聚合只发生在环境事件通道。
    let g = count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1'").await;
    assert_eq!(g, 2, "gift_events 应逐笔记账");
}

#[tokio::test]
async fn different_sku_separate_rows() {
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;
    post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 1})).await;
    post_webhook(&state, json!({"worldId": "w1", "giftSku": "rocket", "count": 1})).await;
    let rows = count(
        &state.db,
        "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .await;
    assert_eq!(rows, 2, "不同 SKU 不应聚合");
}

#[tokio::test]
async fn sku_mapping_resolved() {
    // SKU→boon 映射查表：命中的 boon 进响应与 arena_env_events payload。
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;
    let (status, v) =
        post_webhook(&state, json!({"worldId": "w1", "giftSku": "rose", "count": 1})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["boon"]["effectTag"], "advantage:combat", "rose 应映射到过程增益 advantage:combat");
    assert_eq!(v["boon"]["kind"], "env_prop");

    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    let p: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(p["sku"], "rose");
    assert_eq!(p["boon"]["effectTag"], "advantage:combat");
}

#[tokio::test]
async fn unknown_sku_graceful() {
    // 未知 SKU：优雅处理——200 mapped=false，不写 arena_env_events，仍记 gift_events 账（对账用）。
    let state = arena_state().await;
    seed_world(&state.db, "w1", 0, "running").await;
    let (status, v) =
        post_webhook(&state, json!({"worldId": "w1", "giftSku": "nonesuch", "count": 1})).await;
    assert_eq!(status, StatusCode::OK, "未知 SKU 不应报错");
    assert_eq!(v["mapped"], false);
    assert_eq!(v["boon"], Value::Null);

    let env = count(&state.db, "SELECT COUNT(*) FROM arena_env_events").await;
    assert_eq!(env, 0, "未知 SKU 不写环境事件");
    let g = count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE mapped=0").await;
    assert_eq!(g, 1, "未知 SKU 仍逐笔记账");
}

#[tokio::test]
async fn unknown_world_not_found() {
    let state = arena_state().await;
    let (status, _v) =
        post_webhook(&state, json!({"worldId": "ghost", "giftSku": "rose", "count": 1})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn signature_matches_unit() {
    // 签名校验核心（生产开关关闭 dev 时启用）：正确签名通过，错误签名/篡改 body 拒绝。
    let secret = "s3cr3t";
    let body = br#"{"worldId":"w1","giftSku":"rose","count":1}"#;
    let good = sign_body(secret, body);
    assert!(signature_matches(secret, &good, body));
    assert!(!signature_matches(secret, "deadbeef", body));
    assert!(!signature_matches(secret, &good, b"tampered"), "篡改 body 应验签失败");
}

#[tokio::test]
async fn list_clips_route_returns_generated() {
    // GET /arena/{worldId}/clips：生成一条切片后列表可读（观战资格经 can_view_world）。
    let state = arena_state().await;
    seed_user(&state.db, "u1").await;
    seed_world(&state.db, "w1", 0, "running").await; // official → 可观战
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, public_projection_json, occurred_at) \
         VALUES ('e1','w1',0,1,'de1','action','[\"c1\"]','public','{\"summary\":\"高光\"}', 0)",
    )
    .execute(&state.db)
    .await
    .unwrap();
    crate::clips::generate_clip(&state, "w1", "e1").await.expect("generate_clip");

    let app = crate::app::build_router(state.clone());
    let tk = token(&state, "u1");
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/arena/w1/clips")
                .header("authorization", format!("Bearer {tk}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["clips"].as_array().unwrap().len(), 1, "列表应含一条切片");
    assert_eq!(v["clips"][0]["eventId"], "e1");
}

// ---------- POST /arena/{worldId}/gift：站内观众打赏（AuthUser + 观战资格 + via=in_app + 进流） ----------

/// 带鉴权（可选 Idempotency-Key）的站内打赏请求。
async fn post_gift(
    state: &AppState,
    world: &str,
    user: &str,
    body: Value,
    idem: Option<&str>,
) -> (StatusCode, Value) {
    let tk = token(state, user);
    let app = crate::app::build_router(state.clone());
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/arena/{world}/gift"))
        .header("authorization", format!("Bearer {tk}"))
        .header("content-type", "application/json");
    if let Some(k) = idem {
        builder = builder.header("idempotency-key", k);
    }
    let resp = app.oneshot(builder.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

#[tokio::test]
async fn spectator_gift_maps_to_env_and_stream() {
    let state = arena_state().await;
    seed_user(&state.db, "viewer").await;
    seed_world(&state.db, "w1", 0, "running").await; // official → 可观战

    let (s, v) = post_gift(&state, "w1", "viewer", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["mapped"], true);
    assert_eq!(v["boundary"]["notImmunity"], true);
    assert_eq!(v["boundary"]["notFinalVerdict"], true);

    // 与 webhook 同一系统频道：arena_env_events(gift_boon)。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'").await,
        1
    );
    // gift_events 记账且 via='in_app'（区分站内来源，供分成）。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1' AND via='in_app'").await,
        1
    );
    // 打赏进 public 流：arena_gift（audience NULL → 双硬隔离天然满足）。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w1' AND event_type='arena_gift' AND visibility='public' AND audience_json IS NULL").await,
        1
    );
    // 红线：不写玩家 interventions。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM interventions").await, 0);
}

#[tokio::test]
async fn spectator_gift_unmapped_sku_no_boon() {
    let state = arena_state().await;
    seed_user(&state.db, "viewer").await;
    seed_world(&state.db, "w1", 0, "running").await;

    let (s, v) = post_gift(&state, "w1", "viewer", json!({ "sku": "nonesuch", "count": 1 }), None).await;
    assert_eq!(s, StatusCode::OK, "未映射 SKU 不应报错");
    assert_eq!(v["mapped"], false);
    assert_eq!(v["boon"], Value::Null);

    // 未映射：不写 env、不进流；仍逐笔记 gift_events(via=in_app)（对账用，对齐 webhook 语义）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_env_events").await, 0, "未映射不写环境事件");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_events WHERE event_type='arena_gift'").await, 0, "未映射不进流");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE mapped=0 AND via='in_app'").await, 1);
}

#[tokio::test]
async fn spectator_gift_requires_view_permission() {
    let state = arena_state().await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "stranger").await;
    seed_world(&state.db, "w1", 0, "running").await;
    // 收敛为 private：仅成员/房主可观战 → 可打赏。
    sqlx::query("UPDATE worlds SET visibility='private' WHERE id='w1'").execute(&state.db).await.unwrap();
    seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;

    // 非成员 → 403（守卫挡在 apply_gift 之前，绝不记账）。
    let (s1, _) = post_gift(&state, "w1", "stranger", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s1, StatusCode::FORBIDDEN);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events").await, 0, "被拒绝的打赏不得记账");

    // 成员 → 200。
    let (s2, v2) = post_gift(&state, "w1", "u1", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s2, StatusCode::OK, "body={v2}");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE via='in_app'").await, 1);
}

#[tokio::test]
async fn spectator_gift_idempotent() {
    let state = arena_state().await;
    seed_user(&state.db, "viewer").await;
    seed_world(&state.db, "w1", 0, "running").await;

    // 同 Idempotency-Key 重投两次 → 第二次返回缓存，计数不翻倍。
    let (s1, v1) = post_gift(&state, "w1", "viewer", json!({ "sku": "rose", "count": 3 }), Some("k-1")).await;
    assert_eq!(s1, StatusCode::OK, "body={v1}");
    let (s2, v2) = post_gift(&state, "w1", "viewer", json!({ "sku": "rose", "count": 3 }), Some("k-1")).await;
    assert_eq!(s2, StatusCode::OK, "body={v2}");

    // gift_events 只一笔（幂等），聚合计数 = 3（非 6）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w1'").await, 1, "幂等重投不得重复记账");
    assert_eq!(
        count(&state.db, "SELECT aggregated_count FROM arena_env_events WHERE world_id='w1' AND kind='gift_boon'").await,
        3,
        "幂等重投聚合计数不得翻倍"
    );
    // 进流也只一条 arena_gift。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w1' AND event_type='arena_gift'").await, 1);
}

// ---------- P1 打赏扣费：站内 charge（钱包扣费 + 分成 + 平台抽成 + SUM=0）；外部 webhook 仅记账 ----------

/// 充值钱包（镜像 billing 双写）：recharge journal(user_wallet +amount / recharge_source −amount) + billing_balances 物化。
/// 保证测试起点 user_wallet == billing_balances 恒等。单连接内存库 → 同一 tx 内写两表（不可再借连接）。
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
    .bind(crate::db::now_ms())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// 造世界模板：owner=Some → 创作者模板（official=0）；bps=None → revenue_share_bps NULL（走全局默认 7000）。
async fn seed_template(db: &AnyPool, id: &str, owner: Option<&str>, bps: Option<i64>) {
    let official = if owner.is_some() { 0 } else { 1 };
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, owner_id, revenue_share_bps, created_at) \
         VALUES (?, 't', 'idle', '{}', '{\"mode\":\"open\"}', ?, 1, 'approved', ?, ?, ?)",
    )
    .bind(id)
    .bind(official)
    .bind(owner)
    .bind(bps)
    .bind(crate::db::now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 造世界实例（指向指定模板，visibility=official → 任何登录用户可观战/打赏）。
async fn seed_world_tpl(db: &AnyPool, world_id: &str, template_id: &str) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, member_limit, tick_per_day, \
         state_revision, narrative_state_json, created_at, updated_at) \
         VALUES (?, ?, 1, 'e1', 'p1', 'm1', 'idle', 'w', 'running', 'official', 10, 3, 0, '{}', ?, ?)",
    )
    .bind(world_id)
    .bind(template_id)
    .bind(crate::db::now_ms())
    .bind(crate::db::now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 给某 SKU 定价（分）；gift 总价 = price_cents × count。
async fn set_sku_price(db: &AnyPool, sku: &str, price_cents: i64) {
    sqlx::query("UPDATE gift_sku_map SET price_cents = ? WHERE sku = ?")
        .bind(price_cents)
        .bind(sku)
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

/// 红线不变量：每 journal SUM(postings)==0（有借必有贷）。返回不平衡 journal 数（应为 0）。
async fn unbalanced_journals(db: &AnyPool) -> i64 {
    count(
        db,
        "SELECT COUNT(*) FROM (SELECT journal_id FROM ledger_postings GROUP BY journal_id HAVING SUM(delta_cents) <> 0) t",
    )
    .await
}

#[tokio::test]
async fn spectator_gift_charges_wallet_creator_and_platform() {
    // 站内打赏走钱包扣费：创作者模板（默认 70%）rose 定价 1000，count=1 →
    // 钱包 −1000、创作者 +700、平台 +300；SUM(postings)==0；gift 副作用照旧。
    let state = arena_state().await;
    seed_user(&state.db, "creator").await;
    seed_user(&state.db, "payer").await;
    seed_template(&state.db, "tpl_c", Some("creator"), None).await; // 默认 7000 bps
    seed_world_tpl(&state.db, "wc", "tpl_c").await;
    set_sku_price(&state.db, "rose", 1000).await;
    fund_wallet(&state.db, "payer", 2000).await;

    let (s, v) = post_gift(&state, "wc", "payer", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["mapped"], true);

    // 钱包扣费：payer 2000 − 1000 = 1000；user_wallet == billing_balances 恒等。
    assert_eq!(billing_balance(&state.db, "payer").await, 1000);
    assert_eq!(acct_balance(&state.db, "acct_wallet_payer").await, 1000);
    // 分成入账：创作者 700 + 平台抽成 300。
    assert_eq!(acct_balance(&state.db, "acct_creator_creator").await, 700);
    assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 300);
    // 复式恒等（有借必有贷）。
    assert_eq!(unbalanced_journals(&state.db).await, 0, "每 journal SUM(postings) 必须为 0");
    // gift 副作用照旧：env 事件 + gift_events(via=in_app) + 进流 arena_gift；红线不写 interventions。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_env_events WHERE world_id='wc' AND kind='gift_boon'").await, 1);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='wc' AND via='in_app'").await, 1);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='wc' AND event_type='arena_gift'").await, 1);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM interventions").await, 0);
    // charge 的 journal ref_id 与 gift_events 主键一致（审计链）。
    assert_eq!(
        count(
            &state.db,
            "SELECT COUNT(*) FROM ledger_journals j JOIN gift_events g ON j.ref_id = g.id \
             WHERE j.reason='gift' AND g.world_id='wc'",
        )
        .await,
        1,
        "gift journal 应与 gift_events 通过 ref_id 对齐"
    );
}

#[tokio::test]
async fn spectator_gift_self_tip_zero_share() {
    // 自打赏防刷：owner 给自己世界打赏 → 分成归零，全额入平台，creator 账户不产生分成。
    let state = arena_state().await;
    seed_user(&state.db, "creator").await;
    seed_template(&state.db, "tpl_s", Some("creator"), None).await;
    seed_world_tpl(&state.db, "ws", "tpl_s").await;
    set_sku_price(&state.db, "rose", 500).await;
    fund_wallet(&state.db, "creator", 1000).await;

    let (s, v) = post_gift(&state, "ws", "creator", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s, StatusCode::OK, "body={v}");

    // 钱包仍扣（1000 − 500 = 500），但分成归零、全额入平台。
    assert_eq!(billing_balance(&state.db, "creator").await, 500);
    assert_eq!(acct_balance(&state.db, "acct_creator_creator").await, 0, "自打赏分成必须归零");
    assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 500);
    assert_eq!(unbalanced_journals(&state.db).await, 0);
}

#[tokio::test]
async fn spectator_gift_insufficient_balance_rejected() {
    // 余额不足拒付 → 409，且零副作用（无 gift_events / env / journal，钱包不动）。
    let state = arena_state().await;
    seed_user(&state.db, "poor").await;
    seed_world(&state.db, "w1", 0, "running").await; // official → 可观战
    set_sku_price(&state.db, "rose", 1000).await; // 定价 1000，但 poor 钱包为 0

    let (s, _v) = post_gift(&state, "w1", "poor", json!({ "sku": "rose", "count": 1 }), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "余额不足应 409");

    // 零副作用：charge 在余额校验处返回，tx 回滚。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events").await, 0, "余额不足不得记 gift_events");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_env_events").await, 0, "余额不足不得写环境事件");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM ledger_journals WHERE reason='gift'").await, 0, "余额不足不得产 gift journal");
    assert_eq!(billing_balance(&state.db, "poor").await, 0, "钱包不动");
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_events WHERE event_type='arena_gift'").await, 0);
}

#[tokio::test]
async fn webhook_external_records_no_charge() {
    // 外部 webhook（观众已在直播平台付费）：仅记账（gift_events via='livegate'），**站内不二次扣钱包**（红线）。
    // 即便 SKU 已定价，webhook 路径也绝不产生 charge/journal。
    let state = arena_state().await;
    seed_world(&state.db, "we", 0, "running").await;
    set_sku_price(&state.db, "rose", 1000).await;

    let (s, v) =
        post_webhook(&state, json!({ "worldId": "we", "giftSku": "rose", "count": 1, "fromUser": "ext123" })).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["mapped"], true);

    // 记账：gift_events via='livegate'。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='we' AND via='livegate'").await, 1);
    // 红线：外部路径不扣钱包、不产任何 journal（复式账本全空）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM ledger_journals").await, 0, "外部 webhook 不得产 journal（不站内扣费）");
    assert_eq!(billing_balance(&state.db, "ext123").await, 0, "外部观众钱包无扣费");
    // gift 副作用照旧（env 事件）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_env_events WHERE world_id='we' AND kind='gift_boon'").await, 1);
}
