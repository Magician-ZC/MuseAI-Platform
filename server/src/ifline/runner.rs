//! **if 线推进（跑拍）**——0039 立的项，在这里真正跑起来（迁移 `0041`）。
//!
//! 0039 交付的是「立项与开局」：校验 → 烧副本卡 → 逐字节冻结分叉态 → 注册独立实例 → 可读可审，
//! `status` 恒为 `sealed`。玩家花掉一张副本卡拿到的是一个**冻结的开局**，剧情推不动。
//! 本文件补上 `sealed → running → ended`。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 最高红线：if 线的终局绝不进入任何一条结算管线
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `progression::settle_idle_world_ending_tx`（发历练）· `subplot::settle_subplot_card_tx`（铸卡）·
//! `arena_rewards`（荣誉）—— **一条都不许进**。
//!
//! 理由不是洁癖，是定价结构：**历练是准入门槛与卡位解锁的钥匙**。一旦 if 线的终局能发历练，
//! 「花钱开 if 线」立刻等于「花钱买数值」，踩穿总规格 §0.1「付费只买体验容量，永不买结果」
//! 与平台红线「不卖胜负与数值平权」。
//!
//! ### 为什么它**不可能**回到结算管线（结构性，不是纪律性）
//!
//! 那三条结算全部挂在**同一条自动链路**上：
//!
//! ```text
//!   runtime::commit_tick                     ← 世界线状态 CAS 成功
//!     └─ end_world_tx / finalize_ending_tx   ← 同事务内立即评估终局
//!          ├─ progression::settle_idle_world_ending_tx   发历练
//!          ├─ subplot::settle_subplot_card_tx            铸副本卡
//!          └─ arena_rewards                              发荣誉
//! ```
//!
//! 这条链路的**入口只有两处**：一行 `worlds` 和若干行 `world_members`。于是 if 线的隔离是三层的：
//!
//! | 层 | 保证 | 谁在守 |
//! |---|---|---|
//! | 数据 | if 线不是 `worlds` 行、不是 `world_members` 行；它的每一拍落 `ifline_beats`，**不是 `world_ticks`** | 迁移 0039/0041；`red_line_ifline_is_not_a_world_row` |
//! | 代码 | 本模块**从不调用 `commit_tick`**。推进走的是本文件的 `commit_beat`——另一套表、另一套事务，与 `runtime::commit_tick` 零交叉 | 源码级 `red_line_never_mints_assets`（扫描面含本文件） |
//! | 结果 | 跑到终局后历练总和 / 副本卡总行数 / 背包 / 荣誉 / 贡献账本**全部零变化** | 运行时 `red_line_ifline_ending_grants_nothing` |
//!
//! 🔴 **接线时最容易走错的一步**，是为了复用 `process_tick_inner` 那 500 行而把 if 线塞回
//! `worlds` / `world_ticks`。那样做会一笔勾销上面整张表——因为 tick 管线与结算管线是**连体的**
//! （CAS 成功即评估终局、终局即结算），没有「跑但不结算」的开关可拨。宁可在本文件重写一遍
//! 「组装 RoundInput → 跑 → 提交」，也不共用那条会自动结算的路径。重复的代价是几百行；
//! 复用的代价是这个功能的合规性。
//!
//! ### 终局产物是什么
//!
//! **只有内容**：`ifline_beats.prose` 按拍序拼起来就是那份私人传记，
//! 加上 `ending_reason` / `ending_label` 两个**字符串**收尾。
//! 世界线的结局会经 `finalize_ending_tx` 触发荣誉与结算；if 线的结局只是这条传记的最后一行字。
//! 读取面 `GET /api/me/iflines/{id}/beats` 恒带 `grantedAssets: []` + `affectsOriginWorld: false`，
//! 对客户端明说「你读到的是内容，账户里什么都没多」。
//!
//! 🔴 由此还堵掉一个隐蔽的口子：**if 线里主角"死了"不会封卷传世卡**。封卷是一次
//! `UPDATE cloud_characters`，属被禁写入。所以 if 线里的死亡是**纯内容**——既不能复活
//! （0039 已挡传世卡入场），也不会杀死你在真实世界线的卡。两个方向都不通，才叫平行线。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 成本记在哪（付费功能的成本失真是最不该有的失真）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! if 线跑拍同样烧 token，但**不能写 `world_ticks`**（那是世界线的表，写进去就等于把 if 线接回
//! 上面那条自动链路）。于是成本另起一条独立记账路径：
//!
//! - `ifline_beats.cost_tokens` —— 逐拍实测。口径与 `world_ticks.cost_tokens` **逐字一致**：
//!   共用 `runtime::TokenMeter`（本批次提升为 `pub(crate)`，理由见那里的注释）汇总引擎每次
//!   ModelCall 的 input+output，模型未回报 token 时回退引擎预估。共用一个计量器而不是各写一遍，
//!   是为了让两条路径的成本数字**可比**——否则成本看板会长出一个查不出来的系统性偏差。
//! - `ifline_worlds.cost_tokens_total` —— 该实例累计，同事务累加，按实例查无需扫明细。
//! - 运营读取面：`GET /api/admin/iflines/cost`（本批次新增，按时间窗聚合）。
//!
//! ✅ **已并入主看板（原登记为遗留）**：此处曾写着「`admin_api::dashboards` 只 SUM
//! `world_ticks.cost_tokens`，尚未并入 if 线开销 —— 主看板会系统性漏掉这部分」。
//! 那条已经补上：`GET /api/admin/metrics/overview` 现有 `cost.ifline`（allTime 与 window
//! 两个口径）与 `cost.combined`（世界线 + if 线合计）。
//!
//! 🔴 但 **`cost.total` 的语义一个字没改**，仍是**世界线**口径。这是刻意的：那个字段的历史
//! 对账全都建立在旧语义上，把 if 线悄悄加进去 = 让所有历史数字在同一个字段名下变了含义，
//! 而看板上完全看不出发生过这件事。要平台总开销请读 `cost.combined`。
//!
//! ### 成本闸（付费内容不能有无限算力）
//!
//! 一张副本卡换一条 if 线，如果推进无上限，单条 if 线的算力开销就是无界的——那是个业务窟窿。
//! 故设**拍数上限** `MUSE_IFLINE_MAX_BEATS`（默认 12，可调有界）：到顶强制收尾
//! （`ending_reason='beat_cap'`）。这是成本闸，不是玩法数值，与「不卖胜负」无关。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 叙事质量 SLO 的归属：**不并入世界线 SLO**（本批次不动 `slo/`）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 现有五项 SLO（基尼 / 无戏份 / 收尾 / 二次入世 / 状态-文本矛盾）度量的是**多人世界线**的质量。
//! if 线是**单人**平行线，性质不同：
//!
//! | 指标 | 判定 | 理由 |
//! |---|---|---|
//! | 戏份基尼 | **排除** | 单人样本的基尼恒为 0（完美）。每条 if 线都往池子里灌一个满分，会**稀释真实的多人不公平**——这不是多了点噪声，是让指标失去报警能力 |
//! | 无戏份率 | **排除** | 单人线里结构上不可能有人没戏份，同样只贡献免费的合格样本 |
//! | 二次入世率 | **排除** | if 线**没有入世**这件事（不进 `world_members`、不 join），指标无所指 |
//! | 收尾率 | **排除** | if 线的收尾常由**拍数上限**强制触发，与世界线「叙事弧完成」不是同一件事。混进去会悄悄改掉这项指标的定义 |
//! | 状态-文本矛盾 | **数据留着，读数独立** | 这一项确实同质（逐回合质检，与人数无关）。故逐拍存 `ifline_beats.critic_json`，将来要做 if 线质量读数时数据现成——但那必须是**独立读数**，不是并进世界线同一个池子 |
//!
//! 另有一条工程事实佐证这个判断：`slo/` 判定「一拍真的跑了」的口径是
//! `world_ticks.status='done' AND cost_tokens > 0`。if 线的拍不在那张表里，**默认就是排除的**——
//! 本批次要做的是让这个默认成为**有意的决定并写下来**，而不是一个没人注意到的巧合。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 §14 单人平行线：推进时同样不得引入其他真人玩家
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 开局已按 `world_members` 剥离他人玩家角色（0039），推进时**每一拍再剥一次**（纵深防御）：
//!
//! 1. 组阵容时，`cast_json` 里的候选逐个比对原世界 `world_members` 的他人 `cloud_character_id`，
//!    命中即剔除（`cast_json` 存的本来只有 NPC，这一层挡的是"将来某次装配格式变化把玩家写了进去"）；
//! 2. 每拍跑之前，把活态再过一遍 0039 的 `freeze_snapshot`——引擎的 `StatePatch` 理论上
//!    可以往 `characters` 里塞新键，这一层保证即便塞了他人角色也活不过一拍；
//! 3. 实际上场的角色逐拍落 `ifline_beats.cast_json`，可审。
//!
//! NPC 不剥离：NPC 是世界的，不是谁的。
//!
//! ### 同意门为什么不会把 if 线卡死
//!
//! 引擎的不可逆结果（死亡 / 永久退场 / 永久关系变更）要么当事人已授权、要么当事人是 NPC，
//! 否则整个行动被门控退回并反复重发 `ConsentRequested`。if 线里：
//! NPC 走 `world_controlled` 自动放行；**主角走 `approved_consents`**——
//! 单人平行线里唯一可能被不可逆结果伤到的人就是主人自己，而他**开这条线的动作本身就是同意**
//! （烧掉一张卡 + 手写「如果他那时没退那一步」）。这样 if 线永远不产 `ConsentRequested`，
//! 也就永远不会去写 `consent_requests`（被禁写入表之一）。
//!
//! 生死档沿用原世界的**生效档**（`worlds::effective_lethality`）：分叉点忠实于原世界的契约。
//! 由于主角已预授权，`Consent` 与 `Deathmatch` 在 if 线里行为一致；`Sanctuary` 仍会把致死降级为重伤。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 确定性契约
//! ════════════════════════════════════════════════════════════════════════════
//!
//! **禁三样**：系统随机、浮点 RNG、map 迭代序驱动 RNG。
//!
//! - 每条 if 线首次推进时钉一个 `run_seed`（`fnv1a_64` 派生自不可变身份要素），**此后永不改写**；
//! - 逐拍子流 `Rng(fnv1a_64(run_seed ‖ beat_no) ^ DOMAIN_IFLINE_CAST)`（SplitMix64，域常量
//!   `0x5B`，已登记进 `assembly` 的域常量清单——0x51-0x5A 被装配层与仿真工装占用）；
//! - 抽样对象**先排序成 `Vec` 再抽**（`select_cast` 的入参是排好序的切片，绝不在 BTreeMap 上驱动 RNG）。
//!
//! 于是：**同样的分叉态 + 同样的 `run_seed` + 同样的 `beat_no` → 同样的演员表**，可复算、可复现
//! （用例 `cast_selection_is_deterministic_and_seed_sensitive`）。
//! 模型自身的采样不确定性不在此列——那是外部系统；我们能保证并且已经保证的是
//! **喂进引擎的东西逐字节可复现**。
//!
//! ### 为什么这里可以有 PRNG，而 0039 的 `mod.rs` 是零 RNG
//!
//! 0039 的零 RNG 红线针对的是**资产**：有随机就有爆率，有爆率就是抽卡。本文件不产任何资产，
//! 抽的是「这一拍哪几个 NPC 上场」——纯叙事编排，与掉落无关。若改成「恒取字典序前 N 个」，
//! 每条 if 线会永远只有同样那几个 NPC 出场（后面的 NPC 永无戏份），而且所有 if 线的出场模式一模一样。
//! 用**登记在册的确定性子流**做轮换，既解决了这个问题，又保持每一次结果可复算。
//! 用例 `red_line_module_is_rng_free` 的扫描面已扩到本文件，禁的仍是 `thread_rng` / `random`
//! 这类**不可复现**的系统随机。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::assembly::{fnv1a_64, Rng};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::providers::ModerationVerdict;
use crate::safety;
use crate::worlds::effective_lethality;

