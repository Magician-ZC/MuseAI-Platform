//! 录制差异比较：把两份录制对齐到「**哪一拍 · 哪个角色 · 哪个环节**」再逐字段比。
//!
//! ## 为什么不是一个整体 diff
//!
//! 换模型重跑之后，几乎每个字都不一样。一个整体文本 diff 只能告诉人「变了」，
//! 运营/研发拿到它无从下手。真正能用的粒度是三条同时给出：
//!
//! 1. **哪一拍**（beat）——是开局就崩，还是第三拍以后才走偏；
//! 2. **哪个角色**（character）——是全员漂移，还是只有某一张卡不像它自己了；
//! 3. **哪个环节**（agent：director / roleDecide / arbiter / writer / critic）——
//!    是决策变了，还是只有文笔变了。
//!
//! 再往下一层是**字段级**：`intent` 变了、`targets/0` 从 A 指到 B、`speak/willSpeak`
//! 从 true 翻到 false —— 这几个模型响应字段直接决定管线后续走向，值得单独拎出来。
//!
//! ## ⚠️ 它给的是差异，不是判决
//!
//! 「变了多少」不等于「变差了」。本模块只负责把差异定位到可复查的粒度，
//! **不给任何质量结论**——那需要人评或另建评分口径（VALIDATION §4.1 的另一半，不在这里）。
//!
//! ## 确定性
//!
//! 槽位按 `SlotKey` 的全序（拍 → 角色 → 环节 → 槽内序号）遍历；对象字段按 key 排序遍历；
//! 数组按下标遍历；截断按字符数（不是字节，中文安全）且阈值固定。同一对录制比两次，
//! 输出逐字节相同。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Recording, SlotKey};
use crate::model::extract_json_payload;

/// 比较参数（全部有默认值，且都是**确定性截断**，不影响对齐结果）。
#[derive(Debug, Clone, Copy)]
pub struct DiffOptions {
    /// 每个槽位最多列多少条字段级差异（超出记 `fieldsTruncated`）。
    pub max_field_deltas: usize,
    /// 字段值与文本片段的最大字符数。
    pub max_value_chars: usize,
    /// 是否把「完全一致」的槽位也列进 `slots`（默认否：报告只留变化）。
    pub include_identical: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self { max_field_deltas: 20, max_value_chars: 120, include_identical: false }
    }
}

/// 槽位的主分类。优先级：只在一侧 > 成败翻转 > 响应变化 > 仅 prompt 变化 > 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SlotChange {
    /// 只在基线里有：候选这一跑**少调了**这次（管线调用构成变了）。
    OnlyInBaseline,
    /// 只在候选里有：候选这一跑**多调了**这次。
    OnlyInCandidate,
    /// 一边成功一边失败。
    StatusChanged,
    /// 响应内容变了（换模型时的常态）。
    ResponseChanged,
    /// 响应逐字相同，但 prompt 变了（改了 Prompt 模板却没改变模型输出——值得注意的巧合）。
    PromptChanged,
    Identical,
}

/// 文本差异的紧凑描述（不放全文，放**首个差异点附近**的片段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDelta {
    pub baseline_chars: usize,
    pub candidate_chars: usize,
    /// 首个不同字符的下标（按字符计，中文安全）。
    pub first_diff_char: usize,
    pub baseline_excerpt: String,
    pub candidate_excerpt: String,
}

/// 字段级差异（JSON 指针路径）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDelta {
    /// 形如 `/intent`、`/targets/0`、`/speak/willSpeak`。
    pub path: String,
    /// `None` = 该侧不存在这个字段。
    pub before: Option<String>,
    pub after: Option<String>,
}

/// 一个槽位的差异。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotDelta {
    pub beat: i64,
    pub character: String,
    pub agent: String,
    pub occurrence: u32,
    /// 人读标题：`拍 1 · 角色 shenyan · 环节 roleDecide`。
    pub label: String,
    pub change: SlotChange,
    pub prompt_changed: bool,
    /// 成败翻转时的 `(基线, 候选)` 状态。
    pub status: Option<(String, String)>,
    pub prompt: Option<TextDelta>,
    pub response: Option<TextDelta>,
    /// 两侧响应都能解析成 JSON 时给出的字段级差异（模型响应恒是严格 JSON，故这条通常都在）。
    pub fields: Vec<FieldDelta>,
    pub fields_truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotCounts {
    pub total: usize,
    pub changed: usize,
}

