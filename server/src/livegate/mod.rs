//! 直播网关（P6b，feature=arena）：观众礼物 → 场内环境增益（专用系统通道）。
//!
//! POST /livegate/webhook   直播平台礼物回调（body={worldId, giftSku, count, fromUser?}）。
//!   dev 态：签名校验开关（dev_mode=true 直通模拟事件）；生产：校验回调签名头（真实接入位）。
//!   流程：SKU→boon 映射查表 → **写 arena_env_events(kind='gift_boon')** → 同回合同 SKU 聚合 → 记 gift_events 账。
//! GET  /arena/{worldId}/clips  高光切片列表（clip_jobs 只读；主播控制台/战报用）。
//!
//! 铁律（§2.5）：礼物是**系统代投的环境/道具增益**，走 arena_env_events 专用通道，
//! **不走玩家 interventions**（HC 已禁用玩家 item 干预）。买"过程"不买"结果"——
//! boon 只映射有限枚举的过程增益（advantage/reroll/info），不含免死/最终判定（映射表 0008 已约束）。
//! 聚合：同 world 未落地(applied_tick IS NULL)的同 SKU gift_boon 合并 aggregated_count，防事件风暴。
//!
//! 扣费（P1，feature=arena → ledger 恒在）：
//! - **站内打赏** `POST /arena/{worldId}/gift`：单价从 `gift_sku_map.price_cents` 读，总价 = 单价 × count，
//!   经 `ledger::charge`（world→template.owner 分成、自打赏防刷、余额不足 409 零副作用）**与 gift 副作用同一事务原子**。
//! - **外部 webhook**（观众已在直播平台付费）：仅记账（gift_events via='livegate'），**不站内二次扣钱包**（红线）。
//! seam：未成年人礼物限额 / 外部打赏分成对账批处理（按 via 分流）后接。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Any, Row, Transaction};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/livegate/webhook", post(webhook))
        .route("/arena/{worldId}/gift", post(spectator_gift))
        .route("/arena/{worldId}/clips", get(list_clips))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GiftWebhook {
    world_id: String,
    gift_sku: String,
    #[serde(default = "default_count")]
    count: i64,
    #[serde(default)]
    from_user: Option<String>,
}
fn default_count() -> i64 {
    1
}

/// 礼物回调：签名校验开关 → SKU 映射 → arena_env_events 聚合写入 → gift_events 记账。
///
/// 用原始 body（Bytes）而非 Json 提取器，以便对原文验签；随后手动解析。
async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    verify_signature(&state, &headers, &body)?;

    let req: GiftWebhook = serde_json::from_slice(&body)
        .map_err(|e| ApiError::BadRequest(format!("礼物回调载荷无效: {e}")))?;
    if req.count <= 0 {
        return Err(ApiError::BadRequest("count 必须为正".into()));
    }

    // 幂等（可选）：平台重投带同 Idempotency-Key → 返回缓存，避免重复计数（聚合亦已收敛，双保险）。
    let idem_key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(&body);
    let guard =
        idempotency::guard(&state.db, "livegate", "POST /livegate/webhook", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // 世界必须存在（礼物投向某世界；避免为不存在的世界写入环境事件）。
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM worlds WHERE id = ?")
        .bind(&req.world_id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound);
    }

    // seam：未成年人礼物限额——真实身份接入后在此对 from_user 做限额判定。
    // 外部 webhook：观众已在直播平台付费，站内**不扣钱包**（红线：外部资金入账走对账批处理，非站内 charge）。
    // gift 副作用（env_events + gift_events）单事务原子；进流 arena_gift 为 commit 后事后广播。
    let gift_id = new_id("gift");
    let mut tx = state.db.begin().await?;
    let outcome = apply_gift_tx(
        &mut tx,
        &req.world_id,
        &req.gift_sku,
        req.count,
        req.from_user.as_deref(),
        "livegate",
        &gift_id,
    )
    .await?;
    tx.commit().await?;
    if let Some(emit) = &outcome.emit {
        emit_gift_stream(&state, &req.world_id, emit).await;
    }

    guard.store_response(&state.db, &outcome.response.to_string()).await?;
    Ok(Json(outcome.response))
}

