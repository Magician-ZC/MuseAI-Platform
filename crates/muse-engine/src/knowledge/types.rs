//! P1 数据模型：知识包 / 绑定 / 切块 / 检索结果（规格 §9.4 镜像，serde camelCase）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RightsBasis {
    Owned,
    Licensed,
    PublicDomain,
    PersonalUse,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AllowedUse {
    Extract,
    Retrieve,
    Generate,
    SendToRemoteModel,
    Publish,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    ReferenceOriginal,
    ManagedCopy,
    IndexOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PackMode {
    Knowledge,
    Mind,
    Value,
    Expression,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSource {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub content_hash: String,
    pub rights_basis: RightsBasis,
    pub allowed_uses: Vec<AllowedUse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_attested_at: Option<i64>,
    pub retention: Retention,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Distilled {
    #[serde(default)]
    pub principles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_heuristics: Option<Vec<Heuristic>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_standards: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_rules: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heuristic {
    pub when: String,
    pub prefer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avoid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgePack {
    pub schema_version: u32, // 1
    pub id: String,
    pub title: String,
    pub source: PackSource,
    pub mode: PackMode,
    pub distilled: Distilled,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_boundary: Option<String>,
    /// 内部受控 key（相对 data_root），不接受任意路径
    pub chunk_index_store_key: String,
    /// = sourceHash + chunkerVersion（+ embeddingModel 预留），任何一项变化即重建
    pub index_version: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeBinding {
    pub id: String,
    pub pack_id: String,
    pub character_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story_id: Option<String>,
    /// 0.0–1.0 影响强度
    pub influence: f32,
    pub enabled: bool,
    pub conflict_policy: ConflictPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    CharacterCoreWins,
    AskUser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    pub id: String,
    pub pack_id: String,
    pub ordinal: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    pub char_range: (usize, usize),
}

/// 倒排索引文件（`knowledge/index/<packId>.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkIndex {
    pub schema_version: u32, // 1
    pub pack_id: String,
    pub index_version: String,
    pub chunker_version: String,
    pub chunks: Vec<Chunk>,
    /// term -> chunk ordinal 列表（构建时中文按 2-gram + 英文按词切分）
    pub postings: std::collections::BTreeMap<String, Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievedFragment {
    pub pack_id: String,
    pub pack_title: String,
    pub chunk_id: String,
    pub ordinal: u32,
    pub text: String,
    pub score: f32,
}

/// 使用记录（规格 §4.3.1：每轮引用 100% 可追踪）。存 `knowledge/usage/<runId>.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLogEntry {
    pub run_id: String,
    pub scene_id: String,
    pub character_id: String,
    pub fragments: Vec<UsageFragmentRef>,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageFragmentRef {
    pub pack_id: String,
    pub chunk_id: String,
    pub ordinal: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkStats {
    pub chunk_count: u32,
    pub total_chars: usize,
}
