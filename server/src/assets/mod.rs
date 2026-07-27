//! 角色资产上云（S1，agent-S1 填）。
//!
//! 待实现端点（平台规格 §2.3 / §9.1）：
//! POST   /assets/characters            发布不可变版本：card_json + rightsDeclaration(original|public_domain_adaptation)
//!                                      → 机审 safety::moderate_and_queue(唯一入队/记险方) → cloud_characters(pending|approved)
//!                                      → audit_queue / risk_events 由 moderate_and_queue 统一落库，本模块不二次写；Idempotency-Key 必须
//! GET    /assets/characters/mine       我的云端版本列表（含审核态）
//! GET    /assets/characters/{id}/status     审核态 + rejectReason（驳回理由回显）+ appeal（申诉状态）
//!                                      + takedown（事后下架告知，migration 0044；只给状态与时间，不给运营内部理由）
//!                                      + disposalAppeal（处置申诉最近一条，migration 0045）
//! POST   /assets/characters/{id}/appeal     对机审/人审驳回发起申诉（owner-only，每主体终身一次；不改 moderation）
//! POST   /assets/characters/{id}/disposal-appeal  对**过审后被处置**发起申诉（owner-only，每次处置一次；不改 moderation）
//!                                      —— 与上一条分属两件事：那条受理 rejected（发布时被驳回），
//!                                      这条受理 restricted/removed（过审后被下架）。表与改判路径都不同，见 migration 0045
//! POST   /assets/characters/{id}/withdraw   停止后续投放（withdrawn=1；运行中世界按入场协议处理，S3 消费）
//! DELETE /assets/characters/{id}       异步删除任务（data_requests）：从未投放 → 立删；已投放 → 标记 + 任务清理
//!
//! 铁律：card_json 服务端只做校验与存储，绝不信任客户端声明的审核态/版本号（§9.6）。

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
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

/// 头像原始字节上限（512KB，防滥用/占满对象存储）。
const MAX_AVATAR_BYTES: usize = 512 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/assets/characters", post(publish))
        .route("/assets/characters/mine", get(list_mine))
        .route("/assets/characters/{id}/status", get(status))
        .route("/assets/characters/{id}/appeal", post(appeal))
        .route("/assets/characters/{id}/disposal-appeal", post(disposal_appeal))
        .route("/assets/characters/{id}/manifest", get(manifest))
        .route("/assets/characters/{id}/avatar", post(upload_avatar))
        .route("/assets/characters/{id}/withdraw", post(withdraw))
        .route("/assets/characters/{id}", delete(delete_character))
        // 对象回读（头像 / 世界封面等公开可读资产）：无鉴权，靠不可猜的对象键
        //（角色 id / 世界 id 均为 128 位随机 uuid）充当能力 URL；
        // 可读前缀白名单 + 路径穿越硬防护见 `READABLE_OBJECT_PREFIXES` / `is_safe_object_key`。
        .route("/assets/objects/{*key}", get(get_object))
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
    /// 头像回读 URL；红线：仅头像机审 approved 才回传，否则 null（未过审绝不外泄）。
    avatar_url: Option<String>,
    /// 历练值（波次 2）：挂卡的成长值，只作准入与解锁展示，绝不进入引擎决策。
    mileage: i64,
}

/// 头像上传请求（base64 JSON，复用现有 JSON 栈，不碰 multipart）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AvatarReq {
    /// 标准 base64 编码的原始图片字节（不含 data: 前缀）。
    image_base64: String,
    /// image/png | image/jpeg | image/webp
    mime: String,
}

/// MIME → 对象键扩展名（同时充当受支持 MIME 白名单）。
/// 位图资产共用：角色立绘（本模块 upload_avatar）与世界封面（worlds::upload_cover）同一张白名单，
/// 免得两处各写一份、日后一处加 MIME 另一处忘了加。
pub(crate) fn image_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// 扩展名 → 回读 Content-Type。
fn content_type_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// 可回读对象前缀白名单：`avatars/`（角色立绘）与 `covers/`（世界封面）。
///
/// 🔴 新增前缀的硬前提：键里必须含 128 位随机 id（角色 id / 世界 id 都是 `uuid::Uuid::new_v4`）。
/// `GET /assets/objects/{*key}` **无鉴权**，靠不可猜的键充当能力 URL；可枚举的键（自增号、
/// 用户可控字符串）一旦进白名单就等于把该目录公开可遍历。
const READABLE_OBJECT_PREFIXES: [&str; 2] = ["avatars/", "covers/"];

