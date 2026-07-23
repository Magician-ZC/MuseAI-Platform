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
    /// 角色动态位置（Phase 2：地点维度）。空串 = 无地点/全局场景（向后兼容：老状态无此字段，
    /// serde(default) 补空 → 全体归入单组 "" → 退化为无地点分组的旧行为）。碰撞分组的唯一动态依据、
    /// movement 行动的落定目标。经 reducer 白名单路径 `characters.<id>.location` 标量 Set 落定。
    #[serde(default)]
    pub location: String,
}

/// 地点图节点（Phase 2）：**静态模板数据**，不进 NarrativeState，随 `RoundInput.locations` 每 tick
/// 由调用方组装传入（与 active_cards/fragments 同款「调用方组装、后端无状态」）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocationDef {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// 可直达的地点 id（movement 连通性判定：目标须 ∈ 当前地点 connections）。
    #[serde(default)]
    pub connections: Vec<String>,
    /// 秘境标记（影响可见性隔离：分组时天然与外部隔离）。
    #[serde(default)]
    pub is_secret_realm: bool,
    /// 准入门槛（秘境用）：movement 抵达前须满足 gate。
    #[serde(default)]
    pub gate: Option<LocationGate>,
}

/// 地点准入门槛（Phase 2）：秘境等特殊地点的进入条件。纯静态、随 RoundInput 传入，
/// 仲裁 R6b 读取判定（连通之外的第二道闸）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocationGate {
    /// 需持有的道具 id（角色 resources 中以 `item:<id>` 或裸 id 形式持有）。
    #[serde(default)]
    pub required_item_ids: Vec<String>,
    /// 需具备的 effect_tag（同样以 resources 承载）。
    #[serde(default)]
    pub required_effect_tags: Vec<String>,
    /// 需满足的体系白名单（复用官方 KNOWN_COSMOLOGIES 语义；引擎侧不做体系元数据校验，
    /// 仅在调用方物化 held cosmology 后由 server 侧 check_location_admission 强化，Phase 3）。
    #[serde(default)]
    pub required_cosmologies: Vec<String>,
    /// 强度上限。
    #[serde(default)]
    pub max_power_tier: Option<u8>,
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
    /// 里程碑累积阈值（P1 放置房终局）。`Some` 标识「阈值里程碑」：走阈值累积推进（build_patch）+
    /// 计入 is_terminal 的 MainlineDone 里程碑集；`None` 走旧式 progressed=>done 兼容路径（硬/软老节点零变化）。
    /// 只读配置：仅 seed 写入、仅 build_patch/is_terminal 读取，永不出现在任何 StatePatch 路径（reducer 白名单不受影响）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// 里程碑推进的关系强度谓词门（受限 DSL，复用 constraints::eval_predicate，如
    /// `relations[a->b].affinity > 0.6`）。`Some` 时须谓词命中且 progress>=threshold 才翻 Done；`None` 仅看 progress。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance_when: Option<String>,
    /// 本节点强度权重覆盖；`None` 用全局默认 `IntensityWeights`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<IntensityWeights>,
}

/// 里程碑推进的回合强度权重（P1 放置房终局）。回合强度 = Σ outcomes 折算 + Σ willSpeak 决策互动强度，
/// 累积到 `world.milestoneProgress_<id>`，达 `threshold` 且 `advance_when` 命中即翻 Done。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IntensityWeights {
    /// 每个 `Success` outcome 的强度贡献。
    #[serde(default = "default_weight_success")]
    pub success: f64,
    /// 每个 `PartialSuccess` outcome 的强度贡献。
    #[serde(default = "default_weight_partial")]
    pub partial: f64,
    /// 每个 `Failure` outcome 的强度贡献（失败亦是推进主线的「事件」，贡献非零但更弱）。
    #[serde(default = "default_weight_failure")]
    pub failure: f64,
    /// 每个 `willSpeak=true` 决策的互动强度贡献。
    #[serde(default = "default_weight_speak")]
    pub speak: f64,
}

