//! **直播场**（R3 收官件；总规格 `docs/build/spec-world-ecosystem.md` §2 场次节奏三档 + §15 第 4 层）。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 规格原文（本模块的全部依据）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! §2 场次节奏三档：
//!
//! > | **直播场** | 密集拍，一晚跑完一阶段 | 官方阶段**定档场次**、赛事 | **弹幕流** + 实时观战 + 打赏 |
//! > - 阶段页面明示预期时长：直播场"今晚 2 小时"……
//!
//! §15 运行时内容安全五层漏斗：
//!
//! > | 4 | **直播场延迟 1-2 拍缓冲**（给 2/3 层拦截窗口） | 0 |
//!
//! `docs/VALIDATION.md` §2 T5：开放范围「50-100 人世界；**直播场 + 弹幕**」；
//! 门槛「**直播场观众→玩家转化 ≥2%**」「内容审核成本 ≤ 生成成本的 5%」；
//! 预案「审核成本失控 → **直播延迟拍数上调** + 公开投影降频」。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 本模块**不是**从零建观战 —— 它建在既有观战之上
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 观战早已实现：`events::can_view_world`（资格）· `GET /worlds/{id}/events` +
//! `WS /worlds/{id}/stream`（实时流）· `arena::get_report` / `get_replay`（战报与回放）·
//! `livegate`（礼物 → `arena_env_events` 环境增益）。本模块只补三件既有链路没有的东西：
//!
//! | 件 | 既有观战 | 直播场补什么 |
//! |---|---|---|
//! | **定档** | 无。世界一直在跑，观众不知道何时该看 | `live_sessions`：预告时刻 + 开播时刻 + 场次容量 |
//! | **延迟缓冲** | 无。事件一落库即对观众可见 | 播出水位线：公开播出面落后 N 拍，给 §15 第 2/3 层留拦截窗口 |
//! | **弹幕** | 无。观众只能看，不能出声 | `live_danmaku`：过审核链 + 限频 + 锚定播出拍 |
//!
//! 复用的部分**一行都不重造**：观战资格走 `events::can_view_world`，实时推送走
//! `events::WsHub`（不另起一套推送），审核走 `safety::moderate_and_queue` / `safety::mask`
//! （静态 UGC 的唯一入队/记险入口），未成年门走与 `social::ensure_adult_social` /
//! `worlds::join_world` **逐字一致**的 `users.age_declared == 1` 口径。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 延迟缓冲：它是**内容安全机制**，不是体验设计
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ### 待播内容存在哪
//!
//! **存在它本来就在的地方**：`world_events`。本模块**不建任何待播副本表**。
//!
//! 一拍跑完，`runtime::commit_tick` 在事务里把 `world_events` 落定——那一刻它已经是世界事实
//! （§0.3「公共事实不可回滚」说的正是它）。延迟缓冲不碰这个事实，它只在**直播播出面**上
//! 加一条水位线：
//!
//! ```text
//!   世界内（成员/参赛者） /api/worlds/{id}/events        ── 按真实节奏，不延迟
//!   世界外（直播观众）    /api/live/sessions/{id}/feed   ── tick_no <= 水位线 才播
//! ```
//!
//! 一份内容一处存储，于是不存在"缓冲区与正本不一致"这种错位。反过来，若真去建一张
//! `pending_broadcast_events` 副本表，立刻会有两个事实源：副本写失败 = 世界演过了而直播
//! 永远缺一拍；副本被改写 = 观众看到的与世界记载的不是同一件事。**那才是事实错乱。**
//!
//! ### 水位线怎么算（[`publish_watermark`]）
//!
//! ```text
//!   scheduled / canceled → None（一拍不播）
//!   live                 → max_done_tick - delay_ticks
//!   ended 且已过放行宽限  → max_done_tick（放尾拍，见下）
//!   ended 未过宽限        → max_done_tick - delay_ticks
//!   最后统一取 max(算出来的, published_high_tick)  ← 🔴 单调，见下
//! ```
//!
//! - **只数 `status='done'` 的拍**：跑到一半的拍还不是事实，不该进入播出候选。
//! - 🔴 **单调不回退**。运营把 `delay_ticks` 从 1 上调到 5 的那一刻，若边界纯按现算，
//!   已经在观众屏幕上滚过去的 4 拍会**从播出面消失**——那是对已公开内容的回滚。
//!   `live_sessions.published_high_tick` 是这条边界的单调下界：上调延迟**只勒住未来**。
//!   要撤回已播出的内容只有一条显式的、带审计的路径（见下 `withhold`），且如实标注
//!   「这条是播出后撤的，收不回已经看见的」。
//! - **尾拍放行**：世界收播后不再产新拍，缓冲里剩的最后 N 拍会被永久卡住。故 `ended` 之后
//!   再等 `MUSE_LIVE_DRAIN_GRACE_MS`（默认 5 分钟，参数化）把它们放出去——那段宽限就是
//!   尾拍的审核窗口。
//!
//! ### 审核不通过怎么处理
//!
//! 两条路径，**两条都不改写世界事实**：
//!
//! ① **自动**（既有链路，本模块零改动）：§15 第 2 层 `safety::moderate_runtime_projection`
//!    在**落库前**打码 + 置 `world_events.moderation='pending'`；第 3 层异步复核
//!    （`safety` 模块 `TODO(§15-L3)`，其原文就写着「配合 §15 第 4 层直播场延迟缓冲给这条
//!    异步链留出拦截窗口」——**本模块就是它等的那个窗口**）在缓冲期内把 `moderation` 收紧。
//!    播出面与其它读取面同口径，只出 `moderation='approved'`，未过审内容根本进不来。
//!
//! ② **人工**：`POST /admin/live/sessions/{id}/withhold` 把某条**从这一场直播的播出面**撤下。
//!    🔴 它写 `live_withholds` 独立表，**不是 `UPDATE world_events`**：
//!    - 世界事实一个字节不动（红线用例逐字节快照守死）；
//!    - 参赛者的既有读取面完全不受影响——他们的角色刚刚经历了这件事，把它从他们眼前抹掉
//!      才是真正的事实错乱；
//!    - 撤下只作用于这一场（`session_id` 是唯一键的一半），不外溢到战报 / 回放 / 日报。
//!    回执与落库都如实标注 `preemptive`：**播出前拦下**（缓冲生效，观众从未看见）还是
//!    **播出后撤下**（只减少后续可见性，收不回已经看见的）。`preemptive` 占比就是延迟拍数
//!    配得够不够的度量。
//!
//! ### 时间差为什么不造成事实错乱（四条，均落到代码）
//!
//! 1. **延迟只作用于世界外**。成员的 `/worlds/{id}/events` 一个字符不改；延后当事人等于让世界停摆。
//! 2. **播出面公开标注自己是延迟的**：每次响应都带 `delayTicks` / `publishedThroughTick` /
//!    `worldTickNow` / `pendingTicks`，不假装实时。
//! 3. 🔴 **弹幕锚定播出拍而不是世界当前拍**（`anchor_tick` 由服务端按水位线算，**不收客户端传值**）。
//!    观众评论的永远是他当下看见的那一拍，因此弹幕在结构上不可能"剧透"尚未播出的内容，
//!    回放时也与画面严丝合缝。
//! 4. **播出面与观战/回放同口径**：只出 `visibility='public'` 且 `moderation='approved'`，
//!    私有投影永不经过此路径（双硬隔离天然满足）。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 弹幕：UGC，不是世界事实
//! ════════════════════════════════════════════════════════════════════════════
//!
//! - **永不进 `world_events`**。本模块对 `world_events` 只有 SELECT，没有一条 INSERT/UPDATE/DELETE
//!   （源码级红线用例 `red_line_module_never_writes_world_events`）。于是弹幕进不了战报 / 回放 /
//!   日报 / `RoundInput`——观众的一句话不会变成世界里发生过的事，也不影响任何角色的决策
//!   （§0.1「不卖胜负与数值平权」）。
//! - **过审核链**：`safety::mask`（§15 第 2 层词库，就地打码）+ `safety::moderate_and_queue`
//!   （静态 UGC 的唯一入队/记险入口，注入检测 + provider 机审）。非 `approved` 的**落库但不外发**，
//!   人审改判后无需玩家重发。
//! - **限频**：`MUSE_LIVE_DANMAKU_RATE_PER_WINDOW` 条 / `MUSE_LIVE_DANMAKU_WINDOW_MS` 毫秒，
//!   超限 429。🔴 被拒的弹幕**照样计数**——否则刷屏者可以靠发违规内容白嫖额度。
//! - **未成年保护**：`ensure_adult_live` 挂在发弹幕端点的第一行（`ensure_enabled` 之后、
//!   任何读写之前），403 发生在**零副作用**位置。口径与 `social::ensure_adult_social` /
//!   `worlds::join_world` 的生死状门 / `invitations::deathmatch_age_gate_ok` 逐字一致：
//!   只有 `users.age_declared == 1` 放行，未声明(0)/未成年(2)/用户行缺失一律拒。
//!   ⚠️ **只挡写、不挡看**：未成年可以观看直播（观战本就开放），只是不能发弹幕。
//!   推理与 `social` 把拉黑/举报排除在年龄门之外同源——年龄门挡的是**新增的公开发言与接触面**，
//!   不是挡住基本的观看。
//! - **面具**（§14 恨隔面具原则）：响应体里只有服务端派生的场次内代号 `观众xxxx`，
//!   **没有 `userId`、没有昵称、没有手机号**。`live_danmaku.user_id` 只用于限频与风控溯源。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 与错峰调度的红线**互不干涉**（本模块一行都没碰 `runtime`）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `runtime::offpeak` 有一条已实现的红线：**直播场（`room_type='arena'` ∨
//! `tick_per_day >= MUSE_OFFPEAK_LIVE_TICK_PER_DAY`）永不延后**（`Config::is_live_room`，
//! 用例 `offpeak_never_defers_live_room`）。理由是直播是定时的，把它的 tick 排到夜间折扣时段
//! 等于毁掉「今晚 2 小时跑完一阶段」这个产品定义。
//!
//! 本模块**没有引入第二条判据、没有改调度器一行代码**：`live_sessions` 是**播出层**的排期，
//! `offpeak` 管的是**引擎拍**的排期，两者的输入完全不相交（前者读 `live_sessions`，
//! 后者读 `worlds.room_type` / `tick_per_day`）。换句话说，「一个世界有没有直播场次」
//! 不参与、也不应参与错峰豁免判定——豁免判据必须是世界自身的节奏属性，否则运营建一条
//! 定档记录就能顺手改掉一个世界的调度行为，那是两个不该耦合的旋钮。
//! 源码级用例 `red_line_offpeak_live_exemption_untouched` 钉住这一点。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! ④ 转化度量：T5 门槛「观众→玩家转化 ≥2%」的数据源
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 门槛写了却没有数据源，就等于测不了——本仓库有过先例（OOC 申诉率悬空到 R3 才补上
//! `ooc_appeals`）。直播观看在此前全仓**没有任何埋点**。`live_viewers` 是那个真新建件，
//! 口径与三态见 [`conversion_block`]。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 端点
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ```text
//! 玩家/观众面（AuthUser）
//!   GET  /api/live/sessions?status=&cursor=&cursorId=&limit=   节目单（只列已到预告时刻的场次）
//!   GET  /api/live/sessions/{id}                               单场详情（含播出边界与延迟标注）
//!   GET  /api/live/sessions/{id}/feed?cursor=&cursorId=&limit= 播出面（延迟缓冲后的公开事件）+ 记观众足迹
//!   POST /api/live/sessions/{id}/danmaku                       发弹幕（成年门 + 限频 + 审核链）
//!   GET  /api/live/sessions/{id}/danmaku?anchorTick=&cursor=&cursorId=&limit=  弹幕列表
//!
//! 运营面（AdminUser + 细粒度角色）
//!   POST /api/admin/live/sessions                              定档（operator）
//!   POST /api/admin/live/sessions/{id}                         状态迁移 / 延迟拍数调整（operator）
//!   POST /api/admin/live/sessions/{id}/withhold                缓冲窗口内撤下一条（reviewer）
//! ```
//!
//! ### 未验证功能默认关闭（§0.1）
//!
//! 整块能力由运行时开关 **`MUSE_LIVE_STAGE`** 控制，**默认关闭**，经 `crate::flags` 统一入口
//! 解析（链 user > world > global > env > 代码内默认值）。关闭时**全部端点 404 且零副作用**
//! （不是 403：不向外泄露「平台有这个未开放功能」）。fail-closed 由 `flags::is_enabled` 自带。

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::events::{can_view_world, WsMessage};
use crate::pagination::cursor_id_bound;
use crate::providers::ModerationVerdict;
use crate::safety;

