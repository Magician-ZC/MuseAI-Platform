//! 世界 tick 模型调用的**录制 / 回放接线**——`muse_engine::replay` 的平台轨接口层（任务 #46）。
//!
//! ## 它接的是哪一层
//!
//! 唯一接点是 `runtime::process_tick_inner` 的第 9 步（模型客户端构建处）：那里是**整条 tick 路径上
//! 模型客户端的唯一出口**，`process_tick`（生产：内部造 `HttpModelClient`）与
//! `process_tick_with_model`（注入：golden / simulation）都从这一个口子出去。接在这里意味着：
//! 录制覆盖的是**生产同一条路径**，不是给回归另开的第二条路。
//!
//! ## 🔴 默认关闭 = 当前行为一字节不变（结构性保证，不是"包装层恰好透明"）
//!
//! 未配置时 [`wrap_tick_model`] 返回的是**传进来的那一个 `Arc`**——`Arc::ptr_eq` 成立，
//! 引擎拿到的对象与接线前逐位相同，中间**没有任何一层包装**。所以"默认路径零变化"不需要论证
//! "包装是透明的"，它是类型层面的恒等。锁在 `tests::off_returns_the_very_same_arc`；
//! 端到端那一层由 `tests::golden_world_record_replay_round_trip_is_byte_identical`
//! （关 / 录 / 放三跑黄金世界主线，结构化产物逐字节相等）与 `runtime::golden` 全套用例一起兜。
//!
//! ## ⚠️ 状态语言（VALIDATION §0.3）：本模块最高只到 `Implemented`
//!
//! 交付的是**接线与入口**。真实模型录制需要用户自己的 API Key，**本仓没有、也不该有**；
//! 因此至今**没有任何一份真实模型录制**入库，质量口径（OOC 怎么判、差异多大算退化）**也还没有**。
//! 跑绿本模块的测试 **不得** 被表述为「角色一致性已验证」或「已建立基线」——
//! 它证明的是「录得下来、放得回去、且默认关闭时管线一字节不变」。
//!
//! 录制里的模型响应来自谁，随录制产物一起走：[`SOURCE_LABEL_KEY`] 标签
//! （`scriptedStub` / `real`），同 `slo::quality::QualitySource::SimulatedStub` 的做法——
//! **数会被复制进评审材料，注释不会。**
//!
//! ## 开关（三层，优先级从高到低）
//!
//! | 层 | 谁用 | 怎么开 |
//! |---|---|---|
//! | 进程内按 world 覆盖 | 测试 / 录制入口 | [`set_world_capture`] / [`end_world_capture`] |
//! | 进程 env | 运维 / 一次性录制 | `MUSE_TICK_RECORD` / `MUSE_TICK_REPLAY`（见下） |
//! | 默认 | 所有人 | **Off** |
//!
//! ```text
//! MUSE_TICK_RECORD=<recordingId>     开录制。id 同时是文件名，字符集见 validate_recording_id
//! MUSE_TICK_REPLAY=<recordingId>     开回放（与 RECORD 互斥，同时设置 = 语义冲突 → 一律关闭）
//! MUSE_TICK_REPLAY_MATCH=prompt|slot 回放查表口径，缺省 prompt
//! MUSE_TICK_RECORD_DIR=<绝对路径>     录制产物根目录，缺省 = 该世界的引擎数据目录
//! MUSE_TICK_RECORD_WORLD=<worldId>   只对这一个世界生效，缺省 = 全部世界
//! ```
//!
//! env 只在**进程首次用到时读一次**（`OnceLock`）：后续 `set_var` 不再影响判定，
//! 避免"跑到一半开关自己变了"这种不可复现的局面。
//!
//! ## 录制失败降级、回放失败**不**降级（这条不对称是刻意的）
//!
//! - **录制**出任何问题（id 非法 / 落盘失败 / 超调用上限）→ 记 warn、退回真实模型，**绝不阻断 tick**。
//!   录制是观测面，不该有能力弄挂一个世界。
//! - **回放**加载失败 → **直接让 tick 失败**。回放一旦"降级成真实模型"，那次对比结果就是假的，
//!   而假的对比结果比没有对比更坏。这与引擎侧 `ReplayClient` 结构上没有 inner 字段是同一条纪律。
//!
//! ## 确定性契约（禁三样：系统随机 / 浮点 RNG / map 迭代序驱动 RNG）
//!
//! 本模块**一个随机源都没有**：所有映射 `BTreeMap`；落盘顺序由引擎 `RecordingClient::finish()`
//! 的规范序（拍 → 角色 → 环节 → 槽内序号）决定，与调用到达序无关；不记墙钟
//! （`with_recorded_at` 不调用 ⇒ `recordedAtMs` 恒 0），故同一份录制两次落盘逐字节相等。
//!
//! ## ⚠️ 并发前提：同一世界同一时刻只跑一拍
//!
//! 拍号对齐（`set_beat`）是**会话级**的单值状态。若同一个世界的两拍真的并发跑起来，
//! 后进的那拍会把拍号覆盖掉，录出来的 `beat` 就是错的（内容仍完整，只是对齐维度失真）。
//! 现有 runtime 不会出现这种局面（`world_ticks` 的 `pending→running` 原子认领 +
//! `base_revision` 陈旧检查，两道都把同世界并发拍挡在外面），故**依赖的是运行时的既有不变量**，
//! 本模块没有再加一道锁。🔴 哪天要放开同世界并发跑拍，这里必须一起改。
//!
//! ## 🔑 凭据
//!
//! 唯一新增的落盘路径是 `Recording::save`，脱敏在引擎侧（`redact` / `sanitize_base_url`，
//! `RecordedCall` 结构上没有 api_key 字段）。本模块**不另写任何文件**，标签只放版本号与 id
//! （见 [`wrap_tick_model`] 的 `labels` 参数，调用方传的是 promptSetVersion / modelRouteVersion /
//! templateId，均非凭据）。落盘前恒过一次 `Recording::validate()`，问题只记 warn 不阻断。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;

