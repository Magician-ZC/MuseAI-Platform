//! 跨世界背包（S4）：平台规格 §2.5 章节房 + §9.6 服务端权威。
//!
//! 铁律（§9.6）：物品取得只有两条服务端写入路径——tick 事务内的通关结算（runtime/chapters 调 grant_item），
//! 或支付履约（billing，feature 后）。不存在「客户端声明拥有」的写入接口。
//!
//! 端点：
//! GET  /me/backpack                    我的背包（含物品定义与来源）
//! POST /worlds/{id}/carry              入场携带声明：服务端权威校验物品归属 + admission::check_admission，
//!                                      被拒物品不入场；试图携带不属于自己的物品 → risk_event("forged_state")

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Any, AnyPool, Row, Transaction};

use crate::admission::{self, AdmissionDecision, ItemDefinition, ItemOrigin, WorldAdmissionPolicy};
use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms, Placeholders};
use crate::error::ApiError;
use crate::idempotency;
use crate::worlds::load_world;

// ---------- items 表 ↔ ItemDefinition 映射 ----------

fn map_item(row: &sqlx::any::AnyRow) -> Result<ItemDefinition, ApiError> {
    let effect_tags: String = row.try_get("effect_tags")?;
    let cosmology_json: String = row.try_get("cosmology_json")?;
    let power_tier: i64 = row.try_get("power_tier")?;
    Ok(ItemDefinition {
        id: row.try_get("id")?,
        narrative: row.try_get("narrative")?,
        effect_tags: serde_json::from_str(&effect_tags).unwrap_or_default(),
        origin: ItemOrigin {
            world_template_id: row.try_get("origin_world_template_id")?,
            cosmology: serde_json::from_str(&cosmology_json).unwrap_or_default(),
            power_tier: power_tier.clamp(0, u8::MAX as i64) as u8,
        },
    })
}

/// 读取物品定义（不存在 → None）。
pub async fn load_item(db: &AnyPool, item_id: &str) -> Result<Option<ItemDefinition>, ApiError> {
    let row = sqlx::query(
        "SELECT id, narrative, effect_tags, origin_world_template_id, cosmology_json, power_tier \
         FROM items WHERE id = $1",
    )
    .bind(item_id)
    .fetch_optional(db)
    .await?;
    match row {
        Some(r) => Ok(Some(map_item(&r)?)),
        None => Ok(None),
    }
}

