//! 真人社交解锁（R3；总规格 `docs/build/spec-world-ecosystem.md` §14【拍板 22】恨隔面具原则）。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 规格原文（本模块的全部依据）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! - 默认全员**角色面具**（匿名，以角色身份互动）。
//! - **仅正向羁绊线**（共历生死/结盟/救命）达阈值后**双向自愿**解锁真人身份。
//! - **敌对线永久匿名**——背叛与仇杀不暴露玩家真身，冲突外溢的网暴通道结构性焊死。
//! - 配套：拉黑 / 举报 / **青少年模式限真人社交**。独有社交资产：「我们的角色一起死过」。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 端点（全部 `AuthUser`；admin 面另标）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ```text
//! GET    /worlds/{id}/social/bonds                  我在该世界的社交对端（面具视图）+ 资格 + 凭证 + 状态
//! POST   /worlds/{id}/social/unlock-requests        发起解锁 {targetCharacterId}；Idempotency-Key 可选
//! GET    /me/social/unlock-requests?status=         我**收到**的解锁请求（默认 pending）
//! POST   /me/social/unlock-requests/{id}/respond    接受 / 拒绝 {accept}
//! GET    /me/social/identities                      已双向解锁的真人身份（🔴 全平台唯一下发真身的读路径）
//! GET    /me/social/blocks                          我的黑名单
//! POST   /me/social/blocks                          拉黑 {characterId, reason?}
//! DELETE /me/social/blocks/{id}                     解除拉黑
//! POST   /me/social/reports                         举报 {subjectKind, subjectId, category, detail?, worldId?}
//! GET    /admin/social/reports?status=&cursor=      举报队列（reviewer/support 档）
//! POST   /admin/social/reports/{id}/resolve         处置 {action: actioned|dismissed, reason}
//! ```
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 五条硬约束，改本模块前先读完
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ① **面具是默认，真身是例外**（§14）。除 `GET /me/social/identities` 一处外，
//!    本模块所有响应体只出现 `characterId` / `characterName`（角色面具），
//!    **不出现 `userId`、昵称、手机号**。收件人由服务端用 `target_user_id` 内部定位。
//!    连拒绝文案都统一成一句 `REFUSE_GENERIC`——区分原因就等于把端点变成
//!    「探测对方是不是未成年 / 是不是拉黑了我」的接口，那本身就是对被查询者的信息泄露。
//!    唯一的例外面 `/me/social/identities` 也**只给 userId + 昵称**：手机号是强 PII，
//!    平台内的"认识"不需要它，给了就再也收不回来。
//!
//! ② **敌对线一票否决，且是永久的**（§14「敌对线永久匿名」）。
//!    任一方向的关系达敌对判据（`trust`/`affinity` 跌破负阈值，或 `fear` 超正阈值）→
//!    资格判定直接 false，**不看正向分、不看凭证、不看任何补偿路径**。
//!    这条挡的是「先结盟刷够羁绊、再翻脸拿真身去线下报复」这条最危险的路径。
//!    判定按**当下**的世界线状态实时算——发起时算一次，接受时**再算一次**
//!    （世界会继续跑，昨天的盟友今天可能已是宿敌；只认发起时的快照等于给翻脸留后门）。
//!
//! ③ 🔴 **青少年模式限真人社交是服务端拒绝，不是前端隐藏**（真红线 §0.4 未成年保护）。
//!    `ensure_adult_social` 挂在**每一个身份相关端点的第一行**（`ensure_enabled` 之后、
//!    任何读写之前），fail-closed：只有 `users.age_declared == 1`（已声明成年）放行，
//!    未声明(0)、未成年(2)、用户行缺失一律拒绝，口径与 `worlds::join_world` 的生死状门逐字一致。
//!    **对端同样要过这道门**：即使发起人成年，目标是未成年也一律拒绝（不制造通往未成年的社交入口），
//!    且拒绝文案与其它拒绝无差别（见 ①）。
//!    红线用例 `red_line_minor_rejected_with_zero_side_effect` 钉死「拒绝 + 零副作用」。
//!    ⚠️ **拉黑与举报不设年龄门**：它们是保护工具，不是社交特权。把未成年的举报/拉黑能力
//!    一并关掉，是把"保护未成年"做成了"让未成年无法自保"。
//!
//! ④ 🔴 **「我们的角色一起死过」是关系凭证，不是数值**（平台红线①「不卖胜负与数值平权」）。
//!    它**没有自己的存储**：由既有的 `cloud_characters.memorial_status/memorial_world_id`
//!    与 `world_members` **只读派生**，算完即抛。因此它在结构上不可能被当成可累积的进度——
//!    没有列，就没有人能给它加一。它的**全部**作用是：作为一条合格的正向羁绊线，
//!    让「双向自愿解锁真身」这件事有资格发生。它不发历练、不发道具、不开卡位、不进结算、
//!    不进引擎决策。两条红线用例守死：源码级 `red_line_module_writes_only_social_tables`
//!    （本模块的写入目标只能是三张 social 表 + 风控/审计留痕表）、
//!    运行时级 `red_line_social_asset_has_zero_numeric_effect`（走完全套社交流程后，
//!    资产/进度/世界线九张表逐字节快照相等）。
//!
//! ⑤ **未验证功能默认关闭**（VALIDATION.md §0.1）。整块能力由运行时开关
//!    `MUSE_SOCIAL_IDENTITY_UNLOCK` 控制，**默认关闭**，经 `crate::flags` 解析
//!    （链：用户 > 世界 > 全局 > env > 代码内默认值）。关闭时**全部端点 404 且零副作用**。
//!    唯一的例外方向见下：拉黑是**保护态**，关阀不会让既有拉黑失效
//!    （`is_blocked_pair` 供其它社交通道调用时不看开关——方向与 `MUSE_SAFETY_LEXICON`
//!    的 fail-safe 一致：「安全」永远指向不扩大可达范围的那一侧，不是字面的关）。

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;
use crate::notifications::enqueue_notification;

#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 运营开关（VALIDATION.md §0.1 未验证功能默认关闭）
// ═══════════════════════════════════════════════════════════════════════════

/// 真人社交解锁运行时开关（**开关名即 env 变量名**，见 `flags` 模块头）。
pub(crate) const ENV_SOCIAL_IDENTITY_UNLOCK: &str = "MUSE_SOCIAL_IDENTITY_UNLOCK";

/// 默认 = **关闭**。
///
/// 🔴 真人社交是全平台**不可逆性最强**的一次授予：身份一旦互相看见，关掉开关也收不回
/// 「他知道我是谁」这个既成事实（§0.3 公共事实不可回滚的社交版本）。它同时新增了
/// 骚扰面、网暴外溢面与未成年接触面，属 VALIDATION §2 中 **T4+** 才验证的范围。
/// 代码合并不等于对用户开放。
const DEFAULT_SOCIAL_ENABLED: bool = false;

/// 🔴 **编译期钉死**：默认值出现在两处（本常量 + `flags::KNOWN_FLAGS` 登记表），
/// 两处不一致就是「默认关闭」这条 §0.1 约束有了两个事实源。改一处不改另一处直接编不过。
/// 范式抄 `annotations` / `onboarding`。
const _: () = assert!(
    crate::flags::declared_default(ENV_SOCIAL_IDENTITY_UNLOCK) == DEFAULT_SOCIAL_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_SOCIAL_IDENTITY_UNLOCK 的默认值必须与 DEFAULT_SOCIAL_ENABLED 一致"
);

/// 本模块是否已由运营开启。
///
/// 解析上下文按端点分两档：
///
/// | 端点 | ctx | 理由 |
/// |---|---|---|
/// | `/worlds/{id}/social/**` | user + world | 社交对象来自某个世界，按世界灰度最自然 |
/// | `/me/social/**`          | user（无 world）| 收件箱/黑名单/举报跨世界，没有单一 world 坐标 |
///
/// ⚠️ 由此产生一条**运营须知**（与 `annotations` 同型）：若只按 world 作用域灰度，
/// 玩家能在该世界发起解锁却读不到 `/me/social/unlock-requests` 收件箱。
/// **推荐的灰度作用域是 user 或 global**，world 作用域只用于「临时关掉某个出问题世界的社交入口」
/// 这种收窄动作。
///
/// 🔴 fail-closed 由 `flags::is_enabled` 自带：查库失败 / 记录损坏 → 返回声明的默认值（关），
/// 且**不再回落 env**。
pub(crate) async fn social_enabled(
    db: &AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
) -> bool {
    let mut ctx = crate::flags::FlagCtx::global();
    if let Some(u) = user_id {
        ctx = ctx.with_user(u);
    }
    if let Some(w) = world_id {
        ctx = ctx.with_world(w);
    }
    crate::flags::is_enabled(db, ENV_SOCIAL_IDENTITY_UNLOCK, ctx).await
}

/// 开关门：关闭时整块能力**不存在**（404 而非 403）——不向外泄露「平台有这个未开放功能」。
/// 每个端点的第一行都调它，读端点同样调（读取侧降级：关阀后既有解锁身份也读不出，重开即恢复）。
async fn ensure_enabled(
    db: &AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
) -> Result<(), ApiError> {
    if social_enabled(db, user_id, world_id).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 入口**是否曾经对任何人开放过**（运营面的可见性判据，范式抄 `annotations::entry_ever_open`）。
///
/// 刻意不用全局解析：若运营按世界灰度开了 3 个世界（global 仍为关），那 3 个世界里产生的举报
/// 会真实落库，而按全局解析会把举报队列判成 404——**举报进得来、处置进不去**，队列直接卡死。
/// 举报处置是本功能的闭环环节，它的可见性必须跟「有没有人能举报」一致。
///
/// **fail-safe 方向是 false**（查库失败 → 按「没开过」处理）。
async fn entry_ever_open(db: &AnyPool) -> bool {
    if social_enabled(db, None, None).await {
        return true;
    }
    let n = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM runtime_flags WHERE flag = ? AND enabled = 1",
    )
    .bind(ENV_SOCIAL_IDENTITY_UNLOCK)
    .fetch_one(db)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("n").ok())
    .unwrap_or(0);
    n > 0
}