/// 对象键安全校验（严防路径穿越）：必须命中前缀白名单、无 `..`、非绝对/前导斜杠、无反斜杠/空字节。
fn is_safe_object_key(key: &str) -> bool {
    READABLE_OBJECT_PREFIXES.iter().any(|p| key.starts_with(p))
        && !key.contains("..")
        && !key.starts_with('/')
        && !key.contains('\\')
        && !key.contains('\0')
        && std::path::Path::new(key).is_relative()
}

// ---------------- 辅助 ----------------

/// 本模块（及 `assets::worlds`）的应答统一走 `json_response(serde_json::to_string(&resp).unwrap())`。
///
/// 那个 `unwrap` 是**静态安全**的，不是漏网之鱼：`resp` 一律是 `serde_json::Value`
///（`json!` 宏产物，或从库里读出后 `from_str::<Value>` 成功的那份）。`Value` 的 `Serialize`
/// 实现没有失败分支——非字符串 map 键与 NaN/Inf 这两个 serde_json 仅有的报错来源都构造不出来
///（`json!` 对 NaN 走 `Number::from_f64` → `None` → `Value::Null`），写入目标又是内存 `String`
/// 而非 IO。故**任何脏数据都无法让它 panic**：DB 里存着非法 JSON 时，失败发生在更早的
/// `from_str` 那一步，且那里已用 `.ok()` 降级为 `Value::Null`（见 `manifest` 端点，
/// 由 `tests::manifest_with_corrupt_json_in_db_degrades_to_null` 锁住）。
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

/// 提取「同源指纹 + 原味卡」两列（R1 同源卡同世界唯一，规格 §7）。
///
/// - 指纹 = `identity.sourceWork.sourceId`，即提取源文件的全字节哈希（muse-engine
///   `character::synthesis` 合成时写入）。纯原创卡没有该字段 → `None`，天然不参与同源判定。
/// - 原味卡 = 合成产物出厂态未被改动：`lifecycle == "draft"` 且 `revision == 0`
///   （`synthesis::assemble_card` 的产物）。任一被改动即视为「玩家自己的版本」，join 一律放行。
///
/// 服务端权威（§9.6 铁律）：只从 card_json 推导，绝不接受客户端另传的指纹/原味声明。
fn source_identity(card: &serde_json::Value) -> (Option<String>, bool) {
    let fingerprint = card
        .pointer("/identity/sourceWork/sourceId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let pristine = card.get("lifecycle").and_then(|v| v.as_str()) == Some("draft")
        && card.get("revision").and_then(|v| v.as_i64()) == Some(0);
    (fingerprint, pristine)
}

/// 机审裁决 → 落库字符串（服务端权威，不信客户端声明）。
/// 文本裁决由 safety::moderate_and_queue 统一给出：注入命中即便 provider 直过也已折叠为 Pending
/// （保守阈值，§14 最高优先级威胁），此处只做字符串映射，不重复判定/落库。
/// 位图裁决（check_image）同样经此映射落 avatar_moderation / worlds.cover_moderation。
pub(crate) fn verdict_str(verdict: ModerationVerdict) -> &'static str {
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

    // 卡位检查（波次 2）：owner 现有未撤回云端角色数已占满 users.card_slots → 409。
    // 撤回/删除会释放卡位；解锁更多卡位走历练（POST /me/card-slots/unlock）。
    let active_cards = crate::progression::count_active_cards(&state.db, &user.user_id).await?;
    let slots = crate::progression::card_slots_of(&state.db, &user.user_id).await?;
    if active_cards >= slots {
        return Err(ApiError::Conflict(format!(
            "卡位已满（{active_cards}/{slots}）。通过历练可解锁更多卡位"
        )));
    }

    // 服务端权威版本号：按 owner + localCardId 递增，忽略客户端任何 version 声明。
    let max_version: Option<i64> =
        sqlx::query_scalar("SELECT MAX(version) FROM cloud_characters WHERE owner_id = $1 AND local_card_id = $2")
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

    // 同源指纹 + 原味卡标记（R1，规格 §7）：发布时一次性物化成列，join 热路径直接读，
    // 免去每次投放都反序列化整张卡。判据只来自服务端解析的 card_json。
    let (source_fingerprint, pristine) = source_identity(&req.card_json);

    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, rights_declaration, moderation, withdrawn, manifest_json, source_fingerprint, pristine, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $9, $10, $11)",
    )
    .bind(&id)
    .bind(&user.user_id)
    .bind(&local_card_id)
    .bind(version)
    .bind(&card_text)
    .bind(&req.rights_declaration)
    .bind(moderation)
    .bind(&manifest_text)
    .bind(source_fingerprint.as_deref())
    .bind(i64::from(pristine))
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
        // 发布即快照时尚无头像；头像走独立 POST /assets/characters/{id}/avatar 端点。
        avatar_url: None,
        // 新卡历练从 0 起（唯一写入路径为各结算点的 grant_mileage_tx）。
        mileage: 0,
    };
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// GET /assets/characters/mine：我的云端版本列表（owner 隔离，含审核态与历练值）。
async fn list_mine(State(state): State<AppState>, user: AuthUser) -> Result<Response, ApiError> {
    let rows: Vec<(String, String, i64, String, String, i64, i64, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, local_card_id, version, rights_declaration, moderation, withdrawn, created_at, avatar_url, avatar_moderation, mileage FROM cloud_characters WHERE owner_id = $1 ORDER BY created_at DESC, version DESC, id DESC",
    )
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<CharacterView> = rows
        .into_iter()
        .map(
            |(id, local_card_id, version, rights, moderation, withdrawn, created_at, avatar_url, avatar_moderation, mileage)| {
                CharacterView {
                    id,
                    local_card_id,
                    version,
                    rights_declaration: rights,
                    moderation,
                    withdrawn: withdrawn != 0,
                    created_at,
                    // 红线：仅头像 approved 才回传 URL，否则 null（未过审绝不外泄）。
                    avatar_url: if avatar_moderation.as_deref() == Some("approved") { avatar_url } else { None },
                    mileage,
                }
            },
        )
        .collect();
    let body = serde_json::to_string(&items).map_err(ApiError::internal)?;
    Ok(json_response(body))
}

