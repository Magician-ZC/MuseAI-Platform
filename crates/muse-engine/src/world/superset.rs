//! 超集装配（§防刷 ①）：汇总各维度 → `WorldSkeletonDraft`，做引用完整性收口 +
//! variantGroup 可采样性收口（每组 ≥2 成员）+ 计算 storylines 引用自洽 + sampling 冗余倍率 + `is_superset=true`。

use std::collections::{BTreeMap, BTreeSet};

use super::types::{
    EndingCandidateDraft, ItemDraft, LocationDraft, MainlineNodeDraft, PoolItemDraft,
    SamplingHints, SkeletonSourceDraft, Storyline, WorldCharacterDraft, WorldSkeletonDraft,
    KNOWN_COSMOLOGIES,
};

/// 目标冗余下限（超集量 ÷ 单副本量 ≥ 此值，才够采出内容不同的多副本）。
pub const TARGET_REDUNDANCY: f32 = 3.0;

/// 汇总入参（各维度合成产物）。
pub struct SupersetInput {
    pub source_work: SkeletonSourceDraft,
    pub world_characters: Vec<WorldCharacterDraft>,
    pub locations: Vec<LocationDraft>,
    pub world_items: Vec<ItemDraft>,
    pub mainline_nodes: Vec<MainlineNodeDraft>,
    pub hidden_content_pool: Vec<PoolItemDraft>,
    pub side_hook_pool: Vec<PoolItemDraft>,
    pub ending_pool: Vec<EndingCandidateDraft>,
    pub storylines: Vec<Storyline>,
}

/// 装配超集：引用完整性收口 + variantGroup 收口 + storyline 引用自洽 + sampling 计算。
pub fn assemble(input: SupersetInput) -> WorldSkeletonDraft {
    let SupersetInput {
        source_work,
        world_characters,
        mut locations,
        world_items,
        mut mainline_nodes,
        mut hidden_content_pool,
        mut side_hook_pool,
        mut ending_pool,
        mut storylines,
    } = input;

    let item_ids: BTreeSet<String> = world_items.iter().map(|i| i.id.clone()).collect();
    let loc_ids: BTreeSet<String> = locations.iter().map(|l| l.id.clone()).collect();

    // ---- 引用完整性：地点 connections/residentItemIds/gate 收口 ----
    for loc in locations.iter_mut() {
        loc.connections.retain(|c| loc_ids.contains(c) && c != &loc.id);
        loc.resident_item_ids.retain(|iid| item_ids.contains(iid));
        if let Some(gate) = loc.gate.as_mut() {
            gate.required_item_ids.retain(|iid| item_ids.contains(iid));
            gate.required_cosmologies.retain(|c| KNOWN_COSMOLOGIES.contains(&c.as_str()));
        }
    }

    // ---- 世界角色 carried/home 收口 ----
    let mut world_characters = world_characters;
    for wc in world_characters.iter_mut() {
        wc.carried_item_ids.retain(|iid| item_ids.contains(iid));
        if !wc.home_location.trim().is_empty() && !loc_ids.contains(&wc.home_location) {
            wc.home_location.clear();
        }
    }

    // ---- 池物品 rewardItemRef 悬空 → 清空（装配侧无内联 fallback 时按悬空处理）----
    for pool in [&mut hidden_content_pool, &mut side_hook_pool] {
        for it in pool.iter_mut() {
            if let Some(r) = it.reward_item_ref.as_ref() {
                if !item_ids.contains(r) {
                    it.reward_item_ref = None;
                }
            }
        }
    }

    // ---- variantGroup 可采样性：每组须 ≥2 成员，否则清空该成员的 variantGroup ----
    enforce_variant_groups(&mut mainline_nodes, &mut hidden_content_pool, &mut side_hook_pool, &mut ending_pool);

    // ---- storyline 引用自洽：只保留指向存在元素的 id ----
    let mn_ids: BTreeSet<String> = mainline_nodes.iter().map(|n| n.id.clone()).collect();
    let pool_ids: BTreeSet<String> = hidden_content_pool
        .iter()
        .chain(side_hook_pool.iter())
        .map(|p| p.id.clone())
        .collect();
    let end_ids: BTreeSet<String> = ending_pool.iter().map(|e| e.id.clone()).collect();
    for s in storylines.iter_mut() {
        s.mainline_node_ids.retain(|id| mn_ids.contains(id));
        s.hidden_pool_ids.retain(|id| pool_ids.contains(id));
        s.ending_ids.retain(|id| end_ids.contains(id));
    }

    // ---- sampling：按超集量与目标冗倍率推导单副本抽样量 ----
    let sampling = compute_sampling(
        mainline_nodes.len(),
        hidden_content_pool.len(),
        world_characters.len(),
        locations.len(),
    );

    WorldSkeletonDraft {
        source_work,
        world_characters,
        locations,
        world_items,
        mainline_nodes,
        hidden_content_pool,
        side_hook_pool,
        ending_pool,
        storylines,
        sampling,
        is_superset: true,
    }
}