#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 运营开关（VALIDATION.md §0.1 未验证功能默认关闭）
// ═══════════════════════════════════════════════════════════════════════════

/// 直播场运行时开关（**开关名即 env 变量名**，见 `flags` 模块头）。
pub(crate) const ENV_LIVE_STAGE: &str = "MUSE_LIVE_STAGE";

/// 默认 = **关闭**。直播场属 VALIDATION §2 **T5** 的开放范围（50-100 人 + 弹幕 + 生死状），
/// 是全部阶段里最晚的一档；代码合并不等于对用户开放。
const DEFAULT_LIVE_STAGE_ENABLED: bool = false;

/// 🔴 **编译期钉死**：默认值出现在两处（本常量 + `flags::KNOWN_FLAGS` 登记表），
/// 两处不一致就是「默认关闭」这条 §0.1 约束有了两个事实源。改一处不改另一处直接编不过。
/// 范式抄 `annotations` / `social` / `onboarding`。
const _: () = assert!(
    crate::flags::declared_default(ENV_LIVE_STAGE) == DEFAULT_LIVE_STAGE_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_LIVE_STAGE 的默认值必须与 DEFAULT_LIVE_STAGE_ENABLED 一致"
);

/// 本模块是否已由运营开启。
///
/// 解析上下文按端点分两档（与 `social` 同型）：
///
/// | 端点 | ctx | 理由 |
/// |---|---|---|
/// | `/live/sessions/{id}/**` | user + world | 场次挂在某个世界上，按世界灰度最自然 |
/// | `/live/sessions`（节目单）| user（无 world）| 节目单跨世界，没有单一 world 坐标 |
///
/// ⚠️ 由此产生一条**运营须知**：若只按 `world` 作用域灰度，观众能进那一场却在节目单里看不到它。
/// **推荐的灰度作用域是 `user` 或 `global`**，`world` 只用于「临时关掉某个出问题世界的直播入口」
/// 这种收窄动作。
///
/// 🔴 fail-closed 由 `flags::is_enabled` 自带：查库失败 / 记录损坏 → 返回声明默认值（关），
/// 且**不再回落 env**。
pub(crate) async fn live_stage_enabled(
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
    crate::flags::is_enabled(db, ENV_LIVE_STAGE, ctx).await
}

