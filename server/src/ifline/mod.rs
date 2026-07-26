//! **if 线付费副本**（总规格 `docs/build/spec-world-ecosystem.md` §7「人设保险（三级出口）」**第 3 级**）。
//!
//! 规格原文：
//!
//! > **事后·if 线**：世界结束后花资源以某拍为分叉点开单人平行线副本（**不影响原世界线**）——
//! > 把遗憾变成付费内容。
//!
//! 三级出口至此完整：
//!
//! ```text
//!   事前 · 底线硬约束   卡的 bottomLines 升级为仲裁硬约束（engine / critic）
//!   事中 · 注解权       世界事实不改，私人传记加内心批注（annotations，迁移 0037）
//!   事后 · if 线        原世界线一字不动，另开一条只属于你的平行线（本模块，迁移 0039）
//! ```
//!
//! 三级的共同前提是同一句话：**公共事实不可回滚**（§0.3）。玩家买到的从来不是「改写」，
//! 而是依次的三样东西——事前的保护、事中的解释权、事后的**平行线**。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 红线一：不影响原世界线（§0.3）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 玩家买 if 线的动机恰恰是「想改写那一拍」，所以这一项是三级里最容易滑向违规的一级。
//! 本模块的防线是**结构性的，不是纪律性的**：
//!
//! 1. **if 线不是一行 `worlds`**。这是本批次最重要的一个决定。一行 `worlds` + 若干
//!    `world_members` 会被 `runtime::commit_tick → end_world_tx → finalize_ending_tx` 自动带进
//!    `progression::settle_idle_world_ending_tx`（发历练）/ `subplot::settle_subplot_card_tx`（铸卡）/
//!    `arena_rewards`（荣誉）。历练是准入门槛与卡位解锁的钥匙——**只要 if 线是一行 `worlds`，
//!    「花钱开 if 线」立刻等于「花钱买数值」**，直接踩穿 §0.1「付费只买体验容量，永不买结果」。
//!    放进独立表 `ifline_worlds` 之后，那条反哺路径在物理上不存在（结算管线只认那两张表）。
//! 2. **本模块只对世界线表做 SELECT**。`worlds` / `world_events` / `world_ticks` / `world_members` /
//!    `world_contributions` / `interventions` / `consent_requests` / `world_biographies` /
//!    `backpacks` / `cloud_characters` —— 全部只读。由
//!    `tests::red_line_never_writes_worldline`（源码级）与
//!    `tests::red_line_opening_ifline_leaves_worldline_byte_identical`（运行时逐字节快照）双重守死。
//! 3. **快照是死数据**：分叉态从 `worlds.narrative_state_json` **复制**而来，复制之后与原世界
//!    再无同步通道。原世界不会因为快照变化，快照也不会回流原世界。
//!
//! ### 🔴 if 线在读取面为什么一眼可辨（不冒充原世界线）
//!
//! | 保证 | 形态 |
//! |---|---|
//! | id 空间不同 | if 线 id 前缀 `ifw_`，世界 id 是 `worlds.id`。光看 id 就分得清哪一层 |
//! | 有主人 vs 没主人 | `ifline_worlds.owner_id NOT NULL`（单人）；`worlds` **没有 owner 列**。有主人的世界在形状上就不是「大家共处的那条世界线」（口径同 0037 批注的 `owner_id`） |
//! | 读取管道分离 | if 线只经 `/api/me/iflines/**` 与 `/api/worlds/{id}/ifline-fork-points` 出；世界事实只经 `/api/worlds/{id}/events` 出。两条管道各出各的，本模块从不返回任何 `world_events` 内容 |
//! | 响应恒带层次标签 | 每条响应恒带 `layer="ifline"` · `isWorldFact=false` · `affectsOriginWorld=false` |
//! | 引擎零读取路径 | `runtime` 与 `crates/muse-engine` 对 `ifline_worlds` 零引用（grep 级断言） |
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 红线二：分叉点**不假装**（本项最容易做假的地方）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 规格写「以某拍为分叉点」，但**仓库里没有任何一拍的状态快照**。核实结论（证据见迁移 0039 头）：
//!
//! - `world_ticks` 全部列里**没有一列存状态**，只有 `base_revision`（那一拍基于哪个 revision）；
//! - `worlds.narrative_state_json` 是单行、每拍被 CAS 覆盖的**最终态**，历史版本不留存；
//! - `world_events` 存的是**投影后的展示文本**，引擎的 `StatePatch`（状态变化唯一事实源）
//!   在 `commit_tick` 里被丢弃、从不落库 → 事件流**无法重放**出中间态；
//! - 引擎 FS 是 DB 那一列的每拍物化，同样只有当前态。
//!
//! 于是本模块**只支持终局分叉**（`fork_point='terminal'`），请求中间拍一律 400 明确拒绝。
//! 宁可功能弱一点，也不给玩家一个「看起来是那一拍、其实不是」的分叉——那是在为一个假东西收费。
//! 每一次读取都强制下发 `stateFidelity`，玩家永远知道自己拿到的是哪一档保真度。
//!
//! 🔵 **补齐路径**（将来若要支持任意拍）：先加一张逐拍状态快照表（每拍多存一份完整
//! `NarrativeState`，成本可观），再把 `fork_point` 扩出 `tick` 档。表结构已给这一档留好了位置
//! （`fork_point` / `fork_tick_no` / `state_fidelity` 三列），不必改表。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 红线三：社交防火墙（§14 恨隔面具原则）—— 单人平行线
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 规格写的是「**单人**平行线副本」。冻结快照前，**他人玩家的角色一律剥离**：
//! 按 `world_members` 逐个比对，凡是别人的卡，其 `characters` 条目、涉及它的 `relations` 边、
//! 以及其它角色关系里的 `knownTo` 引用全部移除，剥离台账落 `redaction_json`（玩家可见 + 运营可审）。
//!
//! 这不是洁癖：未经同意把别人的角色拖进你的 if 线、并让它做原主人从没做过的事，
//! 等于**以他人角色之名生成他人未授权的言行**——比暴露真人身份更难挽回，且不可撤销。
//! NPC 不剥离：NPC 是世界的，不是谁的（与 `world_events` 没有 owner 列同一条道理）。
//!
//! ⚠️ 剥离必然造成内容缺口（原世界的一些关系线会在 if 线里断掉）。这是**有意的代价**，
//! 且必须**明示**——所以台账要给玩家看，不能既剥离了又不说剥离了什么。
//! 将来若要支持「经他人同意带入」，正确的做法是走 `consents` 模块的同意流程拿到显式授权，
//! 不是在本模块里加一个开关。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 「花资源」= 烧一张副本卡（§10）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! **不新造货币**（§0.5 无提现红线下多一种货币就多一条 RMT 侧门）。开 if 线消耗
//! `MUSE_IFLINE_CARD_COST`（默认 1）张在手的副本卡——副本卡正是「把你亲历的剧情副本铸成的卡」，
//! 用它开那段剧情的平行线，语义严丝合缝。
//!
//! 消耗走副本卡**既有的状态机**：`status='owned' → 'consumed'` 的条件 UPDATE（CAS），
//! `consumed_into` 指向 if 线 id——与 `subplot::synthesize` 的回收口逐字同款。
//! 🔴 **本模块不 INSERT `subplot_cards`**：铸卡的唯一写入路径仍是 `subplot::grant_card_tx`
//! （§0.2 资产单一写入路径），本模块只改已发出的卡的状态，不产生任何新资产
//! （`tests::red_line_never_mints_assets` 源码级断言）。
//!
//! 为什么是「烧」而不是「占用」：占用需要一张绑定表，而合成端的 CAS 只看 `status='owned'`，
//! 会把被占用的卡照熔不误 → 「卡熔了、if 线还开着」的白嫖漏洞，堵它必须改 `subplot/`（不越界）。
//! 「烧」天然复用同一个状态机：卡一旦 `consumed`，合成端自动排除它，零跨模块接线。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 产出不反哺原世界（§0.1 不卖胜负）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `ifline_worlds` **没有任何数值列**：没有历练、没有贡献分、没有奖励、没有系数、没有余额。
//! if 线的产物只可能是「内容」，永远不会是「资产」。源码级断言：本模块零引用
//! `grant_mileage_tx` / `grant_item_tx` / `grant_card_tx` / `settle_` / `ledger` /
//! `world_contributions` / `arena_rewards`。
//!
//! 另有一条容易漏的：**传世卡不得进 if 线**（§12「传世卡只读、不可再入世界」）。
//! 允许了就是「付费复活」——正是本项最需要避免的「付费改命」形态。受理时校验
//! `memorial_status='living'` 且 `withdrawn=0`。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 端点
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ```text
//! GET  /api/worlds/{id}/ifline-fork-points   可用分叉点 + **限制说明**（诚实面：客户端不必猜）
//! POST /api/worlds/{id}/iflines              开 if 线（烧副本卡）。幂等：同人同卡同分叉点只开一条
//! GET  /api/me/iflines                       我的 if 线列表
//! GET  /api/me/iflines/{id}                  一条 if 线（含冻结快照与剥离台账）
//! POST /api/me/iflines/{id}/beats            推进一拍（0041）。并发闸 = (ifline_id, beat_no) 唯一键
//! GET  /api/me/iflines/{id}/beats            这条平行线的正文（**终局产物 = 这份可读传记**）
//! GET  /api/admin/iflines?status=&limit=     运营列表（operator 档）
//! GET  /api/admin/iflines/cost?since=&until= 运营成本读数（operator 档，0041）
//! ```
//!
//! ### 未验证功能默认关闭（§0.1）
//!
//! 整块能力由运行时开关 **`MUSE_IFLINE_PARALLEL`** 控制，**默认关闭**，经 `crate::flags` 统一入口
//! 解析（解析链 user > world > global > env > 代码内默认值），于是支持按世界/按用户灰度。
//! 关闭时全部端点 404（不是 403：不向外泄露「平台有这个未开放功能」），**读端点同样 404**。
//! fail-closed：查库失败 / 记录损坏 → 按关闭（`flags::is_enabled` 自带）。
//!
//! ### 推进（跑拍）—— 迁移 `0041`，实现在 `runner.rs`
//!
//! 0039 交付的是**立项与开局**（`status` 恒为 `sealed`）；0041 补上 `sealed → running → ended`。
//! 详细设计（含结算隔离的三层保证、成本记账、SLO 归属、§14 纵深防御、确定性契约）
//! 在 `runner.rs` 的模块头，此处只重申那条最高红线与它的落实方式：
//!
//! > 🔴 if 线的终局**绝不允许**进入 `progression::settle_*` / `subplot::settle_subplot_card_tx` /
//! > `arena_rewards` 中的任何一条——它的产出只能是内容，不能是资产。
//!
//! 落实**不靠纪律，靠结构**：推进走 `runner::commit_beat`（写 `ifline_beats` + `ifline_worlds`），
//! 与 `runtime::commit_tick` 零交叉。那三条结算全挂在
//! `commit_tick → end_world_tx → finalize_ending_tx` 这一条自动链路上，而该链路的入口只有
//! 一行 `worlds` 和若干行 `world_members`——if 线两者都不是，于是那条路径在物理上够不着它。
//!
//! 🔴 **接线时最容易走错的一步**，是为了复用 `process_tick_inner` 那 500 行而把 if 线塞回
//! `worlds` / `world_ticks`。tick 管线与结算管线是**连体的**（CAS 成功即评估终局、终局即结算），
//! 没有「跑但不结算」的开关可拨。宁可在 `runner.rs` 重写一遍「组装 → 跑 → 提交」，
//! 也不共用那条会自动结算的路径：重复的代价是几百行，复用的代价是这个功能的合规性。

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
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

