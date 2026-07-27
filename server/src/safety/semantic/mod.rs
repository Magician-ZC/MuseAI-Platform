//! §15 **第 3 层**：语义分类**异步**复核（`docs/build/spec-world-ecosystem.md` §15 五层漏斗）。
//!
//! 落地的是 `safety` 模块头与 `providers::ModerationProvider::check_text` 上挂了很久的
//! `TODO(§15-L3)`。那条 TODO 已经把形态想清楚了，本模块逐条照做，没有改口径。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 先说清楚它现在**拦不住任何东西**
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `ModerationProvider` 当前的唯一实现是 **Dev 桩**（`providers::DevModeration`：一张小关键词表，
//! 其余一律直过）。所以第 3 层接通之后，**真实语义分类一次都没有发生**。
//!
//! 本模块交付的是**管线**（状态语言七档里的 `Implemented`），不是**防线**：
//! 触发时机、事务边界、收紧语义、失败方向、抽样口径、成本口径、可观测面全部就位，
//! 接真实服务商 = 换一个 `ModerationProvider` 实现并把 [`ModerationProvider::is_dev_stub`]
//! 覆写为 `false`，领域代码一行不动。
//!
//! 🔴 **因此任何文档 / 看板 / 评审材料都不得据此表述为「五层漏斗已完整」或「内容安全已就绪」。**
//! 内容安全是合规主体责任——一条纯桩的链路被读成已生效的防线，代价比功能缺失大得多。
//!
//! 这句话**不只写在注释里**。「当前是桩」这个事实随数据一起走，共三处，
//! 范式抄 `slo::quality::QualitySource::SimulatedStub`（把桩的事实做成随报告 JSON 走的字段）：
//!
//! | 载体 | 字段 |
//! |---|---|
//! | 台账表 `safety_recheck_runs` | `provider_stub INTEGER`（一等列，每行都带） |
//! | 每一条风控留痕 `risk_events.detail_json` | `"providerStub": true` |
//! | 运营面 `GET /admin/safety/recheck` | `providerStub` + `source` + `honesty[]` 诚实边界数组 |
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 绝不进 tick 事务
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `check_text` 是**网络调用**。放进 `runtime::commit_tick` 的事务会：
//! (a) 单连接池（测试 / SQLite dev）下 tx 持有唯一连接，调用期间任何再借连接的操作直接死锁
//!     `PoolTimedOut`（同 `record_risk_tx` 的注释）；
//! (b) 让 tick 事务的持有时长被外部 RTT 绑架——而那把 `world_event_seq` 的行级排他锁就在同一事务里。
//!
//! 所以本模块**从不 `begin()`**，全程走连接池上的单条语句（源码级红线用例
//! `red_line_never_opens_a_transaction` 扫死）。`commit_tick` 里新增的唯一一行是
//! `enqueue_after_commit`，它排在 `tx.commit()` **之后**，本身只往进程内队列丢一个
//! `{worldId, tickNo}`，无 IO。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 触发载体：为什么是 `queue` 而不是 `tokio::spawn`
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 三个候选：① tick 提交后直接 `tokio::spawn` 一个任务；② 复用 `runtime` 的 worker 循环；
//! ③ 走 `queue`（`MemQueue`）+ 自己的 worker 循环。选 ③。
//!
//! - **不选 ①**：`tokio::spawn` 没有并发上限。一批世界同时提交拍，就同时发出等量的模型调用——
//!   而这条链路**烧 token**。成本必须有闸，闸就是 worker 数（`MUSE_SAFETY_L3_WORKERS`）。
//! - **不选 ②**：`runtime` 的 worker 是**引擎拍**的执行体，一个 job 就是一次完整的 `run_round`。
//!   把网络复核塞进同一个池，会让审核的排队时间与世界推进的排队时间互相绑架：
//!   provider 慢一点，世界就少跑几拍。两件事的背压必须分开。
//! - **选 ③** 还白拿两样：`due_ms` 天然是**退避重排**的原语（重试不需要占着一个任务 sleep），
//!   `topic` 天然把审核流量与 tick 流量隔在两条独立队列上。
//!
//! ⚠️ **已知边界**：`MemQueue` 是进程内内存队列，**不持久**。进程重启时在飞的复核任务会丢，
//! 那一拍的事件就停留在 `approved` 且再无人复核。这不是本模块引入的新性质（tick 队列同理），
//! 但对审核链更要紧，故如实登记为遗留：接 Redis 实现（`queue::Queue` trait 不变）或加一条
//! 「扫尾未复核拍」的补偿轮询即可闭合。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 只收紧、不改写
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 本模块对 `world_events` 的**唯一**写入是：
//!
//! ```sql
//! UPDATE world_events SET moderation = $1 WHERE id = $2 AND moderation = 'approved'
//! ```
//!
//! - `SET` 列表里**只有 `moderation` 一列**：正文（`public_projection_json` /
//!   `private_projections_json` / `arbiter_note`）一个字节不动。已落定的是公共事实
//!   （§0.3 公共事实不可回滚），第 3 层能做的只有「未过审 → 不外发」，不能改人家说过的话。
//! - `WHERE` 里钉着 `moderation = 'approved'`：**单向棘轮**。既不会把第 2 层已经拦下的
//!   `pending` 悄悄放宽回 `approved`，也不会覆盖人审已经落下的更严裁决。
//! - 本模块**永不写 `'approved'`**（红线用例扫源码）。
//!
//! 收紧之后，事件在**全部读取面**消失：`events::VISIBLE_EVENTS_SQL` 的审核门、
//! WS 推送、日报聚合、直播播出面、战报/回放，口径都是 `moderation = 'approved'`。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 与 §15 第 4 层（直播场延迟缓冲）怎么配合
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `livestage` 的模块头写着「第 3 层异步复核……在缓冲期内把 `moderation` 收紧。
//! **本模块就是它等的那个窗口**」。配合方式是**零耦合**的：
//!
//! - 播出面本来就只出 `moderation='approved'`，所以第 3 层只要在水位线越过该拍之前收紧，
//!   直播观众**根本不会看到**那条内容。不需要 `livestage` 改一行代码。
//! - 本模块只**读**一次 `live_sessions.published_high_tick`（该世界的最大值），用来给每次复核
//!   算一个 `intercepted_before_broadcast`：**窗口够不够用的度量**，口径对齐
//!   `live_withholds.preemptive`。它是运营调 `MUSE_LIVE_DELAY_TICKS` 的依据。
//!
//! 🔴 **窗口内命中了，能不能通知已经拉过该拍的观众？做不到，也不该做。**
//! ① HTTP `feed` 的响应字节一旦返回就收不回；WS 只推 `approved`，早于收紧推出去的那条
//!    在推的当时确实是 approved。**已经看见的收不回**——这与 `livestage::withhold` 对
//!    「播出后撤下」的如实标注是同一句话。
//! ② 就算能推「刚才那条被撤下了」，那条通知本身会把被拦内容重新指出来（越描越黑），
//!    对未看见的人反而是一次曝光。
//! 所以能做的只有两件：**停止后续外发** + **如实度量 preemptive 占比**。两件都做了。
//!
//! ⚠️ 还要说清一件事：延迟缓冲**只作用于世界外**。世界成员的 `/worlds/{id}/events` 不延迟，
//! 所以对成员而言第 3 层**恒为事后收紧**。`intercepted_before_broadcast=1` 只说明
//! 直播观众没看见，不等于「没人看见」。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 fail-closed：审核链自己的故障，绝不转化为放行
//! ════════════════════════════════════════════════════════════════════════════
//!
//! provider 超时 / 报错 / 返回不了裁决时：
//!
//! ```text
//!   第 1..max-1 次尝试失败 → 退避重排（内容仍为 approved）   ← 窗口期，fail-open
//!   第 max 次仍失败        → 收紧为 'pending' + 记险 + 无条件入人审队列  ← 终局，fail-closed
//! ```
//!
//! **方向与 `MUSE_SAFETY_LEXICON` 自洽**：那个开关的 fail-safe 值是**开**（继续过滤），
//! 因为「审核链关掉 = 放行」。同理，若这里 fail-open，那么**打掉审核 provider 就成了绕过
//! 第 3 层的手段**——审核链的可用性会变成内容安全的上限。这个方向**不参数化**
//! （阈值才参数化，方向是红线），由用例 `red_line_provider_outage_fails_closed_not_open` 锁死。
//!
//! 为什么允许重试窗口内 fail-open：那段时间正是第 4 层的缓冲窗口，直播观众还没看到；
//! 而对世界成员来说内容早已下发（见上），此刻「先撤后查」既拦不住已发生的阅读，
//! 又会把 provider 的每一次抖动放大成世界叙事的空洞。**先重试、再收紧**是这两难里唯一
//! 不牺牲方向的解。重试次数 / 退避 / 超时全部参数化（§0.2）。
//!
//! ✅ **已闭合（原登记为遗留）**：`admin_api::audit` 此前对 `world_event` 主体没有回写分支——
//! 人审在队列里点「通过」不会把 `world_events.moderation` 写回 `approved`，而本层是
//! fail-closed 的（provider 每抖动一次就收紧一批并无条件入队），于是内容会**永久卡在 `pending`**。
//! migration 0047 补上了那条路径：
//!
//! - **放宽是第二条写路径，全仓只有一条**，形状为
//!   `UPDATE world_events SET moderation = 'approved' WHERE id = $1 AND moderation = $2
//!    AND moderation IN ('pending', 'rejected')`
//!   ——`SET` 仍只有 `moderation` 一列（正文零改写），起点白名单写死在 SQL 里
//!   （因此永不从 `'approved'` 自我放宽，也永不复活 `'takedown'`），并按 `world_events.id`
//!   **主键**点名一行（`domain_event_id` 跨世界重名，见 0047 文件头）。
//! - **权限两档**：reviewer 只能推翻**机器**收紧；被人审驳回过的事件只能由 admin 走
//!   `POST /admin/audit-queue/{id}/reinstate`（口径抄 0044 的 restricted/removed 两档）。
//! - 本层的棘轮**一个字符都没改**：仍是「只从 `approved` 出发收紧」。两条路径的形状由
//!   源码级红线用例 `red_line_world_events_has_one_ratchet_and_one_guarded_relax` 一并锁死。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 抽样：公开全量、私有抽样，且**确定性**
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 口径来自 TODO 原文：公开投影全量、私有抽样。两档抽样率都是**万分比整数**参数
//! （公开档默认 10000 = 全量，留着是 VALIDATION §2 T5 预案「审核成本失控」的降档旋钮）。
//!
//! 抽样**不掷系统随机数**：种子 = `fnv1a_64(world_id ‖ tick_no ‖ domain_event_id) ^ 0x5C`，
//! 过 `assembly::Rng`（SplitMix64 整数流）。于是同一条事件被抽中与否是**可复现**的——
//! 重试不会换一批样本，复盘时也算得回来。禁三样（系统随机 / 浮点 RNG / map 迭代序驱动 RNG）
//! 逐条守住；域常量 `0x5C` 已登记进 `assembly` 的唯一清单。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 成本：进了看板，但**比值仍算不出来**（如实说）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! VALIDATION §2 T5 有一条门槛「**内容审核成本 ≤ 生成成本的 5%**」，此前**没有数据源**。
//! 本模块补的是分子侧的口径起点：`safety_recheck_runs` 逐次尝试记下送审条数、送审字符数、
//! 命中数、重试与耗时；分母侧（生成成本）一直在 `world_ticks.cost_tokens` 里。
//!
//! ⚠️ 但**比值本身现在算不出来**，本模块不假装能算：`check_text` 只回裁决，不回 token 也不回费用；
//! 桩的调用成本更是恒为 0。所以运营面响应里 `cost.ratioAvailable = false`，
//! 并写明缺的是哪一半。等真实 provider 带来计价口径，把它填进 `chars_checked` 的换算即可。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 未验证功能默认关闭（VALIDATION §0.1）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 运行时开关 **`MUSE_SAFETY_SEMANTIC_RECHECK`**，**默认关闭**，经 `crate::flags` 统一入口解析
//! （链 user > world > global > env > 代码内默认值）。关闭时 `commit_tick` 里那一行的唯一副作用
//! 是一次开关解析，**行为与接线前逐字节一致**（用例 `disabled_is_byte_identical_to_before_wiring`）。
//!
//! ℹ️ **开关名与调参名的前缀不同**，是刻意的：开关会出现在运营后台的登记表里，名字要自解释
//! （`MUSE_SAFETY_SEMANTIC_RECHECK`）；调参只在部署侧改，用短前缀 **`MUSE_SAFETY_L3_*`** 便于一次 grep
//! 捞全（抽样率 / 超时 / 重试 / 退避 / 单拍上限 / worker 数）。两组名字在运营面响应里同时出现
//! （`flag` 字段 + `sampling` / `failClosedPolicy` / `cost.levers`），不需要靠记忆做映射。
//!
//! ⚠️ 有人会问：`MUSE_SAFETY_LEXICON` 是审核链、默认为**开**，为什么这条审核链默认**关**？
//! 因为两者要保护的东西不同：词库闸**已经在线上跑着**，它的 default-on 保护的是「别让一次配置
//! 失误把既有过滤悄悄关掉」；而第 3 层**从未生效过**，把一条从未跑过的链路默认开启，
//! 等于让「合并代码」直接改变线上行为并开始烧 token——那正是 §0.1 明令禁止的。
//! 两者在**同一条原则**上是一致的：**默认值指向「不改变现状」的那一侧**。
//! 等第 3 层接了真实 provider 并验证过，它的默认值应当重新评审为开（届时 fail-safe 方向随之翻转）。

