//! 一致性检查（规格 §12.1）：确定性不变量（阻断）+ 叙事 critic（建议，不改状态）。
//! 文件所有权：agent-E4。

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{DomainEvent, NarrativeState, PatchOp, RoleDecision, StatePatch};

/// critic 模型层默认输出上限。
const CRITIC_MAX_TOKENS: u32 = 1500;

/// 确定性不变量（失败即整回合阻断，§2.4：100% 通过是发布阻断门）：
/// I1 未授权私密字段不得进入其他角色可见产物（prose 中不得出现未揭露 secrets 原文）
/// I2 StatePatch 只能由本回合 outcomes 派生（source_decision_ids ⊆ 本回合 decision id 集）
/// I3 事件引用完整性：DomainEvent.state_patch_id == patch.id；actor/target ⊆ 在场角色
/// I4 锁定场景保护：patch 不得修改 authoring.lockedSceneIds 已锁内容相关状态
/// 返回违规清单（空 = 通过）。纯函数，必测每条。
pub fn deterministic_invariants(
    state: &NarrativeState,
    decisions: &[RoleDecision],
    patch: &StatePatch,
    events: &[DomainEvent],
    prose: &str,
    active_character_ids: &[String],
) -> Vec<String> {
    let mut violations: Vec<String> = Vec::new();
    let active: BTreeSet<&str> = active_character_ids.iter().map(|s| s.as_str()).collect();

    // I1：正文不得逐字包含任一角色的私密内容（secrets）。
    // 私密的合理揭露应经由叙事改写，而非把私密字段原文抄进正文。
    for (cid, cs) in &state.characters {
        for secret in &cs.secrets {
            let s = secret.trim();
            if !s.is_empty() && prose.contains(s) {
                violations.push(format!("I1: 正文逐字泄露角色 {cid} 的私密内容「{s}」"));
            }
        }
    }

    // I2：patch.source_decision_ids ⊆ 本回合决策 id 集合。
    let round_ids: BTreeSet<&str> = decisions.iter().map(|d| d.decision_id.as_str()).collect();
    for sid in &patch.source_decision_ids {
        if !round_ids.contains(sid.as_str()) {
            violations.push(format!("I2: StatePatch 引用了非本回合决策 {sid}"));
        }
    }

    // I3：事件引用完整性。actor/target 的「在场」按**事件所属地点的同组集**判定（Phase 2）：
    // 在场集 = active 中与事件主 actor 同 location 的角色（从 state 派生位置）。
    // 退化：locations 空 → 全体 location 皆 "" → 同组集 = active 全集，与 Phase 1 语义等价。
    let loc_of = |cid: &str| -> &str {
        state.characters.get(cid).map(|c| c.location.as_str()).unwrap_or("")
    };
    for ev in events {
        if ev.state_patch_id != patch.id {
            violations.push(format!(
                "I3: 事件 {} 的 statePatchId({}) 与本回合补丁({}) 不符",
                ev.id, ev.state_patch_id, patch.id
            ));
        }
        // 事件地点 = 主 actor 的 location（无 actor 时退化为 ""）。
        let ev_loc = ev.actor_ids.first().map(|a| loc_of(a)).unwrap_or("");
        let present: BTreeSet<&str> =
            active.iter().copied().filter(|c| loc_of(c) == ev_loc).collect();
        for a in &ev.actor_ids {
            if !present.contains(a.as_str()) {
                violations.push(format!("I3: 事件 {} 的 actor {a} 不在场", ev.id));
            }
        }
        if let Some(targets) = &ev.target_ids {
            for t in targets {
                if !present.contains(t.as_str()) {
                    violations.push(format!("I3: 事件 {} 的 target {t} 不在场", ev.id));
                }
            }
        }
    }

    // I4：锁定场景保护 —— patch 不得移除或整表替换 lockedSceneIds（追加新锁允许）。
    for op in &patch.operations {
        if op.path == "authoring.lockedSceneIds" && matches!(op.op, PatchOp::Remove | PatchOp::Set) {
            violations.push("I4: 补丁试图解锁或改写已锁定场景列表".to_string());
        }
    }

    violations
}

