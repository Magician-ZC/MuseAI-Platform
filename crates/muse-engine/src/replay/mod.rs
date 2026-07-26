//! 录制-回放 `ModelClient`（`docs/VALIDATION.md` §4.1 标注的「唯一真新建件」）。
//!
//! ## 它要回答的问题
//!
//! > **换了模型之后，角色还是不是它自己？**
//!
//! 这个问题现有工装一律回答不了，而且它们自己写着回答不了：
//! - `server/src/runtime/golden.rs` 的 `ScriptedModel` 回放的是**人写的剧本**，按定义永远不 OOC；
//! - `server/src/runtime/simulation.rs` 的 `SimModel` 是**种子驱动的规则化假模型**
//!   （报告里 `QualitySource::SimulatedStub` 就是为了防止有人把它读成质量验证）；
//! - `narrative` 测试里的 `RecordingModel` 录的是 **prompt 输入**，不是 response。
//!
//! 本模块补的是这一层：**用真实模型跑一遍并把每次调用的入参/出参录下来，之后换模型 / 换 Prompt /
//! 换引擎版本时对着同一份录制回放或比对差异。**
//!
//! ## ⚠️ 状态语言（VALIDATION §0.3）：本模块最高只到 `Implemented`
//!
//! 交付的是**能回答那个问题的工具**，不是**那个问题的答案**。跑通本模块的测试
//! **不得**被表述为「角色一致性已验证」——那需要真实模型录制 + 人评/评分口径，两样都不在这里。
//!
//! ## 三个件
//!
//! | 件 | 类型 | 做什么 |
//! |---|---|---|
//! | 录制 | [`RecordingClient`] | 包装**任意** `ModelClient`，透传调用并把入参/出参落成 [`Recording`] |
//! | 回放 | [`ReplayClient`] | 从 [`Recording`] 读回，同样入参给同样出参；**未录制的调用明确失败** |
//! | 比较 | [`diff::diff_recordings`] | 两份录制对齐到「哪一拍 · 哪个角色 · 哪个环节」并给出字段级差异 |
//!
//! ## 🔴 回放不会静默回落到真实模型
//!
//! [`ReplayClient`] **结构上没有 `inner` 字段**——回落不是「被禁止」，是「写不出来」。
//! 未命中一律返回 `EngineError::NotFound`（非 retryable、非 `ModelOutput`，故
//! [`crate::model::json_call`] 会**早退**而不是把重试次数烧光），同时把未命中记进
//! [`ReplayClient::report`]。这条是硬要求：静默回落会让一次「回放」偷偷变成一次真实调用，
//! 那份对比结果就是假的。
//!
//! ## 确定性契约（禁三样：系统随机 / 浮点 RNG / map 迭代序驱动 RNG）
//!
//! 本模块**一个随机源都没有**，且刻意消除了两处会让产物不可复现的东西：
//!
//! 1. **不录墙钟**：不记 latency、`recordedAtMs` 默认 0（要记由调用方显式传入）。
//!    调用延迟已由 `EngineEvent::ModelCall` 观测，录制里再记一份只会让产物不可比。
//! 2. **不按到达序落盘**：引擎的角色决策是**分批并发**的（`narrative/mod.rs` §2b
//!    `DECIDE_CONCURRENCY` + `join_all`），完成顺序取决于各次调用的真实耗时，**不可复现**。
//!    故 [`RecordingClient::finish`] 按**规范序**（拍 → 角色 → 环节 → 槽内序号）排序落盘，
//!    `seq` 是规范序里的位置而不是墙钟到达序。槽内序号仍按到达序编号——同一 (拍, 角色, 环节)
//!    的多次调用（重试 / 底线重生成）在引擎里恒是**串行**的，故它可复现。
//! 3. 所有映射一律 `BTreeMap`/`BTreeSet`，浮点温度以**定点毫值**（`temperature_milli`）入录。
//!
//! ## 🔑 凭据绝不入录
//!
//! [`RecordedCall`] **没有 `api_key` 字段**；`base_url` 落盘前剥掉 query 与 userinfo
//! （见 [`sanitize_base_url`]）；system / user / content / 错误消息统一过一遍
//! [`redact`]——把本次调用的 api_key 原文替换成 `<redacted>`（防某些网关把 key 回显进错误体）。
//!
//! ## 产物存放：复用 golden fixture 的两条约定
//!
//! - **随代码入库的固定录制**：放在模块旁的 `fixtures/` 目录，`include_str!` 编译期内联
//!   （同 `golden/cards.json` + `golden/skeleton.json`）→ [`Recording::from_json`]。
//! - **运行期产出的录制**：一律走宿主注入的 [`HostFs`]（相对 `data_root`），
//!   默认目录 [`DEFAULT_RECORDING_DIR`] → [`Recording::save`] / [`Recording::load`]。
//!   **绝不写 `muse-objects/`** 之类 gitignore 的运行时目录之外的地方，路径合法性由 `HostFs` 兜。
//!
//! ## 平台轨接线（**已接**，2026-07-27 任务 #46）
//!
//! 接线在 `server/src/runtime/record.rs`，接点是 `runtime::process_tick_inner` 第 9 步
//! ——模型客户端在整条 tick 路径上的唯一出口，故 `process_tick`（生产）与
//! `process_tick_with_model`（golden / simulation 注入）都被覆盖。**默认关闭**：
//! 未配置时接线点原样返回传进去的那一个 `Arc`（`Arc::ptr_eq` 成立，中间没有任何一层包装），
//! 所以默认路径逐字节零变化。开关见该文件头（`MUSE_TICK_RECORD` / `MUSE_TICK_REPLAY` …）。
//!
//! ⚠️ 接线 ≠ 结论：**至今没有任何一份真实模型录制**（需用户自己的 API Key），
//! 「差异多大算 OOC」的评分口径也还没有。本模块与接线合起来仍只到 `Implemented`。
//!
//! 下面是接法的最小形态（平台轨的实际实现多了会话管理、落盘守卫与降级纪律）：
//!
//! ```ignore
//! // 录制：拿生产路径跑一遍真实模型，逐拍对齐拍号
//! let rec = Arc::new(RecordingClient::new(real_model, "golden-changan-main@modelX"));
//! for tick_no in 0..N {
//!     rec.set_beat(tick_no);                       // 同 golden 的 ScriptedModel::set_tick
//!     process_tick_with_model(state, world_id, tick_no, rec.clone()).await?;
//! }
//! rec.finish().save(&*fs, Path::new(DEFAULT_RECORDING_DIR))?;
//!
//! // 回放：换 Prompt / 换引擎版本后，对着同一份录制重跑
//! let replay = Arc::new(ReplayClient::new(Recording::load(&*fs, &rel)?));
//! ...
//! assert!(replay.report().missed == 0);            // 有未录制调用 = 管线调用构成变了
//!
//! // 比较：换模型再录一份，对齐到「哪一拍 · 哪个角色 · 哪个环节」
//! let d = diff_recordings(&rec_model_x, &rec_model_y, &DiffOptions::default());
//! println!("{}", d.render_text());
//! ```

