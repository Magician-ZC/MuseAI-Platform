//! 日报系统（S3）：《你的角色昨日人生》——放置房核心产品（平台规格 §2.5）。
//!
//! generate_report（runtime 每日边界调）：按成员可见投影聚合 高光≤5 + 关系变化 + 一句独白
//!   （1 次模型调用，此处 mock/占位）→ daily_reports（唯一键 world+character+day，幂等）→ outbox 通知。
//!   content 明确区分：公开事实 public_fact / 角色私密视角 private_view / 模型推断 model_inference（§2.5）。
//! GET /me/reports?cursor=&date=：列表 / 按日详情；打开详情时回写 opened_at（P4a 北极星埋点）。

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::notifications::enqueue_notification;

const MAX_HIGHLIGHTS: usize = 5;

/// 生成某成员某日日报。幂等（唯一键 world+character+day）：已存在则直接返回其 id。
pub async fn generate_report(
    state: &AppState,
    world_id: &str,
    user_id: &str,
    character_id: &str,
    report_day: &str,
) -> Result<String, ApiError> {
    if let Some((id,)) = sqlx::query_as::<_, (String,)>(
        "SELECT id FROM daily_reports WHERE world_id = ? AND character_id = ? AND report_day = ?",
    )
    .bind(world_id)
    .bind(character_id)
    .bind(report_day)
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(id);
    }

    let (highlights, relation_changes) =
        aggregate_visible(&state.db, world_id, user_id, character_id, report_day).await?;
    let monologue = mock_model_monologue(character_id, highlights.len());

    let content = json!({
        "reportDay": report_day,
        "characterId": character_id,
        "highlights": highlights,            // 每条含 kind: public_fact | private_view
        "relationChanges": relation_changes, // kind: public_fact
        "monologue": { "text": monologue, "kind": "model_inference" },
        "provenanceLegend": {
            "public_fact": "公开事实",
            "private_view": "角色私密视角（仅你可见）",
            "model_inference": "模型推断"
        }
    });

    let id = crate::db::new_id("rpt");
    sqlx::query(
        "INSERT INTO daily_reports (id, world_id, user_id, character_id, report_day, content_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(world_id)
    .bind(user_id)
    .bind(character_id)
    .bind(report_day)
    .bind(content.to_string())
    .bind(crate::db::now_ms())
    .execute(&state.db)
    .await?;

    // 发布与发送解耦：入 outbox，去重键保证同世界同角色同日只发一次。
    let dk = format!("report:{world_id}:{character_id}:{report_day}");
    enqueue_notification(
        state,
        user_id,
        "daily_report",
        json!({"reportId": id, "worldId": world_id, "reportDay": report_day}),
        Some(&dk),
        crate::db::now_ms(),
    )
    .await?;

    Ok(id)
}

/// 按用户（角色主人）可见性聚合当日高光与关系变化。best-effort：world_events 由 S2 落库，形状按 §9.4。
async fn aggregate_visible(
    db: &sqlx::AnyPool,
    world_id: &str,
    user_id: &str,
    _character_id: &str,
    report_day: &str,
) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>), ApiError> {
    let (start, end) = day_bounds(report_day);
    let rows: Vec<(String, String, String, Option<String>, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, event_type, visibility, audience_json, public_projection_json, private_projections_json, occurred_at \
         FROM world_events WHERE world_id = ? AND occurred_at >= ? AND occurred_at < ? \
         AND moderation = 'approved' ORDER BY occurred_at ASC LIMIT 200",
    )
    .bind(world_id)
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;

    let mut highlights = Vec::new();
    let mut relations = Vec::new();
    for (id, etype, visibility, audience, public_proj, private_proj, _at) in rows {
        let visible_private = audience
            .as_deref()
            .and_then(|a| serde_json::from_str::<Vec<String>>(a).ok())
            .map(|ids| ids.iter().any(|p| p == user_id))
            .unwrap_or(false);

        let (kind, summary) = if visibility == "public" {
            ("public_fact", proj_summary(public_proj.as_deref()).unwrap_or_else(|| etype.clone()))
        } else if visible_private {
            ("private_view", private_summary(private_proj.as_deref(), user_id).unwrap_or_else(|| etype.clone()))
        } else {
            continue; // 对该用户不可见，硬隔离。
        };

        let card = json!({"eventId": id, "type": etype, "summary": summary, "kind": kind});
        if etype == "relation_changed" || etype == "alliance" || etype == "conflict" {
            if relations.len() < MAX_HIGHLIGHTS {
                relations.push(card.clone());
            }
        }
        if highlights.len() < MAX_HIGHLIGHTS {
            highlights.push(card);
        }
    }
    Ok((highlights, relations))
}

