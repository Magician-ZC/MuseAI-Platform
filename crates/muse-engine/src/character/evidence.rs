//! 证据账本（规格 §10.2 阶段 4）：从各章 discovery 分片聚合为按角色的 EvidenceRef 全量，纯聚合无模型调用。
//! 存储：`character-engine/evidence/<characterId>.json`。文件所有权：agent-E1。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::host::HostFs;
use crate::store::{content_hash, new_id};
use crate::EngineError;

use super::types::{
    ChapterDiscovery, EvidenceIndex, EvidenceLocator, EvidenceRef, RosterEntry,
};

/// quote_preview 最大字符数（与 §9.1 一致）。
const QUOTE_PREVIEW_MAX: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLedger {
    pub schema_version: u32, // 1
    pub character_id: String,
    pub evidence: Vec<EvidenceRef>,
    pub revision: u64,
    pub updated_at: i64,
}

pub fn ledger_path(character_id: &str) -> PathBuf {
    PathBuf::from("character-engine/evidence").join(format!("{character_id}.json"))
}

/// 按 roster 聚合：mention.surface ∈ entry(aliases∪canonical) 的证据归入该角色；
/// EvidenceRef.id 用 store::new_id("ev")；source_id 为任务的 sourceFingerprint.contentHash；
/// quote 截断到 200 字作 quote_preview；locator 以章内 char 偏移换算为全书偏移。
/// 返回各角色的账本与卡内 EvidenceIndex（content_hash 为账本序列化后的哈希）。
pub fn build_ledgers(
    fs: &Arc<dyn HostFs>,
    now_ms: i64,
    source_id: &str,
    roster: &[RosterEntry],
    discoveries: &[ChapterDiscovery],
    chapter_offsets: &[(usize, usize)],
) -> Result<Vec<(EvidenceLedger, EvidenceIndex)>, EngineError> {
    let mut out = Vec::with_capacity(roster.len());
    for entry in roster {
        // 该角色所有已知称呼。
        let surfaces: BTreeSet<&str> = entry
            .aliases
            .iter()
            .chain(entry.merged_from.iter())
            .map(String::as_str)
            .chain(std::iter::once(entry.canonical_name.as_str()))
            .collect();

        let mut evidence: Vec<EvidenceRef> = Vec::new();
        for d in discoveries {
            let (chap_start, chap_end) = chapter_offsets
                .get(d.chapter_index as usize)
                .copied()
                .unwrap_or((0, 0));
            for m in &d.mentions {
                if !surfaces.contains(m.surface.as_str()) {
                    continue;
                }
                for e in &m.evidence {
                    let quote_len = e.quote.chars().count();
                    // 无逐字符定位数据，以章起点近似证据位置（end 限制在章内）。
                    let start = chap_start;
                    let end = (chap_start + quote_len).min(chap_end.max(chap_start));
                    evidence.push(EvidenceRef {
                        id: new_id("ev"),
                        source_id: source_id.to_string(),
                        chapter_index: d.chapter_index,
                        locator: EvidenceLocator { start, end, heading: None },
                        quote_preview: truncate(&e.quote, QUOTE_PREVIEW_MAX),
                        kind: e.kind,
                        confidence: e.confidence,
                        user_confirmed: None,
                        conflicts_with: None,
                    });
                }
            }
        }

        let ledger = EvidenceLedger {
            schema_version: 1,
            character_id: entry.key.clone(),
            evidence,
            revision: 1,
            updated_at: now_ms,
        };
        // 序列化一次，写盘与算哈希用同一份字节，保证 index.content_hash 与文件一致。
        let bytes = serde_json::to_vec_pretty(&ledger)?;
        let path = ledger_path(&entry.key);
        fs.write_atomic(&path, &bytes)?;
        let index = EvidenceIndex {
            store_key: path.to_string_lossy().to_string(),
            content_hash: content_hash(&bytes),
            count: ledger.evidence.len() as u32,
        };
        out.push((ledger, index));
    }
    Ok(out)
}

