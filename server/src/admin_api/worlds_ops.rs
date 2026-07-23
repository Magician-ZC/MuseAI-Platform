//! 世界运营：活跃世界监控、脱敏卡死诊断、暂停/恢复、官方建房、模板库。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::worlds::{create_world as create_world_inner, load_world, CreateWorldParams};

use super::{audit, clamp_limit, parse_cursor, require_role, ActionQuery};

// ---------------- 世界监控列表 ----------------

#[derive(Debug, Deserialize)]
pub(super) struct WorldListQuery {
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// GET /admin/worlds?status=&cursor=：全量世界监控（含预算/熔断态；不限可见性）。
pub(super) async fn list_worlds(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<WorldListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let page = clamp_limit(q.limit);
    let mut sql = String::from(
        "SELECT w.id, w.title, w.room_type, w.status, w.visibility, w.member_limit, \
         w.tick_per_day, w.template_id, w.template_version, w.engine_version, w.prompt_set_version, \
         w.model_route_version, w.state_revision, w.created_at, \
         COALESCE(b.spent_tokens_today, 0) AS spent_tokens_today, \
         COALESCE(b.daily_token_budget, 0) AS daily_token_budget, \
         COALESCE(b.fused, 0) AS fused \
         FROM worlds w LEFT JOIN world_budgets b ON b.world_id = w.id WHERE 1=1",
    );
    if q.status.is_some() {
        sql.push_str(" AND w.status = ?");
    }
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (w.created_at < ? OR (w.created_at = ? AND w.id < ?))");
    }
    sql.push_str(" ORDER BY w.created_at DESC, w.id DESC LIMIT ?");

    let mut query = sqlx::query(&sql);
    if let Some(s) = &q.status {
        query = query.bind(s);
    }
    if let Some((ts, id)) = &cursor {
        query = query.bind(*ts).bind(*ts).bind(id);
    }
    query = query.bind(page + 1);

    let rows = query.fetch_all(&state.db).await?;
    let has_more = rows.len() as i64 > page;
    let mut items = Vec::new();
    let mut next_cursor: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        if i as i64 >= page {
            break;
        }
        let id: String = row.try_get("id")?;
        let created_at: i64 = row.try_get("created_at")?;
        next_cursor = Some(format!("{created_at}:{id}"));
        items.push(json!({
            "id": id,
            "title": row.try_get::<String, _>("title")?,
            "roomType": row.try_get::<String, _>("room_type")?,
            "status": row.try_get::<String, _>("status")?,
            "visibility": row.try_get::<String, _>("visibility")?,
            "memberLimit": row.try_get::<i64, _>("member_limit")?,
            "tickPerDay": row.try_get::<i64, _>("tick_per_day")?,
            "templateId": row.try_get::<String, _>("template_id")?,
            "templateVersion": row.try_get::<i64, _>("template_version")?,
            "engineVersion": row.try_get::<String, _>("engine_version")?,
            "promptSetVersion": row.try_get::<String, _>("prompt_set_version")?,
            "modelRouteVersion": row.try_get::<String, _>("model_route_version")?,
            "stateRevision": row.try_get::<i64, _>("state_revision")?,
            "spentTokensToday": row.try_get::<i64, _>("spent_tokens_today")?,
            "dailyTokenBudget": row.try_get::<i64, _>("daily_token_budget")?,
            "fused": row.try_get::<i64, _>("fused")? != 0,
            "createdAt": created_at,
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "worlds": items, "nextCursor": next_cursor })))
}

// ---------------- 脱敏诊断 ----------------

