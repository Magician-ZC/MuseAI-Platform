//! 行动仲裁（规格 §12.3）：规则层优先（无模型），规则不能裁决的交模型（0–1 次调用）。
//! 文件所有权：agent-E4。
//!
//! 边界：不改写角色意图原文；输出含规则依据；硬节点与角色底线冲突时可调整事件实现或
//! 返回 Blocked，不能悄悄替角色改主意。状态变化统一交 reducer（本模块不产生 StatePatch）。

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{
    ArbiterOutcome, ArbiterResult, ConstraintLevel, LocationDef, LocationGate, NarrativeState,
    NodeStatus, RoleDecision,
};

/// 移动伪目标前缀（Phase 2）：decision.targets 中 `loc:<id>` 表示「移动到地点 id」的意图。
/// 与角色目标区分——R2 在场校验跳过它，R6 据它判定连通/准入。
pub const LOC_TARGET_PREFIX: &str = "loc:";

/// 解析移动目标：targets 中首个 `loc:<id>` 的 `<id>`（无则非移动决策）。
fn move_dest(d: &RoleDecision) -> Option<String> {
    d.targets.iter().find_map(|t| t.strip_prefix(LOC_TARGET_PREFIX).map(|s| s.to_string()))
}

fn is_loc_target(t: &str) -> bool {
    t.starts_with(LOC_TARGET_PREFIX)
}

/// R6b 秘境准入（引擎侧纯函数）：required_item_ids ⊆ held ∧ required_effect_tags ⊆ held。
/// held = 角色 resources（约定道具以 `item:<id>`、标签以 `tag:<t>` 或裸值承载）。
/// 体系(cosmology)/强度(power_tier)闸门需 per-item 元数据，引擎无此上下文——留待 server 侧
/// check_location_admission 在物化 held cosmology/tier 后强化（Phase 3）；此处只判引擎可确定性验证的持有闸。
fn gate_admits(gate: &LocationGate, held: &[String]) -> bool {
    let holds = |needle: &str| {
        held.iter().any(|h| {
            let h = h.as_str();
            h == needle
                || h.strip_prefix("item:").map(|x| x == needle).unwrap_or(false)
                || h.strip_prefix("tag:").map(|x| x == needle).unwrap_or(false)
        })
    };
    gate.required_item_ids.iter().all(|id| holds(id))
        && gate.required_effect_tags.iter().all(|t| holds(t))
}

pub struct ArbiterPrompts {
    pub system: String,
    pub prompt_version: String,
}

/// 仲裁模型层默认输出上限（结构化裁决，不需要长文本）。
const ARBITER_MAX_TOKENS: u32 = 1500;

fn outcome(d: &RoleDecision, result: ArbiterResult, rule: &str, consequence: &str) -> ArbiterOutcome {
    ArbiterOutcome {
        decision_id: d.decision_id.clone(),
        character_id: d.character_id.clone(),
        result,
        rule_refs: vec![rule.to_string()],
        consequence: consequence.to_string(),
    }
}

/// R1 资源约束：捕捉「动用/消耗/花费…X」类明确的资源消耗声明，若 X 与角色 resources 均不匹配 → 违规。
/// 保守：仅匹配明确的耗用动词；无匹配则不判违规（交后续规则/模型）。
fn violates_resource(state: &NarrativeState, d: &RoleDecision, res_re: &Regex) -> bool {
    let owned: &[String] = state
        .characters
        .get(&d.character_id)
        .map(|c| c.resources.as_slice())
        .unwrap_or(&[]);
    for cap in res_re.captures_iter(&d.action) {
        let object = cap.get(2).map(|m| m.as_str()).unwrap_or("").trim();
        if object.is_empty() {
            continue;
        }
        let matched = owned.iter().any(|r| {
            let r = r.trim();
            !r.is_empty() && (r.contains(object) || object.contains(r))
        });
        if !matched {
            return true; // 声明动用了一项并不持有的资源。
        }
    }
    false
}

/// R3 读心/强制他人：直接获取他人内心/秘密，或强迫他人吐露私密。保守匹配明确句式。
fn violates_mind_control(d: &RoleDecision, coerce_re: &Regex, read_re: &Regex) -> bool {
    coerce_re.is_match(&d.action) || read_re.is_match(&d.action)
}

