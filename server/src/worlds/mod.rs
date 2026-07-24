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
use sqlx::{Any, AnyPool, Row, Transaction};

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
    pub narrative_state_json: String,
    /// 开局装配结果（钉住）：runtime 首 tick 从中提取硬节点/禁止谓词种子（E-1）。
    pub assembled_json: Option<String>,
    /// 时间线模式（第二块 Phase 2）：'interval'（默认，老世界墙钟固定间隔→run_round）
    /// 或 'event'（放置房 DES：背靠背→run_event_step 调度）。世界级渐进闸。
    pub timeline_mode: String,
    /// 世界游戏时钟快照（= NarrativeState.timeline.now，第二块 Phase 2）：commit_tick 每步回写。
    /// interval 世界恒为 0（不推进时钟）。Phase 2 仅回写、暂无读取方（调度器 T 由引擎从 FS 状态自算），
    /// 保留供后续 Phase/展示层读「当前游戏时刻」而不必反序列化整份 narrative_state_json。
    #[allow(dead_code)]
    pub game_time: i64,
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
        assembled_json: row.try_get("assembled_json")?,
        timeline_mode: row.try_get("timeline_mode")?,
        game_time: row.try_get("game_time")?,
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

    // 公开阵容：active 成员 + 角色公开名（AI 标识）+ 头像（仅过审才带）。
    let member_rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, cc.card_json AS card, \
         cc.avatar_url AS avatar_url, cc.avatar_moderation AS avatar_moderation \
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
        let mut item = json!({ "cloudCharacterId": cid, "name": name, "aiLabel": { "visible": true } });
        // 红线：仅头像机审 approved 才带 avatarUrl，否则不带该字段（前端回退首字头像）。
        let avatar_url: Option<String> = r.try_get("avatar_url")?;
        let avatar_moderation: Option<String> = r.try_get("avatar_moderation")?;
        if avatar_moderation.as_deref() == Some("approved") {
            if let Some(url) = avatar_url {
                item["avatarUrl"] = json!(url);
            }
        }
        roster.push(item);
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

    // C-4：人数上限原子化。旧实现是 count→check→insert 的 TOCTOU（唯一索引只挡同角色重复，挡不住并发凑满）。
    // 改为「带人数子查询守卫的条件写」：limit 判定与写入在同一条语句里求值，rows_affected==0 即满员。
    // （sqlite 语句级原子；postgres 同快照下将 TOCTOU 窗口收敛到单语句，配合唯一索引把越额上限收敛到并发不同角色数。）
    let membership_id: String = if let Some(m) = existing {
        let mid: String = m.try_get("id")?;
        let mstatus: String = m.try_get("status")?;
        if mstatus != "active" {
            // 复活：仅当仍有空位时置 active（人数守卫内嵌）；已满 → world_full。
            let res = sqlx::query(
                "UPDATE world_members SET status='active', user_id=?, boundary_json=?, joined_at=? \
                 WHERE id=? AND status != 'active' \
                 AND (SELECT COUNT(*) FROM world_members WHERE world_id=? AND status='active') < ?",
            )
            .bind(&user.user_id)
            .bind(body.boundary.to_string())
            .bind(now_ms())
            .bind(&mid)
            .bind(&id)
            .bind(world.member_limit)
            .execute(&state.db)
            .await?;
            if res.rows_affected() == 0 {
                return Err(ApiError::Conflict("world_full".into()));
            }
        }
        // 已 active：幂等，无需再判上限。
        mid
    } else {
        let mid = new_id("wm");
        // 条件插入：仅当活跃人数 < 上限时落一行（SELECT 常量 + WHERE 子查询守卫）。
        let res = sqlx::query(
            "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
             SELECT ?, ?, ?, ?, ?, 'active', ? \
             WHERE (SELECT COUNT(*) FROM world_members WHERE world_id=? AND status='active') < ?",
        )
        .bind(&mid)
        .bind(&id)
        .bind(&user.user_id)
        .bind(&body.cloud_character_id)
        .bind(body.boundary.to_string())
        .bind(now_ms())
        .bind(&id)
        .bind(world.member_limit)
        .execute(&state.db)
        .await;
        match res {
            Ok(r) if r.rows_affected() == 0 => return Err(ApiError::Conflict("world_full".into())),
            Ok(_) => {}
            // 并发下同角色抢插：唯一索引兜底 → 视为已在场（幂等成功）。
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {}
            Err(e) => return Err(e.into()),
        }
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
    /// 时间线模式：'interval'（默认）或 'event'（放置房 DES）。落 worlds.timeline_mode 列，供调度分派。
    pub timeline_mode: String,
    pub engine_version: Option<String>,
    pub prompt_set_version: Option<String>,
    pub model_route_version: Option<String>,
    pub assembled_json: Option<String>,
    pub initial_state_json: Option<String>,
}

