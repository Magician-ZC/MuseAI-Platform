//! 新手动线（总规格 `docs/build/spec-world-ecosystem.md` §13【拍板 21】）。
//!
//! 规格原文的动线：
//! 注册 → **新人大礼包**（1 张预制精品卡【绕过编卡墙】+ 首个单人速通本 + 3 条托梦 + 1 张低星副本卡）
//! → **5 分钟单人微本**（1 角色数拍，当场看到「卡活了」的魔法时刻；兼教学：托梦/观演/结算）
//! → 渐进捏人解锁（§7）→ 进 1-2★ 官方阶段。**Time-to-first-magic ≤ 10 分钟**为硬指标。
//!
//! 这条动线是 `docs/VALIDATION.md` §2 **T0 门槛**（「10 分钟内完成首个微本 ≥70%」）的被测对象——
//! T0 是「全部商业模式的地基」，没有本模块，T0 无法开测。
//!
//! ## 端点（全部 `AuthUser`）
//!
//! ```text
//! GET  /api/onboarding/presets                  预制卡库（选卡页；不含卡全文，只给 id/名字/卖点）
//! POST /api/me/onboarding/gift                  领取新人大礼包（幂等；每人一次由 DB 主键保证）
//! GET  /api/me/onboarding                       我的新手动线状态（领没领 / 投放没投放 / 下一步）
//! POST /api/me/onboarding/microworld/start      开演（微本世界 open → running）
//! ```
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 六条硬约束，改本模块前先读完
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ### ① 未验证功能默认关闭（VALIDATION.md §0.1）
//! 整块能力由运营开关 `MUSE_ONBOARDING` 控制，**默认关闭**。范式抄 `worlds::deathmatch_enabled`
//! 与 `invitations::invitations_enabled`——**前门拒绝 + 读取侧降级双保险**：关闭时四个端点全 404
//! （不是 403：不向外泄露「平台有这个未开放功能」），且**已领过的礼包也读不出、开不了演**，
//! 再打开则原样恢复。是可逆急停阀，不是一次性阉割。
//!
//! 🔵 **本模块是运行时开关体系（`crate::flags`）的参考接线**：`MUSE_ONBOARDING` 现在经
//! `flags::is_enabled` 解析，支持**按用户灰度**（正对 §2 T0 的「邀请制 ≤100 人」）。
//! env 仍是兜底层——`runtime_flags` 表为空时行为与接线前逐字一致，见 `onboarding_enabled`。
//!
//! ### ② 礼包不是特权通道 —— 本模块**永不写 `world_members`**
//! 体例同 `invitations`（那里有源码断言测试锁死同一性质，本模块的对应用例是
//! `tests::module_never_writes_world_members`）。领取礼包只做两件事：**发一张预制卡** +
//! **建一个属于你的微本世界**。真正入场仍必须调用既有 `POST /worlds/{id}/join`，于是 join 的
//! 全部服务端权威校验一条不少地生效：角色属本人 / approved / 未撤回 · 人数上限 · 一人一卡防自刷 ·
//! **同源唯一** · 星级历练准入 · 生死契约签署 · 未成年门。复制 join 的校验去「预判」就是制造侧路，
//! 侧路必然漂移，漂移就成了绕过红线的口子。
//!
//! ### ③ 每人只领一次 = 数据库主键，不是应用层读-判-写
//! `onboarding_grants.user_id` 是 **PRIMARY KEY**（迁移 0031）。领取事务把「发卡 + 建房 + 登记」
//! 放在同一个事务里，**登记行最后写**：撞主键即整体回滚（卡与世界一起消失），再读回既有登记行、
//! 返回与首次**逐字节相同**的响应。`Idempotency-Key` 是另一层（覆盖同一次点击的 HTTP 重试），
//! 两层都要——单靠幂等键，客户端换个 key 再点就击穿了。
//!
//! ### ④ 微本必须自带 NPC（否则永远卡死，这是本任务最容易做死的地方）
//! `runtime` 的推进门是 `member_ids.is_empty() || active_cards.len() < 2`，而 `active_cards`
//! **把 NPC 也算在内**。「单角色微本」指的是**玩家角色数为 1**，不是世界里只有一个角色。
//! 骨架自带 2 个 NPC（`microworld::skeleton_json`），玩家 1 张卡 + NPC 2 = 3 张活跃卡 → 过门。
//! 详见 `microworld` 模块头。
//!
//! ### ⑤ 同源唯一（§7）的取舍：**一人一世界实例**
//! 预制卡是「发给很多新用户的同一份内容」。若它带提取源指纹且 `pristine=1`，两个新用户拿同一张卡
//! 进同一个世界，第二个人必被 `worlds::join_world` 的同源唯一门拒掉
//! （「这个世界已经有一个「沈砚舟」了」）——新手第一步就撞墙。
//!
//! 本模块选 **(a) 每个新用户的微本是独立世界实例**，理由不止于回避撞车：
//! - **节奏隔离才是主因**：§13 要的是「5 分钟速通 + 从头教学」。共享世界意味着新人从别人的第 37 拍
//!   插进去，既看不懂也等不到结局，Time-to-first-magic 直接崩。一人一世界是这条指标的前提，
//!   躲开同源唯一只是顺带的红利。
//! - 世界数膨胀是**可控的**：`maxWorldTicks` 默认 3 → 微本 4 拍内必然 `ended`，
//!   `runtime::schedule_due_ticks` 的 `WHERE status='running'` 门自动停排，死世界不再消耗任何算力；
//!   `visibility='private'` 使它不进大厅列表（`WHERE visibility IN ('official','public')`）。
//!   代价是 worlds 表按注册用户数线性增长的存档行，这是可接受的。
//!
//! 另有**两道与 (a) 正交的保险**，使预制卡即便被放进共享世界也不会撞车：
//! - 预制卡**原创虚构、无 `identity.sourceWork`** → 落库 `source_fingerprint` 恒为 NULL
//!   → join 的同源判定直接放行（「指纹为 NULL 一律放行」）；
//! - 落库显式写 `pristine=0`（视为「已由用户领取」而非出厂原味卡），即便将来有人给预制卡补了指纹，
//!   同源门的 `pristine=1` 前置条件也不成立。
//! 用例 `two_users_preset_cards_can_join_same_world` 把这两道保险钉住。
//!
//! ### ⑥ 资产单一写入路径（真红线 §0.2）
//! 本模块**不发任何道具、不发任何历练**：不直插背包表，也不直改角色卡的历练列。
//! 微本的产出全部经既有结算路径落地——终局时 `runtime::finalize_ending_tx` →
//! `progression::settle_idle_world_ending_tx` → `grant_mileage_tx`（历练唯一写入路径）。
//! 将来礼包若要带道具，必须走 `backpack::grant_item_tx`，不得在此直插。
//!
//! ## 规格里本次**未实现**的一项（TODO）
//!
//! 礼包的「**1 张低星副本卡**」已于副本卡资产层（迁移 0032）落地后接上：领取事务内调
//! `subplot::grant_card_tx`，`grant_key = starter:{user_id}`，固定 1★。
//! ⚠️ 两个开关是**正交**的——副本卡开关（`MUSE_SUBPLOT_CARDS`）关闭时**跳过发卡而非报错**，
//! 玩家仍拿到预制卡与微本。一块未开放的经济模块不该有能力打死整条新手动线。
//! 「3 条托梦」不单独发放：配额已是 `MUSE_DREAM_QUOTA_PER_STAGE` 全局参数，
//! 在此复述数字只会制造第二个事实源。
//! 「3 条托梦」**不需要单独发放**：托梦配额已是每卡每阶段的全局参数
//! （`interventions::dream_quota_per_stage`，`MUSE_DREAM_QUOTA_PER_STAGE` 默认 3），
//! 新卡自动享有，另造一套发放逻辑只会出现两个事实源。

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;
use crate::worlds::{create_world_tx, CreateWorldParams, LETHALITY_SANCTUARY};

