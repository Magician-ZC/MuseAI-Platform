use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
pub struct FileNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Option<Vec<FileNode>>,
}
#[derive(Serialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
}
#[derive(Serialize)]
pub struct BashToolResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub id: String,
    pub timestamp: u64,
    pub ai_score: Option<u32>,
    pub suggestion: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVersionsMetadata {
    pub versions: Vec<VersionInfo>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct TodoItem {
    pub content: String,
    pub active_form: String,
    pub status: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: Option<String>,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<ChatToolCall>>,
    pub thinking_blocks: Option<Vec<Value>>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStreamRequest {
    pub agent_id: Option<String>,
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub compaction_turn_threshold: Option<u32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub top_p: Option<f32>,
    pub thinking_depth: Option<String>,
    pub system_prompt: String,
    pub workspace_path: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub context_compaction: Option<SessionContextCompaction>,
    pub selected_reference_files: Option<Vec<String>>,
    pub allowed_tools: Option<Vec<String>>,
    pub allowed_write_paths: Option<Vec<String>>,
    pub role_play_context: Option<RolePlayContext>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RolePlayContext {
    pub chat_system_prompt: String,
    pub world_book_content: Option<String>,
    pub user_info: Option<Value>,
    pub character_cards: Vec<RolePlayCharacterCard>,
}
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RolePlayCharacterCard {
    pub id: String,
    pub name: String,
    pub content: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub text: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStreamEvent {
    pub run_id: String,
    pub event_type: String,
    pub delta: Option<String>,
    pub message: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_status: Option<String>,
    pub tool_arguments: Option<String>,
    pub todos: Option<Vec<AgentSessionTodo>>,
    pub context_compaction: Option<SessionContextCompaction>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionTool {
    pub id: Option<String>,
    pub name: String,
    pub result: String,
    pub status: Option<String>,
    pub arguments: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub thinking: Option<String>,
    pub tools: Option<Vec<AgentSessionTool>>,
    pub thinking_blocks: Option<Vec<Value>>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionTodo {
    pub content: String,
    pub status: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRecord {
    pub id: String,
    pub title: String,
    pub saved_at: u64,
    pub session_kind: Option<String>,
    pub messages: Vec<AgentSessionMessage>,
    pub selected_reference_files: Vec<String>,
    pub selected_outline_file: Option<String>,
    pub todos: Vec<AgentSessionTodo>,
    pub context_compaction: Option<SessionContextCompaction>,
    pub is_archived: Option<bool>,
    pub character_card_id: Option<String>,
    pub character_card_ids: Option<Vec<String>>,
    pub selected_world_book_id: Option<String>,
    pub dynamic_role_loading_enabled: Option<bool>,
    pub selected_style_preset_ids: Option<Vec<String>>,
    pub initial_style_preset_ids: Option<Vec<String>>,
    pub initial_system_prompt_snapshot: Option<String>,
    pub book_travel_state: Option<Value>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionSummary {
    pub id: String,
    pub title: String,
    pub saved_at: u64,
    pub session_kind: Option<String>,
    pub character_card_id: Option<String>,
    pub character_card_ids: Option<Vec<String>>,
    pub selected_world_book_id: Option<String>,
    pub dynamic_role_loading_enabled: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextCompaction {
    pub summary: String,
    pub compacted_through_message_id: Option<String>,
    pub compacted_through_index: usize,
    pub source_message_count: usize,
    pub updated_at: u64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeMemoryRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub chat_history: String,
    pub target_character_name: Option<String>,
    pub target_character_content: Option<String>,
    pub current_user_relation_type: String,
    pub current_user_interaction_model: String,
    pub current_user_relation_bottom_line: String,
    pub current_events: String,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertCharacterCardToSillyTavernRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub source_character_card: Value,
    pub world_book_entries: Option<Value>,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateBackgroundItemsRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub text: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateBackgroundStageOneRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub text: String,
    pub include_character_names: bool,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub system_prompt: Option<String>,
    pub task_id: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateBackgroundCharacterCardRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub text: String,
    pub character_name: String,
    pub world_book_context: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub system_prompt: Option<String>,
    pub task_id: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedBackgroundItem {
    pub name: String,
    pub fields: Value,
}

// ---------- 渐进式捏人（总规格 §7【拍板 21】）----------
// 三句话开卡（名字 + 执念 + 底线）→ AI 展开 18 字段草稿。
// 纯本地功能：模型凭据由前端传入（用户自己的 Key），不依赖任何平台账号。

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickCharacterDraftRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 灵魂三句之一：名字
    pub name: String,
    /// 灵魂三句之一：执念（他放不下的东西）
    pub obsession: String,
    /// 灵魂三句之一：底线（他绝不肯做的事）
    pub bottom_line: String,
    /// 人格变奏种子：同样三句话 + 不同种子 → 不同的展开。缺省由后端按时间生成。
    pub variation_seed: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub system_prompt: Option<String>,
    pub task_id: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuickDecisionRule {
    pub when: String,
    pub then: String,
    pub because: String,
}

/// AI 展开出的 15 个字段（另有 3 个字段是用户原话，前端直接落卡，不经模型）。
/// 字段名与 CharacterCardV2 的层内字段同名，前端按固定映射表拼进 createEmptyCardV2 的产物。
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuickCharacterDraftFields {
    pub narrative_role: String,
    pub core_contradiction: String,
    pub surface_goal: String,
    pub hidden_need: String,
    pub core_fear: String,
    pub stakes: String,
    pub value_priorities: Vec<String>,
    pub risk_appetite: String,
    pub decision_rules: Vec<QuickDecisionRule>,
    pub attribution_style: String,
    pub triggers: Vec<String>,
    pub trust_building: String,
    pub sentence_rhythm: String,
    pub plot_seeds: Vec<String>,
    pub immutable_core: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuickCharacterDraftResponse {
    pub fields: QuickCharacterDraftFields,
    /// 本次实际生效的变奏种子（回传以便「换一种变奏」与复现）
    pub variation_seed: String,
    /// 本次选中的变奏轴人类可读描述（UI 展示「这一版偏向什么」）
    pub variation_axes: Vec<String>,
    /// 同源指纹：三句话归一化后的哈希。同样三句话 → 同样指纹（防同质的比对锚点）。
    pub source_fingerprint: String,
    /// 模型没能填上的字段（camelCase 字段名），前端提示「这几项得你自己写」
    pub missing_fields: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundStageOneResponse {
    pub world_books: Vec<GeneratedBackgroundItem>,
    pub character_names: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeCharacterMemoriesRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub text: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineAnalysisRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub article_type: String,
    pub file_paths: Vec<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub system_prompt: Option<String>,
    pub concurrency: Option<u32>,
    pub short_config: Option<ReverseOutlineStageConfig>,
    pub long_summary_config: Option<ReverseOutlineStageConfig>,
    pub long_final_config: Option<ReverseOutlineStageConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineStageConfig {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineSaveRequest {
    pub title: String,
    pub content: String,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineChapterPreview {
    pub title: String,
    pub path: String,
    pub char_count: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineAnalysisStarted {
    pub run_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineProgressEvent {
    pub run_id: String,
    pub phase: String,
    pub total_chapters: usize,
    pub success_chapters: usize,
    pub failed_chapters: usize,
    pub message: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineResultEvent {
    pub run_id: String,
    pub title: Option<String>,
    pub content: Option<String>,
    pub error: Option<String>,
    pub failed_batch_indices: Option<Vec<usize>>,
    pub failed_batch_errors: Option<Vec<ReverseOutlineBatchError>>,
    pub partial_summaries: Option<Vec<Value>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineStreamEvent {
    pub run_id: String,
    pub delta: String,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineBatchError {
    pub index: usize,
    pub range: String,
    pub error: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineRetryRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub file_paths: Vec<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub thinking_depth: Option<String>,
    pub system_prompt: Option<String>,
    pub concurrency: Option<u32>,
    pub long_summary_config: Option<ReverseOutlineStageConfig>,
    pub long_final_config: Option<ReverseOutlineStageConfig>,
    pub failed_batch_indices: Vec<usize>,
    pub partial_summaries: Vec<Value>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundExtractionStarted {
    pub run_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundExtractionEvent {
    pub run_id: String,
    pub event_type: String,
    pub delta: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelStreamStarted {
    pub run_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelStreamEvent {
    pub run_id: String,
    pub event_type: String,
    pub delta: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineSaveResult {
    pub path: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashPermissionRequestPayload {
    pub request_id: String,
    pub command: String,
}

#[derive(Clone)]
pub struct AgentRunOptions {
    pub max_tool_rounds: usize,
    pub emit_events: bool,
    pub emit_todo_updates: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub excluded_tools: Vec<String>,
    pub parent_tool_call_id: Option<String>,
}

impl AgentRunOptions {
    pub fn parent() -> Self {
        Self {
            max_tool_rounds: MAX_AGENT_TOOL_ROUNDS,
            emit_events: true,
            emit_todo_updates: true,
            allowed_tools: None,
            excluded_tools: vec![],
            parent_tool_call_id: None,
        }
    }

    /// 子代理的运行选项。
    ///
    /// 🔴 **必须继承父级的 `allowed_tools`**。此前这里写死 `None`（= 全部工具放行），
    /// 于是父级的能力限制**不被继承**：写作 agent 的白名单是
    /// `[read, write, edit, grep, glob, skill, subagent, todo]`——**刻意不含 `bash`**——
    /// 但它可以调 `subagent`，而子代理拿到的是「全部工具」，其中就有 `bash`。
    ///
    /// 也就是说「不给这个模式 bash」这条限制，被一次 `subagent` 调用就绕过了。
    /// 而子代理的任务文本来自父模型的一次工具调用——被读入的文档里的提示注入可以指定它。
    ///
    /// 语义保持不变的那一半：父级 `None`（不限制）→ 子级仍 `None`，既有行为逐字不变。
    /// `excluded_tools` 仍钉着 `subagent`，子代理不得再派生子代理（防递归）。
    pub fn subagent(
        parent_tool_call_id: Option<String>,
        inherited_allowed_tools: Option<Vec<String>>,
    ) -> Self {
        Self {
            max_tool_rounds: MAX_SUBAGENT_TOOL_ROUNDS,
            emit_events: parent_tool_call_id.is_some(),
            emit_todo_updates: false,
            allowed_tools: inherited_allowed_tools,
            excluded_tools: vec!["subagent".to_string()],
            parent_tool_call_id,
        }
    }

    pub fn allows_tool(&self, name: &str) -> bool {
        if let Some(allowed) = &self.allowed_tools {
            if !allowed.iter().any(|s| s == name) {
                return false;
            }
        }
        !self.excluded_tools.iter().any(|s| s == name)
    }
}
#[derive(Clone)]
pub struct AgentToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}
#[derive(Default)]
pub struct OpenAiRoundResult {
    pub content: String,
    pub tool_calls: Vec<AgentToolCall>,
}
#[derive(Default)]
pub struct AnthropicRoundResult {
    pub content: String,
    pub tool_calls: Vec<AgentToolCall>,
    pub thinking_blocks: Vec<Value>,
}
#[derive(Clone, Default)]
pub struct AgentToolCall {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub arguments: String,
}
pub struct AgentToolExecution {
    pub success: bool,
    pub model_output: String,
}
pub struct OpenAiStreamEvent {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_call_chunks: Vec<OpenAiToolCallChunk>,
    pub finish_reason: Option<String>,
}
pub struct OpenAiToolCallChunk {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}
pub enum AnthropicStreamEvent {
    Text(String),
    MessageDelta {
        stop_reason: Option<String>,
    },
    ThinkingStart {
        index: usize,
    },
    ThinkingDelta {
        index: usize,
        thinking: String,
    },
    ThinkingSignature {
        index: usize,
        signature: String,
    },
    RedactedThinking {
        index: usize,
        data: String,
    },
    ToolStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolInputDelta {
        index: usize,
        partial_json: String,
    },
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub path: std::path::PathBuf,
}

pub const MAX_READ_LINES: usize = 2000;
pub const MAX_SEARCH_RESULTS: usize = 200;
pub const MAX_GLOB_RESULTS: usize = 100;
pub const MAX_BASH_OUTPUT_CHARS: usize = 15_000;
pub const MAX_AGENT_TOOL_OUTPUT_CHARS: usize = 12_000;
pub const MAX_AGENT_TOOL_ROUNDS: usize = 50;
pub const MAX_SUBAGENT_TOOL_ROUNDS: usize = 50;
pub const MAX_SUBAGENT_OUTPUT_CHARS: usize = 5_000;
pub const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
pub const MAX_BASH_TIMEOUT_SECS: u64 = 600;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestConnectionRequest {
    pub model_interface: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyActivity {
    pub date: String,
    pub count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WritingStats {
    pub total_works: usize,
    pub total_word_count: usize,
    pub daily_activity: Vec<DailyActivity>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionStats {
    pub world_count: usize,
    pub character_count: usize,
    pub chat_count: usize,
    pub adventure_count: usize,
    pub book_travel_count: usize,
    pub daily_activity: Vec<DailyActivity>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_run_options_parent_allows_all_tools() {
        let opts = AgentRunOptions::parent();
        assert!(opts.allows_tool("read"));
        assert!(opts.allows_tool("write"));
        assert!(opts.allows_tool("bash"));
        assert!(opts.allows_tool("subagent"));
    }

    #[test]
    fn test_agent_run_options_subagent_excludes_subagent() {
        let opts = AgentRunOptions::subagent(None, None);
        assert!(opts.allows_tool("read"));
        assert!(opts.allows_tool("write"));
        assert!(!opts.allows_tool("subagent"));
    }

    /// 🔴 **子代理必须继承父级的工具白名单**，否则一次 `subagent` 调用就绕过了整套限制。
    ///
    /// 用写作 agent 的**真实**白名单演一遍：它是
    /// `[read, write, edit, grep, glob, skill, subagent, todo]`——**刻意不含 `bash`**，
    /// 却含 `subagent`。此前 `AgentRunOptions::subagent` 写死 `allowed_tools: None`（全部放行），
    /// 于是父级不能用 bash、子代理可以。而子代理的任务文本来自父模型的一次工具调用
    /// ——被读入文档里的提示注入可以指定它。
    #[test]
    fn subagent_inherits_the_parent_tool_whitelist() {
        // 写作 agent（`AgentChat.tsx`）实际下发的白名单，逐字照抄。
        let parent_allowed: Vec<String> = ["read", "write", "edit", "grep", "glob", "skill", "subagent", "todo"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let child = AgentRunOptions::subagent(Some("call_1".into()), Some(parent_allowed.clone()));

        assert!(
            !child.allows_tool("bash"),
            "🔴 父级没有 bash，子代理却拿到了 —— 一次 subagent 调用绕过整套白名单"
        );
        for t in ["read", "write", "edit", "grep", "glob", "skill", "todo"] {
            assert!(child.allows_tool(t), "父级允许的工具，子代理应当照常可用：{t}");
        }
        assert!(
            !child.allows_tool("subagent"),
            "🔴 子代理不得再派生子代理（防递归），这一条不因继承而放宽"
        );
    }

    /// 🔴 **接线红线**：光有继承参数没用，派生点必须真的把父级白名单传进去。
    ///
    /// 改动前的形态是「函数签名里根本没有这个参数」；改动后可能的退化是
    /// 「有参数、但调用点传 `None`」——两者对玩家的后果一模一样，而后者更难看出来。
    /// 故扫源码：`agent/mod.rs` 里每一处 `AgentRunOptions::subagent(` 都必须带上
    /// `child_request.allowed_tools`。
    #[test]
    fn red_line_subagent_call_sites_pass_the_parent_whitelist() {
        let src = include_str!("agent/mod.rs");
        let calls: Vec<usize> = src.match_indices("AgentRunOptions::subagent(").map(|(i, _)| i).collect();
        assert_eq!(calls.len(), 2, "派生点数量变了（当前 {}），请确认每一处都继承了白名单", calls.len());
        for i in calls {
            let window = &src[i..(i + 260).min(src.len())];
            assert!(
                window.contains("child_request.allowed_tools"),
                "🔴 有一处派生子代理时没有传父级白名单 —— 那一处的子代理拿到全部工具：\n{window}"
            );
        }
    }

    /// 不限制的父级（`None`）→ 子级仍不限制：既有行为**逐字不变**。
    #[test]
    fn subagent_of_an_unrestricted_parent_stays_unrestricted() {
        let child = AgentRunOptions::subagent(Some("call_1".into()), None);
        for t in ["read", "write", "bash", "grep"] {
            assert!(child.allows_tool(t), "不限制的父级不该因为本次改动被收紧：{t}");
        }
        assert!(!child.allows_tool("subagent"), "递归仍然禁止");
    }

    #[test]
    fn test_agent_run_options_allowed_tools_filtering() {
        let opts = AgentRunOptions {
            allowed_tools: Some(vec!["read".to_string(), "write".to_string()]),
            ..AgentRunOptions::parent()
        };
        assert!(opts.allows_tool("read"));
        assert!(opts.allows_tool("write"));
        assert!(!opts.allows_tool("bash"));
    }

    #[test]
    fn test_agent_run_options_excluded_tools_filtering() {
        let opts = AgentRunOptions {
            excluded_tools: vec!["bash".to_string()],
            ..AgentRunOptions::parent()
        };
        assert!(opts.allows_tool("read"));
        assert!(!opts.allows_tool("bash"));
    }

    #[test]
    fn test_agent_run_options_allowed_and_excluded_combined() {
        let opts = AgentRunOptions {
            allowed_tools: Some(vec!["read".to_string(), "bash".to_string()]),
            excluded_tools: vec!["bash".to_string()],
            ..AgentRunOptions::parent()
        };
        assert!(opts.allows_tool("read"));
        assert!(!opts.allows_tool("bash"));
        assert!(!opts.allows_tool("write"));
    }

    #[test]
    fn test_reverse_outline_result_event_serializes_new_fields() {
        let event = ReverseOutlineResultEvent {
            run_id: "run-123".to_string(),
            title: None,
            content: None,
            error: Some("部分失败".to_string()),
            failed_batch_indices: Some(vec![1, 3]),
            failed_batch_errors: Some(vec![ReverseOutlineBatchError {
                index: 1,
                range: "11-20".to_string(),
                error: "HTTP 451".to_string(),
            }]),
            partial_summaries: Some(vec![
                serde_json::json!({"batchIndex": 0, "段落序号": "1-10"}),
            ]),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["runId"], "run-123");
        assert_eq!(json["error"], "部分失败");
        assert_eq!(json["failedBatchIndices"], serde_json::json!([1, 3]));
        assert_eq!(json["failedBatchErrors"][0]["range"], "11-20");
        assert_eq!(json["failedBatchErrors"][0]["error"], "HTTP 451");
        assert_eq!(json["partialSummaries"][0]["batchIndex"], 0);
    }

    #[test]
    fn test_reverse_outline_stream_event_serializes_delta() {
        let event = ReverseOutlineStreamEvent {
            run_id: "run-123".to_string(),
            delta: "正在生成".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["runId"], "run-123");
        assert_eq!(json["delta"], "正在生成");
    }

    #[test]
    fn test_reverse_outline_retry_request_deserializes() {
        let json = serde_json::json!({
            "modelInterface": "OpenAI-compatible",
            "baseUrl": "https://api.openai.com/v1",
            "apiKey": "sk-test",
            "model": "gpt-4o",
            "filePaths": ["/tmp/test.md"],
            "failedBatchIndices": [1, 3],
            "partialSummaries": [{"batchIndex": 0, "段落序号": "1-10"}],
        });
        let req: ReverseOutlineRetryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.model_interface, "OpenAI-compatible");
        assert_eq!(req.failed_batch_indices, vec![1, 3]);
        assert_eq!(req.partial_summaries.len(), 1);
    }

    #[test]
    fn test_book_travel_stream_event_serialization() {
        let started = BookTravelStreamStarted {
            run_id: "run-start".to_string(),
        };
        let event = BookTravelStreamEvent {
            run_id: "run-event".to_string(),
            event_type: "delta".to_string(),
            delta: Some("hello".to_string()),
            message: Some("done message".to_string()),
        };
        
        let started_json = serde_json::to_value(&started).unwrap();
        let event_json = serde_json::to_value(&event).unwrap();
        
        assert_eq!(started_json["runId"], "run-start");
        assert_eq!(event_json["runId"], "run-event");
        assert_eq!(event_json["eventType"], "delta");
        assert_eq!(event_json["delta"], "hello");
        assert_eq!(event_json["message"], "done message");
    }
}
