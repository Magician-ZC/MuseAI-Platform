//! 角色评测（P0.a）：互换测试与压力测试。文件所有权：agent-E1。
//!
//! 阳性/阴性对照契约（P0 测试清单）：不同角色 → interchangeable=false 且 findings 有 distinct 项；
//! 同卡复制两份 → interchangeable=true。判定由模型输出，但代码负责：同卡（内容哈希一致）时
//! 直接短路返回 interchangeable=true，不烧模型调用。

use serde::Deserialize;

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::store::content_hash;
use crate::EngineError;

use super::types::{
    CharacterCardV2, EvidenceIndex, StressScenarioResult, StressTestReport, SwapFinding,
    SwapTestReport,
};

pub struct EvalPrompts {
    pub swap_system: String,
    pub stress_system: String,
    pub prompt_version: String,
}

pub async fn run_swap_test(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &EvalPrompts,
    card_a: &CharacterCardV2,
    card_b: &CharacterCardV2,
    scenario: &str,
    cancel: &CancelFlag,
) -> Result<SwapTestReport, EngineError> {
    // 同卡（内容签名一致）短路：直接判为可互换，不发模型调用。
    if content_signature(card_a) == content_signature(card_b) {
        return Ok(SwapTestReport {
            card_a: card_a.id.clone(),
            card_b: card_b.id.clone(),
            scenario: scenario.to_string(),
            findings: Vec::new(),
            interchangeable: true,
            summary: "两张卡内容完全一致（同卡复制），在任何情境下行为不可区分。".to_string(),
        });
    }

    let user = build_swap_prompt(card_a, card_b, scenario);
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.swap_system.clone(),
        user,
        temperature: 0.0,
        max_output_tokens: 2048,
        agent: "characterSwapTest".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: format!("swap-{}-{}", card_a.id, card_b.id),
    };
    let resp: SwapResponse = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(SwapTestReport {
        card_a: card_a.id.clone(),
        card_b: card_b.id.clone(),
        scenario: scenario.to_string(),
        findings: resp.findings,
        interchangeable: resp.interchangeable,
        summary: resp.summary,
    })
}

pub async fn run_stress_test(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &EvalPrompts,
    card: &CharacterCardV2,
    scenarios: &[String],
    cancel: &CancelFlag,
) -> Result<StressTestReport, EngineError> {
    let user = build_stress_prompt(card, scenarios);
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.stress_system.clone(),
        user,
        temperature: 0.0,
        max_output_tokens: 2048,
        agent: "characterStressTest".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: format!("stress-{}", card.id),
    };
    let resp: StressResponse = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    // 一致性以逐场景判定聚合为准，不单独信任模型的顶层结论。
    let consistent = !resp.scenarios.is_empty() && resp.scenarios.iter().all(|s| s.consistent_with_core);
    Ok(StressTestReport {
        card_id: card.id.clone(),
        scenarios: resp.scenarios,
        consistent,
        summary: resp.summary,
    })
}

/// 卡内容签名：抹去 id / 版本 / 时间戳 / 证据索引等易变字段后哈希，
/// 用于识别「同一角色复制两份」。
fn content_signature(card: &CharacterCardV2) -> String {
    let mut c = card.clone();
    c.id = String::new();
    c.revision = 0;
    c.created_at = 0;
    c.updated_at = 0;
    c.lifecycle = super::types::CardLifecycle::Draft;
    c.evidence_index = EvidenceIndex::default();
    let bytes = serde_json::to_vec(&c).unwrap_or_default();
    content_hash(&bytes)
}

fn build_swap_prompt(a: &CharacterCardV2, b: &CharacterCardV2, scenario: &str) -> String {
    format!(
        "在同一情境下分别让角色 A 与角色 B 决策，判断二者行为是否真的不同（可否互换）。\n\
情境：{scenario}\n\n【角色 A】\n{a}\n\n【角色 B】\n{b}\n\n\
请逐维度（决策模型/戏剧内核/情绪动力/关系语法/表达指纹）对比，严格输出 JSON：\n\
{{\"findings\":[{{\"dimension\":\"维度\",\"aBehavior\":\"A的做法\",\"bBehavior\":\"B的做法\",\"distinct\":true}}],\
\"interchangeable\":false,\"summary\":\"结论\"}}\n\
若两者在关键维度行为一致则 interchangeable=true。",
        a = compact_card(a),
        b = compact_card(b),
    )
}

fn build_stress_prompt(card: &CharacterCardV2, scenarios: &[String]) -> String {
    let list = scenarios
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "让以下角色在多个压力情境中分别决策，检验其价值与策略是否保持可解释的一致。\n\
【角色】\n{card}\n\n【情境】\n{list}\n\n\
严格输出 JSON：{{\"scenarios\":[{{\"scenario\":\"情境原文\",\"predictedChoice\":\"预测选择\",\
\"rationale\":\"依据角色内核的理由\",\"consistentWithCore\":true}}],\"consistent\":true,\"summary\":\"结论\"}}",
        card = compact_card(card),
    )
}

