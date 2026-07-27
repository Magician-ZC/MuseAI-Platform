//! 房间邀请（客户端设计文档 `docs/design/client-ui-design.md` §6 辅助栏「房间邀请」区块）。
//!
//! 房主/成员把某个**角色**请进自己的世界；被邀请者在收件箱里看到、可接受或拒绝。
//!
//! 端点（玩家端，全部 `AuthUser`）：
//! POST   /worlds/{id}/invitations          发出邀请 {targetCharacterId}；Idempotency-Key 可选
//! GET    /worlds/{id}/invitations          我在该世界**发出**的邀请（只出自己发的）
//! GET    /me/invitations?status=pending    我**收到**的邀请（默认只出 pending）
//! POST   /me/invitations/{iid}/respond     接受 / 拒绝 {accept}（幂等）
//!
//! ────────────────────────────────────────────────────────────────────────────
//! 🔴 三条硬约束，改本模块前先读完
//! ────────────────────────────────────────────────────────────────────────────
//!
//! ① **邀请是引导入口，不是特权通道**（真红线 §0.4 未成年保护 + §0.2 资产单一写入路径的同一精神）。
//!    本模块**永不写 `world_members`** —— accepted 只是把「去入场」这个入口点亮，真正入场仍必须
//!    调用既有 `POST /worlds/{id}/join`，于是 join 的全部服务端权威校验一条不少地生效：
//!    角色属本人 / approved / 未撤回 · 人数上限 · 同一世界一人一卡（防自刷） · 同源唯一 ·
//!    星级历练准入 · 生死契约二次签署 · **未成年禁入生死状**。
//!    这条不是"约定"，是**结构性**的：邀请表与成员表之间没有任何写入关系，
//!    源码断言测试 `module_never_writes_world_members` 把它锁死。
//!    因此本模块也刻意**不复制** join 的任何校验去"预判"能否入场——复制即漂移，漂移即侧路。
//!    前门只做"该不该发这条邀请"的骚扰治理与未成年保护，能不能进由 join 说了算。
//!
//! ② **社交防火墙**（总规格 §14【拍板 22】恨隔面具原则）：默认全员角色面具。
//!    邀请全程只用**世界维度 + 角色维度**寻址与展示：被邀请者由 `targetCharacterId` 指定，
//!    邀请人只以其在该世界的角色面具示人。`invitee_user_id` 只在服务端内部定位收件人，
//!    **任何响应体都不下发**；手机号等真人身份信息一律不进本模块。
//!
//! ③ **未验证功能默认关闭**（VALIDATION.md §0.1）：整块能力由运营开关 `MUSE_ROOM_INVITATIONS`
//!    控制，**默认关闭**。开关范式抄 `worlds::deathmatch_enabled`——前门拒绝 + 读取侧降级双保险：
//!    关闭时所有端点 404（功能不存在），且**已存在的邀请也读不出、响应不了**，
//!    再打开则原样恢复（可逆急停阀，不是一次性阉割）。

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::AnyPool;

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;
use crate::notifications::enqueue_notification;
use crate::worlds::effective_lethality;

use muse_engine::narrative::types::Lethality;

#[cfg(test)]
mod tests;

// ---------------- 运营开关（VALIDATION.md §0.1 未验证功能默认关闭） ----------------

/// 房间邀请运营开关环境变量。
const ENV_INVITATIONS_ENABLED: &str = "MUSE_ROOM_INVITATIONS";

/// 房间邀请默认值 = **关闭**。
///
/// 🔴 房间邀请是全新的社交通道（骚扰面 + 未成年社交面），属 VALIDATION.md §2 中
/// T2「小群体」之后才验证的范围；代码合并不等于对用户开放，必须运营显式打开。
const DEFAULT_INVITATIONS_ENABLED: bool = false;

/// 🔴 **编译期钉死默认值的两个事实源**（本常量 + `flags::KNOWN_FLAGS`），范式同
/// `semantic` / `livestage` / `social`：改一处不改另一处直接编不过。
const _: () = assert!(
    crate::flags::declared_default(ENV_INVITATIONS_ENABLED) == DEFAULT_INVITATIONS_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_ROOM_INVITATIONS 的默认值必须与 DEFAULT_INVITATIONS_ENABLED 一致"
);

