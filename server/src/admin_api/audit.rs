//! 内容审核：审核队列（机审结果 + 人审操作）。approve/reject 同步回写主体 moderation。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::now_ms;
use crate::error::ApiError;

use super::{audit, clamp_limit, parse_cursor, require_role, ActionQuery};

#[derive(Debug, Deserialize)]
pub(super) struct QueueQuery {
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// GET /admin/audit-queue?status=（默认 open）：机审预标注 + 待人审列表。
pub(super) async fn list_queue(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<QueueQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let page = clamp_limit(q.limit);
    let status = q.status.unwrap_or_else(|| "open".into());

    let mut sql = String::from(
        "SELECT id, subject_kind, subject_id, machine_verdict, machine_hits, status, \
         reviewer_id, reviewed_at, created_at FROM audit_queue WHERE status = ?",
    );
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

    let mut query = sqlx::query(&sql).bind(&status);
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
        let hits_raw: String = row.try_get("machine_hits")?;
        let hits: Value = serde_json::from_str(&hits_raw).unwrap_or_else(|_| json!([]));
        items.push(json!({
            "id": id,
            "subjectKind": row.try_get::<String, _>("subject_kind")?,
            "subjectId": row.try_get::<String, _>("subject_id")?,
            "machineVerdict": row.try_get::<String, _>("machine_verdict")?,
            "machineHits": hits,
            "status": row.try_get::<String, _>("status")?,
            "reviewerId": row.try_get::<Option<String>, _>("reviewer_id")?,
            "reviewedAt": row.try_get::<Option<i64>, _>("reviewed_at")?,
            "createdAt": created_at,
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "items": items, "nextCursor": next_cursor })))
}

/// GET /admin/audit-queue/{id}：审核详情（§10 审核工作台）。
/// character 主体附「卡片全文 cardJson + 可审计 manifest + 同作者历史 authorHistory」，
/// 供人审直接对照，无需再逐字段拉取。reviewer/admin 守卫。
pub(super) async fn detail(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;

    let row = sqlx::query(
        "SELECT id, subject_kind, subject_id, machine_verdict, machine_hits, status, \
         reviewer_id, reviewed_at, created_at FROM audit_queue WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let hits_raw: String = row.try_get("machine_hits")?;
    let hits: Value = serde_json::from_str(&hits_raw).unwrap_or_else(|_| json!([]));

    // 基础队列字段 + character 专属附加字段占位（非 character 主体保持空值）。
    let mut out = json!({
        "id": row.try_get::<String, _>("id")?,
        "subjectKind": subject_kind,
        "subjectId": subject_id,
        "machineVerdict": row.try_get::<String, _>("machine_verdict")?,
        "machineHits": hits,
        "status": row.try_get::<String, _>("status")?,
        "reviewerId": row.try_get::<Option<String>, _>("reviewer_id")?,
        "reviewedAt": row.try_get::<Option<i64>, _>("reviewed_at")?,
        "createdAt": row.try_get::<i64, _>("created_at")?,
        "cardJson": Value::Null,
        "manifest": Value::Null,
        "authorHistory": json!([]),
    });

    if subject_kind == "character" {
        if let Some(crow) =
            sqlx::query("SELECT owner_id, card_json, manifest_json FROM cloud_characters WHERE id = ?")
                .bind(&subject_id)
                .fetch_optional(&state.db)
                .await?
        {
            let owner_id: String = crow.try_get("owner_id")?;
            let card_text: String = crow.try_get("card_json")?;
            let manifest_text: Option<String> = crow.try_get("manifest_json")?;
            // 卡片全文（非第三人称摘要——人审需看原文判定）。
            out["cardJson"] = serde_json::from_str(&card_text).unwrap_or(Value::Null);
            out["manifest"] =
                manifest_text.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);

            // 同作者历史：同 owner 的其他云端角色（不含当前主体），供判断作者一贯性。
            let hist = sqlx::query(
                "SELECT id, version, moderation, created_at FROM cloud_characters \
                 WHERE owner_id = ? AND id != ? ORDER BY created_at DESC, version DESC",
            )
            .bind(&owner_id)
            .bind(&subject_id)
            .fetch_all(&state.db)
            .await?;
            let history: Vec<Value> = hist
                .iter()
                .map(|r| {
                    json!({
                        "id": r.try_get::<String, _>("id").unwrap_or_default(),
                        "version": r.try_get::<i64, _>("version").unwrap_or_default(),
                        "moderation": r.try_get::<String, _>("moderation").unwrap_or_default(),
                        "createdAt": r.try_get::<i64, _>("created_at").unwrap_or_default(),
                    })
                })
                .collect();
            out["authorHistory"] = json!(history);
        }
    }

    Ok(Json(out))
}

/// POST /admin/audit-queue/{id}/approve?reason=
pub(super) async fn approve(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    review(&state, &admin.0, &id, "approved", q.reason()).await
}

/// POST /admin/audit-queue/{id}/reject?reason=
pub(super) async fn reject(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    review(&state, &admin.0, &id, "rejected", q.reason()).await
}

async fn review(
    state: &AppState,
    actor: &AuthUser,
    queue_id: &str,
    verdict: &str,
    reason: &str,
) -> Result<Json<Value>, ApiError> {
    let row =
        sqlx::query("SELECT subject_kind, subject_id, status FROM audit_queue WHERE id = ?")
            .bind(queue_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let cur_status: String = row.try_get("status")?;
    if cur_status != "open" {
        return Err(ApiError::Conflict("already_reviewed".into()));
    }

    sqlx::query("UPDATE audit_queue SET status = ?, reviewer_id = ?, reviewed_at = ? WHERE id = ?")
        .bind(verdict)
        .bind(&actor.user_id)
        .bind(now_ms())
        .bind(queue_id)
        .execute(&state.db)
        .await?;

    // 回写主体 moderation：character→cloud_characters，template→world_templates。
    let moderation = if verdict == "approved" { "approved" } else { "rejected" };
    match subject_kind.as_str() {
        "character" => {
            sqlx::query("UPDATE cloud_characters SET moderation = ? WHERE id = ?")
                .bind(moderation)
                .bind(&subject_id)
                .execute(&state.db)
                .await?;
        }
        // "template"（admin 官方模板）与 "world_template"（创作者 /assets/worlds 资产）同落 world_templates。
        "template" | "world_template" => {
            sqlx::query("UPDATE world_templates SET moderation = ? WHERE id = ?")
                .bind(moderation)
                .bind(&subject_id)
                .execute(&state.db)
                .await?;
        }
        // intervention / event 等主体的回写路径随对应模块接入（当前仅登记裁决）。
        _ => {}
    }

    audit(
        &state.db,
        actor,
        &format!("audit.{verdict}"),
        &format!("{subject_kind}:{subject_id}"),
        reason,
    )
    .await?;

    Ok(Json(json!({
        "id": queue_id,
        "status": verdict,
        "subjectKind": subject_kind,
        "subjectId": subject_id,
        "moderation": moderation,
    })))
}
