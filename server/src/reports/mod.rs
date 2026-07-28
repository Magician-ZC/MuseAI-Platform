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
        "SELECT id FROM daily_reports WHERE world_id = $1 AND character_id = $2 AND report_day = $3",
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
        },
        // 🔴 AI 生成标识（平台红线 §0.6）。⚠️ 它与 `provenanceLegend` **不是一回事**：
        // 后者标的是「这条信息从哪来」（供玩家判断可信度），前者标的是「这是 AI 生成内容」
        // （合规义务，面向监管与知情权）。日报的独白与摘要都出自模型，故整份带标识。
        "aiLabel": { "visible": true }
    });

    let id = crate::db::new_id("rpt");
    let res = sqlx::query(
        "INSERT INTO daily_reports (id, world_id, user_id, character_id, report_day, content_json, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&id)
    .bind(world_id)
    .bind(user_id)
    .bind(character_id)
    .bind(report_day)
    .bind(content.to_string())
    .bind(crate::db::now_ms())
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => {}
        // Low：并发下他者已生成（idx_daily_report_unique 冲突）→ 回退既有 id（幂等），不重复入队通知。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            let existing: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM daily_reports WHERE world_id = $1 AND character_id = $2 AND report_day = $3",
            )
            .bind(world_id)
            .bind(character_id)
            .bind(report_day)
            .fetch_optional(&state.db)
            .await?;
            if let Some((existing_id,)) = existing {
                return Ok(existing_id);
            }
            // 冲突却查不到既有行（不应发生）：还原为 sqlx 错误上抛。
            return Err(sqlx::Error::Database(e).into());
        }
        Err(e) => return Err(e.into()),
    }

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

