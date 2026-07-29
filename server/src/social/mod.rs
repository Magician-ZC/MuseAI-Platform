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
//! GET    /admin/social/reports?status=&category=&subjectKind=&cursor=&cursorId=
//!                                                   举报队列（reviewer/support 档）
//! GET    /admin/social/reports/summary              队列形状：积压 / 类别分布 / 升级阈值（只读聚合）
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
    // 🔴 全仓唯一的「曾经开过吗」判定（见 `flags::entry_ever_open`）。
    // 此前这里是四份手抄之一，其中一处的文档还明写着「口径逐字抄」——
    // 它决定的是**指标诚不诚实**与**审核闭环开不开得了**，任一份漂移都不会有人立刻发现。
    crate::flags::entry_ever_open(db, ENV_SOCIAL_IDENTITY_UNLOCK).await
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

// 「已声明成年」的落库值与判定已**收归 `auth::is_declared_adult`**：
// 一条真红线（§0.4 未成年保护）不该有两份常量、更不该有六份手抄判据。
// 源码级红线用例 `auth::tests::red_line_only_one_place_reads_the_age_declaration` 扫死。

/// 该用户是否**已声明成年**。fail-closed：未声明(0)、未成年(2)、用户行缺失一律 false。
///
/// 口径与 `worlds::join_world` 的生死状门、`invitations::deathmatch_age_gate_ok` **逐字一致**——
/// 三处口径必须同源，否则「未成年保护」会有三种不同的含义，其中至少两种是错的。
async fn is_adult(db: &AnyPool, user_id: &str) -> Result<bool, ApiError> {
    // 🔴 全仓唯一的「已声明成年」判定（真红线 §0.4），见 `auth::is_declared_adult`。
    Ok(crate::auth::is_declared_adult(db, user_id).await)
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

/// `debt`（欠人情）是否计入正向羁绊分（**默认关**，2026-07-28 产品拍板）。
///
/// # 为什么改成不计
///
/// 解锁门的语义写明是「**双向自愿**」（跨边取 min 就是为这个）。而 `debt` 是**义务**不是意愿：
/// 「我欠你一条命」和「我愿意把真身交给你」是两件事。原口径把它并进 `max`，等于让单方面的
/// 亏欠也能往上抬羁绊分。
///
/// # 🔴 它与规格「救命属正向线」冲突吗——**不冲突，且这一条有用例钉着**
///
/// 引擎 `relation_dynamics` 里「救」类命中时是：
/// `affinity +0.08` **双向**、`trust +0.06` **双向**，另加 `debt +0.10` 只加在
/// `被救者→救人者` **单边**。而羁绊分**跨边取 min**——最小值只可能来自
/// **救人者那条边**，那条边上压根没有 debt。
///
/// 所以纯救命场景下，计不计 debt 算出来的分**一模一样**：救命之恩仍然是正向线，
/// 它由双向的 trust/affinity 承载，不由单边的 debt 承载。
/// 见用例 `rescue_bond_is_identical_whether_debt_counts_or_not`。
///
/// 真正被这一改动挡掉的，只有「双方互相亏欠、但彼此并不亲近」这一类——那正是该挡的。
const ENV_BOND_COUNTS_DEBT: &str = "MUSE_SOCIAL_BOND_COUNTS_DEBT";
const DEFAULT_BOND_COUNTS_DEBT: bool = false;


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
fn bond_counts_debt() -> bool {
    parse_bool(std::env::var(ENV_BOND_COUNTS_DEBT).ok().as_deref(), DEFAULT_BOND_COUNTS_DEBT)
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

/// 举报状态白名单（落库值全集，顺序即运营队列里的处理顺序）。
const REPORT_STATUSES: &[&str] = &[REPORT_PENDING, REPORT_ACTIONED, REPORT_DISMISSED];

/// 列表筛选的「不筛」取值。与任何落库值都不重名，故不会与真实状态/类别相撞。
const FILTER_ALL: &str = "all";

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
        // 🔴 解锁资格快照的**分布**读数。此前 `eligibility_json` 写了却**没有任何读取面**——
        // 一份没人看得到的审计快照等于没写。做成聚合（不是逐条列表）是刻意的：
        // 排查阈值配得对不对**不需要知道是谁**，而逐条列表会把真人 id 摆进一个只为调参存在的面。
        .route("/admin/social/bond-distribution", get(bond_distribution_admin))
        .route("/admin/social/reports", get(list_reports_admin))
        .route("/admin/social/reports/summary", get(report_summary_admin))
        .route("/admin/social/reports/{id}/resolve", post(resolve_report_admin))
}

// ═══════════════════════════════════════════════════════════════════════════
// 面具展示（§14：对外可见的"人"只有角色）
// ═══════════════════════════════════════════════════════════════════════════

/// 角色展示名（`card_json.identity.name`）。取不到（卡结构异常/无名/空 id）时兜底，
/// **绝不因取名失败改变任何判定**（口径抄 `invitations::character_name`）。
///
/// 本模块的每一处调用都是在把**对手方**的角色名摆给人看（羁绊列表 / 解锁请求 / 拉黑名单），
/// 所以被处置的卡在这里过 `NameGate`（默认关闭 → 恒等，输出与本闸门存在前逐字节一致）。
/// 闸门在函数内部逐次解析而不是由调用方传入：本模块的六个调用点各自只取 1-2 个名字，
/// 不存在 roster 那样的 N 行循环，逐次解析既不会放大查库次数，也免得六处各自记着传参。
async fn character_name(db: &AnyPool, character_id: &str) -> String {
    if character_id.trim().is_empty() {
        return "该角色".to_string();
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
         WHERE (blocker_user_id = $1 AND blocked_user_id = $2) \
            OR (blocker_user_id = $3 AND blocked_user_id = $4)",
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
    /// 正向羁绊分：`max(trust, affinity)` 取非负部分，**两方向取较小者**
    /// （`debt` 是否参与见 [`ENV_BOND_COUNTS_DEBT`]，默认不参与）。
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
/// - 正向单边分：`max(trust, affinity)` 再与 0 取大。
///   ⚠️ **`debt` 默认不计入**（2026-07-28 产品拍板，见 [`ENV_BOND_COUNTS_DEBT`]）：
///   解锁门是「双向自愿」，而欠人情是义务不是意愿。规格「救命属正向线」不受影响——
///   理由与用例见那个常量的注释。旧口径由该 env 一键取回。
/// - 合并：**取各边的最小值**（见 `BondView::positive`）。
fn bond_between(
    state_json: &str,
    a: &str,
    b: &str,
    hostile_max: f64,
    hostile_fear: f64,
    counts_debt: bool,
) -> BondView {
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
        let edge_positive =
            if counts_debt { trust.max(affinity).max(debt) } else { trust.max(affinity) }.max(0.0);
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
// ⚠️ 此处原有「我们的角色一起死过」凭证（§14 独有社交资产）—— 2026-07-29 删除
// ═══════════════════════════════════════════════════════════════════════════
//
// 它是真人身份解锁的**两条资格路径之一**（另一条是正向羁绊达阈值），
// 判据源是 `cloud_characters.memorial_status='sealed'` + `memorial_world_id`。
// 随 memorial 整块功能删除而一并移除——产品模型已改为**角色卡永不损失**，
// 「死亡」只是这张卡在那一个副本里的剧本结束，「一起死过」这个事实源不再存在。
//
// 🔴 **删的方向是收紧不是放宽**：现在只剩正向羁绊那一条路径，
// 而那本来就是 §14 的主路径（「仅正向羁绊线达阈值后双向自愿解锁」）。
// 社交解锁的门槛只会比之前更严，不会更松——这一点很重要，因为这道门连着
// 未成年保护与「敌对线永久匿名」的网暴防线，任何放宽都要单独评审。
//
// 📄 若将来要补回一条等价凭证，最自然的替代是「**共历终局**」
// （双方都在场至终局 / 一方退场另一方送别），事实源换成 `world_members.status` + `left_at`。
// ⚠️ 但要先答一个问题：退场在新模型下是**常见事件**，而死亡曾是罕见事件——
// 照搬会让这条凭证变得极易获得，等于**放宽**社交门槛。所以它不是改个字段就能补回来的。


// ═══════════════════════════════════════════════════════════════════════════
// 运营读数：解锁资格的正向分分布
// ═══════════════════════════════════════════════════════════════════════════

/// 一次取多少条快照。**有上界**：这是被后台读的端点，且 `eligibility_json` 是 TEXT JSON，
/// 可移植 SQL 子集里没有 JSON 函数，只能取回来在 Rust 侧聚合——不封顶就是把整张表拉回内存。
const BOND_DIST_SAMPLE_CAP: i64 = 5_000;
/// 直方图分桶数（0.0–1.0 均分）。10 桶 = 每桶 0.1，够看形状又不至于噪声。
const BOND_DIST_BUCKETS: usize = 10;

/// GET /admin/social/bond-distribution：解锁请求里**正向羁绊分**的分布。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 它补的是两个缺口，都不是产品决定
/// ════════════════════════════════════════════════════════════════════════════
///
/// ① **审计快照此前没有读取面**：`social_unlock_requests.eligibility_json` 一直在写，
///    而全仓没有任何地方读它。一份没人看得到的审计快照等于没写。
/// ② **快照此前不记被比较的值**：只有 `thresholds.minBond`，没有 `bond` 本身
///    （已在同一批补上，见 `Eligibility::to_audit_json`）。记了标尺没记读数，
///    事后没法回答「我为什么解锁不了」。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 它同时是 `docs/build/open-decisions.md` §2 自己写的那个「settling 证据」
/// ════════════════════════════════════════════════════════════════════════════
///
/// 那一节说：羁绊强度公式的真正验收是「按这个公式算出来的解锁人群，是不是产品想放行的那批」，
/// 而那**需要真实世界跑出来的关系分布**。本端点就是那份分布。
///
/// ⚠️ 它**不替产品定任何东西**：公式没改（`bond_between` 一个字没动），阈值没改，
/// 谁能解锁没改。它只是把「一直在决定事情、却从没有人看得见的那个数」露出来。
///
/// `wouldPassAt` 给的是「若把阈值挪到 X，有多少条会通过」——那正是调阈值时唯一想知道的事，
/// 而它此前得靠人去猜。
///
/// 🔴 **不下发任何真人身份**（§14）：本端点只回计数与分布，逐条明细一律不给。
/// 排查阈值配得对不对**不需要知道是谁**。
async fn bond_distribution_admin(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    // operator 档：这是调参用的只读读数，不是审核动作。
    let role = admin.0.role.as_str();
    if role != "admin" && role != "operator" {
        return Err(ApiError::Forbidden);
    }

    // ORDER BY 全序（`id` 是主键）：PG 对并列行不保证顺序，截断时取到哪一批必须确定。
    let rows = sqlx::query(
        "SELECT eligibility_json FROM social_unlock_requests \
         ORDER BY created_at DESC, id DESC LIMIT $1",
    )
    .bind(BOND_DIST_SAMPLE_CAP)
    .fetch_all(&state.db)
    .await?;

    let threshold = unlock_min_bond();
    let mut buckets = vec![0i64; BOND_DIST_BUCKETS];
    let mut values: Vec<f64> = Vec::with_capacity(rows.len());
    // 🔴 旧快照（本批次之前落的）**没有 `bond` 字段**。单列一格如实报，
    // 绝不按 0 计入分布——那会在直方图最左边堆出一座根本不存在的山。
    let mut legacy_without_bond = 0i64;

    for r in &rows {
        let raw: String = r.try_get("eligibility_json").unwrap_or_default();
        let Some(v) = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|j| j.get("bond").and_then(Value::as_f64))
            .filter(|v| v.is_finite())
        else {
            legacy_without_bond += 1;
            continue;
        };
        let clamped = v.clamp(0.0, 1.0);
        let idx = ((clamped * BOND_DIST_BUCKETS as f64) as usize).min(BOND_DIST_BUCKETS - 1);
        buckets[idx] += 1;
        values.push(v);
    }

    let n = values.len() as i64;
    let pass_now = values.iter().filter(|v| **v >= threshold).count() as i64;
    // 「阈值挪到 X 会有多少条通过」——调阈值时唯一想知道的事。
    let would_pass_at: Vec<Value> = (0..=10)
        .map(|i| {
            let t = i as f64 / 10.0;
            json!({ "threshold": t, "wouldPass": values.iter().filter(|v| **v >= t).count() as i64 })
        })
        .collect();

    Ok(Json(json!({
        "sampled": rows.len() as i64,
        "withBond": n,
        "legacyWithoutBond": legacy_without_bond,
        "truncated": rows.len() as i64 >= BOND_DIST_SAMPLE_CAP,
        "sampleCap": BOND_DIST_SAMPLE_CAP,
        "currentThreshold": threshold,
        "currentThresholdEnv": ENV_UNLOCK_MIN_BOND,
        "passingAtCurrentThreshold": pass_now,
        "histogram": {
            "buckets": BOND_DIST_BUCKETS,
            "width": 1.0 / BOND_DIST_BUCKETS as f64,
            "counts": buckets,
            "note": "第 i 桶 = [i×width, (i+1)×width)，末桶含右端 1.0。",
        },
        "wouldPassAt": would_pass_at,
        "formula": {
            "positiveBond": "逐条关系边取 max(trust, affinity, debt, 0)，再**跨边取 min**（弱的那一向把关）。",
            "whyMin": "「双向自愿」在数据层的前置：单方面的好感不构成羁绊线，一头热的人不该因为自己感觉好就够格要对方的真身。",
            "fearExcluded": true,
            "note": "🔴 本端点**不改公式**（`bond_between` 一个字没动），只是把一直在决定事情、\
                     却从没有人看得见的那个数露出来。",
        },
        "honesty": [
            "🔴 本端点不下发任何真人身份，也不给逐条明细（§14）——排查阈值配得对不对不需要知道是谁。",
            "⚠️ 旧快照（本批次之前落的）没有 bond 字段，单列 legacyWithoutBond 如实报，\
             **不按 0 计入分布**——那会在直方图最左边堆出一座不存在的山。",
            "⚠️ 样本只含**发起过解锁请求**的关系对，不是全平台关系的分布：\
             够不上阈值而没去点的人不在样本里，故这份分布**天然偏高**。",
            "它是 docs/build/open-decisions.md §2 说的那个 settling 证据，不是那一节的答案。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// 资格判定
// ═══════════════════════════════════════════════════════════════════════════

/// 一次资格判定的完整结论（含过程，供审计快照与自查展示）。
struct Eligibility {
    eligible: bool,
    bond: BondView,
    shared_worlds: i64,
    /// 命中的合格路径（当前只有 `positive_bond`）。
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
            },
            "blockers": self.blockers,
        })
    }

    /// 发起那一刻的资格快照（落 `eligibility_json`，**只作审计，不参与任何判定**）。
    ///
    /// 🔴 **必须记下被比较的那个值本身**，不只是门槛。此前这里只有 `thresholds.minBond`
    /// 而没有 `bond`——一份**记了标尺、没记读数**的快照无法完成它自己声明的用途：
    /// 事后有人问「我为什么解锁不了」/「他凭什么能解锁」，快照答不上来，
    /// 因为当时那条关系的正向分**没有任何地方留下过**（叙事态是活的，早已变了）。
    ///
    /// ⚠️ 与自查视图（[`Self::to_self_json`]）的差别是**刻意的**，不是漏了：
    /// 自查视图**不给分**，因为正向分由**双方**的边共同决定（跨边取 min），
    /// 露给本人等于泄露对方对他的感受——那撞 §14 的信息边界。
    /// 审计快照没有这个约束：它只进 admin 档的排查面，不回给任何一方。
    fn to_audit_json(&self) -> Value {
        json!({
            "eligible": self.eligible,
            "hostile": self.bond.hostile,
            "bondEdges": self.bond.edges,
            // 🔴 被 `thresholds.minBond` 比较的就是这个数。见上方说明。
            "bond": self.bond.positive,
            "sharedWorlds": self.shared_worlds,
            "paths": self.paths,
            "thresholds": {
                "minBond": unlock_min_bond(),
                "hostileMax": hostile_max(),
                "hostileFear": hostile_fear(),
                "minSharedWorlds": min_shared_worlds(),
            },
        })
    }
}

const PATH_POSITIVE_BOND: &str = "positive_bond";

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
    let state_json: String = sqlx::query_scalar("SELECT narrative_state_json FROM worlds WHERE id = $1")
        .bind(world_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_else(|| "{}".to_string());
    let bond = bond_between(&state_json, mine, theirs, hostile_max(), hostile_fear(), bond_counts_debt());

    let shared_worlds: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT m1.world_id) FROM world_members m1 \
         JOIN world_members m2 ON m2.world_id = m1.world_id \
         WHERE m1.cloud_character_id = $1 AND m2.cloud_character_id = $2",
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
    if paths.is_empty() {
        blockers.push(BLOCK_BOND);
    }

    let eligible = blockers.is_empty();
    Ok(Eligibility { eligible, bond, shared_worlds, paths, blockers })
}

// ═══════════════════════════════════════════════════════════════════════════
// 惰性过期（本仓库无定时清理器，惰性判定是既有口径，范式同 `invitations`）
// ═══════════════════════════════════════════════════════════════════════════

async fn expire_stale_requests(db: &AnyPool) -> Result<u64, ApiError> {
    let now = now_ms();
    let res = sqlx::query(
        "UPDATE social_unlock_requests SET status = $1, responded_at = $2 \
         WHERE status = 'pending' AND expires_at <= $3",
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
         WHERE world_id = $1 AND user_id = $2 ORDER BY joined_at ASC, id ASC LIMIT 1",
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
        sqlx::query_as("SELECT owner_id, moderation FROM cloud_characters WHERE id = $1")
            .bind(character_id)
            .fetch_optional(db)
            .await?;
    Ok(row)
}

/// 一对角色在某世界的解锁状态（我方视角）。无记录 → `"none"`。
///
/// 唯一键 `idx_social_unlock_pair(world_id, requester_character_id, target_character_id)` 保证
/// 每个方向至多一行，故这里最多命中 2 行（A→B 与 B→A）。两行 `created_at` 并列时（双方同毫秒
/// 互发）单键选谁是任意的，且选中哪行会改变返回的 `status`/`requester`——**是被采用的值，不只是显示顺序**。
/// 补 `id DESC` 使其确定。
async fn unlock_status_for(
    db: &AnyPool,
    world_id: &str,
    mine: &str,
    theirs: &str,
) -> Result<(String, Option<String>), ApiError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, status, requester_character_id FROM social_unlock_requests \
         WHERE world_id = $1 AND ((requester_character_id = $2 AND target_character_id = $3) \
                              OR (requester_character_id = $4 AND target_character_id = $5)) \
         ORDER BY created_at DESC, id DESC LIMIT 1",
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
         WHERE world_id = $1 AND user_id <> $2 ORDER BY joined_at ASC, id ASC LIMIT $3",
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
        "SELECT user_id FROM world_members WHERE world_id = $1 AND cloud_character_id = $2",
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
         WHERE world_id = $1 AND requester_character_id = $2 AND target_character_id = $3 AND status = 'pending'",
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
         WHERE world_id = $1 AND requester_character_id = $2 AND target_character_id = $3",
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
        "SELECT COUNT(*) FROM social_unlock_requests WHERE requester_user_id = $1 AND created_at >= $2",
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
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, $8, 0, 0, $9) \
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
             WHERE world_id = $1 AND requester_character_id = $2 AND target_character_id = $3",
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
         WHERE target_user_id = $1 AND ($2 = 'all' OR status = $3) \
         ORDER BY created_at DESC, id DESC LIMIT $4",
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
                target_character_id, status FROM social_unlock_requests WHERE id = $1",
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
        "UPDATE social_unlock_requests SET status = $1, responded_at = $2 \
         WHERE id = $3 AND status = 'pending'",
    )
    .bind(new_status)
    .bind(now)
    .bind(&request_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        let cur: String =
            sqlx::query_scalar("SELECT status FROM social_unlock_requests WHERE id = $1")
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
         WHERE status = 'accepted' AND (requester_user_id = $1 OR target_user_id = $2) \
         ORDER BY responded_at DESC, id DESC LIMIT $3",
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
        let nickname: Option<String> = sqlx::query_scalar("SELECT nickname FROM users WHERE id = $1")
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
         WHERE blocker_user_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2",
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
        "SELECT id FROM social_blocks WHERE blocker_user_id = $1 AND blocked_user_id = $2",
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
                sqlx::query_scalar("SELECT COUNT(*) FROM social_blocks WHERE blocker_user_id = $1")
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
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
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
                "SELECT id FROM social_blocks WHERE blocker_user_id = $1 AND blocked_user_id = $2",
            )
            .bind(&user.user_id)
            .bind(&blocked_user)
            .fetch_one(&state.db)
            .await?
        }
    };

    // ② 撤销双方之间未终局 / 已达成的解锁 —— 双向一次改完。
    let revoked = sqlx::query(
        "UPDATE social_unlock_requests SET status = $1, revoked_at = $2 \
         WHERE status IN ('pending', 'accepted') \
           AND ((requester_user_id = $3 AND target_user_id = $4) \
             OR (requester_user_id = $5 AND target_user_id = $6))",
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
    let res = sqlx::query("DELETE FROM social_blocks WHERE id = $1 AND blocker_user_id = $2")
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
                "SELECT requester_user_id, target_user_id, world_id FROM social_unlock_requests WHERE id = $1",
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
         WHERE reporter_user_id = $1 AND subject_kind = $2 AND subject_id = $3 AND created_at >= $4 \
         ORDER BY created_at DESC, id DESC LIMIT 1",
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
        "SELECT COUNT(*) FROM social_reports WHERE reporter_user_id = $1 AND created_at >= $2",
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
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending', '', '', $9, 0)",
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
        "SELECT COUNT(*) FROM social_reports WHERE subject_user_id = $1 AND status = 'pending'",
    )
    .bind(&subject_user)
    .fetch_one(&state.db)
    .await?;
    let escalated = pending == report_escalate_at();
    if escalated {
        sqlx::query(
            "INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) \
             VALUES ($1, $2, $3, 'social_report_threshold', $4, $5)",
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
    /// 举报类别筛选（`REPORT_CATEGORIES` 之一，或 `all`）。缺省 = `all`。
    #[serde(default)]
    category: Option<String>,
    /// 主体种类筛选（`SUBJECT_KINDS` 之一，或 `all`）。缺省 = `all`。
    #[serde(default, rename = "subjectKind")]
    subject_kind: Option<String>,
    #[serde(default)]
    cursor: Option<i64>,
    /// 复合游标的第二段（上一页末行的 `id`），见 `crate::pagination`。
    #[serde(default, rename = "cursorId")]
    cursor_id: Option<String>,
}

/// 列表筛选值校验：缺省取 `default`，`all` 与白名单值放行，其余 **400**。
///
/// 🔴 未知筛选值必须报错，不能走「匹配不到 → 返回空列表」那条路：举报队列是安全通道，
/// 一个拼错的筛选参数静默返回空队列，运营读到的是「没有积压」——安全面上最危险的那种误读。
/// （口径同 `create_report` 对未知 `category` 的处理：绝不静默归并。）
fn filter_value(
    raw: Option<String>,
    whitelist: &[&str],
    field: &str,
    default: &str,
) -> Result<String, ApiError> {
    let value = raw
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string());
    if value == FILTER_ALL || whitelist.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(ApiError::BadRequest(format!(
            "{field} 必须是 {} / {FILTER_ALL} 之一",
            whitelist.join(" / ")
        )))
    }
}