/// gift 落地结果：`response` 已成型（handler 直接回客户端）；`emit` 非空 → commit 后进 public 流 `arena_gift`。
struct GiftOutcome {
    response: Value,
    emit: Option<GiftEmit>,
}

/// commit 后进流所需（进流是事后广播，不进事务、失败仅 log，见 `arena::emit_arena_event`）。
struct GiftEmit {
    label_text: String,
    sku: String,
    count: i64,
    aggregated: i64,
}

/// 礼物落地核心（外部 livegate webhook 与站内观众打赏共用，**在调用方事务内**执行，与 charge 原子）：
/// SKU→boon 映射查表 → 命中则写/聚合 `arena_env_events(kind='gift_boon')`（进流延后到 commit）；
/// 无论是否命中都逐笔记 `gift_events`（`via` 区分来源，`gift_id` 与 charge 的 journal ref_id 一致，便于审计溯源）。
///
/// 红线（§2.5）：本函数**只**写 `arena_env_events` + `gift_events` 两张系统频道表，
/// **绝不** touch `arena_matches.eliminations_json/winner_char_id` 或 `interventions`（HC 已禁玩家 item 干预），
/// **绝不**改仲裁/胜负（买过程不买结果）。SKU 映射表（0008）已约束 boon 仅 advantage/reroll/info 过程增益。
async fn apply_gift_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    sku: &str,
    count: i64,
    from_user: Option<&str>,
    via: &str,
    gift_id: &str,
) -> Result<GiftOutcome, ApiError> {
    // SKU → boon 映射查表（未命中或停用 = 未映射）。label 供进流展示文案。
    let mapping: Option<(String, String)> = sqlx::query_as::<_, (String, String)>(
        "SELECT boon_json, label FROM gift_sku_map WHERE sku = ? AND enabled = 1",
    )
    .bind(sku)
    .fetch_optional(&mut **tx)
    .await?;

    let outcome = match mapping {
        Some((boon_json, label)) => {
            let boon: Value = serde_json::from_str(&boon_json).unwrap_or_else(|_| json!({}));
            // 写入/聚合 gift_boon 到 arena_env_events（专用系统环境通道，不走玩家 interventions）。
            let (env_event_id, aggregated) = upsert_gift_boon(tx, world_id, sku, &boon, count).await?;
            record_gift(tx, gift_id, world_id, sku, count, from_user, true, Some(&env_event_id), via).await?;

            let label_text = if label.is_empty() { sku.to_string() } else { label };
            let response = json!({
                "worldId": world_id,
                "sku": sku,
                "count": count,
                "mapped": true,
                "boon": boon,
                "envEventId": env_event_id,
                "aggregatedCount": aggregated,
                // 付费边界（诚实标注）：买过程增益，不是免死、不改最终判定。
                "boundary": { "buys": "process_boon", "notImmunity": true, "notFinalVerdict": true },
            });
            GiftOutcome {
                response,
                emit: Some(GiftEmit { label_text, sku: sku.to_string(), count, aggregated }),
            }
        }
        None => {
            // 未映射：无 boon 可代投（无法凭空生成过程增益）——不写 arena_env_events、不进流，
            // 仍记 gift_events 账用于对账/结算（礼物已在直播端发生）。
            record_gift(tx, gift_id, world_id, sku, count, from_user, false, None, via).await?;
            GiftOutcome {
                response: json!({
                    "worldId": world_id,
                    "sku": sku,
                    "count": count,
                    "mapped": false,
                    "boon": Value::Null,
                    "boundary": { "buys": "process_boon", "notImmunity": true, "notFinalVerdict": true },
                }),
                emit: None,
            }
        }
    };
    Ok(outcome)
}

/// 打赏进流：public 系统事件（观众实时看到环境被注入）。**commit 后的事后广播**——仅广播，
/// 红线内不碰赛制结果字段；失败仅 log（见 `arena::emit_arena_event`），不回滚已落定的 gift。
async fn emit_gift_stream(state: &AppState, world_id: &str, emit: &GiftEmit) {
    crate::arena::emit_arena_event(
        state,
        world_id,
        "arena_gift",
        &format!("观众打赏「{}」×{} 已注入场内环境（系统代投）", emit.label_text, emit.count),
        &[],
        json!({ "arenaKind": "gift", "sku": emit.sku, "aggregatedCount": emit.aggregated }),
    )
    .await;
}

