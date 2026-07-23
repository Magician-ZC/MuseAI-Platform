//! 世界生命周期（S2）：大厅列表 / 详情 / 投放(join) / 离场(leave) + 内部 create_world。
//!
//! 端点：
//! GET  /worlds?type=idle|chapter|arena   大厅列表（cursor 分页；只出 open/running 且 official/public）
//! GET  /worlds/{id}                      详情（世界书简介/规则/公开阵容/AI 标识展示）
//! POST /worlds/{id}/join                 投放角色：AuthUser + Idempotency-Key + cloudCharacterId + boundary
//!   服务端权威校验（§9.6）：角色 approved 且未 withdrawn 且属于本人；人数上限；写 world_members
//! POST /worlds/{id}/leave                离场：置成员 left（离场事件交由下个 tick 叙事化）
//!
//! 官方建房走 admin(S6)，此处提供内部 create_world 供其调用；创建时钉住
//! engine_version/prompt_set_version/model_route_version/template_version（§9.2 版本钉住）。

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;

#[cfg(test)]
mod tests;

/// 世界行（worlds 表投影，runtime 复用）。
#[derive(Debug, Clone)]
pub struct WorldRow {
    pub id: String,
    pub template_id: String,
    pub template_version: i64,
    pub engine_version: String,
    pub prompt_set_version: String,
    pub model_route_version: String,
    pub room_type: String,
    pub title: String,
    pub status: String,
    pub visibility: String,
    pub host_user_id: Option<String>,
    pub member_limit: i64,
    pub tick_per_day: i64,
    pub state_revision: i64,
    /// 当前叙事状态快照（E4 联编后由 worker 读取用于回合恢复/上下文）。
    #[allow(dead_code)]
    pub narrative_state_json: String,
}

fn map_world(row: &sqlx::any::AnyRow) -> Result<WorldRow, ApiError> {
    Ok(WorldRow {
        id: row.try_get("id")?,
        template_id: row.try_get("template_id")?,
        template_version: row.try_get("template_version")?,
        engine_version: row.try_get("engine_version")?,
        prompt_set_version: row.try_get("prompt_set_version")?,
        model_route_version: row.try_get("model_route_version")?,
        room_type: row.try_get("room_type")?,
        title: row.try_get("title")?,
        status: row.try_get("status")?,
        visibility: row.try_get("visibility")?,
        host_user_id: row.try_get("host_user_id")?,
        member_limit: row.try_get("member_limit")?,
        tick_per_day: row.try_get("tick_per_day")?,
        state_revision: row.try_get("state_revision")?,
        narrative_state_json: row.try_get("narrative_state_json")?,
    })
}

/// 读取世界（不存在 → NotFound）。runtime 与 handler 共用。
pub async fn load_world(db: &AnyPool, id: &str) -> Result<WorldRow, ApiError> {
    let row = sqlx::query("SELECT * FROM worlds WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    map_world(&row)
}

async fn active_member_count(db: &AnyPool, world_id: &str) -> Result<i64, ApiError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS n FROM world_members WHERE world_id = ? AND status = 'active'",
    )
    .bind(world_id)
    .fetch_one(db)
    .await?;
    Ok(row.try_get::<i64, _>("n")?)
}

// ---------- 大厅列表 ----------