use muse_engine::character::types::CharacterCardV2;
use muse_engine::host::{CancelFlag, EngineHost, HostFs, StdFs, SystemClock};
use muse_engine::model::{HttpModelClient, ModelClient};
use muse_engine::narrative::types::{LocationDef, NarrativeState, RoundBudget, RunMode};
use muse_engine::narrative::{is_terminal, NarrativeEngine, RoundInput, Terminal};

use super::{foreign_character_ids, freeze_snapshot, IflineRow};

// ═══════════════════════════════════════════════════════════════════════════
// 域常量（登记在 `assembly` 的统一清单里，见那里的注释）
// ═══════════════════════════════════════════════════════════════════════════

/// 逐拍演员表抽样子流。（0x5C 已被 `safety::semantic` 的私有投影抽样占用；**下一个可用域常量是 0x5D**。）
const DOMAIN_IFLINE_CAST: u64 = 0x5B;

// ═══════════════════════════════════════════════════════════════════════════
// 生命周期字面量
// ═══════════════════════════════════════════════════════════════════════════

/// 推进中：至少跑过一拍且尚未收尾。
pub(super) const STATUS_RUNNING: &str = "running";
/// 已收尾：不可再推进。终局产物是**内容**（传记 + 结局名），不是资产。
pub(super) const STATUS_ENDED: &str = "ended";

/// 一拍的状态。
const BEAT_RUNNING: &str = "running";
const BEAT_DONE: &str = "done";
/// 硬节点/不变量不可满足：**不提交状态**（口径同 `runtime` 的 blocked tick），但计入拍数与成本
/// ——它确实烧了 token。
const BEAT_BLOCKED: &str = "blocked";
/// 引擎重试后仍失败：不提交状态、不计成本（没有成功的尝试可计），但占掉这个 `beat_no`
/// （唯一键已被抢占），玩家重推会拿到下一拍。
const BEAT_FAILED: &str = "failed";

/// 终局原因字面量。前三个来自引擎终局信号（与世界线**同一个判定函数** `is_terminal`），
/// 第四个是本模块的成本闸。
const END_MAINLINE_DONE: &str = "mainline_done";
const END_TIME_CAP: &str = "time_cap";
const END_STARVED: &str = "starved";
const END_BEAT_CAP: &str = "beat_cap";

// ═══════════════════════════════════════════════════════════════════════════
// 参数化（VALIDATION.md §0.2 产品规则参数化，禁止写死）
// ═══════════════════════════════════════════════════════════════════════════

/// 一条 if 线最多推进几拍（**成本闸**）。
///
/// 🔴 必须有上限：一张副本卡换一条 if 线，若推进无限，单条 if 线的算力开销就是无界的。
/// 到顶不是报错而是**强制收尾**——玩家拿到的是一个完整的（哪怕仓促的）结局，不是一条断掉的线。
/// 默认 12 是**待验证的默认策略**，不是承诺；真实值要等 T3「用户愿意为什么付钱」的数据说话。
const ENV_IFLINE_MAX_BEATS: &str = "MUSE_IFLINE_MAX_BEATS";
const DEFAULT_IFLINE_MAX_BEATS: i64 = 12;
/// 硬上限：防运营把 env 配成天文数字，让单条 if 线烧掉不设防的算力。
const MAX_IFLINE_MAX_BEATS: i64 = 60;

