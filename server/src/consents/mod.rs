//! 同意机制（S3）：不可逆事件的当事人确认（平台规格 §2.4 边界与同意）。
//!
//! 状态机：runtime 产出 ConsentRequested 域事件 → create_consent 建 pending + 通知当事角色主人 →
//!   各主人 respond → 全同意 approved / 任一拒绝 declined / 超时 expired_conservative（保守默认，
//!   不默认同意死亡或永久关系变化）→ runtime 下一 tick 仅在 approved 时允许不可逆事件落地。
//!
//! 端点：GET /me/consents?status=pending；POST /worlds/{id}/consents/{cid}/respond {approve}（幂等）。

use std::collections::HashSet;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::AnyPool;

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::idempotency;
use crate::notifications::enqueue_notification;

/// 创建同意请求（runtime 在 ConsentRequested 时调用），并通知当事角色主人。返回 consent id。
pub async fn create_consent(
    state: &AppState,
    world_id: &str,
    event_kind: &str,
    subject_character_ids: &[String],
    detail: &str,
    ttl_ms: i64,
) -> Result<String, ApiError> {
    let cid = crate::db::new_id("cs");
    let now = crate::db::now_ms();
    let subjects_json = serde_json::to_string(subject_character_ids).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO consent_requests (id, world_id, event_kind, subject_character_ids, detail, status, responses_json, expires_at, created_at) \
         VALUES (?, ?, ?, ?, ?, 'pending', '{}', ?, ?)",
    )
    .bind(&cid)
    .bind(world_id)
    .bind(event_kind)
    .bind(&subjects_json)
    .bind(detail)
    .bind(now + ttl_ms)
    .bind(now)
    .execute(&state.db)
    .await?;

    // 通知当事角色主人来响应（outbox 去重键含 consent+user）。
    for uid in owners_of(&state.db, world_id, subject_character_ids).await? {
        let dk = format!("consent:{cid}:{uid}");
        enqueue_notification(
            state,
            &uid,
            "consent_request",
            json!({"consentId": cid, "worldId": world_id, "eventKind": event_kind}),
            Some(&dk),
            now,
        )
        .await?;
    }
    Ok(cid)
}