#[allow(dead_code)]
impl CreateWorldParams {
    /// 官方放置世界最小参数（其余默认）。
    /// B-2：官方建房必须带非零 token 预算 + 非零 cny 上限——否则 world_budgets 视为无上限（成本失控）。
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
            // 非零默认预算（daily_token_budget=0 会被 runtime 当作"无上限"）：给官方房一个保守的日 token 上限
            // 与 cny 熔断维度。运营可在 admin 建房时覆盖为具体额度。
            daily_token_budget: 200_000,
            daily_cny_budget_cents: 2_000,
            status: None,
            timeline_mode: "interval".into(),
            engine_version: None,
            prompt_set_version: None,
            model_route_version: None,
            assembled_json: None,
            initial_state_json: None,
        }
    }
}

#[allow(dead_code)]
async fn active_version_tx(tx: &mut Transaction<'_, Any>, table: &str) -> Result<Option<String>, ApiError> {
    let sql = format!("SELECT version FROM {table} WHERE active = 1 ORDER BY created_at DESC LIMIT 1");
    let row = sqlx::query(&sql).fetch_optional(&mut **tx).await?;
    Ok(match row {
        Some(r) => Some(r.try_get("version")?),
        None => None,
    })
}

/// 建房（事务版）：钉住引擎/prompt/模型/模板版本，写 worlds + world_budgets，返回 world_id。
/// **在调用方已开启的事务内执行**——P4b 房主建房把它与开房费 `ledger::charge` 组进同一 tx，
/// charge 失败即随 tx 回滚（零副作用，无 world/budget/journal 残留）；charge 的 resolve_share 需 world 已在 tx 内落库，
/// 故建房必须先于 charge。官方建房经下面的 `create_world` 薄封装（自开自提交 tx）。
#[allow(dead_code)]
pub async fn create_world_tx(tx: &mut Transaction<'_, Any>, p: CreateWorldParams) -> Result<String, ApiError> {
    let engine_version = match p.engine_version {
        Some(v) => v,
        None => muse_engine::ENGINE_VERSION.to_string(),
    };
    let prompt_set_version = match p.prompt_set_version {
        Some(v) => v,
        None => active_version_tx(tx, "prompt_versions").await?.unwrap_or_else(|| "dev-none".into()),
    };
    let model_route_version = match p.model_route_version {
        Some(v) => v,
        None => active_version_tx(tx, "model_routes").await?.unwrap_or_else(|| "dev-none".into()),
    };
    let now = now_ms();
    let id = new_id("wld");
    let status = p.status.unwrap_or_else(|| "open".into());
    // 防御式归一化：admin 入口已做枚举校验，但 P4b 房主建房亦复用；非法值兜底为 interval，
    // 保证落库的 timeline_mode 恒为调度器可分派的合法枚举（interval/event）。
    let timeline_mode = if matches!(p.timeline_mode.as_str(), "interval" | "event") {
        p.timeline_mode.as_str()
    } else {
        "interval"
    };

    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, host_user_id, member_limit, \
         tick_per_day, timeline_mode, assembled_json, state_revision, narrative_state_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?)",
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
    .bind(timeline_mode)
    .bind(&p.assembled_json)
    .bind(p.initial_state_json.unwrap_or_else(|| "{}".into()))
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO world_budgets (world_id, daily_token_budget, daily_cny_budget_cents, \
         spent_tokens_today, budget_day, fused, updated_at) VALUES (?, ?, ?, 0, '', 0, ?)",
    )
    .bind(&id)
    .bind(p.daily_token_budget)
    .bind(p.daily_cny_budget_cents)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    Ok(id)
}

/// 建房（薄封装）：自开自提交事务调 `create_world_tx`。官方建房（admin S6）及 test 复用本签名。
/// 房主建房走 `POST /worlds` 的 `create_room`（把建房 + 开房费 charge 组进同一 tx，不走此封装）。
#[allow(dead_code)]
pub async fn create_world(db: &AnyPool, p: CreateWorldParams) -> Result<String, ApiError> {
    let mut tx = db.begin().await?;
    let id = create_world_tx(&mut tx, p).await?;
    tx.commit().await?;
    Ok(id)
}

pub fn router() -> Router<AppState> {
    // 房主建房 POST /worlds 携开房费 charge（P4b），依赖 `ledger`（feature=billing/arena 才装配）；
    // 无经济 feature 时不暴露该端点（GET /worlds 大厅列表恒在）。feature 一致，见 app.rs / ledger 门控。
    #[cfg(any(feature = "billing", feature = "arena"))]
    let worlds_route = get(list_worlds).post(create_room);
    #[cfg(not(any(feature = "billing", feature = "arena")))]
    let worlds_route = get(list_worlds);

    Router::new()
        .route("/worlds", worlds_route)
        .route("/worlds/{id}", get(world_detail))
        .route("/worlds/{id}/join", post(join_world))
        .route("/worlds/{id}/leave", post(leave_world))
}

// ---------- 房主建房（POST /worlds）+ 开房费 charge（P4b/P2，feature=billing/arena） ----------

