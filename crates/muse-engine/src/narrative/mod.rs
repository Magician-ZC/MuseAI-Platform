//! P2 自主叙事引擎：回合编排（规格 §12.1）。
//!
//! 回合：导演设局 → 活跃角色并发 role_decide → 底线硬约束闸（人设保险第 1 级，违反自己卡的底线
//! → 拒绝提案并重新生成，见 §7 与 `arbiter::screen_bottom_lines`）→ 仲裁（规则→模型）→ 场景写作
//! → 确定性不变量检查（失败阻断）→ narrative_critic（建议）→ reducer 生成校验 StatePatch
//! → 原子提交 → DomainEvent 发射 → 下一场景 / 章节停止点。
//!
//! 文件所有权：mod.rs 归 agent-E4；state/reducer/constraints/snapshot 归 agent-E3。

pub mod arbiter;
pub mod constraints;
pub mod continuity;
pub mod decide;
pub mod reducer;
pub mod relation_dynamics;
pub mod snapshot;
pub mod state;
pub mod types;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures::future::join_all;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::character::types::CharacterCardV2;

/// role_decide 并发上限：全员同步决策时限制对模型的并发请求数，兼顾吞吐与外部限流。
const DECIDE_CONCURRENCY: usize = 8;

// ---------- P2 DES（异步时间线，Phase 1）时间常量 ----------
// 游戏时间单位为抽象量（server 侧可映射 ms/分钟等）。clamp/兜底防「同角色 duration<=0 永远抢占
// 最小 T 而饿死其它角色」「畸形大值溢出」「blocked/gated 同一 T 反复重试锁死」三类风险。

/// 默认行动耗时：模型未给 duration（或给 0/负）时的兜底。
pub const DEFAULT_DURATION: i64 = 60;
/// 行动耗时下限（clamp 下界，> 0 保证 next_time 严格前进）。
pub const MIN_DURATION: i64 = 1;
/// 行动耗时上限（clamp 上界，防畸形大值）。
pub const MAX_DURATION: i64 = 1_000_000;
/// blocked/gated 后 cohort next_time 的兜底推进量（防同一 T 反复重试锁死 → 饿死）。
pub const RETRY_STEP: i64 = 30;

/// 【人设保险 第 1 级】提案违反角色自己卡上的底线时的**重新生成上限**（每拍、每角色）。
/// **参数化集中点**（VALIDATION §0.2：产品规则不写死）。
///
/// 为什么默认只给 1 次：重生成是真实的模型调用，成本与延迟随它线性上涨（§17 成本工程）；
/// 而回执里已经把被违反的底线原文当硬约束钉死了，一次改不过来的，多来几次多半也改不过来
/// （decide 温度通常很低，重复采样收益递减）。改不过来就走降级序列（拦下该提案 → 整拍延后），
/// 而不是无限重试把一拍拖成天价。
pub const MAX_BOTTOM_LINE_REGEN: usize = 1;
use crate::host::{CancelFlag, EngineEvent, EngineHost, ModelCallLog};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;
use types::*;

/// 回合各环节 prompt（默认值在前端 settings store；平台侧在 server 配置层）。
pub struct NarrativePrompts {
    pub director_system: String,
    pub decide_system: String,
    pub arbiter_system: String,
    pub writer_system: String,
    pub critic_system: String,
    pub prompt_version: String,
}

/// 每环节可独立模型（§12.4 分级路由：决策低价模型、写作主力模型）；未配置的环节回退 default。
pub struct ModelRoutes {
    pub default: ModelProfile,
    pub decide: Option<ModelProfile>,
    pub arbiter: Option<ModelProfile>,
    pub writer: Option<ModelProfile>,
    pub critic: Option<ModelProfile>,
    pub director: Option<ModelProfile>,
}

impl ModelRoutes {
    pub fn for_stage(&self, stage: &str) -> &ModelProfile {
        match stage {
            "decide" => self.decide.as_ref().unwrap_or(&self.default),
            "arbiter" => self.arbiter.as_ref().unwrap_or(&self.default),
            "writer" => self.writer.as_ref().unwrap_or(&self.default),
            "critic" => self.critic.as_ref().unwrap_or(&self.default),
            "director" => self.director.as_ref().unwrap_or(&self.default),
            _ => &self.default,
        }
    }
}

/// ⚠️ **加字段前先读这条**：`RoundInput` 有**三个宿主构造点**，且都是穷尽式结构体字面量
/// （没有 `..Default::default()`），所以加字段会让它们**同时编译失败**：
///   - 平台轨世界线 `server/src/runtime/mod.rs`（约 :2520）
///   - 平台轨的 if 线推进模块（`server/src/` 下另起的一条私人分叉线，后加的第三处）
///     ← 这两处 `cargo build --manifest-path server/Cargo.toml` 都会当场报出来
///   - **桌面轨 `src-tauri/src/commands/narrative.rs`（约 :123）** ← 历史上两次都漏了这个
/// 桌面轨漏掉的代价是 CI 的 `rust-test` job 与 `npm run tauri build` 一起红，
/// 而只跑 `cargo test --manifest-path server/Cargo.toml` 是发现不了的。
/// 加字段时请一并更新：本结构体、`Default for RoundInput`、上述三个构造点、本文件内的测试助手。
///
/// （本文件按红线**不得出现 if 线的模块名**：server 侧有一条静态扫描用例守着
/// 「平行线永远进不了原世界的引擎决策」，故此处只描述、不写那个路径字面量。）
pub struct RoundInput {
    pub run_id: String,
    pub mode: RunMode,
    /// 活跃角色（2–5，上限由调用方校验）及其 DNA 卡
    pub active_cards: BTreeMap<String, CharacterCardV2>,
    /// 其他角色的一句话第三人称摘要（防注入：不注原文）
    pub other_cards_brief: BTreeMap<String, String>,
    /// 各角色**自己**的开局站位展示名（平台总规格 §5【拍板 4、5】：身份 = 开局站位）：
    /// `cid → 展示名`（如 `户部主事`）。**只读感知通道**。
    ///
    /// 为何单开一条：`other_cards_brief` 在 `decide::assemble_visible_context` 里恒**剔除自己**，
    /// 于是「你在这个世界是户部主事」只有别人看得见、角色本人看不见。本字段把本人那一条补上
    /// （`run_round` 只把 `self_identities[cid]` 喂给 `cid` 本人，绝不外泄给他人 —— 信息边界）。
    ///
    /// 🔴 平权宪法（总规格 §0.1 真红线 1）：身份**不携带任何数值差异、准入门槛、产出加成、
    /// 难度优待或叙事特权**。本字段**只**流向 `assemble_visible_context` 的展示层 JSON；
    /// 引擎内没有任何判定读它（仲裁 / 确定性不变量 / reducer / StatePatch / 同意门控 /
    /// 关系演化 / 里程碑强度一律不引用），也绝不写回 `NarrativeState`。它更不进
    /// `active_cards`（角色卡不可变 DNA 快照，污染它等于篡改玩家的卡），
    /// 也不进 `whispers`（那是「主人托梦」，有配额且会被标 applied，语义完全不同）。
    ///
    /// 默认空 = 老世界 / 模板未声明身份池 → 上下文里根本不出现该字段，逐字节退化为接线前。
    /// 平台由 runtime 从 `assembled_json` 的 `identityAssignments` 回灌；桌面壳恒默认空。
    ///
    /// 注：`RoundInput` 不派生 serde（纯进程内入参，无 `#[serde(default)]` 可用），
    /// 「不传即默认」由 `Default for RoundInput` 保证。
    /// 确定性：`BTreeMap` 键有序，纯读纯拼接，无随机、不依赖迭代序产生分支。
    pub self_identities: BTreeMap<String, String>,
    /// **观众礼物等环境事件**（平台 `livegate` 注入；桌面壳恒空）。
    ///
    /// # 🔴 这个字段买到的是「被看见」，不是「影响力」
    ///
    /// 产品 2026-07-28 拍板（`docs/build/open-decisions.md` §5 选项 A）：
    /// **观众打赏买到的是一个被看见的机会，不是任何形式的优势。**
    /// 平台红线第一条是「不卖胜负与数值平权」——礼物一旦影响判定，平台就是在卖优势，
    /// 而那件事**一旦被玩家感知为「打赏有用」，撤回就等于承认平台卖过优势**。
    ///
    /// 因此本字段的边界与 [`RoundInput::self_identities`] **同等强度**，且逐条列明：
    ///
    /// - **只**流向 `decide::assemble_visible_context` 的展示层 JSON（`ambient` 一格）；
    /// - 引擎内**没有任何判定读它**：仲裁（rule / model）、确定性不变量、reducer / `StatePatch`、
    ///   同意门控、关系演化、里程碑强度**一律不引用**；
    /// - **绝不写回** `NarrativeState`；
    /// - 不进 `active_cards`（不可变卡快照），不进 `whispers`
    ///   （那是「主人托梦」，有配额、会被标 `applied`，语义完全不同）。
    ///
    /// 🔴 **命名刻意不叫 `boons`**：`boon`（增益）这个词本身就暗示数值好处，
    /// 而它恰恰**不是**增益。名字是给下一个人看的第一道边界。
    ///
    /// # 为什么是 `Vec` 而不是按角色的 map
    ///
    /// 礼物是**公开的、场上的**——所有人看到同一份，没有「谁的观众多谁看到更多」这回事。
    /// 按角色分发会立刻造出一条可优化的差异通道，那正是要避免的。
    ///
    /// 默认空 = 桌面壳 / 未开启该通道 → 上下文里根本不出现 `ambient`，逐字节退化为接线前。
    /// 确定性：`Vec` 由调用方定序（server 侧按 `created_at, id` 取），纯读纯拼接、无随机。
    pub ambient_events: Vec<AmbientEvent>,
    /// 各角色的主人托梦（可空；平台/交互模式注入）
    pub whispers: BTreeMap<String, String>,
    /// 各角色本回合检索片段（P1 集成；由调用方按绑定与时间边界取得）
    pub fragments: BTreeMap<String, Vec<crate::knowledge::types::RetrievedFragment>>,
    pub temperature_decide: f32,
    pub temperature_writer: f32,
    pub max_output_tokens: u32,
    pub budget: RoundBudget,
    /// 已获批的不可逆结果 subject（角色 id）；本回合命中的 subject 可落定其不可逆结果
    /// （角色死亡/永久退场/永久关系变更），未命中的产 ConsentRequested 并门控不落定（REMEDIATION #3）。
    /// 默认空 = 无授权（所有不可逆结果一律门控，保守安全）；平台由 runtime 回灌，桌面壳默认空。
    pub approved_consents: Vec<String>,
    /// 世界固有角色（NPC/反派）id 集合：这些 subject 无主人可授权，其不可逆结果由同意门控
    /// 自动放行（等价于已获批，不产 ConsentRequested、不记 pending_consents）。默认空 = 无世界固有角色，
    /// 退化为纯玩家门控行为。平台由 runtime 从 assembled_json 的 worldCharacterEntries 组装回灌，桌面壳默认空。
    pub world_controlled: Vec<String>,
    /// 本回合地点图（Phase 2）：id → 地点定义。**静态**，随 RoundInput 每 tick 由调用方传入（后端无状态）。
    /// 空 = 无地点维度，全体角色归入单组 ""，完全退化为 Phase 1 单场景行为（成本恒 N+4）。
    /// 平台由 runtime 从 assembled_json 的 locationGraph 组装回灌，桌面壳默认空。
    pub locations: BTreeMap<String, LocationDef>,
    /// DES 调度时钟提示（P2 Phase 1）：本步 cohort 的激活游戏时刻 `T`。`run_event_step` 传入 = 本步 `T`；
    /// interval 模式（`run_round` 直调，老世界）默认 0。run_round 用它给事件打 `timestamp`、给
    /// `decision_id` 加时间段（防同角色跨步撞 id）。**不影响任何既有裁决/落定逻辑**，仅打戳与命名。
    pub now_hint: i64,
    /// 僵局打破提示（B. stall hint）：前几回合连续 blocked 未提交时，由调用方（平台 runtime）
    /// 传入的僵局原因摘要（含连续次数与最近原因）。引擎在导演设局 prompt 中织入该提示，
    /// 促使导演主动改变局势打破僵局。`None` = 无提示（默认，向后兼容；桌面壳恒 None）。
    /// **不动「Blocked 不提交」不变量**——仅影响导演 prompt 文本，不触碰裁决/提交路径。
    pub stall_hint: Option<String>,
    /// 生死契约档（规格 §11【拍板 24】）：世界级参数，与星级正交，随每回合传入（引擎不持有世界配置）。
    /// - `Consent`（**默认**）= 现行同意制，行为与历史完全一致；调用方不传即此档。
    /// - `Sanctuary` = 庇护世界：致死行动在写作前降级为重伤（`apply_lethality`），正文与事件同口径。
    /// - `Deathmatch` = 生死状世界：入场即签，`gate_consents` 不再临场征询；仲裁硬约束原样保留。
    ///
    /// 平台由 runtime 从 worlds.lethality 回灌（server 侧下一阶段接线）；桌面壳恒默认档。
    ///
    /// 注：`RoundInput` 本身不派生 serde（纯进程内入参，无 `#[serde(default)]` 可用），
    /// 「不传即默认」由 `Default for RoundInput` + `Default for Lethality` 保证；
    /// 跨进程边界（server DTO / Tauri 命令入参）的 `#[serde(default)]` 由各自的请求结构负责。
    pub lethality: Lethality,
    /// 本篇戏服（总规格 §6【拍板 3】「境界即布景」）：**全员统一**的入场导演设定。
    /// 语义、三条设计约束与红线见 [`RealmCostume`]。
    ///
    /// 引擎内**唯一**消费者是 `call_director`（织进导演设局 prompt，影响描写口吻与称谓）。
    /// 🔴 它**不是判定输入**：不进 `active_cards` / `CharacterState.resources` / 仲裁 /
    /// StatePatch / DomainEvent / 同意门控，谁也不能据它决定谁赢谁输、谁能压过谁
    /// （§6「跨体系靠风味翻译，不靠数值换算」+ §0.1 平权宪法）。
    ///
    /// `None`（默认）= 本篇无戏服 → 导演 prompt 与接线前逐字节一致；
    /// `Some(空戏服)` 与 `None` 等价（`RealmCostume::is_blank`）。
    /// 平台由 runtime 从 `assembled_json./assembly/realmTier` 回灌；桌面壳恒 `None`。
    pub realm_costume: Option<RealmCostume>,
}

impl Default for RoundInput {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            mode: RunMode::Observe,
            active_cards: BTreeMap::new(),
            other_cards_brief: BTreeMap::new(),
            // 自身开局站位默认空 = 不传 → 可见上下文里根本不出现该字段，行为与接线前逐字节一致。
            self_identities: BTreeMap::new(),
            // 环境事件默认空 = 桌面壳 / 通道未开 → 上下文里不出现 `ambient`，逐字节退化为接线前。
            ambient_events: Vec::new(),
            whispers: BTreeMap::new(),
            fragments: BTreeMap::new(),
            temperature_decide: 0.0,
            temperature_writer: 0.0,
            max_output_tokens: 0,
            budget: RoundBudget { max_total_tokens: 0, spent_tokens: 0, max_scenes: 0 },
            approved_consents: Vec::new(),
            world_controlled: Vec::new(),
            locations: BTreeMap::new(),
            now_hint: 0,
            stall_hint: None,
            // 生死契约默认档 = 同意制（现行机制）：不传 = 行为与历史零差异。
            lethality: Lethality::default(),
            // 本篇无戏服（§6）：不传 → 导演 prompt 一个字节都不多，与接线前完全一致。
            realm_costume: None,
        }
    }
}

/// 一条**观众礼物等环境事件**的展示形态。见 [`RoundInput::ambient_events`] 的边界声明。
///
/// 🔴 **这个结构里永远不该出现数值字段**（加成、权重、成功率、优先级……）。
/// 它只有「场上出现了什么」和「出现了几次」；`count` 是聚合计数（同回合同 SKU 合并，
/// 防事件风暴），**不是强度**——引擎里没有任何地方拿它做乘除或比较。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbientEvent {
    /// 展示文案（如「有人送上一束火把」）。由调用方给定，引擎原样透出。
    pub label: String,
    /// 同回合聚合次数。**计数，不是强度**。
    pub count: u32,
}

#[derive(Debug)]
pub struct RoundOutcome {
    pub scene: SceneRecord,
    pub new_state: NarrativeState,
    pub critic: continuity::CriticReport,
    pub budget: RoundBudget,
    /// 回合进入 blocked（硬节点不可满足等）时为 Some(原因)，未提交任何状态
    pub blocked: Option<String>,
}

/// run 级终态信号（P2 DES，Phase 1）。`RoundOutcome` 原本只有 `blocked`（回合级阻断），无 run 级终局
/// 出口。引擎在此**产信号**；server 侧消费停机（置 world status=ended + end_world）为后续 Phase / 第三块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
    /// 全部里程碑（`threshold.is_some()` 的 `OutlineNode`）Done/Bypassed 且里程碑集非空 → 主线完成
    /// （P1 放置房终局：从「硬节点全 Done」调和为「里程碑全 Done」，空里程碑集恒不发信号——守卫①）。
    /// `ending` 预留给第三块结局判定。
    MainlineDone { ending: Option<String> },
    /// 游戏时钟到达时间上限（`timeline.now >= time_cap`）。纯引擎自足。
    TimeCapReached,
    /// 无可调度角色（cohort 为空 / 无角色）。
    Starved,
}

/// 调度器单步返回（P2 DES，Phase 1）：让 server 知道「推进到哪个游戏时刻、哪些角色动了、是否终局」。
#[derive(Debug)]
pub struct EventStep {
    /// 本步 `run_round` 结果；终局短路（未跑回合）时为 None。
    pub outcome: Option<RoundOutcome>,
    /// 本步激活的 cohort（角色 id，字典序确定）。
    pub activated: Vec<String>,
    /// 本步激活游戏时刻 `T`（= 推进后的 `timeline.now`）。
    pub at_time: i64,
    /// 终局信号；非终局为 None。
    pub terminal: Option<Terminal>,
}

pub struct NarrativeEngine {
    pub host: Arc<EngineHost>,
}

impl NarrativeEngine {
    pub fn new(host: Arc<EngineHost>) -> Self {
        Self { host }
    }

    /// 成本预估（§12.4）：单场景调用数 = N决策 + 组数*2（每组导演+写作） + 仲裁≤1 + 审校1。
    /// 此处按**单地点组**基线估算（组数=1 → N+4）；多地点组的实际成本随组数线性放大，由 run_round
    /// 的预算硬停按「回合起始 location 分组」精确计算（Phase 2，见 run_round 成本公式）。
    pub fn estimate(&self, active_count: u32, max_output_tokens: u32, scenes: u32) -> CostEstimate {
        // 单组基线：N + 1*2 + 2 = N + 4。
        let calls = active_count + 4;
        CostEstimate {
            calls_per_scene: calls,
            estimated_tokens_low: (calls as u64) * (max_output_tokens as u64 / 4) * scenes as u64,
            estimated_tokens_high: (calls as u64) * (max_output_tokens as u64) * scenes as u64,
        }
    }

