//! 开局装配器（S4）：roster-conditioned assembly，平台规格 §9.5.C。
//!
//! 契约：
//! - 输入：world_template.skeleton_json（预审核内容池：主线硬节点序列/结局池/隐藏内容池/支线钩子池
//!   /装配规则）+ 全体入场角色卡（DNA 指标：dramaticCore.coreFear/deniedDesire、agency.plotSeeds/
//!   refusalRules、来源体系、主场标记）；
//! - 动作（实例创建时一次性）：per-character 钩子（每角色 ≥1 个绑定执念/恐惧的隐藏内容，从池中选择
//!   并参数化）、结局分支按阵容加权启用、阵容级参数（支线权重/冲突密度/资源稀缺度）；
//! - 边界：只做「选择 + 参数化」，不自由生成主线；连接文本过 safety::moderate_and_queue 后生效；
//!   装配结果写 worlds.assembled_json 并随实例钉住（§9.2）；个性化内容附难度分标注；
//! - 成本：数次模型调用 + 规则选择，不进 tick 循环。dev/test 走「无模型/占位规则」路径，不发网络。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::admission::ItemDefinition;
use crate::app::AppState;
use crate::db::now_ms;
use crate::error::ApiError;
use crate::providers::ModerationVerdict;
use crate::worlds::load_world;

use muse_engine::character::types::CharacterCardV2;
use muse_engine::narrative::types::{LocationDef, LocationGate};

// ---------- 输出：装配结果（写入 worlds.assembled_json 的 `assembly` 段，随实例钉住） ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssembledInstance {
    pub per_character_hooks: Vec<CharacterHook>,
    pub enabled_endings: Vec<String>,
    pub lineup_params: Value,
    pub difficulty_notes: Vec<String>,
    /// §2.5 主场优劣势：本书角色挂原作预知知识包 + 原作宿命作硬节点（引擎 P1/P2 机制，装配层只标注）。
    #[serde(default)]
    pub home_advantages: Vec<HomeAdvantage>,
    /// 世界固有角色（NPC/反派）装配条目：随实例钉住，runtime 每 tick 注入引擎 active_cards +
    /// world_controlled（不进 members_projection、无日报投影）。空 = 无世界固有角色。
    #[serde(default)]
    pub world_character_entries: Vec<WorldCharacterEntry>,
    /// 地点图（Phase 2）：装配后钉住，runtime 每 tick 读回组装引擎 RoundInput.locations。
    /// 空 = 无地点维度，全体角色单组，退化为单一全局场景。
    #[serde(default)]
    pub location_graph: Vec<LocationDef>,
    /// 地点驻留道具分布（Phase 3）：各地点从 world_items 目录解引用的驻留道具（秘境隐藏道具的单一事实源）。
    /// 空 = 无驻留道具。悬空 id 静默丢弃（与 reward_item_ref/carried 同款防御式），建模板期由引用完整性校验前置拦截。
    #[serde(default)]
    pub resident_items: Vec<ResidentItemGroup>,
    /// 装配采样审计段（防刷第二环）：由固定实例种子驱动的子集采样结果，随实例钉住写入
    /// `worlds.assembled_json` 的 `/assembly/sampling`。**仅服务端 / 审计可见——绝不进 members_projection
    /// 或日报投影**。`None` = 退化路径（非超集旧模板：全量装配、不采样，与改造前行为完全一致）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<InstanceSampling>,
}

/// 装配采样钉住结果（防刷第二环审计段）：种子 + 阵容指纹哈希 + 各维度被选子集 id。
/// 副本内确定（种子由已钉住输入算出，采样纯函数）、副本间不同（world_id 唯一 → 种子唯一）、
/// 可 replay（CAS 写入后读回不重掷）。`seed`/`rosterFingerprint` 仅供服务端审计复算，不外泄。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSampling {
    /// 实例种子（u64 十六进制）：`H(world_id ‖ 阵容指纹 ‖ template_version)`。仅审计，不进任何客户端投影。
    pub seed: String,
    /// 阵容指纹哈希（排序去重 cids 的哈希）：审计用，不回填明文卡。
    pub roster_fingerprint: String,
    pub selected_storylines: Vec<String>,
    /// 被选主线 id（已含全部 fated 硬节点，顺序 = 模板序）。
    pub selected_mainline: Vec<String>,
    pub selected_hidden: Vec<String>,
    pub selected_endings: Vec<String>,
    pub selected_npcs: Vec<String>,
    pub selected_locations: Vec<String>,
    /// 星级封顶剔除清单（波次 3 产出封顶）：奖励道具档位 > 模板星级的隐藏钩子 id（模板序），
    /// 采样前剔除。仅审计（不外泄），`#[serde(default)]` 兼容改造前已钉住的实例回读。
    #[serde(default)]
    pub culled_over_tier: Vec<String>,
    /// 稀有预算剔除清单（波次 3 产出封顶）：入选钩子中奖励档位 ≥ RARE_TIER 超出 RARE_BUDGET
    /// 的部分（确定性序 = 入选模板序，保 replay 一致）。仅审计（不外泄）。
    #[serde(default)]
    pub culled_rare_budget: Vec<String>,
}

/// 地点驻留道具组（Phase 3）：一个地点解引用后的驻留道具集。`is_secret_realm` 标记秘境隐藏道具，
/// 供后续「秘境探索结算 → grant_item_tx 兑现」链路复用（与章节钩子奖励同一幂等发货口径）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidentItemGroup {
    pub location_id: String,
    #[serde(default)]
    pub is_secret_realm: bool,
    pub items: Vec<ItemDefinition>,
}

/// 装配后钉住的世界固有角色条目：runtime 据此把 NPC 卡注入引擎 RoundInput。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldCharacterEntry {
    pub character_id: String,
    pub card: CharacterCardV2,
    /// 初始地点（Phase 1 无地点参与，仅透传）。
    #[serde(default)]
    pub location: String,
    /// 解引用后的携带道具（来自 world_items 目录）。
    #[serde(default)]
    pub carried_items: Vec<ItemDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterHook {
    pub character_id: String,
    pub pool_item_id: String,
    pub parameterized_text: String,
    pub difficulty_score: f32,
    /// 从预审核池挑出的隐藏道具：通关结算（chapters::finish）经 grant_item 兑现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_item: Option<ItemDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeAdvantage {
    pub character_id: String,
    /// 原作预知知识包（挂载标记；实际知识绑定走引擎 P1）。
    pub prescience_pack: bool,
    /// 原作宿命作硬节点 id（引擎 P2 硬节点，装配层标注）。
    pub fated_nodes: Vec<String>,
}

// ---------- 输入：世界模板骨架（预审核内容池 + 装配规则） ----------

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Skeleton {
    #[serde(default)]
    source_work: Option<SkeletonSource>,
    #[serde(default)]
    mainline_nodes: Vec<MainlineNode>,
    #[serde(default)]
    ending_pool: Vec<EndingCandidate>,
    #[serde(default)]
    hidden_content_pool: Vec<PoolItem>,
    #[serde(default)]
    side_hook_pool: Vec<PoolItem>,
    /// 原著固有道具目录（单一事实源）：PoolItem.reward_item_ref 按 id 解引用于此。
    #[serde(default)]
    world_items: Vec<ItemDefinition>,
    /// 世界固有角色（NPC/反派）目录：装配层解引用 + 机审后钉入 worldCharacterEntries，
    /// runtime 每 tick 读回注入引擎（不进日报投影）。空 = 无世界固有角色，退化为纯玩家世界。
    #[serde(default)]
    world_characters: Vec<WorldCharacter>,
    /// 地点图（Phase 2/3）：地点节点 {id,name,connections,isSecretRealm,gate,residentItemIds}。装配后
    /// 拆为引擎 location_graph（LocationDef，丢弃 residentItemIds）+ resident_items 分布。空 = 无地点维度。
    #[serde(default)]
    locations: Vec<LocationSpec>,
    #[serde(default)]
    assembly_rules: AssemblyRules,
    /// 剧情线分组（超集互斥采样单元，防刷第二环）：每条 storyline 引用一组 mainline/hidden/ending id，
    /// 采样时按阵容加权 + 种子扰动选取脊柱子集。空 = 无 storyline 维度（走退化路径）。
    #[serde(default)]
    storylines: Vec<StorylineSpec>,
    /// 副本采样计数提示（每维度每副本抽样量）。全空 = 走退化路径（不采样）。
    #[serde(default)]
    sampling: SamplingSpec,
    /// 超集标记：`true` 且 storylines 非空 且 sampling 非全空 → 走种子采样；否则退化为全量装配。
    #[serde(default)]
    is_superset: bool,
}

/// 剧情线采样单元（对齐 `assets/worlds.rs` StorylineView + affinity）：一条剧情线引用一组
/// mainline/hidden/ending id，并声明阵容倾向（strategist/combat/social）用于加权选取。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StorylineSpec {
    #[serde(default)]
    id: String,
    #[serde(default)]
    mainline_node_ids: Vec<String>,
    #[serde(default)]
    hidden_pool_ids: Vec<String>,
    #[serde(default)]
    ending_ids: Vec<String>,
    /// 阵容倾向：strategist / combat / social / None（无倾向）。
    #[serde(default)]
    affinity: Option<String>,
}

/// 副本采样计数提示（防刷第二环）：每维度每副本抽样量。字段全 `Option` + `#[serde(default)]`，
/// 旧模板缺省 → 全 `None` → 退化路径。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SamplingSpec {
    #[serde(default)]
    instance_storyline_count: Option<usize>,
    #[serde(default)]
    instance_mainline_count: Option<usize>,
    #[serde(default)]
    instance_hidden_count: Option<usize>,
    #[serde(default)]
    instance_npc_count: Option<usize>,
    #[serde(default)]
    instance_location_count: Option<usize>,
}

impl SamplingSpec {
    /// 是否全空（五个计数字段全 `None`）：判退化路径用。
    fn is_empty(&self) -> bool {
        self.instance_storyline_count.is_none()
            && self.instance_mainline_count.is_none()
            && self.instance_hidden_count.is_none()
            && self.instance_npc_count.is_none()
            && self.instance_location_count.is_none()
    }
}

