//! 高光切片管线（P6b，feature=arena）：TTS(DevTts) 旁白 + 条漫式分镜脚本 → 产物入对象存储。
//!
//! 高光判定：从 world_events 取 impact 大的事件（**仅 public**——切片是直播/传播素材，绝不取私有投影，§9.4）。
//! 按需生成（主播控制台/回合结束触发），不进 tick 关键路径。产物 = 切片清单 JSON（引用 TTS 音频 key + 分镜）。
//! 内部管线：无 HTTP 路由（列表读取见 livegate GET /arena/{worldId}/clips）。
//!
//! 本模块 API 由 arena 回合/主播控制台按需触发（P6a 跨 agent seam），非 test 构建下暂未被调用，
//! 故整体 allow(dead_code)（与 events::persist_events 同约定），待 runtime RoundInput 扩展后接线。
#![allow(dead_code)]

use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::providers::{DevTts, TtsProvider};

/// 高光判定选中的事件。
pub struct HighlightPick {
    pub event_id: String,
    pub summary: String,
    pub impact: i64,
}

/// 高光判定：仅在 public 事件中选 impact 最大者；同分取最新（sequence 更大）。无 public 事件返回 None。
pub async fn pick_highlight_event(
    state: &AppState,
    world_id: &str,
) -> Result<Option<HighlightPick>, ApiError> {
    // moderation 过滤（§15 第 2 层）：高光切片是直播/传播素材，外泄面最大——
    // 被运行时词库拦下的事件仍落库留痕供人审，但绝不可进切片。
    let rows = sqlx::query(
        "SELECT id, event_type, arbiter_note, public_projection_json, sequence \
         FROM world_events WHERE world_id = $1 AND visibility = 'public' \
         AND moderation = 'approved' ORDER BY sequence ASC",
    )
    .bind(world_id)
    .fetch_all(&state.db)
    .await?;

    let mut best: Option<HighlightPick> = None;
    for r in &rows {
        let id: String = r.try_get("id")?;
        let event_type: String = r.try_get("event_type")?;
        let arbiter_note: Option<String> = r.try_get("arbiter_note")?;
        let pj: Option<String> = r.try_get("public_projection_json")?;
        let projection: Value =
            pj.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
        let summary = projection.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let score = impact_score(&event_type, arbiter_note.is_some(), &projection);
        // >= 让同分中 sequence 更大者胜出（取最新高光）。
        if best.as_ref().map(|b| score >= b.impact).unwrap_or(true) {
            best = Some(HighlightPick { event_id: id, summary, impact: score });
        }
    }
    Ok(best)
}

/// impact 打分：projection 若带显式 `impact` 数值则优先；否则按事件类型 + 仲裁标记启发式。
fn impact_score(event_type: &str, has_arbiter_note: bool, projection: &Value) -> i64 {
    if let Some(n) = projection.get("impact").and_then(Value::as_i64) {
        return n;
    }
    let base = match event_type {
        "consent_request" => 100, // 死亡/永久退出/重大关系变更 —— 最高光
        "status" => 60,           // 关系/资源变化
        "action" => 40,           // 行动判定
        "world" => 30,            // 大纲推进
        "dialogue" => 10,
        _ => 5,
    };
    base + if has_arbiter_note { 15 } else { 0 }
}

