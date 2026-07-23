//! DNA 合成（规格 §10.2 阶段 6–7）：每角色 1–2 次调用；矛盾审查在合成 prompt 内完成。
//! 文件所有权：agent-E1。

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::store::content_hash;
use crate::EngineError;

use super::evidence::{ledger_path, EvidenceLedger};
use super::types::{
    Agency, CardLifecycle, CharacterCardV2, DecisionModel, DramaticCore, EmotionDynamics,
    EvidenceIndex, ExpressionFingerprint, GrowthArc, Identity, Importance, Perception,
    RelationGrammar, RosterEntry, RosterTier, SourceWork, WorldAdaptation,
};
use super::CharacterPrompts;

/// 证据全量超过此字符数则先做摘要分片。
const SYNTH_DIRECT_MAX: usize = 60_000;
/// 摘要分片的目标大小。
const SYNTH_CHUNK_CHARS: usize = 8_000;

/// 合成单角色：
/// - 证据全量 ≤ 60k 字符时单次调用；超长先按 8k 分片做证据摘要（第 1 次调用/片，聚合后再合成）；
/// - 输出 CharacterCardV2 各层（模型返回 camelCase JSON，直接反序列化到卡结构，schemaVersion/lifecycle/
///   evidence_index/revision/created_at 由代码补齐，不信任模型）；
/// - 合成 prompt 要求区分「成长变化 / 叙述者不可靠 / 真实矛盾」并写入 conflictsWith；
/// - 产出一律 lifecycle=Draft（§9.1：迁移与合成不得伪装完整卡）；
/// - 校验：卡内引用的 evidenceIds ⊆ 账本 id 集（悬空即 Validation 错误）。
pub async fn synthesize_character(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &CharacterPrompts,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    entry: &RosterEntry,
    ledger: &EvidenceLedger,
    source_title: &str,
    cancel: &CancelFlag,
) -> Result<CharacterCardV2, EngineError> {
    // 组装证据块；超长时先分片摘要（每片 1 次调用）。
    let total: usize = ledger.evidence.iter().map(|e| e.quote_preview.chars().count()).sum();
    let evidence_block = if total > SYNTH_DIRECT_MAX {
        summarize_evidence(host, profile, prompts, temperature, run_id, ledger, cancel).await?
    } else {
        format_evidence(ledger)
    };

    let user = build_synthesis_prompt(entry, source_title, &evidence_block);
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.synthesis_system.clone(),
        user,
        temperature,
        max_output_tokens,
        agent: "characterSynthesis".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };
    // 模型可能只给部分层/部分字段：先取原始 JSON，再逐层「默认值叠加模型输出」，
    // 既容忍缺字段又不因缺必填 String 而整体失败（不信任模型，但尽量吸收其有效输出）。
    let value: serde_json::Value = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    let layers = SynthLayers::from_model(&value);

    let card = assemble_card(entry, ledger, source_title, host.clock.now_ms(), layers);

    // 引用完整性：卡内 evidenceIds ⊆ 账本 id 集。
    let ledger_ids: BTreeSet<&str> = ledger.evidence.iter().map(|e| e.id.as_str()).collect();
    for rule in &card.decision_model.decision_rules {
        if let Some(ids) = &rule.evidence_ids {
            for id in ids {
                if !ledger_ids.contains(id.as_str()) {
                    return Err(EngineError::Validation(format!(
                        "角色 {} 的 decisionRule 引用了不存在的证据 id: {id}",
                        entry.canonical_name
                    )));
                }
            }
        }
    }
    Ok(card)
}

