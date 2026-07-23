//! 重要度分层（规格 §10.2 阶段 5）：打分规则 + 模型复核边界情况（≤1 次调用）。
//! 文件所有权：agent-E1。

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{ChapterDiscovery, RosterEntry, RosterTier};
use super::CharacterPrompts;

// 打分权重：出场章节数 > 关系中心性 > 证据条数。
const W_APPEAR: i64 = 3;
const W_EVID: i64 = 1;
const W_COOCCUR: i64 = 2;

// 分层累计分位（按得分排名）：Core 前 15%、Major 前 40%、Functional 前 75%、其余 Extra。
const CUT_CORE: f64 = 0.15;
const CUT_MAJOR: f64 = 0.40;
const CUT_FUNC: f64 = 0.75;

/// 规则打分：出场章节数、证据条数、共现角色数（关系中心性近似）。
/// 初分层：得分 top 分位 → Core/Major/Functional/Extra（阈值内联注释说明，必测边界）。
/// 副作用：按得分从高到低重排 roster（便于 UI 展示与边界复核）。
pub fn score_and_tier(roster: &mut [RosterEntry], discoveries: &[ChapterDiscovery]) {
    let n = roster.len();
    if n == 0 {
        return;
    }
    // 每角色称呼集合。
    let sets: Vec<BTreeSet<&str>> = roster
        .iter()
        .map(|e| {
            e.aliases
                .iter()
                .chain(e.merged_from.iter())
                .map(String::as_str)
                .chain(std::iter::once(e.canonical_name.as_str()))
                .collect()
        })
        .collect();

    let mut appear: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); n];
    let mut evid: Vec<i64> = vec![0; n];
    let mut chap_present: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
    for d in discoveries {
        for m in &d.mentions {
            for (ei, set) in sets.iter().enumerate() {
                if set.contains(m.surface.as_str()) {
                    appear[ei].insert(d.chapter_index);
                    evid[ei] += m.evidence.len() as i64;
                    chap_present.entry(d.chapter_index).or_default().insert(ei);
                }
            }
        }
    }
    let mut cooccur: Vec<i64> = vec![0; n];
    for (ei, item) in cooccur.iter_mut().enumerate() {
        let mut others: BTreeSet<usize> = BTreeSet::new();
        for ch in &appear[ei] {
            if let Some(present) = chap_present.get(ch) {
                for &oj in present {
                    if oj != ei {
                        others.insert(oj);
                    }
                }
            }
        }
        *item = others.len() as i64;
    }

    // 汇总打分与统计，按 key 归档（排序后仍可查）。
    let mut score_by_key: BTreeMap<String, i64> = BTreeMap::new();
    let mut stat_by_key: BTreeMap<String, (usize, i64)> = BTreeMap::new(); // (出场章数, 证据数)
    for (ei, e) in roster.iter().enumerate() {
        let score = appear[ei].len() as i64 * W_APPEAR + evid[ei] * W_EVID + cooccur[ei] * W_COOCCUR;
        score_by_key.insert(e.key.clone(), score);
        stat_by_key.insert(e.key.clone(), (appear[ei].len(), evid[ei]));
    }

    // 按得分降序重排（并列：证据多者、字典序小者优先）。
    roster.sort_by(|a, b| {
        let sa = score_by_key[&a.key];
        let sb = score_by_key[&b.key];
        sb.cmp(&sa)
            .then(stat_by_key[&b.key].1.cmp(&stat_by_key[&a.key].1))
            .then(a.canonical_name.cmp(&b.canonical_name))
    });

    let core_cut = ((n as f64) * CUT_CORE).ceil() as usize;
    let major_cut = ((n as f64) * CUT_MAJOR).ceil() as usize;
    let func_cut = ((n as f64) * CUT_FUNC).ceil() as usize;
    for (rank, e) in roster.iter_mut().enumerate() {
        let (chaps, ev) = stat_by_key[&e.key];
        // 过场判定：至多一次出场且证据 ≤1，直接 Extra，不占用高层名额。
        e.tier = if chaps <= 1 && ev <= 1 {
            RosterTier::Extra
        } else if rank < core_cut {
            RosterTier::Core
        } else if rank < major_cut {
            RosterTier::Major
        } else if rank < func_cut {
            RosterTier::Functional
        } else {
            RosterTier::Extra
        };
    }
}