fn proj_summary(proj_json: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(proj_json?).ok()?;
    v.get("summary").and_then(|s| s.as_str()).map(|s| s.to_string())
}

fn private_summary(proj_json: Option<&str>, user_id: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(proj_json?).ok()?;
    let arr = v.as_array()?;
    // 优先取受众含本用户的私有投影，否则回退第一条。
    let pick = arr
        .iter()
        .find(|p| {
            p.get("audiencePrincipalIds")
                .and_then(|a| a.as_array())
                .map(|ids| ids.iter().any(|x| x.as_str() == Some(user_id)))
                .unwrap_or(false)
        })
        .or_else(|| arr.first())?;
    pick.get("summary").and_then(|s| s.as_str()).map(|s| s.to_string())
}

/// 1 次模型调用的占位：真实实现走世界钉住 prompt 生成角色独白。标注为 model_inference。
fn mock_model_monologue(character_id: &str, highlight_count: usize) -> String {
    if highlight_count == 0 {
        format!("（{character_id} 的独白·占位）今天是平静的一天，没有什么值得写进日记，但我依然在这里。")
    } else {
        format!("（{character_id} 的独白·占位）今天发生了 {highlight_count} 件事，我把它们悄悄收进了心里。")
    }
}

fn day_bounds(day: &str) -> (i64, i64) {
    use chrono::{NaiveDate, NaiveTime};
    match NaiveDate::parse_from_str(day, "%Y-%m-%d") {
        Ok(d) => {
            let start = d.and_time(NaiveTime::MIN).and_utc().timestamp_millis();
            (start, start + 86_400_000)
        }
        Err(_) => (0, i64::MAX),
    }
}

// ---------- 路由 ----------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/reports", get(list_or_detail))
        .route("/me/reports/{id}", get(open_report))
}

#[derive(Debug, Deserialize)]
struct ReportQuery {
    #[serde(default)]
    cursor: Option<i64>,
    #[serde(default)]
    date: Option<String>,
}