pub fn load_ledger(fs: &Arc<dyn HostFs>, character_id: &str) -> Result<EvidenceLedger, EngineError> {
    crate::store::read_json(fs.as_ref(), &ledger_path(character_id))
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::merge::stable_key;
    use crate::character::types::{
        CharacterMention, Confidence, DnaStatus, EvidenceKind, MentionEvidence, RosterTier,
    };
    use crate::host::testing::MemFs;

    fn entry(canonical: &str, aliases: &[&str]) -> RosterEntry {
        RosterEntry {
            key: stable_key(canonical),
            canonical_name: canonical.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            tier: RosterTier::Core,
            merged_from: std::iter::once(canonical.to_string())
                .chain(aliases.iter().map(|s| s.to_string()))
                .collect(),
            user_confirmed: true,
            dna_status: DnaStatus::Pending,
        }
    }

    fn mention(surface: &str, quote: &str, conf: Confidence) -> CharacterMention {
        CharacterMention {
            surface: surface.into(),
            role_hint: String::new(),
            evidence: vec![MentionEvidence {
                kind: EvidenceKind::Action,
                quote: quote.into(),
                note: String::new(),
                confidence: conf,
            }],
        }
    }

    #[test]
    fn attributes_evidence_by_alias_and_persists() {
        let fs: Arc<dyn HostFs> = Arc::new(MemFs::default());
        let roster = vec![entry("林黛玉", &["黛玉"]), entry("宝玉", &[])];
        let discoveries = vec![ChapterDiscovery {
            chapter_index: 0,
            mentions: vec![
                mention("黛玉", "黛玉葬花", Confidence::High), // 通过别名归入林黛玉
                mention("宝玉", "宝玉摔玉", Confidence::Medium),
                mention("路人", "路人甲", Confidence::Low), // 不属任何角色
            ],
        }];
        let offsets = vec![(0usize, 1000usize)];
        let ledgers = build_ledgers(&fs, 42, "src-hash", &roster, &discoveries, &offsets).unwrap();

        let dai = &ledgers[0].0;
        assert_eq!(dai.character_id, stable_key("林黛玉"));
        assert_eq!(dai.evidence.len(), 1);
        assert_eq!(dai.evidence[0].quote_preview, "黛玉葬花");
        assert_eq!(dai.evidence[0].source_id, "src-hash");
        assert_eq!(dai.evidence[0].chapter_index, 0);

        // 引用完整性前置：id 唯一且非空。
        let ids: BTreeSet<&str> = dai.evidence.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), dai.evidence.len());
        assert!(dai.evidence.iter().all(|e| !e.id.is_empty()));

        // index.content_hash 与落盘文件一致，可回读。
        let index = &ledgers[0].1;
        let loaded = load_ledger(&fs, &stable_key("林黛玉")).unwrap();
        assert_eq!(loaded.evidence.len(), 1);
        let bytes = fs.read(&ledger_path(&stable_key("林黛玉"))).unwrap();
        assert_eq!(index.content_hash, content_hash(&bytes));
        assert_eq!(index.count, 1);
    }

    #[test]
    fn quote_preview_truncated_to_200() {
        let fs: Arc<dyn HostFs> = Arc::new(MemFs::default());
        let long = "字".repeat(500);
        let roster = vec![entry("甲", &[])];
        let discoveries =
            vec![ChapterDiscovery { chapter_index: 0, mentions: vec![mention("甲", &long, Confidence::Low)] }];
        let ledgers = build_ledgers(&fs, 0, "s", &roster, &discoveries, &[(0, 1000)]).unwrap();
        assert_eq!(ledgers[0].0.evidence[0].quote_preview.chars().count(), QUOTE_PREVIEW_MAX);
    }

    #[test]
    fn locator_maps_into_chapter_range() {
        let fs: Arc<dyn HostFs> = Arc::new(MemFs::default());
        let roster = vec![entry("甲", &[])];
        // 第 1 章（index 1）起点 500。
        let discoveries =
            vec![ChapterDiscovery { chapter_index: 1, mentions: vec![mention("甲", "四字证据", Confidence::High)] }];
        let offsets = vec![(0, 500), (500, 900)];
        let ledgers = build_ledgers(&fs, 0, "s", &roster, &discoveries, &offsets).unwrap();
        let loc = &ledgers[0].0.evidence[0].locator;
        assert_eq!(loc.start, 500);
        assert_eq!(loc.end, 504); // 500 + 4 字
    }
}
