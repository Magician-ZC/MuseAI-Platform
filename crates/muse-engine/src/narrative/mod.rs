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

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::character::types::CharacterCardV2;
use crate::host::{CancelFlag, EngineEvent, EngineHost};
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

pub struct NarrativeEngine {
    pub host: Arc<EngineHost>,
}

impl NarrativeEngine {
    pub fn new(host: Arc<EngineHost>) -> Self {
        Self { host }
    }

    /// 成本预估（§12.4：N+3~4 公式 + 历史 p50/p95 由调用方补充）。
    pub fn estimate(&self, active_count: u32, max_output_tokens: u32, scenes: u32) -> CostEstimate {
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

        // 预算硬停（§12.4）：本场景预估 = 导演1 + 决策N + 仲裁≤1 + 写作1 + 审校1 = N+4（最坏）。
        let calls = active_ids.len() as u64 + 4;
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

        // 1) 导演设局（本回合生成局势）。
        cancel.check()?;
        let situation = call_director(
            host,
            routes.for_stage("director"),
            &prompts.director_system,
            &prompts.prompt_version,
            &run_id,
            &current,
            &active_ids,
            cancel,
        )
        .await?;

        // 2) 活跃角色顺序（字典序）role_decide —— 顺序 await 满足确定性（活跃角色 ≤5）。
        let empty_frags: Vec<crate::knowledge::types::RetrievedFragment> = Vec::new();
        let mut decisions: Vec<RoleDecision> = Vec::with_capacity(active_ids.len());
        for cid in &active_ids {
            cancel.check()?;
            let card = &input.active_cards[cid];
            let frags = input.fragments.get(cid).unwrap_or(&empty_frags);
            let whisper = input.whispers.get(cid).map(|s| s.as_str());
            let ctx = decide::assemble_visible_context(
                &current,
                cid,
                card,
                &input.other_cards_brief,
                &situation,
                frags,
                whisper,
            )?;
            let d = decide::role_decide(
                host,
                routes.for_stage("decide"),
                &decide_prompts,
                input.temperature_decide,
                input.max_output_tokens,
                &run_id,
                cid,
                &ctx,
                &active_ids,
                cancel,
            )
            .await?;
            decisions.push(d);
        }

        // 3) 仲裁：规则层 → （仅在有 pending 时）模型层。
        let (mut outcomes, pending) = arbiter::rule_arbitrate(&current, &decisions, &active_ids);
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

        // 4) 场景写作。
        cancel.check()?;
        let prose = call_writer(
            host,
            routes.for_stage("writer"),
            &prompts.writer_system,
            &prompts.prompt_version,
            input.temperature_writer,
            input.max_output_tokens,
            &run_id,
            &situation,
            &decisions,
            &outcomes,
            cancel,
        )
        .await?;

        // 4.5) 不可逆结果同意门控（REMEDIATION #3 / 规格 §2.4）：
        // 分类不可逆结果（角色死亡/永久退场/永久关系变更，由 ArbiterResult 成功 + 行动语义判定）；
        // subject 全部命中 approved_consents → 正常落定并清除对应 pending；否则门控——
        // 产 ConsentRequested、剔出落定集（不落定该不可逆结果）、记 narrative.pending_consents。
        let (committing_outcomes, consent_requests, newly_pending, approved_landed) =
            gate_consents(&decisions, &outcomes, &input.approved_consents);

        // 5) reducer 生成 StatePatch + DomainEvent（事件引用 patch.id，供 I3 校验）。
        // 落定集已剔除被门控的不可逆结果 → 其后果不进入 StatePatch/ActionResolved。
        let patch = build_patch(current.revision, &decisions, &committing_outcomes, &current);
        let mut events = build_events(&run_id, &patch.id, &decisions, &committing_outcomes);
        // 门控的不可逆结果追加 ConsentRequested（可见性 Private→当事角色），续接事件序号。
        events.extend(build_consent_events(&run_id, &patch.id, events.len() as u64, &consent_requests));

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
    run_id: &str,
    state: &NarrativeState,
    active_ids: &[String],
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
    let user = format!(
        "当前活跃角色：{active}\n大纲节点：{outline}\n公共世界状态：{world}\n\n\
你是入场导演：为本回合设定一个把当前待推进节点自然展开的开放局势，给在场角色留出做出不同选择的空间，\
不要替角色决定他们会怎么做。严格输出 JSON：{{\"situation\":\"...\"}}",
        active = active_ids.join("、"),
        outline = serde_json::to_string(&outline).unwrap_or_default(),
        world = serde_json::to_string(&public_world(state)).unwrap_or_default(),
    );
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: system.to_string(),
        user,
        temperature: 0.7,
        max_output_tokens: 1024,
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

/// 由仲裁结果生成本回合 StatePatch（走 reducer 白名单路径）：每个非 Invalid 结果追加一条
/// pacingNotes（记录本回合节拍，可追溯）；有成功推进时把当前首个待推进节点标记 done
/// （硬节点完成率 100% 的落点）。source_decision_ids 填本回合全部决策 id（继承 E3 reducer 契约）。
fn build_patch(
    base_revision: u64,
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    state: &NarrativeState,
) -> StatePatch {
    let mut operations: Vec<PatchOperation> = Vec::new();
    let mut progressed = false;

    for o in outcomes {
        if o.result == ArbiterResult::Invalid {
            continue;
        }
        if matches!(o.result, ArbiterResult::Success | ArbiterResult::PartialSuccess) {
            progressed = true;
        }
        let note = format!("{}｜{:?}｜{}", o.character_id, o.result, o.consequence);
        operations.push(PatchOperation {
            op: PatchOp::Append,
            path: "narrative.pacingNotes".to_string(),
            value: Some(json!(note)),
            precondition: None,
        });
    }

    if progressed {
        if let Some(node) = constraints::next_pending(&state.narrative.outline_nodes) {
            operations.push(PatchOperation {
                op: PatchOp::Set,
                path: format!("narrative.outlineNodes[{}].status", node.id),
                value: Some(json!("done")),
                precondition: None,
            });
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
fn build_events(
    run_id: &str,
    patch_id: &str,
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
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
        let targets = d.map(|d| d.targets.clone()).unwrap_or_default();
        let action = d.map(|d| d.action.clone()).unwrap_or_default();

        events.push(DomainEvent {
            schema_version: 1,
            id: format!("{patch_id}-ev-{seq}"),
            run_id: run_id.to_string(),
            sequence: seq,
            event_type: DomainEventType::ActionResolved,
            actor_ids: vec![o.character_id.clone()],
            target_ids: if targets.is_empty() { None } else { Some(targets) },
            fact: json!({
                "result": format!("{:?}", o.result),
                "action": action,
                "consequence": o.consequence,
            }),
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

/// 门控编排：对每个仲裁结果分类不可逆性；当事角色全部命中 approved_consents → 留在落定集并记入
/// 待清除 pending；否则剔出落定集、生成 ConsentRequest、记入新增 pending。非不可逆结果原样落定。
/// 返回 (落定用 outcomes, 待生成 ConsentRequested, 新增 pending, 已落定待清除 pending)。
fn gate_consents(
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    approved_consents: &[String],
) -> (Vec<ArbiterOutcome>, Vec<ConsentRequest>, Vec<PendingConsent>, Vec<PendingConsent>) {
    let rules = IrreversibleRules::new();
    let approved: std::collections::BTreeSet<&str> =
        approved_consents.iter().map(|s| s.as_str()).collect();
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
                // 全部当事角色均获批 → 落定；否则门控不落定。
                let all_approved =
                    !subjects.is_empty() && subjects.iter().all(|s| approved.contains(s.as_str()));
                if all_approved {
                    committing.push(o.clone());
                    for s in &subjects {
                        approved_landed
                            .push(PendingConsent { subject: s.clone(), event_kind: event_kind.clone() });
                    }
                } else {
                    for s in &subjects {
                        newly_pending
                            .push(PendingConsent { subject: s.clone(), event_kind: event_kind.clone() });
                    }
                    requests.push(ConsentRequest {
                        actor: o.character_id.clone(),
                        decision_id: o.decision_id.clone(),
                        event_kind,
                        subjects,
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
        }
    }

    fn benign_decision() -> String {
        r#"{"intent":"观望","action":"上前拱手行礼","speak":{"willSpeak":true,"purpose":"寒暄"},"targets":[],"acceptableCosts":[],"predictions":[]}"#.to_string()
    }

    fn big_budget() -> RoundBudget {
        RoundBudget { max_total_tokens: 1_000_000, spent_tokens: 0, max_scenes: 10 }
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
        assert_eq!(out.scene.decisions[0].decision_id, "dec:run-1:li");
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
            Ok(r#"{"outcomes":[{"decisionId":"dec:run-1:li","result":"blocked","consequence":"该行动会使硬节点无法达成"}]}"#.to_string()), // arbiter
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
}
