//! 干预系统（S3）：影响环-托梦 / 影响环-道具（平台规格 §2.4 三环、§9.6 服务端权威）。
//!
//! POST /worlds/{id}/interventions  {kind, characterId, payload, expectedWorldRevision}
//!   Idempotency-Key + expectedWorldRevision。服务端权威校验（§9.6）：
//!   - 角色属于本人且 active 在场（否则 risk_event + RiskBlocked）；
//!   - expectedRevision 与世界当前 state_revision 不符 → 409；
//!   - whisper：≤100 字、非空，过 moderation（Approved 才 accepted，否则 rejected/moderation）；
//!   - item：物品真在 backpacks(owned/carried，carried 须匹配本世界)，否则 risk_event("forged_state")+RiskBlocked；
//!           世界准入 admission::check_admission 为 S4 占位（当前"存在即通过"，留 TODO）；
//!   - 每节拍固定额度（P4a 所有人相同）超限 → rejected("quota")。
//! GET /worlds/{id}/interventions/mine  我的干预记录与状态。
//!
//! accepted 的干预由 runtime 下一 tick 消费（whisper 进对应角色低优先层），消费后置 applied；本模块不改叙事状态。

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::providers::ModerationVerdict;
use crate::{idempotency, safety};

/// 每用户每世界每节拍固定干预额度（P4a 所有人相同）。以"尚未被 runtime 消费(accepted)的干预数"近似
/// 本节拍额度：runtime 每 tick 将 accepted→applied，自然重置该窗口。
pub const PER_TICK_QUOTA: i64 = 3;

const WHISPER_MAX_CHARS: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterventionReq {
    pub kind: String, // whisper | item
    pub character_id: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub expected_world_revision: i64,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worlds/{id}/interventions", post(create_intervention))
        .route("/worlds/{id}/interventions/mine", get(my_interventions))
}