pub(super) fn ifline_max_beats() -> i64 {
    std::env::var(ENV_IFLINE_MAX_BEATS)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_IFLINE_MAX_BEATS)
        .min(MAX_IFLINE_MAX_BEATS)
}

/// 单拍 token 预算（喂给引擎 `RoundBudget.max_total_tokens`，引擎据此自限）。
const ENV_IFLINE_BEAT_TOKENS: &str = "MUSE_IFLINE_BEAT_TOKENS";
const DEFAULT_IFLINE_BEAT_TOKENS: i64 = 40_000;
const MAX_IFLINE_BEAT_TOKENS: i64 = 400_000;

fn ifline_beat_tokens() -> u64 {
    std::env::var(ENV_IFLINE_BEAT_TOKENS)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_IFLINE_BEAT_TOKENS)
        .min(MAX_IFLINE_BEAT_TOKENS) as u64
}

/// 每拍上场角色总数（主角 + NPC）。
///
/// 引擎需要 ≥2 张活跃卡才有碰撞可写（`runtime` 的 `insufficient_members` 门同此口径）；
/// 上限 5 是引擎的建议规模（`RoundInput.active_cards` 文档「活跃角色 2–5」）。
const ENV_IFLINE_CAST_SIZE: &str = "MUSE_IFLINE_CAST_SIZE";
const DEFAULT_IFLINE_CAST_SIZE: i64 = 4;
const MIN_IFLINE_CAST_SIZE: i64 = 2;
const MAX_IFLINE_CAST_SIZE: i64 = 5;

fn ifline_cast_size() -> usize {
    std::env::var(ENV_IFLINE_CAST_SIZE)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_IFLINE_CAST_SIZE)
        .clamp(MIN_IFLINE_CAST_SIZE, MAX_IFLINE_CAST_SIZE) as usize
}

/// 引擎失败重试次数（口径同 `runtime` 的 `for attempt in 0..2`）。
const BEAT_ATTEMPTS: u32 = 2;

// ═══════════════════════════════════════════════════════════════════════════
// 确定性：种子与演员表抽样
// ═══════════════════════════════════════════════════════════════════════════

/// 一条 if 线的 `run_seed`：派生自**不可变身份要素**，故任何时候重算都是同一个值。
///
/// 用 `\u{1}` 作分隔符（不可能出现在 id 里）避免「a‖bc」与「ab‖c」撞种（口径同
/// `assembly::instance_seed`）。落库是为了即便将来改了派生式，**已开的 if 线也不会换种子**
/// ——种子一旦钉住就是那条线身份的一部分。
pub(super) fn derive_run_seed(row: &IflineRow) -> u64 {
    fnv1a_64(
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
            row.id,
            row.origin_world_id,
            row.character_id,
            row.fork_point,
            row.fork_tick_no,
            row.fork_state_revision
        )
        .as_bytes(),
    )
}

/// 本拍子流（`Rng(fnv1a_64(run_seed ‖ beat_no) ^ DOMAIN)`，范式同 `runtime::simulation::sub_stream`）。
pub(super) fn beat_stream(run_seed: u64, beat_no: i64) -> (Rng, u64) {
    let mixed = fnv1a_64(format!("{run_seed:016x}\u{1}{beat_no}").as_bytes());
    (Rng(mixed ^ DOMAIN_IFLINE_CAST), mixed ^ DOMAIN_IFLINE_CAST)
}

/// 从**已排序**的 NPC 候选里确定性抽 `want` 个。
///
/// 🔴 入参是 `&[String]`（排好序的切片）而不是 map：确定性契约禁止用 map 迭代序驱动 RNG。
/// 算法是部分 Fisher–Yates（在候选副本上做，不动入参），抽完**再排序**返回——
/// 于是返回值只取决于「抽中了哪几个」，与抽中的**次序**无关，比对更稳。
pub(super) fn select_cast(candidates: &[String], want: usize, rng: &mut Rng) -> Vec<String> {
    if want == 0 || candidates.is_empty() {
        return Vec::new();
    }
    if want >= candidates.len() {
        return candidates.to_vec();
    }
    let mut pool: Vec<String> = candidates.to_vec();
    let mut out: Vec<String> = Vec::with_capacity(want);
    for i in 0..want {
        let pick = i + rng.below(pool.len() - i);
        pool.swap(i, pick);
        out.push(pool[i].clone());
    }
    out.sort();
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// 阵容与场景（从原世界装配复制一次后钉死）
// ═══════════════════════════════════════════════════════════════════════════

/// if 线的阵容与场景（`ifline_worlds.cast_json` 的结构）。
#[derive(Debug, Default)]
struct IflineCast {
    /// NPC：`(id, 卡)`，按 id 升序。
    npcs: Vec<(String, CharacterCardV2)>,
    /// NPC 初始地点。
    homes: BTreeMap<String, String>,
    /// 地点图。空 = 退化为单一场景（与 `runtime` 同口径）。
    locations: BTreeMap<String, LocationDef>,
    /// 可用结局名（确定性取首项作为 if 线的结局标签）。
    endings: Vec<String>,
}

/// 从原世界 `assembled_json` 抽出 if 线要用的那部分，**剔除任何他人玩家角色**（§14 纵深防御第 1 层）。
///
/// 🔴 只在**首次推进**时做一次，结果落 `ifline_worlds.cast_json` 后钉死。
/// 每拍现读原世界会把「快照是死数据」这条 0039 的立论作废——原世界一旦被改，if 线跟着变，
/// 那它就不是一条独立的平行线，而是原世界的一个视图。
fn extract_cast(assembled_json: &str, foreign: &BTreeSet<String>) -> Value {
    let mut npcs: Vec<Value> = Vec::new();
    let mut locations: Vec<Value> = Vec::new();
    let mut endings: Vec<String> = Vec::new();

    if let Ok(v) = serde_json::from_str::<Value>(assembled_json) {
        if let Some(arr) = v.pointer("/assembly/worldCharacterEntries").and_then(Value::as_array) {
            for entry in arr {
                let Some(npc_id) = entry.get("characterId").and_then(Value::as_str) else { continue };
                let npc_id = npc_id.trim();
                // 🔴 §14：装配表里出现的任何**他人玩家角色**一律剔除。
                // 正常情况下 worldCharacterEntries 只有 NPC，这一层挡的是将来格式变化把玩家写进来。
                if npc_id.is_empty() || foreign.contains(npc_id) {
                    continue;
                }
                // 卡必须能被引擎解析，否则留着也进不了 active_cards，白占名额。
                if entry
                    .get("card")
                    .and_then(|c| serde_json::from_value::<CharacterCardV2>(c.clone()).ok())
                    .is_none()
                {
                    continue;
                }
                npcs.push(json!({
                    "characterId": npc_id,
                    "card": entry.get("card").cloned().unwrap_or(Value::Null),
                    "location": entry.get("location").and_then(Value::as_str).unwrap_or(""),
                }));
            }
        }
        if let Some(arr) = v.pointer("/assembly/locationGraph").and_then(Value::as_array) {
            for node in arr {
                if serde_json::from_value::<LocationDef>(node.clone()).is_ok() {
                    locations.push(node.clone());
                }
            }
        }
        if let Some(arr) = v.pointer("/assembly/enabledEndings").and_then(Value::as_array) {
            for e in arr {
                // 结局项可能是字符串，也可能是带 id/name 的对象——两种都取得出一个确定的名字。
                let name = e
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| e.get("id").and_then(Value::as_str).map(str::to_string))
                    .or_else(|| e.get("name").and_then(Value::as_str).map(str::to_string));
                if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
                    endings.push(n);
                }
            }
        }
    }
    // NPC 按 id 升序钉死：这是后续抽样的**排序基底**，必须与迭代序无关。
    npcs.sort_by(|a, b| {
        a.get("characterId").and_then(Value::as_str).unwrap_or("")
            .cmp(b.get("characterId").and_then(Value::as_str).unwrap_or(""))
    });
    json!({ "npcs": npcs, "locations": locations, "endings": endings })
}