use muse_engine::host::{CancelFlag, HostFs, StdFs};
use muse_engine::model::{ModelCallSpec, ModelClient, ModelOutput};
use muse_engine::replay::{
    validate_recording_id, MatchMode, Recording, RecordingClient, ReplayClient, ReplayReport,
    DEFAULT_RECORDING_DIR,
};
use muse_engine::EngineError;

use crate::error::ApiError;

// ============================================================================
// §1 开关配置
// ============================================================================

pub(crate) const ENV_RECORD: &str = "MUSE_TICK_RECORD";
pub(crate) const ENV_REPLAY: &str = "MUSE_TICK_REPLAY";
pub(crate) const ENV_REPLAY_MATCH: &str = "MUSE_TICK_REPLAY_MATCH";
pub(crate) const ENV_DIR: &str = "MUSE_TICK_RECORD_DIR";
pub(crate) const ENV_WORLD: &str = "MUSE_TICK_RECORD_WORLD";

/// 单份录制的调用条数上限：长跑世界的录制全在进程内存里，没有上限就是一个慢性 OOM。
/// 触顶后**停止录制**（已录部分照常落盘并可用），而不是继续吃内存。
pub(crate) const MAX_RECORDED_CALLS: usize = 20_000;

/// 录制产物里标注「这些响应是谁给的」的标签键。
///
/// 🔴 它存在的理由和 `slo::quality::QualitySource::SimulatedStub` 一模一样：一份用剧本桩录出来的
/// 录制，长得和真实模型录的**一模一样**；不把来源钉进产物，半年后没人分得清手里这份是哪种，
/// 于是「角色一致性对比」就会拿桩当基线。
pub(crate) const SOURCE_LABEL_KEY: &str = "responseSource";
/// 来源取值：剧本 / 规则桩（`golden::ScriptedModel`、`simulation::SimModel`、各类 mock）。
/// 只由测试经 [`set_world_capture`] 显式声明——env 开关只可能开在真实服务上，不得靠 env 蒙对来源。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SOURCE_SCRIPTED_STUB: &str = "scriptedStub";
/// 来源取值：真实模型（`HttpModelClient`）。
pub(crate) const SOURCE_REAL: &str = "real";