/// 主体申诉行的用户侧视图（status 端点内联回显）：无申诉 → null。
async fn fetch_appeal_view(
    state: &AppState,
    subject_kind: &str,
    subject_id: &str,
) -> Result<serde_json::Value, ApiError> {
    let row: Option<(String, String, Option<String>, i64, Option<i64>)> = sqlx::query_as(
        "SELECT status, appeal_text, resolution_reason, created_at, resolved_at \
         FROM moderation_appeals WHERE subject_kind = $1 AND subject_id = $2",
    )
    .bind(subject_kind)
    .bind(subject_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(match row {
        Some((status, text, resolution, created_at, resolved_at)) => serde_json::json!({
            "status": status,
            "appealText": text,
            "resolutionReason": resolution,
            "createdAt": created_at,
            "resolvedAt": resolved_at,
        }),
        None => serde_json::Value::Null,
    })
}

/// 作者侧下架告知（无处置记录 / 已恢复 → null）。
///
/// 只给「状态 + 时间 + 是否可恢复 + 固定说明」四样，**不给运营内部处置理由**（见调用点注释）。
/// `restored` 行不下发：已经恢复的内容对作者而言就是正常内容，翻旧账没有意义。
async fn fetch_takedown_notice(
    state: &AppState,
    subject_kind: &str,
    subject_id: &str,
) -> Result<serde_json::Value, ApiError> {
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT state, created_at FROM content_takedowns \
         WHERE subject_kind = $1 AND subject_id = $2 AND state IN ('restricted', 'removed')",
    )
    .bind(subject_kind)
    .bind(subject_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(match row {
        Some((state, created_at)) => {
            let reversible = state == "restricted";
            serde_json::json!({
                "state": state,
                "reversible": reversible,
                "takenDownAt": created_at,
                "notice": if reversible {
                    "该内容已被平台下架，暂不对外展示，也不能再投放到新的世界。如有异议请联系客服申诉。"
                } else {
                    "该内容已被平台永久移除，不可恢复。如需重新上线请重新发布。"
                },
            })
        }
        None => serde_json::Value::Null,
    })
}

/// GET /assets/characters/{id}/status：审核态查询（owner 隔离，非本人 → 404 不泄露存在性）。
/// 内联可审计 manifest（§2.3），发布方可直接预览云端副本的字段/用途/可见范围/删除策略。
/// 另回显 rejectReason（卡被驳回时：最新 audit_queue.reject_reason 人审理由；机审直拒不入队，
/// 无队列行/无理由则中文兜底）与 appeal（该主体申诉行，无则 null）。
async fn status(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> Result<Response, ApiError> {
    let row: Option<(String, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT moderation, version, withdrawn, manifest_json FROM cloud_characters WHERE id = $1 AND owner_id = $2",
    )
    .bind(&id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let (moderation, version, withdrawn, manifest_json) = row.ok_or(ApiError::NotFound)?;
    let manifest = manifest_json
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    // rejectReason：仅 moderation=='rejected' 时给值，否则 null。
    let reject_reason = if moderation == "rejected" {
        let reason: Option<Option<String>> = sqlx::query_scalar(
            "SELECT reject_reason FROM audit_queue WHERE subject_kind = 'character' AND subject_id = $1 \
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
        let text = reason
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "未通过机器审核".to_string());
        serde_json::Value::String(text)
    } else {
        serde_json::Value::Null
    };

    let appeal = fetch_appeal_view(&state, "character", &id).await?;

    // 下架告知（migration 0044）：内容被运营事后下架时，作者有权知道「被处置了、什么时候」。
    // 🔴 **不回显 `content_takedowns.reason`**——那是运营内部处置备注，口径同 `audit_logs.reason`
    // （对比：人审驳回理由走的是另一列 `audit_queue.reject_reason`，那一列本就是为回显而设的）。
    // 面向作者的说明用固定文案，避免内部备注经由本端点外泄。
    let takedown = fetch_takedown_notice(&state, "character", &id).await?;

    // 处置申诉（migration 0045）：作者对**处置本身**提的异议及其裁决结论。
    //
    // 🔴 与 `takedown` 字段并列而**不是**嵌在它里面，是因为两者的生命周期不同：处置被恢复后
    // `takedown` 就不再下发（已恢复的内容对作者而言就是正常内容），而申诉记录必须留在原地——
    // 否则「我申诉成功了」这条结论会随着内容恢复一起从作者眼前消失。
    //
    // 🔴 回显的 `resolutionReason` 是复审人**写给作者**的答复，与 `content_takedowns.reason`
    // （运营内部处置备注）不是一回事。后者本端点一个字都不取。
    let disposal_appeal =
        crate::admin_api::disposal_appeal_view(&state.db, &id).await?.unwrap_or(serde_json::Value::Null);

    let resp = serde_json::json!({
        "id": id,
        "moderation": moderation,
        "version": version,
        "withdrawn": withdrawn != 0,
        "manifest": manifest,
        "rejectReason": reject_reason,
        "appeal": appeal,
        "takedown": takedown,
        "disposalAppeal": disposal_appeal,
    });
    Ok(json_response(serde_json::to_string(&resp).unwrap()))
}

/// 处置申诉请求。`subjectKind` 缺省为 `character`（卡文被下架是主要情形）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisposalAppealReq {
    text: String,
    #[serde(default)]
    subject_kind: Option<String>,
}

/// POST /assets/characters/{id}/disposal-appeal body {text, subjectKind?}：对**过审后被处置**发起申诉。
///
/// ## 与 `/appeal`（0018）的分工
///
/// `/appeal` 受理 `rejected`——「我发布的东西没通过审核」；本端点受理 `restricted` / `removed`
/// ——「我已经过审、已经在线上的东西被拿下来了」。两者是**两次独立的处置事件**，
/// 各有各的表、各有各的改判路径（本端点的改判走 `restore` 台阶，不会写常量 `approved`）。
/// 详见 migration 0045 头注释。
///
/// ## 红线
///
/// - **提交不改任何 moderation**：下架继续生效到裁决为止（口径同 `/appeal`）。
///   唯一的恢复路径仍是 `POST /admin/content/{kind}/{id}/restore` 或申诉改判，两者共用同一段实现。
/// - **非 owner → 404**，不泄露存在性（口径同 `status` / `appeal`）。
/// - **每次处置一次**：唯一索引 `(takedown_id, disposal_at)` 冲突 → 409。恢复后又被重新下架
///   是一次**新的**处置（`disposal_at` 变了），作者重新获得申诉权——申诉权挂在处置事件上，
///   不挂在主体上，否则第二次被下架的人永远申诉不了。
async fn disposal_appeal(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<DisposalAppealReq>,
) -> Result<Response, ApiError> {
    let subject_kind = req.subject_kind.unwrap_or_else(|| "character".into());
    if !crate::admin_api::APPEALABLE_SUBJECT_KINDS.contains(&subject_kind.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "subjectKind 仅支持 {}",
            crate::admin_api::APPEALABLE_SUBJECT_KINDS.join(" / ")
        )));
    }

    // owner 鉴权：非本人或不存在 → 404（硬隔离，口径同 status/appeal）。
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_id FROM cloud_characters WHERE id = $1 AND owner_id = $2")
            .bind(&id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    let owner = owner.ok_or(ApiError::NotFound)?;

    let text = req.text.trim().to_string();
    let chars = text.chars().count();
    if chars == 0 || chars > 500 {
        return Err(ApiError::BadRequest("申诉内容必填且不超过 500 字符".into()));
    }

    // 必须有一次**生效中**的处置才谈得上申诉。`restored` 不受理：内容已经回来了。
    let disposal: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT id, state, created_at FROM content_takedowns \
         WHERE subject_kind = $1 AND subject_id = $2 AND state IN ('restricted', 'removed')",
    )
    .bind(&subject_kind)
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;
    let (takedown_id, disposal_state, disposal_at) = disposal.ok_or_else(|| {
        ApiError::BadRequest("该内容当前没有生效中的处置，无需申诉".into())
    })?;

    let appeal_id = new_id("dap");
    let now = now_ms();
    let inserted = sqlx::query(
        "INSERT INTO disposal_appeals \
         (id, takedown_id, disposal_at, subject_kind, subject_id, owner_id, appeal_text, \
          disposal_state, status, resolution_reason, reviewer_id, created_at, resolved_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending', NULL, NULL, $9, NULL)",
    )
    .bind(&appeal_id)
    .bind(&takedown_id)
    .bind(disposal_at)
    .bind(&subject_kind)
    .bind(&id)
    .bind(&owner)
    .bind(&text)
    .bind(&disposal_state)
    .bind(now)
    .execute(&state.db)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::Conflict(
                "本次处置已申诉过，每次处置仅可申诉一次（如内容被恢复后再次处置，可重新申诉）"
                    .into(),
            ));
        }
        Err(e) => return Err(e.into()),
    }

    let resp = serde_json::json!({
        "id": appeal_id,
        "subjectKind": subject_kind,
        "subjectId": id,
        "disposalState": disposal_state,
        "status": "pending",
        "appealText": text,
        "resolutionReason": serde_json::Value::Null,
        "createdAt": now,
        "resolvedAt": serde_json::Value::Null,
        // 提交不改展示态：处置继续生效到裁决为止。写进回执，避免被读成「申诉即恢复」。
        "moderation": moderation_of_subject(&state, &subject_kind, &id).await?,
    });
    Ok(json_response(serde_json::to_string(&resp).unwrap()))
}