pub mod diff;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::EngineError;
use crate::host::{CancelFlag, HostFs};
use crate::model::{ModelCallSpec, ModelClient, ModelInterface, ModelOutput};

/// 录制格式版本。**改结构必须递增**：[`Recording::from_json`] 对不上就拒绝加载，
/// 而不是让一份旧录制在新结构下被解析成半截数据、再拿去当对比基线。
pub const RECORDING_FORMAT_VERSION: u32 = 1;

/// 运行期录制的默认目录（相对 `HostFs::data_root`）。
pub const DEFAULT_RECORDING_DIR: &str = "recordings";

/// 「未标注拍号」。与 `golden::ANY_TICK` 取同一个值（-1），读代码的人不用换脑子。
pub const UNLABELED_BEAT: i64 = -1;

/// 「与角色无关的环节」（director / writer / critic / arbiter）。
pub const NO_CHARACTER: &str = "";

/// 凭据被抹掉后的占位。
pub const REDACTED: &str = "<redacted>";

// ============================================================================
// §1 摘要与脱敏：确定性 + 凭据绝不入录
// ============================================================================

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 稳定内容摘要（SHA-256 前 128 位）。各段之间插 `\u{1f}` 分隔，避免
/// `("ab","c")` 与 `("a","bc")` 拼出同一个摘要。
pub fn content_digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0x1f]);
    }
    hex_lower(&h.finalize())[..32].to_string()
}

/// base_url 脱敏：剥掉 `?query`（有网关把 key 塞在 query 里）与 `user:pass@` userinfo。
/// 保留 scheme + host + path —— 那是「录的是哪个服务」这条信息，对比时有用且不敏感。
pub fn sanitize_base_url(raw: &str) -> String {
    let no_query = raw.split(['?', '#']).next().unwrap_or("").to_string();
    // 只在 "scheme://" 之后找 '@'，避免把路径里的 '@' 当 userinfo。
    let Some(sep) = no_query.find("://") else {
        return no_query;
    };
    let (scheme, rest) = no_query.split_at(sep + 3);
    match rest.find('@') {
        Some(at) => format!("{scheme}{REDACTED}@{}", &rest[at + 1..]),
        None => no_query,
    }
}

/// 把凭据原文从任意文本里抹掉。空串与过短的串不处理（避免把正文误伤成马赛克）。
pub fn redact(text: &str, secret: &str) -> String {
    if secret.trim().len() < 8 {
        return text.to_string();
    }
    text.replace(secret, REDACTED)
}