use std::time::Duration;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::db::{new_id, now_ms, Placeholders};
use crate::error::ApiError;
use crate::providers::ModerationVerdict;

use super::{record_risk, verdict_str, Severity};

#[cfg(test)]
pub(crate) mod testkit;
#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 开关与参数（§0.1 默认关闭 · §0.2 全部参数化）
// ═══════════════════════════════════════════════════════════════════════════

/// 第 3 层运行时开关（**开关名即 env 变量名**，见 `flags` 模块头）。
pub(crate) const ENV_SEMANTIC_RECHECK: &str = "MUSE_SAFETY_SEMANTIC_RECHECK";

/// 默认 = **关闭**。理由见模块头最后一节（不是「审核可以不做」，是「不改变现状」）。
const DEFAULT_SEMANTIC_RECHECK_ENABLED: bool = false;

/// 🔴 **编译期钉死**：默认值出现在两处（本常量 + `flags::KNOWN_FLAGS` 登记表），
/// 两处不一致就是「默认关闭」这条 §0.1 约束有了两个事实源。改一处不改另一处直接编不过。
/// 范式抄 `livestage` / `annotations` / `social`。
const _: () = assert!(
    crate::flags::declared_default(ENV_SEMANTIC_RECHECK) == DEFAULT_SEMANTIC_RECHECK_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_SAFETY_SEMANTIC_RECHECK 的默认值必须与 DEFAULT_SEMANTIC_RECHECK_ENABLED 一致"
);