async fn list_or_detail(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ReportQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(date) = q.date.as_deref() {
        // 按日详情 = 打开：回写 opened_at（仅首次），北极星埋点。
        sqlx::query("UPDATE daily_reports SET opened_at = ? WHERE user_id = ? AND report_day = ? AND opened_at IS NULL")
            .bind(crate::db::now_ms())
            .bind(&user.user_id)
            .bind(date)
            .execute(&state.db)
            .await?;
        let rows: Vec<(String, String, String, String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT id, world_id, character_id, content_json, opened_at, created_at FROM daily_reports \
             WHERE user_id = ? AND report_day = ? ORDER BY created_at DESC",
        )
        .bind(&user.user_id)
        .bind(date)
        .fetch_all(&state.db)
        .await?;
        let reports: Vec<_> = rows.into_iter().map(detail_json).collect();
        return Ok(Json(json!({ "reports": reports })));
    }

    // 列表（浏览，不算打开）。
    let rows: Vec<(String, String, String, String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, world_id, character_id, report_day, opened_at, created_at FROM daily_reports \
         WHERE user_id = ? AND (? IS NULL OR created_at < ?) ORDER BY created_at DESC LIMIT 30",
    )
    .bind(&user.user_id)
    .bind(q.cursor)
    .bind(q.cursor)
    .fetch_all(&state.db)
    .await?;
    let next = rows.last().map(|r| r.5);
    let reports: Vec<_> = rows
        .into_iter()
        .map(|(id, world_id, character_id, report_day, opened_at, created_at)| {
            json!({
                "id": id,
                "worldId": world_id,
                "characterId": character_id,
                "reportDay": report_day,
                "opened": opened_at.is_some(),
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({"reports": reports, "nextCursor": next})))
}

async fn open_report(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 打开单份日报：回写 opened_at（首次），北极星埋点。
    sqlx::query("UPDATE daily_reports SET opened_at = ? WHERE id = ? AND user_id = ? AND opened_at IS NULL")
        .bind(crate::db::now_ms())
        .bind(&id)
        .bind(&user.user_id)
        .execute(&state.db)
        .await?;
    let row: Option<(String, String, String, String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, world_id, character_id, content_json, opened_at, created_at FROM daily_reports WHERE id = ? AND user_id = ?",
    )
    .bind(&id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let row = row.ok_or(ApiError::NotFound)?;
    Ok(Json(detail_json(row)))
}

fn detail_json(row: (String, String, String, String, Option<i64>, i64)) -> serde_json::Value {
    let (id, world_id, character_id, content, opened_at, created_at) = row;
    json!({
        "id": id,
        "worldId": world_id,
        "characterId": character_id,
        "content": serde_json::from_str::<serde_json::Value>(&content).unwrap_or_else(|_| json!({})),
        "openedAt": opened_at,
        "createdAt": created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::testkit::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn opened_at(db: &sqlx::AnyPool, id: &str) -> Option<i64> {
        sqlx::query_scalar::<_, Option<i64>>("SELECT opened_at FROM daily_reports WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn generate_is_idempotent() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let a = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let b = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        assert_eq!(a, b, "同键日报应幂等");
        let n = count(&state.db, "SELECT COUNT(*) FROM daily_reports").await;
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn content_distinguishes_provenance() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let id = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let content: String = sqlx::query_scalar::<_, String>("SELECT content_json FROM daily_reports WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["monologue"]["kind"], "model_inference");
        assert!(v["provenanceLegend"]["public_fact"].is_string());
        assert!(v["provenanceLegend"]["private_view"].is_string());
        assert!(v["highlights"].is_array());
    }

    #[tokio::test]
    async fn open_by_date_sets_opened_at() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let id = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        assert!(opened_at(&state.db, &id).await.is_none(), "生成后未打开");

        let tk = token(&state, "u1");
        let app = crate::app::build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/me/reports?date=2026-07-20")
                    .header("authorization", format!("Bearer {tk}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(opened_at(&state.db, &id).await.is_some(), "打开后应回写 opened_at（北极星埋点）");
    }

    #[tokio::test]
    async fn private_event_hidden_from_non_audience() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        // 一条仅对 u2 可见的私有事件，落在 2026-07-20。
        let at = day_bounds("2026-07-20").0 + 1000;
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
             visibility, audience_json, private_projections_json, moderation, ai_label, occurred_at) \
             VALUES ('ev1', 'w1', 1, 1, 'de1', 'dialogue', '[]', 'private', ?, ?, 'approved', 1, ?)",
        )
        .bind(json!(["u2"]).to_string())
        .bind(json!([{"audiencePrincipalIds": ["u2"], "summary": "u2 的秘密"}]).to_string())
        .bind(at)
        .execute(&state.db)
        .await
        .unwrap();

        let id = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let content: String = sqlx::query_scalar::<_, String>("SELECT content_json FROM daily_reports WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["highlights"].as_array().unwrap().len(), 0, "u1 不应看到 u2 的私有事件");
    }
}
