//! **OOC 注解权**（总规格 `docs/build/spec-world-ecosystem.md` §7「人设保险（三级出口）」**第 2 级**）。
//!
//! 规格原文：
//!
//! > **事中·注解权**：单拍 OOC 申诉——世界事实不改，**私人传记可加内心批注**；
//! > 复核确认模型错误则补偿托梦配额。**事实归世界，解释权归玩家。**
//!
//! 三级出口里，第 1 级（事前·底线硬约束）在 critic 环节，第 3 级（事后·if 线）是付费副本，
//! 本模块是**中间那一级**：世界已经演过去了，玩家觉得「我的角色不会这么做」，
//! 平台给的不是「重来一次」，而是**解释权**——
//!
//! ```text
//!   世界说：他在城门口退了一步。            ← world_events，公共事实，永不改写
//!   玩家写：他不是怕，他在等那个人先走。    ← character_annotations，私人解释，只他自己看得见
//! ```
//!
//! 两句话**并存**。这就是「事实归世界，解释权归玩家」的数据形态。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 为什么这一项的优先级最高：它是 T1 门槛的**测量手段**
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `docs/VALIDATION.md` §2 T1 门槛写着「**OOC/裁决不公申诉 <10%/阶段**」，而 §4.2 的 SLO
//! 数据可得性表里，「OOC 申诉率」是八项中**唯一未解**的一项：全仓唯一的申诉表
//! `moderation_appeals` 是**内容风控申诉**（只受理 rejected 的卡/头像、每主体终身一次），
//! 与「角色演得不像 / 裁决不公」零关系，**不得拿来充数**。
//!
//! 于是没有本模块，T1 无法判定——门槛写了也测不了。本模块的 `ooc_appeals` 表就是那个
//! 「真新建件」，`slo::ooc_appeal_block` 是它的消费端。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 红线一：世界事实不改（§0.3 公共事实不可回滚）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 本模块**没有任何写入世界线的路径**。申诉受理、批注写入、复核改判——三条写路径加起来只碰
//! 三张本批次新建的表（`ooc_appeals` / `character_annotations` / `dream_quota_compensations`）
//! 外加 `audit_logs`（留痕）与 `risk_events`（风控）。
//!
//! 以下表**一个字节不动**，由 `tests::red_line_never_rewrites_worldline`（源码级）与
//! `tests::appeal_and_review_leave_worldline_byte_identical`（运行时逐字节快照）双重守死：
//! `worlds` · `world_events` · `world_ticks` · `world_members` · `world_contributions` ·
//! `consent_requests` · `interventions` · `backpacks` · `cloud_characters` · `world_biographies`。
//!
//! **「承认错误」与「回滚事实」是两件事**：复核 `confirmed` 的含义是「我们承认这一拍演砸了」，
//! 不是「这一拍没发生过」。规格选的是前者——补偿托梦配额（给你下一次说话的机会），
//! 而不是改写已落定的公共事实。
//!
//! ### 🔴 批注在物理上为什么无法冒充事实（四道结构性保证）
//!
//! 1. **独立表的独立行**：批注只存在于 `character_annotations`，事实只存在于 `world_events`。
//!    两表之间无外键、无视图、无 UNION 读路径。想把批注读成事实，得先写一条把两张表并起来的
//!    新查询——那是显式的、要过评审的动作，不会「不小心」发生。
//! 2. **每行批注都有主人**（`owner_id NOT NULL`）：世界事实表**没有 owner 列**
//!    （`world_events` 是世界的，不是谁的）。有主人的数据在形状上就不可能是「世界说的话」。
//! 3. **引擎没有读取路径**：`runtime` 与 `crates/muse-engine` 对本模块三张表零引用
//!    （`tests::red_line_annotations_never_enter_engine` 源码级 grep 断言）。
//!    批注既改不了过去（事实已落定），也影响不了未来（不进 `RoundInput.state`）。
//!    口径与 0025 贡献账本 / 0030 critic / 0034 故人印记逐字一致。
//! 4. **读取面自带层次标签**：批注只经 `/api/me/**` 出，每条恒带 `layer="annotation"` 与
//!    `isWorldFact=false`；世界事实经 `/api/worlds/{id}/events` 出（`events` 模块，本模块零改动）。
//!    两条管道各出各的，前端拿到任何一条都能一眼看出它是哪一层。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 红线二：托梦补偿不改 `interventions` 的计数口径
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 见 `dream_quota_bonus` 上方的完整说明。接线**已完成**：调用点在
//! `interventions/mod.rs` 的托梦配额校验处（`used >= dream_quota_per_stage() + bonus`）。
//! 一句话：本模块只提供**加数**，`interventions` 的 `COUNT(*)` 一个字符不用改。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 红线三：社交防火墙（§14 恨隔面具原则）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 批注**只对本人可见**，且**不出真人身份**：`owner_id` 只用于 `WHERE` 过滤，
//! 从不出现在任何响应体里；不记昵称、不记手机号。别人（同世界玩家、观战者、被申诉那一拍里的
//! 其他角色的主人）看不到批注存在，也看不到「谁申诉了」。运营复核队列是唯一能看到申诉内容的
//! 地方，且走 reviewer 档鉴权 + 全程审计。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 端点
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ```text
//! POST /api/worlds/{id}/ooc-appeals              提 OOC 申诉（可附批注）。幂等：同一拍同一角色只受理一次
//! GET  /api/me/ooc-appeals                       我的申诉（含批注、复核结果、托梦补偿）
//! PUT  /api/me/ooc-appeals/{id}/annotation       给自己的申诉加/改内心批注（可在申诉之后补写）
//! GET  /api/me/characters/{id}/annotations       我的角色传记批注（私人解释层，只对本人）
//! GET  /api/admin/ooc-appeals?status=&limit=     复核队列（reviewer 档）
//! POST /api/admin/ooc-appeals/{id}/review        复核改判（reviewer 档）。确认模型错误 → 补偿托梦配额
//! ```
//!
//! ### 未验证功能默认关闭（§0.1）
//!
//! 整块能力由运行时开关 **`MUSE_OOC_ANNOTATIONS`** 控制，**默认关闭**，经 `crate::flags`
//! 统一入口解析（解析链 user > world > global > env > 代码内默认值），
//! 于是支持**按世界灰度**（正对 §2「开放范围」分阶段开闸）。关闭时全部端点 404
//! （不是 403：不向外泄露「平台有这个未开放功能」），**读端点同样 404**（读取侧降级）。
//! fail-closed：查库失败 / 记录损坏 → 按关闭（`flags::is_enabled` 自带）。

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::providers::ModerationVerdict;
use crate::{idempotency, safety};