/// 规则层（纯函数）：
/// R1 资源约束：action 引用了 resources 中不存在的资源 → Invalid("rule:resource")
/// R2 目标在场：targets 必须都在活跃角色集合 → 越界 Invalid("rule:target")
/// R3 读心/强制他人：action 含对他人内心/秘密的直接获取或强制他人行动 → Invalid("rule:mind_control")
///    （启发式：正则匹配「让/命令/迫使 X 说出/交出 + 秘密/心里」类模式；保守宁可漏判交模型层）
/// R4 同目标冲突：多个决策争夺同一独占目标 → 全部标记 needs_model
/// R5 硬节点保护：action 明显使当前 Pending 硬节点不可能发生 → needs_model（模型层裁决实现调整或 Blocked）
/// 返回：已裁决结果 + 需模型层的决策列表。
///
/// 设计：R1/R2/R3 命中即 Invalid（进 resolved）；干净且无冲突/无硬节点威胁的决策由规则层直接判 Success；
/// 只有 R4（冲突）或 R5（硬节点威胁）的决策进入 pending（交模型层），保证仲裁调用 0–1 次。
///
/// Phase 2 分组语义：调用方逐地点组调用——`active_character_ids` 为**同组在场集**（R2 据此判「目标不在场」，
/// 跨地点角色目标即越界）；`locations` 为本回合地点图（R6 移动合法性据此判连通/准入）。
/// `locations` 空 = 无地点维度，退化为「无移动决策」——R6 不触发，行为与 Phase 1 一致。
pub fn rule_arbitrate(
    state: &NarrativeState,
    decisions: &[RoleDecision],
    active_character_ids: &[String],
    locations: &BTreeMap<String, LocationDef>,
) -> (Vec<ArbiterOutcome>, Vec<RoleDecision>) {
    let res_re = Regex::new(r"(动用|消耗|花费|拿出|掏出|支付)([^\s，。、；：！？…,.!?（）()]{1,8})").unwrap();
    let coerce_re = Regex::new(
        r"(让|命令|迫使|逼迫|逼|强迫|胁迫).{0,12}(说出|交出|供出|坦白|招供|吐露).{0,8}(秘密|真相|心里|心事|底细|隐私|下落)",
    )
    .unwrap();
    let read_re =
        Regex::new(r"(读取|窥探|看穿|洞悉|读心|偷看).{0,8}(内心|心里|想法|秘密|心思)").unwrap();

    let active: BTreeSet<&str> = active_character_ids.iter().map(|s| s.as_str()).collect();

    // R4 预计算：出现在 ≥2 个决策 targets 中的目标视为被争夺（移动伪目标 loc:<id> 不计——
    // 多人同赴一地不是资源争夺）。
    let mut target_count: BTreeMap<&str, usize> = BTreeMap::new();
    for d in decisions {
        for t in &d.targets {
            if is_loc_target(t) {
                continue;
            }
            *target_count.entry(t.as_str()).or_default() += 1;
        }
    }
    let conflict_targets: BTreeSet<&str> =
        target_count.iter().filter(|(_, c)| **c >= 2).map(|(t, _)| *t).collect();

    // R5 预计算：是否存在待推进的硬节点。
    let has_pending_hard = state
        .narrative
        .outline_nodes
        .iter()
        .any(|n| n.constraint == ConstraintLevel::Hard && n.status == NodeStatus::Pending);
    let irreversible_re = Regex::new(
        r"(杀死|杀掉|杀了|处死|毒死|毁掉|摧毁|炸毁|烧毁|销毁|终止|放弃|背叛|叛变|叛逃|自尽|同归于尽)",
    )
    .unwrap();

    // 定序：按 character_id、decision_id 排序，保证确定性输出（§12.5.3）。
    let mut ordered: Vec<&RoleDecision> = decisions.iter().collect();
    ordered.sort_by(|a, b| a.character_id.cmp(&b.character_id).then(a.decision_id.cmp(&b.decision_id)));

    let mut resolved: Vec<ArbiterOutcome> = Vec::new();
    let mut pending: Vec<RoleDecision> = Vec::new();

    for d in ordered {
        // R1
        if violates_resource(state, d, &res_re) {
            resolved.push(outcome(d, ArbiterResult::Invalid, "rule:resource", "行动动用了未持有的资源，无法执行"));
            continue;
        }
        // R2 目标在场：仅校验角色目标；移动伪目标 loc:<id> 交 R6，跨地点角色目标（不在同组在场集）判越界。
        if d.targets.iter().filter(|t| !is_loc_target(t)).any(|t| !active.contains(t.as_str())) {
            resolved.push(outcome(d, ArbiterResult::Invalid, "rule:target", "行动目标不在场，无法执行"));
            continue;
        }
        // R3
        if violates_mind_control(d, &coerce_re, &read_re) {
            resolved.push(outcome(
                d,
                ArbiterResult::Invalid,
                "rule:mind_control",
                "不能直接读取或强取他人私密（信息边界）",
            ));
            continue;
        }

        // R6 移动合法性（Phase 2）：移动是终态裁决（不落入 R4/R5）。
        // R6a 连通：目标 ∈ 当前地点 connections；R6b 准入：目标 gate（秘境）须放行。
        if let Some(dest) = move_dest(d) {
            let cur_loc =
                state.characters.get(&d.character_id).map(|c| c.location.as_str()).unwrap_or("");
            let connected = locations
                .get(cur_loc)
                .map(|l| l.connections.iter().any(|c| c == &dest))
                .unwrap_or(false);
            if !connected {
                resolved.push(outcome(
                    d,
                    ArbiterResult::Invalid,
                    "rule:move_unreachable",
                    "无法从当前位置抵达该地点",
                ));
                continue;
            }
            if let Some(gate) = locations.get(&dest).and_then(|l| l.gate.as_ref()) {
                let held = state
                    .characters
                    .get(&d.character_id)
                    .map(|c| c.resources.as_slice())
                    .unwrap_or(&[]);
                if !gate_admits(gate, held) {
                    resolved.push(outcome(
                        d,
                        ArbiterResult::Invalid,
                        "rule:move_admission",
                        "未满足秘境准入条件",
                    ));
                    continue;
                }
            }
            resolved.push(outcome(d, ArbiterResult::Success, "rule:move", &format!("移动到「{dest}」")));
            continue;
        }

        // R4 / R5：需模型层裁决结果与意外后果。
        let conflict = d.targets.iter().any(|t| conflict_targets.contains(t.as_str()));
        let threatens_hard = has_pending_hard && irreversible_re.is_match(&d.action);
        if conflict || threatens_hard {
            pending.push(d.clone());
        } else {
            // 干净决策：规则层直接判可行，避免不必要的模型调用。
            resolved.push(outcome(d, ArbiterResult::Success, "rule:clear", "行动可行，照常推进"));
        }
    }

    (resolved, pending)
}

