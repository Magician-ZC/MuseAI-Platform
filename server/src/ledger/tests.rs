//! 复式账本单元测试（sqlite::memory + 直接对 tx 调 ledger API，feature=billing/arena）。
//! 覆盖资金红线：SUM(postings)==0 硬约束；账户余额 == SUM(postings)；user_wallet == billing_balances 恒等；
//! 余额不足拒付零副作用；分成拆分 + 取整余数归平台；自打赏归零；未成年 owner 分成挂平台；免费 no-op。

use sqlx::any::AnyPoolOptions;
use sqlx::AnyPool;

use super::{charge, post_journal, AccountRef, Posting};
use crate::db::now_ms;
use crate::error::ApiError;

static INIT: std::sync::Once = std::sync::Once::new();

// ---------- 脚手架 ----------

async fn test_pool() -> AnyPool {
    INIT.call_once(sqlx::any::install_default_drivers);
    let pool = AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

/// 造用户（age_declared：0 未声明 / 1 成年 / 2 未成年）。
async fn seed_user(db: &AnyPool, id: &str, age_declared: i64) {
    sqlx::query("INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) VALUES (?, '', ?, 'active', ?, ?)")
        .bind(id)
        .bind(age_declared)
        .bind(now_ms())
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
}

/// 造世界模板：owner=Some → 创作者模板（official=0）；owner=None → 官方模板（official=1，owner NULL）。
/// bps=None → revenue_share_bps NULL（走全局默认 7000）。
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

/// 充值钱包（镜像 billing 双写）：post_journal(user_wallet+amount / platform_recharge_source−amount) + billing_balances 物化。
/// 保证测试起点 user_wallet == billing_balances 恒等。
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
    // 单连接池：billing_balances 必须在同一 tx 内写（不可再向池借连接，否则死锁 PoolTimedOut）。
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

// ---------- DB 断言辅助 ----------

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

async fn journal_count(db: &AnyPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ledger_journals").fetch_one(db).await.unwrap()
}

/// 可疑交易留痕计数（P4）：按 risk_events.kind 统计（minor_creator_hold / self_tip / large_charge）。
async fn risk_count(db: &AnyPool, kind: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM risk_events WHERE kind = ?")
        .bind(kind)
        .fetch_one(db)
        .await
        .unwrap()
}

/// 红线不变量①：每个 journal SUM(postings)==0（有借必有贷）。返回不平衡 journal 数（应为 0）。
async fn unbalanced_journals(db: &AnyPool) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM (SELECT journal_id FROM ledger_postings GROUP BY journal_id HAVING SUM(delta_cents) <> 0) t",
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// 红线不变量②：每个账户 balance_cents == SUM(其 postings)。返回不符账户数（应为 0）。
async fn accounts_mismatching_postings(db: &AnyPool) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM ledger_accounts a \
         WHERE a.balance_cents <> (SELECT COALESCE(SUM(p.delta_cents), 0) FROM ledger_postings p WHERE p.account_id = a.id)",
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// 全局硬约束断言：账本处处平衡 + 物化余额一致。
async fn assert_ledger_invariants(db: &AnyPool) {
    assert_eq!(unbalanced_journals(db).await, 0, "存在 SUM(postings)!=0 的 journal（账本红线被破坏）");
    assert_eq!(accounts_mismatching_postings(db).await, 0, "存在 balance_cents != SUM(postings) 的账户");
}

fn wallet(uid: &str) -> String {
    format!("acct_wallet_{uid}")
}

// ---------- 测试：post_journal 硬约束 ----------

/// SUM==0 的均衡凭证正常入账；账户物化余额 == postings 之和。
#[tokio::test]
async fn post_journal_balanced_updates_account_balances() {
    let db = test_pool().await;
    let mut tx = db.begin().await.unwrap();
    let jid = post_journal(
        &mut tx,
        "recharge",
        "order",
        "o1",
        None,
        &[
            Posting { account: AccountRef::UserWallet("u".into()), delta_cents: 500 },
            Posting { account: AccountRef::PlatformRechargeSource, delta_cents: -500 },
        ],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert!(!jid.is_empty());
    assert_eq!(acct_balance(&db, &wallet("u")).await, 500);
    assert_eq!(acct_balance(&db, "acct_platform_recharge_source").await, -500);
    assert_ledger_invariants(&db).await;
}

/// 红线：SUM(postings)!=0 → 内部错误（500），绝不外泄；事务回滚，零行落库。
#[tokio::test]
async fn post_journal_rejects_unbalanced() {
    let db = test_pool().await;
    let mut tx = db.begin().await.unwrap();
    let err = post_journal(
        &mut tx,
        "bad",
        "x",
        "x",
        None,
        &[
            Posting { account: AccountRef::UserWallet("u".into()), delta_cents: 100 },
            Posting { account: AccountRef::PlatformRevenue, delta_cents: -99 }, // 差 1 分，不平
        ],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ApiError::Internal(_)), "不平账本必须是内部错误，不外泄");
    drop(tx); // 回滚

    assert_eq!(journal_count(&db).await, 0, "不平凭证不得落任何行");
    assert_eq!(acct_balance(&db, &wallet("u")).await, 0);
}

/// 红线：单条分录（无对手方）拒绝——有借必有贷需 ≥2 条。
#[tokio::test]
async fn post_journal_rejects_too_few_postings() {
    let db = test_pool().await;
    let mut tx = db.begin().await.unwrap();
    let err = post_journal(
        &mut tx,
        "bad",
        "x",
        "x",
        None,
        &[Posting { account: AccountRef::UserWallet("u".into()), delta_cents: 0 }],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ApiError::Internal(_)));
    drop(tx);
    assert_eq!(journal_count(&db).await, 0);
}

