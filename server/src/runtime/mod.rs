//! 世界运行时（S2）：tick 调度器 + worker 池（§9.2）。
//!
//! - 调度器：为 running 世界按 tick_per_day 生成 tick 任务入 queue(topic="world_tick")；
//!   world_ticks(world_id, tick_no) 唯一索引保证重复入队自然幂等；回收崩溃遗留 running、补偿滞留 pending。
//! - worker：pop → **原子认领 tick(pending→running CAS，C-1)** → 读世界(base_revision) → 预算预检
//!   （token + cny 熔断则暂停世界，B-2）→ **回灌：DB narrative_state_json 物化到引擎 FS(单一事实源，E-1)**
//!   （首 tick 用 assembled_json/skeleton 种子硬节点/禁止谓词/在场角色）→ 组装 RoundInput
//!   → NarrativeEngine::run_round(可注入 mock model) → 同一事务写 narrative_state(CAS)/world_ticks(done,
//!   **实测 token 计费 B-1**)/world_events 投影(**先过 §15 第 2 层敏感词库闸**)/
//!   **仅本 tick 实际喂入的干预 applied(Q-3)**/预算累计 → 广播增量(未过审事件不广播)。
//! - CAS 冲突 = 终态化(C-2)，不再无限 re-enqueue；worker Err = 退避重试 + 上限终态化(C-9)。
//! - dev 态：世界无模型配置 → tick 跳过并 warn，不 panic。
//! - **成本记账只有一条判据：这一拍有没有调过模型**（#42）。调过 → 记实测 token（提交拍走
//!   `commit_tick`，阻断拍走 `finish_tick_blocked`）；没调过 → `finish_tick_noop` 记 0。
//!   「提交没提交」与「花没花钱」是两件独立的事，不可互相代替。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Any, AnyPool, Row, Transaction};

use crate::app::AppState;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::events::{self, ProjectionMember, WsMessage};
use crate::queue;
use crate::worlds::{effective_lethality, load_world, WorldRow};

use muse_engine::character::types::CharacterCardV2;
use muse_engine::host::{CancelFlag, EngineEvent, EngineHost, HostEvents, HostFs, StdFs, SystemClock};
use muse_engine::model::{HttpModelClient, ModelClient, ModelProfile};
use muse_engine::narrative::types::{
    CharacterState, ConstraintLevel, DomainEvent, DomainEventType, ForbiddenPredicate, LocationDef,
    NarrativeState, NodeStatus, OutlineNode, RoundBudget, RunMode,
};
use muse_engine::narrative::{ModelRoutes, NarrativeEngine, NarrativePrompts, RoundInput, Terminal};

const TOPIC: &str = "world_tick";

/// 无预算配置时的兜底剩余 token（daily_token_budget=0 亦按此放行，但官方建房已强制非零，见 B-2）。
const DEFAULT_REMAINING_TOKENS: u64 = 100_000;
/// 世界路由未配置 `maxOutputTokens` 时的兜底单次输出上限。给推理模型（DeepSeek-R1 等）留足
/// reasoning 预算，从根上降低「reasoning 段吃光 max_tokens → 空 content」的概率（取代旧的进程级
/// MUSE_MAX_OUTPUT_TOKENS env，改由世界钉住的 model_routes 配置读，可按世界调）。
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 2048;
/// token→cny 估算默认单价（分/1K token）；可用 MUSE_TOKEN_CNY_CENTS_PER_1K 覆盖（真实定价是运营配置）。
const DEFAULT_TOKEN_CNY_CENTS_PER_1K: i64 = 2;
/// 单个 tick 的总处理次数上限（跨重启，C-9）：超限即终态 failed，不再无限重跑。
const MAX_TICK_ATTEMPTS: i64 = 5;
/// running 认领超时（毫秒）：超过视为 worker 崩溃遗留，调度器回收。
const CLAIM_STALE_MS: i64 = 300_000;
/// pending 补偿 re-enqueue 的最小滞留阈值（避免每轮全量 re-enqueue 风暴，C-1）。
const RECLAIM_PENDING_MIN_MS: i64 = 30_000;
/// worker 处理错误的退避基数（毫秒，指数退避，C-9）。
const WORKER_BACKOFF_BASE_MS: u64 = 200;
/// worker 单 job 的错误重试上限（C-9）。
const WORKER_MAX_RETRIES: u32 = 3;
/// #3b 同意请求 TTL（毫秒）：不可逆行动的当事人确认窗口（24h）；超时由 expire_stale_consents 保守过期。
const CONSENT_TTL_MS: i64 = 86_400_000;
/// 放置房终局地板默认值（P1 Phase 0，防秒结束第二道守卫）：任何终局在 tick_no < min 前一律不触发。
const DEFAULT_MIN_WORLD_TICKS: i64 = 3;
/// 放置房世界时间上限默认值（P1 Phase 0，回退口径 = world_ticks.tick_no 计数）：兜底保证任意 idle 房必终止。
const DEFAULT_MAX_WORLD_TICKS: i64 = 120;
/// 叙事 critic 报告落库开关的 env 名（§0.2 参数化：可关，不写死）。
const ENV_CRITIC_PERSIST: &str = "MUSE_CRITIC_PERSIST";

fn token_cny_cents_per_1k() -> i64 {
    std::env::var("MUSE_TOKEN_CNY_CENTS_PER_1K")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TOKEN_CNY_CENTS_PER_1K)
}

#[derive(Debug, Serialize, Deserialize)]
struct TickJob {
    world_id: String,
    tick_no: i64,
}