pub struct CriticPrompts {
    pub system: String,
    pub prompt_version: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticReport {
    #[serde(default)]
    pub character_consistency_issues: Vec<String>,
    #[serde(default)]
    pub causal_issues: Vec<String>,
    #[serde(default)]
    pub revision_suggestions: Vec<String>,
}

fn build_critic_user_prompt(prose: &str, decisions: &[RoleDecision]) -> String {
    let intents: Vec<Value> = decisions
        .iter()
        .map(|d| json!({ "characterId": d.character_id, "intent": d.intent, "action": d.action }))
        .collect();
    format!(
        "本场景正文：\n{prose}\n\n各角色本回合的意图与行动：{intents}\n\n\
你是叙事一致性审校：核查人物是否行为一致（有无沦为通用 AI 人格）、因果链是否成立。\
只给出建议，不改写状态。严格输出 JSON：\n\
{{\"characterConsistencyIssues\":[\"...\"],\"causalIssues\":[\"...\"],\"revisionSuggestions\":[\"...\"]}}",
        intents = serde_json::to_string(&intents).unwrap_or_default(),
    )
}

/// 叙事 critic（1 次调用）：人物一致性与因果质量；只产建议，不直接改状态。
pub async fn narrative_critic(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &CriticPrompts,
    run_id: &str,
    prose: &str,
    decisions: &[RoleDecision],
    cancel: &CancelFlag,
) -> Result<CriticReport, EngineError> {
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.system.clone(),
        user: build_critic_user_prompt(prose, decisions),
        temperature: 0.0,
        max_output_tokens: CRITIC_MAX_TOKENS,
        agent: "critic".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };
    let report: CriticReport =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use crate::narrative::types::{
        CharacterState, DomainEventType, EventVisibility, PatchOperation, SpeakIntent,
    };
    use std::sync::Arc;