#[derive(Debug, Deserialize)]
struct ListParams {
    #[serde(rename = "type")]
    room_type: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// cursor 编码为 `{created_at}:{id}`（created_at 无冒号，按首个冒号切分）。
fn parse_cursor(cursor: &str) -> Option<(i64, String)> {
    let (ts, id) = cursor.split_once(':')?;
    Some((ts.parse().ok()?, id.to_string()))
}

async fn list_worlds(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    let page = params.limit.unwrap_or(20).clamp(1, 100);
    // 仅可见世界：open/running 且 official/public。
    let mut sql = String::from(
        "SELECT id, room_type, title, status, visibility, member_limit, tick_per_day, created_at, \
         (SELECT COUNT(*) FROM world_members m WHERE m.world_id = worlds.id AND m.status='active') AS member_count \
         FROM worlds \
         WHERE status IN ('open','running') AND visibility IN ('official','public')",
    );
    if params.room_type.is_some() {
        sql.push_str(" AND room_type = ?");
    }
    let cursor = params.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

    let mut q = sqlx::query(&sql);
    if let Some(rt) = &params.room_type {
        q = q.bind(rt);
    }
    if let Some((ts, id)) = &cursor {
        q = q.bind(*ts).bind(*ts).bind(id);
    }
    q = q.bind(page + 1);

    let rows = q.fetch_all(&state.db).await?;
    let has_more = rows.len() as i64 > page;
    let mut items = Vec::new();
    let mut next_cursor: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        if i as i64 >= page {
            break;
        }
        let created_at: i64 = row.try_get("created_at")?;
        let id: String = row.try_get("id")?;
        next_cursor = Some(format!("{created_at}:{id}"));
        items.push(json!({
            "id": id,
            "roomType": row.try_get::<String, _>("room_type")?,
            "title": row.try_get::<String, _>("title")?,
            "status": row.try_get::<String, _>("status")?,
            "visibility": row.try_get::<String, _>("visibility")?,
            "memberLimit": row.try_get::<i64, _>("member_limit")?,
            "memberCount": row.try_get::<i64, _>("member_count")?,
            "tickPerDay": row.try_get::<i64, _>("tick_per_day")?,
            "aiLabel": { "visible": true },
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "worlds": items, "nextCursor": next_cursor })))
}

// ---------- 世界详情 ----------

async fn world_detail(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let world = load_world(&state.db, &id).await?;
    // 私有世界仅成员/房主可见详情。
    if world.visibility == "private" {
        let is_host = world.host_user_id.as_deref() == Some(user.user_id.as_str());
        let is_member = sqlx::query(
            "SELECT 1 AS x FROM world_members WHERE world_id = ? AND user_id = ? AND status='active' LIMIT 1",
        )
        .bind(&id)
        .bind(&user.user_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();
        if !is_host && !is_member {
            return Err(ApiError::Forbidden);
        }
    }

    // 公开阵容：active 成员 + 角色公开名（AI 标识）。
    let member_rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, cc.card_json AS card \
         FROM world_members wm JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.world_id = ? AND wm.status='active' ORDER BY wm.joined_at ASC",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut roster = Vec::new();
    for r in &member_rows {
        let cid: String = r.try_get("cid")?;
        let card: String = r.try_get("card")?;
        let name = serde_json::from_str::<Value>(&card)
            .ok()
            .and_then(|v| v["identity"]["name"].as_str().map(str::to_string))
            .unwrap_or_default();
        roster.push(json!({ "cloudCharacterId": cid, "name": name, "aiLabel": { "visible": true } }));
    }

    Ok(Json(json!({
        "id": world.id,
        "title": world.title,
        "roomType": world.room_type,
        "status": world.status,
        "visibility": world.visibility,
        "memberLimit": world.member_limit,
        "memberCount": roster.len(),
        "tickPerDay": world.tick_per_day,
        // 客户端干预用 expectedWorldRevision 做乐观并发校验（C1 集成缝）。
        "stateRevision": world.state_revision,
        "templateId": world.template_id,
        "templateVersion": world.template_version,
        "engineVersion": world.engine_version,
        "promptSetVersion": world.prompt_set_version,
        "modelRouteVersion": world.model_route_version,
        "roster": roster,
        // 合规信息展示（§2.1）：AI 生成标识 + 仲裁公开承诺。
        "aiLabel": { "visible": true },
        "compliance": { "aiGenerated": true, "arbitrationPublic": true },
    })))
}

// ---------- 投放（join） ----------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JoinRequest {
    cloud_character_id: String,
    #[serde(default)]
    boundary: Value,
}

