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
}

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

/// 面向用户的公共摘要：只取事实层可展示字段，不下发链式推理/私密状态（§9.4 透明战报边界）。
fn event_summary(ev: &DomainEvent) -> String {
    for key in ["summary", "narrative", "text"] {
        if let Some(s) = ev.fact.get(key).and_then(Value::as_str) {
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
pub async fn insert_events_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    tick_no: i64,
    projected: &[ProjectedEvent],
) -> Result<Vec<StoredEvent>, ApiError> {
    let base: i64 = sqlx::query("SELECT COALESCE(MAX(sequence), -1) AS m FROM world_events WHERE world_id = ?")
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
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'approved', 1, ?)",
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
        .bind(now)
        .execute(&mut **tx)
        .await?;

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
    let base: i64 = sqlx::query("SELECT COALESCE(MAX(sequence), -1) AS m FROM world_events WHERE world_id = ?")
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
         VALUES (?, ?, ?, ?, ?, ?, ?, 'public', NULL, ?, NULL, NULL, 'approved', 1, ?)",
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
        "SELECT 1 AS x FROM world_members WHERE world_id = ? AND user_id = ? AND status='active' LIMIT 1",
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
fn row_to_event(row: &sqlx::any::AnyRow, principal: &str) -> Result<Option<Value>, ApiError> {
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
    // SQL 先用 LIKE 粗过滤（public + audience 命中），Rust 再精确复核（双层硬隔离）。
    let like = format!("%\"{}\"%", user.user_id);
    let rows = sqlx::query(
        "SELECT * FROM world_events \
         WHERE world_id = ? AND sequence > ? AND (visibility = 'public' OR audience_json LIKE ?) \
         ORDER BY sequence ASC LIMIT ?",
    )
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

    // 断线重连补偿：下发 lastEventId 之后、当前 principal 可见的历史事件。
    if last_event_id >= 0 {
        let like = format!("%\"{principal}\"%");
        if let Ok(rows) = sqlx::query(
            "SELECT * FROM world_events \
             WHERE world_id = ? AND sequence > ? AND (visibility = 'public' OR audience_json LIKE ?) \
             ORDER BY sequence ASC LIMIT 500",
        )
        .bind(&world_id)
        .bind(last_event_id)
        .bind(&like)
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
        "SELECT cloud_character_id FROM world_members WHERE world_id = ? AND user_id = ? AND status = 'active'",
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

    Ok(Json(json!({ "relations": relations, "characters": characters })))
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
        sqlx::query("UPDATE worlds SET narrative_state_json = ? WHERE id = ?")
            .bind(s)
            .bind(world)
            .execute(db)
            .await
            .unwrap();
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
        sqlx::query("UPDATE worlds SET visibility='private' WHERE id=?")
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

    #[tokio::test]
    async fn state_summary_requires_auth() {
        let state = test_state().await;
        seed_world(&state.db, "w1", 0, "running").await;
        let (s, _) = get_summary(&state, None, "w1").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "AuthUser 守卫：缺凭证应 401");
    }
}