/// 开关门：关闭时整块能力**不存在**（404 而非 403）。每个端点第一行都调它，读端点同样调。
async fn ensure_enabled(
    db: &AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
) -> Result<(), ApiError> {
    if live_stage_enabled(db, user_id, world_id).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 入口**是否曾经对任何人开放过**（运营面可见性判据 + [`conversion_block`] 的三态判定，
/// 范式抄 `annotations::entry_ever_open` / `social::entry_ever_open`）。
///
/// 刻意不用全局解析：若运营按世界灰度开了 3 个世界（global 仍为关），那 3 个世界里会真实产生
/// 场次、弹幕与观众足迹，而按全局解析会把运营面判成 404 —— **弹幕进得来、撤不下去**，
/// 审核闭环直接卡死。运营面的可见性必须跟「有没有人能看直播」一致。
///
/// **fail-safe 方向是 false**（查库失败 → 按「没开过」处理 → 转化率报 `—` 而不是 `0%`）。
pub(crate) async fn entry_ever_open(db: &AnyPool) -> bool {
    if live_stage_enabled(db, None, None).await {
        return true;
    }
    let n = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM runtime_flags WHERE flag = $1 AND enabled = 1",
    )
    .bind(ENV_LIVE_STAGE)
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

/// 🔴 **发弹幕的年龄门**：调用者本人必须已声明成年，否则 403。
///
/// 挂在发弹幕端点的第一行（`ensure_enabled` 之后、任何读写之前），因此拒绝发生在**零副作用**
/// 的位置：没有落库、没有消耗限频额度、没有触发机审、没有广播。
/// 口径与 `social::is_adult` / `worlds::join_world` 生死状门 / `invitations::deathmatch_age_gate_ok`
/// **逐字一致**——四处口径必须同源，否则「未成年保护」会有四种不同的含义，其中至少三种是错的。
///
/// 用 403 而非 404：功能确实存在，只是这个账号不被允许（与 `invitations` 的生死状未成年门同码）。
///
/// ⚠️ **只挡发言，不挡观看**：观看直播走 `can_view_world`（观战本就开放）。见模块头。
async fn ensure_adult_live(db: &AnyPool, user_id: &str) -> Result<(), ApiError> {
    let age: Option<(i64,)> = sqlx::query_as("SELECT age_declared FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    if matches!(age, Some((AGE_DECLARED_ADULT,))) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// 细粒度角色守卫。语义与 `admin_api::require_role` 逐字一致（`admin` 为超级用户放行一切）。
/// `admin_api::require_role` 是 `pub(super)`，本模块够不着，故按 `annotations::require_reviewer`
/// 的范式在本地复刻。
fn require_admin_role(admin: &AdminUser, allowed: &[&str]) -> Result<(), ApiError> {
    let role = admin.0.role.as_str();
    if role == "admin" || allowed.contains(&role) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 参数化（VALIDATION.md §0.2 产品规则参数化，禁止写死）
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 下面每一个数都是**待验证假设**，随 §2 的 T5 数据修订。默认值一律取保守方向
//    （延迟宁长勿短、限频宁紧勿松）——放宽随时可以，放出去的内容收不回来。

/// 🔴 **直播延迟拍数**。§15 第 4 层原文是「延迟 1-2 拍」，默认取上限 2。
///
/// 这是 VALIDATION §2 T5 预案「审核成本失控 → **直播延迟拍数上调**」那个运营旋钮的**默认值**；
/// 真正生效的是每场快照进 `live_sessions.delay_ticks` 的那一份（可按场调，见 `update_session`）。
const ENV_DELAY_TICKS: &str = "MUSE_LIVE_DELAY_TICKS";
const DEFAULT_DELAY_TICKS: i64 = 2;
/// 延迟拍数上限（防运营配成天文数字 = 这一场永远不播；也防整数溢出）。
const MAX_DELAY_TICKS: i64 = 1_000;

/// 定档提前量下限：预告公开时刻必须早于开播时刻至少这么久（默认 1 小时）。
/// 「提前多久放出预告」是运营节奏，不是代码常量。
const ENV_ANNOUNCE_LEAD_MS: &str = "MUSE_LIVE_ANNOUNCE_LEAD_MS";
const DEFAULT_ANNOUNCE_LEAD_MS: i64 = 3_600_000;

/// 场次容量默认值（同时进场观看的观众上限；**0 = 不限**）。
const ENV_SESSION_CAPACITY: &str = "MUSE_LIVE_SESSION_CAPACITY";
const DEFAULT_SESSION_CAPACITY: i64 = 0;

/// 收播后的**尾拍放行宽限**（默认 5 分钟）：世界不再产新拍，缓冲里剩的最后 N 拍
/// 若不放行会被永久卡住。这段宽限就是尾拍的审核窗口。
const ENV_DRAIN_GRACE_MS: &str = "MUSE_LIVE_DRAIN_GRACE_MS";
const DEFAULT_DRAIN_GRACE_MS: i64 = 300_000;

/// 弹幕限频：窗口内每人最多几条 / 窗口多长。
const ENV_DANMAKU_RATE: &str = "MUSE_LIVE_DANMAKU_RATE_PER_WINDOW";
const DEFAULT_DANMAKU_RATE: i64 = 20;
const ENV_DANMAKU_WINDOW_MS: &str = "MUSE_LIVE_DANMAKU_WINDOW_MS";
const DEFAULT_DANMAKU_WINDOW_MS: i64 = 60_000;

/// 弹幕正文长度上限（字符数，非字节）。
const ENV_DANMAKU_MAX_LEN: &str = "MUSE_LIVE_DANMAKU_MAX_LEN";
const DEFAULT_DANMAKU_MAX_LEN: i64 = 80;

/// T5 门槛「观众→玩家转化 **≥2%**」的门槛值。原文数值作为**默认值**而非常量语义（§0.2）。
const ENV_CONVERSION_MIN: &str = "MUSE_LIVE_CONVERSION_MIN";
const DEFAULT_CONVERSION_MIN: f64 = 0.02;

/// 分页上限（节目单 / 播出面 / 弹幕列表共用的钳制口径）。
const DEFAULT_PAGE_LIMIT: i64 = 50;
const MAX_PAGE_LIMIT: i64 = 200;

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(default)
}

/// 默认延迟拍数（钳到 `[0, MAX_DELAY_TICKS]`；负值配置一律按 0 处理，不静默变成"提前播"）。
fn default_delay_ticks() -> i64 {
    env_i64(ENV_DELAY_TICKS, DEFAULT_DELAY_TICKS).clamp(0, MAX_DELAY_TICKS)
}
fn announce_lead_ms() -> i64 {
    env_i64(ENV_ANNOUNCE_LEAD_MS, DEFAULT_ANNOUNCE_LEAD_MS).max(0)
}
fn default_capacity() -> i64 {
    env_i64(ENV_SESSION_CAPACITY, DEFAULT_SESSION_CAPACITY).max(0)
}
fn drain_grace_ms() -> i64 {
    env_i64(ENV_DRAIN_GRACE_MS, DEFAULT_DRAIN_GRACE_MS).max(0)
}
fn danmaku_rate_per_window() -> i64 {
    env_i64(ENV_DANMAKU_RATE, DEFAULT_DANMAKU_RATE).max(1)
}
fn danmaku_window_ms() -> i64 {
    env_i64(ENV_DANMAKU_WINDOW_MS, DEFAULT_DANMAKU_WINDOW_MS).max(1)
}
fn danmaku_max_len() -> usize {
    env_i64(ENV_DANMAKU_MAX_LEN, DEFAULT_DANMAKU_MAX_LEN).clamp(1, 1_000) as usize
}
fn conversion_min() -> f64 {
    env_f64(ENV_CONVERSION_MIN, DEFAULT_CONVERSION_MIN)
}
fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

// ═══════════════════════════════════════════════════════════════════════════
// 状态机与场次读取
// ═══════════════════════════════════════════════════════════════════════════

const STATUS_SCHEDULED: &str = "scheduled";
const STATUS_LIVE: &str = "live";
const STATUS_ENDED: &str = "ended";
const STATUS_CANCELED: &str = "canceled";

/// 合法状态迁移（单向，无回环）：
/// `scheduled → live | canceled`；`live → ended`；`ended` / `canceled` 为终局。
fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (STATUS_SCHEDULED, STATUS_LIVE)
            | (STATUS_SCHEDULED, STATUS_CANCELED)
            | (STATUS_LIVE, STATUS_ENDED)
    )
}

#[derive(Debug, Clone)]
struct Session {
    id: String,
    world_id: String,
    title: String,
    status: String,
    announce_at: i64,
    starts_at: i64,
    ends_at: i64,
    delay_ticks: i64,
    published_high_tick: i64,
    capacity: i64,
    started_at: i64,
    ended_at: i64,
}

const SESSION_COLUMNS: &str = "id, world_id, title, status, announce_at, starts_at, ends_at, \
                               delay_ticks, published_high_tick, capacity, started_at, ended_at";

fn row_to_session(row: &sqlx::any::AnyRow) -> Result<Session, ApiError> {
    Ok(Session {
        id: row.try_get("id")?,
        world_id: row.try_get("world_id")?,
        title: row.try_get("title")?,
        status: row.try_get("status")?,
        announce_at: row.try_get("announce_at")?,
        starts_at: row.try_get("starts_at")?,
        ends_at: row.try_get("ends_at")?,
        delay_ticks: row.try_get("delay_ticks")?,
        published_high_tick: row.try_get("published_high_tick")?,
        capacity: row.try_get("capacity")?,
        started_at: row.try_get("started_at")?,
        ended_at: row.try_get("ended_at")?,
    })
}

/// 读一场。不存在 → 404。
///
/// ⚠️ 这是一次**纯 SELECT**，且发生在开关门之前——因为开关要按世界灰度解析就必须先知道
/// `world_id`，而它只能从这张表读出来。「零副作用」约束禁的是**写**，不是读；且开关关闭时
/// 与场次不存在时返回的都是 404，外部无法据此区分（不泄露「平台有这个未开放功能」）。
async fn load_session(db: &AnyPool, id: &str) -> Result<Session, ApiError> {
    let row = sqlx::query(&format!("SELECT {SESSION_COLUMNS} FROM live_sessions WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    row_to_session(&row)
}

/// 世界当前**已完成**的最大拍号（无已完成拍 → None）。
///
/// 只数 `status='done'`：跑到一半的拍还不是事实，不该进入播出候选。
async fn max_done_tick(db: &AnyPool, world_id: &str) -> Result<Option<i64>, ApiError> {
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(tick_no) FROM world_ticks WHERE world_id = $1 AND status = 'done'",
    )
    .bind(world_id)
    .fetch_one(db)
    .await?;
    Ok(v)
}

/// 🔴 **播出水位线**（延迟缓冲的全部数学）。返回 `(可播到第几拍, 世界当前已完成拍)`。
///
/// 规则见模块头。四点复述在此，因为这是本模块最容易改错的函数：
/// 1. `scheduled` / `canceled` → `None`（一拍不播）；
/// 2. `live` → `max_done_tick - delay_ticks`；
/// 3. `ended` 且 `now >= ended_at + drain_grace_ms` → `max_done_tick`（尾拍放行）；
/// 4. 🔴 最后统一 `max(算出来的, published_high_tick)`——**已播出的绝不缩回**。
async fn publish_watermark(
    db: &AnyPool,
    s: &Session,
    now: i64,
) -> Result<(Option<i64>, Option<i64>), ApiError> {
    let latest = max_done_tick(db, &s.world_id).await?;
    if s.status == STATUS_SCHEDULED || s.status == STATUS_CANCELED {
        return Ok((None, latest));
    }
    let Some(latest_done) = latest else {
        return Ok((None, None));
    };
    let drained = s.status == STATUS_ENDED && s.ended_at > 0 && now >= s.ended_at + drain_grace_ms();
    let computed = if drained { latest_done } else { latest_done - s.delay_ticks };
    // 🔴 单调：上调延迟只勒住未来，不追溯已播出的部分。
    let effective = computed.max(s.published_high_tick);
    Ok((if effective < 0 { None } else { Some(effective) }, Some(latest_done)))
}

/// 推进单调水位线（只进不退）。
///
/// 条件 UPDATE 而不是先读后写：`WHERE published_high_tick < $1` 使并发下两个请求同时推进也
/// 只会收敛到较大者，不会出现"后写的把水位线拉回去"。
async fn advance_high_tick(db: &AnyPool, session_id: &str, watermark: i64) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE live_sessions SET published_high_tick = $1, updated_at = $2 \
         WHERE id = $3 AND published_high_tick < $4",
    )
    .bind(watermark)
    .bind(now_ms())
    .bind(session_id)
    .bind(watermark)
    .execute(db)
    .await?;
    Ok(())
}