/// tick 模型调用的录制/回放配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TickCapture {
    /// **默认**：不录不放，`wrap_tick_model` 恒等返回。
    Off,
    Record {
        recording_id: String,
        /// 录制产物根目录；`None` = 用该世界的引擎数据目录。
        root: Option<PathBuf>,
        /// 响应来源标注（[`SOURCE_SCRIPTED_STUB`] / [`SOURCE_REAL`]），随录制产物落盘。
        source: String,
    },
    Replay {
        recording_id: String,
        root: Option<PathBuf>,
        match_mode: MatchMode,
    },
}

impl TickCapture {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_off(&self) -> bool {
        matches!(self, TickCapture::Off)
    }
}

/// env 原始读数（读一次即冻结）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawCaptureEnv {
    pub(crate) record: Option<String>,
    pub(crate) replay: Option<String>,
    pub(crate) match_mode: Option<String>,
    pub(crate) dir: Option<String>,
    pub(crate) world: Option<String>,
}

impl RawCaptureEnv {
    /// 全空 = 默认路径。判定在这里早退，连字符串处理都不做。
    fn is_empty(&self) -> bool {
        self.record.is_none()
            && self.replay.is_none()
            && self.match_mode.is_none()
            && self.dir.is_none()
            && self.world.is_none()
    }

    fn read() -> Self {
        let get = |k: &str| std::env::var(k).ok();
        Self {
            record: get(ENV_RECORD),
            replay: get(ENV_REPLAY),
            match_mode: get(ENV_REPLAY_MATCH),
            dir: get(ENV_DIR),
            world: get(ENV_WORLD),
        }
    }
}

/// 判定结果 + **为什么**。`notes` 不是日志的副产品，而是判定的一部分：
/// "配置写错了所以我关掉了" 必须能被测试断言，不能只飘在 stderr 里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureDecision {
    pub(crate) capture: TickCapture,
    pub(crate) notes: Vec<String>,
}

impl CaptureDecision {
    fn off(note: impl Into<String>) -> Self {
        Self { capture: TickCapture::Off, notes: vec![note.into()] }
    }
    fn silent_off() -> Self {
        Self { capture: TickCapture::Off, notes: Vec::new() }
    }
}

/// 空串与三个假值一律当"没设置"——`MUSE_TICK_RECORD=0` 是人类表达"关掉"的常见写法，
/// 把它当成一个叫 `0` 的录制 id 是纯粹的惊吓。口径与 `critic_persist_from_env_value` 一致。
fn unset(raw: Option<&String>) -> Option<&str> {
    let v = raw?.trim();
    if matches!(v.to_ascii_lowercase().as_str(), "" | "0" | "false" | "off") {
        return None;
    }
    Some(v)
}