/// 模型复核：只对边界带（相邻层分差 < 15%）的角色发一次批量判定；
/// 模型只能在相邻层间移动角色（越级输出丢弃）。
///
/// 实现说明：score_and_tier 已按得分重排 roster，故层级在序列上连续；
/// 取每处层级跳变两侧的相邻角色作为边界带候选（缺乏持久化分数，以「跨切分点的相邻角色」近似 <15% 带）。
pub async fn review_boundaries(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &CharacterPrompts,
    run_id: &str,
    roster: &mut Vec<RosterEntry>,
    cancel: &CancelFlag,
) -> Result<(), EngineError> {
    if roster.len() < 2 {
        return Ok(());
    }
    // 边界候选：相邻两项层级不同 → 两者都入候选。
    let mut boundary: BTreeSet<String> = BTreeSet::new();
    for w in roster.windows(2) {
        if w[0].tier != w[1].tier {
            boundary.insert(w[0].key.clone());
            boundary.insert(w[1].key.clone());
        }
    }
    if boundary.is_empty() {
        return Ok(()); // 无边界 → 不烧模型调用
    }

    let candidates: Vec<&RosterEntry> = roster.iter().filter(|e| boundary.contains(&e.key)).collect();
    let user = build_tier_prompt(&candidates);
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.tiering_system.clone(),
        user,
        temperature: 0.0,
        max_output_tokens: 1024,
        agent: "characterTiering".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };
    let resp: TierResponse = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;

    let cur: BTreeMap<String, RosterTier> =
        roster.iter().map(|e| (e.key.clone(), e.tier)).collect();
    for adj in resp.adjustments {
        // 引用完整性：只接受边界候选；且只允许相邻层移动（越级丢弃）。
        if !boundary.contains(&adj.key) {
            continue;
        }
        let Some(&from) = cur.get(&adj.key) else { continue };
        if (rank_of(from) as i32 - rank_of(adj.tier) as i32).abs() != 1 {
            continue; // 越级
        }
        if let Some(e) = roster.iter_mut().find(|e| e.key == adj.key) {
            e.tier = adj.tier;
        }
    }
    Ok(())
}

fn rank_of(t: RosterTier) -> u8 {
    match t {
        RosterTier::Core => 0,
        RosterTier::Major => 1,
        RosterTier::Functional => 2,
        RosterTier::Extra => 3,
    }
}

fn build_tier_prompt(candidates: &[&RosterEntry]) -> String {
    let mut lines = String::new();
    for e in candidates {
        lines.push_str(&format!(
            "- key={} 姓名={} 当前层级={}\n",
            e.key,
            e.canonical_name,
            tier_name(e.tier)
        ));
    }
    format!(
        "以下角色处于重要度分层的边界，请复核其层级是否恰当。层级从高到低：core > major > functional > extra。\n\
{lines}\n\
严格输出 JSON：{{\"adjustments\":[{{\"key\":\"角色key\",\"tier\":\"core|major|functional|extra\"}}]}}\n\
规则：只能针对上面列出的 key；每个角色至多上调或下调一个相邻层级，禁止跨层调整；无需调整的角色不要列出。"
    )
}

fn tier_name(t: RosterTier) -> &'static str {
    match t {
        RosterTier::Core => "core",
        RosterTier::Major => "major",
        RosterTier::Functional => "functional",
        RosterTier::Extra => "extra",
    }
}

#[derive(Deserialize)]
struct TierResponse {
    #[serde(default)]
    adjustments: Vec<TierAdjustment>,
}

#[derive(Deserialize)]
struct TierAdjustment {
    key: String,
    tier: RosterTier,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::merge::stable_key;
    use crate::character::types::{
        CharacterMention, Confidence, DnaStatus, EvidenceKind, MentionEvidence,
    };
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use std::sync::Arc;

    fn entry(canonical: &str) -> RosterEntry {
        RosterEntry {
            key: stable_key(canonical),
            canonical_name: canonical.into(),
            aliases: vec![],
            tier: RosterTier::Functional,
            merged_from: vec![canonical.into()],
            user_confirmed: false,
            dna_status: DnaStatus::Pending,
        }
    }

