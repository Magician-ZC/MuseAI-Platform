//! 事件投影与推送（S2）：DomainEvent→WorldEvent 受众投影 + 查询/推送双层硬隔离。WsHub 为共享基础设施，勿改其结构。
//!
//! 铁律（§9.4 / §9.6）：
//! - DomainEvent 原始负载永不直接下发；平台生成 WorldEvent 投影（public 与 private 分开存），
//!   查询层（SQL + Rust 精确复核）与推送层（fan-out principal 过滤）都强制按 principal 隔离；
//! - WorldEvent 是只读展示层，不存在以事件回传修改状态的接口。
//!
//! 端点：
//! GET /worlds/{id}/events?cursor= → 仅当前 principal 可见（public + 自己在 audience 的 private）
//! WS  /worlds/{id}/stream        → 校验成员/观战资格；按连接 principal 过滤 audience；lastEventId 补偿
//! 投影：project_domain_events(domain_events, members) → world_events 行（public + 每 principal 私有分开存）。

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Any, AnyPool, Row, Transaction};
use tokio::sync::broadcast;

use crate::app::AppState;
use crate::auth::{verify_access, AuthUser};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;

use muse_engine::narrative::types::{
    CharacterState, DomainEvent, DomainEventType, EventVisibility, NarrativeState,
};

/// 每世界一个广播通道；载荷为(投影后)WorldEvent JSON 字符串 + 受众列表。
#[derive(Default)]
pub struct WsHub {
    channels: Mutex<HashMap<String, broadcast::Sender<WsMessage>>>,
}

#[derive(Debug, Clone)]
pub struct WsMessage {
    pub world_id: String,
    /// None = public；Some = 仅这些 user 可见（fan-out 时按连接 principal 过滤）
    pub audience_user_ids: Option<Vec<String>>,
    pub payload_json: String,
}

impl WsHub {
    pub fn sender(&self, world_id: &str) -> broadcast::Sender<WsMessage> {
        // 🔴 **中毒后恢复而不是 panic**，这是本仓库唯一一处这么做的锁，理由要写清楚。
        //
        // 这把锁在**全世界的 WS 下发路径**上（`publish` → `sender`）。若沿用 `unwrap()`/`expect()`，
        // 一次中毒 = **全站实时推送永久停摆且无法自愈，只能重启进程**——而中毒的定义仅仅是
        // 「此前某个持锁线程在别处 panic 过」，它本身并不意味着这里的数据坏了。
        //
        // 恢复安全的依据是**临界区的形状**，不是「大概没事」：
        // 里面只做 `HashMap::entry` + 建通道 + `clone`，三步都是要么完成要么不发生的原子操作，
        // 不存在「写了一半的 HashMap」这种中间态；且 `channels` 是纯注册表
        // （`world_id → broadcast::Sender`），键与键之间没有任何跨条目不变量可被破坏。
        // 换言之：中毒后这张表**仍然是一张完好的表**。
        //
        // ⚠️ 这条推理不可无脑套用到别的锁上。承载跨字段不变量的状态（例如「计数器与集合必须同步」）
        // 中毒后确实可能处于自相矛盾的中间态，那里 panic 才是对的——中毒恰恰是不变量可能已破的信号。
        //
        // ── 全仓其余 `std::sync::Mutex` 的评估结论（2026-07-27 逐个查过，都**不需要**改）──
        //
        // | 锁 | 结论 | 理由 |
        // |---|---|---|
        // | `queue::MemQueue.topics` | 不适用 | 是 `tokio::sync::Mutex`（`.lock().await`），**没有中毒这回事** |
        // | `flags` 的 `CacheMap` | 已处理 | 三处调用一律 `if let Ok(m) = cache().lock()`，中毒即跳过缓存、退化为每次查库——方向正确，缓存本就可丢弃重建 |
        // | `runtime::StallTracker` / `DeferTracker` | 无需改 | 临界区只有 `HashMap` 的 entry/get/remove 加整数运算，**不含任何可 panic 的操作**，故这两把锁不可能中毒；那句 `expect("…锁不可中毒")` 是**正确的断言**，不是疏漏 |
        //
        // 🔴 **由此也要诚实说明本处改动的实际收益**：`broadcast::channel(256)` 只在 OOM 时 panic，
        // 所以这把锁在今天的代码下同样几乎不可能中毒。这里的恢复防的是一个**极低概率事件**——
        // 它的价值不在于「经常会救场」，而在于**万一发生时后果从「全站永久停摆、只能重启」
        // 降为「留一条 warn 继续跑」**。`WsHub` 是 `pub` 且在全站推送路径上，将来任何人给它加
        // 第二个持锁调用点，都可能引入真正的 panic 源——那时这层防御才开始付钱。
        //
        // 真正的故障（那次 panic）不会因此被吞掉：它有自己的 panic 日志，且下面这行 warn
        // 会在每次撞上中毒锁时留痕，指回去找它。
        let mut lock = self.channels.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(
                "WsHub.channels 锁曾中毒（此前有持锁线程 panic）——已恢复继续推送。\
                 真正的故障在更早的 panic 日志里，请回溯；本行只是它的次生现象。"
            );
            poisoned.into_inner()
        });
        lock.entry(world_id.to_string())
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }

    pub fn publish(&self, msg: WsMessage) {
        let _ = self.sender(&msg.world_id).send(msg);
    }
}

/// 连接 principal 是否可见该广播消息（推送层硬隔离）。
pub fn ws_visible(audience_user_ids: &Option<Vec<String>>, principal: &str) -> bool {
    match audience_user_ids {
        None => true, // public
        Some(list) => list.iter().any(|p| p == principal),
    }
}

// ---------- 投影 ----------

/// 世界成员（角色 → principal 映射；投影时把 audience 角色 id 映射为 principal user id）。
#[derive(Debug, Clone)]
pub struct ProjectionMember {
    /// 引擎内角色 id（= cloud_character_id，runtime 组装 RoundInput 时的键）
    pub character_key: String,
    /// principal（角色主人 user id）
    pub user_id: String,
}

/// 投影后的一条 WorldEvent（未落库；runtime 在事务内分配 id/sequence 后写入）。
#[derive(Debug, Clone)]
pub struct ProjectedEvent {
    pub domain_event_id: String,
    pub event_type: String,
    pub actor_ids: Vec<String>,
    /// public / private
    pub visibility: String,
    /// principal user id 列表（public 为空；private 必填非空语义由投影保证）
    pub audience_user_ids: Vec<String>,
    pub summary: String,
    pub arbiter_note: Option<String>,
    /// 审核态（approved / pending / rejected）。投影时一律 `approved`，由 §15 第 2 层
    /// `safety::moderate_runtime_projection` 在**落库前**改写为非 approved；
    /// 非 approved 的行在全部读取面被过滤，且不进 WS 广播。
    pub moderation: String,
}

/// 投影初始审核态：未过闸前默认放行，闸在落库前收紧（§0.3 落库即最终事实，不做事后改写）。
pub const MODERATION_APPROVED: &str = "approved";

/// 读取面统一 SQL（三重门）：世界 + 游标 + **双硬隔离**（public 或 audience 命中）+ **审核门**。
/// `list_events` 与 `stream_loop` 断线补偿共用同一口径——两处各写一份 SQL 必然漂移，
/// 漏掉任一处的 `moderation = 'approved'` 就等于整层拦截失效。
const VISIBLE_EVENTS_SQL: &str = "SELECT * FROM world_events \
     WHERE world_id = $1 AND sequence > $2 AND (visibility = 'public' OR audience_json LIKE $3) \
     AND moderation = 'approved' ORDER BY sequence ASC LIMIT $4";

/// WS 断线重连补偿的单次上限（原为 SQL 内联字面量 500，抽出以复用 VISIBLE_EVENTS_SQL）。
const STREAM_BACKFILL_LIMIT: i64 = 500;

/// 落库后的一条 WorldEvent（携带客户端可见载荷与推送受众；sequence 已内嵌 payload）。
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub audience_user_ids: Option<Vec<String>>,
    pub payload_json: String,
}

fn display_type(t: DomainEventType) -> &'static str {
    match t {
        DomainEventType::ActionResolved => "action",
        DomainEventType::DialogueSpoken => "dialogue",
        DomainEventType::RelationChanged => "status",
        DomainEventType::ResourceChanged => "status",
        DomainEventType::OutlineProgressed => "world",
        DomainEventType::ConsentRequested => "consent_request",
    }
}