// ---------- 测试：charge 扣费口 ----------

/// 余额不足拒付 → 409 insufficient_balance，且零副作用（无新 journal / 钱包/余额不动）。
#[tokio::test]
async fn charge_insufficient_balance_rejects_with_zero_side_effects() {
    let db = test_pool().await;
    seed_user(&db, "p", 1).await;
    fund_wallet(&db, "p", 500).await; // 只有 500
    let before_journals = journal_count(&db).await; // 1（充值）

    let mut tx = db.begin().await.unwrap();
    let err = charge(&mut tx, "p", 1000, "gift", "gift_event", "g1", None).await.unwrap_err();
    tx.commit().await.unwrap(); // 提交也不该有副作用（charge 在写入前返回）

    match err {
        ApiError::Conflict(m) => assert_eq!(m, "insufficient_balance"),
        other => panic!("期望 Conflict(insufficient_balance)，得到 {other:?}"),
    }
    assert_eq!(journal_count(&db).await, before_journals, "余额不足不得产 journal");
    assert_eq!(billing_balance(&db, "p").await, 500, "余额不动");
    assert_eq!(acct_balance(&db, &wallet("p")).await, 500, "钱包不动");
    assert_ledger_invariants(&db).await;
}

/// 分成拆分：创作者模板（默认 70%）打赏 1000 → 创作者 700 + 平台 300；钱包/余额 −1000 且恒等。
#[tokio::test]
async fn charge_splits_creator_and_platform() {
    let db = test_pool().await;
    seed_user(&db, "creator", 1).await;
    seed_user(&db, "payer", 1).await;
    seed_template(&db, "tpl", Some("creator"), None).await; // 默认 7000 bps
    seed_world(&db, "w1", "tpl").await;
    fund_wallet(&db, "payer", 2000).await;

    let mut tx = db.begin().await.unwrap();
    let r = charge(&mut tx, "payer", 1000, "gift", "gift_event", "g1", Some("w1")).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(r.creator_earnings_cents, 700);
    assert_eq!(r.platform_revenue_cents, 300);
    assert_eq!(r.charged_cents, 1000);
    assert!(r.journal_id.is_some());
    assert_eq!(acct_balance(&db, "acct_creator_creator").await, 700);
    assert_eq!(acct_balance(&db, "acct_platform_revenue").await, 300);
    assert_eq!(acct_balance(&db, &wallet("payer")).await, 1000); // 2000 − 1000
    assert_eq!(billing_balance(&db, "payer").await, 1000, "user_wallet == billing_balances 恒等");
    // 正常分成不留痕（避免噪声淹没真信号）。
    assert_eq!(risk_count(&db, "self_tip").await, 0, "正常分成不记 self_tip");
    assert_eq!(risk_count(&db, "minor_creator_hold").await, 0, "正常分成不记 minor_creator_hold");
    assert_eq!(risk_count(&db, "large_charge").await, 0, "小额不记 large_charge");
    assert_ledger_invariants(&db).await;
}

