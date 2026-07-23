//! 角色资产上云（S1，agent-S1 填）。
//!
//! 待实现端点（平台规格 §2.3 / §9.1）：
//! POST   /assets/characters            发布不可变版本：card_json + rightsDeclaration(original|public_domain_adaptation)
//!                                      → 机审 safety::moderate_and_queue(唯一入队/记险方) → cloud_characters(pending|approved)
//!                                      → audit_queue / risk_events 由 moderate_and_queue 统一落库，本模块不二次写；Idempotency-Key 必须
//! GET    /assets/characters/mine       我的云端版本列表（含审核态）
//! GET    /assets/characters/{id}/status
//! POST   /assets/characters/{id}/withdraw   停止后续投放（withdrawn=1；运行中世界按入场协议处理，S3 消费）
//! DELETE /assets/characters/{id}       异步删除任务（data_requests）：从未投放 → 立删；已投放 → 标记 + 任务清理
//!
//! 铁律：card_json 服务端只做校验与存储，绝不信任客户端声明的审核态/版本号（§9.6）。

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;
use crate::providers::ModerationVerdict;
use crate::safety;

/// 世界超集资产上云端点（`/assets/worlds`，与本模块 `/assets/characters` 同款资产生命周期）。
pub mod worlds;

/// card_json 上限（防滥用）；最小发布清单只需角色版本 + 权利元数据（§2.3）。
const MAX_CARD_BYTES: usize = 256 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/assets/characters", post(publish))
        .route("/assets/characters/mine", get(list_mine))
        .route("/assets/characters/{id}/status", get(status))
        .route("/assets/characters/{id}/manifest", get(manifest))
        .route("/assets/characters/{id}/withdraw", post(withdraw))
        .route("/assets/characters/{id}", delete(delete_character))
        .merge(worlds::router())
}

// ---------------- 请求 / 响应类型 ----------------

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishReq {
    local_card_id: String,
    /// CharacterCardV2 形态（crates/muse-engine character::types）；服务端只做结构校验与存储。
    card_json: serde_json::Value,
    /// original | public_domain_adaptation
    rights_declaration: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CharacterView {
    id: String,
    local_card_id: String,
    version: i64,
    rights_declaration: String,
    moderation: String,
    withdrawn: bool,
    created_at: i64,
}

// ---------------- 辅助 ----------------

fn json_response(body: String) -> Response {
    ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn idem_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn valid_rights(s: &str) -> bool {
    matches!(s, "original" | "public_domain_adaptation")
}

/// 逐字段用途映射（§2.3 可审计 manifest 的「用途」维度，落到字段粒度）。
/// 已知 CharacterCardV2 顶层字段给明确用途，未知字段回落到通用叙事用途。
fn field_purpose(field: &str) -> &'static str {
    match field {
        "schemaVersion" => "卡片结构版本标识",
        "id" | "localCardId" => "本地卡片标识（关联用户私有模板）",
        "lifecycle" => "卡片生命周期状态（draft/reviewed/ready）",
        "identity" => "角色身份设定（姓名/外观/背景）",
        "dramaticCore" => "戏剧核心（核心矛盾与欲望）",
        "decisionModel" => "决策模型（价值排序与行为倾向）",
        "perception" => "感知与信息获取设定",
        "emotionDynamics" => "情绪动力学",
        "relationGrammar" => "关系语法（与他人交互规则）",
        "expressionFingerprint" => "表达指纹（文风与口癖）",
        "agency" => "能动性与目标设定",
        "growthArc" => "成长弧线",
        "worldAdaptation" => "世界适配设定",
        "evidenceIndex" => "证据索引（引用完整性校验）",
        "revision" | "createdAt" | "updatedAt" => "版本与时间元数据",
        _ => "角色运行所需字段",
    }
}

/// 构造可审计 manifest（§2.3）：列明「字段清单 / 用途 / 可见范围 / 删除策略」。
/// 字段清单只列卡片实际上传的顶层字段，兑现「最小发布清单」——不额外声明未上传内容。
fn build_manifest(card: &serde_json::Value, rights: &str, version: i64) -> serde_json::Value {
    let fields: Vec<serde_json::Value> = card
        .as_object()
        .map(|obj| {
            obj.keys()
                .map(|k| serde_json::json!({ "name": k, "purpose": field_purpose(k) }))
                .collect()
        })
        .unwrap_or_default();

    serde_json::json!({
        "schemaVersion": 1,
        "assetKind": "character",
        "version": version,
        "rightsDeclaration": rights,
        "generatedAt": now_ms(),
        // 字段清单：逐字段用途（只含实际上传字段）
        "fields": fields,
        // 用途：整体使用边界
        "purpose": "作为不可变角色快照投放于世界，仅用于叙事决策生成与安全审核；不用于模型训练，不回写本地模板",
        // 可见范围
        "visibility": {
            "scope": "world_participants",
            "note": "仅所投放世界的参与者按受众投影可见；私密房仅降低发现与传播范围，不改变平台审核与版权义务"
        },
        // 删除策略
        "deletionPolicy": {
            "onWithdraw": "撤回后停止后续投放；运行中世界引用的不可变快照按入场协议处理",
            "onDelete": "从未投放立即删除；已投放登记异步删除任务并停止后续投放",
            "retention": "依法或履约必须保留的最小履约日志按期限留存后清除"
        }
    })
}

/// 机审裁决 → cloud_characters.moderation（服务端权威，不信客户端声明）。
/// 裁决由 safety::moderate_and_queue 统一给出：注入命中即便 provider 直过也已折叠为 Pending
/// （保守阈值，§14 最高优先级威胁），此处只做字符串映射，不重复判定/落库。
fn verdict_str(verdict: ModerationVerdict) -> &'static str {
    match verdict {
        ModerationVerdict::Approved => "approved",
        ModerationVerdict::Pending => "pending",
        ModerationVerdict::Rejected => "rejected",
    }
}

