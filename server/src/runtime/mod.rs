//! 世界运行时（S2）：tick 调度器 + worker 池（§9.2）。
//!
//! - 调度器：为 running 世界按 tick_per_day 生成 tick 任务入 queue(topic="world_tick")；
//!   world_ticks(world_id, tick_no) 唯一索引保证重复入队自然幂等；同时补偿 re-enqueue pending ticks。
//! - worker：pop → 认领 tick → 读世界(base_revision) → 预算预检(world_budgets 熔断则暂停世界)
//!   → 组装 muse_engine RoundInput（成员卡从 cloud_characters.card_json，托梦=accepted whisper，检索片段空）
//!   → NarrativeEngine::run_round → 同一事务写 narrative_state(CAS)/world_ticks(done,cost)/world_events 投影/
//!     interventions applied/预算累计 → 提交后 ws_hub.publish 增量；
//! - 失败重试一次 → 仍失败标记 tick failed + 世界 paused；CAS 冲突/重复投递 = 幂等跳过不产生重复事件；
//! - dev 态：世界无模型配置(model_route_version 无匹配或缺 default profile) → tick 跳过并 warn，不 panic。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::events::{self, ProjectionMember, WsMessage};
use crate::queue;
use crate::worlds::load_world;

use muse_engine::character::types::CharacterCardV2;
use muse_engine::host::{CancelFlag, EngineHost, NullEvents, StdFs, SystemClock};
use muse_engine::model::{HttpModelClient, ModelProfile};
use muse_engine::narrative::types::{RoundBudget, RunMode};
use muse_engine::narrative::{ModelRoutes, NarrativeEngine, NarrativePrompts, RoundInput};

const TOPIC: &str = "world_tick";

#[derive(Debug, Serialize, Deserialize)]
struct TickJob {
    world_id: String,
    tick_no: i64,
}

