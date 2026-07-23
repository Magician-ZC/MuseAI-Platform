//! 赛事房核心后端（P6，feature=arena；规格 §2.5 / §9.1）。
//!
//! 赛制状态机（arena_matches，每世界一场）：唯一胜者——现役参赛角色（active 成员）扣除已「落定」淘汰后
//! 收敛到 1 人即 winner；胜者奖励为荣誉性（arena_rewards：称号/立绘框/榜单），非强度。
//!
//! 端点（AuthUser 守卫，前缀 /api）：
//! - POST /arena/{worldId}/host/tick      主播控制台手动触发一回合——**复用 runtime::schedule_tick(due=now)**，
//!     让既有 hardened worker 跑引擎回合；dev 无模型自然 no-op（no_model_config），赛制层不依赖 LLM。
//! - GET  /arena/{worldId}/report         透明战报——聚合 public world_events（谁做了什么 + 判定依据 rule_refs）
//!     + arena_env_events（礼物/环境）；只出规则依据不出隐藏推理（§9.4，对抗「是不是剧本」质疑）。
//! - POST /arena/{worldId}/revive-match   复活赛「资格」——仅记 eligibility；可买资格不可买免死（billing 扣费留 seam）。
//! - POST /arena/{worldId}/eliminate      主播裁定淘汰某 player-owned 角色（不可逆）→ **触发 consents::create_consent**
//!     门控（补 P4a「同意触发源未接通」缺口），台账记 pending_consent，**不**立即落定。
//! - POST /arena/{worldId}/settle         结算：仅同意 approved 才落定淘汰，declined/超时保守免淘汰；落定后重算唯一胜者。
//!
//! 红线（规格 §2.5，写进实现+测试）：买过程不买结果；无免死端点；胜者奖励非强度；淘汰不可逆需同意门控。
//! seam（诚实标注）：礼物→引擎回合真实影响——arena_env_events 已记录并进战报，注入 LLM RoundInput 需
//!   runtime 扩展（HA 域），本期不接；arbiter rule_refs 注入 world_events.arbiter_note 亦为 runtime seam，
//!   report 读取该列作判定依据。复活/礼物实际扣费经 billing（跨 feature）留 TODO。

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;

#[cfg(all(test, feature = "arena"))]
mod tests;

/// 淘汰同意保守窗口（毫秒）：超时未决 → consents 侧 expired_conservative → settle 保守免淘汰。
const CONSENT_TTL_MS: i64 = 24 * 3600 * 1000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/arena/{world_id}/host/tick", post(host_tick))
        .route("/arena/{world_id}/report", get(get_report))
        .route("/arena/{world_id}/revive-match", post(revive_match))
        .route("/arena/{world_id}/eliminate", post(eliminate))
        .route("/arena/{world_id}/settle", post(settle))
}

// ---------- 守卫 / 赛制状态工具 ----------

/// 主播/房主守卫：仅世界的 host_user_id 可控制赛事（host/tick、eliminate、settle）。
async fn require_host(db: &AnyPool, world_id: &str, user_id: &str) -> Result<(), ApiError> {
    let world = crate::worlds::load_world(db, world_id).await?;
    if world.host_user_id.as_deref() != Some(user_id) {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

/// 确保该世界有一场赛事（首次控制台操作时冷创建，phase=lobby）。并发下唯一索引兜底。
async fn ensure_match(db: &AnyPool, world_id: &str) -> Result<(), ApiError> {
    let exists = sqlx::query("SELECT 1 AS x FROM arena_matches WHERE world_id = ?")
        .bind(world_id)
        .fetch_optional(db)
        .await?
        .is_some();
    if exists {
        return Ok(());
    }
    let now = now_ms();
    match sqlx::query(
        "INSERT INTO arena_matches (id, world_id, phase, alliances_json, eliminations_json, winner_char_id, updated_at, created_at) \
         VALUES (?, ?, 'lobby', '[]', '[]', NULL, ?, ?)",
    )
    .bind(new_id("am"))
    .bind(world_id)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// 赛制状态只读视图（report/settle 复用）。无 match 行时回退为空 lobby。
struct MatchView {
    phase: String,
    alliances: Value,
    eliminations: Value,
    winner_char_id: Option<String>,
}

async fn load_match(db: &AnyPool, world_id: &str) -> Result<MatchView, ApiError> {
    let row = sqlx::query(
        "SELECT phase, alliances_json, eliminations_json, winner_char_id FROM arena_matches WHERE world_id = ?",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some(r) => MatchView {
            phase: r.try_get("phase")?,
            alliances: parse_json_array(&r.try_get::<String, _>("alliances_json")?),
            eliminations: parse_json_array(&r.try_get::<String, _>("eliminations_json")?),
            winner_char_id: r.try_get("winner_char_id")?,
        },
        None => MatchView {
            phase: "lobby".into(),
            alliances: json!([]),
            eliminations: json!([]),
            winner_char_id: None,
        },
    })
}

fn parse_json_array(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!([]))
}

async fn eliminations_of(db: &AnyPool, world_id: &str) -> Result<Vec<String>, ApiError> {
    let row = sqlx::query("SELECT eliminations_json FROM arena_matches WHERE world_id = ?")
        .bind(world_id)
        .fetch_optional(db)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<String, _>("eliminations_json").ok())
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default())
}

