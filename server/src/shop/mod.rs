//! P3 平台售卖（feature=billing/arena，与 ledger 一致）：云成长服务位 + 平台道具单向售卖 + 创作者收益查询。
//!
//! 三个端点（全部 AuthUser 守卫，挂 /api 前缀，见 app.rs 的 #[cfg(any(billing,arena))] merge）：
//! - GET  /me/earnings                 创作者收益查询（站内 creator_earnings 余额 + 流水，owner 隔离，**明示不可提现**）
//! - POST /me/cloud-growth  {sku}       云成长服务位购买（平台增值服务，charge world_id=None 全额入平台、不分成）
//! - POST /shop/items/{sku}/purchase    平台道具单向售卖（平台→玩家，charge + 同事务 grant_item_tx 唯一写入路径）
//!
//! 资金红线（守护点，测试锁死）：
//! - **无提现出口**：本模块**绝无** withdraw/transfer/cashout/payout 端点；/me/earnings 只读余额+流水，
//!   `withdrawable` 恒 false，并诚实标注「站内可消费权益，不可提现」。
//! - **不分成（平台自营）**：cloud_growth / item_purchase 均传 `world_id=None` → ledger::resolve_share 全额入平台，
//!   无创作者对手方（对齐规格「付费点整合」表：云成长/平台售卖不分成）。
//! - **买过程/服务不买结果**：云成长只买「容量/服务位」（配额，非战力）；道具是持有物，受 per-world 准入仲裁，
//!   不改任何胜负/免死标志。诚实边界字段随响应返回。
//! - **道具单一写入路径不破**：付费履约复用 `crate::backpack::grant_item_tx`（§9.6 两条合法写入路径之一），
//!   reward_hook_key=订单号做 DB 幂等键防重复发货；**绝无**玩家→玩家交易/转移/回购换 cent。
//! - **不透支/余额不足零副作用**：扣费集中在 ledger::charge（余额不足 → 409，tx 回滚，无 entitlement/backpack/journal 残留）。
//! - **未成年**：消费侧无需重复年龄门——余额只能来自已 age-gate 的充值（未成年余额恒 0 → 有价 SKU 必然 409）。

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;

use crate::admission::{ItemDefinition, ItemOrigin};
use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/earnings", get(my_earnings))
        .route("/me/cloud-growth", post(buy_cloud_growth))
        .route("/shop/items/{sku}/purchase", post(buy_item))
    // 红线：不注册任何 withdraw / transfer / cashout / payout 路由（creator_earnings 站内可消费，本期不可提现）。
}

// ---------------- GET /me/earnings（创作者收益查询，owner 隔离，明示不可提现） ----------------