/// 地点骨架条目（Phase 3）：引擎 LocationDef 的 server 侧镜像 + residentItemIds（道具分布）。
/// 装配时拆两路——结构字段转 LocationDef 传引擎，residentItemIds 解引用 world_items 目录成 resident_items。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LocationSpec {
    id: String,
    #[serde(default)]
    name: String,
    /// 可直达地点 id（连通性；建模板期引用完整性校验须指向存在的 location）。
    #[serde(default)]
    connections: Vec<String>,
    /// 秘境标记（可见性隔离由引擎按 location 分组天然实现；此处仅透传 + 标注驻留道具为隐藏）。
    #[serde(default)]
    is_secret_realm: bool,
    /// 准入门槛（秘境用），与引擎 LocationGate 同形。
    #[serde(default)]
    gate: Option<LocationGate>,
    /// 驻留道具对 world_items 目录的引用（装配时解引用为 ItemDefinition）。
    #[serde(default)]
    resident_item_ids: Vec<String>,
}

/// LocationSpec → 引擎 LocationDef：丢弃 residentItemIds（道具分布走 resident_items，不进引擎地点图）。
fn to_location_def(spec: &LocationSpec) -> LocationDef {
    LocationDef {
        id: spec.id.clone(),
        name: spec.name.clone(),
        connections: spec.connections.clone(),
        is_secret_realm: spec.is_secret_realm,
        gate: spec.gate.clone(),
    }
}

/// 世界固有角色（NPC/反派）骨架条目：复用引擎角色卡 + 初始位置 + 携带道具引用 + 议程节点绑定。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorldCharacter {
    /// 复用引擎 DNA 卡（NPC 与玩家角色同构，参与决策/碰撞）。
    card: CharacterCardV2,
    /// 初始地点（Phase 1 无地点参与时仅钉住透传，运行时不据此分组）。
    #[serde(default)]
    home_location: String,
    /// 携带道具对 world_items 目录的引用（装配时解引用为 ItemDefinition）。
    #[serde(default)]
    carried_item_ids: Vec<String>,
    /// 反派主动议程绑定的 mainline 节点 id（透传标注；引擎不特判，靠卡内容驱动决策）。
    /// 采样时用于 NPC 权重：议程命中被选主线的反派更贴合本副本 → 加权入选。
    #[serde(default)]
    agenda_nodes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SkeletonSource {
    #[serde(default)]
    source_id: String,
    #[serde(default)]
    title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MainlineNode {
    id: String,
    #[serde(default)]
    fated: bool,
    /// 变体组：同组成员互斥，采样只保留一个（fated 成员优先）。None = 无组，直通。
    #[serde(default)]
    variant_group: Option<String>,
    /// 归属的 storyline id 集（arcTags）：命中被选 storyline 即入采样候选。
    #[serde(default)]
    arc_tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EndingCandidate {
    id: String,
    /// 结局倾向：strategist / combat / social / None（无条件）。
    #[serde(default)]
    affinity: Option<String>,
    #[serde(default = "one")]
    base_weight: f32,
    /// 变体组：同组结局互斥，采样只保留一个。None = 无组，直通。
    #[serde(default)]
    variant_group: Option<String>,
    /// 归属的 storyline id 集（arcTags）：命中被选 storyline 即入采样候选。
    #[serde(default)]
    arc_tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PoolItem {
    id: String,
    /// 主题标签（与角色执念/恐惧/剧情种子做重叠匹配）。
    #[serde(default)]
    themes: Vec<String>,
    /// 参数化模板（占位符 {name}/{fear}/{desire}/{seed}）。
    #[serde(default)]
    template: String,
    #[serde(default = "half")]
    difficulty_base: f32,
    /// 通关兑现的隐藏道具对 world_items 目录的引用（单一事实源，优先解引用）。
    #[serde(default)]
    reward_item_ref: Option<String>,
    /// 通关兑现的隐藏道具（内联定义，reward_item_ref 缺失/悬空时的兼容 fallback）。
    #[serde(default)]
    reward_item: Option<ItemDefinition>,
    /// 变体组：同组隐藏内容互斥，采样只保留一个。None = 无组，直通。
    #[serde(default)]
    variant_group: Option<String>,
    /// 归属的 storyline id 集（arcTags）：命中被选 storyline 即入采样候选。
    #[serde(default)]
    arc_tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssemblyRules {
    #[serde(default = "one_usize")]
    hidden_per_character: usize,
    #[serde(default = "half")]
    ending_weight_threshold: f32,
}

impl Default for AssemblyRules {
    fn default() -> Self {
        Self { hidden_per_character: 1, ending_weight_threshold: 0.5 }
    }
}

fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}
fn one_usize() -> usize {
    1
}

// ---------- 装配采样（防刷第二环）：固定实例种子 + 确定性整数 PRNG ----------
//
// 种子 = H(world_id ‖ 阵容指纹 ‖ template_version)，全部输入在首次 start 已钉住。采样为纯函数
// （种子 → SplitMix64 整数流 → 按模板 Vec 序消费），结果经 CAS 写入 assembled_json，退出重进读回同一
// 实例不重掷。**禁三样**：系统随机（thread_rng）、浮点 RNG、HashMap/BTreeMap 迭代序驱动 RNG——
// 变体分桶用「首见序 = 模板序」而非 map 序；跨版本一致性由 FNV/SplitMix 测试向量兜底。

/// FNV-1a 64：种子 / 阵容指纹派生（显式常量，跨 Rust 版本稳定，不用 std SipHash/DefaultHasher）。
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// SplitMix64：确定性整数流（每维度独立子流从含 world_id 的全局 seed 派生，杜绝纯阵容维度可被观测反推）。
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// [0, n) 均匀整数（n=0 → 0）。
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

// ---------- 产出封顶（波次 3）：星级封顶 + 稀有预算（常量集中区，可调） ----------

/// 稀有奖励档位下限：奖励道具 powerTier ≥ 此值视为「稀有」，受单实例预算约束。
const RARE_TIER: u8 = 3;
/// 单实例稀有预算：入选钩子中稀有奖励至多 RARE_BUDGET 个，超出的按确定性顺序剔除。
const RARE_BUDGET: usize = 2;

/// 池物品的奖励道具档位（封顶判定口径 = `resolve_reward_item`：reward_item_ref 优先解引用
/// world_items 目录，缺失/悬空回退内联 reward_item——同口径杜绝内联绕过封顶）。无奖励 → None。
fn reward_tier(pool_item: &PoolItem, world_items: &[ItemDefinition]) -> Option<u8> {
    if let Some(ref_id) = pool_item.reward_item_ref.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(def) = world_items.iter().find(|it| it.id == ref_id) {
            return Some(def.origin.power_tier);
        }
    }
    pool_item.reward_item.as_ref().map(|it| it.origin.power_tier)
}

// 各维度子流域常量（seed ^ DOMAIN 派生，避免维度间串扰）。
const DOMAIN_STORYLINE: u64 = 0x51;
const DOMAIN_MAINLINE: u64 = 0x52;
const DOMAIN_HIDDEN: u64 = 0x53;
const DOMAIN_ENDING: u64 = 0x54;
const DOMAIN_NPC: u64 = 0x55;
const DOMAIN_LOC: u64 = 0x56;

/// 权重整数化缩放（避免浮点 RNG / NaN 比较；每项 +1 保底 → 零权项仍最小概率可被选中）。
fn scale_weight(w: f32) -> u64 {
    ((w.max(0.0) as f64) * 1_000_000.0) as u64 + 1
}

/// 按权重选一个（整数化权重，纯整数取模 → 无浮点 RNG / 无 NaN）。空 → None。
fn weighted_pick_one<'a, T>(rng: &mut Rng, items: &[(&'a T, f32)]) -> Option<&'a T> {
    if items.is_empty() {
        return None;
    }
    let scaled: Vec<u64> = items.iter().map(|(_, w)| scale_weight(*w)).collect();
    let total: u64 = scaled.iter().copied().sum();
    if total == 0 {
        return items.first().map(|(t, _)| *t);
    }
    let mut r = rng.next_u64() % total;
    for (i, s) in scaled.iter().enumerate() {
        if r < *s {
            return Some(items[i].0);
        }
        r -= *s;
    }
    items.last().map(|(t, _)| *t)
}

/// 无放回按权重选 k 个，输出保留模板序（逐次整数化加权抽取；全程整数 RNG，权重高者更早入选）。
fn choose_k<'a, T>(rng: &mut Rng, items: &[(&'a T, f32)], k: usize) -> Vec<&'a T> {
    let n = items.len();
    let take = k.min(n);
    if take == 0 {
        return Vec::new();
    }
    if take == n {
        return items.iter().map(|(t, _)| *t).collect(); // 全取，模板序
    }
    let weights: Vec<u64> = items.iter().map(|(_, w)| scale_weight(*w)).collect();
    let mut picked = vec![false; n];
    let mut chosen: Vec<usize> = Vec::with_capacity(take);
    for _ in 0..take {
        let total: u64 = (0..n).filter(|&i| !picked[i]).map(|i| weights[i]).sum();
        if total == 0 {
            break;
        }
        let mut r = rng.next_u64() % total;
        let mut sel: Option<usize> = None;
        for i in 0..n {
            if picked[i] {
                continue;
            }
            if r < weights[i] {
                sel = Some(i);
                break;
            }
            r -= weights[i];
        }
        let idx = match sel.or_else(|| (0..n).find(|&i| !picked[i])) {
            Some(i) => i,
            None => break,
        };
        picked[idx] = true;
        chosen.push(idx);
    }
    chosen.sort_unstable(); // 还原模板序
    chosen.into_iter().map(|idx| items[idx].0).collect()
}

/// 变体组归约：按 variant_group 分桶（**首见序 = 模板序**，非 map 序），每桶 weighted_pick_one 取一个
/// （等权 → 均匀），无组者直通；输出保留模板序。组内候选为空则跳过该组（风险 §9）。
fn resolve_variant_groups<'a, T>(
    rng: &mut Rng,
    items: &[&'a T],
    group_of: impl Fn(&T) -> Option<&str>,
) -> Vec<&'a T> {
    // 分桶：Vec<(组名, Vec<模板下标>)>，按首见序（模板序）append —— 不用 HashMap 驱动 RNG。
    let mut buckets: Vec<(String, Vec<usize>)> = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        if let Some(g) = group_of(*it).map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(b) = buckets.iter_mut().find(|(name, _)| name == g) {
                b.1.push(idx);
            } else {
                buckets.push((g.to_string(), vec![idx]));
            }
        }
    }
    // 每桶按首见序 weighted_pick_one 选一个 winner（等权）。
    let mut winners: std::collections::BTreeSet<usize> = Default::default();
    for (_, members) in &buckets {
        let choices: Vec<(&usize, f32)> = members.iter().map(|i| (i, 1.0)).collect();
        if let Some(w) = weighted_pick_one(rng, &choices) {
            winners.insert(*w);
        }
    }
    // 输出：模板序；无组者直通，有组者仅 winner。
    items
        .iter()
        .enumerate()
        .filter_map(|(idx, it)| {
            let grouped = group_of(*it).map(str::trim).map(|s| !s.is_empty()).unwrap_or(false);
            if !grouped || winners.contains(&idx) {
                Some(*it)
            } else {
                None
            }
        })
        .collect()
}