/// 纯判定：env 读数 + world_id → 配置。**无副作用、无 IO、无 env 访问**，故可直接单测
/// （进程 env 是全局的，并行测试里不可写——同 `critic_persist_from_env_value` 的处理）。
pub(crate) fn decide_capture(env: &RawCaptureEnv, world_id: &str) -> CaptureDecision {
    let record = unset(env.record.as_ref());
    let replay = unset(env.replay.as_ref());

    match (record, replay) {
        (None, None) => return CaptureDecision::silent_off(),
        (Some(_), Some(_)) => {
            return CaptureDecision::off(format!(
                "{ENV_RECORD} 与 {ENV_REPLAY} 同时设置：录制与回放语义互斥（一个要打真实模型、\
                 一个绝不能打），无法择一 → 两者都不启用"
            ));
        }
        _ => {}
    }

    // 世界过滤：不匹配就静默 Off（这是**正常**情况，不是配置错误，不该每拍刷一条告警）。
    if let Some(w) = unset(env.world.as_ref()) {
        if w != world_id {
            return CaptureDecision::silent_off();
        }
    }

    let root = unset(env.dir.as_ref()).map(PathBuf::from);
    if let Some(p) = &root {
        if !p.is_absolute() {
            return CaptureDecision::off(format!(
                "{ENV_DIR} 必须是绝对路径（它是 HostFs 的 root，相对路径的解释依赖进程 cwd，\
                 换个启动方式产物就落到别处）：{}",
                p.display()
            ));
        }
    }

    if let Some(id) = record {
        if let Err(e) = validate_recording_id(id) {
            return CaptureDecision::off(format!("{ENV_RECORD} 取值非法 → 不启用录制：{e}"));
        }
        return CaptureDecision {
            capture: TickCapture::Record {
                recording_id: id.to_string(),
                root,
                // env 开关只可能开在真实服务上；桩来源只由测试通过 `set_world_capture` 显式声明。
                source: SOURCE_REAL.to_string(),
            },
            notes: Vec::new(),
        };
    }

    let id = replay.expect("上面已排除 (None, None) 与 (Some, Some)");
    if let Err(e) = validate_recording_id(id) {
        return CaptureDecision::off(format!("{ENV_REPLAY} 取值非法 → 不启用回放：{e}"));
    }
    let (match_mode, mut notes) = match unset(env.match_mode.as_ref()) {
        None | Some("prompt") => (MatchMode::Prompt, Vec::new()),
        Some("slot") => (MatchMode::Slot, Vec::new()),
        Some(other) => (
            MatchMode::Prompt,
            vec![format!("{ENV_REPLAY_MATCH}={other:?} 无法识别（只认 prompt / slot）→ 回落 prompt")],
        ),
    };
    if match_mode == MatchMode::Slot {
        notes.push(
            "回放口径 = slot：命中不代表「新 Prompt 下模型也会这么答」，\
             prompt 漂移会记进 ReplayReport.warnings，读结论前先看那一栏"
                .to_string(),
        );
    }
    CaptureDecision { capture: TickCapture::Replay { recording_id: id.to_string(), root, match_mode }, notes }
}

fn env_raw() -> &'static RawCaptureEnv {
    static ENV: OnceLock<RawCaptureEnv> = OnceLock::new();
    ENV.get_or_init(RawCaptureEnv::read)
}

/// env 配置错误只值得说一次（判定每拍都跑，逐拍刷同一条告警毫无信息量）。
fn warn_env_notes_once(notes: &[String]) {
    static DONE: OnceLock<()> = OnceLock::new();
    if notes.is_empty() {
        return;
    }
    if DONE.set(()).is_err() {
        return;
    }
    for n in notes {
        tracing::warn!(target: "muse::runtime::record", "{n}");
    }
}

fn overrides() -> &'static Mutex<BTreeMap<String, TickCapture>> {
    static M: OnceLock<Mutex<BTreeMap<String, TickCapture>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 进程内按 world 显式接线（测试与录制入口用）。优先级高于 env。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_world_capture(world_id: &str, capture: TickCapture) {
    overrides().lock().unwrap().insert(world_id.to_string(), capture);
}

/// 收摊：撤掉覆盖并丢弃该世界的会话（录制内容此前每拍已落盘，丢的只是内存态）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn end_world_capture(world_id: &str) {
    overrides().lock().unwrap().remove(world_id);
    sessions().lock().unwrap().remove(world_id);
}

/// 当前生效配置：按 world 覆盖 → env → Off。
pub(crate) fn resolve_capture(world_id: &str) -> TickCapture {
    if let Some(c) = overrides().lock().unwrap().get(world_id) {
        return c.clone();
    }
    let env = env_raw();
    if env.is_empty() {
        return TickCapture::Off; // ← 默认路径在此早退
    }
    let d = decide_capture(env, world_id);
    warn_env_notes_once(&d.notes);
    d.capture
}

// ============================================================================
// §2 会话：跨 tick 存活的录制器 / 回放器
// ============================================================================