/// 模型层输出（宽松解析：result 缺省视为 success）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOutcome {
    #[serde(default)]
    decision_id: String,
    #[serde(default)]
    result: Option<ArbiterResult>,
    #[serde(default)]
    consequence: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArbiterBatch {
    #[serde(default)]
    outcomes: Vec<RawOutcome>,
}

fn build_arbiter_user_prompt(state: &NarrativeState, situation: &str, pending: &[RoleDecision]) -> String {
    let hard_nodes: Vec<Value> = state
        .narrative
        .outline_nodes
        .iter()
        .filter(|n| n.constraint == ConstraintLevel::Hard && n.status == NodeStatus::Pending)
        .map(|n| json!({ "id": n.id, "summary": n.summary }))
        .collect();
    let items: Vec<Value> = pending
        .iter()
        .map(|d| {
            json!({
                "decisionId": d.decision_id,
                "characterId": d.character_id,
                "intent": d.intent,
                "action": d.action,
                "targets": d.targets,
            })
        })
        .collect();
    format!(
        "局势：{situation}\n待推进硬节点：{hard}\n待裁决行动（互相冲突或可能危及硬节点）：{items}\n\n\
你是行动仲裁器：只裁决可行性、冲突结果与意外后果，绝不改写任何角色的 intent 原文。\
result 取值：success/partialSuccess/failure/invalid/blocked。\
若某行动与硬节点或角色底线冲突且无法调整实现，则该项 result=blocked 并在 consequence 说明冲突。\
每个 decisionId 必须来自上面给定集合，一一给出裁决。严格输出 JSON：\n\
{{\"outcomes\":[{{\"decisionId\":\"...\",\"result\":\"success\",\"consequence\":\"简述结果与后果\"}}]}}",
        hard = serde_json::to_string(&hard_nodes).unwrap_or_default(),
        items = serde_json::to_string(&items).unwrap_or_default(),
    )
}

