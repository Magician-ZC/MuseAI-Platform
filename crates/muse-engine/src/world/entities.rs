//! location/item 维度归并与合成（仿 `character::merge`/`synthesis`，实体无角色分层/敬称启发式）。
//!
//! 归并：union-find 身份归并（exact + 包含关系）+ `stable_key` 复用；模型归并强制引用完整性（越界丢弃）。
//! 合成：地点图 / 道具目录，各一次模型调用；`origin.cosmology` 夹回 `KNOWN_COSMOLOGIES`，`powerTier` clamp 1–5。

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::character::merge::stable_key;
use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{ItemDraft, LocationDraft, WorldRosterEntry, KNOWN_COSMOLOGIES};

/// 单实体归并输入：文中称呼 + 秘境提示（location 用）。
#[derive(Debug, Clone)]
pub struct EntityMention {
    pub surface: String,
    pub is_secret_realm: bool,
}

/// 每批模型判定的候选上限。
const MERGE_BATCH_MAX: usize = 40;

/// 规则归并（无模型调用）：完全同名 + 包含关系（短名 ≥2 字且为长名连续子串）union-find 归簇。
/// 返回 (已归并条目, 未决簇)。秘境标记按簇内任一成员命中即置位。
pub fn rule_merge_entities(mentions: &[EntityMention]) -> (Vec<WorldRosterEntry>, Vec<Vec<String>>) {
    let mut freq: BTreeMap<String, u32> = BTreeMap::new();
    let mut secret: BTreeSet<String> = BTreeSet::new();
    for m in mentions {
        let s = m.surface.trim();
        if s.is_empty() {
            continue;
        }
        *freq.entry(s.to_string()).or_default() += 1;
        if m.is_secret_realm {
            secret.insert(s.to_string());
        }
    }
    let surfaces: Vec<String> = freq.keys().cloned().collect();
    let n = surfaces.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }

    let mut uf = UnionFind::new(n);
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

    let mut clusters: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        clusters.entry(uf.find(i)).or_default().push(i);
    }

    let mut resolved: Vec<WorldRosterEntry> = Vec::new();
    for members in clusters.values() {
        let names: Vec<String> = members.iter().map(|&i| surfaces[i].clone()).collect();
        resolved.push(make_entry(&names, &freq, &secret));
    }
    // 实体无角色敬称歧义启发式：规则阶段不产生未决簇（模型归并留给显式歧义场景）。
    (resolved, Vec::new())
}

/// 模型归并：对未决簇分批判定同一性；输出仅允许引用输入中出现过的 surface（越界丢弃）。
pub async fn model_merge_entities(
    host: &EngineHost,
    profile: &ModelProfile,
    merge_system: &str,
    prompt_version: &str,
    run_id: &str,
    unresolved: Vec<Vec<String>>,
    secret_surfaces: &BTreeSet<String>,
    cancel: &CancelFlag,
) -> Result<Vec<WorldRosterEntry>, EngineError> {
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
    let mut entries: Vec<WorldRosterEntry> = Vec::new();
    for batch in pool.chunks(MERGE_BATCH_MAX) {
        let allowed: BTreeSet<&str> = batch.iter().map(|s| s.as_str()).collect();
        let user = build_merge_prompt(batch);
        let spec = ModelCallSpec {
            max_retries: None,
            profile: profile.clone(),
            system: merge_system.to_string(),
            user,
            temperature: 0.0,
            max_output_tokens: 2048,
            agent: "worldEntityMerge".to_string(),
            prompt_version: prompt_version.to_string(),
            run_id: run_id.to_string(),
        };
        let resp: MergeResponse =
            json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
        for group in resp.groups {
            let mut names: Vec<String> =
                group.surfaces.into_iter().filter(|s| allowed.contains(s.as_str())).collect();
            names.sort();
            names.dedup();
            if names.is_empty() {
                continue;
            }
            entries.push(make_entry(&names, &freq, secret_surfaces));
        }
    }
    Ok(entries)
}

fn build_merge_prompt(batch: &[String]) -> String {
    let mut lines = String::new();
    for s in batch {
        lines.push_str(&format!("- {s}\n"));
    }
    format!(
        "以下是同一部作品中出现的世界实体称呼候选，请判断哪些指向同一个实体（同一地点/同一道具）。\n\
候选清单：\n{lines}\n\
严格输出 JSON：{{\"groups\":[{{\"canonicalName\":\"本名\",\"surfaces\":[\"同属该实体的称呼\"]}}]}}\n\
要求：surfaces 只能来自上面的候选清单，禁止臆造未列出的名字；无法确定同一性的称呼各自独立成组。"
    )
}