pub fn router() -> Router<AppState> {
    Router::new()
        // 观众面
        .route("/live/sessions", get(list_sessions))
        .route("/live/sessions/{id}", get(get_session))
        .route("/live/sessions/{id}/feed", get(get_feed))
        .route("/live/sessions/{id}/danmaku", get(list_danmaku).post(post_danmaku))
        // 运营面
        .route("/admin/live/sessions", post(create_session))
        .route("/admin/live/sessions/{id}", post(update_session))
        .route("/admin/live/sessions/{id}/withhold", post(withhold_event))
}

// ═══════════════════════════════════════════════════════════════════════════
// ① 定档：节目单与单场详情
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionsQuery {
    status: Option<String>,
    /// 游标第一段：上一页末行的 `startsAt`。
    cursor: Option<i64>,
    /// 🔴 游标第二段：上一页末行的 `id`。见 `crate::pagination` —— 单列游标在并列行横跨
    /// 页边界时会**永久丢行**，而"同一时刻开播的两场"完全可能发生（整点定档是常态）。
    cursor_id: Option<String>,
    limit: Option<i64>,
}

/// 升序复合游标的退化下界。
///
/// `pagination::cursor_id_bound` 是给**降序**分页写的：缺 `cursorId` 时用空串当恒假下界
/// （`id < ''` 恒假）。升序这一侧没有对称的"恒假上界"（`id > ''` 对任何 id 恒真），
/// 于是缺省时的安全退化方向**翻了个个儿**：不是"少发"，而是**重发**该 `cursor` 值上的整组并列行。
/// 重复由客户端按 `id` 去重即可，而丢行是不可恢复的数据损失——两害相权取重发。
/// 本模块所有升序分页（节目单、播出面）都返回成对的 `nextCursor` + `nextCursorId`，
/// 正常客户端不会走到这条退化路径上。
fn asc_cursor_id_bound(cursor_id: Option<&str>) -> String {
    cursor_id.unwrap_or("").to_string()
}

/// `GET /live/sessions` —— 节目单。
///
/// 只列出**已到预告时刻**（`announce_at <= now`）的场次：定档的产品意义就是"到点了才公开"，
/// 提前泄露排期等于没有预告。
async fn list_sessions(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<SessionsQuery>,
) -> Result<Json<Value>, ApiError> {
    // 节目单跨世界，无单一 world 坐标 → 按 user/global 解析（运营须知见 `live_stage_enabled`）。
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;

    let limit = clamp_limit(q.limit);
    let any_status = if q.status.is_some() { 0_i64 } else { 1 };
    let status = q.status.clone().unwrap_or_default();
    // i64::MIN 作"从头开始"的下界：任何真实 starts_at 都大于它。
    let cursor = q.cursor.unwrap_or(i64::MIN);
    let cursor_id = asc_cursor_id_bound(q.cursor_id.as_deref());

    let rows = sqlx::query(&format!(
        "SELECT {SESSION_COLUMNS} FROM live_sessions \
         WHERE announce_at <= $1 AND ($2 = 1 OR status = $3) \
           AND (starts_at > $4 OR (starts_at = $5 AND id > $6)) \
         ORDER BY starts_at ASC, id ASC LIMIT $7"
    ))
    .bind(now_ms())
    .bind(any_status)
    .bind(&status)
    .bind(cursor)
    .bind(cursor)
    .bind(&cursor_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    let mut items = Vec::new();
    let mut next: Option<(i64, String)> = None;
    for row in &rows {
        let s = row_to_session(row)?;
        next = Some((s.starts_at, s.id.clone()));
        items.push(json!({
            "id": s.id,
            "worldId": s.world_id,
            "title": s.title,
            "status": s.status,
            "announceAt": s.announce_at,
            "startsAt": s.starts_at,
            "endsAt": s.ends_at,
            // 延迟拍数对观众公开：直播场明说自己是延迟的，不假装实时（§15 第 4 层的诚实标注）。
            "delayTicks": s.delay_ticks,
            "capacity": s.capacity,
        }));
    }

    Ok(Json(json!({
        "sessions": items,
        "nextCursor": next.as_ref().map(|(c, _)| *c),
        "nextCursorId": next.as_ref().map(|(_, i)| i.clone()),
        "notes": [
            "节目单只列出已到预告时刻（announceAt <= now）的场次；定档提前量由 MUSE_LIVE_ANNOUNCE_LEAD_MS 约束。",
            "游标是复合键 (startsAt, id)：翻页请把 nextCursor 与 nextCursorId 一起回传，只传前者会重发同刻并列的场次。",
        ],
    })))
}

/// `GET /live/sessions/{id}` —— 单场详情（含播出边界与延迟标注）。
async fn get_session(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let s = load_session(&state.db, &id).await?;
    ensure_enabled(&state.db, Some(&user.user_id), Some(&s.world_id)).await?;
    if !can_view_world(&state.db, &s.world_id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }
    let (watermark, latest) = publish_watermark(&state.db, &s, now_ms()).await?;
    let viewers: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM live_viewers WHERE session_id = $1",
    )
    .bind(&s.id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(session_view(&s, watermark, latest, viewers)))
}

