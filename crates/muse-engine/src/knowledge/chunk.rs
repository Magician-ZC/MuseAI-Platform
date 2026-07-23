//! 切块（规格 §11.1）：语义段落优先，兜底 800–1200 字滑窗。纯函数。文件所有权：agent-E2。

use super::types::Chunk;

/// 单块目标上限（字符）。段落聚合到此上限即断块。
const TARGET_MAX: usize = 1200;
/// 超长段落滑窗窗口大小（字符）。
const WINDOW: usize = 1200;
/// 滑窗重叠（字符）。
const OVERLAP: usize = 100;

/// 契约：
/// - 以空行/标题行为界聚合段落；单块目标 800–1200 字，段落过长滑窗切分（重叠 100 字）；
/// - `char_range` 为源文本 char 偏移；`ordinal` 从 0 连续递增；
/// - heading 取块内首个疑似标题行（≤ 30 字且独立成行）。
/// 必测：空文本、单段超长文本、全标题文本、中英混排。
pub fn split_chunks(pack_id: &str, text: &str) -> Vec<Chunk> {
    let chars: Vec<char> = text.chars().collect();
    let paras = extract_paragraphs(&chars);

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut ord: u32 = 0;
    // 当前聚合块的 [start, end) char 区间
    let mut cur: Option<(usize, usize)> = None;

    for (ps, pe, is_heading) in paras {
        let plen = pe - ps;

        // 超长段落：先冲刷当前块，再滑窗切分本段。
        if plen > TARGET_MAX {
            if let Some((s, e)) = cur.take() {
                chunks.push(make_chunk(pack_id, &chars, s, e, ord));
                ord += 1;
            }
            let mut ws = ps;
            while ws < pe {
                let we = (ws + WINDOW).min(pe);
                chunks.push(make_chunk(pack_id, &chars, ws, we, ord));
                ord += 1;
                if we >= pe {
                    break;
                }
                ws += WINDOW - OVERLAP;
            }
            continue;
        }

        // 标题行作为分界：另起新块，使块首即标题。
        if is_heading {
            if let Some((s, e)) = cur.take() {
                chunks.push(make_chunk(pack_id, &chars, s, e, ord));
                ord += 1;
            }
            cur = Some((ps, pe));
            continue;
        }

        // 普通段落：能并入当前块（不超上限）则扩展，否则断块。
        match cur {
            Some((s, _)) if pe - s > TARGET_MAX => {
                let (fs, fe) = cur.take().unwrap();
                chunks.push(make_chunk(pack_id, &chars, fs, fe, ord));
                ord += 1;
                cur = Some((ps, pe));
            }
            Some((s, _)) => cur = Some((s, pe)),
            None => cur = Some((ps, pe)),
        }
    }

    if let Some((s, e)) = cur.take() {
        chunks.push(make_chunk(pack_id, &chars, s, e, ord));
    }
    chunks
}

/// 按 char 区间构造一个 Chunk（id 用 ordinal 保证确定性，便于索引复用与溯源）。
fn make_chunk(pack_id: &str, chars: &[char], start: usize, end: usize, ordinal: u32) -> Chunk {
    let text: String = chars[start..end].iter().collect();
    let heading = detect_heading(&text);
    Chunk {
        id: format!("{pack_id}#{ordinal}"),
        pack_id: pack_id.to_string(),
        ordinal,
        text,
        heading,
        char_range: (start, end),
    }
}

/// 块内首个非空行若 ≤ 30 字，取为 heading（剥离 markdown `#` 前缀）。
fn detect_heading(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    let trimmed = line.trim();
    if trimmed.chars().count() > 30 {
        return None;
    }
    let cleaned = trimmed.trim_start_matches('#').trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// 按行切分并聚合段落，返回 `(start, end, is_heading)` 的 char 区间列表：
/// 空行断段；标题行独立成段（`is_heading=true`）；其余连续非空行并为一段。
fn extract_paragraphs(chars: &[char]) -> Vec<(usize, usize, bool)> {
    // 1) 拆行（end 不含 '\n'）。
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            lines.push((start, i));
            start = i + 1;
        }
    }
    // 末行：仅在有剩余内容或全文无换行时补入（避免尾随 '\n' 造出空行）。
    if start < chars.len() || lines.is_empty() {
        lines.push((start, chars.len()));
    }

    // 2) 聚合。
    let mut paras: Vec<(usize, usize, bool)> = Vec::new();
    let mut cur: Option<(usize, usize)> = None;
    for (ls, le) in lines {
        let slice = &chars[ls..le];
        if slice.iter().all(|c| c.is_whitespace()) {
            if let Some((s, e)) = cur.take() {
                paras.push((s, e, false));
            }
        } else if is_heading_line(slice) {
            if let Some((s, e)) = cur.take() {
                paras.push((s, e, false));
            }
            paras.push((ls, le, true));
        } else {
            match &mut cur {
                Some(p) => p.1 = le,
                None => cur = Some((ls, le)),
            }
        }
    }
    if let Some((s, e)) = cur.take() {
        paras.push((s, e, false));
    }
    paras
}