pub mod microworld;
pub mod presets;

#[cfg(test)]
mod tests;

// ---------------- 运营开关（VALIDATION.md §0.1 未验证功能默认关闭） ----------------

/// 新手动线运营开关环境变量。
const ENV_ONBOARDING_ENABLED: &str = "MUSE_ONBOARDING";

/// 新人礼包附赠的副本卡星级（§13「1 张**低星**副本卡」）。
/// 固定 1★ 而非参数化：礼包是**教学物**不是产出物，调高它就等于绕过「打世界换卡」这条正路
/// —— 副本卡的稀缺性由 §10 的确定性产出表与合成回收口共同维持，礼包不该成为侧门。
const STARTER_SUBPLOT_CARD_STAR: i64 = 1;
/// 礼包卡的卡面名。叙事信物性质（§10 道具用途之一），不带任何效力。
const STARTER_SUBPLOT_CARD_LABEL: &str = "新手纪念·初入世界";

/// 新手动线默认值 = **关闭**。
///
/// 🔴 新手动线是 VALIDATION.md §2 的 **T0 被测对象**，不是已验证结论：它要发实物（角色卡占卡位）、
/// 要建世界（消耗模型预算），且「10 分钟完成率 ≥70%」这个门槛还没有任何真实数据。
/// 代码合并不等于对用户开放——必须运营显式打开。
const DEFAULT_ONBOARDING_ENABLED: bool = false;