/// 可替换内层的 `ModelClient`。
///
/// 存在的理由：`RecordingClient` 在**创建时**捕获内层 client，而 `process_tick_inner` 每拍新建一个
/// `HttpModelClient`；直接把第一拍的那个封进录制器，后面所有拍就都在用一个"上一拍留下的"客户端。
/// 现在跑得通不代表以后跑得通——注入路径（golden / simulation）完全可以逐拍换 mock。
/// 这一层把"当前这一拍用哪个模型"从录制器的构造期挪到了每拍的赋值期。
struct SwapModel {
    current: Mutex<Arc<dyn ModelClient>>,
}

impl SwapModel {
    fn new(inner: Arc<dyn ModelClient>) -> Self {
        Self { current: Mutex::new(inner) }
    }
    fn set(&self, inner: Arc<dyn ModelClient>) {
        *self.current.lock().unwrap() = inner;
    }
}

#[async_trait]
impl ModelClient for SwapModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        // 🔴 锁**不得**跨 await：这里先把 Arc 克隆出来，guard 在本语句结束即释放。
        let inner = self.current.lock().unwrap().clone();
        inner.complete(spec, cancel).await
    }
}

struct RecordSession {
    client: Arc<RecordingClient>,
    swap: Arc<SwapModel>,
    fs: Arc<dyn HostFs>,
    /// 触顶告警只发一次。
    capped_warned: AtomicBool,
}

struct ReplaySession {
    client: Arc<ReplayClient>,
}

enum Session {
    Record(RecordSession),
    Replay(ReplaySession),
}

fn sessions() -> &'static Mutex<BTreeMap<String, Session>> {
    static S: OnceLock<Mutex<BTreeMap<String, Session>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 已录条数（无录制会话 → `None`）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn recorded_call_count(world_id: &str) -> Option<usize> {
    match sessions().lock().unwrap().get(world_id)? {
        Session::Record(s) => Some(s.client.call_count()),
        Session::Replay(_) => None,
    }
}

/// 回放报告（无回放会话 → `None`）。
///
/// 🔴 `ReplayReport::is_exact()` 为真只意味着「这次跑的调用构成与录制时逐一对上了」，
/// **不是**任何内容质量结论。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn replay_report(world_id: &str) -> Option<ReplayReport> {
    match sessions().lock().unwrap().get(world_id)? {
        Session::Replay(s) => Some(s.client.report()),
        Session::Record(_) => None,
    }
}

/// 取录制快照（无录制会话 → `None`）。落盘的就是它。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn recording_snapshot(world_id: &str) -> Option<Recording> {
    match sessions().lock().unwrap().get(world_id)? {
        Session::Record(s) => Some(s.client.finish()),
        Session::Replay(_) => None,
    }
}

fn capture_fs(root: Option<&Path>, world_fs: &Arc<dyn HostFs>) -> Arc<dyn HostFs> {
    match root {
        Some(p) => Arc::new(StdFs::new(p.to_path_buf())),
        None => world_fs.clone(),
    }
}

fn recording_rel(recording_id: &str) -> PathBuf {
    Path::new(DEFAULT_RECORDING_DIR).join(format!("{recording_id}.json"))
}

/// 从磁盘读一份录制（回放入口与"录完看一眼"共用同一条读路径）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn load_recording(
    root: Option<&Path>,
    world_fs: &Arc<dyn HostFs>,
    recording_id: &str,
) -> Result<Recording, EngineError> {
    validate_recording_id(recording_id)?;
    let fs = capture_fs(root, world_fs);
    Recording::load(fs.as_ref(), &recording_rel(recording_id))
}

// ============================================================================
// §3 接线点
// ============================================================================

/// 每拍收尾时把录制落盘的 RAII 守卫。
///
/// 用 `Drop` 而不是在 `process_tick_inner` 末尾显式调用：那个函数有十余个 `return`
/// （幂等跳过 / 陈旧 / 预算熔断 / blocked / 终局短路 / CAS 冲突 / 失败…），
/// 逐个补一行落盘既漏得掉也读不动。守卫在任何一条出口（含 panic 展开）上都会跑。
pub(crate) struct TickCaptureGuard {
    world_id: Option<String>,
}

