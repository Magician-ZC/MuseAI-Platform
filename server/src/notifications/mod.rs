//! 通知 outbox（S3）：日报/同意等通知的发布与发送解耦、退订、静默、去重、重试、可审计（§9.2）。
//!
//! - enqueue_notification：写 outbox(pending) + 入队 topic="notify"；dedupe_key 去重（同键只发一次）。
//! - deliver（worker 单步，可直测）：查 preferences → 退订 suppressed / 静默时段延后 / dev "发送"=写 sent+tracing；
//!   失败 attempts+1，<3 退避重入队，=3 标记 failed。
//! - spawn_outbox_worker：pop→deliver 循环。
//! - GET/PUT /me/notification-preferences；GET /me/notifications?cursor=。

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;

const NOTIFY_TOPIC: &str = "notify";
const MAX_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotifyJob {
    outbox_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Sent,
    Suppressed,
    Deferred,
    Retried,
    Failed,
    /// 行不存在或已非 pending（幂等空操作）。
    Skipped,
}

/// 入队一条通知（写 outbox + push queue）。dedupe_key 命中已有行 → 返回 None（不重复）。
pub async fn enqueue_notification(
    state: &AppState,
    user_id: &str,
    kind: &str,
    payload: serde_json::Value,
    dedupe_key: Option<&str>,
    due_ms: i64,
) -> Result<Option<String>, ApiError> {
    if let Some(dk) = dedupe_key {
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM notification_outbox WHERE dedupe_key = ? LIMIT 1")
                .bind(dk)
                .fetch_optional(&state.db)
                .await?;
        if existing.is_some() {
            return Ok(None);
        }
    }
    let id = crate::db::new_id("ntf");
    sqlx::query(
        "INSERT INTO notification_outbox (id, user_id, kind, payload_json, status, attempts, dedupe_key, due_at, created_at) \
         VALUES (?, ?, ?, ?, 'pending', 0, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(kind)
    .bind(payload.to_string())
    .bind(dedupe_key)
    .bind(due_ms)
    .bind(crate::db::now_ms())
    .execute(&state.db)
    .await?;

    crate::queue::push_json(state.queue.as_ref(), NOTIFY_TOPIC, &NotifyJob { outbox_id: id.clone() }, due_ms).await;
    Ok(Some(id))
}

/// 消费一条 outbox（worker 单步）。幂等：已非 pending → Skipped。
pub async fn deliver(state: &AppState, outbox_id: &str) -> Result<DeliveryOutcome, ApiError> {
    let row: Option<(String, String, i64)> =
        sqlx::query_as("SELECT user_id, status, attempts FROM notification_outbox WHERE id = ?")
            .bind(outbox_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((user_id, status, attempts)) = row else {
        return Ok(DeliveryOutcome::Skipped);
    };
    if status != "pending" {
        return Ok(DeliveryOutcome::Skipped);
    }

    // 偏好：退订 / 静默时段。
    let pref: Option<(i64, String)> =
        sqlx::query_as("SELECT unsubscribed, quiet_hours FROM notification_preferences WHERE user_id = ?")
            .bind(&user_id)
            .fetch_optional(&state.db)
            .await?;
    if let Some((unsub, quiet)) = pref {
        if unsub != 0 {
            sqlx::query("UPDATE notification_outbox SET status = 'suppressed' WHERE id = ?")
                .bind(outbox_id)
                .execute(&state.db)
                .await?;
            tracing::info!(outbox_id, user_id, "notification 已退订，抑制");
            return Ok(DeliveryOutcome::Suppressed);
        }
        if let Some(resume_at) = quiet_resume_ms(&quiet, crate::db::now_ms()) {
            sqlx::query("UPDATE notification_outbox SET due_at = ? WHERE id = ?")
                .bind(resume_at)
                .bind(outbox_id)
                .execute(&state.db)
                .await?;
            crate::queue::push_json(state.queue.as_ref(), NOTIFY_TOPIC, &NotifyJob { outbox_id: outbox_id.to_string() }, resume_at)
                .await;
            return Ok(DeliveryOutcome::Deferred);
        }
    }

    // dev "发送"：写日志即视为送达。真实渠道 = provider 接入位。
    let send_ok = dev_send(&user_id, outbox_id).await;
    if send_ok {
        sqlx::query("UPDATE notification_outbox SET status = 'sent', sent_at = ?, attempts = attempts + 1 WHERE id = ?")
            .bind(crate::db::now_ms())
            .bind(outbox_id)
            .execute(&state.db)
            .await?;
        Ok(DeliveryOutcome::Sent)
    } else {
        let next = attempts + 1;
        if next < MAX_ATTEMPTS {
            sqlx::query("UPDATE notification_outbox SET attempts = ? WHERE id = ?")
                .bind(next)
                .bind(outbox_id)
                .execute(&state.db)
                .await?;
            let backoff = crate::db::now_ms() + 60_000 * next;
            crate::queue::push_json(state.queue.as_ref(), NOTIFY_TOPIC, &NotifyJob { outbox_id: outbox_id.to_string() }, backoff).await;
            Ok(DeliveryOutcome::Retried)
        } else {
            sqlx::query("UPDATE notification_outbox SET status = 'failed', attempts = ? WHERE id = ?")
                .bind(next)
                .bind(outbox_id)
                .execute(&state.db)
                .await?;
            Ok(DeliveryOutcome::Failed)
        }
    }
}

/// dev 渠道：仅记录 tracing，恒成功。真实短信/推送渠道替换此处。
async fn dev_send(user_id: &str, outbox_id: &str) -> bool {
    tracing::info!(user_id, outbox_id, "DevNotify 已发送（dev 模式）");
    true
}

pub fn spawn_outbox_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            let Some(job) = crate::queue::pop_json::<NotifyJob>(state.queue.as_ref(), NOTIFY_TOPIC).await else {
                continue;
            };
            if let Err(e) = deliver(&state, &job.outbox_id).await {
                tracing::error!(error = %e, outbox_id = %job.outbox_id, "outbox 投递失败");
            }
        }
    });
}