/// 用代码可信字段补齐模型返回的十层，产出 Draft 卡。
fn assemble_card(
    entry: &RosterEntry,
    ledger: &EvidenceLedger,
    source_title: &str,
    now_ms: i64,
    layers: SynthLayers,
) -> CharacterCardV2 {
    let mut identity = layers.identity;
    identity.name = entry.canonical_name.clone(); // 姓名以 roster 为准
    for a in &entry.aliases {
        if !identity.aliases.contains(a) {
            identity.aliases.push(a.clone());
        }
    }
    identity.importance = tier_to_importance(entry.tier);
    let source_id = ledger.evidence.first().map(|e| e.source_id.clone()).unwrap_or_default();
    identity.source_work = Some(SourceWork {
        source_id,
        title: source_title.to_string(),
        version: None,
    });

    // 从账本对象重算 index（与 build_ledgers 落盘字节一致）。
    let evidence_index = evidence_index_of(ledger);

    CharacterCardV2 {
        schema_version: 2,
        id: entry.key.clone(),
        lifecycle: CardLifecycle::Draft,
        identity,
        dramatic_core: layers.dramatic_core,
        decision_model: layers.decision_model,
        perception: layers.perception,
        emotion_dynamics: layers.emotion_dynamics,
        relation_grammar: layers.relation_grammar,
        expression_fingerprint: layers.expression_fingerprint,
        agency: layers.agency,
        growth_arc: layers.growth_arc,
        world_adaptation: layers.world_adaptation,
        evidence_index,
        revision: 0,
        created_at: now_ms,
        updated_at: now_ms,
    }
}

fn evidence_index_of(ledger: &EvidenceLedger) -> EvidenceIndex {
    let bytes = serde_json::to_vec_pretty(ledger).unwrap_or_default();
    EvidenceIndex {
        store_key: ledger_path(&ledger.character_id).to_string_lossy().to_string(),
        content_hash: content_hash(&bytes),
        count: ledger.evidence.len() as u32,
    }
}

fn tier_to_importance(t: RosterTier) -> Importance {
    match t {
        RosterTier::Core => Importance::Core,
        RosterTier::Major => Importance::Major,
        RosterTier::Functional | RosterTier::Extra => Importance::Functional,
    }
}

fn format_evidence(ledger: &EvidenceLedger) -> String {
    let mut s = String::new();
    for e in &ledger.evidence {
        s.push_str(&format!(
            "[{id}] 第{ch}章 ({kind:?}, {conf:?})：{quote}\n",
            id = e.id,
            ch = e.chapter_index,
            kind = e.kind,
            conf = e.confidence,
            quote = e.quote_preview,
        ));
    }
    s
}

/// 证据超长：分片摘要后聚合为紧凑证据块（每片 1 次调用）。
async fn summarize_evidence(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &CharacterPrompts,
    temperature: f32,
    run_id: &str,
    ledger: &EvidenceLedger,
    cancel: &CancelFlag,
) -> Result<String, EngineError> {
    let chunks = chunk_evidence(ledger, SYNTH_CHUNK_CHARS);
    let mut summaries = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let user = format!(
            "以下是角色「{name}」的部分原文证据，请压缩为不超过 300 字的要点摘要，保留关键行为、选择、情绪与关系，保留可引用的证据 id：\n{chunk}",
            name = ledger.character_id,
            chunk = chunk,
        );
        let spec = ModelCallSpec {
            profile: profile.clone(),
            system: prompts.synthesis_system.clone(),
            user,
            temperature,
            max_output_tokens: 1024,
            agent: "characterEvidenceSummary".to_string(),
            prompt_version: prompts.prompt_version.clone(),
            run_id: run_id.to_string(),
        };
        let resp: SummaryResponse =
            json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
        summaries.push_str(&format!("片段{}：{}\n", i + 1, resp.summary));
    }
    Ok(summaries)
}

