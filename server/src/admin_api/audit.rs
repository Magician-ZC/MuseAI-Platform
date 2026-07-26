//! 内容审核：审核队列（机审结果 + 人审操作）。approve/reject 同步回写主体 moderation，
//! reject 另将理由落 audit_queue.reject_reason（用户侧 status 端点回显）。
//!
//! ⚠️ 本文件只处置**仍在人审队列里**的条目。**已过审内容**的事后处置（再审 / 下架 / 恢复）
//! 在兄弟模块 `super::takedown`（migration 0044）——approve 回写在此加了一道
//! 「不复活已下架主体」的守卫，两处必须一起读。
//! 申诉复审：GET /admin/appeals 列表 + POST /admin/appeals/{id}/resolve（overturn/uphold）——
//! resolve 是机审/人审驳回后的唯一改判路径，必留 audit_logs。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::{now_ms, Placeholders};
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

    // 发号顺序 = 下面 bind 的顺序；cursor 段出不出现要到运行时才知道，编号不能写死。
    let mut ph = Placeholders::new();
    let mut sql = format!(
        "SELECT id, subject_kind, subject_id, machine_verdict, machine_hits, status, \
         reviewer_id, reviewed_at, created_at FROM audit_queue WHERE status = {}",
        ph.take()
    );
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(&format!(
            " AND (created_at < {} OR (created_at = {} AND id < {}))",
            ph.take(),
            ph.take(),
            ph.take()
        ));
    }
    sql.push_str(&format!(" ORDER BY created_at DESC, id DESC LIMIT {}", ph.take()));

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
         reviewer_id, reviewed_at, created_at FROM audit_queue WHERE id = $1",
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
        "subjectImageUrl": Value::Null,
    });

    // 位图主体（立绘 / 世界封面）：把图本身给人审看。
    //
    // 🔴 没有这一段，位图入队就是让人**盲审**——工作台上只有一行 `subjectKind=character_avatar`
    // 和一个空的命中列表，人审无从判断该通过还是驳回，于是「有再审通道」在实践中等于没有。
    // 下发范围与 `cardJson`（整张卡原文）一致：本端点已经是 reviewer 守卫的后台面，
    // 审核者要看的正是未过审的那份内容——玩家侧的「仅 approved 才下发」闸门不受影响。
    if let Some((table, url_column)) = match subject_kind.as_str() {
        "character_avatar" => Some(("cloud_characters", "avatar_url")),
        "world_cover" => Some(("worlds", "cover_url")),
        _ => None,
    } {
        // 表名/列名来自上面这个静态白名单，主体 id 走 $1 绑定。
        let url: Option<Option<String>> =
            sqlx::query_scalar(&format!("SELECT {url_column} FROM {table} WHERE id = $1"))
                .bind(&subject_id)
                .fetch_optional(&state.db)
                .await?;
        if let Some(url) = url.flatten().filter(|u| !u.trim().is_empty()) {
            out["subjectImageUrl"] = json!(url);
        }
    }

    if subject_kind == "character" {
        if let Some(crow) =
            sqlx::query("SELECT owner_id, card_json, manifest_json FROM cloud_characters WHERE id = $1")
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
                 WHERE owner_id = $1 AND id != $2 ORDER BY created_at DESC, version DESC, id DESC",
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

/// 队列主体 → 裁决回写的 `(表, 展示态列)`。返回 `None` = 该主体尚无回写路径（仅登记裁决）。
///
/// 🔴 **本表是「哪些主体可以入队」的事实上限**。往 `audit_queue` 塞一个这里没有分支的
/// `subject_kind`，等于制造一条人审点了通过/驳回也无处落地的**死队列项**——迁移 0027 的注释
/// 正是因为这个原因，让世界封面的机审裁决一直不入队。所以扩队列主体和扩本表必须同一批做：
/// `safety::queue_operator_recheck_image` 与下面两条位图分支是一对，缺任何一半都是回归。
///
/// 位图两列可空（无立绘 / 无封面的行留 NULL），调用点的下架守卫因此必须写成 NULL 安全形式。
pub(super) fn writeback_target(subject_kind: &str) -> Option<(&'static str, &'static str)> {
    match subject_kind {
        "character" => Some(("cloud_characters", "moderation")),
        // "template"（admin 官方模板）与 "world_template"（创作者 /assets/worlds 资产）同落 world_templates。
        "template" | "world_template" => Some(("world_templates", "moderation")),
        // 位图两类：迁移 0016 / 0027 起就有展示态列与读取面闸门，缺的一直是人审入队 + 回写这一段。
        "character_avatar" => Some(("cloud_characters", "avatar_moderation")),
        "world_cover" => Some(("worlds", "cover_moderation")),
        // intervention / world_event 等主体的回写路径随对应模块接入（当前仅登记裁决）。
        _ => None,
    }
}

async fn review(
    state: &AppState,
    actor: &AuthUser,
    queue_id: &str,
    verdict: &str,
    reason: &str,
) -> Result<Json<Value>, ApiError> {
    let row =
        sqlx::query("SELECT subject_kind, subject_id, status FROM audit_queue WHERE id = $1")
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
        "UPDATE audit_queue SET status = $1, reviewer_id = $2, reviewed_at = $3, reject_reason = $4 WHERE id = $5",
    )
    .bind(verdict)
    .bind(&actor.user_id)
    .bind(now_ms())
    .bind(reject_reason)
    .bind(queue_id)
    .execute(&state.db)
    .await?;

    // 回写主体展示态。四类主体各落一张表的一列，见 `writeback_target`。
    let moderation = if verdict == "approved" { "approved" } else { "rejected" };

    if let Some((table, column)) = writeback_target(&subject_kind) {
        // 🔴 **已被运营下架的主体不得经人审队列悄悄复活**（migration 0044 / `super::takedown`）。
        // 场景：先对已过审内容发起再审（入队），期间又把它下架了，随后有人在工作台点「通过」——
        // 若不设防，回写会把展示态写回 `'approved'`，等于**绕过 `restore` 的可逆性台阶与
        // 权限台阶**把下架撤销掉，而且 `content_takedowns` 里还留着一条自称仍在下架的记录。
        //
        // 守卫只加在 **approve 方向**：reject 写 `'rejected'` 是比下架更强的处置，让它落地是对的。
        // 恢复展示的唯一路径是 POST /admin/content/{kind}/{id}/restore。
        //
        // ⚠️ 位图两列（`avatar_moderation` / `cover_moderation`）**可空**，而 `col <> 'takedown'`
        // 在 col 为 NULL 时求值为 NULL 而非 TRUE，整条 WHERE 会静默命中 0 行——那会表现为
        // 「人审点了通过但什么都没发生」。故写成 NULL 安全形式（两个库口径一致）。
        let takedown_guard = if verdict == "approved" {
            format!(
                " AND ({column} IS NULL OR {column} <> '{}')",
                crate::safety::disposal::TAKEDOWN
            )
        } else {
            String::new()
        };
        // 表名/列名只可能来自 `writeback_target` 的静态白名单，请求字段永远不进 SQL 文本。
        sqlx::query(&format!(
            "UPDATE {table} SET {column} = $1 WHERE id = $2{takedown_guard}"
        ))
        .bind(moderation)
        .bind(&subject_id)
        .execute(&state.db)
        .await?;
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
        sql.push_str(&format!(" WHERE status = {}", Placeholders::new().take()));
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
                "SELECT owner_id, card_json, moderation, avatar_moderation FROM cloud_characters WHERE id = $1",
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

    let row = sqlx::query("SELECT subject_kind, subject_id, status FROM moderation_appeals WHERE id = $1")
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
            sqlx::query_as("SELECT moderation, avatar_moderation FROM cloud_characters WHERE id = $1")
                .bind(&subject_id)
                .fetch_optional(&state.db)
                .await?;
        if let Some((moderation, avatar_moderation)) = dims {
            if moderation == "rejected" {
                sqlx::query("UPDATE cloud_characters SET moderation = 'approved' WHERE id = $1")
                    .bind(&subject_id)
                    .execute(&state.db)
                    .await?;
            } else if avatar_moderation.as_deref() == Some("rejected") {
                sqlx::query("UPDATE cloud_characters SET avatar_moderation = 'approved' WHERE id = $1")
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
        "UPDATE moderation_appeals SET status = $1, resolution_reason = $2, reviewer_id = $3, resolved_at = $4 WHERE id = $5",
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
         reviewer_id, created_at, resolved_at FROM moderation_appeals WHERE id = $1",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(appeal_json(&row)?))
}