/// 压缩卡为决策相关的行为层（避免把证据索引等噪声塞入 prompt）。
fn compact_card(card: &CharacterCardV2) -> String {
    let value = serde_json::json!({
        "name": card.identity.name,
        "dramaticCore": card.dramatic_core,
        "decisionModel": card.decision_model,
        "perception": card.perception,
        "emotionDynamics": card.emotion_dynamics,
        "relationGrammar": card.relation_grammar,
        "expressionFingerprint": card.expression_fingerprint,
        "agency": card.agency,
    });
    serde_json::to_string(&value).unwrap_or_default()
}

#[derive(Deserialize)]
struct SwapResponse {
    #[serde(default)]
    findings: Vec<SwapFinding>,
    #[serde(default)]
    interchangeable: bool,
    #[serde(default)]
    summary: String,
}

#[derive(Deserialize)]
struct StressResponse {
    #[serde(default)]
    scenarios: Vec<StressScenarioResult>,
    #[serde(default)]
    summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use std::sync::Arc;

    fn base_card(id: &str, contradiction: &str) -> CharacterCardV2 {
        let mut identity = Identity::default();
        identity.name = "角色".into();
        let mut dramatic_core = DramaticCore::default();
        dramatic_core.core_contradiction = contradiction.into();
        CharacterCardV2 {
            schema_version: 2,
            id: id.into(),
            lifecycle: CardLifecycle::Draft,
            identity,
            dramatic_core,
            decision_model: DecisionModel::default(),
            perception: Perception::default(),
            emotion_dynamics: EmotionDynamics::default(),
            relation_grammar: RelationGrammar::default(),
            expression_fingerprint: ExpressionFingerprint::default(),
            agency: Agency::default(),
            growth_arc: GrowthArc::default(),
            world_adaptation: WorldAdaptation::default(),
            evidence_index: EvidenceIndex::default(),
            revision: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn host_with(model: ScriptedModel) -> EngineHost {
        EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(0)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(model),
        }
    }

    fn profile() -> ModelProfile {
        ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "u".into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    fn eval_prompts() -> EvalPrompts {
        EvalPrompts { swap_system: "s".into(), stress_system: "s".into(), prompt_version: "v1".into() }
    }

    #[tokio::test]
    async fn same_card_copy_short_circuits_without_model() {
        // 两份副本仅 id / 时间戳不同 → 短路，空脚本若被调用会报错。
        let mut a = base_card("card-a", "忠义两难");
        let mut b = base_card("card-b", "忠义两难");
        a.created_at = 100;
        b.created_at = 999;
        b.revision = 5;
        let host = host_with(ScriptedModel::new(vec![]));
        let report =
            run_swap_test(&host, &profile(), &eval_prompts(), &a, &b, "被出卖时如何反应", &CancelFlag::new())
                .await
                .unwrap();
        assert!(report.interchangeable);
        assert!(report.findings.is_empty());
        assert_eq!(report.card_a, "card-a");
        assert_eq!(report.card_b, "card-b");
    }

    #[tokio::test]
    async fn different_cards_report_distinct_findings() {
        let a = base_card("card-a", "忠义两难");
        let b = base_card("card-b", "复仇与救赎"); // 内核不同 → 需模型判定
        let resp = r#"{"findings":[{"dimension":"决策模型","aBehavior":"隐忍","bBehavior":"反击","distinct":true}],
            "interchangeable":false,"summary":"两者选择相反"}"#;
        let host = host_with(ScriptedModel::new(vec![Ok(resp.into())]));
        let report =
            run_swap_test(&host, &profile(), &eval_prompts(), &a, &b, "被出卖时如何反应", &CancelFlag::new())
                .await
                .unwrap();
        assert!(!report.interchangeable);
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].distinct);
    }

    #[tokio::test]
    async fn stress_consistency_aggregated_from_scenarios() {
        let card = base_card("card-a", "忠义两难");
        // 一条一致、一条不一致 → 顶层 consistent 必为 false。
        let resp = r#"{"scenarios":[
            {"scenario":"s1","predictedChoice":"隐忍","rationale":"顾家","consistentWithCore":true},
            {"scenario":"s2","predictedChoice":"暴走","rationale":"矛盾","consistentWithCore":false}
        ],"consistent":true,"summary":"存在偏离"}"#;
        let host = host_with(ScriptedModel::new(vec![Ok(resp.into())]));
        let report = run_stress_test(
            &host,
            &profile(),
            &eval_prompts(),
            &card,
            &["s1".into(), "s2".into()],
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.scenarios.len(), 2);
        assert!(!report.consistent); // 代码聚合覆盖模型的乐观结论
    }
}