/// 创作者收益：读本人 `creator_earnings:{uid}` 账户余额 + 流水（postings join journals 溯源 reason/world）。
/// owner 隔离：account_id 由**认证用户** user_id 确定性派生（acct_creator_{uid}），无 owner 泄漏——他人只能查到自己的。
/// **红线**：`withdrawable` 恒 false（首版无提现出口）；诚实标注「站内可消费权益，不可提现」，避免被认定为承诺兑付。
async fn my_earnings(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let account_id = format!("acct_creator_{}", user.user_id);

    // 余额（无账户视为 0）。双重限定 owner_id 冗余守 owner 隔离（账户 id 已含 uid）。
    let acct: Option<(i64, i64)> = sqlx::query_as(
        "SELECT balance_cents, withdrawable FROM ledger_accounts \
         WHERE id = ? AND kind = 'creator_earnings' AND owner_id = ?",
    )
    .bind(&account_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let (balance_cents, withdrawable_flag) = acct.map(|(b, w)| (b, w != 0)).unwrap_or((0, false));

    // 流水：仅本账户的 postings（owner 隔离），join journals 溯源 reason/world_id/ref。倒序，取近 200 条。
    let rows = sqlx::query(
        "SELECT p.delta_cents AS delta, p.created_at AS at, j.reason AS reason, \
         j.world_id AS world_id, j.ref_kind AS ref_kind, j.ref_id AS ref_id \
         FROM ledger_postings p JOIN ledger_journals j ON j.id = p.journal_id \
         WHERE p.account_id = ? ORDER BY p.created_at DESC, p.id DESC LIMIT 200",
    )
    .bind(&account_id)
    .fetch_all(&state.db)
    .await?;
    let mut entries = Vec::with_capacity(rows.len());
    for r in &rows {
        entries.push(json!({
            "deltaCents": r.try_get::<i64, _>("delta")?,
            "reason": r.try_get::<String, _>("reason")?,
            "worldId": r.try_get::<Option<String>, _>("world_id")?,
            "refKind": r.try_get::<Option<String>, _>("ref_kind")?,
            "refId": r.try_get::<Option<String>, _>("ref_id")?,
            "createdAt": r.try_get::<i64, _>("at")?,
        }));
    }

    Ok(Json(json!({
        "balanceCents": balance_cents,
        // 红线：站内可消费权益，不可提现。withdrawable 恒 false（首版无任何 payout）。
        "withdrawable": withdrawable_flag,
        "note": "创作者收益为站内可消费权益，不可提现（对齐资金红线：无提现/转账出口）。",
        "entries": entries,
    })))
}

// ---------------- POST /me/cloud-growth（云成长服务位购买，平台增值服务不分成） ----------------

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudGrowthReq {
    /// 云成长 SKU（growth_sku_map.sku）：决定 entitlement_kind / 每份份额 / 单价。
    sku: String,
}

/// 云成长服务位购买：扣费（全额入平台，不分成）+ 累加 user_entitlements 配额，单事务原子。
///
/// 红线：
/// - 平台增值服务 → `charge(world_id=None)` → 全额入 platform_revenue（无创作者对手方，**不分成**）。
/// - **只买容量/服务位（过程/服务），不买战力、不买胜负**——荣誉/战力仍由引擎评估（诚实 boundary 标注）。
/// - 余额不足 → 409 insufficient_balance，tx 回滚零副作用（无 entitlement / journal 残留）。
/// - 未成年消费侧无需重复年龄门：有价 SKU 需先充值，未成年余额恒 0 → 必然 409。
async fn buy_cloud_growth(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<CloudGrowthReq>,
) -> Result<Json<Value>, ApiError> {
    // 幂等：同 Idempotency-Key 重投 → 返回缓存（不双扣、不双累加配额）。
    let payload_hash = idempotency::hash_payload(body.sku.as_bytes());
    let idem = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, "shop.cloud_growth", idem, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // SKU 校验（读只在 pool，先于 tx；释放连接再 begin，单连接池不自锁）。未知/停用 → 404。
    let sku_row: Option<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT entitlement_kind, quantity, price_cents, enabled FROM growth_sku_map WHERE sku = ?",
    )
    .bind(&body.sku)
    .fetch_optional(&state.db)
    .await?;
    let (kind, granted_qty, price, enabled) = sku_row.ok_or(ApiError::NotFound)?;
    if enabled == 0 {
        return Err(ApiError::NotFound);
    }

    // 单事务：charge（平台全额，不分成）+ 累加配额原子。ent_id 作 charge 的 journal ref_id（审计溯源）。
    let ent_id = new_id("ent");
    let now = now_ms();
    let mut tx = state.db.begin().await?;
    let receipt =
        crate::ledger::charge(&mut tx, &user.user_id, price, "cloud_growth", "entitlement", &ent_id, None).await?;
    // 累加配额（(user_id, kind) 唯一 → 命中即累加 quantity，ref_id 记最近购买 journal）。
    sqlx::query(
        "INSERT INTO user_entitlements (id, user_id, kind, quantity, ref_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(user_id, kind) DO UPDATE SET \
           quantity = user_entitlements.quantity + excluded.quantity, \
           ref_id = excluded.ref_id, \
           updated_at = excluded.updated_at",
    )
    .bind(&ent_id)
    .bind(&user.user_id)
    .bind(&kind)
    .bind(granted_qty)
    .bind(receipt.journal_id.as_deref())
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // 事务内读回累计配额（反映本次，不受并发影响）。
    let total: (i64,) = sqlx::query_as("SELECT quantity FROM user_entitlements WHERE user_id = ? AND kind = ?")
        .bind(&user.user_id)
        .bind(&kind)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;

    let resp = json!({
        "sku": body.sku,
        "entitlementKind": kind,
        "grantedQuantity": granted_qty,
        "totalQuantity": total.0,
        "chargedCents": receipt.charged_cents,
        // 诚实边界：只买容量/服务位（过程/服务），不买战力、不买胜负；平台增值服务不分成。
        "boundary": { "buys": "capacity_slot", "notPower": true, "notVerdict": true },
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ---------------- POST /shop/items/{sku}/purchase（平台道具单向售卖，复用 grant_item_tx） ----------------

/// 平台道具单向售卖：扣费（平台→玩家，全额入平台不分成）+ 同事务 grant_item_tx 履约，原子。
///
/// 红线：
/// - **道具单一写入路径不破**：付费履约复用 `crate::backpack::grant_item_tx`（§9.6），
///   `reward_hook_key=订单号` 做 DB 幂等键防重复发货（唯一键命中 → 不二次发货）。
/// - **平台单向售卖**：`charge(world_id=None)` → 全额入平台（平台自营，非玩家→玩家结算，不产二级市场/洗钱通道）。
/// - 道具是**持有物**（受 per-world admission 准入仲裁），不改胜负/免死；无回购换 cent、无玩家间转移端点。
/// - 余额不足 → 409，tx 回滚零副作用（无 items/backpacks/journal 残留）。
async fn buy_item(
    State(state): State<AppState>,
    user: AuthUser,
    Path(sku): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    // 幂等：同 Idempotency-Key 重投 → 缓存返回（不双扣、不双发货）。
    let payload_hash = idempotency::hash_payload(sku.as_bytes());
    let idem = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, "shop.item_purchase", idem, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // SKU 目录（读只在 pool）。未知/停用 → 404。
    let row = sqlx::query(
        "SELECT price_cents, narrative, effect_tags, origin_world_template_id, cosmology_json, power_tier, enabled \
         FROM item_sku_map WHERE sku = ?",
    )
    .bind(&sku)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let enabled: i64 = row.try_get("enabled")?;
    if enabled == 0 {
        return Err(ApiError::NotFound);
    }
    let price: i64 = row.try_get("price_cents")?;
    let narrative: String = row.try_get("narrative")?;
    let effect_tags_json: String = row.try_get("effect_tags")?;
    let origin_tpl: String = row.try_get("origin_world_template_id")?;
    let cosmology_json: String = row.try_get("cosmology_json")?;
    let power_tier: i64 = row.try_get("power_tier")?;

    // 道具定义：items.id 按 sku 确定性共享去重（多用户购同 sku 共享同一 items 定义、各自 backpack 归属行）。
    let item = ItemDefinition {
        id: format!("item_sku_{sku}"),
        narrative,
        effect_tags: serde_json::from_str(&effect_tags_json).unwrap_or_default(),
        origin: ItemOrigin {
            world_template_id: origin_tpl,
            cosmology: serde_json::from_str(&cosmology_json).unwrap_or_default(),
            power_tier: power_tier.clamp(0, u8::MAX as i64) as u8,
        },
    };

    // 订单号：既作 charge 的 journal ref_id，又作 grant_item_tx 的 reward_hook_key（防重复发货，§9.6）。
    let order_id = new_id("order");
    // 单事务：charge（平台单向售卖 world_id=None 全额入平台）+ grant_item_tx（唯一写入路径）原子。
    let mut tx = state.db.begin().await?;
    let receipt =
        crate::ledger::charge(&mut tx, &user.user_id, price, "item_purchase", "item_sku", &order_id, None).await?;
    // acquired_world_id='platform_shop' 哨兵（backpacks 无 FK）；reward_hook_key=order_id 幂等防重复发货。
    let bp = crate::backpack::grant_item_tx(&mut tx, &user.user_id, &item, "platform_shop", Some(&order_id)).await?;
    tx.commit().await?;

    let resp = json!({
        "sku": sku,
        "orderId": order_id,
        // Some=本次发货的背包行 id；None=reward_hook_key 命中已发货（幂等，不二次发货）。
        "backpackId": bp,
        "itemId": item.id,
        "chargedCents": receipt.charged_cents,
        // 诚实边界：平台单向售卖（平台→玩家）；道具不可回购换 cent、不可玩家间交易；持有物受 per-world 准入。
        "boundary": { "sale": "platform_to_player", "notTradable": true, "notWithdrawable": true },
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

#[cfg(test)]
mod tests;
