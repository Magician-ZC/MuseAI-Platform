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
    #[serde(default)]
    #[allow(dead_code)]
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
    let skeleton = load_skeleton(&state.db, &world.template_id).await?;

    // 全体在场成员卡。
    let cards = load_active_cards(&state.db, world_id).await?;

    let rules = &skeleton.assembly_rules;
    let profile = roster_profile(&cards);

    let mut hooks: Vec<CharacterHook> = Vec::new();
    let mut difficulty_notes: Vec<String> = Vec::new();
    let mut home_advantages: Vec<HomeAdvantage> = Vec::new();

    for (cid, card) in &cards {
        // 1) per-character 钩子：从隐藏内容池按执念/恐惧重叠度排序，逐个过机审，只嵌入通过者直到配额。
        let terms = obsession_terms(card);
        let candidates = rank_pool_items(&skeleton.hidden_content_pool, &terms);
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

    // 3) 结局池按阵容加权启用。
    let enabled_endings = weight_endings(&skeleton.ending_pool, &profile, rules.ending_weight_threshold);

    // 4) 阵容级参数：支线权重 / 冲突密度 / 资源稀缺度。
    let roster_size = cards.len();
    let lineup_params = json!({
        "sideQuestWeight": side_quest_weight(&skeleton.side_hook_pool, &cards),
        "conflictDensity": (0.3 + 0.1 * roster_size as f32).min(1.0),
        "resourceScarcity": (0.4 + 0.05 * roster_size as f32).min(1.0),
        "rosterProfile": { "strategist": profile.0, "combat": profile.1, "social": profile.2 },
        "rosterSize": roster_size,
    });

    // 5) 世界固有角色（NPC/反派）装配：解引用 world_characters → 过机审门（与钩子同一 S-3 规则）→
    //    仅 Approved 钉入 worldCharacterEntries。NPC 无 owner、不投影日报；携带道具从 world_items 目录解引用。
    let world_character_entries =
        assemble_world_characters(state, world_id, &skeleton.world_characters, &skeleton.world_items).await?;

    // 6) 地点图（Phase 2）：LocationSpec → 引擎 LocationDef（结构数据，无叙事文本机审需求）。
    //    runtime 每 tick 读回组装引擎 RoundInput.locations。空 = 无地点维度，退化为单一全局场景。
    let location_graph: Vec<LocationDef> = skeleton.locations.iter().map(to_location_def).collect();

    // 6b) 道具分布（Phase 3）：各地点 residentItemIds 解引用 world_items 目录（悬空 id 静默丢弃）。
    //     秘境（is_secret_realm）驻留道具即隐藏道具，单一事实源锁定在 world_items 目录。
    let resident_items = distribute_resident_items(&skeleton.locations, &skeleton.world_items);

    let assembled = AssembledInstance {
        per_character_hooks: hooks,
        enabled_endings,
        lineup_params,
        difficulty_notes,
        home_advantages,
        world_character_entries,
        location_graph,
        resident_items,
    };

    // 持久化：assembly 段钉住（含派生的 templateVersion），chapterState 段留给章节会话推进。
    let wrapper = json!({
        "assembly": &assembled,
        "chapterState": empty_chapter_state(),
        "templateVersion": world.template_version,
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

async fn load_skeleton(db: &AnyPool, template_id: &str) -> Result<Skeleton, ApiError> {
    let row = sqlx::query("SELECT skeleton_json FROM world_templates WHERE id = ?")
        .bind(template_id)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        return Ok(Skeleton::default());
    };
    let raw: String = row.try_get("skeleton_json")?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
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