/// 每个 variantGroup 至少 2 成员：不足者清空成员的 variantGroup（不再是「组」，避免采不出差异）。
fn enforce_variant_groups(
    mainline: &mut [MainlineNodeDraft],
    hidden: &mut [PoolItemDraft],
    side: &mut [PoolItemDraft],
    ending: &mut [EndingCandidateDraft],
) {
    // 跨全维度统计每组成员数。
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for n in mainline.iter() {
        if let Some(g) = &n.variant_group {
            *counts.entry(g.clone()).or_default() += 1;
        }
    }
    for p in hidden.iter().chain(side.iter()) {
        if let Some(g) = &p.variant_group {
            *counts.entry(g.clone()).or_default() += 1;
        }
    }
    for e in ending.iter() {
        if let Some(g) = &e.variant_group {
            *counts.entry(g.clone()).or_default() += 1;
        }
    }
    let keep = |g: &Option<String>| -> bool {
        g.as_ref().map(|k| counts.get(k).copied().unwrap_or(0) >= 2).unwrap_or(false)
    };
    for n in mainline.iter_mut() {
        if !keep(&n.variant_group) {
            n.variant_group = None;
        }
    }
    for p in hidden.iter_mut().chain(side.iter_mut()) {
        if !keep(&p.variant_group) {
            p.variant_group = None;
        }
    }
    for e in ending.iter_mut() {
        if !keep(&e.variant_group) {
            e.variant_group = None;
        }
    }
}

/// 单副本抽样量 = ceil(总量 / 目标冗倍率)，clamp 到 [1, 总量]；redundancy_ratio = 各维度实际倍率的最小值。
fn compute_sampling(mainline: usize, hidden: usize, npc: usize, location: usize) -> SamplingHints {
    let inst = |total: usize| -> usize {
        if total == 0 {
            return 0;
        }
        ((total as f32 / TARGET_REDUNDANCY).ceil() as usize).clamp(1, total)
    };
    let im = inst(mainline);
    let ih = inst(hidden);
    let inpc = inst(npc);
    let iloc = inst(location);
    // 冗倍率取有量纲维度中最保守（最小）的 总量/抽样量。
    let ratios: Vec<f32> = [(mainline, im), (hidden, ih), (npc, inpc), (location, iloc)]
        .into_iter()
        .filter(|(_, i)| *i > 0)
        .map(|(t, i)| t as f32 / i as f32)
        .collect();
    let redundancy_ratio = ratios.into_iter().fold(f32::INFINITY, f32::min);
    let redundancy_ratio = if redundancy_ratio.is_finite() { redundancy_ratio } else { 0.0 };
    SamplingHints {
        instance_mainline_count: im,
        instance_hidden_count: ih,
        instance_npc_count: inpc,
        instance_location_count: iloc,
        redundancy_ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mn(id: &str, vg: Option<&str>) -> MainlineNodeDraft {
        MainlineNodeDraft {
            id: id.into(),
            fated: false,
            variant_group: vg.map(String::from),
            arc_tags: vec![],
        }
    }
    fn pool(id: &str, reward: Option<&str>, vg: Option<&str>) -> PoolItemDraft {
        PoolItemDraft {
            id: id.into(),
            themes: vec![],
            template: String::new(),
            difficulty_base: 0.5,
            reward_item_ref: reward.map(String::from),
            variant_group: vg.map(String::from),
            arc_tags: vec![],
        }
    }
    fn item(id: &str) -> ItemDraft {
        ItemDraft { id: id.into(), ..Default::default() }
    }

    #[test]
    fn drops_dangling_reward_ref_and_singleton_variant_group() {
        let input = SupersetInput {
            source_work: SkeletonSourceDraft::default(),
            world_characters: vec![],
            locations: vec![],
            world_items: vec![item("itm-1")],
            // vg-a 有 2 成员（保留），vg-b 只有 1 成员（清空）。
            mainline_nodes: vec![mn("mn-1", Some("vg-a")), mn("mn-2", Some("vg-a")), mn("mn-3", Some("vg-b"))],
            hidden_content_pool: vec![pool("hc-1", Some("itm-404"), None), pool("hc-2", Some("itm-1"), None)],
            side_hook_pool: vec![],
            ending_pool: vec![],
            storylines: vec![Storyline {
                id: "arc-1".into(),
                summary: String::new(),
                mainline_node_ids: vec!["mn-1".into(), "mn-404".into()], // mn-404 悬空 → 剔除
                hidden_pool_ids: vec!["hc-1".into()],
                ending_ids: vec![],
                affinity: None,
            }],
        };
        let draft = assemble(input);
        assert!(draft.is_superset);
        // 悬空 rewardItemRef 清空；合法保留。
        assert_eq!(draft.hidden_content_pool[0].reward_item_ref, None);
        assert_eq!(draft.hidden_content_pool[1].reward_item_ref, Some("itm-1".to_string()));
        // vg-a 保留（2 成员），vg-b 清空（单成员）。
        assert_eq!(draft.mainline_nodes[0].variant_group, Some("vg-a".to_string()));
        assert_eq!(draft.mainline_nodes[2].variant_group, None);
        // storyline 悬空 mainline id 被剔除。
        assert_eq!(draft.storylines[0].mainline_node_ids, vec!["mn-1".to_string()]);
    }

    #[test]
    fn sampling_ratio_is_correct() {
        // 6 主线段 / 目标冗倍率 3 → 每副本 2，倍率 3.0。
        let s = compute_sampling(6, 3, 3, 3);
        assert_eq!(s.instance_mainline_count, 2);
        assert_eq!(s.instance_hidden_count, 1);
        assert!((s.redundancy_ratio - 3.0).abs() < 1e-6);
    }
}