/// 把证据按累计字符数切成 ≤ chunk_chars 的片。
fn chunk_evidence(ledger: &EvidenceLedger, chunk_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for e in &ledger.evidence {
        let line = format!("[{}] 第{}章：{}\n", e.id, e.chapter_index, e.quote_preview);
        let line_len = line.chars().count();
        if cur_len + line_len > chunk_chars && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        cur.push_str(&line);
        cur_len += line_len;
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

fn build_synthesis_prompt(entry: &RosterEntry, source_title: &str, evidence_block: &str) -> String {
    let aliases = if entry.aliases.is_empty() { "无".to_string() } else { entry.aliases.join("、") };
    format!(
        "你要基于以下证据，为角色「{name}」合成结构化 Character DNA（V2）。\n\
作品：{title}\n已知别名：{aliases}\n\n\
【证据账本（引用时用方括号内的 id）】\n{evidence}\n\n\
请严格输出 JSON，字段用 camelCase，包含十层：identity, dramaticCore, decisionModel, perception, \
emotionDynamics, relationGrammar, expressionFingerprint, agency, growthArc, worldAdaptation。\n\
decisionModel.decisionRules 每条形如 {{\"when\":\"..\",\"then\":\"..\",\"because\":\"..\",\"evidenceIds\":[\"..\"]}}，\
evidenceIds 只能引用上面出现过的证据 id。\n\
矛盾审查：区分「成长变化 / 叙述者不可靠 / 真实矛盾」，仅依据证据推断，不要编造证据中没有的设定，无法确定的字段留空。",
        name = entry.canonical_name,
        title = source_title,
        aliases = aliases,
        evidence = evidence_block,
    )
}

/// 模型返回的十层（全部可缺省，缺失即取 Default）。
#[derive(Default)]
struct SynthLayers {
    identity: Identity,
    dramatic_core: DramaticCore,
    decision_model: DecisionModel,
    perception: Perception,
    emotion_dynamics: EmotionDynamics,
    relation_grammar: RelationGrammar,
    expression_fingerprint: ExpressionFingerprint,
    agency: Agency,
    growth_arc: GrowthArc,
    world_adaptation: WorldAdaptation,
}

impl SynthLayers {
    /// 逐层从模型 JSON 提取（camelCase key）；每层用「默认值叠加」容忍缺字段。
    fn from_model(value: &serde_json::Value) -> Self {
        Self {
            identity: layer_from(value.get("identity")),
            dramatic_core: layer_from(value.get("dramaticCore")),
            decision_model: layer_from(value.get("decisionModel")),
            perception: layer_from(value.get("perception")),
            emotion_dynamics: layer_from(value.get("emotionDynamics")),
            relation_grammar: layer_from(value.get("relationGrammar")),
            expression_fingerprint: layer_from(value.get("expressionFingerprint")),
            agency: layer_from(value.get("agency")),
            growth_arc: layer_from(value.get("growthArc")),
            world_adaptation: layer_from(value.get("worldAdaptation")),
        }
    }
}

/// 把模型给出的层字段叠加到该层的默认序列化上，再反序列化。
/// 好处：缺失的必填字段由默认值补齐，模型给出的字段照单接收；解析失败退回 Default。
fn layer_from<T: Serialize + DeserializeOwned + Default>(model: Option<&serde_json::Value>) -> T {
    let mut base = serde_json::to_value(T::default()).unwrap_or(serde_json::Value::Null);
    if let Some(over) = model {
        merge_json(&mut base, over);
    }
    serde_json::from_value(base).unwrap_or_default()
}

/// 递归合并：对象按 key 覆盖，其余整体替换。
fn merge_json(base: &mut serde_json::Value, over: &serde_json::Value) {
    match (base, over) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            for (k, v) in o {
                merge_json(b.entry(k.clone()).or_insert(serde_json::Value::Null), v);
            }
        }
        (b, o) => *b = o.clone(),
    }
}

#[derive(Deserialize)]
struct SummaryResponse {
    #[serde(default)]
    summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::merge::stable_key;
    use crate::character::types::{
        Confidence, DnaStatus, EvidenceKind, EvidenceLocator, EvidenceRef,
    };
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use std::sync::Arc;

