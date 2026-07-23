//! 数据看板 + 经济运营。二者均为只读 SQL 聚合（COUNT/SUM），不产生副作用、不建结算。

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::error::ApiError;

use super::require_role;

/// 单标量计数辅助（列名一律别名 n）。
async fn count(db: &AnyPool, sql: &str) -> Result<i64, ApiError> {
    let row = sqlx::query(sql).fetch_one(db).await?;
    Ok(row.try_get::<i64, _>("n")?)
}

fn rate(numer: i64, denom: i64) -> f64 {
    if denom <= 0 {
        0.0
    } else {
        (numer as f64) / (denom as f64)
    }
}

/// GET /admin/metrics/overview：核心运营指标聚合。
/// 注册数、日报打开率、tick 成功率、按世界 token 成本、审核积压、活跃/熔断世界、风控计数。
pub(super) async fn metrics_overview(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator", "finance"])?;
    let db = &state.db;

    // 用户：注册总数 / 封禁数。
    let users_total = count(db, "SELECT COUNT(*) AS n FROM users").await?;
    let users_banned =
        count(db, "SELECT COUNT(*) AS n FROM users WHERE status = 'banned'").await?;

    // 日报送达/打开率。
    let dr = sqlx::query(
        "SELECT COUNT(*) AS total, \
         COALESCE(SUM(CASE WHEN opened_at IS NOT NULL THEN 1 ELSE 0 END), 0) AS opened \
         FROM daily_reports",
    )
    .fetch_one(db)
    .await?;
    let reports_total: i64 = dr.try_get("total")?;
    let reports_opened: i64 = dr.try_get("opened")?;

    // tick 成功率。
    let tk = sqlx::query(
        "SELECT COUNT(*) AS total, \
         COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) AS done, \
         COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS failed \
         FROM world_ticks",
    )
    .fetch_one(db)
    .await?;
    let ticks_total: i64 = tk.try_get("total")?;
    let ticks_done: i64 = tk.try_get("done")?;
    let ticks_failed: i64 = tk.try_get("failed")?;

    // 按世界 token 成本（Top 10）。
    let cost_rows = sqlx::query(
        "SELECT world_id, COALESCE(SUM(cost_tokens), 0) AS tokens FROM world_ticks \
         GROUP BY world_id ORDER BY tokens DESC LIMIT 10",
    )
    .fetch_all(db)
    .await?;
    let mut token_cost_by_world = Vec::new();
    for r in &cost_rows {
        token_cost_by_world.push(json!({
            "worldId": r.try_get::<String, _>("world_id")?,
            "tokens": r.try_get::<i64, _>("tokens")?,
        }));
    }

    // 审核积压 / 活跃世界 / 熔断世界 / 风控事件。
    let audit_backlog =
        count(db, "SELECT COUNT(*) AS n FROM audit_queue WHERE status = 'open'").await?;
    let worlds_active =
        count(db, "SELECT COUNT(*) AS n FROM worlds WHERE status IN ('open','running')").await?;
    let worlds_fused =
        count(db, "SELECT COUNT(*) AS n FROM world_budgets WHERE fused = 1").await?;
    let risk_total = count(db, "SELECT COUNT(*) AS n FROM risk_events").await?;
    let data_requests_pending = count(
        db,
        "SELECT COUNT(*) AS n FROM data_requests WHERE status IN ('pending','running')",
    )
    .await?;

    Ok(Json(json!({
        "users": { "total": users_total, "banned": users_banned },
        "dailyReports": {
            "total": reports_total,
            "opened": reports_opened,
            "openRate": rate(reports_opened, reports_total),
        },
        "ticks": {
            "total": ticks_total,
            "done": ticks_done,
            "failed": ticks_failed,
            "successRate": rate(ticks_done, ticks_total),
        },
        "tokenCostByWorld": token_cost_by_world,
        "auditBacklog": audit_backlog,
        "worlds": { "active": worlds_active, "fused": worlds_fused },
        "riskEvents": risk_total,
        "dataRequestsPending": data_requests_pending,
    })))
}