/// 按 principal 可见性 **且** 角色参与度聚合当日高光与关系变化（N-4）。两层过滤：
/// ① principal 可见性——public 或 audience_json 含本 user（硬隔离）；
/// ② 角色隔离——actors_json 含 character_id，确保一人多角色时各角色日报内容相异（§2.5 《你的角色昨日人生》）。
/// best-effort：world_events 由 S2 落库，形状按 §9.4。
///
/// 🔴 **排序键是 `sequence` 而不是 `occurred_at`**（日窗过滤仍用 `occurred_at`）：
/// `events::insert_events_tx` 给一个 tick 的整批事件写**同一个** `now_ms()`，
/// 故 `occurred_at` 在批内全同——并列是结构性必然，不是偶发。而 `LIMIT 200` 会在并列处截断，
/// 单键下"取哪 200 条"在 PG 上是任意的，且这 200 条直接决定日报正文。
/// `world_events.sequence` 是同世界内单调递增的落库序（本查询已按 `world_id` 等值过滤），
/// 正是这里要的因果序，且有 `idx_world_events_world(world_id, sequence)` 支撑。
/// 末位再挂 `id`：该索引当前**非唯一**，并发分配理论上可撞号（见 docs/VALIDATION.md §3.3
/// 「相邻风险」），补 `id` 让本站点的确定性不依赖那条尚未加固的不变量。
async fn aggregate_visible(
    db: &sqlx::AnyPool,
    world_id: &str,
    user_id: &str,
    character_id: &str,
    report_day: &str,
) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>), ApiError> {
    let (start, end) = day_bounds(report_day);
    let rows: Vec<(String, String, String, String, Option<String>, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, event_type, visibility, actors_json, audience_json, public_projection_json, private_projections_json, occurred_at \
         FROM world_events WHERE world_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         AND moderation = 'approved' ORDER BY sequence ASC, id ASC LIMIT 200",
    )
    .bind(world_id)
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;

    let mut highlights = Vec::new();
    let mut relations = Vec::new();
    for (id, etype, visibility, actors, audience, public_proj, private_proj, _at) in rows {
        // N-4 角色隔离：仅纳入本角色作为 actor 参与的事件（actors_json 存角色 id / cloud_character_id）。
        // 注：audience_json 是 principal(user) 粒度，无法区分同一 user 的不同角色，故按 actor 参与度隔离。
        if !character_participates(&actors, character_id) {
            continue;
        }
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

/// 本角色是否作为 actor 参与该事件（N-4 角色隔离闸）。actors_json 为角色 id 字符串数组。
fn character_participates(actors_json: &str, character_id: &str) -> bool {
    serde_json::from_str::<Vec<String>>(actors_json)
        .map(|actors| actors.iter().any(|a| a == character_id))
        .unwrap_or(false)
}

fn proj_summary(proj_json: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(proj_json?).ok()?;
    v.get("summary").and_then(|s| s.as_str()).map(|s| s.to_string())
}

fn private_summary(proj_json: Option<&str>, user_id: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(proj_json?).ok()?;
    let arr = v.as_array()?;
    // N-3：严格匹配受众含本 principal 的私有投影；无匹配返回 None，**绝不回退第一条**——
    // 未来逐 principal 差异化投影下，回退 first() 会把他人的私密视角泄漏给本用户。
    let pick = arr.iter().find(|p| {
        p.get("audiencePrincipalIds")
            .and_then(|a| a.as_array())
            .map(|ids| ids.iter().any(|x| x.as_str() == Some(user_id)))
            .unwrap_or(false)
    })?;
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
    /// 复合游标的第二段（上一页末行的 `id`），见 `crate::pagination`。
    #[serde(default, rename = "cursorId")]
    cursor_id: Option<String>,
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
        // opened_at 按单份计（Low）：此处标记的每一份都在下方 SELECT 以「全量 content」随本响应返回，
        // 故按日打开 = 逐份打开当日全部报告，不产生「标记了却未返回」的虚高。单份精确打开另见 open_report。
        sqlx::query("UPDATE daily_reports SET opened_at = $1 WHERE user_id = $2 AND report_day = $3 AND opened_at IS NULL")
            .bind(crate::db::now_ms())
            .bind(&user.user_id)
            .bind(date)
            .execute(&state.db)
            .await?;
        let rows: Vec<(String, String, String, String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT id, world_id, character_id, content_json, opened_at, created_at FROM daily_reports \
             WHERE user_id = $1 AND report_day = $2 ORDER BY created_at DESC, id DESC",
        )
        .bind(&user.user_id)
        .bind(date)
        .fetch_all(&state.db)
        .await?;
        let reports: Vec<_> = rows.into_iter().map(detail_json).collect();
        return Ok(Json(json!({ "reports": reports })));
    }

    // 列表（浏览，不算打开）。复合游标 `(created_at, id)`，见 `crate::pagination`：
    // 一个用户当日在多个世界的日报由同一批生成，`created_at` 并列是常态，
    // 单列游标会把横跨页边界的那组同值行整组丢掉。
    let rows: Vec<(String, String, String, String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, world_id, character_id, report_day, opened_at, created_at FROM daily_reports \
         WHERE user_id = $1 AND ($2 IS NULL OR created_at < $3 OR (created_at = $4 AND id < $5)) \
         ORDER BY created_at DESC, id DESC LIMIT 30",
    )
    .bind(&user.user_id)
    .bind(q.cursor)
    .bind(q.cursor)
    .bind(q.cursor)
    .bind(crate::pagination::cursor_id_bound(q.cursor_id.as_deref()))
    .fetch_all(&state.db)
    .await?;
    let next = rows.last().map(|r| r.5);
    let next_id = rows.last().map(|r| r.0.clone());
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
    Ok(Json(json!({"reports": reports, "nextCursor": next, "nextCursorId": next_id})))
}

async fn open_report(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 打开单份日报：回写 opened_at（首次），北极星埋点。
    sqlx::query("UPDATE daily_reports SET opened_at = $1 WHERE id = $2 AND user_id = $3 AND opened_at IS NULL")
        .bind(crate::db::now_ms())
        .bind(&id)
        .bind(&user.user_id)
        .execute(&state.db)
        .await?;
    let row: Option<(String, String, String, String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, world_id, character_id, content_json, opened_at, created_at FROM daily_reports WHERE id = $1 AND user_id = $2",
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
        sqlx::query_scalar::<_, Option<i64>>("SELECT opened_at FROM daily_reports WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    }

    /// 🔴 AI 生成标识（平台红线 §0.6，《AI 生成合成内容标识办法》）。
    ///
    /// 日报的独白与摘要都出自模型，此前这份 content 里**没有标识**——
    /// 而同为叙事读取面的 `events` / `clips` / `worlds` 都有。
    /// ⚠️ 它与 `provenanceLegend` 不是一回事：后者标「这条信息从哪来」（供玩家判断可信度），
    /// 前者标「这是 AI 生成内容」（合规义务）。两者都要，缺一不可互相顶替。
    #[tokio::test]
    async fn daily_report_carries_the_ai_label() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let id = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let raw: String = sqlx::query_scalar("SELECT content_json FROM daily_reports WHERE id = $1")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["aiLabel"]["visible"], json!(true), "🔴 日报必须带 AI 生成标识");
        assert!(
            v["provenanceLegend"]["model_inference"].is_string(),
            "来源图例照常在——它与 AI 标识并存，不互相顶替"
        );
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
        let content: String = sqlx::query_scalar::<_, String>("SELECT content_json FROM daily_reports WHERE id = $1")
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
        // 一条 c1 参与、但仅对 u2 可见的私有事件，落在 2026-07-20。
        // c1 作为 actor 过 N-4 角色隔离闸，从而真正考验 principal 受众硬隔离（u1 不在 audience → 不可见）。
        let at = day_bounds("2026-07-20").0 + 1000;
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
             visibility, audience_json, private_projections_json, moderation, ai_label, occurred_at) \
             VALUES ('ev1', 'w1', 1, 1, 'de1', 'dialogue', '[\"c1\"]', 'private', $1, $2, 'approved', 1, $3)",
        )
        .bind(json!(["u2"]).to_string())
        .bind(json!([{"audiencePrincipalIds": ["u2"], "summary": "u2 的秘密"}]).to_string())
        .bind(at)
        .execute(&state.db)
        .await
        .unwrap();

        let id = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let content: String = sqlx::query_scalar::<_, String>("SELECT content_json FROM daily_reports WHERE id = $1")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["highlights"].as_array().unwrap().len(), 0, "u1 不应看到 u2 的私有事件");
    }

    async fn content_of(db: &sqlx::AnyPool, id: &str) -> String {
        sqlx::query_scalar::<_, String>("SELECT content_json FROM daily_reports WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    }

    // ---------- PG 排序稳定性（docs/VALIDATION.md §3.3） ----------

    /// 日报素材的排序键是 `world_events.sequence`，**不是** `occurred_at`。
    ///
    /// fixture 有意做成**时钟倒挂**（`occurred_at` 随 sequence 递减）：这是唯一能把
    /// "哪一列说了算"隔离出来的形状。真实成因不是时钟倒转，而是
    /// `events::insert_events_tx` 给一个 tick 的整批事件写**同一个** `now_ms()` ——
    /// `occurred_at` 批内全同，排不动任何东西，谁在前谁在后就交给了数据库的心情
    /// （SQLite 恰好按索引序稳定返回，PG 不保证）。末两条同 `occurred_at` 就是这个真实形状。
    ///
    /// 旧口径 `ORDER BY occurred_at ASC` 在本 fixture 上会得到 4、3、2、1（或 3、4、2、1）；
    /// 新口径必须得到 1、2、3、4 —— 世界内的因果序。
    #[tokio::test]
    async fn highlights_follow_event_sequence_not_the_wall_clock() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let base = day_bounds("2026-07-20").0;
        // (sequence, occurred_at)：时钟递减；seq 3、4 共用一个时刻（同一 tick 的整批落库）。
        for (seq, at) in [(1_i64, base + 4000), (2, base + 3000), (3, base + 1000), (4, base + 1000)] {
            sqlx::query(
                "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
                 actors_json, visibility, public_projection_json, moderation, ai_label, occurred_at) \
                 VALUES ($1, 'w1', 1, $2, $3, 'dialogue', '[\"c1\"]', 'public', $4, 'approved', 1, $5)",
            )
            .bind(format!("ev{seq}"))
            .bind(seq)
            .bind(format!("de{seq}"))
            .bind(json!({ "summary": format!("seq-{seq}") }).to_string())
            .bind(at)
            .execute(&state.db)
            .await
            .unwrap();
        }

        let id = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&content_of(&state.db, &id).await).unwrap();
        let order: Vec<&str> = v["highlights"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["summary"].as_str().unwrap())
            .collect();
        assert_eq!(
            order,
            vec!["seq-1", "seq-2", "seq-3", "seq-4"],
            "日报素材必须按世界内单调的 sequence 排；occurred_at 是墙上时钟，一个 tick 内还是常量"
        );
    }

    /// 复合游标 `(created_at, id)` 不丢行：同毫秒并列行横跨页边界时，第二页必须接得上。
    ///
    /// 播种 31 份 `created_at` **完全相同**的日报（一个用户在 31 个世界的当日日报由同一批生成，
    /// 共用一个 `now_ms()`——这是常态不是巧合）。首页取 30 条，第 31 条恰好落在并列组的另一侧：
    /// - 只回传 `cursor` 的旧客户端：`created_at < cursor` 严格小于 ⇒ 第二页空 ⇒ **那一条永远取不到**；
    /// - 回传 `cursor` + `cursorId`：`(created_at = c AND id < cid)` 把并列组精确切开 ⇒ 取到且不重。
    #[tokio::test]
    async fn keyset_cursor_recovers_the_row_a_single_column_cursor_would_drop() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let at = crate::db::now_ms();
        let mut all: Vec<String> = Vec::new();
        for i in 0..31 {
            let id = crate::db::new_id("rep");
            sqlx::query(
                "INSERT INTO daily_reports (id, world_id, user_id, character_id, report_day, content_json, created_at) \
                 VALUES ($1, $2, 'u1', 'c1', $3, '{}', $4)",
            )
            .bind(&id)
            .bind(format!("w{i}"))
            .bind(format!("2026-07-{:02}", (i % 28) + 1))
            .bind(at) // 整批同毫秒
            .execute(&state.db)
            .await
            .unwrap();
            all.push(id);
        }

        let tk = token(&state, "u1");
        let app = crate::app::build_router(state.clone());
        let get = |uri: String| {
            let app = app.clone();
            let tk = tk.clone();
            async move {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method("GET")
                            .uri(uri)
                            .header("authorization", format!("Bearer {tk}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
            }
        };

        let page1 = get("/api/me/reports".to_string()).await;
        let ids1: Vec<String> = page1["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids1.len(), 30, "首页满页");
        let cursor = page1["nextCursor"].as_i64().unwrap();
        let cursor_id = page1["nextCursorId"].as_str().unwrap().to_string();
        assert_eq!(cursor, at, "游标第一段 = 末行 created_at");

        // ① 旧客户端（只带 cursor）：行为与本次改动前逐字节一致——第二页空，那一条丢了。
        let legacy = get(format!("/api/me/reports?cursor={cursor}")).await;
        assert_eq!(
            legacy["reports"].as_array().unwrap().len(),
            0,
            "单列游标在并列组上必然空翻页——这正是要修的丢行，同时也证明旧客户端零行为变化"
        );

        // ② 复合游标：补回第 31 条，且与首页零重叠。
        let page2 = get(format!("/api/me/reports?cursor={cursor}&cursorId={cursor_id}")).await;
        let ids2: Vec<String> = page2["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids2.len(), 1, "并列组被 id 精确切开，剩下的那条必须出现");
        assert!(ids2.iter().all(|id| !ids1.contains(id)), "两页不得重叠");
        let mut seen = ids1;
        seen.extend(ids2);
        seen.sort();
        let mut expect = all;
        expect.sort();
        assert_eq!(seen, expect, "两页并起来必须恰好是全集：不重不漏");
    }

    // ---------- N-3：私有投影不回退他人视角 ----------

    #[tokio::test]
    async fn private_summary_never_leaks_other_principal() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let at = day_bounds("2026-07-20").0 + 1000;
        // 受众含 u1（visible_private=true），c1 为 actor（过 N-4）；但私有投影数组只有 u2 的视角。
        // 旧逻辑 .or_else(first()) 会把 u2 的私密视角泄漏给 u1；N-3 修复后应返回 None → 回退中性 etype。
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
             visibility, audience_json, private_projections_json, moderation, ai_label, occurred_at) \
             VALUES ('evp', 'w1', 1, 1, 'de-p', 'dialogue', '[\"c1\"]', 'private', $1, $2, 'approved', 1, $3)",
        )
        .bind(json!(["u1", "u2"]).to_string())
        .bind(json!([{"audiencePrincipalIds": ["u2"], "summary": "u2-private-secret"}]).to_string())
        .bind(at)
        .execute(&state.db)
        .await
        .unwrap();

        let id = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let content = content_of(&state.db, &id).await;
        assert!(
            !content.contains("u2-private-secret"),
            "绝不回退到他人 principal 的私有投影（N-3）"
        );
    }

    // ---------- N-4：日报按角色隔离 ----------

    #[tokio::test]
    async fn reports_isolated_per_character() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let at = day_bounds("2026-07-20").0 + 1000;
        // 同一用户 u1 的两个角色 c1/c2，各自参与一条公开事件。
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
             visibility, public_projection_json, moderation, ai_label, occurred_at) \
             VALUES ('evc1', 'w1', 1, 1, 'de-c1', 'dialogue', '[\"c1\"]', 'public', $1, 'approved', 1, $2)",
        )
        .bind(json!({"summary": "highlight-of-c1"}).to_string())
        .bind(at)
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
             visibility, public_projection_json, moderation, ai_label, occurred_at) \
             VALUES ('evc2', 'w1', 1, 2, 'de-c2', 'dialogue', '[\"c2\"]', 'public', $1, 'approved', 1, $2)",
        )
        .bind(json!({"summary": "highlight-of-c2"}).to_string())
        .bind(at + 1)
        .execute(&state.db)
        .await
        .unwrap();

        let r1 = generate_report(&state, "w1", "u1", "c1", "2026-07-20").await.unwrap();
        let r2 = generate_report(&state, "w1", "u1", "c2", "2026-07-20").await.unwrap();
        assert_ne!(r1, r2, "两角色应各自生成独立日报");
        let c1 = content_of(&state.db, &r1).await;
        let c2 = content_of(&state.db, &r2).await;
        assert!(c1.contains("highlight-of-c1"), "c1 日报应含 c1 的事件");
        assert!(!c1.contains("highlight-of-c2"), "c1 日报不应含 c2 的事件（角色隔离 N-4）");
        assert!(c2.contains("highlight-of-c2"), "c2 日报应含 c2 的事件");
        assert!(!c2.contains("highlight-of-c1"), "c2 日报不应含 c1 的事件（角色隔离 N-4）");
    }
}
