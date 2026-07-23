//! 世界运行时（S2）：tick 调度器 + worker 池（§9.2）。
//!
//! - 调度器：为 running 世界按 tick_per_day 生成 tick 任务入 queue(topic="world_tick")；
//!   world_ticks(world_id, tick_no) 唯一索引保证重复入队自然幂等；回收崩溃遗留 running、补偿滞留 pending。
//! - worker：pop → **原子认领 tick(pending→running CAS，C-1)** → 读世界(base_revision) → 预算预检
//!   （token + cny 熔断则暂停世界，B-2）→ **回灌：DB narrative_state_json 物化到引擎 FS(单一事实源，E-1)**
//!   （首 tick 用 assembled_json/skeleton 种子硬节点/禁止谓词/在场角色）→ 组装 RoundInput
//!   → NarrativeEngine::run_round(可注入 mock model) → 同一事务写 narrative_state(CAS)/world_ticks(done,
//!   **实测 token 计费 B-1**)/world_events 投影/**仅本 tick 实际喂入的干预 applied(Q-3)**/预算累计 → 广播增量。
//! - CAS 冲突 = 终态化(C-2)，不再无限 re-enqueue；worker Err = 退避重试 + 上限终态化(C-9)。
//! - dev 态：世界无模型配置 → tick 跳过并 warn，不 panic。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crate::worlds::{load_world, WorldRow};

use muse_engine::character::types::CharacterCardV2;
use muse_engine::host::{CancelFlag, EngineEvent, EngineHost, HostEvents, HostFs, StdFs, SystemClock};
use muse_engine::model::{HttpModelClient, ModelClient, ModelProfile};
use muse_engine::narrative::types::{
    CharacterState, ConstraintLevel, DomainEvent, DomainEventType, ForbiddenPredicate,
    NarrativeState, NodeStatus, OutlineNode, RoundBudget, RunMode,
};
use muse_engine::narrative::{ModelRoutes, NarrativeEngine, NarrativePrompts, RoundInput};

const TOPIC: &str = "world_tick";

/// 无预算配置时的兜底剩余 token（daily_token_budget=0 亦按此放行，但官方建房已强制非零，见 B-2）。
const DEFAULT_REMAINING_TOKENS: u64 = 100_000;
/// token→cny 估算默认单价（分/1K token）；可用 MUSE_TOKEN_CNY_CENTS_PER_1K 覆盖（真实定价是运营配置）。
const DEFAULT_TOKEN_CNY_CENTS_PER_1K: i64 = 2;
/// 单个 tick 的总处理次数上限（跨重启，C-9）：超限即终态 failed，不再无限重跑。
const MAX_TICK_ATTEMPTS: i64 = 5;
/// running 认领超时（毫秒）：超过视为 worker 崩溃遗留，调度器回收。
const CLAIM_STALE_MS: i64 = 300_000;
/// pending 补偿 re-enqueue 的最小滞留阈值（避免每轮全量 re-enqueue 风暴，C-1）。
const RECLAIM_PENDING_MIN_MS: i64 = 30_000;
/// worker 处理错误的退避基数（毫秒，指数退避，C-9）。
const WORKER_BACKOFF_BASE_MS: u64 = 200;
/// worker 单 job 的错误重试上限（C-9）。
const WORKER_MAX_RETRIES: u32 = 3;
/// #3b 同意请求 TTL（毫秒）：不可逆行动的当事人确认窗口（24h）；超时由 expire_stale_consents 保守过期。
const CONSENT_TTL_MS: i64 = 86_400_000;

fn token_cny_cents_per_1k() -> i64 {
    std::env::var("MUSE_TOKEN_CNY_CENTS_PER_1K")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TOKEN_CNY_CENTS_PER_1K)
}

#[derive(Debug, Serialize, Deserialize)]
struct TickJob {
    world_id: String,
    tick_no: i64,
}

