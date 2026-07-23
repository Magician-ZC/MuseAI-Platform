//! P2 数据模型：五层叙事状态 / StatePatch / DomainEvent / 决策协议 / 大纲约束。
//! （本地规格 §9.4、§12.2 + 平台规格 §9.4 的 DomainEvent；serde camelCase 与 TS/平台端一致）

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------- 五层状态 ----------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EmotionEntry {
    pub name: String,
    /// 0.0–1.0
    pub intensity: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CharacterState {
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub emotions: Vec<EmotionEntry>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub misconceptions: Vec<String>,
    #[serde(default)]
    pub plans: Vec<String>,
    #[serde(default)]
    pub arc_stage: String,
}

/// 方向性关系：A→B 与 B→A 独立。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationState {
    pub from: String,
    pub to: String,
    pub trust: f32,
    pub affinity: f32,
    pub fear: f32,
    pub debt: f32,
    /// 哪些角色知道这段关系的存在（信息边界的一部分）
    #[serde(default)]
    pub known_to: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConstraintLevel {
    Hard,
    Soft,
    Free,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NodeStatus {
    Pending,
    Done,
    Bypassed,
    /// 约束互相冲突或不可满足：暂停等待用户裁决，不允许伪造完成（规格 §5.3.1）
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineNode {
    pub id: String,
    pub summary: String,
    pub constraint: ConstraintLevel,
    pub status: NodeStatus,
}

/// 禁止结果是独立的状态谓词，不与节点混为同一枚举（规格 §5.2）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForbiddenPredicate {
    pub id: String,
    /// MVP 表达式：`path op value`，如 `characters.li.secrets contains "身世"` 的受限 DSL，
    /// 由 constraints::eval_predicate 解释；不支持任意代码。
    pub expression: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NarrativeLayer {
    #[serde(default)]
    pub outline_nodes: Vec<OutlineNode>,
    #[serde(default)]
    pub forbidden_predicates: Vec<ForbiddenPredicate>,
    #[serde(default)]
    pub foreshadowing: Vec<String>,
    #[serde(default)]
    pub pacing_notes: Vec<String>,
    /// 待审批的不可逆结果（角色死亡/永久退场/永久关系变更）。引擎门控元数据：不经 reducer
    /// 白名单，由 run_round 在门控未获批的不可逆结果时记入；获批后经 RoundInput.approved_consents
    /// 落定并清除（REMEDIATION #3 / 规格 §2.4）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_consents: Vec<PendingConsent>,
}

/// 待审批的不可逆结果条目（每个当事角色一条）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingConsent {
    /// 当事角色 id（其主人需授权）
    pub subject: String,
    /// 不可逆事件类别：`death` | `permanent_exit` | `permanent_relation_change`
    pub event_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AuthoringLayer {
    #[serde(default)]
    pub locked_scene_ids: Vec<String>,
    #[serde(default)]
    pub branch_snapshot_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NarrativeState {
    pub schema_version: u32, // 1
    pub run_id: String,
    /// compare-and-swap / 原子提交
    pub revision: u64,
    #[serde(default)]
    pub world: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub characters: BTreeMap<String, CharacterState>,
    #[serde(default)]
    pub relations: Vec<RelationState>,
    #[serde(default)]
    pub narrative: NarrativeLayer,
    #[serde(default)]
    pub authoring: AuthoringLayer,
}

// ---------- StatePatch（状态变化唯一事实源） ----------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PatchOp {
    Set,
    Append,
    Remove,
    Increment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchOperation {
    pub op: PatchOp,
    /// 只允许 reducer 白名单路径（reducer::PATH_WHITELIST）
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// 可选前置条件：当前值必须等于它才应用（乐观校验）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatePatch {
    pub id: String,
    pub base_revision: u64,
    pub source_decision_ids: Vec<String>,
    pub operations: Vec<PatchOperation>,
}

// ---------- DomainEvent（宿主无关、版本化；平台层在 P3 包装为 WorldEvent） ----------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainEventType {
    ActionResolved,
    DialogueSpoken,
    RelationChanged,
    ResourceChanged,
    OutlineProgressed,
    ConsentRequested,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainEvent {
    pub schema_version: u32, // 1
    pub id: String,
    pub run_id: String,
    pub sequence: u64,
    #[serde(rename = "type")]
    pub event_type: DomainEventType,
    pub actor_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_ids: Option<Vec<String>>,
    /// 按 type 的事实负载（实现按 type 独立 schema 校验）
    pub fact: serde_json::Value,
    pub state_patch_id: String,
    #[serde(default)]
    pub caused_by: Vec<String>,
    /// 可见性：public / 指定角色主人可见（信息差载体）
    pub visibility: EventVisibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "scope")]
pub enum EventVisibility {
    Public,
    #[serde(rename_all = "camelCase")]
    Private {
        audience_character_ids: Vec<String>,
    },
}

// ---------- role_decide 协议（输出是提案，不是状态变更命令） ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleDecision {
    /// 代码补齐，不来自模型
    #[serde(default)]
    pub decision_id: String,
    #[serde(default)]
    pub character_id: String,
    pub intent: String,
    pub action: String,
    pub speak: SpeakIntent,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub acceptable_costs: Vec<String>,
    #[serde(default)]
    pub predictions: Vec<Prediction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakIntent {
    pub will_speak: bool,
    #[serde(default)]
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prediction {
    pub character_id: String,
    pub expected: String,
    #[serde(default)]
    pub confidence: f32,
}

// ---------- 仲裁 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArbiterOutcome {
    /// 不可变引用：被裁决的决策 id（意图原文不改写）
    pub decision_id: String,
    pub character_id: String,
    pub result: ArbiterResult,
    /// 规则依据（面向透明战报；不含隐藏推理）
    pub rule_refs: Vec<String>,
    #[serde(default)]
    pub consequence: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ArbiterResult {
    Success,
    PartialSuccess,
    Failure,
    Invalid,
    /// 与硬节点/底线冲突且无法调整实现：整回合进入 blocked
    Blocked,
}

// ---------- 回合与场景 ----------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RunMode {
    Interactive,
    Observe,
    ChapterDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneRecord {
    pub scene_id: String,
    pub tick: u64,
    pub situation: String,
    pub decisions: Vec<RoleDecision>,
    pub outcomes: Vec<ArbiterOutcome>,
    pub prose: String,
    pub events: Vec<DomainEvent>,
    pub state_patch: StatePatch,
    pub locked: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundBudget {
    /// 单章/单次运行 token 硬上限；耗尽 → BudgetExhausted 优雅停止（不提交半回合）
    pub max_total_tokens: u64,
    pub spent_tokens: u64,
    pub max_scenes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostEstimate {
    pub calls_per_scene: u32,
    pub estimated_tokens_low: u64,
    pub estimated_tokens_high: u64,
}
