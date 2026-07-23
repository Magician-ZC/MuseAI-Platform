//! 倒排索引构建与打分检索（MVP 无 embedding）。文件所有权：agent-E2。

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::chunk::tokenize;
use super::types::{Chunk, ChunkIndex, RetrievedFragment};
use crate::EngineError;

/// 构建索引：postings term → 有序去重 ordinal 列表。
pub fn build_index(
    pack_id: &str,
    index_version: &str,
    chunker_version: &str,
    chunks: Vec<Chunk>,
) -> ChunkIndex {
    let mut postings: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for c in &chunks {
        // 每块内每个 term 只登记一次该块 ordinal。
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for term in tokenize(&c.text) {
            if seen.insert(term.clone()) {
                postings.entry(term).or_default().push(c.ordinal);
            }
        }
    }
    // 保证每个 postings 列表有序去重（确定性）。
    for v in postings.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    ChunkIndex {
        schema_version: 1,
        pack_id: pack_id.to_string(),
        index_version: index_version.to_string(),
        chunker_version: chunker_version.to_string(),
        chunks,
        postings,
    }
}

/// 查询：tokenize 后按 tf·idf 近似打分（idf = ln(1 + N/df)），返回 top-limit。
/// 必测：命中排序稳定性（同分按 ordinal）、查询无命中返回空、多包合并由上层做。
pub fn query_index(
    index: &ChunkIndex,
    pack_title: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<RetrievedFragment>, EngineError> {
    // 查询词去重（tf 由候选块正文再算）。
    let mut terms = tokenize(query);
    terms.sort();
    terms.dedup();
    if terms.is_empty() || index.chunks.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let n = index.chunks.len() as f64;
    // 命中候选块 + 每个查询词的 idf。
    let mut candidates: BTreeSet<u32> = BTreeSet::new();
    let mut idf: HashMap<&str, f64> = HashMap::new();
    for t in &terms {
        if let Some(list) = index.postings.get(t) {
            let df = list.len() as f64;
            idf.insert(t.as_str(), (1.0 + n / df).ln());
            candidates.extend(list.iter().copied());
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let by_ord: HashMap<u32, &Chunk> = index.chunks.iter().map(|c| (c.ordinal, c)).collect();

    // 打分：score = Σ tf(t) · idf(t)。
    let mut scored: Vec<(f32, u32)> = Vec::new();
    for o in candidates {
        let chunk = match by_ord.get(&o) {
            Some(c) => *c,
            None => continue,
        };
        let toks = tokenize(&chunk.text);
        let mut score = 0f64;
        for t in &terms {
            if let Some(&w) = idf.get(t.as_str()) {
                let tf = toks.iter().filter(|x| x.as_str() == t.as_str()).count();
                if tf > 0 {
                    score += tf as f64 * w;
                }
            }
        }
        scored.push((score as f32, o));
    }

    // 分数降序；同分按 ordinal 升序（确定性）。
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal).then(a.1.cmp(&b.1))
    });
    scored.truncate(limit);

    let out = scored
        .into_iter()
        .map(|(score, o)| {
            let c = by_ord[&o];
            RetrievedFragment {
                pack_id: index.pack_id.clone(),
                pack_title: pack_title.to_string(),
                chunk_id: c.id.clone(),
                ordinal: o,
                text: c.text.clone(),
                score,
            }
        })
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(pack: &str, ord: u32, text: &str) -> Chunk {
        Chunk {
            id: format!("{pack}#{ord}"),
            pack_id: pack.to_string(),
            ordinal: ord,
            text: text.to_string(),
            heading: None,
            char_range: (0, text.chars().count()),
        }
    }

    fn build(chunks: Vec<Chunk>) -> ChunkIndex {
        build_index("pk", "h:chunker-1", "chunker-1", chunks)
    }

    #[test]
    fn build_indexes_terms() {
        let idx = build(vec![ch("pk", 0, "拿破仑 的 战术"), ch("pk", 1, "厨房 的 苹果")]);
        // “战术” 只在块 0；“的” 在两块。
        assert_eq!(idx.postings.get("战术").unwrap(), &vec![0]);
        assert_eq!(idx.postings.get("的").unwrap(), &vec![0, 1]);
    }

    #[test]
    fn query_ranks_relevant_first() {
        let idx = build(vec![
            ch("pk", 0, "拿破仑 的 战术 与 战略 部署"),
            ch("pk", 1, "厨房 里 有 苹果 和 面粉"),
        ]);
        let res = query_index(&idx, "兵法", "战术", 5).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].ordinal, 0);
        assert_eq!(res[0].pack_title, "兵法");
        assert!(res[0].score > 0.0);
    }

    #[test]
    fn query_no_match_returns_empty() {
        let idx = build(vec![ch("pk", 0, "只有 无关 内容")]);
        assert!(query_index(&idx, "t", "量子纠缠", 5).unwrap().is_empty());
        // 空查询也返回空
        assert!(query_index(&idx, "t", "", 5).unwrap().is_empty());
    }

    #[test]
    fn ranking_tie_break_by_ordinal() {
        // 三块正文相同 → 同分，必须按 ordinal 升序稳定返回。
        let idx = build(vec![
            ch("pk", 0, "战术 战术 战术"),
            ch("pk", 1, "战术 战术 战术"),
            ch("pk", 2, "战术 战术 战术"),
        ]);
        let res = query_index(&idx, "t", "战术", 5).unwrap();
        assert_eq!(res.iter().map(|f| f.ordinal).collect::<Vec<_>>(), vec![0, 1, 2]);
        // 同分校验
        assert!((res[0].score - res[1].score).abs() < 1e-6);
    }

    #[test]
    fn limit_truncates() {
        let idx = build(vec![
            ch("pk", 0, "战术"),
            ch("pk", 1, "战术"),
            ch("pk", 2, "战术"),
        ]);
        assert_eq!(query_index(&idx, "t", "战术", 2).unwrap().len(), 2);
    }
}