/// 取整余数归平台：price=101, bps=7000 → 创作者 floor(70.7)=70，平台 31（余数 0.7 分入平台，不凭空产分）。
#[tokio::test]
async fn charge_rounding_remainder_to_platform() {
    let db = test_pool().await;
    seed_user(&db, "creator", 1).await;
    seed_user(&db, "payer", 1).await;
    seed_template(&db, "tpl", Some("creator"), Some(7000)).await;
    seed_world(&db, "w1", "tpl").await;
    fund_wallet(&db, "payer", 500).await;

    let mut tx = db.begin().await.unwrap();
    let r = charge(&mut tx, "payer", 101, "gift", "gift_event", "g1", Some("w1")).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(r.creator_earnings_cents, 70);
    assert_eq!(r.platform_revenue_cents, 31);
    assert_eq!(r.creator_earnings_cents + r.platform_revenue_cents, 101, "分账守恒，无丢分/造分");
    assert_eq!(acct_balance(&db, "acct_creator_creator").await, 70);
    assert_eq!(acct_balance(&db, "acct_platform_revenue").await, 31);
    assert_ledger_invariants(&db).await;
}

/// 自打赏防刷：owner 给自己世界打赏 → 分成归零，全额入平台，creator 账户不产生。
#[tokio::test]
async fn charge_self_tip_zero_share() {
    let db = test_pool().await;
    seed_user(&db, "creator", 1).await;
    seed_template(&db, "tpl", Some("creator"), None).await;
    seed_world(&db, "w1", "tpl").await;
    fund_wallet(&db, "creator", 500).await;

    let mut tx = db.begin().await.unwrap();
    let r = charge(&mut tx, "creator", 100, "gift", "gift_event", "g1", Some("w1")).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(r.creator_earnings_cents, 0, "自打赏分成必须归零");
    assert_eq!(r.platform_revenue_cents, 100);
    assert_eq!(acct_balance(&db, "acct_creator_creator").await, 0, "自打赏不得给自己产分成");
    assert_eq!(acct_balance(&db, "acct_platform_revenue").await, 100);
    // 可疑交易留痕（P4）：自打赏防刷记一条 self_tip（套利刷分成监测），但不改扣费结果。
    assert_eq!(risk_count(&db, "self_tip").await, 1, "自打赏必须留痕 self_tip");
    assert_ledger_invariants(&db).await;
}

/// 未成年不得当创作者收款方：owner age_declared==2 → 分成挂平台（不注入未经充值的余额）。未声明(0) 同理。
#[tokio::test]
async fn charge_minor_creator_hangs_to_platform() {
    let db = test_pool().await;
    seed_user(&db, "minor", 2).await; // 未成年
    seed_user(&db, "undeclared", 0).await; // 未声明
    seed_user(&db, "payer", 1).await;
    seed_template(&db, "tpl_minor", Some("minor"), None).await;
    seed_template(&db, "tpl_undecl", Some("undeclared"), None).await;
    seed_world(&db, "w_minor", "tpl_minor").await;
    seed_world(&db, "w_undecl", "tpl_undecl").await;
    fund_wallet(&db, "payer", 1000).await;

    for (world, owner_acct) in [("w_minor", "acct_creator_minor"), ("w_undecl", "acct_creator_undeclared")] {
        let mut tx = db.begin().await.unwrap();
        let r = charge(&mut tx, "payer", 100, "gift", "gift_event", "g", Some(world)).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(r.creator_earnings_cents, 0, "未成年/未声明 owner 分成必须挂平台");
        assert_eq!(r.platform_revenue_cents, 100);
        assert_eq!(acct_balance(&db, owner_acct).await, 0, "未成年 creator 账户不得入账");
    }
    assert_eq!(acct_balance(&db, "acct_platform_revenue").await, 200);
    // 可疑交易留痕（P4）：两笔未成年/未声明 owner 分成挂账各记一条 minor_creator_hold（合规待成年补实名核查）。
    // 留痕只进 risk_events，**绝不**给未成年 creator 账户注入余额（红线：未成年不得当创作者收款方）。
    assert_eq!(risk_count(&db, "minor_creator_hold").await, 2, "两笔未成年 owner 分成挂账均留痕");
    assert_ledger_invariants(&db).await;
}