/// 回执里如实报当前展示态（证明「提交不改 moderation」）。
async fn moderation_of_subject(
    state: &AppState,
    subject_kind: &str,
    id: &str,
) -> Result<Option<String>, ApiError> {
    let col = if subject_kind == "character_avatar" { "avatar_moderation" } else { "moderation" };
    // 列名来自上面这个二选一，主体 id 走 $1 绑定。
    let v: Option<Option<String>> =
        sqlx::query_scalar(&format!("SELECT {col} FROM cloud_characters WHERE id = $1"))
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    Ok(v.flatten())
}

/// 申诉提交请求。
#[derive(Debug, Deserialize)]
struct AppealReq {
    text: String,
}

/// POST /assets/characters/{id}/appeal body {text}：对机审/人审驳回发起申诉（每主体终身一次）。
///
/// 红线：提交**不改任何 moderation**——改判前「未过审不外泄」继续成立（roster/CharacterView 的
/// approved 门不受影响）；唯一改判路径是后台 POST /admin/appeals/{id}/resolve（必留 audit_logs）。
/// 仅当卡 moderation=='rejected' 或头像 avatar_moderation=='rejected' 才允许申诉；
/// 非 owner → 404 不泄露存在性（与 status 一致）。
async fn appeal(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<AppealReq>,
) -> Result<Response, ApiError> {
    // owner 鉴权：非本人或不存在 → 404（硬隔离）。
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT moderation, avatar_moderation FROM cloud_characters WHERE id = $1 AND owner_id = $2",
    )
    .bind(&id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let (moderation, avatar_moderation) = row.ok_or(ApiError::NotFound)?;

    // 仅驳回态（卡或头像任一维度 rejected）可申诉。
    if moderation != "rejected" && avatar_moderation.as_deref() != Some("rejected") {
        return Err(ApiError::BadRequest("仅审核未通过的内容可发起申诉".into()));
    }

    let text = req.text.trim().to_string();
    let chars = text.chars().count();
    if chars == 0 || chars > 500 {
        return Err(ApiError::BadRequest("申诉内容必填且不超过 500 字符".into()));
    }

    let appeal_id = new_id("apl");
    let now = now_ms();
    // 每主体终身一次：唯一索引 (subject_kind, subject_id) 冲突 → 409。
    let inserted = sqlx::query(
        "INSERT INTO moderation_appeals (id, subject_kind, subject_id, owner_id, appeal_text, status, created_at) \
         VALUES ($1, 'character', $2, $3, $4, 'pending', $5)",
    )
    .bind(&appeal_id)
    .bind(&id)
    .bind(&user.user_id)
    .bind(&text)
    .bind(now)
    .execute(&state.db)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::Conflict("该内容已申诉过，每个内容仅可申诉一次".into()));
        }
        Err(e) => return Err(e.into()),
    }

    let resp = serde_json::json!({
        "id": appeal_id,
        "subjectKind": "character",
        "subjectId": id,
        "status": "pending",
        "appealText": text,
        "resolutionReason": serde_json::Value::Null,
        "createdAt": now,
        "resolvedAt": serde_json::Value::Null,
    });
    Ok(json_response(serde_json::to_string(&resp).unwrap()))
}