/// GET /admin/worlds/{id}/diagnostics：卡死诊断视图。
/// 脱敏：只出调用元数据/tick 错误码/预算/规则命中(风控计数)/事件审核态，
/// 不返回任何私密叙事内容（public/private 投影一律不暴露，§10）。
pub(super) async fn diagnostics(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let world = load_world(&state.db, &id).await?; // 不存在 → NotFound

    // 最近 10 个 tick 的元数据（含错误码），不含叙事产物。
    let tick_rows = sqlx::query(
        "SELECT tick_no, status, error, cost_tokens, started_at, finished_at, created_at \
         FROM world_ticks WHERE world_id = ? ORDER BY tick_no DESC LIMIT 10",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut ticks = Vec::new();
    for r in &tick_rows {
        ticks.push(json!({
            "tickNo": r.try_get::<i64, _>("tick_no")?,
            "status": r.try_get::<String, _>("status")?,
            "error": r.try_get::<Option<String>, _>("error")?,
            "costTokens": r.try_get::<i64, _>("cost_tokens")?,
            "startedAt": r.try_get::<Option<i64>, _>("started_at")?,
            "finishedAt": r.try_get::<Option<i64>, _>("finished_at")?,
            "createdAt": r.try_get::<i64, _>("created_at")?,
        }));
    }

    // 预算/熔断态。
    let budget = sqlx::query(
        "SELECT daily_token_budget, daily_cny_budget_cents, spent_tokens_today, budget_day, fused \
         FROM world_budgets WHERE world_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;
    let budget_json = match budget {
        Some(b) => json!({
            "dailyTokenBudget": b.try_get::<i64, _>("daily_token_budget")?,
            "dailyCnyBudgetCents": b.try_get::<i64, _>("daily_cny_budget_cents")?,
            "spentTokensToday": b.try_get::<i64, _>("spent_tokens_today")?,
            "budgetDay": b.try_get::<String, _>("budget_day")?,
            "fused": b.try_get::<i64, _>("fused")? != 0,
        }),
        None => Value::Null,
    };

    // 规则命中：本世界风控事件按 kind 聚合计数（不出 detail_json 内容）。
    let risk_rows = sqlx::query(
        "SELECT kind, COUNT(*) AS n FROM risk_events WHERE world_id = ? GROUP BY kind",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut risk_counts = Vec::new();
    for r in &risk_rows {
        risk_counts.push(json!({
            "kind": r.try_get::<String, _>("kind")?,
            "count": r.try_get::<i64, _>("n")?,
        }));
    }

    // 事件审核态计数（仅数量，不含投影内容）。
    let ev_rows = sqlx::query(
        "SELECT moderation, COUNT(*) AS n FROM world_events WHERE world_id = ? GROUP BY moderation",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut ev_by_moderation = Vec::new();
    let mut ev_total = 0i64;
    for r in &ev_rows {
        let n: i64 = r.try_get("n")?;
        ev_total += n;
        ev_by_moderation.push(json!({
            "moderation": r.try_get::<String, _>("moderation")?,
            "count": n,
        }));
    }

    Ok(Json(json!({
        "world": {
            "id": world.id,
            "title": world.title,
            "status": world.status,
            "visibility": world.visibility,
            "roomType": world.room_type,
            "stateRevision": world.state_revision,
            "engineVersion": world.engine_version,
            "promptSetVersion": world.prompt_set_version,
            "modelRouteVersion": world.model_route_version,
            "templateId": world.template_id,
            "templateVersion": world.template_version,
        },
        "ticks": ticks,
        "budget": budget_json,
        "riskEventCounts": risk_counts,
        "eventStats": { "total": ev_total, "byModeration": ev_by_moderation },
        "redactionNote": "诊断视图脱敏：不含私密叙事/投影内容；查看必要内容需另行授权（§10）。",
    })))
}

// ---------------- 暂停 / 恢复 ----------------

/// POST /admin/worlds/{id}/pause?reason=
pub(super) async fn pause(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let world = load_world(&state.db, &id).await?;
    if !matches!(world.status.as_str(), "open" | "running") {
        return Err(ApiError::Conflict("world_not_pausable".into()));
    }
    set_world_status(&state, &admin.0, &id, "paused", "world.pause", q.reason()).await
}

/// POST /admin/worlds/{id}/resume?reason=（paused → running，恢复 tick 调度）。
pub(super) async fn resume(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let world = load_world(&state.db, &id).await?;
    if world.status != "paused" {
        return Err(ApiError::Conflict("world_not_paused".into()));
    }
    set_world_status(&state, &admin.0, &id, "running", "world.resume", q.reason()).await
}

async fn set_world_status(
    state: &AppState,
    actor: &AuthUser,
    id: &str,
    status: &str,
    action: &str,
    reason: &str,
) -> Result<Json<Value>, ApiError> {
    sqlx::query("UPDATE worlds SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(now_ms())
        .bind(id)
        .execute(&state.db)
        .await?;
    audit(&state.db, actor, action, id, reason).await?;
    Ok(Json(json!({ "id": id, "status": status })))
}

// ---------------- 官方建房 ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateWorldReq {
    template_id: String,
    #[serde(default = "default_template_version")]
    template_version: i64,
    title: String,
    room_type: Option<String>,
    visibility: Option<String>,
    member_limit: Option<i64>,
    tick_per_day: Option<i64>,
    daily_token_budget: Option<i64>,
    daily_cny_budget_cents: Option<i64>,
    status: Option<String>,
}

fn default_template_version() -> i64 {
    1
}

/// POST /admin/worlds：官方放置世界。调 worlds::create_world 建房（钉住引擎/prompt/模型/模板版本 + 预算）。
pub(super) async fn create_world(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(req): Json<CreateWorldReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    if req.title.trim().is_empty() || req.template_id.trim().is_empty() {
        return Err(ApiError::BadRequest("title 与 templateId 必填".into()));
    }
    let mut p = CreateWorldParams::official(req.template_id, req.template_version, req.title);
    if let Some(rt) = req.room_type {
        if !matches!(rt.as_str(), "idle" | "chapter" | "arena") {
            return Err(ApiError::BadRequest("roomType 非法".into()));
        }
        p.room_type = rt;
    }
    if let Some(v) = req.visibility {
        // 枚举校验（对齐 worlds.visibility 约定），避免自由文本落库污染大厅可见性过滤。
        if !matches!(v.as_str(), "official" | "public" | "private") {
            return Err(ApiError::BadRequest("visibility 非法".into()));
        }
        p.visibility = v;
    }
    if let Some(m) = req.member_limit {
        p.member_limit = m;
    }
    if let Some(t) = req.tick_per_day {
        p.tick_per_day = t;
    }
    if let Some(b) = req.daily_token_budget {
        p.daily_token_budget = b;
    }
    if let Some(c) = req.daily_cny_budget_cents {
        p.daily_cny_budget_cents = c;
    }
    if let Some(s) = req.status {
        // 建房仅允许起始态（open/running）；paused/ended 非法（避免建出不可调度的僵尸房）。
        if !matches!(s.as_str(), "open" | "running") {
            return Err(ApiError::BadRequest("status 非法（建房仅允许 open/running）".into()));
        }
        p.status = Some(s);
    }

    let world_id = create_world_inner(&state.db, p).await?;
    audit(&state.db, &admin.0, "world.create", &world_id, "official world").await?;
    Ok(Json(json!({ "worldId": world_id })))
}

// ---------------- 世界模板库 ----------------

#[derive(Debug, Deserialize)]
pub(super) struct TemplateListQuery {
    moderation: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// GET /admin/world-templates?moderation=&cursor=
pub(super) async fn list_templates(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<TemplateListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator", "reviewer"])?;
    let page = clamp_limit(q.limit);
    let mut sql = String::from(
        "SELECT id, title, room_type, skeleton_json, admission_json, official, version, \
         moderation, created_at FROM world_templates WHERE 1=1",
    );
    if q.moderation.is_some() {
        sql.push_str(" AND moderation = ?");
    }
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

    let mut query = sqlx::query(&sql);
    if let Some(m) = &q.moderation {
        query = query.bind(m);
    }
    if let Some((ts, id)) = &cursor {
        query = query.bind(*ts).bind(*ts).bind(id);
    }
    query = query.bind(page + 1);

    let rows = query.fetch_all(&state.db).await?;
    let has_more = rows.len() as i64 > page;
    let mut items = Vec::new();
    let mut next_cursor: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        if i as i64 >= page {
            break;
        }
        let id: String = row.try_get("id")?;
        let created_at: i64 = row.try_get("created_at")?;
        next_cursor = Some(format!("{created_at}:{id}"));
        let skeleton_raw: String = row.try_get("skeleton_json")?;
        let admission_raw: String = row.try_get("admission_json")?;
        items.push(json!({
            "id": id,
            "title": row.try_get::<String, _>("title")?,
            "roomType": row.try_get::<String, _>("room_type")?,
            "skeletonJson": serde_json::from_str::<Value>(&skeleton_raw).unwrap_or(Value::Null),
            "admissionJson": serde_json::from_str::<Value>(&admission_raw).unwrap_or(Value::Null),
            "official": row.try_get::<i64, _>("official")? != 0,
            "version": row.try_get::<i64, _>("version")?,
            "moderation": row.try_get::<String, _>("moderation")?,
            "createdAt": created_at,
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "templates": items, "nextCursor": next_cursor })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateTemplateReq {
    title: String,
    room_type: String,
    skeleton_json: Value,
    admission_json: Option<Value>,
}

/// POST /admin/world-templates：新建模板（skeleton_json 结构校验 + 进入审核态/审核队列）。
pub(super) async fn create_template(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(req): Json<CreateTemplateReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    if req.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title 必填".into()));
    }
    if !matches!(req.room_type.as_str(), "idle" | "chapter" | "arena") {
        return Err(ApiError::BadRequest("roomType 非法".into()));
    }
    // skeleton_json 校验：必须为对象（主线硬节点/结局池/隐藏内容池/装配规则的容器）。
    if !req.skeleton_json.is_object() {
        return Err(ApiError::BadRequest("skeletonJson 必须是 JSON 对象".into()));
    }
    let admission = req.admission_json.unwrap_or_else(|| json!({ "mode": "open" }));
    if !admission.is_object() {
        return Err(ApiError::BadRequest("admissionJson 必须是 JSON 对象".into()));
    }

    let id = new_id("tpl");
    let now = now_ms();
    // 新模板进入待审核态（官方模板亦走审核工作台）。
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, \
         official, version, moderation, created_at) VALUES (?, ?, ?, ?, ?, 1, 1, 'pending', ?)",
    )
    .bind(&id)
    .bind(req.title.trim())
    .bind(&req.room_type)
    .bind(req.skeleton_json.to_string())
    .bind(admission.to_string())
    .bind(now)
    .execute(&state.db)
    .await?;

    // 登记到审核队列，供审核工作台 approve/reject（回写 world_templates.moderation）。
    sqlx::query(
        "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, machine_hits, \
         status, created_at) VALUES (?, 'template', ?, 'pending', '[]', 'open', ?)",
    )
    .bind(new_id("aq"))
    .bind(&id)
    .bind(now)
    .execute(&state.db)
    .await?;

    audit(&state.db, &admin.0, "template.create", &id, "").await?;
    Ok(Json(json!({ "templateId": id, "moderation": "pending" })))
}