    fn state_with_secret() -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        let mut a = CharacterState::default();
        a.secrets.push("我下毒害死了国王".into());
        s.characters.insert("A".into(), a);
        s.characters.insert("B".into(), CharacterState::default());
        s
    }

    fn decision(id: &str, cid: &str) -> RoleDecision {
        RoleDecision {
            decision_id: id.to_string(),
            character_id: cid.to_string(),
            intent: "i".into(),
            action: "a".into(),
            speak: SpeakIntent { will_speak: false, purpose: String::new() },
            targets: vec![],
            acceptable_costs: vec![],
            predictions: vec![],
            duration: 0,
        }
    }

    fn patch(id: &str, sources: Vec<&str>, ops: Vec<PatchOperation>) -> StatePatch {
        StatePatch {
            id: id.to_string(),
            base_revision: 0,
            source_decision_ids: sources.into_iter().map(String::from).collect(),
            operations: ops,
        }
    }

    fn event(id: &str, patch_id: &str, actors: Vec<&str>, targets: Option<Vec<&str>>) -> DomainEvent {
        DomainEvent {
            schema_version: 1,
            id: id.to_string(),
            run_id: "r".into(),
            sequence: 0,
            timestamp: 0,
            event_type: DomainEventType::ActionResolved,
            actor_ids: actors.into_iter().map(String::from).collect(),
            target_ids: targets.map(|t| t.into_iter().map(String::from).collect()),
            fact: serde_json::json!({}),
            state_patch_id: patch_id.to_string(),
            caused_by: vec![],
            visibility: EventVisibility::Public,
        }
    }

    fn active() -> Vec<String> {
        vec!["A".to_string(), "B".to_string()]
    }

    // ===== I1 私密信息不入正文 =====

    #[test]
    fn i1_flags_secret_in_prose() {
        let s = state_with_secret();
        let p = patch("p1", vec![], vec![]);
        let prose = "众人震惊：原来我下毒害死了国王！";
        let v = deterministic_invariants(&s, &[], &p, &[], prose, &active());
        assert!(v.iter().any(|x| x.starts_with("I1")), "应检出 I1：{v:?}");
    }

    #[test]
    fn i1_passes_clean_prose() {
        let s = state_with_secret();
        let p = patch("p1", vec![], vec![]);
        let prose = "大厅里气氛凝重，众人各怀心事。";
        let v = deterministic_invariants(&s, &[], &p, &[], prose, &active());
        assert!(v.is_empty(), "干净正文不应有违规：{v:?}");
    }

    // ===== I2 patch 只能派生自本回合决策 =====

    #[test]
    fn i2_flags_foreign_source_decision() {
        let s = state_with_secret();
        let d = decision("dec:r:A", "A");
        let p = patch("p1", vec!["dec:r:A", "dec:other:X"], vec![]); // 后者非本回合
        let v = deterministic_invariants(&s, &[d], &p, &[], "clean", &active());
        assert!(v.iter().any(|x| x.starts_with("I2")), "应检出 I2：{v:?}");
    }

    #[test]
    fn i2_passes_subset() {
        let s = state_with_secret();
        let d = decision("dec:r:A", "A");
        let p = patch("p1", vec!["dec:r:A"], vec![]);
        let v = deterministic_invariants(&s, &[d], &p, &[], "clean", &active());
        assert!(v.is_empty());
    }

    // ===== I3 事件引用完整性 =====

    #[test]
    fn i3_flags_mismatched_patch_id() {
        let s = state_with_secret();
        let p = patch("p1", vec![], vec![]);
        let ev = event("e1", "WRONG", vec!["A"], None);
        let v = deterministic_invariants(&s, &[], &p, &[ev], "clean", &active());
        assert!(v.iter().any(|x| x.starts_with("I3")));
    }

    #[test]
    fn i3_flags_offscene_actor_and_target() {
        let s = state_with_secret();
        let p = patch("p1", vec![], vec![]);
        let ev = event("e1", "p1", vec!["ghost"], Some(vec!["phantom"]));
        let v = deterministic_invariants(&s, &[], &p, &[ev], "clean", &active());
        assert_eq!(v.iter().filter(|x| x.starts_with("I3")).count(), 2);
    }

    #[test]
    fn i3_passes_valid_event() {
        let s = state_with_secret();
        let p = patch("p1", vec![], vec![]);
        let ev = event("e1", "p1", vec!["A"], Some(vec!["B"]));
        let v = deterministic_invariants(&s, &[], &p, &[ev], "clean", &active());
        assert!(v.is_empty());
    }

    #[test]
    fn i3_flags_cross_location_target() {
        // Phase 2 同组在场重定义：actor A 在「前厅」、target B 在「密室」→ B 不在 A 的同组集 → I3 违规。
        let mut s = state_with_secret();
        s.characters.get_mut("A").unwrap().location = "前厅".into();
        s.characters.get_mut("B").unwrap().location = "密室".into();
        let p = patch("p1", vec![], vec![]);
        let ev = event("e1", "p1", vec!["A"], Some(vec!["B"]));
        let v = deterministic_invariants(&s, &[], &p, &[ev], "clean", &active());
        assert!(v.iter().any(|x| x.starts_with("I3")), "跨地点 target 应 I3 违规：{v:?}");
    }

    #[test]
    fn i3_passes_same_location_actor_and_target() {
        // 同地点 actor+target → 在场，不违规。
        let mut s = state_with_secret();
        s.characters.get_mut("A").unwrap().location = "密室".into();
        s.characters.get_mut("B").unwrap().location = "密室".into();
        let p = patch("p1", vec![], vec![]);
        let ev = event("e1", "p1", vec!["A"], Some(vec!["B"]));
        let v = deterministic_invariants(&s, &[], &p, &[ev], "clean", &active());
        assert!(v.is_empty(), "同地点 actor+target 应通过：{v:?}");
    }

    // ===== I4 锁定场景保护 =====

    #[test]
    fn i4_flags_unlock_attempt() {
        use crate::narrative::types::PatchOp;
        let s = state_with_secret();
        let op = PatchOperation {
            op: PatchOp::Remove,
            path: "authoring.lockedSceneIds".into(),
            value: Some(serde_json::json!("sc1")),
            precondition: None,
        };
        let p = patch("p1", vec![], vec![op]);
        let v = deterministic_invariants(&s, &[], &p, &[], "clean", &active());
        assert!(v.iter().any(|x| x.starts_with("I4")));
    }

    #[test]
    fn i4_allows_append_lock() {
        use crate::narrative::types::PatchOp;
        let s = state_with_secret();
        let op = PatchOperation {
            op: PatchOp::Append,
            path: "authoring.lockedSceneIds".into(),
            value: Some(serde_json::json!("sc1")),
            precondition: None,
        };
        let p = patch("p1", vec![], vec![op]);
        let v = deterministic_invariants(&s, &[], &p, &[], "clean", &active());
        assert!(v.is_empty(), "追加锁定应被允许：{v:?}");
    }

    // ===== critic =====

    #[tokio::test]
    async fn narrative_critic_parses_report() {
        let resp = r#"{"characterConsistencyIssues":["李四行为偏离价值排序"],"causalIssues":[],"revisionSuggestions":["补一处动机铺垫"]}"#;
        let events = Arc::new(CollectEvents::default());
        let host = Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1000)),
            events,
            model: Arc::new(ScriptedModel::new(vec![Ok(resp.to_string())])),
        });
        let profile = ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "http://x".into(),
            api_key: "k".into(),
            model: "m".into(),
        };
        let prompts = CriticPrompts { system: "你是审校".into(), prompt_version: "v1".into() };
        let report = narrative_critic(
            host.as_ref(),
            &profile,
            &prompts,
            "run-1",
            "正文……",
            &[decision("d1", "A")],
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.character_consistency_issues.len(), 1);
        assert_eq!(report.revision_suggestions.len(), 1);
        assert!(report.causal_issues.is_empty());
    }
}