/// 邀请功能是否已由运营开启。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 已接入运行时开关体系（`crate::flags`，范式抄 `onboarding::onboarding_enabled`）
/// ════════════════════════════════════════════════════════════════════════════
///
/// 本函数曾经只读 env。现在经统一入口解析，回落链：
///
/// ```text
///   runtime_flags(user=<本人>) → runtime_flags(global) → env MUSE_ROOM_INVITATIONS → false
/// ```
///
/// 🔴 **行为零变化**：`runtime_flags` 为空时（迁移 0036 不插种子数据）必然落到 env 分支，
/// 而 `flags::parse_env_bool` 与本函数原实现逐字同构（`1/true/on/yes` 开、`0/false/off/no` 关、
/// 其余回落默认）。本模块既有用例（全在空表上跑）一行不改即为回归保护。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 灰度粒度只取 **user / global，刻意不用 world**
/// ════════════════════════════════════════════════════════════════════════════
///
/// 这一条与 `MUSE_LETHALITY_DEATHMATCH` 的注意事项（「两处口径不同要写清楚」）是同一类坑，
/// 但结论相反：那个开关**能**按世界灰度，这个**不能**——原因是结构性的，不是偏好。
///
/// 邀请有两侧：发件侧（`POST|GET /worlds/{id}/invitations`，路径里**有** world）与
/// 收件侧（`GET /me/invitations`、`POST /me/invitations/{iid}/respond`，**跨世界**，
/// 结构上没有 world 可传）。若允许 world 作用域，运营给世界 W 单独开闸就会得到：
/// 发件侧（world ctx 命中 → 开）能建邀请，收件侧（无 world → 落到 global → 关）读不出也答不了——
/// **一封谁都答不了的邀请**。开关的作用是「开/关一块能力」，不该能把一块能力开成半截。
///
/// 于是四个端点一律传**动作发起人**的 user（发件侧是邀请人，收件侧是被邀请人），
/// 无用户上下文的场合走 global。要整块开就写 global，要灰度就按人写。
///
/// ⚠️ 由此产生的一个**如实边界**：运营只给 A 开、没给 B 开时，A 能发出 B 永远看不到的邀请。
/// 不加防的理由是它已有正确归宿——邀请本就有 TTL（`MUSE_INVITE_TTL_MS`，默认 7 天）会自然过期，
/// 与「邀请了一个再也不登录的人」是同一种结局。反过来若在发件侧校验收件人的开关，
/// 就等于让 A 能探测到「B 有没有被灰度选中」，那是拿运营配置去泄露他人状态。
///
/// 保留 `ENV_INVITATIONS_ENABLED` / `DEFAULT_INVITATIONS_ENABLED` 与下方 RAII 夹具：
/// env 仍是兜底层第 ④ 级，不是被删掉的旧路径。
pub async fn invitations_enabled(db: &AnyPool, user_id: Option<&str>) -> bool {
    let ctx = match user_id {
        Some(u) => crate::flags::FlagCtx::user(u),
        None => crate::flags::FlagCtx::global(),
    };
    crate::flags::is_enabled(db, ENV_INVITATIONS_ENABLED, ctx).await
}

/// 测试专用：邀请功能相关 env 的 RAII 夹具（开关 + 可选的其它参数化 env）。
///
/// 这些 env 是**进程级**的，而本模块用例与其它模块同属一个测试二进制、默认并发跑，
/// 故所有对 env 敏感的用例共用**同一把锁**串行化，并在 Drop 时把 env 恢复原状。
/// 范式同 `worlds::DeathmatchSwitch` / `interventions` 的 QUOTA_ENV_LOCK。
///
/// ⚠️ 与 `worlds::DeathmatchSwitch` 同时使用时，**必须先取本锁再取 DeathmatchSwitch**
/// （固定加锁顺序，避免两把锁交叉持有导致死锁）。
#[cfg(test)]
pub(crate) struct InvitationSwitch {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Vec<(&'static str, Option<String>)>,
}

#[cfg(test)]
impl InvitationSwitch {
    /// 只置总开关。
    pub(crate) fn set(on: bool) -> Self {
        Self::with(on, &[])
    }