/// 🔴 **编译期钉死**：接线后默认值出现在两处（本常量 + `flags::KNOWN_FLAGS` 登记表），
/// 两处不一致就是「默认关闭」这条 §0.1 约束有了两个事实源。改一处不改另一处直接编不过。
const _: () = assert!(
    crate::flags::declared_default(ENV_ONBOARDING_ENABLED) == DEFAULT_ONBOARDING_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_ONBOARDING 的默认值必须与 DEFAULT_ONBOARDING_ENABLED 一致"
);

/// 新手动线是否已由运营开启。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 **运行时开关体系的参考接线**（R1 补齐项，见 `crate::flags` 模块头）
/// ════════════════════════════════════════════════════════════════════════════
///
/// 本函数曾经只读 env。现在改为经统一入口 `flags::is_enabled` 解析，回落链是：
///
/// ```text
///   runtime_flags(user=<本人>) → runtime_flags(global) → env MUSE_ONBOARDING → false
/// ```
///
/// 🔴 **行为零变化的保证**：`runtime_flags` 表为空时（迁移 0036 不插任何种子数据），
/// 解析必然落到 env 分支，且 `flags::parse_env_bool` 与本函数原实现**逐字同构**
/// （`1/true/on/yes` 开、`0/false/off/no` 关、其余回落 `DEFAULT_ONBOARDING_ENABLED`）。
/// 于是本模块既有全部用例（都在空表上跑）一行不改即为回归保护，
/// 另有 `crate::flags::tests::wired_onboarding_matches_legacy_env_semantics` 逐值比对。
///
/// 🔴 **默认仍是关闭**：DB 无记录 + env 未设 → `flags::KNOWN_FLAGS` 里声明的 `false`。
/// 引入开关表**没有**把任何未验证功能变成默认开启（`flags::tests` 有专门红线用例）。
///
/// 灰度粒度选 **user**：新手动线的开放范围就是「哪些人能领礼包」，正对 VALIDATION §2
/// T0 的「邀请制 ≤100 人」——运营给这 100 个 user 各写一条 `enabled=1` 的记录即可开测，
/// 不必也不应把大盘打开。（world 作用域对本模块无意义：微本世界是领礼包时**才创建**的，
/// 判定发生在世界存在之前。）
///
/// 保留 `ENV_ONBOARDING_ENABLED` / `DEFAULT_ONBOARDING_ENABLED` 两个常量与下方 RAII 夹具：
/// env 仍是兜底层，不是被删掉的旧路径。
pub async fn onboarding_enabled(db: &sqlx::AnyPool, user_id: Option<&str>) -> bool {
    let ctx = match user_id {
        Some(u) => crate::flags::FlagCtx::user(u),
        None => crate::flags::FlagCtx::global(),
    };
    crate::flags::is_enabled(db, ENV_ONBOARDING_ENABLED, ctx).await
}