// ---------- POST /arena/{worldId}/gift（站内观众打赏，AuthUser + 观战资格守卫） ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpectatorGiftReq {
    sku: String,
    #[serde(default = "default_count")]
    count: i64,
}

/// 站内观众打赏：走与外部 webhook 同一 `apply_gift_tx`（同 upsert 聚合 + 同 arena_env_events 系统频道 +
/// 同 arena_gift 进流），`via='in_app'` 区分来源。守卫 = AuthUser + `can_view_world`（与观战/回放同口径）。
///
/// 红线：只写系统频道，绝不触碰 eliminations/winner/interventions；charge 成功不改任何仲裁/胜负标志。
/// 扣费（P1）：站内打赏走钱包 —— 单价 `gift_sku_map.price_cents` × count，`ledger::charge`（world→template.owner
/// 分成、自打赏防刷、余额不足 409 零副作用）**与 gift 副作用同一事务原子**，要么全成要么全无。
async fn spectator_gift(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SpectatorGiftReq>,
) -> Result<Json<Value>, ApiError> {
    if body.count <= 0 {
        return Err(ApiError::BadRequest("count 必须为正".into()));
    }
    // 观战资格（复用 events::can_view_world）：official/public 任何登录用户；private 需成员/房主；
    // 世界不存在 → load_world 内返回 404。守卫在扣费/记账之前，被拒请求零副作用。
    if !crate::events::can_view_world(&state.db, &world_id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }

    // 幂等：同 Idempotency-Key 重投 → 返回缓存，计数不翻倍、不双扣（与其它副作用端点同模式）。
    let payload_hash = idempotency::hash_payload(format!("{world_id}:{}:{}", body.sku, body.count).as_bytes());
    let idem = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, "arena.gift", idem, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // 单事务：charge（钱包扣费 + 分成）与 gift 副作用（arena_env_events + gift_events）原子。
    // gift_id 既作 charge 的 journal ref_id，又作 gift_events 主键，审计链一致。
    let gift_id = new_id("gift");
    let mut tx = state.db.begin().await?;
    // SKU 单价（分）：未定价 / 未知 SKU → 0 → charge no-op（免费打赏，dev 播种 SKU 默认 0）。总价 = 单价 × count。
    let unit_price: Option<(i64,)> = sqlx::query_as("SELECT price_cents FROM gift_sku_map WHERE sku = ?")
        .bind(&body.sku)
        .fetch_optional(&mut *tx)
        .await?;
    let total_price = unit_price.map(|(p,)| p).unwrap_or(0).saturating_mul(body.count);
    // 站内打赏扣钱包 + 分成给模板 owner（自打赏防刷、未成年 owner 挂平台、取整余数归平台，均在 charge 内守）。
    // 余额不足 → ApiError::Conflict(insufficient_balance)=409，tx 随 Err drop 回滚 → 零副作用（无 gift/env/journal）。
    crate::ledger::charge(&mut tx, &user.user_id, total_price, "gift", "gift_event", &gift_id, Some(&world_id))
        .await?;
    let outcome =
        apply_gift_tx(&mut tx, &world_id, &body.sku, body.count, Some(&user.user_id), "in_app", &gift_id).await?;
    tx.commit().await?;

    if let Some(emit) = &outcome.emit {
        emit_gift_stream(&state, &world_id, emit).await;
    }
    guard.store_response(&state.db, &outcome.response.to_string()).await?;
    Ok(Json(outcome.response))
}