// ============================================================================
// §2 录制数据结构
// ============================================================================

/// 录制元信息。**不参与任何比对**（比对只看调用本身），只用于「这份录制是谁、什么时候、
/// 拿什么模型录的」这类溯源问题。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingMeta {
    pub format_version: u32,
    /// 录制标识；同时是默认落盘文件名（`<id>.json`），故字符集受限（见 [`validate_recording_id`]）。
    pub recording_id: String,
    /// 录制时的引擎版本（`crate::ENGINE_VERSION`）。
    pub engine_version: String,
    /// 本次录制里出现过的模型标识（去重定序）。换模型对比时，这里就是「换的是哪两个」。
    pub models: Vec<String>,
    /// 出现过的 prompt 版本（去重定序）。
    pub prompt_versions: Vec<String>,
    /// 调用方自填的溯源标签（world_id / 模板版本 / 场景名 …）。`BTreeMap` 定序。
    pub labels: BTreeMap<String, String>,
    /// 录制起始墙钟毫秒。**默认 0**：录墙钟会让两次录制不可逐字节比对，
    /// 要记由调用方显式 [`RecordingClient::with_recorded_at`] 传入。
    pub recorded_at_ms: i64,
    /// 录制期间发现的**可见退化**（不阻断录制，但必须让人看见）。
    /// 目前唯一来源：roleDecide 调用解析不出角色 id（`decide` 的 prompt 包裹改了）。
    pub warnings: Vec<String>,
}

/// 一次模型调用的完整入参与出参。
///
/// 🔑 **没有 api_key 字段**——凭据不是「记了再脱敏」，是根本没有落点。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordedCall {
    /// 规范序里的位置（**不是墙钟到达序**，见模块文档「确定性契约」第 2 条）。
    pub seq: u32,
    /// 对齐三元组之一：拍号。未标注为 [`UNLABELED_BEAT`]。
    pub beat: i64,
    /// 对齐三元组之二：角色 id。与角色无关的环节为 [`NO_CHARACTER`]。
    pub character: String,
    /// 对齐三元组之三：环节名（director / roleDecide / arbiter / writer / critic）。
    pub agent: String,
    /// 同一 (拍, 角色, 环节) 内的第几次调用（重试 / 底线重生成会 >0）。
    pub occurrence: u32,
    pub run_id: String,
    pub prompt_version: String,
    pub model: String,
    pub interface: ModelInterface,
    /// 已脱敏（见 [`sanitize_base_url`]）。
    pub base_url: String,
    /// 温度的**定点毫值**（`round(t * 1000)`）——浮点直接入 JSON 会带格式歧义。
    pub temperature_milli: i64,
    pub max_output_tokens: u32,
    pub system: String,
    pub user: String,
    /// `sha256(agent ‖ system ‖ user)` 前 128 位。回放默认按它查表，比对时按它判「prompt 变没变」。
    pub prompt_digest: String,
    pub outcome: RecordedOutcome,
}

/// 出参：成功的 content + token 计量，或失败的错误（错误同样要能回放——
/// 「模型这一刻返回了 429」也是管线必须能重现的局面）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum RecordedOutcome {
    #[serde(rename_all = "camelCase")]
    Ok {
        content: String,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    },
    #[serde(rename_all = "camelCase")]
    Err {
        /// `EngineError::code()`，稳定错误码。
        code: String,
        message: String,
        retryable: bool,
    },
}

impl RecordedOutcome {
    fn capture(result: &Result<ModelOutput, EngineError>, secret: &str) -> Self {
        match result {
            Ok(o) => RecordedOutcome::Ok {
                content: redact(&o.content, secret),
                input_tokens: o.input_tokens,
                output_tokens: o.output_tokens,
            },
            Err(e) => RecordedOutcome::Err {
                code: e.code().to_string(),
                message: redact(&e.to_string(), secret),
                retryable: e.retryable(),
            },
        }
    }

    /// 回放时把录制还原成调用结果。错误按**稳定错误码**还原语义（retryable / cancelled /
    /// model_output 三类的下游行为完全不同，还原错了回放就不是回放）。
    pub fn to_result(&self) -> Result<ModelOutput, EngineError> {
        match self {
            RecordedOutcome::Ok { content, input_tokens, output_tokens } => Ok(ModelOutput {
                content: content.clone(),
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
            }),
            RecordedOutcome::Err { code, message, retryable } => Err(match code.as_str() {
                "cancelled" => EngineError::Cancelled,
                "model_output" => EngineError::ModelOutput(message.clone()),
                "budget" => EngineError::BudgetExhausted(message.clone()),
                "validation" => EngineError::Validation(message.clone()),
                "not_found" => EngineError::NotFound(message.clone()),
                "conflict" => EngineError::Conflict(message.clone()),
                "serde" => EngineError::Serde(message.clone()),
                "io" => EngineError::Io(message.clone()),
                _ => EngineError::Model { message: message.clone(), retryable: *retryable },
            }),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, RecordedOutcome::Ok { .. })
    }