// ---------- POST /arena/{worldId}/host/tick ----------

/// 主播手动触发一回合（节目节奏优先于定时器）：复用 runtime::schedule_tick 入队，既有 hardened worker
/// 认领并跑引擎回合。dev 无模型 → worker no_model_config 自然 no-op（赛制层测试不依赖 LLM）。
async fn host_tick(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_host(&state.db, &world_id, &user.user_id).await?;

    let payload_hash = idempotency::hash_payload(world_id.as_bytes());
    let idem = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, "arena.host_tick", idem, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    ensure_match(&state.db, &world_id).await?;
    // lobby → running：主播开赛第一击。
    sqlx::query("UPDATE arena_matches SET phase='running', updated_at=? WHERE world_id=? AND phase='lobby'")
        .bind(now_ms())
        .bind(&world_id)
        .execute(&state.db)
        .await?;

    // 复用 runtime（勿改）：schedule_tick(due=now) 排下一 tick 入队；worker 跑一次 P2 回合循环。
    // seam：礼物 boon（arena_env_events）真实注入本回合 RoundInput 需 runtime 扩展，本期只记录+进战报。
    let tick_no = crate::runtime::schedule_tick(&state, &world_id).await?;

    let resp = json!({
        "worldId": world_id,
        "tickNo": tick_no,
        "scheduled": tick_no.is_some(),
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ---------- GET /arena/{worldId}/report（透明战报） ----------

/// 透明战报：聚合 public world_events（谁做了什么 + 判定依据）+ arena_env_events（礼物/环境）+ 赛制状态。
/// 只出规则依据（rule_refs / 结果摘要），不出私有投影与 chain-of-thought（§9.4）。
async fn get_report(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // 观战资格：official/public 世界任何登录用户可观战；private 需成员/房主（复用 events 资格判定）。
    if !crate::events::can_view_world(&state.db, &world_id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }

    let m = load_match(&state.db, &world_id).await?;

    // 仅取 public 事件（透明战报是可公开验证的日志，不含任一 principal 的私有投影）。
    let rows = sqlx::query(
        "SELECT tick_no, sequence, event_type, actors_json, public_projection_json, arbiter_note \
         FROM world_events WHERE world_id = ? AND visibility = 'public' ORDER BY sequence ASC LIMIT 1000",
    )
    .bind(&world_id)
    .fetch_all(&state.db)
    .await?;

    let mut rounds: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
    for r in &rows {
        let tick: i64 = r.try_get("tick_no")?;
        let sequence: i64 = r.try_get("sequence")?;
        let event_type: String = r.try_get("event_type")?;
        let actors_json: String = r.try_get("actors_json")?;
        let actors: Value = serde_json::from_str(&actors_json).unwrap_or_else(|_| json!([]));
        let pj: Option<String> = r.try_get("public_projection_json")?;
        let proj: Value = pj.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or_else(|| json!({}));
        let summary = proj.get("summary").cloned().unwrap_or_else(|| json!(""));
        let arbiter_note: Option<String> = r.try_get("arbiter_note")?;
        let rule_refs = extract_rule_refs(&proj, arbiter_note.as_deref());
        rounds.entry(tick).or_default().push(json!({
            "sequence": sequence,
            "type": event_type,
            "actors": actors,
            "summary": summary,
            "ruleRefs": rule_refs, // 判定依据；无隐藏推理
        }));
    }

    // 环境事件（礼物 boon / 环境）——进战报，标注是否已应用到某回合。
    let env_rows = sqlx::query(
        "SELECT applied_tick, kind, payload_json, aggregated_count FROM arena_env_events \
         WHERE world_id = ? ORDER BY created_at ASC LIMIT 1000",
    )
    .bind(&world_id)
    .fetch_all(&state.db)
    .await?;
    let mut environment: Vec<Value> = Vec::new();
    for r in &env_rows {
        let applied_tick: Option<i64> = r.try_get("applied_tick")?;
        let kind: String = r.try_get("kind")?;
        let payload_json: String = r.try_get("payload_json")?;
        let payload: Value = serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({}));
        let aggregated_count: i64 = r.try_get("aggregated_count")?;
        environment.push(json!({
            "appliedTick": applied_tick,
            "kind": kind,
            "payload": payload,
            "aggregatedCount": aggregated_count,
        }));
    }

    let rounds_out: Vec<Value> = rounds
        .into_iter()
        .map(|(tick, events)| {
            let env_for_tick: Vec<&Value> = environment
                .iter()
                .filter(|e| e.get("appliedTick").and_then(Value::as_i64) == Some(tick))
                .collect();
            json!({ "tick": tick, "events": events, "env": env_for_tick })
        })
        .collect();

    Ok(Json(json!({
        "worldId": world_id,
        "match": {
            "phase": m.phase,
            "alliances": m.alliances,
            "eliminations": m.eliminations,
            "winnerCharId": m.winner_char_id,
        },
        "rounds": rounds_out,
        "environment": environment, // 全部礼物/环境日志（含尚未注入回合的 seam 记录）
        // 合规展示：仲裁公开承诺 + AI 生成标识（对抗「是不是剧本」质疑，§2.1/§9.4）。
        "compliance": { "arbitrationPublic": true, "aiGenerated": true },
    })))
}

/// 判定依据（透明战报）：优先取投影里显式的 ruleRefs 数组，否则把 arbiter_note 拆为分项。
/// 仅出规则依据，不出 chain-of-thought。（引擎 arbiter.rule_refs → world_events.arbiter_note 注入是 runtime seam。）
fn extract_rule_refs(proj: &Value, arbiter_note: Option<&str>) -> Vec<String> {
    if let Some(arr) = proj.get("ruleRefs").and_then(Value::as_array) {
        let refs: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        if !refs.is_empty() {
            return refs;
        }
    }
    match arbiter_note {
        Some(note) if !note.trim().is_empty() => note
            .split([';', ',', '｜', '|'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

// ---------- POST /arena/{worldId}/revive-match（复活赛资格，非免死） ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviveReq {
    cloud_character_id: String,
}

/// 复活赛「资格」：仅记 eligibility。红线（§2.5）——买的是复活赛资格（过程），不是免死、不改最终判定（结果）。
async fn revive_match(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ReviveReq>,
) -> Result<Json<Value>, ApiError> {
    // 世界存在即可（观众可为参赛角色购买复活资格，不限角色主人）。
    let _world = crate::worlds::load_world(&state.db, &world_id).await?;

    let payload_hash = idempotency::hash_payload(format!("{world_id}:{}", body.cloud_character_id).as_bytes());
    let idem = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, "arena.revive_match", idem, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // 目标角色须为该世界参赛角色（成员），避免对任意角色写资格。
    let is_member = sqlx::query("SELECT 1 AS x FROM world_members WHERE world_id=? AND cloud_character_id=? LIMIT 1")
        .bind(&world_id)
        .bind(&body.cloud_character_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();
    if !is_member {
        return Err(ApiError::NotFound);
    }

    // 仅记资格。TODO(seam)：实际扣费经 billing 集成（跨 feature）——billing::charge(user, revive_sku)，本期不接。
    // 绝不在此设置任何免死/复活落定标志，也不触碰 eliminations / winner。
    let grant_id = new_id("rv");
    sqlx::query(
        "INSERT INTO arena_revive_grants (id, world_id, character_id, user_id, status, created_at) \
         VALUES (?, ?, ?, ?, 'eligible', ?)",
    )
    .bind(&grant_id)
    .bind(&world_id)
    .bind(&body.cloud_character_id)
    .bind(&user.user_id)
    .bind(now_ms())
    .execute(&state.db)
    .await?;

    let resp = json!({
        "reviveGrantId": grant_id,
        "worldId": world_id,
        "cloudCharacterId": body.cloud_character_id,
        "status": "eligible",
        // 显式声明付费边界，杜绝「买结果」误解。
        "boundary": { "buys": "revive_eligibility", "notImmunity": true, "notFinalVerdict": true },
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ---------- POST /arena/{worldId}/eliminate（淘汰提案 → 同意门控） ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EliminateReq {
    cloud_character_id: String,
}

/// 主播裁定淘汰某参赛角色。补 P4a 缺口：player-owned 角色淘汰不可逆 → 先 consents::create_consent 门控，
/// 台账记 pending_consent，**不**立即落定。落定发生在 settle（仅同意 approved）。
async fn eliminate(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<EliminateReq>,
) -> Result<Json<Value>, ApiError> {
    require_host(&state.db, &world_id, &user.user_id).await?;

    let payload_hash = idempotency::hash_payload(format!("{world_id}:{}", body.cloud_character_id).as_bytes());
    let idem = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, "arena.eliminate", idem, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    ensure_match(&state.db, &world_id).await?;
    let resp = propose_elimination(&state, &world_id, &body.cloud_character_id).await?;
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

/// 淘汰同意门控核心：player-owned 角色（世界成员）淘汰 → consents::create_consent(permanent_exit)，
/// 台账 pending_consent。幂等：已在台账则返回当前状态，不重复建同意。
async fn propose_elimination(state: &AppState, world_id: &str, character_id: &str) -> Result<Value, ApiError> {
    // 参赛角色（成员）校验：淘汰对象须是该世界的角色。
    let owner: Option<(String,)> =
        sqlx::query_as("SELECT user_id FROM world_members WHERE world_id=? AND cloud_character_id=? LIMIT 1")
            .bind(world_id)
            .bind(character_id)
            .fetch_optional(&state.db)
            .await?;
    if owner.is_none() {
        return Err(ApiError::NotFound);
    }

    // 已在台账 → 幂等返回当前状态（不重复建同意）。
    if let Some(row) = sqlx::query("SELECT status, consent_id FROM arena_eliminations WHERE world_id=? AND character_id=?")
        .bind(world_id)
        .bind(character_id)
        .fetch_optional(&state.db)
        .await?
    {
        let status: String = row.try_get("status")?;
        let consent_id: Option<String> = row.try_get("consent_id")?;
        return Ok(json!({ "worldId": world_id, "characterId": character_id, "status": status, "consentId": consent_id }));
    }

    // 不可逆淘汰须同意门控 —— 触发 consents::create_consent（复用 S3 状态机，不改 consents/引擎）。
    let detail = format!("赛事淘汰：角色 {character_id} 将永久退出本场赛事（不可逆）");
    let consent_id = crate::consents::create_consent(
        state,
        world_id,
        "permanent_exit",
        &[character_id.to_string()],
        &detail,
        CONSENT_TTL_MS,
    )
    .await?;

    let now = now_ms();
    sqlx::query(
        "INSERT INTO arena_eliminations (id, world_id, character_id, consent_id, status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'pending_consent', ?, ?)",
    )
    .bind(new_id("ae"))
    .bind(world_id)
    .bind(character_id)
    .bind(&consent_id)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    Ok(json!({ "worldId": world_id, "characterId": character_id, "status": "pending_consent", "consentId": consent_id }))
}

// ---------- POST /arena/{worldId}/settle（结算：同意落定 + 唯一胜者） ----------

/// 主播结算：把已同意的淘汰落定进 eliminations，保守处理拒绝/超时，然后重算唯一胜者。
async fn settle(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_host(&state.db, &world_id, &user.user_id).await?;
    ensure_match(&state.db, &world_id).await?;
    settle_consented_eliminations(&state, &world_id).await?;
    let m = load_match(&state.db, &world_id).await?;
    Ok(Json(json!({
        "worldId": world_id,
        "phase": m.phase,
        "eliminations": m.eliminations,
        "winnerCharId": m.winner_char_id,
        "alliances": m.alliances,
    })))
}

/// 读同意结果落定淘汰：approved → 落定；declined/expired_conservative → 保守 spared（免淘汰）；
/// pending → 暂不落定。落定后重算唯一胜者。
async fn settle_consented_eliminations(state: &AppState, world_id: &str) -> Result<(), ApiError> {
    // 保守默认：先惰性过期超时未决同意（复用 consents，不改）——超时视为未同意 → 不落定淘汰。
    let _ = crate::consents::expire_stale_consents(&state.db).await;

    let pending = sqlx::query(
        "SELECT character_id, consent_id FROM arena_eliminations WHERE world_id=? AND status='pending_consent'",
    )
    .bind(world_id)
    .fetch_all(&state.db)
    .await?;

    for row in &pending {
        let cid: String = row.try_get("character_id")?;
        let consent_id: Option<String> = row.try_get("consent_id")?;
        let Some(consent_id) = consent_id else { continue };
        let cstatus: Option<(String,)> =
            sqlx::query_as("SELECT status FROM consent_requests WHERE id=?").bind(&consent_id).fetch_optional(&state.db).await?;
        let Some((cstatus,)) = cstatus else { continue };
        match cstatus.as_str() {
            // 仅当事人同意，淘汰才落定（不可逆行动的同意门控）。
            "approved" => {
                add_elimination(&state.db, world_id, &cid).await?;
                mark_elim(&state.db, world_id, &cid, "eliminated").await?;
            }
            // 拒绝或超时未决 → 保守免淘汰（不默认同意死亡/永久退出）。
            "declined" | "expired_conservative" => {
                mark_elim(&state.db, world_id, &cid, "spared").await?;
            }
            // pending：暂不落定，等下次 settle。
            _ => {}
        }
    }

    recompute_winner(state, world_id).await?;
    Ok(())
}

/// 把角色加入 eliminations_json（去重）。
async fn add_elimination(db: &AnyPool, world_id: &str, character_id: &str) -> Result<(), ApiError> {
    let mut elim = eliminations_of(db, world_id).await?;
    if !elim.iter().any(|c| c == character_id) {
        elim.push(character_id.to_string());
    }
    sqlx::query("UPDATE arena_matches SET eliminations_json=?, updated_at=? WHERE world_id=?")
        .bind(serde_json::to_string(&elim).unwrap_or_else(|_| "[]".into()))
        .bind(now_ms())
        .bind(world_id)
        .execute(db)
        .await?;
    Ok(())
}

async fn mark_elim(db: &AnyPool, world_id: &str, character_id: &str, status: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE arena_eliminations SET status=?, updated_at=? WHERE world_id=? AND character_id=?")
        .bind(status)
        .bind(now_ms())
        .bind(world_id)
        .bind(character_id)
        .execute(db)
        .await?;
    Ok(())
}

/// 唯一胜者收敛：现役参赛角色（active 成员）扣除已落定淘汰后仅剩 1 人 → winner + concluded + 荣誉奖励。
/// 需至少 2 人成局，避免空局/单人误判。
async fn recompute_winner(state: &AppState, world_id: &str) -> Result<(), ApiError> {
    let roster: Vec<(String,)> =
        sqlx::query_as("SELECT cloud_character_id FROM world_members WHERE world_id=? AND status='active'")
            .bind(world_id)
            .fetch_all(&state.db)
            .await?;
    let roster: Vec<String> = roster.into_iter().map(|(c,)| c).collect();
    if roster.len() < 2 {
        return Ok(());
    }

    let elim = eliminations_of(&state.db, world_id).await?;
    let remaining: Vec<String> = roster.into_iter().filter(|c| !elim.contains(c)).collect();
    if remaining.len() == 1 {
        let winner = &remaining[0];
        sqlx::query("UPDATE arena_matches SET winner_char_id=?, phase='concluded', updated_at=? WHERE world_id=?")
            .bind(winner)
            .bind(now_ms())
            .bind(world_id)
            .execute(&state.db)
            .await?;
        grant_champion_reward(&state.db, world_id, winner).await?;
    }
    Ok(())
}

/// 胜者荣誉奖励（非强度）：称号。幂等（唯一索引 world+char+kind）。
/// 红线：奖励只入 arena_rewards（称号/立绘框/榜单），绝不写任何强度/属性加成。
async fn grant_champion_reward(db: &AnyPool, world_id: &str, character_id: &str) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO arena_rewards (id, world_id, character_id, kind, label, season, created_at) \
         VALUES (?, ?, ?, 'title', '赛事冠军', NULL, ?) \
         ON CONFLICT(world_id, character_id, kind) DO NOTHING",
    )
    .bind(new_id("rw"))
    .bind(world_id)
    .bind(character_id)
    .bind(now_ms())
    .execute(db)
    .await?;
    Ok(())
}