/// 可疑大额单笔消费留痕（P4）：单笔 charge ≥ 阈值 → 记 large_charge（AML 监测，仅留痕不拦截，扣费照常）。
#[tokio::test]
async fn charge_large_amount_records_suspicious_trace() {
    let db = test_pool().await;
    seed_user(&db, "payer", 1).await;
    fund_wallet(&db, "payer", 600_000).await;

    let mut tx = db.begin().await.unwrap();
    // 500_000 == SUSPICIOUS_CHARGE_THRESHOLD_CENTS（阈值含等号）；平台服务 world_id=None 全额入平台。
    let r = charge(&mut tx, "payer", 500_000, "revive", "revive_grant", "rg", None).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(r.charged_cents, 500_000, "留痕不改扣费结果");
    assert_eq!(billing_balance(&db, "payer").await, 100_000, "大额照常扣费（留痕≠拦截）");
    assert_eq!(risk_count(&db, "large_charge").await, 1, "超阈值大额单笔必须留痕");
    assert_ledger_invariants(&db).await;
}

/// 无世界（平台服务：复活/云成长）→ 全额入平台，无创作者对手方。
#[tokio::test]
async fn charge_no_world_all_platform() {
    let db = test_pool().await;
    seed_user(&db, "payer", 1).await;
    fund_wallet(&db, "payer", 500).await;

    let mut tx = db.begin().await.unwrap();
    let r = charge(&mut tx, "payer", 100, "revive", "revive_grant", "rg1", None).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(r.creator_earnings_cents, 0);
    assert_eq!(r.platform_revenue_cents, 100);
    assert_eq!(acct_balance(&db, "acct_platform_revenue").await, 100);
    assert_eq!(billing_balance(&db, "payer").await, 400);
    assert_ledger_invariants(&db).await;
}

/// 官方模板（owner NULL）→ 无分成对手方，全额入平台。
#[tokio::test]
async fn charge_official_template_all_platform() {
    let db = test_pool().await;
    seed_user(&db, "payer", 1).await;
    seed_template(&db, "official_tpl", None, None).await; // owner NULL
    seed_world(&db, "w1", "official_tpl").await;
    fund_wallet(&db, "payer", 500).await;

    let mut tx = db.begin().await.unwrap();
    let r = charge(&mut tx, "payer", 100, "room_open", "world", "w1", Some("w1")).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(r.creator_earnings_cents, 0);
    assert_eq!(r.platform_revenue_cents, 100);
    assert_ledger_invariants(&db).await;
}

/// 免费（price==0）→ no-op：不产 journal、钱包/余额不动（保留免费开房能力）。
#[tokio::test]
async fn charge_free_is_noop() {
    let db = test_pool().await;
    seed_user(&db, "payer", 1).await;
    seed_template(&db, "tpl", Some("creator"), None).await;
    seed_world(&db, "w1", "tpl").await;
    fund_wallet(&db, "payer", 500).await;
    let before = journal_count(&db).await;

    let mut tx = db.begin().await.unwrap();
    let r = charge(&mut tx, "payer", 0, "room_open", "world", "w1", Some("w1")).await.unwrap();
    tx.commit().await.unwrap();

    assert!(r.journal_id.is_none(), "免费不产 journal");
    assert_eq!(r.charged_cents, 0);
    assert_eq!(journal_count(&db).await, before, "免费不新增 journal");
    assert_eq!(billing_balance(&db, "payer").await, 500, "免费不扣余额");
    assert_ledger_invariants(&db).await;
}

/// 恒等守护：多笔 charge 后 user_wallet == billing_balances 始终成立，且全局账本不变量守恒。
#[tokio::test]
async fn charge_preserves_wallet_billing_identity() {
    let db = test_pool().await;
    seed_user(&db, "creator", 1).await;
    seed_user(&db, "payer", 1).await;
    seed_template(&db, "tpl", Some("creator"), None).await;
    seed_world(&db, "w1", "tpl").await;
    fund_wallet(&db, "payer", 5000).await;

    for (price, world) in [(1000i64, Some("w1")), (777, Some("w1")), (300, None)] {
        let mut tx = db.begin().await.unwrap();
        charge(&mut tx, "payer", price, "gift", "gift_event", "g", world).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            acct_balance(&db, &wallet("payer")).await,
            billing_balance(&db, "payer").await,
            "每笔 charge 后 user_wallet 必须与 billing_balances 恒等"
        );
    }
    // 5000 − 1000 − 777 − 300 = 2923
    assert_eq!(billing_balance(&db, "payer").await, 2923);
    assert_eq!(acct_balance(&db, &wallet("payer")).await, 2923);
    assert_ledger_invariants(&db).await;
}