impl SlotCounts {
    fn bump(&mut self, changed: bool) {
        self.total += 1;
        if changed {
            self.changed += 1;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub baseline_calls: usize,
    pub candidate_calls: usize,
    pub identical: usize,
    pub response_changed: usize,
    pub prompt_changed: usize,
    pub status_changed: usize,
    pub only_in_baseline: usize,
    pub only_in_candidate: usize,
    /// 「哪个角色变得最多」——一眼看出是全员漂移还是单卡漂移。
    pub per_character: BTreeMap<String, SlotCounts>,
    /// 「哪个环节变得最多」——决策漂移和文笔漂移是两件事。
    pub per_agent: BTreeMap<String, SlotCounts>,
    /// 「从第几拍开始变」——JSON 里 key 会序列化成字符串，这是 `BTreeMap` 的常规行为。
    pub per_beat: BTreeMap<i64, SlotCounts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideMeta {
    pub recording_id: String,
    pub engine_version: String,
    pub models: Vec<String>,
    pub prompt_versions: Vec<String>,
}

impl SideMeta {
    fn of(r: &Recording) -> Self {
        Self {
            recording_id: r.meta.recording_id.clone(),
            engine_version: r.meta.engine_version.clone(),
            models: r.meta.models.clone(),
            prompt_versions: r.meta.prompt_versions.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingDiff {
    pub baseline: SideMeta,
    pub candidate: SideMeta,
    pub summary: DiffSummary,
    /// 默认只收有差异的槽位，按 `SlotKey` 全序排列。
    pub slots: Vec<SlotDelta>,
}

impl RecordingDiff {
    /// 两份录制在**调用构成与响应内容**上完全一致。
    ///
    /// ⚠️ 这不是「质量没有回归」，只是「这两次跑，模型给出的每一次响应逐字相同」。
    pub fn is_identical(&self) -> bool {
        self.summary.response_changed == 0
            && self.summary.status_changed == 0
            && self.summary.only_in_baseline == 0
            && self.summary.only_in_candidate == 0
    }

    pub fn to_json(&self) -> Result<String, crate::EngineError> {
        serde_json::to_string_pretty(self).map_err(crate::EngineError::serde)
    }

    /// 人读报告。**给人看的那一份**：先给三张分布表（角色 / 环节 / 拍），再逐槽列字段级差异。
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "录制差异：{} [{}] → {} [{}]\n",
            self.baseline.recording_id,
            self.baseline.models.join(","),
            self.candidate.recording_id,
            self.candidate.models.join(","),
        ));
        let s = &self.summary;
        out.push_str(&format!(
            "调用数 {} → {}；一致 {}，响应变化 {}，成败翻转 {}，仅基线有 {}，仅候选有 {}\n",
            s.baseline_calls,
            s.candidate_calls,
            s.identical,
            s.response_changed,
            s.status_changed,
            s.only_in_baseline,
            s.only_in_candidate,
        ));
        let table = |title: &str, rows: Vec<(String, &SlotCounts)>| {
            let mut t = format!("\n[{title}]\n");
            for (k, c) in rows {
                t.push_str(&format!("  {k:<16} 变化 {}/{}\n", c.changed, c.total));
            }
            t
        };
        out.push_str(&table(
            "按角色",
            s.per_character
                .iter()
                .map(|(k, v)| (if k.is_empty() { "(无角色)".to_string() } else { k.clone() }, v))
                .collect(),
        ));
        out.push_str(&table("按环节", s.per_agent.iter().map(|(k, v)| (k.clone(), v)).collect()));
        out.push_str(&table(
            "按拍",
            s.per_beat.iter().map(|(k, v)| (format!("拍 {k}"), v)).collect(),
        ));

        if self.slots.is_empty() {
            out.push_str("\n无差异槽位。\n");
            return out;
        }
        out.push_str("\n[逐槽差异]\n");
        for d in &self.slots {
            out.push_str(&format!("{}  {}\n", d.label, change_label(d.change)));
            if let Some((a, b)) = &d.status {
                out.push_str(&format!("    状态 {a} → {b}\n"));
            }
            for f in &d.fields {
                out.push_str(&format!(
                    "    {}: {} → {}\n",
                    f.path,
                    f.before.as_deref().unwrap_or("(缺)"),
                    f.after.as_deref().unwrap_or("(缺)")
                ));
            }
            if d.fields_truncated {
                out.push_str("    …（字段差异过多，已截断）\n");
            }
            if d.fields.is_empty() {
                if let Some(t) = &d.response {
                    out.push_str(&format!(
                        "    响应 {} 字 → {} 字，首差第 {} 字：{} → {}\n",
                        t.baseline_chars, t.candidate_chars, t.first_diff_char, t.baseline_excerpt, t.candidate_excerpt
                    ));
                }
            }
            if d.prompt_changed {
                if let Some(t) = &d.prompt {
                    out.push_str(&format!("    ⚠️ prompt 也变了（首差第 {} 字）\n", t.first_diff_char));
                }
            }
        }
        out
    }
}

fn change_label(c: SlotChange) -> &'static str {
    match c {
        SlotChange::OnlyInBaseline => "仅基线有（候选少调了这次）",
        SlotChange::OnlyInCandidate => "仅候选有（候选多调了这次）",
        SlotChange::StatusChanged => "成败翻转",
        SlotChange::ResponseChanged => "响应变化",
        SlotChange::PromptChanged => "仅 prompt 变化（响应逐字相同）",
        SlotChange::Identical => "一致",
    }
}

// ============================================================================
// 文本 / JSON 差异原语
// ============================================================================

fn truncate_chars(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut out: String = chars[..max].iter().collect();
    out.push('…');
    out
}

/// 首个差异字符下标 + 该点附近的片段（按字符切，中文安全）。
fn text_delta(a: &str, b: &str, max_chars: usize) -> TextDelta {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let first = ca.iter().zip(cb.iter()).position(|(x, y)| x != y).unwrap_or(ca.len().min(cb.len()));
    let window = |c: &[char]| -> String {
        let start = first.saturating_sub(max_chars / 4);
        let end = (start + max_chars).min(c.len());
        let mut s = String::new();
        if start > 0 {
            s.push('…');
        }
        s.extend(&c[start.min(c.len())..end]);
        if end < c.len() {
            s.push('…');
        }
        s
    };
    TextDelta {
        baseline_chars: ca.len(),
        candidate_chars: cb.len(),
        first_diff_char: first,
        baseline_excerpt: window(&ca),
        candidate_excerpt: window(&cb),
    }
}

fn compact(v: &Value, max: usize) -> String {
    truncate_chars(&v.to_string(), max)
}

/// 递归比较两个 JSON 值，产出字段级差异。
///
/// 遍历序固定：对象按 key 排序（两侧 key 取并集）、数组按下标；到达上限即停（`out.len()`
/// 判定在每次 push 前，故截断点也是确定的）。
fn json_deltas(a: &Value, b: &Value, path: &str, out: &mut Vec<FieldDelta>, opts: &DiffOptions) {
    if out.len() >= opts.max_field_deltas || a == b {
        return;
    }
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let keys: BTreeSet<&String> = ma.keys().chain(mb.keys()).collect();
            for k in keys {
                if out.len() >= opts.max_field_deltas {
                    return;
                }
                let child = format!("{path}/{k}");
                match (ma.get(k), mb.get(k)) {
                    (Some(x), Some(y)) => json_deltas(x, y, &child, out, opts),
                    (Some(x), None) => out.push(FieldDelta {
                        path: child,
                        before: Some(compact(x, opts.max_value_chars)),
                        after: None,
                    }),
                    (None, Some(y)) => out.push(FieldDelta {
                        path: child,
                        before: None,
                        after: Some(compact(y, opts.max_value_chars)),
                    }),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(va), Value::Array(vb)) => {
            for i in 0..va.len().max(vb.len()) {
                if out.len() >= opts.max_field_deltas {
                    return;
                }
                let child = format!("{path}/{i}");
                match (va.get(i), vb.get(i)) {
                    (Some(x), Some(y)) => json_deltas(x, y, &child, out, opts),
                    (Some(x), None) => out.push(FieldDelta {
                        path: child,
                        before: Some(compact(x, opts.max_value_chars)),
                        after: None,
                    }),
                    (None, Some(y)) => out.push(FieldDelta {
                        path: child,
                        before: None,
                        after: Some(compact(y, opts.max_value_chars)),
                    }),
                    (None, None) => {}
                }
            }
        }
        _ => out.push(FieldDelta {
            path: if path.is_empty() { "/".to_string() } else { path.to_string() },
            before: Some(compact(a, opts.max_value_chars)),
            after: Some(compact(b, opts.max_value_chars)),
        }),
    }
}

/// 模型响应恒是严格 JSON（`json_call` 那一层的契约），故先按 JSON 比；
/// 解析不了（写作环节偶有围栏残留 / 真实模型脏输出）就退回纯文本差异。
fn field_deltas(a: &str, b: &str, opts: &DiffOptions) -> (Vec<FieldDelta>, bool) {
    let (Ok(va), Ok(vb)) = (extract_json_payload(a), extract_json_payload(b)) else {
        return (Vec::new(), false);
    };
    let mut out = Vec::new();
    json_deltas(&va, &vb, "", &mut out, opts);
    let truncated = out.len() >= opts.max_field_deltas;
    (out, truncated)
}

// ============================================================================
// 主入口
// ============================================================================

/// 比较两份录制。`baseline` = 先录的那份，`candidate` = 换模型 / 换 Prompt / 换引擎之后的那份。
pub fn diff_recordings(baseline: &Recording, candidate: &Recording, opts: &DiffOptions) -> RecordingDiff {
    let bi = baseline.slot_index();
    let ci = candidate.slot_index();
    let keys: BTreeSet<SlotKey> = bi.keys().chain(ci.keys()).cloned().collect();

    let mut summary = DiffSummary {
        baseline_calls: baseline.calls.len(),
        candidate_calls: candidate.calls.len(),
        ..Default::default()
    };
    let mut slots = Vec::new();

    for key in keys {
        let b = bi.get(&key).map(|i| &baseline.calls[*i]);
        let c = ci.get(&key).map(|i| &candidate.calls[*i]);
        let label = key.label();

        let (change, prompt_changed, status, prompt, response, fields, truncated) = match (b, c) {
            (Some(_), None) => (SlotChange::OnlyInBaseline, false, None, None, None, Vec::new(), false),
            (None, Some(_)) => (SlotChange::OnlyInCandidate, false, None, None, None, Vec::new(), false),
            (Some(b), Some(c)) => {
                let prompt_changed = b.prompt_digest != c.prompt_digest;
                let prompt_delta = prompt_changed.then(|| {
                    text_delta(
                        &format!("{}\n{}", b.system, b.user),
                        &format!("{}\n{}", c.system, c.user),
                        opts.max_value_chars,
                    )
                });
                let (bt, ct) = (b.outcome.text(), c.outcome.text());
                if b.outcome.status() != c.outcome.status() {
                    (
                        SlotChange::StatusChanged,
                        prompt_changed,
                        Some((b.outcome.status().to_string(), c.outcome.status().to_string())),
                        prompt_delta,
                        Some(text_delta(&bt, &ct, opts.max_value_chars)),
                        Vec::new(),
                        false,
                    )
                } else if bt != ct {
                    let (fields, truncated) = field_deltas(&bt, &ct, opts);
                    (
                        SlotChange::ResponseChanged,
                        prompt_changed,
                        None,
                        prompt_delta,
                        Some(text_delta(&bt, &ct, opts.max_value_chars)),
                        fields,
                        truncated,
                    )
                } else if prompt_changed {
                    (SlotChange::PromptChanged, true, None, prompt_delta, None, Vec::new(), false)
                } else {
                    (SlotChange::Identical, false, None, None, None, Vec::new(), false)
                }
            }
            (None, None) => unreachable!("键来自两侧索引的并集"),
        };

        let changed = change != SlotChange::Identical;
        match change {
            SlotChange::Identical => summary.identical += 1,
            SlotChange::ResponseChanged => summary.response_changed += 1,
            SlotChange::PromptChanged => summary.prompt_changed += 1,
            SlotChange::StatusChanged => summary.status_changed += 1,
            SlotChange::OnlyInBaseline => summary.only_in_baseline += 1,
            SlotChange::OnlyInCandidate => summary.only_in_candidate += 1,
        }
        // prompt 变化在主分类被响应变化盖住时也要计数（「改了 Prompt」是独立事实）。
        if prompt_changed && change != SlotChange::PromptChanged {
            summary.prompt_changed += 1;
        }
        summary.per_character.entry(key.character.clone()).or_default().bump(changed);
        summary.per_agent.entry(key.agent.clone()).or_default().bump(changed);
        summary.per_beat.entry(key.beat).or_default().bump(changed);

        if changed || opts.include_identical {
            slots.push(SlotDelta {
                beat: key.beat,
                character: key.character.clone(),
                agent: key.agent.clone(),
                occurrence: key.occurrence,
                label,
                change,
                prompt_changed,
                status,
                prompt,
                response,
                fields,
                fields_truncated: truncated,
            });
        }
    }

    RecordingDiff {
        baseline: SideMeta::of(baseline),
        candidate: SideMeta::of(candidate),
        summary,
        slots,
    }
}