/// GET /admin/economy/overview：真实只读经济聚合（finance/admin）。
///
/// 数据源恒存在（0001 建 orders/ledger_entries/billing_balances，0008 建 gift_events），
/// 与 billing/arena feature 无关，default 构建即可读。仅 COUNT/SUM 只读聚合——
/// **不建结算、不碰创作者分成**（§2.6：创作者结算是签约主体的另一套账，不在此）。
/// 资金以账本双录（ledger_entries）为权威：recharge 正流水、refund 负流水，
/// 恒有 `充值总额 - 退款总额 == 当前余额合计`（billing 模块单事务保证）。
/// SUM 一律 CAST(... AS BIGINT)：跨 SQLite/Postgres 可移植（PG 下 SUM(bigint) 返回 numeric）。
pub(super) async fn economy_overview(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["finance"])?;
    let db = &state.db;

    // 账本双录（资金权威）：充值正流水 / 退款负流水。
    let recharge_total =
        count(db, "SELECT CAST(COALESCE(SUM(delta_cents),0) AS BIGINT) AS n FROM ledger_entries WHERE reason = 'recharge'").await?;
    let recharge_count =
        count(db, "SELECT COUNT(*) AS n FROM ledger_entries WHERE reason = 'recharge'").await?;
    // 退款账本为负值，取相反数作为出账正数展示。
    let refund_neg =
        count(db, "SELECT CAST(COALESCE(SUM(delta_cents),0) AS BIGINT) AS n FROM ledger_entries WHERE reason = 'refund'").await?;
    let refund_count =
        count(db, "SELECT COUNT(*) AS n FROM ledger_entries WHERE reason = 'refund'").await?;
    let refund_total = -refund_neg;

    // 当前用户钱包余额合计（非创作者结算）。
    let balance_total =
        count(db, "SELECT CAST(COALESCE(SUM(balance_cents),0) AS BIGINT) AS n FROM billing_balances").await?;
    let wallet_count = count(db, "SELECT COUNT(*) AS n FROM billing_balances").await?;

    // 订单按状态计数（created/paid/fulfilled/refunded/failed）。
    let order_rows =
        sqlx::query("SELECT status, COUNT(*) AS n FROM orders GROUP BY status").fetch_all(db).await?;
    let mut by_status = serde_json::Map::new();
    for s in ["created", "paid", "fulfilled", "refunded", "failed"] {
        by_status.insert(s.to_string(), json!(0));
    }
    let mut orders_total: i64 = 0;
    for r in &order_rows {
        let st = r.try_get::<String, _>("status")?;
        let n = r.try_get::<i64, _>("n")?;
        orders_total += n;
        by_status.insert(st, json!(n));
    }

    // 礼物流水（gift_events，0008 恒存在）：条数 / 礼物总量 / 覆盖世界数。
    let gift_events = count(db, "SELECT COUNT(*) AS n FROM gift_events").await?;
    let gift_count =
        count(db, "SELECT CAST(COALESCE(SUM(gift_count),0) AS BIGINT) AS n FROM gift_events").await?;
    let gift_worlds = count(db, "SELECT COUNT(DISTINCT world_id) AS n FROM gift_events").await?;

    // billingEnabled 反映实际：有充值即 true（§2.6 阶段门由真实数据体现，不写死）。
    let billing_enabled = recharge_total > 0;

    Ok(Json(json!({
        "billingEnabled": billing_enabled,
        "recharge": { "totalCents": recharge_total, "count": recharge_count },
        "refund": { "totalCents": refund_total, "count": refund_count },
        "balance": { "totalCents": balance_total, "wallets": wallet_count },
        // 账本净额 = 充值 - 退款，恒等于当前余额合计（双录不变量），供前端自检。
        "ledgerNetCents": recharge_total - refund_total,
        "orders": { "total": orders_total, "byStatus": Value::Object(by_status) },
        "gifts": { "events": gift_events, "giftCount": gift_count, "worlds": gift_worlds },
        // 创作者结算是另一套账（签约主体/合同/税务/KYC/退款准备金），不在本只读聚合内（§2.6）。
        "creatorSettlement": { "enabled": false },
        "notes": [
            "资金来自账本双录（ledger_entries）与钱包余额（billing_balances），只读聚合，不建结算。",
            "充值总额=账本 recharge 正流水；退款总额=账本 refund 出账；余额合计=当前钱包净额（充值−退款）。",
            "用户余额不可提现/转账；创作者结算为另一套账，不在此聚合（§2.6）。"
        ],
    })))
}