#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 运营开关（VALIDATION.md §0.1 未验证功能默认关闭）
// ═══════════════════════════════════════════════════════════════════════════

/// OOC 注解权运营开关（**开关名即 env 变量名**，见 `flags` 模块头）。
pub(crate) const ENV_OOC_ANNOTATIONS: &str = "MUSE_OOC_ANNOTATIONS";

/// 默认 = **关闭**。
///
/// 🔴 OOC 注解权是 VALIDATION §2 **T1 的测量工具**，不是已验证结论：它自带 UGC 入口
/// （玩家手写的批注要过机审）、自带运营工作量（每条申诉都要人复核）、还会发放托梦补偿。
/// 代码合并不等于对用户开放——必须运营显式打开，且可按世界灰度逐步放。
const DEFAULT_OOC_ANNOTATIONS_ENABLED: bool = false;

/// 🔴 **编译期钉死**：默认值出现在两处（本常量 + `flags::KNOWN_FLAGS` 登记表），
/// 两处不一致就是「默认关闭」这条 §0.1 约束有了两个事实源。改一处不改另一处直接编不过。
/// 范式抄 `onboarding`。
const _: () = assert!(
    crate::flags::declared_default(ENV_OOC_ANNOTATIONS) == DEFAULT_OOC_ANNOTATIONS_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_OOC_ANNOTATIONS 的默认值必须与 DEFAULT_OOC_ANNOTATIONS_ENABLED 一致"
);

/// 本模块是否已由运营开启。
///
/// 解析上下文按端点分三档，**差异必须写清楚**（口径同 `flags::MIGRATION_NOTES` 对
/// `MUSE_LETHALITY_DEATHMATCH` 两处 ctx 不同的要求——不写清就会出现「全局关但某世界开，
/// 却读不到自己的申诉」这种查不明白的困惑）：
///
/// | 端点 | ctx | 理由 |
/// |---|---|---|
/// | `POST /worlds/{id}/ooc-appeals` | user + world | 申诉对象是「某世界的某一拍」，按世界开闸最自然 |
/// | `/me/**`（三个读写端点） | user（无 world） | 判定发生在「我的东西」上，跨世界，没有单一 world 坐标 |
/// | `/admin/**` | 见 `ensure_ops_enabled` | 入口**曾对任何人开放过**即可复核，否则队列会卡死 |
///
/// ⚠️ 由此产生一条**运营须知**：若只按 world 作用域灰度（global/user 都关着），
/// 玩家能提申诉但读不到 `/me/ooc-appeals`。**推荐的灰度作用域是 user 或 global**，
/// world 作用域只用于「临时关掉某个出问题世界的申诉入口」这种收窄动作。
///
/// 🔴 fail-closed 由 `flags::is_enabled` 自带：查库失败 / 记录损坏 → 返回登记表里声明的
/// 默认值（本开关为 `false` = 关），且**不再回落 env**。
pub(crate) async fn ooc_annotations_enabled(
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
    crate::flags::is_enabled(db, ENV_OOC_ANNOTATIONS, ctx).await
}

/// 开关门：关闭时整块能力**不存在**（404 而非 403）。每个端点第一行都调它，读端点同样调。
async fn ensure_enabled(
    db: &AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
) -> Result<(), ApiError> {
    if ooc_annotations_enabled(db, user_id, world_id).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 运营面开关门：**入口曾对任何人开放过**即放行，否则 404。
///
/// 刻意不用全局解析：若运营按世界灰度开了 3 个世界（global 仍为关），那 3 个世界里提出的申诉
/// 会真实落库，而 `ensure_enabled(None, None)` 会把复核队列判成 404——**申诉进得来、复核进不去**，
/// 队列直接卡死。复核是本功能的闭环环节，它的可见性必须跟「有没有人能提申诉」一致。
///
/// 反向也成立：把开关全部关掉（急停）后，队列一并不可见——这与 `memorial` 的急停语义一致
/// （关阀只让它暂时不可见，已受理的申诉不会消失，重开即恢复）。
async fn ensure_ops_enabled(db: &AnyPool) -> Result<(), ApiError> {
    if entry_ever_open(db).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 入口**是否曾经对任何人开放过**（`slo::ooc_appeal_block` 与 `ensure_ops_enabled` 共用）。
///
/// 🔴 存在的唯一理由是**别把「入口没开」读成「没人申诉」**：
/// 开关默认关闭，此时窗口内一条申诉都不会有，若 SLO 直接报 `0%`，运营看板上会显示
/// 「OOC 申诉率 0%」——一个看起来棒极了、实际上什么都没测的数。这正是 §4.2 反复强调的
/// 「显示 `—` 与显示 `0%` 是两个完全不同的经营判断」。
///
/// 判定 = 全局解析为开 **或** `runtime_flags` 里存在任何一条 `enabled=1` 的记录
/// （后者覆盖按世界/按用户灰度：大盘关着但 3 个世界开了，入口就是开过的）。
/// **fail-safe 方向是 false**（查库失败 → 按「没开过」处理 → SLO 报「没测过」而不是 0%）。
pub(crate) async fn entry_ever_open(db: &AnyPool) -> bool {
    if ooc_annotations_enabled(db, None, None).await {
        return true;
    }
    let n = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM runtime_flags WHERE flag = ? AND enabled = 1",
    )
    .bind(ENV_OOC_ANNOTATIONS)
    .fetch_one(db)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("n").ok())
    .unwrap_or(0);
    n > 0
}