    /// 可比对文本：成功取 content，失败取 `code: message`。
    pub fn text(&self) -> String {
        match self {
            RecordedOutcome::Ok { content, .. } => content.clone(),
            RecordedOutcome::Err { code, message, .. } => format!("{code}: {message}"),
        }
    }

    pub fn status(&self) -> &'static str {
        match self {
            RecordedOutcome::Ok { .. } => "ok",
            RecordedOutcome::Err { .. } => "err",
        }
    }
}

/// 一份完整录制。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub meta: RecordingMeta,
    pub calls: Vec<RecordedCall>,
}

/// 录制 id 的合法字符集：它同时是文件名，必须挡住路径穿越与奇怪字符。
pub fn validate_recording_id(id: &str) -> Result<(), EngineError> {
    if id.is_empty() || id.len() > 120 {
        return Err(EngineError::Validation(format!("录制 id 长度非法（1-120）: {id:?}")));
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@')) {
        return Err(EngineError::Validation(format!(
            "录制 id 只允许 [A-Za-z0-9-_.@]（它同时是文件名）: {id:?}"
        )));
    }
    if id.starts_with('.') {
        return Err(EngineError::Validation("录制 id 不能以 '.' 开头".into()));
    }
    Ok(())
}

impl Recording {
    /// 序列化：`serde_json::to_string_pretty`，字段序 = 结构体声明序（稳定），
    /// 故同一份录制的两次序列化逐字节相等，可直接进 git diff。
    pub fn to_json(&self) -> Result<String, EngineError> {
        serde_json::to_string_pretty(self).map_err(EngineError::serde)
    }

    pub fn from_json(raw: &str) -> Result<Self, EngineError> {
        let rec: Recording = serde_json::from_str(raw).map_err(EngineError::serde)?;
        if rec.meta.format_version != RECORDING_FORMAT_VERSION {
            return Err(EngineError::Validation(format!(
                "录制格式版本不匹配：文件 {} vs 当前 {RECORDING_FORMAT_VERSION}（旧录制不得当新基线用）",
                rec.meta.format_version
            )));
        }
        validate_recording_id(&rec.meta.recording_id)?;
        Ok(rec)
    }

    /// 落盘到 `<rel_dir>/<recordingId>.json`（宿主注入的 FS，路径合法性由 `HostFs` 兜底）。
    /// 返回写入的相对路径。
    pub fn save(&self, fs: &dyn HostFs, rel_dir: &Path) -> Result<PathBuf, EngineError> {
        validate_recording_id(&self.meta.recording_id)?;
        let rel = rel_dir.join(format!("{}.json", self.meta.recording_id));
        fs.write_atomic(&rel, self.to_json()?.as_bytes())?;
        Ok(rel)
    }

    pub fn load(fs: &dyn HostFs, rel: &Path) -> Result<Self, EngineError> {
        let bytes = fs.read(rel)?;
        let raw = String::from_utf8(bytes).map_err(EngineError::serde)?;
        Self::from_json(&raw)
    }

    /// 列出目录下的录制文件（`HostFs::list` 已定序）。
    pub fn list(fs: &dyn HostFs, rel_dir: &Path) -> Result<Vec<PathBuf>, EngineError> {
        Ok(fs
            .list(rel_dir)?
            .into_iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect())
    }

    /// 自检：结构自洽 + 没有凭据形状的残留。返回问题清单（空 = 通过）。
    /// 拿一份来路不明的录制当基线之前跑一遍。
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.meta.format_version != RECORDING_FORMAT_VERSION {
            issues.push(format!("格式版本 {} ≠ {RECORDING_FORMAT_VERSION}", self.meta.format_version));
        }
        if let Err(e) = validate_recording_id(&self.meta.recording_id) {
            issues.push(e.to_string());
        }
        for (i, c) in self.calls.iter().enumerate() {
            if c.seq as usize != i {
                issues.push(format!("第 {i} 条 seq={} 与位置不符（录制应按规范序落盘）", c.seq));
            }
            let expect = content_digest(&[&c.agent, &c.system, &c.user]);
            if c.prompt_digest != expect {
                issues.push(format!("第 {i} 条 promptDigest 与 (agent, system, user) 不符——录制被手改过？"));
            }
            if c.agent.is_empty() {
                issues.push(format!("第 {i} 条 agent 为空"));
            }
            for (field, text) in [("system", &c.system), ("user", &c.user), ("baseUrl", &c.base_url)] {
                if looks_like_credential(text) {
                    issues.push(format!("第 {i} 条 {field} 疑似含凭据（sk-/Bearer 形状）"));
                }
            }
        }
        issues.extend(self.meta.warnings.iter().map(|w| format!("录制期告警：{w}")));
        issues
    }

    /// 按 (拍, 角色, 环节, 槽内序号) 建索引 —— 比对与回放共用同一套对齐口径。
    pub(crate) fn slot_index(&self) -> BTreeMap<SlotKey, usize> {
        self.calls
            .iter()
            .enumerate()
            .map(|(i, c)| (SlotKey::of(c), i))
            .collect()
    }
}

