//! 计费（P4b，feature=billing）：用户侧钱包充值 / 余额 / 退款。
//!
//! 经济红线（平台规格 §2.6，写进实现与测试）：
//! - 余额**不可提现、不可转账**：本模块只提供 orders / balance / refunds，**绝无** withdraw / transfer 端点。
//! - 账本**双录**：每一次余额变动都写一条 `ledger_entries`，恒有 `SUM(ledger.delta_cents) == balance_cents`。
//! - 订单 / 退款**幂等**：副作用端点走 `idempotency::guard`（同 key 不双扣不双记）；退款另有状态机防重复退。
//! - **保守拒充**：仅 `users.age_declared == 1`（已声明成年）放行；未声明(0)/未成年(2)/无用户行 → 403
//!   （规格 §2.2 未成年人默认保护：无法可靠判断年龄前保守限制付费）。年龄声明入口见 `POST /auth/age-declaration`。
//! - 用户钱包与创作者结算是两套账，本模块只做用户侧（不碰创作者结算 / 不碰世界胜负结果）。
//!
//! 端点（全部 AuthUser 守卫，挂 /api 前缀，见 app.rs 的 #[cfg(feature="billing")] merge）：
//! - POST /billing/orders   {kind:"recharge", amountCents} + Idempotency-Key
//!     → 保守拒充（仅已声明成年放行）→ DevPayment 履约（dev 立即成功）
//!     → **单事务** orders(created→paid→fulfilled) + ledger(+amount) + billing_balances(+amount upsert)
//!     → {orderId, balanceCents}
//! - GET  /billing/balance  → {balanceCents}（无行视为 0）
//! - POST /billing/refunds  {orderId} + Idempotency-Key
//!     → 状态机（仅 fulfilled 可退；已 refunded 幂等返回）
//!     → **单事务** 逆向 ledger(-amount) + 扣余额 + orders.status=refunded → {orderId, status, balanceCents}

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;
use crate::providers::{DevPayment, PaymentProvider};

/// 单笔充值上限（分）——支付风控最小护栏（规格 §商业化：充值限额）；同时防金额溢出/滥用。
const MAX_RECHARGE_CENTS: i64 = 10_000_000; // 10 万元

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/billing/orders", post(create_order))
        .route("/billing/balance", get(get_balance))
        .route("/billing/refunds", post(create_refund))
    // 红线：不注册任何 withdraw / transfer / cashout 路由（余额不可提现不可转账）。
}