/// 举报队列（运营）。这里**可以**看到 `subjectUserId`——处置需要它，且 admin 面本就是特权面；
/// 玩家面一律不下发（见 `create_report`）。
///
/// 🔴 复合游标 `(created_at, id)`（见 `crate::pagination`）：单列游标下，同毫秒到达的一批举报
/// 若横跨页边界，被跳过的那几条**不会出现在队列的任何一页**——运营看不见 = 永远不会被处置。
/// 这是本仓所有游标分页里后果最重的一处（举报是安全通道，漏一条就是漏一次处置）。
///
/// 🔴 **末页必须回 `nextCursor: null`**（多取一行判定，口径同 `admin_api::ops::list_risk_events`）：
/// 只按「末行有没有」发游标的话，最后一页也带着一个游标返回，界面上的「加载更多」于是永远在，
/// 点下去只能得到空页。这在别处只是难看，在举报队列上是**让运营分不清「翻完了」和「还没翻完」**——
/// 而「还有没有没看的举报」正是这个页面唯一要回答的问题。
async fn list_reports_admin(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<ReportListQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_ops_enabled(&state.db).await?;
    require_report_handler(&admin)?;

    let filter = filter_value(q.status, REPORT_STATUSES, "status", REPORT_PENDING)?;
    let category = filter_value(q.category, REPORT_CATEGORIES, "category", FILTER_ALL)?;
    let subject_kind = filter_value(q.subject_kind, SUBJECT_KINDS, "subjectKind", FILTER_ALL)?;

    let page = page_size();
    // `handled_by` / `resolution` 一并投影：没有它们，`status=actioned` 那一屏只能看到「有结论」，
    // 看不到**结论是什么、谁下的**——运营复核档最需要回答的恰是这两件事（申诉与复盘都从这里起）。
    #[allow(clippy::type_complexity)]
    let mut rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
    )> = sqlx::query_as(
            "SELECT id, reporter_user_id, subject_kind, subject_id, subject_user_id, world_id, \
                    category, detail, status, handled_by, resolution, created_at, resolved_at \
             FROM social_reports \
             WHERE ($1 = 'all' OR status = $2) \
               AND ($3 = 'all' OR category = $4) \
               AND ($5 = 'all' OR subject_kind = $6) \
               AND ($7 IS NULL OR created_at < $8 OR (created_at = $9 AND id < $10)) \
             ORDER BY created_at DESC, id DESC LIMIT $11",
        )
        .bind(&filter)
        .bind(&filter)
        .bind(&category)
        .bind(&category)
        .bind(&subject_kind)
        .bind(&subject_kind)
        .bind(q.cursor)
        .bind(q.cursor)
        .bind(q.cursor)
        .bind(crate::pagination::cursor_id_bound(q.cursor_id.as_deref()))
        .bind(page + 1)
        .fetch_all(&state.db)
        .await?;

    // 多取的那一行只用来回答「后面还有没有」，不下发。
    let has_more = rows.len() as i64 > page;
    rows.truncate(page.max(0) as usize);
    let (next, next_id) = if has_more {
        (rows.last().map(|r| r.11), rows.last().map(|r| r.0.clone()))
    } else {
        (None, None)
    };
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
                handled_by,
                resolution,
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
                    "handledBy": handled_by,
                    "resolution": resolution,
                    "createdAt": created_at,
                    "resolvedAt": resolved_at,
                })
            },
        )
        .collect();
    Ok(Json(json!({
        "reports": reports,
        "nextCursor": next,
        "nextCursorId": next_id,
        "status": filter,
        "category": category,
        "subjectKind": subject_kind,
        "pageSize": page,
    })))
}

