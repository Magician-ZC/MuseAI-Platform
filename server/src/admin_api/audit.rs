//! 内容审核：审核队列（机审结果 + 人审操作）。approve/reject 同步回写主体 moderation，
//! reject 另将理由落 audit_queue.reject_reason（用户侧 status 端点回显）。
//! 申诉复审：GET /admin/appeals 列表 + POST /admin/appeals/{id}/resolve（overturn/uphold）——
//! resolve 是机审/人审驳回后的唯一改判路径，必留 audit_logs。

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

    // 人审驳回理由同步落队列行 reject_reason（供用户侧 status 端点回显）；approve 不写（保持 NULL）。
    let reject_reason: Option<&str> =
        if verdict == "rejected" && !reason.trim().is_empty() { Some(reason) } else { None };
    sqlx::query(
        "UPDATE audit_queue SET status = ?, reviewer_id = ?, reviewed_at = ?, reject_reason = ? WHERE id = ?",
    )
    .bind(verdict)
    .bind(&actor.user_id)
    .bind(now_ms())
    .bind(reject_reason)
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

// ---------------- 申诉复审（内容风控申诉，reviewer/admin） ----------------

#[derive(Debug, Deserialize)]
pub(super) struct AppealsQuery {
    status: Option<String>,
}

/// 申诉行 → camelCase JSON（列表与 resolve 响应共用同一形状）。
fn appeal_json(row: &sqlx::any::AnyRow) -> Result<Value, ApiError> {
    Ok(json!({
        "id": row.try_get::<String, _>("id")?,
        "subjectKind": row.try_get::<String, _>("subject_kind")?,
        "subjectId": row.try_get::<String, _>("subject_id")?,
        "ownerId": row.try_get::<String, _>("owner_id")?,
        "appealText": row.try_get::<String, _>("appeal_text")?,
        "status": row.try_get::<String, _>("status")?,
        "resolutionReason": row.try_get::<Option<String>, _>("resolution_reason")?,
        "reviewerId": row.try_get::<Option<String>, _>("reviewer_id")?,
        "createdAt": row.try_get::<i64, _>("created_at")?,
        "resolvedAt": row.try_get::<Option<i64>, _>("resolved_at")?,
    }))
}

