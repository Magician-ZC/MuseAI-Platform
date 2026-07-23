//! 通知 outbox（S3）：日报/同意等通知的发布与发送解耦、退订、静默、去重、重试、可审计（§9.2）。
//!
//! - enqueue_notification：写 outbox(pending) + 入队 topic="notify"；dedupe_key 靠唯一索引去重（C-6，同键只发一次）。
//! - deliver（worker 单步，可直测）：查 preferences → 退订 suppressed / 静默时段延后 / dev "发送"=写 sent+tracing；
//!   失败 attempts+1，<3 退避重入队（同步推 due_at），=3 标记 failed。事务/安全类（consent）豁免退订与静默（N-2）。
//! - rescan_pending：重扫 pending 且到期行重新入队，恢复孤儿与重启丢失（N-1）。
//! - spawn_outbox_worker：pop→deliver 循环 + 定时 rescan 恢复循环；deliver 出错延时重推不丢 job。
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
///
/// C-6：去重靠 `idx_outbox_dedupe_unique` 唯一索引，而非「先 SELECT 再 INSERT」——后者存在 TOCTOU，
/// 并发同键可双插。此处直接 INSERT，唯一冲突即视为已入队（返回 None），收口竞态窗口。
pub async fn enqueue_notification(
    state: &AppState,
    user_id: &str,
    kind: &str,
    payload: serde_json::Value,
    dedupe_key: Option<&str>,
    due_ms: i64,
) -> Result<Option<String>, ApiError> {
    let id = crate::db::new_id("ntf");
    let res = sqlx::query(
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
    .await;
    match res {
        Ok(_) => {}
        // 并发同 dedupe_key：唯一索引兜底去重（冲突即跳过），返回 None 表示已由他者入队。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => return Ok(None),
        Err(e) => return Err(e.into()),
    }

    crate::queue::push_json(state.queue.as_ref(), NOTIFY_TOPIC, &NotifyJob { outbox_id: id.clone() }, due_ms).await;
    Ok(Some(id))
}

/// 消费一条 outbox（worker 单步）。幂等：已非 pending → Skipped。
pub async fn deliver(state: &AppState, outbox_id: &str) -> Result<DeliveryOutcome, ApiError> {
    let row: Option<(String, String, String, i64)> =
        sqlx::query_as("SELECT user_id, kind, status, attempts FROM notification_outbox WHERE id = ?")
            .bind(outbox_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((user_id, kind, status, attempts)) = row else {
        return Ok(DeliveryOutcome::Skipped);
    };
    if status != "pending" {
        return Ok(DeliveryOutcome::Skipped);
    }

    // 偏好：退订 / 静默时段。N-2：事务/安全类（不可逆事件同意请求）豁免退订与静默，始终尽快送达。
    let essential = is_essential_kind(&kind);
    let pref: Option<(i64, String)> =
        sqlx::query_as("SELECT unsubscribed, quiet_hours FROM notification_preferences WHERE user_id = ?")
            .bind(&user_id)
            .fetch_optional(&state.db)
            .await?;
    if let Some((unsub, quiet)) = pref {
        if unsub != 0 && !essential {
            sqlx::query("UPDATE notification_outbox SET status = 'suppressed' WHERE id = ?")
                .bind(outbox_id)
                .execute(&state.db)
                .await?;
            tracing::info!(outbox_id, user_id, "notification 已退订，抑制");
            return Ok(DeliveryOutcome::Suppressed);
        }
        // 静默时段仅延后非事务类；事务类不延后，避免同意通知被推迟到 TTL 之外造成保守失效。
        if !essential {
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
            // due_at 同步推到退避点：rescan 以 due_at 为准，避免退避窗口内被重扫提前重推。
            let backoff = crate::db::now_ms() + 60_000 * next;
            sqlx::query("UPDATE notification_outbox SET attempts = ?, due_at = ? WHERE id = ?")
                .bind(next)
                .bind(backoff)
                .bind(outbox_id)
                .execute(&state.db)
                .await?;
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

/// 事务/安全类通知：不可逆事件（角色永久死亡/退场/羁绊等）的同意请求，须始终送达——
/// 豁免全局退订与静默时段（N-2）。营销/产品类（日报等）尊重退订与静默。
/// 分类以 kind 前缀 `consent` 判定，覆盖 create_consent 发出的 `consent_request` 及后续同类事件。
fn is_essential_kind(kind: &str) -> bool {
    kind.starts_with("consent")
}

/// 重扫 pending 且到期的 outbox 行并重新入队（N-1，对标 runtime schedule_due_ticks 的 pending 恢复）。
///
/// 覆盖两类丢失：① deliver 出错后 job 已出队但行仍 pending 的孤儿；② MemQueue 进程重启后内存队列清空、
/// 而 DB 中 pending 行仍在。以 due_at<=now 为闸（静默延后/退避中的行 due_at 在未来，不会被提前重推）。
/// 重复入队被 deliver 幂等吸收（已非 pending → Skipped）。
pub async fn rescan_pending(state: &AppState) -> Result<u64, ApiError> {
    let now = crate::db::now_ms();
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM notification_outbox WHERE status = 'pending' AND due_at <= ? ORDER BY due_at ASC LIMIT 500",
    )
    .bind(now)
    .fetch_all(&state.db)
    .await?;
    let n = rows.len() as u64;
    for (id,) in rows {
        crate::queue::push_json(state.queue.as_ref(), NOTIFY_TOPIC, &NotifyJob { outbox_id: id }, now).await;
    }
    Ok(n)
}

pub fn spawn_outbox_worker(state: AppState) {
    // 恢复循环：启动即重扫一次 + 定时重扫 pending due（孤儿 + 重启丢失）。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let poll_ms: u64 = std::env::var("MUSE_OUTBOX_RESCAN_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60_000);
            loop {
                if let Err(e) = rescan_pending(&state).await {
                    tracing::warn!(error = %e, "outbox pending 重扫失败");
                }
                tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
            }
        });
    }
    // 消费循环：pop → deliver；deliver 出错则延时重推（不丢 job，行仍 pending 由重扫兜底）。
    tokio::spawn(async move {
        loop {
            let Some(job) = crate::queue::pop_json::<NotifyJob>(state.queue.as_ref(), NOTIFY_TOPIC).await else {
                continue;
            };
            if let Err(e) = deliver(&state, &job.outbox_id).await {
                tracing::error!(error = %e, outbox_id = %job.outbox_id, "outbox 投递失败，延时重推");
                let retry_at = crate::db::now_ms() + 30_000;
                crate::queue::push_json(
                    state.queue.as_ref(),
                    NOTIFY_TOPIC,
                    &NotifyJob { outbox_id: job.outbox_id },
                    retry_at,
                )
                .await;
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

/// 若当前处于静默时段，返回应恢复投递的毫秒时间戳；否则 None。格式 "HH:MM-HH:MM"，可跨夜。
///
/// 注意（Low）：窗口按 **UTC** 解释——`now_ms` 直接对 86_400_000 取模求当日分钟数，未做用户时区偏移。
/// 真实运营下应在 notification_preferences 增设 tz 列并按用户时区换算；此处 dev 态先固定 UTC，
/// 事务/安全类（consent）已在 deliver 中豁免静默，不受此限制。
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

    // ---------- C-6：唯一索引去重 ----------

    #[tokio::test]
    async fn dedupe_index_rejects_duplicate_raw_insert() {
        // 直插两条同 dedupe_key 行：第二条应被 idx_outbox_dedupe_unique 拒绝（证明索引已建且生效）。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let now = crate::db::now_ms();
        let ins = |id: &'static str| {
            sqlx::query(
                "INSERT INTO notification_outbox (id, user_id, kind, payload_json, status, attempts, dedupe_key, due_at, created_at) \
                 VALUES (?, 'u1', 'daily_report', '{}', 'pending', 0, 'dk-x', ?, ?)",
            )
            .bind(id)
            .bind(now)
            .bind(now)
            .execute(&state.db)
        };
        assert!(ins("n1").await.is_ok());
        assert!(ins("n2").await.is_err(), "同 dedupe_key 第二次直插应被唯一索引拒绝");
    }

    #[tokio::test]
    async fn null_dedupe_keys_not_deduped() {
        // 部分索引 WHERE dedupe_key IS NOT NULL：无去重键的通知允许并存多条。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let now = crate::db::now_ms();
        let a = enqueue_notification(&state, "u1", "daily_report", json!({}), None, now).await.unwrap();
        let b = enqueue_notification(&state, "u1", "daily_report", json!({}), None, now).await.unwrap();
        assert!(a.is_some() && b.is_some(), "无 dedupe_key 的通知不应互相去重");
        assert_eq!(count(&state.db, "SELECT COUNT(*) FROM notification_outbox").await, 2);
    }

    // ---------- N-2：事务/安全类豁免退订 ----------

    #[tokio::test]
    async fn consent_notification_survives_unsubscribe() {
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
        // 日报（营销/产品类）→ 退订压制。
        let rep = enqueue_notification(&state, "u1", "daily_report", json!({}), None, crate::db::now_ms())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(deliver(&state, &rep).await.unwrap(), DeliveryOutcome::Suppressed);
        // consent_request（不可逆事件）→ 豁免退订，正常送达。
        let cs = enqueue_notification(&state, "u1", "consent_request", json!({}), None, crate::db::now_ms())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(deliver(&state, &cs).await.unwrap(), DeliveryOutcome::Sent);
    }

    #[test]
    fn essential_kind_classification() {
        assert!(is_essential_kind("consent_request"));
        assert!(is_essential_kind("consent_reminder"));
        assert!(!is_essential_kind("daily_report"));
    }

    // ---------- N-1：pending 重扫恢复 ----------

    #[tokio::test]
    async fn rescan_recovers_orphan_pending() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let now = crate::db::now_ms();
        // 孤儿：直接插 pending 行，不入队（模拟 deliver 出错留下 / MemQueue 重启丢失）。
        sqlx::query(
            "INSERT INTO notification_outbox (id, user_id, kind, payload_json, status, attempts, due_at, created_at) \
             VALUES ('orphan1', 'u1', 'daily_report', '{}', 'pending', 0, ?, ?)",
        )
        .bind(now - 1)
        .bind(now)
        .execute(&state.db)
        .await
        .unwrap();

        let recovered = rescan_pending(&state).await.unwrap();
        assert_eq!(recovered, 1, "应重扫到 1 条 pending due 行");

        // 重扫后应可从队列取出并成功投递（不再是孤儿）。
        let job = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::queue::pop_json::<NotifyJob>(state.queue.as_ref(), NOTIFY_TOPIC),
        )
        .await
        .expect("重扫后队列应有 job")
        .expect("job 解析成功");
        assert_eq!(job.outbox_id, "orphan1");
        assert_eq!(deliver(&state, &job.outbox_id).await.unwrap(), DeliveryOutcome::Sent);
    }

    #[tokio::test]
    async fn rescan_skips_future_due() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let now = crate::db::now_ms();
        // 未到期（静默延后 / 退避中）的 pending 行 due_at 在未来，不应被提前重推。
        sqlx::query(
            "INSERT INTO notification_outbox (id, user_id, kind, payload_json, status, attempts, due_at, created_at) \
             VALUES ('future1', 'u1', 'daily_report', '{}', 'pending', 0, ?, ?)",
        )
        .bind(now + 3_600_000)
        .bind(now)
        .execute(&state.db)
        .await
        .unwrap();
        assert_eq!(rescan_pending(&state).await.unwrap(), 0, "未到期的 pending 行不应被重扫");
    }
}