/// 复核任务队列 topic（与 tick 队列 `world_tick` 物理隔离，见模块头「触发载体」）。
const TOPIC: &str = "safety_semantic_recheck";

/// 万分比满档 = 全量。抽样率一律整数万分比（禁浮点 RNG）。
const SAMPLE_BP_FULL: i64 = 10_000;

/// 公开投影抽样率（万分比）。默认**全量**——TODO 原文口径。
/// 留成参数是为了 VALIDATION §2 T5 预案「审核成本失控 → 降档」，不是给日常调的。
const ENV_PUBLIC_SAMPLE_BP: &str = "MUSE_SAFETY_L3_PUBLIC_SAMPLE_BP";
const DEFAULT_PUBLIC_SAMPLE_BP: i64 = SAMPLE_BP_FULL;

/// 私有投影抽样率（万分比）。默认 5%——TODO 原文「私有抽样」，具体数字是运营配置。
const ENV_PRIVATE_SAMPLE_BP: &str = "MUSE_SAFETY_L3_PRIVATE_SAMPLE_BP";
const DEFAULT_PRIVATE_SAMPLE_BP: i64 = 500;

/// 单次 `check_text` 的超时（毫秒）。超时按 provider 报错处理（走 fail-closed 那条路）。
const ENV_TIMEOUT_MS: &str = "MUSE_SAFETY_L3_TIMEOUT_MS";
const DEFAULT_TIMEOUT_MS: i64 = 5_000;

/// 一拍复核的尝试次数上限（含首次）。到顶仍有报错 → fail-closed 收紧。
const ENV_MAX_ATTEMPTS: &str = "MUSE_SAFETY_L3_MAX_ATTEMPTS";
const DEFAULT_MAX_ATTEMPTS: i64 = 4;

/// 重试退避基数（毫秒）；第 n 次重试的延迟 = base * 2^(n-1)，封顶 `BACKOFF_CAP_SHIFT`。
const ENV_BACKOFF_MS: &str = "MUSE_SAFETY_L3_BACKOFF_MS";
const DEFAULT_BACKOFF_MS: i64 = 2_000;
/// 退避指数封顶（避免 attempt 被配大后溢出成天文数字）。
const BACKOFF_CAP_SHIFT: u32 = 6;

/// 单拍送审条数上限（成本闸：一拍事件再多也不至于把审核预算一次打穿）。
const ENV_MAX_EVENTS: &str = "MUSE_SAFETY_L3_MAX_EVENTS_PER_TICK";
const DEFAULT_MAX_EVENTS: i64 = 200;

/// 复核 worker 并发度（= 同时在飞的 `check_text` 上限，成本与背压的闸）。
const ENV_WORKERS: &str = "MUSE_SAFETY_L3_WORKERS";
const DEFAULT_WORKERS: i64 = 1;

/// 风控留痕的 kind（与第 2 层的 `'lexicon'` 平行按层命名，便于 `/admin/risk-events?kind=` 分流）。
const RISK_KIND: &str = "semantic";

/// 本层在留痕里的层号（第 2 层写 2）。
const LAYER: i64 = 3;

/// 抽样子流域常量。**已登记进 `assembly` 的唯一清单**（那里的「下一个可用」已续到 `0x5D`）。
const DOMAIN_L3_SAMPLE: u64 = 0x5C;

/// 生成侧单价（分 / 1K token）的 env 名。**本模块只读它、绝不给它兜默认值**——见
/// [`explicit_token_price_cents_per_1k`]。（`runtime` 另有一份带默认值的读法，那是给逐拍记账
/// 用的估算；成本比值这条路上不能用估算。）
const ENV_TOKEN_PRICE: &str = "MUSE_TOKEN_CNY_CENTS_PER_1K";

/// VALIDATION §2 T5 门槛「内容审核成本 ≤ 生成成本的 5%」，写成万分比整数（禁浮点）。
const COST_THRESHOLD_BP: i64 = 500;

/// 万分比换算基数。
const BP_BASE: i64 = 10_000;

/// 生成侧单价，**只认显式配置**（未设 / 非正整数 → `None`）。
///
/// 🔴 为什么不回落 `runtime` 的代码内默认估算：T5 是一条**门槛**。拿一个估算单价去算门槛，
/// 得到的是「估算的估算」，而它在看板上和真值长得一模一样——那正是「摆一个假的 5%」。
/// 缺任何一半，`cost.ratioAvailable` 就保持 `false` 并说明缺的是哪一半。
fn explicit_token_price_cents_per_1k() -> Option<i64> {
    std::env::var(ENV_TOKEN_PRICE).ok()?.trim().parse::<i64>().ok().filter(|v| *v > 0)
}

/// 「每 1000 单位 × 单价（分）」→ 分。整数运算（禁浮点：金额与门槛判定都必须可复现）。
fn cost_cents(units: i64, price_per_1k: i64) -> i64 {
    units.saturating_mul(price_per_1k) / 1000
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(default)
}

/// 抽样率夹逼：越界值一律夹回 `[0, 10000]`，**配错不得静默放宽或收紧**（同 `runtime_audit_admits`
/// 对配错值的处理）。抽出来是为了能在不碰进程级 env 的前提下直接测这条规则。
fn clamp_bp(v: i64) -> i64 {
    v.clamp(0, SAMPLE_BP_FULL)
}

fn sample_bp(name: &str, default: i64) -> i64 {
    clamp_bp(env_i64(name, default))
}

fn public_sample_bp() -> i64 {
    sample_bp(ENV_PUBLIC_SAMPLE_BP, DEFAULT_PUBLIC_SAMPLE_BP)
}

fn private_sample_bp() -> i64 {
    sample_bp(ENV_PRIVATE_SAMPLE_BP, DEFAULT_PRIVATE_SAMPLE_BP)
}

fn timeout_ms() -> u64 {
    env_i64(ENV_TIMEOUT_MS, DEFAULT_TIMEOUT_MS).clamp(1, 600_000) as u64
}

/// 尝试次数下限为 1：配成 0 或负数不得变成「一次都不查」。
fn max_attempts() -> i64 {
    env_i64(ENV_MAX_ATTEMPTS, DEFAULT_MAX_ATTEMPTS).max(1)
}

fn backoff_ms(attempt: i64) -> i64 {
    let base = env_i64(ENV_BACKOFF_MS, DEFAULT_BACKOFF_MS).max(0);
    let shift = (attempt.max(1) - 1).min(BACKOFF_CAP_SHIFT as i64) as u32;
    base.saturating_mul(1i64 << shift)
}

fn max_events() -> i64 {
    env_i64(ENV_MAX_EVENTS, DEFAULT_MAX_EVENTS).max(1)
}

