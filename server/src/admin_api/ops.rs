//! 风控（risk_events 检索）+ 客服工单（data_requests 检索/执行占位）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::db::now_ms;
use crate::error::ApiError;

use super::{audit, clamp_limit, parse_cursor, require_role, ActionQuery};

// ---------------- 风控 ----------------

#[derive(Debug, Deserialize)]
pub(super) struct RiskQuery {
    kind: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// GET /admin/risk-events?kind=&cursor=：风险事件流（注入/伪造状态/越权/滥用等）。
pub(super) async fn list_risk_events(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<RiskQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator", "reviewer", "support"])?;
    let page = clamp_limit(q.limit);
    let mut sql = String::from(
        "SELECT id, user_id, world_id, kind, detail_json, created_at FROM risk_events WHERE 1=1",
    );
    if q.kind.is_some() {
        sql.push_str(" AND kind = ?");
    }
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

    let mut query = sqlx::query(&sql);
    if let Some(k) = &q.kind {
        query = query.bind(k);
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
        let detail_raw: String = row.try_get("detail_json")?;
        items.push(json!({
            "id": id,
            "userId": row.try_get::<Option<String>, _>("user_id")?,
            "worldId": row.try_get::<Option<String>, _>("world_id")?,
            "kind": row.try_get::<String, _>("kind")?,
            "detail": serde_json::from_str::<Value>(&detail_raw).unwrap_or(Value::Null),
            "createdAt": created_at,
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "events": items, "nextCursor": next_cursor })))
}

// ---------------- 工单（数据导出/删除请求） ----------------

#[derive(Debug, Deserialize)]
pub(super) struct DataReqQuery {
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// GET /admin/data-requests?status=&cursor=：数据导出/删除工单列表。
pub(super) async fn list_data_requests(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<DataReqQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["support"])?;
    let page = clamp_limit(q.limit);
    let mut sql = String::from(
        "SELECT id, user_id, kind, status, result_key, created_at, updated_at \
         FROM data_requests WHERE 1=1",
    );
    if q.status.is_some() {
        sql.push_str(" AND status = ?");
    }
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

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
            "userId": row.try_get::<String, _>("user_id")?,
            "kind": row.try_get::<String, _>("kind")?,
            "status": row.try_get::<String, _>("status")?,
            "resultKey": row.try_get::<Option<String>, _>("result_key")?,
            "createdAt": created_at,
            "updatedAt": row.try_get::<i64, _>("updated_at")?,
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "requests": items, "nextCursor": next_cursor })))
}

/// POST /admin/data-requests/{id}/run?reason=：执行导出/删除任务（占位实现）→ 标记 done。
/// 真实的对象导出打包 / 级联删除由后续实现填充；此处标记完成并为 export 生成占位 result_key。
pub(super) async fn run_data_request(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["support"])?;
    let row = sqlx::query("SELECT kind, status FROM data_requests WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let kind: String = row.try_get("kind")?;
    let status: String = row.try_get("status")?;

    // 幂等：已完成直接返回当前态。
    if status == "done" {
        return Ok(Json(json!({ "id": id, "status": "done", "kind": kind, "note": "already_done" })));
    }

    // 合规：delete 工单的真实级联删除尚未实现——绝不能标记 done（等于谎报已删除）。
    // 保持 pending，登记一次尝试审计，返回未实现说明，留待真实删除管线接入后处理。
    if kind == "delete" {
        audit(
            &state.db,
            &admin.0,
            "data_request.run_deferred",
            &id,
            &format!("kind=delete not_implemented {}", q.reason()),
        )
        .await?;
        return Ok(Json(json!({
            "id": id,
            "status": "pending",
            "kind": kind,
            "note": "delete_execution_not_implemented",
            "message": "真实级联删除未实现，工单保持 pending 不标记完成（合规）。",
        })));
    }

    // export：占位实现（生成占位 result_key）→ 标记 done。
    let result_key = format!("export/{id}.json");
    sqlx::query("UPDATE data_requests SET status = 'done', result_key = ?, updated_at = ? WHERE id = ?")
        .bind(&result_key)
        .bind(now_ms())
        .bind(&id)
        .execute(&state.db)
        .await?;

    audit(&state.db, &admin.0, "data_request.run", &id, &format!("kind={kind} {}", q.reason()))
        .await?;
    Ok(Json(json!({ "id": id, "status": "done", "kind": kind, "resultKey": result_key })))
}
