//! P3 Phase4 世界提取数据模型：世界提取任务 + 逐章世界实体发现 + 世界内容超集草稿。
//!
//! 复用 character 侧共享类型（`SourceFingerprint`/`ChapterEntry`/`MentionEvidence`/`RosterEntry`），
//! 对齐 server 侧 `assembly::Skeleton` 的 camelCase 字段名——`WorldSkeletonDraft` 序列化即可直接进
//! `world_templates.skeleton_json` 被装配侧消费（P3 装配路径 `serde(default)` 语义安全忽略超集元数据字段）。

use serde::{Deserialize, Serialize};

use crate::character::types::{ChapterEntry, MentionEvidence, RosterEntry, SourceFingerprint};
use crate::narrative::types::LocationGate;

/// 官方体系枚举白名单（与 server `admission::KNOWN_COSMOLOGIES` 逐字一致；引擎侧合成期夹回/丢弃越界体系）。
pub const KNOWN_COSMOLOGIES: &[&str] = &["magic", "tech", "cultivation", "mundane", "psychic", "myth"];

/// 世界实体类别判别（scan mention 携带；未知类别在 sanitize 阶段丢弃）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorldEntityKind {
    Character,
    Location,
    Item,
    PlotBeat,
    EndingClue,
}

impl WorldEntityKind {
    /// 由原始字符串解析（模型可能返回任意 kind；未知 → None，sanitize 据此丢弃）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "character" => Some(Self::Character),
            "location" => Some(Self::Location),
            "item" => Some(Self::Item),
            "plotBeat" => Some(Self::PlotBeat),
            "endingClue" => Some(Self::EndingClue),
            _ => None,
        }
    }
}

// ---------- 提取任务模型 ----------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorldStage {
    Scan,
    Merge,
    Tiering,
    Review,
    Synthesis,
    Assembled,
    Done,
    Cancelled,
}

/// location/item 归并条目（对齐 `RosterEntry` 但实体无角色分层语义）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldRosterEntry {
    /// stable_key（复用 `character::merge::stable_key`）。
    pub key: String,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub merged_from: Vec<String>,
    pub user_confirmed: bool,
    /// location：秘境标记（scan hint 提取；item 恒 false）。
    #[serde(default)]
    pub is_secret_realm: bool,
}

/// 全书级剧情节拍草稿（merge 阶段暂存，Review 后合成为 mainline/hidden）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlotBeatDraft {
    pub surface: String,
    pub chapter_index: u32,
    /// 前序节拍提示（连线剧情线用）。
    #[serde(default)]
    pub links: Vec<String>,
    /// 张力/定位提示（来自 mention role_hint）。
    #[serde(default)]
    pub tension: String,
    /// 隐藏任务标记（role_hint 含「隐藏」）。
    #[serde(default)]
    pub is_hidden: bool,
}

/// 全书级结局线索草稿。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndingClueDraft {
    pub surface: String,
    /// 结局倾向：strategist|combat|social。
    #[serde(default)]
    pub affinity_hint: String,
    pub chapter_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldExtractionTask {
    pub schema_version: u32, // 恒为 1
    pub task_id: String,
    pub work_title: String,
    pub source_path: String,
    pub source_fingerprint: SourceFingerprint,
    pub pipeline_version: String,
    pub chapters: Vec<ChapterEntry>,
    /// 四条平行 roster：character 复用 `RosterEntry`（带 tier/dna_status）；location/item 用 `WorldRosterEntry`。
    #[serde(default)]
    pub character_roster: Vec<RosterEntry>,
    #[serde(default)]
    pub location_roster: Vec<WorldRosterEntry>,
    #[serde(default)]
    pub item_roster: Vec<WorldRosterEntry>,
    /// plot/ending 是全书级派生，Review 前才产；确认后合成。
    #[serde(default)]
    pub plot_beats: Vec<PlotBeatDraft>,
    #[serde(default)]
    pub ending_clues: Vec<EndingClueDraft>,
    pub stage: WorldStage,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------- 章节扫描产物 ----------

/// 单章世界实体发现（模型输出，白名单校验后落分片）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldChapterDiscovery {
    pub chapter_index: u32,
    #[serde(default)]
    pub mentions: Vec<WorldMention>,
}

/// 世界实体 mention：character mention 结构相同 + kind 判别 + 关系/体系提示。
/// `kind` 存原始字符串以容忍未知类别（sanitize 丢弃不可解析者）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldMention {
    pub kind: String,
    pub surface: String,
    #[serde(default)]
    pub role_hint: String,
    /// character 复用；location→连通提示；item→cosmology/tier 提示；plotBeat→前序节点提示。
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<MentionEvidence>,
}

// ---------- 世界内容超集草稿（对齐 server `assembly::Skeleton` camelCase） ----------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkeletonSourceDraft {
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub title: String,
}

