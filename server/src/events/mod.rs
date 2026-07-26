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
        let mut lock = self.channels.lock().unwrap();
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
    let base: i64 = sqlx::query("SELECT COALESCE(MAX(sequence), -1) AS m FROM world_events WHERE world_id = $1")
        .bind(world_id)
        .fetch_one(&mut **tx)
        .await?
        .try_get("m")?;
    let now = now_ms();
    let mut out = Vec::with_capacity(projected.len());
    for (i, pe) in projected.iter().enumerate() {
        let sequence = base + 1 + i as i64;
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
/// 单事务分配 per-world 单调 sequence（复用 `insert_events_tx` 的 `MAX(sequence)+1` 口径）。
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
    let base: i64 = sqlx::query("SELECT COALESCE(MAX(sequence), -1) AS m FROM world_events WHERE world_id = $1")
        .bind(world_id)
        .fetch_one(&mut *tx)
        .await?
        .try_get("m")?;
    let sequence = base + 1;
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
}