// ---------- 路由 ----------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/notification-preferences", get(get_prefs).put(put_prefs))
        .route("/me/notifications", get(list_notifications))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrefReq {
    #[serde(default)]
    channels: serde_json::Value,
    #[serde(default)]
    quiet_hours: String,
    #[serde(default)]
    unsubscribed: bool,
}

async fn get_prefs(State(state): State<AppState>, user: AuthUser) -> Result<Json<serde_json::Value>, ApiError> {
    let row: Option<(String, String, i64)> =
        sqlx::query_as("SELECT channels_json, quiet_hours, unsubscribed FROM notification_preferences WHERE user_id = ?")
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    let (channels, quiet, unsub) = row.unwrap_or_else(|| ("{}".into(), String::new(), 0));
    Ok(Json(json!({
        "channels": serde_json::from_str::<serde_json::Value>(&channels).unwrap_or_else(|_| json!({})),
        "quietHours": quiet,
        "unsubscribed": unsub != 0,
    })))
}

async fn put_prefs(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<PrefReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let channels = if req.channels.is_null() { json!({}) } else { req.channels };
    sqlx::query(
        "INSERT INTO notification_preferences (user_id, channels_json, quiet_hours, unsubscribed, updated_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET channels_json = excluded.channels_json, quiet_hours = excluded.quiet_hours, \
         unsubscribed = excluded.unsubscribed, updated_at = excluded.updated_at",
    )
    .bind(&user.user_id)
    .bind(channels.to_string())
    .bind(&req.quiet_hours)
    .bind(if req.unsubscribed { 1_i64 } else { 0 })
    .bind(crate::db::now_ms())
    .execute(&state.db)
    .await?;
    Ok(Json(json!({"ok": true, "unsubscribed": req.unsubscribed})))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    cursor: Option<i64>,
}

async fn list_notifications(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, kind, payload_json, status, created_at FROM notification_outbox \
         WHERE user_id = ? AND (? IS NULL OR created_at < ?) ORDER BY created_at DESC LIMIT 30",
    )
    .bind(&user.user_id)
    .bind(q.cursor)
    .bind(q.cursor)
    .fetch_all(&state.db)
    .await?;

    let next = rows.last().map(|r| r.4);
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, kind, payload, status, created)| {
            json!({
                "id": id,
                "kind": kind,
                "payload": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or_else(|_| json!({})),
                "status": status,
                "createdAt": created,
            })
        })
        .collect();
    Ok(Json(json!({"notifications": items, "nextCursor": next})))
}

// ---------- 静默时段 ----------

