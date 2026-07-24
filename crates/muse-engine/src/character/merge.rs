//! 别名归并（规格 §10.2 阶段 3）：先规则归并，剩余交模型判定；结果全部进用户确认页。
//! 文件所有权：agent-E1。

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::store::content_hash;
use crate::EngineError;

use super::types::{ChapterDiscovery, RosterEntry, RosterTier};
use super::CharacterPrompts;

/// 每批模型判定的候选上限。
const MERGE_BATCH_MAX: usize = 40;

/// 常见敬称/职务后缀（剥离后若剩余 ≥2 字才用于归并）。
const SUFFIX_TITLES: &[&str] = &[
    "姑娘", "公子", "先生", "大人", "老爷", "夫人", "小姐", "少爷", "将军", "娘娘", "王爷", "师父",
    "师傅", "陛下", "殿下", "嬷嬷", "丫头", "大侠", "真人", "道长", "法师", "教主", "堂主", "帮主",
    "兄", "姐", "哥", "弟", "妹", "君", "公", "侯", "王",
];
/// 常见前缀（剥离后若剩余 ≥2 字才用于归并）。
const PREFIX_TITLES: &[&str] = &["老", "小", "阿", "大"];

/// 规则归并（无模型调用）：
/// - 完全同名合并；
/// - 包含关系（"林黛玉" ⊃ "黛玉"，长度 ≥ 2 且短名在长名中连续出现）合并，长名为 canonical；
/// - 高频敬称/职务前后缀剥离后同名（"林姑娘"→"林"，仅当剥离结果 ≥ 2 字）；
/// - 输出：surface → 簇 的映射与未决簇列表。
/// 必测：包含关系合并、单字不合并、剥离后歧义不合并（进未决）。
pub fn rule_merge(discoveries: &[ChapterDiscovery]) -> (Vec<RosterEntry>, Vec<Vec<String>>) {
    // 收集去重 surface + 频次。
    let mut freq: BTreeMap<String, u32> = BTreeMap::new();
    for d in discoveries {
        for m in &d.mentions {
            let s = m.surface.trim();
            if !s.is_empty() {
                *freq.entry(s.to_string()).or_default() += 1;
            }
        }
    }
    let surfaces: Vec<String> = freq.keys().cloned().collect();
    let n = surfaces.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let idx_of: BTreeMap<&str, usize> = surfaces.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();

    let mut uf = UnionFind::new(n);

    // 包含关系：短名（≥2 字，连续子串）并入长名。
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            let (long, short) = (&surfaces[i], &surfaces[j]);
            if short.chars().count() >= 2
                && long.chars().count() > short.chars().count()
                && long.contains(short.as_str())
            {
                uf.union(i, j);
            }
        }
    }

    // 敬称剥离：剥离结果恰好等于某个已知 surface 时合并（歧义不在此处理）。
    for (i, s) in surfaces.iter().enumerate() {
        for r in strip_titles(s) {
            if let Some(&j) = idx_of.get(r.as_str()) {
                if j != i {
                    uf.union(i, j);
                }
            }
        }
    }

    // 归簇。
    let mut clusters: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        clusters.entry(uf.find(i)).or_default().push(i);
    }

    let mut resolved: Vec<RosterEntry> = Vec::new();
    let mut unresolved: Vec<Vec<String>> = Vec::new();

    for members in clusters.values() {
        if members.len() >= 2 {
            let names: Vec<String> = members.iter().map(|&i| surfaces[i].clone()).collect();
            resolved.push(make_entry(&names, &freq));
        } else {
            // 单成员：先判剥离歧义，歧义则进未决，否则作为独立角色。
            let s = &surfaces[members[0]];
            if let Some(cluster) = ambiguous_cluster(s, &surfaces) {
                unresolved.push(cluster);
            } else {
                resolved.push(make_entry(&[s.clone()], &freq));
            }
        }
    }
    (resolved, unresolved)
}