/// 凭据形状的启发式探测（只用于 `validate` 的提醒，不做拦截——正文里出现 "Bearer " 也可能是正常内容）。
fn looks_like_credential(text: &str) -> bool {
    text.contains("sk-") && text.split("sk-").nth(1).map(|t| t.len() >= 16).unwrap_or(false)
}

/// 对齐槽位：**这就是「哪一拍 · 哪个角色 · 哪个环节」那句话的类型形式**。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SlotKey {
    pub beat: i64,
    pub character: String,
    pub agent: String,
    pub occurrence: u32,
}

impl SlotKey {
    fn of(c: &RecordedCall) -> Self {
        Self {
            beat: c.beat,
            character: c.character.clone(),
            agent: c.agent.clone(),
            occurrence: c.occurrence,
        }
    }

    /// 人读形式（报告里的一行标题）。
    pub fn label(&self) -> String {
        let beat = if self.beat == UNLABELED_BEAT { "-".to_string() } else { self.beat.to_string() };
        let ch = if self.character.is_empty() { "-" } else { self.character.as_str() };
        let occ = if self.occurrence == 0 { String::new() } else { format!(" #{}", self.occurrence + 1) };
        format!("拍 {beat} · 角色 {ch} · 环节 {}{occ}", self.agent)
    }
}

// ============================================================================
// §3 角色标注：把 roleDecide 调用归到具体角色
// ============================================================================

/// 从 `ModelCallSpec` 提取角色 id 的策略。宿主可替换（平台轨的 prompt 包裹若有别的约定）。
pub type CharacterLabeler = Arc<dyn Fn(&ModelCallSpec) -> String + Send + Sync>;

/// `decide::build_decide_user_prompt` 的固定前缀。
///
/// 🔴 **耦合登记**：包裹变了这里就解析不出角色 id。处理方式不是静默退化——
/// [`RecordingClient`] 会把「roleDecide 未能解析角色」记进 `meta.warnings`，
/// 于是它随录制 JSON 一起走，比对报告里也会看到 `角色 -`。
const DECIDE_PROMPT_PREFIX: &str = "以下是【仅你（";

/// 从决策 prompt 头部解析角色 id；非该格式返回空串。
pub fn character_of_decide_prompt(user: &str) -> String {
    let Some(head) = user.strip_prefix(DECIDE_PROMPT_PREFIX) else {
        return String::new();
    };
    head.split('）').next().unwrap_or_default().to_string()
}

/// 默认标注：只有 `roleDecide` 环节有角色维度，其余环节归到 [`NO_CHARACTER`]。
pub fn default_character_labeler() -> CharacterLabeler {
    Arc::new(|spec: &ModelCallSpec| {
        if spec.agent == "roleDecide" {
            character_of_decide_prompt(&spec.user)
        } else {
            String::new()
        }
    })
}

// ============================================================================
// §4 录制端
// ============================================================================

/// 录制期的临时条目：带**到达序**，`finish()` 时才编槽内序号并排成规范序。
struct Pending {
    arrival: u32,
    beat: i64,
    character: String,
    agent: String,
    run_id: String,
    prompt_version: String,
    model: String,
    interface: ModelInterface,
    base_url: String,
    temperature_milli: i64,
    max_output_tokens: u32,
    system: String,
    user: String,
    outcome: RecordedOutcome,
}

/// 包装任意 `ModelClient`：调用透传给内层，入参与出参落进录制。
///
/// 用法与 golden 的 `ScriptedModel` 同构（都是 `ModelClient` 实现、都有 `set_beat`/`set_tick`
/// 对齐拍号），可以直接顶替它进 `process_tick_with_model`。
pub struct RecordingClient {
    inner: Arc<dyn ModelClient>,
    recording_id: String,
    labels: BTreeMap<String, String>,
    recorded_at_ms: i64,
    beat: AtomicI64,
    arrival: AtomicU32,
    pending: Mutex<Vec<Pending>>,
    warnings: Mutex<BTreeSet<String>>,
    labeler: CharacterLabeler,
}