/// 若当前处于静默时段，返回应恢复投递的毫秒时间戳；否则 None。格式 "HH:MM-HH:MM"（UTC，可跨夜）。
fn quiet_resume_ms(quiet: &str, now_ms: i64) -> Option<i64> {
    let (start, end) = parse_quiet(quiet)?;
    let day_ms = 86_400_000_i64;
    let midnight = now_ms - now_ms.rem_euclid(day_ms);
    let cur = (now_ms - midnight) as i32 / 60_000; // minute of day
    let active = if start <= end {
        cur >= start && cur < end
    } else {
        cur >= start || cur < end
    };
    if !active {
        return None;
    }
    // 恢复到 end（若已跨夜且 cur>=start，则次日 end）。
    let resume_min = if start <= end || cur < end { end } else { end + 24 * 60 };
    Some(midnight + resume_min as i64 * 60_000)
}

fn parse_quiet(quiet: &str) -> Option<(i32, i32)> {
    let (a, b) = quiet.split_once('-')?;
    Some((parse_hhmm(a.trim())?, parse_hhmm(b.trim())?))
}

fn parse_hhmm(s: &str) -> Option<i32> {
    let (h, m) = s.split_once(':')?;
    let h: i32 = h.parse().ok()?;
    let m: i32 = m.parse().ok()?;
    if (0..24).contains(&h) && (0..60).contains(&m) {
        Some(h * 60 + m)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::testkit::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn enqueue_dedupes_on_key() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let a = enqueue_notification(&state, "u1", "daily_report", json!({"n": 1}), Some("dk-1"), crate::db::now_ms())
            .await
            .unwrap();
        let b = enqueue_notification(&state, "u1", "daily_report", json!({"n": 2}), Some("dk-1"), crate::db::now_ms())
            .await
            .unwrap();
        assert!(a.is_some());
        assert!(b.is_none(), "同 dedupe_key 第二次应被去重");
        let n = count(&state.db, "SELECT COUNT(*) FROM notification_outbox").await;
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn deliver_marks_sent() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let id = enqueue_notification(&state, "u1", "daily_report", json!({}), None, crate::db::now_ms())
            .await
            .unwrap()
            .unwrap();
        let outcome = deliver(&state, &id).await.unwrap();
        assert_eq!(outcome, DeliveryOutcome::Sent);
        // 二次投递幂等（已 sent）。
        assert_eq!(deliver(&state, &id).await.unwrap(), DeliveryOutcome::Skipped);
        let sent = count(&state.db, "SELECT COUNT(*) FROM notification_outbox WHERE status = 'sent'").await;
        assert_eq!(sent, 1);
    }

    #[tokio::test]
    async fn unsubscribed_user_suppressed() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        sqlx::query(
            "INSERT INTO notification_preferences (user_id, channels_json, quiet_hours, unsubscribed, updated_at) \
             VALUES ('u1', '{}', '', 1, ?)",
        )
        .bind(crate::db::now_ms())
        .execute(&state.db)
        .await
        .unwrap();
        let id = enqueue_notification(&state, "u1", "daily_report", json!({}), None, crate::db::now_ms())
            .await
            .unwrap()
            .unwrap();
        let outcome = deliver(&state, &id).await.unwrap();
        assert_eq!(outcome, DeliveryOutcome::Suppressed);
        let suppressed = count(&state.db, "SELECT COUNT(*) FROM notification_outbox WHERE status = 'suppressed'").await;
        assert_eq!(suppressed, 1);
    }

    #[tokio::test]
    async fn put_then_get_preferences() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let tk = token(&state, "u1");
        let app = crate::app::build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/me/notification-preferences")
                    .header("authorization", format!("Bearer {tk}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"channels": {"push": true}, "quietHours": "23:00-07:00", "unsubscribed": true}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let app = crate::app::build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/me/notification-preferences")
                    .header("authorization", format!("Bearer {tk}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["unsubscribed"], true);
        assert_eq!(v["quietHours"], "23:00-07:00");
    }

    #[test]
    fn quiet_hours_parse_and_activity() {
        assert!(parse_quiet("").is_none());
        assert!(parse_quiet("23:00-07:00").is_some());
        let day = 86_400_000_i64;
        // 02:00 UTC 落在 23:00-07:00 静默窗口内。
        let at_02 = 2 * 3_600_000;
        assert!(quiet_resume_ms("23:00-07:00", at_02).is_some());
        // 12:00 UTC 不在窗口内。
        assert!(quiet_resume_ms("23:00-07:00", 12 * 3_600_000).is_none());
        let _ = day;
    }
}
