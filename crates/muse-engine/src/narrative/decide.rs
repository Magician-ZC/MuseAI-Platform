//! 角色决策（规格 §12.2）：白名单上下文组装 + role_decide 调用。文件所有权：agent-E4。
//!
//! 信息边界铁律：给角色 X 组装的上下文只允许包含——
//! 公共 world 层、X 自己的 CharacterState、from==X 或 to==X 且 known_to 含 X 的关系、
//! 公开场景描述、X 的 DNA 卡、绑定到 X 的知识片段、主人托梦（平台注入，低优先层）。
//! 其他角色的 DNA 内容只能以第三人称一句话摘要出现（防卡片注入，平台规格 §14.1）。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::character::types::CharacterCardV2;
use crate::host::{CancelFlag, EngineHost};
use crate::knowledge::types::RetrievedFragment;
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{NarrativeState, RoleDecision};

pub struct DecidePrompts {
    pub system: String,
    pub prompt_version: String,
}

/// world 层内部保留键（幂等账），绝不进入任何角色可见上下文。
const RESERVED_WORLD_KEY: &str = "appliedPatchIds";

/// 从 DNA 卡中裁出「行为层」视图（角色自己的卡，全部对自己可见）；
/// 剔除存储/版本元数据，仅保留决策相关层，供角色本人推理。
fn dna_view(card: &CharacterCardV2) -> Result<Value, EngineError> {
    let mut v = serde_json::to_value(card)?;
    if let Some(obj) = v.as_object_mut() {
        for k in ["schemaVersion", "id", "lifecycle", "evidenceIndex", "revision", "createdAt", "updatedAt"] {
            obj.remove(k);
        }
    }
    Ok(v)
}

/// 组装角色可见上下文（纯函数，必测隔离性：B 的 secrets 永不出现在 A 的产物中）。
///
/// 铁律实现要点：只读 `state.characters[character_id]` 这一格自身私有状态，
/// 绝不遍历其他角色的 secrets/misconceptions/plans；他人仅以调用方给定的第三人称摘要出现。
pub fn assemble_visible_context(
    state: &NarrativeState,
    character_id: &str,
    card: &CharacterCardV2,
    other_cards_brief: &BTreeMap<String, String>,
    situation: &str,
    fragments: &[RetrievedFragment],
    whisper: Option<&str>,
) -> Result<String, EngineError> {
    // 1) 自己的私有状态：只取自己这一格（缺失视为空态），不触碰任何他人条目。
    let own = state.characters.get(character_id).cloned().unwrap_or_default();
    let own_v = serde_json::to_value(&own)?;

    // 2) 与自己相关且自己知情的关系：
    //    - from==X：X 是关系主体（自身对外的信任/情感），本人天然知情；
    //    - to==X 且 known_to 含 X：X 是关系客体且已被告知，才可见。
    //    其他角色之间的关系一律不进入（最大程度杜绝泄漏）。
    let relations: Vec<Value> = state
        .relations
        .iter()
        .filter(|r| {
            r.from == character_id
                || (r.to == character_id && r.known_to.iter().any(|k| k == character_id))
        })
        .map(|r| {
            json!({
                "from": r.from,
                "to": r.to,
                "trust": r.trust,
                "affinity": r.affinity,
                "fear": r.fear,
                "debt": r.debt,
                "notes": r.notes,
            })
        })
        .collect();

    // 3) 公共 world 层（剔除引擎内部保留键）。
    let world: BTreeMap<&String, &Value> =
        state.world.iter().filter(|(k, _)| k.as_str() != RESERVED_WORLD_KEY).collect();
    let world_v = serde_json::to_value(&world)?;

    // 4) 他人仅以第三人称一句话摘要出现（不含自己；防卡片/原文 DNA 注入）。
    let others: BTreeMap<&String, &String> =
        other_cards_brief.iter().filter(|(k, _)| k.as_str() != character_id).collect();
    let others_v = serde_json::to_value(&others)?;

    // 5) 绑定到自己的知识片段（来源可溯，供审校 100% 追踪）。
    let knowledge: Vec<Value> =
        fragments.iter().map(|f| json!({ "pack": f.pack_title, "text": f.text })).collect();

    // 6) 自己的 DNA 行为层。
    let dna_v = dna_view(card)?;

    let mut ctx = json!({
        "you": character_id,
        "situation": situation,
        "yourDna": dna_v,
        "yourState": own_v,
        "yourRelations": Value::Array(relations),
        "world": world_v,
        "others": others_v,
        "knowledge": Value::Array(knowledge),
    });
    // 主人托梦：最低优先层，仅在提供时附加。
    if let Some(w) = whisper {
        if let Some(obj) = ctx.as_object_mut() {
            obj.insert("whisper".to_string(), json!(w));
        }
    }

    Ok(serde_json::to_string_pretty(&ctx)?)
}