/// 阵容指纹：排序去重 cid（cloud_character_id）后 `\n` 连接（排序消 joined_at 顺序敏感）。
fn roster_fingerprint(cards: &[(String, CharacterCardV2)]) -> String {
    let mut cids: Vec<&str> = cards.iter().map(|(c, _)| c.as_str()).collect();
    cids.sort_unstable();
    cids.dedup();
    cids.join("\n")
}

/// 全阵容执念词条汇总（隐藏内容采样加权用：贴合阵容执念的隐藏内容更可能入选）。
fn aggregate_obsession_terms(cards: &[(String, CharacterCardV2)]) -> Vec<String> {
    let mut all = Vec::new();
    for (_, c) in cards {
        all.extend(obsession_terms(c));
    }
    all
}

/// 实例种子：`fnv1a_64(world_id ‖ 0x01 ‖ 阵容指纹 ‖ 0x01 ‖ template_version)`。
fn instance_seed(world_id: &str, fingerprint: &str, template_version: i64) -> u64 {
    fnv1a_64(format!("{world_id}\u{1}{fingerprint}\u{1}{template_version}").as_bytes())
}

/// storyline 阵容加权 boost（复用 weight_endings 的阵容画像口径）。
fn affinity_boost(affinity: &Option<String>, profile: &(u32, u32, u32)) -> f32 {
    let total = (profile.0 + profile.1 + profile.2).max(1) as f32;
    match affinity.as_deref() {
        Some("strategist") => profile.0 as f32 / total,
        Some("combat") => profile.1 as f32 / total,
        Some("social") => profile.2 as f32 / total,
        _ => 0.0,
    }
}

/// 一次装配的被选子集（+ 钉住审计段）。`audit == None` → 退化路径（全量，不采样）。
struct Selection {
    audit: Option<InstanceSampling>,
    /// per-character 钩子可用的隐藏内容 id 子集（退化 = 全体，模板序）。
    hidden_ids: Vec<String>,
    /// 最终阵容加权启用的结局 id（对被选候选跑 weight_endings 的结果；退化 = 全体池加权）。
    enabled_endings: Vec<String>,
    /// 被选世界固有角色 id 子集（退化 = 全体）。
    npc_ids: Vec<String>,
    /// 被选地点 id 子集（退化 = 全体）。
    loc_ids: Vec<String>,
}

/// 地点采样（保连通 + 计数上限）：从「含驻留道具的地点 + 被选 NPC 主场」作必选种子，沿 connections
/// 用 rng_loc BFS 扩张到 count（严格 ≤ count 上限），保持连通（只加与已选集相邻的地点）。
/// count 未设 / ≥ 地点数 → 全体（退化）。
fn sample_location_ids(
    rng: &mut Rng,
    locations: &[LocationSpec],
    seed_ids: &[String],
    count: Option<usize>,
) -> Vec<String> {
    let all_ids = || -> Vec<String> { locations.iter().map(|l| l.id.clone()).collect() };
    let Some(count) = count else {
        return all_ids();
    };
    if count == 0 || count >= locations.len() {
        return all_ids();
    }
    let exists: std::collections::BTreeSet<&str> = locations.iter().map(|l| l.id.as_str()).collect();
    let conns = |id: &str| -> Vec<String> {
        locations
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.connections.iter().filter(|c| exists.contains(c.as_str())).cloned().collect())
            .unwrap_or_default()
    };
    // 必选种子（模板序、去重、须存在）。
    let mut selected: Vec<String> = Vec::new();
    for l in locations {
        if seed_ids.iter().any(|s| s == &l.id) && !selected.contains(&l.id) {
            selected.push(l.id.clone());
        }
    }
    if selected.is_empty() {
        if let Some(first) = locations.first() {
            selected.push(first.id.clone()); // 无种子 → 以模板首个地点起步（连通根）。
        }
    }
    // 前沿 = 已选集的相邻未选地点。
    let mut frontier: Vec<String> = Vec::new();
    let extend_frontier = |frontier: &mut Vec<String>, selected: &[String], id: &str| {
        for nb in conns(id) {
            if !selected.contains(&nb) && !frontier.contains(&nb) {
                frontier.push(nb);
            }
        }
    };
    for s in selected.clone() {
        extend_frontier(&mut frontier, &selected, &s);
    }
    // BFS 扩张至 count（保持连通：只加相邻地点）。
    while selected.len() < count && !frontier.is_empty() {
        let pos = rng.below(frontier.len());
        let node = frontier.remove(pos);
        if selected.contains(&node) {
            continue;
        }
        selected.push(node.clone());
        extend_frontier(&mut frontier, &selected, &node);
    }
    // 输出模板序；种子多于 count 的边角（模板配置失当）时保种子、补至 count。
    let sel_set: std::collections::BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    let mut out: Vec<String> =
        locations.iter().filter(|l| sel_set.contains(l.id.as_str())).map(|l| l.id.clone()).collect();
    if out.len() > count {
        let seed_set: std::collections::BTreeSet<&str> = seed_ids.iter().map(String::as_str).collect();
        let mut kept: Vec<String> = out.iter().filter(|id| seed_set.contains(id.as_str())).cloned().collect();
        for id in &out {
            if kept.len() >= count {
                break;
            }
            if !kept.contains(id) {
                kept.push(id.clone());
            }
        }
        let kset: std::collections::BTreeSet<&str> = kept.iter().map(String::as_str).collect();
        out = locations.iter().filter(|l| kset.contains(l.id.as_str())).map(|l| l.id.clone()).collect();
    }
    out
}