fn parse_cast(raw: &str) -> IflineCast {
    let mut out = IflineCast::default();
    let Ok(v) = serde_json::from_str::<Value>(raw) else { return out };
    if let Some(arr) = v.get("npcs").and_then(Value::as_array) {
        for e in arr {
            let Some(id) = e.get("characterId").and_then(Value::as_str) else { continue };
            let Some(card) =
                e.get("card").and_then(|c| serde_json::from_value::<CharacterCardV2>(c.clone()).ok())
            else {
                continue;
            };
            out.homes
                .insert(id.to_string(), e.get("location").and_then(Value::as_str).unwrap_or("").into());
            out.npcs.push((id.to_string(), card));
        }
    }
    if let Some(arr) = v.get("locations").and_then(Value::as_array) {
        for node in arr {
            if let Ok(def) = serde_json::from_value::<LocationDef>(node.clone()) {
                if !def.id.trim().is_empty() {
                    out.locations.insert(def.id.clone(), def);
                }
            }
        }
    }
    if let Some(arr) = v.get("endings").and_then(Value::as_array) {
        out.endings = arr.iter().filter_map(Value::as_str).map(str::to_string).collect();
    }
    out.npcs.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// 推进一拍
// ═══════════════════════════════════════════════════════════════════════════

/// 一拍的结果（供端点组装响应）。
#[derive(Debug)]
pub(super) struct BeatOutcome {
    pub(super) beat_id: String,
    pub(super) beat_no: i64,
    pub(super) status: &'static str,
    pub(super) prose_visible: Option<String>,
    pub(super) moderation: String,
    pub(super) cast: Vec<String>,
    pub(super) cost_tokens: i64,
    pub(super) note: Option<String>,
    /// 本拍之后 if 线的生命周期状态（`running` / `ended`）。
    pub(super) ifline_status: &'static str,
    pub(super) ending_reason: String,
    pub(super) ending_label: String,
}

/// 推进一拍。
///
/// 🔴 **本函数与 `runtime::commit_tick` 零交叉**：它写的是 `ifline_beats` + `ifline_worlds`，
/// 从不写 `worlds` / `world_ticks` / `world_members`，也从不调用任何 `settle_*`。
/// 结算管线只认那两张世界线表，本路径与它在物理上没有交点。
///
/// 顺序（每一步失败都不推进状态）：
/// 1. 已收尾 → 409（不可再推进）；
/// 2. 拍数到顶 → **强制收尾**（不跑模型、不花 token，直接落 `beat_cap` 终局）；
/// 3. 钉 `run_seed` / 复制 `cast_json`（首拍一次性，幂等 CAS）；
/// 4. **抢占 `beat_no`**（唯一键 + `ON CONFLICT DO NOTHING`）——推进的并发闸；
/// 5. 组装活态（§14 再剥一次）→ 写隔离的引擎 FS → 组 `RoundInput` → 跑（重试 1 次）；
/// 6. 正文过机审（**事务外**，网络调用）；
/// 7. `commit_beat`：一拍一事务，`live_revision` CAS。
pub(super) async fn advance_one_beat(
    state: &AppState,
    row: &IflineRow,
    model_override: Option<Arc<dyn ModelClient>>,
) -> Result<BeatOutcome, ApiError> {
    let db = &state.db;

    // 1) 已收尾不可再推进。终局是内容的终点，不是一个可以反复触发的按钮。
    if row.status == STATUS_ENDED {
        return Err(ApiError::Conflict(format!(
            "这条 if 线已收尾（{}），不可再推进。终局产物是内容（传记 + 结局名），不是资产——\
             重开一条 if 线需要另烧副本卡。",
            if row.ending_reason.is_empty() { "已结束" } else { &row.ending_reason }
        )));
    }

    let max_beats = ifline_max_beats();
    let beat_no = row.beat_count;

    // 2) 拍数到顶 → 强制收尾。**先于抢占**：不占 beat_no、不跑模型、不花一个 token。
    if beat_no >= max_beats {
        let label = pick_ending_label(&parse_cast(&row.cast_json));
        finalize_ending(db, row, END_BEAT_CAP, &label).await?;
        return Ok(BeatOutcome {
            beat_id: String::new(),
            beat_no,
            status: BEAT_DONE,
            prose_visible: None,
            moderation: "approved".into(),
            cast: Vec::new(),
            cost_tokens: 0,
            note: Some(format!(
                "已达推进上限（{max_beats} 拍，运营参数 {ENV_IFLINE_MAX_BEATS}），本条 if 线收尾。\
                 上限是**成本闸**——付费内容的算力开销必须有界；它不是玩法数值，与胜负无关。"
            )),
            ifline_status: STATUS_ENDED,
            ending_reason: END_BEAT_CAP.into(),
            ending_label: label,
        });
    }

    // ── 原世界（**只读**：本模块对世界线表只有 SELECT） ───────────────────────
    let origin = crate::worlds::load_world(db, &row.origin_world_id).await?;
    // if 线沿用原世界钉住的路由（含备用路由）——分叉忠实于原世界的配置。
    let Some((routes, max_output_tokens, route_fallback)) =
        crate::runtime::resolve_model_routes(db, &origin.model_route_version).await?
    else {
        // 与 `runtime` 的 `no_model_config` 同口径：dev 无模型配置 → 明确拒绝，不空跑。
        return Err(ApiError::BadRequest(
            "原世界钉住的模型路由未配置，if 线无法推进（dev 环境常见）。这不会消耗任何资源。".into(),
        ));
    };
    let prompts = crate::runtime::resolve_prompts(db, &origin.prompt_set_version).await?;

    // ── §14 剥离名单（原世界里他人玩家的角色） ────────────────────────────────
    let foreign_vec = foreign_character_ids(db, &row.origin_world_id, &row.owner_id).await?;
    let foreign: BTreeSet<String> = foreign_vec.iter().cloned().collect();

    // 3) 首拍一次性：钉种子 + 复制阵容。两者都用条件 UPDATE 做幂等（不是先查后写）。
    let run_seed = if row.run_seed.is_empty() {
        let seed = derive_run_seed(row);
        let hex = format!("{seed:016x}");
        // CAS：只有仍为空时才写，并发下先到的钉住，后到的读回同一个值（派生式确定 → 值必然相同）。
        sqlx::query("UPDATE ifline_worlds SET run_seed = $1 WHERE id = $2 AND run_seed = ''")
            .bind(&hex)
            .bind(&row.id)
            .execute(db)
            .await?;
        seed
    } else {
        u64::from_str_radix(&row.run_seed, 16).unwrap_or_else(|_| derive_run_seed(row))
    };

    let cast_json_raw = if row.cast_json.is_empty() {
        let built = extract_cast(origin.assembled_json.as_deref().unwrap_or("{}"), &foreign);
        let raw = built.to_string();
        sqlx::query("UPDATE ifline_worlds SET cast_json = $1 WHERE id = $2 AND cast_json = ''")
            .bind(&raw)
            .bind(&row.id)
            .execute(db)
            .await?;
        // 并发下若被别人先写，读回权威值——`extract_cast` 是纯函数且入参相同，两者必然一致，
        // 但读回仍是对的做法：以库里那份为准，不以本请求算出来的那份为准。
        fetch_cast_json(db, &row.id).await?.unwrap_or(raw)
    } else {
        row.cast_json.clone()
    };
    let cast = parse_cast(&cast_json_raw);

    // ── 主角卡（属本人；0039 受理时已校验在世，此处只取卡） ────────────────────
    let protagonist_card = load_character_card(db, &row.character_id, &row.owner_id).await?;
    let Some(protagonist_card) = protagonist_card else {
        return Err(ApiError::BadRequest(
            "主角卡不可读（已下架或数据损坏），if 线无法推进。未消耗任何资源。".into(),
        ));
    };

    // ── 演员表（确定性抽样；候选已按 id 升序，绝不在 map 上驱动 RNG） ──────────
    let (mut rng, stream_seed) = beat_stream(run_seed, beat_no);
    let npc_ids: Vec<String> = cast
        .npcs
        .iter()
        .map(|(id, _)| id.clone())
        // 🔴 §14 纵深防御第 1 层（cast_json 是首拍钉的，此后原世界成员表若有变化这里仍兜得住）。
        .filter(|id| !foreign.contains(id) && id != &row.character_id)
        .collect();
    let want_npc = ifline_cast_size().saturating_sub(1);
    let chosen_npcs = select_cast(&npc_ids, want_npc, &mut rng);

    // 引擎需要 ≥2 张活跃卡才有碰撞可写。NPC 池为空 → 无法推进（明确告知，不空跑烧钱）。
    if chosen_npcs.is_empty() {
        return Err(ApiError::BadRequest(
            "这条 if 线没有可上场的世界角色（原世界模板未声明 NPC，且他人玩家角色已按 §14 剥离），\
             单人无对手戏可写，无法推进。未消耗任何资源。\
             🔴 剥离造成的内容缺口是**有意的代价**：未经同意把别人的角色拖进你的 if 线，\
             等于以他人角色之名生成他人未授权的言行。"
                .into(),
        ));
    }

    // 4) 🔴 抢占 `beat_no`（唯一键 + ON CONFLICT DO NOTHING）——**推进的并发闸**。
    //    绝不是「先查 beat_count 再写」：那样两个并发请求会读到同一个计数、各跑各的模型、
    //    各花各的 token，最后各写各的状态互相覆盖。抢占失败即 409，一个 token 都不花。
    let beat_id = new_id("ifb");
    let now = now_ms();
    let seed_hex = format!("{stream_seed:016x}");
    let mut cast_ids: Vec<String> = chosen_npcs.clone();
    cast_ids.push(row.character_id.clone());
    cast_ids.sort();
    let claimed = sqlx::query(
        "INSERT INTO ifline_beats (id, ifline_id, beat_no, status, base_revision, seed_hex, \
         cast_json, prose, moderation, critic_json, cost_tokens, terminal_reason, note, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, '', 'pending', '{}', 0, '', NULL, $8) \
         ON CONFLICT (ifline_id, beat_no) DO NOTHING",
    )
    .bind(&beat_id)
    .bind(&row.id)
    .bind(beat_no)
    .bind(BEAT_RUNNING)
    .bind(row.live_revision)
    .bind(&seed_hex)
    .bind(serde_json::to_string(&cast_ids).unwrap_or_else(|_| "[]".into()))
    .bind(now)
    .execute(db)
    .await?;
    if claimed.rows_affected() == 0 {
        return Err(ApiError::Conflict(format!(
            "第 {beat_no} 拍已在推进中或已完成（同一拍只跑一次）。刷新后再看这条 if 线的进度。"
        )));
    }

    // 🔴 抢占之后的一切错误都必须经 `fail_beat` 收口，**否则这条 if 线会被永久卡死**：
    // `beat_no` 已被唯一键占掉，而 `beat_count` 没推进 → 玩家每次重推都算出同一个 `beat_no`
    // → 每次都撞唯一键 409 → 这条付费的线再也推不动，且他已经烧掉了副本卡。
    // 用 async 块把「抢占之后」整段圈起来，比在每个 `?` 前面手写一遍收尾可靠得多
    // ——将来在中间加一个新的 fallible 步骤时，不会因为忘了加收尾而重新引入这个死锁。
    let claimed_result: Result<BeatOutcome, ApiError> = async {

    // 5) 活态：live_state_json 为空则以冻结的分叉态开局。
    //    🔴 `snapshot_json` **永不被覆盖**：它是分叉点证据，一旦被推进覆盖，
    //    玩家与运营就再也无法核验 `state_fidelity` 那一列说的是不是真的。
    let base_state_json =
        if row.live_state_json.is_empty() { &row.snapshot_json } else { &row.live_state_json };

    // 🔴 §14 纵深防御第 2 层：每拍跑之前把活态再过一遍 0039 的剥离函数。
    // 引擎的 StatePatch 理论上能往 `characters` 里塞新键；这一层保证即便塞了他人角色也活不过一拍。
    let cleaned = freeze_snapshot(base_state_json, &foreign_vec, &row.character_id)
        .map(|f| f.snapshot.to_string())
        .unwrap_or_else(|| base_state_json.clone());

    // 🔴 解析失败必须**硬错**，不许 `unwrap_or_default()`：默认值是一个**空世界**，
    // 拿它开跑等于把玩家这条线的全部内容悄悄抹掉再从零演一遍，而他不会收到任何提示。
    // 宁可这一拍推不动（`beat_no` 已被占，玩家重推走下一拍），也不静默毁掉分叉态。
    let Ok(mut engine_state) = serde_json::from_str::<NarrativeState>(&cleaned) else {
        return Err(ApiError::internal(std::io::Error::other(
            "if 线分叉态无法解析为引擎叙事状态，本拍未推进（未改动任何已有内容）",
        )));
    };
    // run_id = if 线 id（`ifw_` 前缀）：与世界的 run_id（= world_id）**不同名空间**，
    // 引擎 FS 上的目录因此天然不可能与任何世界撞名。
    engine_state.run_id = row.id.clone();
    engine_state.revision = row.live_revision.max(0) as u64;
    if engine_state.schema_version == 0 {
        engine_state.schema_version = 1;
    }
    // 上场角色必须在状态里有格子（口径同 `runtime::build_seed_state` 的 `or_default`）。
    let default_start = cast
        .locations
        .values()
        .find(|l| !l.is_secret_realm)
        .map(|l| l.id.clone())
        .unwrap_or_default();
    for cid in &cast_ids {
        let cs = engine_state.characters.entry(cid.clone()).or_default();
        if cs.location.is_empty() {
            let home = cast.homes.get(cid).cloned().unwrap_or_default();
            cs.location = if home.is_empty() { default_start.clone() } else { home };
        }
    }

    // 🔴 引擎 FS 根目录与世界线**物理分离**：`ifline-data/` vs `world-data/`。
    // 同一棵目录树下即便 run_id 撞名也不会互相覆盖——这是「if 线影响不了原世界」在文件层的落实。
    let data_dir =
        std::path::PathBuf::from(&state.config.object_store_dir).join("ifline-data").join(&row.id);
    let fs: Arc<dyn HostFs> = Arc::new(StdFs::new(data_dir));
    muse_engine::store::write_json(
        fs.as_ref(),
        &muse_engine::narrative::state::state_path(&row.id),
        &engine_state,
    )?;

    // active_cards / brief / world_controlled
    let mut active_cards: BTreeMap<String, CharacterCardV2> = BTreeMap::new();
    let mut other_brief: BTreeMap<String, String> = BTreeMap::new();
    other_brief.insert(row.character_id.clone(), protagonist_card.identity.name.clone());
    active_cards.insert(row.character_id.clone(), protagonist_card);
    let mut world_controlled: Vec<String> = Vec::new();
    for (id, card) in &cast.npcs {
        if !chosen_npcs.iter().any(|c| c == id) {
            continue;
        }
        other_brief.insert(id.clone(), card.identity.name.clone());
        active_cards.insert(id.clone(), card.clone());
        world_controlled.push(id.clone());
    }
    world_controlled.sort();

    // 🔴 分叉前提 → 主角的托梦。玩家手写的「如果他那时没退那一步」正是这条线的内容种子，
    // 而托梦本就是「主人对自己角色说的话」——语义严丝合缝，且不必给引擎加任何新字段。
    // **只在第 0 拍投递**：它是分叉的起点，不是每拍复读的指令。
    // 未过审的前提不投递（`premise_moderation != 'approved'`）——私密不豁免机审。
    let mut whispers: BTreeMap<String, String> = BTreeMap::new();
    if beat_no == 0 && row.premise_moderation == "approved" && !row.premise.trim().is_empty() {
        whispers.insert(row.character_id.clone(), row.premise.clone());
    }

    // 🔴 主角预授权不可逆结果（见模块头「同意门为什么不会把 if 线卡死」）。
    // 单人平行线里唯一可能被伤到的人就是主人自己，而开这条线的动作本身就是同意。
    // 于是 if 线永不产 ConsentRequested，也就永不去写 `consent_requests`（被禁写入表之一）。
    let approved_consents = vec![row.character_id.clone()];
    // 生死档沿用**原世界**的生效档（分叉点忠实于原世界的契约），故按原世界解析开关。
    let lethality = effective_lethality(
        &origin.lethality,
        crate::worlds::deathmatch_enabled(db, Some(&row.origin_world_id)).await,
    );

    let model: Arc<dyn ModelClient> = match model_override {
        Some(m) => m,
        None => Arc::new(HttpModelClient::new()?),
    };
    // 备用路由包在最外层（同 `runtime::process_tick_inner`）。if 线的回退次数目前**不单独落库**——
    // `ifline_beats` 没有对应列，而 if 线的成本已逐拍记在 `cost_tokens` 里（回退那次自然在内）。
    // 如实说明：if 线看不出「这一拍走没走备用」，世界线看得出（`world_ticks.fallback_used`）。
    let model: Arc<dyn ModelClient> = Arc::new(crate::runtime::fallback::FallbackModelClient::new(
        model,
        route_fallback,
        Arc::new(crate::runtime::fallback::FallbackMeter::default()),
    ));

    // 6) 跑（失败重试一次，每次尝试独立计量器 → 只计成功那次的 token；口径同 `runtime`）。
    let mut last_err: Option<String> = None;
    for _attempt in 0..BEAT_ATTEMPTS {
        let meter = Arc::new(crate::runtime::TokenMeter::default());
        let host = EngineHost {
            fs: fs.clone(),
            clock: Arc::new(SystemClock),
            events: meter.clone(),
            model: model.clone(),
        };
        let engine = NarrativeEngine::new(Arc::new(host));
        let input = RoundInput {
            // 观众礼物（open-decisions §5）：**if 线没有观众**——它是单人付费内容，
            // 没有直播间、没有打赏入口。恒空不是「暂未接线」，是这条链上不存在这个概念。
            ambient_events: Vec::new(),
            run_id: row.id.clone(),
            mode: RunMode::Observe,
            active_cards: active_cards.clone(),
            other_cards_brief: other_brief.clone(),
            // 身份池是**世界实例**的开局站位（谁在这个世界里是户部主事）。if 线不重新分配身份：
            // 它是从原世界岔出去的一条线，重掷身份会让分叉点失真。空表 = 引擎上下文里不出现该字段。
            self_identities: BTreeMap::new(),
            whispers: whispers.clone(),
            fragments: BTreeMap::new(),
            temperature_decide: 0.0,
            temperature_writer: 0.8,
            max_output_tokens,
            budget: RoundBudget {
                max_total_tokens: ifline_beat_tokens(),
                spent_tokens: 0,
                max_scenes: 1,
            },
            approved_consents: approved_consents.clone(),
            world_controlled: world_controlled.clone(),
            locations: cast.locations.clone(),
            now_hint: 0,
            stall_hint: None,
            lethality,
            // 境界档 / 本篇戏服（总规格 §6【拍板 3】）：本次接线**只覆盖世界线 `runtime`**，
            // if 线保持 `None` = 与接线前逐字节一致。口径同上面的 `self_identities`——
            // 装配层的实例级叙事装饰不在这条私人分叉线上重新发放；要不要让 if 线继承原世界的
            // 那件戏服，等世界线这一维真的被观察过之后再单独决定（未验证功能默认关闭，
            // VALIDATION §0.1），而不是顺手跟着接线一起悄悄开出去。
            realm_costume: None,
            // 世界线烙印（提案第 5 步）：if 线同样保持空 = 与接线前逐字节一致，理由同上一段。
            // 🔵 这一维还多一层：if 线是**从某一拍分叉出去的私人副本**，而烙印记的是
            // 「这张卡在**别的世界**里经历过什么」——分叉线要不要继承那份跨世界记忆，
            // 是个产品问题（分叉出去的是同一张卡，还是一个假设？），不是接线的顺手事。
            worldline_imprints: Default::default(),
            // 私有线索：if 线同样保持空。分叉出去的是一条私人平行线，
            // 原世界那几条未了的事要不要跟过去，是产品问题（跟过去就等于把同一条钩子发两次）。
            personal_threads: Default::default(),
        };
        let cancel = CancelFlag::new();
        // 🔴 恒走 `run_round`，**不走 `run_event_step`**：DES 调度依赖 `timeline.next_time`
        // 这类世界级调度元数据，而 if 线是玩家一拍一拍手动翻页的私人副本，没有世界时钟在推它。
        // 终局判定另用引擎导出的 `is_terminal`（与世界线同一把尺）。
        match engine.run_round(&routes, &prompts, input, &cancel).await {
            Ok(outcome) => {
                let metered = meter.total_tokens();
                let cost = if metered > 0 { metered } else { outcome.budget.spent_tokens } as i64;

                if let Some(reason) = &outcome.blocked {
                    // 硬节点/不变量不可满足：不提交状态（口径同 `runtime` 的 blocked tick），
                    // 但**计入拍数与成本**——它确实烧了 token，装作没发生就是让成本失真。
                    return commit_blocked(state, row, &beat_id, beat_no, cost, reason).await;
                }

                let prose = outcome.scene.prose.clone();
                let critic = serde_json::to_string(&outcome.critic).unwrap_or_else(|_| "{}".into());
                // 序列化失败在这里必须是**硬错**而不是回落空状态：回落会把一份跑出来的状态
                // 静默换成空态提交上去，比直接失败坏得多（玩家的这一拍凭空消失且无从察觉）。
                let Ok(new_state_json) = serde_json::to_string(&outcome.new_state) else {
                    return Err(ApiError::internal(std::io::Error::other(
                        "if 线新状态序列化失败，本拍未落定",
                    )));
                };
                let new_revision = outcome.new_state.revision as i64;
                // 终局信号：与世界线**同一个判定函数**。if 线是一条真的叙事线，不是降级模拟。
                let terminal = is_terminal(&outcome.new_state);

                // 7) 正文机审（**必须在开事务之前**：网络调用，单连接池下事务内调用会死锁 PoolTimedOut）。
                //    私密不豁免机审——私密只决定「谁能看」，不决定「平台是否为它负责」。
                //    无论裁决都落库，读取面仅 approved 给正文（范式同 0037 / 0039）。
                let moderation = if prose.trim().is_empty() {
                    "approved".to_string()
                } else {
                    match safety::moderate_and_queue(state, "ifline_beat", &beat_id, &prose).await? {
                        ModerationVerdict::Approved => "approved".into(),
                        ModerationVerdict::Pending => "pending".into(),
                        ModerationVerdict::Rejected => "rejected".into(),
                    }
                };

                return commit_beat(
                    state,
                    row,
                    CommitBeat {
                        beat_id: &beat_id,
                        beat_no,
                        prose: &prose,
                        moderation: &moderation,
                        critic_json: &critic,
                        cost_tokens: cost,
                        new_state_json: &new_state_json,
                        new_revision,
                        terminal,
                        cast_ids: &cast_ids,
                        max_beats,
                        endings: &cast.endings,
                    },
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(ifline = %row.id, beat_no, error = %e, "if 线推进失败");
                last_err = Some(e.to_string());
            }
        }
    }

    // 重试后仍失败：标记这一拍失败。**不推进 `live_revision`、不计成本**（没有成功的尝试可计），
    // 但这个 `beat_no` 已被占掉——玩家重推会拿到下一拍，不会反复卡在同一拍上。
    let note = last_err.unwrap_or_else(|| "引擎重试后仍失败".into());
    fail_beat(db, &row.id, &beat_id, beat_no, &note).await?;
    Ok(BeatOutcome {
        beat_id: beat_id.clone(),
        beat_no,
        status: BEAT_FAILED,
        prose_visible: None,
        moderation: "pending".into(),
        cast: cast_ids.clone(),
        cost_tokens: 0,
        note: Some(note),
        ifline_status: STATUS_RUNNING,
        ending_reason: String::new(),
        ending_label: String::new(),
    })

    }
    .await;

    // 🔴 收口：抢占之后的任何失败都把这一拍标 failed **并推进 `beat_count`**，
    // 于是玩家下一次推进拿到的是下一拍，而不是永远撞在同一个被占掉的 `beat_no` 上。
    // 收尾本身再失败也只记日志：原始错误必须原样抛给调用方（用收尾的错误覆盖病因，
    // 会让排查从「引擎为什么失败」变成「数据库为什么写不进去」）。
    if claimed_result.is_err() {
        if let Err(e) =
            fail_beat(db, &row.id, &beat_id, beat_no, "推进在抢占后失败（详见服务端日志）").await
        {
            tracing::error!(ifline = %row.id, beat_no, error = %e, "if 线失败拍收尾也失败");
        }
    }
    claimed_result
}

/// 把一拍标记为失败**并推进拍数**。
///
/// 🔴 两件事必须一起做。只标失败不推进拍数，这条 if 线就永久卡死：`beat_no` 被唯一键占着，
/// 玩家每次重推都算出同一个号、每次都撞 409——而他已经烧掉了副本卡。
/// 两处 CAS（`status = 'running'`、`beat_count = beat_no`）保证重复调用幂等。
async fn fail_beat(
    db: &AnyPool,
    ifline_id: &str,
    beat_id: &str,
    beat_no: i64,
    note: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE ifline_beats SET status = $1, note = $2, finished_at = $3 WHERE id = $4 AND status = $5",
    )
    .bind(BEAT_FAILED)
    .bind(note)
    .bind(now_ms())
    .bind(beat_id)
    .bind(BEAT_RUNNING)
    .execute(db)
    .await?;
    bump_beat_count(db, ifline_id, beat_no).await
}

// ═══════════════════════════════════════════════════════════════════════════
// 提交
// ═══════════════════════════════════════════════════════════════════════════

struct CommitBeat<'a> {
    beat_id: &'a str,
    beat_no: i64,
    prose: &'a str,
    moderation: &'a str,
    critic_json: &'a str,
    cost_tokens: i64,
    new_state_json: &'a str,
    new_revision: i64,
    terminal: Option<Terminal>,
    cast_ids: &'a [String],
    max_beats: i64,
    endings: &'a [String],
}