/// 推进（跑拍，迁移 0041）。**独立文件**而不是塞进本文件：推进要组装 `RoundInput`、调引擎、
/// 管活态 CAS，与「立项与开局」是两件事；分开也让红线的源码级扫描面一目了然
/// （`red_line_never_mints_assets` / `red_line_never_writes_worldline` 两个用例的扫描面
/// **本批次已扩到 `runner.rs`**——新增文件若不进扫描面，就是红线的盲区）。
mod runner;

#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 运营开关（VALIDATION.md §0.1 未验证功能默认关闭）
// ═══════════════════════════════════════════════════════════════════════════

/// if 线运营开关（**开关名即 env 变量名**，见 `flags` 模块头）。
pub(crate) const ENV_IFLINE_PARALLEL: &str = "MUSE_IFLINE_PARALLEL";

/// 默认 = **关闭**。
///
/// 🔴 if 线是**付费内容入口 + UGC 入口 + 资产销毁口**三件事叠在一起：它会烧掉玩家真实获得的
/// 副本卡、会接收玩家手写的分叉前提（要过机审）、还会复制一整份世界状态。
/// 代码合并不等于对用户开放——必须运营显式打开，且可按世界灰度逐步放
/// （「先只对已结束的那批实验世界开」是最自然的第一步）。
const DEFAULT_IFLINE_ENABLED: bool = false;

/// 🔴 **编译期钉死**：默认值出现在两处（本常量 + `flags::KNOWN_FLAGS` 登记表），
/// 两处不一致就是「默认关闭」这条 §0.1 约束有了两个事实源。改一处不改另一处直接编不过。
/// 范式抄 `annotations` / `onboarding`。
const _: () = assert!(
    crate::flags::declared_default(ENV_IFLINE_PARALLEL) == DEFAULT_IFLINE_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_IFLINE_PARALLEL 的默认值必须与 DEFAULT_IFLINE_ENABLED 一致"
);

/// 本模块是否已由运营开启。
///
/// 解析上下文按端点分三档（口径同 `annotations::ooc_annotations_enabled`，
/// **差异必须写清楚**，否则会出现「全局关但某世界开，却读不到自己的 if 线」这类查不明白的困惑）：
///
/// | 端点 | ctx | 理由 |
/// |---|---|---|
/// | `/worlds/{id}/ifline-fork-points`、`POST /worlds/{id}/iflines` | user + world | 分叉对象是「某个原世界」，按世界开闸最自然 |
/// | `/me/iflines**` | user（无 world） | 判定发生在「我的东西」上，跨世界，没有单一 world 坐标 |
/// | `/admin/iflines` | 见 `ensure_ops_enabled` | 入口**曾对任何人开放过**即可查，否则运营看不到已开的 if 线 |
///
/// ⚠️ 由此产生一条**运营须知**：若只按 world 作用域灰度（global/user 都关着），
/// 玩家能开 if 线但读不到 `/me/iflines`。**推荐的灰度作用域是 user 或 global**，
/// world 作用域只用于「临时关掉某个世界的分叉入口」这种收窄动作。
pub(crate) async fn ifline_enabled(
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
    crate::flags::is_enabled(db, ENV_IFLINE_PARALLEL, ctx).await
}

