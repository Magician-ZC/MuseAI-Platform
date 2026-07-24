//! 历练值 + 卡位制（波次 2）。
//!
//! 产品定位（红线）：历练 = 参与产出的成长值，挂在云端角色卡上（卡是养成容器），
//! 只用于**准入与解锁**（卡位阈值等），**绝不进入引擎决策**——RoundInput / role_decide /
//! 仲裁一概不读历练（叙事平权是产品根基，见本模块 tests 的 grep 级红线断言）。
//!
//! - `grant_mileage_tx` 是历练的**唯一写入路径**（仿 backpack::grant_item_tx 惯例）：
//!   各结算点（章节房通关/隐藏任务、idle 房终局、arena 结算）在**既有结算事务内**调用，
//!   结算失败即回滚，不发历练。
//! - 卡位：users.card_slots（默认 3，上限 6）；发布云端角色时校验「未撤回角色数 < 卡位数」；
//!   总历练（owner 全部未撤回卡的 mileage 之和）达阈值可解锁下一卡位。
//!
//! 端点：
//! GET  /me/progression        → { totalMileage, cardSlots, maxSlots, nextSlotAt }
//! POST /me/card-slots/unlock  → 达下一阈值则 card_slots+1；未达/已到上限 → 400

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sqlx::{Any, AnyPool, Transaction};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;

// ---------------- 平衡参数集中区（可调，数值即产品策划口径） ----------------

/// 章节房通关：每张参与卡（结算时该世界 active 成员的云端角色）+100。
pub(crate) const MILEAGE_CHAPTER_CLEAR: i64 = 100;
/// 章节房隐藏任务：每完成一个隐藏钩子（与隐藏道具带出同幂等口径）+50。
pub(crate) const MILEAGE_HIDDEN_TASK: i64 = 50;
/// idle 放置房终局：每张在场卡（终局那一刻的 active 成员角色）+60。
pub(crate) const MILEAGE_IDLE_ENDING: i64 = 60;
/// arena 赛事结算：每张参赛卡（收敛落定时的 active 成员角色，含冠军）+40。
#[cfg(feature = "arena")]
pub(crate) const MILEAGE_ARENA_PARTICIPANT: i64 = 40;
/// arena 赛事冠军：唯一胜者在参与分之外另 +120。
#[cfg(feature = "arena")]
pub(crate) const MILEAGE_ARENA_CHAMPION: i64 = 120;

/// 卡位硬上限：历练解锁到第 6 位为止。
pub(crate) const MAX_CARD_SLOTS: i64 = 6;

/// 卡位解锁阈值：当前卡位数 → 解锁下一位所需**总历练**（owner 全部未撤回卡 mileage 之和）。
/// 第 4 位 = 500、第 5 位 = 1500、第 6 位 = 4000；已到上限 → None。
pub(crate) fn next_slot_threshold(current_slots: i64) -> Option<i64> {
    match current_slots {
        ..=3 => Some(500),
        4 => Some(1500),
        5 => Some(4000),
        _ => None,
    }
}

// ---------------- 唯一写入路径 ----------------

/// 发放历练（历练的**唯一写入路径**，仿 grant_item_tx 惯例）：
/// 在调用方结算事务内 `UPDATE cloud_characters SET mileage = mileage + ?`，与既有结算原子——
/// 结算回滚则历练同滚，绝无「结算失败仍发历练」。
/// `amount` 必须为正（历练只增不减，无任何扣减/回收路径）；`reason` 仅作审计日志标注。
/// 返回受影响行数（0 = 目标角色不存在，容忍跳过不报错，避免 NPC/已删卡拖垮整笔结算）。
pub(crate) async fn grant_mileage_tx(
    tx: &mut Transaction<'_, Any>,
    cloud_character_id: &str,
    amount: i64,
    reason: &str,
) -> Result<u64, ApiError> {
    if amount <= 0 {
        return Err(ApiError::BadRequest("历练发放数额必须为正数".into()));
    }
    let res = sqlx::query("UPDATE cloud_characters SET mileage = mileage + ? WHERE id = ?")
        .bind(amount)
        .bind(cloud_character_id)
        .execute(&mut **tx)
        .await?;
    let rows = res.rows_affected();
    if rows > 0 {
        tracing::info!(cloud_character_id, amount, reason, "发放历练");
    } else {
        tracing::warn!(cloud_character_id, amount, reason, "历练发放目标角色不存在，跳过");
    }
    Ok(rows)
}