/// process_tick 结果（可观测/测试用）。
#[derive(Debug, PartialEq, Eq)]
pub enum TickStatus {
    Done,
    /// 跳过原因（no_tick / already_done / claimed_elsewhere / world_not_running / superseded /
    /// no_model_config / insufficient_members / cas_conflict / blocked / terminal）
    Skipped(&'static str),
    /// 预算熔断：世界已暂停
    Fused,
    /// 重试后仍失败：世界已暂停
    Failed,
    /// 世界终局（P1 Phase 0）：放置房走完主线 ∨ 到世界时间上限 → 置 status='ended' 停机。
    /// **成功终态，不重试**（worker_loop 按成功 break，schedule_due_ticks 的 status='running' 门自动停排）。
    Concluded,
}

/// YYYY-MM-DD（UTC）预算日键。
pub fn day_string(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

// ---------- token 计量宿主（B-1：把引擎每次 ModelCall 的实测 token 汇总，作为真实计费口径） ----------

/// 收集 run_round 全过程各环节 ModelCall 的 input+output token 实测值。
///
/// `pub(crate)`：**成本口径必须全平台唯一**。任何在本进程里调用引擎的路径（世界线 tick 之外还有
/// 付费副本的推进）都必须用这同一个计量器汇总 token，否则两条路径的成本数字不可比——
/// 一边算 input+output、另一边只算 output 这类偏差，会让成本看板长出一个查不出来的系统性缺口。
/// 复用一个结构体比「两处各写一遍、约定保持一致」可靠得多。
#[derive(Default)]
pub(crate) struct TokenMeter {
    input: AtomicU64,
    output: AtomicU64,
    calls: AtomicU64,
}

impl TokenMeter {
    pub(crate) fn total_tokens(&self) -> u64 {
        self.input.load(Ordering::Relaxed) + self.output.load(Ordering::Relaxed)
    }
}

impl HostEvents for TokenMeter {
    fn emit(&self, event: EngineEvent) {
        if let EngineEvent::ModelCall(log) = event {
            self.input.fetch_add(log.input_tokens.unwrap_or(0) as u64, Ordering::Relaxed);
            self.output.fetch_add(log.output_tokens.unwrap_or(0) as u64, Ordering::Relaxed);
            self.calls.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ---------- 僵局打破提示（B. stall hint） ----------

/// 连续 blocked 达到该次数后，下一次 RoundInput 携带 stall_hint（织入导演 prompt 破局）。
const STALL_HINT_MIN_STREAK: u32 = 2;

/// 单个世界的僵局账：连续 blocked 次数 + 最近一次原因。
#[derive(Debug, Clone)]
struct StallEntry {
    streak: u32,
    reason: String,
}

/// 僵局追踪器：world_id → 连续 blocked 计数与最近原因。**进程内存态，不持久化**——
/// 重启即清零属可接受降级（重新累计 STALL_HINT_MIN_STREAK 次后恢复提示）。
/// 背景：高对抗场景 arbiter 连续 Blocked 时整回合不提交（硬约束保护，不变量不动），
/// 但 pacingNotes 因未提交而丢失 → director 永远看不到僵局原因 → 空转烧 token。
/// 此账把僵局原因经 RoundInput.stall_hint 回灌给引擎导演环节。
#[derive(Default)]
pub(crate) struct StallTracker {
    inner: Mutex<HashMap<String, StallEntry>>,
}

impl StallTracker {
    /// tick 结果 blocked：streak +1 并记住最近原因。
    pub(crate) fn record_blocked(&self, world_id: &str, reason: &str) {
        let mut g = self.inner.lock().expect("stall tracker 锁不可中毒");
        let e = g
            .entry(world_id.to_string())
            .or_insert_with(|| StallEntry { streak: 0, reason: String::new() });
        e.streak = e.streak.saturating_add(1);
        e.reason = reason.to_string();
    }

    /// tick 提交成功：清零该世界的僵局账。
    pub(crate) fn clear(&self, world_id: &str) {
        self.inner.lock().expect("stall tracker 锁不可中毒").remove(world_id);
    }

    /// streak ≥ `STALL_HINT_MIN_STREAK` 时给出下一次 RoundInput 的 stall_hint
    ///（含最近原因与连续次数）；未达阈值 → None。
    pub(crate) fn hint(&self, world_id: &str) -> Option<String> {
        let g = self.inner.lock().expect("stall tracker 锁不可中毒");
        g.get(world_id)
            .filter(|e| e.streak >= STALL_HINT_MIN_STREAK)
            .map(|e| format!("{}（已连续 {} 回合）", e.reason, e.streak))
    }
}

/// 进程级僵局追踪器单例（worker 池共享；Mutex 保护并发 tick）。
fn stall_tracker() -> &'static StallTracker {
    static TRACKER: OnceLock<StallTracker> = OnceLock::new();
    TRACKER.get_or_init(StallTracker::default)
}

// ---------- 成本工程杠杆①：错峰调度（总规格 §17【拍板 16】） ----------

/// 错峰调度：把**连载场 / 慢炖场**的 tick 优先排进供应商的夜间折扣时段（5-7.5 折常见）。
///
/// ═══════════════════════════════════════════════════════════════════════════
/// 这是什么、为什么在调度器里
/// ═══════════════════════════════════════════════════════════════════════════
/// 总规格 §17 的四条成本杠杆里，②上下文缓存与④分环节路由都在**模型调用侧**（引擎
/// `ModelRoutes` 已承载），只有①错峰是纯粹的**排期问题**——「这一拍什么时候跑」由本文件的
/// `schedule_due_ticks` 独家决定，所以它只能落在这里。
/// 目标是 §17 的量级线：叠加四杠杆后单玩家单阶段压到几毛~一元；这条线直接决定 VALIDATION.md
/// T3 门槛「ARPPU ≥ 3× 单用户月度模型成本」能不能成立。
///
/// ═══════════════════════════════════════════════════════════════════════════
/// 🔴 三条硬约束（各有用例锁死）
/// ═══════════════════════════════════════════════════════════════════════════
/// 1. **默认关闭**（VALIDATION §0.1）。开关 `MUSE_OFFPEAK_SCHEDULING` 默认 `false`，
///    关闭时 `schedule_due_ticks` 走的是**与接线前逐字相同的代码路径**（见该函数里的
///    `if !active { …原路… continue; }` 早退），不多一次查询、不改一次排序。
///    用例 `offpeak_disabled_keeps_scheduling_byte_identical`。
/// 2. **直播场不得延后**。§2 场次节奏三档里直播场的产品定义就是「一晚跑完一阶段 + 弹幕实时」，
///    延后它等于毁掉它。豁免判据见 `Config::is_live_room`。用例 `offpeak_never_defers_live_room`。
/// 3. **不得饿死世界**。错峰是「优先在便宜时段跑」，不是「只在便宜时段跑」：距上一拍超过
///    `interval + 延后预算` 就无视时段照跑。预算参数化，且**恒为有限值**。
///    用例 `offpeak_starvation_guard_fires_outside_window`。
///
/// ═══════════════════════════════════════════════════════════════════════════
/// 时区口径：**全链路只有 UTC 一套**
/// ═══════════════════════════════════════════════════════════════════════════
/// 折扣时段是**供应商**的时间口径，不是用户的、也不是服务器本地时区的。而全仓日界已经统一到
/// UTC（`admin_api::dashboards::utc_day_start_ms` / `runtime::day_string` / `reports::day_bounds`）。
/// 为了不引入第二套口径：
/// - 窗口字面量 `MUSE_OFFPEAK_WINDOWS`（如 `00:30-08:30`）**只在解析期**按
///   `MUSE_OFFPEAK_TZ_OFFSET_MIN`（供应商时区相对 UTC 的分钟偏移，默认 0 = 直接按 UTC 书写）
///   折算成 UTC 当日毫秒偏移；
/// - 折算之后，判定、落库、聚合全部是 UTC。运行期没有任何一处再碰时区。
/// 等价性由用例 `offpeak_utc_day_offset_matches_dashboard_day_boundary` 钉住。
///
/// ═══════════════════════════════════════════════════════════════════════════
/// 为什么窗口内要压缩间隔（而不是简单地「窗口外不排」）
/// ═══════════════════════════════════════════════════════════════════════════
/// 简单地「窗口外不排」会把一个 24 拍/天的连载场变成 8 拍/天——那不是错峰，那是**节奏降档**
/// （§2 的节奏是产品承诺，降它要单独拍板，不能作为成本优化的副作用偷偷发生）。
/// 所以窗口内的有效间隔按窗口占全天的比例压缩：`interval × 窗口总长 / 一天`。
/// 8 小时窗口 → 窗口内间隔是原来的 1/3 → 一天的拍数不变，只是全部挤进便宜时段。
/// 压缩后有地板 `MUSE_OFFPEAK_MIN_INTERVAL_MS`（默认 1 分钟）防止窗口配得过窄时压出突发风暴。
///
/// ═══════════════════════════════════════════════════════════════════════════
/// ⚠️ 杠杆③ Batch API：**本批次未做**，理由与改造路径
/// ═══════════════════════════════════════════════════════════════════════════
/// Batch API（约 5 折）是**异步**的：提交 → 轮询 → 取结果，延迟分钟到小时级。而一拍的形状是
/// `run_round`（引擎内部**串行**跑 director → 每角色 decide → arbiter → writer → critic，
/// 后一环的输入是前一环的产物）→ 同一事务 `commit_tick`。三个致命错配：
/// 1. **串行链 × 异步批**：一拍要 5 次批往返，若每次按 Batch 的小时级 SLA 兑现，
///    一拍就是数小时——连「慢炖场（数小时一拍）」都跑不动，更别说连载场。
///    真正能省的做法是**跨世界同环节合批**（把 100 个世界的 director 调用打成一个批），
///    这要求引擎把 `run_round` 从「一次跑完」改成**可挂起/可恢复的分步状态机**
///    （每个环节产出后能把中间态序列化落库、下次从该环节续跑）。
/// 2. **worker 认领超时**：`CLAIM_STALE_MS = 300_000`（5 分钟）。任何在 `process_tick` 内同步
///    等待批结果的实现都会被调度器判定为「worker 崩溃遗留」回收重排 → 同一拍被重复执行。
/// 3. **中间态即半通管线**：`run_round` 与 `commit_tick` 之间是无持久化的内存态。批途中进程重启，
///    世界会卡在「批已提交、结果无人认领」的中间态——比不做更糟（VALIDATION §5「宁可停拍」
///    的原则是失败要**干净地停**，不是停在一半）。
///
/// **需要谁改什么**（本批次禁止改 `crates/`，故只出改造路径）：
/// - `crates/muse-engine`：`NarrativeEngine::run_round` 拆成分步接口（如
///   `step(RoundState) -> Result<StepOutcome>`，`StepOutcome::Pending{批次句柄}` /
///   `StepOutcome::Next(RoundState)`），`RoundState` 可 serde 落库；`ModelClient` 增加
///   `submit_batch` / `poll_batch` 两个可选方法（默认实现回落同步 `complete`，保证
///   `HttpModelClient` 与桌面轨零改动）。
/// - `server`：新增 `world_round_states` 表存中间态 + 一个批次协调器 topic（提交批、轮询批、
///   把结果喂回下一步），`CLAIM_STALE_MS` 对「等批中」的拍改为不回收（另立 `awaiting_batch` 状态）。
/// - 降级路径（VALIDATION §5）：批提交失败 / 超时 / 结果缺失 → **回落同步 `complete` 重跑该环节**；
///   连回落也失败 → 按现有 `mark_tick_failed_and_pause` 停拍暂停世界，绝不半提交。
/// 工作量与风险都远超本批次范围，且**先做①的收益更确定**（错峰对全部非直播场立即生效，
/// 不需要引擎改动）。故本批次只交付①，③留档待评审。
mod offpeak {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use sqlx::AnyPool;

    /// 一天的毫秒数。窗口归一化与间隔压缩都以它为分母。
    pub(super) const DAY_MS: i64 = 86_400_000;

    /// 总开关名。**同时是运行时开关体系（`server/src/flags/`）的开关名**——两边同名是刻意的：
    /// 运营后台看到的名字、`.env` 里写的名字、报警里出现的名字是同一个。
    pub(super) const ENV_ENABLED: &str = "MUSE_OFFPEAK_SCHEDULING";
    /// 折扣时段列表，`HH:MM-HH:MM` 逗号分隔，跨零点自动识别（`22:30-06:00`）。
    pub(super) const ENV_WINDOWS: &str = "MUSE_OFFPEAK_WINDOWS";
    /// 窗口字面量所处时区相对 UTC 的分钟偏移（北京时间 = 480）。默认 0 = 字面量本身就是 UTC。
    pub(super) const ENV_TZ_OFFSET_MIN: &str = "MUSE_OFFPEAK_TZ_OFFSET_MIN";
    /// 折扣时段的名义价格档位（百分比，50 = 5 折）。只进记账，不参与调度判定。
    pub(super) const ENV_DISCOUNT_PCT: &str = "MUSE_OFFPEAK_DISCOUNT_PCT";
    /// 延后预算 = `interval × 该百分比`（防饿死兜底的第一道口径）。
    pub(super) const ENV_MAX_DEFER_PCT: &str = "MUSE_OFFPEAK_MAX_DEFER_PCT";
    /// 延后预算的绝对上限毫秒（防饿死兜底的第二道口径，与第一道取**较小者**）。
    pub(super) const ENV_MAX_DEFER_MS: &str = "MUSE_OFFPEAK_MAX_DEFER_MS";
    /// 窗口内压缩后的间隔地板（防止窗口配得过窄压出突发风暴）。
    pub(super) const ENV_MIN_INTERVAL_MS: &str = "MUSE_OFFPEAK_MIN_INTERVAL_MS";
    /// `tick_per_day` 达到该值即按**直播场**处理，永不延后（红线 2 的参数化判据）。
    pub(super) const ENV_LIVE_TICK_PER_DAY: &str = "MUSE_OFFPEAK_LIVE_TICK_PER_DAY";

    /// 🔴 默认关闭（VALIDATION §0.1）。
    pub(super) const DEFAULT_ENABLED: bool = false;
    /// 默认窗口（UTC）：`16:30-00:30` ≡ 北京时间 `00:30-08:30`，即国内主流供应商的夜间折扣档。
    /// 仅是**示例默认值**——开关默认关闭，它不会自己生效；真实窗口以供应商合同为准，运营配置。
    pub(super) const DEFAULT_WINDOWS: &str = "16:30-00:30";
    /// 默认名义折扣：5 折（§17 原文「Batch 约 5 折 / 错峰 5-7.5 折」的保守中值）。
    pub(super) const DEFAULT_DISCOUNT_PCT: i64 = 50;
    /// 默认延后预算 = 2 倍 interval。
    pub(super) const DEFAULT_MAX_DEFER_PCT: i64 = 200;
    /// 默认延后预算绝对上限 = 6 小时。**任何世界的最长静默 = interval + min(2×interval, 6h)**，
    /// 恒为有限值——这就是「不得饿死世界」的数学保证。
    pub(super) const DEFAULT_MAX_DEFER_MS: i64 = 21_600_000;
    /// 默认窗口内间隔地板 = 1 分钟。
    pub(super) const DEFAULT_MIN_INTERVAL_MS: i64 = 60_000;
    /// 默认直播场判据：≥48 拍/天（每 30 分钟一拍以上）即视为「密集拍」。
    /// §2 三档里连载场上限是「每小时一拍」(24/天)，取 2 倍留足安全边际——
    /// **判错的方向是把连载场当直播场（不省钱），而不是把直播场当连载场（毁体验）**。
    pub(super) const DEFAULT_LIVE_TICK_PER_DAY: i64 = 48;

    /// ms 时间戳 → 当日 UTC 毫秒偏移 `[0, DAY_MS)`。
    ///
    /// 🔴 **与 `admin_api::dashboards::utc_day_start_ms` 是同一套日界**：后者恒等于
    /// `ms - ms.rem_euclid(DAY_MS)`（Unix 纪元本身对齐 UTC 零点，毫秒时间戳不含闰秒），
    /// 于是本函数就是 `ms - utc_day_start_ms(ms)`。不直接调用那个函数是因为它是
    /// `pub(super)`（`admin_api` 私有），跨模块引用要改它的可见性——那属于「不要碰」的文件。
    /// 等价性由用例 `offpeak_utc_day_offset_matches_dashboard_day_boundary` 用 chrono
    /// 独立复算钉住，任何一侧改口径都会红。
    pub(super) fn utc_ms_of_day(ms: i64) -> i64 {
        ms.rem_euclid(DAY_MS)
    }

    fn env_i64(name: &str, default: i64) -> i64 {
        std::env::var(name).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(default)
    }

    /// env 布尔解析。**与 `flags::parse_env_bool` 逐字同构**（那个是私有的）：
    /// `1/true/on/yes` → 开，`0/false/off/no` → 关，其余（含空串）→ 回落默认。
    pub(super) fn env_bool(name: &str, default: bool) -> bool {
        match std::env::var(name) {
            Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "on" | "yes" => true,
                "0" | "false" | "off" | "no" => false,
                _ => default,
            },
            Err(_) => default,
        }
    }

    /// 解析 `HH:MM`（允许 `24:00`）→ 当日毫秒偏移；非法 → None。
    fn parse_hhmm(s: &str) -> Option<i64> {
        let (h, m) = s.trim().split_once(':')?;
        let h: i64 = h.trim().parse().ok()?;
        let m: i64 = m.trim().parse().ok()?;
        if !(0..=24).contains(&h) || !(0..=59).contains(&m) {
            return None;
        }
        let ms = h * 3_600_000 + m * 60_000;
        (ms <= DAY_MS).then_some(ms)
    }

    /// 窗口列表解析 + 归一化：字面量按 `tz_offset_min` 折算成 UTC → 跨零点拆两段 →
    /// 按 start 升序合并重叠/相接段。返回左闭右开的 UTC 当日毫秒区间。
    ///
    /// 防御式：单条非法只丢该条，不牵连其余（同 `seed_narrative_layer` 的口径）。
    /// `start == end` 视为**非法**并丢弃——它到底是零长度还是整天完全歧义，
    /// 与其猜一个不如让它显式不生效（全部条目被丢弃 → 整个错峰退化为关闭，见 `Config::from_env`）。
    pub(super) fn parse_windows(raw: &str, tz_offset_min: i64) -> Vec<(i64, i64)> {
        let shift = tz_offset_min.saturating_mul(60_000);
        let mut spans: Vec<(i64, i64)> = Vec::new();
        for item in raw.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let Some((a, b)) = item.split_once('-') else { continue };
            let (Some(sa), Some(sb)) = (parse_hhmm(a), parse_hhmm(b)) else { continue };
            // 字面量 → UTC：此处是**唯一**一次时区折算，之后全链路只有 UTC。
            let s = (sa - shift).rem_euclid(DAY_MS);
            let e = (sb - shift).rem_euclid(DAY_MS);
            if s == e {
                continue; // 歧义（零长 or 整天）→ 丢弃。
            }
            if e < s {
                // 跨零点：拆成 [s, 24:00) 与 [00:00, e)。
                spans.push((s, DAY_MS));
                spans.push((0, e));
            } else {
                spans.push((s, e));
            }
        }
        spans.retain(|(s, e)| s < e);
        spans.sort_unstable();
        let mut merged: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
        for (s, e) in spans {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        merged
    }

    /// 错峰配置（每轮调度解析一次，纯 env，无 IO）。
    #[derive(Debug, Clone)]
    pub(super) struct Config {
        /// 归一化后的 UTC 折扣时段（左闭右开、已合并、按 start 升序）。恒非空（空则整个 Config 为 None）。
        pub(super) windows: Vec<(i64, i64)>,
        /// 折扣时段总时长（毫秒），窗口内间隔压缩的分子。
        pub(super) window_total_ms: i64,
        pub(super) discount_pct: i64,
        pub(super) max_defer_pct: i64,
        pub(super) max_defer_ms: i64,
        pub(super) min_interval_ms: i64,
        pub(super) live_tick_per_day: i64,
    }

    impl Config {
        /// 从 env 解析。**窗口一条都解析不出来 → None**（整个错峰退化为关闭，逐字走原路）——
        /// 配错窗口的后果必须是「功能不生效」，而不是「所有世界永远被延后」。
        pub(super) fn from_env() -> Option<Self> {
            let raw = std::env::var(ENV_WINDOWS).unwrap_or_else(|_| DEFAULT_WINDOWS.to_string());
            let windows = parse_windows(&raw, env_i64(ENV_TZ_OFFSET_MIN, 0));
            if windows.is_empty() {
                return None;
            }
            let window_total_ms: i64 = windows.iter().map(|(s, e)| e - s).sum::<i64>().clamp(1, DAY_MS);
            Some(Self {
                windows,
                window_total_ms,
                discount_pct: env_i64(ENV_DISCOUNT_PCT, DEFAULT_DISCOUNT_PCT).clamp(1, 100),
                max_defer_pct: env_i64(ENV_MAX_DEFER_PCT, DEFAULT_MAX_DEFER_PCT).max(0),
                max_defer_ms: env_i64(ENV_MAX_DEFER_MS, DEFAULT_MAX_DEFER_MS).max(0),
                min_interval_ms: env_i64(ENV_MIN_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS).max(1),
                live_tick_per_day: env_i64(ENV_LIVE_TICK_PER_DAY, DEFAULT_LIVE_TICK_PER_DAY).max(1),
            })
        }

        /// 该时刻是否落在折扣时段（UTC 口径）。
        pub(super) fn in_window(&self, now: i64) -> bool {
            let o = utc_ms_of_day(now);
            self.windows.iter().any(|(s, e)| o >= *s && o < *e)
        }

        /// 🔴 **直播场豁免判据**（红线 2）。两条，命中任一即豁免、永不延后：
        /// - `room_type == "arena"`：赛事房。§2 直播场的两个典型场景之一就是赛事，
        ///   且它带弹幕实时观战，延后等于毁产品定义。
        /// - `tick_per_day >= live_tick_per_day`：密集拍。§2 里「一晚跑完一阶段」的直播场在
        ///   代码里的唯一载体就是 tick 节奏；连载场上限「每小时一拍」=24/天，默认阈值取 48。
        ///
        /// `chapter`/`idle` 房不按房型豁免——它们的档位由节奏决定（§2：连载场是官方阶段的
        /// **默认**档，慢炖场是自定义房专属），房型本身（§1 已把 idle/chapter/arena 降级为
        /// 「引擎运行模式」）不再是产品档位。
        pub(super) fn is_live_room(&self, room_type: &str, tick_per_day: i64) -> bool {
            room_type == "arena" || tick_per_day >= self.live_tick_per_day
        }

        /// 窗口内的有效间隔：按窗口占全天的比例压缩，**保住每天的拍数不变**（见模块头）。
        /// 结果钳在 `[min(min_interval_ms, interval), interval]`——绝不比原间隔更长，
        /// 也绝不短于地板（地板本身若比原间隔还大则以原间隔为准，避免钳制区间反转）。
        pub(super) fn compressed_interval(&self, interval: i64) -> i64 {
            if interval <= 0 {
                return interval;
            }
            let scaled = (interval as i128 * self.window_total_ms as i128 / DAY_MS as i128) as i64;
            scaled.max(self.min_interval_ms.min(interval)).min(interval)
        }

        /// 延后预算：`min(interval × max_defer_pct%, max_defer_ms)`。取**较小者**是因为
        /// 阈值越小兜底触发越早、世界越不容易被饿着——安全方向恒为「早点跑」。
        pub(super) fn defer_budget(&self, interval: i64) -> i64 {
            let by_pct = (interval.max(0) as i128 * self.max_defer_pct as i128 / 100) as i64;
            by_pct.min(self.max_defer_ms).max(0)
        }

        /// 🔴 **防饿死兜底线**：距上一拍超过它就无视时段照跑。恒为有限值。
        pub(super) fn max_gap(&self, interval: i64) -> i64 {
            interval.saturating_add(self.defer_budget(interval))
        }
    }

    /// 一拍的排期标记：落 `world_ticks` 的 `off_peak / price_ratio_pct / defer_ms` 三列，
    /// 是「错峰生效了多少、省了多少」在成本仪表上的唯一数据源（口径见迁移 0038 注释）。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct TickMark {
        pub(crate) off_peak: bool,
        pub(crate) price_ratio_pct: i64,
        pub(crate) defer_ms: i64,
    }

    impl Default for TickMark {
        /// 中性值 = 现状：未命中折扣时段、原价、未延后。
        /// 所有非错峰路径（开关关闭 / 直播场 / 手动端点）都用它。
        fn default() -> Self {
            Self { off_peak: false, price_ratio_pct: 100, defer_ms: 0 }
        }
    }

    /// 一轮调度对某个世界的裁决。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Verdict {
        /// 未到点，本轮什么都不做（与错峰无关的原有语义）。
        Idle,
        /// 到点了但落在原价时段且未触兜底 → 本轮延后（错峰真正生效的那一支）。
        Defer,
        /// 排一拍，带标记。
        Schedule(TickMark),
    }

    /// interval 模式（墙钟固定间隔）的错峰裁决。**纯函数**：所有时刻由调用方传入，
    /// 用例可以精确钉住「窗口内 / 窗口外 / 刚好边界 / 已饿死」四种时刻而不依赖真实时钟。
    ///
    /// `last = None`（世界还没有任何一拍）→ **首拍绝不延后**：那是玩家建完房看到的第一印象，
    /// 为省几分钱把它压掉是本末倒置。
    pub(super) fn plan_interval(
        cfg: &Config,
        live: bool,
        now: i64,
        last: Option<i64>,
        interval: i64,
        held_ms: i64,
    ) -> Verdict {
        let in_win = cfg.in_window(now);
        // 直播场按原间隔（豁免压缩，节奏就是它的产品定义）；非直播场在窗口内按压缩间隔。
        let eff_interval = if live || !in_win { interval } else { cfg.compressed_interval(interval) };
        let elapsed = last.map(|t| now - t);
        let due = elapsed.map_or(true, |e| e >= eff_interval);
        if !due {
            return Verdict::Idle;
        }
        // 🔴 直播场豁免（红线 2）：到点即排，从不进入延后分支。
        if live {
            return Verdict::Schedule(TickMark::default());
        }
        if in_win {
            return Verdict::Schedule(TickMark {
                off_peak: true,
                price_ratio_pct: cfg.discount_pct,
                defer_ms: held_ms.max(0),
            });
        }
        // 🔴 防饿死兜底（红线 3）：`last=None`（首拍）与「静默过久」都在这里照跑。
        // 标记按原价（就是原价跑的），但 defer_ms 仍如实记录被压了多久。
        if elapsed.map_or(true, |e| e >= cfg.max_gap(interval)) {
            return Verdict::Schedule(TickMark { defer_ms: held_ms.max(0), ..TickMark::default() });
        }
        Verdict::Defer
    }

    /// event 模式（DES 背靠背，无墙钟 interval）的错峰裁决。调用方已确认「无 outstanding tick」。
    ///
    /// 没有 interval 可依，故防饿死兜底直接用绝对预算 `max_defer_ms`（默认 6h）：
    /// 背靠背世界最长静默 6 小时，之后无视时段照推一拍。
    pub(super) fn plan_event(
        cfg: &Config,
        live: bool,
        now: i64,
        last: Option<i64>,
        held_ms: i64,
    ) -> Verdict {
        if live {
            return Verdict::Schedule(TickMark::default());
        }
        if cfg.in_window(now) {
            return Verdict::Schedule(TickMark {
                off_peak: true,
                price_ratio_pct: cfg.discount_pct,
                defer_ms: held_ms.max(0),
            });
        }
        let elapsed = last.map(|t| now - t);
        if elapsed.map_or(true, |e| e >= cfg.max_defer_ms) {
            return Verdict::Schedule(TickMark { defer_ms: held_ms.max(0), ..TickMark::default() });
        }
        Verdict::Defer
    }

    // ---------- 延后账（进程内存态，度量「错峰生效了多少」） ----------

    /// world_id → **首次被延后的时刻**。排期时取走并清零，差值即本拍的 `defer_ms`。
    ///
    /// 与 `StallTracker` 同口径：**进程内存态、不持久化**，重启即清零属可接受降级
    /// （表现为少记一段延后时长，方向是**低估**错峰效果，不会虚报省钱）。
    /// 不做全量清理：一个世界被延后过又在结束前始终没被排上，会留一条 `(String, i64)` 残留——
    /// 上界是「曾被延后过的世界数」，与 `StallTracker` 同级，可忽略。
    #[derive(Default)]
    pub(super) struct DeferTracker {
        inner: Mutex<HashMap<String, i64>>,
    }

    impl DeferTracker {
        /// 记一次延后（首次记时刻，后续保持不变）。
        pub(super) fn mark(&self, world_id: &str, now: i64) {
            let mut g = self.inner.lock().expect("defer tracker 锁不可中毒");
            g.entry(world_id.to_string()).or_insert(now);
        }
        /// 已被压了多久（只读，供优先级排序用）。
        pub(super) fn held_ms(&self, world_id: &str, now: i64) -> i64 {
            let g = self.inner.lock().expect("defer tracker 锁不可中毒");
            g.get(world_id).map(|t| (now - *t).max(0)).unwrap_or(0)
        }
        /// 取走并清零（真正排上一拍时调用），返回被压的毫秒数。
        pub(super) fn take(&self, world_id: &str, now: i64) -> i64 {
            let mut g = self.inner.lock().expect("defer tracker 锁不可中毒");
            g.remove(world_id).map(|t| (now - t).max(0)).unwrap_or(0)
        }
        #[cfg(test)]
        pub(super) fn clear(&self, world_id: &str) {
            self.inner.lock().expect("defer tracker 锁不可中毒").remove(world_id);
        }
    }

    /// 进程级延后账单例（调度器单线程读写，Mutex 只为与 worker 池共进程时的安全）。
    pub(super) fn defer_tracker() -> &'static DeferTracker {
        static T: OnceLock<DeferTracker> = OnceLock::new();
        T.get_or_init(DeferTracker::default)
    }