// ---------------- handler ----------------

/// POST /assets/characters：发布不可变角色版本（服务端权威版本号 + 机审 + 幂等）。
async fn publish(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<PublishReq>,
) -> Result<Response, ApiError> {
    let local_card_id = req.local_card_id.trim().to_string();
    if local_card_id.is_empty() {
        return Err(ApiError::BadRequest("localCardId 必填".into()));
    }
    if !valid_rights(&req.rights_declaration) {
        return Err(ApiError::BadRequest("rightsDeclaration 非法".into()));
    }
    // card_json 结构校验：必须是非空对象；若声明 schemaVersion 必须为 2（防降级/伪造）。
    let obj = req
        .card_json
        .as_object()
        .ok_or_else(|| ApiError::BadRequest("cardJson 必须是对象".into()))?;
    if obj.is_empty() {
        return Err(ApiError::BadRequest("cardJson 不能为空".into()));
    }
    if let Some(sv) = obj.get("schemaVersion").and_then(|v| v.as_i64()) {
        if sv != 2 {
            return Err(ApiError::BadRequest("schemaVersion 必须为 2".into()));
        }
    }
    let card_text = req.card_json.to_string();
    if card_text.len() > MAX_CARD_BYTES {
        return Err(ApiError::BadRequest("cardJson 过大".into()));
    }

    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let key = idem_key(&headers);
    let guard =
        idempotency::guard(&state.db, &user.user_id, "POST /assets/characters", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }

    // 服务端权威版本号：按 owner + localCardId 递增，忽略客户端任何 version 声明。
    let max_version: Option<i64> =
        sqlx::query_scalar("SELECT MAX(version) FROM cloud_characters WHERE owner_id = ? AND local_card_id = ?")
            .bind(&user.user_id)
            .bind(&local_card_id)
            .fetch_one(&state.db)
            .await?;
    let version = max_version.unwrap_or(0) + 1;

    let id = new_id("cchar");
    let now = now_ms();

    // 机审 + 注入检测由 safety::moderate_and_queue 统一完成——它是唯一的入队(audit_queue)/
    // 记险(risk_events)方；此处只取其返回裁决，绝不再自行落库（消除命中卡 2 条 open + 2 条 risk 的双写）。
    // 检测在「语义拼接文本」（卡片各字段值）而非序列化 JSON 串上进行，绕过跨字段/跨元素分段绕过。
    let scan_text = safety::card_scan_text(&req.card_json);
    let verdict = safety::moderate_and_queue(&state, "character", &id, &scan_text).await?;
    let moderation = verdict_str(verdict);

    // 可审计 manifest（§2.3）：随快照物化，供后台审核 / 合规核对最小发布清单。
    let manifest = build_manifest(&req.card_json, &req.rights_declaration, version);
    let manifest_text = manifest.to_string();

    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, rights_declaration, moderation, withdrawn, manifest_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(&user.user_id)
    .bind(&local_card_id)
    .bind(version)
    .bind(&card_text)
    .bind(&req.rights_declaration)
    .bind(moderation)
    .bind(&manifest_text)
    .bind(now)
    .execute(&state.db)
    .await?;

    let resp = CharacterView {
        id,
        local_card_id,
        version,
        rights_declaration: req.rights_declaration,
        moderation: moderation.to_string(),
        withdrawn: false,
        created_at: now,
    };
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// GET /assets/characters/mine：我的云端版本列表（owner 隔离，含审核态）。
async fn list_mine(State(state): State<AppState>, user: AuthUser) -> Result<Response, ApiError> {
    let rows: Vec<(String, String, i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, local_card_id, version, rights_declaration, moderation, withdrawn, created_at FROM cloud_characters WHERE owner_id = ? ORDER BY created_at DESC, version DESC",
    )
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<CharacterView> = rows
        .into_iter()
        .map(|(id, local_card_id, version, rights, moderation, withdrawn, created_at)| CharacterView {
            id,
            local_card_id,
            version,
            rights_declaration: rights,
            moderation,
            withdrawn: withdrawn != 0,
            created_at,
        })
        .collect();
    let body = serde_json::to_string(&items).map_err(ApiError::internal)?;
    Ok(json_response(body))
}

/// GET /assets/characters/{id}/status：审核态查询（owner 隔离，非本人 → 404 不泄露存在性）。
/// 内联可审计 manifest（§2.3），发布方可直接预览云端副本的字段/用途/可见范围/删除策略。
async fn status(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> Result<Response, ApiError> {
    let row: Option<(String, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT moderation, version, withdrawn, manifest_json FROM cloud_characters WHERE id = ? AND owner_id = ?",
    )
    .bind(&id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let (moderation, version, withdrawn, manifest_json) = row.ok_or(ApiError::NotFound)?;
    let manifest = manifest_json
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let resp = serde_json::json!({
        "id": id,
        "moderation": moderation,
        "version": version,
        "withdrawn": withdrawn != 0,
        "manifest": manifest,
    });
    Ok(json_response(serde_json::to_string(&resp).unwrap()))
}

/// GET /assets/characters/{id}/manifest：可审计 manifest（§2.3，owner 隔离）。
/// 独立端点便于发布前预览与合规审计取用；非本人 → 404 不泄露存在性。
async fn manifest(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> Result<Response, ApiError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT manifest_json FROM cloud_characters WHERE id = ? AND owner_id = ?")
            .bind(&id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    let (manifest_json,) = row.ok_or(ApiError::NotFound)?;
    let manifest = manifest_json
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    Ok(json_response(serde_json::to_string(&manifest).unwrap()))
}

/// POST /assets/characters/{id}/withdraw：停止后续投放（owner 校验 → withdrawn=1；天然幂等）。
async fn withdraw(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let endpoint = format!("POST /assets/characters/{id}/withdraw");
    let payload_hash = idempotency::hash_payload(id.as_bytes());
    let key = idem_key(&headers);
    let guard = idempotency::guard(&state.db, &user.user_id, &endpoint, key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }
    let owned: Option<(String,)> =
        sqlx::query_as("SELECT id FROM cloud_characters WHERE id = ? AND owner_id = ?")
            .bind(&id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    sqlx::query("UPDATE cloud_characters SET withdrawn = 1 WHERE id = ? AND owner_id = ?")
        .bind(&id)
        .bind(&user.user_id)
        .execute(&state.db)
        .await?;
    let resp = serde_json::json!({ "id": id, "withdrawn": true });
    let body = serde_json::to_string(&resp).unwrap();
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// DELETE /assets/characters/{id}：从未投放 → 立删；已投放 → 标记撤回 + data_requests 异步清理任务。
async fn delete_character(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let endpoint = format!("DELETE /assets/characters/{id}");
    let payload_hash = idempotency::hash_payload(id.as_bytes());
    let key = idem_key(&headers);
    let guard = idempotency::guard(&state.db, &user.user_id, &endpoint, key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }
    let owned: Option<(String,)> =
        sqlx::query_as("SELECT id FROM cloud_characters WHERE id = ? AND owner_id = ?")
            .bind(&id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }

    // 是否已投放：world_members 是否引用该云端角色。
    let placed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_members WHERE cloud_character_id = ?")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    let now = now_ms();
    let req_id = new_id("dr");
    let resp = if placed == 0 {
        sqlx::query("DELETE FROM cloud_characters WHERE id = ? AND owner_id = ?")
            .bind(&id)
            .bind(&user.user_id)
            .execute(&state.db)
            .await?;
        sqlx::query(
            "INSERT INTO data_requests (id, user_id, kind, status, created_at, updated_at) VALUES (?, ?, 'delete', 'done', ?, ?)",
        )
        .bind(&req_id)
        .bind(&user.user_id)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
        serde_json::json!({ "id": id, "scope": "immediate", "status": "done", "retained": [] })
    } else {
        // 已投放：不立删（运行中世界仍引用不可变快照），停止后续投放 + 登记异步删除任务。
        sqlx::query("UPDATE cloud_characters SET withdrawn = 1 WHERE id = ? AND owner_id = ?")
            .bind(&id)
            .bind(&user.user_id)
            .execute(&state.db)
            .await?;
        sqlx::query(
            "INSERT INTO data_requests (id, user_id, kind, status, created_at, updated_at) VALUES (?, ?, 'delete', 'pending', ?, ?)",
        )
        .bind(&req_id)
        .bind(&user.user_id)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
        serde_json::json!({
            "id": id,
            "scope": "deferred",
            "status": "pending",
            "retained": ["运行中世界引用的不可变快照与最小履约日志（依约保留）"],
        })
    };
    let body = serde_json::to_string(&resp).unwrap();
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

#[cfg(test)]
mod tests;