/// 结局标签：确定性取 `endings` 首项。
///
/// ⚠️ **这句话原本写着「口径同 `runtime::select_ending`——那边也是首个 = 权重最高」，现已不成立**：
/// 世界线侧修掉了「结局选择丢弃 `instance_seed`」的缺陷（总规格 §5「一个模板，千个平行世界」
/// 在结局维上原本不成立），改为在装配层按权重掷点、结果钉进 `assembled_json` 的 `/assembly/selectedEnding`。
/// if 线这边**行为未变**（`enabledEndings` 的语义一字未改，这里仍取首项），坏掉的只是那句交叉引用。
///
/// if 线要不要也跟着掷点是**独立的产品决定**，尚未拍板：世界线掷点是为了让千个实例分叉，
/// 而 if 线本就是玩家花副本卡买的**单条**平行线，「同一个分叉点每次开出不同结局」是不是玩家想要的，
/// 需要产品侧先回答。
///
/// 🔴 **它是一个字符串，不是一个奖励**。世界线的结局会经 `finalize_ending_tx` 触发荣誉与结算；
/// if 线的结局只是这条私人传记的最后一行字。
fn pick_ending_label(cast: &IflineCast) -> String {
    cast.endings.first().cloned().unwrap_or_default()
}

/// 一拍提交：**一个事务写两张表**（`ifline_beats` 定稿 + `ifline_worlds` 活态 CAS 推进）。
///
/// 🔴 这里是整条推进路径的终点，也是最该被反复确认的一段：
/// 它**没有** `end_world_tx` / `finalize_ending_tx` / `settle_*` / `accumulate_contributions_tx`
/// 中的任何一个调用。收尾只是把 `status` 改成 `'ended'`、把两个字符串（原因、结局名）写进去。
/// 世界线那条链路上的每一步在这里都不存在——不是被跳过了，是**根本没有**。
async fn commit_beat(
    state: &AppState,
    row: &IflineRow,
    c: CommitBeat<'_>,
) -> Result<BeatOutcome, ApiError> {
    let now = now_ms();
    // 终局：引擎信号优先；否则看是否是最后一拍（拍数上限这条成本闸）。
    let (ending_reason, ended) = match &c.terminal {
        Some(Terminal::MainlineDone { .. }) => (END_MAINLINE_DONE.to_string(), true),
        Some(Terminal::TimeCapReached) => (END_TIME_CAP.to_string(), true),
        Some(Terminal::Starved) => (END_STARVED.to_string(), true),
        None if c.beat_no + 1 >= c.max_beats => (END_BEAT_CAP.to_string(), true),
        None => (String::new(), false),
    };
    let ending_label = if ended {
        c.endings.first().cloned().unwrap_or_default()
    } else {
        String::new()
    };
    let next_status = if ended { STATUS_ENDED } else { STATUS_RUNNING };

    let mut tx = state.db.begin().await?;

    sqlx::query(
        "UPDATE ifline_beats SET status = $1, prose = $2, moderation = $3, critic_json = $4, \
         cost_tokens = $5, terminal_reason = $6, finished_at = $7 WHERE id = $8 AND status = $9",
    )
    .bind(BEAT_DONE)
    .bind(c.prose)
    .bind(c.moderation)
    .bind(c.critic_json)
    .bind(c.cost_tokens)
    .bind(&ending_reason)
    .bind(now)
    .bind(c.beat_id)
    .bind(BEAT_RUNNING)
    .execute(&mut *tx)
    .await?;

    // 🔴 活态 CAS（`WHERE live_revision = ?`）：不是先查后写。并发下另一笔已推进过 → 命中 0 行 →
    // 整笔回滚，这一拍的状态不落库。宁可让玩家重推一次，也不允许两笔推进互相覆盖。
    let updated = sqlx::query(
        "UPDATE ifline_worlds SET live_state_json = $1, live_revision = $2, beat_count = $3, \
         cost_tokens_total = cost_tokens_total + $4, status = $5, ending_reason = $6, \
         ending_label = $7, ended_at = $8 WHERE id = $9 AND live_revision = $10",
    )
    .bind(c.new_state_json)
    .bind(c.new_revision)
    .bind(c.beat_no + 1)
    .bind(c.cost_tokens)
    .bind(next_status)
    .bind(&ending_reason)
    .bind(&ending_label)
    .bind(if ended { Some(now) } else { None })
    .bind(&row.id)
    .bind(row.live_revision)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(ApiError::Conflict(
            "这条 if 线的状态已被另一次推进改变（并发冲突），本拍未落定。请刷新后重试。".into(),
        ));
    }

    if ended {
        // 全链审计（§0.2）：收尾必须留痕，且**把「什么都没发」这件事写进审计**——
        // 将来任何人翻这条记录，都能一眼看到 if 线的终局没有产出任何资产。
        sqlx::query(
            "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
             VALUES ($1, $2, 'user', 'ifline.ended', $3, $4, $5)",
        )
        .bind(new_id("aud"))
        .bind(&row.owner_id)
        .bind(format!("ifline:{}", row.id))
        .bind(format!(
            "reason={ending_reason}|ending={ending_label}|beats={}|costTokens={}|\
             grantedAssets=none|settlementEntered=none|worldlineChanged=false",
            c.beat_no + 1,
            c.cost_tokens
        ))
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(BeatOutcome {
        beat_id: c.beat_id.to_string(),
        beat_no: c.beat_no,
        status: BEAT_DONE,
        prose_visible: if c.moderation == "approved" && !c.prose.is_empty() {
            Some(c.prose.to_string())
        } else {
            None
        },
        moderation: c.moderation.to_string(),
        cast: c.cast_ids.to_vec(),
        cost_tokens: c.cost_tokens,
        note: None,
        ifline_status: next_status,
        ending_reason,
        ending_label,
    })
}