/// GET /assets/characters/{id}/manifest：可审计 manifest（§2.3，owner 隔离）。
/// 独立端点便于发布前预览与合规审计取用；非本人 → 404 不泄露存在性。
async fn manifest(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> Result<Response, ApiError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT manifest_json FROM cloud_characters WHERE id = $1 AND owner_id = $2")
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

/// POST /assets/characters/{id}/avatar：上传角色头像（owner 鉴权 + 机审 + 行级字段落库）。
///
/// 头像不进不可变 card_json（不改 CharacterCardV2），作为 cloud_characters 行级可变字段。
/// body {imageBase64, mime}：校验 MIME 白名单 → base64 解码 → 512KB 上限 → 写对象存储 → 机审 →
/// UPDATE avatar_object_key / avatar_url / avatar_moderation。
/// 红线：avatar_url（对象键路径）无论裁决都落库，但响应仅 approved 才回传 URL（未过审绝不外泄）；
/// 私密房不豁免——头像上传与房间无关，恒过机审。非 owner → 404（不泄露存在性，与 status 一致）。
async fn upload_avatar(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<AvatarReq>,
) -> Result<Response, ApiError> {
    // owner 鉴权：非本人或不存在 → 404（硬隔离，不泄露存在性）。
    let owned: Option<(String,)> =
        sqlx::query_as("SELECT id FROM cloud_characters WHERE id = $1 AND owner_id = $2")
            .bind(&id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }

    // MIME 白名单（png/jpeg/webp）。
    let ext = image_ext(req.mime.trim())
        .ok_or_else(|| ApiError::BadRequest("头像格式不支持（仅 image/png、image/jpeg、image/webp）".into()))?;

    // base64 解码 + 大小校验。
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(req.image_base64.trim())
        .map_err(|_| ApiError::BadRequest("头像 base64 解码失败".into()))?;
    if bytes.is_empty() {
        return Err(ApiError::BadRequest("头像数据为空".into()));
    }
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(ApiError::BadRequest("头像超过 512KB 上限".into()));
    }

    // 写对象存储：键以角色 id 命名（不可变快照 id 唯一，覆盖式更新同角色头像）。
    let object_key = format!("avatars/{id}.{ext}");
    state.objects.put(&object_key, &bytes).map_err(ApiError::internal)?;

    // 机审（图片检测）：dev 直过，prod 待接第三方图审。
    let verdict = state
        .moderation
        .check_image(&bytes)
        .await
        .map_err(|e| ApiError::internal(std::io::Error::other(e)))?;
    let moderation = verdict_str(verdict);
    let avatar_url = format!("/api/assets/objects/{object_key}");

    sqlx::query(
        "UPDATE cloud_characters SET avatar_object_key = $1, avatar_url = $2, avatar_moderation = $3 WHERE id = $4 AND owner_id = $5",
    )
    .bind(&object_key)
    .bind(&avatar_url)
    .bind(moderation)
    .bind(&id)
    .bind(&user.user_id)
    .execute(&state.db)
    .await?;

    // 红线：未过审绝不下发 URL。
    let out_url = if verdict == ModerationVerdict::Approved { Some(avatar_url) } else { None };
    let resp = serde_json::json!({ "avatarUrl": out_url, "moderation": moderation });
    Ok(json_response(serde_json::to_string(&resp).unwrap()))
}