/// 决策用户提示：可见上下文 + 严格 JSON 输出契约。
fn build_decide_user_prompt(character_id: &str, visible_context: &str) -> String {
    format!(
        "以下是【仅你（{character_id}）可见】的信息，其它角色的私密一概不在其中：\n{visible_context}\n\n\
请完全代入该角色，基于上述信息做出本回合决策。你的输出只是【提案】，不直接改变世界状态。\
严格输出如下 JSON（不要输出多余文本或解释）：\n\
{{\"intent\":\"你的真实意图\",\"action\":\"你要采取的具体行动\",\
\"speak\":{{\"willSpeak\":true,\"purpose\":\"若发言，目的是什么\"}},\
\"targets\":[\"你行动指向的在场角色id\"],\
\"acceptableCosts\":[\"你愿意为此付出的代价\"],\
\"predictions\":[{{\"characterId\":\"某在场角色id\",\"expected\":\"你预测他会如何反应\",\"confidence\":0.6}}]}}"
    )
}

/// 单角色决策调用：严格 JSON → RoleDecision；decision_id/character_id 由代码补齐；
/// targets 白名单校验（只能指向在场角色），越界目标丢弃并记录。
#[allow(clippy::too_many_arguments)] // 签名由骨架固定：注入宿主 + 决策上下文全量入参
pub async fn role_decide(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &DecidePrompts,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    character_id: &str,
    visible_context: &str,
    active_character_ids: &[String],
    cancel: &CancelFlag,
) -> Result<RoleDecision, EngineError> {
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.system.clone(),
        user: build_decide_user_prompt(character_id, visible_context),
        temperature,
        max_output_tokens,
        agent: "roleDecide".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };

    let mut decision: RoleDecision =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;

    // 代码补齐不可信字段：决策 id 确定性派生（同 run 同角色稳定，便于幂等与定序）。
    decision.decision_id = format!("dec:{run_id}:{character_id}");
    decision.character_id = character_id.to_string();

    // targets 白名单：只保留在场角色，越界目标丢弃（模型原始输出已由 ModelCall 日志留痕）。
    decision.targets.retain(|t| active_character_ids.iter().any(|a| a == t));

    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::{CardLifecycle, Identity};
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use crate::narrative::types::{CharacterState, RelationState};
    use std::sync::Arc;

    fn minimal_card(name: &str) -> CharacterCardV2 {
        CharacterCardV2 {
            schema_version: 2,
            id: name.to_string(),
            lifecycle: CardLifecycle::Draft,
            identity: Identity { name: name.to_string(), ..Default::default() },
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

    fn state_with_secrets() -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        let mut a = CharacterState::default();
        a.secrets.push("我是国王的私生子".into());
        a.plans.push("暗杀公爵".into());
        a.misconceptions.push("误以为王后已死".into());
        a.goals.push("夺回王位".into());
        s.characters.insert("A".into(), a);
        s.characters.insert("B".into(), CharacterState::default());
        // A→B 的关系，仅 A 知情（known_to=[A]），B 不应看到其 notes。
        s.relations.push(RelationState {
            from: "A".into(),
            to: "B".into(),
            trust: 0.9,
            affinity: 0.1,
            fear: 0.0,
            debt: 0.0,
            known_to: vec!["A".into()],
            notes: vec!["秘密同盟标记".into()],
        });
        s.world.insert("phase".into(), serde_json::json!("夜晚"));
        // 内部保留键不得泄漏。
        s.world.insert(RESERVED_WORLD_KEY.into(), serde_json::json!(["patch-x"]));
        s
    }

    // ===== 信息边界铁律（§12.2，最高优先级，必测）=====

    #[test]
    fn iron_law_b_context_never_contains_a_private_fields() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let brief: BTreeMap<String, String> =
            [("A".to_string(), "A 是一名沉默寡言的侍卫。".to_string())].into_iter().collect();

        let ctx = assemble_visible_context(&s, "B", &card_b, &brief, "宫廷大厅", &[], None).unwrap();

        // A 的 secrets / plans / misconceptions / goals 原文一律不得出现在 B 的上下文里。
        assert!(!ctx.contains("私生子"), "泄漏了 A 的 secret：{ctx}");
        assert!(!ctx.contains("暗杀公爵"), "泄漏了 A 的 plan：{ctx}");
        assert!(!ctx.contains("王后已死"), "泄漏了 A 的 misconception：{ctx}");
        assert!(!ctx.contains("夺回王位"), "泄漏了 A 的 goal：{ctx}");
        // A→B 关系仅 A 知情，B 不得看到其 notes。
        assert!(!ctx.contains("秘密同盟标记"), "泄漏了 B 未知情的关系：{ctx}");
        // 引擎内部保留键不得泄漏。
        assert!(!ctx.contains(RESERVED_WORLD_KEY), "泄漏了内部保留键：{ctx}");

        // B 应能看到：他人第三人称摘要、公共 world、场景。
        assert!(ctx.contains("侍卫"), "缺少他人第三人称摘要");
        assert!(ctx.contains("夜晚"), "缺少公共 world 层");
        assert!(ctx.contains("宫廷大厅"), "缺少场景描述");
    }

    #[test]
    fn owner_can_see_own_private_and_own_relations() {
        let s = state_with_secrets();
        let card_a = minimal_card("A");
        let brief: BTreeMap<String, String> =
            [("B".to_string(), "B 是宫廷侍女。".to_string())].into_iter().collect();

        let ctx = assemble_visible_context(&s, "A", &card_a, &brief, "宫廷大厅", &[], None).unwrap();

        // A 自己能看到自己的私密（证明组装器确实注入了自身状态，只是不注入他人的）。
        assert!(ctx.contains("私生子"));
        assert!(ctx.contains("暗杀公爵"));
        // A 是关系主体（from==A），能看到该关系 notes。
        assert!(ctx.contains("秘密同盟标记"));
        // A 能看到他人（B）的第三人称摘要。
        assert!(ctx.contains("宫廷侍女"));
    }

    #[test]
    fn relation_visible_to_target_only_when_known() {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        s.characters.insert("A".into(), CharacterState::default());
        s.characters.insert("B".into(), CharacterState::default());
        // A→B，known_to 含 B：B 作为客体且已知情 → 可见。
        s.relations.push(RelationState {
            from: "A".into(),
            to: "B".into(),
            trust: 0.5,
            affinity: 0.0,
            fear: 0.0,
            debt: 0.0,
            known_to: vec!["A".into(), "B".into()],
            notes: vec!["公开的同僚关系".into()],
        });
        let card_b = minimal_card("B");
        let ctx =
            assemble_visible_context(&s, "B", &card_b, &BTreeMap::new(), "场景", &[], None).unwrap();
        assert!(ctx.contains("公开的同僚关系"), "已知情客体应能看到关系：{ctx}");
    }

    #[test]
    fn whisper_included_when_present() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let ctx = assemble_visible_context(
            &s,
            "B",
            &card_b,
            &BTreeMap::new(),
            "场景",
            &[],
            Some("主人提示：小心那个侍卫"),
        )
        .unwrap();
        assert!(ctx.contains("小心那个侍卫"));
    }

    #[test]
    fn knowledge_fragments_included_with_source() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let frags = vec![RetrievedFragment {
            pack_id: "kp-1".into(),
            pack_title: "宫廷礼仪".into(),
            chunk_id: "c1".into(),
            ordinal: 0,
            text: "面见君主须先行躬身礼。".into(),
            score: 1.0,
        }];
        let ctx =
            assemble_visible_context(&s, "B", &card_b, &BTreeMap::new(), "场景", &frags, None).unwrap();
        assert!(ctx.contains("宫廷礼仪"));
        assert!(ctx.contains("躬身礼"));
    }

    // ===== role_decide：补齐 id + targets 白名单 =====

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

    #[tokio::test]
    async fn role_decide_fills_ids_and_filters_targets() {
        // 模型返回一个含越界目标（ghost 不在场）的决策。
        let resp = r#"{"intent":"试探","action":"逼近王座","speak":{"willSpeak":true,"purpose":"表态"},"targets":["B","ghost"],"acceptableCosts":["名誉"],"predictions":[]}"#;
        let (host, _ev) = test_host(vec![Ok(resp.to_string())]);
        let prompts = DecidePrompts { system: "你是角色决策器".into(), prompt_version: "v1".into() };
        let active = vec!["A".to_string(), "B".to_string()];
        let d = role_decide(
            host.as_ref(),
            &dummy_profile(),
            &prompts,
            0.0,
            512,
            "run-1",
            "A",
            "（可见上下文）",
            &active,
            &CancelFlag::new(),
        )
        .await
        .unwrap();

        assert_eq!(d.decision_id, "dec:run-1:A");
        assert_eq!(d.character_id, "A");
        // ghost 越界被丢弃，仅保留在场的 B。
        assert_eq!(d.targets, vec!["B".to_string()]);
        assert!(d.speak.will_speak);
    }

    #[tokio::test]
    async fn role_decide_propagates_cancel() {
        let (host, _ev) = test_host(vec![Ok("{}".into())]);
        let prompts = DecidePrompts { system: "s".into(), prompt_version: "v1".into() };
        let cancel = CancelFlag::new();
        cancel.cancel();
        let err = role_decide(
            host.as_ref(),
            &dummy_profile(),
            &prompts,
            0.0,
            512,
            "run-1",
            "A",
            "ctx",
            &["A".to_string()],
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "cancelled");
    }
}
