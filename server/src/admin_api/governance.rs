//! 模型与 Prompt 治理：Prompt 版本化 / 灰度（canary）/ 一键回滚（激活即互斥切换）；模型路由版本化 + 激活。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;

use super::{audit, require_role, ActionQuery};

/// Prompt 作用域（对应 muse-engine 各环节，migration 注释）。
const PROMPT_SCOPES: &[&str] = &["director", "decide", "arbiter", "writer", "critic", "report"];

// ---------------- Prompt 版本化 ----------------

#[derive(Debug, Deserialize)]
pub(super) struct PromptListQuery {
    scope: Option<String>,
}

/// GET /admin/prompts?scope=：列出各场景 prompt 版本（含 content 供 diff 视图 + 灰度名单）。
pub(super) async fn list_prompts(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<PromptListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let mut sql = String::from(
        "SELECT id, scope, version, content, active, canary_world_ids, created_at \
         FROM prompt_versions WHERE 1=1",
    );
    if q.scope.is_some() {
        sql.push_str(" AND scope = ?");
    }
    sql.push_str(" ORDER BY scope ASC, created_at DESC");

    let mut query = sqlx::query(&sql);
    if let Some(s) = &q.scope {
        query = query.bind(s);
    }
    let rows = query.fetch_all(&state.db).await?;
    let mut items = Vec::new();
    for row in &rows {
        let canary_raw: String = row.try_get("canary_world_ids")?;
        items.push(json!({
            "id": row.try_get::<String, _>("id")?,
            "scope": row.try_get::<String, _>("scope")?,
            "version": row.try_get::<String, _>("version")?,
            "content": row.try_get::<String, _>("content")?,
            "active": row.try_get::<i64, _>("active")? != 0,
            "canaryWorldIds": serde_json::from_str::<Value>(&canary_raw).unwrap_or_else(|_| json!([])),
            "createdAt": row.try_get::<i64, _>("created_at")?,
        }));
    }
    Ok(Json(json!({ "prompts": items })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreatePromptReq {
    scope: String,
    version: String,
    content: String,
}

/// POST /admin/prompts：登记新 prompt 版本（默认 inactive；不影响线上）。
pub(super) async fn create_prompt(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(req): Json<CreatePromptReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &[])?; // 治理写操作 admin 专属。
    if !PROMPT_SCOPES.contains(&req.scope.as_str()) {
        return Err(ApiError::BadRequest("scope 非法".into()));
    }
    if req.version.trim().is_empty() {
        return Err(ApiError::BadRequest("version 必填".into()));
    }
    let id = new_id("pv");
    sqlx::query(
        "INSERT INTO prompt_versions (id, scope, version, content, active, canary_world_ids, created_at) \
         VALUES (?, ?, ?, ?, 0, '[]', ?)",
    )
    .bind(&id)
    .bind(&req.scope)
    .bind(&req.version)
    .bind(&req.content)
    .bind(now_ms())
    .execute(&state.db)
    .await?;
    audit(
        &state.db,
        &admin.0,
        "prompt.create",
        &id,
        &format!("{}@{}", req.scope, req.version),
    )
    .await?;
    Ok(Json(json!({ "id": id, "scope": req.scope, "version": req.version, "active": false })))
}

/// POST /admin/prompts/{id}/activate?reason=：激活该版本，同 scope 其余置 inactive（互斥）。
/// 一键回滚 = 激活某个旧版本。
pub(super) async fn activate_prompt(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &[])?; // 治理写操作 admin 专属。
    let row = sqlx::query("SELECT scope, version FROM prompt_versions WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let scope: String = row.try_get("scope")?;
    let version: String = row.try_get("version")?;

    // 互斥激活：单语句原子切换（同 scope 内目标置 active，其余置 inactive），
    // 避免「先全 inactive 再激活」两步之间读到零 active 的窗口。CASE 表达式双库可移植。
    sqlx::query("UPDATE prompt_versions SET active = CASE WHEN id = ? THEN 1 ELSE 0 END WHERE scope = ?")
        .bind(&id)
        .bind(&scope)
        .execute(&state.db)
        .await?;

    audit(
        &state.db,
        &admin.0,
        "prompt.activate",
        &id,
        &format!("scope={scope} version={version} {}", q.reason()),
    )
    .await?;
    Ok(Json(json!({ "id": id, "scope": scope, "version": version, "active": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CanaryReq {
    #[serde(default)]
    world_ids: Vec<String>,
}

/// POST /admin/prompts/{id}/canary：写 canary_world_ids（按世界灰度）。
pub(super) async fn canary_prompt(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(req): Json<CanaryReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &[])?; // 治理写操作 admin 专属。
    let exists = sqlx::query("SELECT 1 AS x FROM prompt_versions WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .is_some();
    if !exists {
        return Err(ApiError::NotFound);
    }
    let ids_json = serde_json::to_string(&req.world_ids).unwrap_or_else(|_| "[]".into());
    sqlx::query("UPDATE prompt_versions SET canary_world_ids = ? WHERE id = ?")
        .bind(&ids_json)
        .bind(&id)
        .execute(&state.db)
        .await?;
    audit(
        &state.db,
        &admin.0,
        "prompt.canary",
        &id,
        &format!("{} worlds", req.world_ids.len()),
    )
    .await?;
    Ok(Json(json!({ "id": id, "canaryWorldIds": req.world_ids })))
}

// ---------------- 模型路由 ----------------

/// GET /admin/model-routes：列出模型路由版本（stage→ModelProfile 映射）。
pub(super) async fn list_routes(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let rows = sqlx::query(
        "SELECT id, version, routes_json, active, created_at FROM model_routes \
         ORDER BY created_at DESC",
    )
    .fetch_all(&state.db)
    .await?;
    let mut items = Vec::new();
    for row in &rows {
        let routes_raw: String = row.try_get("routes_json")?;
        items.push(json!({
            "id": row.try_get::<String, _>("id")?,
            "version": row.try_get::<String, _>("version")?,
            "routesJson": serde_json::from_str::<Value>(&routes_raw).unwrap_or(Value::Null),
            "active": row.try_get::<i64, _>("active")? != 0,
            "createdAt": row.try_get::<i64, _>("created_at")?,
        }));
    }
    Ok(Json(json!({ "modelRoutes": items })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateRouteReq {
    version: String,
    routes_json: Value,
}

/// POST /admin/model-routes：登记新模型路由版本（默认 inactive）。
pub(super) async fn create_route(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(req): Json<CreateRouteReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &[])?; // 治理写操作 admin 专属。
    if req.version.trim().is_empty() {
        return Err(ApiError::BadRequest("version 必填".into()));
    }
    if !req.routes_json.is_object() {
        return Err(ApiError::BadRequest("routesJson 必须是 JSON 对象".into()));
    }
    let id = new_id("mr");
    sqlx::query(
        "INSERT INTO model_routes (id, version, routes_json, active, created_at) VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&id)
    .bind(&req.version)
    .bind(req.routes_json.to_string())
    .bind(now_ms())
    .execute(&state.db)
    .await?;
    audit(&state.db, &admin.0, "model_route.create", &id, &req.version).await?;
    Ok(Json(json!({ "id": id, "version": req.version, "active": false })))
}

/// POST /admin/model-routes/{id}/activate?reason=：激活该路由版本，其余置 inactive（互斥）。
/// 一键回滚 = 激活旧版本。
pub(super) async fn activate_route(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &[])?; // 治理写操作 admin 专属。
    let row = sqlx::query("SELECT version FROM model_routes WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let version: String = row.try_get("version")?;

    // 全局单活跃路由：单语句原子切换（目标置 active，其余全 inactive），消除零 active 窗口。
    sqlx::query("UPDATE model_routes SET active = CASE WHEN id = ? THEN 1 ELSE 0 END")
        .bind(&id)
        .execute(&state.db)
        .await?;

    audit(
        &state.db,
        &admin.0,
        "model_route.activate",
        &id,
        &format!("version={version} {}", q.reason()),
    )
    .await?;
    Ok(Json(json!({ "id": id, "version": version, "active": true })))
}