/// process_tick 结果（可观测/测试用）。
#[derive(Debug, PartialEq, Eq)]
pub enum TickStatus {
    Done,
    /// 跳过原因（no_tick / already_done / claimed_elsewhere / world_not_running / superseded /
    /// no_model_config / insufficient_members / cas_conflict / blocked）
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

// ---------- token 计量宿主（B-1：把引擎每次 ModelCall 的实测 token 汇总，作为真实计费口径） ----------

/// 收集 run_round 全过程各环节 ModelCall 的 input+output token 实测值。
#[derive(Default)]
struct TokenMeter {
    input: AtomicU64,
    output: AtomicU64,
    calls: AtomicU64,
}

impl TokenMeter {
    fn total_tokens(&self) -> u64 {
        self.input.load(Ordering::Relaxed) + self.output.load(Ordering::Relaxed)
    }
}

impl HostEvents for TokenMeter {
    fn emit(&self, event: EngineEvent) {
        if let EngineEvent::ModelCall(log) = event {
            self.input.fetch_add(log.input_tokens.unwrap_or(0) as u64, Ordering::Relaxed);
            self.output.fetch_add(log.output_tokens.unwrap_or(0) as u64, Ordering::Relaxed);
            self.calls.fetch_add(1, Ordering::Relaxed);
        }
    }
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

/// 为世界排下一个 tick（tick_no = max+1，base_revision = 当前 state_revision）并入队；已排则返回 None。
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

    // 回收崩溃遗留的 running（started_at 超时）：未超重试上限 → 回 pending 重排；超上限 → 终态 failed（C-1/C-9）。
    let stale_before = now - CLAIM_STALE_MS;
    sqlx::query(
        "UPDATE world_ticks SET status='pending' \
         WHERE status='running' AND started_at IS NOT NULL AND started_at < ? AND attempts < ?",
    )
    .bind(stale_before)
    .bind(MAX_TICK_ATTEMPTS)
    .execute(&state.db)
    .await?;
    sqlx::query(
        "UPDATE world_ticks SET status='failed', error='max_attempts', finished_at=? \
         WHERE status='running' AND started_at IS NOT NULL AND started_at < ? AND attempts >= ?",
    )
    .bind(now)
    .bind(stale_before)
    .bind(MAX_TICK_ATTEMPTS)
    .execute(&state.db)
    .await?;