/// 运营面开关门：入口曾对任何人开放过即放行，否则 404。
async fn ensure_ops_enabled(db: &AnyPool) -> Result<(), ApiError> {
    if entry_ever_open(db).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 未成年门（真红线 §0.4）——服务端拒绝，不是前端隐藏
// ═══════════════════════════════════════════════════════════════════════════

/// 「已声明成年」的落库值（`users.age_declared`：0 未声明 / 1 成年 / 2 未成年）。
const AGE_DECLARED_ADULT: i64 = 1;

/// 该用户是否**已声明成年**。fail-closed：未声明(0)、未成年(2)、用户行缺失一律 false。
///
/// 口径与 `worlds::join_world` 的生死状门、`invitations::deathmatch_age_gate_ok` **逐字一致**——
/// 三处口径必须同源，否则「未成年保护」会有三种不同的含义，其中至少两种是错的。
async fn is_adult(db: &AnyPool, user_id: &str) -> Result<bool, ApiError> {
    let age: Option<(i64,)> = sqlx::query_as("SELECT age_declared FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    Ok(matches!(age, Some((AGE_DECLARED_ADULT,))))
}

/// 🔴 **青少年模式限真人社交的唯一落点**：调用者本人必须已声明成年，否则 403。
///
/// 挂在每个**身份相关**端点的第一行（`ensure_enabled` 之后、任何读写之前），
/// 因此拒绝发生在**零副作用**的位置上：没有落库、没有配额消耗、没有幂等键写入、没有通知。
///
/// 用 403（Forbidden）而非 404：功能确实存在，只是这个账号不被允许——与
/// `invitations` 的生死状未成年门同码，客户端可据 `forbidden` 展示「青少年模式下不开放」。
///
/// ⚠️ 拉黑 / 举报**不调本函数**（见模块头 ③）。
async fn ensure_adult_social(db: &AnyPool, user_id: &str) -> Result<(), ApiError> {
    if is_adult(db, user_id).await? {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 参数化（VALIDATION.md §0.2 产品规则参数化，禁止写死）
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 下面每一个数都是**待验证假设**，随 VALIDATION §2 的 T4+ 数据修订。默认值一律取
//    「宁可解锁不了，不可错解锁」的保守方向——真人身份的误放行不可撤销，误拦截只是麻烦。

/// 正向羁绊解锁阈值 env（默认 0.6）。羁绊分口径见 `bond_between`。
const ENV_UNLOCK_MIN_BOND: &str = "MUSE_SOCIAL_UNLOCK_MIN_BOND";
const DEFAULT_UNLOCK_MIN_BOND: f64 = 0.6;

/// 敌对判据：`trust` / `affinity` 跌到 `-X` 及以下即判敌对（默认 0.3）。
const ENV_HOSTILE_MAX: &str = "MUSE_SOCIAL_HOSTILE_MAX";
const DEFAULT_HOSTILE_MAX: f64 = 0.3;

/// 敌对判据：`fear` 达到 `X` 及以上即判敌对（默认 0.5）。畏惧是单向的敌对信号。
const ENV_HOSTILE_FEAR: &str = "MUSE_SOCIAL_HOSTILE_FEAR";
const DEFAULT_HOSTILE_FEAR: f64 = 0.5;

/// 共同经历的世界数下限（默认 1）。「共历」是 §14 三条正向线的共同前提。
const ENV_MIN_SHARED_WORLDS: &str = "MUSE_SOCIAL_MIN_SHARED_WORLDS";
const DEFAULT_MIN_SHARED_WORLDS: i64 = 1;

/// 「我们的角色一起死过」是否单独构成一条合格的正向羁绊线（默认开）。
///
/// 规格把「共历生死」列在三条正向线之首，故默认 `true`：**羁绊分不够但一起死过**也够格。
/// 🔴 它**不是**敌对判据的例外——敌对线一票否决在它之前生效（见 `evaluate_eligibility`）。
const ENV_DEATH_BOND_COUNTS: &str = "MUSE_SOCIAL_DEATH_BOND_COUNTS";
const DEFAULT_DEATH_BOND_COUNTS: bool = true;

/// 每人每日（滚动 24h）可发起的解锁请求数上限（默认 3）。**跨世界合计**——
/// 只按世界限流会被「换个世界继续问同一个人」绕过（口径同 `invitations::invite_daily_limit`）。
const ENV_UNLOCK_DAILY_LIMIT: &str = "MUSE_SOCIAL_UNLOCK_DAILY_LIMIT";
const DEFAULT_UNLOCK_DAILY_LIMIT: i64 = 3;

/// 解锁请求有效期（默认 7 天）。过期即终局（惰性判定），不长期挂在别人收件箱里。
const ENV_UNLOCK_TTL_MS: &str = "MUSE_SOCIAL_UNLOCK_TTL_MS";
const DEFAULT_UNLOCK_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 黑名单条数上限（默认 500）。上限存在的理由不是限制用户自保，而是防单账号把黑名单
/// 当成批量数据结构刷（每条都要做一次角色→真人解析）。
const ENV_BLOCK_MAX: &str = "MUSE_SOCIAL_BLOCK_MAX";
const DEFAULT_BLOCK_MAX: i64 = 500;

/// 每人每日（滚动 24h）可提交的举报数上限（默认 20）。**刻意比解锁配额宽得多**：
/// 举报是保护工具，宁可多收几条无效举报，也不能让真正需要举报的人被配额挡住。
const ENV_REPORT_DAILY_LIMIT: &str = "MUSE_SOCIAL_REPORT_DAILY_LIMIT";
const DEFAULT_REPORT_DAILY_LIMIT: i64 = 20;

/// 同一举报人对同一对象的举报冷却窗口（默认 24h）。窗口内重复提交**幂等复用**既有那条，
/// 不新增行——既不刷队列，也不让举报人觉得「我点了没反应」。
const ENV_REPORT_COOLDOWN_MS: &str = "MUSE_SOCIAL_REPORT_COOLDOWN_MS";
const DEFAULT_REPORT_COOLDOWN_MS: i64 = 24 * 60 * 60 * 1000;

/// 举报升级阈值（默认 3）：同一被举报人累计 pending 举报数**恰好达到**该值时，
/// 写一条 `risk_events` 升级到既有风控面。用「恰好等于」而非「大于等于」是为了
/// 一次跨越只升级一次（确定性、无重复告警）；处置后计数回落再涨会再次升级，符合直觉。
const ENV_REPORT_ESCALATE_AT: &str = "MUSE_SOCIAL_REPORT_ESCALATE_AT";
const DEFAULT_REPORT_ESCALATE_AT: i64 = 3;

/// 列表页条数（默认 20，clamp [1,100]——上限防一次扫全表）。
const ENV_PAGE_SIZE: &str = "MUSE_SOCIAL_PAGE_SIZE";
const DEFAULT_PAGE_SIZE: i64 = 20;
const MIN_PAGE_SIZE: i64 = 1;
const MAX_PAGE_SIZE: i64 = 100;

/// 滚动配额窗口：24h（与 `invitations::INVITE_QUOTA_WINDOW_MS` 同口径）。
const QUOTA_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// 举报正文长度上限（字符），口径同 `annotations::REASON_MAX_CHARS`。
const DETAIL_MAX_CHARS: usize = 500;
/// 拉黑备注长度上限（字符）。只给自己看的一句话，不需要更长。
const BLOCK_REASON_MAX_CHARS: usize = 200;
/// 运营处置理由长度上限（字符），口径同 `admin_api::audit::resolve_appeal`。
const RESOLUTION_MAX_CHARS: usize = 500;

/// 非负浮点 env（缺失/非法/非有限/负数 → 默认值）。与 env 读取分离，便于无副作用地测回落规则。
fn parse_non_negative_f64(raw: Option<&str>, default: f64) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(default)
}

/// 正整数 env（缺失/非法/非正 → 默认值）。
fn parse_positive(raw: Option<&str>, default: i64) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok()).filter(|v| *v > 0).unwrap_or(default)
}

/// 布尔 env（与 `flags::parse_env_bool` 同构：`1/true/on/yes` 开，`0/false/off/no` 关，其余回落默认）。
fn parse_bool(raw: Option<&str>, default: bool) -> bool {
    match raw.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) => match v.as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            _ => default,
        },
        None => default,
    }
}

