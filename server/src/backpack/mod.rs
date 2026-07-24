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
use crate::db::{new_id, now_ms};
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
         FROM items WHERE id = ?",
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
         JOIN world_templates wt ON wt.id = w.template_id WHERE w.id = ?",
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
    let exists = sqlx::query("SELECT 1 AS x FROM items WHERE id = ?")
        .bind(&item.id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if !exists {
        sqlx::query(
            "INSERT INTO items (id, narrative, effect_tags, origin_world_template_id, cosmology_json, power_tier, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
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
         VALUES (?, ?, ?, ?, 'owned', NULL, ?, ?)",
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
         WHERE b.user_id = ? AND b.status != 'consumed' ORDER BY b.acquired_at DESC",
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

/// 权威「我的角色 × 世界」清单：直接读 world_members（WHERE user_id=本人 AND status='active'），
/// 补齐日报反推的盲区（刚投放尚无日报的角色/世界也在场）。无 owner 泄漏——只出本人成员行。
/// 角色名解析复用 worlds::world_detail 的 `card_json → identity.name`（缺失兜底为 cloud_character_id）。
async fn my_memberships(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, wm.status AS mstatus, wm.joined_at AS joined_at, \
         w.id AS world_id, w.title AS title, w.room_type AS room_type, w.status AS wstatus, \
         w.state_revision AS state_revision, cc.card_json AS card \
         FROM world_members wm \
         JOIN worlds w ON w.id = wm.world_id \
         JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.user_id = ? AND wm.status = 'active' \
         ORDER BY wm.joined_at DESC",
    )
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;

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
        memberships.push(json!({
            "worldId": r.try_get::<String, _>("world_id")?,
            "worldTitle": r.try_get::<String, _>("title")?,
            "roomType": r.try_get::<String, _>("room_type")?,
            "worldStatus": r.try_get::<String, _>("wstatus")?,
            "stateRevision": r.try_get::<i64, _>("state_revision")?,
            "cloudCharacterId": cid,
            "characterName": name,
            "membershipStatus": r.try_get::<String, _>("mstatus")?,
            "joinedAt": r.try_get::<i64, _>("joined_at")?,
        }));
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
            "SELECT id FROM backpacks WHERE user_id = ? AND item_id = ? AND status != 'consumed' LIMIT 1",
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
        "SELECT 1 AS x FROM world_members WHERE world_id = ? AND user_id = ? AND status = 'active' LIMIT 1",
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
            "UPDATE backpacks SET status = ?, carried_world_id = ?, power_tier_override = ?, \
             effect_tags_override = ? WHERE id = ?",
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
             VALUES (?, ?, 'local', 1, ?, 'original', 'approved', 0, ?)",
        )
        .bind(id)
        .bind(owner)
        .bind(card_json)
        .bind(now_ms())
        .execute(db)
        .await
        .expect("seed cloud_character");
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
}