/// 纯采样规划（防刷第二环，无 DB / 无系统随机）：由固定实例种子驱动，从超集各池采子集。
/// 退化路径（`is_superset != true` 或 storylines 空 或 sampling 全空）→ `audit=None` + 全量 id（不采样）。
/// `star_rating`（波次 3 产出封顶）：仅超集采样路径生效——奖励档位 > 星级的钩子在采样前剔除 +
/// 入选稀有奖励受 RARE_BUDGET 约束；退化路径不读星级（与改造前行为完全一致）。
fn plan_sampling(
    skeleton: &Skeleton,
    fingerprint: &str,
    world_id: &str,
    template_version: i64,
    profile: &(u32, u32, u32),
    roster_terms: &[String],
    ending_threshold: f32,
    star_rating: i64,
) -> Selection {
    let superset_mode =
        skeleton.is_superset && !skeleton.storylines.is_empty() && !skeleton.sampling.is_empty();
    if !superset_mode {
        // 退化：全量，行为与改造前完全一致，sampling=None。
        return Selection {
            audit: None,
            hidden_ids: skeleton.hidden_content_pool.iter().map(|p| p.id.clone()).collect(),
            enabled_endings: weight_endings(&skeleton.ending_pool, profile, ending_threshold),
            npc_ids: skeleton.world_characters.iter().map(|w| w.card.id.clone()).collect(),
            loc_ids: skeleton.locations.iter().map(|l| l.id.clone()).collect(),
        };
    }

    let seed = instance_seed(world_id, fingerprint, template_version);

    // 1) Storyline 脊柱（阵容依赖 + 种子扰动）。
    let weighted_sl: Vec<(&StorylineSpec, f32)> =
        skeleton.storylines.iter().map(|s| (s, 1.0 + affinity_boost(&s.affinity, profile))).collect();
    let sl_k = skeleton
        .sampling
        .instance_storyline_count
        .unwrap_or(((skeleton.storylines.len() + 1) / 2).max(1));
    let mut rng_sl = Rng(seed ^ DOMAIN_STORYLINE);
    let mut selected_storylines: Vec<&StorylineSpec> = choose_k(&mut rng_sl, &weighted_sl, sl_k);
    if selected_storylines.is_empty() {
        selected_storylines = skeleton.storylines.iter().collect(); // 空 → 全 storylines（兼容无分组超集）。
    }
    let sl_ids: std::collections::BTreeSet<&str> =
        selected_storylines.iter().map(|s| s.id.as_str()).collect();
    let sl_mainline: std::collections::BTreeSet<&str> =
        selected_storylines.iter().flat_map(|s| s.mainline_node_ids.iter().map(String::as_str)).collect();
    let sl_hidden: std::collections::BTreeSet<&str> =
        selected_storylines.iter().flat_map(|s| s.hidden_pool_ids.iter().map(String::as_str)).collect();
    let sl_ending: std::collections::BTreeSet<&str> =
        selected_storylines.iter().flat_map(|s| s.ending_ids.iter().map(String::as_str)).collect();
    let in_arc = |tags: &[String]| tags.iter().any(|t| sl_ids.contains(t.as_str()));

    // 2) Mainline（fated 必留 + 变体组互斥 + 计数上限）。
    let ml_candidates: Vec<&MainlineNode> = skeleton
        .mainline_nodes
        .iter()
        .filter(|n| sl_mainline.contains(n.id.as_str()) || in_arc(&n.arc_tags))
        .collect();
    // fated 组（全池）：这些 variant_group 由 fated 成员占据，非 fated 同组成员排除（避免互斥冲突）。
    let fated_groups: std::collections::BTreeSet<&str> = skeleton
        .mainline_nodes
        .iter()
        .filter(|n| n.fated)
        .filter_map(|n| n.variant_group.as_deref())
        .collect();
    let nonfated_cand: Vec<&MainlineNode> = ml_candidates
        .iter()
        .copied()
        .filter(|n| !n.fated)
        .filter(|n| n.variant_group.as_deref().map(|g| !fated_groups.contains(g)).unwrap_or(true))
        .collect();
    let mut rng_ml = Rng(seed ^ DOMAIN_MAINLINE);
    let resolved_nonfated = resolve_variant_groups(&mut rng_ml, &nonfated_cand, |n| n.variant_group.as_deref());
    let nonfated_final: Vec<&MainlineNode> =
        if let Some(c) = skeleton.sampling.instance_mainline_count {
            let weighted: Vec<(&MainlineNode, f32)> = resolved_nonfated.iter().map(|&n| (n, 1.0)).collect();
            choose_k(&mut rng_ml, &weighted, c)
        } else {
            resolved_nonfated
        };
    // 强制并入全部 fated（宿命硬节点，采样不得裁）→ selected_mainline 按模板原序。
    let sel_ml: std::collections::BTreeSet<&str> = skeleton
        .mainline_nodes
        .iter()
        .filter(|n| n.fated)
        .map(|n| n.id.as_str())
        .chain(nonfated_final.iter().map(|n| n.id.as_str()))
        .collect();
    let mut selected_mainline: Vec<String> = skeleton
        .mainline_nodes
        .iter()
        .filter(|n| sel_ml.contains(n.id.as_str()))
        .map(|n| n.id.clone())
        .collect();
    if selected_mainline.is_empty() {
        if let Some(first) = skeleton.mainline_nodes.first() {
            selected_mainline.push(first.id.clone()); // 保底 ≥1（副本必须可推进）。
        }
    }

    // 3) Hidden content（约束到脊柱 + 星级封顶 + 变体组归约 + 阵容执念加权 + 计数上限 + 稀有预算）。
    // 3a) 星级封顶（产出封顶第一道）：奖励道具档位 > 模板星级的钩子在采样前剔除。
    //     纯候选集过滤（模板序），不动 RNG 消费协议——同种子下剔除结果确定、可 replay。
    let mut culled_over_tier: Vec<String> = Vec::new();
    let mut hidden_candidates: Vec<&PoolItem> = Vec::new();
    for p in skeleton
        .hidden_content_pool
        .iter()
        .filter(|p| sl_hidden.contains(p.id.as_str()) || in_arc(&p.arc_tags))
    {
        match reward_tier(p, &skeleton.world_items) {
            Some(t) if (t as i64) > star_rating => culled_over_tier.push(p.id.clone()),
            _ => hidden_candidates.push(p),
        }
    }
    let mut rng_hidden = Rng(seed ^ DOMAIN_HIDDEN);
    let resolved_hidden = resolve_variant_groups(&mut rng_hidden, &hidden_candidates, |p| p.variant_group.as_deref());
    let weighted_hidden: Vec<(&PoolItem, f32)> = resolved_hidden
        .iter()
        .map(|&p| {
            let (m, _) = score_pool_item(p, roster_terms);
            (p, 1.0 + m as f32)
        })
        .collect();
    let hk = skeleton.sampling.instance_hidden_count.unwrap_or(resolved_hidden.len());
    let selected_hidden_items = choose_k(&mut rng_hidden, &weighted_hidden, hk);
    // 3b) 稀有预算（产出封顶第二道）：入选钩子中奖励档位 ≥ RARE_TIER 的至多 RARE_BUDGET 个，
    //     超出的按入选序（choose_k 已还原模板序）从前往后保留、之后剔除——纯序规则无 RNG，replay 一致。
    let mut culled_rare_budget: Vec<String> = Vec::new();
    let mut rare_kept = 0usize;
    let mut hidden_ids: Vec<String> = Vec::new();
    for p in &selected_hidden_items {
        let rare = reward_tier(p, &skeleton.world_items).map(|t| t >= RARE_TIER).unwrap_or(false);
        if rare {
            if rare_kept >= RARE_BUDGET {
                culled_rare_budget.push(p.id.clone());
                continue;
            }
            rare_kept += 1;
        }
        hidden_ids.push(p.id.clone());
    }

    // 4) Endings（storyline 约束 + 变体组互斥 → 现有 weight_endings 阵容加权）。
    let mut ending_candidates: Vec<&EndingCandidate> = skeleton
        .ending_pool
        .iter()
        .filter(|e| sl_ending.contains(e.id.as_str()) || in_arc(&e.arc_tags))
        .collect();
    if ending_candidates.is_empty() {
        ending_candidates = skeleton.ending_pool.iter().collect(); // 无则全体（副本必须可结束）。
    }
    let mut rng_end = Rng(seed ^ DOMAIN_ENDING);
    let resolved_endings = resolve_variant_groups(&mut rng_end, &ending_candidates, |e| e.variant_group.as_deref());
    let resolved_owned: Vec<EndingCandidate> = resolved_endings.iter().map(|&e| e.clone()).collect();
    let enabled_endings = weight_endings(&resolved_owned, profile, ending_threshold);

    // 5) World characters（NPC）：议程命中被选主线者加权。
    let sel_ml_set: std::collections::BTreeSet<&str> = selected_mainline.iter().map(String::as_str).collect();
    let weighted_npc: Vec<(&WorldCharacter, f32)> = skeleton
        .world_characters
        .iter()
        .map(|wc| {
            let boost = if wc.agenda_nodes.iter().any(|n| sel_ml_set.contains(n.as_str())) { 1.0 } else { 0.0 };
            (wc, 1.0 + boost)
        })
        .collect();
    let nk = skeleton.sampling.instance_npc_count.unwrap_or(skeleton.world_characters.len());
    let mut rng_npc = Rng(seed ^ DOMAIN_NPC);
    let selected_npc_refs = choose_k(&mut rng_npc, &weighted_npc, nk);
    let npc_ids: Vec<String> = selected_npc_refs.iter().map(|wc| wc.card.id.clone()).collect();

    // 6) Locations + resident items（保连通 + 计数）。
    let npc_set: std::collections::BTreeSet<&str> = npc_ids.iter().map(String::as_str).collect();
    let mut loc_seeds: Vec<String> = Vec::new();
    for l in &skeleton.locations {
        if !l.resident_item_ids.is_empty() {
            loc_seeds.push(l.id.clone()); // 含驻留道具（秘境门槛道具单一事实源）→ 必选。
        }
    }
    for wc in &skeleton.world_characters {
        if npc_set.contains(wc.card.id.as_str()) {
            let h = wc.home_location.trim();
            if !h.is_empty() {
                loc_seeds.push(h.to_string()); // 被选 NPC 主场 → 必选。
            }
        }
    }
    let mut rng_loc = Rng(seed ^ DOMAIN_LOC);
    let loc_ids =
        sample_location_ids(&mut rng_loc, &skeleton.locations, &loc_seeds, skeleton.sampling.instance_location_count);

    let audit = InstanceSampling {
        seed: format!("{seed:016x}"),
        roster_fingerprint: format!("{:016x}", fnv1a_64(fingerprint.as_bytes())),
        selected_storylines: selected_storylines.iter().map(|s| s.id.clone()).collect(),
        selected_mainline: selected_mainline.clone(),
        selected_hidden: hidden_ids.clone(),
        selected_endings: enabled_endings.clone(),
        selected_npcs: npc_ids.clone(),
        selected_locations: loc_ids.clone(),
        culled_over_tier,
        culled_rare_budget,
    };
    Selection { audit: Some(audit), hidden_ids, enabled_endings, npc_ids, loc_ids }
}

// ---------- assembled_json 包装（assembly 段钉住 + chapterState 段可变） ----------

/// 章节房实例的可变会话状态初值（章节推进 / 通关 / 已兑现钩子 / 离线收益）。
pub(crate) fn empty_chapter_state() -> Value {
    json!({
        "currentNode": 0,
        "cleared": false,
        "grantedHookIds": [],
        "offlineGains": [],
        "sessionStartedAt": Value::Null,
    })
}

/// 读取 worlds.assembled_json 包装对象；未装配则返回 {assembly:null, chapterState:初值}。
pub(crate) async fn load_wrapper(db: &AnyPool, world_id: &str) -> Result<Value, ApiError> {
    let row = sqlx::query("SELECT assembled_json FROM worlds WHERE id = ?")
        .bind(world_id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let raw: Option<String> = row.try_get("assembled_json")?;
    let parsed = raw
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v.is_object());
    Ok(parsed.unwrap_or_else(|| json!({ "assembly": Value::Null, "chapterState": empty_chapter_state() })))
}