// ---------------- 请求 / 响应（camelCase 与客户端一致） ----------------

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateOrderReq {
    kind: String,
    amount_cents: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrderResp {
    order_id: String,
    balance_cents: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BalanceResp {
    balance_cents: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefundReq {
    order_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefundResp {
    order_id: String,
    status: String,
    balance_cents: i64,
}

// ---------------- 辅助 ----------------

fn idem_key(headers: &HeaderMap) -> Option<String> {
    headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(|s| s.to_string())
}

fn json_response(body: String) -> Response {
    ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// 读用户余额；无行视为 0。
async fn read_balance(db: &sqlx::AnyPool, user_id: &str) -> Result<i64, ApiError> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM billing_balances WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|(b,)| b).unwrap_or(0))
}

// ---------------- handler ----------------

/// POST /billing/orders：充值下单 + 履约入账（单事务，账本双录，未成年拒充，幂等）。
async fn create_order(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<CreateOrderReq>,
) -> Result<Response, ApiError> {
    // 快速校验（无副作用，先于幂等记账）。
    if req.kind != "recharge" {
        return Err(ApiError::BadRequest("本端点仅支持 kind=recharge".into()));
    }
    if req.amount_cents <= 0 || req.amount_cents > MAX_RECHARGE_CENTS {
        return Err(ApiError::BadRequest("充值金额无效（须为正且不超过限额）".into()));
    }

    // 红线：保守拒充（规格 §2.2 未成年人默认保护）。仅"已声明成年"（age_declared==1）放行；
    // 未声明(0)、未成年(2)、用户行缺失一律 403——无法可靠判断年龄前保守限制付费（堵住"仅拦 2"的空防）。
    // 置于履约前：被拒请求零账务副作用（无 order / 无 ledger / 无 balance）。声明入口：POST /auth/age-declaration。
    let age: Option<(i64,)> = sqlx::query_as("SELECT age_declared FROM users WHERE id = ?")
        .bind(&user.user_id)
        .fetch_optional(&state.db)
        .await?;
    if !matches!(age, Some((1,))) {
        return Err(ApiError::Forbidden);
    }

    // 幂等：同 key 同载荷 → 返回缓存响应（不双扣不双记）；同 key 异载荷 → 409。
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let key = idem_key(&headers);
    let guard =
        idempotency::guard(&state.db, &user.user_id, "POST /billing/orders", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }

    // DevPayment 履约：真实接入位在此替换 provider（trait 依赖，BUILD-STATUS 约定 8）；
    // 网络调用置于事务外，避免长时间持有 DB 连接锁。dev 立即成功。
    let order_id = new_id("order");
    let external_ref = DevPayment
        .create_order(&order_id, req.amount_cents)
        .await
        .map_err(|e| ApiError::Internal(Box::from(e)))?;

    // 单事务：orders(created→paid→fulfilled) + ledger(+amount) + billing_balances(+amount, upsert)。
    // 三笔状态流转 + 账本 + 余额原子提交；中间态永不外泄，履约与入账要么全成要么全无。
    let now = now_ms();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO orders (id, user_id, kind, amount_cents, status, external_ref, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'created', ?, ?, ?)",
    )
    .bind(&order_id)
    .bind(&user.user_id)
    .bind(&req.kind)
    .bind(req.amount_cents)
    .bind(&external_ref)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE orders SET status = 'paid', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(&order_id)
        .execute(&mut *tx)
        .await?;
    // 账本双录：余额 +amount 必配一条 ledger +amount。
    sqlx::query(
        "INSERT INTO ledger_entries (id, user_id, order_id, delta_cents, reason, created_at) \
         VALUES (?, ?, ?, ?, 'recharge', ?)",
    )
    .bind(new_id("ldg"))
    .bind(&user.user_id)
    .bind(&order_id)
    .bind(req.amount_cents)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO billing_balances (user_id, balance_cents, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET \
           balance_cents = billing_balances.balance_cents + excluded.balance_cents, \
           updated_at = excluded.updated_at",
    )
    .bind(&user.user_id)
    .bind(req.amount_cents)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE orders SET status = 'fulfilled', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(&order_id)
        .execute(&mut *tx)
        .await?;
    // 事务内读回余额，保证返回值恰好反映本次履约（不受并发影响）。
    let balance: (i64,) = sqlx::query_as("SELECT balance_cents FROM billing_balances WHERE user_id = ?")
        .bind(&user.user_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;

    let resp = OrderResp { order_id, balance_cents: balance.0 };
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// GET /billing/balance：当前用户余额（无行 → 0）。
async fn get_balance(State(state): State<AppState>, user: AuthUser) -> Result<Json<BalanceResp>, ApiError> {
    let balance = read_balance(&state.db, &user.user_id).await?;
    Ok(Json(BalanceResp { balance_cents: balance }))
}

/// POST /billing/refunds：退款（状态机 + 单事务逆向入账，幂等）。
async fn create_refund(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<RefundReq>,
) -> Result<Response, ApiError> {
    let order_id = req.order_id.trim().to_string();
    if order_id.is_empty() {
        return Err(ApiError::BadRequest("orderId 必填".into()));
    }

    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let key = idem_key(&headers);
    let guard =
        idempotency::guard(&state.db, &user.user_id, "POST /billing/refunds", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }

    let now = now_ms();
    let mut tx = state.db.begin().await?;
    // 行锁 / 串行化：Postgres 下自赋值 UPDATE 等价 SELECT ... FOR UPDATE，序列化并发退款；
    // SQLite 单连接事务本就互斥，此语句为无害占位。防两个并发退款请求双逆向。
    sqlx::query("UPDATE orders SET status = status WHERE id = ?")
        .bind(&order_id)
        .execute(&mut *tx)
        .await?;

    let row: Option<(String, i64, String)> =
        sqlx::query_as("SELECT user_id, amount_cents, status FROM orders WHERE id = ?")
            .bind(&order_id)
            .fetch_optional(&mut *tx)
            .await?;
    // return Err 时 tx 被 drop → sqlx 自动回滚。
    let (owner, amount, status) = row.ok_or(ApiError::NotFound)?;
    if owner != user.user_id {
        return Err(ApiError::Forbidden); // 只能退自己的订单
    }

    // 状态机：仅 fulfilled 可退；已 refunded 幂等返回（不重复逆向）；其余状态冲突。
    let final_status = match status.as_str() {
        "refunded" => "refunded",
        "fulfilled" => {
            // 账本双录：余额 -amount 必配一条 ledger -amount。
            sqlx::query(
                "INSERT INTO ledger_entries (id, user_id, order_id, delta_cents, reason, created_at) \
                 VALUES (?, ?, ?, ?, 'refund', ?)",
            )
            .bind(new_id("ldg"))
            .bind(&user.user_id)
            .bind(&order_id)
            .bind(-amount)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE billing_balances SET balance_cents = balance_cents - ?, updated_at = ? WHERE user_id = ?")
                .bind(amount)
                .bind(now)
                .bind(&user.user_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE orders SET status = 'refunded', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&order_id)
                .execute(&mut *tx)
                .await?;
            "refunded"
        }
        _ => return Err(ApiError::Conflict("仅已履约（fulfilled）订单可退款".into())),
    };

    let balance: (i64,) = sqlx::query_as("SELECT balance_cents FROM billing_balances WHERE user_id = ?")
        .bind(&user.user_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;

    let resp = RefundResp { order_id, status: final_status.to_string(), balance_cents: balance.0 };
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

#[cfg(all(test, feature = "billing"))]
mod tests;