fn default_weight_success() -> f64 {
    1.0
}
fn default_weight_partial() -> f64 {
    0.5
}
fn default_weight_failure() -> f64 {
    0.25
}
fn default_weight_speak() -> f64 {
    0.25
}

impl Default for IntensityWeights {
    fn default() -> Self {
        Self {
            success: default_weight_success(),
            partial: default_weight_partial(),
            failure: default_weight_failure(),
            speak: default_weight_speak(),
        }
    }
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

/// 异步时间线层（P2 第二块 DES，Phase 0）：每角色行动指针 + 世界游戏时钟。
/// **引擎调度元数据，不经 reducer 白名单**（类比 `pending_consents`）——由 `run_event_step`
/// 经 `persist_timeline` 绕过 reducer 直接重写。Phase 0 仅为纯数据字段，`run_round` 不读不写，
/// `interval` 模式老世界完全不感知其存在（`#[serde(default)]` 保证旧存档反序列化为空 timeline）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TimelineLayer {
    /// 游戏时钟（ms 或抽象时间单位）。世界已推进到的最新游戏时刻。
    #[serde(default)]
    pub now: i64,
    /// 角色 id → 下次可行动的游戏时刻。缺席角色（未初始化）由调度器视为 `now`。
    #[serde(default)]
    pub next_time: BTreeMap<String, i64>,
    /// 时间上限（None = 无限）。`now >= time_cap` → `Terminal::TimeCapReached`。
    #[serde(default)]
    pub time_cap: Option<i64>,
    /// 时间线层 schema 版本。
    #[serde(default)]
    pub schema_version: u32,
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
    /// 异步时间线调度层（P2 DES，Phase 0）。`#[serde(default)]` 保证旧存档无此字段时
    /// 反序列化为空 timeline（后向兼容）。
    #[serde(default)]
    pub timeline: TimelineLayer,
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
    /// 事件对应行动在游戏时间轴上的落点（= 本步 cohort 的激活时刻 `T`，P2 DES）。
    /// 与 `sequence` 组成跨步全序 `(timestamp, sequence)`。Phase 0 仅为纯数据字段，
    /// `build_events` 尚未写入（`#[serde(default)]` 兜底为 0）。
    #[serde(default)]
    pub timestamp: i64,
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
    /// 本行动耗时（游戏时间单位，P2 DES）。模型输出，`role_decide` 缺省填 `DEFAULT_DURATION`、
    /// clamp 到 `[MIN_DURATION, MAX_DURATION]`。Phase 0 仅为纯数据字段，`run_round` 忽略之。
    #[serde(default)]
    pub duration: i64,
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

// ---------- Phase 0（P2 DES）序列化 round-trip 测试 ----------
// 目标：新增字段（timeline / duration / timestamp）序列化 round-trip 保真，且 `#[serde(default)]`
// 保证「老存档」（无这些字段的 JSON）能反序列化并兜底为空/0，零行为变化。

#[cfg(test)]
mod phase0_timeline_serde_tests {
    use super::*;

    #[test]
    fn timeline_layer_round_trip() {
        let mut next_time = BTreeMap::new();
        next_time.insert("li".to_string(), 300i64);
        next_time.insert("zhang".to_string(), 150i64);
        let layer = TimelineLayer {
            now: 100,
            next_time,
            time_cap: Some(10_000),
            schema_version: 1,
        };
        let json = serde_json::to_string(&layer).unwrap();
        let back: TimelineLayer = serde_json::from_str(&json).unwrap();
        assert_eq!(layer, back);
        // camelCase 键名确认
        assert!(json.contains("\"nextTime\""));
        assert!(json.contains("\"timeCap\""));
        assert!(json.contains("\"schemaVersion\""));
    }

    #[test]
    fn timeline_layer_defaults_from_empty_json() {
        // 老存档：完全缺省 → 全部兜底
        let layer: TimelineLayer = serde_json::from_str("{}").unwrap();
        assert_eq!(layer, TimelineLayer::default());
        assert_eq!(layer.now, 0);
        assert!(layer.next_time.is_empty());
        assert_eq!(layer.time_cap, None);
        assert_eq!(layer.schema_version, 0);
    }