    /// 执行一个完整回合（一个场景）。取消/预算耗尽/不变量违规时不提交任何状态。
    /// 并发决策的结果按 character_id 字典序定序（§12.5.3 确定性排序）。
    pub async fn run_round(
        &self,
        routes: &ModelRoutes,
        prompts: &NarrativePrompts,
        input: RoundInput,
        cancel: &CancelFlag,
    ) -> Result<RoundOutcome, EngineError> {
        cancel.check()?;
        let host = self.host.as_ref();
        let store = state::NarrativeStore::new(self.host.fs.clone());

        let current = store.load(&input.run_id)?;
        let mut budget = input.budget.clone();
        let now = self.host.clock.now_ms();
        let tick = current.revision;
        let run_id = input.run_id.clone();

        // 活跃角色按 character_id 字典序定序（BTreeMap 键天然有序，§12.5.3 确定性）。
        let active_ids: Vec<String> = input.active_cards.keys().cloned().collect();

        // 按回合起始 location 分组（Phase 2）：groups[loc] = 该地点角色（组按 loc 字典序、组内 char_id 有序，
        // 皆 BTreeMap/有序 Vec → 全序可复现）。locations 空或角色无 location → 全体归入单组 ""，退化为 Phase 1。
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for cid in &active_ids {
            let loc = current.characters.get(cid).map(|c| c.location.clone()).unwrap_or_default();
            groups.entry(loc).or_default().push(cid.clone());
        }
        // 角色 → 所在组 location（决策/仲裁/写作分组索引）。
        let char_loc: BTreeMap<String, String> = groups
            .iter()
            .flat_map(|(loc, members)| members.iter().map(move |m| (m.clone(), loc.clone())))
            .collect();

        // 预算硬停（§12.4）：成本 = N决策 + 每组(导演1+写作1) + 仲裁≤1 + 审校1 = N + 组数*2 + 2（最坏）。
        // 单组（locations 空）时恒等 N+4，退化路径成本不变。
        let group_count = groups.len() as u64;
        let calls = active_ids.len() as u64 + group_count * 2 + 2;
        let scene_cost = calls.saturating_mul(input.max_output_tokens as u64);
        if budget.max_scenes == 0
            || budget.spent_tokens.saturating_add(scene_cost) > budget.max_total_tokens
        {
            return Err(EngineError::BudgetExhausted(format!(
                "预算不足：本场景约需 {scene_cost} tokens，剩余 {}（不提交任何状态）",
                budget.max_total_tokens.saturating_sub(budget.spent_tokens)
            )));
        }

        // 各环节 prompt 包装。
        let decide_prompts = decide::DecidePrompts {
            system: prompts.decide_system.clone(),
            prompt_version: prompts.prompt_version.clone(),
        };
        let arbiter_prompts = arbiter::ArbiterPrompts {
            system: prompts.arbiter_system.clone(),
            prompt_version: prompts.prompt_version.clone(),
        };
        let critic_prompts = continuity::CriticPrompts {
            system: prompts.critic_system.clone(),
            prompt_version: prompts.prompt_version.clone(),
        };

        // 1) 逐组导演设局（Phase 2）：每个地点组各生成局势，确定性按 loc 字典序（BTreeMap 迭代）。
        let mut situations: BTreeMap<String, String> = BTreeMap::new();
        for (loc, members) in &groups {
            cancel.check()?;
            let s = call_director(
                host,
                routes.for_stage("director"),
                &prompts.director_system,
                &prompts.prompt_version,
                input.max_output_tokens,
                &run_id,
                &current,
                members,
                loc,
                input.stall_hint.as_deref(),
                // 本篇戏服（§6【拍板 3】）：**全员统一** ⇒ 多地点组时每组导演拿到的是**同一件**，
                // 不按地点分化（分化就成了「这个院子里的人更强」，那是数值差不是布景）。
                input.realm_costume.as_ref(),
                cancel,
            )
            .await?;
            situations.insert(loc.clone(), s);
        }
        // 合并局势（供全局仲裁 prompt / SceneRecord.situation / stub）：按 loc 序拼接。
        let situation = merge_situations(&situations);

        // 2) 活跃角色【并发】role_decide（§12.1 设计意图）——全员同步决策：并发发起、
        //    限并发度 DECIDE_CONCURRENCY，收集后按 character_id 字典序定序，
        //    与串行确定性等价（§12.5.3：结果只依赖角色集合与共享局势，不依赖完成顺序）。
        // 2b) 按 DECIDE_CONCURRENCY 分批并发：批内 join_all 并发发起、批间串行（限流），
        //     Box::pin 擦除 future 具体类型以避开 async 闭包捕获引用的 HRTB。
        // 每角色分组决策上下文（Phase 2）：所在组 situation + 同组 others brief 子集 + 同组在场集。
        // 秘境隔离铁律在此落实——不同地点角色互不进对方 assemble_visible_context（brief/situation 皆按组过滤）。
        let decide_ctx: BTreeMap<String, DecideCtxInputs> = active_ids
            .iter()
            .map(|cid| {
                let loc = char_loc.get(cid).cloned().unwrap_or_default();
                let members = groups.get(&loc).cloned().unwrap_or_default();
                let members_set: std::collections::BTreeSet<&String> = members.iter().collect();
                let brief: BTreeMap<String, String> = input
                    .other_cards_brief
                    .iter()
                    .filter(|(k, _)| *k != cid && members_set.contains(k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let situation = situations.get(&loc).cloned().unwrap_or_default();
                (cid.clone(), DecideCtxInputs { situation, brief, members })
            })
            .collect();

        let empty_frags: Vec<crate::knowledge::types::RetrievedFragment> = Vec::new();
        let decide_stage = routes.for_stage("decide");
        let (cur_ref, inp_ref, dp_ref, rid_ref, ef_ref, dc_ref) =
            (&current, &input, &decide_prompts, &run_id, &empty_frags, &decide_ctx);
        type DecideFut<'a> =
            std::pin::Pin<Box<dyn std::future::Future<Output = Result<RoleDecision, EngineError>> + Send + 'a>>;
        let mut decisions: Vec<RoleDecision> = Vec::with_capacity(active_ids.len());
        // 单角色决策失败的【确定性降级】（LLM 鲁棒性）：`json_call` 已在内部按 `DEFAULT_MAX_RETRIES`
        // 重试（空 content / 脏 JSON / 可重试模型错误）；若某角色重试耗尽仍失败，本回合【跳过】该角色
        // （不进 `decisions`）而非 abort 整个 `run_round`——其余角色照常裁决/写作/原子提交。
        //
        // 为何跳过而非注入 benign 决策：空 action 经 `arbiter::rule_arbitrate` 会判 Success（rule:clear），
        // 从而产生 ActionResolved 事件 + pacingNote + 里程碑强度累积，污染叙事与推进（见规格风险 #3）；
        // 跳过则零副作用——仲裁/`build_patch`/`build_events` 皆按实际 decisions 迭代，天然兼容缺角色，
        // 不变量 I2/I3 只校验「事件 actor/target ⊆ active」不要求「active ⊆ 有决策者」，故不受影响。
        //
        // 确定性：结果按 `chunk` + `cid` 顺序 zip 收集（`join_all` 保序，与并发完成顺序无关），
        // 同一失败输入恒跳过同一角色，无随机源；DES `next_time` 对缺席角色兜底 `DEFAULT_DURATION`
        // 前进（`run_event_step`），不会饿死。
        // 边界：`Cancelled` 必须原样传播（绝不降级）；非模型类错误（`Serde`/`Io` 等引擎缺陷）fail-hard
        //       上抛以免掩盖真实 bug；仅「模型类」错误（`ModelOutput` / `Model`）降级。
        let mut degraded_count: usize = 0;
        let mut last_degrade_err: Option<EngineError> = None;
        for chunk in active_ids.chunks(DECIDE_CONCURRENCY) {
            let futs: Vec<DecideFut<'_>> = chunk
                .iter()
                .map(|cid| {
                    Box::pin(async move {
                        cancel.check()?;
                        let card = &inp_ref.active_cards[cid];
                        let frags = inp_ref.fragments.get(cid).unwrap_or(ef_ref);
                        let whisper = inp_ref.whispers.get(cid).map(|s| s.as_str());
                        // 自身开局站位（§5【拍板 4、5】）：**只**取 `cid` 自己那一条喂给 `cid` 本人。
                        // 信息边界：他人的自身身份条目绝不进入本角色上下文（他人身份仍只走 brief）。
                        // 🔴 平权（§0.1）：纯展示层，本回合的仲裁/落定/事件/不变量一概不读它。
                        let self_identity = inp_ref.self_identities.get(cid).map(|s| s.as_str());
                        let dctx = &dc_ref[cid];
                        let ctx = decide::assemble_visible_context(
                            cur_ref, cid, card, &dctx.brief, &dctx.situation, frags, whisper,
                            self_identity,
                            // 🔴 全体同一份：礼物是公开的、场上的，不按角色分发
                            //（按角色分发会造出「谁的观众多谁看到更多」这条差异通道）。
                            &inp_ref.ambient_events,
                        )?;
                        decide::role_decide(
                            host, decide_stage, dp_ref, inp_ref.temperature_decide,
                            inp_ref.max_output_tokens, rid_ref, inp_ref.now_hint, cid, &ctx,
                            &dctx.members, cancel,
                        )
                        .await
                    }) as DecideFut<'_>
                })
                .collect();
            for (cid, r) in chunk.iter().zip(join_all(futs).await) {
                match r {
                    Ok(d) => decisions.push(d),
                    // 取消必须原样传播，绝不降级为跳过。
                    Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
                    // 模型类错误：确定性降级——跳过该角色，发观测事件（供告警面板区分「瞬态自愈」vs「真实故障」）。
                    Err(e @ (EngineError::ModelOutput(_) | EngineError::Model { .. })) => {
                        host.events.emit(EngineEvent::ModelCall(ModelCallLog {
                            run_id: run_id.clone(),
                            agent: "roleDecide".to_string(),
                            prompt_version: prompts.prompt_version.clone(),
                            model_id: decide_stage.model.clone(),
                            input_tokens: None,
                            output_tokens: None,
                            latency_ms: 0,
                            retries: 0,
                            error: Some(format!("character_degraded:{cid}:{}", e.code())),
                        }));
                        degraded_count += 1;
                        last_degrade_err = Some(e);
                    }
                    // 非模型类错误（引擎内部缺陷）：fail-hard 上抛，不掩盖。
                    Err(e) => return Err(e),
                }
            }
        }
        // 全部活跃角色都降级 → 合理失败（不静默提交空回合），交上层 tick 重试/暂停。
        if !active_ids.is_empty() && degraded_count == active_ids.len() {
            return Err(last_degrade_err
                .unwrap_or_else(|| EngineError::ModelOutput("全部角色决策失败".into())));
        }
        decisions.sort_by(|a, b| a.character_id.cmp(&b.character_id));

        // 2.5) 【人设保险 第 1 级 · 事前 · 底线硬约束】（总规格 §7）
        //
        // 规格：卡的 bottomLines/refusalRules/immutableCore 升级为仲裁硬约束——角色行为违反自己卡的
        // 底线 → 仲裁拒绝该提案、**重新生成**。这一级是「事前预防」（第 2 级注解权是事中、
        // 第 3 级 if 线是事后），事前拦住的 OOC 越多，事后申诉越少（T1 门槛：申诉 <10%/阶段）。
        //
        // **分层**：判「拦不拦」在规则层（`arbiter::screen_bottom_lines` 纯函数，确定性、零新增
        // 模型调用、可 replay）；做「怎么改」在模型层（带底线回执重跑 `role_decide`，有次数上限），
        // 改完再过同一个确定性闸复检。判定绝不交给模型——那会让提交内容挂在模型输出上，replay 破裂。
        //
        // **降级序列（对齐 VALIDATION §5「宁可停拍，不让失败输出成为永久公共事实」）**：
        //   ① 重新生成 ≤ MAX_BOTTOM_LINE_REGEN 次（把被违反的底线原文回注为硬约束 —— 对应
        //      §5 的「缩短上下文重仲裁」：不是换模型碰运气，是把约束写死再来一次）；
        //   ② 仍违规 → 该提案判 `Invalid`（`rule:bottom_line`）：不落状态、不发事件、不推里程碑、
        //      不进关系演化，写作被显式告知「并未发生」—— 对应 §5 的「延后此拍」，
        //      延后的是**该角色这一拍**，世界其余部分照常推进（不卡死）；
        //   ③ 本拍**全部**提案都被拦下 → 整拍 `blocked`、零提交 —— 对应 §5 的「暂停世界并说明原因」，
        //      由上层（server 的连续 blocked 计数 / stall_hint）接「人工复核队列」。
        //   （§5 的「过审过渡事件」需要一个过审事件库，属宿主侧资产，引擎不自造。）
        //
        // 确定性：`collect_bottom_lines`/`screen_bottom_lines` 全序纯函数；重生成按命中的定序
        // **串行**发起（不并发，避免任何完成序影响）；重生成次数只由输入决定，同输入恒同调用次数。
        let bottom_lines = arbiter::collect_bottom_lines(&input.active_cards);
        // 无任何卡声明底线 → 整段短路：不筛查、不调用、不改预算，行为与接线前逐字节一致。
        let mut bottom_line_rejects: Vec<ArbiterOutcome> = Vec::new();
        let mut regen_calls: u64 = 0;
        if !bottom_lines.is_empty() {
            let mut hits = arbiter::screen_bottom_lines(&bottom_lines, &decisions);
            let mut attempt = 0usize;
            while !hits.is_empty() && attempt < MAX_BOTTOM_LINE_REGEN {
                attempt += 1;
                for hit in &hits {
                    cancel.check()?;
                    let cid = hit.character_id.as_str();
                    let Some(idx) = decisions.iter().position(|d| d.character_id == cid) else {
                        continue;
                    };
                    let dctx = &decide_ctx[cid];
                    let base = decide::assemble_visible_context(
                        &current,
                        cid,
                        &input.active_cards[cid],
                        &dctx.brief,
                        &dctx.situation,
                        input.fragments.get(cid).unwrap_or(&empty_frags),
                        input.whispers.get(cid).map(|s| s.as_str()),
                        input.self_identities.get(cid).map(|s| s.as_str()),
                        &input.ambient_events,
                    )?;
                    let ctx = decide::append_bottom_line_rejection(
                        &base,
                        &hit.line,
                        &decisions[idx].action,
                    )?;
                    regen_calls += 1;
                    match decide::role_decide(
                        host,
                        decide_stage,
                        &decide_prompts,
                        input.temperature_decide,
                        input.max_output_tokens,
                        &run_id,
                        input.now_hint,
                        cid,
                        &ctx,
                        &dctx.members,
                        cancel,
                    )
                    .await
                    {
                        // decision_id 由 run_id/now_hint/cid 确定性派生 → 重生成后 id 不变，
                        // 原地替换不打乱 `decisions` 的既有定序。
                        Ok(nd) => decisions[idx] = nd,
                        Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
                        // 重生成本身失败（模型类错误）：保留原提案交下方 ② 拦截——
                        // 宁可这一拍这个角色不动，也不放行违背底线的行动。
                        Err(e @ (EngineError::ModelOutput(_) | EngineError::Model { .. })) => {
                            host.events.emit(EngineEvent::ModelCall(ModelCallLog {
                                run_id: run_id.clone(),
                                agent: "roleDecide".to_string(),
                                prompt_version: prompts.prompt_version.clone(),
                                model_id: decide_stage.model.clone(),
                                input_tokens: None,
                                output_tokens: None,
                                latency_ms: 0,
                                retries: 0,
                                error: Some(format!("bottom_line_regen_failed:{cid}:{}", e.code())),
                            }));
                        }
                        // 非模型类错误（引擎内部缺陷）：fail-hard 上抛，不掩盖。
                        Err(e) => return Err(e),
                    }
                }
                hits = arbiter::screen_bottom_lines(&bottom_lines, &decisions);
            }
            // ② 重试上限后仍违规 → 拦截该提案（发观测事件供告警面板 / 人工复核队列取数）。
            for hit in &hits {
                host.events.emit(EngineEvent::ModelCall(ModelCallLog {
                    run_id: run_id.clone(),
                    agent: "arbiter".to_string(),
                    prompt_version: prompts.prompt_version.clone(),
                    model_id: decide_stage.model.clone(),
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms: 0,
                    retries: attempt as u32,
                    error: Some(format!(
                        "bottom_line_rejected:{}:{}",
                        hit.character_id, hit.matched
                    )),
                }));
                bottom_line_rejects.push(arbiter::bottom_line_outcome(hit));
            }
        }

        // ③ 全部提案都被底线拦下 → 整拍延后、零提交（不写作、不审校，也不空转出一场
        //    「所有人都临阵收手」的公共事实）。上层据连续 blocked 暂停世界并入人工复核队列。
        if !decisions.is_empty() && bottom_line_rejects.len() == decisions.len() {
            let reason = format!(
                "底线硬约束：本拍全部 {} 条提案均违反角色自己卡上的底线（已重新生成 {} 次仍未通过），\
整拍延后，不提交任何状态",
                bottom_line_rejects.len(),
                MAX_BOTTOM_LINE_REGEN
            );
            let scene = stub_scene(tick, &situation, &decisions, &bottom_line_rejects, now);
            return Ok(RoundOutcome {
                scene,
                new_state: current,
                critic: continuity::CriticReport::default(),
                budget,
                blocked: Some(reason),
            });
        }

        // 3) 逐组仲裁（Phase 2）：每组独立规则层（R2 同组在场 + R6 移动连通/准入），
        //    pending 汇总后全局一次 model_arbitrate（仲裁模型调用仍 ≤1）。
        // 已被底线拦下的提案**不再进 R1–R6**：提案已死，不必再判资源/冲突，也绝不占用模型层
        // pending 名额（拦截不得抬高成本）。它们仍留在 `decisions` 里（战报可见「他提了什么、
        // 为什么被自己的底线否掉」，I2 的 source_decision_ids ⊆ 本回合决策也照旧成立）。
        let rejected_ids: BTreeSet<String> =
            bottom_line_rejects.iter().map(|o| o.decision_id.clone()).collect();
        let dmap_by_cid: BTreeMap<&str, &RoleDecision> =
            decisions.iter().map(|d| (d.character_id.as_str(), d)).collect();
        let mut outcomes: Vec<ArbiterOutcome> = bottom_line_rejects;
        let mut pending: Vec<RoleDecision> = Vec::new();
        for members in groups.values() {
            let group_decisions: Vec<RoleDecision> = members
                .iter()
                .filter_map(|m| dmap_by_cid.get(m.as_str()).map(|d| (*d).clone()))
                .filter(|d| !rejected_ids.contains(&d.decision_id))
                .collect();
            let (mut res, mut pend) =
                arbiter::rule_arbitrate(&current, &group_decisions, members, &input.locations);
            outcomes.append(&mut res);
            pending.append(&mut pend);
        }
        if !pending.is_empty() {
            cancel.check()?;
            let model_outcomes = arbiter::model_arbitrate(
                host,
                routes.for_stage("arbiter"),
                &arbiter_prompts,
                &run_id,
                &current,
                &situation,
                &pending,
                cancel,
            )
            .await?;
            outcomes.extend(model_outcomes);
        }
        // 定序（§12.5.3）。
        outcomes.sort_by(|a, b| {
            a.character_id.cmp(&b.character_id).then_with(|| a.decision_id.cmp(&b.decision_id))
        });

        // Blocked：硬节点与底线冲突不可满足 → 整回合阻断，不提交（§5.3.1）。
        if let Some(b) = outcomes.iter().find(|o| o.result == ArbiterResult::Blocked) {
            let reason = format!("仲裁阻断：{} 的行动与硬约束冲突（{}）", b.character_id, b.consequence);
            let scene = stub_scene(tick, &situation, &decisions, &outcomes, now);
            return Ok(RoundOutcome {
                scene,
                new_state: current,
                critic: continuity::CriticReport::default(),
                budget,
                blocked: Some(reason),
            });
        }

        // 3c) 生死契约档降级（规格 §11【拍板 24】·庇护世界）——**必须在场景写作(4) 之前**：
        // 写作吃的是 `group_outcomes`，若只在门控(4.5) 处拦截死亡，就会出现「正文写着他被杀死了、
        // 公共事实却是退场/未落定」的矛盾（违反 §0.3 公共事实不可回滚的推论：正文即公共事实）。
        // 故在此把致死结果就地降级为重伤，让 prose / ActionResolved.fact / pacingNotes 全部按降级后
        // 的口径生成。非庇护档（含默认 Consent）此步为 no-op，行为零变化。
        apply_lethality(&mut outcomes, &decisions, input.lethality);

        // 4) 逐组场景写作（Phase 2）：每组各写一段，合并为单 SceneRecord.prose（tick=revision 仍单值，
        //    单 patch/单 revision 原子提交契约不变）。单组时即一次写作调用，退化路径不变。
        let outcome_by_cid: BTreeMap<&str, &ArbiterOutcome> =
            outcomes.iter().map(|o| (o.character_id.as_str(), o)).collect();
        let mut prose_segments: Vec<String> = Vec::new();
        for (loc, members) in &groups {
            cancel.check()?;
            let group_decisions: Vec<RoleDecision> = members
                .iter()
                .filter_map(|m| dmap_by_cid.get(m.as_str()).map(|d| (*d).clone()))
                .collect();
            let group_outcomes: Vec<ArbiterOutcome> = members
                .iter()
                .filter_map(|m| outcome_by_cid.get(m.as_str()).map(|o| (*o).clone()))
                .collect();
            let seg = call_writer(
                host,
                routes.for_stage("writer"),
                &prompts.writer_system,
                &prompts.prompt_version,
                input.temperature_writer,
                input.max_output_tokens,
                &run_id,
                situations.get(loc).map(|s| s.as_str()).unwrap_or(""),
                &group_decisions,
                &group_outcomes,
                cancel,
            )
            .await?;
            prose_segments.push(seg);
        }
        let prose = prose_segments.join("\n\n");

        // 4.5) 不可逆结果同意门控（REMEDIATION #3 / 规格 §2.4）：
        // 分类不可逆结果（角色死亡/永久退场/永久关系变更，由 ArbiterResult 成功 + 行动语义判定）；
        // subject 全部命中 approved_consents → 正常落定并清除对应 pending；否则门控——
        // 产 ConsentRequested、剔出落定集（不落定该不可逆结果）、记 narrative.pending_consents。
        // 生死状档（Deathmatch）在此放行全部 subject（入场即签，事后不再临场征询）。
        let (committing_outcomes, consent_requests, newly_pending, approved_landed) = gate_consents(
            &decisions,
            &outcomes,
            &input.approved_consents,
            &input.world_controlled,
            input.lethality,
        );

        // 5) reducer 生成 StatePatch + DomainEvent（事件引用 patch.id，供 I3 校验）。
        // 落定集已剔除被门控的不可逆结果 → 其后果不进入 StatePatch/ActionResolved。
        let patch = build_patch(current.revision, &decisions, &committing_outcomes, &current);
        // build_events 打游戏时间戳 timestamp = 本步激活时刻 T（interval 模式 now_hint=0，退化为旧行为）。
        let mut events =
            build_events(&run_id, &patch.id, input.now_hint, &decisions, &committing_outcomes, &current);
        // 门控的不可逆结果追加 ConsentRequested（可见性 Private→当事角色），续接事件序号，同带 timestamp。
        events.extend(build_consent_events(
            &run_id,
            &patch.id,
            input.now_hint,
            events.len() as u64,
            &consent_requests,
        ));

        // 6) 确定性不变量（失败即阻断，不提交任何状态）。
        let violations =
            continuity::deterministic_invariants(&current, &decisions, &patch, &events, &prose, &active_ids);
        if !violations.is_empty() {
            let scene = SceneRecord {
                scene_id: format!("sc-{tick}"),
                tick,
                situation,
                decisions,
                outcomes,
                prose,
                events,
                state_patch: patch,
                locked: false,
                created_at: now,
            };
            return Ok(RoundOutcome {
                scene,
                new_state: current,
                critic: continuity::CriticReport::default(),
                budget,
                blocked: Some(format!("确定性不变量违规：{}", violations.join("；"))),
            });
        }

        // 7) 叙事 critic（建议，不改状态）。
        cancel.check()?;
        let critic = continuity::narrative_critic(
            host,
            routes.for_stage("critic"),
            &critic_prompts,
            &run_id,
            &prose,
            &decisions,
            cancel,
        )
        .await?;

        // 8) 原子提交（取消后不提交迟到结果）。
        cancel.check()?;
        let scene = SceneRecord {
            scene_id: format!("sc-{tick}"),
            tick,
            situation,
            decisions,
            outcomes,
            prose,
            events,
            state_patch: patch.clone(),
            locked: false,
            created_at: now,
        };
        let new_state = store.commit_scene(&run_id, &scene, &patch)?;

        // 8.5) 门控账回写：清除本回合已落定的 pending，追加未获批的新 pending。
        // pending_consents 不经 reducer 白名单（引擎门控元数据，类比 appliedPatchIds），故直接重写状态。
        let new_state =
            persist_pending_consents(host, &run_id, new_state, &newly_pending, &approved_landed)?;

        // 9) 产出 DomainEvent（宿主决定投递通道；含门控产生的 ConsentRequested）。
        for ev in &scene.events {
            host.events
                .emit(EngineEvent::Narrative { run_id: run_id.clone(), payload: serde_json::to_value(ev)? });
        }

        // 底线重生成的额外决策调用据实计费（§17 成本工程：拦截不许悄悄花钱）。
        // 未发生重生成时 `regen_calls == 0`，本行与接线前逐字节等价。
        // 注：它发生在预算硬停之后，故只可能小幅超出上限 —— 由 `.min()` 夹住，
        // 且上限本身是 `MAX_BOTTOM_LINE_REGEN × 活跃角色数`，有界。
        let regen_cost = regen_calls.saturating_mul(input.max_output_tokens as u64);
        budget.spent_tokens = budget
            .spent_tokens
            .saturating_add(scene_cost)
            .saturating_add(regen_cost)
            .min(budget.max_total_tokens);
        budget.max_scenes = budget.max_scenes.saturating_sub(1);

        Ok(RoundOutcome { scene, new_state, critic, budget, blocked: None })
    }

    /// DES 调度器单步（P2 第二块，Phase 1，规格「核心算法 run_event_step」）。
    ///
    /// **run_round 主体不重写**：本方法是它**上方**的一层调度器 ——
    /// 选 cohort（同刻最小 `next_time`）→ 过滤 `RoundInput`（active_cards 只留 cohort）→ 调
    /// 【未改动核心】`run_round`（仲裁/写作/门控/不变量/原子提交全复用）→ 按 `duration` 推进 cohort
    /// 的 `next_time` → 持久化 timeline（绕 reducer，镜像 `persist_pending_consents`）→ 检查终局。
    ///
    /// **单写者 revision 轴**：所有 cohort 提交串行到同一 revision 轴（不做 per-timeline 分支，
    /// 绕开 `snapshot.rs` 无确定性 merge 的缺口）。**现有契约零改动**：base_revision CAS / 单 patch 单
    /// revision / reducer 幂等 / commit 原子性全部由 `run_round` 原样保证；本方法只做 cohort 过滤与
    /// timeline 推进（timeline 是引擎调度元数据，与 pending_consents 同性质，不经 reducer 白名单）。
    pub async fn run_event_step(
        &self,
        routes: &ModelRoutes,
        prompts: &NarrativePrompts,
        input: RoundInput,
        cancel: &CancelFlag,
    ) -> Result<EventStep, EngineError> {
        cancel.check()?;
        let store = state::NarrativeStore::new(self.host.fs.clone());
        let run_id = input.run_id.clone();
        let state = store.load(&run_id)?;

        // 1) 终局先判（世界结束不等全员 next_time 收敛）。接第三块。
        if let Some(t) = is_terminal(&state) {
            return Ok(EventStep {
                outcome: None,
                activated: Vec::new(),
                at_time: state.timeline.now,
                terminal: Some(t),
            });
        }

        // 2) 求最小 next_time = T（缺席角色视为 now，首步全体入场）。
        //    确定性：BTreeMap 键有序遍历 + 取 min，平手落到同一 cohort 后按 character_id 定序。
        let t = select_time(&state);

        // 2b) 选 cohort（Phase 3 = 同地点碰撞组：同 location + 时间窗 [T,T+dur) 重叠，退化为「空闲于 T」；
        //     单地点/无地点世界完全退化为 Phase 1「同刻」）。cohort 内恒同 location → 下方 run_round 内单组处理。
        let cohort = select_cohort(&state, t);
        if cohort.is_empty() {
            // 无可调度角色（无角色）→ Starved。
            return Ok(EventStep {
                outcome: None,
                activated: Vec::new(),
                at_time: t,
                terminal: Some(Terminal::Starved),
            });
        }

        // 3) 过滤 RoundInput：active_cards 只留 cohort（子集），other_cards_brief 保留全体名以维持在场感知；
        //    now_hint = T 传给 run_round → build_events 打 timestamp、decision_id 加时间段。
        let filtered_cards: BTreeMap<String, CharacterCardV2> = input
            .active_cards
            .iter()
            .filter(|(k, _)| cohort.iter().any(|c| c == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut filtered = input;
        filtered.active_cards = filtered_cards;
        filtered.now_hint = t;

        // 4) 调用【未改动核心】run_round —— cohort 内仍按 character_id 定序、单 patch 单 revision 原子提交，
        //    I2「patch.source_decision_ids ⊆ 本回合决策」在单步内成立（cohort 决策就是本步全部决策）。
        let outcome = self.run_round(routes, prompts, filtered, cancel).await?;

        // 5) 计算 cohort 的 next_time 推进。
        let mut next_time = state.timeline.next_time.clone();
        let blocked = outcome.blocked.is_some();
        if blocked {
            // blocked：run_round 未提交任何状态；cohort 兜底推进防饿死（否则同一 T 反复重试锁死）。
            for c in &cohort {
                next_time.insert(c.clone(), t.saturating_add(RETRY_STEP));
            }
        } else {
            // 按 duration 推进各角色 next_time（duration 来自决策，确定性；clamp 防 0/负）。
            // gated/未落定角色的决策仍在 scene.decisions 内 → 一并推进，避免下一步立刻重抢 T。
            let dur_by_cid: BTreeMap<&str, i64> = outcome
                .scene
                .decisions
                .iter()
                .map(|d| (d.character_id.as_str(), clamp_duration(d.duration)))
                .collect();
            for c in &cohort {
                let dur = dur_by_cid.get(c.as_str()).copied().unwrap_or(DEFAULT_DURATION);
                next_time.insert(c.clone(), t.saturating_add(dur));
            }
        }

        // 6) 持久化 timeline（绕 reducer 白名单直接重写状态，镜像 persist_pending_consents mod.rs）：
        //    now=T + 推进后的 next_time。blocked 时写在未提交的旧状态上（revision 不变，仅推进 timeline）。
        let RoundOutcome { scene, new_state, critic, budget, blocked: blocked_reason } = outcome;
        let new_state = persist_timeline(self.host.as_ref(), &run_id, new_state, next_time, t)?;

        // 7) 终局复判：主线可能刚被本步推进为完成（blocked 未提交 → 不复判）。
        let terminal = if blocked { None } else { is_terminal(&new_state) };

        Ok(EventStep {
            outcome: Some(RoundOutcome { scene, new_state, critic, budget, blocked: blocked_reason }),
            activated: cohort,
            at_time: t,
            terminal,
        })
    }
}

// ---------- P2 DES 调度辅助（Phase 1，绕 reducer / 不改 run_round 主体） ----------

/// 本步激活时刻 `T` = 全体角色 `next_time` 的最小值（缺席角色视为 `timeline.now`）。无角色 → now。
/// 确定性：BTreeMap 键有序 + `min`。
fn select_time(state: &NarrativeState) -> i64 {
    let now = state.timeline.now;
    state
        .characters
        .keys()
        .map(|c| state.timeline.next_time.get(c).copied().unwrap_or(now))
        .min()
        .unwrap_or(now)
}

/// Phase 3 cohort：**同地点碰撞组**——「同 `location` + 时间窗 `[T, T+dur)` 重叠」（复用 P3 的
/// `CharacterState.location`，与 run_round 的 location 分组一致：cohort 内恒同 location → run_round 内单组处理）。
///
/// **窗口重叠在选取时精确退化为 `next_time == T`**：因 `T = min(next_time)`（`select_time`），任何
/// `next_time > T` 的角色仍处在上一动作的时间窗内（忙碌，其窗口起点 > T），不可与本步碰撞——它将在自己
/// 变空闲的那一步（`next_time` 成为新的最小值）另行成组。故「与组内窗口重叠」= 「空闲于 T」= `next_time == T`。
///
/// 在「空闲于 T」的基础上按 location 收窄为单一锚地点：
/// - `anchor` = 空闲于 T 的字典序最小角色（BTreeMap 键有序 → 确定性），其 `location` 为本步锚地点。
/// - cohort = 空闲于 T **且**与 anchor 同 `location` 的全部角色。
///
/// 由此**不同地点的角色即使 `next_time` 同为 T 也不同步行动**：它们分入不同的 `run_event_step`
/// （下一步 `select_time` 仍得 T，锚地点轮到下一个地点），逐地点串行成组，各自单独一个 revision/timestamp。
///
/// 退化（单地点）：全体角色同 `location`（含皆无地点 `""` 的老世界）→ 锚地点收窄不再剔除任何人 →
/// cohort = 空闲于 T 的全集，与 Phase 1「同刻」**完全等价**。
fn select_cohort(state: &NarrativeState, t: i64) -> Vec<String> {
    let now = state.timeline.now;
    let loc_of =
        |c: &String| state.characters.get(c).map(|s| s.location.clone()).unwrap_or_default();
    // 空闲于 T 的角色（`next_time == T`；缺席角色视为 now）。BTreeMap 键有序 → 字典序确定。
    let free_at_t: Vec<String> = state
        .characters
        .keys()
        .filter(|c| state.timeline.next_time.get(*c).copied().unwrap_or(now) == t)
        .cloned()
        .collect();
    // 锚地点 = 字典序最小空闲角色的 location；无空闲角色 → 空 cohort。
    let anchor_loc = match free_at_t.first() {
        Some(a) => loc_of(a),
        None => return Vec::new(),
    };
    // 收窄到锚地点：不同地点角色（异地/秘境）被剔除，留待各自成组。
    free_at_t.into_iter().filter(|c| loc_of(c) == anchor_loc).collect()
}

/// 行动耗时 clamp 到 `[MIN_DURATION, MAX_DURATION]`（防模型给 0/负导致同角色永远抢占最小 T 而饿死）。
fn clamp_duration(d: i64) -> i64 {
    d.clamp(MIN_DURATION, MAX_DURATION)
}

/// 终局判定（P2 DES + P1 放置房终局）：
/// - 全部**里程碑**（`threshold.is_some()` 的 `OutlineNode`）Done/Bypassed 且里程碑集非空 → `MainlineDone`
///   （引擎产信号；server 消费停机）。★守卫①：空里程碑集恒不发 MainlineDone——空 skeleton / 无阈值节点
///   （chapter/arena 的硬节点 threshold=None）绝不在空集上真空成立而秒结束。
/// - `now >= time_cap` → `TimeCapReached`（纯引擎自足）。
/// - 无任何角色 → `Starved`。
///
/// **不以「全员 next_time 耗尽」为条件**：世界结束不等落在远未来的角色（规格 `terminal_not_wait_all`）。
///
/// `pub`：**终局判定必须只有一把尺**。除 DES 调度器（`run_event_step`）之外，宿主还存在
/// 「拿一份状态跑一回合、自己判要不要收尾」的形态（如平台侧的单人平行线副本）。
/// 那类宿主若各自照着这段规则重写一遍，第一次改这里的阈值语义时就会出现「世界线收尾了、
/// 平行线还在跑」的分叉——而这正是最难被测试发现的一类不一致。导出它，让所有线共用同一把尺。
/// 🔴 只读、无副作用：它只看状态、不改状态、不产生任何结算动作。
pub fn is_terminal(state: &NarrativeState) -> Option<Terminal> {
    // 里程碑 = 带 threshold 的节点；chapter/arena 的硬节点（threshold=None）不计入 → 旧硬节点零影响。
    let milestones: Vec<&OutlineNode> =
        state.narrative.outline_nodes.iter().filter(|n| n.threshold.is_some()).collect();
    if !milestones.is_empty()
        && milestones.iter().all(|n| matches!(n.status, NodeStatus::Done | NodeStatus::Bypassed))
    {
        return Some(Terminal::MainlineDone { ending: None });
    }
    if let Some(cap) = state.timeline.time_cap {
        if state.timeline.now >= cap {
            return Some(Terminal::TimeCapReached);
        }
    }
    if state.characters.is_empty() {
        return Some(Terminal::Starved);
    }
    None
}

/// 持久化 timeline（绕 reducer 白名单直接重写状态，镜像 `persist_pending_consents`）：timeline 是引擎
/// 调度元数据（与 `pending_consents` 同性质），不经 reducer。写入 `now=T` 与推进后的 `next_time`。
fn persist_timeline(
    host: &EngineHost,
    run_id: &str,
    mut state: NarrativeState,
    next_time: BTreeMap<String, i64>,
    now: i64,
) -> Result<NarrativeState, EngineError> {
    state.timeline.now = now;
    state.timeline.next_time = next_time;
    crate::store::write_json(host.fs.as_ref(), &state::state_path(run_id), &state)?;
    Ok(state)
}

// ---------- Phase 2 分组辅助 ----------

/// 单角色的分组决策上下文：所在组 situation + 同组 others brief 子集 + 同组在场集（targets 白名单）。
struct DecideCtxInputs {
    situation: String,
    brief: BTreeMap<String, String>,
    members: Vec<String>,
}

/// 合并各地点组的局势为单串（按 loc 字典序确定性拼接）。单组时即原局势，退化路径无副作用。
fn merge_situations(situations: &BTreeMap<String, String>) -> String {
    if situations.len() == 1 {
        return situations.values().next().cloned().unwrap_or_default();
    }
    situations
        .iter()
        .map(|(loc, s)| if loc.is_empty() { s.clone() } else { format!("【{loc}】{s}") })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 解析移动目标（与 arbiter 同约定：targets 含 `loc:<id>`）。用于 build_patch 生成 location Set op
/// 与 build_events 填 from/to。
fn move_dest_of(d: &RoleDecision) -> Option<String> {
    d.targets.iter().find_map(|t| t.strip_prefix(arbiter::LOC_TARGET_PREFIX).map(|s| s.to_string()))
}

// ---------- 环节模型调用（严格 JSON，走 crate::model::json_call） ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectorOut {
    #[serde(default)]
    situation: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriterOut {
    #[serde(default)]
    prose: String,
}

/// 公共 world 层（剔除引擎内部保留幂等账键）。
pub(crate) fn public_world(state: &NarrativeState) -> BTreeMap<String, Value> {
    state
        .world
        .iter()
        // 🔴 与 `decide::assemble_visible_context` 用**同一张表**。
        // 这里此前是一个**裸字面量** `"appliedPatchIds"`——同一个键名在仓库里的第三份拷贝，
        // 而它喂的是**入场导演的 prompt**，同样是模型可见面。
        // 见 `reducer::RESERVED_WORLD_KEYS` 的注释：排除表漏一处 = 静默泄露。
        .filter(|(k, _)| !reducer::RESERVED_WORLD_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn call_director(
    host: &EngineHost,
    profile: &ModelProfile,
    system: &str,
    prompt_version: &str,
    max_output_tokens: u32,
    run_id: &str,
    state: &NarrativeState,
    active_ids: &[String],
    location_id: &str,
    stall_hint: Option<&str>,
    realm_costume: Option<&RealmCostume>,
    cancel: &CancelFlag,
) -> Result<String, EngineError> {
    let outline: Vec<Value> = state
        .narrative
        .outline_nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "summary": n.summary,
                "constraint": format!("{:?}", n.constraint),
                "status": format!("{:?}", n.status),
            })
        })
        .collect();
    // Phase 2：注入「当前地点」，令导演为该组在场角色就地设局（多地点各自独立局势）。
    let place = if location_id.is_empty() {
        String::new()
    } else {
        format!("当前地点：{location_id}\n")
    };
    // 僵局打破提示（B. stall hint）：连续 blocked 未推进时织入原因，引导导演主动改变局势
    //（不动「Blocked 不提交」不变量——僵局信息经调用方回灌，而非在 blocked 回合提交任何状态）。
    let stall = match stall_hint {
        Some(h) if !h.trim().is_empty() => format!(
            "注意：前几回合因『{h}』僵持未能推进。请主动改变局势：拆散拥挤的角色、引入新要素或转移冲突焦点，打破僵局。\n"
        ),
        _ => String::new(),
    };
    // 本篇戏服（§6【拍板 3】「境界即布景」）：入场导演的统一设定，全员同一件。
    // 🔴 末句的免责话术是**红线的一部分**，不是修辞：戏服只改「怎么描写」（水位口吻、招式称谓），
    //    不改「谁能赢」。少了它，模型很容易把「全员斗王档」读成一条战力刻度去裁强弱，
    //    而 §6 明说「跨体系靠风味翻译，不靠数值换算」。改这段文案前请先读 §6 与 §0.1 平权宪法。
    // 空戏服（两段皆空）与未声明等价 → 空串 → prompt 逐字节不变（`RealmCostume::is_blank`）。
    let costume = match realm_costume.filter(|c| !c.is_blank()) {
        None => String::new(),
        Some(c) => {
            let mut s = String::from("本篇戏服（全员统一，同一水位）：");
            let brief = c.briefing.trim();
            s.push_str(if brief.is_empty() { "（未给出说明）" } else { brief });
            s.push('\n');
            let notes: Vec<&str> =
                c.flavor_notes.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).collect();
            if !notes.is_empty() {
                s.push_str(&format!("跨体系风味翻译：{}\n", notes.join("；")));
            }
            s.push_str(
                "以上只决定本篇怎么描写——大家处在什么水位、招式与称谓译成什么风味；\
它不代表任何人更强或更弱，不得据此判定谁能赢、谁能压过谁，也不得凭它给任何角色额外的能力或特权。\n",
            );
            s
        }
    };
    let user = format!(
        "{costume}{place}{stall}当前活跃角色：{active}\n大纲节点：{outline}\n公共世界状态：{world}\n\n\
你是入场导演：为本回合设定一个把当前待推进节点自然展开的开放局势，给在场角色留出做出不同选择的空间，\
不要替角色决定他们会怎么做。严格输出 JSON：{{\"situation\":\"...\"}}",
        active = active_ids.join("、"),
        outline = serde_json::to_string(&outline).unwrap_or_default(),
        world = serde_json::to_string(&public_world(state)).unwrap_or_default(),
    );
    let spec = ModelCallSpec {
        max_retries: None,
        profile: profile.clone(),
        system: system.to_string(),
        user,
        temperature: 0.7,
        max_output_tokens,
        agent: "director".to_string(),
        prompt_version: prompt_version.to_string(),
        run_id: run_id.to_string(),
    };
    let out: DirectorOut = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(out.situation)
}

#[allow(clippy::too_many_arguments)]
async fn call_writer(
    host: &EngineHost,
    profile: &ModelProfile,
    system: &str,
    prompt_version: &str,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    situation: &str,
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    cancel: &CancelFlag,
) -> Result<String, EngineError> {
    let acts: Vec<Value> = decisions
        .iter()
        .map(|d| {
            json!({
                "characterId": d.character_id,
                "intent": d.intent,
                "action": d.action,
                "willSpeak": d.speak.will_speak,
                "purpose": d.speak.purpose,
            })
        })
        .collect();
    let res: Vec<Value> = outcomes
        .iter()
        .map(|o| {
            json!({
                "characterId": o.character_id,
                "result": format!("{:?}", o.result),
                "consequence": o.consequence,
            })
        })
        .collect();
    let user = format!(
        "局势：{situation}\n各角色意图与行动：{acts}\n仲裁结果：{res}\n\n\
据此写出本场景正文，忠实呈现各角色不可替换的选择与其后果；\
不要把任何角色未在场景中公开的私密信息写进正文。严格输出 JSON：{{\"prose\":\"...\"}}",
        acts = serde_json::to_string(&acts).unwrap_or_default(),
        res = serde_json::to_string(&res).unwrap_or_default(),
    );
    let spec = ModelCallSpec {
        max_retries: None,
        profile: profile.clone(),
        system: system.to_string(),
        user,
        temperature,
        max_output_tokens,
        agent: "writer".to_string(),
        prompt_version: prompt_version.to_string(),
        run_id: run_id.to_string(),
    };
    let out: WriterOut = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(out.prose)
}

// ---------- StatePatch / DomainEvent 生成（reducer 白名单路径；校验交 reducer） ----------

/// 本回合事件强度（P1 放置房终局里程碑推进）：Σ outcomes 折算（Success/PartialSuccess/Failure 按权重，
/// Invalid/Blocked 不计）+ Σ `willSpeak=true` 决策互动强度。**确定性**：只依赖 run_round 已定序的
/// outcomes/decisions（§12.5.3），纯函数，replay 可复现。
fn round_intensity(
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    w: &IntensityWeights,
) -> f64 {
    let mut e = 0.0;
    for o in outcomes {
        e += match o.result {
            ArbiterResult::Success => w.success,
            ArbiterResult::PartialSuccess => w.partial,
            ArbiterResult::Failure => w.failure,
            ArbiterResult::Invalid | ArbiterResult::Blocked => 0.0,
        };
    }
    for d in decisions {
        if d.speak.will_speak {
            e += w.speak;
        }
    }
    e
}

/// 由仲裁结果生成本回合 StatePatch（走 reducer 白名单路径）：每个非 Invalid 结果追加一条
/// pacingNotes（记录本回合节拍，可追溯）；并由 relation_dynamics 确定性推导本回合关系数值
/// 变更（`relations[<from>-><to>].*` Set，令 advance_when 关系谓词可被真实驱动）；
/// 有成功推进时把当前首个待推进节点标记 done。
/// **推进分两路（P1 放置房终局）**：
/// - 阈值里程碑（`threshold.is_some()`）：本回合强度累积到 `world.milestoneProgress_<id>`（Increment，
///   单段键合规），达阈值且 `advance_when` 谓词命中才翻 Done（每回合至多推首个 Pending 里程碑）。
/// - 旧式节点（`threshold=None`）：保留「有 success 就 done」兼容路径（硬/软老节点零行为变化）。
///
/// source_decision_ids 填本回合全部决策 id（继承 E3 reducer 契约）。
fn build_patch(
    base_revision: u64,
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    state: &NarrativeState,
) -> StatePatch {
    let dmap: BTreeMap<&str, &RoleDecision> =
        decisions.iter().map(|d| (d.decision_id.as_str(), d)).collect();
    let mut operations: Vec<PatchOperation> = Vec::new();
    let mut progressed = false;

    for o in outcomes {
        if o.result == ArbiterResult::Invalid {
            continue;
        }
        if matches!(o.result, ArbiterResult::Success | ArbiterResult::PartialSuccess) {
            progressed = true;
            // 移动落定（Phase 2）：合法移动 → characters.<id>.location 标量 Set 到目标地点。
            // 契约不变——仍走 reducer 白名单路径、并入本回合单 patch/单 revision 原子提交。
            if let Some(dest) = dmap.get(o.decision_id.as_str()).and_then(|d| move_dest_of(d)) {
                operations.push(PatchOperation {
                    op: PatchOp::Set,
                    path: format!("characters.{}.location", o.character_id),
                    value: Some(json!(dest)),
                    precondition: None,
                });
            }
        }
        let note = format!("{}｜{:?}｜{}", o.character_id, o.result, o.consequence);
        operations.push(PatchOperation {
            op: PatchOp::Append,
            path: "narrative.pacingNotes".to_string(),
            value: Some(json!(note)),
            precondition: None,
        });
    }

    // 关系演化（A. relation_dynamics）：由本回合已落定结果确定性推导 `relations[<from>-><to>].*`
    // 数值 Set（同键先累加、clamp 后发终值、(from,to,field) 有序），并入同一 patch 原子提交。
    // 不存在的边由 reducer 在写入时对已知角色端点零值自动建边（known_to=[from,to]）。
    operations.extend(relation_dynamics::derive_relation_ops(decisions, outcomes, state));

    if let Some(node) = constraints::next_pending(&state.narrative.outline_nodes) {
        match node.threshold {
            // 阈值里程碑（P1 放置房终局）：本回合强度累积到 milestoneProgress_<id>，达阈值 + advance_when
            // 谓词命中才翻 Done。关系维度经谓词门、事件维度经阈值累积，二者「与」= 强度累积到阈值。
            Some(threshold) => {
                let w = node.weights.clone().unwrap_or_default();
                let delta = round_intensity(decisions, outcomes, &w);
                // 单段键（reducer.rs:48-55 world.<key> 无 . / [），与保留键 appliedPatchIds 靠固定前缀隔离。
                let key = format!("milestoneProgress_{}", node.id);
                let cur = state.world.get(&key).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let next = cur + delta;
                // 只在有强度时累加（Increment 单调不减，进度键仅经此路径写入）。
                if delta > 0.0 {
                    operations.push(PatchOperation {
                        op: PatchOp::Increment,
                        path: format!("world.{key}"),
                        value: Some(json!(delta)),
                        precondition: None,
                    });
                }
                // advance_when 谓词门（复用 constraints::eval_predicate；谓词非法/实体缺失 => 未命中，不误推进）。
                let gate_ok = match &node.advance_when {
                    None => true,
                    Some(expr) => constraints::eval_predicate(
                        state,
                        &ForbiddenPredicate {
                            id: String::new(),
                            expression: expr.clone(),
                            reason: String::new(),
                        },
                    )
                    .unwrap_or(false),
                };
                if next >= threshold && gate_ok {
                    operations.push(PatchOperation {
                        op: PatchOp::Set,
                        path: format!("narrative.outlineNodes[{}].status", node.id),
                        value: Some(json!("done")),
                        precondition: None,
                    });
                }
            }
            // 旧式节点（threshold=None）：向后兼容——有 success 就推进首个 Pending 节点（硬/软老节点零变化）。
            None if progressed => {
                operations.push(PatchOperation {
                    op: PatchOp::Set,
                    path: format!("narrative.outlineNodes[{}].status", node.id),
                    value: Some(json!("done")),
                    precondition: None,
                });
            }
            None => {}
        }
    }

    StatePatch {
        id: format!("patch-{base_revision}"),
        base_revision,
        source_decision_ids: decisions.iter().map(|d| d.decision_id.clone()).collect(),
        operations,
    }
}

/// 由仲裁结果生成 DomainEvent（宿主无关、版本化）。
/// 每个非 Invalid/Blocked 结果 → 1 个 ActionResolved；willSpeak 追加 1 个 DialogueSpoken。
/// `at_time`（P2 DES）：本步 cohort 激活游戏时刻 `T`，写入每个事件 `timestamp`，与步内 `sequence`
/// 组成跨步全序 `(timestamp, sequence)`。interval 模式 `at_time=0`（退化为旧行为）。
fn build_events(
    run_id: &str,
    patch_id: &str,
    at_time: i64,
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    state: &NarrativeState,
) -> Vec<DomainEvent> {
    let dmap: BTreeMap<&str, &RoleDecision> =
        decisions.iter().map(|d| (d.decision_id.as_str(), d)).collect();
    let mut events: Vec<DomainEvent> = Vec::new();
    let mut seq: u64 = 0;

    for o in outcomes {
        if matches!(o.result, ArbiterResult::Invalid | ArbiterResult::Blocked) {
            continue;
        }
        let d = dmap.get(o.decision_id.as_str()).copied();
        // 角色目标（排除移动伪目标 loc:<id>——移动落在 fact.from/to，不进 target_ids 以免 I3 在场校验误伤）。
        let targets: Vec<String> = d
            .map(|d| d.targets.iter().filter(|t| !t.starts_with(arbiter::LOC_TARGET_PREFIX)).cloned().collect())
            .unwrap_or_default();
        let action = d.map(|d| d.action.clone()).unwrap_or_default();

        // 移动事实（Phase 2）：Success/PartialSuccess 的移动附 from（回合起始 location）/to（目标地点）。
        let mut fact = json!({
            "result": format!("{:?}", o.result),
            "action": action,
            "consequence": o.consequence,
        });
        if let Some(dest) = d.and_then(move_dest_of) {
            let from = state.characters.get(&o.character_id).map(|c| c.location.clone()).unwrap_or_default();
            fact["from"] = json!(from);
            fact["to"] = json!(dest);
        }

        // 🔴 **这个 id 在世界之间会重名，不可当唯一键用。**
        //
        // `patch_id` 形如 `patch-{base_revision}`,于是本 id 形如 `patch-0-ev-0` ——
        // 它只在**一条世界线内部**唯一,**不含任何世界维度**。两个不同世界在同一 revision 上
        // 的第 N 个事件,id 逐字相同(任意两个新世界的第一拍都有 `patch-0-ev-0`)。
        //
        // 这是刻意的:引擎是宿主无关的,它不知道"世界 id"这回事,而确定性 id 又是黄金世界
        // 回归能逐字节比对的前提。**问题不在这里,在使用方**——宿主侧若拿它当定位键写
        // `WHERE domain_event_id = $1`,就会**跨世界误伤**:2026-07-27 查实,人审队列的
        // world_event 回写路径正是这么写的,一次「通过」会把别的世界里正被拦下的同名事件
        // 一并放行。修法是 server 侧按 `(world_id, domain_event_id)` 定位到主键再改
        // (迁移 `0047` 给 `audit_queue` 补了 `subject_world_id`)。
        //
        // ⚠️ 这类缺陷**在单世界测试下永远不会暴露** —— 只有一个世界时不存在重名。
        // 任何将来按本 id 定位的代码都会重新踩它,写之前先想清楚"同名的另一个世界在哪"。
        events.push(DomainEvent {
            schema_version: 1,
            id: format!("{patch_id}-ev-{seq}"),
            run_id: run_id.to_string(),
            sequence: seq,
            timestamp: at_time,
            event_type: DomainEventType::ActionResolved,
            actor_ids: vec![o.character_id.clone()],
            target_ids: if targets.is_empty() { None } else { Some(targets) },
            fact,
            state_patch_id: patch_id.to_string(),
            caused_by: vec![o.decision_id.clone()],
            visibility: EventVisibility::Public,
        });
        seq += 1;

        if let Some(d) = d {
            if d.speak.will_speak {
                events.push(DomainEvent {
                    schema_version: 1,
                    id: format!("{patch_id}-ev-{seq}"),
                    run_id: run_id.to_string(),
                    sequence: seq,
                    timestamp: at_time,
                    event_type: DomainEventType::DialogueSpoken,
                    actor_ids: vec![o.character_id.clone()],
                    target_ids: None,
                    fact: json!({ "purpose": d.speak.purpose }),
                    state_patch_id: patch_id.to_string(),
                    caused_by: vec![o.decision_id.clone()],
                    visibility: EventVisibility::Public,
                });
                seq += 1;
            }
        }
    }
    events
}

// ---------- 生死契约档降级（规格 §11【拍板 24】·庇护世界） ----------

/// 庇护档降级后写入 `ArbiterOutcome.rule_refs` 的规则标记（透明战报可见「为什么没死」）。
const SANCTUARY_RULE_REF: &str = "lethality:sanctuaryDowngrade";

/// 庇护档降级后的 `consequence` 文案（**参数化集中点**，VALIDATION §0.2：产品规则不散落硬编码）。
///
/// 该文本同时是**给写作环节的指令**——`call_writer` 把 `consequence` 原样喂给写手，故必须
/// 显式否定死亡，否则写手会顺着决策原文（仍含「杀死」）把人写死，造成正文与公共事实矛盾。
/// 该文本同时会进入公共事实（`ActionResolved.fact.consequence` 与 `pacingNotes`），故写成可直读的
/// 中文陈述句，不夹排版记号。
const SANCTUARY_DOWNGRADE_CONSEQUENCE: &str =
    "致命一击未能取人性命：对方重伤倒地、失去行动力，但仍然活着\
（本世界为庇护世界，任何角色都不会死亡——正文不得写出任何角色的死亡）";

/// 按生死契约档对仲裁结果做**写作前**的确定性降级（纯函数式变换，无随机源、无 map 迭代序依赖：
/// 只按 `outcomes` 既有顺序原地改写，同输入恒同输出，满足 replay 契约）。
///
/// 目前仅 `Sanctuary` 有降级动作：致死结果（`is_lethal`）→ `PartialSuccess` + 重伤语义 consequence
/// + 规则标记。`Consent`/`Deathmatch` 为 no-op（默认档行为零变化）。
///
/// **调用位置铁律**：必须在场景写作之前（`run_round` 步骤 3c），因为写作吃的就是这些 outcomes；
/// 降级后 prose / `ActionResolved.fact.consequence` / `pacingNotes` 三者同源，公共事实自洽。
fn apply_lethality(
    outcomes: &mut [ArbiterOutcome],
    decisions: &[RoleDecision],
    lethality: Lethality,
) {
    if lethality != Lethality::Sanctuary {
        return;
    }
    let rules = IrreversibleRules::new();
    let dmap: BTreeMap<&str, &RoleDecision> =
        decisions.iter().map(|d| (d.decision_id.as_str(), d)).collect();
    for o in outcomes.iter_mut() {
        let Some(d) = dmap.get(o.decision_id.as_str()).copied() else {
            continue;
        };
        if !rules.is_lethal(o, d) {
            continue;
        }
        // 降级：成功致死 → 部分成功（打中了，但没打死）。
        o.result = ArbiterResult::PartialSuccess;
        o.consequence = SANCTUARY_DOWNGRADE_CONSEQUENCE.to_string();
        if !o.rule_refs.iter().any(|r| r == SANCTUARY_RULE_REF) {
            o.rule_refs.push(SANCTUARY_RULE_REF.to_string());
        }
    }
}

// ---------- 不可逆结果同意门控（REMEDIATION #3 / 规格 §2.4） ----------

/// 一个待授权的不可逆结果（用于生成 ConsentRequested 域事件）。
struct ConsentRequest {
    /// 发起该不可逆行动的角色（事件 actor）
    actor: String,
    decision_id: String,
    /// death | permanent_exit | permanent_relation_change
    event_kind: String,
    /// 当事角色 id（其主人需授权）
    subjects: Vec<String>,
    detail: String,
}

/// 门控编排：对每个仲裁结果分类不可逆性；当事角色全部「可放行」（命中 approved_consents，或属于
/// world_controlled 世界固有角色——无主人可授权，自动放行）→ 留在落定集并记入待清除 pending；
/// 否则剔出落定集、生成 ConsentRequest、记入新增 pending。非不可逆结果原样落定。
/// world_controlled subject 从不进 pending_consents（无 owner），故既不记待清除也不记新增。
///
/// **生死契约档（规格 §11【拍板 24】）在此分派**：
/// - `Consent`（默认）：如上，行为与历史完全一致。
/// - `Deathmatch`：入场即签生死状、**事后不再临场征询**——全部 subject 视为可放行
///   （等价于把玩家 subject 当 world_controlled 看待）。因 `all_landable` 恒真，
///   `ConsentRequest` 分支不可达 → 该档天然不产 ConsentRequested、不写 newly_pending。
/// - `Sanctuary`：不改门控，改的是上游 `apply_lethality` + `classify`（致死已降级为重伤，不再是
///   不可逆事件）；其余不可逆事件（永久退场/永久关系变更）仍走本函数的同意制流程。
///
/// 返回 (落定用 outcomes, 待生成 ConsentRequested, 新增 pending, 已落定待清除 pending)。
fn gate_consents(
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    approved_consents: &[String],
    world_controlled: &[String],
    lethality: Lethality,
) -> (Vec<ArbiterOutcome>, Vec<ConsentRequest>, Vec<PendingConsent>, Vec<PendingConsent>) {
    let rules = IrreversibleRules::new();
    let approved: std::collections::BTreeSet<&str> =
        approved_consents.iter().map(|s| s.as_str()).collect();
    // 世界固有角色：无主人可授权，其作为 subject 的不可逆结果一律自动放行（NPC/反派死亡等）。
    let world: std::collections::BTreeSet<&str> =
        world_controlled.iter().map(|s| s.as_str()).collect();
    // 生死状档：join 时已知情同意，本回合无须再问任何人。
    let signed = lethality == Lethality::Deathmatch;
    let dmap: BTreeMap<&str, &RoleDecision> =
        decisions.iter().map(|d| (d.decision_id.as_str(), d)).collect();

    let mut committing: Vec<ArbiterOutcome> = Vec::with_capacity(outcomes.len());
    let mut requests: Vec<ConsentRequest> = Vec::new();
    let mut newly_pending: Vec<PendingConsent> = Vec::new();
    let mut approved_landed: Vec<PendingConsent> = Vec::new();

    for o in outcomes {
        match dmap.get(o.decision_id.as_str()).and_then(|d| rules.classify(o, d, lethality)) {
            None => committing.push(o.clone()), // 非不可逆：正常落定
            Some((event_kind, subjects)) => {
                // 每个当事角色须「可放行」：生死状档（全员）/ world_controlled（自动放行）/ 已获批。
                // 全部可放行 → 落定。**注意**：能走到这里说明仲裁已判 Success/PartialSuccess——
                // 被判 Invalid/Blocked 的致死行动根本不进 classify（红线：取消的是否决权，不是裁判）。
                let landable = |s: &str| signed || world.contains(s) || approved.contains(s);
                let all_landable =
                    !subjects.is_empty() && subjects.iter().all(|s| landable(s.as_str()));
                if all_landable {
                    committing.push(o.clone());
                    for s in &subjects {
                        // world_controlled subject 从不入 pending，无需记入待清除；仅玩家 subject 记 approved_landed。
                        //
                        // 生死状档同样记 approved_landed（**取舍**）：该档本就不产 pending，正常情况下
                        // `persist_pending_consents` 的 retain 是空操作；但世界的契约档是**可配置参数**
                        // （VALIDATION §0.2），存在 Consent → Deathmatch 中途改档的合法路径，届时旧档遗留的
                        // pending 若不清除，就会在死亡已经落定之后留下一条永远悬空的「请求你同意自己的死亡」。
                        // 记账（幂等、确定性）换取改档自愈，代价仅是含不可逆结果的少数回合多一次同内容重写。
                        if !world.contains(s.as_str()) {
                            approved_landed.push(PendingConsent {
                                subject: s.clone(),
                                event_kind: event_kind.clone(),
                            });
                        }
                    }
                } else {
                    // 门控：world_controlled subject 剔除（无 owner 可授权）；仅未获批玩家 subject 记 pending / 请求。
                    let gated: Vec<String> =
                        subjects.iter().filter(|s| !world.contains(s.as_str())).cloned().collect();
                    for s in &gated {
                        newly_pending
                            .push(PendingConsent { subject: s.clone(), event_kind: event_kind.clone() });
                    }
                    requests.push(ConsentRequest {
                        actor: o.character_id.clone(),
                        decision_id: o.decision_id.clone(),
                        event_kind,
                        subjects: gated,
                        detail: o.consequence.clone(),
                    });
                }
            }
        }
    }
    (committing, requests, newly_pending, approved_landed)
}

/// 由 ConsentRequest 生成 ConsentRequested 域事件（可见性 Private→当事角色∪发起者）；
/// 事件序号从 start_seq 续接（与 build_events 共用一条序号轴）。
fn build_consent_events(
    run_id: &str,
    patch_id: &str,
    at_time: i64,
    start_seq: u64,
    requests: &[ConsentRequest],
) -> Vec<DomainEvent> {
    let mut out: Vec<DomainEvent> = Vec::with_capacity(requests.len());
    for (i, r) in requests.iter().enumerate() {
        let seq = start_seq + i as u64;
        let mut audience: Vec<String> = r.subjects.clone();
        if !audience.contains(&r.actor) {
            audience.push(r.actor.clone());
        }
        audience.sort();
        audience.dedup();
        out.push(DomainEvent {
            schema_version: 1,
            id: format!("{patch_id}-ev-{seq}"),
            run_id: run_id.to_string(),
            sequence: seq,
            timestamp: at_time,
            event_type: DomainEventType::ConsentRequested,
            actor_ids: vec![r.actor.clone()],
            target_ids: None, // 当事角色放 fact.subjectCharacterIds，避免 I3 在场校验误伤
            fact: json!({
                "eventKind": r.event_kind,
                "subjectCharacterIds": r.subjects,
                "detail": r.detail,
            }),
            state_patch_id: patch_id.to_string(),
            caused_by: vec![r.decision_id.clone()],
            visibility: EventVisibility::Private { audience_character_ids: audience },
        });
    }
    out
}

/// 门控账回写：清除已落定的 pending、去重追加新增 pending；有变更则重写状态落盘（revision 不变）。
/// pending_consents 不经 reducer 白名单（引擎门控元数据），故直接重写。
fn persist_pending_consents(
    host: &EngineHost,
    run_id: &str,
    mut new_state: NarrativeState,
    newly_pending: &[PendingConsent],
    approved_landed: &[PendingConsent],
) -> Result<NarrativeState, EngineError> {
    if newly_pending.is_empty() && approved_landed.is_empty() {
        return Ok(new_state);
    }
    for landed in approved_landed {
        new_state.narrative.pending_consents.retain(|p| p != landed);
    }
    for np in newly_pending {
        if !new_state.narrative.pending_consents.contains(np) {
            new_state.narrative.pending_consents.push(np.clone());
        }
    }
    crate::store::write_json(host.fs.as_ref(), &state::state_path(run_id), &new_state)?;
    Ok(new_state)
}

/// 不可逆行动语义分类器（预编译正则）：区分角色死亡 / 永久退场 / 永久关系变更。
/// 与 arbiter 的 irreversible_re 家族同源，但细分类别并聚焦「角色级」不可逆（不含单纯物件损毁）。
struct IrreversibleRules {
    death: Regex,
    self_death: Regex,
    exit: Regex,
    relation: Regex,
}

impl IrreversibleRules {
    // 四个 pattern 都是**字面量**，编译期即固定：`Regex::new` 只会因模式非法而失败，
    // 而模式不含任何运行期输入（模型文本只进 `is_match`，不进 `new`）。故 unwrap 是
    // 静态安全的——若有人改坏了 pattern，本 crate 任一条用例首次构造即 panic，在 CI 上暴露。
    fn new() -> Self {
        Self {
            death: Regex::new(r"(杀死|杀掉|杀了|杀害|处死|赐死|斩杀|毒死|勒死|绞死|自尽|自刎|殉|同归于尽)").unwrap(),
            self_death: Regex::new(r"(自尽|自刎|殉|同归于尽)").unwrap(),
            exit: Regex::new(r"(流放|放逐|逐出|驱逐|永远离开|远走高飞|退隐|归隐|遁入空门|出走|永别)").unwrap(),
            relation: Regex::new(r"(背叛|叛变|叛逃|反目成仇|反目|决裂|绝交|断绝)").unwrap(),
        }
    }

    /// 致死行动判定：**`apply_lethality` 降级 与 `classify` 死亡分类 的唯一共同口径**。
    /// 二者必须命中同一集合，否则会出现「正文写死了、事件却是退场/未落定」的公共事实矛盾（§0.3）。
    /// 前置条件与 `classify` 完全一致：仅「实际发生」（Success/PartialSuccess）才算致死。
    /// **红线**：`Invalid`/`Blocked`（仲裁判定行动不成立/危及硬节点）在此恒为 false —— 任何契约档
    /// 都不会让被裁判否掉的致死行动落定。
    fn is_lethal(&self, o: &ArbiterOutcome, d: &RoleDecision) -> bool {
        matches!(o.result, ArbiterResult::Success | ArbiterResult::PartialSuccess)
            && self.death.is_match(&d.action)
    }

    /// 仅「实际发生」（Success/PartialSuccess）的结果才产生不可逆后果。
    /// 返回 (eventKind, subjectCharacterIds)，非不可逆返回 None。死亡优先级最高。
    /// `lethality`：庇护档下致死行动已被 `apply_lethality` 降级为重伤，此处**不再产出 death**——
    /// 两处共用 `is_lethal` 判据，口径天然一致。
    fn classify(
        &self,
        o: &ArbiterOutcome,
        d: &RoleDecision,
        lethality: Lethality,
    ) -> Option<(String, Vec<String>)> {
        if !matches!(o.result, ArbiterResult::Success | ArbiterResult::PartialSuccess) {
            return None;
        }
        let action = d.action.as_str();
        let actor = d.character_id.as_str();

        if self.is_lethal(o, d) {
            // 庇护档：本条 outcome 已在写作前被降级为「重伤」（可恢复 → 不是不可逆事实），
            // 故整条 return None——既不产 death，也**不再落到 exit/relation 分支**（原行动语义已被
            // 降级覆盖，若改判永久退场就会与正文的「重伤但活着」再次矛盾）。
            // 与 `apply_lethality` 的降级集合逐条对应（同一个 `is_lethal`）。
            if lethality == Lethality::Sanctuary {
                return None;
            }
            let mut subjects = d.targets.clone();
            // 自尽/同归于尽：行动者本人亦为当事；无目标时同样归为行动者。
            if subjects.is_empty() || self.self_death.is_match(action) {
                subjects.push(actor.to_string());
            }
            return Some(("death".to_string(), dedup_sorted(subjects)));
        }
        if self.exit.is_match(action) {
            let mut subjects = d.targets.clone();
            if subjects.is_empty() {
                subjects.push(actor.to_string());
            }
            return Some(("permanent_exit".to_string(), dedup_sorted(subjects)));
        }
        if self.relation.is_match(action) {
            // 关系变更：行动者与目标皆为当事。
            let mut subjects = d.targets.clone();
            subjects.push(actor.to_string());
            return Some(("permanent_relation_change".to_string(), dedup_sorted(subjects)));
        }
        None
    }
}

fn dedup_sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// 阻断前的诊断场景（空 patch / 空 events，未提交）。
fn stub_scene(
    tick: u64,
    situation: &str,
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    now: i64,
) -> SceneRecord {
    SceneRecord {
        scene_id: format!("sc-{tick}"),
        tick,
        situation: situation.to_string(),
        decisions: decisions.to_vec(),
        outcomes: outcomes.to_vec(),
        prose: String::new(),
        events: Vec::new(),
        state_patch: StatePatch {
            id: format!("patch-{tick}"),
            base_revision: tick,
            source_decision_ids: decisions.iter().map(|d| d.decision_id.clone()).collect(),
            operations: Vec::new(),
        },
        locked: false,
        created_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::{CardLifecycle, Identity};
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use crate::narrative::state::NarrativeStore;
    use crate::narrative::types::CharacterState;

    fn profile() -> ModelProfile {
        ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "http://x".into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    fn routes() -> ModelRoutes {
        ModelRoutes {
            default: profile(),
            decide: None,
            arbiter: None,
            writer: None,
            critic: None,
            director: None,
        }
    }

    fn prompts() -> NarrativePrompts {
        NarrativePrompts {
            director_system: "导演".into(),
            decide_system: "决策".into(),
            arbiter_system: "仲裁".into(),
            writer_system: "写作".into(),
            critic_system: "审校".into(),
            prompt_version: "v1".into(),
        }
    }

    fn minimal_card(name: &str) -> CharacterCardV2 {
        CharacterCardV2 {
            schema_version: 2,
            id: name.into(),
            lifecycle: CardLifecycle::Draft,
            identity: Identity { name: name.into(), ..Default::default() },
            dramatic_core: Default::default(),
            decision_model: Default::default(),
            perception: Default::default(),
            emotion_dynamics: Default::default(),
            relation_grammar: Default::default(),
            expression_fingerprint: Default::default(),
            agency: Default::default(),
            growth_arc: Default::default(),
            world_adaptation: Default::default(),
            evidence_index: Default::default(),
            revision: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn host_with(responses: Vec<Result<String, EngineError>>) -> (Arc<EngineHost>, Arc<CollectEvents>) {
        let events = Arc::new(CollectEvents::default());
        let host = Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1000)),
            events: events.clone(),
            model: Arc::new(ScriptedModel::new(responses)),
        });
        (host, events)
    }

    /// 捕获每次模型调用 `(agent, user prompt)` 的脚本化模型：用于断言「谁的可见上下文里有什么」。
    /// 响应消费顺序与 `ScriptedModel` 完全一致（并发决策在无 yield 的 mock 下按 poll 序确定性消费）。
    struct CapturingModel {
        responses: std::sync::Mutex<Vec<Result<String, EngineError>>>,
        captured: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl CapturingModel {
        fn new(responses: Vec<Result<String, EngineError>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
                captured: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 取指定角色的 roleDecide user prompt（`build_decide_user_prompt` 的固定包裹前缀）。
        fn decide_prompt_of(&self, cid: &str) -> String {
            let needle = format!("以下是【仅你（{cid}）可见】");
            self.captured
                .lock()
                .unwrap()
                .iter()
                .find(|(agent, user)| agent == "roleDecide" && user.starts_with(&needle))
                .map(|(_, user)| user.clone())
                .unwrap_or_else(|| panic!("未捕获到 {cid} 的决策上下文"))
        }

        /// 全部 roleDecide user prompt。
        fn decide_prompts(&self) -> Vec<String> {
            self.captured
                .lock()
                .unwrap()
                .iter()
                .filter(|(agent, _)| agent == "roleDecide")
                .map(|(_, user)| user.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl crate::model::ModelClient for CapturingModel {
        async fn complete(
            &self,
            spec: &crate::model::ModelCallSpec,
            cancel: &CancelFlag,
        ) -> Result<crate::model::ModelOutput, EngineError> {
            cancel.check()?;
            self.captured.lock().unwrap().push((spec.agent.clone(), spec.user.clone()));
            let mut lock = self.responses.lock().unwrap();
            if lock.is_empty() {
                return Err(EngineError::Model { message: "脚本响应耗尽".into(), retryable: false });
            }
            lock.remove(0).map(|content| crate::model::ModelOutput {
                content,
                input_tokens: Some(1),
                output_tokens: Some(1),
            })
        }
    }

    /// 从 roleDecide user prompt 中切出可见上下文 JSON（`build_decide_user_prompt` 的固定包裹）。
    fn decide_ctx_json(user: &str) -> Value {
        let start = user.find('{').expect("可见上下文 JSON 起点");
        let end = user.rfind("\n\n请完全代入").expect("可见上下文 JSON 终点");
        serde_json::from_str(&user[start..end]).expect("可见上下文必须是合法 JSON")
    }

    fn host_capturing(model: Arc<CapturingModel>) -> Arc<EngineHost> {
        Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1000)),
            events: Arc::new(CollectEvents::default()),
            model,
        })
    }

    /// 两角色 happy path 的模型脚本：director → decide(li) → decide(wang) → writer → critic。
    fn two_char_script() -> Vec<Result<String, EngineError>> {
        vec![
            Ok(r#"{"situation":"堂前灯火通明，众人各自落座"}"#.to_string()),
            Ok(benign_decision()),
            Ok(benign_decision()),
            Ok(r#"{"prose":"堂中礼数周全，暗流未起。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ]
    }

    /// 初始化含 li/wang 两角色 + 一个 pending 硬节点的 run。
    fn init_run(host: &EngineHost, run_id: &str, with_hard_node: bool) {
        let mut s = NarrativeState { schema_version: 1, run_id: run_id.into(), ..Default::default() };
        s.characters.insert("li".into(), CharacterState::default());
        s.characters.insert("wang".into(), CharacterState::default());
        if with_hard_node {
            s.narrative.outline_nodes.push(OutlineNode {
                id: "n1".into(),
                summary: "两位大臣在密室摊牌".into(),
                constraint: ConstraintLevel::Hard,
                status: NodeStatus::Pending,
                // 旧式硬节点（无 threshold）：走 build_patch progressed=>done 兼容路径。
                threshold: None,
                advance_when: None,
                weights: None,
            });
        }
        NarrativeStore::new(host.fs.clone()).init(&s).unwrap();
    }

    fn cards() -> BTreeMap<String, CharacterCardV2> {
        [("li".to_string(), minimal_card("李")), ("wang".to_string(), minimal_card("王"))]
            .into_iter()
            .collect()
    }

    fn round_input(run_id: &str, budget: RoundBudget) -> RoundInput {
        RoundInput {
            run_id: run_id.into(),
            mode: RunMode::Observe,
            active_cards: cards(),
            other_cards_brief: BTreeMap::new(),
            // 默认档 = 不传自身身份：既有全部用例走的都是历史路径（回归保护）。
            self_identities: BTreeMap::new(),
            ambient_events: Vec::new(),
            whispers: BTreeMap::new(),
            fragments: BTreeMap::new(),
            temperature_decide: 0.0,
            temperature_writer: 0.7,
            max_output_tokens: 100,
            budget,
            approved_consents: Vec::new(),
            world_controlled: Vec::new(),
            locations: BTreeMap::new(),
            now_hint: 0,
            stall_hint: None,
            // 默认档 = 同意制：既有全部用例走的都是历史路径（回归保护）。
            lethality: Lethality::default(),
            // 默认档 = 无戏服：既有全部用例的导演 prompt 与接线前逐字节一致（回归保护）。
            realm_costume: None,
        }
    }

    fn benign_decision() -> String {
        r#"{"intent":"观望","action":"上前拱手行礼","speak":{"willSpeak":true,"purpose":"寒暄"},"targets":[],"acceptableCosts":[],"predictions":[]}"#.to_string()
    }

    fn big_budget() -> RoundBudget {
        RoundBudget { max_total_tokens: 1_000_000, spent_tokens: 0, max_scenes: 10 }
    }

    fn locdef(id: &str, connections: &[&str]) -> LocationDef {
        LocationDef {
            id: id.into(),
            name: id.into(),
            connections: connections.iter().map(|s| s.to_string()).collect(),
            is_secret_realm: false,
            gate: None,
        }
    }

    /// 两角色分处两地点 A/B 的初始状态（多组测试基态）。
    fn two_location_state(a_loc: &str, b_loc: &str) -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "run-1".into(), ..Default::default() };
        let mut li = CharacterState::default();
        li.location = a_loc.into();
        let mut wang = CharacterState::default();
        wang.location = b_loc.into();
        s.characters.insert("li".into(), li);
        s.characters.insert("wang".into(), wang);
        s
    }

    fn two_location_map() -> BTreeMap<String, LocationDef> {
        [("A".to_string(), locdef("A", &["B"])), ("B".to_string(), locdef("B", &["A"]))]
            .into_iter()
            .collect()
    }

    // ===== 完整回合 happy path =====

    #[tokio::test]
    async fn run_round_happy_path_commits_and_advances_outline() {
        // 调用顺序：director, decide(li), decide(wang), writer, critic（无仲裁模型调用）。
        let responses = vec![
            Ok(r#"{"situation":"密室之中，烛火摇曳"}"#.to_string()),
            Ok(benign_decision()),
            Ok(benign_decision()),
            Ok(r#"{"prose":"两位大臣于烛下各怀心事，礼数周全。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, ev) = host_with(responses);
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());

        let out = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);
        // 决策定序：li 在 wang 前，decision_id 确定性派生。
        assert_eq!(out.scene.decisions.len(), 2);
        assert_eq!(out.scene.decisions[0].character_id, "li");
        // decision_id 加时间段（P2 DES）：interval 路径 now_hint=0 → dec:{run}:0:{cid}。
        assert_eq!(out.scene.decisions[0].decision_id, "dec:run-1:0:li");
        assert_eq!(out.scene.decisions[1].character_id, "wang");
        // 硬节点被推进为 done（硬节点完成率）。
        assert_eq!(out.new_state.narrative.outline_nodes[0].status, NodeStatus::Done);
        // 节拍记录写入。
        assert!(!out.new_state.narrative.pacing_notes.is_empty());
        // 场景与状态落盘。
        let store = NarrativeStore::new(host.fs.clone());
        assert_eq!(store.list_scene_ids("run-1").unwrap(), vec!["sc-0".to_string()]);
        assert_eq!(store.load("run-1").unwrap().revision, 1);
        // 发射了 Narrative 领域事件（2 个 ActionResolved + 2 个 DialogueSpoken）。
        let narrative_events = ev
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::Narrative { .. }))
            .count();
        assert_eq!(narrative_events, 4);
    }

    // ===== 自身开局站位（身份池自身感知，总规格 §5【拍板 4、5】）=====

    /// 默认档（`self_identities` 不传）：整条回合链路上没有任何角色的上下文出现身份字段，
    /// 且回合产物与历史一致（提交、推进大纲）。这是「不传即零变化」的端到端守卫。
    #[tokio::test]
    async fn run_round_without_self_identities_is_unchanged() {
        let model = Arc::new(CapturingModel::new(two_char_script()));
        let host = host_capturing(model.clone());
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());

        let out = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);
        let prompts_seen = model.decide_prompts();
        assert_eq!(prompts_seen.len(), 2);
        for p in &prompts_seen {
            assert!(!p.contains("yourIdentity"), "不传身份 → 上下文里不得出现该字段：{p}");
            assert!(!p.contains("开局站位"), "不传身份 → 措辞也不得出现");
        }
    }

    /// 传入后：每个角色**只**看得见自己的开局站位；他人的自身身份绝不越界（信息边界）。
    /// 🔴 同时守红线：身份不进 DNA 卡（`active_cards` 不可变快照），不进角色私有状态，
    ///    不进 StatePatch / DomainEvent（下游一概读不到它，无从据身份改判定）。
    #[tokio::test]
    async fn run_round_feeds_self_identity_only_to_its_owner() {
        let model = Arc::new(CapturingModel::new(two_char_script()));
        let host = host_capturing(model.clone());
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());

        let mut input = round_input("run-1", big_budget());
        input.self_identities = [
            ("li".to_string(), "户部主事".to_string()),
            ("wang".to_string(), "漕帮商贾".to_string()),
        ]
        .into_iter()
        .collect();

        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();
        assert!(out.blocked.is_none());

        let li = model.decide_prompt_of("li");
        let wang = model.decide_prompt_of("wang");
        // 本人看得见自己的站位。
        assert!(li.contains("户部主事"), "li 必须看得见自己的开局站位");
        assert!(wang.contains("漕帮商贾"), "wang 必须看得见自己的开局站位");
        // 信息边界：他人的自身身份条目绝不外泄。
        assert!(!li.contains("漕帮商贾"), "信息边界：li 不得看见 wang 的自身身份");
        assert!(!wang.contains("户部主事"), "信息边界：wang 不得看见 li 的自身身份");

        let ctx_li = decide_ctx_json(&li);
        assert_eq!(ctx_li["yourIdentity"]["display"], json!("户部主事"));
        assert!(
            ctx_li["yourIdentity"]["note"].as_str().unwrap_or_default().contains("开局站位"),
            "措辞必须讲明这是开局站位"
        );
        // 🔴 红线①：角色卡快照（active_cards → yourDna）一个字节都不许被身份改写。
        assert_eq!(ctx_li["yourDna"]["identity"]["name"], json!("李"), "红线：卡上的名字必须原样");
        assert!(!ctx_li["yourDna"].to_string().contains("户部主事"), "红线：身份不得进 DNA 卡");
        // 🔴 红线②：身份不得渗进角色私有状态（那会被 reducer 落回世界状态并被下游读到）。
        assert!(!ctx_li["yourState"].to_string().contains("户部主事"), "红线：身份不得进角色状态");

        // 🔴 红线③：身份不进任何落定物——StatePatch / DomainEvent / 提交后的世界状态。
        let dumped = serde_json::to_string(&out.scene.state_patch).unwrap()
            + &serde_json::to_string(&out.scene.events).unwrap()
            + &serde_json::to_string(&out.new_state).unwrap();
        assert!(!dumped.contains("户部主事"), "红线：身份绝不进 patch/事件/世界状态");
        assert!(!dumped.contains("漕帮商贾"), "红线：身份绝不进 patch/事件/世界状态");
    }