/// 写回 worlds.assembled_json 包装对象。
pub(crate) async fn save_wrapper(db: &AnyPool, world_id: &str, wrapper: &Value) -> Result<(), ApiError> {
    sqlx::query("UPDATE worlds SET assembled_json = ?, updated_at = ? WHERE id = ?")
        .bind(wrapper.to_string())
        .bind(now_ms())
        .bind(world_id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------- 核心：开局装配 ----------

/// 读取骨架 + 全体入场角色卡 → per-character 钩子 / 结局加权 / 阵容参数 / 主场标注。
/// 连接文本过机审后生效，结果写 worlds.assembled_json（钉住）。返回装配结果。
pub async fn assemble_instance(state: &AppState, world_id: &str) -> Result<AssembledInstance, ApiError> {
    let world = load_world(&state.db, world_id).await?;

    // 骨架（预审核池）：缺失/解析失败 → 空池（装配退化为无个性化，但不 panic）。
    // star_rating 同查读出：产出封顶输入 + 快照进 assembled_json（服务端留档）。
    let (skeleton, star_rating) = load_skeleton(&state.db, &world.template_id).await?;

    // 全体在场成员卡。
    let cards = load_active_cards(&state.db, world_id).await?;

    let rules = &skeleton.assembly_rules;
    let profile = roster_profile(&cards);

    // 装配采样（防刷第二环）：固定实例种子 → 从超集各池采子集（退化路径 audit=None，全量装配）。
    // 种子由已钉住输入（world_id + 阵容指纹 + template_version）算出，纯函数、可 replay。
    let fingerprint = roster_fingerprint(&cards);
    let agg_terms = aggregate_obsession_terms(&cards);
    let selection = plan_sampling(
        &skeleton,
        &fingerprint,
        world_id,
        world.template_version,
        &profile,
        &agg_terms,
        rules.ending_weight_threshold,
        star_rating,
    );
    let sampled = selection.audit.is_some();

    // per-character 钩子只在被选隐藏内容子集上跑（退化路径 = 全池，行为不变）。
    let hidden_pool: Vec<PoolItem> = if sampled {
        let set: std::collections::BTreeSet<&str> = selection.hidden_ids.iter().map(String::as_str).collect();
        skeleton.hidden_content_pool.iter().filter(|p| set.contains(p.id.as_str())).cloned().collect()
    } else {
        skeleton.hidden_content_pool.clone()
    };

    let mut hooks: Vec<CharacterHook> = Vec::new();
    let mut difficulty_notes: Vec<String> = Vec::new();
    let mut home_advantages: Vec<HomeAdvantage> = Vec::new();

    for (cid, card) in &cards {
        // 1) per-character 钩子：从（被选）隐藏内容池按执念/恐惧重叠度排序，逐个过机审，只嵌入通过者直到配额。
        let terms = obsession_terms(card);
        let candidates = rank_pool_items(&hidden_pool, &terms);
        let quota = rules.hidden_per_character.max(1);
        let mut embedded = 0usize;
        for (pool_item, matches, matched_term) in candidates {
            if embedded >= quota {
                break;
            }
            let text = parameterize(pool_item, cid, card, matched_term.as_deref());
            let verdict = crate::safety::moderate_and_queue(
                state,
                "assembly_hook",
                &format!("{world_id}:{cid}:{}", pool_item.id),
                &text,
            )
            .await?;
            // S-3：仅 Approved 才嵌入并钉住；Rejected/Pending（含注入命中）一律跳过换下一候选——
            // 不把未复核内容钉进实例（moderate_and_queue 已将 Pending 入人审队列 + 记 risk_events）。
            if verdict != ModerationVerdict::Approved {
                continue;
            }
            let difficulty = (pool_item.difficulty_base + 0.15 * matches as f32).clamp(0.0, 1.0);
            difficulty_notes.push(format!(
                "{cid}:{} 绑定 {matches} 项执念 → difficulty={difficulty:.2}",
                pool_item.id
            ));
            hooks.push(CharacterHook {
                character_id: cid.clone(),
                pool_item_id: pool_item.id.clone(),
                parameterized_text: text,
                difficulty_score: difficulty,
                reward_item: resolve_reward_item(pool_item, &skeleton.world_items),
            });
            embedded += 1;
        }
        // ≥1 目标为 best-effort：池非空且存在过审候选时满足；候选全 Pending/Rejected 时该角色无钩子（安全优先）。

        // 2) 主场优劣势标注：本书角色挂预知知识包 + 原作宿命硬节点。
        if is_home_character(card, skeleton.source_work.as_ref()) {
            let fated: Vec<String> = skeleton
                .mainline_nodes
                .iter()
                .filter(|n| n.fated)
                .map(|n| n.id.clone())
                .collect();
            home_advantages.push(HomeAdvantage {
                character_id: cid.clone(),
                prescience_pack: true,
                fated_nodes: fated,
            });
        }
    }

    // 3) 结局：采样已按 storyline 约束 + 变体组互斥 + 阵容加权算出 enabled_endings（退化 = 全池加权）。
    let enabled_endings = selection.enabled_endings.clone();

    // 4) 阵容级参数：支线权重 / 冲突密度 / 资源稀缺度。
    let roster_size = cards.len();
    let lineup_params = json!({
        "sideQuestWeight": side_quest_weight(&skeleton.side_hook_pool, &cards),
        "conflictDensity": (0.3 + 0.1 * roster_size as f32).min(1.0),
        "resourceScarcity": (0.4 + 0.05 * roster_size as f32).min(1.0),
        "rosterProfile": { "strategist": profile.0, "combat": profile.1, "social": profile.2 },
        "rosterSize": roster_size,
    });

    // 5) 世界固有角色（NPC/反派）装配：仅处理被选 NPC 子集（退化 = 全体）→ 过机审门（与钩子同一 S-3 规则）→
    //    仅 Approved 钉入 worldCharacterEntries。NPC 无 owner、不投影日报；携带道具从 world_items 目录解引用。
    let world_characters_sel: Vec<WorldCharacter> = if sampled {
        let set: std::collections::BTreeSet<&str> = selection.npc_ids.iter().map(String::as_str).collect();
        skeleton.world_characters.iter().filter(|w| set.contains(w.card.id.as_str())).cloned().collect()
    } else {
        skeleton.world_characters.clone()
    };
    let world_character_entries =
        assemble_world_characters(state, world_id, &world_characters_sel, &skeleton.world_items).await?;

    // 6) 地点图（Phase 2）：仅被选地点（退化 = 全体）LocationSpec → 引擎 LocationDef（结构数据，无叙事文本机审需求）。
    //    runtime 每 tick 读回组装引擎 RoundInput.locations。空 = 无地点维度，退化为单一全局场景。
    let locations_sel: Vec<LocationSpec> = if sampled {
        let set: std::collections::BTreeSet<&str> = selection.loc_ids.iter().map(String::as_str).collect();
        skeleton.locations.iter().filter(|l| set.contains(l.id.as_str())).cloned().collect()
    } else {
        skeleton.locations.clone()
    };
    let location_graph: Vec<LocationDef> = locations_sel.iter().map(to_location_def).collect();

    // 6b) 道具分布（Phase 3）：各（被选）地点 residentItemIds 解引用 world_items 目录（悬空 id 静默丢弃）。
    //     秘境（is_secret_realm）驻留道具即隐藏道具，单一事实源锁定在 world_items 目录。
    let resident_items = distribute_resident_items(&locations_sel, &skeleton.world_items);

    let assembled = AssembledInstance {
        per_character_hooks: hooks,
        enabled_endings,
        lineup_params,
        difficulty_notes,
        home_advantages,
        world_character_entries,
        location_graph,
        resident_items,
        sampling: selection.audit,
    };

    // 持久化：assembly 段钉住（含派生的 templateVersion + 装配时模板星级快照），chapterState 段留给章节会话推进。
    let wrapper = json!({
        "assembly": &assembled,
        "chapterState": empty_chapter_state(),
        "templateVersion": world.template_version,
        "starRating": star_rating,
        "assembledAt": now_ms(),
    });

    // C-7：首次装配并发保护——仅当尚未装配（assembled_json IS NULL）时占位写入（CAS）。
    // 输了竞争（已被并发 start 装配写入）→ 复用已持久化结果，避免覆盖导致 chapterState 重置 / 装配发散。
    let claimed = sqlx::query(
        "UPDATE worlds SET assembled_json = ?, updated_at = ? WHERE id = ? AND assembled_json IS NULL",
    )
    .bind(wrapper.to_string())
    .bind(now_ms())
    .bind(world_id)
    .execute(&state.db)
    .await?;
    if claimed.rows_affected() == 0 {
        // 已有装配：读回并复用（两个并发 start 得到一致实例，不重复覆盖）。
        let existing = load_wrapper(&state.db, world_id).await?;
        if let Some(a) = existing
            .get("assembly")
            .filter(|v| v.is_object())
            .and_then(|v| serde_json::from_value::<AssembledInstance>(v.clone()).ok())
        {
            return Ok(a);
        }
        // 兜底：assembled_json 非空但无 assembly 段（非常规路径）→ 强制落本次结果，避免房间卡死。
        save_wrapper(&state.db, world_id, &wrapper).await?;
    }

    Ok(assembled)
}

// ---------- 读取辅助 ----------

/// 读骨架 + 星级（波次 3：star_rating 装配时从 world_templates 读出，供产出封顶并快照进
/// assembled_json）。模板行缺失（测试/历史数据）→ (空骨架, 1★)：退化装配且封顶按最保守档。
async fn load_skeleton(db: &AnyPool, template_id: &str) -> Result<(Skeleton, i64), ApiError> {
    let row = sqlx::query("SELECT skeleton_json, star_rating FROM world_templates WHERE id = ?")
        .bind(template_id)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        return Ok((Skeleton::default(), 1));
    };
    let raw: String = row.try_get("skeleton_json")?;
    let star_rating: i64 = row.try_get("star_rating")?;
    Ok((serde_json::from_str(&raw).unwrap_or_default(), star_rating))
}

async fn load_active_cards(db: &AnyPool, world_id: &str) -> Result<Vec<(String, CharacterCardV2)>, ApiError> {
    let rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, cc.card_json AS card \
         FROM world_members wm JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.world_id = ? AND wm.status = 'active' ORDER BY wm.joined_at ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    let mut cards = Vec::new();
    for r in &rows {
        let cid: String = r.try_get("cid")?;
        let card_json: String = r.try_get("card")?;
        if let Ok(card) = serde_json::from_str::<CharacterCardV2>(&card_json) {
            cards.push((cid, card));
        }
    }
    Ok(cards)
}

// ---------- 装配规则（纯函数，可单测） ----------

/// 解析池物品的奖励道具：优先按 reward_item_ref 从 world_items 目录解引用（单一事实源），
/// ref 缺失或悬空（目录无此 id）时退回内联 reward_item（兼容期 fallback）。
/// 下游 chapter_finish/grant_item_tx 仍只认解出的 ItemDefinition，链路不变。
fn resolve_reward_item(pool_item: &PoolItem, world_items: &[ItemDefinition]) -> Option<ItemDefinition> {
    if let Some(ref_id) = pool_item.reward_item_ref.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(def) = world_items.iter().find(|it| it.id == ref_id) {
            return Some(def.clone());
        }
    }
    pool_item.reward_item.clone()
}

/// 地点驻留道具分布（Phase 3）：逐地点把 residentItemIds 从 world_items 目录解引用为 ItemDefinition。
/// 悬空 id 静默丢弃（与 reward_item_ref/carried_item_ids 同款防御式）；无解出道具的地点不产组。
fn distribute_resident_items(
    locations: &[LocationSpec],
    world_items: &[ItemDefinition],
) -> Vec<ResidentItemGroup> {
    locations
        .iter()
        .filter_map(|spec| {
            let items: Vec<ItemDefinition> = spec
                .resident_item_ids
                .iter()
                .filter_map(|iid| world_items.iter().find(|it| &it.id == iid).cloned())
                .collect();
            if items.is_empty() {
                None
            } else {
                Some(ResidentItemGroup {
                    location_id: spec.id.clone(),
                    is_secret_realm: spec.is_secret_realm,
                    items,
                })
            }
        })
        .collect()
}

