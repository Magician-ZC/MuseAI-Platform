//! 用户管理：检索（脱敏，不展示原始证件/身份引用）+ 封禁/解封。

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
pub(super) struct UserQuery {
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// 手机号脱敏：保留前 3、后 4，其余打码。
fn mask_phone(phone: &str) -> String {
    let chars: Vec<char> = phone.chars().collect();
    let n = chars.len();
    if n <= 4 {
        return "*".repeat(n);
    }
    let head = 3.min(n);
    let tail = 4.min(n - head);
    let mut out: String = chars[..head].iter().collect();
    out.push_str("****");
    out.extend(&chars[n - tail..]);
    out
}

/// 邮箱脱敏：本地部分只留首字符。
fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let head = local.chars().next().map(|c| c.to_string()).unwrap_or_default();
            format!("{head}***@{domain}")
        }
        None => "***".to_string(),
    }
}

/// GET /admin/users?query=&cursor=&limit=：账号检索（脱敏）。
/// 展示身份验证「状态」（identity_verification_refs.status），绝不展示 reference_id/原始证件。
pub(super) async fn list_users(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<UserQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["support"])?;
    let page = clamp_limit(q.limit);
    let term = q.query.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let mut sql = String::from(
        "SELECT id, phone, email, nickname, age_declared, role, status, created_at, \
         (SELECT status FROM identity_verification_refs r WHERE r.user_id = users.id \
          ORDER BY created_at DESC LIMIT 1) AS verification_status \
         FROM users WHERE 1=1",
    );
    if term.is_some() {
        sql.push_str(" AND (nickname LIKE ? OR phone LIKE ? OR email LIKE ? OR id = ?)");
    }
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

    let mut query = sqlx::query(&sql);
    if let Some(t) = term {
        let like = format!("%{t}%");
        query = query.bind(like.clone()).bind(like.clone()).bind(like).bind(t.to_string());
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
        let phone: Option<String> = row.try_get("phone")?;
        let email: Option<String> = row.try_get("email")?;
        let verification: Option<String> = row.try_get("verification_status")?;
        items.push(json!({
            "id": id,
            "nickname": row.try_get::<String, _>("nickname")?,
            "phone": phone.as_deref().map(mask_phone),
            "email": email.as_deref().map(mask_email),
            "ageDeclared": row.try_get::<i64, _>("age_declared")?,
            "role": row.try_get::<String, _>("role")?,
            "status": row.try_get::<String, _>("status")?,
            "verificationStatus": verification.unwrap_or_else(|| "none".into()),
            "createdAt": created_at,
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "users": items, "nextCursor": next_cursor })))
}

/// POST /admin/users/{id}/ban?reason=
pub(super) async fn ban_user(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["support"])?;
    set_status(&state, &admin.0, &id, "banned", "user.ban", q.reason()).await
}

/// POST /admin/users/{id}/unban?reason=
pub(super) async fn unban_user(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["support"])?;
    set_status(&state, &admin.0, &id, "active", "user.unban", q.reason()).await
}

async fn set_status(
    state: &AppState,
    actor: &AuthUser,
    id: &str,
    status: &str,
    action: &str,
    reason: &str,
) -> Result<Json<Value>, ApiError> {
    let res = sqlx::query("UPDATE users SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(now_ms())
        .bind(id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit(&state.db, actor, action, id, reason).await?;
    Ok(Json(json!({ "id": id, "status": status })))
}