/// idle 放置房终局结算：每张在场卡 +60（runtime 终局事务内调用）。
/// 收在本模块是刻意设计：runtime/mod.rs（RoundInput 组装处）不出现任何历练字段引用，
/// 红线「历练不进引擎决策」在 grep 级即可验证（见 tests）。
pub(crate) async fn settle_idle_world_ending_tx(
    tx: &mut Transaction<'_, Any>,
    member_char_ids: &[String],
) -> Result<(), ApiError> {
    for cid in member_char_ids {
        grant_mileage_tx(tx, cid, MILEAGE_IDLE_ENDING, "idle_world_ending").await?;
    }
    Ok(())
}

// ---------------- 读侧辅助（卡位校验 / 进度查询共用） ----------------

/// owner 的总历练 = 全部**未撤回**云端角色的 mileage 之和。
/// 汇总在 Rust 侧完成（避免 SQL SUM 的双库类型差异，遵守 db.rs 可移植子集约定）。
pub(crate) async fn total_mileage(db: &AnyPool, owner_id: &str) -> Result<i64, ApiError> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT mileage FROM cloud_characters WHERE owner_id = ? AND withdrawn = 0")
            .bind(owner_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().map(|(m,)| m).sum())
}

/// owner 现有未撤回云端角色数（发布卡位校验的分子）。
pub(crate) async fn count_active_cards(db: &AnyPool, owner_id: &str) -> Result<i64, ApiError> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cloud_characters WHERE owner_id = ? AND withdrawn = 0")
            .bind(owner_id)
            .fetch_one(db)
            .await?;
    Ok(n)
}

/// 用户当前卡位数（users.card_slots；行缺失按默认 3 兜底，不因历史数据报错）。
pub(crate) async fn card_slots_of(db: &AnyPool, user_id: &str) -> Result<i64, ApiError> {
    let slots: Option<i64> = sqlx::query_scalar("SELECT card_slots FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    Ok(slots.unwrap_or(3))
}

// ---------------- 端点 ----------------

/// GET /me/progression：我的历练进度与卡位。
/// `nextSlotAt` = 解锁下一卡位所需总历练阈值；已到 6 上限 → null。
async fn my_progression(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let total = total_mileage(&state.db, &user.user_id).await?;
    let slots = card_slots_of(&state.db, &user.user_id).await?;
    Ok(Json(json!({
        "totalMileage": total,
        "cardSlots": slots,
        "maxSlots": MAX_CARD_SLOTS,
        "nextSlotAt": next_slot_threshold(slots),
    })))
}

/// POST /me/card-slots/unlock：总历练达下一阈值则解锁一个卡位（card_slots+1）。
/// - 已到 6 上限 → 400；
/// - 总历练未达阈值 → 400（文案含还差多少）；
/// - 并发解锁由 `card_slots = 旧值` 的 CAS 挡住（0 行命中 → 409 请重试），绝不越阈值双跳。
async fn unlock_card_slot(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let slots = card_slots_of(&state.db, &user.user_id).await?;
    let Some(threshold) = next_slot_threshold(slots) else {
        return Err(ApiError::BadRequest(format!("卡位已达上限（{MAX_CARD_SLOTS}），无法继续解锁")));
    };
    let total = total_mileage(&state.db, &user.user_id).await?;
    if total < threshold {
        let lack = threshold - total;
        return Err(ApiError::BadRequest(format!(
            "历练不足：解锁第 {} 个卡位需总历练 {threshold}，当前 {total}，还差 {lack}",
            slots + 1
        )));
    }
    // CAS：仅当卡位仍是读到的旧值才 +1（历练只增不减，阈值判定单调安全；并发解锁只成一次）。
    let res = sqlx::query("UPDATE users SET card_slots = card_slots + 1, updated_at = ? WHERE id = ? AND card_slots = ?")
        .bind(crate::db::now_ms())
        .bind(&user.user_id)
        .bind(slots)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::Conflict("卡位状态已变化，请刷新后重试".into()));
    }
    let new_slots = slots + 1;
    Ok(Json(json!({
        "cardSlots": new_slots,
        "totalMileage": total,
        "maxSlots": MAX_CARD_SLOTS,
        "nextSlotAt": next_slot_threshold(new_slots),
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/progression", get(my_progression))
        .route("/me/card-slots/unlock", post(unlock_card_slot))
}

#[cfg(test)]
mod tests;