/// 本模块是否已由运营开启。ctx 取 **user 无 / world 有**：复核对象天然挂在一个世界上，
/// 按世界灰度是最自然的开闸单位（先开一个世界跑真实 provider，再放大）。
///
/// 🔴 fail-closed 由 `flags::is_enabled` 自带：查库失败 / 记录损坏 → 返回声明默认值（关），
/// 且**不再回落 env**。
pub(crate) async fn enabled(db: &AnyPool, world_id: &str) -> bool {
    crate::flags::is_enabled(db, ENV_SEMANTIC_RECHECK, crate::flags::FlagCtx::world(world_id)).await
}

/// 全局解析（运营面读数用：告诉运营「大盘上这条链开着没有」）。
async fn enabled_global(db: &AnyPool) -> bool {
    crate::flags::is_enabled(db, ENV_SEMANTIC_RECHECK, crate::flags::FlagCtx::global()).await
}

// ═══════════════════════════════════════════════════════════════════════════
// 任务与 worker
// ═══════════════════════════════════════════════════════════════════════════

/// 复核任务载荷。**只带坐标，不带内容**——事件清单每次从 `world_events` 现查，
/// 于是任务天然幂等可重放，且已被第 2 层拦下的事件不会被重复送审（它们已不是 `approved`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecheckJob {
    pub(crate) world_id: String,
    pub(crate) tick_no: i64,
    /// 第几次尝试（从 1 起）。
    #[serde(default)]
    pub(crate) attempt: i64,
    /// 重试时**只重查这些事件 id**（上一次报错的那几条）。首次为空 = 查整拍。
    /// 不重查已经拿到裁决的那些，是为了不让一条抖动的事件把整拍的成本乘以尝试次数。
    #[serde(default)]
    pub(crate) retry_ids: Vec<String>,
}

/// 🔴 **tick 提交后**的唯一接线点（由 `runtime::commit_tick` 在 `tx.commit()` **之后**调用）。
///
/// 本函数只做两件事：解析开关、往进程内队列丢一个坐标。**没有网络 IO、没有事务、不返回错误**——
/// 审核链的排队问题绝不能反过来把一拍已经提交的世界事实弄失败。
pub(crate) async fn enqueue_after_commit(state: &AppState, world_id: &str, tick_no: i64) {
    if !enabled(&state.db, world_id).await {
        return;
    }
    let job = RecheckJob {
        world_id: world_id.to_string(),
        tick_no,
        attempt: 1,
        retry_ids: Vec::new(),
    };
    crate::queue::push_json(&*state.queue, TOPIC, &job, now_ms()).await;
}

/// 启动复核 worker 池（`main` 调用）。并发度 `MUSE_SAFETY_L3_WORKERS`（默认 1）。
///
/// 与 `runtime::spawn_workers` **分池**：审核的排队时间与世界推进的排队时间不得互相绑架。
pub fn spawn_workers(state: AppState) {
    let workers = env_i64(ENV_WORKERS, DEFAULT_WORKERS).clamp(1, 64) as usize;
    for _ in 0..workers {
        tokio::spawn(worker_loop(state.clone()));
    }
    tracing::info!(workers, "§15 第 3 层语义复核 worker 已启动（默认关闭，开关 MUSE_SAFETY_SEMANTIC_RECHECK）");
}