/// process_tick 结果（可观测/测试用）。
#[derive(Debug, PartialEq, Eq)]
pub enum TickStatus {
    Done,
    /// 跳过原因（no_tick / already_done / world_not_running / no_model_config / insufficient_members / cas_conflict / blocked）
    Skipped(&'static str),
    /// 预算熔断：世界已暂停
    Fused,
    /// 重试后仍失败：世界已暂停
    Failed,
}

/// YYYY-MM-DD（UTC）预算日键。
pub fn day_string(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

// ---------- 调度 ----------

/// 插入 pending tick（唯一索引幂等）；已存在返回 false。
pub async fn insert_tick(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    base_revision: i64,
) -> Result<bool, ApiError> {
    let exists = sqlx::query("SELECT 1 AS x FROM world_ticks WHERE world_id = ? AND tick_no = ?")
        .bind(world_id)
        .bind(tick_no)
        .fetch_optional(db)
        .await?
        .is_some();
    if exists {
        return Ok(false);
    }
    match sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, created_at) \
         VALUES (?, ?, ?, ?, 'pending', ?)",
    )
    .bind(new_id("tick"))
    .bind(world_id)
    .bind(tick_no)
    .bind(base_revision)
    .bind(now_ms())
    .execute(db)
    .await
    {
        Ok(_) => Ok(true),
        // 并发下唯一索引兜底：重复入队自然幂等。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// 为世界排下一个 tick（tick_no = max+1）并入队；已排则返回 None。
pub async fn schedule_tick(state: &AppState, world_id: &str) -> Result<Option<i64>, ApiError> {
    let world = load_world(&state.db, world_id).await?;
    let max: i64 = sqlx::query("SELECT COALESCE(MAX(tick_no), -1) AS m FROM world_ticks WHERE world_id = ?")
        .bind(world_id)
        .fetch_one(&state.db)
        .await?
        .try_get("m")?;
    let next = max + 1;
    if insert_tick(&state.db, world_id, next, world.state_revision).await? {
        queue::push_json(
            &*state.queue,
            TOPIC,
            &TickJob { world_id: world_id.to_string(), tick_no: next },
            now_ms(),
        )
        .await;
        Ok(Some(next))
    } else {
        Ok(None)
    }
}

async fn schedule_due_ticks(state: &AppState) -> Result<(), ApiError> {
    let interval_override: Option<i64> =
        std::env::var("MUSE_TICK_INTERVAL_MS").ok().and_then(|v| v.parse().ok());
    let now = now_ms();
    let worlds = sqlx::query("SELECT id, tick_per_day FROM worlds WHERE status = 'running'")
        .fetch_all(&state.db)
        .await?;
    for w in &worlds {
        let world_id: String = w.try_get("id")?;
        let tick_per_day: i64 = w.try_get("tick_per_day")?;

        // 恢复：re-enqueue 遗留 pending ticks（重复投递被 process_tick 幂等吸收）。
        let pend = sqlx::query("SELECT tick_no FROM world_ticks WHERE world_id = ? AND status = 'pending'")
            .bind(&world_id)
            .fetch_all(&state.db)
            .await?;
        for p in &pend {
            let tick_no: i64 = p.try_get("tick_no")?;
            queue::push_json(
                &*state.queue,
                TOPIC,
                &TickJob { world_id: world_id.clone(), tick_no },
                now,
            )
            .await;
        }

        // 到点则排新 tick。
        let last: Option<i64> =
            sqlx::query("SELECT MAX(created_at) AS m FROM world_ticks WHERE world_id = ?")
                .bind(&world_id)
                .fetch_one(&state.db)
                .await?
                .try_get("m")?;
        let interval = interval_override.unwrap_or_else(|| 86_400_000 / tick_per_day.max(1));
        let due = match last {
            Some(t) => now - t >= interval,
            None => true,
        };
        if due {
            let _ = schedule_tick(state, &world_id).await?;
        }
    }
    Ok(())
}

// ---------- 版本钉住解析 ----------

#[derive(Debug, Deserialize)]
struct RoutesConfig {
    default: Option<ModelProfile>,
    decide: Option<ModelProfile>,
    arbiter: Option<ModelProfile>,
    writer: Option<ModelProfile>,
    critic: Option<ModelProfile>,
    director: Option<ModelProfile>,
}

/// 解析世界钉住的 model_route_version → ModelRoutes；无匹配/缺 default profile → None（dev 跳过信号）。
async fn resolve_model_routes(db: &AnyPool, version: &str) -> Result<Option<ModelRoutes>, ApiError> {
    let Some(row) = sqlx::query("SELECT routes_json FROM model_routes WHERE version = ? LIMIT 1")
        .bind(version)
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    let routes_json: String = row.try_get("routes_json")?;
    let Ok(cfg) = serde_json::from_str::<RoutesConfig>(&routes_json) else {
        return Ok(None);
    };
    let Some(default) = cfg.default else {
        return Ok(None);
    };
    Ok(Some(ModelRoutes {
        default,
        decide: cfg.decide,
        arbiter: cfg.arbiter,
        writer: cfg.writer,
        critic: cfg.critic,
        director: cfg.director,
    }))
}

/// 解析世界钉住的 prompt_set_version（按 version 聚合各 scope 行）→ NarrativePrompts。
async fn resolve_prompts(db: &AnyPool, version: &str) -> Result<NarrativePrompts, ApiError> {
    let rows = sqlx::query("SELECT scope, content FROM prompt_versions WHERE version = ?")
        .bind(version)
        .fetch_all(db)
        .await?;
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for r in &rows {
        map.insert(r.try_get("scope")?, r.try_get("content")?);
    }
    let take = |m: &mut BTreeMap<String, String>, k: &str| m.remove(k).unwrap_or_default();
    Ok(NarrativePrompts {
        director_system: take(&mut map, "director"),
        decide_system: take(&mut map, "decide"),
        arbiter_system: take(&mut map, "arbiter"),
        writer_system: take(&mut map, "writer"),
        critic_system: take(&mut map, "critic"),
        prompt_version: version.to_string(),
    })
}

// ---------- tick 收尾工具 ----------

async fn finish_tick_noop(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let now = now_ms();
    sqlx::query(
        "UPDATE world_ticks SET status='done', cost_tokens=0, error=?, \
         started_at=COALESCE(started_at, ?), finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(note)
    .bind(now)
    .bind(now)
    .bind(world_id)
    .bind(tick_no)
    .execute(db)
    .await?;
    Ok(())
}

async fn pause_world(db: &AnyPool, world_id: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE worlds SET status='paused', updated_at=? WHERE id=? AND status='running'")
        .bind(now_ms())
        .bind(world_id)
        .execute(db)
        .await?;
    Ok(())
}

async fn mark_tick_failed_and_pause(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    note: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE world_ticks SET status='failed', error=?, finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(note)
    .bind(now_ms())
    .bind(world_id)
    .bind(tick_no)
    .execute(&state.db)
    .await?;
    pause_world(&state.db, world_id).await?;
    Ok(())
}

// ---------- 核心：处理一个 tick ----------

/// 处理一个 tick（认领 → 预算 → 组装 → run_round → 事务提交 → 广播）。幂等：重复投递被吸收。
pub async fn process_tick(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
) -> Result<TickStatus, ApiError> {
    let db = &state.db;

    // 1) 认领 tick：仅处理 pending；done/failed 直接幂等跳过。
    let Some(trow) = sqlx::query("SELECT status, base_revision FROM world_ticks WHERE world_id=? AND tick_no=?")
        .bind(world_id)
        .bind(tick_no)
        .fetch_optional(db)
        .await?
    else {
        return Ok(TickStatus::Skipped("no_tick"));
    };
    let tstatus: String = trow.try_get("status")?;
    let base_revision: i64 = trow.try_get("base_revision")?;
    if tstatus == "done" || tstatus == "failed" {
        return Ok(TickStatus::Skipped("already_done"));
    }

    // 2) 世界必须 running。
    let world = load_world(db, world_id).await?;
    if world.status != "running" {
        finish_tick_noop(db, world_id, tick_no, Some("world_not_running")).await?;
        return Ok(TickStatus::Skipped("world_not_running"));
    }

    // 3) 预算预检 + 熔断。
    let today = day_string(now_ms());
    let mut remaining_tokens: u64 = 100_000;
    if let Some(brow) = sqlx::query(
        "SELECT daily_token_budget, spent_tokens_today, budget_day, fused FROM world_budgets WHERE world_id=?",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?
    {
        let daily: i64 = brow.try_get("daily_token_budget")?;
        let mut spent: i64 = brow.try_get("spent_tokens_today")?;
        let day: String = brow.try_get("budget_day")?;
        let fused: i64 = brow.try_get("fused")?;
        // 新的一天：重置窗口。
        if day != today {
            spent = 0;
            sqlx::query("UPDATE world_budgets SET spent_tokens_today=0, budget_day=?, fused=0, updated_at=? WHERE world_id=?")
                .bind(&today)
                .bind(now_ms())
                .bind(world_id)
                .execute(db)
                .await?;
        } else if fused != 0 {
            pause_world(db, world_id).await?;
            finish_tick_noop(db, world_id, tick_no, Some("budget_fused")).await?;
            return Ok(TickStatus::Fused);
        }
        if daily > 0 && spent >= daily {
            // 熔断：先暂停并记录，不悄悄降级模型（§9.2）。
            sqlx::query("UPDATE world_budgets SET fused=1, budget_day=?, spent_tokens_today=?, updated_at=? WHERE world_id=?")
                .bind(&today)
                .bind(spent)
                .bind(now_ms())
                .bind(world_id)
                .execute(db)
                .await?;
            pause_world(db, world_id).await?;
            finish_tick_noop(db, world_id, tick_no, Some("budget_fused")).await?;
            tracing::warn!(world_id, "world 预算熔断，已暂停");
            return Ok(TickStatus::Fused);
        }
        remaining_tokens = if daily > 0 { (daily - spent).max(0) as u64 } else { 100_000 };
    }

    // 4) 模型配置解析：无配置 → dev 跳过（不 panic）。
    let Some(routes) = resolve_model_routes(db, &world.model_route_version).await? else {
        tracing::warn!(world_id, version = %world.model_route_version, "world 无模型配置，tick 跳过");
        finish_tick_noop(db, world_id, tick_no, Some("no_model_config")).await?;
        return Ok(TickStatus::Skipped("no_model_config"));
    };

    // 5) 组装成员卡与 principal 投影表。
    let mrows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, wm.user_id AS uid, cc.card_json AS card \
         FROM world_members wm JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.world_id = ? AND wm.status='active' ORDER BY wm.joined_at ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;

    let mut members_projection: Vec<ProjectionMember> = Vec::new();
    let mut active_cards: BTreeMap<String, CharacterCardV2> = BTreeMap::new();
    let mut other_brief: BTreeMap<String, String> = BTreeMap::new();
    for row in &mrows {
        let cid: String = row.try_get("cid")?;
        let uid: String = row.try_get("uid")?;
        let card_json: String = row.try_get("card")?;
        members_projection.push(ProjectionMember { character_key: cid.clone(), user_id: uid });
        if let Ok(card) = serde_json::from_str::<CharacterCardV2>(&card_json) {
            if active_cards.len() < 5 {
                active_cards.insert(cid, card);
            } else {
                other_brief.insert(cid, card.identity.name);
            }
        }
    }
    if active_cards.len() < 2 {
        finish_tick_noop(db, world_id, tick_no, Some("insufficient_members")).await?;
        return Ok(TickStatus::Skipped("insufficient_members"));
    }

    // 6) 托梦（accepted whisper）：character_id → 文本。
    let mut whispers: BTreeMap<String, String> = BTreeMap::new();
    let wrows = sqlx::query(
        "SELECT character_id, payload_json FROM interventions \
         WHERE world_id=? AND status='accepted' AND kind='whisper'",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    for row in &wrows {
        let cid: String = row.try_get("character_id")?;
        let payload: String = row.try_get("payload_json")?;
        let text = serde_json::from_str::<Value>(&payload)
            .ok()
            .and_then(|v| {
                for k in ["text", "message", "content"] {
                    if let Some(s) = v.get(k).and_then(Value::as_str) {
                        return Some(s.to_string());
                    }
                }
                None
            })
            .unwrap_or(payload);
        whispers.insert(cid, text);
    }

    // 7) 构建引擎宿主（StdFs 指向世界数据目录 + HttpModelClient + NullEvents）。
    let data_dir = std::path::PathBuf::from(&state.config.object_store_dir)
        .join("world-data")
        .join(world_id);
    let host = EngineHost {
        fs: Arc::new(StdFs::new(data_dir)),
        clock: Arc::new(SystemClock),
        events: Arc::new(NullEvents),
        model: Arc::new(HttpModelClient::new()?),
    };
    let engine = NarrativeEngine::new(Arc::new(host));
    let prompts = resolve_prompts(db, &world.prompt_set_version).await?;

    // 8) run_round（失败重试一次）。
    let run_id = format!("{world_id}:{tick_no}");
    let mut last_err: Option<ApiError> = None;
    for attempt in 0..2u32 {
        let input = RoundInput {
            run_id: run_id.clone(),
            mode: RunMode::Observe,
            active_cards: active_cards.clone(),
            other_cards_brief: other_brief.clone(),
            whispers: whispers.clone(),
            fragments: BTreeMap::new(),
            temperature_decide: 0.0,
            temperature_writer: 0.8,
            max_output_tokens: 1024,
            budget: RoundBudget { max_total_tokens: remaining_tokens, spent_tokens: 0, max_scenes: 1 },
        };
        let cancel = CancelFlag::new();
        match engine.run_round(&routes, &prompts, input, &cancel).await {
            Ok(outcome) => {
                if let Some(reason) = &outcome.blocked {
                    tracing::warn!(world_id, tick_no, reason = %reason, "tick blocked（硬节点不可满足），不提交状态");
                    finish_tick_noop(db, world_id, tick_no, Some("blocked")).await?;
                    return Ok(TickStatus::Skipped("blocked"));
                }
                return commit_tick(
                    state,
                    world_id,
                    tick_no,
                    base_revision,
                    &outcome,
                    &members_projection,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(world_id, tick_no, attempt, error = %e, "run_round 失败");
                last_err = Some(e.into());
            }
        }
    }

    // 9) 重试后仍失败：标记失败 + 暂停世界。
    tracing::error!(world_id, tick_no, "tick 重试后仍失败，世界暂停");
    mark_tick_failed_and_pause(state, world_id, tick_no, "run_round_failed").await?;
    let _ = last_err;
    Ok(TickStatus::Failed)
}

/// 同一事务写：narrative_state(CAS)/world_ticks(done,cost)/world_events 投影/interventions applied/预算累计。
async fn commit_tick(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    base_revision: i64,
    outcome: &muse_engine::narrative::RoundOutcome,
    members: &[ProjectionMember],
) -> Result<TickStatus, ApiError> {
    let now = now_ms();
    let cost = outcome.budget.spent_tokens as i64;
    let new_state_json = serde_json::to_string(&outcome.new_state).map_err(ApiError::internal)?;
    let new_revision = base_revision + 1;

    let mut tx = state.db.begin().await?;

    // CAS：仅当世界仍处 base_revision 时推进；否则视为已被处理（幂等跳过，不产生重复事件）。
    let cas = sqlx::query(
        "UPDATE worlds SET narrative_state_json=?, state_revision=?, updated_at=? WHERE id=? AND state_revision=?",
    )
    .bind(&new_state_json)
    .bind(new_revision)
    .bind(now)
    .bind(world_id)
    .bind(base_revision)
    .execute(&mut *tx)
    .await?;
    if cas.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(TickStatus::Skipped("cas_conflict"));
    }

    sqlx::query(
        "UPDATE world_ticks SET status='done', cost_tokens=?, started_at=COALESCE(started_at, ?), \
         finished_at=?, error=NULL WHERE world_id=? AND tick_no=?",
    )
    .bind(cost)
    .bind(now)
    .bind(now)
    .bind(world_id)
    .bind(tick_no)
    .execute(&mut *tx)
    .await?;

    // 受众投影 + 落库（分配 per-world sequence）。
    let projected = events::project_domain_events(&outcome.scene.events, members);
    let stored = events::insert_events_tx(&mut tx, world_id, tick_no, &projected).await?;

    // 消费本 tick 的 accepted 干预。
    sqlx::query("UPDATE interventions SET status='applied' WHERE world_id=? AND status='accepted'")
        .bind(world_id)
        .execute(&mut *tx)
        .await?;

    // 预算累计。
    sqlx::query(
        "UPDATE world_budgets SET spent_tokens_today = spent_tokens_today + ?, budget_day=?, updated_at=? WHERE world_id=?",
    )
    .bind(cost)
    .bind(day_string(now))
    .bind(now)
    .bind(world_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // 提交后广播增量（推送层 principal 过滤在 stream 连接处执行）。
    for s in stored {
        state.ws_hub.publish(WsMessage {
            world_id: world_id.to_string(),
            audience_user_ids: s.audience_user_ids,
            payload_json: s.payload_json,
        });
    }

    // 集成接线（跨模块，非事务关键路径，失败不回滚 tick）：
    // ① 每 tick 清理超时未决同意（保守默认，见 consents 状态机）；
    // ② 幂等生成当日日报——放置房北极星，daily_reports 按 (world,character,day) 唯一去重。
    let _ = crate::consents::expire_stale_consents(&state.db).await;
    let today = day_string(now);
    for m in members {
        let _ = crate::reports::generate_report(state, world_id, &m.user_id, &m.character_key, &today).await;
    }

    Ok(TickStatus::Done)
}

// ---------- 后台任务 ----------

async fn worker_loop(state: AppState) {
    loop {
        let Some(job) = queue::pop_json::<TickJob>(&*state.queue, TOPIC).await else {
            continue;
        };
        let world_id = job.world_id.clone();
        let tick_no = job.tick_no;
        // 单独 spawn 隔离引擎侧 panic（如 E4 WIP），panic 不拖垮 worker，而是暂停世界。
        let handle = {
            let state = state.clone();
            let world_id = world_id.clone();
            tokio::spawn(async move { process_tick(&state, &world_id, tick_no).await })
        };
        match handle.await {
            Ok(Ok(status)) => tracing::debug!(world_id, tick_no, ?status, "tick 处理完成"),
            Ok(Err(e)) => tracing::error!(world_id, tick_no, error = %e, "tick 处理错误"),
            Err(join_err) => {
                tracing::error!(world_id, tick_no, error = %join_err, "tick 处理 panic，暂停世界");
                let _ = mark_tick_failed_and_pause(&state, &world_id, tick_no, "engine_panic").await;
            }
        }
    }
}

async fn scheduler_loop(state: AppState) {
    let poll_ms: u64 =
        std::env::var("MUSE_TICK_POLL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5_000);
    loop {
        if let Err(e) = schedule_due_ticks(&state).await {
            tracing::warn!(error = %e, "tick 调度轮询失败");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// 启动调度器与 worker 池（main 调用）。并发度 MUSE_TICK_WORKERS（默认 2）。
pub fn spawn_workers(state: AppState) {
    let workers: usize =
        std::env::var("MUSE_TICK_WORKERS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    tokio::spawn(scheduler_loop(state.clone()));
    for _ in 0..workers.max(1) {
        tokio::spawn(worker_loop(state.clone()));
    }
    tracing::info!(workers, "runtime 调度器与 worker 池已启动");
}