    /// 置总开关 + 若干额外 env（如 `MUSE_INVITE_DAILY_LIMIT`）；返回值存活期间取值稳定。
    pub(crate) fn with(on: bool, extra: &[(&'static str, &str)]) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut prev: Vec<(&'static str, Option<String>)> =
            vec![(ENV_INVITATIONS_ENABLED, std::env::var(ENV_INVITATIONS_ENABLED).ok())];
        std::env::set_var(ENV_INVITATIONS_ENABLED, if on { "1" } else { "0" });
        for (k, v) in extra {
            prev.push((k, std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        Self { _guard: guard, prev }
    }
}

#[cfg(test)]
impl Drop for InvitationSwitch {
    fn drop(&mut self) {
        for (k, v) in &self.prev {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// 开关门：关闭时整块能力**不存在**（404，而非 403）——不向外泄露"平台有这个未开放功能"。
/// 每个端点的第一行都调它，读端点同样调（读取侧降级：开关关掉后历史邀请立即读不出、响应不了）。
///
/// `user_id` 传**动作发起人**（发件侧 = 邀请人，收件侧 = 被邀请人），使按人灰度生效；
/// 为什么不传 world 见 [`invitations_enabled`] 的「灰度粒度」一节。
async fn ensure_enabled(db: &AnyPool, user_id: Option<&str>) -> Result<(), ApiError> {
    if invitations_enabled(db, user_id).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

// ---------------- 参数化（VALIDATION.md §0.2 产品规则参数化，禁止写死） ----------------

/// 邀请有效期默认值：7 天。过期即失效（惰性判定），不长期挂在别人收件箱里。
const DEFAULT_INVITE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 每位邀请人每日（滚动 24h）可发出的邀请总数默认上限：20 条。**跨世界合计**——
/// 只按世界限流会被"换个世界继续骚扰同一个人"绕过。
const DEFAULT_INVITE_DAILY_LIMIT: i64 = 20;

/// 日配额的滚动窗口：24h。
const INVITE_QUOTA_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// 邀请有效期（毫秒）；env `MUSE_INVITE_TTL_MS`（正整数）覆盖，非法/缺省回落默认。
fn invite_ttl_ms() -> i64 {
    parse_positive(std::env::var("MUSE_INVITE_TTL_MS").ok().as_deref(), DEFAULT_INVITE_TTL_MS)
}

/// 每人每日邀请上限；env `MUSE_INVITE_DAILY_LIMIT`（正整数）覆盖，非法/缺省回落默认。
fn invite_daily_limit() -> i64 {
    parse_positive(
        std::env::var("MUSE_INVITE_DAILY_LIMIT").ok().as_deref(),
        DEFAULT_INVITE_DAILY_LIMIT,
    )
}

/// 正整数 env 解析（与 env 读取分离，便于无副作用地测试回落规则）。
fn parse_positive(raw: Option<&str>, default: i64) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok()).filter(|v| *v > 0).unwrap_or(default)
}

// ---------------- 状态枚举 ----------------

const STATUS_PENDING: &str = "pending";
const STATUS_ACCEPTED: &str = "accepted";
const STATUS_DECLINED: &str = "declined";
const STATUS_EXPIRED: &str = "expired";

/// 统一的"不能邀请"拒绝文案。
///
/// 🔴 刻意**不区分原因**：未成年保护相关的拒绝（对方未声明成年、被邀请进生死状世界）与
/// 其它资格类拒绝共用同一句话，否则邀请端点会变成"探测任意用户是否未成年"的接口——
/// 那本身就是对被邀请者的隐私泄露（§14 社交防火墙 + 真红线 §0.4）。
const REFUSE_GENERIC: &str = "该角色暂时不能被邀请到这个世界";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worlds/{id}/invitations", post(create_invitation).get(list_sent))
        .route("/me/invitations", get(list_received))
        .route("/me/invitations/{iid}/respond", post(respond))
}

// ---------------- 请求 / 响应类型 ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateInvitationReq {
    /// 被邀请者的**角色 id**（面具寻址，§14）：服务端据此解析收件人，
    /// 邀请人自始至终拿不到对方的真人身份。角色 id 来自世界详情的公开阵容/羁绊图谱等既有面。
    target_character_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RespondReq {
    accept: bool,
}

#[derive(Debug, Deserialize)]
struct StatusQuery {
    #[serde(default)]
    status: Option<String>,
}

// ---------------- 内部工具 ----------------

/// 惰性过期：pending 且已过 TTL → expired。读写路径都先调一次，保证结果反映真实有效性
/// （范式同 `consents::expire_stale_consents`——本仓库无定时清理器，惰性判定是既有口径）。
async fn expire_stale_invitations(db: &AnyPool) -> Result<u64, ApiError> {
    let now = now_ms();
    let res = sqlx::query(
        "UPDATE room_invitations SET status = $1, responded_at = $2 \
         WHERE status = 'pending' AND expires_at <= $3",
    )
    .bind(STATUS_EXPIRED)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

/// 角色展示名（`card_json.identity.name`）——面具身份，唯一对外可见的"人"的标识。
/// 取不到名字（卡结构异常/无名/空 id）时兜底，绝不因取名失败改变任何判定。
///
/// 被处置的卡在这里过 `NameGate`（默认关闭 → 恒等）：邀请两侧看到的都是**对方**的角色名，
/// 与 `social::character_name` 同一条口径。
async fn character_name(db: &AnyPool, character_id: &str) -> String {
    if character_id.trim().is_empty() {
        return "房主".to_string();
    }
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT card_json, moderation FROM cloud_characters WHERE id = $1")
            .bind(character_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    let (card, moderation) = match row {
        Some((c, m)) => (Some(c), Some(m)),
        None => (None, None),
    };
    let name = card
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|v| v.pointer("/identity/name").and_then(|n| n.as_str()).map(str::to_string))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "该角色".to_string());
    let gate = crate::safety::disposal::NameGate::resolve(db, crate::flags::FlagCtx::global()).await;
    gate.display_name(character_id, moderation.as_deref(), name)
}

/// 世界的邀请相关投影：状态 / 房主 / 契约档落库值 / 标题。
struct WorldBrief {
    status: String,
    host_user_id: Option<String>,
    lethality: String,
    title: String,
}

async fn load_world_brief(db: &AnyPool, world_id: &str) -> Result<WorldBrief, ApiError> {
    let row: Option<(String, Option<String>, String, String)> =
        sqlx::query_as("SELECT status, host_user_id, lethality, title FROM worlds WHERE id = $1")
            .bind(world_id)
            .fetch_optional(db)
            .await?;
    let (status, host_user_id, lethality, title) = row.ok_or(ApiError::NotFound)?;
    Ok(WorldBrief { status, host_user_id, lethality, title })
}

/// 未成年门（真红线 §0.4）：生效档为生死状的世界，只有**已声明成年**（age_declared==1）才可参与。
///
/// fail-closed，口径与 `worlds::join_world` 的生死状门逐字一致：未声明(0)、未成年(2)、用户行缺失
/// 一律视为未成年。**生效档**经 `worlds::effective_lethality` 换算（含运营开关降级），
/// 保证"邀请看到的档"和"join 跑的档"同源。
///
/// 用在两处（前门拒绝 + 读取侧复查的双保险，范式同 lethality 本身）：
/// - 发出邀请时：不让未成年收到通往生死状世界的邀请（不制造诱导入口）；
/// - 接受邀请时：世界升档/运营开关中途打开，也不放行。
/// 真正的禁入判定仍在 join，本函数只是把侧路提前堵住，不替代 join。
async fn deathmatch_age_gate_ok(
    db: &AnyPool,
    world_id: &str,
    world: &WorldBrief,
    user_id: &str,
) -> Result<bool, ApiError> {
    let dm = crate::worlds::deathmatch_enabled(db, Some(world_id)).await;
    if effective_lethality(&world.lethality, dm) != Lethality::Deathmatch {
        return Ok(true);
    }
    // 🔴 全仓唯一的「已声明成年」判定（真红线 §0.4）。此前这里手抄了一份，
    // 与另外四处靠巧合保持一致——见 `auth::is_declared_adult` 的说明。
    Ok(crate::auth::is_declared_adult(db, user_id).await)
}

// ---------------- POST /worlds/{id}/invitations ----------------

/// 发出邀请。前门只治理"该不该发"（骚扰 + 未成年保护），**不预判"能不能进"**——那是 join 的事。
async fn create_invitation(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CreateInvitationReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;

    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(
        &serde_json::to_vec(&json!({ "worldId": world_id, "body": body })).unwrap_or_default(),
    );
    let guard =
        idempotency::guard(&state.db, &user.user_id, "invitations.create", idem_key, &payload_hash)
            .await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    expire_stale_invitations(&state.db).await?;

    let target_id = body.target_character_id.trim().to_string();
    if target_id.is_empty() {
        return Err(ApiError::BadRequest("targetCharacterId 必填".into()));
    }

    let world = load_world_brief(&state.db, &world_id).await?;
    if !matches!(world.status.as_str(), "open" | "running") {
        return Err(ApiError::Conflict("world_not_joinable".into()));
    }

    // 邀请人资格：必须是该世界的 active 成员，或世界房主。路人不能拿别人的房当骚扰通道。
    let my_membership: Option<(String,)> = sqlx::query_as(
        "SELECT cloud_character_id FROM world_members \
         WHERE world_id = $1 AND user_id = $2 AND status = 'active'",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let is_host = world.host_user_id.as_deref() == Some(user.user_id.as_str());
    if my_membership.is_none() && !is_host {
        return Err(ApiError::Forbidden);
    }
    // 邀请人的面具：其在本世界的角色；房主未投放角色时留空，展示侧回落「房主」。
    let inviter_character_id = my_membership.map(|(c,)| c).unwrap_or_default();

    // 目标角色（面具寻址）→ 服务端内部解析收件人。
    let target: Option<(String, String, i64)> =
        sqlx::query_as("SELECT owner_id, moderation, withdrawn FROM cloud_characters WHERE id = $1")
            .bind(&target_id)
            .fetch_optional(&state.db)
            .await?;
    let (invitee_user_id, moderation, withdrawn) = target.ok_or(ApiError::NotFound)?;
    if moderation != "approved" || withdrawn != 0 {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    }
    if invitee_user_id == user.user_id {
        return Err(ApiError::Conflict("不能邀请自己的角色".into()));
    }

    // 已在场 → 无须邀请（阵容对本世界成员本就可见，此处不构成新的信息泄露）。
    let already_in: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM world_members WHERE world_id = $1 AND user_id = $2 AND status = 'active'",
    )
    .bind(&world_id)
    .bind(&invitee_user_id)
    .fetch_one(&state.db)
    .await?;
    if already_in > 0 {
        return Err(ApiError::Conflict("对方已经在这个世界里了".into()));
    }

    // 🔴 未成年保护前门：不让未成年收到通往生死状世界的邀请。
    // 拒绝文案统一为 REFUSE_GENERIC —— 不得让邀请人从错误码里读出对方的年龄声明。
    if !deathmatch_age_gate_ok(&state.db, &world_id, &world, &invitee_user_id).await? {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    }

    // 🔴 拉黑前门（`social` 模块，总规格 §14 配套治理）：任一方向拉黑 → 发不出邀请。
    //
    // 为什么邀请也要看拉黑：拉黑的承诺是「对方无法向你发起社交动作」，而房间邀请就是
    // 一条社交动作通道——只挡真人身份解锁、不挡邀请，等于给被拉黑者留了一条继续打扰的路。
    //
    // ⚠️ 刻意**不看** `MUSE_SOCIAL_IDENTITY_UNLOCK` 开关：拉黑是**保护态**，
    // 社交功能被急停/灰度收窄时既有拉黑仍须生效（方向同 `MUSE_SAFETY_LEXICON` 的 fail-safe）。
    // 开关关闭时 `social_blocks` 表为空，本判定恒为 false —— 行为逐字不变、零副作用。
    // 拒绝文案同样是 REFUSE_GENERIC：不得让邀请人从错误码里读出「我被拉黑了」。
    if crate::social::is_blocked_pair(&state.db, &user.user_id, &invitee_user_id).await? {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    }

    // 防骚扰 ①：**拒绝即终局**。同一邀请人被同一角色拒过一次，就不能再把它请进同一个世界。
    // （换角色、换世界、换邀请人仍可——这条治理的是"被拒后反复纠缠"。）
    let declined_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM room_invitations \
         WHERE world_id = $1 AND inviter_user_id = $2 AND invitee_character_id = $3 AND status = 'declined'",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(&target_id)
    .fetch_one(&state.db)
    .await?;
    if declined_before > 0 {
        return Err(ApiError::Conflict("对方已拒绝过这个邀请，不能再次邀请".into()));
    }

    // 防骚扰 ②：同一 (世界, 邀请人, 被邀请角色) 同时至多一条 pending —— 重复邀请**幂等复用**同一条，
    // 不刷新有效期、不再发一次通知，也就无法靠反复调用制造通知轰炸。
    let existing: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT id, expires_at, created_at FROM room_invitations \
         WHERE world_id = $1 AND inviter_user_id = $2 AND invitee_character_id = $3 AND status = 'pending'",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(&target_id)
    .fetch_optional(&state.db)
    .await?;
    if let Some((id, expires_at, created_at)) = existing {
        let resp = json!({
            "id": id,
            "worldId": world_id,
            "worldTitle": world.title,
            "inviteeCharacterId": target_id,
            "inviteeCharacterName": character_name(&state.db, &target_id).await,
            "status": STATUS_PENDING,
            "expiresAt": expires_at,
            "createdAt": created_at,
        });
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    // 防骚扰 ③：每人每日（滚动 24h）发出总量上限，**跨世界合计**。
    let now = now_ms();
    let sent_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM room_invitations WHERE inviter_user_id = $1 AND created_at >= $2",
    )
    .bind(&user.user_id)
    .bind(now - INVITE_QUOTA_WINDOW_MS)
    .fetch_one(&state.db)
    .await?;
    if sent_today >= invite_daily_limit() {
        return Err(ApiError::Conflict("今日邀请次数已达上限，请稍后再试".into()));
    }

    let id = new_id("inv");
    let expires_at = now + invite_ttl_ms();
    sqlx::query(
        "INSERT INTO room_invitations \
         (id, world_id, inviter_user_id, inviter_character_id, invitee_user_id, invitee_character_id, \
          status, expires_at, responded_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, 0, $8)",
    )
    .bind(&id)
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(&inviter_character_id)
    .bind(&invitee_user_id)
    .bind(&target_id)
    .bind(expires_at)
    .bind(now)
    .execute(&state.db)
    .await?;

    // 通知收件人。payload 只含世界维度与角色面具，**不含任何真人身份**（§14）。
    // kind 非 `consent*` 前缀 → 属可退订/可静默类（notifications::is_essential_kind），
    // 即用户可用既有通知偏好把这类打扰关掉，是防骚扰的第四道闸。
    let dedupe = format!("room_invitation:{id}");
    enqueue_notification(
        &state,
        &invitee_user_id,
        "room_invitation",
        json!({
            "invitationId": id,
            "worldId": world_id,
            "worldTitle": world.title,
            "inviterCharacterName": character_name(&state.db, &inviter_character_id).await,
        }),
        Some(&dedupe),
        now,
    )
    .await?;

    let resp = json!({
        "id": id,
        "worldId": world_id,
        "worldTitle": world.title,
        "inviteeCharacterId": target_id,
        "inviteeCharacterName": character_name(&state.db, &target_id).await,
        "status": STATUS_PENDING,
        "expiresAt": expires_at,
        "createdAt": now,
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ---------------- GET /worlds/{id}/invitations ----------------

/// 我在该世界发出的邀请（发件箱）。**只出自己发的**——不得看见同世界其他人发的邀请。
async fn list_sent(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;
    expire_stale_invitations(&state.db).await?;

    let filter = q.status.unwrap_or_else(|| STATUS_PENDING.to_string());
    let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, invitee_character_id, status, expires_at, created_at FROM room_invitations \
         WHERE world_id = $1 AND inviter_user_id = $2 AND ($3 = 'all' OR status = $4) \
         ORDER BY created_at DESC, id DESC LIMIT 100",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(&filter)
    .bind(&filter)
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::new();
    for (id, invitee_character_id, status, expires_at, created_at) in rows {
        out.push(json!({
            "id": id,
            "worldId": world_id,
            // 只回角色面具，不回 invitee_user_id（§14）。
            "inviteeCharacterId": invitee_character_id,
            "inviteeCharacterName": character_name(&state.db, &invitee_character_id).await,
            "status": status,
            "expiresAt": expires_at,
            "createdAt": created_at,
        }));
    }
    Ok(Json(json!({ "invitations": out })))
}

// ---------------- GET /me/invitations ----------------

/// 我收到的邀请（收件箱，前端辅助栏「房间邀请」区块的数据源）。
async fn list_received(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<StatusQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;
    expire_stale_invitations(&state.db).await?;

    let filter = q.status.unwrap_or_else(|| STATUS_PENDING.to_string());
    let rows: Vec<(String, String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT i.id, i.world_id, w.title, i.inviter_character_id, i.invitee_character_id, \
                i.expires_at, i.created_at \
         FROM room_invitations i JOIN worlds w ON w.id = i.world_id \
         WHERE i.invitee_user_id = $1 AND ($2 = 'all' OR i.status = $3) \
         ORDER BY i.created_at DESC, i.id DESC LIMIT 100",
    )
    .bind(&user.user_id)
    .bind(&filter)
    .bind(&filter)
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::new();
    for (id, world_id, world_title, inviter_character_id, invitee_character_id, expires_at, created_at) in rows {
        out.push(json!({
            "id": id,
            "worldId": world_id,
            "worldTitle": world_title,
            // 🔴 只给角色面具名（§14）：既不给 inviter_user_id，也不给昵称/手机号。
            "inviterCharacterName": character_name(&state.db, &inviter_character_id).await,
            "myCharacterId": invitee_character_id,
            "myCharacterName": character_name(&state.db, &invitee_character_id).await,
            "expiresAt": expires_at,
            "createdAt": created_at,
        }));
    }
    Ok(Json(json!({ "invitations": out })))
}

// ---------------- POST /me/invitations/{iid}/respond ----------------

/// 接受 / 拒绝邀请（幂等）。
///
/// 🔴 接受**不入场**：只把邀请置 accepted 并回一个 `next` 指引，玩家仍需自行调用
/// `POST /worlds/{id}/join`（带该世界要求的全部入场参数，如生死状二次确认）。
/// 本函数不写 `world_members`，因此 join 的每一条校验都仍然生效。
async fn respond(
    State(state): State<AppState>,
    user: AuthUser,
    Path(invitation_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RespondReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id)).await?;

    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(
        &serde_json::to_vec(&json!({ "id": invitation_id, "body": body })).unwrap_or_default(),
    );
    let guard = idempotency::guard(
        &state.db,
        &user.user_id,
        "invitations.respond",
        idem_key,
        &payload_hash,
    )
    .await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    expire_stale_invitations(&state.db).await?;

    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT world_id, invitee_user_id, invitee_character_id, status FROM room_invitations WHERE id = $1",
    )
    .bind(&invitation_id)
    .fetch_optional(&state.db)
    .await?;
    let (world_id, invitee_user_id, invitee_character_id, status) = row.ok_or(ApiError::NotFound)?;
    // 非收件人一律 404（而非 403）：既挡越权，也不泄露"这条邀请存在"。
    if invitee_user_id != user.user_id {
        return Err(ApiError::NotFound);
    }
    if status != STATUS_PENDING {
        // 已解决 → 幂等返回当前状态，不改写终局。
        let resp = json!({ "id": invitation_id, "worldId": world_id, "status": status });
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    let new_status = if body.accept {
        let world = load_world_brief(&state.db, &world_id).await?;
        if !matches!(world.status.as_str(), "open" | "running") {
            return Err(ApiError::Conflict("world_not_joinable".into()));
        }
        // 🔴 未成年保护读取侧复查（双保险）：世界中途升档、或运营把生死状开关打开时，
        // 已发出的邀请也不得成为侧路。403 = 永久禁入（口径同 join 的生死状红线分支）。
        if !deathmatch_age_gate_ok(&state.db, &world_id, &world, &user.user_id).await? {
            return Err(ApiError::Forbidden);
        }
        STATUS_ACCEPTED
    } else {
        STATUS_DECLINED
    };

    let now = now_ms();
    // 条件 UPDATE（CAS）：并发下只有一次能把 pending 推进，避免读改写丢更新。
    let res = sqlx::query(
        "UPDATE room_invitations SET status = $1, responded_at = $2 WHERE id = $3 AND status = 'pending'",
    )
    .bind(new_status)
    .bind(now)
    .bind(&invitation_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        // 被并发抢先解决 → 回读权威状态，幂等返回。
        let cur: String =
            sqlx::query_scalar("SELECT status FROM room_invitations WHERE id = $1")
                .bind(&invitation_id)
                .fetch_one(&state.db)
                .await?;
        let resp = json!({ "id": invitation_id, "worldId": world_id, "status": cur });
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    let mut resp = json!({
        "id": invitation_id,
        "worldId": world_id,
        "status": new_status,
    });
    if new_status == STATUS_ACCEPTED {
        // 明示：接受 ≠ 入场。入场仍走既有 join，其全部校验（同源唯一 / 防自刷 / 星级准入 /
        // 生死契约签署 / 未成年门 / 人数上限）一条不少。
        resp["next"] = json!({
            "method": "POST",
            "path": format!("/api/worlds/{world_id}/join"),
            "suggestedCloudCharacterId": invitee_character_id,
            "note": "接受邀请只是引导入口；入场仍需调用 join，并通过该世界的全部入场校验",
        });
    }
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}