/// 建模板期引用完整性校验（Phase 3，`worlds_ops::create_template` 调用）：把 skeleton_json 试解析为
/// `Skeleton`，校验目录引用无悬空——`reward_item_ref`（无内联 fallback 时）/`connections`/`residentItemIds`/
/// `carried_item_ids`/`gate.requiredItemIds` 须能在对应目录（world_items / locations）解引用，
/// `gate.requiredCosmologies` 须 ∈ KNOWN_COSMOLOGIES。返回首个悬空引用的中文说明（Err）或通过（Ok）。
///
/// 宽松边界：解析失败（类型不符）→ Ok（沿用 load_skeleton 的防御式 unwrap_or_default 语义，不因无关字段拦截合法模板）；
/// 只在结构成立时对「明确写了引用」的字段判悬空，避免误伤退化路径（空目录 / 无地点的老模板全部放行）。
pub(crate) fn validate_skeleton_refs(skeleton: &Value) -> Result<(), String> {
    let Ok(sk) = serde_json::from_value::<Skeleton>(skeleton.clone()) else {
        return Ok(()); // 解析不出结构化骨架 → 不做引用校验（防御式，与运行时装配一致）。
    };
    let item_ids: std::collections::BTreeSet<&str> =
        sk.world_items.iter().map(|it| it.id.as_str()).collect();
    let loc_ids: std::collections::BTreeSet<&str> =
        sk.locations.iter().map(|l| l.id.as_str()).collect();

    // 1) 池物品 reward_item_ref：写了 ref 且目录无此 id 且无内联 fallback → 悬空。
    for pool in [&sk.hidden_content_pool, &sk.side_hook_pool] {
        for it in pool {
            if let Some(ref_id) = it.reward_item_ref.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if !item_ids.contains(ref_id) && it.reward_item.is_none() {
                    return Err(format!("rewardItemRef 悬空：池物品 `{}` 引用了不存在的 worldItems.id `{ref_id}`", it.id));
                }
            }
        }
    }

    // 2) 地点：connections 指向存在的 location；residentItemIds / gate.requiredItemIds 指向 world_items；
    //    gate.requiredCosmologies ∈ KNOWN_COSMOLOGIES。
    for loc in &sk.locations {
        for c in &loc.connections {
            if !loc_ids.contains(c.as_str()) {
                return Err(format!("connections 悬空：地点 `{}` 连向不存在的地点 `{c}`", loc.id));
            }
        }
        for iid in &loc.resident_item_ids {
            if !item_ids.contains(iid.as_str()) {
                return Err(format!("residentItemIds 悬空：地点 `{}` 引用了不存在的 worldItems.id `{iid}`", loc.id));
            }
        }
        if let Some(gate) = &loc.gate {
            for iid in &gate.required_item_ids {
                if !item_ids.contains(iid.as_str()) {
                    return Err(format!("gate.requiredItemIds 悬空：地点 `{}` 准入需不存在的 worldItems.id `{iid}`", loc.id));
                }
            }
            for cos in &gate.required_cosmologies {
                if !crate::admission::KNOWN_COSMOLOGIES.contains(&cos.as_str()) {
                    return Err(format!("gate.requiredCosmologies 非法：地点 `{}` 的体系 `{cos}` 不在官方枚举内", loc.id));
                }
            }
        }
    }

    // 3) 世界固有角色 carried_item_ids 指向 world_items；home_location（非空）指向存在的地点。
    for wc in &sk.world_characters {
        for iid in &wc.carried_item_ids {
            if !item_ids.contains(iid.as_str()) {
                return Err(format!("carriedItemIds 悬空：世界角色 `{}` 携带不存在的 worldItems.id `{iid}`", wc.card.id));
            }
        }
        let home = wc.home_location.trim();
        if !home.is_empty() && !loc_ids.contains(home) {
            return Err(format!("homeLocation 悬空：世界角色 `{}` 落在不存在的地点 `{home}`", wc.card.id));
        }
    }

    Ok(())
}

/// 世界固有角色（NPC/反派）装配：逐个过机审门（复用钩子的 S-3 规则——仅 Approved 钉入，
/// Pending/Rejected 跳过不钉），携带道具从 world_items 目录解引用。返回可钉入 assembled_json 的条目集。
async fn assemble_world_characters(
    state: &AppState,
    world_id: &str,
    world_characters: &[WorldCharacter],
    world_items: &[ItemDefinition],
) -> Result<Vec<WorldCharacterEntry>, ApiError> {
    let mut entries: Vec<WorldCharacterEntry> = Vec::new();
    for wc in world_characters {
        let npc_id = wc.card.id.trim().to_string();
        if npc_id.is_empty() {
            continue; // 无 id 的 NPC 无法被 runtime 注入/区分，跳过。
        }
        // S-3：NPC 卡可叙述文本过机审门，仅 Approved 钉入（未复核内容不进实例）。
        let verdict = crate::safety::moderate_and_queue(
            state,
            "assembly_npc",
            &format!("{world_id}:{npc_id}"),
            &npc_scan_text(&wc.card),
        )
        .await?;
        if verdict != ModerationVerdict::Approved {
            continue;
        }
        // 携带道具解引用（悬空 id 静默丢弃，与 reward_item_ref 同款防御式）。
        let carried_items: Vec<ItemDefinition> = wc
            .carried_item_ids
            .iter()
            .filter_map(|iid| world_items.iter().find(|it| &it.id == iid).cloned())
            .collect();
        entries.push(WorldCharacterEntry {
            character_id: npc_id,
            card: wc.card.clone(),
            location: wc.home_location.clone(),
            carried_items,
        });
    }
    Ok(entries)
}

/// NPC 卡的机审文本：拼接可叙述字段（名字 / 核心矛盾 / 表层目标 / 长期议程），供预审核门判定。
fn npc_scan_text(card: &CharacterCardV2) -> String {
    let dc = &card.dramatic_core;
    let mut parts: Vec<String> = Vec::new();
    for s in [
        card.identity.name.as_str(),
        dc.core_contradiction.as_str(),
        dc.surface_goal.as_str(),
        card.agency.long_term_agenda.as_str(),
    ] {
        let t = s.trim();
        if !t.is_empty() {
            parts.push(t.to_string());
        }
    }
    parts.join(" / ")
}

/// 角色执念词条：恐惧 / 被否认的欲望 / 核心矛盾 / 隐藏需求 / 剧情种子 / 拒绝规则。
fn obsession_terms(card: &CharacterCardV2) -> Vec<String> {
    let dc = &card.dramatic_core;
    let mut terms: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let t = s.trim();
        if !t.is_empty() {
            terms.push(t.to_lowercase());
        }
    };
    push(&dc.core_fear);
    if let Some(d) = &dc.denied_desire {
        push(d);
    }
    push(&dc.core_contradiction);
    push(&dc.hidden_need);
    for s in &card.agency.plot_seeds {
        push(s);
    }
    for r in &card.agency.refusal_rules {
        push(r);
    }
    terms
}

/// term 与 theme 是否相关：小写后互为子串，或共享 ≥2 长度的 ASCII 词元。
fn related(term: &str, theme: &str) -> bool {
    let theme = theme.trim().to_lowercase();
    if theme.is_empty() {
        return false;
    }
    if term.contains(&theme) || theme.contains(term) {
        return true;
    }
    let split = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 2)
            .map(|w| w.to_string())
            .collect()
    };
    let tw = split(term);
    split(&theme).iter().any(|w| tw.iter().any(|x| x == w))
}

/// 为一个池物品打分：命中的执念词条数 + 首个命中的词条（用于参数化时的绑定展示）。
fn score_pool_item(pool_item: &PoolItem, terms: &[String]) -> (usize, Option<String>) {
    let mut matches = 0usize;
    let mut matched_term: Option<String> = None;
    for term in terms {
        if pool_item.themes.iter().any(|th| related(term, th)) {
            matches += 1;
            if matched_term.is_none() {
                matched_term = Some(term.clone());
            }
        }
    }
    (matches, matched_term)
}

/// 按命中执念数降序排列全部候选（稳定序保留池内原顺序作平手）；池空 → 空。
/// 不预截断——调用方按配额 + 机审逐个嵌入，Pending/Rejected 跳过换下一候选（S-3）。
fn rank_pool_items<'a>(
    pool: &'a [PoolItem],
    terms: &[String],
) -> Vec<(&'a PoolItem, usize, Option<String>)> {
    let mut scored: Vec<(&PoolItem, usize, Option<String>)> = pool
        .iter()
        .map(|p| {
            let (m, term) = score_pool_item(p, terms);
            (p, m, term)
        })
        .collect();
    // 命中数降序；稳定排序保留池内原顺序作为平手序。
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored
}

/// 参数化连接文本：填充模板并显式嵌入绑定的执念词条（保证可验证的执念绑定）。
fn parameterize(
    pool_item: &PoolItem,
    character_id: &str,
    card: &CharacterCardV2,
    matched_term: Option<&str>,
) -> String {
    let name = if card.identity.name.trim().is_empty() {
        character_id
    } else {
        card.identity.name.trim()
    };
    let fear = card.dramatic_core.core_fear.trim();
    let desire = card.dramatic_core.denied_desire.as_deref().unwrap_or("").trim();
    let seed = card.agency.plot_seeds.first().map(|s| s.as_str()).unwrap_or("").trim();

    // 绑定词条：优先用命中的执念词条，否则退回恐惧 / 核心矛盾 / 首个剧情种子。
    let binding = matched_term
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| non_empty(fear))
        .or_else(|| non_empty(card.dramatic_core.core_contradiction.trim()))
        .or_else(|| non_empty(seed))
        .unwrap_or_else(|| "未言明的执念".into());

    let base = if pool_item.template.trim().is_empty() {
        format!("围绕「{binding}」展开的隐藏支线")
    } else {
        pool_item
            .template
            .replace("{name}", name)
            .replace("{fear}", fear)
            .replace("{desire}", desire)
            .replace("{seed}", seed)
    };
    format!("{base}（{name} · 执念绑定：{binding}）")
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// 阵容画像：统计三类原型（谋略 / 战斗 / 社交）在全体成员上的倾向计数。
fn roster_profile(cards: &[(String, CharacterCardV2)]) -> (u32, u32, u32) {
    const STRAT: &[&str] = &["谋", "策", "算", "智", "计", "布局", "strateg", "plan", "cunning", "mind"];
    const COMBAT: &[&str] = &["战", "斗", "武", "杀", "力量", "暴力", "combat", "fight", "force", "attack"];
    const SOCIAL: &[&str] = &["社", "说服", "关系", "魅", "情谊", "结盟", "social", "persuad", "charm", "ally"];
    let mut acc = (0u32, 0u32, 0u32);
    for (_, card) in cards {
        let mut blob = String::new();
        let dm = &card.decision_model;
        blob.push_str(&dm.risk_appetite);
        for s in &dm.value_priorities {
            blob.push_str(s);
        }
        for s in &dm.default_strategies {
            blob.push_str(s);
        }
        blob.push_str(&card.dramatic_core.core_contradiction);
        blob.push_str(&card.agency.long_term_agenda);
        let blob = blob.to_lowercase();
        let hit = |kw: &[&str]| kw.iter().any(|k| blob.contains(k));
        if hit(STRAT) {
            acc.0 += 1;
        }
        if hit(COMBAT) {
            acc.1 += 1;
        }
        if hit(SOCIAL) {
            acc.2 += 1;
        }
    }
    acc
}

