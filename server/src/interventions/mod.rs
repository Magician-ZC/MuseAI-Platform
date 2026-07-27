//! 干预系统（S3）：影响环-托梦 / 影响环-道具（平台规格 §2.4 三环、§9.6 服务端权威）。
//!
//! POST /worlds/{id}/interventions  {kind, characterId, payload, expectedWorldRevision}
//!   Idempotency-Key + expectedWorldRevision。服务端权威校验（§9.6）：
//!   - 角色属于本人且 active 在场（否则 risk_event + RiskBlocked）；
//!   - expectedRevision 与世界当前 state_revision 不符 → 409；
//!   - whisper：≤100 字、非空，过 moderation（Approved 才 accepted，否则 rejected/moderation）；
//!   - item：物品真在 backpacks(owned/carried，carried 须匹配本世界)，否则 risk_event("forged_state")+RiskBlocked；
//!           世界准入 admission::check_admission 为 S4 占位（当前"存在即通过"，留 TODO）；
//!   - 托梦配额（R1，规格 §8【拍板 12】）：**每卡每阶段 N 条**（默认 3，运营可调，见
//!     `dream_quota_per_stage`），超限 → rejected("quota")。
//! GET /worlds/{id}/interventions/mine  我的干预记录与状态。
//!
//! accepted 的干预由 runtime 下一 tick 消费（whisper 进对应角色低优先层），消费后置 applied；本模块不改叙事状态。
//! 注意：applied **仍计入配额**——消费不是退额度，否则配额形同虚设（见 `dream_quota_per_stage` 上方注释）。

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::providers::ModerationVerdict;
use crate::{idempotency, safety};

const WHISPER_MAX_CHARS: usize = 100;

/// 托梦配额默认值：每卡每阶段 3 条（规格 §8【拍板 12】）。稀缺化让"何时说、说什么"成为真决策，
/// 物理封死"开局倒攻略"。
const DEFAULT_DREAM_QUOTA_PER_STAGE: i64 = 3;

/// 托梦配额（每卡每阶段）。**运营可调**（VALIDATION.md §0.2「产品规则参数化」，禁止写死）：
/// 环境变量 `MUSE_DREAM_QUOTA_PER_STAGE`（正整数）覆盖，非法/缺省回落 `DEFAULT_DREAM_QUOTA_PER_STAGE`。
/// 范式同 `runtime::token_cny_cents_per_1k`——本仓库尚无配置表，env 覆盖是当前唯一的运营开关形态；
/// 将来配置表落地后只改本函数内部（读表 + env 兜底），调用点与拒绝语义不变。
///
/// ── R1 过渡口径：**一个 world 实例 = 一个阶段** ──────────────────────────────────────────────
/// 规格 §8 的原文是"每卡每阶段"，但 Saga 阶段制（§3）在干预表上尚无 `saga_id` / `stage_no` 坐标，
/// 故当前按 `(world_id, character_id)` **全量累计、不带时间窗**：一个世界实例从开局到终局即一个阶段。
/// （若 Saga 最终落成"阶段 = 一个世界模板 / 一局世界"的粒度，本口径与规格原文等价，无需再改计数。）
///
/// 因此原 Q-1「按 tick 节奏把一天均分为额度时间窗」的计数（`quota_window_ms`）已废除：时间窗让配额
/// 随墙钟自动回补（等价于每天 tick_per_day × N 条），与"每阶段 N 条"的稀缺语义直接冲突，封不死
/// "开局倒攻略"。同理，计数覆盖 `status IN ('accepted','applied')`——runtime 在 commit 事务内把已
/// 喂入引擎的托梦由 accepted 置 applied（`runtime::commit`），只数 accepted 会让托梦一被消费就白送
/// 回额度；这两条是同一个漏洞的两面，必须一起堵。
///
/// TODO(saga)：若 Saga 落成"一个世界实例跨多个阶段"的粒度，需把 `stage_no`（或 `saga_id`）落到
/// interventions 行上并加进计数 SQL 的 WHERE，口径即回到规格原文"每卡每阶段"；
/// 届时无需改动调用点、拒绝语义（`reject_reason = "quota"`）与响应结构。
fn dream_quota_per_stage() -> i64 {
    parse_quota_override(std::env::var("MUSE_DREAM_QUOTA_PER_STAGE").ok().as_deref())
}

/// 配额覆盖值解析（与 env 读取分离，便于无副作用地测试回落规则）。
fn parse_quota_override(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_DREAM_QUOTA_PER_STAGE)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterventionReq {
    pub kind: String, // whisper | item
    pub character_id: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub expected_world_revision: i64,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worlds/{id}/interventions", post(create_intervention))
        .route("/worlds/{id}/interventions/mine", get(my_interventions))
}