/// blocked 拍：不提交状态，但计拍数与成本（它确实烧了 token）。
async fn commit_blocked(
    state: &AppState,
    row: &IflineRow,
    beat_id: &str,
    beat_no: i64,
    cost: i64,
    reason: &str,
) -> Result<BeatOutcome, ApiError> {
    let now = now_ms();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE ifline_beats SET status = $1, cost_tokens = $2, note = $3, finished_at = $4 \
         WHERE id = $5 AND status = $6",
    )
    .bind(BEAT_BLOCKED)
    .bind(cost)
    .bind(reason)
    .bind(now)
    .bind(beat_id)
    .bind(BEAT_RUNNING)
    .execute(&mut *tx)
    .await?;
    // 只推进拍数与成本，**不动 `live_state_json` / `live_revision`**（blocked 不提交状态）。
    sqlx::query(
        "UPDATE ifline_worlds SET beat_count = $1, cost_tokens_total = cost_tokens_total + $2 \
         WHERE id = $3 AND beat_count = $4",
    )
    .bind(beat_no + 1)
    .bind(cost)
    .bind(&row.id)
    .bind(beat_no)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(BeatOutcome {
        beat_id: beat_id.to_string(),
        beat_no,
        status: BEAT_BLOCKED,
        prose_visible: None,
        moderation: "approved".into(),
        cast: Vec::new(),
        cost_tokens: cost,
        note: Some(format!("这一拍被硬约束挡住，未推进剧情：{reason}")),
        ifline_status: STATUS_RUNNING,
        ending_reason: String::new(),
        ending_label: String::new(),
    })
}