impl RecordingClient {
    pub fn new(inner: Arc<dyn ModelClient>, recording_id: impl Into<String>) -> Self {
        Self {
            inner,
            recording_id: recording_id.into(),
            labels: BTreeMap::new(),
            recorded_at_ms: 0,
            beat: AtomicI64::new(UNLABELED_BEAT),
            arrival: AtomicU32::new(0),
            pending: Mutex::new(Vec::new()),
            warnings: Mutex::new(BTreeSet::new()),
            labeler: default_character_labeler(),
        }
    }

    /// 溯源标签（world_id / 模板版本 / 场景名 …）。
    pub fn with_label(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.labels.insert(k.into(), v.into());
        self
    }

    /// 显式写入录制墙钟。**不传就是 0**——默认不录墙钟是为了让产物可逐字节比对。
    pub fn with_recorded_at(mut self, ms: i64) -> Self {
        self.recorded_at_ms = ms;
        self
    }

    pub fn with_character_labeler(mut self, labeler: CharacterLabeler) -> Self {
        self.labeler = labeler;
        self
    }

    /// 对齐拍号：每拍开始前调一次（同 golden `ScriptedModel::set_tick`）。
    pub fn set_beat(&self, beat: i64) {
        self.beat.store(beat, Ordering::SeqCst);
    }

    pub fn call_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// 生成录制快照（可多次调用，不清空内部状态）。
    ///
    /// 排序在这里发生：按 (拍, 角色, 环节, 槽内序号) 排规范序，`seq` = 规范序位置。
    /// 槽内序号按到达序编 —— 同槽调用在引擎里恒串行，故可复现；跨槽的并发到达序**不入产物**。
    pub fn finish(&self) -> Recording {
        let pending = self.pending.lock().unwrap();

        // ① 按到达序编槽内序号（同槽串行 ⇒ 可复现）。
        let mut order: Vec<&Pending> = pending.iter().collect();
        order.sort_by_key(|p| p.arrival);
        let mut occ_counter: BTreeMap<(i64, String, String), u32> = BTreeMap::new();
        let mut with_occ: Vec<(SlotKey, &Pending)> = Vec::with_capacity(order.len());
        for p in order {
            let k = (p.beat, p.character.clone(), p.agent.clone());
            let occ = occ_counter.entry(k).or_insert(0);
            with_occ.push((
                SlotKey {
                    beat: p.beat,
                    character: p.character.clone(),
                    agent: p.agent.clone(),
                    occurrence: *occ,
                },
                p,
            ));
            *occ += 1;
        }

        // ② 排规范序并落 seq。
        with_occ.sort_by(|a, b| a.0.cmp(&b.0));
        let calls: Vec<RecordedCall> = with_occ
            .into_iter()
            .enumerate()
            .map(|(i, (slot, p))| RecordedCall {
                seq: i as u32,
                beat: slot.beat,
                character: slot.character,
                agent: slot.agent,
                occurrence: slot.occurrence,
                run_id: p.run_id.clone(),
                prompt_version: p.prompt_version.clone(),
                model: p.model.clone(),
                interface: p.interface,
                base_url: p.base_url.clone(),
                temperature_milli: p.temperature_milli,
                max_output_tokens: p.max_output_tokens,
                system: p.system.clone(),
                user: p.user.clone(),
                prompt_digest: content_digest(&[&p.agent, &p.system, &p.user]),
                outcome: p.outcome.clone(),
            })
            .collect();

        let models: Vec<String> = calls.iter().map(|c| c.model.clone()).collect::<BTreeSet<_>>().into_iter().collect();
        let prompt_versions: Vec<String> =
            calls.iter().map(|c| c.prompt_version.clone()).collect::<BTreeSet<_>>().into_iter().collect();

        Recording {
            meta: RecordingMeta {
                format_version: RECORDING_FORMAT_VERSION,
                recording_id: self.recording_id.clone(),
                engine_version: crate::ENGINE_VERSION.to_string(),
                models,
                prompt_versions,
                labels: self.labels.clone(),
                recorded_at_ms: self.recorded_at_ms,
                warnings: self.warnings.lock().unwrap().iter().cloned().collect(),
            },
            calls,
        }
    }
}

#[async_trait]
impl ModelClient for RecordingClient {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        let arrival = self.arrival.fetch_add(1, Ordering::SeqCst);
        let beat = self.beat.load(Ordering::SeqCst);
        let character = (self.labeler)(spec);
        if spec.agent == "roleDecide" && character.is_empty() {
            // 静默退化会让整份录制的角色维度失效（所有决策挤进同一个槽），必须让人看见。
            self.warnings.lock().unwrap().insert(
                "roleDecide 调用未能解析角色 id（decide 的 prompt 包裹变了？）——本次录制的角色维度不完整"
                    .to_string(),
            );
        }

