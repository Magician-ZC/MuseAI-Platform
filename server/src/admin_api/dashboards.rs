//! 数据看板 + 经济运营（P4a 占位）。看板为只读 SQL 聚合（COUNT/SUM）。

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::error::ApiError;

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
    _admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
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

/// GET /admin/economy/overview：P4a 占位。P4a 无充值/无平台币/无分成，不建实际结算（§2.6）。
pub(super) async fn economy_overview(
    State(_state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "stage": "P4a",
        "billingEnabled": false,
        "message": "P4a 未启用计费：无充值、无平台币、无分成（§2.6）。经济模块占位，不进行实际结算。",
        "orders": { "total": 0, "paid": 0, "refunded": 0 },
        "userBalances": { "totalCents": 0 },
        "creatorSettlement": { "enabled": false },
        "notes": [
            "P4b 获批后再增加订单/退款/对账（feature=billing 编译）。",
            "创作者结算与用户余额是两套账，不混用同一 wallet 概念（§2.6）。",
            "分成比例在真实成本/渠道费/税费/退款出来前保持 TBD，不预填。"
        ],
    })))
}