    #[test]
    fn narrative_state_old_archive_deserializes_with_empty_timeline() {
        // 模拟 Phase 0 之前落盘的 NarrativeState：无 timeline 字段。
        let old_json = r#"{
            "schemaVersion": 1,
            "runId": "run-1",
            "revision": 7,
            "characters": { "li": { "goals": ["活下去"] } }
        }"#;
        let state: NarrativeState = serde_json::from_str(old_json).unwrap();
        assert_eq!(state.run_id, "run-1");
        assert_eq!(state.revision, 7);
        // timeline 兜底为空，不影响任何既有层
        assert_eq!(state.timeline, TimelineLayer::default());
        assert!(state.timeline.next_time.is_empty());
        assert_eq!(state.characters.get("li").unwrap().goals, vec!["活下去"]);
    }

    #[test]
    fn narrative_state_round_trip_with_timeline() {
        let mut state = NarrativeState {
            schema_version: 1,
            run_id: "run-2".to_string(),
            revision: 3,
            ..Default::default()
        };
        state.timeline.now = 500;
        state.timeline.next_time.insert("a".to_string(), 700);
        state.timeline.time_cap = Some(9_000);
        state.timeline.schema_version = 1;
        let json = serde_json::to_string(&state).unwrap();
        let back: NarrativeState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timeline, state.timeline);
        assert_eq!(back.revision, 3);
        assert!(json.contains("\"timeline\""));
    }

    #[test]
    fn role_decision_old_archive_defaults_duration_zero() {
        // 老决策 JSON：无 duration → 兜底 0（Phase 0 被忽略，Phase 1 role_decide 才补齐/clamp）
        let old_json = r#"{
            "characterId": "li",
            "intent": "试探",
            "action": "发问",
            "speak": { "willSpeak": true, "purpose": "刺探" },
            "targets": ["zhang"]
        }"#;
        let dec: RoleDecision = serde_json::from_str(old_json).unwrap();
        assert_eq!(dec.duration, 0);
        assert_eq!(dec.character_id, "li");
        assert_eq!(dec.targets, vec!["zhang"]);
    }

    #[test]
    fn role_decision_round_trip_with_duration() {
        let dec = RoleDecision {
            decision_id: "dec:run-1:li".to_string(),
            character_id: "li".to_string(),
            intent: "行动".to_string(),
            action: "移动".to_string(),
            speak: SpeakIntent {
                will_speak: false,
                purpose: String::new(),
            },
            targets: vec![],
            acceptable_costs: vec![],
            predictions: vec![],
            duration: 240,
        };
        let json = serde_json::to_string(&dec).unwrap();
        let back: RoleDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back.duration, 240);
        assert!(json.contains("\"duration\""));
    }

    #[test]
    fn domain_event_old_archive_defaults_timestamp_zero() {
        // 老事件 JSON：无 timestamp → 兜底 0
        let old_json = r#"{
            "schemaVersion": 1,
            "id": "ev-1",
            "runId": "run-1",
            "sequence": 2,
            "type": "dialogue_spoken",
            "actorIds": ["li"],
            "fact": {},
            "statePatchId": "patch-1",
            "visibility": { "scope": "public" }
        }"#;
        let ev: DomainEvent = serde_json::from_str(old_json).unwrap();
        assert_eq!(ev.timestamp, 0);
        assert_eq!(ev.sequence, 2);
        assert_eq!(ev.event_type, DomainEventType::DialogueSpoken);
    }

    #[test]
    fn domain_event_round_trip_with_timestamp() {
        let ev = DomainEvent {
            schema_version: 1,
            id: "ev-2".to_string(),
            run_id: "run-1".to_string(),
            sequence: 5,
            timestamp: 1_234,
            event_type: DomainEventType::ActionResolved,
            actor_ids: vec!["li".to_string()],
            target_ids: None,
            fact: serde_json::json!({}),
            state_patch_id: "patch-2".to_string(),
            caused_by: vec![],
            visibility: EventVisibility::Public,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: DomainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timestamp, 1_234);
        assert!(json.contains("\"timestamp\""));
    }
}