        // 透传：录制器不改变任何调用行为（含取消语义 —— cancel 检查交给内层）。
        let result = self.inner.complete(spec, cancel).await;

        let secret = spec.profile.api_key.as_str();
        self.pending.lock().unwrap().push(Pending {
            arrival,
            beat,
            character,
            agent: spec.agent.clone(),
            run_id: spec.run_id.clone(),
            prompt_version: spec.prompt_version.clone(),
            model: spec.profile.model.clone(),
            interface: spec.profile.interface,
            base_url: sanitize_base_url(&spec.profile.base_url),
            temperature_milli: (spec.temperature * 1000.0).round() as i64,
            max_output_tokens: spec.max_output_tokens,
            system: redact(&spec.system, secret),
            user: redact(&spec.user, secret),
            outcome: RecordedOutcome::capture(&result, secret),
        });
        result
    }
}

// ============================================================================
// §5 回放端
// ============================================================================

/// 查表口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchMode {
    /// **默认**：按 `(agent, system, user)` 的摘要查表。同样的入参 ⇒ 同样的出参，
    /// 这才是「回放」的字面含义。prompt 变了就查不到 ⇒ 明确失败。
    #[default]
    Prompt,
    /// 按 (拍, 角色, 环节, 槽内序号) 查表。**回答的是另一个问题**：
    /// 「Prompt 改了，但先不管模型会怎么答，只想看管线在同样的响应下还走不走得通」。
    /// 🔴 命中但 prompt 摘要不同时会记一条 `PromptDrift` 警告 —— 这种命中**不能**当作
    /// 「新 Prompt 下模型也会这么答」的证据。
    Slot,
}

/// 未命中的一次调用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReplayMiss {
    pub beat: i64,
    pub character: String,
    pub agent: String,
    pub prompt_digest: String,
    pub reason: MissReason,
    /// prompt 开头片段，便于人肉定位「多出来的这次调用是什么」。
    pub user_excerpt: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MissReason {
    /// 录制里根本没有这个调用（管线新增了调用 / prompt 变了）。
    NoSuchCall,
    /// 录制里有，但已经被消费完（管线对同一入参多调了几次，比如重试次数变多）。
    Exhausted,
}

/// 命中但**非查表字段**发生漂移。不阻断回放，但必须出现在报告里。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReplayWarning {
    pub slot: String,
    pub field: String,
    pub recorded: String,
    pub requested: String,
}

/// 回放报告。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReplayReport {
    pub recording_id: String,
    pub recorded_calls: usize,
    pub served: usize,
    pub missed: usize,
    pub misses: Vec<ReplayMiss>,
    pub warnings: Vec<ReplayWarning>,
    /// 录制里从未被取用的条目（说明这次跑的调用比录制时**少**）。
    pub unused: Vec<String>,
}

impl ReplayReport {
    /// 一次「干净回放」：零未命中、零未取用。**这不代表内容质量结论**，只代表
    /// 这次跑的调用构成与录制时逐一对上了。
    pub fn is_exact(&self) -> bool {
        self.missed == 0 && self.unused.is_empty()
    }
}

/// 回放客户端：从录制读回，同样入参给同样出参。
///
/// 🔴 **没有 inner 字段** —— 未命中时**没有**任何东西可以回落，静默变成真实调用在结构上不可能。
pub struct ReplayClient {
    recording: Recording,
    mode: MatchMode,
    /// 查表键 → 录制条目下标（按 seq 定序）。
    index: BTreeMap<String, Vec<usize>>,
    /// 查表键 → 已消费个数。
    cursors: Mutex<BTreeMap<String, usize>>,
    misses: Mutex<Vec<ReplayMiss>>,
    warnings: Mutex<Vec<ReplayWarning>>,
    served: AtomicU32,
    beat: AtomicI64,
    labeler: CharacterLabeler,
}

fn slot_lookup_key(beat: i64, character: &str, agent: &str) -> String {
    format!("{beat}\u{1f}{character}\u{1f}{agent}")
}

impl ReplayClient {
    pub fn new(recording: Recording) -> Self {
        Self::with_mode(recording, MatchMode::default())
    }