/// 结局池按阵容加权：weight = base_weight * (1 + 该倾向占比)；≥ 阈值则启用，保底至少启用权重最高者。
fn weight_endings(pool: &[EndingCandidate], profile: &(u32, u32, u32), threshold: f32) -> Vec<String> {
    if pool.is_empty() {
        return Vec::new();
    }
    let total = (profile.0 + profile.1 + profile.2).max(1) as f32;
    let boost = |aff: &Option<String>| -> f32 {
        match aff.as_deref() {
            Some("strategist") => profile.0 as f32 / total,
            Some("combat") => profile.1 as f32 / total,
            Some("social") => profile.2 as f32 / total,
            _ => 0.0,
        }
    };
    let mut weighted: Vec<(&EndingCandidate, f32)> =
        pool.iter().map(|e| (e, e.base_weight * (1.0 + boost(&e.affinity)))).collect();
    let enabled: Vec<String> =
        weighted.iter().filter(|(_, w)| *w >= threshold).map(|(e, _)| e.id.clone()).collect();
    if !enabled.is_empty() {
        return enabled;
    }
    // 保底：无一过阈值时启用权重最高的单个结局（副本必须可结束）。
    weighted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    weighted.first().map(|(e, _)| vec![e.id.clone()]).unwrap_or_default()
}

/// 支线权重：随支线钩子池容量与阵容剧情种子密度上调。
fn side_quest_weight(side_pool: &[PoolItem], cards: &[(String, CharacterCardV2)]) -> f32 {
    let seeds: usize = cards.iter().map(|(_, c)| c.agency.plot_seeds.len()).sum();
    let base = 0.3 + 0.02 * side_pool.len() as f32 + 0.03 * seeds as f32;
    base.min(1.0)
}

/// 主场判定：角色卡来源作品与骨架声明的源作品一致（source_id 或 title 匹配）。
fn is_home_character(card: &CharacterCardV2, source: Option<&SkeletonSource>) -> bool {
    let Some(src) = source else {
        return false;
    };
    let Some(sw) = &card.identity.source_work else {
        return false;
    };
    let eq = |a: &str, b: &str| !a.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim());
    eq(&sw.source_id, &src.source_id) || eq(&sw.title, &src.title)
}

// 供内部/测试构造收益条目复用。
pub(crate) fn build_offline_gain(character_id: &str, kind: &str, summary: &str) -> Value {
    json!({
        "characterId": character_id,
        "kind": kind,
        "summary": summary,
        "createdAt": now_ms(),
        "claimed": false,
    })
}

// ---------- 装配采样纯函数单测（防刷第二环；无 DB / 无系统随机） ----------

#[cfg(test)]
mod sampling_tests {
    use super::*;
    use std::collections::BTreeSet;

    const PROFILE: (u32, u32, u32) = (1, 1, 1);

    /// 超集骨架：3 storylines（默认采 2）；mainline 含 fated + 两个变体组；hidden/ending 各含变体组；
    /// 4 地点（含一个秘境，驻留道具）。计数上限压到子集。
    fn superset() -> Skeleton {
        serde_json::from_value(serde_json::json!({
            "isSuperset": true,
            "storylines": [
                { "id": "arc-A", "affinity": "combat",     "mainlineNodeIds": ["mn-a1","mn-a2"], "hiddenPoolIds": ["hc-a1","hc-a2"], "endingIds": ["end-a1","end-a2"] },
                { "id": "arc-B", "affinity": "social",     "mainlineNodeIds": ["mn-b1","mn-b2"], "hiddenPoolIds": ["hc-b1"],         "endingIds": ["end-b1"] },
                { "id": "arc-C", "affinity": "strategist", "mainlineNodeIds": ["mn-c1"],         "hiddenPoolIds": ["hc-c1"],         "endingIds": ["end-c1"] }
            ],
            "mainlineNodes": [
                { "id": "mn-fate", "fated": true, "arcTags": ["arc-A","arc-B","arc-C"] },
                { "id": "mn-a1", "variantGroup": "vg1", "arcTags": ["arc-A"] },
                { "id": "mn-a2", "variantGroup": "vg1", "arcTags": ["arc-A"] },
                { "id": "mn-b1", "variantGroup": "vg2", "arcTags": ["arc-B"] },
                { "id": "mn-b2", "variantGroup": "vg2", "arcTags": ["arc-B"] },
                { "id": "mn-c1", "arcTags": ["arc-C"] }
            ],
            "hiddenContentPool": [
                { "id": "hc-a1", "variantGroup": "vh", "arcTags": ["arc-A"], "themes": ["a"] },
                { "id": "hc-a2", "variantGroup": "vh", "arcTags": ["arc-A"], "themes": ["a"] },
                { "id": "hc-b1", "arcTags": ["arc-B"], "themes": ["b"] },
                { "id": "hc-c1", "arcTags": ["arc-C"], "themes": ["c"] }
            ],
            "endingPool": [
                { "id": "end-a1", "variantGroup": "ve", "arcTags": ["arc-A"], "affinity": "combat" },
                { "id": "end-a2", "variantGroup": "ve", "arcTags": ["arc-A"], "affinity": "combat" },
                { "id": "end-b1", "arcTags": ["arc-B"], "affinity": "social" },
                { "id": "end-c1", "arcTags": ["arc-C"], "affinity": "strategist" }
            ],
            "worldItems": [
                { "id": "wi", "narrative": "秘境道具", "effectTags": [], "origin": { "worldTemplateId": "t", "cosmology": ["myth"], "powerTier": 1 } }
            ],
            "locations": [
                { "id": "loc-hub", "connections": ["loc-a","loc-b"] },
                { "id": "loc-a", "connections": ["loc-hub","loc-secret"] },
                { "id": "loc-secret", "isSecretRealm": true, "connections": ["loc-a"], "residentItemIds": ["wi"] },
                { "id": "loc-b", "connections": ["loc-hub"] }
            ],
            "sampling": { "instanceMainlineCount": 2, "instanceHiddenCount": 1, "instanceLocationCount": 2 }
        }))
        .unwrap()
    }

    /// 默认按 5★ 规划（不触发星级封顶）：既有采样测试聚焦随机性协议，封顶另有专项（star_cap_tests）。
    fn plan(sk: &Skeleton, world_id: &str, fp: &str) -> Selection {
        plan_star(sk, world_id, fp, 5)
    }

    fn plan_star(sk: &Skeleton, world_id: &str, fp: &str, star: i64) -> Selection {
        plan_sampling(sk, fp, world_id, 1, &PROFILE, &[], 0.5, star)
    }

    // #10 PRNG 测试向量：锁死跨版本一致性（FNV-1a / SplitMix64 均为规范实现）。
    #[test]
    fn prng_test_vectors() {
        assert_eq!(fnv1a_64(b"museai"), 0xd2b6_e20e_3fd2_d255);
        let mut r = Rng(fnv1a_64(b"museai"));
        assert_eq!(r.next_u64(), 0x0f17_9d52_19b9_fab1);
        assert_eq!(r.next_u64(), 0xc458_c510_8aff_a280);
        assert_eq!(r.next_u64(), 0x25e6_26b7_137b_99c7);
        // 规范 SplitMix64 seed=0 首输出（实现自证）。
        assert_eq!(Rng(0).next_u64(), 0xe220_a839_7b1d_cdaf);
    }

    // #1 副本内确定：同 (world_id, roster, template) 连调两次 → 逐字段一致。
    #[test]
    fn same_seed_same_sampling() {
        let sk = superset();
        let a = plan(&sk, "world_fixed_1", "cidA\ncidB");
        let b = plan(&sk, "world_fixed_1", "cidA\ncidB");
        let (sa, sb) = (a.audit.unwrap(), b.audit.unwrap());
        assert_eq!(sa.seed, sb.seed);
        assert_eq!(sa.selected_storylines, sb.selected_storylines);
        assert_eq!(sa.selected_mainline, sb.selected_mainline);
        assert_eq!(sa.selected_hidden, sb.selected_hidden);
        assert_eq!(sa.selected_endings, sb.selected_endings);
        assert_eq!(sa.selected_locations, sb.selected_locations);
    }

    // #2 副本间不同：不同 world_id、同阵容同模板 → 采样有差异（多实例统计覆盖）。
    #[test]
    fn different_instance_different_sampling() {
        let sk = superset();
        let sigs: BTreeSet<String> = (0..12)
            .map(|i| {
                let s = plan(&sk, &format!("world_inst_{i}"), "cidA\ncidB").audit.unwrap();
                format!("{}|{}|{}", s.selected_storylines.join(","), s.selected_mainline.join(","), s.selected_hidden.join(","))
            })
            .collect();
        assert!(sigs.len() >= 2, "不同实例应采出内容不同的副本，实得 {} 种", sigs.len());
    }

    // #4 阵容敏感：换一张卡 → 指纹变 → 种子变 → 采样（大概率）不同。
    #[test]
    fn roster_fingerprint_changes_seed() {
        let sk = superset();
        let a = plan(&sk, "world_fixed_2", "cidA\ncidB").audit.unwrap();
        let b = plan(&sk, "world_fixed_2", "cidA\ncidB\ncidC").audit.unwrap();
        assert_ne!(a.seed, b.seed, "阵容指纹变 → 种子必变");
        assert_ne!(a.roster_fingerprint, b.roster_fingerprint);
    }

    // #5 fated 必留：任意种子下 selected_mainline ⊇ {fated 节点}。
    #[test]
    fn fated_always_retained() {
        let sk = superset();
        for i in 0..16 {
            let s = plan(&sk, &format!("world_fate_{i}"), "cidA").audit.unwrap();
            assert!(s.selected_mainline.contains(&"mn-fate".to_string()), "seed {i} 漏了 fated 硬节点");
        }
    }

    // #6 变体组互斥：各 variantGroup 在选中集内 ≤1 成员（mainline/hidden/ending 三处）。
    #[test]
    fn variant_groups_exclusive() {
        let sk = superset();
        for i in 0..16 {
            let s = plan(&sk, &format!("world_vg_{i}"), "cidA").audit.unwrap();
            let count = |ids: &[String], group: &[&str]| group.iter().filter(|g| ids.iter().any(|x| x == *g)).count();
            assert!(count(&s.selected_mainline, &["mn-a1", "mn-a2"]) <= 1, "vg1 互斥破坏: {:?}", s.selected_mainline);
            assert!(count(&s.selected_mainline, &["mn-b1", "mn-b2"]) <= 1, "vg2 互斥破坏: {:?}", s.selected_mainline);
            assert!(count(&s.selected_hidden, &["hc-a1", "hc-a2"]) <= 1, "vh 互斥破坏: {:?}", s.selected_hidden);
            assert!(count(&s.selected_endings, &["end-a1", "end-a2"]) <= 1, "ve 互斥破坏: {:?}", s.selected_endings);
        }
    }