/// GET /assets/objects/{*key}：对象回读（头像 / 世界封面等）。无鉴权（能力 URL：对象键含 128 位随机 id）。
/// 严防路径穿越：`is_safe_object_key` 拒绝白名单外前缀、`..`、绝对/前导斜杠、反斜杠；缺失 → 404。
///
/// 注意：本端点只保证「键猜不到」，**不做审核态判定**——未过审资产的防线在**下发面**
/// （CharacterView / roster / worlds 列表与详情只在 approved 时才把 URL 写进响应），
/// 键从未外泄即等价于不可达。新增可回读前缀时必须同步保证其下发面有同样的 approved 门。
async fn get_object(State(state): State<AppState>, Path(key): Path<String>) -> Result<Response, ApiError> {
    if !is_safe_object_key(&key) {
        return Err(ApiError::NotFound);
    }
    let ext = key.rsplit('.').next().unwrap_or("");
    let content_type = content_type_for_ext(ext).ok_or(ApiError::NotFound)?;
    let bytes = state.objects.get(&key).map_err(|_| ApiError::NotFound)?;
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response())
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
        sqlx::query_as("SELECT id FROM cloud_characters WHERE id = $1 AND owner_id = $2")
            .bind(&id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    sqlx::query("UPDATE cloud_characters SET withdrawn = 1 WHERE id = $1 AND owner_id = $2")
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
        sqlx::query_as("SELECT id FROM cloud_characters WHERE id = $1 AND owner_id = $2")
            .bind(&id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }

    // 是否已投放：world_members 是否引用该云端角色。
    let placed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_members WHERE cloud_character_id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    let now = now_ms();
    let req_id = new_id("dr");
    let resp = if placed == 0 {
        sqlx::query("DELETE FROM cloud_characters WHERE id = $1 AND owner_id = $2")
            .bind(&id)
            .bind(&user.user_id)
            .execute(&state.db)
            .await?;
        sqlx::query(
            "INSERT INTO data_requests (id, user_id, kind, status, created_at, updated_at) VALUES ($1, $2, 'delete', 'done', $3, $4)",
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
        sqlx::query("UPDATE cloud_characters SET withdrawn = 1 WHERE id = $1 AND owner_id = $2")
            .bind(&id)
            .bind(&user.user_id)
            .execute(&state.db)
            .await?;
        sqlx::query(
            "INSERT INTO data_requests (id, user_id, kind, status, created_at, updated_at) VALUES ($1, $2, 'delete', 'pending', $3, $4)",
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