/// 开关门：关闭时整块能力**不存在**（404 而非 403）。每个端点第一行都调它，读端点同样调。
async fn ensure_enabled(
    db: &AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
) -> Result<(), ApiError> {
    if ifline_enabled(db, user_id, world_id).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 运营面开关门：**入口曾对任何人开放过**即放行，否则 404。
///
/// 刻意不用全局解析：若运营按世界灰度开了几个世界（global 仍为关），那几个世界里开出的 if 线
/// 会真实落库，而 `ensure_enabled(None, None)` 会把运营列表判成 404——**if 线开得出来、运营看不见**。
/// 已经烧掉玩家副本卡的东西必须运营可查，否则客诉无从处理。
/// 反向也成立：把开关全部关掉（急停）后列表一并不可见，与 `annotations` / `memorial` 的急停语义一致
/// （关阀只让它暂时不可见，已开的 if 线不会消失，重开即恢复）。
async fn ensure_ops_enabled(db: &AnyPool) -> Result<(), ApiError> {
    if entry_ever_open(db).await {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 入口**是否曾经对任何人开放过**。
///
/// 判定 = 全局解析为开 **或** `runtime_flags` 里存在任何一条本开关 `enabled=1` 的记录
/// （后者覆盖按世界/按用户灰度）。**fail-safe 方向是 false**（查库失败 → 按「没开过」处理）。
/// 口径逐字抄 `annotations::entry_ever_open`。
async fn entry_ever_open(db: &AnyPool) -> bool {
    if ifline_enabled(db, None, None).await {
        return true;
    }
    let n = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM runtime_flags WHERE flag = $1 AND enabled = 1",
    )
    .bind(ENV_IFLINE_PARALLEL)
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

/// 开一条 if 线要烧几张副本卡（运营可调）。
///
/// 默认 1：if 线的定价是**待验证的默认策略**，不是承诺（§0 默认策略）。定价高低要等 T3
/// 「用户愿意为什么付钱」的数据说话，此处只保证它是个参数而不是字面量。
const ENV_IFLINE_CARD_COST: &str = "MUSE_IFLINE_CARD_COST";
const DEFAULT_IFLINE_CARD_COST: i64 = 1;
/// 上限：防运营把 env 配成天文数字，把 if 线变成一次性清空玩家资产的按钮。
const MAX_IFLINE_CARD_COST: i64 = 10;

/// 开一条 if 线的副本卡开销（缺失/非法/非正数 → 默认；超上限 → 截到上限）。
fn ifline_card_cost() -> i64 {
    std::env::var(ENV_IFLINE_CARD_COST)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_IFLINE_CARD_COST)
        .min(MAX_IFLINE_CARD_COST)
}

/// 分叉前提（玩家手写的「如果……」）长度上限。比申诉理由宽——它是内容不是工单，
/// 口径同 `annotations::ANNOTATION_MAX_CHARS`。
const PREMISE_MAX_CHARS: usize = 1000;
/// 列表页条数上限。
const MAX_PAGE_SIZE: i64 = 100;
const DEFAULT_PAGE_SIZE: i64 = 20;

// ═══════════════════════════════════════════════════════════════════════════
// 字面量
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **当前唯一支持的分叉点**：原世界终局态。见模块头「红线二」。
const FORK_POINT_TERMINAL: &str = "terminal";
/// 尚不支持的分叉点档位（请求它会得到一段说明为什么不支持，而不是一个假分叉）。
const FORK_POINT_TICK: &str = "tick";
/// 🔴 状态保真度字面量：原世界终局态的**逐字节复制**（剥离他人角色后）。
const FIDELITY_ORIGIN_TERMINAL: &str = "origin_terminal_state";

/// if 线生命周期：`sealed`（已立项、分叉态已冻结、资源已扣，尚未跑过任何一拍）
/// → `running`（至少跑过一拍）→ `ended`（已收尾，不可再推进）。
/// 后两个字面量在 `runner` 里定义（推进是那边的事），此处只声明起点。
const STATUS_SEALED: &str = "sealed";

/// 副本卡状态字面量（**读的是 `subplot` 的表，字面量必须与那边一致**）。
/// 刻意在此重声明而不是 `use subplot::*`：那边是私有常量；两处漂移会让 CAS 静默命中 0 行，
/// 表现为「玩家有卡却说没卡」，故用例 `card_status_literals_match_subplot` 源码级比对两边。
const CARD_STATUS_OWNED: &str = "owned";
const CARD_STATUS_CONSUMED: &str = "consumed";

pub fn router() -> Router<AppState> {
    Router::new()
        // 玩家面
        .route("/worlds/{id}/ifline-fork-points", get(fork_points))
        .route("/worlds/{id}/iflines", post(open_ifline))
        .route("/me/iflines", get(my_iflines))
        .route("/me/iflines/{id}", get(my_ifline_detail))
        // 推进与正文（0041）
        .route("/me/iflines/{id}/beats", post(advance_ifline).get(list_beats))
        // 运营面（operator 档：只读排查，不是改判类动作）
        .route("/admin/iflines", get(list_iflines_admin))
        // 🔴 成本读数（0041）。单开一个端点而不是等主看板：付费功能的开销必须**现在就查得到**，
        // 不能因为「主看板还没接」就在任何地方都看不见。主看板未接这件事写在响应的 `notes` 里。
        .route("/admin/iflines/cost", get(ifline_cost_admin))
}

// ═══════════════════════════════════════════════════════════════════════════
// 鉴权辅助
// ═══════════════════════════════════════════════════════════════════════════

/// 细粒度角色守卫。语义与 `admin_api::require_role` 逐字一致（`admin` 为超级用户放行一切）。
/// 本模块运营面**只读**（排查「这条 if 线烧了谁的卡」），故取 `operator` 档而非 `reviewer`。
fn require_operator(admin: &AdminUser) -> Result<(), ApiError> {
    let role = admin.0.role.as_str();
    if role == "admin" || role == "operator" {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 原世界的只读视图
// ═══════════════════════════════════════════════════════════════════════════

/// 原世界的只读快照（**本模块对 `worlds` 的唯一交互形态**）。
struct OriginWorld {
    id: String,
    status: String,
    state_revision: i64,
    narrative_state_json: String,
    template_id: String,
    template_version: i64,
}

async fn load_origin(db: &AnyPool, world_id: &str) -> Result<Option<OriginWorld>, ApiError> {
    let row = sqlx::query(
        "SELECT id, status, state_revision, narrative_state_json, template_id, template_version \
         FROM worlds WHERE id = $1",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(OriginWorld {
        id: r.try_get("id")?,
        status: r.try_get("status")?,
        state_revision: r.try_get("state_revision")?,
        narrative_state_json: r.try_get("narrative_state_json")?,
        template_id: r.try_get("template_id").unwrap_or_default(),
        template_version: r.try_get("template_version").unwrap_or(0),
    }))
}

/// 原世界**最后一拍已落定的拍号**（= 唯一支持的分叉点坐标）。
///
/// 🔴 只数 `status='done'`：failed / pending / running 的拍没有落定的状态，
/// 拿它当分叉点等于分在半空中。
async fn terminal_tick_no(db: &AnyPool, world_id: &str) -> Result<Option<i64>, ApiError> {
    let row = sqlx::query(
        "SELECT MAX(tick_no) AS n FROM world_ticks WHERE world_id = $1 AND status = 'done'",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|r| r.try_get::<Option<i64>, _>("n").ok().flatten()))
}

/// 原世界里**他人玩家**的角色 id 集合（§14 剥离名单）。
///
/// NPC 不在 `world_members` 里，因此天然不入名单——NPC 是世界的，不是谁的。
async fn foreign_character_ids(
    db: &AnyPool,
    world_id: &str,
    owner_id: &str,
) -> Result<Vec<String>, ApiError> {
    let rows = sqlx::query(
        "SELECT cloud_character_id, user_id FROM world_members WHERE world_id = $1 AND user_id <> $2",
    )
    .bind(world_id)
    .bind(owner_id)
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(r.try_get::<String, _>("cloud_character_id")?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 分叉态冻结 + §14 剥离
// ═══════════════════════════════════════════════════════════════════════════

/// 剥离结果。
struct Frozen {
    snapshot: Value,
    redaction: Value,
    protagonist_present: bool,
    character_count: usize,
    relation_count: usize,
}

/// 把原世界的终局态**复制**成 if 线的冻结分叉态，并按 §14 剥离他人玩家角色。
///
/// 🔴 用 `serde_json::Value` 而不是引擎的 `NarrativeState` 类型：
/// ① 本模块不该依赖引擎类型（if 线永远不进引擎决策，见模块头）；
/// ② 反序列化再序列化会**丢掉未知字段**——引擎状态结构在演进，用弱类型才能保证
///    「除了被剥离的那些，其余一个字节不改」这句话为真。
///
/// 剥离三处（缺一都会留下引用悬空）：
/// 1. `characters` 里他人角色的条目；
/// 2. `relations` 里 `from`/`to` 命中他人角色的整条边；
/// 3. 剩余关系边的 `knownTo` 数组里对他人角色的引用。
fn freeze_snapshot(state_json: &str, foreign: &[String], protagonist: &str) -> Option<Frozen> {
    let mut root: Value = serde_json::from_str(state_json).ok()?;
    let obj = root.as_object_mut()?;
    // 世界从未演过任何一拍时这一列是 `{}`：没有状态就没有可分叉的东西。
    if obj.is_empty() {
        return None;
    }

    let mut removed_characters: Vec<String> = Vec::new();
    let mut removed_relations: i64 = 0;
    let mut removed_known_to: i64 = 0;

    // ① characters（引擎里的键 = cloud_character_id，见 `events::ProjectionMember::character_key`）。
    if let Some(Value::Object(chars)) = obj.get_mut("characters") {
        for cid in foreign {
            if chars.remove(cid).is_some() {
                removed_characters.push(cid.clone());
            }
        }
    }

    // ② + ③ relations。
    if let Some(Value::Array(rels)) = obj.get_mut("relations") {
        let before = rels.len();
        rels.retain(|r| {
            let hit = |k: &str| {
                r.get(k).and_then(Value::as_str).map(|v| foreign.iter().any(|f| f == v)).unwrap_or(false)
            };
            !(hit("from") || hit("to"))
        });
        removed_relations = (before - rels.len()) as i64;
        for r in rels.iter_mut() {
            let Some(Value::Array(known)) = r.get_mut("knownTo") else { continue };
            let before = known.len();
            known.retain(|k| {
                k.as_str().map(|v| !foreign.iter().any(|f| f == v)).unwrap_or(true)
            });
            removed_known_to += (before - known.len()) as i64;
        }
    }

    let character_count =
        obj.get("characters").and_then(Value::as_object).map(|m| m.len()).unwrap_or(0);
    let relation_count =
        obj.get("relations").and_then(Value::as_array).map(|v| v.len()).unwrap_or(0);
    let protagonist_present = obj
        .get("characters")
        .and_then(Value::as_object)
        .map(|m| m.contains_key(protagonist))
        .unwrap_or(false);

    removed_characters.sort();
    let redaction = json!({
        "rule": "§14 社交防火墙：未经同意不得把他人角色带进你的 if 线",
        "removedCharacterIds": removed_characters,
        "removedRelationEdges": removed_relations,
        "removedKnownToRefs": removed_known_to,
        "npcRetained": true,
        "note": "NPC 不剥离——NPC 是世界的，不是谁的；被剥离的是其他玩家的卡。剥离会造成内容缺口，这是有意的代价。",
    });

    Some(Frozen {
        snapshot: root,
        redaction,
        protagonist_present,
        character_count,
        relation_count,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /worlds/{id}/ifline-fork-points —— 诚实面：能从哪儿分叉、为什么只有这一个
// ═══════════════════════════════════════════════════════════════════════════

/// 可用分叉点 + **限制说明**。
///
/// 🔴 单开这个端点的理由：本功能最大的诚信风险是「玩家以为能从第 N 拍分叉」。
/// 把限制做成一个**可被客户端读到的结构化事实**，而不是只在 400 的报错文案里出现——
/// 前端就不必猜，也不会先让玩家填完表单再告诉他不行。
async fn fork_points(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), Some(&world_id)).await?;

    let world = load_origin(&state.db, &world_id).await?.ok_or(ApiError::NotFound)?;
    let terminal = terminal_tick_no(&state.db, &world_id).await?;

    // 资格判定（顺序即优先级；每一条都给出可读理由，不给「无法开启」这种没信息量的话）。
    let ineligible: Option<&str> = if world.status != "ended" {
        Some("原世界尚未结束——if 线是**事后**出口（§7），世界还在演的时候不开分叉")
    } else if terminal.is_none() {
        Some("原世界没有任何已落定的拍（world_ticks.status='done'），无从分叉")
    } else if world.state_revision <= 0 || world.narrative_state_json.trim() == "{}" {
        Some("原世界从未产生过叙事状态，没有可复制的分叉态")
    } else {
        None
    };

    let supported = match (ineligible, terminal) {
        (None, Some(t)) => vec![json!({
            "kind": FORK_POINT_TERMINAL,
            "tickNo": t,
            "stateRevision": world.state_revision,
            "stateFidelity": FIDELITY_ORIGIN_TERMINAL,
            "isApproximate": false,
            "desc": "原世界终局态的逐字节复制（剥离他人玩家角色后）。这是当前唯一状态完整的分叉点。",
        })],
        _ => Vec::new(),
    };

    Ok(Json(json!({
        "worldId": world.id,
        "layer": "ifline",
        "isWorldFact": false,
        "affectsOriginWorld": false,
        "worldStatus": world.status,
        "eligible": ineligible.is_none(),
        "ineligibleReason": ineligible,
        "supportedForkPoints": supported,
        "unsupportedForkPoints": [{
            "kind": FORK_POINT_TICK,
            "reason": "仓库不存逐拍状态快照：world_ticks 只有 base_revision 不含状态；\
                       worlds.narrative_state_json 是每拍被覆盖的最终态；world_events 是投影文本，\
                       引擎 StatePatch 从不落库故事件流无法重放中间态。因此「精确还原第 N 拍」做不到。",
            "whyNotDegrade": "用终局态冒充第 N 拍会让玩家为一个假分叉付费——宁可拒绝，也不给看起来像那一拍的东西。",
            "unlockPath": "先加一张逐拍状态快照表（每拍多存一份完整 NarrativeState），再扩出本档。表结构已留位。",
        }],
        "cost": {
            "subplotCards": ifline_card_cost(),
            "note": "开 if 线烧掉在手的副本卡（§10 内容燃料）。不新造货币，不产出任何可反哺原世界的资产。",
        },
        "notes": [
            "🔴 if 线是**平行线，不是改写**：原世界的世界线、结算与账本一个字节都不会变（§0.3 公共事实不可回滚）。",
            "🔴 单人副本：其他玩家的角色一律不进你的 if 线（§14 社交防火墙），剥离台账随 if 线一并可见。",
            "🔴 传世卡（已封卷的卡）不得进 if 线（§12「不可再入世界」）——if 线不是付费复活。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /worlds/{id}/iflines —— 开 if 线
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct OpenReq {
    /// 主角卡（必须是本人在该世界的**在世**卡）。
    character_id: String,
    /// 分叉点档位，缺省 `terminal`。当前只接受 `terminal`。
    #[serde(default)]
    fork_point: Option<String>,
    /// 分叉拍号（可选）。给了就必须**等于**终局拍，否则 400——见模块头「红线二」。
    #[serde(default)]
    tick_no: Option<i64>,
    /// 玩家写的「如果……」（可选，≤1000 字，过机审）。
    #[serde(default)]
    premise: Option<String>,
    /// 愿意烧掉的副本卡 id（数量必须**恰好**等于 `MUSE_IFLINE_CARD_COST`）。
    /// 🔴 由玩家显式点名而不是后端挑：烧的是他辛苦打出来的资产，不能由系统替他选。
    #[serde(default)]
    card_ids: Vec<String>,
}

/// 开一条 if 线。
///
/// 服务端权威校验（顺序即优先级，每一步失败都不动任何资源）：
/// 1. 运营开关（关 → 404，读写一致）；
/// 2. 原世界存在且 **已结束**（`status='ended'`，规格「世界结束后」）；
/// 3. 分叉点合法（只接受 `terminal`；给了 `tickNo` 必须等于终局拍）；
/// 4. 卡属本人 + 在过该世界（`world_members` 有行，**不要求仍 active**——中途退场的人也有遗憾）；
/// 5. 卡**在世**（`memorial_status='living'` 且 `withdrawn=0`）——传世卡不得复活（§12）；
/// 6. 原世界确有可复制的状态；
/// 7. 副本卡数量正确、全部在手、全部属本人。
///
/// **幂等三层**：① `Idempotency-Key`（同一次点击的 HTTP 重试）；
/// ② **DB 唯一键** `(owner_id, fork_key)`——换个 key 再点也只读回既有那条，`created:false`；
/// ③ 副本卡 `status='owned'` 的 **CAS**——抢不到即整笔回滚，绝不出现「卡烧了但 if 线没开」。
/// 三层缺一：单靠幂等键会被「换 key 再点」击穿；单靠唯一键挡不住并发下两笔同时扣卡。
async fn open_ifline(
    State(state): State<AppState>,
    user: AuthUser,
    Path(world_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<OpenReq>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), Some(&world_id)).await?;

    // 幂等层一：同 key 同载荷 → 返回缓存响应；同 key 异载荷 → 409。
    let endpoint = "POST /worlds/:id/iflines";
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let idem_key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard =
        idempotency::guard(&state.db, &user.user_id, endpoint, idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    // ── 入参 ────────────────────────────────────────────────────────────────
    let character_id = req.character_id.trim().to_string();
    if character_id.is_empty() {
        return Err(ApiError::BadRequest("必须指定主角卡".into()));
    }
    let fork_point = req.fork_point.as_deref().unwrap_or(FORK_POINT_TERMINAL).trim().to_string();
    if fork_point != FORK_POINT_TERMINAL {
        // 🔴 不静默降级成 terminal：那正是「看起来是那一拍、其实不是」。
        return Err(ApiError::BadRequest(format!(
            "分叉点 `{fork_point}` 不受支持。当前只支持 `{FORK_POINT_TERMINAL}`（原世界终局态）——\
             仓库不存逐拍状态快照（world_ticks 无状态列、narrative_state_json 每拍被覆盖、\
             world_events 只是投影文本且 StatePatch 从不落库），无法精确还原任意一拍。\
             详见 GET /api/worlds/{{id}}/ifline-fork-points"
        )));
    }
    let premise = req.premise.as_deref().map(str::trim).unwrap_or("").to_string();
    if premise.chars().count() > PREMISE_MAX_CHARS {
        return Err(ApiError::BadRequest(format!("分叉前提不能超过 {PREMISE_MAX_CHARS} 字")));
    }

    let required_cards = ifline_card_cost();
    let mut card_ids: Vec<String> =
        req.card_ids.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    card_ids.sort();
    let unique = {
        let mut c = card_ids.clone();
        c.dedup();
        c.len()
    };
    if unique != card_ids.len() || card_ids.len() as i64 != required_cards {
        return Err(ApiError::BadRequest(format!(
            "开一条 if 线需要 {required_cards} 张在手的副本卡（不得重复），实际提交 {}",
            req.card_ids.len()
        )));
    }

    // ── 原世界（只读） ──────────────────────────────────────────────────────
    let world = load_origin(&state.db, &world_id).await?.ok_or(ApiError::NotFound)?;
    if world.status != "ended" {
        return Err(ApiError::BadRequest(format!(
            "if 线是**事后**出口（§7）：原世界当前状态为 `{}`，世界结束后才能开平行线",
            world.status
        )));
    }
    let Some(terminal) = terminal_tick_no(&state.db, &world_id).await? else {
        return Err(ApiError::BadRequest("原世界没有任何已落定的拍，无从分叉".into()));
    };
    if let Some(requested) = req.tick_no {
        if requested != terminal {
            return Err(ApiError::BadRequest(format!(
                "无法从第 {requested} 拍分叉：仓库不存逐拍状态快照，那一拍的世界态已不可复原。\
                 当前唯一支持的分叉点是终局拍（第 {terminal} 拍）。\
                 🔴 平台不会拿终局态冒充第 {requested} 拍——那是在为一个假分叉收费。"
            )));
        }
    }

    // ── 卡：属本人 + 在过该世界 + 在世 ─────────────────────────────────────
    let member: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM world_members WHERE world_id = $1 AND cloud_character_id = $2 AND user_id = $3",
    )
    .bind(&world_id)
    .bind(&character_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    if member.is_none() {
        // 伪造他人角色 → 记风控 + RiskBlocked，口径同 `annotations::create_appeal`。
        safety::record_risk(
            &state.db,
            Some(&user.user_id),
            Some(&world_id),
            "ifline_denied",
            json!({"reason": "character_not_owned_or_never_present", "characterId": character_id}),
        )
        .await?;
        return Err(ApiError::RiskBlocked);
    }

    let card_row = sqlx::query(
        "SELECT memorial_status, withdrawn FROM cloud_characters WHERE id = $1 AND owner_id = $2",
    )
    .bind(&character_id)
    .bind(&user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(card_row) = card_row else { return Err(ApiError::NotFound) };
    let memorial_status: String = card_row.try_get("memorial_status").unwrap_or_default();
    let withdrawn: i64 = card_row.try_get("withdrawn").unwrap_or(0);
    if memorial_status != "living" || withdrawn != 0 {
        // 🔴 §12：传世卡只读、不可再入世界。放它进 if 线就是「付费复活」= 付费改命。
        return Err(ApiError::BadRequest(
            "这张卡已封卷为传世卡（或已下架），不可进入 if 线——传世卡不可再入世界（§12），\
             if 线不是付费复活。内核可复制，履历不可复制：同内核开新卡是转世，不是复活。"
                .into(),
        ));
    }

    // ── 冻结分叉态（§14 剥离） ─────────────────────────────────────────────
    let foreign = foreign_character_ids(&state.db, &world_id, &user.user_id).await?;
    let Some(frozen) = freeze_snapshot(&world.narrative_state_json, &foreign, &character_id) else {
        return Err(ApiError::BadRequest("原世界没有可复制的叙事状态，无从分叉".into()));
    };

    // ── 幂等层二：同人同卡同分叉点只开一条 ─────────────────────────────────
    let fork_key = format!("{world_id}:{character_id}:{fork_point}:{terminal}");
    if let Some(existing) = find_by_fork_key(&state.db, &user.user_id, &fork_key).await? {
        let resp = ifline_response(&existing, false, false);
        guard.store_response(&state.db, &resp.to_string()).await?;
        return Ok(Json(resp));
    }

    // ── 机审（**必须在开事务之前**：单连接池下事务内做网络调用会死锁 PoolTimedOut） ──
    // 🔴 私密不豁免机审：私密只决定「谁能看」，不决定「平台是否为它负责」。
    // 无论裁决都落库，读取面仅 approved 才给正文——人审改判后无需玩家重写（范式同 0037）。
    let ifline_id = new_id("ifw");
    let premise_moderation = if premise.is_empty() {
        // 空前提无内容可审，直接视为通过（读取面本就没有正文可给）。
        "approved".to_string()
    } else {
        match safety::moderate_and_queue(&state, "ifline_premise", &ifline_id, &premise).await? {
            ModerationVerdict::Approved => "approved".into(),
            ModerationVerdict::Pending => "pending".into(),
            ModerationVerdict::Rejected => "rejected".into(),
        }
    };

    // ── 事务：建 if 线 + 烧副本卡 + 审计（三件事同成同败） ─────────────────
    // 🔴 顺序刻意是「先建后烧」（范式同 `subplot::synthesize` 的「先铸后熔」）：
    // 唯一键在动任何一张卡之前就把重复请求挡下，绝不会出现「卡烧了、if 线没建成」。
    let now = now_ms();
    let mut tx = state.db.begin().await?;

    let insert = sqlx::query(
        "INSERT INTO ifline_worlds (id, owner_id, character_id, origin_world_id, origin_template_id, \
         origin_template_version, fork_point, fork_tick_no, fork_state_revision, state_fidelity, \
         snapshot_json, redaction_json, protagonist_in_snapshot, premise, premise_moderation, \
         cost_card_ids_json, status, fork_key, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
    )
    .bind(&ifline_id)
    .bind(&user.user_id)
    .bind(&character_id)
    .bind(&world_id)
    .bind(&world.template_id)
    .bind(world.template_version)
    .bind(&fork_point)
    .bind(terminal)
    .bind(world.state_revision)
    .bind(FIDELITY_ORIGIN_TERMINAL)
    .bind(frozen.snapshot.to_string())
    .bind(frozen.redaction.to_string())
    .bind(i64::from(frozen.protagonist_present))
    .bind(&premise)
    .bind(&premise_moderation)
    .bind(serde_json::to_string(&card_ids).unwrap_or_else(|_| "[]".into()))
    .bind(STATUS_SEALED)
    .bind(&fork_key)
    .bind(now)
    .execute(&mut *tx)
    .await;

    if let Err(e) = insert {
        // 并发下撞唯一键：读回既有那条（开过就是开过），**绝不二次扣卡**。
        // 🔴 显式 `rollback().await` 而不是靠 Drop：单连接池（SQLite/测试）下必须先把连接**确定地**
        // 还回池里，才轮得到下面那次读——靠 Drop 的隐式回滚是延迟的，会把自己锁死在 PoolTimedOut。
        tx.rollback().await?;
        return match find_by_fork_key(&state.db, &user.user_id, &fork_key).await? {
            Some(existing) => {
                let resp = ifline_response(&existing, false, false);
                guard.store_response(&state.db, &resp.to_string()).await?;
                Ok(Json(resp))
            }
            None => Err(e.into()),
        };
    }

    // 烧副本卡：`owned → consumed` 的 CAS（与 `subplot::synthesize` 的回收口逐字同款）。
    // 🔴 本模块**不 INSERT `subplot_cards`**：铸卡的唯一写入路径仍是 `subplot::grant_card_tx`。
    // 抢不到（不属本人 / 已被并发请求熔掉 / 已作为合成材料消耗）→ 整笔回滚，if 线随之消失。
    for cid in &card_ids {
        let res = sqlx::query(
            "UPDATE subplot_cards SET status = $1, consumed_at = $2, consumed_into = $3 \
             WHERE id = $4 AND owner_id = $5 AND status = $6",
        )
        .bind(CARD_STATUS_CONSUMED)
        .bind(now)
        .bind(&ifline_id)
        .bind(cid)
        .bind(&user.user_id)
        .bind(CARD_STATUS_OWNED)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            // 🔴 整笔回滚：if 线不会留下，前面几张卡也不会被熔——「钱货两清」的底线是
            // **要么全成，要么全不成**，绝不允许出现「烧了 2 张、第 3 张没抢到、if 线没开」。
            tx.rollback().await?;
            return Err(ApiError::Conflict(format!(
                "副本卡不在手中或不属于你：{cid}（if 线未开启，未消耗任何资源）"
            )));
        }
    }

    // 全链审计（§0.2）：烧资产必须留痕。
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES ($1, $2, 'user', 'ifline.opened', $3, $4, $5)",
    )
    .bind(new_id("aud"))
    .bind(&user.user_id)
    .bind(format!("ifline:{ifline_id}"))
    .bind(format!(
        "origin={world_id}|forkPoint={fork_point}|tick={terminal}|revision={}|character={character_id}|\
         cards={}|redactedCharacters={}|worldlineChanged=false",
        world.state_revision,
        card_ids.join(","),
        frozen.redaction["removedCharacterIds"].as_array().map(Vec::len).unwrap_or(0),
    ))
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let row = fetch_ifline(&state.db, &ifline_id).await?.ok_or(ApiError::NotFound)?;
    let mut resp = ifline_response(&row, true, false);
    if let Some(obj) = resp.as_object_mut() {
        obj.insert(
            "snapshotSummary".into(),
            json!({
                "characters": frozen.character_count,
                "relations": frozen.relation_count,
            }),
        );
    }
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /me/iflines —— 我的 if 线
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

async fn my_iflines(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    let limit = clamp_page(q.limit);
    let offset = q.offset.unwrap_or(0).max(0);

    // 🔴 `WHERE owner_id = ?` 是硬边界：本端点永远只出本人的 if 线。
    // 状态过滤**下推进 SQL**（不在 Rust 侧过滤已取回的页）：页内过滤会让 limit/offset 与实际条数
    // 对不上，翻页越翻越少，是最容易被当成「数据丢了」的那类 bug（口径同 `annotations::my_appeals`）。
    let rows = match q.status.as_deref() {
        Some(status) => sqlx::query(&format!(
            "SELECT {IFLINE_COLUMNS} FROM ifline_worlds WHERE owner_id = $1 AND status = $2 \
             ORDER BY created_at DESC, id DESC LIMIT $3 OFFSET $4"
        ))
        .bind(&user.user_id)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?,
        None => sqlx::query(&format!(
            "SELECT {IFLINE_COLUMNS} FROM ifline_worlds WHERE owner_id = $1 \
             ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3"
        ))
        .bind(&user.user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?,
    };

    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(ifline_response(&IflineRow::from_row(r)?, false, false));
    }

    Ok(Json(json!({
        "items": items,
        "limit": limit,
        "offset": offset,
        "layer": "ifline",
        "isWorldFact": false,
        "affectsOriginWorld": false,
        "notes": [
            "🔴 if 线是**平行线，不是改写**：原世界的世界线、结算与账本一个字节都不会变（§0.3）。",
            "🔴 if 线的产物只可能是内容，不会是资产——它不产历练、不铸副本卡、不进任何结算账本（§0.1 不卖胜负）。",
            "世界事实的唯一读路径是 /api/worlds/{id}/events；本端点从不返回任何世界事实。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /me/iflines/{id} —— 一条 if 线（含冻结快照）
// ═══════════════════════════════════════════════════════════════════════════

async fn my_ifline_detail(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ifline_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    let row = fetch_ifline(&state.db, &ifline_id).await?.ok_or(ApiError::NotFound)?;
    // 🔴 越权一律 404：不确认「这条 if 线存在」这件事本身（信息边界，§14）。
    if row.owner_id != user.user_id {
        return Err(ApiError::NotFound);
    }
    Ok(Json(ifline_response(&row, false, true)))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /me/iflines/{id}/beats —— 推进一拍（0041）
// ═══════════════════════════════════════════════════════════════════════════

/// 推进一拍。
///
/// 🔴 **玩家拉动，不是调度器推动**（这是一个刻意的设计选择，不是省事）：
/// 世界是按调度器自己往前走的——那是「大家共处的那条线在流逝」。if 线是玩家买下来的私人副本，
/// 由他自己一拍一拍翻页。这条差别有两个直接后果：
/// ① **没有任何调度器会碰 `ifline_worlds`**，于是 if 线不可能被世界 tick 管线顺手捡走
///    （`runtime::scheduler_loop` 只扫 `worlds`）；
/// ② 付费内容的消耗节奏由付费的人掌握，不会在他没看的时候自己烧完。
///
/// ⚠️ **遗留（本批次未做）**：本端点在请求内同步调用模型，长回合会是一个长连接请求。
/// 生产化应改为「入队 + 后台 worker + 轮询/推送」（`queue` 模块已具备该能力）。
/// 之所以本批次不做：if 线默认关闭、状态只标到 `Implemented`，而加一条独立的
/// worker 循环会把改动面显著放大且需要单独评审。已写进 `docs/VALIDATION.md` 的遗留栏。
async fn advance_ifline(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ifline_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    let row = fetch_ifline(&state.db, &ifline_id).await?.ok_or(ApiError::NotFound)?;
    // 🔴 越权一律 404（信息边界，§14）：不确认「这条 if 线存在」这件事本身。
    if row.owner_id != user.user_id {
        return Err(ApiError::NotFound);
    }

    // 幂等：同一次点击的 HTTP 重试不重复推进。载荷为空，故 hash 固定——
    // 真正的并发闸是 `(ifline_id, beat_no)` 唯一键（见 `runner::advance_one_beat`），
    // 幂等键只挡「同一次点击被网络层重放」。
    let endpoint = "POST /me/iflines/:id/beats";
    let payload_hash = idempotency::hash_payload(ifline_id.as_bytes());
    let idem_key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let guard =
        idempotency::guard(&state.db, &user.user_id, endpoint, idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or_else(|_| json!({}))));
    }

    let outcome = runner::advance_one_beat(&state, &row, None).await?;
    let fresh = fetch_ifline(&state.db, &ifline_id).await?.ok_or(ApiError::NotFound)?;
    let resp = beat_response(&outcome, &fresh);
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}

/// 一拍的响应体。恒带层次标签 —— 与 if 线的其它读取面同口径。
fn beat_response(o: &runner::BeatOutcome, row: &IflineRow) -> Value {
    json!({
        "beat": {
            "id": o.beat_id,
            "beatNo": o.beat_no,
            "status": o.status,
            // 未过审的正文只回状态不回正文（私密不豁免机审，范式同 0037 / 0039）。
            "prose": o.prose_visible,
            "proseWithheld": o.prose_visible.is_none() && o.status == "done",
            "moderation": o.moderation,
            "cast": o.cast,
            // 🔴 成本对玩家也可见：他花的是自己的副本卡，平台花了多少算力不该是黑箱。
            "costTokens": o.cost_tokens,
            // 本拍是否把这条线送到了终局（'' = 没有）。取自本次推进的结果而不是回读的行——
            // 「这一拍做了什么」与「这条线现在什么样」是两个问题，分开答才不会在并发下互相冒充。
            "terminalReason": o.ending_reason,
            "endingLabel": o.ending_label,
            "note": o.note,
        },
        "ifline": {
            "id": row.id,
            "status": o.ifline_status,
            "progress": runner::progress_json(row),
            "ending": runner::ending_json(row),
        },
        // ── 层次标签（恒 false 且写进响应：对客户端明说原世界一个字节都没动） ──
        "layer": "ifline",
        "isWorldFact": false,
        "affectsOriginWorld": false,
        "notes": [
            "🔴 推进 if 线**不写**世界线：worlds / world_events / world_ticks / world_members 一个字节都没变（§0.3 公共事实不可回滚）。",
            "🔴 if 线的产出只可能是内容：这一拍的正文。它不发历练、不铸副本卡、不发荣誉、不记贡献（§0.1 付费只买体验容量，永不买结果）。",
            "🔴 单人平行线：其他玩家的角色已按 §14 剥离，每一拍的演员表都可审（cast）。",
        ],
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /me/iflines/{id}/beats —— 这条平行线的正文（**终局产物就是这份东西**）
// ═══════════════════════════════════════════════════════════════════════════

/// 按拍序读回正文 = 那份只属于你的私人传记。
///
/// 🔴 这是 if 线终局产物的**全部形态**：一份可读的文本，加两个字符串（结局原因、结局名）。
/// 没有第二个端点会把它兑换成任何东西——因为不存在那样的东西。
async fn list_beats(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ifline_id): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled(&state.db, Some(&user.user_id), None).await?;
    let row = fetch_ifline(&state.db, &ifline_id).await?.ok_or(ApiError::NotFound)?;
    if row.owner_id != user.user_id {
        return Err(ApiError::NotFound);
    }
    let limit = clamp_page(q.limit);
    let offset = q.offset.unwrap_or(0).max(0);

    let rows = sqlx::query(
        "SELECT id, beat_no, status, prose, moderation, cast_json, cost_tokens, terminal_reason, \
         note, created_at FROM ifline_beats WHERE ifline_id = $1 \
         ORDER BY beat_no ASC LIMIT $2 OFFSET $3",
    )
    .bind(&ifline_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let moderation: String = r.try_get("moderation").unwrap_or_default();
        let prose: String = r.try_get("prose").unwrap_or_default();
        let approved = moderation == "approved";
        items.push(json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "beatNo": r.try_get::<i64, _>("beat_no").unwrap_or(0),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "prose": if approved && !prose.is_empty() { Value::String(prose) } else { Value::Null },
            "proseWithheld": !approved,
            "moderation": moderation,
            "cast": parse_json_col(&r.try_get::<String, _>("cast_json").unwrap_or_default()),
            "costTokens": r.try_get::<i64, _>("cost_tokens").unwrap_or(0),
            "terminalReason": r.try_get::<String, _>("terminal_reason").unwrap_or_default(),
            "note": r.try_get::<Option<String>, _>("note").unwrap_or(None),
            "createdAt": r.try_get::<i64, _>("created_at").unwrap_or(0),
        }));
    }

    Ok(Json(json!({
        "iflineId": row.id,
        "items": items,
        "limit": limit,
        "offset": offset,
        "progress": runner::progress_json(&row),
        "ending": runner::ending_json(&row),
        "layer": "ifline",
        "isWorldFact": false,
        "affectsOriginWorld": false,
        // 🔴 明写在每一次读取上：产物是内容，账户里什么都没多。
        "grantedAssets": [],
        "notes": [
            "🔴 这是 if 线终局产物的**全部形态**：一份可读的私人传记 + 结局名。没有任何端点会把它兑换成资产。",
            "🔴 这些正文**不是世界事实**：它们不在 world_events 里，别人读不到，也不改变原世界的任何一行（§0.3）。",
            "世界事实的唯一读路径是 /api/worlds/{id}/events；本端点从不返回任何世界事实。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /admin/iflines —— 运营列表（只读）
// ═══════════════════════════════════════════════════════════════════════════

async fn list_iflines_admin(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_operator(&admin)?;
    // 🔴 运营面同样受开关约束（读取侧降级一致），但判定用 `ensure_ops_enabled`：
    // 只要入口对任何人开放过就能查，否则按世界灰度时已烧掉玩家副本卡的 if 线运营看不见。
    ensure_ops_enabled(&state.db).await?;

    let limit = clamp_page(q.limit);
    let offset = q.offset.unwrap_or(0).max(0);
    let status = q.status.unwrap_or_else(|| STATUS_SEALED.to_string());
    let rows = sqlx::query(&format!(
        "SELECT {IFLINE_COLUMNS} FROM ifline_worlds WHERE status = $1 \
         ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3"
    ))
    .bind(&status)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let a = IflineRow::from_row(r)?;
        // 🔴 不下发 `owner_id`（§14：与 `annotations` 的复核队列同口径，运营看得到事，看不到人名）。
        items.push(json!({
            "id": a.id,
            "originWorldId": a.origin_world_id,
            "characterId": a.character_id,
            "forkPoint": a.fork_point,
            "forkTickNo": a.fork_tick_no,
            "stateFidelity": a.state_fidelity,
            "status": a.status,
            "premiseModeration": a.premise_moderation,
            "costCardIds": parse_json_col(&a.cost_card_ids_json),
            // ── 推进态（0041）：运营排查「这条线跑到哪了、花了多少」 ──
            "beatCount": a.beat_count,
            "costTokensTotal": a.cost_tokens_total,
            "endingReason": a.ending_reason,
            "endingLabel": a.ending_label,
            "endedAt": a.ended_at,
            "createdAt": a.created_at,
        }));
    }
    Ok(Json(json!({
        "items": items,
        "status": status,
        "limit": limit,
        "offset": offset,
        "layer": "ifline",
        "affectsOriginWorld": false,
        "notes": [
            "🔴 if 线不是 worlds 行：它不进世界列表、不进发现页、不进结算管线、不进世界线 SLO 与贡献账本。",
            "🔴 开一条 if 线会**烧掉**玩家的副本卡（subplot_cards: owned → consumed，consumed_into 指向本条 if 线 id）。客诉排查从这条反向血缘入手。",
            "生命周期：sealed（已立项未推进）→ running（已跑过拍）→ ended（已收尾）。按 status 过滤取这三个值。",
            "🔴 ended 的 if 线**没有发放任何资产**：终局只写 endingReason/endingLabel 两个字符串，审计见 audit_logs 的 ifline.ended（reason 里明写 grantedAssets=none|settlementEntered=none）。",
        ],
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /admin/iflines/cost —— 🔴 成本读数（0041）
// ═══════════════════════════════════════════════════════════════════════════

/// if 线的 token 开销聚合。
///
/// 🔴 **为什么单开这个端点**：if 线推进烧 token 但**不能写 `world_ticks`**（那是世界线的表，
/// 写进去就等于把 if 线接回会自动结算的那条链路）。于是它的开销不在主成本看板的 SUM 里——
/// 付费功能的成本失真是最不该有的失真，所以宁可多一个端点，也不让这笔钱在任何地方都查不到。
///
/// ⚠️ **现状（写在响应里，不靠人记）**：`admin_api::dashboards` 的主看板尚未并入本项开销。
/// 接入是一句 SQL（索引 `idx_ifline_beats_created` 已建好），见响应的 `dashboardIntegration`。
async fn ifline_cost_admin(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<CostQuery>,
) -> Result<Json<Value>, ApiError> {
    require_operator(&admin)?;
    ensure_ops_enabled(&state.db).await?;

    // 时间窗：BIGINT 毫秒比较（双库可移植——不用 strftime / date_trunc）。
    let until = q.until.unwrap_or_else(now_ms);
    // 默认回看 30 天。窗口是参数，不是字面量（§0.2）。
    let since = q.since.unwrap_or(until - 30 * 24 * 60 * 60 * 1000);

    let row = sqlx::query(
        "SELECT CAST(COALESCE(SUM(cost_tokens), 0) AS BIGINT) AS tokens, \
         CAST(COUNT(*) AS BIGINT) AS beats \
         FROM ifline_beats WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(since)
    .bind(until)
    .fetch_one(&state.db)
    .await?;
    let tokens: i64 = row.try_get("tokens").unwrap_or(0);
    let beats: i64 = row.try_get("beats").unwrap_or(0);

    let lines = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n, \
         CAST(COALESCE(SUM(cost_tokens_total), 0) AS BIGINT) AS total \
         FROM ifline_worlds WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(since)
    .bind(until)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({
        "since": since,
        "until": until,
        "beats": beats,
        "costTokens": tokens,
        "iflinesOpened": lines.try_get::<i64, _>("n").unwrap_or(0),
        "costTokensByIflineCreatedAt": lines.try_get::<i64, _>("total").unwrap_or(0),
        "layer": "ifline",
        "dashboardIntegration": {
            "mainDashboardIncludesIfline": false,
            "why": "主看板只 SUM world_ticks.cost_tokens；if 线的拍**不能**落 world_ticks——那是世界线的表，落进去会把 if 线接回 commit_tick → end_world_tx → settle_* 那条自动结算链路，踩穿「付费只买体验容量」。",
            "howToIntegrate": "在 admin_api::dashboards 的成本聚合里并上：SELECT SUM(cost_tokens) FROM ifline_beats WHERE created_at >= $1 AND created_at < $2（索引 idx_ifline_beats_created 已建好）。",
            "status": "本批次未接：dashboards.rs 由并行批次在改，跨批次抢改会把两边的账都搅乱。已登记在 docs/VALIDATION.md 的遗留栏。",
        },
        "notes": [
            "🔴 这里的数字是**花出去的算力**，不是发下来的收益。if 线没有任何产出型数值列。",
            "推进上限 MUSE_IFLINE_MAX_BEATS 是成本闸：一张副本卡换一条 if 线，若推进无限则单条开销无界。",
        ],
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CostQuery {
    /// 起（含），BIGINT 毫秒。缺省 = `until - 30 天`。
    #[serde(default)]
    since: Option<i64>,
    /// 止（不含），BIGINT 毫秒。缺省 = 现在。
    #[serde(default)]
    until: Option<i64>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 行与响应
// ═══════════════════════════════════════════════════════════════════════════

const IFLINE_COLUMNS: &str = "id, owner_id, character_id, origin_world_id, origin_template_id, \
     origin_template_version, fork_point, fork_tick_no, fork_state_revision, state_fidelity, \
     snapshot_json, redaction_json, protagonist_in_snapshot, premise, premise_moderation, \
     cost_card_ids_json, status, fork_key, created_at, run_seed, live_state_json, live_revision, \
     beat_count, cast_json, cost_tokens_total, ending_reason, ending_label, ended_at";

#[derive(Debug, Clone)]
struct IflineRow {
    id: String,
    owner_id: String,
    character_id: String,
    origin_world_id: String,
    origin_template_id: String,
    origin_template_version: i64,
    fork_point: String,
    fork_tick_no: i64,
    fork_state_revision: i64,
    state_fidelity: String,
    /// 🔴 **冻结的分叉态，永不被推进覆盖**（分叉点证据；推进写的是 `live_state_json`）。
    snapshot_json: String,
    redaction_json: String,
    protagonist_in_snapshot: i64,
    premise: String,
    premise_moderation: String,
    cost_card_ids_json: String,
    status: String,
    created_at: i64,
    // ── 推进态（0041） ──────────────────────────────────────────────────────
    /// 确定性种子（十六进制 u64 文本；'' = 尚未推进过）。首次推进钉死后永不改写。
    run_seed: String,
    /// 当前**活的**叙事态（'' = 尚未推进过，此时以 `snapshot_json` 为准）。
    live_state_json: String,
    /// 活态修订号（推进的 CAS 令牌）。
    live_revision: i64,
    beat_count: i64,
    /// 阵容与场景（首次推进时从原世界装配复制一次后钉死）。
    cast_json: String,
    /// 🔴 累计 token 开销。**花出去的成本，不是发下来的收益**——本表唯一的数字列，方向与产出相反。
    cost_tokens_total: i64,
    ending_reason: String,
    ending_label: String,
    ended_at: Option<i64>,
}

impl IflineRow {
    fn from_row(r: &sqlx::any::AnyRow) -> Result<Self, ApiError> {
        Ok(Self {
            id: r.try_get("id")?,
            owner_id: r.try_get("owner_id")?,
            character_id: r.try_get("character_id")?,
            origin_world_id: r.try_get("origin_world_id")?,
            origin_template_id: r.try_get("origin_template_id")?,
            origin_template_version: r.try_get("origin_template_version")?,
            fork_point: r.try_get("fork_point")?,
            fork_tick_no: r.try_get("fork_tick_no")?,
            fork_state_revision: r.try_get("fork_state_revision")?,
            state_fidelity: r.try_get("state_fidelity")?,
            snapshot_json: r.try_get("snapshot_json")?,
            redaction_json: r.try_get("redaction_json")?,
            protagonist_in_snapshot: r.try_get("protagonist_in_snapshot")?,
            premise: r.try_get("premise")?,
            premise_moderation: r.try_get("premise_moderation")?,
            cost_card_ids_json: r.try_get("cost_card_ids_json")?,
            status: r.try_get("status")?,
            created_at: r.try_get("created_at")?,
            run_seed: r.try_get("run_seed").unwrap_or_default(),
            live_state_json: r.try_get("live_state_json").unwrap_or_default(),
            live_revision: r.try_get("live_revision").unwrap_or(0),
            beat_count: r.try_get("beat_count").unwrap_or(0),
            cast_json: r.try_get("cast_json").unwrap_or_default(),
            cost_tokens_total: r.try_get("cost_tokens_total").unwrap_or(0),
            ending_reason: r.try_get("ending_reason").unwrap_or_default(),
            ending_label: r.try_get("ending_label").unwrap_or_default(),
            ended_at: r.try_get("ended_at").unwrap_or(None),
        })
    }
}

async fn fetch_ifline(db: &AnyPool, id: &str) -> Result<Option<IflineRow>, ApiError> {
    let row = sqlx::query(&format!("SELECT {IFLINE_COLUMNS} FROM ifline_worlds WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    row.as_ref().map(IflineRow::from_row).transpose()
}

async fn find_by_fork_key(
    db: &AnyPool,
    owner_id: &str,
    fork_key: &str,
) -> Result<Option<IflineRow>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {IFLINE_COLUMNS} FROM ifline_worlds WHERE owner_id = $1 AND fork_key = $2"
    ))
    .bind(owner_id)
    .bind(fork_key)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(IflineRow::from_row).transpose()
}

/// JSON 文本列 → `Value`（数组或对象皆可）。解析失败回落空对象：
/// 一条读取面绝不能因为一列脏数据整体 500——那会让玩家连自己烧了几张卡都查不到。
fn parse_json_col(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({}))
}

/// 一条 if 线的响应体。
///
/// 🔴 **不含 `ownerId`**（§14：owner 只用于 SQL 过滤，从不出现在响应里）。
/// 🔴 恒带 `layer` / `isWorldFact` / `affectsOriginWorld` / `forkPoint.stateFidelity`：
/// 前四个字段是「if 线不冒充原世界线」与「分叉点不假装」在读取面的保证——
/// 前端拿到任意一条都能一眼分清它是哪一层、以及它的分叉态有多准。
/// 未过审的分叉前提只回状态不回正文（`withheld=true`，范式同 0037 的批注）。
fn ifline_response(row: &IflineRow, created: bool, include_snapshot: bool) -> Value {
    let approved = row.premise_moderation == "approved";
    let mut out = json!({
        "id": row.id,
        "created": created,
        // ── 层次标签（读取面可区分的硬保证） ──
        "layer": "ifline",
        "isWorldFact": false,
        // 🔴 恒 false 且**写进响应**：对客户端明说「原世界一个字节都没动」，
        // 免得前端/玩家把「开了 if 线」误当成「那一拍被改写了」。
        "affectsOriginWorld": false,
        "status": row.status,
        "characterId": row.character_id,
        "protagonistInSnapshot": row.protagonist_in_snapshot != 0,
        "origin": {
            "worldId": row.origin_world_id,
            "templateId": row.origin_template_id,
            "templateVersion": row.origin_template_version,
            // 只读指针：本模块对原世界只有 SELECT。
            "readOnly": true,
        },
        "forkPoint": {
            "kind": row.fork_point,
            "tickNo": row.fork_tick_no,
            "stateRevision": row.fork_state_revision,
            // 🔴 保真度必须随每一次读取下发：玩家永远知道自己拿到的是哪一档。
            "stateFidelity": row.state_fidelity,
            "isApproximate": row.state_fidelity != FIDELITY_ORIGIN_TERMINAL,
        },
        // ── 推进（0041） ──
        "progress": runner::progress_json(row),
        // 🔴 终局投影恒带 `grantedAssets: []`：对客户端和玩家直说「你读到的是内容，账户里什么都没多」。
        "ending": runner::ending_json(row),
        "redaction": parse_json_col(&row.redaction_json),
        "premise": if approved && !row.premise.is_empty() { Value::String(row.premise.clone()) } else { Value::Null },
        "premiseWithheld": !approved,
        "premiseModeration": row.premise_moderation,
        "cost": {
            "subplotCardIds": parse_json_col(&row.cost_card_ids_json),
            "note": "这些副本卡已烧掉（subplot_cards: owned → consumed，consumed_into 指向本条 if 线）。§10 内容燃料，非货币。",
        },
        "createdAt": row.created_at,
        "notes": [
            "🔴 平行线，不是改写：原世界的 world_events / narrative_state / 结算账本一个字节都没变（§0.3）。",
            "🔴 单人副本：其他玩家的角色已按 §14 剥离，台账见 redaction；推进时每一拍再剥一次。",
            "🔴 if 线不产出任何资产：不发历练、不铸副本卡、不进结算与贡献账本（§0.1 付费只买体验容量）。",
            "分叉点保真度见 forkPoint.stateFidelity；当前只支持终局分叉，原因见 GET /api/worlds/{id}/ifline-fork-points。",
            "推进走 POST /api/me/iflines/{id}/beats（一次一拍）；正文读 GET /api/me/iflines/{id}/beats。",
            "🔴 snapshot 是**冻结的分叉点证据**，推进永不覆盖它——覆盖了 stateFidelity 就无从核验。当前活态在服务端另存。",
        ],
    });
    if include_snapshot {
        if let Some(obj) = out.as_object_mut() {
            // 冻结的分叉态（**死数据**：与原世界再无同步通道）。只对本人出。
            obj.insert(
                "snapshot".into(),
                serde_json::from_str::<Value>(&row.snapshot_json).unwrap_or_else(|_| json!({})),
            );
        }
    }
    out
}