    /// 某个世界是否启用错峰。
    ///
    /// 🔵 **优先走运行时开关体系**（`server/src/flags/`，迁移 0036），作用域取 **world**——
    /// 错峰天然按世界灰度：先给几个慢炖场开，看成本曲线，再逐步铺开。
    /// 但 `flags` 的登记表 `KNOWN_FLAGS` 是**白名单**，未登记的名字一律 fail-closed 到 false，
    /// 而登记表在 `server/src/flags/mod.rs`（本批次不改该目录，避免与并行改动撞车）。
    /// 故此处显式判定：
    /// - **已登记** → 走 `flags::is_enabled`，user > world > global > env > 默认 全链生效；
    /// - **未登记**（当前状态）→ 退回 env 兜底，语义与解析链第 ④ 层逐字一致，且**不发一次查询**。
    ///
    /// 后续把开关接进体系只需在 `KNOWN_FLAGS` 里加一条
    /// `FlagDef { name: "MUSE_OFFPEAK_SCHEDULING", default_enabled: false, owner: "runtime", wired: true, .. }`
    /// （并把 `flags::tests` 里的登记数断言 +1），**本文件一行都不用改**，世界级灰度即刻可用。
    pub(super) async fn enabled_for_world(db: &AnyPool, world_id: &str) -> bool {
        if crate::flags::find_flag(ENV_ENABLED).is_some() {
            crate::flags::is_enabled(db, ENV_ENABLED, crate::flags::FlagCtx::world(world_id)).await
        } else {
            env_bool(ENV_ENABLED, DEFAULT_ENABLED)
        }
    }
}

pub(crate) use offpeak::TickMark;

// ---------- 调度 ----------

/// 插入 pending tick（唯一索引幂等）；已存在返回 false。排期标记取中性值（非错峰路径）。
///
/// `allow(dead_code)`：生产路径统一走 `insert_tick_marked`（错峰调度器要带标记），本函数留作
/// **既有公开入口**——`worlds`/`onboarding`/`golden` 的用例与将来的手动排拍仍按中性标记调它，
/// 删掉它等于强迫每个调用方都去写一个恒为 `TickMark::default()` 的参数。
#[allow(dead_code)]
pub async fn insert_tick(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    base_revision: i64,
) -> Result<bool, ApiError> {
    insert_tick_marked(db, world_id, tick_no, base_revision, TickMark::default()).await
}

/// `insert_tick` 的带标记版本：额外落 `off_peak / price_ratio_pct / defer_ms`（迁移 0038）。
/// 中性标记下与 `insert_tick` 的落库结果逐列等价（三列都是各自的 DEFAULT 值）。
pub(crate) async fn insert_tick_marked(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    base_revision: i64,
    mark: TickMark,
) -> Result<bool, ApiError> {
    let exists = sqlx::query("SELECT 1 AS x FROM world_ticks WHERE world_id = ? AND tick_no = ?")
        .bind(world_id)
        .bind(tick_no)
        .fetch_optional(db)
        .await?
        .is_some();
    if exists {
        return Ok(false);
    }
    match sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, created_at, \
         off_peak, price_ratio_pct, defer_ms) \
         VALUES (?, ?, ?, ?, 'pending', ?, ?, ?, ?)",
    )
    .bind(new_id("tick"))
    .bind(world_id)
    .bind(tick_no)
    .bind(base_revision)
    .bind(now_ms())
    .bind(i64::from(mark.off_peak))
    .bind(mark.price_ratio_pct)
    .bind(mark.defer_ms)
    .execute(db)
    .await
    {
        Ok(_) => Ok(true),
        // 并发下唯一索引兜底：重复入队自然幂等。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// 为世界排下一个 tick（tick_no = max+1，base_revision = 当前 state_revision）并入队；已排则返回 None。
pub async fn schedule_tick(state: &AppState, world_id: &str) -> Result<Option<i64>, ApiError> {
    schedule_tick_marked(state, world_id, TickMark::default()).await
}

/// `schedule_tick` 的带标记版本（仅错峰调度器使用；手动端点一律走中性的 `schedule_tick`）。
pub(crate) async fn schedule_tick_marked(
    state: &AppState,
    world_id: &str,
    mark: TickMark,
) -> Result<Option<i64>, ApiError> {
    let world = load_world(&state.db, world_id).await?;
    let max: i64 = sqlx::query("SELECT COALESCE(MAX(tick_no), -1) AS m FROM world_ticks WHERE world_id = ?")
        .bind(world_id)
        .fetch_one(&state.db)
        .await?
        .try_get("m")?;
    let next = max + 1;
    if insert_tick_marked(&state.db, world_id, next, world.state_revision, mark).await? {
        queue::push_json(
            &*state.queue,
            TOPIC,
            &TickJob { world_id: world_id.to_string(), tick_no: next },
            now_ms(),
        )
        .await;
        Ok(Some(next))
    } else {
        Ok(None)
    }
}

async fn schedule_due_ticks(state: &AppState) -> Result<(), ApiError> {
    let interval_override: Option<i64> =
        std::env::var("MUSE_TICK_INTERVAL_MS").ok().and_then(|v| v.parse().ok());
    let now = now_ms();

    // 回收崩溃遗留的 running（started_at 超时）：未超重试上限 → 回 pending 重排；超上限 → 终态 failed（C-1/C-9）。
    let stale_before = now - CLAIM_STALE_MS;
    sqlx::query(
        "UPDATE world_ticks SET status='pending' \
         WHERE status='running' AND started_at IS NOT NULL AND started_at < ? AND attempts < ?",
    )
    .bind(stale_before)
    .bind(MAX_TICK_ATTEMPTS)
    .execute(&state.db)
    .await?;
    sqlx::query(
        "UPDATE world_ticks SET status='failed', error='max_attempts', finished_at=? \
         WHERE status='running' AND started_at IS NOT NULL AND started_at < ? AND attempts >= ?",
    )
    .bind(now)
    .bind(stale_before)
    .bind(MAX_TICK_ATTEMPTS)
    .execute(&state.db)
    .await?;

    let rows =
        sqlx::query("SELECT id, tick_per_day, timeline_mode, room_type FROM worlds WHERE status = 'running'")
            .fetch_all(&state.db)
            .await?;
    let mut worlds: Vec<SchedWorld> = Vec::with_capacity(rows.len());
    for w in &rows {
        worlds.push(SchedWorld {
            id: w.try_get("id")?,
            tick_per_day: w.try_get("tick_per_day")?,
            timeline_mode: w.try_get("timeline_mode")?,
            // P2 Stage3：调度节奏由 room_type 驱动（与引擎 dispatch 的 timeline_mode 解耦）。
            room_type: w.try_get("room_type")?,
        });
    }

    // ── R3 成本工程杠杆①：错峰调度（总规格 §17【拍板 16】，模块头有完整设计说明） ──────────
    // 🔴 默认关闭。`cfg=None`（窗口配错/配空）或某世界开关关闭 → 该世界走下方 `!active` 早退，
    // 与接线前**逐字相同**。开关未登记进 flags 白名单时 `enabled_for_world` 只读 env，不发查询。
    let offpeak_cfg = offpeak::Config::from_env();
    let mut offpeak_on: HashMap<String, bool> = HashMap::new();
    if offpeak_cfg.is_some() {
        for w in &worlds {
            offpeak_on.insert(w.id.clone(), offpeak::enabled_for_world(&state.db, &w.id).await);
        }
    }
    // **折扣时段优先级**：被错峰压得最久的世界先排（先入队 = 先被 worker 领走）。
    // `sort_by_key` 是**稳定排序**，未被延后的世界键恒为 0 → 无人被延后时**逐字保留原顺序**，
    // 这是「开关关闭时行为不变」这条回归保证的一部分。
    let sort_now = now;
    worlds.sort_by_key(|w| std::cmp::Reverse(offpeak::defer_tracker().held_ms(&w.id, sort_now)));

    for w in &worlds {
        let world_id = w.id.clone();
        let tick_per_day = w.tick_per_day;
        let timeline_mode = w.timeline_mode.as_str();
        let room_type = w.room_type.as_str();
        // 该世界本轮是否走错峰路径。false → 下方全部早退回原逻辑。
        let active = offpeak_cfg.is_some() && *offpeak_on.get(&world_id).unwrap_or(&false);
        // 🔴 直播场豁免（红线 2）：命中即永不延后。
        let live = offpeak_cfg
            .as_ref()
            .map(|c| c.is_live_room(room_type, tick_per_day))
            .unwrap_or(true);

        // event 模式（DES，第二块 Phase 2 + P2 Stage3 全房型）：**去掉墙钟 interval 依赖**。调度节奏按 room_type：
        // idle 放置房「背靠背立即推进」——只要 running 且无 outstanding（pending/running）tick 就立即排新 tick；
        // chapter/arena event 房**不自动排新 tick**（手动端点驱动，见下方 room_type 分支）。
        // 游戏时间由引擎在 run_event_step 内按最小 next_time 自算推进，不依赖墙钟到点。
        // 补偿 re-enqueue 改用绝对阈值 RECLAIM_PENDING_MIN_MS（无 interval 可依）。
        // 说明：P1 Phase 0 起，终局（主线走完 / 世界时间上限）由 commit_tick/终局短路置 status='ended' 停机；
        // 外层 WHERE status='running' 使 ended 世界**自动停排 tick**（无需在此显式判终局）。遗留 pending/在飞
        // tick 命中 process_tick_inner 的 world_not_running noop 收尾（良性）。
        if timeline_mode == "event" {
            let straggler_before = now - RECLAIM_PENDING_MIN_MS;
            let pend = sqlx::query(
                "SELECT tick_no FROM world_ticks WHERE world_id = ? AND status = 'pending' AND created_at < ?",
            )
            .bind(&world_id)
            .bind(straggler_before)
            .fetch_all(&state.db)
            .await?;
            for p in &pend {
                let tick_no: i64 = p.try_get("tick_no")?;
                queue::push_json(
                    &*state.queue,
                    TOPIC,
                    &TickJob { world_id: world_id.clone(), tick_no },
                    now,
                )
                .await;
            }
            // ★P2 Stage3：背靠背自治仅 idle 放置房。event 现过载了两个正交语义——(a) 引擎 dispatch
            // （run_event_step 的地点碰撞 + 游戏时钟，由 timeline_mode 驱动，房型无关）与 (b) 调度节奏
            // （背靠背自治，由 room_type 驱动）。解耦二者：
            //   - idle：自治放置房，无 outstanding（上一 tick done 或无 tick）→ 立即排下一 tick，引擎自算时钟推进。
            //   - chapter/arena：event 房只走引擎 DES 碰撞，但**不自动排新 tick**——新 tick 全部来自手动端点
            //     （arena host_tick、chapter start / advance），保 arena「节目节奏优先于定时器」与 chapter「会话驱动」。
            //     上方 straggler 补偿 re-enqueue 仍保留（房型无关），只是不新增 tick。
            if room_type == "idle" {
                let outstanding: i64 = sqlx::query(
                    "SELECT COUNT(*) AS c FROM world_ticks WHERE world_id = ? AND status IN ('pending','running')",
                )
                .bind(&world_id)
                .fetch_one(&state.db)
                .await?
                .try_get("c")?;
                if outstanding == 0 {
                    if !active {
                        // 原路（错峰关闭）：逐字不变。
                        let _ = schedule_tick(state, &world_id).await?;
                    } else {
                        // 错峰路径：背靠背世界是**慢炖场**的实现形态，正是最该错峰的一类
                        //（它一天到晚满速烧 token，没有墙钟节奏可依）。
                        let cfg = offpeak_cfg.as_ref().expect("active 蕴含 cfg 已解析");
                        let last: Option<i64> = sqlx::query(
                            "SELECT MAX(created_at) AS m FROM world_ticks WHERE world_id = ?",
                        )
                        .bind(&world_id)
                        .fetch_one(&state.db)
                        .await?
                        .try_get("m")?;
                        match offpeak::plan_event(
                            cfg,
                            live,
                            now,
                            last,
                            offpeak::defer_tracker().held_ms(&world_id, now),
                        ) {
                            offpeak::Verdict::Idle => {}
                            offpeak::Verdict::Defer => {
                                offpeak::defer_tracker().mark(&world_id, now);
                                tracing::debug!(world_id, "错峰：原价时段，背靠背世界本轮延后");
                            }
                            offpeak::Verdict::Schedule(mut mark) => {
                                mark.defer_ms = offpeak::defer_tracker().take(&world_id, now);
                                let _ = schedule_tick_marked(state, &world_id, mark).await?;
                            }
                        }
                    }
                }
            }
            continue;
        }

        // interval 模式（默认，老世界完全走原路）：墙钟固定间隔排 tick。
        let interval = interval_override.unwrap_or_else(|| 86_400_000 / tick_per_day.max(1));

        // 补偿：只 re-enqueue 滞留过久的 pending（早于一个 interval 且至少 RECLAIM_PENDING_MIN_MS），
        // 而非每轮无条件全量 re-enqueue（后者会让长回合被多 worker 重复投递，C-1）。
        let straggler_before = now - interval.max(RECLAIM_PENDING_MIN_MS);
        let pend = sqlx::query(
            "SELECT tick_no FROM world_ticks WHERE world_id = ? AND status = 'pending' AND created_at < ?",
        )
        .bind(&world_id)
        .bind(straggler_before)
        .fetch_all(&state.db)
        .await?;
        for p in &pend {
            let tick_no: i64 = p.try_get("tick_no")?;
            queue::push_json(
                &*state.queue,
                TOPIC,
                &TickJob { world_id: world_id.clone(), tick_no },
                now,
            )
            .await;
        }

        // 到点则排新 tick。
        let last: Option<i64> =
            sqlx::query("SELECT MAX(created_at) AS m FROM world_ticks WHERE world_id = ?")
                .bind(&world_id)
                .fetch_one(&state.db)
                .await?
                .try_get("m")?;
        if !active {
            // 原路（错峰关闭）：逐字不变。
            let due = match last {
                Some(t) => now - t >= interval,
                None => true,
            };
            if due {
                let _ = schedule_tick(state, &world_id).await?;
            }
            continue;
        }

        // 错峰路径（连载场 / 慢炖场）：窗口内压缩间隔照排、窗口外延后、超兜底线无视时段照跑。
        let cfg = offpeak_cfg.as_ref().expect("active 蕴含 cfg 已解析");

        // 窗口内的间隔被压缩（默认 8h 窗口 → 3 倍速），若上一拍跑得比压缩后的间隔还慢，
        // 就会排出一拍 base_revision 已过期的 tick——它最终只会命中 C-2 的 cas_conflict 终态化，
        // 白占一个拍位而世界不推进。故错峰路径先确认没有在飞的 tick（与 event×idle 分支同口径）。
        // 这不会造成饿死：所有失败/冲突路径都会把 tick 终态化，超时 running 由本函数开头回收。
        let outstanding: i64 = sqlx::query(
            "SELECT COUNT(*) AS c FROM world_ticks WHERE world_id = ? AND status IN ('pending','running')",
        )
        .bind(&world_id)
        .fetch_one(&state.db)
        .await?
        .try_get("c")?;
        if outstanding > 0 {
            continue;
        }

        match offpeak::plan_interval(
            cfg,
            live,
            now,
            last,
            interval,
            offpeak::defer_tracker().held_ms(&world_id, now),
        ) {
            offpeak::Verdict::Idle => {}
            offpeak::Verdict::Defer => {
                offpeak::defer_tracker().mark(&world_id, now);
                tracing::debug!(world_id, interval, "错峰：原价时段，本轮延后");
            }
            offpeak::Verdict::Schedule(mut mark) => {
                mark.defer_ms = offpeak::defer_tracker().take(&world_id, now);
                let _ = schedule_tick_marked(state, &world_id, mark).await?;
            }
        }
    }
    Ok(())
}

/// `schedule_due_ticks` 一轮里用到的世界字段（先取完再排序，排序需要 `world_id`）。
struct SchedWorld {
    id: String,
    tick_per_day: i64,
    timeline_mode: String,
    room_type: String,
}

// ---------- 版本钉住解析 ----------

// rename_all=camelCase：与 ModelProfile（baseUrl/apiKey）及桌面端 agentConfigs 命名一致；
// 既有单词字段（default/decide/...）不受影响，仅新增字段以 camelCase（maxOutputTokens）出现。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoutesConfig {
    default: Option<ModelProfile>,
    decide: Option<ModelProfile>,
    arbiter: Option<ModelProfile>,
    writer: Option<ModelProfile>,
    critic: Option<ModelProfile>,
    director: Option<ModelProfile>,
    /// 单次输出上限（本回合各环节 request 的 max_tokens）。世界钉住、可按世界调；缺省 → 兜底
    /// `DEFAULT_MAX_OUTPUT_TOKENS`。`#[serde(default)]` 保证旧 routes_json（无此字段）零改动向后兼容。
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

/// 解析世界钉住的 model_route_version → (ModelRoutes, max_output_tokens)；无匹配/缺 default profile →
/// None（dev 跳过信号）。max_output_tokens 取配置值，缺省回退 `DEFAULT_MAX_OUTPUT_TOKENS`（>0 兜底）。
///
/// `pub(crate)`：模型路由是**运营配置的唯一解析口**（含"dev 无配置则跳过"这条 fail-closed 语义）。
/// 本进程内其它调用引擎的路径必须走同一个解析器，否则会出现"运营把某版本路由停用了、
/// 另一条路径还在照跑"的配置漂移。
pub(crate) async fn resolve_model_routes(
    db: &AnyPool,
    version: &str,
) -> Result<Option<(ModelRoutes, u32)>, ApiError> {
    let Some(row) = sqlx::query("SELECT routes_json FROM model_routes WHERE version = ? LIMIT 1")
        .bind(version)
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    let routes_json: String = row.try_get("routes_json")?;
    let Ok(cfg) = serde_json::from_str::<RoutesConfig>(&routes_json) else {
        return Ok(None);
    };
    let Some(default) = cfg.default else {
        return Ok(None);
    };
    let max_output_tokens =
        cfg.max_output_tokens.filter(|v| *v > 0).unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    Ok(Some((
        ModelRoutes {
            default,
            decide: cfg.decide,
            arbiter: cfg.arbiter,
            writer: cfg.writer,
            critic: cfg.critic,
            director: cfg.director,
        },
        max_output_tokens,
    )))
}

/// 解析世界钉住的 prompt_set_version（按 version 聚合各 scope 行）→ NarrativePrompts。
///
/// `pub(crate)`：理由同 `resolve_model_routes`——提示词版本是运营可调的产品参数，
/// 必须只有一个解析入口。
pub(crate) async fn resolve_prompts(db: &AnyPool, version: &str) -> Result<NarrativePrompts, ApiError> {
    let rows = sqlx::query("SELECT scope, content FROM prompt_versions WHERE version = ?")
        .bind(version)
        .fetch_all(db)
        .await?;
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for r in &rows {
        map.insert(r.try_get("scope")?, r.try_get("content")?);
    }
    let take = |m: &mut BTreeMap<String, String>, k: &str| m.remove(k).unwrap_or_default();
    Ok(NarrativePrompts {
        director_system: take(&mut map, "director"),
        decide_system: take(&mut map, "decide"),
        arbiter_system: take(&mut map, "arbiter"),
        writer_system: take(&mut map, "writer"),
        critic_system: take(&mut map, "critic"),
        prompt_version: version.to_string(),
    })
}

// ---------- E-1：种子 / 回灌（DB narrative_state_json ↔ 引擎 FS 单一事实源） ----------

/// 引擎 FS 中该世界叙事状态文件的相对路径（run_id = world_id，稳定到 world 粒度）。
fn engine_state_path(run_id: &str) -> std::path::PathBuf {
    muse_engine::narrative::state::state_path(run_id)
}

/// 构造本 tick 用于回灌到引擎 FS 的权威 NarrativeState：
/// - DB narrative_state_json 可解析（tick>0 已是完整状态）→ 以 DB 为准，对齐 revision/run_id、补齐在场角色；
/// - 否则（首 tick，"{}"）→ 从 assembled_json/skeleton 冷启动：在场角色空态 + 硬节点 + 禁止谓词。
async fn build_seed_state(
    db: &AnyPool,
    world: &WorldRow,
    member_ids: &[String],
    base_revision: i64,
) -> Result<NarrativeState, ApiError> {
    if let Ok(mut s) = serde_json::from_str::<NarrativeState>(&world.narrative_state_json) {
        // DB 权威：回灌前把 run_id 固定到 world、revision 对齐 base_revision（CAS 前提），补齐新加入成员。
        s.run_id = world.id.clone();
        s.revision = base_revision as u64;
        for id in member_ids {
            s.characters.entry(id.clone()).or_default();
        }
        return Ok(s);
    }

    // 首 tick 冷启动种子。
    let mut s = NarrativeState {
        schema_version: 1,
        run_id: world.id.clone(),
        revision: base_revision as u64,
        ..Default::default()
    };
    for id in member_ids {
        s.characters.insert(id.clone(), CharacterState::default());
    }
    seed_narrative_layer(db, world, &mut s).await?;
    Ok(s)
}

/// 从世界模板 skeleton_json + 实例 assembled_json 种入叙事层（硬节点 + 禁止谓词）。
/// 全程防御式解析：字段缺失/格式不符 → 退化为空叙事层（引擎照常运行，不 panic、不 fail-closed）。
async fn seed_narrative_layer(
    db: &AnyPool,
    world: &WorldRow,
    s: &mut NarrativeState,
) -> Result<(), ApiError> {
    // assembled_json 标注的宿命(硬)节点 id（装配层 home_advantages.fatedNodes）+ 采样钉住的被选主线子集。
    let mut fated: std::collections::BTreeSet<String> = Default::default();
    // 采样子集（防刷第二环）：仅对被选主线节点建 outline；缺失（退化/旧实例）→ None → 全量。
    let mut selected_mainline: Option<std::collections::BTreeSet<String>> = None;
    if let Some(raw) = &world.assembled_json {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            if let Some(arr) = v.pointer("/assembly/homeAdvantages").and_then(Value::as_array) {
                for ha in arr {
                    if let Some(nodes) = ha.get("fatedNodes").and_then(Value::as_array) {
                        for n in nodes {
                            if let Some(id) = n.as_str() {
                                fated.insert(id.to_string());
                            }
                        }
                    }
                }
            }
            if let Some(arr) = v.pointer("/assembly/sampling/selectedMainline").and_then(Value::as_array) {
                let set: std::collections::BTreeSet<String> =
                    arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
                if !set.is_empty() {
                    selected_mainline = Some(set);
                }
            }
        }
    }

    // 模板骨架（预审核内容池）：mainlineNodes → 大纲节点；forbiddenPredicates → 禁止谓词。
    let Some(row) = sqlx::query("SELECT skeleton_json FROM world_templates WHERE id = ?")
        .bind(&world.template_id)
        .fetch_optional(db)
        .await?
    else {
        return Ok(());
    };
    let raw: String = row.try_get("skeleton_json")?;
    let Ok(sk) = serde_json::from_str::<Value>(&raw) else {
        return Ok(());
    };

    if let Some(nodes) = sk.get("mainlineNodes").and_then(Value::as_array) {
        for node in nodes {
            let Some(id) = node.get("id").and_then(Value::as_str) else {
                continue;
            };
            // 采样钉住时仅对被选主线节点建 outline（通关判定与大纲节点数须按被选主线，见装配采样第二环）。
            if let Some(sel) = &selected_mainline {
                if !sel.contains(id) {
                    continue;
                }
            }
            let summary = node.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
            let is_fated = node.get("fated").and_then(Value::as_bool).unwrap_or(false) || fated.contains(id);
            let constraint = match node.get("constraint").and_then(Value::as_str) {
                Some("hard") => ConstraintLevel::Hard,
                Some("soft") => ConstraintLevel::Soft,
                Some("free") => ConstraintLevel::Free,
                _ if is_fated => ConstraintLevel::Hard,
                _ => ConstraintLevel::Soft,
            };
            // P1 放置房终局：里程碑阈值 + 关系谓词门（弱类型 raw 读，缺失即老式节点走兼容路径）。
            let threshold = node.get("threshold").and_then(Value::as_f64);
            // 里程碑进度键为 world.milestoneProgress_<id>（单段键，reducer 要求无 . / [）；非法 id 的里程碑
            // 跳过并 warn（进度键非法会致 Increment 落库失败）。非里程碑（threshold=None）不受此约束。
            if threshold.is_some() && (id.contains('.') || id.contains('[')) {
                tracing::warn!(node_id = id, "里程碑节点 id 含非法字符（. 或 [），跳过该节点（进度键非法）");
                continue;
            }
            // advanceWhen 谓词：语法非法则丢弃谓词、保留节点为纯阈值门（不 fail-closed）。
            let advance_when = node
                .get("advanceWhen")
                .and_then(Value::as_str)
                .filter(|expr| muse_engine::narrative::constraints::parse_predicate(expr).is_ok())
                .map(|s| s.to_string());
            s.narrative.outline_nodes.push(OutlineNode {
                id: id.to_string(),
                summary,
                constraint,
                status: NodeStatus::Pending,
                threshold,
                advance_when,
                weights: None,
            });
        }
    }

    if let Some(preds) = sk.get("forbiddenPredicates").and_then(Value::as_array) {
        for p in preds {
            let (Some(id), Some(expr)) =
                (p.get("id").and_then(Value::as_str), p.get("expression").and_then(Value::as_str))
            else {
                continue;
            };
            // 仅种入语法合法的谓词，避免 eval 阶段 Validation 失败（受限 DSL，见 constraints）。
            if muse_engine::narrative::constraints::parse_predicate(expr).is_err() {
                continue;
            }
            let reason = p.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
            s.narrative.forbidden_predicates.push(ForbiddenPredicate {
                id: id.to_string(),
                expression: expr.to_string(),
                reason,
            });
        }
    }
    Ok(())
}