impl TickCaptureGuard {
    /// 关闭态守卫：Drop 什么都不做。
    fn off() -> Self {
        Self { world_id: None }
    }
}

impl Drop for TickCaptureGuard {
    fn drop(&mut self) {
        let Some(world_id) = self.world_id.take() else {
            return;
        };
        if let Err(e) = flush_recording(&world_id) {
            // 🔴 录制落盘失败绝不能影响 tick 结论：此处已在 Drop 里，无处可返回错误，也不该有。
            tracing::warn!(
                target: "muse::runtime::record",
                world_id = %world_id, error = %e,
                "tick 录制落盘失败（tick 结果不受影响）"
            );
        }
    }
}

/// 把当前录制整份写到 `<root>/recordings/<id>.json`（每拍覆写，故文件恒是最新全量）。
/// 返回写入的绝对路径；无录制会话 → `Ok(None)`。
pub(crate) fn flush_recording(world_id: &str) -> Result<Option<PathBuf>, EngineError> {
    let guard = sessions().lock().unwrap();
    let Some(Session::Record(s)) = guard.get(world_id) else {
        return Ok(None);
    };
    let rec = s.client.finish();
    // 自检只记 warn 不阻断：一份"有问题但存下来了"的录制，永远好过"因为有问题所以没存"。
    let issues = rec.validate();
    if !issues.is_empty() {
        tracing::warn!(
            target: "muse::runtime::record",
            world_id, issues = %issues.join(" | "),
            "录制自检有问题（仍照常落盘，拿它当基线前先处理）"
        );
    }
    let rel = rec.save(s.fs.as_ref(), Path::new(DEFAULT_RECORDING_DIR))?;
    Ok(Some(s.fs.data_root().join(rel)))
}

/// **接线点**：把这一拍的模型客户端按当前配置包一层（或原样返回）。
///
/// 返回的守卫必须持有到本拍结束——落盘发生在它 Drop 的时候。
///
/// - `Off`（默认）：返回**传进来的那一个 Arc**，`Arc::ptr_eq(&base, &out)` 成立。
/// - `Record`：返回录制器；录制器出任何问题 → 记 warn、退回 `base`，**不阻断 tick**。
/// - `Replay`：返回回放器；加载失败 → **返回 Err 让本拍失败**，绝不悄悄退回真实模型
///   （静默回落会把一次"回放"变成一次真实调用，那份对比结果就是假的）。
pub(crate) fn wrap_tick_model(
    world_id: &str,
    tick_no: i64,
    base: Arc<dyn ModelClient>,
    world_fs: &Arc<dyn HostFs>,
    labels: &[(&str, &str)],
) -> Result<(Arc<dyn ModelClient>, TickCaptureGuard), ApiError> {
    match resolve_capture(world_id) {
        TickCapture::Off => Ok((base, TickCaptureGuard::off())),
        TickCapture::Record { recording_id, root, source } => {
            match attach_record(world_id, tick_no, &base, world_fs, &recording_id, root.as_deref(), &source, labels)
            {
                Ok(Some(m)) => Ok((m, TickCaptureGuard { world_id: Some(world_id.to_string()) })),
                // 触顶：停止录制，已录部分留在盘上。
                Ok(None) => Ok((base, TickCaptureGuard::off())),
                Err(e) => {
                    tracing::warn!(
                        target: "muse::runtime::record",
                        world_id, tick_no, error = %e,
                        "录制接线失败 → 本拍按未录制跑（录制是观测面，不得阻断世界）"
                    );
                    Ok((base, TickCaptureGuard::off()))
                }
            }
        }
        TickCapture::Replay { recording_id, root, match_mode } => {
            let m = attach_replay(world_id, tick_no, world_fs, &recording_id, root.as_deref(), match_mode)
                .map_err(|e| {
                    ApiError::internal(EngineError::Validation(format!(
                        "回放接线失败（录制 {recording_id}）：{e}。\
                         🔴 回放**不会**退回真实模型——退回去这次对比就是假的，故本拍直接失败。"
                    )))
                })?;
            Ok((m, TickCaptureGuard::off()))
        }
    }
}