/// 举报队列的**形状**：积压、类别/主体分布、最久未处理、达升级阈值的对象数。只读聚合，零写入。
///
/// 为什么单独一个端点、而不让界面拿列表页自己数：列表是**游标分页**的，按已加载的那一页统计
/// 出来的「待处理 12 条」意思是「这一页里有 12 条」，不是队列真实积压。
/// 在别的看板上这只是口径不准，在举报队列上它会被读成「没什么要处理的」——
/// 与 `docs/design/admin-ui-design.md` §9.1「只渲染接口真实返回的字段」是同一条纪律。
///
/// 每项**一次聚合查询**（`GROUP BY` / 单行取），无 N+1、无逐行回传；SQL 全落在
/// `db.rs` 的双库可移植子集内（只用 `COUNT` / `GROUP BY` / `CAST(... AS BIGINT)`，占位符 `$N`）。
/// `ORDER BY` 全序：分组键本身唯一，故按分组键升序即全序。
async fn report_summary_admin(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    ensure_ops_enabled(&state.db).await?;
    require_report_handler(&admin)?;

    // ① 按状态（积压口径）。
    let status_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, CAST(COUNT(*) AS BIGINT) AS n FROM social_reports \
         GROUP BY status ORDER BY status ASC",
    )
    .fetch_all(&state.db)
    .await?;
    let mut by_status = serde_json::Map::new();
    // 白名单三档恒出现（哪怕是 0）：缺档会让界面把「这一档没有」渲染成「这一档没数据源」。
    for s in REPORT_STATUSES {
        by_status.insert((*s).to_string(), json!(0));
    }
    let mut total: i64 = 0;
    for (status, n) in &status_rows {
        total += *n;
        by_status.insert(status.clone(), json!(*n));
    }

    // ② 按类别 × 状态、③ 按主体种类 × 状态（两条分布，各一次 GROUP BY）。
    let category_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT category, status, CAST(COUNT(*) AS BIGINT) AS n FROM social_reports \
         GROUP BY category, status ORDER BY category ASC, status ASC",
    )
    .fetch_all(&state.db)
    .await?;
    let kind_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT subject_kind, status, CAST(COUNT(*) AS BIGINT) AS n FROM social_reports \
         GROUP BY subject_kind, status ORDER BY subject_kind ASC, status ASC",
    )
    .fetch_all(&state.db)
    .await?;

    // ④ 最久未处理的待办（全序取一行，不用 MIN 聚合：`MIN` 在空表上回 NULL，
    //    Any 驱动上还要额外处理空聚合的类型，取一行更直白且顺带证明「确实有这么一条」）。
    let oldest_pending: Option<i64> = sqlx::query_scalar(
        "SELECT created_at FROM social_reports WHERE status = $1 \
         ORDER BY created_at ASC, id ASC LIMIT 1",
    )
    .bind(REPORT_PENDING)
    .fetch_optional(&state.db)
    .await?;

    // ⑤ 已达升级阈值的被举报人数（与 `create_report` 写 `risk_events` 用的是同一个阈值）。
    //    这里只给**数量**，不给名单：名单的既有去处是风控面
    //    （`risk_events(kind='social_report_threshold')`），本页不复制一份。
    let escalate_at = report_escalate_at();
    let escalated_subjects: i64 = sqlx::query_scalar(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM ( \
           SELECT subject_user_id FROM social_reports WHERE status = $1 \
           GROUP BY subject_user_id HAVING COUNT(*) >= $2 \
         ) t",
    )
    .bind(REPORT_PENDING)
    .bind(escalate_at)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({
        "total": total,
        "byStatus": Value::Object(by_status),
        "byCategory": group_distribution(&category_rows, REPORT_CATEGORIES),
        "bySubjectKind": group_distribution(&kind_rows, SUBJECT_KINDS),
        "oldestPendingCreatedAt": oldest_pending,
        "escalateAt": escalate_at,
        "escalatedSubjectCount": escalated_subjects,
        "notes": [
            "计数为全量聚合，不受列表分页与筛选影响。",
            "升级阈值 escalateAt 可运营配置（MUSE_SOCIAL_REPORT_ESCALATE_AT），不是写死的常量。",
            "达阈值对象的名单在风控面：risk_events.kind = 'social_report_threshold'。",
        ],
    })))
}

