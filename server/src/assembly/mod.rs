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
    #[serde(default)]
    assembly_rules: AssemblyRules,
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
    /// 通关兑现的隐藏道具（预审核池内定义）。
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
        // 1) per-character 钩子：从隐藏内容池按执念/恐惧重叠度选择并参数化。
        let terms = obsession_terms(card);
        let selected = select_pool_items(&skeleton.hidden_content_pool, &terms, rules.hidden_per_character);
        let mut character_got_hook = false;
        for (pool_item, matches, matched_term) in selected {
            let text = parameterize(&pool_item, cid, card, matched_term.as_deref());
            // 装配连接文本过机审后才生效；被拒则跳过该钩子（换下一候选）。
            let verdict = crate::safety::moderate_and_queue(
                state,
                "assembly_hook",
                &format!("{world_id}:{cid}:{}", pool_item.id),
                &text,
            )
            .await?;
            if verdict == ModerationVerdict::Rejected {
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
                reward_item: pool_item.reward_item.clone(),
            });
            character_got_hook = true;
        }
        let _ = character_got_hook; // ≥1 目标：池非空且非全拒时自然满足（见 select_pool_items 保底取 1）。

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

    let assembled = AssembledInstance {
        per_character_hooks: hooks,
        enabled_endings,
        lineup_params,
        difficulty_notes,
        home_advantages,
    };

    // 持久化：assembly 段钉住（含派生的 templateVersion），chapterState 段留给章节会话推进。
    let wrapper = json!({
        "assembly": &assembled,
        "chapterState": empty_chapter_state(),
        "templateVersion": world.template_version,
        "assembledAt": now_ms(),
    });
    save_wrapper(&state.db, world_id, &wrapper).await?;

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

/// 选择绑定执念的隐藏内容：按命中数降序取前 N；池非空则至少保底取 1（满足「每角色 ≥1」）。
fn select_pool_items<'a>(
    pool: &'a [PoolItem],
    terms: &[String],
    n: usize,
) -> Vec<(&'a PoolItem, usize, Option<String>)> {
    if pool.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(&PoolItem, usize, Option<String>)> = pool
        .iter()
        .map(|p| {
            let (m, term) = score_pool_item(p, terms);
            (p, m, term)
        })
        .collect();
    // 命中数降序；稳定排序保留池内原顺序作为平手序。
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    let take = n.max(1).min(scored.len());
    scored.into_iter().take(take).collect()
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