/// 超时未响应的 pending 同意 → expired_conservative（保守默认）。runtime 每 tick / 读接口惰性调用。
pub async fn expire_stale_consents(db: &AnyPool) -> Result<u64, ApiError> {
    let now = crate::db::now_ms();
    let res = sqlx::query(
        "UPDATE consent_requests SET status = 'expired_conservative', resolved_at = ? WHERE status = 'pending' AND expires_at <= ?",
    )
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

async fn owners_of(db: &AnyPool, world_id: &str, char_ids: &[String]) -> Result<Vec<String>, ApiError> {
    let mut owners: Vec<String> = Vec::new();
    for cid in char_ids {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT user_id FROM world_members WHERE world_id = ? AND cloud_character_id = ?")
                .bind(world_id)
                .bind(cid)
                .fetch_optional(db)
                .await?;
        if let Some((u,)) = row {
            if !owners.contains(&u) {
                owners.push(u);
            }
        }
    }
    Ok(owners)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/consents", get(my_consents))
        .route("/worlds/{id}/consents/{cid}/respond", post(respond))
}

#[derive(Debug, Deserialize)]
struct StatusQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn my_consents(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<StatusQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 读时惰性过期，保证结果反映保守默认。
    expire_stale_consents(&state.db).await?;

    // 用户在各世界的角色。
    let members: Vec<(String, String)> =
        sqlx::query_as("SELECT world_id, cloud_character_id FROM world_members WHERE user_id = ?")
            .bind(&user.user_id)
            .fetch_all(&state.db)
            .await?;
    if members.is_empty() {
        return Ok(Json(json!({ "consents": [] })));
    }

    let status_filter = q.status.unwrap_or_else(|| "pending".into());
    let rows: Vec<(String, String, String, String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT DISTINCT c.id, c.world_id, c.event_kind, c.subject_character_ids, c.detail, c.status, c.responses_json, c.expires_at, c.created_at \
         FROM consent_requests c JOIN world_members m ON m.world_id = c.world_id AND m.user_id = ? \
         WHERE (? = 'all' OR c.status = ?) ORDER BY c.created_at DESC LIMIT 100",
    )
    .bind(&user.user_id)
    .bind(&status_filter)
    .bind(&status_filter)
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::new();
    for (id, world_id, event_kind, subjects_json, detail, status, responses_json, expires_at, created_at) in rows {
        let subjects: Vec<String> = serde_json::from_str(&subjects_json).unwrap_or_default();
        let my_chars: HashSet<&String> = members
            .iter()
            .filter(|(w, _)| *w == world_id)
            .map(|(_, c)| c)
            .collect();
        let my_subjects: Vec<String> = subjects.iter().filter(|s| my_chars.contains(*s)).cloned().collect();
        if my_subjects.is_empty() {
            continue; // 我不是当事人，隐藏。
        }
        let resp_map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&responses_json).unwrap_or_default();
        let responded = my_subjects.iter().all(|c| resp_map.contains_key(c));
        out.push(json!({
            "id": id,
            "worldId": world_id,
            "eventKind": event_kind,
            "detail": detail,               // 只展示规则/后果，不含模型隐藏推理（§2.4）
            "options": ["approve", "decline"],
            "status": status,
            "mySubjects": my_subjects,
            "responded": responded,
            "expiresAt": expires_at,
            "createdAt": created_at,
        }));
    }
    Ok(Json(json!({ "consents": out })))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RespondReq {
    approve: bool,
}

async fn respond(
    State(state): State<AppState>,
    user: AuthUser,
    Path((world_id, cid)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<RespondReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let endpoint = "POST /worlds/:id/consents/:cid/respond";
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let idem_key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, endpoint, idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    expire_stale_consents(&state.db).await?;

    let c: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT world_id, subject_character_ids, status, responses_json FROM consent_requests WHERE id = ?",
    )
    .bind(&cid)
    .fetch_optional(&state.db)
    .await?;
    let (c_world, subjects_json, status, responses_json) = c.ok_or(ApiError::NotFound)?;
    if c_world != world_id {
        return Err(ApiError::NotFound);
    }

    // 当事人校验：用户在本世界的角色 ∩ subjects。
    let my_chars: Vec<(String,)> =
        sqlx::query_as("SELECT cloud_character_id FROM world_members WHERE world_id = ? AND user_id = ?")
            .bind(&world_id)
            .bind(&user.user_id)
            .fetch_all(&state.db)
            .await?;
    let my_set: HashSet<String> = my_chars.into_iter().map(|(c,)| c).collect();
    let subjects: Vec<String> = serde_json::from_str(&subjects_json).unwrap_or_default();
    let my_subjects: Vec<String> = subjects.iter().filter(|s| my_set.contains(*s)).cloned().collect();
    if my_subjects.is_empty() {
        return Err(ApiError::Forbidden);
    }

    // 已解决 → 幂等返回当前状态。
    if status != "pending" {
        let resp = json!({"consentId": cid, "status": status});
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    let mut resp_map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&responses_json).unwrap_or_default();
    let verdict = if req.approve { "approved" } else { "declined" };
    for s in &my_subjects {
        resp_map.insert(s.clone(), json!(verdict));
    }

    // 任一拒绝 → declined；全部当事角色 approved → approved；否则仍 pending。
    let any_declined = resp_map.values().any(|v| v == "declined");
    let all_approved = subjects.iter().all(|s| resp_map.get(s).map(|v| v == "approved").unwrap_or(false));
    let new_status = if any_declined {
        "declined"
    } else if all_approved {
        "approved"
    } else {
        "pending"
    };
    let resolved_at = if new_status == "pending" { None } else { Some(crate::db::now_ms()) };

    sqlx::query("UPDATE consent_requests SET responses_json = ?, status = ?, resolved_at = ? WHERE id = ?")
        .bind(serde_json::to_string(&resp_map).unwrap_or_else(|_| "{}".into()))
        .bind(new_status)
        .bind(resolved_at)
        .bind(&cid)
        .execute(&state.db)
        .await?;

    let resp = json!({"consentId": cid, "status": new_status, "responded": my_subjects});
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::testkit::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn status_of(db: &AnyPool, cid: &str) -> String {
        sqlx::query_scalar::<_, String>("SELECT status FROM consent_requests WHERE id = ?")
            .bind(cid)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn respond_via_http(state: &AppState, user: &str, world: &str, cid: &str, approve: bool) -> (StatusCode, serde_json::Value) {
        let tk = token(state, user);
        let app = crate::app::build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/worlds/{world}/consents/{cid}/respond"))
                    .header("authorization", format!("Bearer {tk}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"approve": approve}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let s = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (s, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
    }

    #[tokio::test]
    async fn timeout_expires_conservative() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        // ttl 为负 → 立即过期。
        let cid = create_consent(&state, "w1", "death", &["c1".into()], "角色 c1 将永久死亡", -1000)
            .await
            .unwrap();
        let n = expire_stale_consents(&state.db).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(status_of(&state.db, &cid).await, "expired_conservative");
    }

    #[tokio::test]
    async fn single_owner_approve() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let cid = create_consent(&state, "w1", "death", &["c1".into()], "角色 c1 将永久死亡", 3_600_000)
            .await
            .unwrap();
        let (s, v) = respond_via_http(&state, "u1", "w1", &cid, true).await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "approved");
    }

    #[tokio::test]
    async fn any_decline_declines_multi_subject() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_user(&state.db, "u2").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_member(&state.db, "m2", "w1", "u2", "c2", "active").await;
        let cid = create_consent(&state, "w1", "relationship", &["c1".into(), "c2".into()], "c1 与 c2 结为永久羁绊", 3_600_000)
            .await
            .unwrap();
        // u1 同意 → 仍 pending（尚缺 c2）。
        let (_, v1) = respond_via_http(&state, "u1", "w1", &cid, true).await;
        assert_eq!(v1["status"], "pending");
        // u2 拒绝 → declined。
        let (_, v2) = respond_via_http(&state, "u2", "w1", &cid, false).await;
        assert_eq!(v2["status"], "declined");
        assert_eq!(status_of(&state.db, &cid).await, "declined");
    }

    #[tokio::test]
    async fn non_subject_forbidden_and_list_scoped() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_user(&state.db, "u2").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_member(&state.db, "m2", "w1", "u2", "c2", "active").await;
        // 仅 c1 是当事人。
        let cid = create_consent(&state, "w1", "death", &["c1".into()], "角色 c1 将永久死亡", 3_600_000)
            .await
            .unwrap();
        // u2 不是当事人 → Forbidden。
        let (s, _) = respond_via_http(&state, "u2", "w1", &cid, true).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // u2 的 /me/consents 不含该请求。
        let tk = token(&state, "u2");
        let app = crate::app::build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/me/consents?status=pending")
                    .header("authorization", format!("Bearer {tk}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["consents"].as_array().unwrap().len(), 0);
    }
}