/// 道具来源（对齐 server `admission::ItemOrigin`）。`world_template_id` 提取期为空，发布/装配期钉入。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ItemOriginDraft {
    #[serde(default)]
    pub world_template_id: String,
    #[serde(default)]
    pub cosmology: Vec<String>,
    #[serde(default)]
    pub power_tier: u8, // 1–5
}

/// 道具目录条目（对齐 server `admission::ItemDefinition`）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ItemDraft {
    pub id: String,
    #[serde(default)]
    pub narrative: String,
    #[serde(default)]
    pub effect_tags: Vec<String>,
    #[serde(default)]
    pub origin: ItemOriginDraft,
}

/// 地点条目（对齐 server `assembly::LocationSpec` = 引擎 `LocationDef` + residentItemIds）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocationDraft {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub connections: Vec<String>,
    #[serde(default)]
    pub is_secret_realm: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<LocationGate>,
    /// 驻留道具对 worldItems 目录的引用（装配时解引用）。
    #[serde(default)]
    pub resident_item_ids: Vec<String>,
}

/// 世界固有角色（NPC/反派）条目（对齐 server `assembly::WorldCharacter`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldCharacterDraft {
    /// 复用引擎 DNA 卡（NPC 与玩家角色同构）。
    pub card: crate::character::types::CharacterCardV2,
    #[serde(default)]
    pub home_location: String,
    #[serde(default)]
    pub carried_item_ids: Vec<String>,
    /// 反派主动议程绑定的 mainline 节点 id（透传标注）。
    #[serde(default)]
    pub agenda_nodes: Vec<String>,
}

/// 主线段（对齐 server `assembly::MainlineNode` + 超集采样元数据）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MainlineNodeDraft {
    pub id: String,
    #[serde(default)]
    pub fated: bool,
    /// 同组互斥（采样每组至多取一）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_group: Option<String>,
    /// 所属剧情线。
    #[serde(default)]
    pub arc_tags: Vec<String>,
}

/// 内容池条目（对齐 server `assembly::PoolItem` + 超集采样元数据）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PoolItemDraft {
    pub id: String,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub template: String,
    #[serde(default = "half")]
    pub difficulty_base: f32,
    /// 通关兑现的隐藏道具对 worldItems 目录的引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_item_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_group: Option<String>,
    #[serde(default)]
    pub arc_tags: Vec<String>,
}

/// 结局候选（对齐 server `assembly::EndingCandidate` + 超集采样元数据）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EndingCandidateDraft {
    pub id: String,
    /// strategist / combat / social / None（无条件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<String>,
    #[serde(default = "one")]
    pub base_weight: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_group: Option<String>,
    #[serde(default)]
    pub arc_tags: Vec<String>,
}

// ---------- 超集元数据（§防刷 ①） ----------

/// 剧情线分组：主线段/隐藏任务归属的可选弧（互斥弧：采样器每实例选子集）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Storyline {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub mainline_node_ids: Vec<String>,
    #[serde(default)]
    pub hidden_pool_ids: Vec<String>,
    #[serde(default)]
    pub ending_ids: Vec<String>,
    /// strategist|combat|social（阵容依赖采样用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<String>,
}

/// 采样提示：建议每副本抽样量 + 冗余倍率标注（建模板期校验 ≥ 目标下限）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SamplingHints {
    pub instance_mainline_count: usize,
    pub instance_hidden_count: usize,
    pub instance_npc_count: usize,
    pub instance_location_count: usize,
    /// 超集量 ÷ 单副本量（≥ 目标下限，如 ≥3.0，才够采出内容不同的多副本）。
    pub redundancy_ratio: f32,
}

/// 提取管线最终产物：世界内容超集。字段名严格对齐 server `assembly::Skeleton`（camelCase）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorldSkeletonDraft {
    #[serde(default)]
    pub source_work: SkeletonSourceDraft,
    #[serde(default)]
    pub world_characters: Vec<WorldCharacterDraft>,
    #[serde(default)]
    pub locations: Vec<LocationDraft>,
    #[serde(default)]
    pub world_items: Vec<ItemDraft>,
    #[serde(default)]
    pub mainline_nodes: Vec<MainlineNodeDraft>,
    #[serde(default)]
    pub hidden_content_pool: Vec<PoolItemDraft>,
    #[serde(default)]
    pub side_hook_pool: Vec<PoolItemDraft>,
    #[serde(default)]
    pub ending_pool: Vec<EndingCandidateDraft>,
    // ---- 超集元数据（装配 P3 忽略未知字段） ----
    #[serde(default)]
    pub storylines: Vec<Storyline>,
    #[serde(default)]
    pub sampling: SamplingHints,
    /// 恒 true：标注为内容池而非单副本，使下游可识别「须采样，不可整体投放」。
    #[serde(default = "yes")]
    pub is_superset: bool,
}

fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}
fn yes() -> bool {
    true
}