    pub fn with_mode(recording: Recording, mode: MatchMode) -> Self {
        let mut index: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, c) in recording.calls.iter().enumerate() {
            let key = match mode {
                MatchMode::Prompt => c.prompt_digest.clone(),
                MatchMode::Slot => slot_lookup_key(c.beat, &c.character, &c.agent),
            };
            index.entry(key).or_default().push(i);
        }
        Self {
            recording,
            mode,
            index,
            cursors: Mutex::new(BTreeMap::new()),
            misses: Mutex::new(Vec::new()),
            warnings: Mutex::new(Vec::new()),
            served: AtomicU32::new(0),
            beat: AtomicI64::new(UNLABELED_BEAT),
            labeler: default_character_labeler(),
        }
    }

    pub fn with_character_labeler(mut self, labeler: CharacterLabeler) -> Self {
        self.labeler = labeler;
        self
    }

    /// 对齐拍号（`MatchMode::Slot` 必需；`Prompt` 模式下只用于未命中报告的定位信息）。
    pub fn set_beat(&self, beat: i64) {
        self.beat.store(beat, Ordering::SeqCst);
    }

    pub fn recording(&self) -> &Recording {
        &self.recording
    }

    pub fn report(&self) -> ReplayReport {
        let cursors = self.cursors.lock().unwrap();
        let mut consumed: BTreeSet<usize> = BTreeSet::new();
        for (key, used) in cursors.iter() {
            if let Some(idxs) = self.index.get(key) {
                consumed.extend(idxs.iter().take(*used).copied());
            }
        }
        let unused: Vec<String> = self
            .recording
            .calls
            .iter()
            .enumerate()
            .filter(|(i, _)| !consumed.contains(i))
            .map(|(_, c)| SlotKey::of(c).label())
            .collect();
        let misses = self.misses.lock().unwrap().clone();
        ReplayReport {
            recording_id: self.recording.meta.recording_id.clone(),
            recorded_calls: self.recording.calls.len(),
            served: self.served.load(Ordering::SeqCst) as usize,
            missed: misses.len(),
            misses,
            warnings: self.warnings.lock().unwrap().clone(),
            unused,
        }
    }

    fn note_drift(&self, slot: &str, field: &str, recorded: &str, requested: &str) {
        if recorded != requested {
            self.warnings.lock().unwrap().push(ReplayWarning {
                slot: slot.to_string(),
                field: field.to_string(),
                recorded: recorded.to_string(),
                requested: requested.to_string(),
            });
        }
    }
}

#[async_trait]
impl ModelClient for ReplayClient {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let beat = self.beat.load(Ordering::SeqCst);
        let character = (self.labeler)(spec);
        let digest = content_digest(&[&spec.agent, &spec.system, &spec.user]);
        let key = match self.mode {
            MatchMode::Prompt => digest.clone(),
            MatchMode::Slot => slot_lookup_key(beat, &character, &spec.agent),
        };

        let idx = {
            let mut cursors = self.cursors.lock().unwrap();
            let used = cursors.entry(key.clone()).or_insert(0);
            match self.index.get(&key) {
                Some(idxs) if *used < idxs.len() => {
                    let i = idxs[*used];
                    *used += 1;
                    Some(i)
                }
                Some(_) => None, // 有这个键，但被消费完了
                None => None,
            }
        };

        let Some(i) = idx else {
            let reason = if self.index.contains_key(&key) { MissReason::Exhausted } else { MissReason::NoSuchCall };
            let miss = ReplayMiss {
                beat,
                character,
                agent: spec.agent.clone(),
                prompt_digest: digest,
                reason,
                user_excerpt: spec.user.chars().take(60).collect(),
            };
            let label = format!(
                "拍 {} · 角色 {} · 环节 {}",
                miss.beat,
                if miss.character.is_empty() { "-" } else { &miss.character },
                miss.agent
            );
            self.misses.lock().unwrap().push(miss);
            // 🔴 明确失败，绝不回落。NotFound 既非 retryable 也非 ModelOutput ⇒
            // `json_call` 会**早退**，不会把重试次数烧光后给出一个误导性的「模型输出错误」。
            return Err(EngineError::NotFound(format!(
                "回放未命中（{reason:?}）：{label}；录制 {} 里没有匹配的调用。\
                 回放不会回落到真实模型 —— 请确认管线的调用构成 / prompt 是否已改变。",
                self.recording.meta.recording_id
            )));
        };

        let call = &self.recording.calls[i];
        let slot = SlotKey::of(call).label();
        // 命中但非查表字段漂移：不阻断，但进报告。
        self.note_drift(&slot, "model", &call.model, &spec.profile.model);
        self.note_drift(
            &slot,
            "temperature",
            &call.temperature_milli.to_string(),
            &((spec.temperature * 1000.0).round() as i64).to_string(),
        );
        self.note_drift(
            &slot,
            "maxOutputTokens",
            &call.max_output_tokens.to_string(),
            &spec.max_output_tokens.to_string(),
        );
        if self.mode == MatchMode::Slot {
            self.note_drift(&slot, "promptDigest", &call.prompt_digest, &digest);
        }

        self.served.fetch_add(1, Ordering::SeqCst);
        call.outcome.to_result()
    }
}

#[cfg(test)]
mod tests;