    // #7 脊柱自洽：selected_hidden ⊆ 所选 storyline 的 hiddenPoolIds。
    #[test]
    fn hidden_subset_of_selected_storylines() {
        let sk = superset();
        for i in 0..16 {
            let s = plan(&sk, &format!("world_spine_{i}"), "cidA").audit.unwrap();
            let allowed: BTreeSet<String> = sk
                .storylines
                .iter()
                .filter(|sl| s.selected_storylines.contains(&sl.id))
                .flat_map(|sl| sl.hidden_pool_ids.clone())
                .collect();
            for h in &s.selected_hidden {
                assert!(allowed.contains(h), "seed {i} 选了脊柱外的隐藏内容 {h}（allowed={allowed:?}）");
            }
        }
    }

    // #8 计数上限：hidden ≤ count；location ≤ count；mainline 非 fated 部分 ≤ count。
    #[test]
    fn count_caps_respected() {
        let sk = superset();
        for i in 0..16 {
            let s = plan(&sk, &format!("world_cap_{i}"), "cidA").audit.unwrap();
            assert!(s.selected_hidden.len() <= 1, "hidden 超上限: {:?}", s.selected_hidden);
            assert!(s.selected_locations.len() <= 2, "location 超上限: {:?}", s.selected_locations);
            let nonfated = s.selected_mainline.iter().filter(|id| *id != "mn-fate").count();
            assert!(nonfated <= 2, "mainline 非 fated 超上限: {:?}", s.selected_mainline);
        }
    }

    // 秘境保连通：被选秘境 loc-secret 必伴随其通路 loc-a（避免孤立秘境，风险 §3）。
    #[test]
    fn secret_realm_stays_connected() {
        let sk = superset();
        for i in 0..16 {
            let s = plan(&sk, &format!("world_loc_{i}"), "cidA").audit.unwrap();
            if s.selected_locations.contains(&"loc-secret".to_string()) {
                assert!(
                    s.selected_locations.contains(&"loc-a".to_string()),
                    "秘境 loc-secret 入选但通路 loc-a 未入选（孤立秘境）: {:?}",
                    s.selected_locations
                );
            }
        }
    }

    // #9 退化：非超集模板 → 全量 + sampling=None（与改造前一致）。
    #[test]
    fn non_superset_degrades_to_full() {
        // 无 isSuperset / storylines / sampling 的旧骨架。
        let sk: Skeleton = serde_json::from_value(serde_json::json!({
            "mainlineNodes": [ { "id": "n1", "fated": true }, { "id": "n2" } ],
            "hiddenContentPool": [ { "id": "h1", "themes": ["x"] }, { "id": "h2", "themes": ["y"] } ],
            "endingPool": [ { "id": "e1", "baseWeight": 1.0 } ],
            "locations": [ { "id": "l1" }, { "id": "l2" } ]
        }))
        .unwrap();
        let s = plan(&sk, "world_degrade", "cidA");
        assert!(s.audit.is_none(), "退化路径不产采样审计段");
        assert_eq!(s.hidden_ids, vec!["h1".to_string(), "h2".to_string()], "退化 = 全量隐藏池");
        assert_eq!(s.loc_ids, vec!["l1".to_string(), "l2".to_string()], "退化 = 全量地点");
        assert_eq!(s.enabled_endings, vec!["e1".to_string()], "退化 = 全池阵容加权");
    }

    // is_superset=true 但 sampling 全空 → 仍退化（三判据之一不满足）。
    #[test]
    fn superset_flag_without_sampling_degrades() {
        let mut sk = superset();
        sk.sampling = SamplingSpec::default();
        let s = plan(&sk, "world_nosampling", "cidA");
        assert!(s.audit.is_none(), "sampling 全空 → 退化");
    }

    // ---------- 波次 3 产出封顶：星级封顶 + 稀有预算（确定性可重放） ----------

    /// 封顶专用超集：单 storyline 全含；隐藏池 5 项——hr-1(ref t1)、hr-3a(ref t3)、
    /// hr-3b(**内联** t3，验证内联不绕过封顶口径)、hr-4(ref t4)、hr-5(ref t5)；
    /// instanceHiddenCount=9（全取）隔离封顶效果——被剔除的只可能是封顶所为。
    fn reward_superset() -> Skeleton {
        let item = |id: &str, tier: u8| {
            serde_json::json!({
                "id": id, "narrative": format!("道具{id}"), "effectTags": [],
                "origin": { "worldTemplateId": "t", "cosmology": ["myth"], "powerTier": tier }
            })
        };
        serde_json::from_value(serde_json::json!({
            "isSuperset": true,
            "storylines": [
                { "id": "arc-R", "mainlineNodeIds": ["mn-1"],
                  "hiddenPoolIds": ["hr-1","hr-3a","hr-3b","hr-4","hr-5"], "endingIds": ["end-1"] }
            ],
            "mainlineNodes": [ { "id": "mn-1", "fated": true, "arcTags": ["arc-R"] } ],
            "hiddenContentPool": [
                { "id": "hr-1",  "arcTags": ["arc-R"], "rewardItemRef": "wi-t1" },
                { "id": "hr-3a", "arcTags": ["arc-R"], "rewardItemRef": "wi-t3" },
                { "id": "hr-3b", "arcTags": ["arc-R"],
                  "rewardItem": { "id": "inline-t3", "narrative": "内联稀有", "effectTags": [],
                    "origin": { "worldTemplateId": "t", "cosmology": ["myth"], "powerTier": 3 } } },
                { "id": "hr-4",  "arcTags": ["arc-R"], "rewardItemRef": "wi-t4" },
                { "id": "hr-5",  "arcTags": ["arc-R"], "rewardItemRef": "wi-t5" }
            ],
            "endingPool": [ { "id": "end-1", "arcTags": ["arc-R"] } ],
            "worldItems": [ item("wi-t1", 1), item("wi-t3", 3), item("wi-t4", 4), item("wi-t5", 5) ],
            "sampling": { "instanceStorylineCount": 1, "instanceHiddenCount": 9 }
        }))
        .unwrap()
    }

    // 星级封顶：2★ 模板 → 奖励档位 >2 的钩子（ref 与内联同口径）采样前剔除，仅留 tier1。
    #[test]
    fn star_cap_culls_over_tier_rewards_on_two_star() {
        let sk = reward_superset();
        let s = plan_star(&sk, "world_star2", "cidA", 2).audit.unwrap();
        assert_eq!(s.selected_hidden, vec!["hr-1".to_string()], "2★ 只可留 tier≤2 奖励钩子");
        assert_eq!(
            s.culled_over_tier,
            vec!["hr-3a".to_string(), "hr-3b".to_string(), "hr-4".to_string(), "hr-5".to_string()],
            "tier3/4/5（含内联）应全数进星级剔除清单（模板序）"
        );
        assert!(s.culled_rare_budget.is_empty(), "星级已剔净，稀有预算无事可做");
    }

    // 5★ 模板：星级封顶全放行（tier≤5），稀有预算兜底——tier≥3 至多 RARE_BUDGET=2，超出按确定性序剔除。
    #[test]
    fn five_star_keeps_tiers_within_rare_budget() {
        let sk = reward_superset();
        let s = plan_star(&sk, "world_star5", "cidA", 5).audit.unwrap();
        assert!(s.culled_over_tier.is_empty(), "5★ 无档位越界");
        assert_eq!(
            s.selected_hidden,
            vec!["hr-1".to_string(), "hr-3a".to_string(), "hr-3b".to_string()],
            "tier≥3 只保留前 2 个（模板序），tier1 不占预算"
        );
        assert_eq!(
            s.culled_rare_budget,
            vec!["hr-4".to_string(), "hr-5".to_string()],
            "超预算稀有按确定性序剔除"
        );
    }

    // 确定性可重放：同种子两次规划 → 选中集与两份剔除清单逐字段一致；
    // 计数上限压到 3（choose_k 真吃 RNG）跨多实例仍恒守稀有预算。
    #[test]
    fn cap_culling_is_deterministic_and_replayable() {
        let mut sk = reward_superset();
        sk.sampling.instance_hidden_count = Some(3);
        for i in 0..12 {
            let wid = format!("world_replay_{i}");
            let a = plan_star(&sk, &wid, "cidA\ncidB", 5).audit.unwrap();
            let b = plan_star(&sk, &wid, "cidA\ncidB", 5).audit.unwrap();
            assert_eq!(a.selected_hidden, b.selected_hidden, "同种子两次装配选中集必须一致");
            assert_eq!(a.culled_over_tier, b.culled_over_tier, "星级剔除清单必须可重放");
            assert_eq!(a.culled_rare_budget, b.culled_rare_budget, "稀有预算剔除清单必须可重放");
            let rare_count = a
                .selected_hidden
                .iter()
                .filter(|id| id.as_str() != "hr-1") // 除 tier1 外全为 tier≥3
                .count();
            assert!(rare_count <= RARE_BUDGET, "实例 {i} 稀有奖励超预算: {:?}", a.selected_hidden);
        }
    }

    // 退化路径不读星级：非超集模板带高档奖励 + 1★ → 全量装配无剔除（与改造前行为完全一致）。
    #[test]
    fn degraded_path_ignores_star_cap() {
        let sk: Skeleton = serde_json::from_value(serde_json::json!({
            "worldItems": [ { "id": "wi-t5", "narrative": "神器", "effectTags": [],
                "origin": { "worldTemplateId": "t", "cosmology": ["myth"], "powerTier": 5 } } ],
            "hiddenContentPool": [
                { "id": "h1", "themes": ["x"], "rewardItemRef": "wi-t5" },
                { "id": "h2", "themes": ["y"] }
            ],
            "endingPool": [ { "id": "e1", "baseWeight": 1.0 } ]
        }))
        .unwrap();
        let s = plan_star(&sk, "world_degrade_star", "cidA", 1);
        assert!(s.audit.is_none(), "退化路径不产采样审计段");
        assert_eq!(
            s.hidden_ids,
            vec!["h1".to_string(), "h2".to_string()],
            "退化路径不读星级：tier5 奖励钩子照旧全量装配"
        );
    }
}