/// 摘要取值的**优先级键表**（顺序即优先级，第一个非空者胜出）。
///
/// 分两梯队，都是「事实层可展示字段」，不下发链式推理/私密状态（§9.4 透明战报边界）：
///
/// 1. **显式摘要**（`summary` / `narrative` / `text`）：合成事件（arena 等）自带的成稿文案，优先级最高。
/// 2. **引擎事实字段**（`consequence` / `purpose` / `action`）：引擎 `DomainEvent` 真正携带的内容
///    （`crates/muse-engine/src/narrative/mod.rs` 的事件构造处）——
///    `ActionResolved.fact = {result, action, consequence}`、`DialogueSpoken.fact = {purpose}`。
///    这一梯队此前**完全缺席**，导致引擎产的每一条事件都掉进下方兜底分支、投影成
///    `"action · 沈砚"` 这种零信息串：事件流里根本不存在可比对的叙事文本，
///    `docs/VALIDATION.md` §4.2「剧情重复率」因此无从计算。
///
/// 取值顺序的理由：
/// - `consequence` 是仲裁给出的**结果事实**（这一步实际发生了什么），是公共事实层最权威、
///   也是重复率比对最该用的那段文本；
/// - `purpose` 是发言的公开目的（`DialogueSpoken` 唯一的事实字段）；
/// - `action` 是角色的行动描述，仅在仲裁未给出 consequence 时兜底。
///
/// 刻意**只取一个字段、不做拼接**（如 `"{action} · {consequence}"`）：两段模型文本拼在一起会把
/// 同一件事的措辞重复计入，正是「剧情重复率」这个指标最怕的噪声源；且投影是对外展示文本，
/// 单句比拼接串更像人写的战报。
const SUMMARY_KEYS: [&str; 6] = ["summary", "narrative", "text", "consequence", "purpose", "action"];

/// 面向用户的公共摘要：按 `SUMMARY_KEYS` 取首个非空事实字段；全空则退化为 `"{类型} · {演员}"`。
///
/// 🔴 **本函数的产出是对外读取面文本**，但它**不是**内容安全的把关处：调用它的
/// `project_domain_events` 产出 `ProjectedEvent`，`runtime::commit_tick` 随后在**同一事务内、
/// 落库之前**对 `ProjectedEvent.summary` 跑 §15 第 2 层闸 `safety::moderate_runtime_projection`。
/// 也就是说这里新增的 `consequence`/`purpose`/`action` 文本与原有的 `summary`/`narrative`/`text`
/// 走的是**同一条闸**，不存在绕过词库过滤的旁路（见 `events::tests` 的
/// `engine_fact_text_still_passes_through_the_safety_gate` 红线用例）。
/// 任何将来新增的摘要来源都必须落在这个函数里，而不是绕到 `insert_events_tx` 之后再拼文本。
fn event_summary(ev: &DomainEvent) -> String {
    for key in SUMMARY_KEYS {
        if let Some(s) = ev.fact.get(key).and_then(Value::as_str) {
            let s = s.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    format!("{} · {}", display_type(ev.event_type), ev.actor_ids.join(","))
}

/// 把引擎 DomainEvent + 成员表投影为 WorldEvent 行：
/// - public 事件 → 公共投影；
/// - private 事件 → 受众角色映射为 principal 并集，私有投影按 principal 存，audience 非空。
pub fn project_domain_events(events: &[DomainEvent], members: &[ProjectionMember]) -> Vec<ProjectedEvent> {
    let mut owners: HashMap<&str, Vec<String>> = HashMap::new();
    for m in members {
        owners.entry(m.character_key.as_str()).or_default().push(m.user_id.clone());
    }
    events
        .iter()
        .map(|ev| {
            let summary = event_summary(ev);
            match &ev.visibility {
                EventVisibility::Public => ProjectedEvent {
                    domain_event_id: ev.id.clone(),
                    event_type: display_type(ev.event_type).into(),
                    actor_ids: ev.actor_ids.clone(),
                    visibility: "public".into(),
                    audience_user_ids: Vec::new(),
                    summary,
                    arbiter_note: None,
                    moderation: MODERATION_APPROVED.into(),
                },
                EventVisibility::Private { audience_character_ids } => {
                    // 受众角色 → principal（owner）并集，排序去重（确定性）。
                    let mut principals: Vec<String> = audience_character_ids
                        .iter()
                        .filter_map(|c| owners.get(c.as_str()))
                        .flatten()
                        .cloned()
                        .collect();
                    principals.sort();
                    principals.dedup();
                    ProjectedEvent {
                        domain_event_id: ev.id.clone(),
                        event_type: display_type(ev.event_type).into(),
                        actor_ids: ev.actor_ids.clone(),
                        visibility: "private".into(),
                        audience_user_ids: principals,
                        summary,
                        arbiter_note: None,
                        moderation: MODERATION_APPROVED.into(),
                    }
                }
            }
        })
        .collect()
}

fn build_payload(
    id: &str,
    world_id: &str,
    tick_no: i64,
    sequence: i64,
    pe: &ProjectedEvent,
    occurred_at: i64,
) -> String {
    json!({
        "id": id,
        "worldId": world_id,
        "tick": tick_no,
        "sequence": sequence,
        "domainEventId": pe.domain_event_id,
        "type": pe.event_type,
        "actors": pe.actor_ids,
        "visibility": pe.visibility,
        "projection": { "summary": pe.summary },
        "aiLabel": { "visible": true },
        "occurredAt": occurred_at,
    })
    .to_string()
}

/// 事务内领取 `count` 个连续 sequence，返回区间首号（即 `[base, base + count)`）。
///
/// 🔴 **这是全仓唯一的 sequence 分配入口。** 改造前两处（本文件 `insert_events_tx` 与
/// `persist_and_broadcast_public_event`）各自跑 `SELECT COALESCE(MAX(sequence),-1)+1`
/// 读-改-写，而 `idx_world_events_world(world_id, sequence)` 非唯一：SQLite 的单写者锁让它
/// 事实上串行，**Postgres 在 READ COMMITTED 下不会**——`SELECT MAX()` 不取锁，两个并发事务
/// 读到同一个 MAX 就各写一条同号事件并**静默提交**。竞态真实可达：`arena::emit_arena_event`
/// 是玩家/运营触发的 HTTP 路径，与 `runtime::commit_tick` 并行写同一个世界。
///
/// 撞号最重的后果不是显示顺序，是 [`VISIBLE_EVENTS_SQL`] 拿 `sequence > $cursor` 当
/// **WS 断线补偿游标**：同号的两条里有一条**永远不会补给重连的客户端**。
///
/// ## 正确性来自 ② 的行级排他锁，不是唯一约束
///
/// 并发事务对同一 `world_id` 行的 `UPDATE` 会阻塞到前者提交/回滚，随后在**更新后**的值上
/// 叠加 —— 无丢失更新。两个库都有这条语义，故不需要 `FOR UPDATE`（PG 方言）
/// 也不需要 `RETURNING`（SQLite 3.35+ 才有）。迁移 `0043` 的文件头有完整推导。
///
/// 事务回滚时自增一并回滚 ⇒ **不产生空洞**（与 PG 原生 sequence 不同，这一点被用例断言）。
///
/// ## ① 的初值为什么不是常数 0
///
/// 取 `MAX(sequence)+1` 是对「这个世界有事件、却没有发号器行」的兜底：迁移 `0043` 已把存量
/// 一次性回填，但滚动发布期间旧实例仍可能用老口径写入。初值一对齐，该世界后续分配就恒在
/// 历史之后。`ON CONFLICT DO NOTHING` 保证这条子查询只在建行那一次起作用，
/// 之后每次分配它都不产生任何写；PG 下并发首次分配也安全——冲突方会等到对方提交后再 DO NOTHING。
///
/// `count <= 0` 直接短路：空 tick（本轮无投影事件）不必去碰那把行锁。
async fn allocate_sequences_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    count: i64,
) -> Result<i64, ApiError> {
    if count <= 0 {
        return Ok(0);
    }
    // ① 幂等建行；初值对齐既有历史（理由见上）。
    sqlx::query(
        "INSERT INTO world_event_seq (world_id, next_seq) \
         VALUES ($1, COALESCE((SELECT MAX(sequence) + 1 FROM world_events WHERE world_id = $2), 0)) \
         ON CONFLICT(world_id) DO NOTHING",
    )
    .bind(world_id)
    .bind(world_id)
    .execute(&mut **tx)
    .await?;
    // ② 领号 —— 行级排他锁在这里拿到，持有到本事务 commit/rollback 为止。
    sqlx::query("UPDATE world_event_seq SET next_seq = next_seq + $1 WHERE world_id = $2")
        .bind(count)
        .bind(world_id)
        .execute(&mut **tx)
        .await?;
    // ③ 回读（事务内看得见自己的写）。
    let next: i64 = sqlx::query("SELECT next_seq FROM world_event_seq WHERE world_id = $1")
        .bind(world_id)
        .fetch_one(&mut **tx)
        .await?
        .try_get("next_seq")?;
    Ok(next - count)
}

/// 在事务内落库投影事件（分配 per-world 单调 sequence），返回落库结果供 ws 广播。
///
/// `moderation` 从 `ProjectedEvent` 取（原先硬编码字面量 `'approved'`，等于运行时产出零审核）；
/// `ai_label` 保持硬编码 1——AI 生成标识是平台红线（总规格 §0.6 / §16），**不可参数化、不可绕过**。
/// 返回值只含 `moderation='approved'` 的事件：非 approved 的行落库留痕但**不进 WS 广播**，
/// 与读取面的 `VISIBLE_EVENTS_SQL` 一起构成推送/查询双层过滤。
pub async fn insert_events_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    tick_no: i64,
    projected: &[ProjectedEvent],
) -> Result<Vec<StoredEvent>, ApiError> {
    // 整批一次领号（`[base, base + projected.len())`），批内按投影次序逐条落位。
    let base = allocate_sequences_tx(tx, world_id, projected.len() as i64).await?;
    let now = now_ms();
    let mut out = Vec::with_capacity(projected.len());
    for (i, pe) in projected.iter().enumerate() {
        let sequence = base + i as i64;
        let id = new_id("we");
        let actors_json = serde_json::to_string(&pe.actor_ids).unwrap_or_else(|_| "[]".into());
        let (audience_json, public_proj, private_proj) = if pe.visibility == "public" {
            (None, Some(json!({ "summary": pe.summary }).to_string()), None)
        } else {
            let audience = serde_json::to_string(&pe.audience_user_ids).unwrap_or_else(|_| "[]".into());
            let private = json!([{ "audiencePrincipalIds": pe.audience_user_ids, "summary": pe.summary }]).to_string();
            (Some(audience), None, Some(private))
        };
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
             actors_json, visibility, audience_json, public_projection_json, private_projections_json, \
             arbiter_note, moderation, ai_label, occurred_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 1, $14)",
        )
        .bind(&id)
        .bind(world_id)
        .bind(tick_no)
        .bind(sequence)
        .bind(&pe.domain_event_id)
        .bind(&pe.event_type)
        .bind(&actors_json)
        .bind(&pe.visibility)
        .bind(&audience_json)
        .bind(&public_proj)
        .bind(&private_proj)
        .bind(&pe.arbiter_note)
        .bind(&pe.moderation)
        .bind(now)
        .execute(&mut **tx)
        .await?;

        // 未过审事件不进推送层：落库留痕（供人审/申诉），但不广播给任何连接。
        if pe.moderation != MODERATION_APPROVED {
            continue;
        }
        out.push(StoredEvent {
            audience_user_ids: if pe.visibility == "public" {
                None
            } else {
                Some(pe.audience_user_ids.clone())
            },
            payload_json: build_payload(&id, world_id, tick_no, sequence, pe, now),
        });
    }
    Ok(out)
}