    /// 只给一部分角色分配了身份（池配额小于人数）→ 未分配者上下文里根本没有该字段，
    /// 与默认档逐字段一致；已分配者照常看得见。退化路径可局部生效，不是全有全无。
    #[tokio::test]
    async fn run_round_partial_self_identities_degrade_per_character() {
        let model = Arc::new(CapturingModel::new(two_char_script()));
        let host = host_capturing(model.clone());
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());

        let mut input = round_input("run-1", big_budget());
        input.self_identities =
            [("li".to_string(), "户部主事".to_string())].into_iter().collect();

        engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert!(model.decide_prompt_of("li").contains("户部主事"));
        let wang = decide_ctx_json(&model.decide_prompt_of("wang"));
        assert!(wang.get("yourIdentity").is_none(), "未分配身份的角色：字段完全不出现");
    }

    // ===== 预算硬停：不提交、返回 BudgetExhausted =====

    #[tokio::test]
    async fn run_round_budget_exhausted_stops_gracefully() {
        let (host, _ev) = host_with(vec![]); // 一旦有模型调用即耗尽脚本
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());
        // max_output_tokens=100, active=2 → calls=6, scene_cost=600 > 预算 500。
        let budget = RoundBudget { max_total_tokens: 500, spent_tokens: 0, max_scenes: 10 };
        let err = engine
            .run_round(&routes(), &prompts(), round_input("run-1", budget), &CancelFlag::new())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "budget");
        // 未提交任何状态。
        assert_eq!(NarrativeStore::new(host.fs.clone()).load("run-1").unwrap().revision, 0);
    }

    // ===== 仲裁阻断：硬节点冲突 → blocked，不提交 =====

    #[tokio::test]
    async fn run_round_blocks_on_arbiter_blocked_without_commit() {
        // li 的行动带不可逆关键词 + 存在 pending 硬节点 → R5 → 交模型；模型判 blocked。
        let li_kill = r#"{"intent":"清除障碍","action":"当场杀死叛徒王五","speak":{"willSpeak":false,"purpose":""},"targets":[],"acceptableCosts":[],"predictions":[]}"#;
        let responses = vec![
            Ok(r#"{"situation":"对峙一触即发"}"#.to_string()),
            Ok(li_kill.to_string()),           // decide li
            Ok(benign_decision()),             // decide wang
            Ok(r#"{"outcomes":[{"decisionId":"dec:run-1:0:li","result":"blocked","consequence":"该行动会使硬节点无法达成"}]}"#.to_string()), // arbiter（decision_id 含时间段 :0:）
        ];
        let (host, _ev) = host_with(responses);
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());

        let out = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_some(), "应进入 blocked");
        assert_eq!(out.new_state.revision, 0); // 未提交
        assert!(NarrativeStore::new(host.fs.clone()).list_scene_ids("run-1").unwrap().is_empty());
    }

    // ===== 不变量阻断：正文泄露私密 → blocked，不提交 =====

    #[tokio::test]
    async fn run_round_blocks_on_invariant_violation_without_commit() {
        // 给 li 一个 secret；写手把 secret 抄进正文 → I1 违规。
        let mut s = NarrativeState { schema_version: 1, run_id: "run-1".into(), ..Default::default() };
        let mut li = CharacterState::default();
        li.secrets.push("我私通了敌国".into());
        s.characters.insert("li".into(), li);
        s.characters.insert("wang".into(), CharacterState::default());
        let (host, _ev) = host_with(vec![
            Ok(r#"{"situation":"宴席之上"}"#.to_string()),
            Ok(benign_decision()), // li
            Ok(benign_decision()), // wang
            Ok(r#"{"prose":"席间有人低语：我私通了敌国。"}"#.to_string()), // writer 泄密
        ]);
        NarrativeStore::new(host.fs.clone()).init(&s).unwrap();
        let engine = NarrativeEngine::new(host.clone());

        let out = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.as_deref().unwrap().contains("I1"), "应因 I1 阻断：{:?}", out.blocked);
        assert_eq!(out.new_state.revision, 0);
        assert!(NarrativeStore::new(host.fs.clone()).list_scene_ids("run-1").unwrap().is_empty());
    }

    // ===== 不可逆结果同意门控（REMEDIATION #3 / §2.4）=====

    fn kill_decision(target: &str) -> String {
        format!(
            r#"{{"intent":"除掉隐患","action":"拔剑当场杀死叛徒","speak":{{"willSpeak":false,"purpose":""}},"targets":["{target}"],"acceptableCosts":[],"predictions":[]}}"#
        )
    }

    #[tokio::test]
    async fn run_round_gates_irreversible_without_approval() {
        // 无硬节点 → 「杀死」判 Success（clean），进入门控分类。
        let responses = vec![
            Ok(r#"{"situation":"对峙时刻"}"#.to_string()),
            Ok(kill_decision("wang")), // decide li：杀死 wang（不可逆·死亡）
            Ok(benign_decision()),     // decide wang
            Ok(r#"{"prose":"局势骤变，刀光一闪。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, ev) = host_with(responses);
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());

        // approved_consents 空 → wang 的死亡未获批。
        let out = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1); // 场景仍提交（其余角色行动落定）

        // ① 产出 ConsentRequested：fact.eventKind=death、subjectCharacterIds=[wang]、可见性 Private 含 wang。
        let cr: Vec<&DomainEvent> = out
            .scene
            .events
            .iter()
            .filter(|e| e.event_type == DomainEventType::ConsentRequested)
            .collect();
        assert_eq!(cr.len(), 1);
        assert_eq!(cr[0].fact["eventKind"], "death");
        assert_eq!(cr[0].fact["subjectCharacterIds"], serde_json::json!(["wang"]));
        match &cr[0].visibility {
            EventVisibility::Private { audience_character_ids } => {
                assert!(audience_character_ids.contains(&"wang".to_string()));
            }
            _ => panic!("ConsentRequested 应为 Private→当事角色"),
        }

        // ② 不落定：li 的不可逆结果未进入 StatePatch（无 li 节拍记录）；wang 正常落定。
        assert!(
            !out.new_state.narrative.pacing_notes.iter().any(|n| n.starts_with("li｜")),
            "li 的不可逆结果不应落定"
        );
        assert!(out.new_state.narrative.pacing_notes.iter().any(|n| n.starts_with("wang｜")));

        // ③ 记入 pending_consents。
        assert!(out
            .new_state
            .narrative
            .pending_consents
            .iter()
            .any(|p| p.subject == "wang" && p.event_kind == "death"));

        // ConsentRequested 也经领域事件通道发射。
        let emitted = ev
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::Narrative { payload, .. }
                if payload.get("type").and_then(|v| v.as_str()) == Some("consent_requested")))
            .count();
        assert_eq!(emitted, 1);
    }

    #[tokio::test]
    async fn run_round_lands_irreversible_when_approved_and_clears_pending() {
        let responses = vec![
            Ok(r#"{"situation":"对峙时刻"}"#.to_string()),
            Ok(kill_decision("wang")), // li 杀死 wang
            Ok(benign_decision()),     // wang
            Ok(r#"{"prose":"尘埃落定。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        // 预置状态：li/wang + 一条既有待审批 {wang, death}（模拟上一回合已请求）。
        let mut s = NarrativeState { schema_version: 1, run_id: "run-1".into(), ..Default::default() };
        s.characters.insert("li".into(), CharacterState::default());
        s.characters.insert("wang".into(), CharacterState::default());
        s.narrative
            .pending_consents
            .push(PendingConsent { subject: "wang".into(), event_kind: "death".into() });
        NarrativeStore::new(host.fs.clone()).init(&s).unwrap();
        let engine = NarrativeEngine::new(host.clone());

        // approved_consents 含 wang → 本回合可落定 wang 的死亡。
        let mut input = round_input("run-1", big_budget());
        input.approved_consents = vec!["wang".to_string()];
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert!(out.blocked.is_none());
        // 已获批 → 无 ConsentRequested。
        assert_eq!(
            out.scene
                .events
                .iter()
                .filter(|e| e.event_type == DomainEventType::ConsentRequested)
                .count(),
            0
        );
        // li 的不可逆结果落定（节拍记录含 li）。
        assert!(out.new_state.narrative.pacing_notes.iter().any(|n| n.starts_with("li｜")));
        // 既有 pending 被清除。
        assert!(out.new_state.narrative.pending_consents.is_empty(), "获批落定后应清除对应 pending");
    }

    // ===== 世界固有角色（NPC/反派）同意门控豁免：无主人可授权 → 自动放行落定 =====

    #[tokio::test]
    async fn run_round_world_controlled_subject_lands_without_consent() {
        // wang 为世界固有角色（NPC，无主人）：li 杀死 wang 的不可逆结果应自动放行落定，
        // 不产 ConsentRequested、不记 pending_consents（对照 gates_irreversible：玩家 subject 仍门控）。
        let responses = vec![
            Ok(r#"{"situation":"对峙时刻"}"#.to_string()),
            Ok(kill_decision("wang")), // decide li：杀死 NPC wang（不可逆·死亡）
            Ok(benign_decision()),     // decide wang
            Ok(r#"{"prose":"局势骤变，刀光一闪。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());

        // world_controlled 含 wang；approved_consents 仍空——豁免仅来自 world_controlled。
        let mut input = round_input("run-1", big_budget());
        input.world_controlled = vec!["wang".to_string()];
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);

        // ① NPC subject 自动放行 → 无 ConsentRequested。
        assert_eq!(
            out.scene
                .events
                .iter()
                .filter(|e| e.event_type == DomainEventType::ConsentRequested)
                .count(),
            0,
            "world_controlled subject 的不可逆结果不应产生 ConsentRequested"
        );
        // ② li 的不可逆结果直接落定（节拍记录含 li）。
        assert!(
            out.new_state.narrative.pacing_notes.iter().any(|n| n.starts_with("li｜")),
            "NPC 死亡应直接落定，无需门控"
        );
        // ③ NPC subject 不记 pending_consents（无 owner）。
        assert!(
            !out.new_state.narrative.pending_consents.iter().any(|p| p.subject == "wang"),
            "world_controlled subject 不应记入 pending_consents"
        );
    }

    // ===== Phase 2：多地点分组 / 成本 / 隔离 / 移动 =====

    #[tokio::test]
    async fn run_round_multi_location_splits_directors_and_writers() {
        // li@A、wang@B → 2 组：导演 2 + 决策 2 + 写作 2 + 审校 1 = 7 调用（无仲裁模型）。
        let responses = vec![
            Ok(r#"{"situation":"A 厅烛火"}"#.to_string()), // director loc A（loc 字典序 A 先）
            Ok(r#"{"situation":"B 苑月色"}"#.to_string()), // director loc B
            Ok(benign_decision()),                          // decide li
            Ok(benign_decision()),                          // decide wang
            Ok(r#"{"prose":"A 厅一幕。"}"#.to_string()),    // writer loc A
            Ok(r#"{"prose":"B 苑一幕。"}"#.to_string()),    // writer loc B
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, ev) = host_with(responses);
        NarrativeStore::new(host.fs.clone()).init(&two_location_state("A", "B")).unwrap();
        let engine = NarrativeEngine::new(host.clone());

        let mut input = round_input("run-1", big_budget());
        input.locations = two_location_map();
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);
        // 两组各写一段，合并进单 SceneRecord.prose（证明逐组写作）。
        assert!(out.scene.prose.contains("A 厅一幕"), "缺 A 组正文：{}", out.scene.prose);
        assert!(out.scene.prose.contains("B 苑一幕"), "缺 B 组正文：{}", out.scene.prose);
        // 恰好 7 次模型调用（分组 → 导演/写作各按组放大；成本公式落实）。
        let calls = ev
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::ModelCall(_)))
            .count();
        assert_eq!(calls, 7, "2 组应产生 7 次模型调用（导演2+决策2+写作2+审校1）");
        // 事件仍全局汇总：每角色 ActionResolved + DialogueSpoken = 4。
        let narrative_events = ev
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::Narrative { .. }))
            .count();
        assert_eq!(narrative_events, 4);
    }

    #[tokio::test]
    async fn run_round_multi_location_cost_scales_and_can_exhaust() {
        // 2 组、N=2：成本 = N + 组数*2 + 2 = 8；max_output=100 → scene_cost=800 > 预算 700 → 硬停。
        // （单组时成本=6→600<700 不会硬停，故此断言证明成本随地点组数放大。）
        let (host, _ev) = host_with(vec![]);
        NarrativeStore::new(host.fs.clone()).init(&two_location_state("A", "B")).unwrap();
        let engine = NarrativeEngine::new(host.clone());
        let mut input =
            round_input("run-1", RoundBudget { max_total_tokens: 700, spent_tokens: 0, max_scenes: 10 });
        input.locations = two_location_map();
        let err =
            engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap_err();
        assert_eq!(err.code(), "budget");
        // 未提交任何状态。
        assert_eq!(NarrativeStore::new(host.fs.clone()).load("run-1").unwrap().revision, 0);
    }

    #[tokio::test]
    async fn run_round_cross_location_target_is_isolated() {
        // li@A 想攻击 wang（在 B 组）→ 同组在场集 {li} 不含 wang：
        // role_decide 目标白名单按同组收窄，跨地点角色 wang 被丢弃 → li 无法跨地点作用于 wang（异地/秘境隔离）。
        let attack = r#"{"intent":"袭击","action":"挥剑砍向对面","speak":{"willSpeak":false,"purpose":""},"targets":["wang"],"acceptableCosts":[],"predictions":[]}"#;
        let responses = vec![
            Ok(r#"{"situation":"A 厅"}"#.to_string()),
            Ok(r#"{"situation":"B 苑"}"#.to_string()),
            Ok(attack.to_string()),          // decide li
            Ok(benign_decision()),           // decide wang
            Ok(r#"{"prose":"A 厅。"}"#.to_string()),
            Ok(r#"{"prose":"B 苑。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        NarrativeStore::new(host.fs.clone()).init(&two_location_state("A", "B")).unwrap();
        let engine = NarrativeEngine::new(host.clone());
        let mut input = round_input("run-1", big_budget());
        input.locations = two_location_map();
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert!(out.blocked.is_none());
        // 同组白名单收窄：li 的跨地点目标 wang 被丢弃。
        let li_d = out.scene.decisions.iter().find(|d| d.character_id == "li").unwrap();
        assert!(li_d.targets.is_empty(), "跨地点目标 wang 应被同组白名单丢弃：{:?}", li_d.targets);
        // 无任何事件以 wang 为 target（li 无法跨地点作用于异地角色）。
        assert!(
            !out.scene.events.iter().any(|e| e
                .target_ids
                .as_ref()
                .map(|t| t.contains(&"wang".to_string()))
                .unwrap_or(false)),
            "不应有跨地点指向 wang 的事件"
        );
    }

    #[tokio::test]
    async fn run_round_movement_lands_location_change() {
        // li、wang 同在「前厅」（单组，5 调用）；li 决策移动到连通的「密室」→ R6 Success →
        // build_patch 生成 characters.li.location Set → reducer 落定；wang 留原地。
        let move_li = r#"{"intent":"转移","action":"前往密室","speak":{"willSpeak":false,"purpose":""},"targets":["loc:密室"],"acceptableCosts":[],"predictions":[]}"#;
        let responses = vec![
            Ok(r#"{"situation":"前厅对峙"}"#.to_string()),
            Ok(move_li.to_string()), // decide li（移动）
            Ok(benign_decision()),   // decide wang
            Ok(r#"{"prose":"李某转身离去。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        NarrativeStore::new(host.fs.clone()).init(&two_location_state("前厅", "前厅")).unwrap();
        let engine = NarrativeEngine::new(host.clone());
        let mut input = round_input("run-1", big_budget());
        input.locations = [
            ("前厅".to_string(), locdef("前厅", &["密室"])),
            ("密室".to_string(), locdef("密室", &["前厅"])),
        ]
        .into_iter()
        .collect();
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);
        assert_eq!(out.new_state.characters["li"].location, "密室", "合法移动应落定新地点");
        assert_eq!(out.new_state.characters["wang"].location, "前厅", "未移动者留原地");
        // 移动事件附 from/to。
        let mv = out
            .scene
            .events
            .iter()
            .find(|e| e.actor_ids.contains(&"li".to_string()) && e.fact.get("to").is_some())
            .expect("应有携 from/to 的移动 ActionResolved");
        assert_eq!(mv.fact["from"], "前厅");
        assert_eq!(mv.fact["to"], "密室");
    }

    // ===== 取消：不提交 =====

    #[tokio::test]
    async fn run_round_cancelled_before_start() {
        let (host, _ev) = host_with(vec![]);
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());
        let cancel = CancelFlag::new();
        cancel.cancel();
        let err = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "cancelled");
        assert_eq!(NarrativeStore::new(host.fs.clone()).load("run-1").unwrap().revision, 0);
    }

    // ===== estimate 公式 =====

    #[test]
    fn estimate_uses_n_plus_4() {
        let (host, _ev) = host_with(vec![]);
        let engine = NarrativeEngine::new(host);
        let est = engine.estimate(3, 1000, 2);
        assert_eq!(est.calls_per_scene, 7); // 3 + 4
    }

    // ===== P2 DES（异步时间线，Phase 1）：run_event_step 调度 =====

    /// benign 决策 + 指定 duration（willSpeak=true → 每角色产 ActionResolved + DialogueSpoken）。
    fn benign_decision_dur(dur: i64) -> String {
        format!(
            r#"{{"intent":"观望","action":"上前拱手行礼","speak":{{"willSpeak":true,"purpose":"寒暄"}},"targets":[],"acceptableCosts":[],"predictions":[],"duration":{dur}}}"#
        )
    }

    /// 初始化含指定角色 + timeline（next_time/now/time_cap）+ 可选 pending 硬节点的 run。
    fn init_run_timeline(
        host: &EngineHost,
        run_id: &str,
        chars: &[&str],
        next_time: &[(&str, i64)],
        now: i64,
        time_cap: Option<i64>,
        hard_node_status: Option<NodeStatus>,
    ) {
        let mut s = NarrativeState { schema_version: 1, run_id: run_id.into(), ..Default::default() };
        for c in chars {
            s.characters.insert((*c).into(), CharacterState::default());
        }
        for (c, t) in next_time {
            s.timeline.next_time.insert((*c).into(), *t);
        }
        s.timeline.now = now;
        s.timeline.time_cap = time_cap;
        if let Some(status) = hard_node_status {
            s.narrative.outline_nodes.push(OutlineNode {
                id: "n1".into(),
                summary: "主线节点".into(),
                constraint: ConstraintLevel::Hard,
                status,
                // 硬里程碑：constraint=Hard 供 R5（arbiter 硬节点保护）测试；threshold=Some 使其计入
                // is_terminal 的里程碑集（P1 调和后 MainlineDone 判据），供 terminal_not_wait_all 触发终局。
                threshold: Some(1.0),
                advance_when: None,
                weights: None,
            });
        }
        NarrativeStore::new(host.fs.clone()).init(&s).unwrap();
    }

    /// 为指定角色集组装 RoundInput（active_cards 含各角色卡；cohort 过滤在引擎内做）。
    fn round_input_for(run_id: &str, chars: &[&str], budget: RoundBudget) -> RoundInput {
        let active_cards: BTreeMap<String, CharacterCardV2> =
            chars.iter().map(|c| ((*c).to_string(), minimal_card(c))).collect();
        RoundInput {
            run_id: run_id.into(),
            mode: RunMode::Observe,
            active_cards,
            other_cards_brief: BTreeMap::new(),
            // 默认档 = 不传自身身份：既有全部用例走的都是历史路径（回归保护）。
            self_identities: BTreeMap::new(),
            ambient_events: Vec::new(),
            whispers: BTreeMap::new(),
            fragments: BTreeMap::new(),
            temperature_decide: 0.0,
            temperature_writer: 0.7,
            max_output_tokens: 100,
            budget,
            approved_consents: Vec::new(),
            world_controlled: Vec::new(),
            locations: BTreeMap::new(),
            now_hint: 0,
            stall_hint: None,
            // 默认档 = 同意制：既有全部用例走的都是历史路径（回归保护）。
            lethality: Lethality::default(),
            // 默认档 = 无戏服：既有全部用例的导演 prompt 与接线前逐字节一致（回归保护）。
            realm_costume: None,
        }
    }

    #[tokio::test]
    async fn event_step_picks_min_next_time() {
        // li next_time=100、wang next_time=50 → 只激活最小者 wang（T=50，cohort={wang}）。
        // 单角色 cohort：导演 + 决策(wang) + 写作 + 审校 = 4 调用。
        let responses = vec![
            Ok(r#"{"situation":"月下独行"}"#.to_string()),
            Ok(benign_decision_dur(40)),
            Ok(r#"{"prose":"王某独自前行。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run_timeline(host.as_ref(), "run-1", &["li", "wang"], &[("li", 100), ("wang", 50)], 0, None, None);
        let engine = NarrativeEngine::new(host.clone());

        let step = engine
            .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li", "wang"], big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert_eq!(step.at_time, 50);
        assert_eq!(step.activated, vec!["wang".to_string()]);
        let out = step.outcome.expect("应有回合结果");
        assert!(out.blocked.is_none());
        // 只有 wang 被激活（li 未进入本步决策）。
        assert_eq!(out.scene.decisions.len(), 1);
        assert_eq!(out.scene.decisions[0].character_id, "wang");
        // wang 推进到 50+40=90；li 不变（仍 100）；世界钟推进到 T=50。
        assert_eq!(out.new_state.timeline.next_time["wang"], 90);
        assert_eq!(out.new_state.timeline.next_time["li"], 100);
        assert_eq!(out.new_state.timeline.now, 50);
    }

    #[tokio::test]
    async fn event_step_advances_next_time() {
        // li,wang next_time=0（同刻 cohort），sun next_time=1000（非 cohort）。
        // T=0，cohort={li,wang} 各推进到 duration；sun 不变。
        let dur = 70;
        let responses = vec![
            Ok(r#"{"situation":"晨会"}"#.to_string()),
            Ok(benign_decision_dur(dur)), // decide li
            Ok(benign_decision_dur(dur)), // decide wang
            Ok(r#"{"prose":"二人各表其志。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run_timeline(
            host.as_ref(),
            "run-1",
            &["li", "sun", "wang"],
            &[("li", 0), ("wang", 0), ("sun", 1000)],
            0,
            None,
            None,
        );
        let engine = NarrativeEngine::new(host.clone());

        let step = engine
            .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li", "sun", "wang"], big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert_eq!(step.at_time, 0);
        assert_eq!(step.activated, vec!["li".to_string(), "wang".to_string()]);
        let out = step.outcome.unwrap();
        assert_eq!(out.new_state.timeline.next_time["li"], dur);
        assert_eq!(out.new_state.timeline.next_time["wang"], dur);
        assert_eq!(out.new_state.timeline.next_time["sun"], 1000, "非 cohort 角色 next_time 不变");
    }

    #[tokio::test]
    async fn decision_id_includes_time() {
        // 单角色 li 跨两步（T=0 → T=dur），decision_id 含时间段且两步不撞。
        let dur = 60;
        let responses = vec![
            // step 1 @ T=0
            Ok(r#"{"situation":"第一幕"}"#.to_string()),
            Ok(benign_decision_dur(dur)),
            Ok(r#"{"prose":"其一。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
            // step 2 @ T=dur
            Ok(r#"{"situation":"第二幕"}"#.to_string()),
            Ok(benign_decision_dur(dur)),
            Ok(r#"{"prose":"其二。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run_timeline(host.as_ref(), "run-1", &["li"], &[("li", 0)], 0, None, None);
        let engine = NarrativeEngine::new(host.clone());

        let step1 = engine
            .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li"], big_budget()), &CancelFlag::new())
            .await
            .unwrap();
        let id1 = step1.outcome.unwrap().scene.decisions[0].decision_id.clone();

        let step2 = engine
            .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li"], big_budget()), &CancelFlag::new())
            .await
            .unwrap();
        let id2 = step2.outcome.unwrap().scene.decisions[0].decision_id.clone();

        assert_eq!(id1, "dec:run-1:0:li");
        assert_eq!(id2, format!("dec:run-1:{dur}:li"));
        assert_ne!(id1, id2, "同角色跨步 decision_id 不应相撞");
    }

    #[tokio::test]
    async fn blocked_step_does_not_starve() {
        // li 带不可逆行动 + pending 硬节点 → 模型仲裁 blocked → 不提交，但 cohort next_time += RETRY_STEP，
        // 下一步不再撞同一 T（防饿死/锁死）。
        let li_kill = r#"{"intent":"清除","action":"当场杀死叛徒","speak":{"willSpeak":false,"purpose":""},"targets":[],"acceptableCosts":[],"predictions":[],"duration":40}"#;
        let responses = vec![
            Ok(r#"{"situation":"对峙"}"#.to_string()),
            Ok(li_kill.to_string()),
            Ok(r#"{"outcomes":[{"decisionId":"dec:run-1:0:li","result":"blocked","consequence":"危及硬节点"}]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run_timeline(host.as_ref(), "run-1", &["li"], &[("li", 0)], 0, None, Some(NodeStatus::Pending));
        let engine = NarrativeEngine::new(host.clone());

        let step = engine
            .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li"], big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        let out = step.outcome.as_ref().unwrap();
        assert!(out.blocked.is_some(), "应进入 blocked");
        assert_eq!(out.new_state.revision, 0, "blocked 不提交状态");
        // 兜底推进：li next_time = 0 + RETRY_STEP。
        assert_eq!(out.new_state.timeline.next_time["li"], RETRY_STEP);
        assert_eq!(out.new_state.timeline.now, 0);
        assert!(step.terminal.is_none());
        // 磁盘 timeline 已推进（下步取 min 得 RETRY_STEP，不再锁死于 0）。
        let reloaded = NarrativeStore::new(host.fs.clone()).load("run-1").unwrap();
        assert_eq!(reloaded.timeline.next_time["li"], RETRY_STEP);
        assert_eq!(reloaded.revision, 0, "blocked 不 bump revision");
    }

    #[tokio::test]
    async fn terminal_not_wait_all() {
        // 主线全 Done + 一角色 next_time 远在未来 → is_terminal 判 MainlineDone（不跑回合、不等该角色）。
        let (host, ev) = host_with(vec![]); // 无模型调用
        init_run_timeline(
            host.as_ref(),
            "run-1",
            &["li"],
            &[("li", 1_000_000)],
            0,
            None,
            Some(NodeStatus::Done),
        );
        let engine = NarrativeEngine::new(host.clone());

        let step = engine
            .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li"], big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert_eq!(step.terminal, Some(Terminal::MainlineDone { ending: None }));
        assert!(step.outcome.is_none(), "终局短路不跑回合");
        let calls =
            ev.0.lock().unwrap().iter().filter(|e| matches!(e, EngineEvent::ModelCall(_))).count();
        assert_eq!(calls, 0, "终局短路不应有模型调用");
    }

    #[tokio::test]
    async fn duration_clamped() {
        // 模型给 duration=0 → role_decide 兜底 DEFAULT_DURATION → li next_time 推进正量（不锁死 T）。
        let zero_dur = r#"{"intent":"观望","action":"原地不动","speak":{"willSpeak":false,"purpose":""},"targets":[],"acceptableCosts":[],"predictions":[],"duration":0}"#;
        let responses = vec![
            Ok(r#"{"situation":"静默"}"#.to_string()),
            Ok(zero_dur.to_string()),
            Ok(r#"{"prose":"无事发生。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run_timeline(host.as_ref(), "run-1", &["li"], &[("li", 0)], 0, None, None);
        let engine = NarrativeEngine::new(host.clone());

        let step = engine
            .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li"], big_budget()), &CancelFlag::new())
            .await
            .unwrap();
        let out = step.outcome.unwrap();
        // 0 被兜底为 DEFAULT_DURATION → li 从 0 推进到 DEFAULT_DURATION（严格 > 0，未锁死于 T=0）。
        assert_eq!(out.new_state.timeline.next_time["li"], DEFAULT_DURATION);
        assert!(out.new_state.timeline.next_time["li"] > 0);
    }

    /// confluence 场景：li(dur=30)/wang(dur=50) 两角色，从 next_time=0 起跑 3 个 event_step。
    /// 因 duration 不同产生跨步交错（step1@0 双人 → step2@30 仅 li → step3@50 仅 wang）。
    /// 返回 (终态 state JSON, 事件全序 (timestamp, sequence) 列表)，供两次独立执行比对。
    async fn run_confluence_scenario() -> (String, Vec<(i64, u64)>) {
        let responses = vec![
            // step1 @ T=0：导演 + li(30) + wang(50) + 写作 + 审校
            Ok(r#"{"situation":"s1"}"#.to_string()),
            Ok(benign_decision_dur(30)),
            Ok(benign_decision_dur(50)),
            Ok(r#"{"prose":"p1"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
            // step2 @ T=30：cohort={li}
            Ok(r#"{"situation":"s2"}"#.to_string()),
            Ok(benign_decision_dur(30)),
            Ok(r#"{"prose":"p2"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
            // step3 @ T=50：cohort={wang}
            Ok(r#"{"situation":"s3"}"#.to_string()),
            Ok(benign_decision_dur(50)),
            Ok(r#"{"prose":"p3"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run_timeline(host.as_ref(), "run-1", &["li", "wang"], &[("li", 0), ("wang", 0)], 0, None, None);
        let engine = NarrativeEngine::new(host.clone());

        let mut events: Vec<(i64, u64)> = Vec::new();
        for _ in 0..3 {
            let step = engine
                .run_event_step(&routes(), &prompts(), round_input_for("run-1", &["li", "wang"], big_budget()), &CancelFlag::new())
                .await
                .unwrap();
            let out = step.outcome.expect("非终局步应有回合结果");
            assert!(out.blocked.is_none());
            for e in &out.scene.events {
                events.push((e.timestamp, e.sequence));
            }
        }
        let final_state = NarrativeStore::new(host.fs.clone()).load("run-1").unwrap();
        (serde_json::to_string(&final_state).unwrap(), events)
    }

    #[tokio::test]
    async fn confluence() {
        // 确定性核心：同一组 per-character 决策（含 duration），两次独立执行 →
        // 相同终态 state + 相同事件全序 (timestamp, sequence)。
        let (state_a, events_a) = run_confluence_scenario().await;
        let (state_b, events_b) = run_confluence_scenario().await;

        assert_eq!(state_a, state_b, "独立执行应收敛到相同终态 state");
        assert_eq!(events_a, events_b, "独立执行应产生相同事件全序");

        // 事件 timestamp 跨步单调不减 → 全序 (timestamp, sequence) 有效可 replay。
        for w in events_a.windows(2) {
            assert!(w[0].0 <= w[1].0, "timestamp 应单调不减：{events_a:?}");
        }
        // 具体锚点：step1@0（li/wang 各 ActionResolved+DialogueSpoken = 4 事件）→ step2@30（2）→ step3@50（2）。
        let timestamps: Vec<i64> = events_a.iter().map(|(t, _)| *t).collect();
        assert_eq!(timestamps, vec![0, 0, 0, 0, 30, 30, 50, 50]);

        // 终态锚点：li 60、wang 100、世界钟 50。
        let final_state: NarrativeState = serde_json::from_str(&state_a).unwrap();
        assert_eq!(final_state.timeline.next_time["li"], 60);
        assert_eq!(final_state.timeline.next_time["wang"], 100);
        assert_eq!(final_state.timeline.now, 50);
        assert_eq!(final_state.revision, 3, "3 步 3 次原子提交");
    }

    // ===== P2 DES（异步时间线，Phase 3）：同地点碰撞 cohort（接 P3 location） =====

    /// 构造带 `location` + `timeline.next_time` 的状态（纯 select_cohort 单测用，不落盘）。
    fn state_with_locs_times(entries: &[(&str, &str, i64)], now: i64) -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "run-1".into(), ..Default::default() };
        for (cid, loc, t) in entries {
            let mut cs = CharacterState::default();
            cs.location = (*loc).into();
            s.characters.insert((*cid).into(), cs);
            s.timeline.next_time.insert((*cid).into(), *t);
        }
        s.timeline.now = now;
        s
    }

    #[test]
    fn select_cohort_same_location_collision() {
        // li@A、zhang@A、wang@B 三人 next_time 同为 0：T=0 的碰撞组只含锚地点
        // （字典序最小空闲角色 li 的 location=A）的同地点角色 {li, zhang}；异地 wang 被剔除。
        let s = state_with_locs_times(&[("li", "A", 0), ("zhang", "A", 0), ("wang", "B", 0)], 0);
        let t = select_time(&s);
        assert_eq!(t, 0);
        assert_eq!(
            select_cohort(&s, t),
            vec!["li".to_string(), "zhang".to_string()],
            "同刻但异地的 wang 不应进入 A 组碰撞 cohort"
        );

        // wang 留待下一步：li/zhang 推进到未来后，同一 T=0 的 select_cohort 锚地点轮到 B → {wang}。
        let mut s2 = s.clone();
        s2.timeline.next_time.insert("li".into(), 30);
        s2.timeline.next_time.insert("zhang".into(), 30);
        let t2 = select_time(&s2);
        assert_eq!(t2, 0, "wang 仍空闲于 0");
        assert_eq!(select_cohort(&s2, t2), vec!["wang".to_string()], "异地角色在后续步单独成组");
    }

    #[test]
    fn select_cohort_single_location_degenerates_to_same_tick() {
        // 全体同地点「广场」：碰撞组不再按地点剔除任何人 → 退化为 Phase 1「同刻」：
        // T=0 的 cohort = 全部 next_time==0 的角色 {li, wang}；busy 的 sun(next_time=1000) 不入组。
        let s = state_with_locs_times(
            &[("li", "广场", 0), ("wang", "广场", 0), ("sun", "广场", 1000)],
            0,
        );
        let t = select_time(&s);
        assert_eq!(t, 0);
        assert_eq!(select_cohort(&s, t), vec!["li".to_string(), "wang".to_string()]);

        // 对照：同一 next_time 但皆无地点（老世界 location=""）→ 结果完全一致（证明单地点=同刻退化）。
        let s_default =
            state_with_locs_times(&[("li", "", 0), ("wang", "", 0), ("sun", "", 1000)], 0);
        assert_eq!(
            select_cohort(&s_default, select_time(&s_default)),
            select_cohort(&s, t),
            "单一地点世界的 cohort 应与无地点（同刻）世界完全一致"
        );
    }

    #[tokio::test]
    async fn event_step_different_locations_do_not_sync() {
        // li@A、wang@B 同刻空闲（next_time=0）：碰撞按地点收窄 → 两人分入不同 event_step，
        // 各自单独一个 revision/timestamp（不同地点不同步行动，端到端验证）。
        let responses = vec![
            // step1 @ T=0：cohort={li}（锚地点 A）：导演 + 决策(li) + 写作 + 审校 = 4
            Ok(r#"{"situation":"A 厅"}"#.to_string()),
            Ok(benign_decision_dur(30)),
            Ok(r#"{"prose":"李某于 A 厅。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
            // step2 @ T=0：同刻但锚地点轮到 B → cohort={wang}
            Ok(r#"{"situation":"B 苑"}"#.to_string()),
            Ok(benign_decision_dur(50)),
            Ok(r#"{"prose":"王某于 B 苑。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        // li@A、wang@B，next_time 同为 0。
        let mut s = NarrativeState { schema_version: 1, run_id: "run-1".into(), ..Default::default() };
        let mut li = CharacterState::default();
        li.location = "A".into();
        let mut wang = CharacterState::default();
        wang.location = "B".into();
        s.characters.insert("li".into(), li);
        s.characters.insert("wang".into(), wang);
        s.timeline.next_time.insert("li".into(), 0);
        s.timeline.next_time.insert("wang".into(), 0);
        NarrativeStore::new(host.fs.clone()).init(&s).unwrap();
        let engine = NarrativeEngine::new(host.clone());

        // step1：只激活 li（锚地点 A），wang 不动。
        let step1 = engine
            .run_event_step(
                &routes(),
                &prompts(),
                round_input_for("run-1", &["li", "wang"], big_budget()),
                &CancelFlag::new(),
            )
            .await
            .unwrap();
        assert_eq!(step1.at_time, 0);
        assert_eq!(step1.activated, vec!["li".to_string()], "异地 wang 不应与 li 同步入组");
        let out1 = step1.outcome.unwrap();
        assert!(out1.blocked.is_none());
        assert_eq!(out1.scene.decisions.len(), 1);
        assert_eq!(out1.scene.decisions[0].character_id, "li");
        assert_eq!(out1.new_state.timeline.next_time["li"], 30);
        assert_eq!(out1.new_state.timeline.next_time["wang"], 0, "wang 仍空闲于 0，未被本步推进");
        assert_eq!(out1.new_state.revision, 1);

        // step2：同一 T=0，锚地点轮到 B → 激活 wang（证明 wang 在独立的一步/一个 revision 内行动）。
        let step2 = engine
            .run_event_step(
                &routes(),
                &prompts(),
                round_input_for("run-1", &["li", "wang"], big_budget()),
                &CancelFlag::new(),
            )
            .await
            .unwrap();
        assert_eq!(step2.at_time, 0);
        assert_eq!(step2.activated, vec!["wang".to_string()]);
        let out2 = step2.outcome.unwrap();
        assert!(out2.blocked.is_none());
        assert_eq!(out2.scene.decisions.len(), 1);
        assert_eq!(out2.scene.decisions[0].character_id, "wang");
        assert_eq!(out2.new_state.timeline.next_time["wang"], 50);
        // 两步各独立提交（revision 1 → 2）：不同地点角色未在同一回合/同一 revision 内同步行动。
        assert_eq!(out2.new_state.revision, 2);
    }

    // ===== P1 放置房终局（Phase 1）：阈值推进 + 里程碑守卫 =====

    /// 阈值里程碑节点（constraint=Soft，带 threshold + 可选 advance_when 谓词门）。
    fn milestone_node(
        id: &str,
        threshold: f64,
        advance_when: Option<&str>,
        status: NodeStatus,
    ) -> OutlineNode {
        OutlineNode {
            id: id.into(),
            summary: format!("里程碑 {id}"),
            constraint: ConstraintLevel::Soft,
            status,
            threshold: Some(threshold),
            advance_when: advance_when.map(|s| s.into()),
            weights: None,
        }
    }

    /// 静默决策（willSpeak=false → 不产互动强度，回合强度只来自 outcome）。
    fn silent_decision(cid: &str, decision_id: &str) -> RoleDecision {
        RoleDecision {
            decision_id: decision_id.into(),
            character_id: cid.into(),
            intent: "推进".into(),
            action: "行动".into(),
            speak: SpeakIntent { will_speak: false, purpose: String::new() },
            targets: vec![],
            acceptable_costs: vec![],
            predictions: vec![],
            duration: 0,
        }
    }

    fn outcome_of(cid: &str, decision_id: &str, result: ArbiterResult) -> ArbiterOutcome {
        ArbiterOutcome {
            decision_id: decision_id.into(),
            character_id: cid.into(),
            result,
            rule_refs: vec![],
            consequence: "后果".into(),
        }
    }

    /// build_patch 是否含把节点 <id> 翻 done 的 Set op。
    fn has_status_done(patch: &StatePatch, node_id: &str) -> bool {
        patch.operations.iter().any(|o| {
            o.op == PatchOp::Set
                && o.path == format!("narrative.outlineNodes[{node_id}].status")
                && o.value.as_ref().and_then(|v| v.as_str()) == Some("done")
        })
    }

    /// build_patch 中节点 <id> 的进度累加 Increment 的 delta（无则 None）。
    fn progress_delta(patch: &StatePatch, node_id: &str) -> Option<f64> {
        patch.operations.iter().find_map(|o| {
            if o.op == PatchOp::Increment && o.path == format!("world.milestoneProgress_{node_id}") {
                o.value.as_ref().and_then(|v| v.as_f64())
            } else {
                None
            }
        })
    }

    fn state_with_chars(chars: &[&str]) -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        for c in chars {
            s.characters.insert((*c).into(), CharacterState::default());
        }
        s
    }

    #[test]
    fn milestone_threshold_accumulates_monotonically_and_advances() {
        // 里程碑 threshold=3.0、无谓词门；单角色 Success（delta=1.0/回合）。
        let mut s = state_with_chars(&["li"]);
        s.narrative.outline_nodes.push(milestone_node("m1", 3.0, None, NodeStatus::Pending));
        let decisions = vec![silent_decision("li", "d")];
        let outcomes = vec![outcome_of("li", "d", ArbiterResult::Success)];

        let mut seq = vec![];
        for _ in 0..3 {
            let patch = build_patch(s.revision, &decisions, &outcomes, &s);
            s = reducer::validate_and_apply(&s, &patch).unwrap();
            seq.push(s.world.get("milestoneProgress_m1").and_then(|v| v.as_f64()).unwrap());
        }
        // 单调增：1.0 → 2.0 → 3.0（进度键仅经 Increment 写入，永不回退）。
        assert_eq!(seq, vec![1.0, 2.0, 3.0]);
        // 前两回合未达阈值保持 Pending，第三回合达阈值翻 Done。
        assert_eq!(s.narrative.outline_nodes[0].status, NodeStatus::Done);
    }

    #[test]
    fn milestone_below_threshold_accumulates_but_does_not_advance() {
        // 达标前一回合：progress 未过阈值 → 生成 Increment 但不翻 Done。
        let mut s = state_with_chars(&["li"]);
        s.narrative.outline_nodes.push(milestone_node("m1", 3.0, None, NodeStatus::Pending));
        s.world.insert("milestoneProgress_m1".into(), serde_json::json!(1.0));
        let decisions = vec![silent_decision("li", "d")];
        let outcomes = vec![outcome_of("li", "d", ArbiterResult::Success)]; // +1.0 → 2.0 < 3.0
        let patch = build_patch(s.revision, &decisions, &outcomes, &s);
        assert_eq!(progress_delta(&patch, "m1"), Some(1.0), "仍累积进度");
        assert!(!has_status_done(&patch, "m1"), "2.0 < 3.0 不应翻 Done");
    }

    #[test]
    fn milestone_advance_when_gates_on_relation() {
        // threshold=1.0 一回合即达标，但 advance_when 关系谓词未命中 → 不翻转；关系达标后翻转。
        let mut s = state_with_chars(&["li", "wang"]);
        s.narrative.outline_nodes.push(milestone_node(
            "m1",
            1.0,
            Some("relations[li->wang].affinity > 0.6"),
            NodeStatus::Pending,
        ));
        s.relations.push(RelationState {
            from: "li".into(),
            to: "wang".into(),
            trust: 0.0,
            affinity: 0.3, // 未命中 > 0.6
            fear: 0.0,
            debt: 0.0,
            known_to: vec![],
            notes: vec![],
        });
        let decisions = vec![silent_decision("li", "d")];
        let outcomes = vec![outcome_of("li", "d", ArbiterResult::Success)];

        // progress 达标（1.0>=1.0）但谓词未命中 → 累积但不翻转。
        let p1 = build_patch(s.revision, &decisions, &outcomes, &s);
        assert_eq!(progress_delta(&p1, "m1"), Some(1.0));
        assert!(!has_status_done(&p1, "m1"), "谓词未命中不应翻 Done");

        // 关系升到 0.7（命中谓词）→ progress 达标 + 谓词命中 → 翻 Done。
        s.relations[0].affinity = 0.7;
        let p2 = build_patch(s.revision, &decisions, &outcomes, &s);
        assert!(has_status_done(&p2, "m1"), "谓词命中 + 达阈值应翻 Done");
    }

    #[test]
    fn milestone_advance_when_missing_or_invalid_predicate_does_not_advance() {
        // 关系实体缺失 / 谓词非法 → eval 返 false/Err → gate_ok=false → 不误推进。
        let mut s = state_with_chars(&["li", "wang"]);
        s.narrative.outline_nodes.push(milestone_node(
            "m1",
            1.0,
            Some("relations[li->wang].trust > 0.5"), // 无 relations → 未命中
            NodeStatus::Pending,
        ));
        let decisions = vec![silent_decision("li", "d")];
        let outcomes = vec![outcome_of("li", "d", ArbiterResult::Success)];
        let p1 = build_patch(s.revision, &decisions, &outcomes, &s);
        assert!(!has_status_done(&p1, "m1"), "关系实体缺失 → 不推进");

        // 非法谓词表达式 → eval_predicate 返 Err → unwrap_or(false) → 不推进（防御路径）。
        s.narrative.outline_nodes[0].advance_when = Some("非法谓词无操作符".into());
        let p2 = build_patch(s.revision, &decisions, &outcomes, &s);
        assert_eq!(progress_delta(&p2, "m1"), Some(1.0), "仍累积进度");
        assert!(!has_status_done(&p2, "m1"), "谓词非法 → gate_ok=false → 不推进");
    }

    #[test]
    fn only_first_pending_milestone_advances_per_round() {
        // 两个里程碑均 Pending：一回合只推首个 Pending（m1），不碰 m2（保 5–8 里程碑顺序节拍）。
        let mut s = state_with_chars(&["li"]);
        s.narrative.outline_nodes.push(milestone_node("m1", 1.0, None, NodeStatus::Pending));
        s.narrative.outline_nodes.push(milestone_node("m2", 1.0, None, NodeStatus::Pending));
        let decisions = vec![silent_decision("li", "d")];
        let outcomes = vec![outcome_of("li", "d", ArbiterResult::Success)];
        let patch = build_patch(s.revision, &decisions, &outcomes, &s);
        assert!(progress_delta(&patch, "m1").is_some() && has_status_done(&patch, "m1"), "首个 m1 应推进");
        assert!(progress_delta(&patch, "m2").is_none(), "m2 本回合不应累积进度");
        assert!(!has_status_done(&patch, "m2"), "m2 本回合不应翻 Done");

        // 应用后：m1 Done、m2 仍 Pending。
        let s2 = reducer::validate_and_apply(&s, &patch).unwrap();
        assert_eq!(s2.narrative.outline_nodes[0].status, NodeStatus::Done);
        assert_eq!(s2.narrative.outline_nodes[1].status, NodeStatus::Pending);
    }

    #[test]
    fn is_terminal_milestone_guard() {
        // 守卫①：空里程碑集恒不发 MainlineDone。
        let mut s = state_with_chars(&["li"]); // 角色非空 → 排除 Starved
        assert_eq!(is_terminal(&s), None, "空 outline → 空里程碑 → 不 MainlineDone");

        // 旧硬节点（threshold=None）即使全 Done 也不计入里程碑 → 仍不 MainlineDone（旧硬节点零影响）。
        s.narrative.outline_nodes.push(OutlineNode {
            id: "h1".into(),
            summary: "硬节点".into(),
            constraint: ConstraintLevel::Hard,
            status: NodeStatus::Done,
            threshold: None,
            advance_when: None,
            weights: None,
        });
        assert_eq!(is_terminal(&s), None, "无 threshold 的硬节点全 Done 不触发 MainlineDone");

        // 混入一个 Pending 里程碑 → 里程碑集非空但未全 Done → 不 MainlineDone。
        s.narrative.outline_nodes.push(milestone_node("m1", 1.0, None, NodeStatus::Pending));
        assert_eq!(is_terminal(&s), None, "里程碑含 Pending → 不 MainlineDone");

        // 里程碑翻 Done → 里程碑集非空且全 Done → MainlineDone。
        s.narrative.outline_nodes[1].status = NodeStatus::Done;
        assert_eq!(
            is_terminal(&s),
            Some(Terminal::MainlineDone { ending: None }),
            "里程碑全 Done 且非空 → MainlineDone"
        );
    }

    #[test]
    fn legacy_node_without_threshold_uses_progressed_compat_path() {
        // 旧式节点（threshold=None）回归零变化：有 success → progressed=>done；无 success → 不推进；不写进度键。
        let mut s = state_with_chars(&["li"]);
        s.narrative.outline_nodes.push(OutlineNode {
            id: "n1".into(),
            summary: "老节点".into(),
            constraint: ConstraintLevel::Hard,
            status: NodeStatus::Pending,
            threshold: None,
            advance_when: None,
            weights: None,
        });
        let decisions = vec![silent_decision("li", "d")];

        // 有 success：兼容路径翻 Done，且不生成任何 milestoneProgress 进度键。
        let ok = build_patch(s.revision, &decisions, &[outcome_of("li", "d", ArbiterResult::Success)], &s);
        assert!(has_status_done(&ok, "n1"), "旧节点有 success 应 progressed=>done");
        assert!(
            !ok.operations.iter().any(|o| o.path.starts_with("world.milestoneProgress")),
            "旧节点不写进度键（阈值逻辑严格门 threshold.is_some()）"
        );

        // 全 Failure（progressed=false）：不推进（保留旧语义）。
        let fail = build_patch(s.revision, &decisions, &[outcome_of("li", "d", ArbiterResult::Failure)], &s);
        assert!(!has_status_done(&fail, "n1"), "旧节点无 success 不推进");
    }

    // ===== 关系演化（A. relation_dynamics）：build_patch 集成 + advance_when 复活回归 =====

    /// 带角色目标的友善决策（willSpeak=false → 不叠加 speak 项，便于数值断言）。
    fn friendly_decision_to(cid: &str, decision_id: &str, target: &str) -> RoleDecision {
        RoleDecision {
            decision_id: decision_id.into(),
            character_id: cid.into(),
            intent: "示好".into(),
            action: "上前帮助整理行装".into(),
            speak: SpeakIntent { will_speak: false, purpose: String::new() },
            targets: vec![target.into()],
            acceptable_costs: vec![],
            predictions: vec![],
            duration: 0,
        }
    }

    #[test]
    fn build_patch_emits_relation_ops() {
        // 回归核心：此前 build_patch 从不产生 relations 操作 → 关系图恒无边、恒为 0。
        let s = state_with_chars(&["li", "wang"]);
        let decisions = vec![friendly_decision_to("li", "d", "wang")];
        let outcomes = vec![outcome_of("li", "d", ArbiterResult::Success)];
        let patch = build_patch(s.revision, &decisions, &outcomes, &s);
        let rel_ops: Vec<&PatchOperation> =
            patch.operations.iter().filter(|o| o.path.starts_with("relations[")).collect();
        assert!(!rel_ops.is_empty(), "patch 应含关系操作：{:?}", patch.operations);
        assert!(rel_ops.iter().all(|o| o.op == PatchOp::Set), "关系操作应为 clamp 后终值 Set");
        assert!(
            rel_ops.iter().any(|o| o.path == "relations[li->wang].affinity"),
            "应含 li->wang affinity：{rel_ops:?}"
        );
        // 无角色目标的决策不产关系操作（既有回归不受影响）。
        let silent = vec![silent_decision("li", "d")];
        let p2 = build_patch(s.revision, &silent, &outcomes, &s);
        assert!(!p2.operations.iter().any(|o| o.path.starts_with("relations[")));
    }

    #[test]
    fn multi_round_friendly_interaction_revives_advance_when_predicate() {
        // advanceWhen 复活回归：relations 初始为空、谓词 `relations[li->wang].affinity > 0.2`
        // 此前永远无法命中；多回合友善互动（+0.08/回合）后应从未命中变命中。
        let mut s = state_with_chars(&["li", "wang"]);
        s.narrative.outline_nodes.push(milestone_node(
            "m1",
            1.0,
            Some("relations[li->wang].affinity > 0.2"),
            NodeStatus::Pending,
        ));
        let decisions = vec![friendly_decision_to("li", "d", "wang")];
        let outcomes = vec![outcome_of("li", "d", ArbiterResult::Success)];

        // 谓词按提交前状态评估：r1 看 0.0、r2 看 0.08、r3 看 0.16（皆未命中），
        // r4 看 0.24 > 0.2 → 命中翻 Done。
        let mut done_round: Option<u32> = None;
        for round in 1..=4u32 {
            let patch = build_patch(s.revision, &decisions, &outcomes, &s);
            if has_status_done(&patch, "m1") {
                done_round = Some(round);
            }
            s = reducer::validate_and_apply(&s, &patch).unwrap();
            if done_round.is_some() {
                break;
            }
        }
        assert_eq!(done_round, Some(4), "第 4 回合谓词应从未命中变命中");
        assert_eq!(s.narrative.outline_nodes[0].status, NodeStatus::Done);
        // 自动建边落定（reducer 零值建边）：known_to=[from,to]，affinity 已越过谓词阈值。
        let r = s.relations.iter().find(|r| r.from == "li" && r.to == "wang").expect("应自动建边");
        assert!(r.affinity > 0.2, "友善累积应越过 0.2：{}", r.affinity);
        assert_eq!(r.known_to, vec!["li".to_string(), "wang".to_string()]);
        let back = s.relations.iter().find(|r| r.from == "wang" && r.to == "li").expect("反向边亦建");
        assert!(back.affinity > 0.2);
    }

    // ===== 僵局打破提示（B. stall hint）：织入导演 prompt =====

    /// 记录每次 ModelCallSpec 的脚本化模型：校验各环节实际收到的 prompt 文本。
    struct RecordingModel {
        responses: std::sync::Mutex<Vec<Result<String, EngineError>>>,
        specs: Arc<std::sync::Mutex<Vec<crate::model::ModelCallSpec>>>,
    }

    #[async_trait::async_trait]
    impl crate::model::ModelClient for RecordingModel {
        async fn complete(
            &self,
            spec: &crate::model::ModelCallSpec,
            cancel: &CancelFlag,
        ) -> Result<crate::model::ModelOutput, EngineError> {
            cancel.check()?;
            self.specs.lock().unwrap().push(spec.clone());
            let mut lock = self.responses.lock().unwrap();
            if lock.is_empty() {
                return Err(EngineError::Model { message: "脚本响应耗尽".into(), retryable: false });
            }
            lock.remove(0).map(|content| crate::model::ModelOutput {
                content,
                input_tokens: Some(1),
                output_tokens: Some(1),
            })
        }
    }

    fn recording_host(
        responses: Vec<Result<String, EngineError>>,
    ) -> (Arc<EngineHost>, Arc<std::sync::Mutex<Vec<crate::model::ModelCallSpec>>>) {
        let specs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let host = Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1000)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(RecordingModel {
                responses: std::sync::Mutex::new(responses),
                specs: specs.clone(),
            }),
        });
        (host, specs)
    }

    fn happy_script() -> Vec<Result<String, EngineError>> {
        vec![
            Ok(r#"{"situation":"密室之中"}"#.to_string()),
            Ok(benign_decision()),
            Ok(benign_decision()),
            Ok(r#"{"prose":"一幕。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ]
    }

    #[tokio::test]
    async fn stall_hint_woven_into_director_prompt() {
        let (host, specs) = recording_host(happy_script());
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());
        let mut input = round_input("run-1", big_budget());
        input.stall_hint = Some("仲裁阻断：li 的行动与硬约束冲突（已连续 2 回合）".to_string());
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();
        assert!(out.blocked.is_none());
        let specs = specs.lock().unwrap();
        let director = specs.iter().find(|s| s.agent == "director").expect("应有导演调用");
        assert!(
            director.user.contains("仲裁阻断：li 的行动与硬约束冲突（已连续 2 回合）"),
            "导演 prompt 应含僵局提示：{}",
            director.user
        );
        assert!(director.user.contains("打破僵局"), "导演 prompt 应含打破僵局指引");
        // 提示只织入导演环节，不进决策环节。
        let decide = specs.iter().find(|s| s.agent == "roleDecide").expect("应有决策调用");
        assert!(!decide.user.contains("打破僵局"));
    }

    #[tokio::test]
    async fn none_stall_hint_keeps_director_prompt_clean() {
        let (host, specs) = recording_host(happy_script());
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());
        // round_input 默认 stall_hint: None。
        let out = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();
        assert!(out.blocked.is_none());
        let specs = specs.lock().unwrap();
        let director = specs.iter().find(|s| s.agent == "director").expect("应有导演调用");
        assert!(!director.user.contains("僵持未能推进"), "无提示时不应织入：{}", director.user);
        assert!(!director.user.contains("打破僵局"));
    }

    // ===== 本篇戏服（境界档，总规格 §6【拍板 3】「境界即布景」）=====

    fn douwang_costume() -> RealmCostume {
        RealmCostume {
            briefing: "本篇全员领斗王档戏服：能御空短距、能扛一记斗皇余威，仅此而已。".into(),
            flavor_notes: vec!["魂技译为斗气招式风味，内核不变".into()],
        }
    }

    /// 传入戏服 → **只**织进入场导演 prompt（§6「入场导演统一设定」），
    /// 且必带那句「不得据此判定谁能赢」的免责话术。
    /// 🔴 决策 / 仲裁 / 写作 / 审校四个环节一个字都不许看到它：戏服是给导演布景用的，
    ///    漏进决策就等于给角色发了一条「你现在是斗王」的自我暗示，那是能力暗示不是布景。
    #[tokio::test]
    async fn realm_costume_only_reaches_director() {
        let (host, specs) = recording_host(happy_script());
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());
        let mut input = round_input("run-1", big_budget());
        input.realm_costume = Some(douwang_costume());
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();
        assert!(out.blocked.is_none());

        let specs = specs.lock().unwrap();
        let director = specs.iter().find(|s| s.agent == "director").expect("应有导演调用");
        assert!(director.user.contains("本篇戏服（全员统一，同一水位）"), "导演应看到戏服标题：{}", director.user);
        assert!(director.user.contains("能扛一记斗皇余威"), "briefing 必须原文进导演 prompt");
        assert!(director.user.contains("跨体系风味翻译：魂技译为斗气招式风味，内核不变"), "flavorNotes 必须进导演 prompt");
        // 🔴 免责话术是红线的一部分：没有它，「全员斗王档」会被模型读成一把战力刻度。
        assert!(director.user.contains("不得据此判定谁能赢"), "必须带「只改描写、不改胜负」的免责话术");

        for s in specs.iter().filter(|s| s.agent != "director") {
            assert!(
                !s.user.contains("斗王档") && !s.user.contains("斗气招式"),
                "戏服泄漏到了 {} 环节：{}",
                s.agent,
                s.user
            );
        }
    }

    /// 🔴 **戏服绝不进判定域**：跑完一整回合后，StatePatch / DomainEvent / 提交后的世界状态
    /// （含 `CharacterState.resources`）与角色 DNA 卡上下文里都不得出现戏服的任何一个字。
    /// 它只活在导演那一次调用的 prompt 里，落地即消失——这正是「布景不是数值」的可执行定义。
    #[tokio::test]
    async fn realm_costume_never_reaches_state_or_events() {
        let model = Arc::new(CapturingModel::new(two_char_script()));
        let host = host_capturing(model.clone());
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());
        let mut input = round_input("run-1", big_budget());
        input.realm_costume = Some(douwang_costume());

        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();
        assert!(out.blocked.is_none());

        let dumped = serde_json::to_string(&out.scene.state_patch).unwrap()
            + &serde_json::to_string(&out.scene.events).unwrap()
            + &serde_json::to_string(&out.new_state).unwrap();
        for needle in ["斗王档", "斗皇余威", "斗气招式"] {
            assert!(!dumped.contains(needle), "红线：戏服「{needle}」渗进了 patch/事件/世界状态");
        }
        // 角色卡快照（active_cards → yourDna）与角色私有状态同样一个字节都不许被戏服改写。
        let ctx_li = decide_ctx_json(&model.decide_prompt_of("li"));
        assert!(!ctx_li["yourDna"].to_string().contains("斗王"), "红线：戏服不得进 DNA 卡");
        assert!(!ctx_li["yourState"].to_string().contains("斗王"), "红线：戏服不得进角色状态");
        // resources 是引擎判定域（谓词/仲裁读它）：戏服绝不能物化成持有事实。
        let resources = serde_json::to_string(
            &out.new_state.characters.get("li").map(|c| c.resources.clone()).unwrap_or_default(),
        )
        .unwrap();
        assert!(!resources.contains("斗王"), "红线：戏服不得物化进 CharacterState.resources");
    }

    /// 不传戏服（默认档）→ 导演 prompt 里**一个字节都不多**。
    /// 空戏服（两段皆空）必须与不传**逐字节等价**——否则"声明了一件没词儿的戏服"会悄悄改写
    /// 所有已声明世界的导演输入，而黄金骨架恰恰不声明 realmTier，那正是回归基线赖以不变的前提。
    #[tokio::test]
    async fn absent_or_blank_realm_costume_keeps_director_prompt_byte_identical() {
        async fn director_user(costume: Option<RealmCostume>) -> String {
            let (host, specs) = recording_host(happy_script());
            init_run(host.as_ref(), "run-1", false);
            let engine = NarrativeEngine::new(host.clone());
            let mut input = round_input("run-1", big_budget());
            input.realm_costume = costume;
            engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();
            let specs = specs.lock().unwrap();
            specs.iter().find(|s| s.agent == "director").expect("应有导演调用").user.clone()
        }

        let none = director_user(None).await;
        assert!(!none.contains("本篇戏服"), "不传戏服时导演 prompt 不得出现戏服段：{none}");

        let blank = director_user(Some(RealmCostume::default())).await;
        assert_eq!(blank, none, "空戏服必须与不传逐字节等价");

        let blank_ws = director_user(Some(RealmCostume {
            briefing: "   ".into(),
            flavor_notes: vec!["".into(), "  ".into()],
        }))
        .await;
        assert_eq!(blank_ws, none, "只有空白字符的戏服同样等价于不传");

        // 反向守卫：真戏服确实会改变导演 prompt（否则上面三条断言可能只是「什么都没接」）。
        let dressed = director_user(Some(douwang_costume())).await;
        assert_ne!(dressed, none, "真戏服必须真的进导演 prompt");
    }

    /// 🔴 **平权红线锁**（§6「跨体系靠风味翻译，不靠数值换算」+ §0.1）：戏服序列化后
    /// **一个数字 / 布尔都不许有**。与 server 侧 `assembly::realm_tier_carries_no_numeric_field`
    /// 是同一条红线的两端——谁想给它加 `level` / `powerTier` / `combatBonus`，两端都会先红。
    #[test]
    fn realm_costume_carries_no_numeric_field() {
        let v = serde_json::to_value(douwang_costume()).unwrap();
        for (k, val) in v.as_object().expect("戏服应序列化为对象") {
            let ok = val.is_string()
                || val.as_array().is_some_and(|a| a.iter().all(Value::is_string));
            assert!(ok, "戏服字段 `{k}` 不是字符串 / 字符串数组：{val} —— 数值化 = 平权红线违规");
        }
    }

    // ===== LLM 鲁棒性：role_decide 单角色确定性降级（空 content 兜底）=====

    /// 初始化含 a/b/c 三角色（无硬节点）的 run。
    fn init_run3(host: &EngineHost, run_id: &str) {
        let mut s = NarrativeState { schema_version: 1, run_id: run_id.into(), ..Default::default() };
        for c in ["a", "b", "c"] {
            s.characters.insert(c.into(), CharacterState::default());
        }
        NarrativeStore::new(host.fs.clone()).init(&s).unwrap();
    }

    fn cards3() -> BTreeMap<String, CharacterCardV2> {
        ["a", "b", "c"].into_iter().map(|n| (n.to_string(), minimal_card(n))).collect()
    }

    fn round_input3(run_id: &str, budget: RoundBudget) -> RoundInput {
        RoundInput {
            run_id: run_id.into(),
            mode: RunMode::Observe,
            active_cards: cards3(),
            other_cards_brief: BTreeMap::new(),
            // 默认档 = 不传自身身份：既有全部用例走的都是历史路径（回归保护）。
            self_identities: BTreeMap::new(),
            ambient_events: Vec::new(),
            whispers: BTreeMap::new(),
            fragments: BTreeMap::new(),
            temperature_decide: 0.0,
            temperature_writer: 0.7,
            max_output_tokens: 100,
            budget,
            approved_consents: Vec::new(),
            world_controlled: Vec::new(),
            locations: BTreeMap::new(),
            now_hint: 0,
            stall_hint: None,
            // 默认档 = 同意制：既有全部用例走的都是历史路径（回归保护）。
            lethality: Lethality::default(),
            // 默认档 = 无戏服：既有全部用例的导演 prompt 与接线前逐字节一致（回归保护）。
            realm_costume: None,
        }
    }

    /// 单组、三角色时 run_round 的模型脚本：director → decide(a) → decide(b) → decide(c) → writer → critic。
    /// `b` 的所有 attempt 返回空 content（DEFAULT_MAX_RETRIES 次）；a/c 正常。
    /// 并发决策在 ScriptedModel 下同步完成（无 yield），故脚本按 a→b→c 顺序确定性消费。
    fn degrade_middle_script() -> Vec<Result<String, EngineError>> {
        let mut resp: Vec<Result<String, EngineError>> = vec![
            Ok(r#"{"situation":"三人对坐，烛火摇曳。"}"#.to_string()), // director
            Ok(benign_decision()),                                    // decide a
        ];
        resp.extend((0..crate::model::DEFAULT_MAX_RETRIES).map(|_| Ok(String::new()))); // decide b：全空
        resp.push(Ok(benign_decision())); // decide c
        resp.push(Ok(r#"{"prose":"三人各怀心事，礼数周全。"}"#.to_string())); // writer
        resp.push(Ok(
            r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string(),
        )); // critic
        resp
    }

    // 测试点 #3 + #4：持续空 content → 单角色降级不 abort，整 tick 仍 commit。
    #[tokio::test]
    async fn single_role_degradation_skips_and_still_commits() {
        let (host, ev) = host_with(degrade_middle_script());
        init_run3(host.as_ref(), "run-1");
        let engine = NarrativeEngine::new(host.clone());
        let out = engine
            .run_round(&routes(), &prompts(), round_input3("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        // 未 abort：整 tick 正常提交。
        assert!(out.blocked.is_none(), "单角色降级不应 blocked");
        assert_eq!(out.new_state.revision, 1, "整 tick 应提交，revision 前进");
        // 降级角色 b 缺席，仅 a/c 进入 decisions（确定性定序）。
        let ids: Vec<&str> = out.scene.decisions.iter().map(|d| d.character_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"], "降级角色 b 应缺席，a/c 正常");
        // 场景与状态落盘。
        let store = NarrativeStore::new(host.fs.clone());
        assert_eq!(store.list_scene_ids("run-1").unwrap(), vec!["sc-0".to_string()]);
        assert_eq!(store.load("run-1").unwrap().revision, 1);
        // 其余两角色 outcomes/events 正常生成（a/c 各 ActionResolved + DialogueSpoken = 4 事件），b 无事件。
        let narrative_events = ev
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::Narrative { .. }))
            .count();
        assert_eq!(narrative_events, 4, "仅 a/c 产事件，降级角色 b 不产任何事件");
        // 发出确定性降级观测事件（含 cid=b）。
        let degraded = ev.0.lock().unwrap().iter().any(|e| {
            matches!(e, EngineEvent::ModelCall(l)
                if l.error.as_deref().map(|s| s.starts_with("character_degraded:b:")).unwrap_or(false))
        });
        assert!(degraded, "应发出 b 的确定性降级观测事件");
    }

    // 测试点：全部角色都失败 → run_round 合理失败（不静默提交空回合）。
    #[tokio::test]
    async fn all_roles_degradation_fails_round_without_commit() {
        let mut resp: Vec<Result<String, EngineError>> =
            vec![Ok(r#"{"situation":"三人对坐。"}"#.to_string())]; // director
        for _ in 0..3 {
            resp.extend((0..crate::model::DEFAULT_MAX_RETRIES).map(|_| Ok(String::new())));
        }
        let (host, _ev) = host_with(resp);
        init_run3(host.as_ref(), "run-1");
        let engine = NarrativeEngine::new(host.clone());
        let err = engine
            .run_round(&routes(), &prompts(), round_input3("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "model_output", "全角色降级应上抛模型输出错误");
        // 未提交任何状态、无场景落盘。
        let store = NarrativeStore::new(host.fs.clone());
        assert_eq!(store.load("run-1").unwrap().revision, 0);
        assert!(store.list_scene_ids("run-1").unwrap().is_empty());
    }

    // 测试点 #6：确定性——同一脚本（含空 content 序列）两次 run_round 的 scene / new_state 逐字节一致。
    #[tokio::test]
    async fn degradation_is_deterministic_across_runs() {
        let (h1, _) = host_with(degrade_middle_script());
        init_run3(h1.as_ref(), "run-det");
        let o1 = NarrativeEngine::new(h1.clone())
            .run_round(&routes(), &prompts(), round_input3("run-det", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        let (h2, _) = host_with(degrade_middle_script());
        init_run3(h2.as_ref(), "run-det");
        let o2 = NarrativeEngine::new(h2.clone())
            .run_round(&routes(), &prompts(), round_input3("run-det", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        assert_eq!(
            serde_json::to_string(&o1.scene).unwrap(),
            serde_json::to_string(&o2.scene).unwrap(),
            "scene（decisions/outcomes/StatePatch/events）应逐字节一致"
        );
        assert_eq!(
            serde_json::to_string(&o1.new_state).unwrap(),
            serde_json::to_string(&o2.new_state).unwrap(),
            "new_state 应逐字节一致"
        );
    }

    // 测试点 #7：Cancelled 不被降级吞掉——决策阶段返回 Cancelled 必须原样传播。
    #[tokio::test]
    async fn cancelled_not_swallowed_by_degradation() {
        let resp = vec![
            Ok(r#"{"situation":"对坐。"}"#.to_string()), // director
            Err(EngineError::Cancelled),                 // decide li → Cancelled
        ];
        let (host, _ev) = host_with(resp);
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());
        let err = engine
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "cancelled", "Cancelled 必须透传，不被降级为跳过");
        assert_eq!(NarrativeStore::new(host.fs.clone()).load("run-1").unwrap().revision, 0);
    }

    // ===== 生死契约三档（规格 §11【拍板 24】）=====

    /// 致死行动脚本（与 `kill_decision` 同源，另给一个可指定行动文本的版本）。
    fn exile_decision(target: &str) -> String {
        format!(
            r#"{{"intent":"永绝后患","action":"将其流放到千里之外的荒岛","speak":{{"willSpeak":false,"purpose":""}},"targets":["{target}"],"acceptableCosts":[],"predictions":[]}}"#
        )
    }

    /// 致死回合脚本：director → decide(li 杀 wang) → decide(wang) → writer → critic（无硬节点 → 无仲裁模型调用）。
    fn kill_script() -> Vec<Result<String, EngineError>> {
        vec![
            Ok(r#"{"situation":"对峙时刻"}"#.to_string()),
            Ok(kill_decision("wang")),
            Ok(benign_decision()),
            Ok(r#"{"prose":"刀光一闪。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ]
    }

    fn count_consent_requested(out: &RoundOutcome) -> usize {
        out.scene
            .events
            .iter()
            .filter(|e| e.event_type == DomainEventType::ConsentRequested)
            .count()
    }

    /// 某角色的 ActionResolved 事件（不含被门控/被剔除者）。
    fn action_resolved_of<'a>(out: &'a RoundOutcome, cid: &str) -> Option<&'a DomainEvent> {
        out.scene.events.iter().find(|e| {
            e.event_type == DomainEventType::ActionResolved && e.actor_ids.iter().any(|a| a == cid)
        })
    }

    // --- 回归保护：默认档 = 同意制，行为与历史零差异 ---

    #[test]
    fn default_lethality_is_consent() {
        // 本任务最重要的兼容性约束：调用方不传 → 同意制（现行机制）。
        assert_eq!(Lethality::default(), Lethality::Consent);
        assert_eq!(RoundInput::default().lethality, Lethality::Consent);
        assert_eq!(round_input("run-1", big_budget()).lethality, Lethality::Consent);
    }

    #[tokio::test]
    async fn consent_default_and_explicit_are_byte_identical() {
        // 显式传 Consent 与不传（默认）必须产出逐字节一致的 scene/state——默认档不走任何新分支。
        let (h1, _) = host_with(kill_script());
        init_run(h1.as_ref(), "run-1", false);
        let o1 = NarrativeEngine::new(h1.clone())
            .run_round(&routes(), &prompts(), round_input("run-1", big_budget()), &CancelFlag::new())
            .await
            .unwrap();

        let (h2, _) = host_with(kill_script());
        init_run(h2.as_ref(), "run-1", false);
        let mut input = round_input("run-1", big_budget());
        input.lethality = Lethality::Consent;
        let o2 =
            NarrativeEngine::new(h2.clone()).run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert_eq!(
            serde_json::to_string(&o1.scene).unwrap(),
            serde_json::to_string(&o2.scene).unwrap(),
            "默认档与显式 Consent 的 scene 必须逐字节一致"
        );
        assert_eq!(
            serde_json::to_string(&o1.new_state).unwrap(),
            serde_json::to_string(&o2.new_state).unwrap(),
        );
        // 且仍是历史行为：死亡被门控，产 ConsentRequested + pending_consents。
        assert_eq!(count_consent_requested(&o1), 1, "同意制下死亡仍须临场征询");
        assert!(o1.new_state.narrative.pending_consents.iter().any(|p| p.subject == "wang"));
    }

    // --- 生死状档（Deathmatch）：不再临场征询，但裁判仍在 ---

    #[tokio::test]
    async fn deathmatch_lands_death_without_consent_request() {
        let (host, ev) = host_with(kill_script());
        init_run(host.as_ref(), "run-1", false);
        let engine = NarrativeEngine::new(host.clone());
        let mut input = round_input("run-1", big_budget());
        input.lethality = Lethality::Deathmatch;
        // approved_consents / world_controlled 皆空——放行只能来自契约档本身。
        let out = engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);
        // ① 死亡直接落定：li 的结果进 StatePatch（节拍记录）与 ActionResolved 事件。
        assert!(
            out.new_state.narrative.pacing_notes.iter().any(|n| n.starts_with("li｜")),
            "生死状档下致死结果应直接落定"
        );
        let ar = action_resolved_of(&out, "li").expect("li 应有 ActionResolved");
        assert_eq!(ar.fact["result"], "Success", "结果未被降级（生死状档不降级）");
        // ② 不产生 ConsentRequest。
        assert_eq!(count_consent_requested(&out), 0, "入场即签生死状 → 事后不再临场征询");
        // ③ 不写 pending_consents。
        assert!(
            out.new_state.narrative.pending_consents.is_empty(),
            "生死状档不产生待审批同意"
        );
        // ④ 领域事件通道也不发 consent_requested。
        let emitted = ev
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::Narrative { payload, .. }
                if payload.get("type").and_then(|v| v.as_str()) == Some("consent_requested")))
            .count();
        assert_eq!(emitted, 0);
    }

    #[tokio::test]
    async fn deathmatch_clears_stale_pending_from_previous_contract() {
        // 改档自愈（approved_landed 取舍的依据）：Consent 档遗留的 pending 在死亡落定后被清除，
        // 不留「请求你同意自己的死亡」的悬空请求。
        let (host, _ev) = host_with(kill_script());
        let mut s = NarrativeState { schema_version: 1, run_id: "run-1".into(), ..Default::default() };
        s.characters.insert("li".into(), CharacterState::default());
        s.characters.insert("wang".into(), CharacterState::default());
        s.narrative
            .pending_consents
            .push(PendingConsent { subject: "wang".into(), event_kind: "death".into() });
        NarrativeStore::new(host.fs.clone()).init(&s).unwrap();

        let mut input = round_input("run-1", big_budget());
        input.lethality = Lethality::Deathmatch;
        let out = NarrativeEngine::new(host.clone())
            .run_round(&routes(), &prompts(), input, &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.new_state.narrative.pending_consents.is_empty(), "旧档遗留 pending 应被清除");
    }

    #[tokio::test]
    async fn deathmatch_does_not_override_arbiter_invalid() {
        // 【规格红线】取消的是被杀者否决权，不是裁判：仲裁判 Invalid 的致死行动，
        // 生死状档同样不得落定（`classify` 的 Success|PartialSuccess 前置条件原样保留）。
        let responses = vec![
            Ok(r#"{"situation":"对峙一触即发"}"#.to_string()),
            Ok(kill_decision("wang")), // decide li：致死 + 存在 pending 硬节点 → R5 交模型裁决
            Ok(benign_decision()),     // decide wang
            Ok(r#"{"outcomes":[{"decisionId":"dec:run-1:0:li","result":"invalid","consequence":"叙事上不成立：他手中并无兵刃"}]}"#.to_string()),
            Ok(r#"{"prose":"杀意未及出手便已落空。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run(host.as_ref(), "run-1", true); // 含 pending 硬节点 → 致死行动进模型仲裁
        let mut input = round_input("run-1", big_budget());
        input.lethality = Lethality::Deathmatch;
        let out = NarrativeEngine::new(host.clone())
            .run_round(&routes(), &prompts(), input, &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_none(), "Invalid 不阻断整回合，只是该行动不成立");
        // 裁决确已生效（模型漏判会回退 Success，故此断言同时守住脚本 decision_id 的正确性）。
        let o = out.scene.outcomes.iter().find(|o| o.character_id == "li").expect("li 应有 outcome");
        assert_eq!(o.result, ArbiterResult::Invalid);
        // 被判 Invalid → 无 ActionResolved、无节拍记录：死亡未落定。
        assert!(action_resolved_of(&out, "li").is_none(), "Invalid 的致死行动不得产生 ActionResolved");
        assert!(
            !out.new_state.narrative.pacing_notes.iter().any(|n| n.starts_with("li｜")),
            "Invalid 的致死行动不得进入 StatePatch"
        );
        assert_eq!(count_consent_requested(&out), 0);
    }

    #[tokio::test]
    async fn deathmatch_does_not_override_arbiter_blocked() {
        // 同上：判 Blocked（危及硬节点，arbiter R5 硬节点保护）→ 整回合阻断、零提交，与契约档无关。
        let responses = vec![
            Ok(r#"{"situation":"对峙一触即发"}"#.to_string()),
            Ok(kill_decision("wang")),
            Ok(benign_decision()),
            Ok(r#"{"outcomes":[{"decisionId":"dec:run-1:0:li","result":"blocked","consequence":"该行动会使硬节点无法达成"}]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run(host.as_ref(), "run-1", true);
        let mut input = round_input("run-1", big_budget());
        input.lethality = Lethality::Deathmatch;
        let out = NarrativeEngine::new(host.clone())
            .run_round(&routes(), &prompts(), input, &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_some(), "Blocked 仍阻断整回合");
        assert_eq!(out.new_state.revision, 0, "生死状档不得绕过硬节点保护而提交");
        assert!(NarrativeStore::new(host.fs.clone()).list_scene_ids("run-1").unwrap().is_empty());
    }

    // --- 庇护档（Sanctuary）：写作前降级，正文与公共事实同口径 ---

    #[tokio::test]
    async fn sanctuary_downgrades_lethal_action_and_keeps_prose_in_sync() {
        // 用 RecordingModel 抓写作环节实际收到的 outcomes：**写作输入必须已是降级后的口径**，
        // 否则会出现「正文写死了、事件却是重伤/退场」的公共事实矛盾（§0.3）。
        let (host, specs) = recording_host(kill_script());
        init_run(host.as_ref(), "run-1", false);
        let mut input = round_input("run-1", big_budget());
        input.lethality = Lethality::Sanctuary;
        let out = NarrativeEngine::new(host.clone())
            .run_round(&routes(), &prompts(), input, &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);

        // ① 写作输入已降级：写手看到 PartialSuccess + 重伤语义 consequence（且被明确告知不得写死亡）。
        let writer = {
            let specs = specs.lock().unwrap();
            specs.iter().find(|s| s.agent == "writer").expect("应有写作调用").user.clone()
        };
        assert!(
            writer.contains(SANCTUARY_DOWNGRADE_CONSEQUENCE),
            "写作 prompt 应含降级后的 consequence：{writer}"
        );
        assert!(writer.contains("PartialSuccess"), "写作 prompt 中 result 应已降级：{writer}");

        // ② 仲裁结果就地降级 + 规则标记（透明战报可解释「为什么没死」）。
        let o = out.scene.outcomes.iter().find(|o| o.character_id == "li").expect("li 应有 outcome");
        assert_eq!(o.result, ArbiterResult::PartialSuccess);
        assert_eq!(o.consequence, SANCTUARY_DOWNGRADE_CONSEQUENCE);
        assert!(o.rule_refs.iter().any(|r| r == SANCTUARY_RULE_REF));

        // ③ 口径一致：公共事实（ActionResolved.fact）与写作输入同源同字。
        let ar = action_resolved_of(&out, "li").expect("降级后的结果应正常落定");
        assert_eq!(ar.fact["result"], "PartialSuccess");
        assert_eq!(ar.fact["consequence"], SANCTUARY_DOWNGRADE_CONSEQUENCE);
        assert!(out
            .new_state
            .narrative
            .pacing_notes
            .iter()
            .any(|n| n.starts_with("li｜PartialSuccess｜") && n.contains("仍然活着")));

        // ④ 庇护世界不存在死亡这件事 → 无死亡门控、无 pending。
        assert_eq!(count_consent_requested(&out), 0, "已降级为重伤（可恢复）→ 无不可逆事件可征询");
        assert!(out.new_state.narrative.pending_consents.is_empty());
    }

    #[tokio::test]
    async fn sanctuary_keeps_consent_gate_for_non_lethal_irreversible() {
        // 庇护档只管死亡：永久退场（流放）仍走原有同意制流程，一字未改。
        let responses = vec![
            Ok(r#"{"situation":"朝堂问罪"}"#.to_string()),
            Ok(exile_decision("wang")), // li：流放 wang（不可逆·永久退场）
            Ok(benign_decision()),      // wang
            Ok(r#"{"prose":"判词落下。"}"#.to_string()),
            Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string()),
        ];
        let (host, _ev) = host_with(responses);
        init_run(host.as_ref(), "run-1", false);
        let mut input = round_input("run-1", big_budget());
        input.lethality = Lethality::Sanctuary;
        let out = NarrativeEngine::new(host.clone())
            .run_round(&routes(), &prompts(), input, &CancelFlag::new())
            .await
            .unwrap();

        assert!(out.blocked.is_none());
        // 未被降级（不是致死行动）。
        let o = out.scene.outcomes.iter().find(|o| o.character_id == "li").expect("li 应有 outcome");
        assert_eq!(o.result, ArbiterResult::Success);
        assert!(!o.rule_refs.iter().any(|r| r == SANCTUARY_RULE_REF), "非致死行动不应被降级标记");
        // 仍走同意制门控：产 ConsentRequested(permanent_exit) + pending，且不落定。
        let cr: Vec<&DomainEvent> = out
            .scene
            .events
            .iter()
            .filter(|e| e.event_type == DomainEventType::ConsentRequested)
            .collect();
        assert_eq!(cr.len(), 1, "永久退场在庇护档下仍须临场同意");
        assert_eq!(cr[0].fact["eventKind"], "permanent_exit");
        assert!(out
            .new_state
            .narrative
            .pending_consents
            .iter()
            .any(|p| p.subject == "wang" && p.event_kind == "permanent_exit"));
        assert!(
            !out.new_state.narrative.pacing_notes.iter().any(|n| n.starts_with("li｜")),
            "未获批的永久退场不得落定"
        );
    }

    // --- apply_lethality 纯函数层：确定性 + 非庇护档 no-op ---

    fn lethal_decision_of(cid: &str, decision_id: &str) -> RoleDecision {
        let mut d = silent_decision(cid, decision_id);
        d.action = "拔剑杀死叛徒".into();
        d.targets = vec!["wang".into()];
        d
    }

    #[test]
    fn apply_lethality_is_noop_for_consent_and_deathmatch() {
        let decisions = vec![lethal_decision_of("li", "d1")];
        for arch in [Lethality::Consent, Lethality::Deathmatch] {
            let mut outcomes = vec![outcome_of("li", "d1", ArbiterResult::Success)];
            let before = outcomes.clone();
            apply_lethality(&mut outcomes, &decisions, arch);
            assert_eq!(outcomes[0].result, before[0].result, "{arch:?} 不应降级");
            assert_eq!(outcomes[0].consequence, before[0].consequence);
            assert!(outcomes[0].rule_refs.is_empty());
        }
    }

    #[test]
    fn apply_lethality_only_touches_landed_lethal_outcomes() {
        // 降级集合 = `is_lethal`：仅 Success/PartialSuccess 的致死行动；Invalid/Blocked/非致死一律不动。
        // 这同时是 `classify` 在庇护档下「不产出 death」的同一判据（两处口径一致的机械证明）。
        let decisions = vec![
            lethal_decision_of("a", "d-a"), // 致死 + Success → 降级
            lethal_decision_of("b", "d-b"), // 致死 + Invalid → 不动
            silent_decision("c", "d-c"),    // 非致死 + Success → 不动
        ];
        let mut outcomes = vec![
            outcome_of("a", "d-a", ArbiterResult::Success),
            outcome_of("b", "d-b", ArbiterResult::Invalid),
            outcome_of("c", "d-c", ArbiterResult::Success),
        ];
        apply_lethality(&mut outcomes, &decisions, Lethality::Sanctuary);

        assert_eq!(outcomes[0].result, ArbiterResult::PartialSuccess);
        assert_eq!(outcomes[0].consequence, SANCTUARY_DOWNGRADE_CONSEQUENCE);
        assert_eq!(outcomes[1].result, ArbiterResult::Invalid, "被裁判否掉的致死行动不参与降级");
        assert_eq!(outcomes[1].consequence, "后果");
        assert_eq!(outcomes[2].result, ArbiterResult::Success);
        assert_eq!(outcomes[2].consequence, "后果");

        // 幂等（确定性 replay）：再跑一次结果不变，规则标记不重复累积。
        let snapshot = serde_json::to_string(&outcomes).unwrap();
        apply_lethality(&mut outcomes, &decisions, Lethality::Sanctuary);
        assert_eq!(serde_json::to_string(&outcomes).unwrap(), snapshot);
        assert_eq!(outcomes[0].rule_refs.iter().filter(|r| *r == SANCTUARY_RULE_REF).count(), 1);
    }

    #[test]
    fn classify_and_apply_lethality_share_one_verdict() {
        // 两处口径一致的直接断言：庇护档下 `apply_lethality` 降级的那一条，
        // `classify` 必须同时停止产出 death（否则正文写重伤、事件写死亡）。
        let rules = IrreversibleRules::new();
        let d = lethal_decision_of("li", "d1");
        let mut o = outcome_of("li", "d1", ArbiterResult::Success);

        assert!(rules.is_lethal(&o, &d));
        // 同意制/生死状：仍分类为 death（仅门控放行策略不同）。
        assert_eq!(rules.classify(&o, &d, Lethality::Consent).unwrap().0, "death");
        assert_eq!(rules.classify(&o, &d, Lethality::Deathmatch).unwrap().0, "death");
        // 庇护：不产出 death，且不改判为 permanent_exit/relation（与降级后的「重伤仍活着」同口径）。
        assert!(rules.classify(&o, &d, Lethality::Sanctuary).is_none());

        // 降级后（PartialSuccess）判据不变，仍在同一集合内。
        apply_lethality(std::slice::from_mut(&mut o), std::slice::from_ref(&d), Lethality::Sanctuary);
        assert!(rules.is_lethal(&o, &d));
        assert!(rules.classify(&o, &d, Lethality::Sanctuary).is_none());
    }

    // ========================================================================
    // 人设保险 第 1 级 · 事前 · 底线硬约束（总规格 §7）—— run_round 端到端
    // ========================================================================

    fn card_with_bottom_lines(name: &str, lines: &[&str]) -> CharacterCardV2 {
        let mut c = minimal_card(name);
        c.dramatic_core.bottom_lines = lines.iter().map(|s| s.to_string()).collect();
        c
    }

    /// 在 `round_input` 基础上替换活跃卡（只动底线声明，其余入参一律默认档）。
    fn input_with_bottom_lines(
        run_id: &str,
        li_lines: &[&str],
        wang_lines: &[&str],
    ) -> RoundInput {
        let mut input = round_input(run_id, big_budget());
        input.active_cards = [
            ("li".to_string(), card_with_bottom_lines("李", li_lines)),
            ("wang".to_string(), card_with_bottom_lines("王", wang_lines)),
        ]
        .into_iter()
        .collect();
        input
    }

    /// 违反「不做伪证」的提案（字面重述禁止行为）。
    fn forging_decision() -> String {
        r#"{"intent":"保住他","action":"在堂上做伪证，一口咬定他当夜不在场","speak":{"willSpeak":true,"purpose":"作证"},"targets":[],"acceptableCosts":[],"predictions":[]}"#.to_string()
    }

    fn director() -> Result<String, EngineError> {
        Ok(r#"{"situation":"堂前灯火通明，众人各自落座"}"#.to_string())
    }
    fn writer() -> Result<String, EngineError> {
        Ok(r#"{"prose":"堂中礼数周全，暗流未起。"}"#.to_string())
    }
    fn critic() -> Result<String, EngineError> {
        Ok(r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string())
    }

    fn outcome_of_cid<'a>(out: &'a RoundOutcome, cid: &str) -> &'a ArbiterOutcome {
        out.scene.outcomes.iter().find(|o| o.character_id == cid).expect("该角色应有裁决")
    }

    /// 🔴 回归保护（最重要）：卡上没声明底线 → 整段短路，调用序列与产物与接线前完全一致。
    /// 并且——声明了底线但**没违反**时，产物与「压根没声明」逐字节相同：
    /// 底线不带任何数值/结果影响（§0.1 平权：底线是「不会做什么」，不是「更强」）。
    #[tokio::test]
    async fn bottom_lines_absent_or_unviolated_are_byte_identical() {
        let run = |cards: Option<BTreeMap<String, CharacterCardV2>>| async move {
            let model = Arc::new(CapturingModel::new(two_char_script()));
            let host = host_capturing(model.clone());
            init_run(host.as_ref(), "run-1", true);
            let engine = NarrativeEngine::new(host.clone());
            let mut input = round_input("run-1", big_budget());
            if let Some(c) = cards {
                input.active_cards = c;
            }
            let out =
                engine.run_round(&routes(), &prompts(), input, &CancelFlag::new()).await.unwrap();
            (serde_json::to_string(&out.scene).unwrap(), model.decide_prompts().len(), out)
        };

        // ① 无底线声明：调用序列 = director + 决策×2 + writer + critic（无重生成）。
        let (plain_scene, plain_decides, plain) = run(None).await;
        assert_eq!(plain_decides, 2, "无底线 → 不得多出任何决策调用");
        assert!(plain.blocked.is_none());
        assert_eq!(plain.new_state.revision, 1);

        // ② 声明底线但未违反：逐字节相同 —— 底线不改变任何结果。
        let lines: &[&str] = &["不做伪证", "不篡改记录"];
        let (declared_scene, declared_decides, declared) = run(Some(
            [
                ("li".to_string(), card_with_bottom_lines("李", lines)),
                ("wang".to_string(), card_with_bottom_lines("王", lines)),
            ]
            .into_iter()
            .collect(),
        ))
        .await;
        assert_eq!(declared_decides, 2, "未违反 → 不得触发重生成");
        assert_eq!(declared_scene, plain_scene, "声明底线但没违反 → 产物必须逐字节一致");
        assert_eq!(declared.budget.spent_tokens, plain.budget.spent_tokens, "也不得多花一个 token");
    }

    /// 规格主路径：违反自己卡的底线 → **拒绝该提案、重新生成**；重生成合规后照常落定。
    #[tokio::test]
    async fn bottom_line_violation_is_regenerated_then_committed() {
        // 调用序列：director → decide(li 违规) → decide(wang) → 【重生成 li】→ writer → critic。
        let model = Arc::new(CapturingModel::new(vec![
            director(),
            Ok(forging_decision()),
            Ok(benign_decision()),
            Ok(benign_decision()), // 重生成：换了一个不违背底线的行动
            writer(),
            critic(),
        ]));
        let host = host_capturing(model.clone());
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());

        let out = engine
            .run_round(
                &routes(),
                &prompts(),
                input_with_bottom_lines("run-1", &["不做伪证"], &[]),
                &CancelFlag::new(),
            )
            .await
            .unwrap();

        // 重生成确实发生了一次（且只有违规的那个角色被重问）。
        let decide_prompts = model.decide_prompts();
        assert_eq!(decide_prompts.len(), 3, "只应多出违规角色的那一次重生成");
        let regen = &decide_prompts[2];
        assert!(regen.starts_with("以下是【仅你（li）可见】"), "重问的必须是违规的 li");
        assert!(regen.contains("bottomLineRejection"), "重生成上下文须带底线拦截回执");
        assert!(regen.contains("不做伪证"), "回执须带被违反的底线原文");
        assert!(regen.contains("在堂上做伪证"), "回执须带被拒的提案原文");
        // 其余两次（首轮决策）不得出现回执字段。
        assert!(!decide_prompts[0].contains("bottomLineRejection"));
        assert!(!decide_prompts[1].contains("bottomLineRejection"));

        // 重生成后的提案合规 → 正常落定、正常提交，战报里不留底线拦截痕迹。
        assert!(out.blocked.is_none());
        assert_eq!(out.new_state.revision, 1);
        assert_eq!(outcome_of_cid(&out, "li").result, ArbiterResult::Success);
        assert!(!out
            .scene
            .outcomes
            .iter()
            .any(|o| o.rule_refs.iter().any(|r| r == arbiter::BOTTOM_LINE_RULE_REF)));
        // 落定的是重生成后的行动，违规原文不进公共事实。
        assert_eq!(out.scene.decisions[0].action, "上前拱手行礼");
        assert!(!serde_json::to_string(&out.scene).unwrap().contains("做伪证"));
        // 重生成据实计费（§17）：场景基线 N+组数*2+2 = 6 次调用，外加 1 次重生成 = 7。
        assert_eq!(out.budget.spent_tokens, 7 * 100);
    }

    /// 降级序列 ②：重试上限用尽仍违规 → 该提案判 `Invalid`(`rule:bottom_line`)，
    /// 不落状态、不发事件、不进节拍记录；**世界照常推进**（只延后这个角色这一拍）。
    #[tokio::test]
    async fn bottom_line_rejected_after_regen_limit_and_world_keeps_running() {
        let model = Arc::new(CapturingModel::new(vec![
            director(),
            Ok(forging_decision()),
            Ok(benign_decision()),
            Ok(forging_decision()), // 重生成后依旧违规
            writer(),
            critic(),
        ]));
        let host = host_capturing(model.clone());
        init_run(host.as_ref(), "run-1", true);
        let engine = NarrativeEngine::new(host.clone());

        let out = engine
            .run_round(
                &routes(),
                &prompts(),
                input_with_bottom_lines("run-1", &["不做伪证"], &[]),
                &CancelFlag::new(),
            )
            .await
            .unwrap();

        // 重试上限生效：只重生成 MAX_BOTTOM_LINE_REGEN 次，不无限重试。
        assert_eq!(model.decide_prompts().len(), 2 + MAX_BOTTOM_LINE_REGEN);

        let li = outcome_of_cid(&out, "li");
        assert_eq!(li.result, ArbiterResult::Invalid, "拦截 = 拒绝提案");
        assert_eq!(li.rule_refs, vec![arbiter::BOTTOM_LINE_RULE_REF.to_string()]);
        assert!(li.consequence.contains("并未发生"), "写作须被告知它没发生：{}", li.consequence);

        // 世界没有卡死：整拍照常提交，别人照常行动。
        assert!(out.blocked.is_none(), "单个角色违规不得阻断整个世界");
        assert_eq!(out.new_state.revision, 1);
        assert_eq!(outcome_of_cid(&out, "wang").result, ArbiterResult::Success);

        // 被拦提案在下游完全惰性：无 ActionResolved、无节拍记录、无状态操作。
        assert!(action_resolved_of(&out, "li").is_none(), "Invalid 不得产生 ActionResolved");
        assert!(
            !out.new_state.narrative.pacing_notes.iter().any(|n| n.contains("做伪证")),
            "被拦行动不得进节拍记录（那是公共事实）：{:?}",
            out.new_state.narrative.pacing_notes
        );
        assert!(
            !out.scene
                .state_patch
                .operations
                .iter()
                .any(|op| serde_json::to_string(op).unwrap().contains("做伪证")),
            "被拦行动不得进 StatePatch"
        );
        // 提案本身仍留在战报里（透明：看得见他提了什么、被自己的哪条底线否掉）。
        assert!(out.scene.decisions.iter().any(|d| d.action.contains("做伪证")));
        // I2 不变量仍成立：patch 的来源决策 ⊆ 本回合决策。
        assert_eq!(out.scene.state_patch.source_decision_ids.len(), 2);
    }

    /// 🔴 红线（§0.1 平权）：拦截**只能拒绝提案，不能改判成功率**。
    /// 对照组 = 同一份剧本、同一个违规行动，但 li 没声明底线。
    /// 断言：拦截既不给 li 更好的结果，也不改动 wang 的任何裁决。
    #[tokio::test]
    async fn bottom_line_rejection_never_improves_any_outcome() {
        let script = || {
            vec![
                director(),
                Ok(forging_decision()),
                Ok(benign_decision()),
                Ok(forging_decision()),
                writer(),
                critic(),
            ]
        };
        // 对照组：无底线 → 同样的行动被判 Success（规则层 rule:clear）。
        let (h0, _) = host_with(script());
        init_run(h0.as_ref(), "run-1", true);
        let control = NarrativeEngine::new(h0.clone())
            .run_round(
                &routes(),
                &prompts(),
                input_with_bottom_lines("run-1", &[], &[]),
                &CancelFlag::new(),
            )
            .await
            .unwrap();
        // 实验组：声明底线 → 同样的行动被拦。
        let (h1, _) = host_with(script());
        init_run(h1.as_ref(), "run-1", true);
        let guarded = NarrativeEngine::new(h1.clone())
            .run_round(
                &routes(),
                &prompts(),
                input_with_bottom_lines("run-1", &["不做伪证"], &[]),
                &CancelFlag::new(),
            )
            .await
            .unwrap();

        // ① 声明底线的角色只可能变差（Success → Invalid），绝不可能变好。
        assert_eq!(outcome_of_cid(&control, "li").result, ArbiterResult::Success);
        assert_eq!(outcome_of_cid(&guarded, "li").result, ArbiterResult::Invalid);
        // ② 拦截不产生任何新的成功：实验组的成功数严格少于对照组。
        let count_ok = |o: &RoundOutcome| {
            o.scene
                .outcomes
                .iter()
                .filter(|x| {
                    matches!(x.result, ArbiterResult::Success | ArbiterResult::PartialSuccess)
                })
                .count()
        };
        assert_eq!(count_ok(&control), 2);
        assert_eq!(count_ok(&guarded), 1, "拦截只减不增，绝不改判成功率");
        // ③ 旁人不受影响：wang 的裁决逐字节一致（底线不外溢成任何优势或惩罚）。
        assert_eq!(
            serde_json::to_string(outcome_of_cid(&control, "wang")).unwrap(),
            serde_json::to_string(outcome_of_cid(&guarded, "wang")).unwrap()
        );
        // ④ 被拦者不得因此少付代价或多拿东西：状态里没有任何属于 li 的新操作
        //    （对照组有 li 的节拍记录，实验组必须一条都没有）。
        let li_ops = |o: &RoundOutcome| {
            o.scene
                .state_patch
                .operations
                .iter()
                .filter(|op| {
                    op.path.starts_with("characters.li")
                        || op
                            .value
                            .as_ref()
                            .and_then(|v| v.as_str())
                            .map(|s| s.starts_with("li｜"))
                            .unwrap_or(false)
                })
                .count()
        };
        assert_eq!(li_ops(&control), 1, "对照组里 li 的行动会留下节拍记录");
        assert_eq!(li_ops(&guarded), 0, "被拦者不得在状态里留下任何操作");
    }

    /// 降级序列 ③：本拍**全部**提案都被底线拦下 → 整拍 `blocked`、零提交
    /// （VALIDATION §5「宁可停拍，不让失败输出成为永久公共事实」；上层据此暂停世界 + 人工复核）。
    #[tokio::test]
    async fn bottom_line_all_proposals_rejected_blocks_the_tick() {
        let (host, _ev) = host_with(vec![
            director(),
            Ok(forging_decision()),
            Ok(forging_decision()),
            Ok(forging_decision()), // 重生成 li：仍违规
            Ok(forging_decision()), // 重生成 wang：仍违规
            // 后面不该再有任何调用（不写作、不审校）——脚本到此为止。
        ]);
        init_run(host.as_ref(), "run-1", true);
        let out = NarrativeEngine::new(host.clone())
            .run_round(
                &routes(),
                &prompts(),
                input_with_bottom_lines("run-1", &["不做伪证"], &["不做伪证"]),
                &CancelFlag::new(),
            )
            .await
            .unwrap();

        let reason = out.blocked.expect("全员违规必须整拍延后");
        assert!(reason.contains("底线"), "阻断原因须说明白：{reason}");
        // 零提交：状态一个字节都没动，场景没落盘。
        assert_eq!(out.new_state.revision, 0);
        assert!(out.scene.events.is_empty());
        assert!(out.scene.state_patch.operations.is_empty());
        assert!(out.scene.prose.is_empty());
        assert!(NarrativeStore::new(host.fs.clone()).list_scene_ids("run-1").unwrap().is_empty());
        // critic 报告为默认空（阻断路径不调用模型）。
        assert!(out.critic.character_consistency_issues.is_empty());
    }

    /// 误伤控制：底线写得极宽（"绝不伤害任何人"）也不会让世界卡死 ——
    /// 过宽条目不进规则层，普通行动照常落定，且不触发任何重生成。
    #[tokio::test]
    async fn overbroad_bottom_lines_never_stall_the_world() {
        let model = Arc::new(CapturingModel::new(two_char_script()));
        let host = host_capturing(model.clone());
        init_run(host.as_ref(), "run-1", true);

        let wide: &[&str] = &["绝不伤害任何人", "不做任何事", "不逃", "答应过的路一定走完"];
        let out = NarrativeEngine::new(host.clone())
            .run_round(
                &routes(),
                &prompts(),
                input_with_bottom_lines("run-1", wide, wide),
                &CancelFlag::new(),
            )
            .await
            .unwrap();

        assert!(out.blocked.is_none(), "过宽底线不得阻断世界");
        assert_eq!(out.new_state.revision, 1);
        assert_eq!(model.decide_prompts().len(), 2, "过宽底线不得触发重生成（否则拍拍烧钱）");
        assert_eq!(outcome_of_cid(&out, "li").result, ArbiterResult::Success);
    }

    /// 确定性：含重生成与拦截的回合，同一份脚本重跑两次，产物逐字节相同（replay 契约）。
    #[tokio::test]
    async fn bottom_line_round_is_replay_deterministic() {
        let once = || async {
            let (host, _ev) = host_with(vec![
                director(),
                Ok(forging_decision()),
                Ok(benign_decision()),
                Ok(forging_decision()),
                writer(),
                critic(),
            ]);
            init_run(host.as_ref(), "run-1", true);
            let out = NarrativeEngine::new(host.clone())
                .run_round(
                    &routes(),
                    &prompts(),
                    input_with_bottom_lines("run-1", &["不做伪证"], &[]),
                    &CancelFlag::new(),
                )
                .await
                .unwrap();
            (
                serde_json::to_string(&out.scene).unwrap(),
                serde_json::to_string(&out.new_state).unwrap(),
            )
        };
        let a = once().await;
        let b = once().await;
        assert_eq!(a.0, b.0, "scene 必须逐字节可复现");
        assert_eq!(a.1, b.1, "state 必须逐字节可复现");
    }

    /// critic 的既有产出口径不被破坏（它已落库为 SLO「状态-文本矛盾率」的数据源）：
    /// 拦截发生的回合里，critic 照常被调用一次、三个字段照常解析回填。
    #[tokio::test]
    async fn bottom_line_keeps_critic_contract_intact() {
        let (host, _ev) = host_with(vec![
            director(),
            Ok(forging_decision()),
            Ok(benign_decision()),
            Ok(forging_decision()),
            writer(),
            Ok(r#"{"characterConsistencyIssues":["李四行为偏离价值排序"],"causalIssues":["因果链缺一环"],"revisionSuggestions":["补一处动机铺垫"]}"#.to_string()),
        ]);
        init_run(host.as_ref(), "run-1", true);
        let out = NarrativeEngine::new(host.clone())
            .run_round(
                &routes(),
                &prompts(),
                input_with_bottom_lines("run-1", &["不做伪证"], &[]),
                &CancelFlag::new(),
            )
            .await
            .unwrap();

        assert_eq!(out.critic.character_consistency_issues, vec!["李四行为偏离价值排序".to_string()]);
        assert_eq!(out.critic.causal_issues, vec!["因果链缺一环".to_string()]);
        assert_eq!(out.critic.revision_suggestions, vec!["补一处动机铺垫".to_string()]);
    }
}