async fn join_world(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<JoinRequest>,
) -> Result<Json<Value>, ApiError> {
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(
        &serde_json::to_vec(&json!({ "worldId": id, "body": body })).unwrap_or_default(),
    );
    let guard =
        idempotency::guard(&state.db, &user.user_id, "worlds.join", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    let world = load_world(&state.db, &id).await?;
    if !matches!(world.status.as_str(), "open" | "running") {
        return Err(ApiError::Conflict("world_not_joinable".into()));
    }

    // 角色服务端权威校验：属本人 + approved + 未撤回。
    let ch = sqlx::query(
        "SELECT owner_id, moderation, withdrawn FROM cloud_characters WHERE id = ?",
    )
    .bind(&body.cloud_character_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let owner_id: String = ch.try_get("owner_id")?;
    let moderation: String = ch.try_get("moderation")?;
    let withdrawn: i64 = ch.try_get("withdrawn")?;
    if owner_id != user.user_id {
        return Err(ApiError::Forbidden);
    }
    if moderation != "approved" {
        return Err(ApiError::Conflict("character_not_approved".into()));
    }
    if withdrawn != 0 {
        return Err(ApiError::Conflict("character_withdrawn".into()));
    }

    // 已有成员记录（唯一键 world+character）：active 直接幂等返回；left/retired 复活。
    let existing = sqlx::query(
        "SELECT id, status FROM world_members WHERE world_id = ? AND cloud_character_id = ?",
    )
    .bind(&id)
    .bind(&body.cloud_character_id)
    .fetch_optional(&state.db)
    .await?;

    let membership_id: String = if let Some(m) = existing {
        let mid: String = m.try_get("id")?;
        let mstatus: String = m.try_get("status")?;
        if mstatus != "active" {
            // 复活前校验人数上限。
            if active_member_count(&state.db, &id).await? >= world.member_limit {
                return Err(ApiError::Conflict("world_full".into()));
            }
            sqlx::query(
                "UPDATE world_members SET status='active', user_id=?, boundary_json=?, joined_at=? WHERE id=?",
            )
            .bind(&user.user_id)
            .bind(body.boundary.to_string())
            .bind(now_ms())
            .bind(&mid)
            .execute(&state.db)
            .await?;
        }
        mid
    } else {
        if active_member_count(&state.db, &id).await? >= world.member_limit {
            return Err(ApiError::Conflict("world_full".into()));
        }
        let mid = new_id("wm");
        sqlx::query(
            "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
             VALUES (?, ?, ?, ?, ?, 'active', ?)",
        )
        .bind(&mid)
        .bind(&id)
        .bind(&user.user_id)
        .bind(&body.cloud_character_id)
        .bind(body.boundary.to_string())
        .bind(now_ms())
        .execute(&state.db)
        .await?;
        mid
    };

    let response = json!({
        "membershipId": membership_id,
        "worldId": id,
        "cloudCharacterId": body.cloud_character_id,
        "status": "active",
    });
    guard.store_response(&state.db, &response.to_string()).await?;
    Ok(Json(response))
}

// ---------- 离场（leave） ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaveRequest {
    cloud_character_id: String,
}

async fn leave_world(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<LeaveRequest>,
) -> Result<Json<Value>, ApiError> {
    let res = sqlx::query(
        "UPDATE world_members SET status='left' \
         WHERE world_id=? AND cloud_character_id=? AND user_id=? AND status='active'",
    )
    .bind(&id)
    .bind(&body.cloud_character_id)
    .bind(&user.user_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    // 离场事件的叙事化在下个 tick 由 runtime 处理（仅在场成员参与回合）。
    Ok(Json(json!({ "worldId": id, "cloudCharacterId": body.cloud_character_id, "status": "left" })))
}

// ---------- 内部建房（供 admin S6 调用） ----------

/// 创建世界参数。版本字段留 None 时由 create_world 解析当前 active 版本并钉住。
/// （供 admin S6 官方建房调用；本 crate 内目前仅测试消费）
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CreateWorldParams {
    pub template_id: String,
    pub template_version: i64,
    pub room_type: String,
    pub title: String,
    pub visibility: String,
    pub host_user_id: Option<String>,
    pub member_limit: i64,
    pub tick_per_day: i64,
    pub daily_token_budget: i64,
    pub daily_cny_budget_cents: i64,
    pub status: Option<String>,
    pub engine_version: Option<String>,
    pub prompt_set_version: Option<String>,
    pub model_route_version: Option<String>,
    pub assembled_json: Option<String>,
    pub initial_state_json: Option<String>,
}

#[allow(dead_code)]
impl CreateWorldParams {
    /// 官方放置世界最小参数（其余默认）。
    pub fn official(template_id: impl Into<String>, template_version: i64, title: impl Into<String>) -> Self {
        Self {
            template_id: template_id.into(),
            template_version,
            room_type: "idle".into(),
            title: title.into(),
            visibility: "official".into(),
            host_user_id: None,
            member_limit: 10,
            tick_per_day: 3,
            daily_token_budget: 0,
            daily_cny_budget_cents: 0,
            status: None,
            engine_version: None,
            prompt_set_version: None,
            model_route_version: None,
            assembled_json: None,
            initial_state_json: None,
        }
    }
}

#[allow(dead_code)]
async fn active_version(db: &AnyPool, table: &str) -> Result<Option<String>, ApiError> {
    let sql = format!("SELECT version FROM {table} WHERE active = 1 ORDER BY created_at DESC LIMIT 1");
    let row = sqlx::query(&sql).fetch_optional(db).await?;
    Ok(match row {
        Some(r) => Some(r.try_get("version")?),
        None => None,
    })
}

/// 建房：钉住引擎/prompt/模型/模板版本，写 worlds + world_budgets。返回 world_id。
/// 官方建房由 admin S6 调用；房主建房（P4b）亦复用本函数。
#[allow(dead_code)]
pub async fn create_world(db: &AnyPool, p: CreateWorldParams) -> Result<String, ApiError> {
    let engine_version = match p.engine_version {
        Some(v) => v,
        None => muse_engine::ENGINE_VERSION.to_string(),
    };
    let prompt_set_version = match p.prompt_set_version {
        Some(v) => v,
        None => active_version(db, "prompt_versions").await?.unwrap_or_else(|| "dev-none".into()),
    };
    let model_route_version = match p.model_route_version {
        Some(v) => v,
        None => active_version(db, "model_routes").await?.unwrap_or_else(|| "dev-none".into()),
    };
    let now = now_ms();
    let id = new_id("wld");
    let status = p.status.unwrap_or_else(|| "open".into());

    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, host_user_id, member_limit, \
         tick_per_day, assembled_json, state_revision, narrative_state_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&p.template_id)
    .bind(p.template_version)
    .bind(&engine_version)
    .bind(&prompt_set_version)
    .bind(&model_route_version)
    .bind(&p.room_type)
    .bind(&p.title)
    .bind(&status)
    .bind(&p.visibility)
    .bind(&p.host_user_id)
    .bind(p.member_limit)
    .bind(p.tick_per_day)
    .bind(&p.assembled_json)
    .bind(p.initial_state_json.unwrap_or_else(|| "{}".into()))
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;

    sqlx::query(
        "INSERT INTO world_budgets (world_id, daily_token_budget, daily_cny_budget_cents, \
         spent_tokens_today, budget_day, fused, updated_at) VALUES (?, ?, ?, 0, '', 0, ?)",
    )
    .bind(&id)
    .bind(p.daily_token_budget)
    .bind(p.daily_cny_budget_cents)
    .bind(now)
    .execute(db)
    .await?;

    Ok(id)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worlds", get(list_worlds))
        .route("/worlds/{id}", get(world_detail))
        .route("/worlds/{id}/join", post(join_world))
        .route("/worlds/{id}/leave", post(leave_world))
}