/// 池级封装（测试/独立调用）：自开事务落库投影事件。
#[allow(dead_code)]
pub async fn persist_events(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    projected: &[ProjectedEvent],
) -> Result<Vec<StoredEvent>, ApiError> {
    let mut tx = db.begin().await?;
    let out = insert_events_tx(&mut tx, world_id, tick_no, projected).await?;
    tx.commit().await?;
    Ok(out)
}

/// 落一行 **public** world_event 并广播（供 arena 等系统频道复用）。
///
/// 双硬隔离天然满足：`visibility='public'` + `audience_json=NULL` + 无私有投影 → 推送层 `ws_visible`
/// 与查询层 `row_to_event` 对 public 一律放行，任何观战者可见、且不携带任一 principal 的私密投影。
/// `extra` 合并进 public 投影（如 arenaKind/characterId/sku/aggregatedCount，纯展示层）。
/// 单事务分配 per-world 单调 sequence（与 `insert_events_tx` 共用 [`allocate_sequences_tx`]——
/// 🔴 **本函数正是与 `commit_tick` 撞号的那条 HTTP 路径**，共用同一把行锁才谈得上安全）。
#[allow(dead_code)]
pub async fn persist_and_broadcast_public_event(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    event_type: &str,
    summary: &str,
    actors: &[String],
    extra: Value,
) -> Result<StoredEvent, ApiError> {
    let mut tx = state.db.begin().await?;
    let sequence = allocate_sequences_tx(&mut tx, world_id, 1).await?;
    let id = new_id("we");
    let domain_event_id = new_id("sys"); // 合成来源标识（非引擎 DomainEvent）
    let now = now_ms();

    // public 投影 = { summary } 合并 extra（仅展示字段；不含任何私密）。
    let mut proj = json!({ "summary": summary });
    if let (Some(obj), Some(extra_obj)) = (proj.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    let actors_json = serde_json::to_string(actors).unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, audience_json, public_projection_json, private_projections_json, \
         arbiter_note, moderation, ai_label, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'public', NULL, $8, NULL, NULL, 'approved', 1, $9)",
    )
    .bind(&id)
    .bind(world_id)
    .bind(tick_no)
    .bind(sequence)
    .bind(&domain_event_id)
    .bind(event_type)
    .bind(&actors_json)
    .bind(proj.to_string())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let payload = json!({
        "id": id,
        "worldId": world_id,
        "tick": tick_no,
        "sequence": sequence,
        "domainEventId": domain_event_id,
        "type": event_type,
        "actors": actors,
        "visibility": "public",
        "projection": proj,
        "aiLabel": { "visible": true },
        "occurredAt": now,
    });
    let stored = StoredEvent { audience_user_ids: None, payload_json: payload.to_string() };
    // 提交后广播（推送层对 public 广播给全部连接；audience=None）。
    state.ws_hub.publish(WsMessage {
        world_id: world_id.to_string(),
        audience_user_ids: None,
        payload_json: stored.payload_json.clone(),
    });
    Ok(stored)
}

// ---------- 访问资格 ----------

/// 成员/观战资格：world public/official → 允许观战；private → 必须是成员或房主。
pub async fn can_view_world(db: &AnyPool, world_id: &str, principal: &str) -> Result<bool, ApiError> {
    let world = crate::worlds::load_world(db, world_id).await?;
    if matches!(world.visibility.as_str(), "official" | "public") {
        return Ok(true);
    }
    if world.host_user_id.as_deref() == Some(principal) {
        return Ok(true);
    }
    let is_member = sqlx::query(
        "SELECT 1 AS x FROM world_members WHERE world_id = $1 AND user_id = $2 AND status='active' LIMIT 1",
    )
    .bind(world_id)
    .bind(principal)
    .fetch_optional(db)
    .await?
    .is_some();
    Ok(is_member)
}

// ---------- GET /worlds/{id}/events ----------

#[derive(Debug, Deserialize)]
struct EventsQuery {
    cursor: Option<i64>,
    limit: Option<i64>,
}

/// 把一行 world_events 组装为当前 principal 可见的展示对象；不可见返回 None（查询层硬隔离复核）。
///
/// 两道复核：**审核门**（moderation 必须 approved）+ **principal 硬隔离**（private 须在 audience 内）。
/// 审核门在 Rust 侧再判一次，与 `VISIBLE_EVENTS_SQL` 的 SQL 过滤构成双层——将来新增读取面若忘了带
/// SQL 条件，这里仍然拦得住（口径与 avatar_moderation 的「读取面双过滤」一致）。
fn row_to_event(row: &sqlx::any::AnyRow, principal: &str) -> Result<Option<Value>, ApiError> {
    let moderation: String = row.try_get("moderation")?;
    if moderation != MODERATION_APPROVED {
        return Ok(None); // 未过审（§15 第 2/3 层拦下）→ 不下发
    }
    let visibility: String = row.try_get("visibility")?;
    let sequence: i64 = row.try_get("sequence")?;
    let id: String = row.try_get("id")?;
    let world_id: String = row.try_get("world_id")?;
    let tick_no: i64 = row.try_get("tick_no")?;
    let domain_event_id: String = row.try_get("domain_event_id")?;
    let event_type: String = row.try_get("event_type")?;
    let actors_json: String = row.try_get("actors_json")?;
    let ai_label: i64 = row.try_get("ai_label")?;
    let occurred_at: i64 = row.try_get("occurred_at")?;
    let actors: Value = serde_json::from_str(&actors_json).unwrap_or_else(|_| json!([]));

    let projection: Value = if visibility == "public" {
        let pj: Option<String> = row.try_get("public_projection_json")?;
        pj.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}))
    } else {
        // 精确复核：principal 必须在 audience_json 内，否则不可见。
        let audience_json: Option<String> = row.try_get("audience_json")?;
        let audience: Vec<String> =
            audience_json.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if !audience.iter().any(|p| p == principal) {
            return Ok(None);
        }
        let pj: Option<String> = row.try_get("private_projections_json")?;
        pj.and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
            .map(|entry| json!({ "summary": entry.get("summary").cloned().unwrap_or(json!("")) }))
            .unwrap_or_else(|| json!({}))
    };

    Ok(Some(json!({
        "id": id,
        "worldId": world_id,
        "tick": tick_no,
        "sequence": sequence,
        "domainEventId": domain_event_id,
        "type": event_type,
        "actors": actors,
        "visibility": visibility,
        "projection": projection,
        "aiLabel": { "visible": ai_label != 0 },
        "occurredAt": occurred_at,
    })))
}

