//! 四类蒸馏调用组装（规格 §4.2/§9.4）。文件所有权：agent-E2。

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{Chunk, Distilled, PackMode};
use super::DistillPrompts;

/// 采样上限：块数与总字符。
const MAX_SAMPLE_CHUNKS: usize = 30;
const MAX_SAMPLE_CHARS: usize = 40_000;

/// 采样策略：均匀取 ≤ 30 块、总长 ≤ 40k 字符；mind→decisionHeuristics 必填，
/// value→principles 必填，expression→expressionRules 必填；缺失视为 ModelOutput 错误触发重试。
pub async fn distill_from_chunks(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &DistillPrompts,
    run_id: &str,
    mode: PackMode,
    chunks: &[Chunk],
    cancel: &CancelFlag,
) -> Result<Distilled, EngineError> {
    let sampled = sample_chunks(chunks);
    if sampled.is_empty() {
        return Err(EngineError::Validation("无可用切块，无法蒸馏".into()));
    }

    let key = mode_key(mode);
    let system = prompts.system_by_mode.get(key).cloned().unwrap_or_default();
    let mut user = String::from("以下是资料片段，请据此完成蒸馏并输出严格 JSON：\n\n");
    for (i, c) in sampled.iter().enumerate() {
        user.push_str(&format!("【片段{}】\n{}\n\n", i + 1, c.text));
    }

    let spec = ModelCallSpec {
        profile: profile.clone(),
        system,
        user,
        temperature: 0.0, // 抽取类固定 temperature=0（§8.2）
        max_output_tokens: 2048,
        agent: format!("distill:{key}"),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };

    // 缺必填字段视为 ModelOutput 错误并重试一次（json_call 内部已含解析级重试）。
    for _ in 0..2 {
        let distilled: Distilled =
            json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
        if required_present(&distilled, mode) {
            return Ok(distilled);
        }
    }
    Err(EngineError::ModelOutput(format!("蒸馏输出缺少 {key} 模式必填字段")))
}

/// PackMode → serde 名（用于 prompt 路由与观测标签）。
fn mode_key(mode: PackMode) -> &'static str {
    match mode {
        PackMode::Knowledge => "knowledge",
        PackMode::Mind => "mind",
        PackMode::Value => "value",
        PackMode::Expression => "expression",
    }
}

/// 各模式必填字段校验。
fn required_present(d: &Distilled, mode: PackMode) -> bool {
    match mode {
        PackMode::Mind => d.decision_heuristics.as_ref().is_some_and(|v| !v.is_empty()),
        PackMode::Value => !d.principles.is_empty(),
        PackMode::Expression => d.expression_rules.as_ref().is_some_and(|v| !v.is_empty()),
        PackMode::Knowledge => true, // knowledge 模式不走蒸馏
    }
}

/// 均匀采样：块数 ≤ MAX_SAMPLE_CHUNKS，累计字符 ≤ MAX_SAMPLE_CHARS。
fn sample_chunks(chunks: &[Chunk]) -> Vec<&Chunk> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let n = chunks.len().min(MAX_SAMPLE_CHUNKS);
    let stride = chunks.len() as f64 / n as f64;
    let mut out: Vec<&Chunk> = Vec::new();
    let mut total = 0usize;
    let mut last: Option<usize> = None;
    for i in 0..n {
        let idx = ((i as f64 * stride) as usize).min(chunks.len() - 1);
        if Some(idx) == last {
            continue;
        }
        let c = &chunks[idx];
        let len = c.text.chars().count();
        if total + len > MAX_SAMPLE_CHARS && !out.is_empty() {
            break;
        }
        out.push(c);
        total += len;
        last = Some(idx);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(ord: u32, text: &str) -> Chunk {
        Chunk {
            id: format!("pk#{ord}"),
            pack_id: "pk".into(),
            ordinal: ord,
            text: text.to_string(),
            heading: None,
            char_range: (0, text.chars().count()),
        }
    }

    #[test]
    fn sampling_caps_chunk_count() {
        let chunks: Vec<Chunk> = (0..100).map(|i| ch(i, "短块")).collect();
        let picked = sample_chunks(&chunks);
        assert_eq!(picked.len(), MAX_SAMPLE_CHUNKS);
        // 均匀分布：首尾都被覆盖到。
        assert_eq!(picked.first().unwrap().ordinal, 0);
    }

    #[test]
    fn sampling_caps_total_chars() {
        // 每块 5000 字，30 块共 150k 远超 40k → 被字符预算截断。
        let big = "字".repeat(5000);
        let chunks: Vec<Chunk> = (0..30).map(|i| ch(i, &big)).collect();
        let picked = sample_chunks(&chunks);
        let total: usize = picked.iter().map(|c| c.text.chars().count()).sum();
        assert!(total <= MAX_SAMPLE_CHARS + 5000); // 最多超出一块
        assert!(picked.len() < 30);
    }

    #[test]
    fn required_field_matrix() {
        let empty = Distilled::default();
        assert!(!required_present(&empty, PackMode::Mind));
        assert!(!required_present(&empty, PackMode::Value));
        assert!(!required_present(&empty, PackMode::Expression));
        assert!(required_present(&empty, PackMode::Knowledge));

        let mut d = Distilled::default();
        d.principles.push("原则".into());
        assert!(required_present(&d, PackMode::Value));
    }
}