/// 生成高光切片：TTS 旁白 + 条漫分镜脚本 → 产物写对象存储 → 记 clip_jobs。返回产物对象 key。
///
/// event_id 必须属于 world_id 且为 public 事件（私有投影不得进切片，§9.4）。
pub async fn generate_clip(
    state: &AppState,
    world_id: &str,
    event_id: &str,
) -> Result<String, ApiError> {
    let row = sqlx::query(
        "SELECT event_type, actors_json, public_projection_json, visibility, moderation \
         FROM world_events WHERE id = $1 AND world_id = $2",
    )
    .bind(event_id)
    .bind(world_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    let visibility: String = row.try_get("visibility")?;
    if visibility != "public" {
        return Err(ApiError::Forbidden); // 只切公共事件
    }
    // 本条按 id 单取，无法在 SQL 里随列表一起过滤，故在此复核（§15 第 2 层）：
    // 未过审事件即便被指名，也不得渲染成切片——切片是最终对外传播物。
    let moderation: String = row.try_get("moderation")?;
    if moderation != "approved" {
        return Err(ApiError::Forbidden);
    }
    let event_type: String = row.try_get("event_type")?;
    let actors_json: String = row.try_get("actors_json")?;
    let pj: Option<String> = row.try_get("public_projection_json")?;
    let projection: Value =
        pj.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
    let summary = projection.get("summary").and_then(Value::as_str).unwrap_or("高光时刻").to_string();
    let actors: Vec<String> = serde_json::from_str(&actors_json).unwrap_or_default();

    // 旁白 → DevTts（写占位音频到本地对象存储；DevTts 复用同一 LocalObjectStore）。
    let narration = format!("高光回放：{summary}");
    let tts = DevTts { store: (*state.objects).clone() };
    let audio_key = tts
        .synthesize(&narration, "narrator-cn")
        .await
        .map_err(|e| ApiError::internal(std::io::Error::other(e)))?;

    // 条漫式分镜脚本（占位结构化 JSON）。
    let storyboard = build_storyboard(&event_type, &summary, &actors);

    // 切片清单产物入对象存储。
    let clip_id = new_id("clip");
    let object_key = format!("clips/{clip_id}.json");
    let manifest = json!({
        "clipId": clip_id,
        "worldId": world_id,
        "eventId": event_id,
        "format": "motion_comic",
        "narration": narration,
        "audioKey": audio_key,
        "storyboard": storyboard,
        "aiLabel": { "visible": true }, // AI 生成内容标识（§2.7 / §9）
        "createdAt": now_ms(),
    });
    state.objects.put(&object_key, manifest.to_string().as_bytes()).map_err(ApiError::internal)?;

    // 记 clip_jobs。
    sqlx::query(
        "INSERT INTO clip_jobs (id, world_id, event_id, object_key, status, created_at) \
         VALUES ($1, $2, $3, $4, 'done', $5)",
    )
    .bind(&clip_id)
    .bind(world_id)
    .bind(event_id)
    .bind(&object_key)
    .bind(now_ms())
    .execute(&state.db)
    .await?;

    Ok(object_key)
}

/// 便捷：判定高光 → 生成切片；无高光返回 None。
pub async fn generate_highlight_clip(
    state: &AppState,
    world_id: &str,
) -> Result<Option<String>, ApiError> {
    match pick_highlight_event(state, world_id).await? {
        Some(pick) => Ok(Some(generate_clip(state, world_id, &pick.event_id).await?)),
        None => Ok(None),
    }
}

/// 条漫式分镜（motion comic）占位脚本：开场 establishing → 主角特写/中景 → 收尾大场景。
fn build_storyboard(event_type: &str, summary: &str, actors: &[String]) -> Value {
    let mut panels = vec![json!({
        "index": 0, "shot": "establishing", "caption": summary, "speaker": Value::Null
    })];
    for (i, a) in actors.iter().take(3).enumerate() {
        panels.push(json!({
            "index": i as i64 + 1,
            "shot": if i == 0 { "closeup" } else { "medium" },
            "caption": format!("{a} · {event_type}"),
            "speaker": a,
        }));
    }
    panels.push(json!({
        "index": panels.len() as i64, "shot": "wide", "caption": "全场沸腾", "speaker": Value::Null
    }));
    json!({ "format": "motion_comic", "panels": panels })
}

#[cfg(all(test, feature = "arena"))]
mod tests {
    use super::*;
    use crate::safety::testkit::{seed_world, test_state};
    use sqlx::AnyPool;

    #[allow(clippy::too_many_arguments)]
    async fn seed_event(
        db: &AnyPool,
        id: &str,
        world_id: &str,
        seq: i64,
        event_type: &str,
        visibility: &str,
        summary: &str,
        actors: &[&str],
        impact: Option<i64>,
    ) {
        let mut proj = json!({ "summary": summary });
        if let Some(im) = impact {
            proj["impact"] = json!(im);
        }
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
             actors_json, visibility, public_projection_json, occurred_at) \
             VALUES ($1, $2, 0, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(world_id)
        .bind(seq)
        .bind(format!("de_{id}"))
        .bind(event_type)
        .bind(serde_json::to_string(actors).unwrap())
        .bind(visibility)
        .bind(proj.to_string())
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn generate_clip_writes_artifact_and_job() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_event(&state.db, "e1", "w1", 1, "action", "public", "决斗胜负已分", &["c1", "c2"], None).await;

        let key = generate_clip(&state, "w1", "e1").await.expect("generate_clip");
        assert!(key.starts_with("clips/"), "产物应写入 clips/ 前缀: {key}");

        // 产物确实落对象存储，且是可解析清单，含 TTS 音频 key + 分镜。
        let bytes = state.objects.get(&key).expect("产物应存在于对象存储");
        let manifest: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(manifest["eventId"], "e1");
        assert!(manifest["audioKey"].as_str().unwrap().starts_with("clips/"));
        assert!(manifest["storyboard"]["panels"].as_array().unwrap().len() >= 2);
        assert_eq!(manifest["aiLabel"]["visible"], true, "切片须带 AI 标识");

        // clip_jobs 记录一条 done。
        let n = crate::safety::testkit::count(
            &state.db,
            "SELECT COUNT(*) FROM clip_jobs WHERE world_id='w1' AND event_id='e1' AND status='done'",
        )
        .await;
        assert_eq!(n, 1, "应记录一条 clip_jobs");
    }

    #[tokio::test]
    async fn pick_highlight_selects_max_impact() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_event(&state.db, "e1", "w1", 1, "dialogue", "public", "闲聊", &[], None).await;
        seed_event(&state.db, "e2", "w1", 2, "consent_request", "public", "生死关头的同意", &["c1"], None).await;
        seed_event(&state.db, "e3", "w1", 3, "action", "public", "反击", &["c2"], None).await;

        let pick = pick_highlight_event(&state, "w1").await.expect("pick").expect("有高光");
        assert_eq!(pick.event_id, "e2", "consent_request 应为最高 impact");
        assert_eq!(pick.impact, 100);
    }

    #[tokio::test]
    async fn explicit_impact_hint_wins() {
        // projection 显式 impact 覆盖类型启发式：dialogue(基线10) 带 impact=999 应压过 consent_request(100)。
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_event(&state.db, "e1", "w1", 1, "consent_request", "public", "同意", &["c1"], None).await;
        seed_event(&state.db, "e2", "w1", 2, "dialogue", "public", "一句封神台词", &["c2"], Some(999)).await;

        let pick = pick_highlight_event(&state, "w1").await.unwrap().unwrap();
        assert_eq!(pick.event_id, "e2");
        assert_eq!(pick.impact, 999);
    }

    #[tokio::test]
    async fn pick_ignores_private_events() {
        // 切片绝不取私有投影（§9.4）：仅有 private 事件时 pick 返回 None，generate_highlight_clip 无产物。
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_event(&state.db, "e1", "w1", 1, "consent_request", "private", "私密", &["c1"], None).await;

        assert!(pick_highlight_event(&state, "w1").await.unwrap().is_none());
        assert!(generate_highlight_clip(&state, "w1").await.unwrap().is_none());

        // 直接对私有事件生成切片应被拒。
        let err = generate_clip(&state, "w1", "e1").await.unwrap_err();
        assert!(matches!(err, ApiError::Forbidden));
    }

    /// §15 第 2 层红线：未过审事件绝不进切片链路。
    /// 切片是最终对外传播物（直播/分享素材），外泄面比站内事件流更大——
    /// 高光挑选与按 id 渲染两条路径都必须拦住，缺一条整层失效。
    #[tokio::test]
    async fn blocked_events_never_reach_clip_pipeline() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        // e_bad 的 impact 更高（consent_request=100 > action），正常情况下必被选为高光。
        seed_event(&state.db, "e_ok", "w1", 1, "action", "public", "干净的反击", &["c1"], None).await;
        seed_event(&state.db, "e_bad", "w1", 2, "consent_request", "public", "被拦下的公开事实", &["c2"], None).await;
        sqlx::query("UPDATE world_events SET moderation = 'pending' WHERE id = 'e_bad'")
            .execute(&state.db)
            .await
            .unwrap();

        // ① 高光挑选：跳过未过审事件，退而选次高的过审事件（而不是整体返回空）。
        let pick = pick_highlight_event(&state, "w1").await.expect("pick").expect("应仍有高光");
        assert_eq!(pick.event_id, "e_ok", "未过审事件不得被选为高光，即便 impact 更高");

        // ② 按 id 渲染：即便被指名也拒绝（该路径按 id 单取，靠 Rust 侧复核）。
        let err = generate_clip(&state, "w1", "e_bad").await.expect_err("未过审事件应拒绝渲染");
        assert!(matches!(err, ApiError::Forbidden), "应为 Forbidden，实际 {err:?}");

        // ③ 未过审事件仍在库里留痕（§0.3 落库即事实，不做事后删改，供人审/申诉）。
        let n = crate::safety::testkit::count(
            &state.db,
            "SELECT COUNT(*) FROM world_events WHERE id='e_bad'",
        )
        .await;
        assert_eq!(n, 1, "被拦事件必须仍然留痕");
    }

}