/// 模型层：一次调用裁决剩余决策的结果与意外后果；输出 decision_id 必须 ⊆ 输入集合（引用完整性）。
#[allow(clippy::too_many_arguments)] // 签名由骨架固定
pub async fn model_arbitrate(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &ArbiterPrompts,
    run_id: &str,
    state: &NarrativeState,
    situation: &str,
    pending: &[RoleDecision],
    cancel: &CancelFlag,
) -> Result<Vec<ArbiterOutcome>, EngineError> {
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    let spec = ModelCallSpec {
        max_retries: None,
        profile: profile.clone(),
        system: prompts.system.clone(),
        user: build_arbiter_user_prompt(state, situation, pending),
        temperature: 0.0, // 裁决类：确定性
        max_output_tokens: ARBITER_MAX_TOKENS,
        agent: "arbiter".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };

    let batch: ArbiterBatch =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;

    // 引用完整性：只接受 decision_id ∈ pending 的裁决；其余丢弃。
    let pending_ids: BTreeSet<&str> = pending.iter().map(|d| d.decision_id.as_str()).collect();
    let mut by_id: BTreeMap<String, (ArbiterResult, String)> = BTreeMap::new();
    for o in batch.outcomes {
        if pending_ids.contains(o.decision_id.as_str()) {
            by_id.insert(o.decision_id.clone(), (o.result.unwrap_or(ArbiterResult::Success), o.consequence));
        }
    }

    // 覆盖每个 pending 决策（模型漏判则回退 Success）；character_id 以本地决策为准，防篡改。
    let mut out: Vec<ArbiterOutcome> = Vec::with_capacity(pending.len());
    for d in pending {
        let (result, consequence) =
            by_id.get(&d.decision_id).cloned().unwrap_or((ArbiterResult::Success, String::new()));
        out.push(ArbiterOutcome {
            decision_id: d.decision_id.clone(),
            character_id: d.character_id.clone(),
            result,
            rule_refs: vec!["model:arbiter".to_string()],
            consequence,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use crate::narrative::types::{
        CharacterState, OutlineNode, SpeakIntent,
    };
    use std::sync::Arc;

    fn decision(id: &str, cid: &str, action: &str, targets: Vec<&str>) -> RoleDecision {
        RoleDecision {
            decision_id: id.to_string(),
            character_id: cid.to_string(),
            intent: "意图".into(),
            action: action.to_string(),
            speak: SpeakIntent { will_speak: false, purpose: String::new() },
            targets: targets.into_iter().map(String::from).collect(),
            acceptable_costs: vec![],
            predictions: vec![],
            duration: 0,
        }
    }

    fn base_state() -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        s.characters.insert("li".into(), CharacterState::default());
        s.characters.insert("wang".into(), CharacterState::default());
        s
    }

    fn active() -> Vec<String> {
        vec!["li".to_string(), "wang".to_string()]
    }

    /// 无地点维度（退化路径）：locations 空 → R6 不触发，行为与 Phase 1 一致。
    fn no_locations() -> BTreeMap<String, LocationDef> {
        BTreeMap::new()
    }

    fn move_decision(id: &str, cid: &str, dest: &str) -> RoleDecision {
        decision(id, cid, &format!("前往{dest}"), vec![&format!("loc:{dest}")])
    }

    // ===== R1 资源约束 =====

    #[test]
    fn r1_rejects_unowned_resource() {
        let s = base_state(); // li 无任何 resources
        let d = decision("d1", "li", "动用禁军包围皇宫", vec![]);
        let (resolved, pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert!(pending.is_empty());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:resource".to_string()]);
    }

    #[test]
    fn r1_allows_owned_resource() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().resources.push("禁军".into());
        let d = decision("d1", "li", "动用禁军包围皇宫", vec![]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        // 持有该资源 → 不因 R1 违规；干净决策判 Success。
        assert_eq!(resolved[0].result, ArbiterResult::Success);
    }

    // ===== R2 目标在场 =====

    #[test]
    fn r2_rejects_offscene_target() {
        let s = base_state();
        let d = decision("d1", "li", "攻击对方", vec!["ghost"]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:target".to_string()]);
    }

    // ===== R3 读心 / 强制他人 =====

    #[test]
    fn r3_rejects_coercing_secret() {
        let s = base_state();
        let d = decision("d1", "li", "命令王五说出他的秘密", vec![]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:mind_control".to_string()]);
    }

    #[test]
    fn r3_rejects_mind_reading() {
        let s = base_state();
        let d = decision("d1", "li", "窥探对方的内心想法", vec![]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:mind_control".to_string()]);
    }

    // ===== R4 同目标冲突 =====

    #[test]
    fn r4_conflicting_target_goes_to_model() {
        let s = base_state();
        let d1 = decision("d1", "li", "抢夺王座", vec!["throne_holder"]);
        let d2 = decision("d2", "wang", "抢夺王座", vec!["throne_holder"]);
        // 目标须在场，加入 active 集合。
        let act = vec!["li".to_string(), "wang".to_string(), "throne_holder".to_string()];
        let (resolved, pending) = rule_arbitrate(&s, &[d1, d2], &act, &no_locations());
        assert!(resolved.is_empty(), "冲突决策不应被规则层直接判定");
        assert_eq!(pending.len(), 2);
    }

    // ===== R5 硬节点保护 =====

    #[test]
    fn r5_irreversible_near_hard_node_goes_to_model() {
        let mut s = base_state();
        s.narrative.outline_nodes.push(OutlineNode {
            id: "n1".into(),
            summary: "主角与对手决战".into(),
            constraint: ConstraintLevel::Hard,
            status: NodeStatus::Pending,
            threshold: None,
            advance_when: None,
            weights: None,
        });
        let d = decision("d1", "li", "杀死关键人物王五", vec![]);
        let (resolved, pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert!(resolved.is_empty());
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn clean_decision_auto_success_without_model() {
        let s = base_state();
        let d = decision("d1", "li", "礼貌地上前问候", vec![]);
        let (resolved, pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert!(pending.is_empty(), "干净决策不需要模型层");
        assert_eq!(resolved[0].result, ArbiterResult::Success);
    }

    #[test]
    fn deterministic_ordering_of_resolved() {
        let s = base_state();
        // 乱序输入，输出应按 character_id 定序。
        let d2 = decision("d2", "wang", "问候", vec![]);
        let d1 = decision("d1", "li", "问候", vec![]);
        let (resolved, _p) = rule_arbitrate(&s, &[d2, d1], &active(), &no_locations());
        assert_eq!(resolved[0].character_id, "li");
        assert_eq!(resolved[1].character_id, "wang");
    }

    // ===== R6 移动合法性（Phase 2）：连通 + 秘境准入 =====

    fn locmap(defs: Vec<LocationDef>) -> BTreeMap<String, LocationDef> {
        defs.into_iter().map(|d| (d.id.clone(), d)).collect()
    }

    fn loc(id: &str, connections: &[&str]) -> LocationDef {
        LocationDef {
            id: id.into(),
            name: id.into(),
            connections: connections.iter().map(|s| s.to_string()).collect(),
            is_secret_realm: false,
            gate: None,
        }
    }

    #[test]
    fn r6_move_to_connected_location_succeeds() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().location = "前厅".into();
        let locs = locmap(vec![loc("前厅", &["密室"]), loc("密室", &["前厅"])]);
        let d = move_decision("d1", "li", "密室");
        let (resolved, pending) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert!(pending.is_empty(), "移动是终态裁决，不进模型层");
        assert_eq!(resolved[0].result, ArbiterResult::Success);
        assert_eq!(resolved[0].rule_refs, vec!["rule:move".to_string()]);
    }

    #[test]
    fn r6_move_to_unconnected_location_is_invalid() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().location = "前厅".into();
        // 前厅只连密室，不连塔顶。
        let locs = locmap(vec![loc("前厅", &["密室"]), loc("塔顶", &[])]);
        let d = move_decision("d1", "li", "塔顶");
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:move_unreachable".to_string()]);
    }

    #[test]
    fn r6_secret_realm_admission_denied_without_item() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().location = "前厅".into();
        // li 无 resources；秘境 gate 需玉钥。
        let mut secret = loc("秘境", &[]);
        secret.is_secret_realm = true;
        secret.gate =
            Some(LocationGate { required_item_ids: vec!["玉钥".into()], ..Default::default() });
        let locs = locmap(vec![loc("前厅", &["秘境"]), secret]);
        let d = move_decision("d1", "li", "秘境");
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:move_admission".to_string()]);
    }

    #[test]
    fn r6_secret_realm_admission_passes_with_item() {
        let mut s = base_state();
        {
            let li = s.characters.get_mut("li").unwrap();
            li.location = "前厅".into();
            li.resources.push("item:玉钥".into()); // 持有玉钥
        }
        let mut secret = loc("秘境", &[]);
        secret.is_secret_realm = true;
        secret.gate =
            Some(LocationGate { required_item_ids: vec!["玉钥".into()], ..Default::default() });
        let locs = locmap(vec![loc("前厅", &["秘境"]), secret]);
        let d = move_decision("d1", "li", "秘境");
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert_eq!(resolved[0].result, ArbiterResult::Success, "持有准入道具 → 放行");
    }

    #[test]
    fn r6_cross_group_character_target_is_invalid() {
        // R2 同组在场重定义：active = 同组 {li}，li 攻击不在同组的 wang（跨地点目标）→ 越界 Invalid。
        let s = base_state();
        let d = decision("d1", "li", "攻击王五", vec!["wang"]);
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:target".to_string()]);
    }

    // ===== 模型层 =====

    fn test_host(responses: Vec<Result<String, EngineError>>) -> (Arc<EngineHost>, Arc<CollectEvents>) {
        let events = Arc::new(CollectEvents::default());
        let host = Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1000)),
            events: events.clone(),
            model: Arc::new(ScriptedModel::new(responses)),
        });
        (host, events)
    }

    fn dummy_profile() -> ModelProfile {
        ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "http://x".into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    fn prompts() -> ArbiterPrompts {
        ArbiterPrompts { system: "你是仲裁器".into(), prompt_version: "v1".into() }
    }

    #[tokio::test]
    async fn model_arbitrate_no_call_when_empty() {
        let (host, ev) = test_host(vec![]);
        let s = base_state();
        let out = model_arbitrate(
            host.as_ref(),
            &dummy_profile(),
            &prompts(),
            "run-1",
            &s,
            "局势",
            &[],
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert!(out.is_empty());
        // 无 pending 时不发任何模型调用。
        assert_eq!(ev.0.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn model_arbitrate_covers_all_and_enforces_integrity() {
        // 模型返回一个越界 decisionId（不在 pending）+ 漏掉 d2。
        let resp = r#"{"outcomes":[
            {"decisionId":"d1","result":"failure","consequence":"被拦下"},
            {"decisionId":"ghost","result":"success","consequence":"不该出现"}
        ]}"#;
        let (host, _ev) = test_host(vec![Ok(resp.to_string())]);
        let s = base_state();
        let pending = vec![decision("d1", "li", "a", vec![]), decision("d2", "wang", "b", vec![])];
        let out = model_arbitrate(
            host.as_ref(),
            &dummy_profile(),
            &prompts(),
            "run-1",
            &s,
            "局势",
            &pending,
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        // 每个 pending 决策都被覆盖；ghost 被丢弃。
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].decision_id, "d1");
        assert_eq!(out[0].result, ArbiterResult::Failure);
        assert_eq!(out[1].decision_id, "d2");
        assert_eq!(out[1].result, ArbiterResult::Success); // 漏判回退
        assert!(out.iter().all(|o| o.decision_id != "ghost"));
    }

    #[tokio::test]
    async fn model_arbitrate_propagates_blocked() {
        let resp = r#"{"outcomes":[{"decisionId":"d1","result":"blocked","consequence":"与硬节点冲突"}]}"#;
        let (host, _ev) = test_host(vec![Ok(resp.to_string())]);
        let s = base_state();
        let pending = vec![decision("d1", "li", "a", vec![])];
        let out = model_arbitrate(
            host.as_ref(),
            &dummy_profile(),
            &prompts(),
            "run-1",
            &s,
            "局势",
            &pending,
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(out[0].result, ArbiterResult::Blocked);
    }
}