fn unlock_min_bond() -> f64 {
    parse_non_negative_f64(std::env::var(ENV_UNLOCK_MIN_BOND).ok().as_deref(), DEFAULT_UNLOCK_MIN_BOND)
}
fn hostile_max() -> f64 {
    parse_non_negative_f64(std::env::var(ENV_HOSTILE_MAX).ok().as_deref(), DEFAULT_HOSTILE_MAX)
}
fn hostile_fear() -> f64 {
    parse_non_negative_f64(std::env::var(ENV_HOSTILE_FEAR).ok().as_deref(), DEFAULT_HOSTILE_FEAR)
}
fn min_shared_worlds() -> i64 {
    parse_positive(std::env::var(ENV_MIN_SHARED_WORLDS).ok().as_deref(), DEFAULT_MIN_SHARED_WORLDS)
}
fn death_bond_counts() -> bool {
    parse_bool(std::env::var(ENV_DEATH_BOND_COUNTS).ok().as_deref(), DEFAULT_DEATH_BOND_COUNTS)
}
fn unlock_daily_limit() -> i64 {
    parse_positive(std::env::var(ENV_UNLOCK_DAILY_LIMIT).ok().as_deref(), DEFAULT_UNLOCK_DAILY_LIMIT)
}
fn unlock_ttl_ms() -> i64 {
    parse_positive(std::env::var(ENV_UNLOCK_TTL_MS).ok().as_deref(), DEFAULT_UNLOCK_TTL_MS)
}
fn block_max() -> i64 {
    parse_positive(std::env::var(ENV_BLOCK_MAX).ok().as_deref(), DEFAULT_BLOCK_MAX)
}
fn report_daily_limit() -> i64 {
    parse_positive(std::env::var(ENV_REPORT_DAILY_LIMIT).ok().as_deref(), DEFAULT_REPORT_DAILY_LIMIT)
}
fn report_cooldown_ms() -> i64 {
    parse_positive(std::env::var(ENV_REPORT_COOLDOWN_MS).ok().as_deref(), DEFAULT_REPORT_COOLDOWN_MS)
}
fn report_escalate_at() -> i64 {
    parse_positive(std::env::var(ENV_REPORT_ESCALATE_AT).ok().as_deref(), DEFAULT_REPORT_ESCALATE_AT)
}
fn page_size() -> i64 {
    parse_positive(std::env::var(ENV_PAGE_SIZE).ok().as_deref(), DEFAULT_PAGE_SIZE)
        .clamp(MIN_PAGE_SIZE, MAX_PAGE_SIZE)
}

// ═══════════════════════════════════════════════════════════════════════════
// 状态字面量与文案
// ═══════════════════════════════════════════════════════════════════════════

const UNLOCK_PENDING: &str = "pending";
const UNLOCK_ACCEPTED: &str = "accepted";
const UNLOCK_DECLINED: &str = "declined";
const UNLOCK_EXPIRED: &str = "expired";
const UNLOCK_REVOKED: &str = "revoked";

const REPORT_PENDING: &str = "pending";
const REPORT_ACTIONED: &str = "actioned";
const REPORT_DISMISSED: &str = "dismissed";

/// 举报主体种类白名单。
const SUBJECT_KINDS: &[&str] = &["character", "unlock_request"];

/// 举报类别白名单（放代码不放 DB CHECK：双库可移植子集禁 CHECK，且类别随真实数据演进，属 §0.2）。
/// 未知类别 → 400，绝不静默归到 `other`：那会污染类别分布，而分布正是运营要看的东西。
const REPORT_CATEGORIES: &[&str] =
    &["harassment", "impersonation", "minor_risk", "sexual", "violence", "fraud", "other"];

/// 🔴 **统一的"不能解锁"拒绝文案**（见模块头 ①）。
///
/// 刻意**不区分原因**：未成年、被拉黑、敌对线、羁绊不足、对方卡被驳回……全部共用这一句。
/// 区分原因就等于把端点变成探测器——「我发给他被拒了、发给她成功了」即可反推对方的年龄声明
/// 或黑名单状态，那是对**被查询者**的信息泄露，而被查询者从头到尾没有参与这次请求。
const REFUSE_GENERIC: &str = "暂时无法向该角色发起真人身份解锁";

pub fn router() -> Router<AppState> {
    Router::new()
        // 玩家面 · 世界维度
        .route("/worlds/{id}/social/bonds", get(list_bonds))
        .route("/worlds/{id}/social/unlock-requests", post(create_unlock_request))
        // 玩家面 · 账户维度
        .route("/me/social/unlock-requests", get(list_incoming_requests))
        .route("/me/social/unlock-requests/{id}/respond", post(respond_unlock_request))
        .route("/me/social/identities", get(list_identities))
        .route("/me/social/blocks", get(list_blocks).post(create_block))
        .route("/me/social/blocks/{id}", delete(remove_block))
        .route("/me/social/reports", post(create_report))
        // 运营面（reviewer/support 档：举报处置属内容风控 + 客服交叉领域）
        .route("/admin/social/reports", get(list_reports_admin))
        .route("/admin/social/reports/{id}/resolve", post(resolve_report_admin))
}

// ═══════════════════════════════════════════════════════════════════════════
// 面具展示（§14：对外可见的"人"只有角色）
// ═══════════════════════════════════════════════════════════════════════════

/// 角色展示名（`card_json.identity.name`）。取不到（卡结构异常/无名/空 id）时兜底，
/// **绝不因取名失败改变任何判定**（口径抄 `invitations::character_name`）。
async fn character_name(db: &AnyPool, character_id: &str) -> String {
    if character_id.trim().is_empty() {
        return "该角色".to_string();
    }
    let card: Option<String> =
        sqlx::query_scalar("SELECT card_json FROM cloud_characters WHERE id = ?")
            .bind(character_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    card.as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|v| v.pointer("/identity/name").and_then(|n| n.as_str()).map(str::to_string))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "该角色".to_string())
}

// ═══════════════════════════════════════════════════════════════════════════
// 拉黑判定（对外可见的唯一接口）
// ═══════════════════════════════════════════════════════════════════════════

/// 两人之间**任一方向**存在拉黑。
///
/// 🔴 **双向对称**是刻意的：A 拉黑 B 之后，不但 B 找不到 A，A 也找不到 B。
/// 只挡单向会留下「拉黑了对方，自己却还能凑上去」这条路——那不是保护，是单向静音。
///
/// 🔴 **不看功能开关**：拉黑是保护态，运营把 `MUSE_SOCIAL_IDENTITY_UNLOCK` 关掉
/// （急停 / 灰度收窄）不应让既有拉黑失效。方向与 `MUSE_SAFETY_LEXICON` 的 fail-safe 一致。
/// 供其它社交通道（如 `invitations`）在前门调用。
///
/// 查库失败**向上抛**（由调用方 500），而不是吞掉返回 false：吞掉等于「数据库抖了一下，
/// 拉黑就临时失效了」——这正是最不该在保护性判定上发生的降级。
pub(crate) async fn is_blocked_pair(db: &AnyPool, a: &str, b: &str) -> Result<bool, ApiError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM social_blocks \
         WHERE (blocker_user_id = ? AND blocked_user_id = ?) \
            OR (blocker_user_id = ? AND blocked_user_id = ?)",
    )
    .bind(a)
    .bind(b)
    .bind(b)
    .bind(a)
    .fetch_one(db)
    .await?;
    Ok(n > 0)
}

// ═══════════════════════════════════════════════════════════════════════════
// 羁绊折算（纯函数：只读 `worlds.narrative_state_json`，绝不写它）
// ═══════════════════════════════════════════════════════════════════════════

/// 一对角色之间的羁绊视图。**不落库、不下发原始分值**——
/// 下发分值等于把引擎的关系数值做成可被玩家优化的仪表盘，那是另一种形式的数值化。
#[derive(Debug, Clone, Default, PartialEq)]
struct BondView {
    /// 正向羁绊分：`max(trust, affinity, debt)` 取非负部分，**两方向取较小者**。
    ///
    /// 取较小者（而不是较大者/平均）是「双向自愿」在数据层的前置：单方面的好感不构成羁绊线，
    /// 一头热的人不该因为自己感觉好就够格去要对方的真身。缺失的方向按 0 计。
    positive: f64,
    /// 🔴 任一方向达敌对判据 → 一票否决（§14 敌对线永久匿名）。
    hostile: bool,
    /// 参与折算的关系边数（0 = 两人之间引擎没记过任何关系）。诊断用，不下发。
    edges: usize,
}

/// 从世界叙事状态里折算两角色的羁绊（纯函数，可单测；无 IO、无随机、无 map 迭代序依赖）。
///
/// 关系来源：`worlds.narrative_state_json` 的 `relations` 数组（引擎 `relation_dynamics` 的产出，
/// `from`/`to` 即 `cloud_character_id`），口径与 `memorial::grant_departed_marks_tx` **同源**。
/// 🔴 本函数**只读那一列**：写它等于把社交状态回灌进 `RoundInput.state`，直接踩平权红线。
///
/// 折算规则（三条，全部是"少给"的方向，宁可解锁不了不可错解锁）：
/// - 敌对：任一方向 `trust <= -hostile_max` 或 `affinity <= -hostile_max` 或 `fear >= hostile_fear`；
/// - 正向单边分：`max(trust, affinity, debt)` 再与 0 取大（`debt` 计入是因为 §14 明写「救命」
///   属正向线，而救命之恩在引擎里正是 `debt`）；
/// - 合并：**取各边的最小值**（见 `BondView::positive`）。
fn bond_between(state_json: &str, a: &str, b: &str, hostile_max: f64, hostile_fear: f64) -> BondView {
    // 结构损坏 / 空状态 → 中性视图（无边、非敌对、正向分 0）：解锁不了，但也不冤枉成敌对。
    let Ok(state) = serde_json::from_str::<Value>(state_json) else { return BondView::default() };
    let relations = state.get("relations").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);

    let mut view = BondView::default();
    let mut min_positive: Option<f64> = None;
    // 按数组既有次序遍历（引擎已定序），不经任何 HashMap —— 同一份状态恒得同一个结论。
    for rel in relations {
        let from = rel.get("from").and_then(Value::as_str).unwrap_or("");
        let to = rel.get("to").and_then(Value::as_str).unwrap_or("");
        let matched = (from == a && to == b) || (from == b && to == a);
        if !matched {
            continue;
        }
        let f = |k: &str| rel.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
        let (trust, affinity, fear, debt) = (f("trust"), f("affinity"), f("fear"), f("debt"));

        if trust <= -hostile_max || affinity <= -hostile_max || fear >= hostile_fear {
            view.hostile = true;
        }
        let edge_positive = trust.max(affinity).max(debt).max(0.0);
        min_positive = Some(match min_positive {
            Some(cur) => cur.min(edge_positive),
            None => edge_positive,
        });
        view.edges += 1;
    }
    view.positive = min_positive.unwrap_or(0.0);
    view
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 独有社交资产：「我们的角色一起死过」（关系凭证，不是数值）
// ═══════════════════════════════════════════════════════════════════════════