/// 房主建房请求。`templateId` 必填（用哪个模板建房，决定 room_type/版本/开房费/分成对手方）；
/// `visibility` 仅 public/private（official 是运营专属，房主不可自建官方房）；其余留空取默认。
#[cfg(any(feature = "billing", feature = "arena"))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateRoomReq {
    template_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    member_limit: Option<i64>,
    #[serde(default)]
    tick_per_day: Option<i64>,
}

/// POST /worlds：房主建房 + 开房费扣费（单事务，账本红线集中在 ledger::charge）。
///
/// 流程：模板校验（存在 + approved + 未撤回，读 owner/room_type/版本/开房费）→ 幂等 guard →
///   开事务 → `create_world_tx`（先建房，charge 分成溯源需 world 已落库）→
///   `ledger::charge(host, 开房费, "room_open", world_id=Some(新世界))`（分成给模板 owner；
///   自打赏防刷/未成年 owner 挂平台/取整余数归平台/SUM=0 全在 charge 内守；余额不足 409 → tx 回滚零副作用）→ 提交。
///
/// 红线：
/// - 建房**不设年龄硬门**（建房 ≠ 充值；但消费余额只能来自已 age-gate 的充值 → 未成年余额恒 0 →
///   开房费 > 0 时必然余额不足 409；免费房 room_open_price==0 走 charge no-op 仍可建）。
/// - 分成认 **template.owner_id**（创作者），非 worlds.host_user_id（房主）；官方模板 owner NULL → 全额平台。
/// - 免费房（开房费 0）保留：charge no-op 不产 journal。
#[cfg(any(feature = "billing", feature = "arena"))]
async fn create_room(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<CreateRoomReq>,
) -> Result<Json<Value>, ApiError> {
    // 模板校验（读只在 pool 上，先于 tx；释放连接后再 begin，单连接池不自锁）。
    let tpl = sqlx::query(
        "SELECT title, room_type, version, moderation, COALESCE(withdrawn, 0) AS withdrawn, \
         COALESCE(room_open_price_cents, 0) AS price FROM world_templates WHERE id = ?",
    )
    .bind(&body.template_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let moderation: String = tpl.try_get("moderation")?;
    let withdrawn: i64 = tpl.try_get("withdrawn")?;
    if moderation != "approved" {
        return Err(ApiError::Conflict("template_not_approved".into()));
    }
    if withdrawn != 0 {
        return Err(ApiError::Conflict("template_withdrawn".into()));
    }
    let tpl_title: String = tpl.try_get("title")?;
    let room_type: String = tpl.try_get("room_type")?;
    let template_version: i64 = tpl.try_get("version")?;
    let room_open_price: i64 = tpl.try_get("price")?;

    // 房主建房可见性仅 public/private（official 运营专属）。缺省 private。
    let visibility = match body.visibility.as_deref() {
        Some("public") => "public",
        None | Some("private") => "private",
        Some(_) => return Err(ApiError::BadRequest("visibility 仅支持 public/private".into())),
    };
    let title = match &body.title {
        Some(t) if !t.trim().is_empty() => t.clone(),
        _ => tpl_title,
    };
    let member_limit = body.member_limit.unwrap_or(10).clamp(1, 100);
    let tick_per_day = body.tick_per_day.unwrap_or(3).clamp(1, 100);

    // 幂等：同 key 同载荷 → 缓存返回（不双扣不双建）。
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&body).unwrap_or_default());
    let guard = idempotency::guard(&state.db, &user.user_id, "worlds.create", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    let params = CreateWorldParams {
        template_id: body.template_id.clone(),
        template_version,
        room_type: room_type.clone(),
        title,
        visibility: visibility.into(),
        host_user_id: Some(user.user_id.clone()),
        member_limit,
        tick_per_day,
        // 房主房沿用保守默认预算（B-2：非零 token/cny 上限，避免成本失控）。
        daily_token_budget: 200_000,
        daily_cny_budget_cents: 2_000,
        status: None,
        timeline_mode: "interval".into(),
        engine_version: None,
        prompt_set_version: None,
        model_route_version: None,
        assembled_json: None,
        initial_state_json: None,
    };

    // 单事务：建房 + 开房费 charge 原子。先建房（charge 溯源分成需 world 已在 tx 内），再 charge。
    let mut tx = state.db.begin().await?;
    let world_id = create_world_tx(&mut tx, params).await?;
    let receipt = crate::ledger::charge(
        &mut tx,
        &user.user_id,
        room_open_price,
        "room_open",
        "world",
        &world_id,
        Some(&world_id),
    )
    .await?;
    tx.commit().await?;

    let resp = json!({
        "worldId": world_id,
        "templateId": body.template_id,
        "roomType": room_type,
        "visibility": visibility,
        "hostUserId": user.user_id,
        "roomOpenPriceCents": room_open_price,
        // 开房费分账明细（诚实标注）：创作者分成 + 平台抽成（自打赏/官方模板/未成年 owner → 创作者 0）。
        "charge": {
            "chargedCents": receipt.charged_cents,
            "creatorEarningsCents": receipt.creator_earnings_cents,
            "platformRevenueCents": receipt.platform_revenue_cents,
        },
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}
