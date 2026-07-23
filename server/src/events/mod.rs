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

use muse_engine::narrative::types::{DomainEvent, DomainEventType, EventVisibility};

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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worlds/{id}/events", get(list_events))
        .route("/worlds/{id}/stream", get(stream))
}