// ═══════════════════════════════════════════════════════════════════════════
// 参数化（VALIDATION.md §0.2 产品规则参数化，禁止写死）
// ═══════════════════════════════════════════════════════════════════════════

/// 复核确认模型错误时补偿的托梦条数 env（默认 1）。
///
/// 默认 1 而不是「补满」：托梦配额每卡每阶段默认 3 条，稀缺化本身是产品设计
/// （§8「让『何时说、说什么』成为真决策」）。补偿的语义是「这一次不算你的」，
/// 不是「这一局随便说」——补太多等于用申诉换额度，会把申诉入口变成刷额度的通道。
const ENV_COMPENSATION_WHISPERS: &str = "MUSE_OOC_COMPENSATION_WHISPERS";
const DEFAULT_COMPENSATION_WHISPERS: i64 = 1;
/// 补偿上限（防运营把 env 配成天文数字，间接绕过配额稀缺性）。
const MAX_COMPENSATION_WHISPERS: i64 = 10;

/// 单次复核补偿的托梦条数（运营可调；缺失/非法/非正数 → 默认；超上限 → 截到上限）。
fn compensation_whispers() -> i64 {
    std::env::var(ENV_COMPENSATION_WHISPERS)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_COMPENSATION_WHISPERS)
        .min(MAX_COMPENSATION_WHISPERS)
}

/// 申诉理由正文长度上限（字符）。
const REASON_MAX_CHARS: usize = 500;
/// 内心批注长度上限（字符）。比理由宽——批注是玩家写给自己角色的话，是内容不是工单。
const ANNOTATION_MAX_CHARS: usize = 1000;
/// 复核理由长度上限（字符），口径同 `admin_api::audit::resolve_appeal` 的 500。
const REVIEW_REASON_MAX_CHARS: usize = 500;
/// 列表页条数上限。
const MAX_PAGE_SIZE: i64 = 100;
const DEFAULT_PAGE_SIZE: i64 = 20;

/// 异议类别白名单。正对 VALIDATION §2 T1 门槛原文「**OOC/裁决不公**申诉 <10%/阶段」的两个词：
/// - `ooc`：角色演得不像自己（人设崩坏）；
/// - `unfair_ruling`：裁决不公（仲裁结果与既有事实/规则对不上）。
///
/// 放在代码里而不是 DB CHECK：双库可移植子集禁 CHECK，且分类会随真实申诉数据演进
/// （§0.2 产品规则参数化）。未知类别 → 400，绝不静默归到 `ooc`：那会污染两类的分布统计，
/// 而这两类的**比例**恰恰是 T1 之后要看的东西（人设问题 vs 规则问题，改法完全不同）。
const REASON_CODES: &[&str] = &["ooc", "unfair_ruling"];

// ═══════════════════════════════════════════════════════════════════════════
// 状态字面量
// ═══════════════════════════════════════════════════════════════════════════

/// 待复核。
const STATUS_PENDING: &str = "pending";
/// **确认模型错误**（申诉成立）→ 触发托梦补偿。注意：成立 ≠ 改写世界线。
///
/// 🔴 刻意**不叫 `upheld`**：`moderation_appeals`（内容风控申诉）里的 `upheld` 意思是
/// 「**维持原判**」= 申诉被驳回，与这里的「申诉成立」正好相反。同一个词在两张表里指相反的事，
/// 是最容易在看板/报表上算反的那类命名。`confirmed`（确认模型错误）与请求体的
/// `decision="confirm_model_error"` 同源，读起来只有一个意思。
const STATUS_CONFIRMED: &str = "confirmed";
/// 不予支持（维持原判）。
const STATUS_DISMISSED: &str = "dismissed";

/// 复核决定字面量（请求体取值）。
const DECISION_CONFIRM: &str = "confirm_model_error";
const DECISION_DISMISS: &str = "dismiss";

pub fn router() -> Router<AppState> {
    Router::new()
        // 玩家面
        .route("/worlds/{id}/ooc-appeals", post(create_appeal))
        .route("/me/ooc-appeals", get(my_appeals))
        .route("/me/ooc-appeals/{id}/annotation", put(put_annotation))
        .route("/me/characters/{id}/annotations", get(my_character_annotations))
        // 运营面（reviewer 档，与内容风控申诉同档：都是改判类动作）
        .route("/admin/ooc-appeals", get(list_appeals_admin))
        .route("/admin/ooc-appeals/{id}/review", post(review_appeal))
}

// ═══════════════════════════════════════════════════════════════════════════
// 鉴权辅助
// ═══════════════════════════════════════════════════════════════════════════

