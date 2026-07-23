//! P0 数据模型：Character DNA V2、证据、提取任务（规格 §9.1 / §9.3 的 Rust 镜像，serde camelCase 与 TS 端一致）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRef {
    pub id: String,
    pub source_id: String,
    pub chapter_index: u32,
    pub locator: EvidenceLocator,
    /// UI 预览，≤200 字；非完整原文副本
    pub quote_preview: String,
    pub kind: EvidenceKind,
    pub confidence: Confidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts_with: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLocator {
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceKind {
    Description,
    Action,
    OtherView,
    Inference,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRule {
    pub when: String,
    pub then: String,
    pub because: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CardLifecycle {
    Draft,
    Reviewed,
    Ready,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Importance {
    Core,
    Major,
    Functional,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrative_role: Option<String>,
    #[serde(default = "default_importance")]
    pub importance: Importance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_work: Option<SourceWork>,
    /// V1 原样保留区，禁止类型收窄丢数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_v1_fields: Option<serde_json::Value>,
}

impl Default for Importance {
    fn default() -> Self {
        Importance::Functional
    }
}
fn default_importance() -> Importance {
    Importance::Functional
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceWork {
    pub source_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DramaticCore {
    pub core_contradiction: String,
    pub surface_goal: String,
    pub hidden_need: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_desire: Option<String>,
    pub core_fear: String,
    pub stakes: String,
    #[serde(default)]
    pub bottom_lines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_deception: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DecisionModel {
    #[serde(default)]
    pub value_priorities: Vec<String>,
    #[serde(default)]
    pub risk_appetite: String,
    #[serde(default)]
    pub default_strategies: Vec<String>,
    #[serde(default)]
    pub escalation_path: Vec<String>,
    #[serde(default)]
    pub sacrifice_order: Vec<String>,
    #[serde(default)]
    pub known_biases: Vec<String>,
    #[serde(default)]
    pub decision_rules: Vec<DecisionRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Perception {
    #[serde(default)]
    pub first_notices: Vec<String>,
    #[serde(default)]
    pub blind_spots: Vec<String>,
    #[serde(default)]
    pub attribution_style: String,
    #[serde(default)]
    pub trust_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EmotionDynamics {
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub masking_style: String,
    #[serde(default)]
    pub outburst_pattern: String,
    #[serde(default)]
    pub recovery_conditions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressure_shift: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RelationGrammar {
    #[serde(default)]
    pub trust_building: String,
    #[serde(default)]
    pub trust_repair: String,
    #[serde(default)]
    pub modes_by_relation: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub attracted_by: Vec<String>,
    #[serde(default)]
    pub provoked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionFingerprint {
    #[serde(default)]
    pub sentence_rhythm: String,
    #[serde(default)]
    pub metaphor_sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questioning_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lying_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humor_style: Option<String>,
    #[serde(default)]
    pub say_vs_think_gap: String,
    #[serde(default)]
    pub signature_gestures: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_variants: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    pub forbidden_phrases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Agency {
    #[serde(default)]
    pub initiative_triggers: Vec<String>,
    #[serde(default)]
    pub default_plans: Vec<String>,
    #[serde(default)]
    pub long_term_agenda: String,
    #[serde(default)]
    pub leverage: Vec<String>,
    #[serde(default)]
    pub plot_seeds: Vec<String>,
    #[serde(default)]
    pub refusal_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GrowthArc {
    #[serde(default)]
    pub immutable_core: Vec<String>,
    #[serde(default)]
    pub mutable_beliefs: Vec<String>,
    #[serde(default)]
    pub break_points: Vec<String>,
    #[serde(default)]
    pub awakening_points: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorldAdaptation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_mapping: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_mapping: Option<String>,
    #[serde(default)]
    pub must_preserve: Vec<String>,
    #[serde(default)]
    pub localizable: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_fallback: Option<String>,
}

/// 证据外置索引：全量证据存 `character-engine/evidence/<characterId>.json`，卡内只留索引。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceIndex {
    pub store_key: String,
    pub content_hash: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterCardV2 {
    pub schema_version: u32, // 恒为 2
    pub id: String,
    pub lifecycle: CardLifecycle,
    pub identity: Identity,
    pub dramatic_core: DramaticCore,
    pub decision_model: DecisionModel,
    pub perception: Perception,
    pub emotion_dynamics: EmotionDynamics,
    pub relation_grammar: RelationGrammar,
    pub expression_fingerprint: ExpressionFingerprint,
    pub agency: Agency,
    pub growth_arc: GrowthArc,
    pub world_adaptation: WorldAdaptation,
    pub evidence_index: EvidenceIndex,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------- 提取任务模型（§9.3） ----------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ChapterStatus {
    Pending,
    Running,
    Scanned,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterEntry {
    pub id: String,
    pub index: u32,
    pub title: String,
    pub char_range: (usize, usize),
    pub status: ChapterStatus,
    pub attempt: u32,
    /// 大结果分片存储 key（相对 data_root），任务文件不无限增长
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_store_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<TaskError>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RosterTier {
    Core,
    Major,
    Functional,
    Extra,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DnaStatus {
    Pending,
    Generated,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterEntry {
    /// 首次确认后稳定，不以名称作为主键
    pub key: String,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub tier: RosterTier,
    pub merged_from: Vec<String>,
    pub user_confirmed: bool,
    pub dna_status: DnaStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskStage {
    Preprocess,
    Scan,
    Merge,
    Tiering,
    Synthesis,
    Review,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFingerprint {
    pub size: u64,
    pub modified_at: i64,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionTask {
    pub schema_version: u32, // 恒为 1
    pub task_id: String,
    pub work_title: String,
    pub source_path: String,
    pub source_fingerprint: SourceFingerprint,
    pub pipeline_version: String,
    pub chapters: Vec<ChapterEntry>,
    pub roster: Vec<RosterEntry>,
    pub stage: TaskStage,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------- 章节扫描产物 ----------

/// 单章角色发现结果（模型输出，字段白名单校验后落盘为分片）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterDiscovery {
    pub chapter_index: u32,
    pub mentions: Vec<CharacterMention>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterMention {
    /// 文中出现的表述（本名/别名/称呼/代称归属）
    pub surface: String,
    #[serde(default)]
    pub role_hint: String,
    /// 本章中该角色的行为/选择/情绪/关系/表达样本
    #[serde(default)]
    pub evidence: Vec<MentionEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MentionEvidence {
    pub kind: EvidenceKind,
    pub quote: String,
    #[serde(default)]
    pub note: String,
    pub confidence: Confidence,
}

// ---------- 覆盖报告与评测 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub scanned_chapters: u32,
    pub total_chapters: u32,
    pub failed_chapters: Vec<u32>,
    pub roster_size: u32,
    pub unresolved_aliases: Vec<String>,
    pub low_confidence_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapTestReport {
    pub card_a: String,
    pub card_b: String,
    pub scenario: String,
    /// 每个维度的差异描述与是否可互换判定
    pub findings: Vec<SwapFinding>,
    pub interchangeable: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapFinding {
    pub dimension: String,
    pub a_behavior: String,
    pub b_behavior: String,
    pub distinct: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StressTestReport {
    pub card_id: String,
    pub scenarios: Vec<StressScenarioResult>,
    pub consistent: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StressScenarioResult {
    pub scenario: String,
    pub predicted_choice: String,
    pub rationale: String,
    pub consistent_with_core: bool,
}