    let worlds = sqlx::query("SELECT id, tick_per_day FROM worlds WHERE status = 'running'")
        .fetch_all(&state.db)
        .await?;
    for w in &worlds {
        let world_id: String = w.try_get("id")?;
        let tick_per_day: i64 = w.try_get("tick_per_day")?;
        let interval = interval_override.unwrap_or_else(|| 86_400_000 / tick_per_day.max(1));

        // 补偿：只 re-enqueue 滞留过久的 pending（早于一个 interval 且至少 RECLAIM_PENDING_MIN_MS），
        // 而非每轮无条件全量 re-enqueue（后者会让长回合被多 worker 重复投递，C-1）。
        let straggler_before = now - interval.max(RECLAIM_PENDING_MIN_MS);
        let pend = sqlx::query(
            "SELECT tick_no FROM world_ticks WHERE world_id = ? AND status = 'pending' AND created_at < ?",
        )
        .bind(&world_id)
        .bind(straggler_before)
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

// ---------- E-1：种子 / 回灌（DB narrative_state_json ↔ 引擎 FS 单一事实源） ----------

/// 引擎 FS 中该世界叙事状态文件的相对路径（run_id = world_id，稳定到 world 粒度）。
fn engine_state_path(run_id: &str) -> std::path::PathBuf {
    muse_engine::narrative::state::state_path(run_id)
}

/// 构造本 tick 用于回灌到引擎 FS 的权威 NarrativeState：
/// - DB narrative_state_json 可解析（tick>0 已是完整状态）→ 以 DB 为准，对齐 revision/run_id、补齐在场角色；
/// - 否则（首 tick，"{}"）→ 从 assembled_json/skeleton 冷启动：在场角色空态 + 硬节点 + 禁止谓词。
async fn build_seed_state(
    db: &AnyPool,
    world: &WorldRow,
    member_ids: &[String],
    base_revision: i64,
) -> Result<NarrativeState, ApiError> {
    if let Ok(mut s) = serde_json::from_str::<NarrativeState>(&world.narrative_state_json) {
        // DB 权威：回灌前把 run_id 固定到 world、revision 对齐 base_revision（CAS 前提），补齐新加入成员。
        s.run_id = world.id.clone();
        s.revision = base_revision as u64;
        for id in member_ids {
            s.characters.entry(id.clone()).or_default();
        }
        return Ok(s);
    }

    // 首 tick 冷启动种子。
    let mut s = NarrativeState {
        schema_version: 1,
        run_id: world.id.clone(),
        revision: base_revision as u64,
        ..Default::default()
    };
    for id in member_ids {
        s.characters.insert(id.clone(), CharacterState::default());
    }
    seed_narrative_layer(db, world, &mut s).await?;
    Ok(s)
}

/// 从世界模板 skeleton_json + 实例 assembled_json 种入叙事层（硬节点 + 禁止谓词）。
/// 全程防御式解析：字段缺失/格式不符 → 退化为空叙事层（引擎照常运行，不 panic、不 fail-closed）。
async fn seed_narrative_layer(
    db: &AnyPool,
    world: &WorldRow,
    s: &mut NarrativeState,
) -> Result<(), ApiError> {
    // assembled_json 标注的宿命(硬)节点 id（装配层 home_advantages.fatedNodes）。
    let mut fated: std::collections::BTreeSet<String> = Default::default();
    if let Some(raw) = &world.assembled_json {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            if let Some(arr) = v.pointer("/assembly/homeAdvantages").and_then(Value::as_array) {
                for ha in arr {
                    if let Some(nodes) = ha.get("fatedNodes").and_then(Value::as_array) {
                        for n in nodes {
                            if let Some(id) = n.as_str() {
                                fated.insert(id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // 模板骨架（预审核内容池）：mainlineNodes → 大纲节点；forbiddenPredicates → 禁止谓词。
    let Some(row) = sqlx::query("SELECT skeleton_json FROM world_templates WHERE id = ?")
        .bind(&world.template_id)
        .fetch_optional(db)
        .await?
    else {
        return Ok(());
    };
    let raw: String = row.try_get("skeleton_json")?;
    let Ok(sk) = serde_json::from_str::<Value>(&raw) else {
        return Ok(());
    };

    if let Some(nodes) = sk.get("mainlineNodes").and_then(Value::as_array) {
        for node in nodes {
            let Some(id) = node.get("id").and_then(Value::as_str) else {
                continue;
            };
            let summary = node.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
            let is_fated = node.get("fated").and_then(Value::as_bool).unwrap_or(false) || fated.contains(id);
            let constraint = match node.get("constraint").and_then(Value::as_str) {
                Some("hard") => ConstraintLevel::Hard,
                Some("soft") => ConstraintLevel::Soft,
                Some("free") => ConstraintLevel::Free,
                _ if is_fated => ConstraintLevel::Hard,
                _ => ConstraintLevel::Soft,
            };
            s.narrative.outline_nodes.push(OutlineNode {
                id: id.to_string(),
                summary,
                constraint,
                status: NodeStatus::Pending,
            });
        }
    }

    if let Some(preds) = sk.get("forbiddenPredicates").and_then(Value::as_array) {
        for p in preds {
            let (Some(id), Some(expr)) =
                (p.get("id").and_then(Value::as_str), p.get("expression").and_then(Value::as_str))
            else {
                continue;
            };
            // 仅种入语法合法的谓词，避免 eval 阶段 Validation 失败（受限 DSL，见 constraints）。
            if muse_engine::narrative::constraints::parse_predicate(expr).is_err() {
                continue;
            }
            let reason = p.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
            s.narrative.forbidden_predicates.push(ForbiddenPredicate {
                id: id.to_string(),
                expression: expr.to_string(),
                reason,
            });
        }
    }
    Ok(())
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

/// C-2：CAS 冲突终态化（标 done + error=cas_conflict）。冲突是良性「已被更早的 tick 推进」，非失败，不暂停世界，
/// 也不再留 pending 无限 re-enqueue/重跑。
async fn finalize_cas_conflict(db: &AnyPool, world_id: &str, tick_no: i64) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE world_ticks SET status='done', error='cas_conflict', finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(now_ms())
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

/// 预算熔断收尾：置 fused、暂停世界、tick 终态。
async fn fuse_and_pause(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    today: &str,
    spent: i64,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE world_budgets SET fused=1, budget_day=?, spent_tokens_today=?, updated_at=? WHERE world_id=?")
        .bind(today)
        .bind(spent)
        .bind(now_ms())
        .bind(world_id)
        .execute(db)
        .await?;
    pause_world(db, world_id).await?;
    finish_tick_noop(db, world_id, tick_no, Some("budget_fused")).await?;
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

// ---------- #3b 不可逆同意：消费 ConsentRequested + 已获批 subject 回灌（规格 §2.4 / REMEDIATION #3） ----------

/// 回灌：本世界已获批(approved)的不可逆同意 → subject 角色 id 列表。
/// 喂入引擎 `RoundInput.approved_consents`，本回合命中的 subject 可落定其不可逆结果
/// （death/permanent_exit/permanent_relation_change）；未命中一律门控不落定（保守安全默认）。
async fn load_approved_consent_subjects(db: &AnyPool, world_id: &str) -> Result<Vec<String>, ApiError> {
    let rows = sqlx::query(
        "SELECT subject_character_ids FROM consent_requests \
         WHERE world_id = ? AND status = 'approved' \
         AND event_kind IN ('death', 'permanent_exit', 'permanent_relation_change')",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    let mut subjects: Vec<String> = Vec::new();
    for r in &rows {
        let raw: String = r.try_get("subject_character_ids")?;
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&raw) {
            subjects.extend(list);
        }
    }
    subjects.sort();
    subjects.dedup();
    Ok(subjects)
}

/// 消费：把本回合引擎产出的 ConsentRequested 域事件落成同意请求（consents::create_consent），
/// 并通知当事角色主人。幂等：同 world+event_kind+subject 集合已有 pending 同意时不重复建
/// （引擎在获批前每回合都会重发该门控事件）。非事务关键路径：失败仅告警，不回滚已提交的 tick。
async fn create_consents_for_round(state: &AppState, world_id: &str, events: &[DomainEvent]) {
    for ev in events {
        if ev.event_type != DomainEventType::ConsentRequested {
            continue;
        }
        let event_kind = ev.fact.get("eventKind").and_then(Value::as_str).unwrap_or("");
        let detail = ev.fact.get("detail").and_then(Value::as_str).unwrap_or("");
        let mut subjects: Vec<String> = ev
            .fact
            .get("subjectCharacterIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        subjects.sort();
        subjects.dedup();
        if event_kind.is_empty() || subjects.is_empty() {
            continue;
        }
        // 幂等去重：规范化(排序去重)后的 subjects_json 与写入口径一致，精确匹配同 world+kind+subjects 的未决同意。
        let subjects_json = serde_json::to_string(&subjects).unwrap_or_else(|_| "[]".into());
        let dup = sqlx::query(
            "SELECT 1 AS x FROM consent_requests \
             WHERE world_id = ? AND event_kind = ? AND subject_character_ids = ? AND status = 'pending' LIMIT 1",
        )
        .bind(world_id)
        .bind(event_kind)
        .bind(&subjects_json)
        .fetch_optional(&state.db)
        .await;
        match dup {
            Ok(Some(_)) => continue, // 已有未决同意，勿重复建/重复通知
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(world_id, error = %e, "consent 幂等检查失败，跳过本条");
                continue;
            }
        }
        if let Err(e) =
            crate::consents::create_consent(state, world_id, event_kind, &subjects, detail, CONSENT_TTL_MS)
                .await
        {
            tracing::warn!(world_id, event_kind, error = %e, "创建同意请求失败（非事务关键路径）");
        }
    }
}

// ---------- 核心：处理一个 tick ----------

/// 处理一个 tick（生产入口：内部构建 HttpModelClient）。幂等：重复投递被吸收。
pub async fn process_tick(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
) -> Result<TickStatus, ApiError> {
    process_tick_inner(state, world_id, tick_no, None).await
}

/// 处理一个 tick（注入 model：集成测试用 mock ModelClient 走完整 run_round→commit）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn process_tick_with_model(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    model: Arc<dyn ModelClient>,
) -> Result<TickStatus, ApiError> {
    process_tick_inner(state, world_id, tick_no, Some(model)).await
}

/// tick 核心：认领 → 预算 → 回灌种子 → run_round → 事务提交 → 广播。
async fn process_tick_inner(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    model_override: Option<Arc<dyn ModelClient>>,
) -> Result<TickStatus, ApiError> {
    let db = &state.db;

    // 1) 读 tick 行。done/failed → 幂等跳过；超重试上限 → 终态化（C-9）。
    let Some(trow) =
        sqlx::query("SELECT status, base_revision, attempts FROM world_ticks WHERE world_id=? AND tick_no=?")
            .bind(world_id)
            .bind(tick_no)
            .fetch_optional(db)
            .await?
    else {
        return Ok(TickStatus::Skipped("no_tick"));
    };
    let tstatus: String = trow.try_get("status")?;
    let base_revision: i64 = trow.try_get("base_revision")?;
    let attempts: i64 = trow.try_get("attempts")?;
    if tstatus == "done" || tstatus == "failed" {
        return Ok(TickStatus::Skipped("already_done"));
    }
    if attempts >= MAX_TICK_ATTEMPTS {
        mark_tick_failed_and_pause(state, world_id, tick_no, "max_attempts").await?;
        return Ok(TickStatus::Failed);
    }

    // 2) 原子认领（C-1）：pending→running CAS，attempts+1。rows=0 → 已被别的 worker 认领/非 pending，跳过。
    let claimed = sqlx::query(
        "UPDATE world_ticks SET status='running', attempts=attempts+1, started_at=COALESCE(started_at, ?) \
         WHERE world_id=? AND tick_no=? AND status='pending'",
    )
    .bind(now_ms())
    .bind(world_id)
    .bind(tick_no)
    .execute(db)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(TickStatus::Skipped("claimed_elsewhere"));
    }

    // 3) 世界必须 running。
    let world = load_world(db, world_id).await?;
    if world.status != "running" {
        finish_tick_noop(db, world_id, tick_no, Some("world_not_running")).await?;
        return Ok(TickStatus::Skipped("world_not_running"));
    }
    // 陈旧 tick：state_revision 已被更早的 tick 推进（base_revision 不再匹配）→ 终态跳过，不做无谓的昂贵回合。
    if world.state_revision != base_revision {
        finish_tick_noop(db, world_id, tick_no, Some("superseded")).await?;
        return Ok(TickStatus::Skipped("superseded"));
    }

    // 4) 预算预检 + 熔断（token + cny，B-2）。
    let today = day_string(now_ms());
    let mut remaining_tokens: u64 = DEFAULT_REMAINING_TOKENS;
    if let Some(brow) = sqlx::query(
        "SELECT daily_token_budget, daily_cny_budget_cents, spent_tokens_today, budget_day, fused \
         FROM world_budgets WHERE world_id=?",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?
    {
        let daily: i64 = brow.try_get("daily_token_budget")?;
        let daily_cny: i64 = brow.try_get("daily_cny_budget_cents")?;
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
        // token 熔断：先暂停并记录，不悄悄降级模型（§9.2）。
        if daily > 0 && spent >= daily {
            fuse_and_pause(db, world_id, tick_no, &today, spent).await?;
            tracing::warn!(world_id, "world token 预算熔断，已暂停");
            return Ok(TickStatus::Fused);
        }
        // cny 熔断（B-2）：按 token→cny 估算达上限即熔断（真实定价留运营配置）。
        if daily_cny > 0 {
            let est_cny = spent.saturating_mul(token_cny_cents_per_1k()) / 1000;
            if est_cny >= daily_cny {
                fuse_and_pause(db, world_id, tick_no, &today, spent).await?;
                tracing::warn!(world_id, est_cny, daily_cny, "world cny 预算熔断，已暂停");
                return Ok(TickStatus::Fused);
            }
        }
        remaining_tokens = if daily > 0 { (daily - spent).max(0) as u64 } else { DEFAULT_REMAINING_TOKENS };
    }

    // 5) 模型配置解析：无配置 → dev 跳过（不 panic）。
    let Some(routes) = resolve_model_routes(db, &world.model_route_version).await? else {
        tracing::warn!(world_id, version = %world.model_route_version, "world 无模型配置，tick 跳过");
        finish_tick_noop(db, world_id, tick_no, Some("no_model_config")).await?;
        return Ok(TickStatus::Skipped("no_model_config"));
    };

    // 6) 组装成员卡与 principal 投影表。
    let mrows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, wm.user_id AS uid, cc.card_json AS card \
         FROM world_members wm JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.world_id = ? AND wm.status='active' ORDER BY wm.joined_at ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;

    let mut members_projection: Vec<ProjectionMember> = Vec::new();
    let mut member_ids: Vec<String> = Vec::new();
    let mut active_cards: BTreeMap<String, CharacterCardV2> = BTreeMap::new();
    let mut other_brief: BTreeMap<String, String> = BTreeMap::new();
    for row in &mrows {
        let cid: String = row.try_get("cid")?;
        let uid: String = row.try_get("uid")?;
        let card_json: String = row.try_get("card")?;
        members_projection.push(ProjectionMember { character_key: cid.clone(), user_id: uid });
        member_ids.push(cid.clone());
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

    // 7) 托梦（accepted whisper）：Q-3 只喂给真正参与本回合决策的活跃角色，并记录被喂入的干预 id，
    //    仅这些在 commit 时置 applied（避免把长回合中途新到、或非在场角色的 whisper 静默标 applied 却从不投递）。
    let mut whispers: BTreeMap<String, String> = BTreeMap::new();
    let mut fed_intervention_ids: Vec<String> = Vec::new();
    let wrows = sqlx::query(
        "SELECT id, character_id, payload_json FROM interventions \
         WHERE world_id=? AND status='accepted' AND kind='whisper' ORDER BY created_at ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    for row in &wrows {
        let cid: String = row.try_get("character_id")?;
        if !active_cards.contains_key(&cid) {
            continue; // 非活跃决策角色：本 tick 不喂入、不消费。
        }
        let iid: String = row.try_get("id")?;
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
        // 同一角色多条 whisper：全部投递（换行拼接）、全部消费。
        whispers
            .entry(cid)
            .and_modify(|existing| {
                existing.push('\n');
                existing.push_str(&text);
            })
            .or_insert(text);
        fed_intervention_ids.push(iid);
    }

    // 8) 回灌（E-1）：把 DB 权威叙事状态物化到引擎 FS（run_id=world_id 稳定），首 tick 用 skeleton/assembled 种子。
    //    这是「DB narrative_state_json ↔ 引擎 FS 单一事实源」的每 tick 落实点——引擎 store.load 不再 fail-closed，
    //    也不放宽约束（硬节点/禁止谓词随种子进入 FS，回合仍受约束）。
    let data_dir = std::path::PathBuf::from(&state.config.object_store_dir)
        .join("world-data")
        .join(world_id);
    let fs: Arc<dyn HostFs> = Arc::new(StdFs::new(data_dir));
    let run_id = world_id.to_string();
    let seed = build_seed_state(db, &world, &member_ids, base_revision).await?;
    muse_engine::store::write_json(fs.as_ref(), &engine_state_path(&run_id), &seed)?;

    // 9) 模型客户端（注入优先；否则构建 HttpModelClient——放到跳过检查之后避免每 tick 无谓构建）。
    let model: Arc<dyn ModelClient> = match model_override {
        Some(m) => m,
        None => Arc::new(HttpModelClient::new()?),
    };
    let prompts = resolve_prompts(db, &world.prompt_set_version).await?;

    // 9.5) #3b 回灌：本世界已获批(approved)的不可逆同意 subject → 引擎门控白名单。
    //     命中的 subject 本回合可落定其不可逆结果（death/permanent_exit/permanent_relation_change）；
    //     其余一律门控不落定并（重新）产 ConsentRequested。空 = 保守安全默认（全部门控）。
    let approved_consents = load_approved_consent_subjects(db, world_id).await?;

    // 10) run_round（失败重试一次）；每次尝试独立 TokenMeter，取成功尝试的实测 token 计费（B-1）。
    let mut last_err: Option<ApiError> = None;
    for attempt in 0..2u32 {
        let meter = Arc::new(TokenMeter::default());
        let host = EngineHost {
            fs: fs.clone(),
            clock: Arc::new(SystemClock),
            events: meter.clone(),
            model: model.clone(),
        };
        let engine = NarrativeEngine::new(Arc::new(host));
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
            // #3b：已获批不可逆同意 subject 回灌；命中者本回合门控放行落定。
            approved_consents: approved_consents.clone(),
        };
        let cancel = CancelFlag::new();
        match engine.run_round(&routes, &prompts, input, &cancel).await {
            Ok(outcome) => {
                if let Some(reason) = &outcome.blocked {
                    tracing::warn!(world_id, tick_no, reason = %reason, "tick blocked（硬节点/不变量不可满足），不提交状态");
                    finish_tick_noop(db, world_id, tick_no, Some("blocked")).await?;
                    return Ok(TickStatus::Skipped("blocked"));
                }
                // 实测 token（B-1）；模型未回报 token 时回退到引擎预估，保证预算仍累计。
                let metered = meter.total_tokens();
                let cost = if metered > 0 { metered } else { outcome.budget.spent_tokens };
                return commit_tick(
                    state,
                    world_id,
                    tick_no,
                    base_revision,
                    &outcome,
                    &members_projection,
                    cost,
                    &fed_intervention_ids,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(world_id, tick_no, attempt, error = %e, "run_round 失败");
                last_err = Some(e.into());
            }
        }
    }

    // 11) 重试后仍失败：标记失败 + 暂停世界。
    tracing::error!(world_id, tick_no, "tick 重试后仍失败，世界暂停");
    mark_tick_failed_and_pause(state, world_id, tick_no, "run_round_failed").await?;
    let _ = last_err;
    Ok(TickStatus::Failed)
}

/// 同一事务写：narrative_state(CAS)/world_ticks(done,实测 token 成本)/world_events 投影/
/// 本 tick 实际喂入的 whisper 干预 applied/预算累计。
#[allow(clippy::too_many_arguments)]
async fn commit_tick(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    base_revision: i64,
    outcome: &muse_engine::narrative::RoundOutcome,
    members: &[ProjectionMember],
    cost_tokens: u64,
    fed_intervention_ids: &[String],
) -> Result<TickStatus, ApiError> {
    let now = now_ms();
    let cost = cost_tokens as i64;
    let new_state_json = serde_json::to_string(&outcome.new_state).map_err(ApiError::internal)?;
    // 引擎已把状态 revision 推进到 base_revision+1（回灌保证 FS revision == base_revision）；DB 单一事实源与之对齐。
    let new_revision = outcome.new_state.revision as i64;

    let mut tx = state.db.begin().await?;

    // CAS：仅当世界仍处 base_revision 时推进；否则视为已被更早的 tick 处理 → 回滚 + 终态化（C-2）。
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
        finalize_cas_conflict(&state.db, world_id, tick_no).await?;
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

    // Q-3：只消费本 tick 实际喂入的 accepted 干预（按 id 精确置 applied），不 blanket 标全部 accepted。
    for iid in fed_intervention_ids {
        sqlx::query("UPDATE interventions SET status='applied' WHERE id=? AND world_id=? AND status='accepted'")
            .bind(iid)
            .bind(world_id)
            .execute(&mut *tx)
            .await?;
    }

    // 预算累计（B-1：实测 token）。
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
    // ① #3b 消费本回合 ConsentRequested → 建同意请求（放置房不可逆行动的同意触发源，幂等去重）；
    // ② 每 tick 清理超时未决同意（保守默认，见 consents 状态机）；
    // ③ 幂等生成当日日报——放置房北极星，daily_reports 按 (world,character,day) 唯一去重。
    create_consents_for_round(state, world_id, &outcome.scene.events).await;
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

        // C-9：worker 处理错误退避重试，超上限终态化，不再静默丢错让 tick 无限 re-enqueue。
        let mut retry: u32 = 0;
        loop {
            // 单独 spawn 隔离引擎侧 panic：panic 不拖垮 worker，而是暂停世界。
            let handle = {
                let state = state.clone();
                let world_id = world_id.clone();
                tokio::spawn(async move { process_tick(&state, &world_id, tick_no).await })
            };
            match handle.await {
                Ok(Ok(status)) => {
                    tracing::debug!(world_id, tick_no, ?status, "tick 处理完成");
                    break;
                }
                Ok(Err(e)) => {
                    retry += 1;
                    if retry >= WORKER_MAX_RETRIES {
                        tracing::error!(world_id, tick_no, error = %e, "tick 处理错误达上限，终态化并暂停世界");
                        let _ = mark_tick_failed_and_pause(&state, &world_id, tick_no, "worker_error").await;
                        break;
                    }
                    let backoff = WORKER_BACKOFF_BASE_MS * (1u64 << retry.min(5));
                    tracing::warn!(world_id, tick_no, retry, error = %e, "tick 处理错误，退避重试");
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
                Err(join_err) => {
                    tracing::error!(world_id, tick_no, error = %join_err, "tick 处理 panic，暂停世界");
                    let _ = mark_tick_failed_and_pause(&state, &world_id, tick_no, "engine_panic").await;
                    break;
                }
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

#[cfg(test)]
mod tests;