async fn list_events(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Value>, ApiError> {
    if !can_view_world(&state.db, &id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }
    let cursor = q.cursor.unwrap_or(-1);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    // SQL 先粗过滤（public + audience 命中 + 审核门），Rust 再精确复核（双层硬隔离 + 审核复核）。
    let like = format!("%\"{}\"%", user.user_id);
    let rows = sqlx::query(VISIBLE_EVENTS_SQL)
        .bind(&id)
        .bind(cursor)
        .bind(&like)
        .bind(limit)
        .fetch_all(&state.db)
        .await?;

    let mut events = Vec::new();
    let mut next_cursor: Option<i64> = None;
    for row in &rows {
        if let Some(item) = row_to_event(row, &user.user_id)? {
            next_cursor = Some(row.try_get::<i64, _>("sequence")?);
            events.push(item);
        }
    }
    Ok(Json(json!({ "events": events, "nextCursor": next_cursor })))
}

// ---------- WS /worlds/{id}/stream ----------

#[derive(Debug, Deserialize)]
struct StreamQuery {
    token: Option<String>,
    access_token: Option<String>,
    last_event_id: Option<i64>,
}

async fn stream(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    // 浏览器 WS 无法带 Authorization 头：token 走查询参数。
    let token = q.token.or(q.access_token).ok_or(ApiError::Unauthorized)?;
    let claims = verify_access(&state.config.jwt_secret, &token)?;
    let principal = claims.sub;
    if !can_view_world(&state.db, &id, &principal).await? {
        return Err(ApiError::Forbidden);
    }
    let last_event_id = q.last_event_id.unwrap_or(-1);
    Ok(ws.on_upgrade(move |socket| stream_loop(state, id, principal, last_event_id, socket)))
}

async fn stream_loop(
    state: AppState,
    world_id: String,
    principal: String,
    last_event_id: i64,
    mut socket: WebSocket,
) {
    // 订阅先于补偿，避免补偿与实时之间丢事件（客户端按 sequence 去重）。
    let mut rx = state.ws_hub.sender(&world_id).subscribe();

    // 断线重连补偿：下发 lastEventId 之后、当前 principal 可见**且已过审**的历史事件
    // （与 list_events 共用 VISIBLE_EVENTS_SQL，避免两处口径漂移）。
    if last_event_id >= 0 {
        let like = format!("%\"{principal}\"%");
        if let Ok(rows) = sqlx::query(VISIBLE_EVENTS_SQL)
            .bind(&world_id)
            .bind(last_event_id)
            .bind(&like)
            .bind(STREAM_BACKFILL_LIMIT)
            .fetch_all(&state.db)
            .await
        {
            for row in &rows {
                if let Ok(Some(item)) = row_to_event(row, &principal) {
                    if socket.send(Message::Text(item.to_string().into())).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(msg) => {
                    if ws_visible(&msg.audience_user_ids, &principal)
                        && socket.send(Message::Text(msg.payload_json.into())).await.is_err()
                    {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None => return,
                Some(Err(_)) => return,
                _ => {} // 忽略客户端上行（只读通道）
            },
        }
    }
}

// ---------- GET /worlds/{id}/state-summary（#6a：权威关系/状态快照，按 principal 过滤 / REMEDIATION #6 / §11） ----------

/// 角色公共活跃度：目标 + 计划 + 情绪条数之和（粗粒度投入度量，仅暴露数量不暴露私密内容）。
fn character_activity(cs: &CharacterState) -> i64 {
    (cs.goals.len() + cs.plans.len() + cs.emotions.len()) as i64
}

/// 权威关系/状态快照：从 worlds.narrative_state_json 派生，按 principal 过滤。
/// - 资格：AuthUser + 成员/观战资格（can_view_world，与事件流一致）。
/// - 关系（信息边界，§9.4）：仅 `from == viewer 的本世界角色` 或 `knownTo 含之` 的有向边可见；
///   非当事、非知情者（含仅作为 `to` 目标者）看不到。观战者(无本世界角色)只见公共角色摘要、零关系。
/// - 角色：公共摘要 `{id, arcStage, activity}`（不下发目标/秘密/情绪等私密内容）。
async fn world_state_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !can_view_world(&state.db, &id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }
    let world = crate::worlds::load_world(&state.db, &id).await?;

    // viewer 在本世界持有的角色（成员）；观战者为空集 → 见不到任何私有关系。
    let rows = sqlx::query(
        "SELECT cloud_character_id FROM world_members WHERE world_id = $1 AND user_id = $2 AND status = 'active'",
    )
    .bind(&id)
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;
    let mut mine: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in &rows {
        mine.insert(r.try_get("cloud_character_id")?);
    }

    // 从权威叙事状态派生；首 tick 前（"{}" / 不可解析）优雅退化为空快照，不报错。
    let st: NarrativeState = serde_json::from_str(&world.narrative_state_json).unwrap_or_default();

    let relations: Vec<Value> = st
        .relations
        .iter()
        .filter(|rel| {
            mine.contains(&rel.from) || rel.known_to.iter().any(|k| mine.contains(k))
        })
        .map(|rel| {
            json!({
                "from": rel.from,
                "to": rel.to,
                "trust": rel.trust,
                "affinity": rel.affinity,
                "fear": rel.fear,
                "debt": rel.debt,
            })
        })
        .collect();

    let characters: Vec<Value> = st
        .characters
        .iter()
        .map(|(cid, cs)| {
            json!({
                "id": cid,
                "arcStage": cs.arc_stage,
                "activity": character_activity(cs),
            })
        })
        .collect();

    // 地点投影（Phase 2）：从 assembled_json 的 assembly.locationGraph 读回钉住的地点图。
    // public 投影——只下发 {id, name, connections, isSecretRealm}，gate 细节（准入门槛/道具）不下发（防剧透）。
    let wrapper = crate::assembly::load_wrapper(&state.db, &id).await?;
    let mut secret_realms: std::collections::HashSet<String> = std::collections::HashSet::new();
    let locations: Vec<Value> = wrapper
        .get("assembly")
        .and_then(|a| a.get("locationGraph"))
        .and_then(|g| g.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|loc| {
                    let lid = loc.get("id").and_then(|v| v.as_str())?;
                    let is_secret = loc
                        .get("isSecretRealm")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if is_secret {
                        secret_realms.insert(lid.to_string());
                    }
                    Some(json!({
                        "id": lid,
                        "name": loc.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "connections": loc.get("connections").cloned().unwrap_or_else(|| json!([])),
                        "isSecretRealm": is_secret,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    // 角色位置投影：{characterId: locationId}，从 NarrativeState.characters[].location 派生，按 principal 过滤。
    // 防剧透：秘境（isSecretRealm）内的角色位置仅角色主人可见——观战者/他人看不到「谁进了秘境」，
    // 非秘境（公共地点）位置对全体资格 viewer 可见。空 location（无地点/全局场景）不下发。
    let mut positions = serde_json::Map::new();
    for (cid, cs) in &st.characters {
        if cs.location.is_empty() {
            continue;
        }
        if secret_realms.contains(&cs.location) && !mine.contains(cid) {
            continue; // 私密位置不泄露
        }
        positions.insert(cid.clone(), Value::String(cs.location.clone()));
    }

    Ok(Json(json!({
        "relations": relations,
        "characters": characters,
        "locations": locations,
        "positions": positions,
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worlds/{id}/events", get(list_events))
        .route("/worlds/{id}/stream", get(stream))
        .route("/worlds/{id}/state-summary", get(world_state_summary))
}

#[cfg(test)]
mod ws_hub_poison_tests {
    use super::*;

    /// 🔴 **一次锁中毒不得让全站 WS 推送永久停摆。**
    ///
    /// 用例的形状就是那个故障场景本身：先让一个线程在**持锁期间** panic（这是「中毒」的
    /// 唯一成因），再证明后续调用照常拿到通道。若哪天有人把 `unwrap_or_else` 改回
    /// `unwrap()`/`expect()`，这里会立刻 panic 而不是返回——测试红得很直接。
    ///
    /// 注意这**不是**在纵容 panic：那次 panic 有它自己的日志，本用例只断言它的次生影响
    /// 被限制在「那一次」，而不是扩散成「这个进程从此再也推不了消息」。
    #[test]
    fn a_poisoned_lock_does_not_kill_ws_delivery_forever() {
        let hub = std::sync::Arc::new(WsHub::default());

        // 中毒前：正常可用，且拿到的是同一个世界的同一条通道。
        let before = hub.sender("w1");
        assert_eq!(before.receiver_count(), 0);

        // 制造中毒：持锁线程 panic。std::sync::Mutex 只有这一种中毒途径。
        let h = std::sync::Arc::clone(&hub);
        let joined = std::thread::spawn(move || {
            let _guard = h.channels.lock().expect("首次上锁必然成功");
            panic!("刻意 panic：模拟持锁线程在别处崩溃");
        })
        .join();
        assert!(joined.is_err(), "该线程必须真的 panic，否则锁没中毒，本用例就是空跑");
        assert!(hub.channels.is_poisoned(), "锁必须真的处于中毒态，否则后面的断言没有意义");

        // 🔴 中毒之后：推送链路仍然活着。
        let after = hub.sender("w1");
        assert!(
            after.same_channel(&before),
            "中毒后应当拿回同一条通道——注册表内容完好，这正是可以恢复的依据"
        );

        // 新世界照样能建通道（不只是老键还能读）。
        let fresh = hub.sender("w2");
        assert!(!fresh.same_channel(&before), "不同世界必须是不同通道");

        // publish 走的就是 sender，一并证明它没被中毒卡死。
        let mut rx = after.subscribe();
        hub.publish(WsMessage {
            world_id: "w1".into(),
            audience_user_ids: None,
            payload_json: "{}".into(),
        });
        assert!(rx.try_recv().is_ok(), "中毒后 publish 必须仍能送达订阅者");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::testkit::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::collections::BTreeSet;
    use tower::ServiceExt;

    /// 一份权威叙事状态（camelCase 与引擎 serde 对齐）：3 角色 + 3 条有向关系，known_to 各异，用于 principal 过滤。
    fn sample_state_json() -> String {
        json!({
            "schemaVersion": 1,
            "runId": "w1",
            "revision": 3,
            "characters": {
                "c1": { "arcStage": "rising", "goals": ["夺权", "复仇"], "plans": ["结盟"], "emotions": [{"name": "愤怒", "intensity": 0.8}] },
                "c2": { "arcStage": "setup", "goals": ["自保"] },
                "c3": { "arcStage": "", "goals": [] }
            },
            "relations": [
                // c1→c2：仅 c1 知情（from==c1）。
                { "from": "c1", "to": "c2", "trust": 60, "affinity": 40, "fear": 0, "debt": 0, "knownTo": ["c1"] },
                // c2→c1：c1、c2 皆知情。
                { "from": "c2", "to": "c1", "trust": 20, "affinity": 10, "fear": 50, "debt": 0, "knownTo": ["c2", "c1"] },
                // c2→c3：仅 c2 知情（c3 作为 to 不获可见权）。
                { "from": "c2", "to": "c3", "trust": 30, "affinity": 30, "fear": 0, "debt": 0, "knownTo": ["c2"] }
            ]
        })
        .to_string()
    }

    async fn set_state(db: &AnyPool, world: &str, s: &str) {
        sqlx::query("UPDATE worlds SET narrative_state_json = $1 WHERE id = $2")
            .bind(s)
            .bind(world)
            .execute(db)
            .await
            .unwrap();
    }

    async fn set_assembled(db: &AnyPool, world: &str, s: &str) {
        sqlx::query("UPDATE worlds SET assembled_json = $1 WHERE id = $2")
            .bind(s)
            .bind(world)
            .execute(db)
            .await
            .unwrap();
    }

    /// 带地点维度的权威状态：c1 在公共前厅、c2 在秘境、c3 无地点。
    fn state_with_locations_json() -> String {
        json!({
            "schemaVersion": 1,
            "runId": "w1",
            "revision": 4,
            "characters": {
                "c1": { "arcStage": "rising", "goals": ["夺权"], "location": "hall" },
                "c2": { "arcStage": "setup", "goals": ["自保"], "location": "secret" },
                "c3": { "arcStage": "", "goals": [], "location": "" }
            },
            "relations": []
        })
        .to_string()
    }

    /// 装配包装：钉住地点图（含秘境 gate 细节，投影时应被剥离）。
    fn assembled_with_locations_json() -> String {
        json!({
            "assembly": {
                "locationGraph": [
                    { "id": "hall", "name": "前厅", "connections": ["secret"] },
                    { "id": "secret", "name": "密室", "connections": ["hall"], "isSecretRealm": true,
                      "gate": { "requiredItemIds": ["jade_key"], "maxPowerTier": 3 } }
                ]
            },
            "chapterState": { "currentNode": 0 }
        })
        .to_string()
    }

    async fn get_summary(state: &AppState, bearer: Option<&str>, world: &str) -> (StatusCode, Value) {
        let app = crate::app::build_router(state.clone());
        let mut builder =
            Request::builder().method("GET").uri(format!("/api/worlds/{world}/state-summary"));
        if let Some(tk) = bearer {
            builder = builder.header("authorization", format!("Bearer {tk}"));
        }
        let resp = app.oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
        let s = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (s, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
    }

    fn edges(v: &Value) -> BTreeSet<(String, String)> {
        v["relations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["from"].as_str().unwrap().into(), r["to"].as_str().unwrap().into()))
            .collect()
    }

    fn activity_of(v: &Value, id: &str) -> i64 {
        v["characters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .and_then(|c| c["activity"].as_i64())
            .unwrap()
    }

    fn arc_of(v: &Value, id: &str) -> String {
        v["characters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .and_then(|c| c["arcStage"].as_str())
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn state_summary_relations_filtered_by_principal() {
        let state = test_state().await;
        // official 世界 → 允许观战；u1 持 c1、u2 持 c2、u3 无角色（观战者）。
        seed_user(&state.db, "u1").await;
        seed_user(&state.db, "u2").await;
        seed_user(&state.db, "u3").await;
        seed_world(&state.db, "w1", 3, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_member(&state.db, "m2", "w1", "u2", "c2", "active").await;
        set_state(&state.db, "w1", &sample_state_json()).await;

        // u1（持 c1）：仅见 from==c1 或 known_to 含 c1 的边 → {c1→c2, c2→c1}，不见 c2→c3。
        let (s1, v1) = get_summary(&state, Some(&token(&state, "u1")), "w1").await;
        assert_eq!(s1, StatusCode::OK, "body={v1}");
        assert_eq!(
            edges(&v1),
            BTreeSet::from([("c1".into(), "c2".into()), ("c2".into(), "c1".into())]),
            "c1 应见其为 from 或知情的关系，不见 c2→c3"
        );

        // u2（持 c2）：见 {c2→c1, c2→c3}；关键：不见 c1→c2（c2 仅是 to、不在 known_to → 非当事非知情看不到）。
        let (_, v2) = get_summary(&state, Some(&token(&state, "u2")), "w1").await;
        assert_eq!(
            edges(&v2),
            BTreeSet::from([("c2".into(), "c1".into()), ("c2".into(), "c3".into())]),
            "作为 to 目标但不在 known_to 的 c1→c2 对 c2 不可见"
        );
        assert!(!edges(&v2).contains(&("c1".into(), "c2".into())));

        // u3（观战者，无本世界角色）：零私有关系，但仍见公共角色摘要。
        let (_, v3) = get_summary(&state, Some(&token(&state, "u3")), "w1").await;
        assert!(edges(&v3).is_empty(), "非当事非知情的观战者看不到任何关系");
        assert_eq!(v3["characters"].as_array().unwrap().len(), 3, "观战者仍见公共角色摘要");

        // 公共角色摘要（对所有资格 viewer 一致）：arcStage + activity(=goals+plans+emotions 计数)。
        assert_eq!(arc_of(&v1, "c1"), "rising");
        assert_eq!(activity_of(&v1, "c1"), 4, "c1 活跃度 = 目标2 + 计划1 + 情绪1");
        assert_eq!(activity_of(&v1, "c2"), 1, "c2 活跃度 = 目标1");
        assert_eq!(activity_of(&v1, "c3"), 0, "c3 活跃度 = 0");
    }

    #[tokio::test]
    async fn state_summary_locations_and_positions_filtered_by_principal() {
        let state = test_state().await;
        // official 世界 → 允许观战；u1 持 c1（公共前厅）、u2 持 c2（秘境）、u3 无角色（观战者）。
        seed_user(&state.db, "u1").await;
        seed_user(&state.db, "u2").await;
        seed_user(&state.db, "u3").await;
        seed_world(&state.db, "w1", 3, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_member(&state.db, "m2", "w1", "u2", "c2", "active").await;
        set_state(&state.db, "w1", &state_with_locations_json()).await;
        set_assembled(&state.db, "w1", &assembled_with_locations_json()).await;

        // 地点图对所有资格 viewer 一致：两地点全下发，但 gate 细节（准入门槛）被剥离（防剧透）。
        let (s3, v3) = get_summary(&state, Some(&token(&state, "u3")), "w1").await;
        assert_eq!(s3, StatusCode::OK, "body={v3}");
        let locs = v3["locations"].as_array().unwrap();
        assert_eq!(locs.len(), 2, "两地点全下发（拓扑公开）");
        let secret = locs.iter().find(|l| l["id"] == "secret").unwrap();
        assert_eq!(secret["isSecretRealm"], json!(true), "秘境标记保留");
        assert_eq!(secret["name"], json!("密室"));
        assert!(secret.get("gate").is_none(), "gate 细节不下发（防剧透）");

        // u3（观战者）：只见 public 地点的角色位置（c1@hall），秘境内 c2 位置不泄露；c3 无地点不下发。
        let positions3 = v3["positions"].as_object().unwrap();
        assert_eq!(positions3.get("c1"), Some(&json!("hall")), "观众可见公共地点角色位置");
        assert!(!positions3.contains_key("c2"), "秘境内角色位置对观战者不泄露");
        assert!(!positions3.contains_key("c3"), "无地点角色不下发位置");

        // u2（持 c2）：秘境是自己的角色 → 可见 c2@secret；同时也见公共 c1@hall。
        let (_, v2) = get_summary(&state, Some(&token(&state, "u2")), "w1").await;
        let positions2 = v2["positions"].as_object().unwrap();
        assert_eq!(positions2.get("c2"), Some(&json!("secret")), "角色主人可见自己在秘境的位置");
        assert_eq!(positions2.get("c1"), Some(&json!("hall")), "公共地点位置对全体可见");
    }

    #[tokio::test]
    async fn state_summary_empty_state_degrades_gracefully() {
        // 首 tick 前 narrative_state_json 为 "{}"（seed 默认）→ 空快照而非报错。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let (s, v) = get_summary(&state, Some(&token(&state, "u1")), "w1").await;
        assert_eq!(s, StatusCode::OK);
        assert!(v["relations"].as_array().unwrap().is_empty());
        assert!(v["characters"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn state_summary_private_world_requires_membership() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_user(&state.db, "u2").await;
        seed_world(&state.db, "w1", 3, "running").await;
        // 收敛为 private：观战不再开放，仅成员/房主可见。
        sqlx::query("UPDATE worlds SET visibility='private' WHERE id=$1")
            .bind("w1")
            .execute(&state.db)
            .await
            .unwrap();
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        set_state(&state.db, "w1", &sample_state_json()).await;

        // 成员 u1 → 200。
        let (s1, _) = get_summary(&state, Some(&token(&state, "u1")), "w1").await;
        assert_eq!(s1, StatusCode::OK);
        // 非成员 u2 → 403（成员/观战资格守卫）。
        let (s2, _) = get_summary(&state, Some(&token(&state, "u2")), "w1").await;
        assert_eq!(s2, StatusCode::FORBIDDEN);
    }

    // ---------- §15 第 2/3 层：未过审事件在全部读取面被过滤 ----------

    fn projected(domain_id: &str, summary: &str, moderation: &str, audience: &[&str]) -> ProjectedEvent {
        let private = !audience.is_empty();
        ProjectedEvent {
            domain_event_id: domain_id.into(),
            event_type: "dialogue".into(),
            actor_ids: vec!["c1".into()],
            visibility: if private { "private".into() } else { "public".into() },
            audience_user_ids: audience.iter().map(|s| s.to_string()).collect(),
            summary: summary.into(),
            arbiter_note: None,
            moderation: moderation.into(),
        }
    }

    async fn get_events(state: &AppState, bearer: &str, world: &str) -> (StatusCode, Value) {
        let app = crate::app::build_router(state.clone());
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/worlds/{world}/events"))
            .header("authorization", format!("Bearer {bearer}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let s = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (s, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
    }

    fn domain_ids(items: &[Value]) -> Vec<String> {
        items.iter().map(|e| e["domainEventId"].as_str().unwrap_or_default().to_string()).collect()
    }

    /// 拦截只有在**全部**读取面生效才算数：查询层 / 推送层实时广播 / 推送层断线补偿 / 逐行复核。
    #[tokio::test]
    async fn blocked_events_are_filtered_from_every_read_face() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;

        let stored = persist_events(
            &state.db,
            "w1",
            0,
            &[
                projected("de-ok", "正常公开事实", MODERATION_APPROVED, &[]),
                projected("de-bad", "被拦下的公开事实", "pending", &[]),
                projected("de-priv-bad", "被拦下的私有视角", "pending", &["u1"]),
                projected("de-priv-ok", "正常私有视角", MODERATION_APPROVED, &["u1"]),
            ],
        )
        .await
        .unwrap();

        // ① 推送层（实时广播）：未过审事件不产生 StoredEvent，压根不进 ws_hub。
        assert_eq!(stored.len(), 2, "只有过审事件进 WS 广播");
        assert!(stored.iter().all(|s| !s.payload_json.contains("被拦下")), "广播载荷不得含未过审文本");

        // 但四条都已落库留痕（供人审/申诉；§0.3 落库即事实，不做事后删改）。
        let all = sqlx::query("SELECT * FROM world_events WHERE world_id='w1' ORDER BY sequence ASC")
            .fetch_all(&state.db)
            .await
            .unwrap();
        assert_eq!(all.len(), 4, "未过审事件仍落库留痕，只是不外发");

        // ② 查询层 GET /worlds/{id}/events。
        let (code, body) = get_events(&state, &token(&state, "u1"), "w1").await;
        assert_eq!(code, StatusCode::OK, "body={body}");
        assert_eq!(
            domain_ids(body["events"].as_array().unwrap()),
            vec!["de-ok", "de-priv-ok"],
            "未过审事件不得出现在事件列表"
        );

        // ③ 推送层断线补偿：与 list_events 共用 VISIBLE_EVENTS_SQL（同一口径，不会漂移）。
        let rows = sqlx::query(VISIBLE_EVENTS_SQL)
            .bind("w1")
            .bind(-1i64)
            .bind("%\"u1\"%")
            .bind(STREAM_BACKFILL_LIMIT)
            .fetch_all(&state.db)
            .await
            .unwrap();
        let backfilled: Vec<Value> =
            rows.iter().filter_map(|r| row_to_event(r, "u1").unwrap()).collect();
        assert_eq!(domain_ids(&backfilled), vec!["de-ok", "de-priv-ok"], "断线补偿不得回放未过审事件");

        // ④ 逐行复核：即便某个新读取面忘了带 SQL 审核门，row_to_event 仍然拦得住（双层）。
        let visible: Vec<Value> = all.iter().filter_map(|r| row_to_event(r, "u1").unwrap()).collect();
        assert_eq!(domain_ids(&visible), vec!["de-ok", "de-priv-ok"], "row_to_event 必须独立拦截未过审行");
    }

    /// AI 标识是红线（总规格 §0.6）：审核态改写不得连带把 ai_label 改掉。
    #[tokio::test]
    async fn ai_label_stays_on_for_every_projected_event() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        persist_events(
            &state.db,
            "w1",
            0,
            &[
                projected("de-ok", "正常公开事实", MODERATION_APPROVED, &[]),
                projected("de-bad", "被拦下的公开事实", "pending", &[]),
            ],
        )
        .await
        .unwrap();
        let off = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM world_events WHERE ai_label <> 1")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(off, 0, "ai_label 必须恒为 1，不可被审核路径改写或绕过");
        let (_, body) = get_events(&state, &token(&state, "u1"), "w1").await;
        assert_eq!(body["events"][0]["aiLabel"]["visible"], json!(true));
    }

    // ---------- §4.2 ②：引擎事实字段（consequence / purpose / action）进事件投影 ----------

    /// 造一条引擎口径的 DomainEvent（字段与 `crates/muse-engine/src/narrative/mod.rs` 的事件构造处一致）。
    fn domain_event(id: &str, event_type: DomainEventType, actors: &[&str], fact: Value) -> DomainEvent {
        DomainEvent {
            schema_version: 1,
            id: id.into(),
            run_id: "w1".into(),
            sequence: 0,
            timestamp: 0,
            event_type,
            actor_ids: actors.iter().map(|s| s.to_string()).collect(),
            target_ids: None,
            fact,
            state_patch_id: "patch-0".into(),
            caused_by: Vec::new(),
            visibility: EventVisibility::Public,
        }
    }

    fn summary_of(ev: &DomainEvent) -> String {
        project_domain_events(std::slice::from_ref(ev), &[]).remove(0).summary
    }

    /// `ActionResolved.fact = {result, action, consequence}` / `DialogueSpoken.fact = {purpose}` —— 此前
    /// 这些键一个都不在取值表里，引擎产的每条事件都掉进兜底分支投影成 `"action · 沈砚"`，
    /// 事件流里没有任何可比对的叙事文本（VALIDATION §4.2「剧情重复率」缺口的直接成因）。
    #[test]
    fn engine_fact_fields_enter_projection_summary() {
        // ① ActionResolved：取仲裁给出的结果事实 consequence（而非 result 枚举名、而非兜底串）。
        let action = domain_event(
            "de-act",
            DomainEventType::ActionResolved,
            &["shenyan"],
            json!({ "result": "Success", "action": "上前拱手行礼", "consequence": "裴照侧身避开了这一礼。" }),
        );
        assert_eq!(summary_of(&action), "裴照侧身避开了这一礼。");

        // ② DialogueSpoken：唯一的事实字段就是 purpose。
        let dialogue =
            domain_event("de-say", DomainEventType::DialogueSpoken, &["cuie"], json!({ "purpose": "试探对方来意" }));
        assert_eq!(summary_of(&dialogue), "试探对方来意");

        // ③ 仲裁未给 consequence（空串）→ 回落 action，仍是有信息的文本。
        let no_conseq = domain_event(
            "de-noc",
            DomainEventType::ActionResolved,
            &["shenyan"],
            json!({ "result": "PartialSuccess", "action": "推开半掩的门", "consequence": "" }),
        );
        assert_eq!(summary_of(&no_conseq), "推开半掩的门");

        // ④ 显式摘要优先级仍最高（合成事件/arena 自带成稿文案不被引擎字段抢走）。
        let explicit = domain_event(
            "de-sum",
            DomainEventType::ActionResolved,
            &["shenyan"],
            json!({ "summary": "成稿文案", "action": "行礼", "consequence": "被避开" }),
        );
        assert_eq!(summary_of(&explicit), "成稿文案");

        // ⑤ 事实层全空 → 仍退化为原兜底串（老行为不回退）。
        let empty = domain_event(
            "de-empty",
            DomainEventType::ActionResolved,
            &["shenyan", "peizhao"],
            json!({ "result": "Success", "action": "   ", "consequence": "" }),
        );
        assert_eq!(summary_of(&empty), "action · shenyan,peizhao");
    }

    /// 🔴 **红线用例**：新增的事实字段文本必须与原有摘要走**同一条**内容安全闸。
    ///
    /// 复刻 `runtime::commit_tick` 的真实次序（投影 → §15 第 2 层闸 → 落库），断言一条带敏感词的
    /// `consequence` 被打码 + 置 pending + 不进广播 + 不进读取面。若 `event_summary` 新增的文本
    /// 绕到闸之后再拼进投影，这条会红 —— 那等于开了一条绕过词库过滤的通道。
    #[tokio::test]
    async fn engine_fact_text_still_passes_through_the_safety_gate() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;

        let dirty = domain_event(
            "de-dirty",
            DomainEventType::ActionResolved,
            &["c1"],
            json!({ "result": "Success", "action": "翻开包袱", "consequence": "他从怀里掏出一包冰毒。" }),
        );
        let clean = domain_event(
            "de-clean",
            DomainEventType::DialogueSpoken,
            &["c1"],
            json!({ "purpose": "邀对方入席共饮" }),
        );

        // 闸之前：敏感文本确实已经进了投影（证明它走的是被把关的那条路，而不是绕过去的旁路）。
        let mut projected = project_domain_events(&[dirty, clean], &[]);
        assert_eq!(projected[0].summary, "他从怀里掏出一包冰毒。", "consequence 必须在过闸前就进投影");

        // commit_tick 的真实次序：同一事务内 投影 → 第 2 层闸 → 落库。
        let mut tx = state.db.begin().await.unwrap();
        let blocked = crate::safety::moderate_runtime_projection(&mut tx, "w1", &mut projected).await.unwrap();
        let stored = insert_events_tx(&mut tx, "w1", 0, &projected).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(blocked, 1, "带敏感词的 consequence 必须被闸拦下");
        assert!(!projected[0].summary.contains('冰') && !projected[0].summary.contains('毒'), "{}", projected[0].summary);
        assert_eq!(projected[0].moderation, "pending", "命中事件置为待人审");
        assert_eq!(projected[1].summary, "邀对方入席共饮", "未命中的 purpose 文本原样放行");
        assert_eq!(projected[1].moderation, MODERATION_APPROVED);

        // 落库的即是打码后的最终事实（§0.3 落库前改写，不做事后回滚）。
        let db_proj: String = sqlx::query_scalar(
            "SELECT public_projection_json FROM world_events WHERE domain_event_id='de-dirty'",
        )
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert!(!db_proj.contains("冰毒"), "落库文本不得含敏感词：{db_proj}");

        // 推送层：未过审事件不进广播。
        assert_eq!(stored.len(), 1);
        assert!(stored[0].payload_json.contains("de-clean"));
        assert!(!stored[0].payload_json.contains("冰毒"));

        // 查询层：未过审事件不下发。
        let (code, body) = get_events(&state, &token(&state, "u1"), "w1").await;
        assert_eq!(code, StatusCode::OK, "body={body}");
        assert_eq!(domain_ids(body["events"].as_array().unwrap()), vec!["de-clean"]);
        assert!(!body.to_string().contains("冰毒"), "读取面不得泄露被拦文本");
    }

    #[tokio::test]
    async fn state_summary_requires_auth() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        let (s, _) = get_summary(&state, None, "w1").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "AuthUser 守卫：缺凭证应 401");
    }

    // ---------- sequence 发号器（迁移 0043 / `allocate_sequences_tx`） ----------

    /// 读回某世界全部事件的 sequence，升序（`id` 作次级键：撞号时并列行在 PG 上顺序不定，
    /// 补齐次级键才是全序——口径同 `docs/VALIDATION.md` §3.3 第 1 类处置）。
    async fn sequences_of(db: &AnyPool, world: &str) -> Vec<i64> {
        sqlx::query_scalar::<_, i64>(
            "SELECT sequence FROM world_events WHERE world_id = $1 ORDER BY sequence ASC, id ASC",
        )
        .bind(world)
        .fetch_all(db)
        .await
        .unwrap()
    }

    /// 发号器行的当前值（未建行则 None）。
    async fn next_seq_of(db: &AnyPool, world: &str) -> Option<i64> {
        sqlx::query_scalar::<_, i64>("SELECT next_seq FROM world_event_seq WHERE world_id = $1")
            .bind(world)
            .fetch_optional(db)
            .await
            .unwrap()
    }

    /// 基础口径：从 0 起、批内连续、跨批续上，且发号器行与已落库事件严格对齐。
    ///
    /// 这条同时锁住「空世界首条事件拿 0」——改造前 `COALESCE(MAX(sequence), -1) + 1` 给的也是 0，
    /// 发号器的 `DEFAULT 0` 必须与之逐值一致，否则全仓断言 `sequence == 0` 的用例会集体漂移。
    #[tokio::test]
    async fn sequence_allocation_is_contiguous_across_batches() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        assert_eq!(next_seq_of(&state.db, "w1").await, None, "建世界不建发号器行（惰性建行）");

        persist_events(&state.db, "w1", 0, &[projected("de-1", "一", MODERATION_APPROVED, &[])])
            .await
            .unwrap();
        persist_events(
            &state.db,
            "w1",
            1,
            &[
                projected("de-2", "二", MODERATION_APPROVED, &[]),
                projected("de-3", "三", MODERATION_APPROVED, &[]),
            ],
        )
        .await
        .unwrap();

        assert_eq!(sequences_of(&state.db, "w1").await, vec![0, 1, 2]);
        assert_eq!(next_seq_of(&state.db, "w1").await, Some(3), "发号器停在下一个待发号上");

        // 世界之间互不干扰：每个世界一把独立的号（也就是一把独立的行锁）。
        seed_world(&state.db, "w2", 0, "running").await;
        persist_events(&state.db, "w2", 0, &[projected("de-x", "别处", MODERATION_APPROVED, &[])])
            .await
            .unwrap();
        assert_eq!(sequences_of(&state.db, "w2").await, vec![0], "另一个世界仍从 0 起");
        assert_eq!(next_seq_of(&state.db, "w1").await, Some(3), "w2 的分配不动 w1 的号");
    }

    /// 发号器行**缺失**、而世界已有事件时，首次分配必须续在历史之后而不是从 0 重来。
    ///
    /// 这正是迁移 `0043` 回填语句所保证的性质（`MAX(sequence)+1`），此处用等价的运行时路径
    /// （`allocate_sequences_tx` 建行时同一条子查询）把它钉住：
    /// - 覆盖**滚动发布**残余窗口——旧实例用老口径写入的世界尚未登记进发号器；
    /// - 也覆盖全仓大量「先 raw INSERT 造 world_events 再走落库路径」的既有 fixture，
    ///   它们依赖的正是"接着历史往下发号"，改造后必须逐值不变。
    #[tokio::test]
    async fn allocation_resumes_after_rows_written_outside_the_counter() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        // 模拟存量：绕过发号器直接落三行（sequence 0/1/2）。
        for seq in 0..3i64 {
            sqlx::query(
                "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, \
                 event_type, visibility, occurred_at) \
                 VALUES ($1, 'w1', 0, $2, $3, 'dialogue', 'public', 0)",
            )
            .bind(new_id("we"))
            .bind(seq)
            .bind(new_id("de"))
            .execute(&state.db)
            .await
            .unwrap();
        }
        assert_eq!(next_seq_of(&state.db, "w1").await, None, "存量世界此刻没有发号器行");

        persist_events(&state.db, "w1", 1, &[projected("de-new", "新", MODERATION_APPROVED, &[])])
            .await
            .unwrap();

        assert_eq!(sequences_of(&state.db, "w1").await, vec![0, 1, 2, 3], "必须续号，不得从 0 重来");
        assert_eq!(next_seq_of(&state.db, "w1").await, Some(4));
    }

    /// 事务回滚 ⇒ 号一并回滚，**不留空洞**（与 PG 原生 sequence 的语义差别，值得钉死）。
    ///
    /// 空洞不是无害的美观问题：`VISIBLE_EVENTS_SQL` 的断线补偿游标按 `sequence > cursor` 推进，
    /// 空洞本身可容忍，但「回滚会烧号」意味着号与事件条数不再对应，后续任何按号做的计数/对账
    /// （SLO 戏份聚合、回放 seek 分页）都会失真。
    #[tokio::test]
    async fn rolled_back_allocation_leaves_no_gap() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;

        let mut tx = state.db.begin().await.unwrap();
        let base = allocate_sequences_tx(&mut tx, "w1", 5).await.unwrap();
        assert_eq!(base, 0);
        tx.rollback().await.unwrap();

        assert_eq!(next_seq_of(&state.db, "w1").await, None, "回滚连建行一起撤销");
        persist_events(&state.db, "w1", 0, &[projected("de-1", "一", MODERATION_APPROVED, &[])])
            .await
            .unwrap();
        assert_eq!(sequences_of(&state.db, "w1").await, vec![0], "回滚掉的 5 个号没有被烧掉");
    }

    /// 🔴 **本任务的核心用例：同一世界的并发写不得撞号。**
    ///
    /// 形状刻意复刻 `docs/VALIDATION.md` §3.3 ① 点名的那条真实竞态——**两个分配站点同时打**：
    /// - 偶数任务走 `persist_events`（= `runtime::commit_tick` 的批量落库，每次 3 条）；
    /// - 奇数任务走 `persist_and_broadcast_public_event`（= `arena::emit_arena_event` 的
    ///   玩家/运营 HTTP 路径，每次 1 条）。
    ///
    /// 断言三条——① 分配出的 sequence 无重复；② 无空洞；③ 总条数正确。三条合起来等价于
    /// 「结果集恰好是 `0..总数` 的连续整数」，故直接断言相等。
    ///
    /// ⚠️ **只有 Postgres 那一遍是证据。** SQLite 的 `:memory:` 不支持多连接（每连接一个独立库），
    /// 池回落到单连接 ⇒ 本用例退化为顺序执行；且即便用文件库，SQLite 的单写者锁也会让
    /// 旧的 `MAX(sequence)+1` 口径同样通过 = 假绿。跑 PG 那遍：
    /// `MUSE_TEST_DATABASE_URL=postgres://... cargo test concurrent_sequence`。
    ///
    /// **实测（PG 16.9，`TASKS=24` ⇒ 48 条事件）**：把分配临时回退成改造前的
    /// `SELECT COALESCE(MAX(sequence),-1)+1` 之后，48 条事件只拿到 **23 个不同的号**
    /// （`[0,1,1,2,2,2,3,3,4,5,5,…]`，25 条撞号），本条立刻变红；改回发号器后是连续的 0..47。
    /// 同一次回退在 **SQLite 上①②③三条断言全绿**（只有末尾那条发号器行的对账断言红，
    /// 而那条是新设计特有的）——单写者锁把读-改-写事实上串行掉了。
    /// 这两组对照才是本用例的证据，「CI 绿了」不是。
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_sequence_allocation_never_collides_or_gaps() {
        const TASKS: usize = 24;
        const BATCH_EVENTS: usize = 3;

        let pool = crate::testkit::test_pool_concurrent(TASKS as u32).await;
        let state = AppState::new(pool.clone(), crate::config::ServerConfig::from_env());
        let mut handles = Vec::with_capacity(TASKS);
        for t in 0..TASKS {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                if t % 2 == 0 {
                    // commit_tick 侧：整批一次领号。
                    let batch: Vec<ProjectedEvent> = (0..BATCH_EVENTS)
                        .map(|i| {
                            projected(&format!("de-{t}-{i}"), "并发事实", MODERATION_APPROVED, &[])
                        })
                        .collect();
                    persist_events(&state.db, "w_race", t as i64, &batch)
                        .await
                        .expect("并发批量落库不应报错");
                } else {
                    // arena 侧：玩家/运营 HTTP 路径，单条领号。
                    persist_and_broadcast_public_event(
                        &state,
                        "w_race",
                        t as i64,
                        "arena_gift",
                        "并发系统事件",
                        &["c1".to_string()],
                        json!({ "arenaKind": "gift" }),
                    )
                    .await
                    .expect("并发系统事件落库不应报错");
                }
            }));
        }
        for h in handles {
            h.await.expect("任务不应 panic");
        }

        let seqs = sequences_of(&pool, "w_race").await;
        // 偶数任务各 3 条 + 奇数任务各 1 条。
        let total = TASKS / 2 * BATCH_EVENTS + TASKS / 2;

        // ③ 总数正确（先查这条：条数不对说明有事务整体失败，后两条的诊断会被带偏）。
        assert_eq!(seqs.len(), total, "落库条数应为 {total}");
        // ① 无重复。
        let uniq: BTreeSet<i64> = seqs.iter().copied().collect();
        assert_eq!(uniq.len(), seqs.len(), "sequence 撞号：{seqs:?}");
        // ②（+①+③）无空洞 —— 合起来即「恰好是 0..total 的连续整数」。
        assert_eq!(seqs, (0..total as i64).collect::<Vec<_>>(), "sequence 应连续无空洞");

        // 发号器自身的终值也必须与事件条数对齐（防「号发了但事件没落」的静默错位）。
        assert_eq!(next_seq_of(&pool, "w_race").await, Some(total as i64));
    }

    /// 批内每一条都拿到**自己**的号（批量领号不是"整批共用一个号"）。
    ///
    /// 与上一条互补：并发用例每任务 3 条，若批内共号，`uniq.len()` 会掉下来但很难定位到成因。
    #[tokio::test]
    async fn batch_allocation_hands_out_one_number_per_event() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        let batch: Vec<ProjectedEvent> = (0..4)
            .map(|i| projected(&format!("de-{i}"), "批内", MODERATION_APPROVED, &[]))
            .collect();
        let stored = persist_events(&state.db, "w1", 0, &batch).await.unwrap();
        assert_eq!(stored.len(), 4);
        assert_eq!(sequences_of(&state.db, "w1").await, vec![0, 1, 2, 3]);
        // 广播载荷里的 sequence 与落库值同源（客户端按它去重 + 推进补偿游标）。
        let in_payload: Vec<i64> = stored
            .iter()
            .map(|s| serde_json::from_str::<Value>(&s.payload_json).unwrap()["sequence"]
                .as_i64()
                .unwrap())
            .collect();
        assert_eq!(in_payload, vec![0, 1, 2, 3]);
    }
}