/// 一对角色的「一起死过」凭证。**只读派生，没有任何存储**（见模块头 ④）。
#[derive(Debug, Clone, Default)]
struct DiedTogether {
    /// 构成凭证的世界（按 world_id 升序，确定性）。空 = 没有这段共同经历。
    worlds: Vec<Value>,
}

impl DiedTogether {
    fn present(&self) -> bool {
        !self.worlds.is_empty()
    }

    /// 下发形状。🔴 `note` 是写给未来读代码的人的：这条凭证**永远**不该长出数值字段。
    fn to_json(&self) -> Value {
        json!({
            "kind": "died_together",
            "present": self.present(),
            "worlds": self.worlds,
            "note": "关系凭证，非数值：不计分、不发道具、不影响历练 / 卡位 / 背包 / 结算 / 引擎决策",
        })
    }
}

/// 派生「我们的角色一起死过」（只读；`db.rs` 可移植 SQL 子集内完成）。
///
/// 判据的**唯一事实源**是既有的死亡公共事实，本函数一个字节都不新增：
/// - 共同世界 = 两张卡在 `world_members` 里都有行的世界（足迹一行不删，死亡不抹掉走过的路）；
/// - 死亡落定 = `cloud_characters.memorial_status = 'sealed'` 且 `memorial_world_id` 指向该世界
///   （封卷是 `memorial` 模块对死亡公共事实的服务端权威核验之后才发生的，本函数不再重复核验，
///   也**绝不**接受任何客户端声明）。
///
/// 三档 `grade`（都算数，因为「一起死过」说的是共同经历而不是同时咽气）：
/// - `both_fell`  两张卡都在这个世界里封了卷 —— 同殁；
/// - `they_fell`  对方倒下、我还在场 —— 我送别了他；
/// - `i_fell`     我倒下、对方在场 —— 他送别了我。
///
/// `markedAsDeparted` 另附既有的「故人」印记（`memorial_marks`）是否已打——它是同一段关系
/// 在 `memorial` 侧的独立记录，两边对得上才说明这段共同经历是真的（对不上也不影响判定，
/// 只作展示，因为印记本身还受 `MUSE_MEMORIAL_BOND_MIN` 阈值影响）。
async fn died_together(db: &AnyPool, mine: &str, theirs: &str) -> Result<DiedTogether, ApiError> {
    // 共同世界（按 world_id 升序 → 结果确定，可 replay）。
    let shared: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT m1.world_id FROM world_members m1 \
         JOIN world_members m2 ON m2.world_id = m1.world_id \
         WHERE m1.cloud_character_id = ? AND m2.cloud_character_id = ? \
         ORDER BY m1.world_id ASC",
    )
    .bind(mine)
    .bind(theirs)
    .fetch_all(db)
    .await?;
    if shared.is_empty() {
        return Ok(DiedTogether::default());
    }

    let my_grave = grave_of(db, mine).await?;
    let their_grave = grave_of(db, theirs).await?;
    if my_grave.is_none() && their_grave.is_none() {
        // 两张卡都还在世 —— 没有「一起死过」这回事。
        return Ok(DiedTogether::default());
    }

    // 「故人」印记（任一方向即算：他记得你、你记得他，是同一段羁绊）。
    let marked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM memorial_marks \
         WHERE (character_id = ? AND deceased_character_id = ?) \
            OR (character_id = ? AND deceased_character_id = ?)",
    )
    .bind(mine)
    .bind(theirs)
    .bind(theirs)
    .bind(mine)
    .fetch_one(db)
    .await?;

    let mut out = DiedTogether::default();
    for (world_id,) in shared {
        let i_fell = my_grave.as_deref() == Some(world_id.as_str());
        let they_fell = their_grave.as_deref() == Some(world_id.as_str());
        let grade = match (i_fell, they_fell) {
            (true, true) => "both_fell",
            (false, true) => "they_fell",
            (true, false) => "i_fell",
            (false, false) => continue,
        };
        out.worlds.push(json!({
            "worldId": world_id,
            "grade": grade,
            "markedAsDeparted": marked > 0,
        }));
    }
    Ok(out)
}

/// 某张卡封卷于哪个世界；在世 / 卡不存在 / 未记录落款 → `None`。
async fn grave_of(db: &AnyPool, character_id: &str) -> Result<Option<String>, ApiError> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT memorial_status, memorial_world_id FROM cloud_characters WHERE id = ?")
            .bind(character_id)
            .fetch_optional(db)
            .await?;
    Ok(match row {
        Some((status, world)) if status == "sealed" => world.filter(|w| !w.trim().is_empty()),
        _ => None,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// 资格判定
// ═══════════════════════════════════════════════════════════════════════════

/// 一次资格判定的完整结论（含过程，供审计快照与自查展示）。
struct Eligibility {
    eligible: bool,
    bond: BondView,
    credential: DiedTogether,
    shared_worlds: i64,
    /// 命中的合格路径（`positive_bond` / `died_together`）。
    paths: Vec<&'static str>,
    /// 不合格原因（**只对本人展示**，绝不作为对他人的拒绝文案——见 `REFUSE_GENERIC`）。
    blockers: Vec<&'static str>,
}

impl Eligibility {
    /// 自查视图（`GET /worlds/{id}/social/bonds` 用）。🔴 不含任何真人身份，也不含原始羁绊分。
    fn to_self_json(&self) -> Value {
        json!({
            "eligible": self.eligible,
            "hostile": self.bond.hostile,
            "sharedWorlds": self.shared_worlds,
            "paths": {
                "positiveBond": self.paths.contains(&PATH_POSITIVE_BOND),
                "diedTogether": self.paths.contains(&PATH_DIED_TOGETHER),
            },
            "blockers": self.blockers,
            "credential": self.credential.to_json(),
        })
    }

    /// 发起那一刻的资格快照（落 `eligibility_json`，**只作审计，不参与任何判定**）。
    fn to_audit_json(&self) -> Value {
        json!({
            "eligible": self.eligible,
            "hostile": self.bond.hostile,
            "bondEdges": self.bond.edges,
            "sharedWorlds": self.shared_worlds,
            "paths": self.paths,
            "diedTogetherWorlds": self.credential.worlds.len(),
            "thresholds": {
                "minBond": unlock_min_bond(),
                "hostileMax": hostile_max(),
                "hostileFear": hostile_fear(),
                "minSharedWorlds": min_shared_worlds(),
                "deathBondCounts": death_bond_counts(),
            },
        })
    }
}

const PATH_POSITIVE_BOND: &str = "positive_bond";
const PATH_DIED_TOGETHER: &str = "died_together";

const BLOCK_HOSTILE: &str = "hostile_line_permanently_anonymous";
const BLOCK_SHARED: &str = "insufficient_shared_worlds";
const BLOCK_BOND: &str = "bond_below_threshold";

/// 判定 A 的角色与 B 的角色之间**当下**是否够格解锁真人身份。
///
/// 🔴 判定次序即产品语义，**不可调换**：
///   ① 敌对线一票否决 —— 在任何补偿路径之前，且不因「一起死过」而豁免
///      （最危险的一类关系恰恰是「一起死过之后反目」）；
///   ② 共历门槛 —— 没有共同经历就谈不上羁绊线；
///   ③ 两条正向路径**任一**成立即可：正向羁绊达阈值 / 一起死过（后者可由运营参数关掉）。
async fn evaluate_eligibility(
    db: &AnyPool,
    world_id: &str,
    mine: &str,
    theirs: &str,
) -> Result<Eligibility, ApiError> {
    let state_json: String = sqlx::query_scalar("SELECT narrative_state_json FROM worlds WHERE id = ?")
        .bind(world_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_else(|| "{}".to_string());
    let bond = bond_between(&state_json, mine, theirs, hostile_max(), hostile_fear());
    let credential = died_together(db, mine, theirs).await?;

    let shared_worlds: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT m1.world_id) FROM world_members m1 \
         JOIN world_members m2 ON m2.world_id = m1.world_id \
         WHERE m1.cloud_character_id = ? AND m2.cloud_character_id = ?",
    )
    .bind(mine)
    .bind(theirs)
    .fetch_one(db)
    .await?;

    let mut paths: Vec<&'static str> = Vec::new();
    let mut blockers: Vec<&'static str> = Vec::new();

    // ① 敌对线一票否决（§14 敌对线永久匿名）。
    if bond.hostile {
        blockers.push(BLOCK_HOSTILE);
        return Ok(Eligibility {
            eligible: false,
            bond,
            credential,
            shared_worlds,
            paths,
            blockers,
        });
    }
    // ② 共历门槛。
    if shared_worlds < min_shared_worlds() {
        blockers.push(BLOCK_SHARED);
    }
    // ③ 两条正向路径。
    if bond.positive >= unlock_min_bond() && bond.edges > 0 {
        paths.push(PATH_POSITIVE_BOND);
    }
    if credential.present() && death_bond_counts() {
        paths.push(PATH_DIED_TOGETHER);
    }
    if paths.is_empty() {
        blockers.push(BLOCK_BOND);
    }

    let eligible = blockers.is_empty();
    Ok(Eligibility { eligible, bond, credential, shared_worlds, paths, blockers })
}

// ═══════════════════════════════════════════════════════════════════════════
// 惰性过期（本仓库无定时清理器，惰性判定是既有口径，范式同 `invitations`）
// ═══════════════════════════════════════════════════════════════════════════

async fn expire_stale_requests(db: &AnyPool) -> Result<u64, ApiError> {
    let now = now_ms();
    let res = sqlx::query(
        "UPDATE social_unlock_requests SET status = ?, responded_at = ? \
         WHERE status = 'pending' AND expires_at <= ?",
    )
    .bind(UNLOCK_EXPIRED)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

// ═══════════════════════════════════════════════════════════════════════════
// 共用查询
// ═══════════════════════════════════════════════════════════════════════════

/// 我在某世界的面具（角色 id）。同一世界一人一卡由 `join` 的防自刷校验保证；
/// 万一有多张（历史数据），按入场时刻取最早那张，**确定性**优先于"聪明"。
async fn my_mask_in_world(db: &AnyPool, world_id: &str, user_id: &str) -> Result<Option<String>, ApiError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT cloud_character_id FROM world_members \
         WHERE world_id = ? AND user_id = ? ORDER BY joined_at ASC, id ASC LIMIT 1",
    )
    .bind(world_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(c,)| c))
}

