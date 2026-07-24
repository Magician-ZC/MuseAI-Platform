//! finance 对账后台（P4 合规增强）：GET /admin/ledger/reconcile。
//!
//! 只读全账对账（finance/admin，与 economy_overview 一致的 role gate）。**无副作用、不触发任何结算/提现**
//! （红线：无提现出口——本端点只核验账本，不动一分钱）。三类硬约束核验：
//! - **全账复式恒等**：所有 postings 之和恒为 0（每 journal SUM=0 ⇒ 全局=0）。任何非 0 即账本被破坏。
//! - **每 journal 平衡**：SUM(该 journal postings)==0（有借必有贷），列出不平 journal 供 triage。
//! - **账户物化余额对账**：每账户 `balance_cents` 恒等于其 postings 之和。
//! - **user_wallet == billing_balances**：迁移期双写不变量，正反双查。
//!
//! 账本表（ledger_accounts/journals/postings，0013）与 billing_balances（0006）恒存在（迁移不随 feature 门控），
//! 本端点只跑只读 SQL、不调 `ledger::` 函数，故与 economy_overview 一样无条件编译（default 构建亦可运行）。
//! SUM 一律 CAST(... AS BIGINT)：跨 SQLite/Postgres 可移植（PG 下 SUM(bigint) 返回 numeric）。

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::db::now_ms;
use crate::error::ApiError;

use super::require_role;

/// 单标量（列名一律别名 n）。
async fn scalar(db: &AnyPool, sql: &str) -> Result<i64, ApiError> {
    Ok(sqlx::query(sql).fetch_one(db).await?.try_get::<i64, _>("n")?)
}

/// 收集不合规 id（列名别名 id；SQL 内联 LIMIT，供 triage 取样，非分页）。
async fn sample_ids(db: &AnyPool, sql: &str) -> Result<Vec<String>, ApiError> {
    let rows = sqlx::query(sql).fetch_all(db).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(r.try_get::<String, _>("id")?);
    }
    Ok(out)
}

/// GET /admin/ledger/reconcile：finance/admin 全账对账（只读）。
pub(super) async fn ledger_reconcile(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["finance"])?;
    let db = &state.db;

    // ① 全账复式恒等：所有 postings 之和必为 0（每 journal SUM=0 ⇒ 全局=0）。任何非 0 = 账本红线被破坏。
    let global_posting_sum =
        scalar(db, "SELECT CAST(COALESCE(SUM(delta_cents),0) AS BIGINT) AS n FROM ledger_postings").await?;

    // ② 每 journal SUM(postings)==0：不平 journal 计数 + 样例 id（有借必有贷）。
    let journals_total = scalar(db, "SELECT COUNT(*) AS n FROM ledger_journals").await?;
    let unbalanced_journals = scalar(
        db,
        "SELECT COUNT(*) AS n FROM \
         (SELECT journal_id FROM ledger_postings GROUP BY journal_id HAVING SUM(delta_cents) <> 0) t",
    )
    .await?;
    let unbalanced_journal_ids = sample_ids(
        db,
        "SELECT journal_id AS id FROM ledger_postings GROUP BY journal_id HAVING SUM(delta_cents) <> 0 LIMIT 50",
    )
    .await?;

    // ③ 账户物化余额对账：balance_cents == SUM(该账户 postings)。
    let accounts_total = scalar(db, "SELECT COUNT(*) AS n FROM ledger_accounts").await?;
    let mismatched_accounts = scalar(
        db,
        "SELECT COUNT(*) AS n FROM ledger_accounts a \
         WHERE a.balance_cents <> (SELECT COALESCE(SUM(p.delta_cents),0) FROM ledger_postings p WHERE p.account_id = a.id)",
    )
    .await?;
    let mismatched_account_ids = sample_ids(
        db,
        "SELECT id FROM ledger_accounts a \
         WHERE a.balance_cents <> (SELECT COALESCE(SUM(p.delta_cents),0) FROM ledger_postings p WHERE p.account_id = a.id) \
         LIMIT 50",
    )
    .await?;

    // ④ user_wallet == billing_balances 恒等（迁移期双写不变量），正反双查：
    //    正向——每个 user_wallet 账户余额须等于其 billing_balances（无行视作 0）；
    //    反向——每条 billing_balances 须等于对应 user_wallet 账户余额（无账户视作 0）。
    let wallet_vs_billing = scalar(
        db,
        "SELECT COUNT(*) AS n FROM ledger_accounts a WHERE a.kind = 'user_wallet' \
         AND a.balance_cents <> COALESCE((SELECT b.balance_cents FROM billing_balances b WHERE b.user_id = a.owner_id), 0)",
    )
    .await?;
    let billing_vs_wallet = scalar(
        db,
        "SELECT COUNT(*) AS n FROM billing_balances b \
         WHERE b.balance_cents <> COALESCE((SELECT a.balance_cents FROM ledger_accounts a WHERE a.kind = 'user_wallet' AND a.owner_id = b.user_id), 0)",
    )
    .await?;
    let wallet_billing_mismatch = wallet_vs_billing + billing_vs_wallet;

    // 账户分类汇总（供 finance 概览：各科目账户数 + 余额合计）。
    let kind_rows = sqlx::query(
        "SELECT kind, COUNT(*) AS c, CAST(COALESCE(SUM(balance_cents),0) AS BIGINT) AS total \
         FROM ledger_accounts GROUP BY kind ORDER BY kind",
    )
    .fetch_all(db)
    .await?;
    let mut accounts_by_kind = Vec::with_capacity(kind_rows.len());
    for r in &kind_rows {
        accounts_by_kind.push(json!({
            "kind": r.try_get::<String, _>("kind")?,
            "count": r.try_get::<i64, _>("c")?,
            "balanceCents": r.try_get::<i64, _>("total")?,
        }));
    }

    // 总判定：四项硬约束全过才算账本平衡。
    let balanced = global_posting_sum == 0
        && unbalanced_journals == 0
        && mismatched_accounts == 0
        && wallet_billing_mismatch == 0;

    Ok(Json(json!({
        "balanced": balanced,
        "checkedAt": now_ms(),
        // 全账复式恒等：必为 0。
        "globalPostingSumCents": global_posting_sum,
        "journals": {
            "total": journals_total,
            "unbalanced": unbalanced_journals,
            "unbalancedIds": unbalanced_journal_ids,
        },
        "accounts": {
            "total": accounts_total,
            "mismatched": mismatched_accounts,
            "mismatchedIds": mismatched_account_ids,
        },
        "walletBillingIdentity": {
            "mismatched": wallet_billing_mismatch,
            "walletVsBilling": wallet_vs_billing,
            "billingVsWallet": billing_vs_wallet,
        },
        "accountsByKind": accounts_by_kind,
        "notes": [
            "全账复式恒等：globalPostingSumCents 必为 0（每 journal SUM=0 ⇒ 全局=0）。",
            "账户物化余额对账：每账户 balance_cents 必等于其 postings 之和。",
            "user_wallet == billing_balances（迁移期双写不变量），正反双查。",
            "只读对账，无副作用；不触发任何结算/提现（红线：无提现出口）。"
        ],
    })))
}