/// 开关门：关闭时整块能力**不存在**（404，而非 403）——不向外泄露「平台有这个未开放功能」。
/// 每个端点第一行都调它，**读端点同样调**（读取侧降级：开关关掉后已领的礼包立即读不出、开不了演）。
///
/// 传 `user_id` 使按用户灰度生效（T0 邀请制）；无用户上下文时传 `None` 走全局解析。
async fn ensure_enabled(db: &sqlx::AnyPool, user_id: Option<&str>) -> Result<(), ApiError> {
    if onboarding_enabled(db, user_id).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 测试专用：新手动线相关 env 的 RAII 夹具（开关 + 可选的参数化 env）。
///
/// 这些 env 是**进程级**的，而本模块用例与其它模块同属一个测试二进制、默认并发跑，
/// 故所有对 env 敏感的用例共用**同一把锁**串行化，并在 Drop 时把 env 恢复原状。
/// 范式同 `worlds::DeathmatchSwitch` / `invitations::InvitationSwitch`。
#[cfg(test)]
pub(crate) struct OnboardingSwitch {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Vec<(&'static str, Option<String>)>,
}

#[cfg(test)]
impl OnboardingSwitch {
    pub(crate) fn set(on: bool) -> Self {
        Self::with(on, &[])
    }

    pub(crate) fn with(on: bool, extra: &[(&'static str, &str)]) -> Self {
        Self::raw(Some(if on { "1" } else { "0" }), extra)
    }

    /// 置任意原始值（`None` = **移除该 env**），并可附带其它 env。
    ///
    /// 🔴 `crate::flags::tests` 也用这把夹具（而不是自建一把锁）——`MUSE_ONBOARDING` 是进程级 env，
    /// 两个模块各拿各的锁等于没有锁：flags 的用例会与 onboarding 的用例并发改同一个变量，
    /// 症状是随机失败的「开关值对不上」。**碰同一个 env 的用例必须共用同一把锁。**
    /// 「移除」这个能力是 flags 侧需要的：它要验证「env 未设时默认关闭」这条红线。
    pub(crate) fn raw(value: Option<&str>, extra: &[(&'static str, &str)]) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut prev: Vec<(&'static str, Option<String>)> =
            vec![(ENV_ONBOARDING_ENABLED, std::env::var(ENV_ONBOARDING_ENABLED).ok())];
        match value {
            Some(v) => std::env::set_var(ENV_ONBOARDING_ENABLED, v),
            None => std::env::remove_var(ENV_ONBOARDING_ENABLED),
        }
        for (k, v) in extra {
            prev.push((k, std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        Self { _guard: guard, prev }
    }
}

#[cfg(test)]
impl Drop for OnboardingSwitch {
    fn drop(&mut self) {
        for (k, v) in &self.prev {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

// ---------------- 路由 ----------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/onboarding/presets", get(list_presets))
        .route("/me/onboarding", get(my_onboarding))
        .route("/me/onboarding/gift", post(claim_gift))
        .route("/me/onboarding/microworld/start", post(start_microworld))
}

// ---------------- 登记行 ----------------

/// `onboarding_grants` 一行的投影。
#[derive(Debug, Clone)]
struct Grant {
    preset_id: String,
    cloud_character_id: String,
    world_id: String,
    created_at: i64,
}

async fn load_grant(db: &AnyPool, user_id: &str) -> Result<Option<Grant>, ApiError> {
    let Some(row) = sqlx::query(
        "SELECT preset_id, cloud_character_id, world_id, created_at FROM onboarding_grants WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    Ok(Some(Grant {
        preset_id: row.try_get("preset_id")?,
        cloud_character_id: row.try_get("cloud_character_id")?,
        world_id: row.try_get("world_id")?,
        created_at: row.try_get("created_at")?,
    }))
}

/// 领取回执（**纯函数**：只由登记行决定）。
///
/// 首次领取与任意次重放走的都是这一个构造器，于是「重复领取返回逐字节相同的响应」是结构性成立的，
/// 而不是靠两处代码手工对齐字段。
fn grant_response(g: &Grant) -> Value {
    json!({
        "presetId": g.preset_id,
        "cloudCharacterId": g.cloud_character_id,
        "worldId": g.world_id,
        "claimedAt": g.created_at,
        "microworld": {
            "templateId": microworld::MICROWORLD_TEMPLATE_ID,
            "title": microworld::MICROWORLD_TITLE,
            "starRating": microworld::MICROWORLD_STAR,
            // 庇护档：新手教学场死亡不可能（§11 三档里最保守的一档），故 join 无需任何契约签署。
            "lethality": LETHALITY_SANCTUARY,
        },
        // 🔴 下一步**必须**走既有 join：礼包不写 world_members（见模块头 ②）。
        "next": [
            { "step": "join", "method": "POST", "path": format!("/api/worlds/{}/join", g.world_id) },
            { "step": "start", "method": "POST", "path": "/api/me/onboarding/microworld/start" },
        ],
        "aiLabel": { "visible": true },
    })
}

// ---------------- GET /onboarding/presets ----------------

/// 预制卡库列表（选卡页）。**不下发卡全文**：卡内容在领取时才落库，列表只给 id / 名字 / 一句话卖点，
/// 免得把整张 DNA 卡当公开内容外泄（也顺带压住响应体积）。
async fn list_presets(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;
    let items: Vec<Value> = presets::PRESETS
        .iter()
        .map(|p| {
            json!({
                "presetId": p.id,
                "name": p.name,
                "tagline": p.tagline,
                "isDefault": p.id == presets::DEFAULT_PRESET_ID,
                "aiLabel": { "visible": true },
            })
        })
        .collect();
    Ok(Json(json!({ "presets": items, "defaultPresetId": presets::DEFAULT_PRESET_ID })))
}

// ---------------- POST /me/onboarding/gift ----------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ClaimReq {
    /// 想要的预制卡 id；缺省 → `presets::DEFAULT_PRESET_ID`。未知 id → 400
    /// （绝不静默发一张别的卡：新手看到的第一张卡不该是「系统随便给的」）。
    #[serde(default)]
    preset_id: Option<String>,
}

/// 领取新人大礼包：**发一张预制精品卡** + **建一个属于你的单人微本世界**。
///
/// 幂等两层（缺一不可，见模块头 ③）：
/// - `Idempotency-Key`（可选）→ 同 key 同载荷返回缓存响应，覆盖同一次点击的网络重试；
/// - `onboarding_grants.user_id` 主键 → 覆盖「换个 key 再点」「并发双击」，是真正的「每人一次」。
///
/// 卡位判断（`users.card_slots`，迁移 0019）：预制卡**占卡位**。
/// 理由是产品语义——卡位是「你能同时养几个角色」的产品约束，卡是养成容器（羁绊/记忆/背包/履历
/// 都挂在卡上）。礼包卡拿到手后与自建卡完全同权（能进任何世界、能吃历练、能被撤回释放卡位），
/// 若不占位就等于开了一条「白得一个养成容器」的侧路，卡位这个约束当场失效；
/// 且新用户默认 3 位、0 张卡，礼包必然装得下，占位对目标用户零摩擦。
/// 卡位已满 → 409（文案与 `assets::publish` 的卡位拒绝同口径），撤回旧卡即可释放。
async fn claim_gift(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<ClaimReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;

    let preset = presets::find(body.preset_id.as_deref()).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "没有这张预制卡「{}」。可选的卡见 GET /api/onboarding/presets",
            body.preset_id.as_deref().unwrap_or("")
        ))
    })?;

    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&body).unwrap_or_default());
    let guard =
        idempotency::guard(&state.db, &user.user_id, "onboarding.gift", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    // 快路径：已领过 → 直接回既有回执（重复领取是**幂等成功**，不是错误——新手重进页面点两下
    // 不该看到红字）。真正的唯一性由下方事务里的主键保证，这里只是省掉一次无谓的建房。
    if let Some(g) = load_grant(&state.db, &user.user_id).await? {
        let resp = grant_response(&g);
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    // 卡位门（放在「已领过」之后：已领过的人即便后来把卡位填满，也照样读得到自己的回执）。
    let active = crate::progression::count_active_cards(&state.db, &user.user_id).await?;
    let slots = crate::progression::card_slots_of(&state.db, &user.user_id).await?;
    if active >= slots {
        return Err(ApiError::Conflict(format!(
            "卡位已满（{active}/{slots}），新人礼包的预制卡同样占一个卡位。\
             撤回一张不再用的角色卡，或通过历练解锁更多卡位后再来领取"
        )));
    }

    // 模板 ensure 放在事务外：它自身幂等（且绝大多数调用是一次纯读），没必要把这段 IO 圈进
    // 领取事务里拉长持锁时间。
    let template_version = microworld::ensure_template(&state.db).await?;

    let cloud_character_id = new_id("cchar");
    let card = preset.card_for(&cloud_character_id);
    let card_json = serde_json::to_string(&card).map_err(ApiError::internal)?;
    let now = now_ms();

    let mut tx = state.db.begin().await?;

    // 1) 预制卡落库。
    //
    // 🔴 `moderation='approved'`：预制卡是官方产物、不走用户发布审核（`assets::publish` 的机审路径
    //    只管用户上传内容），但**库里的状态必须对**——`worlds::join_world` 的资格门只认这一列，
    //    留 'pending' 会让新人第一步就撞 `character_not_approved`。
    //    「官方内容因此绕过安全检查」由 `tests::preset_cards_are_injection_clean` 兜底：
    //    卡库全文过 `safety::detect_injection`，塞进注入片段当场红。
    // 🔴 `source_fingerprint = NULL` + `pristine = 0`：同源唯一的两道保险，见模块头 ⑤。
    //    这里显式写死而不是调 `assets::source_identity` 推导——预制卡的这两个值是**设计决定**，
    //    不是从卡内容里算出来的，写死才拦得住「有人给预制卡加了 sourceWork」这类后续改动。
    // 🔴 `mileage` 不出现在本 INSERT：历练唯一写入路径是 `progression::grant_mileage_tx`，
    //    新卡按列默认从 0 起（迁移 0019）。
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, source_fingerprint, pristine, created_at) \
         VALUES (?, ?, ?, 1, ?, 'original', 'approved', 0, NULL, 0, ?)",
    )
    .bind(&cloud_character_id)
    .bind(&user.user_id)
    // local_card_id 用 preset id：这张卡不来自任何本地卡，用预制卡 id 占位使
    // `assets::publish` 的「按 owner+localCardId 递增版本号」对同一张预制卡自然连续。
    .bind(preset.id)
    .bind(&card_json)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    // 2) 建微本世界（一人一实例，见模块头 ⑤）。
    let params = CreateWorldParams {
        template_id: microworld::MICROWORLD_TEMPLATE_ID.into(),
        template_version,
        // 🔴 必须 `idle`：`runtime::load_endgame_policy` 的终局策略是**严格门 room_type=='idle'**，
        //    且首 tick 的装配兜底也只覆盖 idle / event 房——换成别的房型，微本既不装配 NPC
        //    （→ 永远 insufficient_members）也永远不会收束。
        room_type: "idle".into(),
        title: microworld::MICROWORLD_TITLE.into(),
        // private：不进大厅列表（`WHERE visibility IN ('official','public')`）。这是**你的**微本，
        // 不是公共内容；host 记本人，于是投放前也看得到世界详情（`world_detail` 的私有房门）。
        visibility: "private".into(),
        host_user_id: Some(user.user_id.clone()),
        // 单人微本：一个位置，物理上不会有第二个玩家挤进来。
        member_limit: 1,
        tick_per_day: microworld::tick_per_day(),
        // 非零熔断上限（B-2：0 会被 runtime 当作「无上限」）。
        daily_token_budget: microworld::daily_token_budget(),
        daily_cny_budget_cents: microworld::daily_cny_budget_cents(),
        // 'open'：**先投放，后开演**。若在此直接 running，调度器可能在玩家 join 之前就跑首 tick，
        // 而首 tick 会把装配结果 CAS 钉死（`assembly::assemble_instance`）——阵容为空的装配
        // 意味着新人拿不到任何 per-character 钩子，「这个世界认得我这张卡」的第一印象直接没了。
        status: Some("open".into()),
        timeline_mode: "interval".into(),
        // 🔴 庇护档（§11【拍板 24】最保守的一档）：教学场里死亡不可能，引擎在写作前把致死行动
        //    降级为重伤/退场。新手第一局绝不该死人；也因此 join 不需要任何契约签署与年龄门。
        lethality: LETHALITY_SANCTUARY.into(),
        engine_version: None,
        prompt_set_version: None,
        model_route_version: None,
        assembled_json: None,
        initial_state_json: None,
    };
    let world_id = create_world_tx(&mut tx, params).await?;

    // 2.5) 新人礼包的「1 张低星副本卡」（§13）。副本卡资产层（迁移 0032）落地后补上。
    //
    //    🔴 **副本卡有自己的开关**（`MUSE_SUBPLOT_CARDS`，默认关闭）：关闭时**跳过发卡而非报错**——
    //    新手动线不该因为另一块未开放的能力而整体失败，玩家仍应拿到预制卡与微本。
    //    这是两个正交开关的组合：本模块开着、副本卡关着 → 礼包少一张卡，其余照常。
    //
    //    幂等是双层的：`subplot_cards(owner_id, grant_key)` 唯一约束挡重放，
    //    外层 `onboarding_grants.user_id` 主键挡整个礼包重领。两层都撞不穿才算安全。
    // 返回值刻意不用：副本卡是否附赠**不进礼包回执**。
    // 回执由纯函数 `grant_response` 构造，且被「首次领取」与「重复领取读回」两条路径共用——
    // 若把发卡结果塞进去，重复领取那条路径就得再查一次库才能拼出同样的回执，
    // 幂等回执"逐字节相同"这条性质会被打破。玩家在 `GET /me/subplot-cards` 看得到卡，够了。
    let _starter_card_id = if crate::subplot::subplot_cards_enabled() {
        crate::subplot::grant_card_tx(
            &mut tx,
            &crate::subplot::NewSubplotCard {
                owner_id: &user.user_id,
                star_rating: STARTER_SUBPLOT_CARD_STAR,
                label: STARTER_SUBPLOT_CARD_LABEL,
                origin_kind: crate::subplot::ORIGIN_GRANT,
                grant_key: format!("starter:{}", user.user_id),
                source_world_id: None,
                source_template_id: None,
                source_template_version: None,
                synthesized_from: Vec::new(),
            },
        )
        .await?
    } else {
        None
    };

    // 3) 登记行**最后写**：撞主键 → 整个事务回滚（卡与世界一起消失，无残留）→ 读回既有登记行。
    //    这就是「每人只领一次」的唯一权威，不依赖上面那条快路径的读-判-写。
    let res = sqlx::query(
        "INSERT INTO onboarding_grants (user_id, preset_id, cloud_character_id, world_id, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&user.user_id)
    .bind(preset.id)
    .bind(&cloud_character_id)
    .bind(&world_id)
    .bind(now)
    .execute(&mut *tx)
    .await;

    match res {
        Ok(_) => {
            tx.commit().await?;
        }
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            tx.rollback().await?;
            let g = load_grant(&state.db, &user.user_id)
                .await?
                .ok_or_else(|| ApiError::Conflict("新人礼包正在发放中，请稍后重试".into()))?;
            let resp = grant_response(&g);
            guard.store_response(&state.db, &resp.to_string()).await?;
            return Ok(Json(resp));
        }
        Err(e) => {
            tx.rollback().await?;
            return Err(e.into());
        }
    }

    let g = Grant {
        preset_id: preset.id.to_string(),
        cloud_character_id,
        world_id,
        created_at: now,
    };
    let resp = grant_response(&g);
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ---------------- GET /me/onboarding ----------------

/// 我的新手动线状态：领没领 / 卡投放没投放 / 微本跑到哪一步 / 下一步该干什么。
///
/// 这个端点同时是 T0 门槛（「10 分钟内完成首个微本 ≥70%」）的客户端侧读口径：
/// `claimedAt` 是计时起点，`world.status == "ended"` 是完成判据。
async fn my_onboarding(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;

    let Some(g) = load_grant(&state.db, &user.user_id).await? else {
        return Ok(Json(json!({
            "claimed": false,
            "next": [{ "step": "claim", "method": "POST", "path": "/api/me/onboarding/gift" }],
        })));
    };

    // 世界状态 + 本人是否已把卡投放进去（只读 world_members，绝不写，见模块头 ②）。
    let world_status: Option<String> =
        sqlx::query_scalar("SELECT status FROM worlds WHERE id = ?").bind(&g.world_id).fetch_optional(&state.db).await?;
    let joined: bool = sqlx::query(
        "SELECT 1 AS x FROM world_members WHERE world_id = ? AND cloud_character_id = ? AND status = 'active' LIMIT 1",
    )
    .bind(&g.world_id)
    .bind(&g.cloud_character_id)
    .fetch_optional(&state.db)
    .await?
    .is_some();
    // 已跑过的拍数（观演进度；world_ticks 是唯一事实源）。
    let ticks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_ticks WHERE world_id = ? AND status = 'done'")
        .bind(&g.world_id)
        .fetch_one(&state.db)
        .await?;

    let status = world_status.as_deref().unwrap_or("missing");
    let next = match (joined, status) {
        (false, _) => json!([{ "step": "join", "method": "POST", "path": format!("/api/worlds/{}/join", g.world_id) }]),
        (true, "open") => {
            json!([{ "step": "start", "method": "POST", "path": "/api/me/onboarding/microworld/start" }])
        }
        (true, "ended") => json!([{ "step": "graduate", "method": "GET", "path": "/api/worlds?type=idle" }]),
        // running / paused：观演中，看事件流即可。
        _ => json!([{ "step": "watch", "method": "GET", "path": format!("/api/worlds/{}/events", g.world_id) }]),
    };

    let mut out = grant_response(&g);
    out["claimed"] = json!(true);
    out["joined"] = json!(joined);
    out["ticksDone"] = json!(ticks);
    out["world"] = json!({
        "id": g.world_id,
        "status": status,
        // 微本的收束保证：跑到这个拍数必然终局（见 microworld::max_world_ticks）。
        "maxWorldTicks": microworld::max_world_ticks(),
    });
    out["next"] = next;
    Ok(Json(out))
}

// ---------------- POST /me/onboarding/microworld/start ----------------

/// 开演：把自己的微本世界从 `open` 推到 `running`，调度器随即开始排拍。
///
/// 为什么开演是**独立一步**而不是领取时就 running：装配（`assembly::assemble_instance`）在首 tick
/// 触发并被 CAS 钉死，阵容快照 = 那一刻的在场成员。玩家还没 join 就开跑 → 钉住一个空阵容 →
/// 没有 per-character 钩子、没有主场标注，新手看到的是一个与自己无关的世界。
/// 故本端点**要求已投放**（未投放 → 409 并指回 join）。
///
/// 幂等：`WHERE status='open'` 守卫使重复开演的第二次 `rows_affected=0`；已 running → 原样成功返回，
/// 已 ended → 409（世界演完了，不能倒回去重开——公共事实不可回滚，§0.3）。
async fn start_microworld(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;

    let g = load_grant(&state.db, &user.user_id)
        .await?
        .ok_or_else(|| ApiError::Conflict("还没有领取新人礼包，请先领取再开演".into()))?;

    let joined: bool = sqlx::query(
        "SELECT 1 AS x FROM world_members WHERE world_id = ? AND cloud_character_id = ? \
         AND user_id = ? AND status = 'active' LIMIT 1",
    )
    .bind(&g.world_id)
    .bind(&g.cloud_character_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?
    .is_some();
    if !joined {
        return Err(ApiError::Conflict(format!(
            "还没有把角色投放进这个微本。请先 POST /api/worlds/{}/join 再来开演",
            g.world_id
        )));
    }

    // 归属守卫写进 SQL（`host_user_id = 本人`）：即便登记行被人篡改指向别人的世界，也开不动它。
    let res = sqlx::query(
        "UPDATE worlds SET status = 'running', updated_at = ? \
         WHERE id = ? AND status = 'open' AND host_user_id = ?",
    )
    .bind(now_ms())
    .bind(&g.world_id)
    .bind(&user.user_id)
    .execute(&state.db)
    .await?;

    if res.rows_affected() == 0 {
        let status: String = sqlx::query_scalar("SELECT status FROM worlds WHERE id = ?")
            .bind(&g.world_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        return match status.as_str() {
            // 已开演：幂等成功。
            "running" => Ok(Json(json!({ "worldId": g.world_id, "status": "running" }))),
            "ended" => Err(ApiError::Conflict("这个微本已经演完了。去大厅挑一个 1-2 星的世界开始下一段吧".into())),
            other => Err(ApiError::Conflict(format!("微本当前状态为 {other}，无法开演"))),
        };
    }

    Ok(Json(json!({
        "worldId": g.world_id,
        "status": "running",
        "next": [{ "step": "watch", "method": "GET", "path": format!("/api/worlds/{}/events", g.world_id) }],
    })))
}