async fn create_intervention(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<InterventionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let kind = req.kind.as_str();
    if kind != "whisper" && kind != "item" {
        return Err(ApiError::BadRequest("kind 必须为 whisper 或 item".into()));
    }

    // 幂等：同 key 同载荷 → 返回缓存响应；同 key 异载荷 → 409。
    let endpoint = "POST /worlds/:id/interventions";
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let idem_key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard = idempotency::guard(&state.db, &user.user_id, endpoint, idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // 世界存在 + 运行态 + revision CAS。（tick_per_day 原供额度时间窗，R1 改为每卡每阶段累计后不再参与计数。）
    let world: Option<(i64, String, i64)> =
        sqlx::query_as("SELECT state_revision, status, tick_per_day FROM worlds WHERE id = $1")
            .bind(&world_id)
            .fetch_optional(&state.db)
            .await?;
    let (state_revision, status, _tick_per_day) = world.ok_or(ApiError::NotFound)?;
    if status != "open" && status != "running" {
        return Err(ApiError::Conflict("world_not_running".into()));
    }
    if req.expected_world_revision != state_revision {
        return Err(ApiError::Conflict("revision".into()));
    }

    // 角色必须属于本人且 active 在场（服务端权威，§9.6）。
    let member: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM world_members WHERE world_id = $1 AND cloud_character_id = $2 AND user_id = $3 AND status = 'active'",
    )
    .bind(&world_id)
    .bind(&req.character_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    if member.is_none() {
        safety::record_risk(
            &state.db,
            Some(&user.user_id),
            Some(&world_id),
            "intervention_denied",
            json!({"reason": "character_not_present_or_owned", "characterId": req.character_id, "kind": kind}),
        )
        .await?;
        return Err(ApiError::RiskBlocked);
    }

    // 分类别授权/校验。
    if kind == "item" {
        // Q-2 + S-4：道具干预 P5 尚未接线（runtime 从不消费 accepted item，受理只会误导用户+白占额度）。
        // 受理前先做物权真实性校验（伪造声明仍记 forged_state），合法则明确返回 unsupported——
        // 无论哪条分支都在此提前返回：既不写 interventions 也不计额度，whisper 路径完全不受影响。
        let item_id = req
            .payload
            .get("itemId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ApiError::BadRequest("payload.itemId 缺失".into()))?;

        // 取该用户此道具的全部背包记录（含 sealed/consumed），据此区分三态：
        // 合法可用(owned / carried-here) / 良性不可用(sealed / consumed) / 伪造(不存在 / carried-elsewhere)。
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, carried_world_id FROM backpacks WHERE user_id = $1 AND item_id = $2",
        )
        .bind(&user.user_id)
        .bind(item_id)
        .fetch_all(&state.db)
        .await?;

        if rows.is_empty() {
            // 声明一个根本不在背包里的道具 = 伪造状态（§9.6 伪造背包清单）。
            safety::record_risk(
                &state.db,
                Some(&user.user_id),
                Some(&world_id),
                "forged_state",
                json!({"reason": "item_not_in_backpack", "itemId": item_id}),
            )
            .await?;
            return Err(ApiError::RiskBlocked);
        }

        // 是否持有可在本世界使用的副本（owned 或 carried-here）。
        let usable = rows.iter().any(|(st, carried)| {
            st == "owned" || (st == "carried" && carried.as_deref() == Some(world_id.as_str()))
        });
        if usable {
            // 物权合法但道具干预尚未开放（Q-2：P5 接线前明确拒绝，不写库不计额度）。
            return Err(ApiError::BadRequest("道具干预暂未开放".into()));
        }

        // 道具已随角色进入其他世界，声明投放本世界 = 伪造状态。
        let carried_elsewhere = rows
            .iter()
            .any(|(st, carried)| st == "carried" && carried.as_deref() != Some(world_id.as_str()));
        if carried_elsewhere {
            safety::record_risk(
                &state.db,
                Some(&user.user_id),
                Some(&world_id),
                "forged_state",
                json!({"reason": "item_carried_elsewhere", "itemId": item_id}),
            )
            .await?;
            return Err(ApiError::RiskBlocked);
        }

        // 剩余：sealed / consumed —— 合法持有但当前不可用（S-4：良性拒绝，不记 forged_state 以免污染风控）。
        return Err(ApiError::BadRequest("道具当前不可用".into()));
    }

    // whisper：长度与非空校验。
    let text = req.payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.trim().is_empty() {
        return Err(ApiError::BadRequest("whisper text 不能为空".into()));
    }
    if text.chars().count() > WHISPER_MAX_CHARS {
        return Err(ApiError::BadRequest("whisper 不能超过 100 字".into()));
    }

    // 托梦配额校验（超限即 rejected("quota")，不作为攻击）。R1 口径：按 **(world_id, character_id)**
    // 全量累计、不带时间窗（"一个 world 实例 = 一个阶段"，详见 dream_quota_per_stage 上方注释）。
    // - kind='whisper'：道具干预在上方已提前 return，从不写库，也不得被拖进计数；
    // - status IN ('accepted','applied')：applied 是"已被引擎消费"，不是"额度归还"——只数 accepted
    //   会让托梦一被 runtime 消费就白送回一格额度，配额形同虚设。
    // 顺序红线：本校验必须在机审之前，否则被机审拒的文本也会吃掉额度。
    let mut reject_reason: Option<String> = None;
    let used: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM interventions \
         WHERE world_id = $1 AND character_id = $2 AND kind = 'whisper' \
           AND status IN ('accepted', 'applied')",
    )
    .bind(&world_id)
    .bind(&req.character_id)
    .fetch_one(&state.db)
    .await?;
    // OOC 注解权补偿（§7 第 2 级）：复核确认「模型确实演错了」时补发的额度。
    // 🔴 **只加加数、不动被加数**：补偿存在独立的加数表里，上面那条计数 SQL 一个字符都没改——
    //    因为另外三条路都是错的：往 interventions 插「假托梦」会伪造玩家从没说过的话
    //    （且 runtime 会当真把它喂给引擎）；把 applied 改回 accepted 会抹掉「已被引擎消费」
    //    这个已落定的事实；新增一种 status 则直接篡改了上面这条计数的口径。
    //    这里变的只是**被比较的阈值**，配额的事实来源仍然唯一。
    let bonus = crate::annotations::dream_quota_bonus(&state.db, &world_id, &req.character_id).await;
    if used >= dream_quota_per_stage() + bonus {
        reject_reason = Some("quota".into());
    }

    let iid = crate::db::new_id("iv");

    // whisper moderation（额度通过后再机审，避免超额时多余模型调用）。
    if reject_reason.is_none() && kind == "whisper" {
        let text = req.payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let verdict = safety::moderate_and_queue(&state, "intervention", &iid, text).await?;
        if verdict != ModerationVerdict::Approved {
            reject_reason = Some("moderation".into());
        }
    }

    let final_status = if reject_reason.is_some() { "rejected" } else { "accepted" };

    sqlx::query(
        "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, expected_revision, status, reject_reason, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&iid)
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(&req.character_id)
    .bind(kind)
    .bind(req.payload.to_string())
    .bind(req.expected_world_revision)
    .bind(final_status)
    .bind(reject_reason.as_deref())
    .bind(crate::db::now_ms())
    .execute(&state.db)
    .await?;

    let resp = json!({
        "id": iid,
        "worldId": world_id,
        "kind": kind,
        "characterId": req.character_id,
        "status": final_status,
        "rejectReason": reject_reason,
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

/// 我在这个世界里的干预记录 + **剩余托梦额度**。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 为什么必须给额度，而不只是给记录
/// ════════════════════════════════════════════════════════════════════════════
///
/// 此前玩家**只能靠「被拒绝」发现自己没额度了**（`rejectReason: "quota"`）。
/// 那已经够糟，而真正的问题是：有效额度是 `基础配额 + OOC 申诉补偿`，
/// 而补偿是复核「确认模型确实演错了」之后补发的——**玩家在构造上算不出这个数**。
/// 他既不知道自己被补过几条，也没有任何地方能查。
///
/// 于是「还能不能托梦」这件事对玩家是**不可知**的，只能试。托梦是 §8 的核心互动，
/// 让它变成一个碰运气的按钮，是把一个确定性的规则做成了体验上的随机。
///
/// ⚠️ 这里**没有改任何口径**：`used` 的统计 SQL 与 `create_intervention` 里那条
/// **逐字相同**（同一个 `WHERE world_id AND character_id AND kind='whisper'
/// AND status IN ('accepted','applied')`），补偿也走同一个 `dream_quota_bonus`。
/// 露出来的就是**正在决定事情的那个数**，不是另算一份。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 「每阶段」= 每世界实例（`docs/build/open-decisions.md` §3）
/// ════════════════════════════════════════════════════════════════════════════
///
/// 配额按 `(world_id, character_id)` 统计，即**一个世界实例一份**。
/// 「Saga 是否落成一个实例跨多个阶段」尚未拍板（见 open-decisions §3），
/// 在那之前「阶段」与「世界实例」是同一件事。响应里把这句话**写出来**，
/// 免得玩家把它理解成「每天」或「每个 Saga」——那是两个数量级的差别。
async fn my_interventions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows: Vec<(String, String, String, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, kind, character_id, status, reject_reason, created_at FROM interventions \
         WHERE world_id = $1 AND user_id = $2 ORDER BY created_at DESC, id DESC LIMIT 50",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, kind, cid, status, reason, created)| {
            json!({"id": id, "kind": kind, "characterId": cid, "status": status, "rejectReason": reason, "createdAt": created})
        })
        .collect();

    // 本人在这个世界的角色（一人一卡：`world_members` 上 (world_id, cloud_character_id) 唯一，
    // 且 join 有防自刷门，故至多一张）。没入场过 → 没有额度可言，给 null 而不是 0：
    // 「还没进这个世界」与「额度用光了」是两件事，混成 0 会让玩家以为自己被封了。
    let cid: Option<String> = sqlx::query_scalar(
        "SELECT cloud_character_id FROM world_members \
         WHERE world_id = $1 AND user_id = $2 AND status = 'active' \
         ORDER BY cloud_character_id ASC LIMIT 1",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;

    let quota = match cid.as_deref() {
        None => json!({
            "applicable": false,
            "why": "你还没有角色在这个世界里（未入场），故没有托梦额度可言。\
                    ⚠️ 这不是「额度用光了」——入场后额度按下方口径重新算。",
        }),
        Some(cid) => {
            // 🔴 与 `create_intervention` 的判定**逐字相同**的统计口径。两处一旦漂移，
            // 玩家看到的「还剩 2 条」和实际能发的条数就会对不上，而那种 bug 只有玩家会遇到。
            let used: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM interventions \
                 WHERE world_id = $1 AND character_id = $2 AND kind = 'whisper' \
                   AND status IN ('accepted', 'applied')",
            )
            .bind(&world_id)
            .bind(cid)
            .fetch_one(&state.db)
            .await?;
            let base = dream_quota_per_stage();
            let bonus = crate::annotations::dream_quota_bonus(&state.db, &world_id, cid).await;
            json!({
                "applicable": true,
                "characterId": cid,
                "base": base,
                // 🔴 补偿单列而不是并进 base：玩家有权知道「多出来的这条是申诉换来的」，
                // 而不是以为基础额度变了。口径同 `annotations` 的加数表。
                "bonus": bonus,
                "effective": base + bonus,
                "used": used,
                "remaining": (base + bonus - used).max(0),
                "scope": "per_world_instance_per_character",
                "scopeNote": "每个**世界实例**一份，按角色算。当前「阶段」与「世界实例」是同一件事\
                              （Saga 是否跨阶段尚未拍板）。⚠️ 它**不是**「每天」——额度不随墙钟回补，\
                              世界一直不提交拍时早先发出的托梦依然占额度。",
                "bonusNote": "bonus 来自 OOC 申诉复核确认「模型确实演错了」后的补偿（§7 第 2 级）。\
                              它是**加数**：基础额度与已用计数一个字节都不受影响。",
            })
        }
    };

    Ok(Json(json!({ "interventions": items, "dreamQuota": quota })))
}

#[cfg(test)]
// 🔴 `await_holding_lock`：本模块用例经 `quota_guard()` 拿一把**进程级 env 锁**，
// 并**刻意跨 await 持有**——`MUSE_DREAM_QUOTA_PER_STAGE` 是进程级的，而用例与其它模块
// 同属一个测试二进制、默认并发跑；不跨 await 持有，锁就只覆盖到第一个 `.await` 之前，
// 等于没锁（症状是单跑必绿、全跑偶发红，本轮在 `ContainerSwitch` 上实际踩过一次）。
//
// 安全性：`#[tokio::test]` 默认 current_thread 运行时，一个用例一个运行时，
// 不存在「同一线程上另一个任务来抢同一把 std::sync::Mutex」这个死锁前提；
// 跨线程的并发用例由这把锁**正确串行化**，那正是它存在的理由。
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::safety::testkit::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// 配额相关用例共享同一把锁：`MUSE_DREAM_QUOTA_PER_STAGE` 是进程级 env，
    /// 覆盖用例与其它按默认配额断言的用例并发跑会互相污染，故串行化（中毒也照常取用，不阻塞后续）。
    static QUOTA_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn quota_guard() -> std::sync::MutexGuard<'static, ()> {
        QUOTA_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 直接插一条干预（绕过 API），用于预置配额基数：可指定 kind / status / created_at。
    #[allow(clippy::too_many_arguments)]
    async fn seed_intervention(
        state: &AppState,
        id: &str,
        world: &str,
        user: &str,
        character: &str,
        kind: &str,
        status: &str,
        created_at: i64,
    ) {
        sqlx::query(
            "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, expected_revision, status, created_at) \
             VALUES ($1, $2, $3, $4, $5, '{}', 0, $6, $7)",
        )
        .bind(id)
        .bind(world)
        .bind(user)
        .bind(character)
        .bind(kind)
        .bind(status)
        .bind(created_at)
        .execute(&state.db)
        .await
        .expect("seed intervention");
    }

    async fn post_intervention(state: &AppState, token_str: &str, world: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let app = crate::app::build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/worlds/{world}/interventions"))
                    .header("authorization", format!("Bearer {token_str}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v = serde_json::from_slice(&bytes).unwrap_or(json!(null));
        (status, v)
    }

    /// 取 `GET /worlds/{id}/interventions/mine` 的 `dreamQuota`。
    async fn quota_of(state: &AppState, tk: &str, world: &str) -> serde_json::Value {
        let resp = crate::app::build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/worlds/{world}/interventions/mine"))
                    .header("authorization", format!("Bearer {tk}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(json!(null));
        v["dreamQuota"].clone()
    }

    /// 🔴 **剩余额度必须在花掉之前查得到**。
    ///
    /// 此前玩家只能靠「被拒绝」发现没额度了，而有效额度 = 基础 + OOC 申诉补偿——
    /// 补偿是复核之后补发的，玩家**在构造上算不出这个数**。于是「还能不能托梦」
    /// 对他是不可知的，只能试。托梦是 §8 的核心互动，让它变成碰运气的按钮，
    /// 是把一条确定性的规则做成了体验上的随机。
    #[tokio::test]
    async fn remaining_dream_quota_is_visible_before_you_spend_it() {
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let q = quota_of(&state, &tk, "w1").await;
        assert_eq!(q["applicable"], json!(true), "{q}");
        assert_eq!(q["used"], json!(0));
        assert_eq!(q["remaining"], q["effective"], "一条没发时 remaining == effective: {q}");
        assert_eq!(q["scope"], "per_world_instance_per_character");

        // 发一条 → used 加一、remaining 减一（口径与 create 的判定同源）。
        post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "早些歇息"}, "expectedWorldRevision": 0}),
        )
        .await;
        let q2 = quota_of(&state, &tk, "w1").await;
        assert_eq!(q2["used"], json!(1), "{q2}");
        assert_eq!(
            q2["remaining"].as_i64().unwrap(),
            q["remaining"].as_i64().unwrap() - 1,
            "🔴 读出来的剩余必须与实际能发的条数同源，否则玩家看到的「还剩 2 条」会对不上"
        );

        // 🔴 **已被引擎消费的托梦（applied）仍然占额度**——这是配额口径的要害，
        // 也是读取面最容易与判定漂移的地方（少算 applied，玩家就会看到一个比实际大的剩余，
        // 然后在发的时候被拒）。故意直插一条 applied 行来钉住它。
        //
        // ⚠️ 这一条是故障注入补出来的：上一版只发了一条 accepted，于是把读取面的
        // `IN ('accepted','applied')` 改成 `IN ('accepted')` 后**所有用例照样绿**。
        sqlx::query(
            "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, \
             expected_revision, status, reject_reason, created_at) \
             VALUES ('iv_applied', 'w1', 'u1', 'c1', 'whisper', '{}', 0, 'applied', NULL, $1)",
        )
        .bind(crate::db::now_ms())
        .execute(&state.db)
        .await
        .unwrap();
        let q3 = quota_of(&state, &tk, "w1").await;
        assert_eq!(
            q3["used"], json!(2),
            "🔴 已被引擎消费的托梦仍占额度 —— 少算它，玩家会看到一个比实际大的剩余然后被拒: {q3}"
        );
    }

    /// 🔴 **没入场 ≠ 额度用光**。给 `applicable: false` 而不是 `remaining: 0`——
    /// 后者会让玩家以为自己被封了。
    #[tokio::test]
    async fn not_being_in_the_world_is_not_the_same_as_having_no_quota_left() {
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        // 刻意不 seed_member：这个人没进过这个世界。
        let tk = token(&state, "u1");

        let q = quota_of(&state, &tk, "w1").await;
        assert_eq!(q["applicable"], json!(false), "{q}");
        assert!(q.get("remaining").is_none(), "🔴 不得给 remaining: 0 —— 那会被读成「额度用光了」: {q}");
        assert!(
            q["why"].as_str().unwrap_or("").contains("不是「额度用光了」"),
            "得把这两件事的区别直说: {q}"
        );
    }

    /// OOC 申诉补偿**单列**，不并进基础额度：玩家有权知道「多出来的这条是申诉换来的」。
    #[tokio::test]
    async fn the_appeal_bonus_is_reported_separately_from_the_base_quota() {
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        sqlx::query(
            "INSERT INTO dream_quota_compensations (id, appeal_id, world_id, character_id, \
             user_id, grants, reason, created_at) VALUES ('dqc1', 'ap1', 'w1', 'c1', 'u1', 2, 'x', $1)",
        )
        .bind(crate::db::now_ms())
        .execute(&state.db)
        .await
        .unwrap();

        let q = quota_of(&state, &token(&state, "u1"), "w1").await;
        assert_eq!(q["bonus"], json!(2), "{q}");
        assert_eq!(
            q["effective"].as_i64().unwrap(),
            q["base"].as_i64().unwrap() + 2,
            "🔴 有效额度 = 基础 + 补偿；把补偿并进 base 会让玩家以为基础额度变了: {q}"
        );
    }

    #[tokio::test]
    async fn whisper_accepted() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "记得完成今天的画作"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "accepted");
    }

    #[tokio::test]
    async fn revision_mismatch_conflicts() {
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 7, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let (status, _v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "hi"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn forged_item_blocked_and_recorded() {
        // 越权道具：声明一个不在背包里的道具 → RiskBlocked + risk_event(forged_state)。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let (status, _v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "item", "characterId": "c1", "payload": {"itemId": "sword_of_nobody"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let n = count(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind = 'forged_state'").await;
        assert_eq!(n, 1, "应记录一条 forged_state 风控事件");
    }

    #[tokio::test]
    async fn owned_item_unsupported_no_quota() {
        // Q-2：道具干预 P5 未接线 —— 合法持有的道具受理前即返回 unsupported，不写库不计额度。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_backpack(&state.db, "b1", "u1", "gem", "owned", None).await;
        let tk = token(&state, "u1");

        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "item", "characterId": "c1", "payload": {"itemId": "gem"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body={v}");
        // 不写 interventions（不占额度），不记 forged_state（合法持有）。
        let ivs = count(&state.db, "SELECT COUNT(*) FROM interventions WHERE user_id='u1'").await;
        assert_eq!(ivs, 0, "道具干预不应写库占额度");
        let risks = count(&state.db, "SELECT COUNT(*) FROM risk_events").await;
        assert_eq!(risks, 0, "合法持有不应记风控");
    }

    #[tokio::test]
    async fn sealed_or_consumed_item_benign_no_forged() {
        // S-4：sealed / consumed 是合法持有的当前不可用态 → 良性 BadRequest，不记 forged_state。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_backpack(&state.db, "b1", "u1", "relic", "sealed", None).await;
        seed_backpack(&state.db, "b2", "u1", "potion", "consumed", None).await;
        let tk = token(&state, "u1");

        for item in ["relic", "potion"] {
            let (status, v) = post_intervention(
                &state,
                &tk,
                "w1",
                json!({"kind": "item", "characterId": "c1", "payload": {"itemId": item}, "expectedWorldRevision": 0}),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "item={item} body={v}");
        }
        let n = count(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind = 'forged_state'").await;
        assert_eq!(n, 0, "sealed/consumed 合法道具不应记 forged_state");
    }

    #[tokio::test]
    async fn carried_elsewhere_item_forged() {
        // S-4 边界：道具已随角色进入别的世界，声明投放本世界仍属伪造 → RiskBlocked + forged_state。
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_backpack(&state.db, "b1", "u1", "blade", "carried", Some("w_other")).await;
        let tk = token(&state, "u1");

        let (status, _v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "item", "characterId": "c1", "payload": {"itemId": "blade"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let n = count(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind = 'forged_state'").await;
        assert_eq!(n, 1, "异地携带却声明本世界投放应记 forged_state");
    }

    #[tokio::test]
    async fn quota_never_resets_by_wall_clock() {
        // 【原 quota_window_resets_when_world_never_commits 的重写】R1 口径下时间窗已废除：
        // 世界反复不提交（accepted 从不 →applied）时，很久以前创建的托梦**依然占额度**，
        // 额度不随墙钟回补——否则"每阶段 N 条"退化为"每天 tick_per_day × N 条"，封不死开局倒攻略。
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        // 旧口径下 tick_per_day=3 → 窗口 8h；置 3 条 9h 前创建、至今仍 accepted 的托梦（模拟一直 noop）。
        let old = crate::db::now_ms() - 9 * 3_600_000;
        for i in 0..DEFAULT_DREAM_QUOTA_PER_STAGE {
            seed_intervention(&state, &format!("old{i}"), "w1", "u1", "c1", "whisper", "accepted", old).await;
        }
        let tk = token(&state, "u1");
        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "隔了很久再来一条"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "rejected", "旧口径的时间窗回补必须已失效");
        assert_eq!(v["rejectReason"], "quota");
    }

    #[tokio::test]
    async fn quota_exceeded_rejected() {
        // 【重写】维度从 (world, user, 时间窗) 改为 (world, 卡)：同一张卡在本阶段内连发，
        // 前 N 条 accepted，第 N+1 条 rejected("quota")。
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        for i in 0..DEFAULT_DREAM_QUOTA_PER_STAGE {
            let (status, v) = post_intervention(
                &state,
                &tk,
                "w1",
                json!({"kind": "whisper", "characterId": "c1", "payload": {"text": format!("第 {i} 条托梦")}, "expectedWorldRevision": 0}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "body={v}");
            assert_eq!(v["status"], "accepted", "额度内第 {i} 条应受理");
        }

        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "再来一条"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "rejected");
        assert_eq!(v["rejectReason"], "quota");
    }

    #[tokio::test]
    async fn applied_whispers_still_consume_quota() {
        // 最关键的回归防线：runtime commit 会把已喂入引擎的托梦 accepted→applied。
        // 若计数只数 accepted，托梦一被消费就白送回额度 → 配额形同虚设。applied 必须照样占额度。
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let now = crate::db::now_ms();
        for i in 0..DEFAULT_DREAM_QUOTA_PER_STAGE {
            seed_intervention(&state, &format!("ap{i}"), "w1", "u1", "c1", "whisper", "applied", now).await;
        }
        let tk = token(&state, "u1");
        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "被消费后不该回血"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "rejected", "applied 必须计入额度，消费不等于退额度");
        assert_eq!(v["rejectReason"], "quota");
    }

    #[tokio::test]
    async fn quota_counted_per_character() {
        // "每卡"：c1 用满（accepted + applied 混合）不影响同一用户在同一世界的另一张卡 c2。
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        seed_member(&state.db, "m2", "w1", "u1", "c2", "active").await;
        let now = crate::db::now_ms();
        for i in 0..DEFAULT_DREAM_QUOTA_PER_STAGE {
            let st = if i % 2 == 0 { "accepted" } else { "applied" };
            seed_intervention(&state, &format!("c1_{i}"), "w1", "u1", "c1", "whisper", st, now).await;
        }
        let tk = token(&state, "u1");

        // c1 已满额 → 拒。
        let (_s, v1) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "c1 超额"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(v1["rejectReason"], "quota");

        // c2 独立计数 → 受理。
        let (status, v2) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c2", "payload": {"text": "c2 的第一条"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v2}");
        assert_eq!(v2["status"], "accepted", "配额按卡独立，不应被同用户另一张卡吃掉");
    }

    #[tokio::test]
    async fn item_interventions_excluded_from_quota() {
        // 计数带 kind='whisper' 过滤：历史/外部写入的 item 行不得挤占托梦额度
        //（现网 item 分支本就提前 return 不写库，此处防的是计数维度回归）。
        let _g = quota_guard();
        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let now = crate::db::now_ms();
        for i in 0..(DEFAULT_DREAM_QUOTA_PER_STAGE + 2) {
            seed_intervention(&state, &format!("it{i}"), "w1", "u1", "c1", "item", "accepted", now).await;
        }
        let tk = token(&state, "u1");
        let (status, v) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "item 不该吃托梦额度"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={v}");
        assert_eq!(v["status"], "accepted", "item 干预不得计入托梦配额");
    }

    #[tokio::test]
    async fn quota_configurable_via_env() {
        // VALIDATION.md §0.2：配额必须运营可调。env 置 1 后，第 2 条即超额。
        let _g = quota_guard();
        std::env::set_var("MUSE_DREAM_QUOTA_PER_STAGE", "1");
        assert_eq!(dream_quota_per_stage(), 1, "env 覆盖应生效");

        let state = test_state().await;
        seed_user(&state.db, "u1").await;
        seed_world(&state.db, "w1", 0, "running").await;
        seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
        let tk = token(&state, "u1");

        let (_s1, v1) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "唯一的一条"}, "expectedWorldRevision": 0}),
        )
        .await;
        assert_eq!(v1["status"], "accepted", "body={v1}");

        // 默认配额 3 时这条会被受理，env 覆盖为 1 后必须被拒。
        let (_s2, v2) = post_intervention(
            &state,
            &tk,
            "w1",
            json!({"kind": "whisper", "characterId": "c1", "payload": {"text": "第二条"}, "expectedWorldRevision": 0}),
        )
        .await;
        std::env::remove_var("MUSE_DREAM_QUOTA_PER_STAGE");

        assert_eq!(v2["status"], "rejected", "body={v2}");
        assert_eq!(v2["rejectReason"], "quota");
        assert_eq!(dream_quota_per_stage(), DEFAULT_DREAM_QUOTA_PER_STAGE, "移除 env 后回落默认值");
    }

    #[test]
    fn quota_override_falls_back_on_invalid() {
        // 非正整数/垃圾值一律回落默认，避免运营误配把配额调成 0 或负数把托梦通道锁死。
        assert_eq!(parse_quota_override(Some("5")), 5);
        assert_eq!(parse_quota_override(Some(" 2 ")), 2);
        assert_eq!(parse_quota_override(Some("0")), DEFAULT_DREAM_QUOTA_PER_STAGE);
        assert_eq!(parse_quota_override(Some("-1")), DEFAULT_DREAM_QUOTA_PER_STAGE);
        assert_eq!(parse_quota_override(Some("abc")), DEFAULT_DREAM_QUOTA_PER_STAGE);
        assert_eq!(parse_quota_override(Some("")), DEFAULT_DREAM_QUOTA_PER_STAGE);
        assert_eq!(parse_quota_override(None), DEFAULT_DREAM_QUOTA_PER_STAGE);
    }

    /// OOC 补偿真正兑现：复核确认「模型确实演错了」后，用满基础配额的角色能再发一条托梦。
    ///
    /// 🔴 本用例守的是「只加加数、不动被加数」：补偿入账后 `used` 仍是 3
    /// （补偿一行都没往 interventions 里插——那会伪造玩家从没说过的话，且 runtime 会当真喂给引擎），
    /// 变的只是被比较的阈值。配额的事实来源始终唯一。
    #[tokio::test]
    async fn ooc_compensation_raises_threshold_without_touching_the_count() {
        let _g = quota_guard();
        let state = test_state().await;
        seed_world(&state.db, "w_comp", 0, "running").await;

        // 用满基础配额（默认 3 条）。
        for i in 0..3 {
            seed_intervention(&state, &format!("iv_c{i}"), "w_comp", "u1", "c1", "whisper", "accepted", 0).await;
        }
        let count_sql = "SELECT COUNT(*) FROM interventions \
             WHERE world_id = $1 AND character_id = $2 AND kind = 'whisper' \
               AND status IN ('accepted', 'applied')";
        let used_before: i64 = sqlx::query_scalar(count_sql)
            .bind("w_comp")
            .bind("c1")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(used_before, 3, "前置：基础配额已用满");
        assert!(used_before >= dream_quota_per_stage(), "前置：此时应已超限");

        // 补偿入账（模拟 OOC 复核确认的产物）。
        sqlx::query(
            "INSERT INTO dream_quota_compensations \
             (id, appeal_id, world_id, character_id, user_id, grants, created_at) \
             VALUES ('dqc1', 'ap1', 'w_comp', 'c1', 'u1', 1, 0)",
        )
        .execute(&state.db)
        .await
        .unwrap();

        let used_after: i64 = sqlx::query_scalar(count_sql)
            .bind("w_comp")
            .bind("c1")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(
            used_after, 3,
            "🔴 补偿绝不能往 interventions 插行：那是伪造玩家没说过的话，runtime 会当真把它喂给引擎"
        );

        let bonus = crate::annotations::dream_quota_bonus(&state.db, "w_comp", "c1").await;
        assert_eq!(bonus, 1, "补偿账应记到 1 条");
        assert!(
            used_after < dream_quota_per_stage() + bonus,
            "补偿后阈值抬高，应重新有额度：used={used_after} base={} bonus={bonus}",
            dream_quota_per_stage()
        );
    }

}
