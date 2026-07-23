//! 章节房（S4；P5 期权——路由挂载但功能开关默认关，world.room_type=chapter 才生效）。
//!
//! 端点：
//! POST /worlds/{id}/chapters/start     开始一章会话（复用 S2 runtime 的 tick，tick 来源改会话驱动）
//! POST /worlds/{id}/chapters/finish    章节结算：主线节点推进 + 通关判定 + grant_item（隐藏道具兑现）+ 离线夹层启动
//! GET  /worlds/{id}/offline-gains       离线夹层收益（角色自动训练/探索摘要，回来领取）
//!
//! 主场优劣势（§2.5）：装配时本书角色自动挂原作预知知识包（引擎 P1 机制）+ 原作宿命作硬节点（装配层已标注）。
//! 会话与离线状态存于 worlds.assembled_json 的 `chapterState` 段（assembly 段钉住，本模块只动 chapterState）。

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::assembly::{self, AssembledInstance};
use crate::auth::AuthUser;
use crate::backpack::grant_item;
use crate::error::ApiError;
use crate::idempotency;
use crate::worlds::load_world;

#[cfg(test)]
mod tests;

// ---------- 辅助 ----------

/// 本人在该世界的在场角色（章节房结算/收益的主体）。
async fn user_active_character(
    db: &AnyPool,
    world_id: &str,
    user_id: &str,
) -> Result<Option<String>, ApiError> {
    let row = sqlx::query(
        "SELECT cloud_character_id FROM world_members \
         WHERE world_id = ? AND user_id = ? AND status = 'active' LIMIT 1",
    )
    .bind(world_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some(r) => Some(r.try_get("cloud_character_id")?),
        None => None,
    })
}

/// 主线硬节点数（通关判定用）：读模板骨架 mainlineNodes 长度。
async fn mainline_node_count(db: &AnyPool, template_id: &str) -> Result<usize, ApiError> {
    let row = sqlx::query("SELECT skeleton_json FROM world_templates WHERE id = ?")
        .bind(template_id)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        return Ok(0);
    };
    let raw: String = row.try_get("skeleton_json")?;
    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
    Ok(v["mainlineNodes"].as_array().map(|a| a.len()).unwrap_or(0))
}

fn require_chapter_room(room_type: &str) -> Result<(), ApiError> {
    // 功能开关：仅 room_type=chapter 生效（P5 期权）。
    if room_type != "chapter" {
        return Err(ApiError::Conflict("not_chapter_room".into()));
    }
    Ok(())
}

/// 读取已装配结果（assembly 段）；未装配 → None。
fn assembly_of(wrapper: &Value) -> Option<AssembledInstance> {
    wrapper
        .get("assembly")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<AssembledInstance>(v.clone()).ok())
}

fn chapter_state_of(wrapper: &Value) -> Value {
    wrapper
        .get("chapterState")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(assembly::empty_chapter_state)
}

// ---------- POST /worlds/{id}/chapters/start ----------