/// 返回 `Ok(None)` = 已触顶，本拍不录。
#[allow(clippy::too_many_arguments)]
fn attach_record(
    world_id: &str,
    tick_no: i64,
    base: &Arc<dyn ModelClient>,
    world_fs: &Arc<dyn HostFs>,
    recording_id: &str,
    root: Option<&Path>,
    source: &str,
    labels: &[(&str, &str)],
) -> Result<Option<Arc<dyn ModelClient>>, EngineError> {
    validate_recording_id(recording_id)?;
    let mut map = sessions().lock().unwrap();
    // 先只做"要不要建"的判断（各分支都不产出借用），建完再取——否则 `map.get()` 的借用会活到
    // 匹配结果被用完，与同一分支里的 `map.insert()` 打架。
    match map.get(world_id) {
        Some(Session::Record(_)) => {}
        Some(Session::Replay(_)) => {
            return Err(EngineError::Conflict(format!(
                "世界 {world_id} 已有一个回放会话在跑，不能同时录制（先 end_world_capture）"
            )))
        }
        None => {
            let swap = Arc::new(SwapModel::new(base.clone()));
            let mut client = RecordingClient::new(swap.clone(), recording_id)
                .with_label("worldId", world_id)
                .with_label(SOURCE_LABEL_KEY, source)
                .with_label("engineVersion", muse_engine::ENGINE_VERSION);
            for (k, v) in labels {
                client = client.with_label(*k, *v);
            }
            map.insert(
                world_id.to_string(),
                Session::Record(RecordSession {
                    client: Arc::new(client),
                    swap,
                    fs: capture_fs(root, world_fs),
                    capped_warned: AtomicBool::new(false),
                }),
            );
        }
    }
    let Some(Session::Record(entry)) = map.get(world_id) else { unreachable!("上面刚保证过是录制会话") };

    if entry.client.call_count() >= MAX_RECORDED_CALLS {
        if !entry.capped_warned.swap(true, Ordering::SeqCst) {
            tracing::warn!(
                target: "muse::runtime::record",
                world_id, tick_no, cap = MAX_RECORDED_CALLS,
                "录制已达调用条数上限 → 停止录制（已录部分保留在盘上）。\
                 录制全量驻留内存，无上限就是慢性 OOM"
            );
        }
        return Ok(None);
    }

    // 每拍两件事：把内层换成本拍真正在用的 client；对齐拍号（同 golden `ScriptedModel::set_tick`）。
    entry.swap.set(base.clone());
    entry.client.set_beat(tick_no);
    Ok(Some(entry.client.clone() as Arc<dyn ModelClient>))
}

fn attach_replay(
    world_id: &str,
    tick_no: i64,
    world_fs: &Arc<dyn HostFs>,
    recording_id: &str,
    root: Option<&Path>,
    match_mode: MatchMode,
) -> Result<Arc<dyn ModelClient>, EngineError> {
    validate_recording_id(recording_id)?;
    let mut map = sessions().lock().unwrap();
    match map.get(world_id) {
        Some(Session::Replay(_)) => {}
        Some(Session::Record(_)) => {
            return Err(EngineError::Conflict(format!(
                "世界 {world_id} 已有一个录制会话在跑，不能同时回放（先 end_world_capture）"
            )))
        }
        None => {
            let fs = capture_fs(root, world_fs);
            let rec = Recording::load(fs.as_ref(), &recording_rel(recording_id))?;
            map.insert(
                world_id.to_string(),
                Session::Replay(ReplaySession { client: Arc::new(ReplayClient::with_mode(rec, match_mode)) }),
            );
        }
    }
    let Some(Session::Replay(entry)) = map.get(world_id) else { unreachable!("上面刚保证过是回放会话") };
    let client = entry.client.clone();
    client.set_beat(tick_no);
    Ok(client as Arc<dyn ModelClient>)
}

#[cfg(test)]
mod tests;
