//! 章节切分（规格 §10.2 阶段 1）：正则 + 目录启发式，兜底 8000 字硬切。
//! 纯函数，无 IO、无模型调用。文件所有权：agent-E1。

use super::types::{ChapterEntry, ChapterStatus};
use crate::store::new_id;
use crate::EngineError;

/// 超短章阈值（字符）：低于此长度并入相邻章。
const SHORT_CHAPTER_CHARS: usize = 50;
/// 章节标题行最大长度：超过则视为正文段落，不当作标题（防误判）。
const HEADING_MAX_CHARS: usize = 40;
/// 特殊标题（序章/楔子…）行最大长度：更严，避免误伤正文。
const SPECIAL_HEADING_MAX_CHARS: usize = 20;
/// 标题截断长度。
const TITLE_MAX_CHARS: usize = 80;

/// 切分契约：
/// - 识别常见章节标题（`第X章/节/回/卷`、`Chapter N`、`卷X 第X章`、纯数字行、`序章/楔子/番外/尾声`）；
/// - 标题识别不到时按 `fallback_chunk_chars`（默认 8000）硬切，标题命名为「第 N 段」；
/// - `char_range` 为字符（char）偏移半开区间 [start, end)，覆盖全文无缝隙无重叠；
/// - 超短章（< 50 字）并入前章；`id` 用 store::new_id("ch")，`status=Pending, attempt=0`。
///
/// 必测（P0 测试清单）：无目录纯文本、混合编码已 lossy 后的文本、超短章合并、
/// 全文只有一章、章节标题在行中而非行首（不识别）。
pub fn split_chapters(text: &str, fallback_chunk_chars: usize) -> Result<Vec<ChapterEntry>, EngineError> {
    let chunk = if fallback_chunk_chars == 0 { 8000 } else { fallback_chunk_chars };
    let total_chars = text.chars().count();
    if total_chars == 0 {
        return Ok(Vec::new());
    }

    // 逐行扫描，按字符偏移定位标题行（仅行首识别）。
    let detector = HeadingDetector::new();
    let mut boundaries: Vec<(usize, String)> = Vec::new(); // (行首字符偏移, 标题)
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let line_chars = line.chars().count();
        let trimmed = line.trim();
        if detector.is_heading(trimmed) {
            boundaries.push((offset, truncate_chars(trimmed, TITLE_MAX_CHARS)));
        }
        offset += line_chars;
    }

    // 生成原始区间。
    let mut raw: Vec<(usize, usize, String)> = Vec::new();
    if boundaries.is_empty() {
        // 无目录：按 chunk 硬切。
        let mut start = 0usize;
        let mut seg = 1usize;
        while start < total_chars {
            let end = (start + chunk).min(total_chars);
            raw.push((start, end, format!("第 {seg} 段")));
            start = end;
            seg += 1;
        }
    } else {
        // 首个标题之前的内容独立成「开篇」章。
        if boundaries[0].0 > 0 {
            raw.push((0, boundaries[0].0, "开篇".to_string()));
        }
        for i in 0..boundaries.len() {
            let start = boundaries[i].0;
            let end = boundaries.get(i + 1).map(|b| b.0).unwrap_or(total_chars);
            raw.push((start, end, boundaries[i].1.clone()));
        }
    }

    // 超短章合并：先向前并（无前章的首个短章暂留），再把仍然超短的首章并入下一章。
    let mut merged: Vec<(usize, usize, String)> = Vec::new();
    for (s, e, title) in raw {
        if e - s < SHORT_CHAPTER_CHARS && !merged.is_empty() {
            merged.last_mut().unwrap().1 = e; // 扩展前章 end，保持无缝
        } else {
            merged.push((s, e, title));
        }
    }
    if merged.len() >= 2 && (merged[0].1 - merged[0].0) < SHORT_CHAPTER_CHARS {
        let first_start = merged[0].0;
        merged[1].0 = first_start; // 首个短章并入下一章
        merged.remove(0);
    }

    let chapters = merged
        .into_iter()
        .enumerate()
        .map(|(idx, (start, end, title))| ChapterEntry {
            id: new_id("ch"),
            index: idx as u32,
            title,
            char_range: (start, end),
            status: ChapterStatus::Pending,
            attempt: 0,
            discovery_store_key: None,
            error: None,
        })
        .collect();
    Ok(chapters)
}

/// 按 char_range 取章节文本（供扫描与证据预览使用）。
pub fn chapter_text<'a>(full_text: &'a str, range: (usize, usize)) -> &'a str {
    let chars: Vec<(usize, char)> = full_text.char_indices().collect();
    let start = chars.get(range.0).map(|(i, _)| *i).unwrap_or(full_text.len());
    let end = chars.get(range.1).map(|(i, _)| *i).unwrap_or(full_text.len());
    &full_text[start..end]
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// 标题识别器：一次性编译正则，逐行复用。
struct HeadingDetector {
    cn_chapter: regex::Regex,
    juan: regex::Regex,
    en_chapter: regex::Regex,
    special: regex::Regex,
}