/// 由簇成员构造 WorldRosterEntry：canonical 取最长（并列取高频、再取字典序）。
fn make_entry(members: &[String], freq: &BTreeMap<String, u32>, secret: &BTreeSet<String>) -> WorldRosterEntry {
    let canonical = members
        .iter()
        .max_by(|a, b| {
            a.chars()
                .count()
                .cmp(&b.chars().count())
                .then(freq.get(*a).copied().unwrap_or(0).cmp(&freq.get(*b).copied().unwrap_or(0)))
                .then(b.as_str().cmp(a.as_str()))
        })
        .cloned()
        .unwrap_or_default();
    let aliases: Vec<String> = members.iter().filter(|m| **m != canonical).cloned().collect();
    let mut merged_from = members.to_vec();
    merged_from.sort();
    let is_secret_realm = members.iter().any(|m| secret.contains(m));
    WorldRosterEntry {
        key: stable_key(&canonical),
        canonical_name: canonical,
        aliases,
        merged_from,
        user_confirmed: false,
        is_secret_realm,
    }
}

// ---------- 合成 ----------

/// 合成道具目录：一次模型调用。cosmology 夹回 KNOWN_COSMOLOGIES（越界剔除，空则 mundane），powerTier clamp 1–5。
pub async fn synthesize_items(
    host: &EngineHost,
    profile: &ModelProfile,
    system: &str,
    prompt_version: &str,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    roster: &[WorldRosterEntry],
    source_title: &str,
    cancel: &CancelFlag,
) -> Result<Vec<ItemDraft>, EngineError> {
    if roster.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<String> = roster.iter().map(|e| e.canonical_name.clone()).collect();
    let user = format!(
        "作品：{title}\n以下是从原文提取、经人工确认的道具/法宝清单：\n{list}\n\n\
为每个道具合成结构化定义，严格输出 JSON：\n\
{{\"worldItems\":[{{\"id\":\"itm-唯一标识\",\"narrative\":\"叙事外皮\",\"effectTags\":[\"advantage:combat\"],\
\"origin\":{{\"cosmology\":[\"cultivation\"],\"powerTier\":4}}}}]}}\n\
要求：id 全局唯一且非空；cosmology 每项须 ∈ {cos:?}；powerTier 为 1–5 的整数；只合成清单中的道具。",
        title = source_title,
        list = names.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n"),
        cos = KNOWN_COSMOLOGIES,
    );
    let spec = ModelCallSpec {
        max_retries: None,
        profile: profile.clone(),
        system: system.to_string(),
        user,
        temperature,
        max_output_tokens,
        agent: "worldItemSynthesis".to_string(),
        prompt_version: prompt_version.to_string(),
        run_id: run_id.to_string(),
    };
    let resp: ItemSynthResponse =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    let mut out: Vec<ItemDraft> = Vec::new();
    for mut it in resp.world_items {
        if it.id.trim().is_empty() {
            continue;
        }
        // cosmology 夹回白名单；空则 mundane。
        it.origin.cosmology.retain(|c| KNOWN_COSMOLOGIES.contains(&c.as_str()));
        if it.origin.cosmology.is_empty() {
            it.origin.cosmology.push("mundane".to_string());
        }
        it.origin.power_tier = it.origin.power_tier.clamp(1, 5);
        out.push(it);
    }
    Ok(out)
}