fn session_view(s: &Session, watermark: Option<i64>, latest: Option<i64>, viewers: i64) -> Value {
    let pending = match (watermark, latest) {
        (Some(w), Some(l)) => (l - w).max(0),
        (None, Some(l)) => l + 1,
        _ => 0,
    };
    json!({
        "id": s.id,
        "worldId": s.world_id,
        "title": s.title,
        "status": s.status,
        "announceAt": s.announce_at,
        "startsAt": s.starts_at,
        "endsAt": s.ends_at,
        "startedAt": s.started_at,
        "endedAt": s.ended_at,
        "capacity": s.capacity,
        "viewerCount": viewers,
        // 🔴 延迟缓冲的对外全貌：观众看得见"我落后世界几拍"，不假装实时。
        "broadcast": {
            "delayTicks": s.delay_ticks,
            "publishedThroughTick": watermark,
            "worldTickNow": latest,
            "pendingTicks": pending,
            "notes": [
                "直播播出面落后世界 delayTicks 拍，这段时间差是内容审核窗口（总规格 §15 第 4 层），不是网络延迟。",
                "世界事实在拍提交时即已落定（§0.3 公共事实不可回滚）；延迟的是公开投影的播出时刻，不是事实本身。",
                "世界成员的 /api/worlds/{id}/events 不受本延迟影响——延后当事人等于让世界停摆。",
                "publishedThroughTick 单调不回退：上调 delayTicks 只勒住未来，已播出的不缩回。",
            ],
        },
        "aiLabel": { "visible": true },
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// ② 延迟缓冲：播出面
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedQuery {
    cursor: Option<i64>,
    cursor_id: Option<String>,
    limit: Option<i64>,
}

/// `GET /live/sessions/{id}/feed` —— 直播播出面。
///
/// 三重过滤，缺一不可：
/// 1. **延迟缓冲**：`tick_no <= 播出水位线`（本模块的核心）；
/// 2. **双硬隔离**：`visibility='public'`（任一 principal 的私有投影永不经过此路径，与
///    `arena::get_replay` 同口径）；
/// 3. **审核门**：`moderation='approved'`（§15 第 2/3 层拦下的内容不外发）
///    + `NOT EXISTS live_withholds`（运营在缓冲窗口内人工撤下的）。
///
/// 副作用（唯一一处）：记一行**观众足迹**（`live_viewers`），它是 T5「观众→玩家转化」
/// 门槛的唯一数据源。开关关闭时本函数在写入之前就已 404 返回，故零副作用。
async fn get_feed(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(q): Query<FeedQuery>,
) -> Result<Json<Value>, ApiError> {
    let s = load_session(&state.db, &id).await?;
    ensure_enabled(&state.db, Some(&user.user_id), Some(&s.world_id)).await?;
    if !can_view_world(&state.db, &s.world_id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }

    let now = now_ms();
    // 场次容量（§0.2 参数化）：已在场的老观众不再受限，新观众撞上限 → 409。
    // 判定在任何写入之前，被拒请求零副作用。
    let already_viewer = sqlx::query("SELECT 1 AS x FROM live_viewers WHERE session_id = $1 AND user_id = $2")
        .bind(&s.id)
        .bind(&user.user_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();
    if s.capacity > 0 && !already_viewer {
        let viewers: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(COUNT(*) AS BIGINT) FROM live_viewers WHERE session_id = $1",
        )
        .bind(&s.id)
        .fetch_one(&state.db)
        .await?;
        if viewers >= s.capacity {
            return Err(ApiError::Conflict("session_full".into()));
        }
    }

    let (watermark, latest) = publish_watermark(&state.db, &s, now).await?;
    record_viewer(&state, &s, &user.user_id, now, already_viewer).await?;

    let mut events: Vec<Value> = Vec::new();
    let mut next: Option<(i64, String)> = None;
    if let Some(watermark) = watermark {
        let limit = clamp_limit(q.limit);
        let cursor = q.cursor.unwrap_or(-1);
        let cursor_id = asc_cursor_id_bound(q.cursor_id.as_deref());
        let rows = sqlx::query(
            "SELECT we.id, we.tick_no, we.sequence, we.event_type, we.actors_json, \
                    we.public_projection_json, we.arbiter_note, we.occurred_at \
             FROM world_events we \
             WHERE we.world_id = $1 AND we.visibility = 'public' AND we.moderation = 'approved' \
               AND we.tick_no <= $2 \
               AND (we.sequence > $3 OR (we.sequence = $4 AND we.id > $5)) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM live_withholds lw WHERE lw.session_id = $6 AND lw.event_id = we.id \
               ) \
             ORDER BY we.sequence ASC, we.id ASC LIMIT $7",
        )
        .bind(&s.world_id)
        .bind(watermark)
        .bind(cursor)
        .bind(cursor)
        .bind(&cursor_id)
        .bind(&s.id)
        .bind(limit)
        .fetch_all(&state.db)
        .await?;

        for r in &rows {
            let eid: String = r.try_get("id")?;
            let sequence: i64 = r.try_get("sequence")?;
            let tick_no: i64 = r.try_get("tick_no")?;
            let actors_json: String = r.try_get("actors_json")?;
            let pj: Option<String> = r.try_get("public_projection_json")?;
            let proj: Value =
                pj.and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| json!({}));
            next = Some((sequence, eid.clone()));
            events.push(json!({
                "id": eid,
                "tick": tick_no,
                "sequence": sequence,
                "type": r.try_get::<String, _>("event_type")?,
                "actors": serde_json::from_str::<Value>(&actors_json).unwrap_or_else(|_| json!([])),
                "summary": proj.get("summary").cloned().unwrap_or_else(|| json!("")),
                "projection": proj,
                "occurredAt": r.try_get::<i64, _>("occurred_at")?,
                // AI 生成标识（§16，与 events / arena 读取面同口径）。
                "aiLabel": { "visible": true },
            }));
        }
        advance_high_tick(&state.db, &s.id, watermark).await?;
    }

    let pending = match (watermark, latest) {
        (Some(w), Some(l)) => (l - w).max(0),
        (None, Some(l)) => l + 1,
        _ => 0,
    };
    Ok(Json(json!({
        "sessionId": s.id,
        "worldId": s.world_id,
        "status": s.status,
        "events": events,
        "nextCursor": next.as_ref().map(|(c, _)| *c),
        "nextCursorId": next.as_ref().map(|(_, i)| i.clone()),
        "broadcast": {
            "delayTicks": s.delay_ticks,
            "publishedThroughTick": watermark,
            "worldTickNow": latest,
            "pendingTicks": pending,
        },
        "notes": [
            "本面只出 visibility='public' 且 moderation='approved' 的事件（双硬隔离 + 审核门），与观战/回放同口径。",
            "延迟缓冲：tick_no <= publishedThroughTick 才播出，缓冲期是 §15 第 2/3 层的拦截窗口。",
            "缓冲期内被审核判否或被运营撤下的事件不会出现在本面；世界事实本身一个字节未改（§0.3）。",
            "游标是复合键 (sequence, id)：翻页请把 nextCursor 与 nextCursorId 一起回传。",
        ],
    })))
}

/// 记一行观众足迹（T5 转化率的数据源）。
///
/// 🔴 `was_player` **在首次写入那一刻冻结**：转化率问的是「当时还不是玩家的观众里，
/// 后来有多少入了场」。若在统计时现算，那些看完就入场的人会因为"现在是玩家了"
/// 而被移出分母，分子分母一起缩水，转化率被系统性低估。
///
/// 幂等：唯一索引 `(session_id, user_id)` + `ON CONFLICT DO NOTHING`，随后无条件刷新
/// `last_seen_at`。老观众只走一条 UPDATE，不重复判定"是不是玩家"。
async fn record_viewer(
    state: &AppState,
    s: &Session,
    user_id: &str,
    now: i64,
    already_viewer: bool,
) -> Result<(), ApiError> {
    if !already_viewer {
        let is_player = sqlx::query("SELECT 1 AS x FROM world_members WHERE user_id = $1 LIMIT 1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?
            .is_some();
        sqlx::query(
            "INSERT INTO live_viewers (id, session_id, world_id, user_id, was_player, first_seen_at, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT(session_id, user_id) DO NOTHING",
        )
        .bind(new_id("lvw"))
        .bind(&s.id)
        .bind(&s.world_id)
        .bind(user_id)
        .bind(if is_player { 1_i64 } else { 0 })
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
    }
    sqlx::query("UPDATE live_viewers SET last_seen_at = $1 WHERE session_id = $2 AND user_id = $3")
        .bind(now)
        .bind(&s.id)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// ③ 弹幕
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DanmakuReq {
    body: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DanmakuQuery {
    /// 只取锚定在某一播出拍上的弹幕（回放对齐用）。
    anchor_tick: Option<i64>,
    cursor: Option<i64>,
    cursor_id: Option<String>,
    limit: Option<i64>,
}

/// 场次内面具（§14 恨隔面具原则）。
///
/// `sha256(session_id . ':' . user_id)` 前 4 个 hex 字符 → `观众a3f1`。三条性质：
/// - 同一个人在同一场里**稳定**（观众之间能认出"又是他"，这是弹幕的社交价值）；
/// - **跨场不可关联**（掺了 session_id，换一场就是另一个代号）；
/// - **不可逆**且不含任何真身信息（不记昵称、不记手机号、响应体里没有 userId）。
fn masked_handle(session_id: &str, user_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(session_id.as_bytes());
    h.update(b":");
    h.update(user_id.as_bytes());
    let digest = format!("{:x}", h.finalize());
    format!("观众{}", &digest[..4])
}

/// `POST /live/sessions/{id}/danmaku` —— 发弹幕。
///
/// 守卫次序（每一步都在任何写入之前，被拒请求零副作用）：
/// `场次存在 → 开关门(404) → 🔴 成年门(403) → 观战资格(403) → 直播中(409) →
///  正文校验(400) → 限频(429) → 审核链 → 落库 → 仅 approved 广播`
async fn post_danmaku(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<DanmakuReq>,
) -> Result<Json<Value>, ApiError> {
    let s = load_session(&state.db, &id).await?;
    ensure_enabled(&state.db, Some(&user.user_id), Some(&s.world_id)).await?;
    // 🔴 未成年门在最前面（`ensure_enabled` 之后、任何读写之前）。
    ensure_adult_live(&state.db, &user.user_id).await?;
    if !can_view_world(&state.db, &s.world_id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }
    // 弹幕只在直播中可发：未开播时没有可锚定的播出拍，收播后再发也无处对齐。
    if s.status != STATUS_LIVE {
        return Err(ApiError::Conflict(format!("场次当前为 {}，仅直播中可发弹幕", s.status)));
    }

    let text = body.body.trim();
    if text.is_empty() {
        return Err(ApiError::BadRequest("弹幕内容不能为空".into()));
    }
    let max_len = danmaku_max_len();
    if text.chars().count() > max_len {
        return Err(ApiError::BadRequest(format!("弹幕最长 {max_len} 字")));
    }

    // 限频（§0.2 参数化）。🔴 分母含被拒的弹幕——否则刷屏者可以靠发违规内容白嫖额度。
    let now = now_ms();
    let window_ms = danmaku_window_ms();
    let rate = danmaku_rate_per_window();
    let recent: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM live_danmaku WHERE user_id = $1 AND created_at > $2",
    )
    .bind(&user.user_id)
    .bind(now - window_ms)
    .fetch_one(&state.db)
    .await?;
    if recent >= rate {
        return Err(ApiError::TooManyRequests(format!(
            "弹幕发送过于频繁（{window_ms} 毫秒内最多 {rate} 条）"
        )));
    }

    // 审核链（两道，缺一不可）：
    // ① §15 第 2 层运行时词库 `safety::mask` —— 就地打码。落库的是**打码后的文本**
    //    （§0.3：落库即最终内容，不做事后回写）。
    // ② `safety::moderate_and_queue` —— 静态 UGC 的**唯一**入队/记险入口（注入检测 + provider 机审）。
    //    🔴 传进去的是**原文**而不是打码后的文本：注入检测要看真话，`***` 会把注入指令的句式
    //    特征抹平，等于用第 2 层把第 3 层的眼睛蒙上。落库的仍是打码版，两者互不干扰。
    let (masked, hits) = safety::mask(text);
    let danmaku_id = new_id("dmk");
    let verdict = safety::moderate_and_queue(&state, "live_danmaku", &danmaku_id, text).await?;
    // 🔴 **词库命中 → 打码后仍不外发，置 pending 转人审**。
    //
    // 方向与 `safety::moderate_runtime_projection`（命中 → pending）一致，但处置更硬：
    // 那边是模型产出的叙事正文，拦下会在世界线上留个洞，所以只能打码放行；
    // 一条弹幕拦下来零代价，而它是**实时公开发言面**——命中词库说明发言者在试探边界，
    // 对着几十上百个观众放行一条打了码但意图明确的挑衅，收益为负。
    //
    // ⚠️ 诚实标注：词库命中**不额外写 `risk_events` / `audit_queue`** —— 那两张表的写入权
    // 专属于 `safety` 的两条入口（模块头契约），本模块不得绕过。词库命中的留痕面是
    // `live_danmaku.moderation='pending'` 这一行本身，运营可按 session 检索。
    let moderation = if !hits.is_empty() && verdict == ModerationVerdict::Approved {
        safety::verdict_str(ModerationVerdict::Pending)
    } else {
        safety::verdict_str(verdict)
    };
    let delivered = moderation == crate::events::MODERATION_APPROVED;

    // 🔴 锚定播出拍：由服务端按当前水位线算，**不接受客户端传值**——否则观众可以把弹幕
    // 锚到尚未播出的拍上，等于替世界剧透（见模块头「时间差为什么不造成事实错乱」第 3 条）。
    let (watermark, _) = publish_watermark(&state.db, &s, now).await?;
    let anchor_tick = watermark.unwrap_or(-1);
    let display_name = masked_handle(&s.id, &user.user_id);

    sqlx::query(
        "INSERT INTO live_danmaku (id, session_id, world_id, user_id, display_name, body, \
         anchor_tick, moderation, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&danmaku_id)
    .bind(&s.id)
    .bind(&s.world_id)
    .bind(&user.user_id)
    .bind(&display_name)
    .bind(&masked)
    .bind(anchor_tick)
    .bind(moderation)
    .bind(now)
    .execute(&state.db)
    .await?;

    // 仅过审的才外发。复用既有 `events::WsHub`（不另起一套推送）：`audience_user_ids=None`
    // = public 广播，与世界事件走同一条世界通道；载荷带 `channel="live_danmaku"` 供客户端分流。
    // 🔴 它**不进 `world_events`**——弹幕不是世界事实（见模块头）。
    if delivered {
        state.ws_hub.publish(WsMessage {
            world_id: s.world_id.clone(),
            audience_user_ids: None,
            payload_json: json!({
                "channel": "live_danmaku",
                "type": "live_danmaku",
                "sessionId": s.id,
                "worldId": s.world_id,
                "id": danmaku_id,
                "displayName": display_name,
                "body": masked,
                "anchorTick": anchor_tick,
                "createdAt": now,
                // 弹幕是人写的：不打 AI 标识，也不假装是世界事实。
                "aiGenerated": false,
                "isWorldFact": false,
            })
            .to_string(),
        });
    }

    Ok(Json(json!({
        "id": danmaku_id,
        "sessionId": s.id,
        "displayName": display_name,
        "body": masked,
        "anchorTick": anchor_tick,
        "moderation": moderation,
        "delivered": delivered,
        "isWorldFact": false,
        "rateLimit": { "windowMs": window_ms, "maxPerWindow": rate, "usedInWindow": recent + 1 },
    })))
}

/// `GET /live/sessions/{id}/danmaku` —— 弹幕列表（新的在前）。
///
/// 🔴 复合游标 `(created_at DESC, id DESC)`：弹幕是**同毫秒批量写入的重灾区**
/// （一场直播里几十个人同时发），单列 `created_at` 游标在并列行横跨页边界时会**永久丢行**
/// （见 `crate::pagination` 模块头）。第二段缺省走 `cursor_id_bound` 的恒假下界，
/// 逐字节退化为旧的单列语义。
async fn list_danmaku(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(q): Query<DanmakuQuery>,
) -> Result<Json<Value>, ApiError> {
    let s = load_session(&state.db, &id).await?;
    ensure_enabled(&state.db, Some(&user.user_id), Some(&s.world_id)).await?;
    if !can_view_world(&state.db, &s.world_id, &user.user_id).await? {
        return Err(ApiError::Forbidden);
    }

    let limit = clamp_limit(q.limit);
    let any_anchor = if q.anchor_tick.is_some() { 1_i64 } else { 0 };
    let anchor = q.anchor_tick.unwrap_or(0);
    let cursor = q.cursor.unwrap_or(i64::MAX);
    let cursor_id = cursor_id_bound(q.cursor_id.as_deref());

    let rows = sqlx::query(
        "SELECT id, display_name, body, anchor_tick, created_at FROM live_danmaku \
         WHERE session_id = $1 AND moderation = 'approved' \
           AND ($2 = 0 OR anchor_tick = $3) \
           AND (created_at < $4 OR (created_at = $5 AND id < $6)) \
         ORDER BY created_at DESC, id DESC LIMIT $7",
    )
    .bind(&s.id)
    .bind(any_anchor)
    .bind(anchor)
    .bind(cursor)
    .bind(cursor)
    .bind(&cursor_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    let mut items = Vec::new();
    let mut next: Option<(i64, String)> = None;
    for r in &rows {
        let did: String = r.try_get("id")?;
        let created_at: i64 = r.try_get("created_at")?;
        next = Some((created_at, did.clone()));
        items.push(json!({
            "id": did,
            // 🔴 只有面具，没有 userId / 昵称 / 手机号（§14）。
            "displayName": r.try_get::<String, _>("display_name")?,
            "body": r.try_get::<String, _>("body")?,
            "anchorTick": r.try_get::<i64, _>("anchor_tick")?,
            "createdAt": created_at,
            "isWorldFact": false,
        }));
    }

    Ok(Json(json!({
        "sessionId": s.id,
        "danmaku": items,
        "nextCursor": next.as_ref().map(|(c, _)| *c),
        "nextCursorId": next.as_ref().map(|(_, i)| i.clone()),
        "notes": [
            "只出过审弹幕（moderation='approved'）；未过审的落库但不外发，人审改判后无需玩家重发。",
            "anchorTick = 发言时观众看到的那一拍（服务端按播出水位线算），回放按它与画面对齐。",
            "弹幕是观众 UGC，不是世界事实：永不进 world_events / 战报 / 回放 / 日报 / 引擎决策。",
            "游标是复合键 (createdAt, id)：翻页请把 nextCursor 与 nextCursorId 一起回传，只传前者在同毫秒并列时会丢行。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// 运营面：定档 / 状态迁移 / 缓冲窗口内撤下
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionReq {
    world_id: String,
    #[serde(default)]
    title: String,
    starts_at: i64,
    #[serde(default)]
    announce_at: Option<i64>,
    #[serde(default)]
    ends_at: Option<i64>,
    #[serde(default)]
    delay_ticks: Option<i64>,
    #[serde(default)]
    capacity: Option<i64>,
}

/// `POST /admin/live/sessions` —— 定档（operator 档）。
///
/// 校验：世界存在 · 开播时刻在未来 · **预告提前量 ≥ `MUSE_LIVE_ANNOUNCE_LEAD_MS`**
/// （定档的产品意义是让观众来得及安排时间，提前量太短等于没定档）。
async fn create_session(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(body): Json<CreateSessionReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_ops_enabled(&state.db).await?;
    require_admin_role(&admin, &["operator"])?;

    // 世界必须存在（不给不存在的世界定档）。
    let _world = crate::worlds::load_world(&state.db, &body.world_id).await?;

    let now = now_ms();
    if body.starts_at <= now {
        return Err(ApiError::BadRequest("开播时刻必须在未来".into()));
    }
    let lead = announce_lead_ms();
    let announce_at = body.announce_at.unwrap_or(body.starts_at - lead);
    if body.starts_at - announce_at < lead {
        return Err(ApiError::BadRequest(format!(
            "预告必须早于开播至少 {lead} 毫秒（MUSE_LIVE_ANNOUNCE_LEAD_MS）"
        )));
    }
    let delay_ticks = body.delay_ticks.unwrap_or_else(default_delay_ticks).clamp(0, MAX_DELAY_TICKS);
    let capacity = body.capacity.unwrap_or_else(default_capacity).max(0);

    let id = new_id("lvs");
    sqlx::query(
        "INSERT INTO live_sessions (id, world_id, title, status, announce_at, starts_at, ends_at, \
         delay_ticks, published_high_tick, capacity, created_by, started_at, ended_at, created_at, updated_at) \
         VALUES ($1, $2, $3, 'scheduled', $4, $5, $6, $7, -1, $8, $9, 0, 0, $10, $11)",
    )
    .bind(&id)
    .bind(&body.world_id)
    .bind(&body.title)
    .bind(announce_at)
    .bind(body.starts_at)
    .bind(body.ends_at.unwrap_or(0))
    .bind(delay_ticks)
    .bind(capacity)
    .bind(&admin.0.user_id)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    audit(
        &state.db,
        &admin,
        "live.session.create",
        &id,
        &format!("world={} startsAt={} delayTicks={delay_ticks}", body.world_id, body.starts_at),
    )
    .await?;

    let s = load_session(&state.db, &id).await?;
    Ok(Json(session_view(&s, None, None, 0)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSessionReq {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    delay_ticks: Option<i64>,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /admin/live/sessions/{id}` —— 状态迁移 + 延迟拍数调整（operator 档）。
///
/// 🔴 **`delayTicks` 就是 VALIDATION §2 T5 预案「审核成本失控 → 直播延迟拍数上调」那个旋钮。**
/// 上调立即收紧未来的播出边界；**已播出的不受影响**（`published_high_tick` 单调守住）。
/// 下调是放宽方向（缩短审核窗口），同样允许但一并落审计——运营要为它负责。
async fn update_session(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(body): Json<UpdateSessionReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_ops_enabled(&state.db).await?;
    require_admin_role(&admin, &["operator"])?;
    let s = load_session(&state.db, &id).await?;
    let now = now_ms();

    if let Some(delay) = body.delay_ticks {
        if !(0..=MAX_DELAY_TICKS).contains(&delay) {
            return Err(ApiError::BadRequest(format!("延迟拍数须在 0..={MAX_DELAY_TICKS}")));
        }
        sqlx::query("UPDATE live_sessions SET delay_ticks = $1, updated_at = $2 WHERE id = $3")
            .bind(delay)
            .bind(now)
            .bind(&s.id)
            .execute(&state.db)
            .await?;
        audit(
            &state.db,
            &admin,
            "live.session.delay",
            &s.id,
            &format!(
                "{} → {} 拍；{}",
                s.delay_ticks,
                delay,
                body.reason.as_deref().unwrap_or("(无理由)")
            ),
        )
        .await?;
    }

    if let Some(to) = body.status.as_deref() {
        if !transition_allowed(&s.status, to) {
            return Err(ApiError::Conflict(format!("不允许的状态迁移：{} → {to}", s.status)));
        }
        // 状态与时刻同一条 UPDATE：`started_at` / `ended_at` 参与尾拍放行判定，
        // 与状态分两条写会出现「已 ended 但 ended_at=0」的中间态，那会让尾拍永不放行。
        let (started_at, ended_at) = match to {
            STATUS_LIVE => (now, 0),
            STATUS_ENDED => (s.started_at, now),
            _ => (s.started_at, s.ended_at),
        };
        sqlx::query(
            "UPDATE live_sessions SET status = $1, started_at = $2, ended_at = $3, updated_at = $4 \
             WHERE id = $5 AND status = $6",
        )
        .bind(to)
        .bind(started_at)
        .bind(ended_at)
        .bind(now)
        .bind(&s.id)
        // CAS：并发下只有一个请求能完成这次迁移，另一个读到新状态后被 `transition_allowed` 拒。
        .bind(&s.status)
        .execute(&state.db)
        .await?;
        audit(
            &state.db,
            &admin,
            "live.session.status",
            &s.id,
            &format!("{} → {to}；{}", s.status, body.reason.as_deref().unwrap_or("(无理由)")),
        )
        .await?;

        // 状态变化广播（复用 WsHub；**不落 world_events**——开播/收播是播出层的事，不是世界事实）。
        state.ws_hub.publish(WsMessage {
            world_id: s.world_id.clone(),
            audience_user_ids: None,
            payload_json: json!({
                "channel": "live_session",
                "type": "live_status",
                "sessionId": s.id,
                "worldId": s.world_id,
                "status": to,
                "at": now,
                "isWorldFact": false,
            })
            .to_string(),
        });
    }

    let s = load_session(&state.db, &id).await?;
    let (watermark, latest) = publish_watermark(&state.db, &s, now).await?;
    Ok(Json(session_view(&s, watermark, latest, 0)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WithholdReq {
    event_id: String,
    #[serde(default)]
    reason: String,
}

/// `POST /admin/live/sessions/{id}/withhold` —— 缓冲窗口内把一条从**本场播出面**撤下（reviewer 档）。
///
/// 🔴 **不 `UPDATE world_events`**。写的是 `live_withholds` 独立表，于是：
/// - 世界事实一个字节不动（§0.3 公共事实不可回滚）；
/// - 世界成员的 `/worlds/{id}/events`、战报、回放、日报**全部不受影响**——他们的角色刚刚
///   经历了这件事，把它从当事人眼前抹掉才是真正的事实错乱；
/// - 撤下只作用于这一场（`session_id` 是唯一键的一半）。
///
/// 回执与落库如实标注 `preemptive`：
/// - `true`  = **播出前拦下**（`tick_no > published_high_tick`）。延迟缓冲正在起作用，观众从未看见。
/// - `false` = **播出后撤下**。只减少后续可见性，**收不回已经看见的**——不假装能撤回。
///   `preemptive` 的占比就是「延迟拍数配得够不够」的直接度量。
///
/// 幂等：`ON CONFLICT(session_id, event_id) DO NOTHING`，重复调用读回既有那行。
async fn withhold_event(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(body): Json<WithholdReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_ops_enabled(&state.db).await?;
    require_admin_role(&admin, &["reviewer"])?;
    let s = load_session(&state.db, &id).await?;

    let ev = sqlx::query("SELECT world_id, tick_no FROM world_events WHERE id = $1")
        .bind(&body.event_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let ev_world: String = ev.try_get("world_id")?;
    let tick_no: i64 = ev.try_get("tick_no")?;
    if ev_world != s.world_id {
        // 事件不属于这一场直播的世界：拒绝，避免用一场直播的运营权限去动另一个世界的播出面。
        return Err(ApiError::NotFound);
    }

    // 已播出 = tick_no <= 单调水位线。判定用 `published_high_tick` 而不是现算的水位线：
    // 「有没有真的发出去过」是既成事实，只有那条单调线记得住。
    let preemptive = tick_no > s.published_high_tick;

    sqlx::query(
        "INSERT INTO live_withholds (id, session_id, world_id, event_id, tick_no, preemptive, \
         reason, actor_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT(session_id, event_id) DO NOTHING",
    )
    .bind(new_id("lwh"))
    .bind(&s.id)
    .bind(&s.world_id)
    .bind(&body.event_id)
    .bind(tick_no)
    .bind(if preemptive { 1_i64 } else { 0 })
    .bind(&body.reason)
    .bind(&admin.0.user_id)
    .bind(now_ms())
    .execute(&state.db)
    .await?;

    audit(
        &state.db,
        &admin,
        "live.event.withhold",
        &body.event_id,
        &format!(
            "session={} tick={tick_no} preemptive={preemptive}；{}",
            s.id,
            if body.reason.trim().is_empty() { "(无理由)" } else { body.reason.trim() }
        ),
    )
    .await?;

    Ok(Json(json!({
        "sessionId": s.id,
        "eventId": body.event_id,
        "tickNo": tick_no,
        "preemptive": preemptive,
        "publishedThroughTick": s.published_high_tick,
        "notes": [
            if preemptive {
                "播出前拦下：延迟缓冲窗口内完成，观众从未看见这条。"
            } else {
                "🔴 播出后撤下：本场后续不再出现这条，但收不回观众已经看见的部分——延迟拍数可能配得不够。"
            },
            "世界事实未被改写：world_events 一个字节未动，成员读取面 / 战报 / 回放 / 日报全部不受影响（§0.3）。",
            "撤下只作用于本场直播（session_id 是唯一键的一半），不外溢到其它场次或其它读取面。",
        ],
    })))
}

/// 审计留痕（本模块的写操作统一调用）。
/// `admin_api::audit` 是 `pub(super)`，本模块够不着，按 `annotations` / `social` 的范式复刻。
async fn audit(
    db: &AnyPool,
    admin: &AdminUser,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(new_id("aud"))
    .bind(&admin.0.user_id)
    .bind(&admin.0.role)
    .bind(action)
    .bind(subject)
    .bind(reason)
    .bind(now_ms())
    .execute(db)
    .await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// ④ 转化度量：T5 门槛「直播场观众→玩家转化 ≥2%」
// ═══════════════════════════════════════════════════════════════════════════

/// 观众→玩家转化率。**只读聚合**（本函数无任何写入），挂进 `/admin/metrics/overview`。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 口径（分子 / 分母 / 窗口）
/// ════════════════════════════════════════════════════════════════════════════
///
/// - **分母** = 窗口内**首次观看**直播、且**当时还不是玩家**的人数（按 `user_id` 去重）。
///   - 「首次观看」取 `MIN(first_seen_at)`：一个人看了 3 场只算一个人，不然重度观众会把
///     分母灌水、转化率被稀释。
///   - 「当时还不是玩家」取 `live_viewers.was_player = 0` —— 🔴 **冻结值，不是统计时现算**。
///     现算的话，看完就入场的人会因为"现在是玩家了"被移出分母，分子分母一起缩水。
///     老玩家来看直播不算"待转化的观众"，把他们算进分母同样是稀释。
/// - **分子** = 这些人里，在**首次观看之后**且仍在窗口内**入了场**的人数
///   （`EXISTS world_members WHERE user_id = v.user_id AND joined_at > v.t0 AND joined_at < 窗口右界`）。
///   🔴 `joined_at > t0` 这个**严格**方向是本口径的要害：先入场后看直播不是"直播带来的转化"，
///   把它算进分子等于把留存记成拉新。
///   分子的过滤是分母的**子集条件**（同一张派生表上加 `EXISTS`），故 `分子 ≤ 分母` 恒成立，
///   不会出现 >100% 的转化率。
/// - **窗口** = 调用方给定的 `[start, end)` 毫秒区间（与叙事 SLO 同一把尺，UTC 日界由
///   `dashboards::utc_day_start_ms` 在 Rust 侧算好传入；SQL 里没有任何方言日期函数）。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 三种「没有数」的状态必须分得开（范式抄 `slo::ooc_appeal_block`）
/// ════════════════════════════════════════════════════════════════════════════
///
/// | 情形 | status | value | 后台显示 |
/// |---|---|---|---|
/// | 直播场入口从未对任何人开放过（开关默认关闭） | `entry_not_open` | `null` | `—` |
/// | 入口开着，但窗口内没有一个新观众 | `no_data_in_window` | `null` | `—` |
/// | 入口开着、有观众、**没人转化** | `ok` | `0.0` | `0%` |
///
/// 第一行是本指标特有的坑：本功能**默认关闭**，此时窗口内一个观众足迹都不会有。若直接报
/// `0%`，运营看板上会出现「观众→玩家转化 0%」——一个看起来糟透了、实际上什么都没测的数，
/// 而 T5 恰恰要拿它决定「继续 / 调整 / 停止」。**「没测过」与「没人转化」是完全不同的两件事。**
pub(crate) async fn conversion_block(
    db: &AnyPool,
    window_start: i64,
    window_end: i64,
) -> Result<Value, ApiError> {
    if !entry_ever_open(db).await {
        return Ok(json!({
            "metric": "liveViewerToPlayerConversion",
            "title": "直播场观众→玩家转化率",
            "status": "entry_not_open",
            "value": Value::Null,
            "notes": [
                "直播场入口（运行时开关 MUSE_LIVE_STAGE）从未对任何人开放，窗口内不可能有观众足迹。",
                "🔴 这是「没测过」不是「没人转化」：后台必须显示 —，显示 0% 即为误报（T5 门槛会据此误判为不通过）。",
                "开放方式：运营后台 POST /admin/flags 写一条 enabled=1 的记录（global / world / user 三档任选）。",
            ],
        }));
    }

    // 分母：窗口内首次观看、且当时还不是玩家的去重人数。
    let viewers: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM ( \
             SELECT lv.user_id, MIN(lv.first_seen_at) AS t0 FROM live_viewers lv \
             WHERE lv.first_seen_at >= $1 AND lv.first_seen_at < $2 AND lv.was_player = 0 \
             GROUP BY lv.user_id \
         ) v",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_one(db)
    .await?;

    // 分子：这些人里，首次观看**之后**入了场的。
    let converted: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM ( \
             SELECT lv.user_id, MIN(lv.first_seen_at) AS t0 FROM live_viewers lv \
             WHERE lv.first_seen_at >= $1 AND lv.first_seen_at < $2 AND lv.was_player = 0 \
             GROUP BY lv.user_id \
         ) v \
         WHERE EXISTS ( \
             SELECT 1 FROM world_members wm \
             WHERE wm.user_id = v.user_id AND wm.joined_at > v.t0 AND wm.joined_at < $3 \
         )",
    )
    .bind(window_start)
    .bind(window_end)
    .bind(window_end)
    .fetch_one(db)
    .await?;

    // 辅助数（不进 value，但进响应）：场次数 / 弹幕量 / 撤下量与拦截前置率。
    let sessions: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM live_sessions WHERE starts_at >= $1 AND starts_at < $2",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_one(db)
    .await?;
    let danmaku_total: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM live_danmaku WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_one(db)
    .await?;
    let danmaku_blocked: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM live_danmaku \
         WHERE created_at >= $1 AND created_at < $2 AND moderation <> 'approved'",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_one(db)
    .await?;
    let withheld_row = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS total, \
                CAST(COALESCE(SUM(preemptive), 0) AS BIGINT) AS preemptive \
         FROM live_withholds WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(window_start)
    .bind(window_end)
    .fetch_one(db)
    .await?;
    let withheld_total: i64 = withheld_row.try_get("total")?;
    let withheld_preemptive: i64 = withheld_row.try_get("preemptive")?;

    // 🔴 延迟缓冲**有效性**：撤下里有多少是在播出前拦住的。低于 100% 就说明延迟拍数不够
    // （有内容已经播出去才被撤）。一条撤下都没有 → null（"没发生过"不是"0% 拦住了"）。
    let preemptive_rate = if withheld_total > 0 {
        json!(withheld_preemptive as f64 / withheld_total as f64)
    } else {
        Value::Null
    };

    if viewers == 0 {
        return Ok(json!({
            "metric": "liveViewerToPlayerConversion",
            "title": "直播场观众→玩家转化率",
            "status": "no_data_in_window",
            "value": Value::Null,
            "viewersCounted": 0,
            "sessionsInWindow": sessions,
            "notes": [
                "入口开着，但窗口内没有任何「当时还不是玩家」的新观众 —— 分母为零样本，「没测过」不是「转化率 0」。",
                "分母口径 = 窗口内 live_viewers 里 was_player=0 的行，按 user_id 去重（取 MIN(first_seen_at)）。",
            ],
        }));
    }

    let value = converted as f64 / viewers as f64;
    let threshold = conversion_min();
    Ok(json!({
        "metric": "liveViewerToPlayerConversion",
        "title": "直播场观众→玩家转化率",
        "status": "ok",
        "value": value,
        "thresholdMin": threshold,
        // T5 门槛是 **≥2%**（下限），与基尼那种上限门槛方向相反——别把两者的比较符抄串。
        "belowThreshold": value < threshold,
        "viewersCounted": viewers,
        "convertedCount": converted,
        "sessionsInWindow": sessions,
        "danmakuTotal": danmaku_total,
        "danmakuBlocked": danmaku_blocked,
        "withheldTotal": withheld_total,
        "withheldPreemptive": withheld_preemptive,
        "withheldPreemptiveRate": preemptive_rate,
        "notes": [
            "VALIDATION §2 T5 门槛「直播场观众→玩家转化 ≥2%」的直接实现；门槛可配（MUSE_LIVE_CONVERSION_MIN）。",
            "分母 = 窗口内首次观看直播、且**当时还不是玩家**的去重人数（was_player 在首次观看时冻结，不在统计时现算）。",
            "分子 = 其中在首次观看**之后**入场的人数（joined_at > 首次观看时刻）——先入场后看直播不算转化。",
            "分子的过滤是分母的子集条件，故 分子 ≤ 分母 恒成立，不会出现 >100% 的转化率。",
            "🔴 三态：entry_not_open（入口没开过，—）/ no_data_in_window（零样本，—）/ ok（真数，可以是 0%）。三者不可混同。",
            "withheldPreemptiveRate 是延迟缓冲的**有效性**度量：低于 1 说明有内容已播出才被撤下，即延迟拍数配得不够（T5 预案的上调依据）。",
            "danmakuBlocked/danmakuTotal 是弹幕审核负担的一手数据，供 T5 门槛「内容审核成本 ≤ 生成成本的 5%」参考。",
        ],
    }))
}

/// `?slo=0` 等调用方跳过时的占位（与三态互不混淆，范式同 `slo::skipped_by_request`）。
pub(crate) fn conversion_skipped() -> Value {
    json!({
        "metric": "liveViewerToPlayerConversion",
        "title": "直播场观众→玩家转化率",
        "status": "skipped_by_request",
        "value": Value::Null,
        "notes": ["调用方传了 ?slo=0，本次未计算直播场转化率（高频轮询减负开关）。去掉该参数即恢复。"],
    })
}