// ---------- Phase 3：道具事实源单一化（backpack / NPC 携带 → CharacterState.resources） ----------

/// 从 ItemDefinition JSON 列表提取持有事实：`item:<id>` + 每个 effectTag 的 `tag:<t>`（去重，保留出现序）。
/// 引擎 R6b（arbiter::gate_admits）据此判秘境准入的 item/tag 持有闸。
fn item_facts_from_defs(defs: &[Value]) -> Vec<String> {
    let mut facts: Vec<String> = Vec::new();
    for d in defs {
        let Some(id) = d.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        else {
            continue;
        };
        let f = format!("item:{id}");
        if !facts.contains(&f) {
            facts.push(f);
        }
        if let Some(tags) = d.get("effectTags").and_then(Value::as_array) {
            for t in tags.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                let tf = format!("tag:{t}");
                if !facts.contains(&tf) {
                    facts.push(tf);
                }
            }
        }
    }
    facts
}

/// 幂等把持有事实刷进 resources：清除既有 `item:`/`tag:` 派生项（丢弃上 tick 物化），再按当前事实重灌。
/// 自由文本 resources（R1 资源消耗判定用）保留——只重写道具派生项，避免跨 tick 累积、且道具增减实时反映。
fn materialize_item_facts(resources: &mut Vec<String>, facts: &[String]) {
    resources.retain(|r| !r.starts_with("item:") && !r.starts_with("tag:"));
    for f in facts {
        if !resources.contains(f) {
            resources.push(f.clone());
        }
    }
}

/// 玩家 backpack 物化：读本世界 `carried` 状态的物品 → 按角色 id 聚合持有事实（`item:<id>`/`tag:<t>`）。
/// 事实源单一化：物品事实只来自 backpacks（§9.6 服务端权威写入路径），不信任 CharacterState 历史残留。
/// effectTags 优先取 carry 落库的降档覆盖（S-5 转译入场覆盖列），否则取 items 定义。
async fn load_carried_item_facts(
    db: &AnyPool,
    world_id: &str,
) -> Result<BTreeMap<String, Vec<String>>, ApiError> {
    let rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, i.id AS item_id, \
                COALESCE(b.effect_tags_override, i.effect_tags) AS effect_tags \
         FROM world_members wm \
         JOIN backpacks b ON b.user_id = wm.user_id AND b.carried_world_id = wm.world_id \
         JOIN items i ON i.id = b.item_id \
         WHERE wm.world_id = ? AND wm.status = 'active' AND b.status = 'carried' \
         ORDER BY wm.cloud_character_id, i.id",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in &rows {
        let cid: String = r.try_get("cid")?;
        let item_id: String = r.try_get("item_id")?;
        let effect_tags: String = r.try_get("effect_tags")?;
        let entry = map.entry(cid).or_default();
        let f = format!("item:{item_id}");
        if !entry.contains(&f) {
            entry.push(f);
        }
        if let Ok(tags) = serde_json::from_str::<Vec<String>>(&effect_tags) {
            for t in tags.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let tf = format!("tag:{t}");
                if !entry.contains(&tf) {
                    entry.push(tf);
                }
            }
        }
    }
    Ok(map)
}

// ---------- 身份池叙事接线（总规格 §5【拍板 4、5】：身份 = 开局站位） ----------
//
// 🔴 **平权宪法（总规格 §0.1 真红线 1，锁进测试）**：身份只是「这个角色在这个世界的开局站位」
//（户部主事 / 漕帮商贾 / 被退婚的嫡女），**不携带任何数值差异、准入门槛、产出加成、难度优待
// 或叙事特权**。本节读回的分配结果**只**流向引擎的两条**感知**通道，别无他处：
//   ① `RoundInput.other_cards_brief`——「他人如何被本角色感知」的第三人称一句话摘要（`唐三（户部主事）`）；
//   ② `RoundInput.self_identities`——「本角色自己的开局站位」（引擎 decide 组装上下文时恒剔除自己，
//      少了这条角色本人反而看不见自己的站位）。两条同源同值，故不可能出现「别人叫你户部主事、
//      你自己却不知道」的错位。
// **绝不进入 `active_cards`**（角色卡不可变快照，污染它等于篡改玩家的卡）、绝不占用 `whispers`
// （托梦有配额且会被标 applied，语义完全不同），也绝不得被任何下游用来改判定 / 改发奖 / 开权限 /
// 调难度。戏份靠玩出来。
//
// 退化契约：老世界 / 未装配 / 模板未声明 `identityPool` / JSON 结构损坏 → 一律静默退化为
// 「brief 只放名字 + 上下文里根本没有身份字段」的现状，逐字段零行为变化；不 panic、不阻断 tick
// （同本文件 `seed_narrative_layer` 的防御式解析口径）。
//
// 确定性：分配结果由装配层按 cid 升序钉死在 `assembled_json`，本节纯读、纯拼接，无随机、
// 不依赖 map 迭代序产生分支 —— 同一 (world_id, 阵容, template_version) 恒得同一份身份呈现。

/// 从实例 `assembled_json` 读回身份分配 `[[cid, identityId], …]`（`/assembly/identityAssignments`）。
/// 任一层结构不符（无 assembled / 非 JSON / 无该字段 / 非数组）→ 空 Vec = 完全退化；
/// 单个条目损坏只跳过该条，不牵连其余（防御式，同 `worldCharacterEntries` 口径）。
fn parse_identity_assignments(assembled_json: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = assembled_json else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(arr) = v.pointer("/assembly/identityAssignments").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for pair in arr {
        let Some(p) = pair.as_array() else {
            continue; // 条目不是 [cid, identityId] 二元组：跳过。
        };
        let (Some(cid), Some(identity_id)) = (
            p.first().and_then(Value::as_str).map(str::trim),
            p.get(1).and_then(Value::as_str).map(str::trim),
        ) else {
            continue;
        };
        if cid.is_empty() || identity_id.is_empty() {
            continue;
        }
        out.push((cid.to_string(), identity_id.to_string()));
    }
    out
}

/// 从模板 skeleton 的 `identityPool[]` 取叙事展示名表：`identityId → 展示名`。
/// **展示名优先 `label`，`label` 缺失/为空 → 回落 `id`**（与 `docs/build/example-idle-skeleton.md`
/// 字段说明一致）。模板未声明 `identityPool` / 结构不符 → 空表 → 上层完全退化。
fn parse_identity_labels(skeleton_json: &str) -> BTreeMap<String, String> {
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let Ok(sk) = serde_json::from_str::<Value>(skeleton_json) else {
        return labels;
    };
    let Some(arr) = sk.get("identityPool").and_then(Value::as_array) else {
        return labels;
    };
    for spec in arr {
        let Some(id) =
            spec.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        else {
            continue; // 无 id 的身份不可分配（建模板期已被校验拦截），此处防御式跳过。
        };
        let label = spec.get("label").and_then(Value::as_str).map(str::trim).unwrap_or("");
        labels.insert(id.to_string(), if label.is_empty() { id } else { label }.to_string());
    }
    labels
}