/// 强制收尾（拍数上限）：不跑模型、不花 token，只把生命周期与两个字符串写定。
///
/// 🔴 同样**没有**任何结算调用。收尾在 if 线里就只是改个状态。
async fn finalize_ending(
    db: &AnyPool,
    row: &IflineRow,
    reason: &str,
    label: &str,
) -> Result<(), ApiError> {
    let now = now_ms();
    // CAS on status：并发两笔同时到顶，只有一笔写成，另一笔读回同样的终局（幂等）。
    sqlx::query(
        "UPDATE ifline_worlds SET status = $1, ending_reason = $2, ending_label = $3, ended_at = $4 \
         WHERE id = $5 AND status <> $6",
    )
    .bind(STATUS_ENDED)
    .bind(reason)
    .bind(label)
    .bind(now)
    .bind(&row.id)
    .bind(STATUS_ENDED)
    .execute(db)
    .await?;
    Ok(())
}

/// 失败拍也要推进计数（否则玩家会反复卡在同一拍）。CAS 保证幂等。
async fn bump_beat_count(db: &AnyPool, ifline_id: &str, beat_no: i64) -> Result<(), ApiError> {
    sqlx::query("UPDATE ifline_worlds SET beat_count = $1 WHERE id = $2 AND beat_count = $3")
        .bind(beat_no + 1)
        .bind(ifline_id)
        .bind(beat_no)
        .execute(db)
        .await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 小工具
// ═══════════════════════════════════════════════════════════════════════════

async fn fetch_cast_json(db: &AnyPool, id: &str) -> Result<Option<String>, ApiError> {
    let row = sqlx::query("SELECT cast_json FROM ifline_worlds WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.and_then(|r| r.try_get::<String, _>("cast_json").ok()).filter(|s| !s.is_empty()))
}

/// 主角卡（属本人 + 未下架）。解析失败 → None（引擎读不了的卡不能上场）。
async fn load_character_card(
    db: &AnyPool,
    character_id: &str,
    owner_id: &str,
) -> Result<Option<CharacterCardV2>, ApiError> {
    let row = sqlx::query(
        "SELECT card_json FROM cloud_characters WHERE id = $1 AND owner_id = $2 AND withdrawn = 0",
    )
    .bind(character_id)
    .bind(owner_id)
    .fetch_optional(db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let raw: String = r.try_get("card_json")?;
    Ok(serde_json::from_str::<CharacterCardV2>(&raw).ok())
}

/// 一条 if 线的推进进度投影（读取面复用）。
pub(super) fn progress_json(row: &IflineRow) -> Value {
    let max_beats = ifline_max_beats();
    json!({
        "beatCount": row.beat_count,
        "maxBeats": max_beats,
        "remainingBeats": (max_beats - row.beat_count).max(0),
        "liveRevision": row.live_revision,
        "advanceable": row.status != STATUS_ENDED && row.beat_count < max_beats,
        // 🔴 成本是**花出去的**，不是发下来的。写进读取面是为了让玩家与运营都看得见这条线的开销，
        // 而不是让它变成一个可以攒的数字。
        "costTokensTotal": row.cost_tokens_total,
        "note": "推进上限是成本闸（付费内容的算力开销必须有界），不是玩法数值——它与胜负无关。",
    })
}

/// 终局投影。**明写 `grantedAssets: []`**：对客户端和玩家直说「你读到的是内容，账户里什么都没多」。
pub(super) fn ending_json(row: &IflineRow) -> Value {
    if row.status != STATUS_ENDED {
        return Value::Null;
    }
    json!({
        "reason": row.ending_reason,
        "label": row.ending_label,
        "endedAt": row.ended_at,
        "isContentOnly": true,
        "grantedAssets": [],
        "notes": [
            "🔴 if 线的终局**不进任何结算管线**：不发历练、不铸副本卡、不发荣誉、不记世界线贡献（§0.1 付费只买体验容量，永不买结果）。",
            "🔴 终局产物只有内容：这条平行线的正文（见 /api/me/iflines/{id}/beats）+ 结局名。",
            "🔴 if 线里发生的死亡不会封卷你的传世卡——原世界的卡一个字节都没变（§0.3）。",
        ],
    })
}