/// 写入/聚合 gift_boon 到 arena_env_events。返回 (env_event_id, aggregated_count)。
///
/// 聚合键 = (world_id, kind='gift_boon', applied_tick IS NULL, payload.sku)。JSON 内 SKU 无法建
/// 唯一索引，故"先查后并"（select-then-merge，可移植；dev/test 单连接、每 world 单写者，竞态极小）。
/// applied_tick=NULL 表示"待引擎回合消费"（P6a 消费时落 tick——seam）；同一待落地窗口内同 SKU 合并计数。
async fn upsert_gift_boon(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    sku: &str,
    boon: &Value,
    add_count: i64,
) -> Result<(String, i64), ApiError> {
    let payload = json!({ "sku": sku, "boon": boon, "source": "livegate_gift" });

    let rows = sqlx::query(
        "SELECT id, payload_json, aggregated_count FROM arena_env_events \
         WHERE world_id = ? AND kind = 'gift_boon' AND applied_tick IS NULL",
    )
    .bind(world_id)
    .fetch_all(&mut **tx)
    .await?;

    for r in &rows {
        let pj: String = r.try_get("payload_json")?;
        let same_sku = serde_json::from_str::<Value>(&pj)
            .ok()
            .and_then(|v| v.get("sku").and_then(Value::as_str).map(str::to_string))
            .as_deref()
            == Some(sku);
        if same_sku {
            let id: String = r.try_get("id")?;
            let next: i64 = r.try_get::<i64, _>("aggregated_count")? + add_count;
            sqlx::query("UPDATE arena_env_events SET aggregated_count = ?, payload_json = ? WHERE id = ?")
                .bind(next)
                .bind(payload.to_string())
                .bind(&id)
                .execute(&mut **tx)
                .await?;
            return Ok((id, next));
        }
    }

    let id = new_id("aee");
    sqlx::query(
        "INSERT INTO arena_env_events (id, world_id, applied_tick, kind, payload_json, aggregated_count, created_at) \
         VALUES (?, ?, NULL, 'gift_boon', ?, ?, ?)",
    )
    .bind(&id)
    .bind(world_id)
    .bind(payload.to_string())
    .bind(add_count)
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    Ok((id, add_count))
}

/// 记 gift_events 账（战报 + 结算 seam）。`via` 区分来源（'livegate' 外部 / 'in_app' 站内）供分成/审计。
/// `id` 由调用方生成（站内打赏 == charge 的 journal ref_id，审计链一致）。
#[allow(clippy::too_many_arguments)]
async fn record_gift(
    tx: &mut Transaction<'_, Any>,
    id: &str,
    world_id: &str,
    sku: &str,
    count: i64,
    from_user: Option<&str>,
    mapped: bool,
    env_event_id: Option<&str>,
    via: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO gift_events (id, world_id, sku, gift_count, from_user, mapped, env_event_id, via, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(world_id)
    .bind(sku)
    .bind(count)
    .bind(from_user)
    .bind(if mapped { 1_i64 } else { 0 })
    .bind(env_event_id)
    .bind(via)
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ---------- 签名校验（开关；生产为真实接入位） ----------

/// dev_mode=true → 直通（模拟礼物事件端点）。生产 → 校验回调签名头。
/// secret 从环境读取（不改共享 ServerConfig）；未配置密钥即 fail-closed。
/// 注：占位签名 = hex(SHA256(secret . body))；真实直播平台按其协议（多为 HMAC）替换。
fn verify_signature(state: &AppState, headers: &HeaderMap, body: &[u8]) -> Result<(), ApiError> {
    if state.config.dev_mode {
        return Ok(());
    }
    let secret = std::env::var("MUSE_LIVEGATE_SECRET").unwrap_or_default();
    if secret.is_empty() {
        return Err(ApiError::Unauthorized);
    }
    let provided = headers.get("x-livegate-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if signature_matches(&secret, provided, body) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

fn sign_body(secret: &str, body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.update(b".");
    h.update(body);
    format!("{:x}", h.finalize())
}

/// 常量时间比较，避免时序侧信道。
fn signature_matches(secret: &str, provided: &str, body: &[u8]) -> bool {
    let expected = sign_body(secret, body);
    let (a, b) = (provided.as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------- GET /arena/{worldId}/clips ----------

/// 高光切片列表（clip_jobs 只读）。观战资格复用 events::can_view_world。
async fn list_clips(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !crate::events::can_view_world(&state.db, &world_id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, event_id, object_key, status, created_at FROM clip_jobs \
         WHERE world_id = ? ORDER BY created_at DESC LIMIT 100",
    )
    .bind(&world_id)
    .fetch_all(&state.db)
    .await?;
    let clips: Vec<_> = rows
        .into_iter()
        .map(|(id, event_id, object_key, status, created_at)| {
            json!({ "id": id, "eventId": event_id, "objectKey": object_key, "status": status, "createdAt": created_at })
        })
        .collect();
    Ok(Json(json!({ "clips": clips })))
}

#[cfg(all(test, feature = "arena"))]
mod tests;