/// 开始一章会话：（首次）触发开局装配 → 置世界 running → 会话驱动排一个 tick。
async fn chapter_start(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(world_id.as_bytes());
    let guard =
        idempotency::guard(&state.db, &user.user_id, "chapters.start", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    let world = load_world(&state.db, &world_id).await?;
    require_chapter_room(&world.room_type)?;

    // 参与校验：必须在场（本人角色已投放）。
    let cid = user_active_character(&state.db, &world_id, &user.user_id)
        .await?
        .ok_or(ApiError::Forbidden)?;

    // 一次性开局装配（幂等：已装配则复用，不重装）。
    let mut wrapper = assembly::load_wrapper(&state.db, &world_id).await?;
    let assembled = match assembly_of(&wrapper) {
        Some(a) => a,
        None => {
            let a = assembly::assemble_instance(&state, &world_id).await?;
            wrapper = assembly::load_wrapper(&state.db, &world_id).await?;
            a
        }
    };

    // 置 running（open→running），使会话 tick 可被 runtime 处理。
    if world.status == "open" {
        sqlx::query("UPDATE worlds SET status = 'running', updated_at = ? WHERE id = ? AND status = 'open'")
            .bind(crate::db::now_ms())
            .bind(&world_id)
            .execute(&state.db)
            .await?;
    }

    // 会话驱动：排一个 tick（复用 S2 runtime；无模型时 runtime 自然跳过，不 panic）。
    let tick_no = crate::runtime::schedule_tick(&state, &world_id).await?;

    // 记录会话开始时间（只改 chapterState，assembly 段保持钉住）。
    let mut cs = chapter_state_of(&wrapper);
    cs["sessionStartedAt"] = json!(crate::db::now_ms());
    wrapper["chapterState"] = cs.clone();
    assembly::save_wrapper(&state.db, &world_id, &wrapper).await?;

    let response = json!({
        "worldId": world_id,
        "roomType": world.room_type,
        "characterId": cid,
        "chapterSession": {
            "startedAt": cs["sessionStartedAt"],
            "currentNode": cs["currentNode"],
            "tickNo": tick_no,
        },
        "enabledEndings": assembled.enabled_endings,
        "hookCount": assembled.per_character_hooks.len(),
        "homeAdvantages": assembled.home_advantages,
    });
    guard.store_response(&state.db, &response.to_string()).await?;
    Ok(Json(response))
}

// ---------- POST /worlds/{id}/chapters/finish ----------

/// 章节结算：主线推进 + 通关判定 + grant_item 兑现隐藏道具 + 离线夹层启动。
async fn chapter_finish(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(world_id.as_bytes());
    let guard =
        idempotency::guard(&state.db, &user.user_id, "chapters.finish", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    let world = load_world(&state.db, &world_id).await?;
    require_chapter_room(&world.room_type)?;
    let cid = user_active_character(&state.db, &world_id, &user.user_id)
        .await?
        .ok_or(ApiError::Forbidden)?;

    let mut wrapper = assembly::load_wrapper(&state.db, &world_id).await?;
    let assembled = assembly_of(&wrapper)
        .ok_or_else(|| ApiError::BadRequest("chapter_not_started".into()))?;
    let mut cs = chapter_state_of(&wrapper);

    // 已兑现钩子集合（跨重复结算幂等，不二次发货）。
    let mut granted_ids: Vec<String> = cs["grantedHookIds"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    // 兑现隐藏道具：仅本人角色、携带 reward_item、且未兑现过的钩子。
    // grant_item 是 §9.6 两条合法写入路径之一（tick 结算侧）。
    let mut granted_items = Vec::new();
    for hook in &assembled.per_character_hooks {
        if hook.character_id != cid {
            continue;
        }
        let Some(reward) = &hook.reward_item else {
            continue;
        };
        let key = format!("{}:{}", hook.character_id, hook.pool_item_id);
        if granted_ids.contains(&key) {
            continue;
        }
        grant_item(&state.db, &user.user_id, reward, &world_id).await?;
        granted_ids.push(key);
        granted_items.push(json!({ "itemId": reward.id, "narrative": reward.narrative }));
    }

    // 主线推进 + 通关判定。
    let total_nodes = mainline_node_count(&state.db, &world.template_id).await?;
    let next_node = cs["currentNode"].as_i64().unwrap_or(0) + 1;
    let cleared = total_nodes > 0 && next_node as usize >= total_nodes;

    // 离线夹层启动：为本人角色追加一条离线收益（自动训练摘要，回来领取）。
    let mut offline_gains: Vec<Value> = cs["offlineGains"].as_array().cloned().unwrap_or_default();
    let summary = format!(
        "章节结算后角色于离线夹层自动训练：巩固第 {next_node} 幕经历{}。",
        if cleared { "，副本主线通关" } else { "" }
    );
    offline_gains.push(assembly::build_offline_gain(&cid, "training", &summary));

    // 写回 chapterState（assembly 段不动）。
    cs["currentNode"] = json!(next_node);
    cs["cleared"] = json!(cleared);
    cs["grantedHookIds"] = json!(granted_ids);
    cs["offlineGains"] = json!(offline_gains);
    wrapper["chapterState"] = cs;
    assembly::save_wrapper(&state.db, &world_id, &wrapper).await?;

    let response = json!({
        "worldId": world_id,
        "characterId": cid,
        "advancedTo": next_node,
        "totalNodes": total_nodes,
        "cleared": cleared,
        "grantedItems": granted_items,
        "offlineStarted": true,
    });
    guard.store_response(&state.db, &response.to_string()).await?;
    Ok(Json(response))
}

// ---------- GET /worlds/{id}/offline-gains ----------

/// 离线夹层收益：返回本人角色累积的离线收益条目。
async fn offline_gains(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let world = load_world(&state.db, &world_id).await?;
    require_chapter_room(&world.room_type)?;
    let cid = user_active_character(&state.db, &world_id, &user.user_id)
        .await?
        .ok_or(ApiError::Forbidden)?;

    let wrapper = assembly::load_wrapper(&state.db, &world_id).await?;
    let cs = chapter_state_of(&wrapper);
    let gains: Vec<Value> = cs["offlineGains"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|g| g["characterId"].as_str() == Some(cid.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(json!({ "worldId": world_id, "characterId": cid, "gains": gains })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worlds/{id}/chapters/start", post(chapter_start))
        .route("/worlds/{id}/chapters/finish", post(chapter_finish))
        .route("/worlds/{id}/offline-gains", get(offline_gains))
}