impl HeadingDetector {
    fn new() -> Self {
        // 数字部分允许中文数字与阿拉伯数字；末尾锚定明确的章节量词，规避「第一次/第三部分」误判。
        let num = "[〇零一二三四五六七八九十百千两0-9]";
        Self {
            cn_chapter: regex::Regex::new(&format!("^第{num}{{1,9}}[章回卷节篇]")).unwrap(),
            juan: regex::Regex::new(&format!("^卷{num}{{1,9}}")).unwrap(),
            en_chapter: regex::Regex::new(r"^(?i)chapter\s+\d+").unwrap(),
            special: regex::Regex::new(
                r"^(序章|序言|楔子|引子|尾声|后记|终章|番外|外传|卷首语|前言|序幕|序)(?:$|[\s:：、。·\-—　（(])",
            )
            .unwrap(),
        }
    }

    fn is_heading(&self, trimmed: &str) -> bool {
        if trimmed.is_empty() {
            return false;
        }
        let clen = trimmed.chars().count();
        // 纯数字单行章节号（如目录抽取后的「12」）。
        if clen <= 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        if clen <= HEADING_MAX_CHARS
            && (self.cn_chapter.is_match(trimmed)
                || self.juan.is_match(trimmed)
                || self.en_chapter.is_match(trimmed))
        {
            return true;
        }
        if clen <= SPECIAL_HEADING_MAX_CHARS && self.special.is_match(trimmed) {
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(chs: &[ChapterEntry]) -> Vec<(usize, usize)> {
        chs.iter().map(|c| c.char_range).collect()
    }

    // 覆盖全文无缝隙无重叠。
    fn assert_seamless(chs: &[ChapterEntry], total: usize) {
        assert_eq!(chs[0].char_range.0, 0);
        assert_eq!(chs.last().unwrap().char_range.1, total);
        for w in chs.windows(2) {
            assert_eq!(w[0].char_range.1, w[1].char_range.0);
        }
    }

    #[test]
    fn no_toc_plain_text_hard_split() {
        let text = "甲".repeat(20000); // 20000 字，无标题
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 3); // 8000 + 8000 + 4000
        assert_eq!(ranges(&chs), vec![(0, 8000), (8000, 16000), (16000, 20000)]);
        assert!(chs.iter().all(|c| matches!(c.status, ChapterStatus::Pending) && c.attempt == 0));
        assert_seamless(&chs, 20000);
    }

    #[test]
    fn recognizes_common_headings() {
        let body = "内容".repeat(60); // 每章 120 字，非超短
        let text = format!("第一章 起\n{body}\n第二章 承\n{body}\nChapter 3\n{body}");
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 3);
        assert_eq!(chs[0].title, "第一章 起");
        assert_eq!(chs[2].title, "Chapter 3");
        assert_seamless(&chs, text.chars().count());
    }

    #[test]
    fn mixed_encoding_lossy_does_not_panic() {
        // 模拟 lossy 后夹带替换符 U+FFFD 的文本。
        let text = format!("第一章\n{}\u{FFFD}{}", "字".repeat(80), "尾".repeat(40));
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 1);
        assert_seamless(&chs, text.chars().count());
    }

    #[test]
    fn short_chapter_merges_into_previous() {
        let long = "内容".repeat(60); // 120 字
        // 第二章内容仅几字（超短），应并入第一章。
        let text = format!("第一章\n{long}\n第二章\n短\n第三章\n{long}");
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 2); // 第二章被并入第一章
        assert_eq!(chs[0].title, "第一章");
        assert_eq!(chs[1].title, "第三章");
        assert_seamless(&chs, text.chars().count());
    }

    #[test]
    fn short_first_chapter_merges_into_next() {
        let long = "内容".repeat(60);
        let text = format!("短\n第一章\n{long}"); // 开篇仅「短」一字
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 1); // 开篇并入第一章
        assert_eq!(chs[0].char_range.0, 0);
        assert_seamless(&chs, text.chars().count());
    }

    #[test]
    fn single_chapter_whole_text() {
        let text = format!("第一章 唯一\n{}", "正文".repeat(100));
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 1);
        assert_seamless(&chs, text.chars().count());
    }

    #[test]
    fn heading_in_line_middle_not_recognized() {
        // 「第一章」出现在行中而非行首，不应识别为标题。
        let text = format!("他翻到第一章的位置\n{}", "读了很久".repeat(80));
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 1);
        assert_eq!(chs[0].title, "第 1 段"); // 走硬切兜底
    }

    #[test]
    fn false_positive_guarded() {
        // 「第一次」「第三部分」不含章节量词或含歧义量词，不识别。
        let text = format!("第一次见面\n{}\n第三部分开始", "叙述".repeat(80));
        let chs = split_chapters(&text, 8000).unwrap();
        assert_eq!(chs.len(), 1);
    }

    #[test]
    fn empty_text_yields_no_chapters() {
        assert!(split_chapters("", 8000).unwrap().is_empty());
    }
}