    fn ev_n(n: usize) -> Vec<MentionEvidence> {
        (0..n)
            .map(|_| MentionEvidence {
                kind: EvidenceKind::Action,
                quote: "q".into(),
                note: String::new(),
                confidence: Confidence::High,
            })
            .collect()
    }

    fn m(surface: &str, ev: usize) -> CharacterMention {
        CharacterMention { surface: surface.into(), role_hint: String::new(), evidence: ev_n(ev) }
    }

    fn tier_of<'a>(roster: &'a [RosterEntry], canonical: &str) -> RosterTier {
        roster.iter().find(|e| e.canonical_name == canonical).unwrap().tier
    }

    #[test]
    fn frequent_hero_is_core_oneoff_is_extra() {
        let mut roster = vec![entry("主角"), entry("配角"), entry("龙套"), entry("路人")];
        // 主角出现在 4 章、证据多、共现广；路人只出场一次一条证据。
        let discoveries = vec![
            ChapterDiscovery { chapter_index: 0, mentions: vec![m("主角", 5), m("配角", 3)] },
            ChapterDiscovery { chapter_index: 1, mentions: vec![m("主角", 4), m("配角", 2), m("龙套", 1)] },
            ChapterDiscovery { chapter_index: 2, mentions: vec![m("主角", 4), m("龙套", 1)] },
            ChapterDiscovery { chapter_index: 3, mentions: vec![m("主角", 3), m("路人", 1)] },
        ];
        score_and_tier(&mut roster, &discoveries);
        assert_eq!(tier_of(&roster, "主角"), RosterTier::Core);
        assert_eq!(tier_of(&roster, "路人"), RosterTier::Extra); // 单次出场过场
        // 重排后首位应为主角。
        assert_eq!(roster[0].canonical_name, "主角");
    }

    #[tokio::test]
    async fn review_drops_cross_level_keeps_adjacent() {
        // 构造两层边界：A=Core、B=Functional（相邻项层级不同 → 均入边界）。
        let mut roster = vec![entry("A"), entry("B")];
        let discoveries = vec![
            ChapterDiscovery { chapter_index: 0, mentions: vec![m("A", 6), m("B", 2)] },
            ChapterDiscovery { chapter_index: 1, mentions: vec![m("A", 6), m("B", 2)] },
        ];
        score_and_tier(&mut roster, &discoveries);
        let a_tier = tier_of(&roster, "A");
        let b_tier = tier_of(&roster, "B");
        assert_ne!(a_tier, b_tier); // 确有边界

        // 模型：把 A 越级降到 extra（丢弃），把 B 相邻上调一级（应用）。
        let b_up = match rank_of(b_tier) {
            r if r > 0 => tier_name(match r - 1 {
                0 => RosterTier::Core,
                1 => RosterTier::Major,
                _ => RosterTier::Functional,
            }),
            _ => "core",
        };
        let resp = format!(
            r#"{{"adjustments":[{{"key":"{}","tier":"extra"}},{{"key":"{}","tier":"{}"}}]}}"#,
            stable_key("A"),
            stable_key("B"),
            b_up
        );
        let host = EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(0)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(ScriptedModel::new(vec![Ok(resp)])),
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
        review_boundaries(&host, &profile, &prompts, "t", &mut roster, &CancelFlag::new()).await.unwrap();
        // A 的越级调整被丢弃，层级不变。
        assert_eq!(tier_of(&roster, "A"), a_tier);
        // B 被相邻上调。
        assert_eq!(rank_of(tier_of(&roster, "B")), rank_of(b_tier) - 1);
    }

    #[tokio::test]
    async fn review_no_boundary_skips_model_call() {
        // 单一角色 → 无相邻边界 → 不调用模型（空脚本若被调用会报错）。
        let mut roster = vec![entry("独")];
        let host = EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(0)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(ScriptedModel::new(vec![])),
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
        review_boundaries(&host, &profile, &prompts, "t", &mut roster, &CancelFlag::new()).await.unwrap();
    }
}