/// `(key, status, n)` 分组行 → `[{key, pending, actioned, dismissed, total}]`。
///
/// **白名单里的键恒出现（哪怕全 0）**：筛选下拉要按白名单给全，而不是「库里有什么给什么」——
/// 后者会让一个还没人用过的举报类别在界面上根本不存在，运营也就无从按它筛。
/// 白名单外的键（历史数据 / 直写库）追加在后面原样回显，不静默丢弃。
fn group_distribution(rows: &[(String, String, i64)], whitelist: &[&str]) -> Value {
    let mut order: Vec<String> = whitelist.iter().map(|s| (*s).to_string()).collect();
    for (key, _, _) in rows {
        if !order.iter().any(|k| k == key) {
            order.push(key.clone());
        }
    }
    let items: Vec<Value> = order
        .into_iter()
        .map(|key| {
            let mut obj = serde_json::Map::new();
            obj.insert("key".into(), json!(key));
            let mut sum = 0i64;
            for status in REPORT_STATUSES {
                let n = rows
                    .iter()
                    .find(|(k, s, _)| k == &key && s == status)
                    .map(|(_, _, n)| *n)
                    .unwrap_or(0);
                sum += n;
                obj.insert((*status).to_string(), json!(n));
            }
            obj.insert("total".into(), json!(sum));
            Value::Object(obj)
        })
        .collect();
    Value::Array(items)
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
        "UPDATE social_reports SET status = $1, handled_by = $2, resolution = $3, resolved_at = $4 \
         WHERE id = $5 AND status = 'pending'",
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
         VALUES ($1, $2, $3, 'social.report_resolved', $4, $5, $6)",
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
