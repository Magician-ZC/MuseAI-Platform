//! P2 自主叙事引擎：回合编排（规格 §12.1）。
//!
//! 回合：导演设局 → 活跃角色并发 role_decide → 仲裁（规则→模型）→ 场景写作
//! → 确定性不变量检查（失败阻断）→ narrative_critic（建议）→ reducer 生成校验 StatePatch
//! → 原子提交 → DomainEvent 发射 → 下一场景 / 章节停止点。
//!
//! 文件所有权：mod.rs 归 agent-E4；state/reducer/constraints/snapshot 归 agent-E3。

pub mod arbiter;
pub mod constraints;
pub mod continuity;
pub mod decide;
pub mod reducer;
pub mod snapshot;
pub mod state;
pub mod types;

use std::collections::BTreeMap;
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

pub struct RoundInput {
    pub run_id: String,
    pub mode: RunMode,
    /// 活跃角色（2–5，上限由调用方校验）及其 DNA 卡
    pub active_cards: BTreeMap<String, CharacterCardV2>,
    /// 其他角色的一句话第三人称摘要（防注入：不注原文）
    pub other_cards_brief: BTreeMap<String, String>,
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
}

impl Default for RoundInput {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            mode: RunMode::Observe,
            active_cards: BTreeMap::new(),
            other_cards_brief: BTreeMap::new(),
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
        }
    }
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
                        let dctx = &dc_ref[cid];
                        let ctx = decide::assemble_visible_context(
                            cur_ref, cid, card, &dctx.brief, &dctx.situation, frags, whisper,
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

        // 3) 逐组仲裁（Phase 2）：每组独立规则层（R2 同组在场 + R6 移动连通/准入），
        //    pending 汇总后全局一次 model_arbitrate（仲裁模型调用仍 ≤1）。
        let dmap_by_cid: BTreeMap<&str, &RoleDecision> =
            decisions.iter().map(|d| (d.character_id.as_str(), d)).collect();
        let mut outcomes: Vec<ArbiterOutcome> = Vec::new();
        let mut pending: Vec<RoleDecision> = Vec::new();
        for members in groups.values() {
            let group_decisions: Vec<RoleDecision> = members
                .iter()
                .filter_map(|m| dmap_by_cid.get(m.as_str()).map(|d| (*d).clone()))
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
        let (committing_outcomes, consent_requests, newly_pending, approved_landed) =
            gate_consents(&decisions, &outcomes, &input.approved_consents, &input.world_controlled);

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

        budget.spent_tokens = budget.spent_tokens.saturating_add(scene_cost).min(budget.max_total_tokens);
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
fn is_terminal(state: &NarrativeState) -> Option<Terminal> {
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
fn public_world(state: &NarrativeState) -> BTreeMap<String, Value> {
    state
        .world
        .iter()
        .filter(|(k, _)| k.as_str() != "appliedPatchIds")
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
    let user = format!(
        "{place}当前活跃角色：{active}\n大纲节点：{outline}\n公共世界状态：{world}\n\n\
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
/// pacingNotes（记录本回合节拍，可追溯）；有成功推进时把当前首个待推进节点标记 done。
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
/// 返回 (落定用 outcomes, 待生成 ConsentRequested, 新增 pending, 已落定待清除 pending)。
fn gate_consents(
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    approved_consents: &[String],
    world_controlled: &[String],
) -> (Vec<ArbiterOutcome>, Vec<ConsentRequest>, Vec<PendingConsent>, Vec<PendingConsent>) {
    let rules = IrreversibleRules::new();
    let approved: std::collections::BTreeSet<&str> =
        approved_consents.iter().map(|s| s.as_str()).collect();
    // 世界固有角色：无主人可授权，其作为 subject 的不可逆结果一律自动放行（NPC/反派死亡等）。
    let world: std::collections::BTreeSet<&str> =
        world_controlled.iter().map(|s| s.as_str()).collect();
    let dmap: BTreeMap<&str, &RoleDecision> =
        decisions.iter().map(|d| (d.decision_id.as_str(), d)).collect();

    let mut committing: Vec<ArbiterOutcome> = Vec::with_capacity(outcomes.len());
    let mut requests: Vec<ConsentRequest> = Vec::new();
    let mut newly_pending: Vec<PendingConsent> = Vec::new();
    let mut approved_landed: Vec<PendingConsent> = Vec::new();

    for o in outcomes {
        match dmap.get(o.decision_id.as_str()).and_then(|d| rules.classify(o, d)) {
            None => committing.push(o.clone()), // 非不可逆：正常落定
            Some((event_kind, subjects)) => {
                // 每个当事角色须「可放行」：world_controlled（自动放行）或已获批。全部可放行 → 落定。
                let landable = |s: &str| world.contains(s) || approved.contains(s);
                let all_landable =
                    !subjects.is_empty() && subjects.iter().all(|s| landable(s.as_str()));
                if all_landable {
                    committing.push(o.clone());
                    for s in &subjects {
                        // world_controlled subject 从不入 pending，无需记入待清除；仅玩家 subject 记 approved_landed。
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
    fn new() -> Self {
        Self {
            death: Regex::new(r"(杀死|杀掉|杀了|杀害|处死|赐死|斩杀|毒死|勒死|绞死|自尽|自刎|殉|同归于尽)").unwrap(),
            self_death: Regex::new(r"(自尽|自刎|殉|同归于尽)").unwrap(),
            exit: Regex::new(r"(流放|放逐|逐出|驱逐|永远离开|远走高飞|退隐|归隐|遁入空门|出走|永别)").unwrap(),
            relation: Regex::new(r"(背叛|叛变|叛逃|反目成仇|反目|决裂|绝交|断绝)").unwrap(),
        }
    }

    /// 仅「实际发生」（Success/PartialSuccess）的结果才产生不可逆后果。
    /// 返回 (eventKind, subjectCharacterIds)，非不可逆返回 None。死亡优先级最高。
    fn classify(&self, o: &ArbiterOutcome, d: &RoleDecision) -> Option<(String, Vec<String>)> {
        if !matches!(o.result, ArbiterResult::Success | ArbiterResult::PartialSuccess) {
            return None;
        }
        let action = d.action.as_str();
        let actor = d.character_id.as_str();

        if self.death.is_match(action) {
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
}