/// 组装本世界的 `cid → 身份展示名`（叙事感知层唯一出口）。
///
/// 「只在真有分配时才回查模板」：老世界 / 老模板的 `identityAssignments` 为空（装配层
/// `skip_serializing_if` 保证字段根本不写出）→ 直接返回空表，**零额外查询、零行为变化**。
/// DB 读失败 / 列类型不符 / 骨架非 JSON → 一律静默退化为空表：身份是叙事装饰，
/// 绝不因它 fail-closed 或阻断 tick。
async fn load_identity_display_names(db: &AnyPool, world: &WorldRow) -> BTreeMap<String, String> {
    let assignments = parse_identity_assignments(world.assembled_json.as_deref());
    if assignments.is_empty() {
        return BTreeMap::new();
    }
    let Ok(Some(row)) = sqlx::query("SELECT skeleton_json FROM world_templates WHERE id = ?")
        .bind(&world.template_id)
        .fetch_optional(db)
        .await
    else {
        return BTreeMap::new();
    };
    let Ok(raw) = row.try_get::<String, _>("skeleton_json") else {
        return BTreeMap::new();
    };
    let labels = parse_identity_labels(&raw);
    if labels.is_empty() {
        return BTreeMap::new(); // 模板已不再声明 identityPool → 完全退化。
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (cid, identity_id) in assignments {
        // 池中查不到该身份 id（模板版本被改过）→ 该角色不加身份，退化为只放名字。
        if let Some(display) = labels.get(&identity_id) {
            out.insert(cid, display.clone());
        }
    }
    out
}

/// 感知层展示名拼接：`唐三（户部主事）`。
/// 无身份 / 展示名为空 → 原样只放名字（退化路径，与接线前逐字节一致）；
/// 卡名为空（异常卡）→ 只放身份，避免产出孤零零的括号。
fn brief_with_identity(name: &str, display: Option<&String>) -> String {
    match display.map(String::as_str).map(str::trim).filter(|d| !d.is_empty()) {
        None => name.to_string(),
        Some(d) if name.trim().is_empty() => d.to_string(),
        Some(d) => format!("{name}（{d}）"),
    }
}

// ---------- tick 收尾工具 ----------

/// **空转拍**收尾：置 done + note，成本恒 0。
///
/// 🔴 恒 0 是**语义**不是省事，本函数只服务「一次模型都没调过」的拍。判据只有一条：
/// 调用点是否在 `engine.run_round` / `run_event_step` **真正跑出模型调用之前**返回。
/// 逐一对照（改动此表前请先确认调用点仍满足该判据）：
///
/// | note | 调用点 | 为什么 0 是对的 |
/// |---|---|---|
/// | `world_not_running` | 认领后第 3 步 | 世界不 running，直接返回，未构建引擎 |
/// | `superseded` | 第 3 步陈旧门 | base_revision 不匹配即返回，未构建引擎 |
/// | `budget_fused` | 第 4 步预算预检 / `fuse_and_pause` | 熔断在跑之前，未构建引擎 |
/// | `no_model_config` | 第 5 步 | 连模型路由都没解析出来，不可能有调用 |
/// | `insufficient_members` | 第 6 步门槛 | 成员不足即返回，未构建引擎 |
/// | `terminal` | `run_event_step` 返回 `outcome: None` | 见下 |
///
/// `terminal` 这一条是唯一需要读引擎才能确认的：`run_event_step`（`narrative/mod.rs`）里
/// `outcome: None` 只有两个出口——① 开头 `is_terminal(&state)` 命中；② `select_cohort` 空
/// （`Starved`）。两者都在调用 `run_round` **之前** `return`，而 `is_terminal` / `select_cohort`
/// 都是对 `NarrativeState` 的纯读函数，不碰 `host.model`。故这一拍确实一个 token 都没烧。
///
/// **反例在下面**：`blocked` 拍走 `finish_tick_blocked`，因为引擎已经跑完整个回合。
/// 把两者混在同一个「noop」里，正是 #42 的成因。
async fn finish_tick_noop(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let now = now_ms();
    sqlx::query(
        "UPDATE world_ticks SET status='done', cost_tokens=0, error=?, \
         started_at=COALESCE(started_at, ?), finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(note)
    .bind(now)
    .bind(now)
    .bind(world_id)
    .bind(tick_no)
    .execute(db)
    .await?;
    Ok(())
}

/// **阻断拍**收尾：置 `done` + `error='blocked'`，并记**真实实测 token** + 累计进日预算。
///
/// 与 `finish_tick_noop` 的差别只有一处，但它是本函数存在的全部理由：**这一拍引擎跑完了**。
/// 被内容安全闸 / 硬节点 / 不变量拦下的是**提交**，不是**计算**——导演、逐角色决策、仲裁、
/// 可能的底线重生成全都已经发出去了，账单已经产生。把它记 0 会让成本**结构性**偏低：
/// 越是被阻断多的世界（往往正是内容风险高、审核成本也高的那些）低估得越厉害，
/// 于是 `docs/VALIDATION.md` 的 T3（ARPPU ≥ 3× 单用户月度模型成本）与 T5（审核成本 ≤ 生成
/// 成本 5%、毛利为正）会**在最危险的地方最乐观**。
///
/// 口径与 `commit_tick` 逐字一致（同一个 `TokenMeter` 汇总 input+output，模型未回报 token 时
/// 同样回退引擎预估），也与单人平行线（if 线）里那条 `commit_blocked` 一致——
/// 三条跑引擎的路径记同一种成本，成本看板才可比。事实上平行线那边一开始就是对的，
/// 是世界线这边落后了：`TokenMeter` 的 `pub(crate)` 注释要求的正是「成本口径全平台唯一」。
///
/// 两条 UPDATE 同事务：`world_ticks.cost_tokens`（看板/SLO 读）与
/// `world_budgets.spent_tokens_today`（熔断读）绝不允许一个记上、另一个没记。
/// 顺带修好熔断：在此之前，一个陷入连续 blocked 的世界可以无限烧 token 而日预算永远不涨，
/// B-2 熔断器对它完全失效。
async fn finish_tick_blocked(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    cost_tokens: u64,
) -> Result<(), ApiError> {
    let now = now_ms();
    let cost = cost_tokens as i64;
    let mut tx = db.begin().await?;
    sqlx::query(
        "UPDATE world_ticks SET status='done', cost_tokens=?, error='blocked', \
         started_at=COALESCE(started_at, ?), finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(cost)
    .bind(now)
    .bind(now)
    .bind(world_id)
    .bind(tick_no)
    .execute(&mut *tx)
    .await?;
    // 预算累计：与 commit_tick 的那条 UPDATE 逐字同形（含 budget_day 刷新），保证熔断口径唯一。
    sqlx::query(
        "UPDATE world_budgets SET spent_tokens_today = spent_tokens_today + ?, budget_day=?, updated_at=? WHERE world_id=?",
    )
    .bind(cost)
    .bind(day_string(now))
    .bind(now)
    .bind(world_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// C-2：CAS 冲突终态化（标 done + error=cas_conflict）。冲突是良性「已被更早的 tick 推进」，非失败，不暂停世界，
/// 也不再留 pending 无限 re-enqueue/重跑。
async fn finalize_cas_conflict(db: &AnyPool, world_id: &str, tick_no: i64) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE world_ticks SET status='done', error='cas_conflict', finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(now_ms())
    .bind(world_id)
    .bind(tick_no)
    .execute(db)
    .await?;
    Ok(())
}

async fn pause_world(db: &AnyPool, world_id: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE worlds SET status='paused', updated_at=? WHERE id=? AND status='running'")
        .bind(now_ms())
        .bind(world_id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------- 放置房终局（P1 Phase 0：server 终局停机 + 防秒结束，引擎零改，复用 P2 Terminal） ----------

/// 放置房终局策略（P1 Phase 0）。**严格门 `room_type=='idle'`**：非 idle 房 `enabled=false`，终局逻辑全跳过
/// （chapter/arena 既有收敛旁路零影响）。策略来源：worlds 行的 `room_type` + 模板 skeleton 的 `endgame` 对象。
#[derive(Debug, Clone)]
pub struct RoomEndgamePolicy {
    /// 仅 `room_type=='idle'` 为 true；否则整个终局评估短路跳过。
    pub enabled: bool,
    /// 终局地板（默认 3）：任何终局在 `tick_no < min_world_ticks` 前一律不触发——第二道防秒结束守卫。
    pub min_world_ticks: i64,
    /// 世界时间上限（回退口径：`world_ticks.tick_no` 计数）。`tick_no >= max_world_ticks` 即时间上限终局，
    /// **兜底保证任意 idle 房必终止**（防无限跑）。
    pub max_world_ticks: i64,
    /// 与 P2 世界时钟集成点：`Some` 时另按游戏时间 `worlds.game_time >= world_time_limit` 判终局（可能早于
    /// tick 上限）；`None` 时仅回退 `max_world_ticks`。block-2 世界时钟就绪后仅换比较量，不动结构。
    pub world_time_limit: Option<i64>,
    /// 关键角色 id（cloud_character_id）集合（P1 Phase 3，终局条件(3)）：其永久退场——成员表 left/retired，
    /// 或已 landed（approved）的 permanent_exit consent——触发终局。空 = 无关键角色，条件(3)恒不触发。
    pub key_character_ids: Vec<String>,
}

/// 读取放置房终局策略：`room_type`（worlds 行）+ skeleton `endgame` 对象（弱类型 raw 读，缺失即默认）。
/// 防御式解析：字段缺失/类型不符 → 退化为默认值（不 panic、不 fail-closed）。
async fn load_endgame_policy(db: &AnyPool, world: &WorldRow) -> Result<RoomEndgamePolicy, ApiError> {
    let enabled = world.room_type == "idle";
    let mut min_world_ticks = DEFAULT_MIN_WORLD_TICKS;
    let mut max_world_ticks = DEFAULT_MAX_WORLD_TICKS;
    let mut world_time_limit: Option<i64> = None;
    let mut key_character_ids: Vec<String> = Vec::new();

    if let Some(row) = sqlx::query("SELECT skeleton_json FROM world_templates WHERE id = ?")
        .bind(&world.template_id)
        .fetch_optional(db)
        .await?
    {
        let raw: String = row.try_get("skeleton_json")?;
        if let Ok(sk) = serde_json::from_str::<Value>(&raw) {
            if let Some(eg) = sk.get("endgame") {
                if let Some(v) = eg.get("minWorldTicks").and_then(Value::as_i64) {
                    min_world_ticks = v.max(0);
                }
                if let Some(v) = eg.get("maxWorldTicks").and_then(Value::as_i64) {
                    max_world_ticks = v.max(1);
                }
                // worldTimeLimit=null 或缺失 → None（回退 tick 计数，即 block-2 世界时钟未就绪的默认）。
                world_time_limit = eg.get("worldTimeLimit").and_then(Value::as_i64).filter(|v| *v > 0);
                // keyCharacterIds（P1 Phase 3）：防御式解析，非数组/非字符串项静默丢弃。
                if let Some(arr) = eg.get("keyCharacterIds").and_then(Value::as_array) {
                    key_character_ids = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .filter(|s| !s.trim().is_empty())
                        .collect();
                }
            }
        }
    }
    Ok(RoomEndgamePolicy {
        enabled,
        min_world_ticks,
        max_world_ticks,
        world_time_limit,
        key_character_ids,
    })
}

/// 世界时间上限判定（P1 Phase 0）。
/// - `tick_no >= max_world_ticks`：tick 计数硬上限，**任意 idle 房必在此终止**（无限跑的最终兜底）。
/// - 另有 `world_time_limit`（P2 世界时钟）就绪时，`game_time >= limit` 亦终局（可能早于 tick 上限）。
fn reached_time_limit(policy: &RoomEndgamePolicy, game_time: i64, tick_no: i64) -> bool {
    if tick_no >= policy.max_world_ticks {
        return true;
    }
    if let Some(limit) = policy.world_time_limit {
        return game_time >= limit;
    }
    false
}

/// 引擎终局信号 → 终局原因串（审计/日志/world_ticks.error 用）。
fn terminal_reason(t: &Terminal) -> &'static str {
    match t {
        Terminal::MainlineDone { .. } => "mainline_complete",
        Terminal::TimeCapReached => "time_cap",
        Terminal::Starved => "starved",
    }
}

/// 终局条件(3) 关键角色退场（P1 Phase 3）：`policy.key_character_ids` 中任一角色永久退场即终局。
/// 「永久退场」两个判据（任一命中即真）：
/// - **成员表**：`world_members.status IN ('left','retired')` 命中关键角色（cloud_character_id）。
/// - **已 landed 的 permanent_exit consent**：`consent_requests` 中 `status='approved'` 且
///   `event_kind='permanent_exit'` 的 subject 命中关键角色（引擎在获批后才落定该不可逆退场）。
///
/// 空 `key_character_ids` → 恒 `false`（零成本短路，非 idle / 未配置关键角色的世界零影响）。
/// **读外部已提交事实**（成员退场/同意由其它已提交事务写入），调用方须在开启终局事务前调用，
/// 避免单连接测试池「事务持连接时再取连接」死锁。
async fn key_character_exited(
    db: &AnyPool,
    world_id: &str,
    policy: &RoomEndgamePolicy,
) -> Result<bool, ApiError> {
    if policy.key_character_ids.is_empty() {
        return Ok(false);
    }
    let key: std::collections::BTreeSet<&str> =
        policy.key_character_ids.iter().map(String::as_str).collect();

    // (a) 成员表永久退场：left/retired。
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT cloud_character_id FROM world_members \
         WHERE world_id = ? AND status IN ('left', 'retired')",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    if members.iter().any(|(c,)| key.contains(c.as_str())) {
        return Ok(true);
    }

    // (b) 已 landed 的 permanent_exit consent（approved）：subject 命中关键角色。
    let consents = sqlx::query(
        "SELECT subject_character_ids FROM consent_requests \
         WHERE world_id = ? AND status = 'approved' AND event_kind = 'permanent_exit'",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    for r in &consents {
        let raw: String = r.try_get("subject_character_ids")?;
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&raw) {
            if list.iter().any(|c| key.contains(c.as_str())) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// 读回装配层**定盘**的终局结局（P1 Phase 3）。**确定性**：纯读实例钉住的 assembled_json，不发模型、
/// 不掷点——掷点发生在装配层（`assembly::pick_ending`，`Rng(instance_seed ^ DOMAIN_ENDING)` 子流按权重抽），
/// 结果随实例一次性钉死在 `/assembly/selectedEnding`。同模板同阵容的不同实例 world_id 不同 ⇒ 实例种子不同
/// ⇒ 落到的结局不同（总规格 §5「一个模板，千个平行世界」）。
///
/// 回退口径（`selectedEnding` 缺失时取 `enabledEndings` 首个）只服务两类 assembled_json：
/// ① 本次修复**之前**已钉住的老实例——它们的权重在运行期已不可复原，且**已钉住的实例不因一次代码
///    变更改写结局**；② 测试/边角路径手写的最小包装。二者都不该走掷点。
///
/// assembled_json 缺失 / 两个键都无 / 空池 → `None`（世界仍可停机，仅无结局产出与荣誉奖励）。
fn select_ending(world: &WorldRow) -> Option<String> {
    let raw = world.assembled_json.as_ref()?;
    let v: Value = serde_json::from_str(raw).ok()?;
    let pinned = v
        .pointer("/assembly/selectedEnding")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(id) = pinned {
        return Some(id.to_string());
    }
    v.pointer("/assembly/enabledEndings")
        .and_then(Value::as_array)
        .and_then(|arr| arr.iter().find_map(|e| e.as_str().map(str::to_string)))
}

/// 终局产出（P1 Phase 3）：在**调用方终局事务内**，`end_world` 真正结算（rows>0）后写终局审计留痕 +
/// 颁发终局荣誉奖励（复用 arena_rewards 荣誉旁路）。仅在真正结算的那一次 tick 调用一次。
///
/// **arena 红线（§2.5）**：奖励只入 `arena_rewards`（称号/荣誉，schema 无任何强度/属性字段）——
/// **荣誉非战力、无买判定**（不经任何计费/订单路径、不写强度）。幂等：唯一索引 (world,character,kind)
/// + `ON CONFLICT DO NOTHING`，重复/并发不重复发放。仅当选出结局（`ending=Some`）时发奖。
///
/// R1 三层结算（总规格 §9）：`participants` 传 `(cloud_character_id, user_id)`——③ 世界线层要给
/// **卡的主人**发产出道具，只有角色 id 不够。① 保底层 + ③ 世界线层的发放逻辑整体收在
/// `progression::settle_idle_world_ending_tx`（本文件不出现任何成长数值字段，守红线 grep 断言）；
/// 崩塌判定只把 `reason` 串交给 progression 判，系数与产出表都在那边。
async fn finalize_ending_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    reason: &str,
    ending: Option<&str>,
    participants: &[(String, String)],
) -> Result<(), ApiError> {
    // 终局审计留痕（reason + 选定结局）。
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES (?, 'system', 'system', 'world.ended', ?, ?, ?)",
    )
    .bind(new_id("aud"))
    .bind(world_id)
    .bind(format!("{reason}|ending={}", ending.unwrap_or("none")))
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;

    // 三层结算（波次 2 + R1）：① 保底层（每张在场卡的出席产出）+ ③ 世界线层（里程碑推动者按贡献分
    // 查公示产出表确定发放，无随机数），与终局停机同事务（只在真正结算那一次；participants 仅含
    // 玩家成员卡，NPC 不在列）。世界线崩塌（关键角色退场等 reason）→ ③ 归零 + ① 减半 + ② 已锁定保留，
    // 系数与判定全在 progression。发放逻辑收在 progression 模块——本文件（RoundInput 组装处）
    // 不引用任何成长数值字段，红线「成长数值不进引擎决策」grep 级可验。
    let collapsed = crate::progression::is_collapse_reason(reason);
    crate::progression::settle_idle_world_ending_tx(tx, world_id, participants, collapsed).await?;

    // 终局荣誉奖励：每位在场成员角色获一枚「结局」荣誉（label=选定结局 id）。无强度、无购买。
    if let Some(ending_id) = ending {
        for (cid, _) in participants {
            sqlx::query(
                "INSERT INTO arena_rewards (id, world_id, character_id, kind, label, season, created_at) \
                 VALUES (?, ?, ?, 'ending', ?, NULL, ?) \
                 ON CONFLICT(world_id, character_id, kind) DO NOTHING",
            )
            .bind(new_id("rw"))
            .bind(world_id)
            .bind(cid)
            .bind(ending_id)
            .bind(now_ms())
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

/// 终局停机（P1 Phase 0）：在调用方事务内置 `worlds.status='ended'`。
/// **幂等**：`WHERE status='running'` → 非 running（已被更早的 tick 结算/暂停/已 ended）则 `rows=0`，**只结算一次**。
/// 与状态 CAS 同事务调用，保证「状态提交」与「终局停机」原子（不裂）。返回受影响行数（>0 = 本次真正结算）。
async fn end_world_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    reason: &str,
) -> Result<u64, ApiError> {
    let res = sqlx::query("UPDATE worlds SET status='ended', updated_at=? WHERE id=? AND status='running'")
        .bind(now_ms())
        .bind(world_id)
        .execute(&mut **tx)
        .await?;
    let rows = res.rows_affected();
    if rows > 0 {
        tracing::info!(world_id, reason, "world 终局停机（status=ended），schedule_due_ticks 将自动停排");
    }
    Ok(rows)
}

/// 终局停机（无回合可提交时）：用于两条无回合路径——(a) `run_event_step` 起始即判引擎终局
/// （`step.outcome==None`，reason=mainline_complete/time_cap/starved）；(b) P1 Phase 3 早期关键角色退场
/// （reason=key_character_exit，先于回合运行即停机）。单独事务内「tick 收尾(done) + end_world +
/// 终局产出(finalize_ending_tx)」原子结算，提交后补生成终局日报（复用 reports::generate_report，
/// 幂等 per world+character+day）。返回 Concluded（真正结算）或 Skipped("terminal")（已被并发结算，
/// rows=0，幂等）。
///
/// `cost_tokens=0` 在这里是**对的**（判据同 `finish_tick_noop`）：两条路径都在模型调用之前返回——
/// (a) `run_event_step` 的 `outcome==None` 出口在 `run_round` 之前 `return`；
/// (b) 关键角色退场判定（6a.1）排在引擎构建之前，读的是 DB 里已提交的事实。
/// 与之相对，`blocked` 拍已经跑完整个回合，走 `finish_tick_blocked` 记真实成本（#42）。
async fn conclude_world_no_round(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    reason: &str,
    ending: Option<&str>,
    members: &[ProjectionMember],
) -> Result<TickStatus, ApiError> {
    let now = now_ms();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE world_ticks SET status='done', cost_tokens=0, error=?, \
         started_at=COALESCE(started_at, ?), finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(reason)
    .bind(now)
    .bind(now)
    .bind(world_id)
    .bind(tick_no)
    .execute(&mut *tx)
    .await?;
    let rows = end_world_tx(&mut tx, world_id, reason).await?;
    let concluded = rows > 0;
    if concluded {
        // (cid, user_id)：③ 世界线层产出发给卡的主人，故 user_id 必须一并带上。
        let participants: Vec<(String, String)> =
            members.iter().map(|m| (m.character_key.clone(), m.user_id.clone())).collect();
        finalize_ending_tx(&mut tx, world_id, reason, ending, &participants).await?;
    }
    tx.commit().await?;
    if concluded {
        // 终局日报（提交后，非事务关键路径，best-effort；幂等 per world+character+day）。
        let today = day_string(now);
        for m in members {
            let _ = crate::reports::generate_report(state, world_id, &m.user_id, &m.character_key, &today)
                .await;
        }
        Ok(TickStatus::Concluded)
    } else {
        Ok(TickStatus::Skipped("terminal"))
    }
}

/// 预算熔断收尾：置 fused、暂停世界、tick 终态。
async fn fuse_and_pause(
    db: &AnyPool,
    world_id: &str,
    tick_no: i64,
    today: &str,
    spent: i64,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE world_budgets SET fused=1, budget_day=?, spent_tokens_today=?, updated_at=? WHERE world_id=?")
        .bind(today)
        .bind(spent)
        .bind(now_ms())
        .bind(world_id)
        .execute(db)
        .await?;
    pause_world(db, world_id).await?;
    finish_tick_noop(db, world_id, tick_no, Some("budget_fused")).await?;
    Ok(())
}

async fn mark_tick_failed_and_pause(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    note: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE world_ticks SET status='failed', error=?, finished_at=? WHERE world_id=? AND tick_no=?",
    )
    .bind(note)
    .bind(now_ms())
    .bind(world_id)
    .bind(tick_no)
    .execute(&state.db)
    .await?;
    pause_world(&state.db, world_id).await?;
    Ok(())
}

// ---------- #3b 不可逆同意：消费 ConsentRequested + 已获批 subject 回灌（规格 §2.4 / REMEDIATION #3） ----------

/// 回灌：本世界已获批(approved)的不可逆同意 → subject 角色 id 列表。
/// 喂入引擎 `RoundInput.approved_consents`，本回合命中的 subject 可落定其不可逆结果
/// （death/permanent_exit/permanent_relation_change）；未命中一律门控不落定（保守安全默认）。
async fn load_approved_consent_subjects(db: &AnyPool, world_id: &str) -> Result<Vec<String>, ApiError> {
    let rows = sqlx::query(
        "SELECT subject_character_ids FROM consent_requests \
         WHERE world_id = ? AND status = 'approved' \
         AND event_kind IN ('death', 'permanent_exit', 'permanent_relation_change')",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    let mut subjects: Vec<String> = Vec::new();
    for r in &rows {
        let raw: String = r.try_get("subject_character_ids")?;
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&raw) {
            subjects.extend(list);
        }
    }
    subjects.sort();
    subjects.dedup();
    Ok(subjects)
}

/// 消费：把本回合引擎产出的 ConsentRequested 域事件落成同意请求（consents::create_consent），
/// 并通知当事角色主人。幂等：同 world+event_kind+subject 集合已有 pending 同意时不重复建
/// （引擎在获批前每回合都会重发该门控事件）。非事务关键路径：失败仅告警，不回滚已提交的 tick。
async fn create_consents_for_round(state: &AppState, world_id: &str, events: &[DomainEvent]) {
    for ev in events {
        if ev.event_type != DomainEventType::ConsentRequested {
            continue;
        }
        let event_kind = ev.fact.get("eventKind").and_then(Value::as_str).unwrap_or("");
        let detail = ev.fact.get("detail").and_then(Value::as_str).unwrap_or("");
        let mut subjects: Vec<String> = ev
            .fact
            .get("subjectCharacterIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        subjects.sort();
        subjects.dedup();
        if event_kind.is_empty() || subjects.is_empty() {
            continue;
        }
        // 幂等去重：规范化(排序去重)后的 subjects_json 与写入口径一致，精确匹配同 world+kind+subjects 的未决同意。
        let subjects_json = serde_json::to_string(&subjects).unwrap_or_else(|_| "[]".into());
        let dup = sqlx::query(
            "SELECT 1 AS x FROM consent_requests \
             WHERE world_id = ? AND event_kind = ? AND subject_character_ids = ? AND status = 'pending' LIMIT 1",
        )
        .bind(world_id)
        .bind(event_kind)
        .bind(&subjects_json)
        .fetch_optional(&state.db)
        .await;
        match dup {
            Ok(Some(_)) => continue, // 已有未决同意，勿重复建/重复通知
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(world_id, error = %e, "consent 幂等检查失败，跳过本条");
                continue;
            }
        }
        if let Err(e) =
            crate::consents::create_consent(state, world_id, event_kind, &subjects, detail, CONSENT_TTL_MS)
                .await
        {
            tracing::warn!(world_id, event_kind, error = %e, "创建同意请求失败（非事务关键路径）");
        }
    }
}

// ---------- 叙事 critic 报告落库（叙事质量 SLO「状态-文本矛盾率」的数据源，docs/VALIDATION.md §4.2 ①） ----------

/// critic 落库是否开启（`MUSE_CRITIC_PERSIST`）。
///
/// **默认开启**，与 §0.1「未验证功能默认关闭」不冲突：那条约束管的是**对用户开放的产品功能**
/// （能不能玩到、能不能买到），而本项是纯观测记录——不产生任何面向玩家的行为差异、不进任何读取面、
/// 引擎也永不读取它。默认关掉等于把「补齐 SLO 数据缺口」这件事本身关掉，缺口原地不动。
///
/// 之所以仍留一个开关（§0.2 参数化）：它是每 tick 一次的额外写。虽然相对同一 tick 内 5+ 次模型调用
/// 可忽略（纯本地 INSERT、无 IO、无额外事务），但真遇到写放大问题时要能一键停掉而不必改代码发版。
/// 取值：`0` / `false` / `off`（大小写不敏感）关闭，其余一律开启（**配错值不静默丢数据**）。
fn critic_persist_enabled() -> bool {
    critic_persist_from_env_value(std::env::var(ENV_CRITIC_PERSIST).ok().as_deref())
}

/// `critic_persist_enabled` 的纯判定部分（抽出以便测试：进程 env 是全局的，并行测试里不可写）。
pub(crate) fn critic_persist_from_env_value(raw: Option<&str>) -> bool {
    match raw {
        Some(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off"),
        None => true,
    }
}

/// 把本回合的叙事 critic 报告落进 `world_tick_critic`（迁移 0030），**在调用方的 tick 事务内**。
///
/// 背景：引擎每回合已经跑了一次 critic（`muse_engine::narrative::continuity::narrative_critic`），
/// 产出三条结构化列表随 `RoundOutcome.critic` 传回 —— 此前在这里被直接丢弃。模型已经算了、
/// token 已经花了，不留就是白扔（VALIDATION §4.2 把它列为「最可惜的缺口」）。
///
/// 🔴 **绝不写进 `worlds.narrative_state_json`**：那份 JSON 每 tick 经 `build_seed_state` 原样回灌进
/// 引擎 `RoundInput.state`，写进去的任何东西都可能被 `role_decide` / 仲裁读到。critic 是**对模型自身
/// 产出的评价**，回灌即「引擎读自己的评分再决策」，属 §0.1 平权红线禁区（同 `world_contributions`
/// 与角色成长数值的口径）。本表是纯结算/观测侧账本，**引擎永不读取**。
///
/// 🔴 **本表不是读取面**：issue 文本是模型原始输出、**未过 §15 第 2 层敏感词库闸**
/// （那道闸作用在 `ProjectedEvent` 上）。仅供运营 SLO 统计与人工复盘，不得未经过闸下发给玩家。
///
/// **每个已提交 tick 恒落一行**（哪怕三条列表全空）：行的存在本身就是「critic 跑过」的证据，
/// 也就是矛盾率的**分母**。只在有问题时才写的话，「跑了但很干净」与「压根没落库」在库里无法区分。
///
/// 幂等：主键 (world_id, tick_no) 与 `world_ticks` 唯一索引同口径；CAS 只放行一次提交，
/// 故正常路径不可能重复插入。真撞上（并发重放）则唯一冲突 → 视为已记录，静默跳过，不牵连 tick。
async fn persist_critic_report_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    tick_no: i64,
    critic: &muse_engine::narrative::continuity::CriticReport,
) -> Result<(), ApiError> {
    if !critic_persist_enabled() {
        return Ok(());
    }
    // 序列化失败（理论上不可能：三个 Vec<String>）→ 退化为空报告，只保留计数，绝不因观测数据阻断 tick。
    let report_json = serde_json::to_string(critic).unwrap_or_else(|_| "{}".into());
    let res = sqlx::query(
        "INSERT INTO world_tick_critic (world_id, tick_no, consistency_issue_count, \
         causal_issue_count, revision_suggestion_count, report_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(world_id)
    .bind(tick_no)
    .bind(critic.character_consistency_issues.len() as i64)
    .bind(critic.causal_issues.len() as i64)
    .bind(critic.revision_suggestions.len() as i64)
    .bind(&report_json)
    .bind(now_ms())
    .execute(&mut **tx)
    .await;
    match res {
        Ok(_) => Ok(()),
        // 并发重放兜底：同一 (world, tick) 已有记录 → 幂等跳过。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// ---------- 核心：处理一个 tick ----------

/// 处理一个 tick（生产入口：内部构建 HttpModelClient）。幂等：重复投递被吸收。
pub async fn process_tick(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
) -> Result<TickStatus, ApiError> {
    process_tick_inner(state, world_id, tick_no, None).await
}

/// 处理一个 tick（注入 model：集成测试用 mock ModelClient 走完整 run_round→commit）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn process_tick_with_model(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    model: Arc<dyn ModelClient>,
) -> Result<TickStatus, ApiError> {
    process_tick_inner(state, world_id, tick_no, Some(model)).await
}

/// tick 核心：认领 → 预算 → 回灌种子 → run_round → 事务提交 → 广播。
async fn process_tick_inner(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    model_override: Option<Arc<dyn ModelClient>>,
) -> Result<TickStatus, ApiError> {
    let db = &state.db;

    // 1) 读 tick 行。done/failed → 幂等跳过；超重试上限 → 终态化（C-9）。
    let Some(trow) =
        sqlx::query("SELECT status, base_revision, attempts FROM world_ticks WHERE world_id=? AND tick_no=?")
            .bind(world_id)
            .bind(tick_no)
            .fetch_optional(db)
            .await?
    else {
        return Ok(TickStatus::Skipped("no_tick"));
    };
    let tstatus: String = trow.try_get("status")?;
    let base_revision: i64 = trow.try_get("base_revision")?;
    let attempts: i64 = trow.try_get("attempts")?;
    if tstatus == "done" || tstatus == "failed" {
        return Ok(TickStatus::Skipped("already_done"));
    }
    if attempts >= MAX_TICK_ATTEMPTS {
        mark_tick_failed_and_pause(state, world_id, tick_no, "max_attempts").await?;
        return Ok(TickStatus::Failed);
    }

    // 2) 原子认领（C-1）：pending→running CAS，attempts+1。rows=0 → 已被别的 worker 认领/非 pending，跳过。
    let claimed = sqlx::query(
        "UPDATE world_ticks SET status='running', attempts=attempts+1, started_at=COALESCE(started_at, ?) \
         WHERE world_id=? AND tick_no=? AND status='pending'",
    )
    .bind(now_ms())
    .bind(world_id)
    .bind(tick_no)
    .execute(db)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(TickStatus::Skipped("claimed_elsewhere"));
    }

    // 3) 世界必须 running。
    let world = load_world(db, world_id).await?;
    if world.status != "running" {
        finish_tick_noop(db, world_id, tick_no, Some("world_not_running")).await?;
        return Ok(TickStatus::Skipped("world_not_running"));
    }
    // 陈旧 tick：state_revision 已被更早的 tick 推进（base_revision 不再匹配）→ 终态跳过，不做无谓的昂贵回合。
    if world.state_revision != base_revision {
        finish_tick_noop(db, world_id, tick_no, Some("superseded")).await?;
        return Ok(TickStatus::Skipped("superseded"));
    }

    // 4) 预算预检 + 熔断（token + cny，B-2）。
    let today = day_string(now_ms());
    let mut remaining_tokens: u64 = DEFAULT_REMAINING_TOKENS;
    if let Some(brow) = sqlx::query(
        "SELECT daily_token_budget, daily_cny_budget_cents, spent_tokens_today, budget_day, fused \
         FROM world_budgets WHERE world_id=?",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?
    {
        let daily: i64 = brow.try_get("daily_token_budget")?;
        let daily_cny: i64 = brow.try_get("daily_cny_budget_cents")?;
        let mut spent: i64 = brow.try_get("spent_tokens_today")?;
        let day: String = brow.try_get("budget_day")?;
        let fused: i64 = brow.try_get("fused")?;
        // 新的一天：重置窗口。
        if day != today {
            spent = 0;
            sqlx::query("UPDATE world_budgets SET spent_tokens_today=0, budget_day=?, fused=0, updated_at=? WHERE world_id=?")
                .bind(&today)
                .bind(now_ms())
                .bind(world_id)
                .execute(db)
                .await?;
        } else if fused != 0 {
            pause_world(db, world_id).await?;
            finish_tick_noop(db, world_id, tick_no, Some("budget_fused")).await?;
            return Ok(TickStatus::Fused);
        }
        // token 熔断：先暂停并记录，不悄悄降级模型（§9.2）。
        if daily > 0 && spent >= daily {
            fuse_and_pause(db, world_id, tick_no, &today, spent).await?;
            tracing::warn!(world_id, "world token 预算熔断，已暂停");
            return Ok(TickStatus::Fused);
        }
        // cny 熔断（B-2）：按 token→cny 估算达上限即熔断（真实定价留运营配置）。
        if daily_cny > 0 {
            let est_cny = spent.saturating_mul(token_cny_cents_per_1k()) / 1000;
            if est_cny >= daily_cny {
                fuse_and_pause(db, world_id, tick_no, &today, spent).await?;
                tracing::warn!(world_id, est_cny, daily_cny, "world cny 预算熔断，已暂停");
                return Ok(TickStatus::Fused);
            }
        }
        remaining_tokens = if daily > 0 { (daily - spent).max(0) as u64 } else { DEFAULT_REMAINING_TOKENS };
    }

    // 5) 模型配置解析：无配置 → dev 跳过（不 panic）。max_output_tokens 随路由一并钉住（按世界可调）。
    let Some((routes, max_output_tokens)) = resolve_model_routes(db, &world.model_route_version).await?
    else {
        tracing::warn!(world_id, version = %world.model_route_version, "world 无模型配置，tick 跳过");
        finish_tick_noop(db, world_id, tick_no, Some("no_model_config")).await?;
        return Ok(TickStatus::Skipped("no_model_config"));
    };

    // 5.5) idle 房通用装配（缺口②）：首 tick 前若未装配则一次性装配，随后 reload world。
    //   与 chapter_start（chapters/mod.rs）同一入口 assemble_instance（房型无关），仅去掉 require_chapter_room
    //   门控——chapter 房经 start 已在排 tick 前装配，故限定 room_type=="idle" 使 chapter 路径逐字节不变；
    //   arena 为主播驱动、不自动装配，排除。
    //   触发条件唯一：assembled_json IS NULL（不加"模板含 worldCharacters/locations"守卫）——assemble_instance
    //   同时产出 enabledEndings（select_ending/终局依赖）、perCharacterHooks、fatedNodes、sampling.selectedMainline
    //   （seed_narrative_layer 建 outline 用），纯主线模板若被守卫拦掉将丢结局/钩子/宿命种入；空池天然退化
    //   （空 hooks/空 NPC）且写入非 NULL wrapper → 自限，不会每 tick 重装。
    //   幂等：assemble_instance 内 C-7 CAS（assembly/mod.rs `WHERE assembled_json IS NULL`）保证仅首次写入，
    //   两个并发首 tick reload 得同一 assembled_json；后续 tick world.assembled_json.is_some() 直接短路 → 每房仅一次。
    //   阵容快照语义：装配在首 tick 触发，阵容指纹 = 首 tick 时的在场成员集，经 CAS 钉住；首 tick 后加入的玩家
    //   不改已钉装配（与 chapter 房"start 时刻钉住"同语义）。assemble_instance 只 CAS assembled_json，不动
    //   state_revision，故上方 :1107 陈旧门（world.state_revision != base_revision）判定不受 reload 影响。
    //   置于 running/superseded/budget/model 各门之后：不为将被 skip 的世界白装配；且必须在下方
    //   active_cards.len() < 2 门之前——NPC 来自装配的 worldCharacterEntries，装配前注入不了 NPC，否则单人 idle
    //   + NPC 模板会永远卡在 insufficient_members（本缺口的死锁）。
    //   P2 Stage3 扩容：装配兜底也覆盖 **event 房**（room_type=="idle" || timeline_mode=="event"），使
    //   event×arena 房在首个 host/tick 排下的 tick 里一次性装配（产 locationGraph/worldCharacterEntries——
    //   select_cohort 地点碰撞的前提，否则退化为单一全局 cohort）。chapter 已在 start 装配 → 命中
    //   assembled_json.is_some() 短路，逐字节不变；interval 世界 timeline_mode!="event" 且非 idle → 不触发，零影响。
    let world = if world.assembled_json.is_none()
        && (world.room_type == "idle" || world.timeline_mode == "event")
    {
        crate::assembly::assemble_instance(state, world_id).await?;
        // 重载：使下方 6b 的 worldCharacterEntries/locationGraph 注入、seed_narrative_layer 的
        // fatedNodes/selectedMainline 种入、select_ending 的 enabledEndings 读取均命中新装配。
        load_world(db, world_id).await?
    } else {
        world
    };

    // 6) 组装成员卡与 principal 投影表。
    // 次级排序键 cloud_character_id 不可省，理由同 assembly::load_active_cards：joined_at 撞毫秒时
    // 行序由 DB 决定，会让每 tick 的成员遍历序在重放间漂移。本处影响面比装配层更直接——
    // other_cards_brief 与 principal 投影都按此序构造，进而喂给引擎。
    let mrows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, wm.user_id AS uid, cc.card_json AS card \
         FROM world_members wm JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.world_id = ? AND wm.status='active' \
         ORDER BY wm.joined_at ASC, wm.cloud_character_id ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;

    // 身份池叙事接线（总规格 §5【拍板 4、5】）：读回实例装配钉住的开局站位 `cid → 展示名`。
    // 空表 = 老世界 / 模板未声明 identityPool / 结构损坏 → 下方 brief 拼接完全退化为只放名字。
    // 🔴 平权红线（§0.1）：身份只进感知层 brief，不携带数值差异 / 准入门槛 / 产出加成 / 难度优待 /
    //    叙事特权，且**绝不进 active_cards**（角色卡快照不可变）。详见 `load_identity_display_names` 注释。
    let identity_display = load_identity_display_names(db, &world).await;

    let mut members_projection: Vec<ProjectionMember> = Vec::new();
    let mut member_ids: Vec<String> = Vec::new();
    let mut active_cards: BTreeMap<String, CharacterCardV2> = BTreeMap::new();
    let mut other_brief: BTreeMap<String, String> = BTreeMap::new();
    for row in &mrows {
        let cid: String = row.try_get("cid")?;
        let uid: String = row.try_get("uid")?;
        let card_json: String = row.try_get("card")?;
        members_projection.push(ProjectionMember { character_key: cid.clone(), user_id: uid });
        member_ids.push(cid.clone());
        if let Ok(card) = serde_json::from_str::<CharacterCardV2>(&card_json) {
            // 平权吃鸡：所有角色每回合同步决策行动（取消「仅前 5」上限）。
            // brief 收录全体名字（每个角色借此感知在场其他人），active 收录全体完整卡（逐一决策）。
            // 身份**只**织进 brief（`唐三（户部主事）`）——它是"这个人在这个世界的开局站位"如何被
            // 他人感知；card 原封不动进 active_cards，一个字节都不改。
            other_brief
                .insert(cid.clone(), brief_with_identity(&card.identity.name, identity_display.get(&cid)));
            active_cards.insert(cid, card);
        }
    }

    // 6a) 放置房终局策略（P1 Phase 0/3）：room_type + skeleton endgame（含 key_character_ids）。严格门
    //     room_type=='idle'（非 idle 房 enabled=false，终局逻辑全跳过）。装配层选定结局（enabled_endings 保底
    //     ≥1，装配层按权重掷点定盘 selectedEnding，runtime 只读不掷点；非 idle / 未装配 → None）。
    //     供早期关键角色退场终局 + 终局短路 + commit_tick 复用。
    let endgame_policy = load_endgame_policy(db, &world).await?;
    let selected_ending = if endgame_policy.enabled { select_ending(&world) } else { None };

    // 6a.1) 终局条件(3) 关键角色退场（P1 Phase 3）：关键角色永久退场（成员表 left/retired 或已 landed 的
    //       permanent_exit consent）→ 主线无谓续跑，直接终局停机。**必须先于下方 insufficient_members 门**：
    //       关键角色退场常使在场成员跌破 2，若排在门后将永远走不到终局评估而使世界卡死（skip 无限循环）。
    //       严格门 policy.enabled(idle) + 防秒结束地板 min_world_ticks；读外部已提交事实，尚未开终局事务。
    if endgame_policy.enabled
        && tick_no >= endgame_policy.min_world_ticks
        && key_character_exited(db, world_id, &endgame_policy).await?
    {
        tracing::info!(world_id, tick_no, "关键角色退场 → 终局停机（P1 Phase 3）");
        return conclude_world_no_round(
            state,
            world_id,
            tick_no,
            "key_character_exit",
            selected_ending.as_deref(),
            &members_projection,
        )
        .await;
    }

    // 6b) 世界固有角色（NPC/反派）注入（Phase 1）：从 assembled_json 的 worldCharacterEntries 读回，
    //     插入 active_cards（参与决策/碰撞）+ other_brief（被玩家感知），但【不进 members_projection】——
    //     NPC 无 owner、不投影日报。id 收入 world_controlled，供引擎同意门控豁免（无主人可授权）。
    let mut world_controlled: Vec<String> = Vec::new();
    // Phase 2：NPC 初始地点（home_location）+ 本回合地点图，供种子/引擎 RoundInput.locations。
    let mut npc_home_locations: BTreeMap<String, String> = BTreeMap::new();
    let mut locations: BTreeMap<String, LocationDef> = BTreeMap::new();
    // Phase 3：NPC 携带道具的持有事实（item:<id>/tag:<t>），物化进其 CharacterState.resources
    //（NPC 道具事实源 = 装配钉住的 carriedItems；玩家道具事实源 = backpack，见 load_carried_item_facts）。
    let mut npc_item_facts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(raw) = &world.assembled_json {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            if let Some(arr) = v.pointer("/assembly/worldCharacterEntries").and_then(Value::as_array) {
                for entry in arr {
                    let Some(npc_id) = entry.get("characterId").and_then(Value::as_str) else {
                        continue;
                    };
                    // 空 id 或与玩家成员冲突：跳过（玩家优先，避免覆盖成员卡）。
                    if npc_id.trim().is_empty() || active_cards.contains_key(npc_id) {
                        continue;
                    }
                    let Some(card) = entry
                        .get("card")
                        .and_then(|c| serde_json::from_value::<CharacterCardV2>(c.clone()).ok())
                    else {
                        continue; // 卡解析失败：防御式跳过，不 panic。
                    };
                    other_brief.insert(npc_id.to_string(), card.identity.name.clone());
                    active_cards.insert(npc_id.to_string(), card);
                    world_controlled.push(npc_id.to_string());
                    // NPC 初始地点（Phase 2）：供 build_seed_state 后种入角色格。
                    let home = entry.get("location").and_then(Value::as_str).unwrap_or("").to_string();
                    npc_home_locations.insert(npc_id.to_string(), home);
                    // NPC 携带道具（Phase 3）：解引用后的 carriedItems → 持有事实，物化进 resources。
                    let carried =
                        entry.get("carriedItems").and_then(Value::as_array).cloned().unwrap_or_default();
                    npc_item_facts.insert(npc_id.to_string(), item_facts_from_defs(&carried));
                }
            }
            // Phase 2：钉住的地点图 → id→LocationDef，传引擎 RoundInput.locations。缺失 → 空（退化为单一全局场景）。
            if let Some(arr) = v.pointer("/assembly/locationGraph").and_then(Value::as_array) {
                for node in arr {
                    if let Ok(def) = serde_json::from_value::<LocationDef>(node.clone()) {
                        if !def.id.trim().is_empty() {
                            locations.insert(def.id.clone(), def);
                        }
                    }
                }
            }
        }
    }
    world_controlled.sort();
    world_controlled.dedup();
    // Phase 2：默认起点 = 地点图中首个非秘境地点 id（BTreeMap 键有序 → 确定性）；无地点图 → ""（退化）。
    let default_start = locations
        .values()
        .find(|l| !l.is_secret_realm)
        .map(|l| l.id.clone())
        .unwrap_or_default();

    // 门槛：无玩家成员（纯 NPC 世界）短路跳过，防空跑；活跃卡（含 NPC）不足 2 仍无碰撞可推进。
    if member_ids.is_empty() || active_cards.len() < 2 {
        finish_tick_noop(db, world_id, tick_no, Some("insufficient_members")).await?;
        return Ok(TickStatus::Skipped("insufficient_members"));
    }

    // 7) 托梦（accepted whisper）：Q-3 只喂给真正参与本回合决策的活跃角色，并记录被喂入的干预 id，
    //    仅这些在 commit 时置 applied（避免把长回合中途新到、或非在场角色的 whisper 静默标 applied 却从不投递）。
    let mut whispers: BTreeMap<String, String> = BTreeMap::new();
    let mut fed_intervention_ids: Vec<String> = Vec::new();
    let wrows = sqlx::query(
        "SELECT id, character_id, payload_json FROM interventions \
         WHERE world_id=? AND status='accepted' AND kind='whisper' ORDER BY created_at ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    for row in &wrows {
        let cid: String = row.try_get("character_id")?;
        if !active_cards.contains_key(&cid) {
            continue; // 非活跃决策角色：本 tick 不喂入、不消费。
        }
        let iid: String = row.try_get("id")?;
        let payload: String = row.try_get("payload_json")?;
        let text = serde_json::from_str::<Value>(&payload)
            .ok()
            .and_then(|v| {
                for k in ["text", "message", "content"] {
                    if let Some(s) = v.get(k).and_then(Value::as_str) {
                        return Some(s.to_string());
                    }
                }
                None
            })
            .unwrap_or(payload);
        // 同一角色多条 whisper：全部投递（换行拼接）、全部消费。
        whispers
            .entry(cid)
            .and_modify(|existing| {
                existing.push('\n');
                existing.push_str(&text);
            })
            .or_insert(text);
        fed_intervention_ids.push(iid);
    }

    // 8) 回灌（E-1）：把 DB 权威叙事状态物化到引擎 FS（run_id=world_id 稳定），首 tick 用 skeleton/assembled 种子。
    //    这是「DB narrative_state_json ↔ 引擎 FS 单一事实源」的每 tick 落实点——引擎 store.load 不再 fail-closed，
    //    也不放宽约束（硬节点/禁止谓词随种子进入 FS，回合仍受约束）。
    let data_dir = std::path::PathBuf::from(&state.config.object_store_dir)
        .join("world-data")
        .join(world_id);
    let fs: Arc<dyn HostFs> = Arc::new(StdFs::new(data_dir));
    let run_id = world_id.to_string();
    let mut seed = build_seed_state(db, &world, &member_ids, base_revision).await?;
    // Phase 2：种入 NPC 角色格 + 初始地点（首 tick 冷启动 & 跨 tick 补齐）。已有 location 不覆盖——
    // 保留上一 tick 移动落定的位置（DB 权威）。无地点图时 default_start="" → 全体归 ""，退化为单一场景。
    for (npc_id, home) in &npc_home_locations {
        let cs = seed.characters.entry(npc_id.clone()).or_default();
        if cs.location.is_empty() {
            cs.location = if home.is_empty() { default_start.clone() } else { home.clone() };
        }
    }
    if !default_start.is_empty() {
        for m in &member_ids {
            let cs = seed.characters.entry(m.clone()).or_default();
            if cs.location.is_empty() {
                cs.location = default_start.clone();
            }
        }
    }
    // Phase 3：道具事实源单一化——把玩家 backpack（carried 到本世界）+ NPC 携带道具物化进
    // CharacterState.resources（`item:<id>` / `tag:<t>`）。这是引擎 R6b 秘境准入读「角色持有道具」的
    // 唯一事实源。每 tick 幂等刷新：先清除派生的 item:/tag: 项（丢弃上 tick 的物化），再按当前
    // backpack/携带重灌 —— 避免跨 tick 累积、且道具增减实时反映（自由文本 resources 保留供 R1 判定）。
    let player_item_facts = load_carried_item_facts(db, world_id).await?;
    for (cid, cs) in seed.characters.iter_mut() {
        let facts = player_item_facts.get(cid).or_else(|| npc_item_facts.get(cid));
        materialize_item_facts(&mut cs.resources, facts.map(Vec::as_slice).unwrap_or(&[]));
    }
    muse_engine::store::write_json(fs.as_ref(), &engine_state_path(&run_id), &seed)?;

    // 9) 模型客户端（注入优先；否则构建 HttpModelClient——放到跳过检查之后避免每 tick 无谓构建）。
    let model: Arc<dyn ModelClient> = match model_override {
        Some(m) => m,
        None => Arc::new(HttpModelClient::new()?),
    };
    let prompts = resolve_prompts(db, &world.prompt_set_version).await?;

    // 9.5) #3b 回灌：本世界已获批(approved)的不可逆同意 subject → 引擎门控白名单。
    //     命中的 subject 本回合可落定其不可逆结果（death/permanent_exit/permanent_relation_change）；
    //     其余一律门控不落定并（重新）产 ConsentRequested。空 = 保守安全默认（全部门控）。
    let approved_consents = load_approved_consent_subjects(db, world_id).await?;
    // 放置房终局策略 endgame_policy / 装配选定结局 selected_ending 已在 6a) 组装（供本段终局短路 +
    // commit_tick 事务内终局评估复用）。

    // 9.6) 僵局打破提示（B. stall hint）：该世界连续 blocked ≥ 阈值 → 组装 stall_hint
    //（含最近原因与连续次数），经 RoundInput 织入引擎导演 prompt，促使导演主动破局。
    let stall_hint = stall_tracker().hint(world_id);

    // 10) run_round（失败重试一次）；每次尝试独立 TokenMeter，取成功尝试的实测 token 计费（B-1）。
    let mut last_err: Option<ApiError> = None;
    for attempt in 0..2u32 {
        let meter = Arc::new(TokenMeter::default());
        let host = EngineHost {
            fs: fs.clone(),
            clock: Arc::new(SystemClock),
            events: meter.clone(),
            model: model.clone(),
        };
        let engine = NarrativeEngine::new(Arc::new(host));
        let input = RoundInput {
            run_id: run_id.clone(),
            mode: RunMode::Observe,
            active_cards: active_cards.clone(),
            other_cards_brief: other_brief.clone(),
            // 身份池「自身感知」（总规格 §5【拍板 4、5】）：把开局站位喂给**角色本人**。
            // 引擎 decide 组装可见上下文时恒剔除自己，故 brief 里的身份只有别人看得见；
            // 此通道补上「你在这个世界是户部主事」这一句，玩家才感知得到自己的开局站位。
            // 复用同一份 `load_identity_display_names` 读回结果（不另起第二套解析）：
            // 「别人怎么看你」（brief）与「你知道自己是谁」（self_identities）永远同源同值、同确定性。
            // 🔴 平权红线（§0.1）：只进感知层，不携带数值差异 / 准入门槛 / 产出加成 / 难度优待 /
            //    叙事特权；绝不进 active_cards（角色卡不可变快照），也绝不占用托梦（whispers）配额。
            // 退化：空表（老世界 / 模板未声明 identityPool / assembled_json 结构损坏）→ 引擎上下文
            //    里根本不出现该字段，逐字节与接线前一致，且任何解析失败都不阻断 tick。
            self_identities: identity_display.clone(),
            whispers: whispers.clone(),
            fragments: BTreeMap::new(),
            temperature_decide: 0.0,
            temperature_writer: 0.8,
            // 世界钉住的路由配置值（缺省已回退 DEFAULT_MAX_OUTPUT_TOKENS）——不再读进程级 env。
            max_output_tokens,
            budget: RoundBudget { max_total_tokens: remaining_tokens, spent_tokens: 0, max_scenes: 1 },
            // #3b：已获批不可逆同意 subject 回灌；命中者本回合门控放行落定。
            approved_consents: approved_consents.clone(),
            // Phase 1：世界固有角色（NPC/反派）id 集合，供引擎同意门控自动放行（无主人可授权）。
            world_controlled: world_controlled.clone(),
            // Phase 2：本回合地点图，供引擎按 location 分组 + 移动 R6 连通/准入判定。空 = 退化为单一场景。
            locations: locations.clone(),
            // P2 DES：now_hint 仅对 interval 模式生效（=0 → 事件 timestamp/decision_id 时间段退化为 0，行为不变）。
            // event 模式下 run_event_step 会用本步激活时刻 T 覆盖它（见 run_event_step 内 filtered.now_hint = t），
            // 故此处传 0 无副作用。
            now_hint: 0,
            // 僵局打破提示（B）：连续 blocked ≥ 阈值时携带最近原因，None = 无僵局（默认路径零变化）。
            stall_hint: stall_hint.clone(),
            // R1 生死契约三档（总规格 §11【拍板 24】）：从世界行 `worlds.lethality`（0026）回灌。
            // `effective_lethality` 与 join 契约门是**同一个函数**——玩家签的那一档就是引擎跑的那一档，
            // 不可能出现"签了生死状却跑同意制"或反过来的错配。
            // 两处保守降级都在该函数内：非法/未知值 → 同意制；生死状但运营开关未开 → 同意制
            //（未验证功能默认关闭，VALIDATION.md §0.1）。历史世界落库即 'consent'，行为零变化。
            lethality: effective_lethality(&world.lethality),
        };
        let cancel = CancelFlag::new();
        // 时间线模式分派（第二块 Phase 2）：event 世界走 DES 调度器 run_event_step（内部做 cohort 过滤 +
        // next_time 推进 + timeline 持久化，产 EventStep）；interval 世界（默认，老世界）**完全走原 run_round**。
        // run_event_step 内部自算激活时刻 T 并 cohort 过滤，故 server 照常传全体 active_cards（选择逻辑在引擎内）。
        // P1 Phase 0：event 步的 EventStep.terminal（P2 引擎产 MainlineDone/TimeCapReached/Starved）被消费——
        // 起始即终局（无回合提交）走终局短路收尾；跑完回合后的终局信号随 outcome 带到 commit_tick 事务内评估。
        let run_result: Result<(_, Option<Terminal>), muse_engine::EngineError> = if world.timeline_mode
            == "event"
        {
            match engine.run_event_step(&routes, &prompts, input, &cancel).await {
                Ok(step) => {
                    let terminal = step.terminal;
                    match step.outcome {
                        Some(outcome) => Ok((outcome, terminal)),
                        None => {
                            // 终局短路：run_event_step 起始即判终局，无状态可提交（无回合）。消费终局信号（P1 Phase 0）：
                            // policy.enabled 且过终局地板 → end_world 停机（无 CAS，单独事务原子结算）；否则保持 running
                            //（未到地板/非 idle 沿用 P2 noop 收尾，世界继续跑到地板/时间上限）。
                            let reason = terminal.as_ref().map(terminal_reason).unwrap_or("terminal");
                            if endgame_policy.enabled && tick_no >= endgame_policy.min_world_ticks {
                                tracing::info!(world_id, tick_no, reason, "event 步终局短路，消费终局信号 → 停机");
                                return conclude_world_no_round(
                                    state,
                                    world_id,
                                    tick_no,
                                    reason,
                                    selected_ending.as_deref(),
                                    &members_projection,
                                )
                                .await;
                            }
                            tracing::info!(
                                world_id, tick_no, reason,
                                "event 步终局短路，未到终局地板或非 idle 房，保持 running"
                            );
                            finish_tick_noop(db, world_id, tick_no, Some("terminal")).await?;
                            return Ok(TickStatus::Skipped("terminal"));
                        }
                    }
                }
                Err(e) => Err(e),
            }
        } else {
            engine.run_round(&routes, &prompts, input, &cancel).await.map(|o| (o, None))
        };
        match run_result {
            Ok((outcome, terminal)) => {
                // 实测 token（B-1）；模型未回报 token 时回退到引擎预估，保证预算仍累计。
                // 🔴 必须算在 blocked 分支**之前**：阻断拍与提交拍烧的是同一批调用，
                //    成本口径不能因为「这一拍提交没提交」而分叉（#42）。
                let metered = meter.total_tokens();
                let cost = if metered > 0 { metered } else { outcome.budget.spent_tokens };
                if let Some(reason) = &outcome.blocked {
                    tracing::warn!(
                        world_id, tick_no, cost, reason = %reason,
                        "tick blocked（硬节点/不变量不可满足），不提交状态但照记成本"
                    );
                    // 僵局账 +1 并记原因（B. stall hint）：streak ≥ 阈值时下一 tick 携带 stall_hint。
                    // 「Blocked 不提交」不变量不动——**叙事状态**一个字节都不写；
                    // 记的是账（tick 成本 + 日预算），不是状态。
                    stall_tracker().record_blocked(world_id, reason);
                    finish_tick_blocked(db, world_id, tick_no, cost).await?;
                    return Ok(TickStatus::Skipped("blocked"));
                }
                let status = commit_tick(
                    state,
                    world_id,
                    tick_no,
                    base_revision,
                    &outcome,
                    &members_projection,
                    cost,
                    &fed_intervention_ids,
                    &endgame_policy,
                    terminal.as_ref(),
                    selected_ending.as_deref(),
                )
                .await?;
                // 提交成功（Done/Concluded）→ 清零该世界的连续 blocked 计数（B. stall hint）。
                if matches!(status, TickStatus::Done | TickStatus::Concluded) {
                    stall_tracker().clear(world_id);
                }
                return Ok(status);
            }
            Err(e) => {
                tracing::warn!(world_id, tick_no, attempt, error = %e, "run_round 失败");
                last_err = Some(e.into());
            }
        }
    }

    // 11) 重试后仍失败：标记失败 + 暂停世界。
    tracing::error!(world_id, tick_no, "tick 重试后仍失败，世界暂停");
    mark_tick_failed_and_pause(state, world_id, tick_no, "run_round_failed").await?;
    let _ = last_err;
    Ok(TickStatus::Failed)
}

/// 同一事务写：narrative_state(CAS)/world_ticks(done,实测 token 成本)/world_events 投影
/// （**过 §15 第 2 层敏感词库闸**：命中即打码 + 置 moderation='pending' + 记险）/
/// 本 tick 实际喂入的 whisper 干预 applied/预算累计。
///
/// P1 Phase 0：CAS 成功后、tx.commit 前，插入放置房终局评估——引擎终局信号（`terminal`：event 步跑完回合后
/// 复判的 MainlineDone/TimeCapReached/Starved）∨ 世界时间上限（`reached_time_limit`，优先 game_time、回退
/// tick_no），过 `policy.enabled`(严格门 idle) + `min_world_ticks` 地板 → 事务内 `end_world_tx` 停机、返回
/// `TickStatus::Concluded`。**终局与状态 CAS 同事务保原子**（不裂：状态提交与停机同成同败）。
#[allow(clippy::too_many_arguments)]
async fn commit_tick(
    state: &AppState,
    world_id: &str,
    tick_no: i64,
    base_revision: i64,
    outcome: &muse_engine::narrative::RoundOutcome,
    members: &[ProjectionMember],
    cost_tokens: u64,
    fed_intervention_ids: &[String],
    policy: &RoomEndgamePolicy,
    terminal: Option<&Terminal>,
    ending: Option<&str>,
) -> Result<TickStatus, ApiError> {
    let now = now_ms();
    let cost = cost_tokens as i64;
    let new_state_json = serde_json::to_string(&outcome.new_state).map_err(ApiError::internal)?;
    // 引擎已把状态 revision 推进到 base_revision+1（回灌保证 FS revision == base_revision）；DB 单一事实源与之对齐。
    let new_revision = outcome.new_state.revision as i64;
    // 世界游戏时钟回写（第二块 Phase 2）：= NarrativeState.timeline.now（event 模式由 run_event_step 推进到本步 T；
    // interval 模式恒为 0，不推进时钟）。与 narrative_state_json 同事务原子写，保证 game_time 与状态一致。
    let game_time = outcome.new_state.timeline.now;

    let mut tx = state.db.begin().await?;

    // CAS：仅当世界仍处 base_revision 时推进；否则视为已被更早的 tick 处理 → 回滚 + 终态化（C-2）。
    let cas = sqlx::query(
        "UPDATE worlds SET narrative_state_json=?, state_revision=?, game_time=?, updated_at=? WHERE id=? AND state_revision=?",
    )
    .bind(&new_state_json)
    .bind(new_revision)
    .bind(game_time)
    .bind(now)
    .bind(world_id)
    .bind(base_revision)
    .execute(&mut *tx)
    .await?;
    if cas.rows_affected() == 0 {
        tx.rollback().await?;
        finalize_cas_conflict(&state.db, world_id, tick_no).await?;
        return Ok(TickStatus::Skipped("cas_conflict"));
    }

    // ③ 世界线层贡献归因（R1，总规格 §9）：把引擎本回合已定序的 decisions/outcomes 与 state_patch
    // 交给 progression，逐角色折算并累积进独立账本 world_contributions。与状态 CAS 同事务——
    // tick 回滚则贡献同滚，绝无"状态没提交但贡献已记"的错位。
    //
    // **必须排在下方终局评估之前**：触发终局的那一拍，正是把最后一个里程碑推过阈值的一拍；
    // 若排在终局结算之后，这笔最具决定性的贡献会被系统性漏记，③ 层归因就失真了。
    //
    // 🔴 绝不写进 narrative_state_json：那份 JSON 每 tick 回灌进引擎 RoundInput，任何写进去的数值
    //    都可能被 role_decide / 仲裁读到，直接违反平权红线（§0.1 数值优势不得进入引擎决策）。
    crate::progression::accumulate_contributions_tx(
        &mut tx,
        world_id,
        &outcome.scene.decisions,
        &outcome.scene.outcomes,
        &outcome.scene.state_patch,
    )
    .await?;

    // 放置房终局评估（P1 Phase 0）：CAS 成功后、commit 前，与状态 CAS 同事务。严格门 policy.enabled(idle) +
    // 防秒结束地板 min_world_ticks。终局条件本 Phase：引擎信号(MainlineDone/TimeCapReached/Starved) ∨
    // 世界时间上限(reached_time_limit)。命中 → end_world_tx 停机（幂等 WHERE running）→ Concluded。
    let mut final_status = TickStatus::Done;
    if policy.enabled && tick_no >= policy.min_world_ticks {
        let reason: Option<&str> = if let Some(t) = terminal {
            Some(terminal_reason(t))
        } else if reached_time_limit(policy, game_time, tick_no) {
            Some("time_limit")
        } else {
            None
        };
        if let Some(reason) = reason {
            // rows>0 = 本 tick 真正结算 → Concluded + 终局产出（审计 + 荣誉奖励，同事务原子）；
            // rows==0 = 已被并发 tick 结算（幂等）→ 保持 Done、不重复产出。
            if end_world_tx(&mut tx, world_id, reason).await? > 0 {
                // (cid, user_id)：③ 世界线层产出发给卡的主人，故 user_id 必须一并带上。
                let participants: Vec<(String, String)> =
                    members.iter().map(|m| (m.character_key.clone(), m.user_id.clone())).collect();
                finalize_ending_tx(&mut tx, world_id, reason, ending, &participants).await?;
                final_status = TickStatus::Concluded;
            }
        }
    }

    sqlx::query(
        "UPDATE world_ticks SET status='done', cost_tokens=?, started_at=COALESCE(started_at, ?), \
         finished_at=?, error=NULL WHERE world_id=? AND tick_no=?",
    )
    .bind(cost)
    .bind(now)
    .bind(now)
    .bind(world_id)
    .bind(tick_no)
    .execute(&mut *tx)
    .await?;

    // 叙事 critic 报告落库（VALIDATION §4.2 ①「状态-文本矛盾率」的数据源）：模型本回合已经跑过的
    // 一致性/因果审校结果，此前在本函数的 outcome 解构处被直接丢弃。与状态 CAS 同事务——
    // critic 是观测数据、丢了不影响正确性，但同事务更简单且同成同败：CAS 冲突回滚时 critic 行一并
    // 消失，(world_id, tick_no) 与 world_ticks 的 done 行严格一一对应，不会留孤儿观测行。
    // 🔴 只进独立表，绝不进 narrative_state_json（回灌引擎 = 平权红线，见 persist_critic_report_tx）。
    persist_critic_report_tx(&mut tx, world_id, tick_no, &outcome.critic).await?;

    // 受众投影 + 内容安全第 2 层 + 落库（分配 per-world sequence）。
    //
    // 拦截点选在「投影之后、落库之前」：此处的 ProjectedEvent.summary 已是**面向用户的最终文本**
    // （引擎 DomainEvent 的原始负载永不下发），比在引擎里拦更准；且整段仍在本事务内，与状态 CAS
    // 同成同败——CAS 冲突回滚时打码与风控留痕一并消失，不产生「事实没落但险已记」的错位。
    // 第 2 层是纯本地词表匹配（≈0 成本、无 IO），放事务内安全；**第 3 层语义分类是网络调用，
    // 绝不能挪进来**（单连接池会死锁 PoolTimedOut，见 safety::moderate_runtime_projection 的 TODO）。
    let mut projected = events::project_domain_events(&outcome.scene.events, members);
    crate::safety::moderate_runtime_projection(&mut tx, world_id, &mut projected).await?;
    let stored = events::insert_events_tx(&mut tx, world_id, tick_no, &projected).await?;

    // Q-3：只消费本 tick 实际喂入的 accepted 干预（按 id 精确置 applied），不 blanket 标全部 accepted。
    for iid in fed_intervention_ids {
        sqlx::query("UPDATE interventions SET status='applied' WHERE id=? AND world_id=? AND status='accepted'")
            .bind(iid)
            .bind(world_id)
            .execute(&mut *tx)
            .await?;
    }

    // 预算累计（B-1：实测 token）。
    sqlx::query(
        "UPDATE world_budgets SET spent_tokens_today = spent_tokens_today + ?, budget_day=?, updated_at=? WHERE world_id=?",
    )
    .bind(cost)
    .bind(day_string(now))
    .bind(now)
    .bind(world_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // 提交后广播增量（推送层 principal 过滤在 stream 连接处执行）。
    for s in stored {
        state.ws_hub.publish(WsMessage {
            world_id: world_id.to_string(),
            audience_user_ids: s.audience_user_ids,
            payload_json: s.payload_json,
        });
    }

    // 集成接线（跨模块，非事务关键路径，失败不回滚 tick）：
    // ① #3b 消费本回合 ConsentRequested → 建同意请求（放置房不可逆行动的同意触发源，幂等去重）；
    // ② 每 tick 清理超时未决同意（保守默认，见 consents 状态机）；
    // ③ 幂等生成当日日报——放置房北极星，daily_reports 按 (world,character,day) 唯一去重。
    create_consents_for_round(state, world_id, &outcome.scene.events).await;
    let _ = crate::consents::expire_stale_consents(&state.db).await;
    let today = day_string(now);
    for m in members {
        let _ = crate::reports::generate_report(state, world_id, &m.user_id, &m.character_key, &today).await;
    }

    Ok(final_status)
}

// ---------- 后台任务 ----------

async fn worker_loop(state: AppState) {
    loop {
        let Some(job) = queue::pop_json::<TickJob>(&*state.queue, TOPIC).await else {
            continue;
        };
        let world_id = job.world_id.clone();
        let tick_no = job.tick_no;

        // C-9：worker 处理错误退避重试，超上限终态化，不再静默丢错让 tick 无限 re-enqueue。
        let mut retry: u32 = 0;
        loop {
            // 单独 spawn 隔离引擎侧 panic：panic 不拖垮 worker，而是暂停世界。
            let handle = {
                let state = state.clone();
                let world_id = world_id.clone();
                tokio::spawn(async move { process_tick(&state, &world_id, tick_no).await })
            };
            match handle.await {
                // 任一 TickStatus（含 P1 Phase 0 的 Concluded 终局）均为成功终态 → break，不重试。
                // Concluded 后世界已 ended，schedule_due_ticks 的 status='running' 门自动停止排新 tick。
                Ok(Ok(status)) => {
                    tracing::debug!(world_id, tick_no, ?status, "tick 处理完成");
                    break;
                }
                Ok(Err(e)) => {
                    retry += 1;
                    if retry >= WORKER_MAX_RETRIES {
                        tracing::error!(world_id, tick_no, error = %e, "tick 处理错误达上限，终态化并暂停世界");
                        let _ = mark_tick_failed_and_pause(&state, &world_id, tick_no, "worker_error").await;
                        break;
                    }
                    let backoff = WORKER_BACKOFF_BASE_MS * (1u64 << retry.min(5));
                    tracing::warn!(world_id, tick_no, retry, error = %e, "tick 处理错误，退避重试");
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
                Err(join_err) => {
                    tracing::error!(world_id, tick_no, error = %join_err, "tick 处理 panic，暂停世界");
                    let _ = mark_tick_failed_and_pause(&state, &world_id, tick_no, "engine_panic").await;
                    break;
                }
            }
        }
    }
}

async fn scheduler_loop(state: AppState) {
    let poll_ms: u64 =
        std::env::var("MUSE_TICK_POLL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5_000);
    loop {
        if let Err(e) = schedule_due_ticks(&state).await {
            tracing::warn!(error = %e, "tick 调度轮询失败");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// 启动调度器与 worker 池（main 调用）。并发度 MUSE_TICK_WORKERS（默认 2）。
pub fn spawn_workers(state: AppState) {
    let workers: usize =
        std::env::var("MUSE_TICK_WORKERS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    tokio::spawn(scheduler_loop(state.clone()));
    for _ in 0..workers.max(1) {
        tokio::spawn(worker_loop(state.clone()));
    }
    tracing::info!(workers, "runtime 调度器与 worker 池已启动");
}

/// 黄金世界回归（VALIDATION §4.1 验证基建）：固定 world_id + 固定 fixture + 剧本化模型，
/// 比对结构化产物是否逐字节不变。binary-only crate 无 lib.rs，只能落在 `#[cfg(test)]` 子模块，
/// 好处是自动进现有 `platform-test` CI job，CI 改动为零。
#[cfg(test)]
mod golden;
/// 仿真试跑工装（总规格 §4 内容中台工业线「仿真试跑（自动化压测）」）：种子驱动的批量世界
/// 生命周期驱动器 + 世界质量三指标（完读率/阻断率/结局分布，口径在 `crate::slo::quality`）。
/// 与 `golden` 同源不同层——golden 抓"某条固定路径的字节变了"，它抓"一整批世界的统计形状变了"。
/// 🔴 用的是种子驱动的**假模型**，三个指标**不度量内容质量**，见该文件头「诚实划界」。
#[cfg(test)]
mod simulation;
#[cfg(test)]
mod tests;