/// 角色 → (主人, 机审状态)。角色不存在 → None。
///
/// 🔴 刻意**不查 `withdrawn`**：传世卡（死者）的 `withdrawn` 恒为 1（见 0034 迁移），
/// 而「我们的角色一起死过」这条线的对端**必然**是已封卷的卡。用 `withdrawn` 当社交资格门
/// 会把这条独有社交资产整条掐死。真正需要挡的是**未过审**的卡，那是 `moderation` 的事。
async fn owner_of(db: &AnyPool, character_id: &str) -> Result<Option<(String, String)>, ApiError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT owner_id, moderation FROM cloud_characters WHERE id = ?")
            .bind(character_id)
            .fetch_optional(db)
            .await?;
    Ok(row)
}

/// 一对角色在某世界的解锁状态（我方视角）。无记录 → `"none"`。
async fn unlock_status_for(
    db: &AnyPool,
    world_id: &str,
    mine: &str,
    theirs: &str,
) -> Result<(String, Option<String>), ApiError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, status, requester_character_id FROM social_unlock_requests \
         WHERE world_id = ? AND ((requester_character_id = ? AND target_character_id = ?) \
                              OR (requester_character_id = ? AND target_character_id = ?)) \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(world_id)
    .bind(mine)
    .bind(theirs)
    .bind(theirs)
    .bind(mine)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        None => ("none".to_string(), None),
        Some((id, status, requester)) => {
            let view = if status == UNLOCK_PENDING {
                if requester == mine {
                    "pending_outgoing".to_string()
                } else {
                    "pending_incoming".to_string()
                }
            } else {
                status
            };
            (view, Some(id))
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /worlds/{id}/social/bonds
// ═══════════════════════════════════════════════════════════════════════════

/// 我在该世界的社交对端（面具视图）：资格自查 + 「我们的角色一起死过」凭证 + 当前解锁状态。
///
/// 🔴 响应体**不含任何真人身份**。被我拉黑 / 拉黑了我的人直接从列表中消失（互相不可见），
/// 这就是"拉黑有服务端实效"的读取侧表现。
async fn list_bonds(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), Some(&world_id)).await?;
    ensure_adult_social(&state.db, &user.user_id).await?;
    expire_stale_requests(&state.db).await?;

    let Some(mine) = my_mask_in_world(&state.db, &world_id, &user.user_id).await? else {
        // 不是（也从未是）这个世界的成员 → 没有可社交的对端。404 而非空列表：
        // 空列表会让「世界不存在」与「我不在这个世界」长得一样。
        return Err(ApiError::NotFound);
    };

    let limit = page_size();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT user_id, cloud_character_id FROM world_members \
         WHERE world_id = ? AND user_id <> ? ORDER BY joined_at ASC, id ASC LIMIT ?",
    )
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::new();
    for (other_user, other_char) in rows {
        // 拉黑（任一方向）→ 从社交面消失。
        if is_blocked_pair(&state.db, &user.user_id, &other_user).await? {
            continue;
        }
        let elig = evaluate_eligibility(&state.db, &world_id, &mine, &other_char).await?;
        let (status, request_id) = unlock_status_for(&state.db, &world_id, &mine, &other_char).await?;
        let mut item = elig.to_self_json();
        item["characterId"] = json!(other_char);
        item["characterName"] = json!(character_name(&state.db, &other_char).await);
        item["unlockStatus"] = json!(status);
        item["unlockRequestId"] = json!(request_id);
        out.push(item);
    }

    Ok(Json(json!({
        "worldId": world_id,
        "myCharacterId": mine,
        "bonds": out,
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /worlds/{id}/social/unlock-requests
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateUnlockReq {
    /// 对端的**角色 id**（面具寻址，§14）：服务端据此解析收件人，
    /// 发起人自始至终拿不到对方的真人身份（除非对方接受）。
    target_character_id: String,
}

/// 发起真人身份解锁请求（双向自愿的第一步）。
async fn create_unlock_request(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CreateUnlockReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), Some(&world_id)).await?;
    // 🔴 未成年门在任何落库之前（含幂等键）——拒绝必须零副作用。
    ensure_adult_social(&state.db, &user.user_id).await?;

    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(
        &serde_json::to_vec(&json!({ "worldId": world_id, "body": body })).unwrap_or_default(),
    );
    let guard = idempotency::guard(
        &state.db,
        &user.user_id,
        "social.unlock.create",
        idem_key,
        &payload_hash,
    )
    .await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    expire_stale_requests(&state.db).await?;

    let target_char = body.target_character_id.trim().to_string();
    if target_char.is_empty() {
        return Err(ApiError::BadRequest("targetCharacterId 必填".into()));
    }

    let Some(mine) = my_mask_in_world(&state.db, &world_id, &user.user_id).await? else {
        return Err(ApiError::Forbidden);
    };
    if mine == target_char {
        return Err(ApiError::BadRequest("不能对自己的角色发起解锁".into()));
    }

    // 对端必须是这个世界的成员（社交对象来自共同经历，不是全站搜人）。
    let target_member: Option<(String,)> = sqlx::query_as(
        "SELECT user_id FROM world_members WHERE world_id = ? AND cloud_character_id = ?",
    )
    .bind(&world_id)
    .bind(&target_char)
    .fetch_optional(&state.db)
    .await?;
    let Some((target_user,)) = target_member else {
        return Err(ApiError::NotFound);
    };
    if target_user == user.user_id {
        return Err(ApiError::BadRequest("不能对自己的角色发起解锁".into()));
    }

    // ── 以下全部拒绝共用 REFUSE_GENERIC（见模块头 ①，不得区分原因） ──────────────
    let Some((_, moderation)) = owner_of(&state.db, &target_char).await? else {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    };
    if moderation != "approved" {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    }
    // 🔴 对端未成年门：不制造通往未成年的真人社交入口（真红线 §0.4）。
    if !is_adult(&state.db, &target_user).await? {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    }
    // 🔴 拉黑（任一方向）：拉黑后对方**真的**发不出社交动作。
    if is_blocked_pair(&state.db, &user.user_id, &target_user).await? {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    }

    // 反向已有 pending → 不新建第二条线，引导去收件箱回应（那才是"双向自愿"的正确形状）。
    let reverse: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM social_unlock_requests \
         WHERE world_id = ? AND requester_character_id = ? AND target_character_id = ? AND status = 'pending'",
    )
    .bind(&world_id)
    .bind(&target_char)
    .bind(&mine)
    .fetch_optional(&state.db)
    .await?;
    if let Some((rid,)) = reverse {
        return Err(ApiError::Conflict(format!(
            "对方已向你发起过解锁请求，请到收件箱回应（requestId={rid}）"
        )));
    }

    // 同一条线只有一行（唯一索引）：pending 幂等复用；终局态一律拒绝（拒绝/过期/撤销即终局）。
    let existing: Option<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, status, expires_at, created_at FROM social_unlock_requests \
         WHERE world_id = ? AND requester_character_id = ? AND target_character_id = ?",
    )
    .bind(&world_id)
    .bind(&mine)
    .bind(&target_char)
    .fetch_optional(&state.db)
    .await?;
    if let Some((id, status, expires_at, created_at)) = existing {
        if status != UNLOCK_PENDING {
            return Err(ApiError::Conflict(
                "这条解锁请求已有终局结果，不能重新发起（真人身份只问一次）".into(),
            ));
        }
        let resp = unlock_receipt(&id, &world_id, &target_char, &status, expires_at, created_at);
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    // 日配额（跨世界合计）。
    let now = now_ms();
    let sent_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM social_unlock_requests WHERE requester_user_id = ? AND created_at >= ?",
    )
    .bind(&user.user_id)
    .bind(now - QUOTA_WINDOW_MS)
    .fetch_one(&state.db)
    .await?;
    if sent_today >= unlock_daily_limit() {
        return Err(ApiError::Conflict("今日解锁请求次数已达上限，请稍后再试".into()));
    }

    // 🔴 资格判定（敌对线一票否决 / 共历门槛 / 两条正向路径）。不够格 → 同一句 REFUSE_GENERIC。
    let elig = evaluate_eligibility(&state.db, &world_id, &mine, &target_char).await?;
    if !elig.eligible {
        return Err(ApiError::Conflict(REFUSE_GENERIC.into()));
    }

    let id = new_id("sul");
    let expires_at = now + unlock_ttl_ms();
    // 🔴 UPSERT 而非「先查后插」：上面那次 existing 查询与这里的 INSERT 之间存在竞态窗口，
    // 两个并发请求会双双查到"不存在"然后都插入，其中一个撞 `idx_social_unlock_pair` 唯一索引
    // 变成 500。`DO NOTHING` 把竞态收敛为「先到的赢」，后到者走下面的回读分支幂等返回——
    // 于是**并发重复发起既不会报错，也不会发出第二条通知**（防骚扰的最后一寸）。
    let inserted = sqlx::query(
        "INSERT INTO social_unlock_requests \
         (id, world_id, requester_user_id, requester_character_id, target_user_id, \
          target_character_id, status, eligibility_json, expires_at, responded_at, revoked_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?, 0, 0, ?) \
         ON CONFLICT(world_id, requester_character_id, target_character_id) DO NOTHING",
    )
    .bind(&id)
    .bind(&world_id)
    .bind(&user.user_id)
    .bind(&mine)
    .bind(&target_user)
    .bind(&target_char)
    .bind(elig.to_audit_json().to_string())
    .bind(expires_at)
    .bind(now)
    .execute(&state.db)
    .await?;

    if inserted.rows_affected() == 0 {
        // 并发对手先建好了同一条线 → 回读权威行，幂等返回（不再发通知）。
        let (eid, estatus, eexpires, ecreated): (String, String, i64, i64) = sqlx::query_as(
            "SELECT id, status, expires_at, created_at FROM social_unlock_requests \
             WHERE world_id = ? AND requester_character_id = ? AND target_character_id = ?",
        )
        .bind(&world_id)
        .bind(&mine)
        .bind(&target_char)
        .fetch_one(&state.db)
        .await?;
        let resp = unlock_receipt(&eid, &world_id, &target_char, &estatus, eexpires, ecreated);
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    // 通知收件人。payload 只含世界维度与角色面具，**不含任何真人身份**（§14）。
    // kind 非 `consent*` 前缀 → 可退订类，用户能用既有通知偏好把这类打扰关掉（防骚扰的最后一道闸）。
    let dedupe = format!("social_unlock:{id}");
    enqueue_notification(
        &state,
        &target_user,
        "social_unlock_request",
        json!({
            "requestId": id,
            "worldId": world_id,
            "fromCharacterName": character_name(&state.db, &mine).await,
        }),
        Some(&dedupe),
        now,
    )
    .await?;

    let resp = unlock_receipt(&id, &world_id, &target_char, UNLOCK_PENDING, expires_at, now);
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

/// 发起回执（面具视图，无真人身份）。
fn unlock_receipt(
    id: &str,
    world_id: &str,
    target_char: &str,
    status: &str,
    expires_at: i64,
    created_at: i64,
) -> Value {
    json!({
        "id": id,
        "worldId": world_id,
        "targetCharacterId": target_char,
        "status": status,
        "expiresAt": expires_at,
        "createdAt": created_at,
        "note": "接受之前双方都看不到对方真人身份；接受后经 GET /api/me/social/identities 读取",
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /me/social/unlock-requests
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
struct StatusQuery {
    #[serde(default)]
    status: Option<String>,
}

/// 我**收到**的解锁请求（收件箱）。🔴 只出角色面具名——在我点「接受」之前，
/// 我不知道对面是谁，这正是"双向自愿"要保护的东西。
async fn list_incoming_requests(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<StatusQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    ensure_adult_social(&state.db, &user.user_id).await?;
    expire_stale_requests(&state.db).await?;

    let filter = q.status.unwrap_or_else(|| UNLOCK_PENDING.to_string());
    let rows: Vec<(String, String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, world_id, requester_user_id, requester_character_id, status, expires_at, created_at \
         FROM social_unlock_requests \
         WHERE target_user_id = ? AND (? = 'all' OR status = ?) \
         ORDER BY created_at DESC LIMIT ?",
    )
    .bind(&user.user_id)
    .bind(&filter)
    .bind(&filter)
    .bind(page_size())
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::new();
    for (id, world_id, requester_user, requester_char, status, expires_at, created_at) in rows {
        // 拉黑（任一方向）→ 该请求从收件箱消失（读取侧的拉黑实效）。
        if is_blocked_pair(&state.db, &user.user_id, &requester_user).await? {
            continue;
        }
        out.push(json!({
            "id": id,
            "worldId": world_id,
            // 🔴 只给面具名：既不给 requester_user_id，也不给昵称/手机号。
            "fromCharacterId": requester_char,
            "fromCharacterName": character_name(&state.db, &requester_char).await,
            "status": status,
            "expiresAt": expires_at,
            "createdAt": created_at,
        }));
    }
    Ok(Json(json!({ "requests": out })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /me/social/unlock-requests/{id}/respond
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RespondReq {
    accept: bool,
}

/// 接受 / 拒绝解锁请求（幂等）。
///
/// 🔴 **接受时用当下数据重算资格**：世界线会继续跑，发起时的正向羁绊到今天可能已翻脸成敌对线。
/// 只认发起时的快照等于给「先示好拿到 pending、再翻脸等对方点接受」留后门。
/// 重算不通过 → 403（且**不改状态**，请求仍留在 pending 直到过期，因为关系还可能回暖）。
async fn respond_unlock_request(
    State(state): State<AppState>,
    user: AuthUser,
    Path(request_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RespondReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    ensure_adult_social(&state.db, &user.user_id).await?;

    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(
        &serde_json::to_vec(&json!({ "id": request_id, "body": body })).unwrap_or_default(),
    );
    let guard = idempotency::guard(
        &state.db,
        &user.user_id,
        "social.unlock.respond",
        idem_key,
        &payload_hash,
    )
    .await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    expire_stale_requests(&state.db).await?;

    let row: Option<(String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT world_id, requester_user_id, requester_character_id, target_user_id, \
                target_character_id, status FROM social_unlock_requests WHERE id = ?",
    )
    .bind(&request_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((world_id, requester_user, requester_char, target_user, target_char, status)) = row
    else {
        return Err(ApiError::NotFound);
    };
    // 非收件人一律 404（而非 403）：既挡越权，也不泄露「这条请求存在」。
    if target_user != user.user_id {
        return Err(ApiError::NotFound);
    }
    if status != UNLOCK_PENDING {
        let resp = json!({ "id": request_id, "worldId": world_id, "status": status });
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    let new_status = if body.accept {
        // 🔴 三道复查（全部按**当下**数据）：拉黑 → 对端未成年 → 资格重算。
        if is_blocked_pair(&state.db, &user.user_id, &requester_user).await? {
            return Err(ApiError::Forbidden);
        }
        if !is_adult(&state.db, &requester_user).await? {
            return Err(ApiError::Forbidden);
        }
        let elig = evaluate_eligibility(&state.db, &world_id, &target_char, &requester_char).await?;
        if !elig.eligible {
            return Err(ApiError::Forbidden);
        }
        UNLOCK_ACCEPTED
    } else {
        UNLOCK_DECLINED
    };

    let now = now_ms();
    // 条件 UPDATE（CAS）：并发下只有一次能把 pending 推进，避免读改写丢更新。
    let res = sqlx::query(
        "UPDATE social_unlock_requests SET status = ?, responded_at = ? \
         WHERE id = ? AND status = 'pending'",
    )
    .bind(new_status)
    .bind(now)
    .bind(&request_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        let cur: String =
            sqlx::query_scalar("SELECT status FROM social_unlock_requests WHERE id = ?")
                .bind(&request_id)
                .fetch_one(&state.db)
                .await?;
        let resp = json!({ "id": request_id, "worldId": world_id, "status": cur });
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    if new_status == UNLOCK_ACCEPTED {
        // 通知发起人「对方接受了」。payload 仍只含面具名——真人身份要各自去
        // `/me/social/identities` 读，通知系统不做身份下发面（少一个可能被转发/截图的泄露面）。
        let dedupe = format!("social_unlock_accepted:{request_id}");
        enqueue_notification(
            &state,
            &requester_user,
            "social_unlock_accepted",
            json!({
                "requestId": request_id,
                "worldId": world_id,
                "withCharacterName": character_name(&state.db, &target_char).await,
            }),
            Some(&dedupe),
            now,
        )
        .await?;
    }

    let mut resp = json!({ "id": request_id, "worldId": world_id, "status": new_status });
    if new_status == UNLOCK_ACCEPTED {
        resp["next"] = json!({
            "method": "GET",
            "path": "/api/me/social/identities",
            "note": "双向自愿已达成；真人身份只在这一个读路径下发，且随时可被任一方拉黑收回可见性",
        });
    } else {
        resp["note"] = json!("已拒绝。这条线是终局的，对方不能再次发起（防反复施压）");
    }
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /me/social/identities —— 🔴 全平台唯一下发真人身份的读路径
// ═══════════════════════════════════════════════════════════════════════════

/// 已双向解锁的真人身份。
///
/// 🔴 下发的**只有 `userId` + 昵称**。手机号 / 实名信息是强 PII，平台内的"认识"不需要它们，
/// 一旦下发就再也收不回来。
///
/// 四道读取侧闸（任一不过 → 该条不出现，**不报错**：静默消失比"你被拉黑了"更安全）：
/// ① 功能开关（关阀 = 读不出，重开即恢复）；② 本人成年门；③ 任一方向拉黑；④ 对端成年门。
/// ③ 是「拉黑有服务端实效」在读取侧最硬的一条：**拉黑立刻收回已授予的身份可见性**。
async fn list_identities(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    ensure_adult_social(&state.db, &user.user_id).await?;

    let rows: Vec<(String, String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, world_id, requester_user_id, requester_character_id, target_user_id, \
                target_character_id, responded_at FROM social_unlock_requests \
         WHERE status = 'accepted' AND (requester_user_id = ? OR target_user_id = ?) \
         ORDER BY responded_at DESC LIMIT ?",
    )
    .bind(&user.user_id)
    .bind(&user.user_id)
    .bind(page_size())
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::new();
    for (id, world_id, req_user, req_char, tgt_user, tgt_char, at) in rows {
        let (other_user, other_char) = if req_user == user.user_id {
            (tgt_user, tgt_char)
        } else {
            (req_user, req_char)
        };
        if is_blocked_pair(&state.db, &user.user_id, &other_user).await? {
            continue;
        }
        if !is_adult(&state.db, &other_user).await? {
            continue;
        }
        let nickname: Option<String> = sqlx::query_scalar("SELECT nickname FROM users WHERE id = ?")
            .bind(&other_user)
            .fetch_optional(&state.db)
            .await?;
        out.push(json!({
            "unlockId": id,
            "worldId": world_id,
            "counterpartCharacterId": other_char,
            "counterpartCharacterName": character_name(&state.db, &other_char).await,
            "identity": {
                "userId": other_user,
                "nickname": nickname.unwrap_or_default(),
            },
            "unlockedAt": at,
        }));
    }
    Ok(Json(json!({
        "identities": out,
        "note": "任一方拉黑即刻收回身份可见性；本接口不下发手机号等强 PII",
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// 拉黑（保护工具：**不设年龄门**，见模块头 ③）
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBlockReq {
    /// 被拉黑者的**角色 id**（面具寻址）：真人 id 由服务端解析，请求方与响应体都拿不到。
    character_id: String,
    #[serde(default)]
    world_id: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// 我的黑名单（只出面具，不出被拉黑者的真人身份）。
async fn list_blocks(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, blocked_character_id, world_id, reason, created_at FROM social_blocks \
         WHERE blocker_user_id = ? ORDER BY created_at DESC LIMIT ?",
    )
    .bind(&user.user_id)
    .bind(page_size())
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::new();
    for (id, ch, world_id, reason, created_at) in rows {
        out.push(json!({
            "id": id,
            "characterId": ch,
            "characterName": character_name(&state.db, &ch).await,
            "worldId": world_id,
            "reason": reason,
            "createdAt": created_at,
        }));
    }
    Ok(Json(json!({ "blocks": out })))
}

/// 拉黑一张面具（服务端解析到真人，按 user 维度生效）。
///
/// 三件事一起发生（**顺序不可换**）：
/// ① 落一行 `social_blocks`（重复拉黑幂等复用同一行，不刷新时间、不新增行）；
/// ② **撤销**双方之间所有 `pending` / `accepted` 的解锁请求 → `revoked`
///    （已授予的真人身份可见性立刻收回，这是"拉黑有实效"最硬的一条）；
/// ③ 不通知对方——通知等于把拉黑变成一次挑衅，且会泄露"我拉黑了你"这个本该单向的事实。
async fn create_block(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateBlockReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;

    let character_id = body.character_id.trim().to_string();
    if character_id.is_empty() {
        return Err(ApiError::BadRequest("characterId 必填".into()));
    }
    let reason = body.reason.unwrap_or_default().trim().to_string();
    if reason.chars().count() > BLOCK_REASON_MAX_CHARS {
        return Err(ApiError::BadRequest(format!("拉黑备注不得超过 {BLOCK_REASON_MAX_CHARS} 字")));
    }
    let Some((blocked_user, _)) = owner_of(&state.db, &character_id).await? else {
        return Err(ApiError::NotFound);
    };
    if blocked_user == user.user_id {
        return Err(ApiError::BadRequest("不能拉黑自己".into()));
    }

    let now = now_ms();
    // 已拉黑 → 直接复用既有那行，且**跳过配额判定**：否则黑名单满额时，对同一个人再点一次
    // 拉黑会误报「已达上限」，而那次调用本来什么都不需要新增。
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM social_blocks WHERE blocker_user_id = ? AND blocked_user_id = ?",
    )
    .bind(&user.user_id)
    .bind(&blocked_user)
    .fetch_optional(&state.db)
    .await?;

    let id = match existing {
        Some((id,)) => id,
        None => {
            // 配额是**软闸**：并发下最多超出一格。为它上锁不值得——上限的目的是防单账号把
            // 黑名单当批量数据结构刷，差一格不影响这个目的。
            let total: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM social_blocks WHERE blocker_user_id = ?")
                    .bind(&user.user_id)
                    .fetch_one(&state.db)
                    .await?;
            if total >= block_max() {
                return Err(ApiError::Conflict("黑名单已达上限，请先清理".into()));
            }
            // 🔴 UPSERT 而非「先查后插」：查与插之间有竞态窗口，两个并发拉黑会双双查到
            // "不存在"然后都插入，其中一个撞 `idx_social_blocks_pair` 唯一索引变成 500——
            // 而**拉黑失败是最不该出现的失败**（用户正在试图保护自己）。
            // `DO NOTHING` 保留先到那行的 `created_at`（黑名单是事实，不是活跃度）。
            sqlx::query(
                "INSERT INTO social_blocks \
                 (id, blocker_user_id, blocked_user_id, blocked_character_id, world_id, reason, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(blocker_user_id, blocked_user_id) DO NOTHING",
            )
            .bind(new_id("sbk"))
            .bind(&user.user_id)
            .bind(&blocked_user)
            .bind(&character_id)
            .bind(body.world_id.unwrap_or_default().trim())
            .bind(&reason)
            .bind(now)
            .execute(&state.db)
            .await?;
            // 回读权威 id（无论是本次插入的还是并发对手插入的，此刻必然存在）。
            sqlx::query_scalar(
                "SELECT id FROM social_blocks WHERE blocker_user_id = ? AND blocked_user_id = ?",
            )
            .bind(&user.user_id)
            .bind(&blocked_user)
            .fetch_one(&state.db)
            .await?
        }
    };

    // ② 撤销双方之间未终局 / 已达成的解锁 —— 双向一次改完。
    let revoked = sqlx::query(
        "UPDATE social_unlock_requests SET status = ?, revoked_at = ? \
         WHERE status IN ('pending', 'accepted') \
           AND ((requester_user_id = ? AND target_user_id = ?) \
             OR (requester_user_id = ? AND target_user_id = ?))",
    )
    .bind(UNLOCK_REVOKED)
    .bind(now)
    .bind(&user.user_id)
    .bind(&blocked_user)
    .bind(&blocked_user)
    .bind(&user.user_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    Ok(Json(json!({
        "id": id,
        "characterId": character_id,
        "revokedUnlocks": revoked,
        "note": "已生效：对方无法向你发起任何社交动作，已解锁的真人身份可见性立即收回",
    })))
}

/// 解除拉黑。
///
/// 🔴 **不恢复**被撤销的解锁：`revoked` 是终局态，且唯一索引使同一条线不能重开。
/// 这是刻意的——解除拉黑意味着「愿意再见面」，不意味着「当初那次身份授予自动复活」。
/// 想重新解锁，只能重新在世界里把关系演出来（而这条线已被唯一索引占住 → 实际上
/// 需要一段**新的**共同经历，即一个新世界）。宁可严，不可让拉黑变成可反复横跳的开关。
async fn remove_block(
    State(state): State<AppState>,
    user: AuthUser,
    Path(block_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    let res = sqlx::query("DELETE FROM social_blocks WHERE id = ? AND blocker_user_id = ?")
        .bind(&block_id)
        .bind(&user.user_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({
        "id": block_id,
        "removed": true,
        "note": "已解除。此前被撤销的真人身份解锁不会恢复（revoked 是终局态）",
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// 举报（保护工具：**不设年龄门**，见模块头 ③）
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateReportReq {
    /// `character` / `unlock_request`
    subject_kind: String,
    subject_id: String,
    category: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    world_id: Option<String>,
}

/// 提交举报 → 进可运营的处理队列。
///
/// 服务端把「面具 / 一次解锁请求」解析成被举报的**真人 id** 落库供运营累计与处置，
/// 但**任何玩家侧响应体都不下发它**（举报不是探测对方真身的接口）。
///
/// 累计到阈值 → 另写一条 `risk_events`（复用既有风控面，不另造看板）。
async fn create_report(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateReportReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;

    let subject_kind = body.subject_kind.trim().to_string();
    if !SUBJECT_KINDS.contains(&subject_kind.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "subjectKind 必须是 {} 之一",
            SUBJECT_KINDS.join(" / ")
        )));
    }
    let category = body.category.trim().to_string();
    if !REPORT_CATEGORIES.contains(&category.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "category 必须是 {} 之一",
            REPORT_CATEGORIES.join(" / ")
        )));
    }
    let subject_id = body.subject_id.trim().to_string();
    if subject_id.is_empty() {
        return Err(ApiError::BadRequest("subjectId 必填".into()));
    }
    let detail = body.detail.unwrap_or_default().trim().to_string();
    if detail.chars().count() > DETAIL_MAX_CHARS {
        return Err(ApiError::BadRequest(format!("举报说明不得超过 {DETAIL_MAX_CHARS} 字")));
    }

    // 主体 → 被举报真人（服务端内部解析）。
    let (subject_user, mut world_id) = match subject_kind.as_str() {
        "character" => {
            let Some((owner, _)) = owner_of(&state.db, &subject_id).await? else {
                return Err(ApiError::NotFound);
            };
            (owner, String::new())
        }
        _ => {
            let row: Option<(String, String, String)> = sqlx::query_as(
                "SELECT requester_user_id, target_user_id, world_id FROM social_unlock_requests WHERE id = ?",
            )
            .bind(&subject_id)
            .fetch_optional(&state.db)
            .await?;
            let Some((req_user, tgt_user, w)) = row else {
                return Err(ApiError::NotFound);
            };
            // 只有这条线的**当事人**能举报它；被举报的是对面那个人。
            let other = if req_user == user.user_id {
                tgt_user
            } else if tgt_user == user.user_id {
                req_user
            } else {
                return Err(ApiError::NotFound);
            };
            (other, w)
        }
    };
    if subject_user == user.user_id {
        return Err(ApiError::BadRequest("不能举报自己".into()));
    }
    if world_id.is_empty() {
        world_id = body.world_id.unwrap_or_default().trim().to_string();
    }

    let now = now_ms();
    // 冷却窗口内重复提交 → 幂等复用既有那条（既不刷队列，也不让举报人觉得"点了没反应"）。
    let recent: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT id, status, created_at FROM social_reports \
         WHERE reporter_user_id = ? AND subject_kind = ? AND subject_id = ? AND created_at >= ? \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&user.user_id)
    .bind(&subject_kind)
    .bind(&subject_id)
    .bind(now - report_cooldown_ms())
    .fetch_optional(&state.db)
    .await?;
    if let Some((id, status, created_at)) = recent {
        return Ok(Json(json!({
            "id": id, "status": status, "createdAt": created_at, "deduped": true,
        })));
    }

    let today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM social_reports WHERE reporter_user_id = ? AND created_at >= ?",
    )
    .bind(&user.user_id)
    .bind(now - QUOTA_WINDOW_MS)
    .fetch_one(&state.db)
    .await?;
    if today >= report_daily_limit() {
        return Err(ApiError::Conflict("今日举报次数已达上限，请稍后再试".into()));
    }

    let id = new_id("srp");
    sqlx::query(
        "INSERT INTO social_reports \
         (id, reporter_user_id, subject_kind, subject_id, subject_user_id, world_id, category, \
          detail, status, handled_by, resolution, created_at, resolved_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', '', '', ?, 0)",
    )
    .bind(&id)
    .bind(&user.user_id)
    .bind(&subject_kind)
    .bind(&subject_id)
    .bind(&subject_user)
    .bind(&world_id)
    .bind(&category)
    .bind(&detail)
    .bind(now)
    .execute(&state.db)
    .await?;

    // 累计升级：pending 数**恰好**达到阈值时写一条 risk_events（一次跨越只升级一次）。
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM social_reports WHERE subject_user_id = ? AND status = 'pending'",
    )
    .bind(&subject_user)
    .fetch_one(&state.db)
    .await?;
    let escalated = pending == report_escalate_at();
    if escalated {
        sqlx::query(
            "INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) \
             VALUES (?, ?, ?, 'social_report_threshold', ?, ?)",
        )
        .bind(new_id("rsk"))
        .bind(&subject_user)
        .bind(&world_id)
        .bind(
            json!({
                "pendingReports": pending,
                "threshold": report_escalate_at(),
                "latestReportId": id,
                "latestCategory": category,
            })
            .to_string(),
        )
        .bind(now)
        .execute(&state.db)
        .await?;
    }

    Ok(Json(json!({
        "id": id,
        "status": REPORT_PENDING,
        "createdAt": now,
        "deduped": false,
        // 🔴 不回 subjectUserId：举报不是探测对方真身的接口。
        "note": "已进入运营处理队列",
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// 运营面（举报队列）
// ═══════════════════════════════════════════════════════════════════════════

/// 细粒度角色守卫。语义与 `admin_api::require_role` 逐字一致（`admin` 为超级用户放行一切），
/// 此处重写一份是因为那个函数是 `pub(super)`（admin_api 私有），而本模块是它的兄弟模块。
///
/// **取 `reviewer` + `support` 双档**：社交举报同时是内容风控问题（骚扰/色情/冒充 → reviewer）
/// 与客服问题（用户申告链路 → support），两条线都要能看到同一个队列，否则会各建一套口径。
fn require_report_handler(admin: &AdminUser) -> Result<(), ApiError> {
    let role = admin.0.role.as_str();
    if role == "admin" || role == "reviewer" || role == "support" {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

#[derive(Debug, Deserialize)]
struct ReportListQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    cursor: Option<i64>,
}

/// 举报队列（运营）。这里**可以**看到 `subjectUserId`——处置需要它，且 admin 面本就是特权面；
/// 玩家面一律不下发（见 `create_report`）。
async fn list_reports_admin(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<ReportListQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_ops_enabled(&state.db).await?;
    require_report_handler(&admin)?;

    let filter = q.status.unwrap_or_else(|| REPORT_PENDING.to_string());
    let rows: Vec<(String, String, String, String, String, String, String, String, String, i64, i64)> =
        sqlx::query_as(
            "SELECT id, reporter_user_id, subject_kind, subject_id, subject_user_id, world_id, \
                    category, detail, status, created_at, resolved_at FROM social_reports \
             WHERE (? = 'all' OR status = ?) AND (? IS NULL OR created_at < ?) \
             ORDER BY created_at DESC LIMIT ?",
        )
        .bind(&filter)
        .bind(&filter)
        .bind(q.cursor)
        .bind(q.cursor)
        .bind(page_size())
        .fetch_all(&state.db)
        .await?;

    let next = rows.last().map(|r| r.9);
    let reports: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                id,
                reporter,
                subject_kind,
                subject_id,
                subject_user,
                world_id,
                category,
                detail,
                status,
                created_at,
                resolved_at,
            )| {
                json!({
                    "id": id,
                    "reporterUserId": reporter,
                    "subjectKind": subject_kind,
                    "subjectId": subject_id,
                    "subjectUserId": subject_user,
                    "worldId": world_id,
                    "category": category,
                    "detail": detail,
                    "status": status,
                    "createdAt": created_at,
                    "resolvedAt": resolved_at,
                })
            },
        )
        .collect();
    Ok(Json(json!({ "reports": reports, "nextCursor": next })))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResolveReportReq {
    /// `actioned`（已处置）/ `dismissed`（不予支持）
    action: String,
    reason: String,
}

/// 处置一条举报。状态更新与审计留痕在**同一个事务**里——没有留痕就没有复盘。
///
/// 🔴 本端点**只改举报单自身的状态**：真正的处置动作（封禁 / 下架 / 改判）走既有的
/// `admin/users/{id}/ban`、`admin/audit-queue/*` 等专用路径，各自带自己的审计与权限。
/// 把处置动作塞进这里等于给封禁开一条绕过既有权限矩阵的侧门。
async fn resolve_report_admin(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(report_id): Path<String>,
    Json(body): Json<ResolveReportReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_ops_enabled(&state.db).await?;
    require_report_handler(&admin)?;

    let action = body.action.trim();
    let new_status = match action {
        "actioned" => REPORT_ACTIONED,
        "dismissed" => REPORT_DISMISSED,
        _ => return Err(ApiError::BadRequest("action 必须是 actioned / dismissed".into())),
    };
    let reason = body.reason.trim().to_string();
    if reason.is_empty() {
        return Err(ApiError::BadRequest("处置理由必填".into()));
    }
    if reason.chars().count() > RESOLUTION_MAX_CHARS {
        return Err(ApiError::BadRequest(format!("处置理由不得超过 {RESOLUTION_MAX_CHARS} 字")));
    }

    let now = now_ms();
    let mut tx = state.db.begin().await?;
    // CAS：只有 pending 能被推进（重复处置 / 并发处置命中 0 行 → 409，不覆盖别人的结论）。
    let res = sqlx::query(
        "UPDATE social_reports SET status = ?, handled_by = ?, resolution = ?, resolved_at = ? \
         WHERE id = ? AND status = 'pending'",
    )
    .bind(new_status)
    .bind(&admin.0.user_id)
    .bind(&reason)
    .bind(now)
    .bind(&report_id)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(ApiError::Conflict("该举报已被处理或不存在".into()));
    }
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES (?, ?, ?, 'social.report_resolved', ?, ?, ?)",
    )
    .bind(new_id("aud"))
    .bind(&admin.0.user_id)
    .bind(&admin.0.role)
    .bind(&report_id)
    .bind(format!("{new_status}|{reason}"))
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({ "id": report_id, "status": new_status, "resolvedAt": now })))
}