/// 模型归并：对未决簇分批（每批 ≤ 40 个候选）调用模型判定同一性；
/// 输出仅允许引用输入中出现过的 surface（引用完整性校验），越界输出丢弃。
/// merged_from 记录来源 surface；user_confirmed 一律 false（人工确认页决定）。
pub async fn model_merge(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &CharacterPrompts,
    run_id: &str,
    unresolved: Vec<Vec<String>>,
    context_samples: &BTreeMap<String, Vec<String>>,
    cancel: &CancelFlag,
) -> Result<Vec<RosterEntry>, EngineError> {
    // 扁平化去重候选。
    let mut pool: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for cluster in &unresolved {
        for s in cluster {
            if seen.insert(s.clone()) {
                pool.push(s.clone());
            }
        }
    }
    if pool.is_empty() {
        return Ok(Vec::new());
    }

    let freq: BTreeMap<String, u32> = pool.iter().map(|s| (s.clone(), 1)).collect();
    let mut entries: Vec<RosterEntry> = Vec::new();
    for batch in pool.chunks(MERGE_BATCH_MAX) {
        let allowed: BTreeSet<&str> = batch.iter().map(|s| s.as_str()).collect();
        let user = build_merge_prompt(batch, context_samples);
        let spec = ModelCallSpec {
            max_retries: None,
            profile: profile.clone(),
            system: prompts.merge_system.clone(),
            user,
            temperature: 0.0,
            max_output_tokens: 2048,
            agent: "characterMerge".to_string(),
            prompt_version: prompts.prompt_version.clone(),
            run_id: run_id.to_string(),
        };
        let resp: MergeResponse =
            json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
        for group in resp.groups {
            // 引用完整性：只保留出现在本批输入中的 surface。
            let mut names: Vec<String> =
                group.surfaces.into_iter().filter(|s| allowed.contains(s.as_str())).collect();
            names.sort();
            names.dedup();
            if names.is_empty() {
                continue;
            }
            entries.push(make_entry(&names, &freq));
        }
    }
    Ok(entries)
}

fn build_merge_prompt(batch: &[String], samples: &BTreeMap<String, Vec<String>>) -> String {
    let mut lines = String::new();
    for s in batch {
        lines.push_str(&format!("- {s}"));
        if let Some(ctx) = samples.get(s) {
            if let Some(first) = ctx.first() {
                lines.push_str(&format!("（例：{}）", truncate(first, 40)));
            }
        }
        lines.push('\n');
    }
    format!(
        "以下是同一部作品中出现的角色称呼候选，请判断哪些指向同一个人物。\n\
候选清单：\n{lines}\n\
严格输出 JSON：{{\"groups\":[{{\"canonicalName\":\"本名\",\"surfaces\":[\"同属该人物的称呼\"]}}]}}\n\
要求：surfaces 只能来自上面的候选清单，禁止臆造未列出的名字；无法确定同一性的称呼各自独立成组。"
    )
}