/// 合成地点图：一次模型调用。可用道具 id 供 residentItemIds/gate 引用（越界引用在超集装配时剔除）。
/// 秘境标记：模型输出 ∪ roster 提示。
pub async fn synthesize_locations(
    host: &EngineHost,
    profile: &ModelProfile,
    system: &str,
    prompt_version: &str,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    roster: &[WorldRosterEntry],
    item_ids: &[String],
    source_title: &str,
    cancel: &CancelFlag,
) -> Result<Vec<LocationDraft>, EngineError> {
    if roster.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<String> = roster.iter().map(|e| e.canonical_name.clone()).collect();
    let user = format!(
        "作品：{title}\n以下是从原文提取、经人工确认的地点/秘境清单：\n{list}\n\
可引用的道具 id：{items:?}\n\n\
为每个地点合成节点，严格输出 JSON：\n\
{{\"locations\":[{{\"id\":\"loc-唯一标识\",\"name\":\"地点名\",\"connections\":[\"loc-其它\"],\
\"isSecretRealm\":true,\"gate\":{{\"requiredItemIds\":[],\"requiredCosmologies\":[\"cultivation\"],\"maxPowerTier\":4}},\
\"residentItemIds\":[\"itm-xxx\"]}}]}}\n\
要求：id 全局唯一且非空；connections/residentItemIds 只引用已存在的地点/道具；秘境须置 isSecretRealm=true。",
        title = source_title,
        list = names.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n"),
        items = item_ids,
    );
    let spec = ModelCallSpec {
        max_retries: None,
        profile: profile.clone(),
        system: system.to_string(),
        user,
        temperature,
        max_output_tokens,
        agent: "worldLocationSynthesis".to_string(),
        prompt_version: prompt_version.to_string(),
        run_id: run_id.to_string(),
    };
    let resp: LocationSynthResponse =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    // 秘境提示：roster 中标注秘境的 canonical_name 集合。
    let secret_names: BTreeSet<&str> =
        roster.iter().filter(|e| e.is_secret_realm).map(|e| e.canonical_name.as_str()).collect();
    let mut out: Vec<LocationDraft> = Vec::new();
    for mut loc in resp.locations {
        if loc.id.trim().is_empty() {
            continue;
        }
        if secret_names.contains(loc.name.as_str()) {
            loc.is_secret_realm = true;
        }
        // gate cosmology 夹回白名单。
        if let Some(gate) = loc.gate.as_mut() {
            gate.required_cosmologies.retain(|c| KNOWN_COSMOLOGIES.contains(&c.as_str()));
        }
        out.push(loc);
    }
    Ok(out)
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
    #[serde(default)]
    surfaces: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemSynthResponse {
    #[serde(default)]
    world_items: Vec<ItemDraft>,
}

#[derive(Deserialize)]
struct LocationSynthResponse {
    #[serde(default)]
    locations: Vec<LocationDraft>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use std::sync::Arc;

    fn em(surface: &str, secret: bool) -> EntityMention {
        EntityMention { surface: surface.into(), is_secret_realm: secret }
    }

    fn host_with(model: ScriptedModel) -> EngineHost {
        EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(0)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(model),
        }
    }

    fn profile() -> ModelProfile {
        ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "u".into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    #[test]
    fn containment_merges_and_carries_secret_flag() {
        // 「剑冢」并入「无尽剑冢」；秘境标记随簇上浮。
        let (resolved, unresolved) =
            rule_merge_entities(&[em("无尽剑冢", true), em("剑冢", false), em("无尽剑冢", true)]);
        assert!(unresolved.is_empty());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].canonical_name, "无尽剑冢");
        assert!(resolved[0].aliases.contains(&"剑冢".to_string()));
        assert!(resolved[0].is_secret_realm);
    }

    #[tokio::test]
    async fn model_merge_enforces_referential_integrity() {
        // 模型输出越界 surface「诛仙台」，必须被丢弃。
        let resp = r#"{"groups":[{"canonicalName":"剑冢","surfaces":["剑冢入口","剑冢深处","诛仙台"]}]}"#;
        let host = host_with(ScriptedModel::new(vec![Ok(resp.into())]));
        let entries = model_merge_entities(
            &host,
            &profile(),
            "sys",
            "v1",
            "wt",
            vec![vec!["剑冢入口".into(), "剑冢深处".into()]],
            &BTreeSet::new(),
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(entries.len(), 1);
        let mf: BTreeSet<String> = entries[0].merged_from.iter().cloned().collect();
        assert_eq!(mf, BTreeSet::from(["剑冢入口".to_string(), "剑冢深处".to_string()]));
    }

    #[tokio::test]
    async fn item_synthesis_clamps_cosmology_and_tier() {
        // cosmology 含越界「chaos」被剔除；powerTier 9 clamp 到 5；第二个道具体系全越界 → mundane 兜底。
        let resp = r#"{"worldItems":[
            {"id":"itm-fenji","narrative":"焚寂","effectTags":["advantage:combat"],
             "origin":{"cosmology":["cultivation","chaos"],"powerTier":9}},
            {"id":"itm-x","narrative":"凡铁","effectTags":[],"origin":{"cosmology":["chaos"],"powerTier":0}}
        ]}"#;
        let host = host_with(ScriptedModel::new(vec![Ok(resp.into())]));
        let roster = vec![
            WorldRosterEntry {
                key: "k1".into(),
                canonical_name: "焚寂剑".into(),
                aliases: vec![],
                merged_from: vec!["焚寂剑".into()],
                user_confirmed: true,
                is_secret_realm: false,
            },
            WorldRosterEntry {
                key: "k2".into(),
                canonical_name: "凡铁".into(),
                aliases: vec![],
                merged_from: vec!["凡铁".into()],
                user_confirmed: true,
                is_secret_realm: false,
            },
        ];
        let items = synthesize_items(&host, &profile(), "sys", "v1", 0.0, 2048, "wt", &roster, "书", &CancelFlag::new())
            .await
            .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].origin.cosmology, vec!["cultivation".to_string()]); // chaos 剔除
        assert_eq!(items[0].origin.power_tier, 5); // clamp
        assert_eq!(items[1].origin.cosmology, vec!["mundane".to_string()]); // 全越界 → 兜底
        assert_eq!(items[1].origin.power_tier, 1); // 0 clamp 到 1
    }
}