async fn worker_loop(state: AppState) {
    loop {
        let Some(job) = crate::queue::pop_json::<RecheckJob>(&*state.queue, TOPIC).await else {
            continue;
        };
        // 单条任务失败不拖垮 worker：审核是旁路，最坏结果是这一拍没复核（已记 warn）。
        if let Err(e) = run_recheck(&state, &job).await {
            tracing::warn!(
                world_id = %job.world_id, tick_no = job.tick_no, attempt = job.attempt, error = %e,
                "§15 第 3 层复核任务失败"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 复核主流程
// ═══════════════════════════════════════════════════════════════════════════

/// 一次复核尝试的结果（落 `safety_recheck_runs` + 供用例断言）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RunReport {
    pub(crate) public_candidates: i64,
    pub(crate) public_checked: i64,
    pub(crate) private_candidates: i64,
    pub(crate) private_checked: i64,
    pub(crate) chars_checked: i64,
    pub(crate) tightened: i64,
    pub(crate) provider_errors: i64,
    pub(crate) failed_closed: i64,
    pub(crate) intercepted_before_broadcast: i64,
    pub(crate) outcome: &'static str,
}

/// 一条待复核事件（从 `world_events` 现查；**只取送审需要的列**）。
struct Candidate {
    id: String,
    domain_event_id: String,
    is_public: bool,
    text: String,
}

pub(crate) const OUTCOME_DONE: &str = "done";
pub(crate) const OUTCOME_RETRY: &str = "retry_scheduled";
pub(crate) const OUTCOME_FAILED_CLOSED: &str = "failed_closed";
pub(crate) const OUTCOME_SKIPPED: &str = "skipped";

/// 跑一次复核尝试。**事务外**（本模块从不 `begin()`）。
pub(crate) async fn run_recheck(state: &AppState, job: &RecheckJob) -> Result<RunReport, ApiError> {
    let started = now_ms();
    let attempt = job.attempt.max(1);

    // 每次尝试都重解析开关：运营在重试期间关掉第 3 层，在飞的任务立刻停手（不再收紧任何东西）。
    if !enabled(&state.db, &job.world_id).await {
        return Ok(RunReport { outcome: OUTCOME_SKIPPED, ..Default::default() });
    }

    let candidates = load_candidates(&state.db, &job.world_id, job.tick_no, &job.retry_ids).await?;
    if candidates.is_empty() {
        return Ok(RunReport { outcome: OUTCOME_SKIPPED, ..Default::default() });
    }

    let (pub_bp, priv_bp) = (public_sample_bp(), private_sample_bp());
    let stub = state.moderation.is_dev_stub();
    // 直播播出水位线：整拍算一次即可（口径对齐 `live_withholds.preemptive`）。
    let intercepted_before_broadcast =
        matches!(live_published_high_tick(&state.db, &job.world_id).await, Some(hi) if job.tick_no > hi);

    let mut r = RunReport { outcome: OUTCOME_DONE, ..Default::default() };
    let mut errored: Vec<String> = Vec::new();

    for c in &candidates {
        if c.is_public {
            r.public_candidates += 1;
        } else {
            r.private_candidates += 1;
        }
        let bp = if c.is_public { pub_bp } else { priv_bp };
        if !sampled(&job.world_id, job.tick_no, &c.domain_event_id, bp) {
            continue;
        }
        if c.is_public {
            r.public_checked += 1;
        } else {
            r.private_checked += 1;
        }
        r.chars_checked += c.text.chars().count() as i64;

        match check_with_timeout(state, &c.text).await {
            Ok(ModerationVerdict::Approved) => {}
            Ok(verdict) => {
                // 🔴 收紧：只动 moderation 一列，且 WHERE 钉着 approved（单向棘轮）。
                if tighten(&state.db, &c.id, verdict).await? {
                    r.tightened += 1;
                    if intercepted_before_broadcast {
                        r.intercepted_before_broadcast += 1;
                    }
                    record_hit(state, job, c, verdict, None, stub, intercepted_before_broadcast)
                        .await?;
                    // 入队策略复用第 2 层那一个旋钮（`MUSE_SAFETY_RUNTIME_AUDIT`），
                    // 不新造第二个：Rejected 视同高危、Pending 视同低危。
                    let sev = if verdict == ModerationVerdict::Rejected {
                        Severity::High
                    } else {
                        Severity::Low
                    };
                    if super::runtime_audit_admits(sev) {
                        super::insert_runtime_audit(
                            &state.db,
                            &job.world_id,
                            &c.domain_event_id,
                            verdict,
                            &machine_hits(verdict, None, stub),
                        )
                        .await?;
                    }
                }
            }
            Err(err) => {
                r.provider_errors += 1;
                errored.push(c.id.clone());
                tracing::warn!(
                    world_id = %job.world_id, tick_no = job.tick_no, attempt,
                    event_id = %c.id, error = %err, "§15 第 3 层 provider 调用失败"
                );
            }
        }
    }

    // ── 失败处置：先重试，到顶再 fail-closed（方向不参数化，见模块头） ──────────
    if !errored.is_empty() {
        if attempt < max_attempts() {
            let next = RecheckJob {
                world_id: job.world_id.clone(),
                tick_no: job.tick_no,
                attempt: attempt + 1,
                // 只重查报错的那几条，避免一条抖动把整拍成本乘以尝试次数。
                // 排序保证重试批次的顺序确定（不依赖上游 map/查询的偶然顺序）。
                retry_ids: {
                    let mut v = errored.clone();
                    v.sort();
                    v
                },
            };
            crate::queue::push_json(&*state.queue, TOPIC, &next, now_ms() + backoff_ms(attempt))
                .await;
            r.outcome = OUTCOME_RETRY;
        } else {
            r.outcome = OUTCOME_FAILED_CLOSED;
            for c in candidates.iter().filter(|c| errored.contains(&c.id)) {
                // 🔴 fail-closed：给不出裁决 ≠ 放行。收紧为 pending（待人审），不是 rejected
                //    （rejected 是终判；「机器没能判定」不该被记成终判）。
                if tighten(&state.db, &c.id, ModerationVerdict::Pending).await? {
                    r.tightened += 1;
                    r.failed_closed += 1;
                    if intercepted_before_broadcast {
                        r.intercepted_before_broadcast += 1;
                    }
                    record_hit(
                        state,
                        job,
                        c,
                        ModerationVerdict::Pending,
                        Some(REASON_PROVIDER_UNAVAILABLE),
                        stub,
                        intercepted_before_broadcast,
                    )
                    .await?;
                    // 🔴 **无条件入队**（不看 `runtime_audit_admits`）：入队理由不是「机器判定可疑」，
                    // 而是「机器没能判定」——那正是必须有人来看的情形，同 `queue_operator_recheck`
                    // 「无论裁决如何都入队」的推理。
                    super::insert_runtime_audit(
                        &state.db,
                        &job.world_id,
                        &c.domain_event_id,
                        ModerationVerdict::Pending,
                        &machine_hits(
                            ModerationVerdict::Pending,
                            Some(REASON_PROVIDER_UNAVAILABLE),
                            stub,
                        ),
                    )
                    .await?;
                }
            }
        }
    }

    persist_run(&state.db, job, attempt, &r, pub_bp, priv_bp, stub, now_ms() - started).await?;
    if r.tightened > 0 {
        tracing::warn!(
            world_id = %job.world_id, tick_no = job.tick_no, attempt,
            tightened = r.tightened, failed_closed = r.failed_closed, provider_stub = stub,
            "§15 第 3 层语义复核收紧了运行时投影（未过审 → 不外发；正文未改写）"
        );
    }
    Ok(r)
}

/// fail-closed 留痕里的原因标记：区分「机器判定可疑」与「机器没能判定」。
const REASON_PROVIDER_UNAVAILABLE: &str = "provider_unavailable";

async fn check_with_timeout(state: &AppState, text: &str) -> Result<ModerationVerdict, String> {
    match tokio::time::timeout(
        Duration::from_millis(timeout_ms()),
        state.moderation.check_text(text),
    )
    .await
    {
        Ok(inner) => inner,
        Err(_) => Err(format!("check_text 超时（>{}ms）", timeout_ms())),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 候选装载（**只读** `world_events`）
// ═══════════════════════════════════════════════════════════════════════════

/// 取本拍仍为 `approved` 的投影事件。
///
/// `moderation = 'approved'` 这个条件同时干三件事：跳过第 2 层已拦下的、跳过人审已改判的、
/// 跳过本任务上一次尝试已经收紧的——**幂等由数据本身保证**，不需要额外状态。
///
/// `ORDER BY sequence ASC, id ASC` 全序（PG 对并列行不保证顺序；`sequence` 虽已由发号器保证
/// 世界内唯一，次级键仍写上，口径与 `events` 读取面一致）。
async fn load_candidates(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    retry_ids: &[String],
) -> Result<Vec<Candidate>, ApiError> {
    // 占位符按 **SQL 文本顺序**发号（`Placeholders` 保证严格升序不复用），bind 顺序与之一一对应。
    let mut ph = Placeholders::new();
    let p_world = ph.take();
    let p_tick = ph.take();
    let mut sql = format!(
        "SELECT id, domain_event_id, visibility, public_projection_json, private_projections_json, \
         arbiter_note FROM world_events WHERE world_id = {p_world} AND tick_no = {p_tick} \
         AND moderation = 'approved'"
    );
    if !retry_ids.is_empty() {
        let list = ph.list(retry_ids.len());
        sql.push_str(&format!(" AND id IN ({list})"));
    }
    let p_limit = ph.take();
    sql.push_str(&format!(" ORDER BY sequence ASC, id ASC LIMIT {p_limit}"));

    let mut q = sqlx::query(&sql).bind(world_id).bind(tick_no);
    for id in retry_ids {
        q = q.bind(id);
    }
    let rows = q.bind(max_events()).fetch_all(db).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let visibility: String = row.try_get("visibility")?;
        let is_public = visibility == "public";
        let public_json: Option<String> = row.try_get("public_projection_json")?;
        let private_json: Option<String> = row.try_get("private_projections_json")?;
        let arbiter_note: Option<String> = row.try_get("arbiter_note")?;
        let text = compose_text(public_json.as_deref(), private_json.as_deref(), arbiter_note.as_deref());
        if text.trim().is_empty() {
            continue;
        }
        out.push(Candidate {
            id: row.try_get("id")?,
            domain_event_id: row.try_get("domain_event_id")?,
            is_public,
            text,
        });
    }
    Ok(out)
}

/// 拼出送审文本：公共投影 summary ∪ 私有投影各 summary ∪ arbiter_note。
///
/// 与第 2 层 `moderate_runtime_projection` 覆盖的字段**同一套**（summary + arbiter_note）——
/// 两层看的不是同一段文本，就会出现「词库查了但语义没查」的字段级缺口。
fn compose_text(public_json: Option<&str>, private_json: Option<&str>, note: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = public_json.and_then(|s| serde_json::from_str::<Value>(s).ok()) {
        if let Some(s) = v.get("summary").and_then(Value::as_str) {
            parts.push(s.to_string());
        }
    }
    if let Some(v) = private_json.and_then(|s| serde_json::from_str::<Value>(s).ok()) {
        if let Some(arr) = v.as_array() {
            for item in arr {
                if let Some(s) = item.get("summary").and_then(Value::as_str) {
                    parts.push(s.to_string());
                }
            }
        }
    }
    if let Some(n) = note.filter(|s| !s.trim().is_empty()) {
        parts.push(n.to_string());
    }
    parts.join("\n")
}

// ═══════════════════════════════════════════════════════════════════════════
// 抽样（确定性；禁三样）
// ═══════════════════════════════════════════════════════════════════════════

/// 这条事件这次是否送审。
///
/// 种子 = `fnv1a_64(world_id ‖ tick_no ‖ domain_event_id) ^ 0x5C`，过 SplitMix64。
/// **不掷系统随机数、不用浮点、不依赖任何 map 迭代序**——重试拿到的是同一批样本，
/// 复盘时也算得回来「那条为什么没被查」。
pub(crate) fn sampled(world_id: &str, tick_no: i64, domain_event_id: &str, bp: i64) -> bool {
    if bp >= SAMPLE_BP_FULL {
        return true;
    }
    if bp <= 0 {
        return false;
    }
    let seed = crate::assembly::fnv1a_64(
        format!("{world_id}\u{1}{tick_no}\u{1}{domain_event_id}").as_bytes(),
    ) ^ DOMAIN_L3_SAMPLE;
    let mut rng = crate::assembly::Rng(seed);
    (rng.below(SAMPLE_BP_FULL as usize) as i64) < bp
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 收紧（本模块对 `world_events` 的唯一写入）
// ═══════════════════════════════════════════════════════════════════════════

/// 把一条事件的审核态从 `approved` **收紧**为非 approved。返回是否真的改到了行。
///
/// - `SET` 只有 `moderation` 一列 → 正文逐字节不变（§0.3）；
/// - `WHERE ... AND moderation = 'approved'` → 单向棘轮：既不放宽，也不覆盖更严的既有裁决；
/// - 调用方保证 `verdict != Approved`（用例 `red_line_tightening_never_writes_approved` 扫源码兜底）。
async fn tighten(db: &AnyPool, event_id: &str, verdict: ModerationVerdict) -> Result<bool, ApiError> {
    debug_assert_ne!(verdict, ModerationVerdict::Approved, "第 3 层只收紧，绝不写 approved");
    let res = sqlx::query("UPDATE world_events SET moderation = $1 WHERE id = $2 AND moderation = 'approved'")
        .bind(verdict_str(verdict))
        .bind(event_id)
        .execute(db)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// 该世界所有直播场次里最靠前的播出水位线（`-1` = 有场次但一拍都没播；`None` = 读不出来）。
///
/// 只读一次、只读一列（本模块对 `live_sessions` 只有这一条 SELECT，零写入）。
/// 取 `MAX` 是保守方向：只要**任何**一场已经播过这一拍，就不算 preemptive。
///
/// 🔴 读失败返回 `None`，调用方据此判为**不是** preemptive。方向是刻意的：算不出来时
/// 宁可少报「缓冲救下了一条」，也不要凭一次查询失败去**夸大**这层防护的效果——
/// 这个数存在的意义正是告诉运营「延迟拍数够不够」，把它报高等于把旋钮往错误方向推。
async fn live_published_high_tick(db: &AnyPool, world_id: &str) -> Option<i64> {
    sqlx::query(
        "SELECT CAST(COALESCE(MAX(published_high_tick), -1) AS BIGINT) AS hi FROM live_sessions WHERE world_id = $1",
    )
    .bind(world_id)
    .fetch_one(db)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("hi").ok())
}

// ═══════════════════════════════════════════════════════════════════════════
// 留痕与台账
// ═══════════════════════════════════════════════════════════════════════════

/// 人审队列的 `machine_hits` 载荷。第 3 层没有词表命中点可展示，
/// 于是如实给出「裁决来自哪一层、provider 说了什么、**这是不是桩**」。
fn machine_hits(verdict: ModerationVerdict, reason: Option<&str>, stub: bool) -> String {
    json!({
        "layer": LAYER,
        "verdict": verdict_str(verdict),
        "reason": reason.unwrap_or("semantic_classification"),
        // 🔴 人审工作台上必须看得见「这条是桩判的」。
        "providerStub": stub,
    })
    .to_string()
}

/// 风控留痕。🔴 走 `safety::record_risk`（既有入口），**本模块不自己 INSERT `risk_events`**。
#[allow(clippy::too_many_arguments)]
async fn record_hit(
    state: &AppState,
    job: &RecheckJob,
    c: &Candidate,
    verdict: ModerationVerdict,
    reason: Option<&str>,
    stub: bool,
    intercepted_before_broadcast: bool,
) -> Result<(), ApiError> {
    record_risk(
        &state.db,
        None,
        Some(&job.world_id),
        RISK_KIND,
        json!({
            "layer": LAYER,
            "subjectKind": "world_event",
            "eventId": c.id,
            "domainEventId": c.domain_event_id,
            "visibility": if c.is_public { "public" } else { "private" },
            "tickNo": job.tick_no,
            "attempt": job.attempt.max(1),
            "verdict": verdict_str(verdict),
            "reason": reason.unwrap_or("semantic_classification"),
            // 🔴 只收紧、不改写：正文一个字节没动，这里也**不复制正文**（留痕不是内容副本）。
            "bodyRewritten": false,
            "interceptedBeforeBroadcast": intercepted_before_broadcast,
            // 🔴 「当前是桩」随每一条留痕走。
            "providerStub": stub,
        }),
    )
    .await
}

/// 落一行运行台账（一次尝试一行）。重复投递靠唯一索引 + `ON CONFLICT DO NOTHING` 幂等。
#[allow(clippy::too_many_arguments)]
async fn persist_run(
    db: &AnyPool,
    job: &RecheckJob,
    attempt: i64,
    r: &RunReport,
    pub_bp: i64,
    priv_bp: i64,
    stub: bool,
    latency_ms: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO safety_recheck_runs (id, world_id, tick_no, attempt, public_candidates, \
         public_checked, private_candidates, private_checked, public_sample_bp, private_sample_bp, \
         chars_checked, tightened, provider_errors, failed_closed, intercepted_before_broadcast, \
         latency_ms, provider_stub, outcome, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
         ON CONFLICT(world_id, tick_no, attempt) DO NOTHING",
    )
    .bind(new_id("srr"))
    .bind(&job.world_id)
    .bind(job.tick_no)
    .bind(attempt)
    .bind(r.public_candidates)
    .bind(r.public_checked)
    .bind(r.private_candidates)
    .bind(r.private_checked)
    .bind(pub_bp)
    .bind(priv_bp)
    .bind(r.chars_checked)
    .bind(r.tightened)
    .bind(r.provider_errors)
    .bind(r.failed_closed)
    .bind(r.intercepted_before_broadcast)
    .bind(latency_ms)
    // 布尔一律 INTEGER 0/1（双库可移植），绑定用 i64 —— 与仓库既有写法一致
    // （PG 在 INSERT 目标上对 int8→int4 做赋值转换，SQLite 无类型之分）。
    .bind(if stub { 1_i64 } else { 0 })
    .bind(r.outcome)
    .bind(now_ms())
    .execute(db)
    .await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 运营面：GET /api/admin/safety/recheck
// ═══════════════════════════════════════════════════════════════════════════

/// 挂进 `admin_api::router()`（不改 `app.rs`：领域模块自带路由、由 admin 聚合）。
pub(crate) fn admin_router() -> Router<AppState> {
    Router::new().route("/admin/safety/recheck", get(recheck_overview))
}

/// 细粒度角色守卫。语义与 `admin_api::require_role` 逐字一致（`admin` 放行一切）。
/// 本端点**只读**，取 `operator` 档（成本读数是运营动作，不是审核动作）。
fn require_operator(admin: &AdminUser) -> Result<(), ApiError> {
    let role = admin.0.role.as_str();
    if role == "admin" || role == "operator" {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowQuery {
    /// 起（含），BIGINT 毫秒。缺省 = `until - 30 天`。
    #[serde(default)]
    since: Option<i64>,
    /// 止（不含），BIGINT 毫秒。缺省 = 现在。
    #[serde(default)]
    until: Option<i64>,
}

/// 🔴 这批数**测得了什么、测不了什么**。
///
/// 它是响应 JSON 里的一等字段而不是文档里的一句话，因为**数会被复制走，文档不会**
/// （范式与理由同 `slo::quality::QualitySource::caveats`）。
fn honesty(stub: bool) -> Vec<&'static str> {
    if stub {
        vec![
            "🔴 数据源 = Dev 桩（providers::DevModeration）：check_text 只做一张小关键词表的匹配，\
             不做任何语义分类。第 3 层管线已接通，但它当前**拦不住任何东西**。",
            "🔴 因此本页数字**不得**表述为「五层漏斗已完整」「内容安全已就绪」「语义审核已上线」。\
             本批次交付的是管线（Implemented），不是防线（未 Validated）。",
            "接真实服务商 = 换一个 ModerationProvider 实现，并把 is_dev_stub() 覆写为 false；\
             那一刻 providerStub 自动变 false，届时这些数字才开始有内容安全含义。",
            "tightened / failedClosed 在桩下几乎恒为 0：桩不会说违规的话，也几乎不会报错。\
             该通道在当前部署里**没有被真实检验**，只是被计算了。",
        ]
    } else {
        vec![
            "数据源 = 真实 ModerationProvider；此时 tightened / failedClosed 才携带内容安全含义。",
            "⚠️ 「已接真实 provider」仍不等于「已验证」：召回率/误伤率需要独立评测，本页不度量它们。",
        ]
    }
}

/// 成本口径。
///
/// ## 🔴 `ratioAvailable` 现在**可以**翻成 true 了，但只在两侧单价都被显式配置时
///
/// VALIDATION §2 T5 有一条门槛「内容审核成本 ≤ 生成成本的 5%」。此前这里恒为 `false`，
/// 理由是「`check_text` 只回裁决、不回 token 也不回费用」。那句话对**响应体**依然成立——
/// 阿里云 / 腾讯云 / 百度的文本审核响应里根本没有计费字段，它们按调用次数离线结算。
/// 所以补法不是去响应里抠一个不存在的字段，而是把缺的那一半变成显式配置：
///
/// | 侧 | 用量（已有） | 单价（新增，须显式配置） |
/// |---|---|---|
/// | 分子 · 审核 | `moderationCallsInWindow`（含重试——重试是真实开销） | `ModerationProvider::call_price_cents_per_1k`（`MUSE_MODERATION_HTTP_PRICE_CENTS_PER_1K_CALLS`） |
/// | 分母 · 生成 | `world_ticks.cost_tokens` | `MUSE_TOKEN_CNY_CENTS_PER_1K` |
///
/// **任一半缺失 → 仍是 `false`**，且 `why` 明说缺的是哪一半：
/// - Dev 桩的 `call_price_cents_per_1k()` 默认 `None`（桩的调用成本恒为 0，算出来的比值毫无意义）；
/// - 生成侧单价**不回落代码内默认估算**——见 [`explicit_token_price_cents_per_1k`]。
///
/// 比值用**万分比整数**给（`ratioBp`），不给浮点：金额与门槛判定必须逐位可复现。
fn cost_block(state: &AppState, checks: i64, chars: i64, gen_tokens: i64) -> Value {
    let mod_unit = state.moderation.call_price_cents_per_1k();
    let gen_unit = explicit_token_price_cents_per_1k();

    let mut v = json!({
        "generationTokensInWindow": gen_tokens,
        "moderationCallsInWindow": checks,
        "moderationCharsInWindow": chars,
        "moderationUnitPriceCentsPer1kCalls": mod_unit,
        "generationUnitPriceCentsPer1kTokens": gen_unit,
        "thresholdBp": COST_THRESHOLD_BP,
        "unit": "金额一律「分」；比值一律万分比整数（500 = 5%），禁浮点。",
        "levers": [
            "MUSE_SAFETY_L3_PUBLIC_SAMPLE_BP / MUSE_SAFETY_L3_PRIVATE_SAMPLE_BP（降档抽样）",
            "MUSE_SAFETY_L3_MAX_EVENTS_PER_TICK（单拍送审条数上限）",
            "MUSE_SAFETY_L3_MAX_ATTEMPTS（重试预算 —— 重试是真实开销，不是免费的）",
            "MUSE_LIVE_DELAY_TICKS（T5 预案「审核成本失控 → 直播延迟拍数上调」的那一个旋钮）",
        ],
    });
    let o = v.as_object_mut().expect("json! 造的是对象");

    let missing = match (mod_unit, gen_unit) {
        (None, None) => Some(format!(
            "两侧单价都缺：审核侧要么用的是 Dev 桩（调用成本恒为 0），要么真实 provider 没配\
             {}；生成侧 {ENV_TOKEN_PRICE} 也未显式配置。",
            crate::providers::http_moderation::ENV_PRICE_CENTS_PER_1K_CALLS
        )),
        (None, Some(_)) => Some(format!(
            "缺**审核侧**单价：ModerationProvider::call_price_cents_per_1k() 返回 None。\
             厂商响应里没有计费字段（按调用次数离线结算），故请把合同单价配进 {}。",
            crate::providers::http_moderation::ENV_PRICE_CENTS_PER_1K_CALLS
        )),
        (Some(_), None) => Some(format!(
            "缺**生成侧**单价：{ENV_TOKEN_PRICE} 未显式配置。🔴 本端点刻意不回落代码内的默认\
             估算——拿估算算 T5 门槛得到的是「估算的估算」，而它在看板上和真值长得一样。"
        )),
        (Some(_), Some(_)) => None,
    };

    match (missing, mod_unit, gen_unit) {
        (Some(why), ..) => {
            o.insert("ratioAvailable".into(), json!(false));
            o.insert(
                "why".into(),
                json!(format!(
                    "VALIDATION §2 T5 门槛「内容审核成本 ≤ 生成成本的 5%」需要**两侧单价**。{why} \
                     用量侧两边都已可查（调用量含重试、送审字符数、生成 token）。"
                )),
            );
        }
        (None, Some(mu), Some(gu)) => {
            let mod_cents = cost_cents(checks, mu);
            let gen_cents = cost_cents(gen_tokens, gu);
            o.insert("moderationCostCents".into(), json!(mod_cents));
            o.insert("generationCostCents".into(), json!(gen_cents));
            if gen_cents > 0 {
                let ratio_bp = mod_cents.saturating_mul(BP_BASE) / gen_cents;
                o.insert("ratioAvailable".into(), json!(true));
                o.insert("ratioBp".into(), json!(ratio_bp));
                o.insert("withinThreshold".into(), json!(ratio_bp <= COST_THRESHOLD_BP));
                o.insert(
                    "why".into(),
                    json!("两侧单价均已显式配置，比值可算。⚠️ 单价是**运营填的合同价**，\
                          不是厂商响应回报的实测计费（文本审核响应里没有计费字段），\
                          故它的准确度等于那两个配置值的准确度。"),
                );
            } else {
                o.insert("ratioAvailable".into(), json!(false));
                o.insert(
                    "why".into(),
                    json!("两侧单价都有，但窗口内生成成本为 0（分母为 0），比值无定义——\
                          不是缺配置，是这段窗口里没有世界推进过。"),
                );
            }
        }
        _ => unreachable!("missing 为 None 时两侧单价必然都是 Some"),
    }
    v
}

async fn recheck_overview(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, ApiError> {
    require_operator(&admin)?;

    // 时间窗：BIGINT 毫秒比较（双库可移植——不用 strftime / date_trunc / NOW()）。
    let until = q.until.unwrap_or_else(now_ms);
    // 默认回看 30 天。窗口是参数，不是字面量（§0.2）。
    let since = q.since.unwrap_or(until - 30 * 24 * 60 * 60 * 1000);

    // PG 下 SUM(bigint) 返回 numeric，一律 CAST(... AS BIGINT)。
    let row = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS runs, \
         CAST(COALESCE(SUM(public_candidates), 0) AS BIGINT) AS pub_cand, \
         CAST(COALESCE(SUM(public_checked), 0) AS BIGINT) AS pub_chk, \
         CAST(COALESCE(SUM(private_candidates), 0) AS BIGINT) AS priv_cand, \
         CAST(COALESCE(SUM(private_checked), 0) AS BIGINT) AS priv_chk, \
         CAST(COALESCE(SUM(chars_checked), 0) AS BIGINT) AS chars, \
         CAST(COALESCE(SUM(tightened), 0) AS BIGINT) AS tightened, \
         CAST(COALESCE(SUM(provider_errors), 0) AS BIGINT) AS errors, \
         CAST(COALESCE(SUM(failed_closed), 0) AS BIGINT) AS failed_closed, \
         CAST(COALESCE(SUM(intercepted_before_broadcast), 0) AS BIGINT) AS preemptive, \
         CAST(COALESCE(SUM(provider_stub), 0) AS BIGINT) AS stub_runs, \
         CAST(COALESCE(MAX(latency_ms), 0) AS BIGINT) AS max_latency \
         FROM safety_recheck_runs WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(since)
    .bind(until)
    .fetch_one(&state.db)
    .await?;
    let n = |k: &str| row.try_get::<i64, _>(k).unwrap_or(0);

    // 分母侧（生成成本）：世界线拍的实测 token。分子侧还差 provider 的计价口径，见下 `cost`。
    let gen_tokens: i64 = sqlx::query(
        "SELECT CAST(COALESCE(SUM(cost_tokens), 0) AS BIGINT) AS tokens FROM world_ticks \
         WHERE finished_at >= $1 AND finished_at < $2",
    )
    .bind(since)
    .bind(until)
    .fetch_one(&state.db)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("tokens").ok())
    .unwrap_or(0);

    let stub = state.moderation.is_dev_stub();
    let runs = n("runs");
    let checks = n("pub_chk") + n("priv_chk");
    let cost = cost_block(&state, checks, n("chars"), gen_tokens);

    Ok(Json(json!({
        "layer": LAYER,
        "since": since,
        "until": until,
        // ── 🔴 「当前是桩」随数据一起走 ─────────────────────────────────────
        "providerStub": stub,
        "source": if stub { "dev-stub" } else { "production" },
        "honesty": honesty(stub),
        "stubRuns": n("stub_runs"),
        "productionRuns": (runs - n("stub_runs")).max(0),
        // ── 开关（未验证功能默认关闭） ──────────────────────────────────────
        "flag": ENV_SEMANTIC_RECHECK,
        "enabledGlobally": enabled_global(&state.db).await,
        "note": "开关按世界灰度解析（user > world > global > env > 默认关）；\
                 enabledGlobally 只是大盘值，某个世界单独开着时它仍可能是 false。",
        // ── 送审口径 ───────────────────────────────────────────────────────
        "runs": runs,
        "publicCandidates": n("pub_cand"),
        "publicChecked": n("pub_chk"),
        "privateCandidates": n("priv_cand"),
        "privateChecked": n("priv_chk"),
        "checks": checks,
        "charsChecked": n("chars"),
        "sampling": {
            "publicBp": public_sample_bp(),
            "privateBp": private_sample_bp(),
            "envs": [ENV_PUBLIC_SAMPLE_BP, ENV_PRIVATE_SAMPLE_BP, ENV_MAX_EVENTS],
            "unit": "万分比整数（10000 = 全量）",
            "deterministic": true,
            "note": "公开投影默认全量、私有投影抽样（总规格 §15 第 3 层口径）。抽样是确定性的\
                     （fnv1a_64 种子 + SplitMix64，不掷系统随机数），故重试拿到的是同一批样本。",
        },
        // ── 处置结果 ───────────────────────────────────────────────────────
        "tightened": n("tightened"),
        "providerErrors": n("errors"),
        "failedClosed": n("failed_closed"),
        "failClosedPolicy": {
            "direction": "fail-closed",
            "parameterized": false,
            "why": "审核链自身的故障绝不转化为放行——否则打掉 provider 就成了绕过第 3 层的手段。\
                    方向与 MUSE_SAFETY_LEXICON 的 fail-safe（默认「继续过滤」）自洽。\
                    可配的只有阈值：重试次数 / 退避 / 超时。",
            "maxAttempts": max_attempts(),
            "timeoutMs": timeout_ms(),
            "envs": [ENV_MAX_ATTEMPTS, ENV_TIMEOUT_MS, ENV_BACKOFF_MS],
        },
        "maxLatencyMs": n("max_latency"),
        // ── 与 §15 第 4 层（直播场延迟缓冲）的配合 ───────────────────────────
        "interceptedBeforeBroadcast": n("preemptive"),
        "broadcastWindow": {
            "meaning": "收紧发生在该拍越过任何一场直播的播出水位线之前 —— 直播观众从未看见。\
                        口径对齐 live_withholds.preemptive，是「MUSE_LIVE_DELAY_TICKS 配得够不够」的度量。",
            "🔴 caveat": "它**不等于**「没人看见」：延迟缓冲只作用于世界外，\
                          世界成员的 /worlds/{id}/events 不延迟，对成员而言第 3 层恒为事后收紧。\
                          已经返回给客户端的字节收不回，平台也不会为此另发撤回通知（越描越黑）。",
        },
        // ── 成本 ───────────────────────────────────────────────────────────
        "cost": cost,
        "dashboardIntegration": {
            "mainDashboardIncludesL3": false,
            "where": "GET /api/admin/safety/recheck（本端点）。主看板 /admin/metrics/overview 尚未并入本项。",
            "why": "并进主看板前应当先有真实 provider 的计价口径，否则会在成本看板上摆一个恒为 0 的\
                    「审核成本」列，那比没有更误导。",
        },
    })))
}