/// 读取目标世界的准入策略（world → template.admission_json）。解析失败或缺失 → 默认全开放。
pub async fn load_admission_policy(db: &AnyPool, world_id: &str) -> Result<WorldAdmissionPolicy, ApiError> {
    let row = sqlx::query(
        "SELECT wt.admission_json AS aj FROM worlds w \
         JOIN world_templates wt ON wt.id = w.template_id WHERE w.id = $1",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let aj: String = row.try_get("aj")?;
    Ok(serde_json::from_str(&aj).unwrap_or_default())
}

// ---------- 唯一合法写入路径之一：通关结算入包 ----------

/// 通关结算入包（tx 版；chapters::finish 在 CAS 事务内调）：
/// 物品定义 upsert 到 items + 新增 backpacks 归属行（owned）。
/// `reward_hook_key` 非空时作为 (user_id, reward_hook_key) DB 幂等键——命中唯一约束即「已发货」，
/// 返回 `Ok(None)`（不二次发货）；成功写入返回 `Ok(Some(bp_id))`。为 None 时不参与去重（多行并存）。
/// 这是 §9.6 两条合法写入路径之一（另一条是支付履约）。
pub(crate) async fn grant_item_tx(
    tx: &mut Transaction<'_, Any>,
    user_id: &str,
    item: &ItemDefinition,
    acquired_world_id: &str,
    reward_hook_key: Option<&str>,
) -> Result<Option<String>, ApiError> {
    // 物品定义按 id 共享；已存在则跳过写入（SELECT-exists 守住常见路径，事务内避免不可移植的 upsert）。
    let exists = sqlx::query("SELECT 1 AS x FROM items WHERE id = $1")
        .bind(&item.id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if !exists {
        sqlx::query(
            "INSERT INTO items (id, narrative, effect_tags, origin_world_template_id, cosmology_json, power_tier, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&item.id)
        .bind(&item.narrative)
        .bind(serde_json::to_string(&item.effect_tags).unwrap_or_else(|_| "[]".into()))
        .bind(&item.origin.world_template_id)
        .bind(serde_json::to_string(&item.origin.cosmology).unwrap_or_else(|_| "[]".into()))
        .bind(item.origin.power_tier as i64)
        .bind(now_ms())
        .execute(&mut **tx)
        .await?;
    }

    let bp_id = new_id("bp");
    let res = sqlx::query(
        "INSERT INTO backpacks (id, user_id, item_id, acquired_world_id, status, carried_world_id, reward_hook_key, acquired_at) \
         VALUES ($1, $2, $3, $4, 'owned', NULL, $5, $6)",
    )
    .bind(&bp_id)
    .bind(user_id)
    .bind(&item.id)
    .bind(acquired_world_id)
    .bind(reward_hook_key)
    .bind(now_ms())
    .execute(&mut **tx)
    .await;

    match res {
        Ok(_) => Ok(Some(bp_id)),
        // (user_id, reward_hook_key) 唯一键命中：该钩子已发货 → 幂等，不二次发货。
        Err(sqlx::Error::Database(e)) if reward_hook_key.is_some() && e.is_unique_violation() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// 通关结算入包（pool 版，供测试/支付履约等非事务调用者）：包一个事务委托 grant_item_tx。
/// reward_hook_key = None → 不去重，必得 backpack 行 id。
/// （§9.6 合法写入路径的对外 API；本 crate 内目前由测试与后续 billing 履约消费。）
#[allow(dead_code)]
pub async fn grant_item(
    db: &AnyPool,
    user_id: &str,
    item: &ItemDefinition,
    acquired_world_id: &str,
) -> Result<String, ApiError> {
    let mut tx = db.begin().await?;
    let bp = grant_item_tx(&mut tx, user_id, item, acquired_world_id, None).await?;
    tx.commit().await?;
    Ok(bp.unwrap_or_default())
}

// ---------- GET /me/backpack ----------

async fn my_backpack(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT b.id AS bp_id, b.status AS bp_status, b.acquired_world_id, b.carried_world_id, b.acquired_at, \
         i.id AS id, i.narrative, i.effect_tags, i.origin_world_template_id, i.cosmology_json, i.power_tier \
         FROM backpacks b JOIN items i ON i.id = b.item_id \
         WHERE b.user_id = $1 AND b.status != 'consumed' ORDER BY b.acquired_at DESC",
    )
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;

    let mut items = Vec::new();
    for r in &rows {
        let item = map_item(r)?;
        let carried_world_id: Option<String> = r.try_get("carried_world_id")?;
        items.push(json!({
            "backpackId": r.try_get::<String, _>("bp_id")?,
            "status": r.try_get::<String, _>("bp_status")?,
            "acquiredWorldId": r.try_get::<String, _>("acquired_world_id")?,
            "carriedWorldId": carried_world_id,
            "item": {
                "id": item.id,
                "narrative": item.narrative,
                "effectTags": item.effect_tags,
                "origin": {
                    "worldTemplateId": item.origin.world_template_id,
                    "cosmology": item.origin.cosmology,
                    "powerTier": item.origin.power_tier,
                },
            },
        }));
    }
    Ok(Json(json!({ "items": items })))
}

// ---------- GET /me/memberships ----------

/// 「最近互动时间」口径（`lastActiveAt`）——**本人对该角色在该世界发起、且被受理的最近一次干预时刻**：
///
/// - 数据源：`interventions`（kind = whisper 托梦 / item 投物），取 `MAX(created_at)`；
/// - 主体限定：`user_id = 本人`——这是「**你**与这张卡的互动」，不是「角色在世界里有动静」。
///   世界自身推进（tick 产出的 world_events）不算互动：那是世界在动，不是你在陪它；
/// - 状态限定：`status IN ('accepted','applied')`，与托梦配额计数口径逐字一致
///   （见 `interventions::create` 上方注释）——`applied` 是「已被引擎消费」而非「额度归还」，
///   仍是一次落地的互动；`rejected`（超配额 / 机审拒）没有落地，不算互动；
/// - 无记录 → **不下发该字段**（不下发 0，也不回退 joinedAt——joinedAt 已是独立字段，前端自行兜底）。
///
/// 为什么不用 `world_events.actors_json`：那是「角色戏份」而非「你的互动」，且 actors_json 是 JSON
/// 文本数组，按角色筛选只能 `LIKE '%cid%'`——不可索引、且 id 互为前缀时会误命中，列表接口承受不起。
///
/// 性能（列表接口铁律）：一次 `GROUP BY` + `IN(本人在场世界 id)` 取回全部成员行的时间戳，
/// **绝不逐成员发 SQL**——大厅每次进入都拉这个接口，N+1 会随持卡数放大 QPS。
/// `world_id` 前导命中 `idx_interventions_char(world_id, character_id, status)`（迁移 0022），无需新索引。
async fn last_interaction_by_pair(
    db: &AnyPool,
    user_id: &str,
    world_ids: &[String],
) -> Result<std::collections::HashMap<(String, String), i64>, ApiError> {
    let mut out = std::collections::HashMap::new();
    if world_ids.is_empty() {
        return Ok(out);
    }
    // 发号顺序 = 下面的 bind 顺序：先 user_id，再整串 world_ids。
    let mut ph = Placeholders::new();
    let user = ph.take();
    let placeholders = ph.list(world_ids.len());
    // CAST(MAX(...) AS BIGINT)：双库可移植子集，PG/SQLite 通用，解码类型确定（同 worlds_ops 聚合先例）。
    let sql = format!(
        "SELECT world_id, character_id, CAST(MAX(created_at) AS BIGINT) AS last_at \
         FROM interventions \
         WHERE user_id = {user} AND status IN ('accepted', 'applied') AND world_id IN ({placeholders}) \
         GROUP BY world_id, character_id"
    );
    let mut query = sqlx::query(&sql).bind(user_id);
    for wid in world_ids {
        query = query.bind(wid.as_str());
    }
    for r in query.fetch_all(db).await? {
        let wid: String = r.try_get("world_id")?;
        let cid: String = r.try_get("character_id")?;
        out.insert((wid, cid), r.try_get::<i64, _>("last_at")?);
    }
    Ok(out)
}

/// 权威「我的角色 × 世界」清单：直接读 world_members（WHERE user_id=本人 AND status='active'），
/// 补齐日报反推的盲区（刚投放尚无日报的角色/世界也在场）。无 owner 泄漏——只出本人成员行。
/// 角色名解析复用 worlds::world_detail 的 `card_json → identity.name`（缺失兜底为 cloud_character_id）。
///
/// 展示字段（客户端大厅辅助栏「你的羁绊角色」）：
/// - `avatarUrl`：仅 `avatar_moderation == 'approved'` 才下发（红线，见下方注释）；
/// - `lastActiveAt`：口径见 `last_interaction_by_pair`；无记录不下发该字段。
///
/// TODO（羁绊强度 `bondStrength`，需产品先拍板口径）：本端点**有意不下发**羁绊强度。
/// 总规格 §14 只定了「仅正向羁绊线达阈值后双向自愿解锁真人身份」，**没有定义强度公式**；
/// 引擎侧的原始关系维度（trust/affinity/fear/debt，`relation_dynamics`）已有权威下发面
/// `GET /worlds/{id}/state-summary`（events::world_state_summary），且在那里按 §9.4 信息边界过滤
/// （只放行 `from == viewer 本世界角色` 或 `knownTo 含之` 的有向边）。此处不重复第二套过滤，
/// 也不自造加权合成指标——合成值一旦下发就会被前端拿去排序/展示，成为无人认可的既成事实。
/// 另有硬约束：关系存在 `worlds.narrative_state_json` 整块 blob 内，列表接口逐世界拉回完整叙事状态
/// 反序列化，载荷与 CPU 是 O(世界数 × 状态大小) 且随 tick 增长——不满足列表接口的有界取数要求。
/// 待产品定义强度口径后，宜在**单世界**面（state-summary）上做，而非跨世界列表。
/// 🔴 次级键 `wm.id` 不可省（同 `worlds::world_detail` 阵容、`assembly::load_active_cards`）：
/// 一个用户可以在同一毫秒进多个世界（新手礼包建房 + 自动入场就是批量写），`joined_at` 单键
/// 在 PG 上不定序；SQLite 恰好按 rowid 稳定，故这类顺序 bug 在只跑 SQLite 的测试里永远看不见。
async fn my_memberships(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, wm.status AS mstatus, wm.joined_at AS joined_at, \
         w.id AS world_id, w.title AS title, w.room_type AS room_type, w.status AS wstatus, \
         w.state_revision AS state_revision, cc.card_json AS card, \
         cc.avatar_url AS avatar_url, cc.avatar_moderation AS avatar_moderation \
         FROM world_members wm \
         JOIN worlds w ON w.id = wm.world_id \
         JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.user_id = $1 AND wm.status = 'active' \
         ORDER BY wm.joined_at DESC, wm.id DESC",
    )
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;

    // 第 2 条（也是最后一条）查询：整页世界 id 去重后一次性取回最近互动时间。总查询数恒为 2（空清单为 1）。
    let mut world_ids: Vec<String> = Vec::new();
    for r in &rows {
        let wid: String = r.try_get("world_id")?;
        if !world_ids.contains(&wid) {
            world_ids.push(wid);
        }
    }
    let last_map = last_interaction_by_pair(&state.db, &user.user_id, &world_ids).await?;

    let mut memberships = Vec::new();
    for r in &rows {
        let cid: String = r.try_get("cid")?;
        let card: String = r.try_get("card")?;
        // identity.name 缺失时兜底为角色 id（非空，供列表展示），同 world_detail 的解析法。
        let name = serde_json::from_str::<Value>(&card)
            .ok()
            .and_then(|v| v["identity"]["name"].as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cid.clone());
        let world_id: String = r.try_get("world_id")?;
        let mut item = json!({
            "worldId": world_id.clone(),
            "worldTitle": r.try_get::<String, _>("title")?,
            "roomType": r.try_get::<String, _>("room_type")?,
            "worldStatus": r.try_get::<String, _>("wstatus")?,
            "stateRevision": r.try_get::<i64, _>("state_revision")?,
            "cloudCharacterId": cid.clone(),
            "characterName": name,
            "membershipStatus": r.try_get::<String, _>("mstatus")?,
            "joinedAt": r.try_get::<i64, _>("joined_at")?,
        });
        // 🔴 红线（头像读取面双过滤，providers::ModerationProvider::check_image）：
        // 仅头像机审 approved 才带 avatarUrl；未过审 / 无头像一律**不带该字段**（不下发空串，
        // 前端据此走无头像布局）。口径与 worlds::world_detail 的公开阵容逐字一致。
        let avatar_url: Option<String> = r.try_get("avatar_url")?;
        let avatar_moderation: Option<String> = r.try_get("avatar_moderation")?;
        if avatar_moderation.as_deref() == Some("approved") {
            if let Some(url) = avatar_url.filter(|u| !u.is_empty()) {
                item["avatarUrl"] = json!(url);
            }
        }
        // 最近互动时间：无落地干预记录时不下发该字段（行为与新增前完全一致）。
        if let Some(at) = last_map.get(&(world_id, cid)) {
            item["lastActiveAt"] = json!(at);
        }
        memberships.push(item);
    }
    Ok(Json(json!({ "memberships": memberships })))
}

// ---------- POST /worlds/{id}/carry ----------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CarryRequest {
    /// 声明入场携带的物品 id 列表（items.id）。服务端按归属 + 准入独立判定。
    #[serde(default)]
    item_ids: Vec<String>,
}

fn decision_str(d: AdmissionDecision) -> &'static str {
    match d {
        AdmissionDecision::Admitted => "admitted",
        AdmissionDecision::Rejected => "rejected",
        AdmissionDecision::Sealed => "sealed",
        AdmissionDecision::Translated => "translated",
    }
}

/// 入场携带声明。§9.6 服务端权威：
/// 1) 归属校验——每件物品必须在本人背包（非 consumed）；否则记 risk_event("forged_state") 并整单拒绝；
/// 2) 准入校验——admission::check_admission 按目标世界策略判定；被拒物品不入场（留背包）。
async fn carry(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CarryRequest>,
) -> Result<Json<Value>, ApiError> {
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(
        &serde_json::to_vec(&json!({ "worldId": world_id, "body": &body })).unwrap_or_default(),
    );
    let guard =
        idempotency::guard(&state.db, &user.user_id, "worlds.carry", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    // 世界必须存在（并借此拿到准入策略 + 世界态）。
    let world = load_world(&state.db, &world_id).await?;
    let policy = load_admission_policy(&state.db, &world_id).await?;

    // 第一遍：归属校验（发现伪造立即整单拒绝并记风控；先于成员/世界态校验，保留伪造证据）。
    struct Owned {
        backpack_id: String,
        item: ItemDefinition,
    }
    let mut owned: Vec<Owned> = Vec::new();
    for item_id in &body.item_ids {
        let bp = sqlx::query(
            "SELECT id FROM backpacks WHERE user_id = $1 AND item_id = $2 AND status != 'consumed' LIMIT 1",
        )
        .bind(&user.user_id)
        .bind(item_id)
        .fetch_optional(&state.db)
        .await?;
        let Some(bp_row) = bp else {
            // 携带非本人物品（或根本不存在）：抓包伪造背包清单 → 记风控并整单拒绝。
            crate::safety::record_risk(
                &state.db,
                Some(&user.user_id),
                Some(&world_id),
                "forged_state",
                json!({ "reason": "carry_unowned_item", "itemId": item_id, "worldId": world_id }),
            )
            .await?;
            return Err(ApiError::RiskBlocked);
        };
        let backpack_id: String = bp_row.try_get("id")?;
        let item = load_item(&state.db, item_id)
            .await?
            .ok_or_else(|| ApiError::BadRequest("物品定义缺失".into()))?;
        owned.push(Owned { backpack_id, item });
    }

    // 携带随入场：世界须处可加入态且本人角色在场（否则不得携带）。
    if !matches!(world.status.as_str(), "open" | "running") {
        return Err(ApiError::Conflict("world_not_joinable".into()));
    }
    let is_member = sqlx::query(
        "SELECT 1 AS x FROM world_members WHERE world_id = $1 AND user_id = $2 AND status = 'active' LIMIT 1",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?
    .is_some();
    if !is_member {
        return Err(ApiError::Forbidden);
    }

    // 第二遍：逐件准入判定并落地背包状态（含 S-5 转译降档持久化）。
    let mut results = Vec::new();
    for o in &owned {
        let decision = admission::check_admission(&policy, &o.item)?; // 非法标签 → BadRequest
        // S-5：per-carry 降档覆盖。转译入场把降档后的 powerTier/effectTags 落库（未来强度仲裁读覆盖值），
        // 非转译入场写 NULL 清除历史覆盖，堵住「转译只进响应」的 maxPowerTier 后门。
        let translated = if decision == AdmissionDecision::Translated {
            Some(admission::translate_item(&policy, &o.item))
        } else {
            None
        };
        let (new_status, carried, carried_world): (&str, bool, Option<&str>) = match decision {
            AdmissionDecision::Admitted | AdmissionDecision::Translated => {
                ("carried", true, Some(world_id.as_str()))
            }
            AdmissionDecision::Sealed => ("sealed", true, Some(world_id.as_str())),
            // 拒收：留账号背包，不随角色入场。
            AdmissionDecision::Rejected => ("owned", false, None),
        };
        let pt_override: Option<i64> = translated.as_ref().map(|t| t.origin.power_tier as i64);
        let et_override: Option<String> = translated
            .as_ref()
            .map(|t| serde_json::to_string(&t.effect_tags).unwrap_or_else(|_| "[]".into()));
        sqlx::query(
            "UPDATE backpacks SET status = $1, carried_world_id = $2, power_tier_override = $3, \
             effect_tags_override = $4 WHERE id = $5",
        )
        .bind(new_status)
        .bind(carried_world)
        .bind(pt_override)
        .bind(et_override.as_deref())
        .bind(&o.backpack_id)
        .execute(&state.db)
        .await?;
        let mut result = json!({
            "itemId": o.item.id,
            "decision": decision_str(decision),
            "carried": carried,
        });
        // 转译入场：给出结构化降档后的 effectTags/powerTier（叙事外皮重写由装配器生成）。
        if let Some(t) = &translated {
            result["translatedTo"] = json!({ "powerTier": t.origin.power_tier, "effectTags": t.effect_tags });
        }
        results.push(result);
    }

    let response = json!({ "worldId": world_id, "results": results });
    guard.store_response(&state.db, &response.to_string()).await?;
    Ok(Json(response))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/backpack", get(my_backpack))
        .route("/me/memberships", get(my_memberships))
        .route("/worlds/{id}/carry", post(carry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::testkit::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// 播种一枚云端角色（memberships 端点 JOIN cloud_characters，需其存在以解析名字）。
    async fn seed_cloud_char(db: &AnyPool, id: &str, owner: &str, card_json: &str) {
        sqlx::query(
            "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
             rights_declaration, moderation, withdrawn, created_at) \
             VALUES ($1, $2, 'local', 1, $3, 'original', 'approved', 0, $4)",
        )
        .bind(id)
        .bind(owner)
        .bind(card_json)
        .bind(now_ms())
        .execute(db)
        .await
        .expect("seed cloud_character");
    }

    /// 给已播种的角色补头像（object_key/url/裁决）。裁决取 approved / pending / rejected。
    async fn set_avatar(db: &AnyPool, char_id: &str, url: &str, moderation: &str) {
        sqlx::query(
            "UPDATE cloud_characters SET avatar_object_key = $1, avatar_url = $2, avatar_moderation = $3 \
             WHERE id = $4",
        )
        .bind(format!("obj/{char_id}"))
        .bind(url)
        .bind(moderation)
        .bind(char_id)
        .execute(db)
        .await
        .expect("set avatar");
    }

    /// 直接插一条干预（绕过 API）：供 lastActiveAt 口径用例指定 user/角色/状态/时刻。
    #[allow(clippy::too_many_arguments)]
    async fn seed_intervention(
        db: &AnyPool,
        id: &str,
        world: &str,
        user: &str,
        character: &str,
        kind: &str,
        status: &str,
        created_at: i64,
    ) {
        sqlx::query(
            "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, \
             expected_revision, status, created_at) VALUES ($1, $2, $3, $4, $5, '{}', 0, $6, $7)",
        )
        .bind(id)
        .bind(world)
        .bind(user)
        .bind(character)
        .bind(kind)
        .bind(status)
        .bind(created_at)
        .execute(db)
        .await
        .expect("seed intervention");
    }

    async fn get_memberships(state: &AppState, bearer: Option<&str>) -> (StatusCode, Value) {
        let app = crate::app::build_router(state.clone());
        let mut builder = Request::builder().method("GET").uri("/api/me/memberships");
        if let Some(tk) = bearer {
            builder = builder.header("authorization", format!("Bearer {tk}"));
        }
        let resp = app.oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
        let s = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (s, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
    }

    #[tokio::test]
    async fn memberships_lists_active_and_isolates_owner() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_user(&state.db, "u2").await;
        seed_world(&state.db, "w1", 7, "running").await;
        seed_world(&state.db, "w2", 0, "running").await;
        // u1：c1 active in w1（有名字）、c2 已离场 in w2（不应出现）。
        seed_cloud_char(&state.db, "c1", "u1", &json!({ "identity": { "name": "沈霜" } }).to_string()).await;
        seed_cloud_char(&state.db, "c2", "u1", &json!({ "identity": { "name": "游侠" } }).to_string()).await;
        // u2：c3 active in w1（他人角色，绝不能出现在 u1 的清单——owner 隔离）。
        seed_cloud_char(&state.db, "c3", "u2", &json!({ "identity": { "name": "他人" } }).to_string()).await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_member(&state.db, "m2", "w2", "u1", "c2", "left").await;
        seed_member(&state.db, "m3", "w1", "u2", "c3", "active").await;

        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        let ms = v["memberships"].as_array().unwrap();
        assert_eq!(ms.len(), 1, "仅 active 且属本人：只余 c1（c2 已离场、c3 属他人）");
        assert_eq!(ms[0]["cloudCharacterId"], "c1");
        assert_eq!(ms[0]["characterName"], "沈霜");
        assert_eq!(ms[0]["worldId"], "w1");
        assert_eq!(ms[0]["worldTitle"], "测试世界");
        assert_eq!(ms[0]["roomType"], "idle");
        assert_eq!(ms[0]["worldStatus"], "running");
        assert_eq!(ms[0]["stateRevision"].as_i64().unwrap(), 7, "stateRevision 供直达世界预填干预 CAS");
        assert_eq!(ms[0]["membershipStatus"], "active");
    }

    #[tokio::test]
    async fn memberships_name_falls_back_to_char_id() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        // card_json 无 identity.name → 名字兜底为 cloud_character_id（非空，供列表展示）。
        seed_cloud_char(&state.db, "cNoName", "u1", "{}").await;
        seed_member(&state.db, "m1", "w1", "u1", "cNoName", "active").await;
        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK);
        let ms = v["memberships"].as_array().unwrap();
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0]["characterName"], "cNoName", "identity.name 缺失兜底为角色 id");
    }

    #[tokio::test]
    async fn memberships_empty_when_none() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK);
        assert!(v["memberships"].as_array().unwrap().is_empty(), "无成员关系 → 空清单，不报错");
    }

    #[tokio::test]
    async fn memberships_requires_auth() {
        let state = test_state().await;
        let (s, _) = get_memberships(&state, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "AuthUser 守卫：缺凭证应 401");
    }

    // ---------- 头像下发（红线：未过审绝不外泄） ----------

    #[tokio::test]
    async fn memberships_ships_approved_avatar() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_cloud_char(&state.db, "c1", "u1", &json!({ "identity": { "name": "沈霜" } }).to_string()).await;
        set_avatar(&state.db, "c1", "/api/assets/objects/av1.png", "approved").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;

        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        let ms = v["memberships"].as_array().unwrap();
        assert_eq!(ms[0]["avatarUrl"], "/api/assets/objects/av1.png", "机审 approved 的头像应下发");
    }

    /// 🔴 红线：avatar_moderation 非 approved（pending 待人审 / rejected 已拒）一律不下发头像，
    /// 且**不下发空串**——字段整体缺席，前端据此走无头像布局。
    #[tokio::test]
    async fn memberships_never_ships_unapproved_avatar() {
        for verdict in ["pending", "rejected"] {
            let state = test_state().await;
            seed_user(&state.db, "u1").await;
            seed_world(&state.db, "w1", 0, "running").await;
            seed_cloud_char(&state.db, "c1", "u1", &json!({ "identity": { "name": "沈霜" } }).to_string())
                .await;
            set_avatar(&state.db, "c1", "/api/assets/objects/secret.png", verdict).await;
            seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;

            let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
            assert_eq!(s, StatusCode::OK);
            let m = &v["memberships"][0];
            assert!(
                m.get("avatarUrl").is_none(),
                "avatar_moderation={verdict} 时绝不下发 avatarUrl（含空串），实际 body={v}"
            );
            // 反证：URL 字符串不得以任何形式出现在响应里。
            assert!(!v.to_string().contains("secret.png"), "未过审头像路径不得外泄：{v}");
        }
    }

    #[tokio::test]
    async fn memberships_omits_avatar_field_when_none() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        // 从未上传头像：三列均为 NULL（迁移 0016 历史行形态）。
        seed_cloud_char(&state.db, "c1", "u1", &json!({ "identity": { "name": "无像" } }).to_string()).await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;

        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK);
        let m = &v["memberships"][0];
        assert!(m.get("avatarUrl").is_none(), "无头像 → 不含该字段（不是空串、不是 null）：{v}");
        assert_eq!(m["characterName"], "无像", "其余字段行为与新增前完全一致");
    }

    // ---------- lastActiveAt 口径 ----------

    /// 口径：本人对「该世界 × 该角色」发起、且 status IN ('accepted','applied') 的干预的 MAX(created_at)。
    /// 本例同时把四类噪声钉死：他人的干预、rejected 的干预、同世界他角色、同角色他世界。
    #[tokio::test]
    async fn memberships_last_active_at_uses_accepted_interventions_max() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_user(&state.db, "u2").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_world(&state.db, "w2", 0, "running").await;
        seed_cloud_char(&state.db, "c1", "u1", &json!({ "identity": { "name": "沈霜" } }).to_string()).await;
        seed_cloud_char(&state.db, "c2", "u1", &json!({ "identity": { "name": "别的卡" } }).to_string()).await;
        seed_cloud_char(&state.db, "c3", "u2", &json!({ "identity": { "name": "他人" } }).to_string()).await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_member(&state.db, "m2", "w1", "u1", "c2", "active").await;
        seed_member(&state.db, "m3", "w1", "u2", "c3", "active").await;

        // 目标对 (w1, c1)：accepted@1000、applied@3000（applied 计入——已被引擎消费仍是一次落地互动）。
        seed_intervention(&state.db, "iv1", "w1", "u1", "c1", "whisper", "accepted", 1_000).await;
        seed_intervention(&state.db, "iv2", "w1", "u1", "c1", "item", "applied", 3_000).await;
        // 噪声 ①：rejected（超配额/机审拒）时刻最新，但没落地 → 不得成为 lastActiveAt。
        seed_intervention(&state.db, "iv3", "w1", "u1", "c1", "whisper", "rejected", 9_000).await;
        // 噪声 ②：他人（u2）对同世界的干预，绝不能算进本人的互动。
        seed_intervention(&state.db, "iv4", "w1", "u2", "c3", "whisper", "accepted", 8_000).await;
        // 噪声 ③：同世界的另一张卡 → 只影响 c2 自己的时刻。
        seed_intervention(&state.db, "iv5", "w1", "u1", "c2", "whisper", "accepted", 2_000).await;
        // 噪声 ④：本人本角色但在他世界（w2，本人不在场）→ 不得渗进 w1 的行。
        seed_intervention(&state.db, "iv6", "w2", "u1", "c1", "whisper", "accepted", 7_000).await;

        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        let ms = v["memberships"].as_array().unwrap();
        assert_eq!(ms.len(), 2, "u1 在 w1 有两张卡");
        let by_cid: std::collections::HashMap<&str, &Value> =
            ms.iter().map(|m| (m["cloudCharacterId"].as_str().unwrap(), m)).collect();
        assert_eq!(
            by_cid["c1"]["lastActiveAt"].as_i64(),
            Some(3_000),
            "取 accepted/applied 的 MAX；rejected@9000、他人@8000、他世界@7000 均不计：{v}"
        );
        assert_eq!(by_cid["c2"]["lastActiveAt"].as_i64(), Some(2_000), "逐 (世界,角色) 独立取值");
    }

    #[tokio::test]
    async fn memberships_omits_last_active_at_without_landed_intervention() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_cloud_char(&state.db, "c1", "u1", &json!({ "identity": { "name": "沈霜" } }).to_string()).await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        // 仅有 rejected 记录 → 视同从未互动。
        seed_intervention(&state.db, "iv1", "w1", "u1", "c1", "whisper", "rejected", 5_000).await;

        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK);
        let m = &v["memberships"][0];
        assert!(
            m.get("lastActiveAt").is_none(),
            "无落地互动 → 不含该字段（不下发 0、不回退 joinedAt）：{v}"
        );
        assert!(m["joinedAt"].as_i64().is_some(), "joinedAt 仍在，前端自行兜底");
    }

    // ---------- 羁绊强度：有意不下发（口径未拍板） ----------

    /// 本端点**不得**出现 bondStrength 一类合成指标：总规格 §14 只定「达阈值双向解锁」，
    /// 未定义强度公式；原始关系维度另有带 §9.4 信息边界过滤的下发面 `/worlds/{id}/state-summary`。
    /// 本用例是防回归闸——防止后来者顺手加一个自造加权值，让未经评审的口径变成既成事实。
    #[tokio::test]
    async fn memberships_does_not_ship_synthetic_bond_strength() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_cloud_char(&state.db, "c1", "u1", &json!({ "identity": { "name": "沈霜" } }).to_string()).await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        // 世界带上关系图：即便有数据，本端点也不得据此合成任何强度值。
        sqlx::query("UPDATE worlds SET narrative_state_json = $1 WHERE id = 'w1'")
            .bind(
                json!({ "relations": [
                    { "from": "c1", "to": "npcX", "trust": 0.8, "affinity": 0.9, "fear": 0.0, "debt": 0.0 }
                ] })
                .to_string(),
            )
            .execute(&state.db)
            .await
            .expect("set narrative state");

        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK);
        let m = &v["memberships"][0];
        assert!(m.get("bondStrength").is_none(), "羁绊强度口径未拍板，不下发合成指标：{v}");
        assert!(
            !v.to_string().contains("affinity") && !v.to_string().contains("npcX"),
            "关系维度不从本列表接口外泄（信息边界由 state-summary 单世界面把守）：{v}"
        );
    }

    // ---------- 性能：固定条数查询，无 N+1 ----------

    /// 批量取数单元测试：**一次调用**解析全部 (world, character) 对。
    /// 这是「无 N+1」的结构性证据——端点侧总查询数恒为 2（成员行 1 条 + 本聚合 1 条），与成员数无关。
    #[tokio::test]
    async fn last_interaction_batch_resolves_all_pairs_in_one_call() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        let mut world_ids = Vec::new();
        for i in 0..40 {
            let w = format!("w{i}");
            seed_world(&state.db, &w, 0, "running").await;
            seed_intervention(
                &state.db,
                &format!("iv{i}"),
                &w,
                "u1",
                &format!("c{i}"),
                "whisper",
                "accepted",
                1_000 + i as i64,
            )
            .await;
            world_ids.push(w);
        }
        let map = last_interaction_by_pair(&state.db, "u1", &world_ids).await.unwrap();
        assert_eq!(map.len(), 40, "一次调用取回全部 40 对");
        for i in 0..40 {
            assert_eq!(map.get(&(format!("w{i}"), format!("c{i}"))), Some(&(1_000 + i as i64)));
        }
        // 空世界集合 → 直接短路，连这条聚合都不发。
        assert!(last_interaction_by_pair(&state.db, "u1", &[]).await.unwrap().is_empty());
    }

    /// 端到端：40 个成员行，头像与 lastActiveAt 逐行正确（列表规模不改变取数条数，也不退化正确性）。
    #[tokio::test]
    async fn memberships_batch_fields_correct_at_scale() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        for i in 0..40 {
            let (w, c) = (format!("w{i}"), format!("c{i}"));
            seed_world(&state.db, &w, 0, "running").await;
            seed_cloud_char(&state.db, &c, "u1", &json!({ "identity": { "name": c } }).to_string()).await;
            seed_member(&state.db, &format!("m{i}"), &w, "u1", &c, "active").await;
            // 偶数号：过审头像 + 一次落地互动；奇数号：未过审头像 + 无互动（两条分支都在规模下验一遍）。
            if i % 2 == 0 {
                set_avatar(&state.db, &c, &format!("/api/assets/objects/{c}.png"), "approved").await;
                seed_intervention(
                    &state.db,
                    &format!("iv{i}"),
                    &w,
                    "u1",
                    &c,
                    "whisper",
                    "accepted",
                    1_000 + i as i64,
                )
                .await;
            } else {
                set_avatar(&state.db, &c, &format!("/api/assets/objects/{c}.png"), "pending").await;
            }
        }

        let (s, v) = get_memberships(&state, Some(&token(&state, "u1"))).await;
        assert_eq!(s, StatusCode::OK, "body={v}");
        let ms = v["memberships"].as_array().unwrap();
        assert_eq!(ms.len(), 40);
        let by_cid: std::collections::HashMap<&str, &Value> =
            ms.iter().map(|m| (m["cloudCharacterId"].as_str().unwrap(), m)).collect();
        for i in 0..40 {
            let c = format!("c{i}");
            let m = by_cid[c.as_str()];
            if i % 2 == 0 {
                assert_eq!(m["avatarUrl"], format!("/api/assets/objects/{c}.png"), "第 {i} 行头像");
                assert_eq!(m["lastActiveAt"].as_i64(), Some(1_000 + i as i64), "第 {i} 行互动时刻");
            } else {
                assert!(m.get("avatarUrl").is_none(), "第 {i} 行 pending 头像不得下发");
                assert!(m.get("lastActiveAt").is_none(), "第 {i} 行无互动不得有该字段");
            }
        }
    }
}