/// GET /admin/appeals?status=pending|upheld|overturned|all（默认 pending）：申诉列表 + 主体摘要。
/// character 主体摘要：名字（card_json identity.name）、moderation、avatar_moderation、owner_id。
pub(super) async fn list_appeals(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<AppealsQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let status = q.status.unwrap_or_else(|| "pending".into());
    if !matches!(status.as_str(), "pending" | "upheld" | "overturned" | "all") {
        return Err(ApiError::BadRequest("status 仅支持 pending/upheld/overturned/all".into()));
    }

    let mut sql = String::from(
        "SELECT id, subject_kind, subject_id, owner_id, appeal_text, status, resolution_reason, \
         reviewer_id, created_at, resolved_at FROM moderation_appeals",
    );
    if status != "all" {
        sql.push_str(" WHERE status = ?");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC");
    let mut query = sqlx::query(&sql);
    if status != "all" {
        query = query.bind(&status);
    }
    let rows = query.fetch_all(&state.db).await?;

    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut item = appeal_json(row)?;
        let subject_kind: String = row.try_get("subject_kind")?;
        let subject_id: String = row.try_get("subject_id")?;
        // 主体摘要：当前仅 character；主体缺失（已删除）留 null，申诉行本身仍可见。
        let mut subject = Value::Null;
        if subject_kind == "character" {
            if let Some(crow) = sqlx::query(
                "SELECT owner_id, card_json, moderation, avatar_moderation FROM cloud_characters WHERE id = ?",
            )
            .bind(&subject_id)
            .fetch_optional(&state.db)
            .await?
            {
                let card_text: String = crow.try_get("card_json")?;
                let name = serde_json::from_str::<Value>(&card_text)
                    .ok()
                    .and_then(|c| c["identity"]["name"].as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                subject = json!({
                    "name": name,
                    "moderation": crow.try_get::<String, _>("moderation")?,
                    "avatarModeration": crow.try_get::<Option<String>, _>("avatar_moderation")?,
                    "ownerId": crow.try_get::<String, _>("owner_id")?,
                });
            }
        }
        item["subject"] = subject;
        items.push(item);
    }
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResolveAppealReq {
    decision: String,
    #[serde(default)]
    reason: String,
}

/// POST /admin/appeals/{id}/resolve body {decision:'overturn'|'uphold', reason}：申诉复审裁决。
///
/// overturn 是驳回后的**唯一改判路径**：只翻转「当时处于 rejected 的那个维度」——
/// 卡 moderation=='rejected' 则改卡为 approved；仅当卡不处于 rejected 而头像
/// avatar_moderation=='rejected' 时才改头像为 approved。不整体放行（卡与头像分开审、分开改判，
/// 避免申诉卡文案却顺带放行未过审头像）。uphold 维持原判，任何 moderation 不动。
/// 两者都：更新申诉行 + audit_logs 留痕（appeal_overturn/appeal_uphold）。
pub(super) async fn resolve_appeal(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(req): Json<ResolveAppealReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    if !matches!(req.decision.as_str(), "overturn" | "uphold") {
        return Err(ApiError::BadRequest("decision 仅支持 overturn/uphold".into()));
    }
    let reason = req.reason.trim().to_string();
    let reason_chars = reason.chars().count();
    if reason_chars == 0 || reason_chars > 500 {
        return Err(ApiError::BadRequest("复审理由必填且不超过 500 字符".into()));
    }

    let row = sqlx::query("SELECT subject_kind, subject_id, status FROM moderation_appeals WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let cur_status: String = row.try_get("status")?;
    if cur_status != "pending" {
        return Err(ApiError::Conflict("该申诉已处理，不可重复裁决".into()));
    }

    if req.decision == "overturn" && subject_kind == "character" {
        // 改判只翻转当时处于 rejected 的那个维度（见函数注释）：卡优先，头像仅在卡未被驳回时翻转。
        let dims: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT moderation, avatar_moderation FROM cloud_characters WHERE id = ?")
                .bind(&subject_id)
                .fetch_optional(&state.db)
                .await?;
        if let Some((moderation, avatar_moderation)) = dims {
            if moderation == "rejected" {
                sqlx::query("UPDATE cloud_characters SET moderation = 'approved' WHERE id = ?")
                    .bind(&subject_id)
                    .execute(&state.db)
                    .await?;
            } else if avatar_moderation.as_deref() == Some("rejected") {
                sqlx::query("UPDATE cloud_characters SET avatar_moderation = 'approved' WHERE id = ?")
                    .bind(&subject_id)
                    .execute(&state.db)
                    .await?;
            }
        }
    }
    // uphold：主体 moderation 一律不动（维持原判）。

    let (new_status, action) = if req.decision == "overturn" {
        ("overturned", "appeal_overturn")
    } else {
        ("upheld", "appeal_uphold")
    };
    sqlx::query(
        "UPDATE moderation_appeals SET status = ?, resolution_reason = ?, reviewer_id = ?, resolved_at = ? WHERE id = ?",
    )
    .bind(new_status)
    .bind(&reason)
    .bind(&admin.0.user_id)
    .bind(now_ms())
    .bind(&id)
    .execute(&state.db)
    .await?;

    audit(&state.db, &admin.0, action, &format!("{subject_kind}:{subject_id}"), &reason).await?;

    let row = sqlx::query(
        "SELECT id, subject_kind, subject_id, owner_id, appeal_text, status, resolution_reason, \
         reviewer_id, created_at, resolved_at FROM moderation_appeals WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(appeal_json(&row)?))
}