/// 细粒度角色守卫。语义与 `admin_api::require_role` 逐字一致（`admin` 为超级用户放行一切，
/// 其余角色须在白名单内），此处重写一份是因为那个函数是 `pub(super)`（admin_api 私有），
/// 而本模块是它的兄弟模块。**复核属改判类动作 → 取 `reviewer` 档**，
/// 与 `admin_api::audit::resolve_appeal`（内容风控申诉的唯一改判路径）同档。
fn require_reviewer(admin: &AdminUser) -> Result<(), ApiError> {
    let role = admin.0.role.as_str();
    if role == "admin" || role == "reviewer" {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// 审计留痕。形状与 `admin_api::audit` 完全一致（同一张 `audit_logs`、同样六列），
/// 同样因 `pub(super)` 不可见而重写。🔴 复核是运营改判，**没有留痕就没有复盘**，
/// 故本函数与状态更新写在**同一个事务**里：不存在「改判了但审计没落」的中间态。
async fn audit_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    actor: &AuthUser,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id("aud"))
    .bind(&actor.user_id)
    .bind(&actor.role)
    .bind(action)
    .bind(subject)
    .bind(reason)
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 托梦配额补偿：与 `interventions` 的接缝
// ═══════════════════════════════════════════════════════════════════════════

/// 某卡在某世界已获得的**托梦配额补偿总数**（本模块对外的唯一「生效」接口）。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 为什么补偿是「加数」而不是「改计数」
/// ════════════════════════════════════════════════════════════════════════════
///
/// 托梦配额当前由 `interventions::create_intervention` 判定，口径是：
///
/// ```sql
/// SELECT COUNT(*) FROM interventions
///  WHERE world_id = ? AND character_id = ? AND kind = 'whisper'
///    AND status IN ('accepted', 'applied')
/// ```
///
/// 三种「看起来可行」的补偿方式全部被否决：
///
/// | 做法 | 为什么不行 |
/// |---|---|
/// | 往 `interventions` 插一行「补偿托梦」 | 伪造一条玩家从未说过的话，且 runtime 会把它当真喂给引擎 |
/// | 把某条 `applied` 改回 `accepted` | 让「它已被引擎消费过」这个已落定的事实消失（§0.3） |
/// | 把某条改成 `compensated` 之类的新状态 | 同上，且直接篡改了那条 SQL 的计数口径 |
///
/// 本模块选**第四种**：一张独立的加数表，有效配额 =
/// `dream_quota_per_stage()`（不变）**+** `dream_quota_bonus()`（本函数）。
/// 于是上面那条 `COUNT(*)` **一个字符都不用改**——变的只是它被拿去比较的阈值。
/// 这也是「不改动它的计数口径也能生效」的字面含义。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔵 接线待办（**需要 `interventions` 的负责人改一行**，本批次刻意不越界）
/// ════════════════════════════════════════════════════════════════════════════
///
/// `server/src/interventions/mod.rs` 中：
///
/// ```ignore
/// // 现状：
/// if used >= dream_quota_per_stage() {
///     reject_reason = Some("quota".into());
/// }
/// // 接线后：
/// let bonus = crate::annotations::dream_quota_bonus(&state.db, &world_id, &req.character_id).await;
/// if used >= dream_quota_per_stage() + bonus {
///     reject_reason = Some("quota".into());
/// }
/// ```
///
/// 改动面：**1 行判定 + 1 行取值**，无表结构变化、无计数 SQL 变化、无拒绝语义变化
/// （超限仍是 `rejected("quota")`）。附带建议：`GET /worlds/{id}/interventions/mine` 的响应
/// 里把 `quota` 拆成 `base` / `bonus` / `total` 三个数，否则玩家会看不懂自己为什么多了一条。
///
/// 在该行接上之前，补偿**已经真实入账、可查、可审计**（玩家在 `/me/ooc-appeals` 看得到），
/// 只是尚未在托梦受理处兑现。这是刻意的边界：本批次不碰 `interventions/`。
///
/// 🔴 查库失败一律返回 **0**（fail-closed 方向：宁可少给一条补偿，
/// 也不能因一次超时给出无限额度——配额是防「开局倒攻略」的结构性设计，不能被异常放大）。
pub(crate) async fn dream_quota_bonus(db: &AnyPool, world_id: &str, character_id: &str) -> i64 {
    sqlx::query(
        "SELECT CAST(COALESCE(SUM(grants), 0) AS BIGINT) AS n FROM dream_quota_compensations \
         WHERE world_id = ? AND character_id = ?",
    )
    .bind(world_id)
    .bind(character_id)
    .fetch_one(db)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("n").ok())
    .filter(|v| *v >= 0)
    .unwrap_or(0)
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /worlds/{id}/ooc-appeals —— 提申诉
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CreateAppealReq {
    /// 被申诉的那一拍（`world_ticks.tick_no`）。
    tick_no: i64,
    /// 演得不像的是哪张卡（必须是本人在该世界的卡）。
    character_id: String,
    /// 异议类别，缺省 `ooc`。
    #[serde(default)]
    reason_code: Option<String>,
    /// 玩家自述的理由（必填）。
    #[serde(default)]
    reason_text: String,
    /// 可选的内心批注——**私人解释层**，只有本人看得见。
    #[serde(default)]
    annotation: Option<String>,
}

/// 提 OOC 申诉。
///
/// 服务端权威校验（顺序即优先级）：
/// 1. 运营开关（关 → 404，读写一致）；
/// 2. 世界存在（否则 404）；
/// 3. **卡属本人且确实在过这个世界**（`world_members` 有行即可，**不要求仍 active**——
///    申诉的是过去某一拍，中途退场的人当然有权对他在场时的那一拍提异议）；
/// 4. **那一拍确实已落定**（`world_ticks.status='done'`）——不许对不存在或未提交的拍申诉，
///    否则申诉率的分子会被无意义的噪声撑大；
/// 5. 类别在白名单内、理由非空且不超长、批注不超长。
///
/// **幂等两层**：① `Idempotency-Key`（同一次点击的 HTTP 重试）；
/// ② **DB 唯一键** `(world_id, tick_no, character_id)`——换个 key 再点也只会读回既有那条，
/// 返回 `created:false`。单靠幂等键会被「换 key 再点」击穿，单靠唯一键挡不住同一次点击的重试。
async fn create_appeal(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CreateAppealReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), Some(&world_id)).await?;

    // 幂等层一：同 key 同载荷 → 返回缓存响应；同 key 异载荷 → 409。
    let endpoint = "POST /worlds/:id/ooc-appeals";
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let idem_key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard =
        idempotency::guard(&state.db, &user.user_id, endpoint, idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // 入参校验。
    let reason_code = req.reason_code.as_deref().unwrap_or("ooc").trim().to_string();
    if !REASON_CODES.contains(&reason_code.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "reasonCode 仅支持 {}",
            REASON_CODES.join(" / ")
        )));
    }
    let reason_text = req.reason_text.trim().to_string();
    if reason_text.is_empty() {
        return Err(ApiError::BadRequest("申诉理由不能为空".into()));
    }
    if reason_text.chars().count() > REASON_MAX_CHARS {
        return Err(ApiError::BadRequest(format!("申诉理由不能超过 {REASON_MAX_CHARS} 字")));
    }
    let annotation = req.annotation.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(text) = annotation {
        if text.chars().count() > ANNOTATION_MAX_CHARS {
            return Err(ApiError::BadRequest(format!("内心批注不能超过 {ANNOTATION_MAX_CHARS} 字")));
        }
    }

    // 世界必须存在（状态不限：世界已 ended 仍可申诉——申诉的是过去的一拍，
    // 而「对已结束世界的那一拍不服」恰恰是最常见的情形）。
    let world_exists: Option<(String,)> = sqlx::query_as("SELECT id FROM worlds WHERE id = ?")
        .bind(&world_id)
        .fetch_optional(&state.db)
        .await?;
    if world_exists.is_none() {
        return Err(ApiError::NotFound);
    }

    // 卡必须属本人且在过这个世界（服务端权威，§9.6）。伪造他人角色 → 记风控 + RiskBlocked，
    // 口径与 `interventions::create_intervention` 一致。
    let member: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM world_members WHERE world_id = ? AND cloud_character_id = ? AND user_id = ?",
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
            "ooc_appeal_denied",
            json!({"reason": "character_not_owned_or_never_present", "characterId": req.character_id}),
        )
        .await?;
        return Err(ApiError::RiskBlocked);
    }

    // 那一拍必须已落定。
    let tick: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM world_ticks WHERE world_id = ? AND tick_no = ? AND status = 'done'",
    )
    .bind(&world_id)
    .bind(req.tick_no)
    .fetch_optional(&state.db)
    .await?;
    if tick.is_none() {
        return Err(ApiError::BadRequest("该拍不存在或尚未落定，无法申诉".into()));
    }

    // 幂等层二：唯一键。先查后插，插失败再查一次（覆盖并发），全程不依赖方言错误码。
    if let Some(existing) = find_appeal_by_slot(&state.db, &world_id, req.tick_no, &req.character_id).await? {
        let resp = appeal_response(&state.db, &existing, false).await?;
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    let appeal_id = new_id("ooc");
    let now = now_ms();
    let insert = sqlx::query(
        "INSERT INTO ooc_appeals (id, world_id, tick_no, character_id, user_id, reason_code, \
         reason_text, status, reviewer_id, review_reason, reviewed_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, '', '', 0, ?)",
    )
    .bind(&appeal_id)
    .bind(&world_id)
    .bind(req.tick_no)
    .bind(&req.character_id)
    .bind(&user.user_id)
    .bind(&reason_code)
    .bind(&reason_text)
    .bind(STATUS_PENDING)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = insert {
        // 并发下撞唯一键：读回既有那条（受理过就是受理过），而不是把重复点击报成错误。
        return match find_appeal_by_slot(&state.db, &world_id, req.tick_no, &req.character_id).await? {
            Some(existing) => {
                let resp = appeal_response(&state.db, &existing, false).await?;
                guard.store_response(&state.db, &resp.to_string()).await?;
                Ok(Json(resp))
            }
            None => Err(e.into()),
        };
    }

    // 批注（可选）。机审后落库：无论裁决都存，读取面仅 approved 才给正文。
    if let Some(text) = annotation {
        write_annotation(&state, &appeal_id, &user.user_id, &req.character_id, &world_id, req.tick_no, text)
            .await?;
    }

    let row = fetch_appeal(&state.db, &appeal_id).await?.ok_or(ApiError::NotFound)?;
    let resp = appeal_response(&state.db, &row, true).await?;
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ═══════════════════════════════════════════════════════════════════════════
// PUT /me/ooc-appeals/{id}/annotation —— 加/改内心批注
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnnotationReq {
    /// 批注正文。空串 = 清空（把自己写的话删掉是玩家的权利）。
    #[serde(default)]
    body: String,
}

/// 给自己的申诉加/改内心批注。
///
/// 单独开一个端点而不是只在提申诉时带：**「想说什么」往往比「气不过」晚到**——
/// 玩家当场提申诉，隔天才想清楚该怎么给自己的角色解释这一步。
/// 只能改自己的（`WHERE owner 判定`），改别人的一律 404（不是 403：不泄露该申诉存在）。
async fn put_annotation(
    State(state): State<AppState>,
    user: AuthUser,
    Path(appeal_id): Path<String>,
    Json(req): Json<AnnotationReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;

    let row = fetch_appeal(&state.db, &appeal_id).await?.ok_or(ApiError::NotFound)?;
    // 🔴 越权一律 404：不确认「这条申诉存在」这件事本身（信息边界，§14）。
    if row.user_id != user.user_id {
        return Err(ApiError::NotFound);
    }

    let body = req.body.trim();
    if body.chars().count() > ANNOTATION_MAX_CHARS {
        return Err(ApiError::BadRequest(format!("内心批注不能超过 {ANNOTATION_MAX_CHARS} 字")));
    }
    if body.is_empty() {
        sqlx::query("DELETE FROM character_annotations WHERE appeal_id = ? AND owner_id = ?")
            .bind(&appeal_id)
            .bind(&user.user_id)
            .execute(&state.db)
            .await?;
    } else {
        write_annotation(
            &state,
            &appeal_id,
            &user.user_id,
            &row.character_id,
            &row.world_id,
            row.tick_no,
            body,
        )
        .await?;
    }

    let resp = appeal_response(&state.db, &row, false).await?;
    Ok(Json(resp))
}

/// 写批注：机审 → upsert（同一申诉至多一条，改写走 UPDATE 不产生第二行）。
///
/// 🔴 **私密不豁免机审**。私密只决定「谁能看」，不决定「平台是否为它负责」：
/// 批注会随传记导出、会进人工复核视野，它是平台承载的内容。范式与 `interventions` 的
/// whisper 一致（`safety::moderate_and_queue`，Pending 自动进 `audit_queue` 人审）。
/// 无论裁决都落库——人审改判后无需玩家重写（范式同 `worlds.cover_url`）。
#[allow(clippy::too_many_arguments)]
async fn write_annotation(
    state: &AppState,
    appeal_id: &str,
    owner_id: &str,
    character_id: &str,
    world_id: &str,
    tick_no: i64,
    body: &str,
) -> Result<(), ApiError> {
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT id FROM character_annotations WHERE appeal_id = ?")
            .bind(appeal_id)
            .fetch_optional(&state.db)
            .await?;
    let ann_id = existing.as_ref().map(|(id,)| id.clone()).unwrap_or_else(|| new_id("ann"));

    let verdict = safety::moderate_and_queue(state, "annotation", &ann_id, body).await?;
    let moderation = match verdict {
        ModerationVerdict::Approved => "approved",
        ModerationVerdict::Pending => "pending",
        ModerationVerdict::Rejected => "rejected",
    };
    let now = now_ms();

    if existing.is_some() {
        sqlx::query(
            "UPDATE character_annotations SET body = ?, moderation = ?, updated_at = ? \
             WHERE id = ? AND owner_id = ?",
        )
        .bind(body)
        .bind(moderation)
        .bind(now)
        .bind(&ann_id)
        .bind(owner_id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO character_annotations (id, owner_id, character_id, world_id, tick_no, \
             appeal_id, body, moderation, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&ann_id)
        .bind(owner_id)
        .bind(character_id)
        .bind(world_id)
        .bind(tick_no)
        .bind(appeal_id)
        .bind(body)
        .bind(moderation)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /me/ooc-appeals —— 我的申诉
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
}

fn clamp_page(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE)
}

async fn my_appeals(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    let limit = clamp_page(q.limit);
    let offset = q.offset.unwrap_or(0).max(0);

    // 🔴 `WHERE user_id = ?` 是硬边界：本端点永远只出本人的申诉。
    // 状态过滤**下推进 SQL**（不在 Rust 侧过滤已取回的页）：页内过滤会让 limit/offset 与
    // 实际条数对不上，翻页越翻越少，是最容易被当成「数据丢了」的那类 bug。
    let rows = match q.status.as_deref() {
        Some(status) => sqlx::query(&format!(
            "SELECT {APPEAL_COLUMNS} FROM ooc_appeals WHERE user_id = ? AND status = ? \
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        ))
        .bind(&user.user_id)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?,
        None => sqlx::query(&format!(
            "SELECT {APPEAL_COLUMNS} FROM ooc_appeals WHERE user_id = ? \
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        ))
        .bind(&user.user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?,
    };

    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let appeal = AppealRow::from_row(r)?;
        items.push(appeal_response(&state.db, &appeal, false).await?);
    }

    Ok(Json(json!({
        "items": items,
        "limit": limit,
        "offset": offset,
        "notes": [
            "🔴 申诉不改写世界事实：复核成立（confirmed）的含义是「我们承认这一拍演砸了」，不是「这一拍没发生过」。",
            "批注是你的私人解释层（layer=annotation），与公共世界线并存；除你之外无人可见。",
            "复核确认模型错误会补偿托梦配额，见每条的 compensation 字段。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /me/characters/{id}/annotations —— 我的角色传记批注
// ═══════════════════════════════════════════════════════════════════════════

/// 私人传记批注（**解释层**）。
///
/// 🔴 三重信息边界：
/// ① 卡必须属本人（否则 404，连「这张卡存在」都不确认）；
/// ② SQL 恒带 `WHERE owner_id = ?`；
/// ③ 响应体不含 `ownerId`、不含任何真人字段（§14 社交防火墙）。
///
/// 🔴 响应恒带 `layer="annotation"` 与 `isWorldFact=false`：前端拿到任意一条都能一眼分清
/// 它是玩家的解释还是世界的事实。世界事实走 `/api/worlds/{id}/events`（`events` 模块），
/// 两条管道各出各的，本模块从不返回任何 `world_events` 的内容。
async fn my_character_annotations(
    State(state): State<AppState>,
    user: AuthUser,
    Path(character_id): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;

    // 卡必须属本人。
    let owns: Option<(String,)> =
        sqlx::query_as("SELECT id FROM cloud_characters WHERE id = ? AND owner_id = ?")
            .bind(&character_id)
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
    if owns.is_none() {
        return Err(ApiError::NotFound);
    }

    let limit = clamp_page(q.limit);
    let offset = q.offset.unwrap_or(0).max(0);
    let rows = sqlx::query(
        "SELECT id, world_id, tick_no, appeal_id, body, moderation, created_at, updated_at \
         FROM character_annotations WHERE owner_id = ? AND character_id = ? \
         ORDER BY world_id ASC, tick_no ASC, id ASC LIMIT ? OFFSET ?",
    )
    .bind(&user.user_id)
    .bind(&character_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(annotation_json(r)?);
    }

    Ok(Json(json!({
        "characterId": character_id,
        "layer": "annotation",
        "isWorldFact": false,
        "items": items,
        "limit": limit,
        "offset": offset,
        "notes": [
            "🔴 这些是**你的解释**，不是世界事实：世界线永不因批注改变（§0.3 公共事实不可回滚）。",
            "世界事实的唯一读路径是 /api/worlds/{id}/events；本端点从不返回任何世界事实。",
            "批注只有你看得见——不进任何公开投影、不进引擎决策、不出现在他人的任何视图里。",
            "未过审的批注只回 moderation 状态、不回正文（withheld=true），人审改判后自动恢复。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /admin/ooc-appeals —— 复核队列
// ═══════════════════════════════════════════════════════════════════════════

async fn list_appeals_admin(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_reviewer(&admin)?;
    // 🔴 运营面同样受开关约束（读取侧降级一致），但判定用 `ensure_ops_enabled`：
    // 只要入口对任何人开放过就能复核，否则按世界灰度时队列会卡死。
    ensure_ops_enabled(&state.db).await?;

    let limit = clamp_page(q.limit);
    let offset = q.offset.unwrap_or(0).max(0);
    let status = q.status.unwrap_or_else(|| STATUS_PENDING.to_string());
    let rows = sqlx::query(&format!(
        "SELECT {APPEAL_COLUMNS} FROM ooc_appeals \
         WHERE status = ? ORDER BY created_at ASC, id ASC LIMIT ? OFFSET ?"
    ))
    .bind(&status)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let a = AppealRow::from_row(r)?;
        items.push(json!({
            "id": a.id,
            "worldId": a.world_id,
            "tickNo": a.tick_no,
            "characterId": a.character_id,
            "reasonCode": a.reason_code,
            "reasonText": a.reason_text,
            "status": a.status,
            "createdAt": a.created_at,
        }));
    }
    Ok(Json(json!({
        "items": items,
        "status": status,
        "limit": limit,
        "offset": offset,
        "notes": [
            "复核只回答一个问题：这一拍是不是模型演错了。**不是**「要不要改掉这一拍」——世界事实不可回滚。",
            "确认模型错误 → 自动补偿托梦配额（MUSE_OOC_COMPENSATION_WHISPERS，默认 1 条），补偿落独立账、不改 interventions。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /admin/ooc-appeals/{id}/review —— 复核
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewReq {
    /// `confirm_model_error`（确认模型错误 → 补偿托梦配额）| `dismiss`（不予支持）。
    decision: String,
    /// 复核理由（必填，≤500 字）。运营改判必须说得清为什么。
    #[serde(default)]
    reason: String,
}

/// 运营复核。**唯一的改判路径**，reviewer 档，落审计。
///
/// 🔴 三件事在**同一个事务**里完成，缺一不可：
/// ① 状态 CAS（`WHERE status='pending'`）—— 抢到才继续，重复复核命中 0 行 → 409；
/// ② 确认模型错误时写补偿账（唯一索引 `appeal_id` 作第二道幂等闸）；
/// ③ 落 `audit_logs`。
///
/// 放在一个事务里是刻意的：任何一步单独成功都会产生**不可复盘的中间态**
/// （改判了但没审计 / 审计了但没补偿 / 补了两次）。
///
/// 🔴 **复核绝不改写世界线**：无论确认还是驳回，`world_events` / `narrative_state_json` /
/// 结算账本一行不动。承认错误 ≠ 回滚事实。
async fn review_appeal(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(appeal_id): Path<String>,
    Json(req): Json<ReviewReq>,
) -> Result<Json<Value>, ApiError> {
    require_reviewer(&admin)?;
    ensure_ops_enabled(&state.db).await?;

    let decision = req.decision.trim();
    if decision != DECISION_CONFIRM && decision != DECISION_DISMISS {
        return Err(ApiError::BadRequest(format!(
            "decision 仅支持 {DECISION_CONFIRM} / {DECISION_DISMISS}"
        )));
    }
    let reason = req.reason.trim().to_string();
    let n = reason.chars().count();
    if n == 0 || n > REVIEW_REASON_MAX_CHARS {
        return Err(ApiError::BadRequest(format!(
            "复核理由必填且不超过 {REVIEW_REASON_MAX_CHARS} 字"
        )));
    }

    let appeal = fetch_appeal(&state.db, &appeal_id).await?.ok_or(ApiError::NotFound)?;
    if appeal.status != STATUS_PENDING {
        return Err(ApiError::Conflict("该申诉已复核，不可重复裁决".into()));
    }

    let confirmed = decision == DECISION_CONFIRM;
    let new_status = if confirmed { STATUS_CONFIRMED } else { STATUS_DISMISSED };
    let grants = if confirmed { compensation_whispers() } else { 0 };
    let now = now_ms();

    let mut tx = state.db.begin().await?;

    // ① 状态 CAS：只有从 pending 出发的那一次能成功。
    let updated = sqlx::query(
        "UPDATE ooc_appeals SET status = ?, reviewer_id = ?, review_reason = ?, reviewed_at = ? \
         WHERE id = ? AND status = ?",
    )
    .bind(new_status)
    .bind(&admin.0.user_id)
    .bind(&reason)
    .bind(now)
    .bind(&appeal_id)
    .bind(STATUS_PENDING)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        tx.rollback().await?;
        return Err(ApiError::Conflict("该申诉已复核，不可重复裁决".into()));
    }

    // ② 补偿托梦配额（仅确认模型错误时）。
    if confirmed {
        sqlx::query(
            "INSERT INTO dream_quota_compensations (id, appeal_id, world_id, character_id, user_id, \
             grants, granted_by, reason, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id("dqc"))
        .bind(&appeal_id)
        .bind(&appeal.world_id)
        .bind(&appeal.character_id)
        .bind(&appeal.user_id)
        .bind(grants)
        .bind(&admin.0.user_id)
        .bind(&reason)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    // ③ 审计（运营改判必须留痕）。
    let action = if confirmed { "ooc_appeal.confirmed" } else { "ooc_appeal.dismissed" };
    let subject = format!("ooc_appeal:{appeal_id}");
    let audit_reason = format!(
        "{}|world={}|tick={}|character={}|compensation={}|{}",
        new_status, appeal.world_id, appeal.tick_no, appeal.character_id, grants, reason
    );
    audit_tx(&mut tx, &admin.0, action, &subject, &audit_reason).await?;

    tx.commit().await?;

    let row = fetch_appeal(&state.db, &appeal_id).await?.ok_or(ApiError::NotFound)?;
    let mut resp = appeal_response(&state.db, &row, false).await?;
    if let Some(obj) = resp.as_object_mut() {
        obj.insert("decision".into(), json!(decision));
        obj.insert(
            "worldlineChanged".into(),
            // 🔴 恒 false 且**写进响应**：复核结果对客户端明说「世界线没动」，
            // 免得前端/运营把「申诉成立」误当成「这一拍被撤销」。
            json!(false),
        );
    }
    Ok(Json(resp))
}

// ═══════════════════════════════════════════════════════════════════════════
// 行与响应
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
struct AppealRow {
    id: String,
    world_id: String,
    tick_no: i64,
    character_id: String,
    user_id: String,
    reason_code: String,
    reason_text: String,
    status: String,
    reviewer_id: String,
    review_reason: String,
    reviewed_at: i64,
    created_at: i64,
}

impl AppealRow {
    fn from_row(r: &sqlx::any::AnyRow) -> Result<Self, ApiError> {
        Ok(Self {
            id: r.try_get("id")?,
            world_id: r.try_get("world_id")?,
            tick_no: r.try_get("tick_no")?,
            character_id: r.try_get("character_id")?,
            user_id: r.try_get("user_id")?,
            reason_code: r.try_get("reason_code")?,
            reason_text: r.try_get("reason_text")?,
            status: r.try_get("status")?,
            reviewer_id: r.try_get("reviewer_id")?,
            review_reason: r.try_get("review_reason")?,
            reviewed_at: r.try_get("reviewed_at")?,
            created_at: r.try_get("created_at")?,
        })
    }
}

const APPEAL_COLUMNS: &str = "id, world_id, tick_no, character_id, user_id, reason_code, \
     reason_text, status, reviewer_id, review_reason, reviewed_at, created_at";

async fn fetch_appeal(db: &AnyPool, id: &str) -> Result<Option<AppealRow>, ApiError> {
    let row = sqlx::query(&format!("SELECT {APPEAL_COLUMNS} FROM ooc_appeals WHERE id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    row.as_ref().map(AppealRow::from_row).transpose()
}

async fn find_appeal_by_slot(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    character_id: &str,
) -> Result<Option<AppealRow>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {APPEAL_COLUMNS} FROM ooc_appeals \
         WHERE world_id = ? AND tick_no = ? AND character_id = ?"
    ))
    .bind(world_id)
    .bind(tick_no)
    .bind(character_id)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(AppealRow::from_row).transpose()
}

/// 一条批注的响应体。
///
/// 🔴 **不含 `ownerId`**（§14：批注不泄露真人身份；owner 只用于 SQL 过滤）。
/// 🔴 恒带 `layer` / `isWorldFact`：这两个字段是「批注不冒充事实」的读取面保证。
/// 未过审的批注只回状态不回正文（`withheld=true`）。
fn annotation_json(r: &sqlx::any::AnyRow) -> Result<Value, ApiError> {
    let moderation: String = r.try_get("moderation")?;
    let body: String = r.try_get("body")?;
    let approved = moderation == "approved";
    Ok(json!({
        "id": r.try_get::<String, _>("id")?,
        "worldId": r.try_get::<String, _>("world_id")?,
        "tickNo": r.try_get::<i64, _>("tick_no")?,
        "appealId": r.try_get::<String, _>("appeal_id")?,
        "body": if approved { Value::String(body) } else { Value::Null },
        "withheld": !approved,
        "moderation": moderation,
        "createdAt": r.try_get::<i64, _>("created_at")?,
        "updatedAt": r.try_get::<i64, _>("updated_at")?,
        "layer": "annotation",
        "isWorldFact": false,
    }))
}

/// 一条申诉的完整响应（含批注与补偿）。
async fn appeal_response(db: &AnyPool, a: &AppealRow, created: bool) -> Result<Value, ApiError> {
    let ann = sqlx::query(
        "SELECT id, world_id, tick_no, appeal_id, body, moderation, created_at, updated_at \
         FROM character_annotations WHERE appeal_id = ?",
    )
    .bind(&a.id)
    .fetch_optional(db)
    .await?;
    let annotation = match &ann {
        Some(r) => annotation_json(r)?,
        None => Value::Null,
    };

    let comp = sqlx::query(
        "SELECT grants, created_at FROM dream_quota_compensations WHERE appeal_id = ?",
    )
    .bind(&a.id)
    .fetch_optional(db)
    .await?;
    let compensation = match &comp {
        Some(r) => json!({
            "dreamWhispers": r.try_get::<i64, _>("grants")?,
            "grantedAt": r.try_get::<i64, _>("created_at")?,
        }),
        None => Value::Null,
    };

    Ok(json!({
        "id": a.id,
        "created": created,
        "worldId": a.world_id,
        "tickNo": a.tick_no,
        "characterId": a.character_id,
        "reasonCode": a.reason_code,
        "reasonText": a.reason_text,
        "status": a.status,
        "reviewReason": a.review_reason,
        "reviewedAt": a.reviewed_at,
        "reviewerAssigned": !a.reviewer_id.is_empty(),
        "createdAt": a.created_at,
        "annotation": annotation,
        "compensation": compensation,
        // 🔴 恒 false。申诉的产物是解释权与补偿，永远不是「改掉那一拍」。
        "worldFactChanged": false,
        "dreamQuotaBonus": dream_quota_bonus(db, &a.world_id, &a.character_id).await,
    }))
}