/// 剥离前后缀得到的候选形（≥2 字）。
fn strip_titles(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for t in SUFFIX_TITLES {
        if let Some(r) = s.strip_suffix(t) {
            if r.chars().count() >= 2 {
                out.push(r.to_string());
            }
        }
    }
    for t in PREFIX_TITLES {
        if let Some(r) = s.strip_prefix(t) {
            if r.chars().count() >= 2 {
                out.push(r.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 剥离后歧义检测：剥离结果 R（≥2 字）不是任何已知 surface，却是 ≥2 个其他 surface 的子串 → 歧义。
fn ambiguous_cluster(s: &str, surfaces: &[String]) -> Option<Vec<String>> {
    for r in strip_titles(s) {
        let exact = surfaces.iter().any(|x| x == &r);
        if exact {
            continue; // 精确匹配已在规则合并处理
        }
        let candidates: Vec<String> =
            surfaces.iter().filter(|x| x.as_str() != s && x.contains(&r)).cloned().collect();
        if candidates.len() >= 2 {
            let mut cluster = vec![s.to_string()];
            cluster.extend(candidates);
            cluster.sort();
            cluster.dedup();
            return Some(cluster);
        }
    }
    None
}

/// 由簇成员构造 RosterEntry：canonical 取最长（并列取高频、再取字典序最小）。
fn make_entry(members: &[String], freq: &BTreeMap<String, u32>) -> RosterEntry {
    let canonical = members
        .iter()
        .max_by(|a, b| {
            a.chars()
                .count()
                .cmp(&b.chars().count())
                .then(freq.get(*a).copied().unwrap_or(0).cmp(&freq.get(*b).copied().unwrap_or(0)))
                .then(b.as_str().cmp(a.as_str())) // 字典序小者优先（反向后取 max）
        })
        .cloned()
        .unwrap_or_default();
    let aliases: Vec<String> = members.iter().filter(|m| **m != canonical).cloned().collect();
    let mut merged_from = members.to_vec();
    merged_from.sort();
    RosterEntry {
        key: stable_key(&canonical),
        canonical_name: canonical,
        aliases,
        tier: RosterTier::Functional, // 占位，分层阶段覆盖
        merged_from,
        user_confirmed: false,
        dna_status: super::types::DnaStatus::Pending,
    }
}

/// 由 canonical 派生稳定 key（同输入幂等，便于恢复与去重）。
pub fn stable_key(canonical: &str) -> String {
    let h = content_hash(canonical.as_bytes());
    format!("role-{}", &h[..12])
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect() }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.parent[r] != r {
            r = self.parent[r];
        }
        // 路径压缩
        let mut cur = x;
        while self.parent[cur] != r {
            let next = self.parent[cur];
            self.parent[cur] = r;
            cur = next;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

#[derive(Deserialize)]
struct MergeResponse {
    #[serde(default)]
    groups: Vec<MergeGroup>,
}

#[derive(Deserialize)]
struct MergeGroup {
    #[serde(default, rename = "canonicalName")]
    _canonical_name: String,
    #[serde(default)]
    surfaces: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::{CharacterMention, MentionEvidence};
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use std::sync::Arc;

    fn disc(surfaces: &[&str]) -> ChapterDiscovery {
        ChapterDiscovery {
            chapter_index: 0,
            mentions: surfaces
                .iter()
                .map(|s| CharacterMention {
                    surface: s.to_string(),
                    role_hint: String::new(),
                    evidence: vec![MentionEvidence {
                        kind: crate::character::types::EvidenceKind::Action,
                        quote: "x".into(),
                        note: String::new(),
                        confidence: crate::character::types::Confidence::Medium,
                    }],
                })
                .collect(),
        }
    }

    fn canon_set(entries: &[RosterEntry]) -> BTreeSet<String> {
        entries.iter().map(|e| e.canonical_name.clone()).collect()
    }

    #[test]
    fn containment_merges_long_name_as_canonical() {
        let (resolved, _) = rule_merge(&[disc(&["林黛玉", "黛玉", "林黛玉"])]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].canonical_name, "林黛玉");
        assert!(resolved[0].aliases.contains(&"黛玉".to_string()));
        assert_eq!(resolved[0].merged_from, vec!["林黛玉".to_string(), "黛玉".to_string()]);
    }

    #[test]
    fn single_char_not_merged() {
        // 「宝」单字不并入「宝玉」（短名需 ≥2 字）。
        let (resolved, _) = rule_merge(&[disc(&["宝玉", "宝"])]);
        let canons = canon_set(&resolved);
        assert!(canons.contains("宝玉"));
        assert!(canons.contains("宝")); // 独立成条，未被并入
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn suffix_strip_exact_match_merges() {
        // 「黛玉姑娘」剥离 → 「黛玉」（2 字）精确匹配 → 合并。
        let (resolved, _) = rule_merge(&[disc(&["黛玉", "黛玉姑娘"])]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].canonical_name, "黛玉姑娘");
    }

    #[test]
    fn ambiguous_after_strip_goes_unresolved() {
        // 「明月姑娘」剥离 → 「明月」，非精确 surface，却是 明月心/明月楼 的子串（≥2）→ 未决。
        let (resolved, unresolved) = rule_merge(&[disc(&["明月姑娘", "明月心", "明月楼"])]);
        assert_eq!(unresolved.len(), 1);
        assert!(unresolved[0].contains(&"明月姑娘".to_string()));
        // 明月姑娘不出现在 resolved（交给模型）。
        assert!(!canon_set(&resolved).contains("明月姑娘"));
    }

    #[tokio::test]
    async fn model_merge_enforces_referential_integrity() {
        // 模型输出一个越界 surface「幽灵」，必须被丢弃。
        let resp = r#"{"groups":[{"canonicalName":"甲乙","surfaces":["甲","乙","幽灵"]}]}"#;
        let host = EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(0)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(ScriptedModel::new(vec![Ok(resp.into())])),
        };
        let profile = ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "u".into(),
            api_key: "k".into(),
            model: "m".into(),
        };
        let prompts = CharacterPrompts {
            scan_system: "s".into(),
            merge_system: "s".into(),
            tiering_system: "s".into(),
            synthesis_system: "s".into(),
            prompt_version: "v1".into(),
        };
        let entries = model_merge(
            &host,
            &profile,
            &prompts,
            "task-1",
            vec![vec!["甲".into(), "乙".into()]],
            &BTreeMap::new(),
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(entries.len(), 1);
        // 幽灵越界被剔除，只保留合法候选。
        let mf: BTreeSet<String> = entries[0].merged_from.iter().cloned().collect();
        assert_eq!(mf, BTreeSet::from(["甲".to_string(), "乙".to_string()]));
    }

    #[tokio::test]
    async fn model_merge_noop_on_empty() {
        let host = EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(0)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(ScriptedModel::new(vec![])), // 不应被调用
        };
        let profile = ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "u".into(),
            api_key: "k".into(),
            model: "m".into(),
        };
        let prompts = CharacterPrompts {
            scan_system: "s".into(),
            merge_system: "s".into(),
            tiering_system: "s".into(),
            synthesis_system: "s".into(),
            prompt_version: "v1".into(),
        };
        let entries =
            model_merge(&host, &profile, &prompts, "t", vec![], &BTreeMap::new(), &CancelFlag::new())
                .await
                .unwrap();
        assert!(entries.is_empty());
    }
}