/// 疑似标题行：≤ 30 字，且命中常见标题标记（markdown `#`、第X章/节/回、Chapter、序章等）。
fn is_heading_line(line: &[char]) -> bool {
    let s: String = line.iter().collect();
    let t = s.trim();
    let cc = t.chars().count();
    if cc == 0 || cc > 30 {
        return false;
    }
    if t.starts_with('#') {
        return true;
    }
    if t.starts_with('第') && t.chars().take(12).any(|c| "章节回卷幕部集篇".contains(c)) {
        return true;
    }
    if t.to_ascii_lowercase().starts_with("chapter") {
        return true;
    }
    for kw in ["序章", "楔子", "尾声", "番外", "前言", "后记", "序言", "引言", "目录"] {
        if t.starts_with(kw) {
            return true;
        }
    }
    false
}

/// 检索词切分：中文 2-gram + ASCII 词（小写化）。索引构建与查询共用，保证一致。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    // 冲刷 ASCII 词缓冲。
    fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
        if !buf.is_empty() {
            out.push(std::mem::take(buf));
        }
    }
    // 冲刷 CJK 缓冲：长度 ≥ 2 出 2-gram，单字出单 token。
    fn flush_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
        if buf.len() == 1 {
            out.push(buf[0].to_string());
        } else if buf.len() >= 2 {
            for w in buf.windows(2) {
                out.push(w.iter().collect());
            }
        }
        buf.clear();
    }

    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut tokens);
            ascii.push(c.to_ascii_lowercase());
        } else if is_cjk(c) {
            flush_ascii(&mut ascii, &mut tokens);
            cjk.push(c);
        } else {
            // 分隔符（空白/标点等）：冲刷两个缓冲。
            flush_ascii(&mut ascii, &mut tokens);
            flush_cjk(&mut cjk, &mut tokens);
        }
    }
    flush_ascii(&mut ascii, &mut tokens);
    flush_cjk(&mut cjk, &mut tokens);
    tokens
}

/// 判定 CJK 字符（基本汉字 + 扩展 A + 兼容 + 日文假名）。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF |   // 平假名 + 片假名
        0x3400..=0x4DBF |   // 扩展 A
        0x4E00..=0x9FFF |   // 基本汉字
        0xF900..=0xFAFF |   // 兼容表意
        0x20000..=0x2A6DF   // 扩展 B
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_yields_no_chunks() {
        assert!(split_chunks("p", "").is_empty());
        assert!(split_chunks("p", "   \n\n  \n").is_empty());
    }

    #[test]
    fn long_single_paragraph_slides_with_overlap() {
        let text = "甲".repeat(3000);
        let chunks = split_chunks("p", &text);
        // 3000 → [0,1200) [1100,2300) [2200,3000)
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].ordinal, 0);
        assert_eq!(chunks[1].ordinal, 1);
        assert_eq!(chunks[2].ordinal, 2);
        assert_eq!(chunks[0].char_range, (0, 1200));
        // 重叠 100 字
        assert_eq!(chunks[1].char_range.0, chunks[0].char_range.1 - OVERLAP);
        assert_eq!(chunks[2].char_range, (2200, 3000));
        // text 与 char_range 宽度一致
        for c in &chunks {
            assert_eq!(c.text.chars().count(), c.char_range.1 - c.char_range.0);
        }
    }

    #[test]
    fn all_heading_text_sets_heading_per_block() {
        let text = "# 标题一\n\n# 标题二\n\n# 标题三";
        let chunks = split_chunks("p", text);
        assert_eq!(chunks.len(), 3);
        let headings: Vec<_> = chunks.iter().map(|c| c.heading.clone().unwrap()).collect();
        assert_eq!(headings, vec!["标题一", "标题二", "标题三"]);
    }

    #[test]
    fn heading_led_body_stays_together() {
        // 无空行，靠标题行分界：两个「标题+正文」块。
        let text = "第一章 开端\n正文内容一。\n第二章 转折\n正文内容二。";
        let chunks = split_chunks("p", text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading.as_deref(), Some("第一章 开端"));
        assert!(chunks[0].text.contains("正文内容一"));
        assert_eq!(chunks[1].heading.as_deref(), Some("第二章 转折"));
    }

    #[test]
    fn mixed_cjk_ascii_preserved_in_one_chunk() {
        let text = "孙子兵法讲 strategy 与 tactics，强调 the art of war 的取舍权衡与形势判断。";
        let chunks = split_chunks("p", text);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("strategy"));
        assert!(chunks[0].text.contains("取舍权衡"));
        assert_eq!(chunks[0].char_range.0, 0);
    }

    #[test]
    fn tokenize_bigrams_and_ascii_lowercase() {
        let toks = tokenize("军师 studies Napoleon的战术");
        assert_eq!(toks, vec!["军师", "studies", "napoleon", "的战", "战术"]);
        assert!(tokenize("").is_empty());
        // 单个 CJK 字出单 token
        assert_eq!(tokenize("水"), vec!["水".to_string()]);
    }
}