    fn ledger_with(ids: &[&str]) -> EvidenceLedger {
        EvidenceLedger {
            schema_version: 1,
            character_id: stable_key("林冲"),
            evidence: ids
                .iter()
                .enumerate()
                .map(|(i, id)| EvidenceRef {
                    id: id.to_string(),
                    source_id: "src".into(),
                    chapter_index: i as u32,
                    locator: EvidenceLocator { start: 0, end: 4, heading: None },
                    quote_preview: "证据".into(),
                    kind: EvidenceKind::Action,
                    confidence: Confidence::High,
                    user_confirmed: None,
                    conflicts_with: None,
                })
                .collect(),
            revision: 1,
            updated_at: 0,
        }
    }

    fn entry() -> RosterEntry {
        RosterEntry {
            key: stable_key("林冲"),
            canonical_name: "林冲".into(),
            aliases: vec!["豹子头".into()],
            tier: RosterTier::Core,
            merged_from: vec!["林冲".into(), "豹子头".into()],
            user_confirmed: true,
            dna_status: DnaStatus::Pending,
        }
    }

    fn host_with(model: ScriptedModel) -> EngineHost {
        EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(9_000)),
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

    fn prompts() -> CharacterPrompts {
        CharacterPrompts {
            scan_system: "s".into(),
            merge_system: "s".into(),
            tiering_system: "s".into(),
            synthesis_system: "s".into(),
            prompt_version: "v1".into(),
        }
    }

    #[tokio::test]
    async fn synthesizes_draft_card_with_code_managed_fields() {
        let resp = r#"{
            "identity": {"name":"模型乱填","narrativeRole":"主角"},
            "dramaticCore": {"coreContradiction":"忠义两难","coreFear":"家破"},
            "decisionModel": {"decisionRules":[{"when":"被逼","then":"隐忍","because":"顾家","evidenceIds":["ev-1"]}]}
        }"#;
        let host = host_with(ScriptedModel::new(vec![Ok(resp.into())]));
        let ledger = ledger_with(&["ev-1", "ev-2"]);
        let card = synthesize_character(
            &host, &profile(), &prompts(), 0.2, 4096, "task-1", &entry(), &ledger, "水浒传", &CancelFlag::new(),
        )
        .await
        .unwrap();

        assert_eq!(card.schema_version, 2);
        assert!(matches!(card.lifecycle, CardLifecycle::Draft));
        assert_eq!(card.id, stable_key("林冲"));
        assert_eq!(card.identity.name, "林冲"); // 覆盖模型的乱填
        assert!(card.identity.aliases.contains(&"豹子头".to_string()));
        assert!(matches!(card.identity.importance, Importance::Core));
        assert_eq!(card.identity.source_work.as_ref().unwrap().title, "水浒传");
        assert_eq!(card.created_at, 9_000);
        assert_eq!(card.evidence_index.count, 2);
        assert_eq!(card.dramatic_core.core_contradiction, "忠义两难");
    }

    #[tokio::test]
    async fn dangling_evidence_id_is_validation_error() {
        let resp = r#"{"decisionModel":{"decisionRules":[{"when":"a","then":"b","because":"c","evidenceIds":["ev-404"]}]}}"#;
        let host = host_with(ScriptedModel::new(vec![Ok(resp.into())]));
        let ledger = ledger_with(&["ev-1"]);
        let err = synthesize_character(
            &host, &profile(), &prompts(), 0.0, 4096, "t", &entry(), &ledger, "水浒传", &CancelFlag::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn chunk_evidence_splits_by_char_budget() {
        // 每条约 300 字，budget 500 → 每片最多 1 条。
        let big: Vec<String> = (0..5).map(|i| format!("ev-{i}")).collect();
        let ids: Vec<&str> = big.iter().map(String::as_str).collect();
        let mut ledger = ledger_with(&ids);
        for e in ledger.evidence.iter_mut() {
            e.quote_preview = "字".repeat(300);
        }
        let chunks = chunk_evidence(&ledger, 500);
        assert_eq!(chunks.len(), 5);
    }
}