async fn create_intervention(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<InterventionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let kind = req.kind.as_str();
    if kind != "whisper" && kind != "item" {
        return Err(ApiError::BadRequest("kind 必须为 whisper 或 item".into()));
    }

    // 幂等：同 key 同载荷 → 返回缓存响应；同 key 异载荷 → 409。
    let endpoint = "POST /worlds/:id/interventions";
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let idem_key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, endpoint, idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // 世界存在 + 运行态 + revision CAS。
    let world: Option<(i64, String)> = sqlx::query_as("SELECT state_revision, status FROM worlds WHERE id = ?")
        .bind(&world_id)
        .fetch_optional(&state.db)
        .await?;
    let (state_revision, status) = world.ok_or(ApiError::NotFound)?;
    if status != "open" && status != "running" {
        return Err(ApiError::Conflict("world_not_running".into()));
    }
    if req.expected_world_revision != state_revision {
        return Err(ApiError::Conflict("revision".into()));
    }

    // 角色必须属于本人且 active 在场（服务端权威，§9.6）。
    let member: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM world_members WHERE world_id = ? AND cloud_character_id = ? AND user_id = ? AND status = 'active'",
    )
    .bind(&world_id)
    .bind(&req.character_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    if member.is_none() {
        safety::record_risk(
            &state.db,
            Some(&user.user_id),
            Some(&world_id),
            "intervention_denied",
            json!({"reason": "character_not_present_or_owned", "characterId": req.character_id, "kind": kind}),
        )
        .await?;
        return Err(ApiError::RiskBlocked);
    }

    // 分类别授权/校验。
    if kind == "item" {
        let item_id = req
            .payload
            .get("itemId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ApiError::BadRequest("payload.itemId 缺失".into()))?;
        let bp: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, carried_world_id FROM backpacks WHERE user_id = ? AND item_id = ? AND status IN ('owned','carried') LIMIT 1",
        )
        .bind(&user.user_id)
        .bind(item_id)
        .fetch_optional(&state.db)
        .await?;
        match bp {
            None => {
                // 声明一个不在背包里的道具 = 伪造状态（§9.6 伪造背包清单）。
                safety::record_risk(
                    &state.db,
                    Some(&user.user_id),
                    Some(&world_id),
                    "forged_state",
                    json!({"reason": "item_not_in_backpack", "itemId": item_id}),
                )
                .await?;
                return Err(ApiError::RiskBlocked);
            }
            Some((st, carried)) if st == "carried" && carried.as_deref() != Some(world_id.as_str()) => {
                // 该道具已随角色进入其他世界，不能同时投放到本世界。
                safety::record_risk(
                    &state.db,
                    Some(&user.user_id),
                    Some(&world_id),
                    "forged_state",
                    json!({"reason": "item_carried_elsewhere", "itemId": item_id}),
                )
                .await?;
                return Err(ApiError::RiskBlocked);
            }
            Some(_) => {
                // 世界准入校验（§9.5.B）：道具体系不符目标世界策略则拒绝投放（Admitted/Translated 放行）。
                // 物品无结构化定义（仅背包记录、无 items 行）时按"存在即通过"，不阻断。
                if let Some(item_def) = crate::backpack::load_item(&state.db, item_id).await? {
                    let policy = crate::backpack::load_admission_policy(&state.db, &world_id).await?;
                    match crate::admission::check_admission(&policy, &item_def) {
                        Ok(crate::admission::AdmissionDecision::Admitted)
                        | Ok(crate::admission::AdmissionDecision::Translated) => {}
                        Ok(_) => return Err(ApiError::BadRequest("该道具不符合本世界的物品准入规则".into())),
                        Err(_) => return Err(ApiError::BadRequest("道具体系标签非法".into())),
                    }
                }
            }
        }
    } else {
        // whisper：长度与非空校验。
        let text = req.payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            return Err(ApiError::BadRequest("whisper text 不能为空".into()));
        }
        if text.chars().count() > WHISPER_MAX_CHARS {
            return Err(ApiError::BadRequest("whisper 不能超过 100 字".into()));
        }
    }

    // 额度校验（超限即 rejected("quota")，不作为攻击）。
    let mut reject_reason: Option<String> = None;
    let pending: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM interventions WHERE world_id = ? AND user_id = ? AND status = 'accepted'",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .fetch_one(&state.db)
    .await?;
    if pending >= PER_TICK_QUOTA {
        reject_reason = Some("quota".into());
    }

    let iid = crate::db::new_id("iv");

    // whisper moderation（额度通过后再机审，避免超额时多余模型调用）。
    if reject_reason.is_none() && kind == "whisper" {
        let text = req.payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let verdict = safety::moderate_and_queue(&state, "intervention", &iid, text).await?;
        if verdict != ModerationVerdict::Approved {
            reject_reason = Some("moderation".into());
        }
    }

    let final_status = if reject_reason.is_some() { "rejected" } else { "accepted" };

    sqlx::query(
        "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, expected_revision, status, reject_reason, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&iid)
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(&req.character_id)
    .bind(kind)
    .bind(req.payload.to_string())
    .bind(req.expected_world_revision)
    .bind(final_status)
    .bind(reject_reason.as_deref())
    .bind(crate::db::now_ms())
    .execute(&state.db)
    .await?;

    let resp = json!({
        "id": iid,
        "worldId": world_id,
        "kind": kind,
        "characterId": req.character_id,
        "status": final_status,
        "rejectReason": reject_reason,
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

async fn my_interventions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows: Vec<(String, String, String, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, kind, character_id, status, reject_reason, created_at FROM interventions \
         WHERE world_id = ? AND user_id = ? ORDER BY created_at DESC LIMIT 50",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, kind, cid, status, reason, created)| {
            json!({"id": id, "kind": kind, "characterId": cid, "status": status, "rejectReason": reason, "createdAt": created})
        })
        .collect();
    Ok(Json(json!({ "interventions": items })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::testkit::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn post_intervention(state: &AppState, token_str: &str, world: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let app = crate::app::build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/worlds/{world}/interventions"))
                    .header("authorization", format!("Bearer {token_str}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v = serde_json::from_slice(&bytes).unwrap_or(json!(null));
        (status, v)
    }

    #[tokio::test]
    async fn whisper_accepted() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "记得完成今天的画作"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "accepted");
    }

    #[tokio::test]
    async fn revision_mismatch_conflicts() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 7, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let (status, _v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "hi"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn forged_item_blocked_and_recorded() {
        // 越权道具：声明一个不在背包里的道具 → RiskBlocked + risk_event(forged_state)。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let (status, _v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "item", "characterId": "c1", "payload": {"itemId": "sword_of_nobody"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let n = count(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind = 'forged_state'").await;
        assert_eq!(n, 1, "应记录一条 forged_state 风控事件");
    }

    #[tokio::test]
    async fn owned_item_accepted() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_backpack(&state.db, "b1", "u1", "gem", "owned", None).await;
        let tk = token(&state, "u1");

        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "item", "characterId": "c1", "payload": {"itemId": "gem"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "accepted");
    }

    #[tokio::test]
    async fn quota_exceeded_rejected() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        // 预置已达额度的 accepted 干预。
        for i in 0..PER_TICK_QUOTA {
            sqlx::query(
                "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, expected_revision, status, created_at) \
                 VALUES (?, 'w1', 'u1', 'c1', 'whisper', '{}', 0, 'accepted', ?)",
            )
            .bind(format!("pre{i}"))
            .bind(crate::db::now_ms())
            .execute(&state.db)
            .await
            .unwrap();
        }
        let tk = token(&state, "u1");
        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "再来一条"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "rejected");
        assert_eq!(v["rejectReason"], "quota");
    }
}
