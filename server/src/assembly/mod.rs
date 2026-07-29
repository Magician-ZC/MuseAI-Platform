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
use muse_engine::narrative::types::{IntensityWeights, LocationDef, LocationGate};

// ---------- 输出：装配结果（写入 worlds.assembled_json 的 `assembly` 段，随实例钉住） ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssembledInstance {
    pub per_character_hooks: Vec<CharacterHook>,
    pub enabled_endings: Vec<String>,
    /// 本实例**定盘**的结局 id（总规格 §5「一个模板，千个平行世界」）：在 `enabled_endings` 之内
    /// 按加权权重、由 `DOMAIN_ENDING` 子流掷点选定，随实例钉住。`runtime::select_ending` 读它。
    ///
    /// 🔴 `enabled_endings` 回答的是「这局**有哪些**结局在台上」（叙事投影 / 章节接口口径不变），
    /// 本字段回答「最终**落到哪一个**」——两者语义分离，谁也不改谁。
    ///
    /// `None` = 结局池为空（无结局可落）。老实例的 `assembled_json` 无此键
    /// （`skip_serializing_if` 保证不写空键），`select_ending` 对它们保持既有回退口径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_ending: Option<String>,
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
    /// 身份池分配结果（总规格 §5【拍板 4、5】）：`(cloud_character_id, identity_id)`，按 cid 升序。
    /// runtime 读回后**只作叙事层的开局站位**（"你在这个世界是户部主事 / 被退婚的嫡女"）。
    ///
    /// **平权红线（§0.1 平权宪法，锁进测试）**：身份不携带任何数值差异、准入门槛、产出加成或叙事特权——
    /// 严禁任何下游据此改判定、改发奖、开权限、调难度。戏份靠玩出来。
    /// 空 = 模板未声明 identityPool（`skip_serializing_if` 保证老模板 assembled_json 逐字节不变）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_assignments: Vec<(String, String)>,
    /// 装配采样审计段（防刷第二环）：由固定实例种子驱动的子集采样结果，随实例钉住写入
    /// `worlds.assembled_json` 的 `/assembly/sampling`。**仅服务端 / 审计可见——绝不进 members_projection
    /// 或日报投影**。`None` = 退化路径（非超集旧模板：全量装配、不采样，与改造前行为完全一致）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<InstanceSampling>,
    /// 公示产出表（总规格 §10【拍板 17】）：三层结算算贡献分 → 查本表 → 确定发放，随实例钉住。
    /// `None` = 模板未声明 → ③ 世界线层只累计贡献分、不发放（未验证功能默认关闭，VALIDATION §0.1）。
    /// `skip_serializing_if` 保证老实例 `assembled_json` 逐字节不变（同 `identity_assignments` 范式）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout_table: Option<PayoutTable>,
    /// 境界档快照（总规格 §6【拍板 3】戏服原则）：**全员统一的一件戏服**，装配时把模板声明原样钉住。
    /// `None` = 模板未声明 → 本实例无境界维度（`skip_serializing_if` 保证老实例 / 未声明模板的
    /// `assembled_json` **逐字节不变**，同 `payout_table` 范式）。
    ///
    /// 消费者（唯一一条）：`runtime::parse_realm_costume` 读回 `briefing` + `flavorNotes` →
    /// `RoundInput.realm_costume` → 引擎 `call_director` 的入场导演设局 prompt
    ///（§6「入场导演统一设定」）。**只影响模型怎么描写，不进任何判定域**
    ///（见 `admin_api::calibration` 的效力自述 `narrativeLayer: Implemented` /
    /// `numericLayer: NeverByDesign`）。其余五个字段仍只服务审计与运营展示，不进模型上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm_tier: Option<RealmTier>,
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
    /// 身份池分配审计副本（总规格 §5【拍板 4、5】"身份配额进采样域"）：`(cid, identity_id)` 按 cid 升序。
    /// 与 `AssembledInstance.identity_assignments` 同源同值，此处仅供服务端审计复算/replay 对账。
    /// `#[serde(default)]` 兼容改造前已钉住实例的回读（同 `culled_over_tier` 范式）。
    #[serde(default)]
    pub identity_assignments: Vec<(String, String)>,
    /// 卡集合指纹哈希（R2 自定义房装配，四段式种子第四段）：排序去重的 `{cardId}@{cardVersion}`
    /// 以 `\n` 连接后取 fnv 哈希——与 `roster_fingerprint` 同款，只存哈希不存明文。
    /// **非容器形态恒为空**，`skip_serializing_if` 保证既有实例 `assembled_json` 逐字节不变。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub card_set_fingerprint: String,
    /// 本实例装入的副本卡 id（模板 `subplotCardRefs` 序）。仅服务端 / 审计可见，绝不进
    /// members_projection 或日报投影（同 `seed` / `roster_fingerprint` 的契约）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_cards: Vec<String>,
}

// ---------- 公示产出表（总规格 §10【拍板 17】：确定性产出，无 RNG 抽卡） ----------

/// 公示产出表：**三层结算算贡献分 → 查本表 → 确定发放**。
///
/// 为什么钉在骨架/实例里而不是全局配置：产出表随模板版本走、随实例快照进 `assembled_json`，
/// 于是「同一实例、同一贡献分 → 同一产出」可被 replay 复算，与 §12.5.3 确定性契约同源；
/// 结算点（chapters::finish / runtime 终局）本就把 `assembled_json` 读在手里，零额外查询。
///
/// **合规定性防线（§16 去抽卡化）**：查表发放，**全程零随机数**——张力来自"能否完成钩子 / 推动主线"
/// 的过程不确定性，不来自开箱爆率。任何在此引入 RNG 的改动都需显式评审。
///
/// **未验证功能默认关闭（VALIDATION §0.1）**：模板未声明 `payoutTable` → ③ 层只累计贡献分、
/// 不发放任何产出。开闸靠运营录入数据，不靠代码合并。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PayoutTable {
    /// ③ 世界线层阶梯（公示）：取「门槛 ≤ 贡献分」中门槛最高的一档发放；无档命中 → 不发放。
    /// 空 = 有表但无档位（等价于不发放，留作运营灰度开关）。
    #[serde(default)]
    pub worldline_tiers: Vec<PayoutTier>,
    /// 贡献分折算权重：**直接复用引擎 `IntensityWeights`**（同结构、同默认值 1.0/0.5/0.25/0.25）。
    /// 口径一致由类型本身保证——server 侧逐角色折算的分项之和恒等于引擎 `round_intensity` 标量。
    #[serde(default)]
    pub contribution_weights: IntensityWeights,
    /// 世界线崩塌系数（§9）：③ 归零 + ① 减半的参数化开关。
    #[serde(default)]
    pub collapse: CollapsePolicy,
}

/// 产出表的一档（公示口径）。**无概率字段**——命中即发，不存在爆率。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PayoutTier {
    /// 档位公示名（前端展示 + 审计留痕）。
    #[serde(default)]
    pub label: String,
    /// 进入本档所需的最低世界线贡献分（含）。同一表内不得重复（重复 = 同分歧义，破坏确定性）。
    #[serde(default)]
    pub min_score: f64,
    /// 本档稀有产出（`None` = 只发历练）。
    /// **产出封顶不可绕过**：`origin.powerTier > 实例星级` 由结算侧强制剔除（不降级、不替换）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemDefinition>,
    /// 本档世界线层历练（≤ 0 = 不发）。
    #[serde(default)]
    pub mileage: i64,
}

/// 世界线崩塌系数（总规格 §9）：崩塌 = ③ 归零 + ① 减半 + ② 已锁定保留。
/// 两个系数均可由模板覆盖（VALIDATION §0.2 产品规则参数化），缺省回落 progression 平衡参数常量。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollapsePolicy {
    /// **通关奖励**折算系数（默认 0.0 = 崩塌不发；⚠️ 2026-07-29 由 0.5 改为 0.0，
    /// 因为 §12 重定后这一层不是「保底」而是「通关奖励」，而崩塌恰恰意味着世界线没走完。
    /// JSON 键名 `baselineFactor` 是历史名，不改是为了不让存量模板的键变成「无人读取的键」）。
    #[serde(default = "default_collapse_baseline_factor")]
    pub baseline_factor: f64,
    /// ③ 世界线层折算系数（默认 0.0 = 归零；≤ 0 即整层不发放）。
    #[serde(default = "default_collapse_worldline_factor")]
    pub worldline_factor: f64,
}

fn default_collapse_baseline_factor() -> f64 {
    crate::progression::COLLAPSE_BASELINE_FACTOR
}

fn default_collapse_worldline_factor() -> f64 {
    crate::progression::COLLAPSE_WORLDLINE_FACTOR
}

// ---------- 境界档（总规格 §6【拍板 3】：戏服原则——境界即布景） ----------

/// 题材枚举（§6「副本补两个标注：**题材（genre）** + 冲突烈度」）。
///
/// ⚠️ **总规格没有枚举题材**——§6 只点名了「都市 / 言情 / 历史」三个无战力体系题材，
/// 与斗破 / 斗罗那类玄幻作对照。下表是据此给的最小可用集，属**实现补白**而非规格原文；
/// 新增题材在此续号，并同步 `admin/src/pages/Calibration.tsx` 的中文映射。
/// 收白名单不收自由文本，口径同 `admission::KNOWN_COSMOLOGIES`。
pub const KNOWN_GENRES: &[&str] =
    &["xuanhuan", "xianxia", "wuxia", "urban", "romance", "history", "scifi", "mystery", "other"];

/// 冲突烈度枚举（§6 原文「**文斗 / 武斗 / 生死**」三档，一一对应，无补白）。
pub const KNOWN_CONFLICT_INTENSITIES: &[&str] = &["civil", "martial", "lethal"];

/// 按 §6「历史题材涉真实人物走更严审核档（合规）」需要更严审核的题材。
///
/// 🔴 **当前只是运营提示，未接进任何审核链路**：`safety::` 的机审 / 人审档位不读本常量，
/// 建模板期也不因题材改变 `moderation` 初值。校准面把它作为一条提示渲染（状态：Concept），
/// 谁都不该以为"标了 history 就自动进严审"。
pub const STRICTER_MODERATION_GENRES: &[&str] = &["history"];

/// 境界档：**阶段模板发给全员的同一件戏服**（§6「境界跟着副本走，不跟着角色走」）。
///
/// 三条设计约束直接来自 §6，改本类型前先回去读那一节：
///
/// 1. **全员统一** ⇒ 落点是 `Option<RealmTier>` 而**不是**池 / 数组。进「黑角域篇」全员领斗王档，
///    没有 per-character 差异，于是没有分配、没有配额、**没有抽样** —— 本维度因此
///    **不占用任何 RNG 域常量**（域清单里下一个可用的是 `0x5D`），装配层只是把它原样钉住。
///    这也是它与身份池（§5，有池有配额有种子分配）的根本区别：**身份各不相同，境界人人一样**。
/// 2. **零数值** ⇒ 全字段是字符串 / 字符串数组，**一个数字都没有**。§6「跨体系靠风味翻译，
///    不靠数值换算」+ §0.1 平权宪法：境界是布景不是战力。给它一个 `level: i64` 就等于开了
///    「选阶段 = 选强度」的侧门，`realm_tier_carries_no_numeric_field` 把这条锁进测试。
/// 3. **永不带走** ⇒ 只钉进实例 `assembled_json`，**绝不写回角色卡**（§6「角色带走道具与历练，
///    永不带走境界——境界留在那个世界的那场戏里」）。本模块从不触碰 `cloud_characters`。
///
/// 全字段 `#[serde(default)]`：老模板无 `realmTier` → `None` → 零影响（同 `payoutTable` 哲学）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RealmTier {
    /// 档位 id（跨阶段对账与审计的稳定键）。空 → 建模板期拒绝。
    #[serde(default)]
    pub id: String,
    /// 公示档名："斗王档" / "六品京官" / "破产后的第三个月"。空 → 展示层回落 id。
    #[serde(default)]
    pub label: String,
    /// 所属体系标签（∈ `admission::KNOWN_COSMOLOGIES`，与道具准入同一枚举，自由文本一律不收）。
    /// **空 = 无战力体系题材**（§6「都市 / 言情 / 历史：境界泛化为处境——财富 / 人脉 / 官阶 /
    /// 关系位置」），完全合法，不是缺数据。
    #[serde(default)]
    pub cosmology: String,
    /// 题材（§6「副本补两个标注」之一），∈ `KNOWN_GENRES`。空 = 未标注。
    #[serde(default)]
    pub genre: String,
    /// 冲突烈度（§6「副本补两个标注」之二），∈ `KNOWN_CONFLICT_INTENSITIES`：
    /// `civil` 文斗 / `martial` 武斗 / `lethal` 生死。空 = 未标注。
    ///
    /// ⚠️ **它不是生死开关**：世界是否致命由建房参数 `lethality` 与 §11 死亡规则独立决定，
    /// 本字段只是给入场导演与运营看的题材标注。两者互不读取、互不覆盖——
    /// 把 `lethal` 接成"打开死亡"就等于让一个叙事标注改判定，属平权红线违规。
    #[serde(default)]
    pub conflict_intensity: String,
    /// 入场导演的统一戏服说明（§6「入场导演统一设定」）：一句话交代"这一篇全员是什么水位"。
    /// **本字段与 `flavor_notes` 是七个字段里仅有的两个会进模型上下文的**——
    /// `runtime::parse_realm_costume` 读它 → 引擎 `call_director` 的设局 prompt。
    /// 它只改变导演怎么描写，不改判定（红线见本类型上方第 2 条与 `RoundInput.realm_costume`）。
    #[serde(default)]
    pub briefing: String,
    /// 跨体系风味翻译提示（§6「唐三入斗破世界，魂技译为斗气招式风味，内核不变」）。
    /// 同 `briefing`，一并进入入场导演 prompt（空数组 = 无风味翻译，该段不出现）。
    #[serde(default)]
    pub flavor_notes: Vec<String>,
}

impl Default for CollapsePolicy {
    fn default() -> Self {
        Self {
            baseline_factor: default_collapse_baseline_factor(),
            worldline_factor: default_collapse_worldline_factor(),
        }
    }
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
    /// 身份池（总规格 §5【拍板 4、5】"身份配额进采样域"）：阶段模板声明可用的开局身份
    /// （官员×N、商贾×N、江湖客×N、"被退婚主位"×1~2……各带配额与专属钩子引力），
    /// 装配时按「内核匹配 + 种子随机」分配给入场卡。空 = 无身份维度（老模板零影响）。
    #[serde(default)]
    identity_pool: Vec<IdentitySpec>,
    /// 公示产出表（总规格 §10【拍板 17】"确定性产出，无 RNG 抽卡"）：三层结算的 ③ 世界线层
    /// 按贡献分查本表确定发放。装配时原样钉进 `assembled_json`（随模板版本快照，可 replay 复算）。
    /// 缺省 `None` → ③ 层只累计贡献分、不发放（老模板零影响；开闸靠运营录入数据）。
    #[serde(default)]
    payout_table: Option<PayoutTable>,
    /// 境界档（总规格 §6【拍板 3】「戏服原则——境界即布景」）：本阶段发给**全员的同一件戏服**
    /// （进「黑角域篇」全员领斗王档）。「阶段天然携带境界档——你选阶段，就是在选境界」。
    ///
    /// 与 `identity_pool` 的分工：**身份各不相同（叙事层站位，有池有配额有种子分配），
    /// 境界人人一样（布景，无池无配额无抽样）**。故此处是 `Option` 而不是 `Vec`，
    /// 装配层对它**不掷一次骰子**，只是原样钉进 `assembled_json`。
    ///
    /// 缺省 `None` → 本模板无境界维度（老模板零影响；装配产物逐字节不变，有专项回归守护）。
    #[serde(default)]
    realm_tier: Option<RealmTier>,
    /// 副本采样计数提示（每维度每副本抽样量）。全空 = 走退化路径（不采样）。
    #[serde(default)]
    sampling: SamplingSpec,
    /// 超集标记：`true` 且 storylines 非空 且 sampling 非全空 → 走种子采样；否则退化为全量装配。
    #[serde(default)]
    is_superset: bool,
    /// 副本卡引用列表（R2 容器形态，技术附录 §3.1）：容器模板声明「本房装哪几张卡」，
    /// 版本钉住。空 = 普通模板（**走原三段式种子 + 原装配路径，字节级不变**）。
    #[serde(default)]
    subplot_card_refs: Vec<SubplotCardRef>,
    /// 跨卡缝合边（技术附录 §3.3）：两端须分别是各自卡 `anchors` 白名单成员。
    /// 卡内 `connections` 只许闭包在卡内，跨卡连接**只能**经本字段显式声明。
    #[serde(default)]
    seams: Vec<Seam>,
    /// 容器枢纽地点声明（技术附录 §3.3）：合并后地点图不连通时自动生成 `loc-nexus` 把各分量接起来。
    #[serde(default)]
    nexus: Option<NexusSpec>,
    /// 对外缝合口白名单：本骨架（容器本体或卡片段）允许被缝合边落上的地点 id。
    /// 空 = 回落「首个非秘境地点」。**秘境永不可作缝合口**（gate 语义必须完整保留在卡内）。
    #[serde(default)]
    anchors: Vec<String>,
    /// 容器装配计划：**非反序列化字段**，只由 `compose_container_skeleton` 产出。
    /// `None` = 非容器形态（种子走三段式、storyline 权重不乘卡权重、审计段无卡字段）——
    /// 这是「容器形态之外字节级不变」的唯一开关点，所有容器行为都挂在它是不是 `Some` 上。
    #[serde(skip)]
    container: Option<ContainerPlan>,
}

/// 副本卡引用（技术附录 §3.1）：容器模板 → 卡的**精确版本钉住**引用。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SubplotCardRef {
    /// `subplot_cards.id`。命名空间前缀取自它，故**不得含 `:`**（建房期校验）。
    #[serde(default)]
    card_id: String,
    /// 期望的卡蓝图版本（`subplot_cards.source_template_version`）。
    /// 声明了就必须与服务端读到的版本一致（钉住校验）；未声明则由服务端权威读取——
    /// 无论哪种，进指纹的**恒是服务端值**，客户端声明改不动种子。
    #[serde(default)]
    card_version: Option<i64>,
    /// 采样权重（乘进该卡各 storyline 的选取权重）。
    /// **只影响"哪条戏更容易上演"，不影响任何数值 / 产出 / 准入**（§0.1 平权红线）。
    #[serde(default = "one")]
    weight: f32,
}

/// 跨卡缝合边（技术附录 §3.3）：`from`/`to` 为命名空间全限定地点 id
/// （卡内地点写 `{cardId}:{locId}`，容器本体地点写裸 id）。双向连通。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Seam {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
}

/// 容器枢纽地点（技术附录 §3.3）：多卡地点图裂成孤岛时的交汇之地。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct NexusSpec {
    #[serde(default)]
    name: String,
}

/// 容器装配计划（`compose_container_skeleton` 产物，随 `Skeleton` 传给 `plan_sampling`）。
/// 它是「容器形态」的**唯一标志**：`Skeleton.container.is_none()` ⇒ 全部行为与改造前逐字节一致。
#[derive(Debug, Clone, Default)]
struct ContainerPlan {
    /// 卡集合指纹**明文**（技术附录 §4.1）：排序去重的 `{cardId}@{cardVersion}` 以 `\n` 连接。
    /// 进种子的是明文（对齐 `roster_fingerprint` 进种子的方式），进审计段的是它的哈希。
    fingerprint: String,
    /// `(cardId, weight)`，模板序。前缀即归属映射（`{cardId}:xxx`），故按前缀查权重即可。
    weights: Vec<(String, f32)>,
    /// 缝合钉住地点（nexus + 全部 seam 端点）：进地点采样的**必选种子**，
    /// 保证被选各卡的地点分量在实例内互达（技术附录 §3.3 连通性保障）。
    pinned_locations: Vec<String>,
    /// 装入的卡 id（模板序），仅进审计段。
    card_ids: Vec<String>,
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

/// 身份池条目（总规格 §5【拍板 4、5】）：一个可被分配的**开局站位**。
///
/// **平权红线（§0.1 平权宪法）**：身份 = 开局站位，**只进叙事层**——不携带数值差异、准入门槛、
/// 产出加成或叙事特权。`quota` 只是"这个位最多几个人站"，`hookAffinity` 只影响"哪条戏更容易找上你"，
/// `isLead` 只是戏眼站位的抽取倾向；三者都**不得**被任何下游用来改判定 / 改发奖 / 开权限 / 调难度。
/// 戏份靠玩出来。
///
/// 全字段 `#[serde(default)]`：老模板无 `identityPool` → 空 Vec → 零影响（同 `SamplingSpec` 哲学）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentitySpec {
    /// 身份 id（分配结果与审计的稳定键）。空 → 该条目不可分配（建模板期由校验前置拦截）。
    #[serde(default)]
    id: String,
    /// 叙事展示名（"户部主事" / "漕帮舵主" / "被退婚的嫡女"）。空 → 展示层回落 id。
    #[serde(default)]
    label: String,
    /// 配额：本实例最多几个人站这个位（**上限**，不是必须填满——实际人数由实例种子决定，
    /// 于是"1 号世界 10 个官员、2 号世界 1 个"）。缺省 = 1；显式 0 建模板期被拒（运行期按不可分配处理）。
    #[serde(default = "one_usize")]
    quota: usize,
    /// 主题词：与角色执念 / 恐惧 / 剧情种子做重叠匹配 —— **内核匹配**的依据（复用 `related` 口径）。
    #[serde(default)]
    themes: Vec<String>,
    /// 专属钩子引力：指向 storyline id 或隐藏 / 支线池物品 id，声明这个身份天然贴近哪条戏。
    /// 命中本实例在演的内容 → 抽取权重上调。**只影响叙事贴合度，不影响任何数值。**
    #[serde(default)]
    hook_affinity: Vec<String>,
    /// 戏眼主位标记（"被退婚主位"×1~2 这类站位）：仅上调抽取权重，**不给任何特权**；
    /// 不保证必被占用（人数不足或种子未选中时可空缺，由入场导演在叙事层处理）。
    #[serde(default)]
    is_lead: bool,
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
pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// SplitMix64：确定性整数流（每维度独立子流从含 world_id 的全局 seed 派生，杜绝纯阵容维度可被观测反推）。
pub(crate) struct Rng(pub(crate) u64);
impl Rng {
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// [0, n) 均匀整数（n=0 → 0）。
    pub(crate) fn below(&mut self, n: usize) -> usize {
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
// ═══════════════════════════════════════════════════════════════════════════
// 气运与机缘（2026-07-29）：**角色自己的**两个数
// ═══════════════════════════════════════════════════════════════════════════
//
// 产品口径（2026-07-29 二次澄清）：**这两个数对自己有影响，对世界没有影响。**
//
// 🔴 我上一版做成了世界级（全员共享），那是**错的**——不是实现错，是理解错。
// 当时的推理是「作用于世界就不构成个体优势，红线更好过」，于是把产品要的东西
// 改成了另一个东西。产品要的一直是「这张卡的气运/机缘」，
// 「气运高的可以降低遇到 boss 或获取宝物的难度」这句话里的主语从头到尾都是角色。
// 🔵 记在这里的教训：**为了让红线好过而改需求的语义，是把红线的成本转嫁给了产品**。
// 该做的是照需求做、把风险说清楚，而不是悄悄换一个自己更好交差的形态。
//
// 于是世界这一层整个撤掉（`plan_sampling` 回到接线前，逐字节），改落在
// **per-character 钩子**上——那本来就是这个装配器里唯一「按角色分发内容」的地方：
//
// | | 是什么 | 落点 |
// |---|---|---|
// | **机缘** | 主线间隙里的事**多久找上你一次** | 这个角色的钩子配额 `quota` |
// | **气运** | 那些事**离常态多远**（两个方向都算） | 这个角色的钩子候选排序 |
//
// 🔴 仍然成立的三条（它们与作用对象无关）：
// 1. **气运不等于「更好」**：取的是**绝对偏离**，最温和的和最凶险的一起被抬到前面。
//    高气运的角色更容易撞上不该他这个境界撞的敌人——那是产品原话里就写着的。
// 2. **产出封顶与稀有预算一个字不动**（`RARE_TIER` / `RARE_BUDGET` / 星级封顶）。
//    那三道闸在**世界**这一层、在钩子分发**之前**就把「这局最多能有几件好东西」定死了；
//    机缘让你**碰到**更多事，不让这个世界**多出**一件好东西。
// 3. **买不到**：只能靠经历长（满档六十局以上）且封顶，道具接口预留着但不可破顶。
//
// ⚠️ **一条必须说出来的真实后果**：机缘高的角色系统性分到更多钩子 ⇒ 戏份分布会变得不均，
// 而「基尼系数 / 无戏份占比」正是 `slo::narrative_slo` 在盯的叙事质量指标。
// 这不是讲道理层面的隐忧，是**可观测的**——开闸后要看那两个数。
// 削弱它的手段现成（调小 `MUSE_OPPORTUNITY_EXTRA_HOOKS`，甚至归零），但要先看到数。

/// 机缘满档时**额外**多分到几个钩子。
///
/// ⚠️ 为什么是「额外几个」而不是「乘数」：`hidden_per_character` 默认是 **1**，
/// 而 1 × 1.2 四舍五入回 1——乘数在这个基数上等于没有。第一版世界级那版用的是乘数，
/// 搬过来会得到一个**永远不生效**的机制，且没有任何症状。
const OPPORTUNITY_EXTRA_HOOKS: i64 = 2;
/// 气运满档时的两极化强度（万分比）。默认 0.5：与「命中一项执念」等量级，
/// 足以改变排序，不足以压过执念匹配本身（执念绑定是这个装配器的主判据，气运不该篡位）。
const FORTUNE_SWING_BP: i64 = 5_000;
const ENV_OPPORTUNITY_EXTRA_HOOKS: &str = "MUSE_OPPORTUNITY_EXTRA_HOOKS";
const ENV_FORTUNE_SWING_BP: &str = "MUSE_FORTUNE_SWING_BP";

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<i64>().ok()).filter(|n| *n >= 0).unwrap_or(default)
}

/// 档位 → `[0, 1]` 的比例。零档 → 0（恒等），满档 → 1。
fn swing_ratio(level: i64) -> f32 {
    (level as f32 / crate::imprint::SWING_MAX_LEVEL as f32).clamp(0.0, 1.0)
}

/// 机缘：这个角色本局分到几个钩子。
///
/// 🔴 **只增不减**，且零档恒等于 `base` ⇒ 无烙印的卡（全新库、新卡）拿到的钩子数
/// 与本层落地前**一模一样**。这是它能作为纯增量上线的全部依据。
fn personal_hook_quota(base: usize, opportunity_level: i64) -> usize {
    let extra = swing_ratio(opportunity_level) * env_i64(ENV_OPPORTUNITY_EXTRA_HOOKS, OPPORTUNITY_EXTRA_HOOKS) as f32;
    base + extra.round().max(0.0) as usize
}

/// 这批候选的难度常态：均值 + 最大绝对偏离。空集合返回 `(0.5, 0.0)`（恒等）。
///
/// ⚠️ **常态是这批候选自己的均值，不是写死的 0.5**：真实数据里 `difficultyBase`
/// 落在 0.2–0.5（golden skeleton / 新手微本 / 提取产物都是这个量级），
/// 0.5 在那儿是**上界而不是中心**——把它当中心，「凶险的那一极」在数据上根本不存在，
/// 「两个方向都算」退化成一句空话。
fn difficulty_center(items: &[PoolItem]) -> (f32, f32) {
    if items.is_empty() {
        return (0.5, 0.0);
    }
    let mean = items.iter().map(|p| p.difficulty_base).sum::<f32>() / items.len() as f32;
    let max_dev = items.iter().map(|p| (p.difficulty_base - mean).abs()).fold(0.0_f32, f32::max);
    (mean, max_dev)
}

/// 气运给一个候选加的**排序分**：离这批候选的难度常态越远，加得越多。
///
/// 🔴 「两个方向都算」在这里是字面的——取的是**绝对偏离**：极温和的和极凶险的
/// 一起被抬到前面，中庸的一起被压下去。若只抬高难度那一半，它就变成了
/// 「高气运 = 更好的东西」，当场违宪。
///
/// 🔴 **零气运恒返回 0.0** ⇒ 排序键退化为纯命中数 ⇒ 与本层落地前**逐字节相同**。
/// 全体难度相同时 `max_dev == 0`，同样返回 0.0——没有「极端」可言，气运无处着力。
fn fortune_bonus(difficulty: f32, mean: f32, max_dev: f32, fortune_level: i64) -> f32 {
    if max_dev <= f32::EPSILON {
        return 0.0;
    }
    let extremeness = ((difficulty - mean).abs() / max_dev).clamp(0.0, 1.0);
    swing_ratio(fortune_level) * (env_i64(ENV_FORTUNE_SWING_BP, FORTUNE_SWING_BP) as f32 / 10_000.0) * extremeness
}

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
const DOMAIN_IDENTITY: u64 = 0x57;
// 0x58-0x5A：`runtime::simulation`（离线仿真试跑工装）的三条子流，登记在此以免与装配域撞号。
// 它们与装配采样物理隔离（不同 seed、不同进程路径），登记只为让「域常量唯一」这条纪律有单一清单。
// 0x5B：`ifline::runner`（if 线付费副本推进）的逐拍演员表抽样子流 `DOMAIN_IFLINE_CAST`。
// 同样物理隔离（种子来自 `ifline_worlds.run_seed`，与实例装配 seed 无交集）。
// 0x5C：`safety::semantic`（§15 第 3 层语义分类异步复核）的**私有投影抽样**子流 `DOMAIN_L3_SAMPLE`。
// 种子 = H(world_id ‖ tick_no ‖ domain_event_id)，与装配 seed 无交集；登记同样只为让域号唯一。
// 它必须确定性的理由与装配不同：重试要拿到**同一批样本**，且事后复盘要算得回来「那条为什么没被查」。
// 境界档（§6 拍板 3）**不占域**：它全员统一、无池无配额、装配层一次骰子都不掷，
// 只把模板声明原样钉进 `assembled_json` —— 没有抽样就不该占号，取了号反而误导后来者。
// **下一个可用域常量是 0x5D**；新增子流请在此续号并写明归属，不要跳号也不要复用。

// ---------- 身份池分配权重（§5 拍板 4、5；纯叙事倾向常量，可调，禁止赋予任何数值含义） ----------

/// 专属钩子引力加权：身份的 `hookAffinity` 每命中一条本实例在演的内容，抽取权重 +此值。
const IDENTITY_HOOK_BOOST: f32 = 0.5;
/// 戏眼主位加权：`isLead` 身份的抽取权重 +此值（**只影响谁更可能站上这个位，不给任何特权**）。
const IDENTITY_LEAD_BOOST: f32 = 1.0;

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

/// 实例种子（**三段式，普通模板恒走这条**）：
/// `fnv1a_64(world_id ‖ 0x01 ‖ 阵容指纹 ‖ 0x01 ‖ template_version)`。
///
/// ⚠️ 本函数的字节口径被 `prng_test_vectors` / `same_seed_same_sampling` /
/// `roster_fingerprint_changes_seed` 与黄金世界回归逐字节锁死，**不得改动**。
/// 容器形态的第四段走下面的 `container_instance_seed`，与本函数物理分离。
fn instance_seed(world_id: &str, fingerprint: &str, template_version: i64) -> u64 {
    fnv1a_64(format!("{world_id}\u{1}{fingerprint}\u{1}{template_version}").as_bytes())
}

/// 实例种子（**四段式，仅容器形态**，技术附录 §4.1）：三段式尾部追加**卡集合指纹**。
///
/// 目的是防「换一张卡组合刷同一世界」：
/// ① 即便有人绕过版本递增（后台直改 `skeleton_json` 换卡而 `template_version` 未动），
///    换卡即换种子，无法拿同一实例试探不同卡组合下的高收益路径；
/// ② 审计可复算——`world_container_cards` 的行 + 审计段 `cardSetFingerprint` 两侧交叉对账，
///    能证明这份装配由哪个卡集合产生。
fn container_instance_seed(
    world_id: &str,
    fingerprint: &str,
    template_version: i64,
    card_set_fingerprint: &str,
) -> u64 {
    fnv1a_64(
        format!("{world_id}\u{1}{fingerprint}\u{1}{template_version}\u{1}{card_set_fingerprint}")
            .as_bytes(),
    )
}

/// 实例种子（**第五段：世界线烙印**）：在三/四段式之上追加**烙印指纹**。
///
/// 提案见 `docs/build/spec-worldline-imprint.md` §2.2「共鸣 · 结构层」。它要兑现的那半句是：
/// **即使别人一字不差复刻了内核，抽到的剧情线 / 隐藏钩子 / 地点组合也不同**——
/// 因为这张卡带着一段别人没有的经历，而经历进了种子。
///
/// 🔴 **烙印指纹为空时，本函数一定不被调用**（见 [`resolve_instance_seed`] 的分派）。
/// 那条分派是黄金世界回归能继续绿的全部依据：全新库、全新卡没有烙印 ⇒ 走原路径 ⇒
/// 种子一个 bit 都不变。**这一层是纯增量，不是改写。**
///
/// 🔵 顺带它加强了防刷：种子里又多了一个玩家**不可控、不可预测、不可复现**的分量
/// （烙印由服务端按 world_events 派生，客户端写不进来）。
fn imprinted_instance_seed(base: u64, imprint_fingerprint: &str) -> u64 {
    fnv1a_64(format!("{base}\u{1}{imprint_fingerprint}").as_bytes())
}

/// 种子分派：**容器形态才四段，其余一律三段**；两者之上再按烙印指纹追加第五段。
///
/// 这是「四段式只在容器形态生效」的唯一落点——`Skeleton.container` 只可能由
/// `compose_container_skeleton` 置上，而它只在「开关已开 + 模板声明了 subplotCardRefs +
/// 卡全部解引用成功」时才被调用。任何一条不满足 → `None` → 与改造前逐字节相同的三段式。
fn resolve_instance_seed(
    skeleton: &Skeleton,
    world_id: &str,
    fingerprint: &str,
    template_version: i64,
    imprint_fingerprint: &str,
) -> u64 {
    let base = match &skeleton.container {
        Some(plan) => {
            container_instance_seed(world_id, fingerprint, template_version, &plan.fingerprint)
        }
        None => instance_seed(world_id, fingerprint, template_version),
    };
    // 🔴 空指纹 → 原样返回。这一句就是「零烙印时逐字节不变」的落点。
    if imprint_fingerprint.is_empty() {
        return base;
    }
    imprinted_instance_seed(base, imprint_fingerprint)
}

/// storyline 阵容加权 boost（复用 weight_endings_scored 的阵容画像口径）。
fn affinity_boost(affinity: &Option<String>, profile: &(u32, u32, u32)) -> f32 {
    let total = (profile.0 + profile.1 + profile.2).max(1) as f32;
    match affinity.as_deref() {
        Some("strategist") => profile.0 as f32 / total,
        Some("combat") => profile.1 as f32 / total,
        Some("social") => profile.2 as f32 / total,
        _ => 0.0,
    }
}

/// 身份内核匹配度：身份主题词 × 角色执念词条的重叠计数（复用 `related` 口径，与钩子匹配同款）。
/// 这就是「内核匹配」——身份贴不贴这张卡的内核（恐惧 / 被否认的欲望 / 核心矛盾 / 剧情种子 / 拒绝规则），
/// 而不是贴他的战力或资历。**匹配度只进抽取权重，不进任何数值。**
fn identity_match_score(spec: &IdentitySpec, terms: &[String]) -> usize {
    terms.iter().filter(|t| spec.themes.iter().any(|th| related(t.as_str(), th))).count()
}

/// 身份展示名：`label` 非空取 label，否则回落 id（仅用于人读的建模板期报错文案）。
fn identity_display(spec: &IdentitySpec) -> &str {
    let label = spec.label.trim();
    if label.is_empty() {
        spec.id.trim()
    } else {
        label
    }
}

/// 身份池分配（总规格 §5【拍板 4、5】"身份配额进采样域"）：**内核匹配 + 种子随机**。
///
/// 算法（纯函数，整数 RNG，无系统随机 / 无浮点 RNG / 无 map 迭代序驱动 RNG）：
/// 1. 阵容按 **cid 升序去重** 遍历（消 joined_at 顺序敏感，与 `roster_fingerprint` 同一哲学）——
///    保证同一 (world_id, 阵容, template_version) 无论入场先后都得到**同一份分配**（确定性契约）。
/// 2. 每人从「尚有余量的身份」里按权重抽一个：
///    `w = 1 + 内核匹配数 + IDENTITY_HOOK_BOOST×在演钩子命中数 + (isLead ? IDENTITY_LEAD_BOOST : 0)`，
///    走既有 `weighted_pick_one`（整数化权重，+1 保底 → 零匹配者仍有最小概率被选中，不会锁死成"只有内核
///    匹配的人才有身份"）。
/// 3. 抽中即扣该身份余量 —— **配额是硬上限，绝不超发**。
///
/// 配额边界策略（Σquota vs 实际入场人数不一致时）：
/// - **人多于 Σquota**：配额用尽后剩余角色**不分配身份**（不在结果里出现）。无身份 = 无预设站位，
///   由入场导演在叙事层自由安排处境；**不因此损失任何数值 / 产出 / 权限**（平权红线）。
/// - **人少于 Σquota**：只分配到人数为止，多余槽位空置。不足额不是错误——身份是站位不是编制，
///   `quota` 只封顶不保底（这正是"1 号世界 10 个官员、2 号世界 1 个"的来源：实际人数由种子决定）。
/// - `quota == 0`（建模板期已被 `validate_skeleton_refs` 拒绝）或 id 空白 → 运行期按「不可分配」处理。
///
/// **平权红线**：返回值只写进 assembled_json 的叙事段供 runtime 当"开局站位"读，
/// **绝不允许**任何下游据身份改判定、改发奖、开权限、调难度或改准入。
fn assign_identities(
    pool: &[IdentitySpec],
    cards: &[(String, CharacterCardV2)],
    in_play_ids: &std::collections::BTreeSet<&str>,
    seed: u64,
) -> Vec<(String, String)> {
    if pool.is_empty() || cards.is_empty() {
        return Vec::new(); // 老模板（无 identityPool）走原路径，零影响。
    }
    // 阵容：cid 升序去重（分配与入场顺序无关）。
    let mut roster: Vec<(&str, &CharacterCardV2)> =
        cards.iter().map(|(cid, card)| (cid.as_str(), card)).collect();
    roster.sort_unstable_by(|a, b| a.0.cmp(b.0));
    roster.dedup_by(|a, b| a.0 == b.0);

    // 余量按模板序（Vec 下标）跟踪 —— 不用 map 迭代序驱动 RNG。id 空白者置 0（不可分配）。
    let mut remaining: Vec<usize> =
        pool.iter().map(|s| if s.id.trim().is_empty() { 0 } else { s.quota }).collect();
    let mut rng = Rng(seed ^ DOMAIN_IDENTITY);
    let mut assignments: Vec<(String, String)> = Vec::new();

    for (cid, card) in roster {
        let terms = obsession_terms(card);
        let open: Vec<usize> = (0..pool.len()).filter(|i| remaining[*i] > 0).collect();
        if open.is_empty() {
            break; // 配额全部用尽 → 其余角色无身份（人多于 Σquota 的边界）。
        }
        let weighted: Vec<(&usize, f32)> = open
            .iter()
            .map(|i| {
                let spec = &pool[*i];
                let matched = identity_match_score(spec, &terms) as f32;
                let hooks = spec
                    .hook_affinity
                    .iter()
                    .filter(|h| {
                        let h = h.trim();
                        !h.is_empty() && in_play_ids.contains(h)
                    })
                    .count() as f32;
                let lead = if spec.is_lead { IDENTITY_LEAD_BOOST } else { 0.0 };
                (i, 1.0 + matched + IDENTITY_HOOK_BOOST * hooks + lead)
            })
            .collect();
        let Some(&picked) = weighted_pick_one(&mut rng, &weighted) else {
            break;
        };
        remaining[picked] -= 1;
        assignments.push((cid.to_string(), pool[picked].id.trim().to_string()));
    }
    assignments
}

/// 一次装配的被选子集（+ 钉住审计段）。`audit == None` → 退化路径（全量，不采样）。
struct Selection {
    audit: Option<InstanceSampling>,
    /// per-character 钩子可用的隐藏内容 id 子集（退化 = 全体，模板序）。
    hidden_ids: Vec<String>,
    /// 最终阵容加权启用的结局 id（对被选候选跑 weight_endings_scored 的结果；退化 = 全体池加权）。
    enabled_endings: Vec<String>,
    /// 在 `enabled_endings` 中按权重掷点定盘的那一个（`DOMAIN_ENDING` 子流）。池空 → None。
    selected_ending: Option<String>,
    /// 被选世界固有角色 id 子集（退化 = 全体）。
    npc_ids: Vec<String>,
    /// 被选地点 id 子集（退化 = 全体）。
    loc_ids: Vec<String>,
    /// 身份池分配结果 `(cid, identity_id)`，按 cid 升序。模板无 identityPool → 空（老模板零影响）。
    identity_assignments: Vec<(String, String)>,
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
/// `cards`（§5 身份池）：身份是分配给**入场卡**的，故除阵容派生量外还需要卡本体做内核匹配。
fn plan_sampling(
    skeleton: &Skeleton,
    fingerprint: &str,
    world_id: &str,
    template_version: i64,
    profile: &(u32, u32, u32),
    roster_terms: &[String],
    cards: &[(String, CharacterCardV2)],
    ending_threshold: f32,
    star_rating: i64,
    // 世界线烙印指纹（空 = 无烙印 ⇒ 种子与接线前逐字节相同）。
    //
    // 🔵 这里**只有指纹**：气运与机缘 2026-07-29 从世界级改成了**角色级**
    // （产品口径：这两个数对自己有影响、对世界没有影响），于是它们整个离开了本函数——
    // 世界这一层的采样回到了接线前的样子，逐字节。落点见 `assemble_instance` 的 per-character 钩子。
    imprint_fingerprint: &str,
) -> Selection {
    let superset_mode =
        skeleton.is_superset && !skeleton.storylines.is_empty() && !skeleton.sampling.is_empty();
    if !superset_mode {
        // 退化：全量，行为与改造前完全一致，sampling=None。
        // 身份池是**独立维度**（不搭超集判据的车）：模板声明了 identityPool 才分配；未声明 → 空 Vec
        // → 老模板零行为变化（assemble 侧 skip_serializing_if 保证 assembled_json 也逐字节不变）。
        // 退化路径无采样，故"在演内容" = 全部 storyline / 隐藏 / 支线钩子。
        let degraded_seed =
            resolve_instance_seed(skeleton, world_id, fingerprint, template_version, imprint_fingerprint);
        let identity_assignments = if skeleton.identity_pool.is_empty() {
            Vec::new()
        } else {
            let mut in_play: std::collections::BTreeSet<&str> =
                skeleton.storylines.iter().map(|s| s.id.as_str()).collect();
            in_play.extend(skeleton.hidden_content_pool.iter().map(|p| p.id.as_str()));
            in_play.extend(skeleton.side_hook_pool.iter().map(|p| p.id.as_str()));
            assign_identities(&skeleton.identity_pool, cards, &in_play, degraded_seed)
        };
        // 结局：退化路径同样要**定盘**（否则同模板同阵容的所有实例都落同一个结局，§5 不成立）。
        // 独立子流 `Rng(seed ^ DOMAIN_ENDING)`：本路径此前没有任何结局侧 RNG，新开这一次消费
        // 不与任何既有子流共享状态 → 其余维度（身份/隐藏/NPC/地点）逐字段不变。
        let scored = weight_endings_scored(&skeleton.ending_pool, profile, ending_threshold);
        let mut rng_end = Rng(degraded_seed ^ DOMAIN_ENDING);
        let selected_ending = pick_ending(&mut rng_end, &sorted_for_pick(&scored));
        return Selection {
            audit: None,
            hidden_ids: skeleton.hidden_content_pool.iter().map(|p| p.id.clone()).collect(),
            enabled_endings: scored.into_iter().map(|(id, _)| id).collect(),
            selected_ending,
            npc_ids: skeleton.world_characters.iter().map(|w| w.card.id.clone()).collect(),
            loc_ids: skeleton.locations.iter().map(|l| l.id.clone()).collect(),
            identity_assignments,
        };
    }

    let seed = resolve_instance_seed(skeleton, world_id, fingerprint, template_version, imprint_fingerprint);

    // 1) Storyline 脊柱（阵容依赖 + 种子扰动）。
    //    容器形态额外乘上**卡权重**（`subplotCardRefs[].weight`，按 id 前缀归属查表）——
    //    于是「卡 × storyline」的联合选取在**同一级采样**里完成，不必新开 RNG 子流域，
    //    也就不会扰动既有六个域的消费协议。非容器形态走原表达式，逐字节不变。
    let weighted_sl: Vec<(&StorylineSpec, f32)> = match &skeleton.container {
        None => skeleton.storylines.iter().map(|s| (s, 1.0 + affinity_boost(&s.affinity, profile))).collect(),
        Some(plan) => skeleton
            .storylines
            .iter()
            .map(|s| (s, (1.0 + affinity_boost(&s.affinity, profile)) * container_card_weight(plan, &s.id)))
            .collect(),
    };
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
    // ⚠️ 下限**也**必须被候选总数夹住：隐藏池为空（完全合法的世界：只有主线、没有间隙内容）
    // 时，写死成 1 就成了 `clamp(1, 0)` —— Rust 的 clamp 在 min > max 时 panic，装配整条挂掉返 500。
    // 空池必须落到 0，而不是「至少来一条」。（这条是气运那一版留下的教训，见 VALIDATION §3.63 ④。）
    let hidden_cap = resolved_hidden.len();
    let hk = skeleton.sampling.instance_hidden_count.unwrap_or(hidden_cap).min(hidden_cap);
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

    // 4) Endings（storyline 约束 + 变体组互斥 → weight_endings_scored 阵容加权 → pick_ending 掷点定盘）。
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
    let scored_endings = weight_endings_scored(&resolved_owned, profile, ending_threshold);
    let enabled_endings: Vec<String> = scored_endings.iter().map(|(id, _)| id.clone()).collect();
    // 结局定盘：**续用同一条 `DOMAIN_ENDING` 子流**（不新开域）。这次消费**排在
    // `resolve_variant_groups` 之后**，故变体分组与其后所有子流（NPC/地点/身份，各自独立域）
    // 的取数逐字节不变——新增的只是这条子流末尾多抽一个数。
    let selected_ending = pick_ending(&mut rng_end, &sorted_for_pick(&scored_endings));

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
    // 容器形态：枢纽 + 全部 seam 端点并入必选种子（技术附录 §3.3 连通性保障）——
    // 否则 BFS 可能只采到某一张卡的地点分量，实例内其余卡的地点变成到不了的孤岛。
    if let Some(plan) = &skeleton.container {
        loc_seeds.extend(plan.pinned_locations.iter().cloned());
    }
    let mut rng_loc = Rng(seed ^ DOMAIN_LOC);
    let loc_ids =
        sample_location_ids(&mut rng_loc, &skeleton.locations, &loc_seeds, skeleton.sampling.instance_location_count);

    // 7) 身份池分配（总规格 §5【拍板 4、5】"身份配额进采样域"）：内核匹配 + 种子随机。
    //    独立子流域 DOMAIN_IDENTITY，排在全部内容维度之后 → 不扰动前六段的 RNG 消费协议
    //    （加不加 identityPool，剧情采样结果逐字段不变；有专项回归守护）。
    //    "在演内容" = 本实例被选中的 storyline + 隐藏钩子 + 全部支线钩子（支线池不采样，恒在演）。
    //    这是**防刷第二重**的第三条腿：连身份分布都随实例种子变（1 号世界 10 个官员、2 号世界 1 个），
    //    外部攻略对不上盘。
    //    **平权红线（§0.1）**：分配结果只进叙事层——身份不带数值差异、不带准入、不带产出加成、
    //    不带叙事特权；戏份靠玩出来。任何下游据身份改判定/发奖/权限都属红线违规。
    let identity_assignments = if skeleton.identity_pool.is_empty() {
        Vec::new()
    } else {
        let mut in_play: std::collections::BTreeSet<&str> = sl_ids.iter().copied().collect();
        in_play.extend(hidden_ids.iter().map(String::as_str));
        in_play.extend(skeleton.side_hook_pool.iter().map(|p| p.id.as_str()));
        assign_identities(&skeleton.identity_pool, cards, &in_play, seed)
    };

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
        identity_assignments: identity_assignments.clone(),
        // 容器形态才写（`skip_serializing_if` 保证非容器实例 assembled_json 逐字节不变）。
        // 指纹只存哈希不存明文，口径同 `roster_fingerprint`。
        card_set_fingerprint: skeleton
            .container
            .as_ref()
            .map(|p| format!("{:016x}", fnv1a_64(p.fingerprint.as_bytes())))
            .unwrap_or_default(),
        selected_cards: skeleton
            .container
            .as_ref()
            .map(|p| p.card_ids.clone())
            .unwrap_or_default(),
    };
    Selection {
        audit: Some(audit),
        hidden_ids,
        enabled_endings,
        selected_ending,
        npc_ids,
        loc_ids,
        identity_assignments,
    }
}

// ============================================================================
// R2 自定义房装配：容器世界 + 命名空间 + 缝合 + 卡集合指纹
// 总规格 §10「自定义房闭环」；技术附录 `docs/build/spec-subplot-cards.md` §3/§4/§5
// （⚠️ 该文件 §1/§2/§6/§7/§8 业务假设已作废，只有 §3/§4/§5 技术方案有效）
// ============================================================================
//
// 🔴 四条硬约束，改本节前先读完
//
// ① **确定性可 replay**：同一 (world_id, 阵容, template_version, 卡集合) 必得同一份装配。
//    合并（compose）是纯函数、按模板序遍历；种子四段式；**禁三样**（系统随机 / 浮点 RNG /
//    map 迭代序驱动 RNG）在本节同样适用——本节压根不掷骰子，只做确定性重写与图运算。
//
// ② **平权（§0.1）**：副本卡是**内容燃料，永不加战力**。落点有三：
//    - 卡只贡献「内容四类」（剧情线 / 主线 / 内容池 / 结局 / NPC / 道具 / 地点），
//      **绝不贡献规则维度**——`payoutTable`（产出表）、`identityPool`（身份池）、
//      `assemblyRules`、`sampling`、`isSuperset`、`admission` 一律只认容器本体（`merge_fragment` 只搬内容）；
//    - 卡带入的道具 `powerTier` 在合并时被夹到 `min(容器星级, 卡星级)`（`translate_item` 只降不升语义，
//      `effectTags` 恒不变），杜绝「借高星卡在低星容器刷高价值道具」；
//    - 卡内道具逐件过容器 `admission` 闸（`check_admission`），不兼容即**建房期拒绝**，
//      没有"运行时静默降级"的中间态。
//
// ③ **未验证功能默认关闭（VALIDATION §0.1）**：`MUSE_CONTAINER_ASSEMBLY` 默认关闭。
//    前门拒绝（建模板期不许声明 `subplotCardRefs`）+ 读取侧降级（装配期忽略 refs，走原路径、
//    产物逐字节不变）双保险，范式抄 `worlds::deathmatch_enabled`。
//
// ④ **装配不消耗卡（§10【拍板 11】）**：副本卡是**永久蓝图**——「装入自定义房，房散卡在」。
//    装配只在 `world_container_cards` 记一行引用，**绝不 UPDATE/DELETE `subplot_cards`**
//    （唯一销毁语义是合成，归 `subplot/` 独占）。源码级断言守死，见
//    `container_tests::red_line_assembly_never_writes_subplot_cards`。

/// 容器装配（自定义房）运营开关环境变量。
const ENV_CONTAINER_ASSEMBLY: &str = "MUSE_CONTAINER_ASSEMBLY";

/// 容器装配默认值 = **关闭**（VALIDATION.md §0.1）。
///
/// 🔴 自定义房是副本卡经济闭环的消费端，属 T4「平台生态」才验证的范围；代码合并不等于对用户开放。
const DEFAULT_CONTAINER_ASSEMBLY_ENABLED: bool = false;

/// 命名空间分隔符：卡内 id 一律重写为 `{cardId}:{原id}`。
/// 容器本体 id **不加前缀**（见 `compose_container_skeleton` 头部说明）。
const NS_SEP: char = ':';

/// 容器枢纽地点 id（保留字）：合并后地点图裂成孤岛时自动生成，把各连通分量接起来。
const NEXUS_LOCATION_ID: &str = "loc-nexus";

/// 枢纽地点缺省名（容器可用 `nexus.name` 覆盖）。
const NEXUS_DEFAULT_NAME: &str = "交汇之地";

/// 🔴 **编译期钉死默认值的两个事实源**（本常量 + `flags::KNOWN_FLAGS`）。
const _: () = assert!(
    crate::flags::declared_default(ENV_CONTAINER_ASSEMBLY) == DEFAULT_CONTAINER_ASSEMBLY_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_CONTAINER_ASSEMBLY 的默认值必须与 DEFAULT_CONTAINER_ASSEMBLY_ENABLED 一致"
);

/// 容器装配是否已由运营开启。
///
/// 开关是**可逆急停阀**：关掉之后所有容器房**立即**降级为「只装容器本体」的普通装配
/// （种子回三段式、卡内容不进实例），再打开则原样恢复。已装配实例被 C-7 CAS 钉住不重掷，
/// 故开关不会让在跑的房内容漂移。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 已接入运行时开关体系（`crate::flags`）—— 两处 ctx 口径不同
/// ════════════════════════════════════════════════════════════════════════════
///
/// | 消费点 | ctx | 为什么 |
/// |---|---|---|
/// | 装配期（`load_container_cards`，忽略 refs 走原路径） | **world + global** | 世界已存在，按房灰度是「先在一个房试自定义房装配」的自然单位 |
/// | 建模板前门（`validate_container_refs`，不许声明 `subplotCardRefs`） | **只能 global** | 建模板时**没有世界**（模板是世界的蓝图），也没有 user 语义。结构性的，不是选择 |
///
/// ℹ️ 两侧口径不同在这里**不会造成半开半关**：全局关时模板根本声明不了 refs，于是也没有
/// 哪个世界会有 refs 可装；全局开而某房关，则那个房走「读取侧降级」——那正是本开关**设计上
/// 就有的**行为（「装配期忽略 refs，走原路径、产物逐字节不变」），不是新引入的不一致。
///
/// 🔴 **本函数不在事务里被调用**：`load_container_cards` 的唯一调用点在
/// `assemble_instance` 里、**C-7 那次 CAS 占位写入之前**；建模板前门更在事务之外。
/// 收 bool 的 `validate_container_refs` 保持同步，是为了让它继续是一个**纯校验函数**
/// （可被任意上下文复用），而不是因为事务边界。
pub async fn container_assembly_enabled(db: &AnyPool, world_id: Option<&str>) -> bool {
    let ctx = match world_id {
        Some(w) => crate::flags::FlagCtx::world(w),
        None => crate::flags::FlagCtx::global(),
    };
    crate::flags::is_enabled(db, ENV_CONTAINER_ASSEMBLY, ctx).await
}

// 测试专用的开关 RAII 夹具 `ContainerSwitch` 定义在文件末尾的 `container_tests` 模块里。
//
// ⚠️ 本文件的**生产代码段不得出现那个 test-only 编译属性的字面量**，连注释里都不许写出来
// （真要提，请像 `concat!("#[cfg", "(test)]")` 那样拆开）。原因：
// `member_order_tests::order_by_clause_pins_secondary_key` 与
// `container_tests::red_line_assembly_never_writes_subplot_cards` 都用 `include_str!("mod.rs")`
// 并按**该属性的首次出现**截断，只扫生产代码段；中途插一个（哪怕在注释里）就会把
// `load_container_cards` / `load_active_cards` 截在扫描范围之外，两条防回归断言随即静默失效（假绿）。

/// 一张已解引用的副本卡（DB 读取产物 → 合并器输入）。纯数据，合并是纯函数。
#[derive(Debug, Clone)]
struct ContainerCard {
    /// `subplot_cards.id`，同时是命名空间前缀。
    card_id: String,
    /// 卡蓝图版本（`subplot_cards.source_template_version`），进卡集合指纹。
    card_version: i64,
    /// 卡星级（`subplot_cards.star_rating`）：与容器星级取小，作为卡内道具的档位上限。
    star_rating: i64,
    /// 采样权重（容器 ref 声明）。
    weight: f32,
    /// 卡的内容蓝图 = `source_template_id` 指向的模板骨架（0032 原话：
    /// "来源世界与模板（内容蓝图指针，**自定义房装配的解引用入口**）"）。
    fragment: Skeleton,
}

/// 命名空间重写：`{cardId}:{原id}`。空 id 原样返回空（下游按"无引用"处理，不造出 `card:` 这种残 id）。
fn ns(card_id: &str, id: &str) -> String {
    let t = id.trim();
    if t.is_empty() {
        String::new()
    } else {
        format!("{card_id}{NS_SEP}{t}")
    }
}

/// 批量命名空间重写（保留模板序，空项丢弃）。
fn ns_all(card_id: &str, ids: &[String]) -> Vec<String> {
    ids.iter().map(|i| ns(card_id, i)).filter(|s| !s.is_empty()).collect()
}

/// 归属映射：`{cardId}:xxx` → `Some(cardId)`；裸 id（容器本体）→ `None`。
/// **前缀即归属，无需附表**——这是命名空间设计的全部价值。
fn ns_owner(id: &str) -> Option<&str> {
    id.split_once(NS_SEP).map(|(card, _)| card)
}

/// 容器 storyline 的采样权重乘数：按前缀查卡权重；容器本体（无前缀）恒 1.0。
fn container_card_weight(plan: &ContainerPlan, storyline_id: &str) -> f32 {
    match ns_owner(storyline_id) {
        Some(card) => plan
            .weights
            .iter()
            .find(|(c, _)| c == card)
            .map(|(_, w)| w.max(0.0))
            .unwrap_or(1.0),
        None => 1.0,
    }
}

/// 骨架内**全部会被当作 id 使用**的字符串（含定义位与引用位），供「不得含命名空间分隔符」校验。
/// 只收集 id 与 id 引用，不收集叙事文本（文本里出现冒号完全合法）。
fn collect_id_like(sk: &Skeleton) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let t = s.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
    };
    for n in &sk.mainline_nodes {
        push(&n.id);
        if let Some(g) = &n.variant_group {
            push(g);
        }
        n.arc_tags.iter().for_each(|t| push(t));
    }
    for s in &sk.storylines {
        push(&s.id);
        s.mainline_node_ids.iter().for_each(|t| push(t));
        s.hidden_pool_ids.iter().for_each(|t| push(t));
        s.ending_ids.iter().for_each(|t| push(t));
    }
    for pool in [&sk.hidden_content_pool, &sk.side_hook_pool] {
        for p in pool {
            push(&p.id);
            if let Some(r) = &p.reward_item_ref {
                push(r);
            }
            if let Some(g) = &p.variant_group {
                push(g);
            }
            p.arc_tags.iter().for_each(|t| push(t));
        }
    }
    for e in &sk.ending_pool {
        push(&e.id);
        if let Some(g) = &e.variant_group {
            push(g);
        }
        e.arc_tags.iter().for_each(|t| push(t));
    }
    for it in &sk.world_items {
        push(&it.id);
    }
    for l in &sk.locations {
        push(&l.id);
        l.connections.iter().for_each(|c| push(c));
        l.resident_item_ids.iter().for_each(|i| push(i));
        if let Some(g) = &l.gate {
            g.required_item_ids.iter().for_each(|i| push(i));
        }
    }
    for wc in &sk.world_characters {
        push(&wc.card.id);
        push(&wc.home_location);
        wc.carried_item_ids.iter().for_each(|i| push(i));
        wc.agenda_nodes.iter().for_each(|n| push(n));
    }
    for a in &sk.anchors {
        push(a);
    }
    out
}

/// 把一张卡的内容块**全命名空间化**后并入容器骨架（技术附录 §3.2）。
///
/// 重写覆盖**定义位与引用位全集**：
/// `mainlineNodes.id/variantGroup/arcTags` · `storylines.id` 及其三个 id 列表 ·
/// `hiddenContentPool`/`sideHookPool` 的 `id/rewardItemRef/variantGroup/arcTags` ·
/// `endingPool.id/variantGroup/arcTags` · `worldItems.id` ·
/// `locations.id/connections/residentItemIds/gate.requiredItemIds` ·
/// `worldCharacters.card.id/carriedItemIds/homeLocation/agendaNodes`。
///
/// 两个有意的语义：
/// - **`variantGroup` 带前缀 ⇒ 互斥组天然不跨卡**（不同小说的变体不该互相排斥）；
/// - **不搬规则维度**：产出表 / 身份池 / 装配规则 / 采样计数 / 超集标记 / 来源作品 / 嵌套卡引用
///   一律不从卡继承（§0.1 平权：卡是内容燃料，不带来产出加成、准入豁免或规则特权；
///   顺带杜绝「卡里再引用卡」的递归炸弹）。
fn merge_fragment(target: &mut Skeleton, card: &ContainerCard, item_tier_cap: u8) {
    let cid = card.card_id.as_str();
    let f = &card.fragment;

    for n in &f.mainline_nodes {
        target.mainline_nodes.push(MainlineNode {
            id: ns(cid, &n.id),
            fated: n.fated,
            variant_group: n.variant_group.as_deref().map(|g| ns(cid, g)).filter(|s| !s.is_empty()),
            arc_tags: ns_all(cid, &n.arc_tags),
        });
    }
    for s in &f.storylines {
        target.storylines.push(StorylineSpec {
            id: ns(cid, &s.id),
            mainline_node_ids: ns_all(cid, &s.mainline_node_ids),
            hidden_pool_ids: ns_all(cid, &s.hidden_pool_ids),
            ending_ids: ns_all(cid, &s.ending_ids),
            affinity: s.affinity.clone(),
        });
    }
    let rewrite_pool = |p: &PoolItem| PoolItem {
        id: ns(cid, &p.id),
        themes: p.themes.clone(),
        template: p.template.clone(),
        difficulty_base: p.difficulty_base,
        reward_item_ref: p.reward_item_ref.as_deref().map(|r| ns(cid, r)).filter(|s| !s.is_empty()),
        // 内联奖励同样夹档：`reward_item_ref` 与内联 `reward_item` 是同一口径（`reward_tier`），
        // 不夹内联就等于给了一条绕过封顶的内联后门。
        reward_item: p.reward_item.as_ref().map(|it| cap_item(it, cid, item_tier_cap)),
        variant_group: p.variant_group.as_deref().map(|g| ns(cid, g)).filter(|s| !s.is_empty()),
        arc_tags: ns_all(cid, &p.arc_tags),
    };
    target.hidden_content_pool.extend(f.hidden_content_pool.iter().map(&rewrite_pool));
    target.side_hook_pool.extend(f.side_hook_pool.iter().map(&rewrite_pool));
    for e in &f.ending_pool {
        target.ending_pool.push(EndingCandidate {
            id: ns(cid, &e.id),
            affinity: e.affinity.clone(),
            base_weight: e.base_weight,
            variant_group: e.variant_group.as_deref().map(|g| ns(cid, g)).filter(|s| !s.is_empty()),
            arc_tags: ns_all(cid, &e.arc_tags),
        });
    }
    target.world_items.extend(f.world_items.iter().map(|it| cap_item(it, cid, item_tier_cap)));
    for l in &f.locations {
        target.locations.push(LocationSpec {
            id: ns(cid, &l.id),
            name: l.name.clone(),
            connections: ns_all(cid, &l.connections),
            is_secret_realm: l.is_secret_realm,
            gate: l.gate.as_ref().map(|g| {
                let mut g = g.clone();
                g.required_item_ids = ns_all(cid, &g.required_item_ids);
                g
            }),
            resident_item_ids: ns_all(cid, &l.resident_item_ids),
        });
    }
    for wc in &f.world_characters {
        let mut card_v2 = wc.card.clone();
        card_v2.id = ns(cid, &wc.card.id);
        target.world_characters.push(WorldCharacter {
            card: card_v2,
            home_location: ns(cid, &wc.home_location),
            carried_item_ids: ns_all(cid, &wc.carried_item_ids),
            agenda_nodes: ns_all(cid, &wc.agenda_nodes),
        });
    }
}

/// 卡内道具的命名空间化 + **档位夹持**（§0.1 平权红线：卡永不加战力）。
/// 语义逐字对齐 `admission::translate_item`：`power_tier` 只降不升，`effect_tags` 恒不变。
fn cap_item(item: &ItemDefinition, card_id: &str, tier_cap: u8) -> ItemDefinition {
    let mut it = item.clone();
    it.id = ns(card_id, &item.id);
    if it.origin.power_tier > tier_cap {
        it.origin.power_tier = tier_cap;
    }
    it
}

/// 地点图的**无向连通分量**（下标分组，按模板序）。用于判断多卡混装后是否裂成孤岛。
fn location_components(locations: &[LocationSpec]) -> Vec<Vec<usize>> {
    let n = locations.len();
    let idx_of = |id: &str| locations.iter().position(|l| l.id == id);
    // 邻接表（无向：connections 两端都记）。
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, l) in locations.iter().enumerate() {
        for c in &l.connections {
            if let Some(j) = idx_of(c) {
                if i != j {
                    if !adj[i].contains(&j) {
                        adj[i].push(j);
                    }
                    if !adj[j].contains(&i) {
                        adj[j].push(i);
                    }
                }
            }
        }
    }
    let mut seen = vec![false; n];
    let mut comps: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if seen[start] {
            continue;
        }
        let mut comp = vec![start];
        seen[start] = true;
        let mut head = 0usize;
        while head < comp.len() {
            let cur = comp[head];
            head += 1;
            for &nb in &adj[cur] {
                if !seen[nb] {
                    seen[nb] = true;
                    comp.push(nb);
                }
            }
        }
        comp.sort_unstable(); // 模板序，确定性输出。
        comps.push(comp);
    }
    comps
}

/// 某个连通分量的缝合代表：白名单内的首个 anchor；无 anchor 则首个非秘境地点。
/// 全是秘境 → `None`（**秘境不可作缝合口**，gate 语义必须完整保留在卡内）。
fn component_anchor(
    locations: &[LocationSpec],
    comp: &[usize],
    anchors: &std::collections::BTreeSet<String>,
) -> Option<String> {
    comp.iter()
        .map(|&i| &locations[i])
        .find(|l| !l.is_secret_realm && anchors.contains(&l.id))
        .or_else(|| comp.iter().map(|&i| &locations[i]).find(|l| !l.is_secret_realm))
        .map(|l| l.id.clone())
}

/// 加一条**双向**连接（幂等；两端须存在，由调用方前置校验）。
fn link_bidirectional(locations: &mut [LocationSpec], a: &str, b: &str) {
    for l in locations.iter_mut() {
        if l.id == a && !l.connections.iter().any(|c| c == b) {
            l.connections.push(b.to_string());
        } else if l.id == b && !l.connections.iter().any(|c| c == a) {
            l.connections.push(a.to_string());
        }
    }
}

/// **容器合并器**（纯函数，可单测；技术附录 §3.2 + §3.3 + §4.1 + §5.1）。
///
/// 输入：容器本体骨架 + 已解引用的卡集合 + 容器星级 + 容器准入策略。
/// 输出：合并后的骨架（`container` 计划已填），或**建房期拒绝**的中文原因。
///
/// ⚠️ **容器本体 id 不加前缀**（与技术附录「隐式卡前缀 = 模板 id」的差异，有意为之）：
/// 章节钩子的发货幂等键是 `hook_key = {world_id}:{cid}:{pool_item_id}`（`chapters/mod.rs`），
/// 若本体 id 随容器开关一开一关而漂移，同一钩子会被算成两个 key → **重复发货**。
/// 开关是可逆急停阀，因此本体 id 必须在两种形态下逐字相同。
/// 归属仍然明确：**裸 id = 容器本体，带前缀 = 卡**（`ns_owner`），且本体 id 不许含 `:`（建房期校验）
/// 保证两个空间不可能相交。
///
/// 步骤（全程无 RNG、按模板序）：
/// 1. 本体自查：id 不含分隔符、未占用枢纽保留 id；
/// 2. 逐卡校验 + 命名空间化并入（卡内引用须闭包在卡内；道具过准入闸 + 档位夹持）；
/// 3. 全局 id 唯一性复核（前缀在构造上保证，此处是"断言而非期望"）；
/// 4. 显式 seams 落边（两端须在 anchors 白名单、非秘境）；
/// 5. 连通性保障：仍有多个分量 → 生成枢纽地点接上各分量代表；
/// 6. 产出 `ContainerPlan`（卡集合指纹 / 权重 / 钉住地点 / 卡 id）。
fn compose_container_skeleton(
    container: Skeleton,
    cards: &[ContainerCard],
    star_rating: i64,
    policy: &crate::admission::WorldAdmissionPolicy,
) -> Result<Skeleton, String> {
    let mut merged = container;

    // 1) 容器本体自查。
    for id in collect_id_like(&merged) {
        if id.contains(NS_SEP) {
            return Err(format!(
                "容器本体 id `{id}` 含保留分隔符 `{NS_SEP}`：装卡的容器里，`{NS_SEP}` 是命名空间前缀专用（`卡id:原id`），本体 id 不得占用"
            ));
        }
    }
    if merged.locations.iter().any(|l| l.id == NEXUS_LOCATION_ID) {
        return Err(format!(
            "地点 id `{NEXUS_LOCATION_ID}` 是容器枢纽保留字：请给本体地点换个 id"
        ));
    }

    // 2) 逐卡合并（模板序）。
    let container_cap = star_rating.clamp(0, u8::MAX as i64) as u8;
    for card in cards {
        let cid = card.card_id.as_str();
        // 2a) 卡内 id 不得含分隔符（否则前缀解析会把归属算错）。
        for id in collect_id_like(&card.fragment) {
            if id.contains(NS_SEP) {
                return Err(format!(
                    "副本卡 `{cid}` 的内容 id `{id}` 含保留分隔符 `{NS_SEP}`：命名空间前缀无法解析"
                ));
            }
        }
        // 2b) 卡内引用闭包：跨卡连接只能经 seams，卡内 connections 不许指向卡外。
        validate_fragment_closure(cid, &card.fragment)?;
        // 2c) cosmology / 强度兼容（技术附录 §5.1，全部复用既有闸门，零新机制）。
        //     不兼容 → **建房期拒绝**，不留到运行时静默退化。
        let card_cap = card.star_rating.clamp(0, u8::MAX as i64) as u8;
        let tier_cap = container_cap.min(card_cap);
        for item in &card.fragment.world_items {
            check_card_item_admission(cid, item, policy)?;
        }
        // 2d) 命名空间化并入（含道具档位夹持）。
        merge_fragment(&mut merged, card, tier_cap);
    }

    // 3) 全局 id 唯一性复核（前缀保证不可能撞车；此处按"断言"处理，撞了说明前缀逻辑坏了）。
    assert_unique_ids(&merged)?;

    // 4) 显式缝合边。anchors 白名单 = 容器本体 anchors ∪ 各卡命名空间化后的 anchors。
    let mut anchor_set: std::collections::BTreeSet<String> =
        merged.anchors.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    for card in cards {
        for a in ns_all(&card.card_id, &card.fragment.anchors) {
            anchor_set.insert(a);
        }
    }
    let loc_ids: std::collections::BTreeSet<String> =
        merged.locations.iter().map(|l| l.id.clone()).collect();
    let mut pinned: Vec<String> = Vec::new();
    let seams = merged.seams.clone();
    for seam in &seams {
        let (from, to) = (seam.from.trim().to_string(), seam.to.trim().to_string());
        for end in [&from, &to] {
            if !loc_ids.contains(end.as_str()) {
                return Err(format!("seams 悬空：缝合口 `{end}` 不是本容器（含所装卡）的地点"));
            }
            if !anchor_set.contains(end.as_str()) {
                return Err(format!(
                    "seams 越界：缝合口 `{end}` 不在其声明的 anchors 白名单内（缝合边只能落在锚点上）"
                ));
            }
            if merged.locations.iter().any(|l| &l.id == end && l.is_secret_realm) {
                return Err(format!("seams 非法：秘境 `{end}` 不可作缝合口（gate 语义须完整保留在卡内）"));
            }
        }
        link_bidirectional(&mut merged.locations, &from, &to);
        pinned.push(from);
        pinned.push(to);
    }

    // 5) 连通性保障：仍裂成多个分量 → 生成枢纽地点，把各分量代表锚点接上去。
    //    只在真的不连通时才造枢纽——本来就连通的容器不该凭空多出一个地点。
    let comps = location_components(&merged.locations);
    if comps.len() > 1 {
        let mut reps: Vec<String> = Vec::new();
        for comp in &comps {
            match component_anchor(&merged.locations, comp, &anchor_set) {
                Some(rep) => reps.push(rep),
                None => {
                    let sample = comp.first().map(|&i| merged.locations[i].id.clone()).unwrap_or_default();
                    return Err(format!(
                        "地点图不连通且无合法缝合口：含 `{sample}` 的分量全是秘境，无法接入枢纽（请给它声明非秘境 anchors）"
                    ));
                }
            }
        }
        let nexus_name = merged
            .nexus
            .as_ref()
            .map(|n| n.name.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(NEXUS_DEFAULT_NAME)
            .to_string();
        merged.locations.push(LocationSpec {
            id: NEXUS_LOCATION_ID.to_string(),
            name: nexus_name,
            connections: Vec::new(),
            is_secret_realm: false,
            gate: None,
            resident_item_ids: Vec::new(),
        });
        for rep in &reps {
            link_bidirectional(&mut merged.locations, NEXUS_LOCATION_ID, rep);
        }
        pinned.push(NEXUS_LOCATION_ID.to_string());
        pinned.extend(reps);
    }

    // 6) 计划落定：卡集合指纹（排序去重）+ 权重 + 钉住地点 + 卡 id（模板序）。
    let mut fp_parts: Vec<String> =
        cards.iter().map(|c| format!("{}@{}", c.card_id, c.card_version)).collect();
    fp_parts.sort_unstable();
    fp_parts.dedup();
    pinned.sort_unstable();
    pinned.dedup();
    merged.container = Some(ContainerPlan {
        fingerprint: fp_parts.join("\n"),
        weights: cards.iter().map(|c| (c.card_id.clone(), c.weight)).collect(),
        pinned_locations: pinned,
        card_ids: cards.iter().map(|c| c.card_id.clone()).collect(),
    });
    Ok(merged)
}

/// 卡内引用闭包校验（技术附录 §3.3：卡内 `connections` 只许指向卡内地点）。
/// 卡片段来自已过审模板，正常闭包成立；此处是**建房期硬门**，不成立就拒绝装入而不是运行时静默丢弃。
fn validate_fragment_closure(card_id: &str, f: &Skeleton) -> Result<(), String> {
    let loc_ids: std::collections::BTreeSet<&str> = f.locations.iter().map(|l| l.id.as_str()).collect();
    let item_ids: std::collections::BTreeSet<&str> = f.world_items.iter().map(|i| i.id.as_str()).collect();
    for l in &f.locations {
        for c in &l.connections {
            if !loc_ids.contains(c.as_str()) {
                return Err(format!(
                    "副本卡 `{card_id}` 引用悬空：地点 `{}` 连向卡外地点 `{c}`（跨卡连接只能经容器 seams 声明）",
                    l.id
                ));
            }
        }
        for iid in &l.resident_item_ids {
            if !item_ids.contains(iid.as_str()) {
                return Err(format!(
                    "副本卡 `{card_id}` 引用悬空：地点 `{}` 的驻留道具 `{iid}` 不在本卡道具目录内",
                    l.id
                ));
            }
        }
    }
    for wc in &f.world_characters {
        let home = wc.home_location.trim();
        if !home.is_empty() && !loc_ids.contains(home) {
            return Err(format!(
                "副本卡 `{card_id}` 引用悬空：世界角色 `{}` 落在卡外地点 `{home}`",
                wc.card.id
            ));
        }
        for iid in &wc.carried_item_ids {
            if !item_ids.contains(iid.as_str()) {
                return Err(format!(
                    "副本卡 `{card_id}` 引用悬空：世界角色 `{}` 携带卡外道具 `{iid}`",
                    wc.card.id
                ));
            }
        }
    }
    for a in &f.anchors {
        let a = a.trim();
        if !a.is_empty() && !loc_ids.contains(a) {
            return Err(format!("副本卡 `{card_id}` 引用悬空：anchors 指向不存在的地点 `{a}`"));
        }
    }
    Ok(())
}

/// 卡内道具的准入相容判定（技术附录 §5.1）：直接复用 `admission::check_admission`，零新机制。
/// - `Admitted` → 放行；
/// - `Translated` → 放行（容器显式声明 `rejectedHandling=translate` 才可能走到，
///   合并时会按 `min(容器星级, 卡星级)` 夹档，语义与 `translate_item` 一致：只降不升）；
/// - `Rejected` / `Sealed` → **建房期拒绝**（不兼容的世界观不该缝在一起，也不该运行时静默退化）；
/// - 体系标签不在 `KNOWN_COSMOLOGIES` → 拒绝（自由文本一律不收）。
fn check_card_item_admission(
    card_id: &str,
    item: &ItemDefinition,
    policy: &crate::admission::WorldAdmissionPolicy,
) -> Result<(), String> {
    use crate::admission::AdmissionDecision;
    match crate::admission::check_admission(policy, item) {
        Ok(AdmissionDecision::Admitted) | Ok(AdmissionDecision::Translated) => Ok(()),
        Ok(AdmissionDecision::Rejected) | Ok(AdmissionDecision::Sealed) => Err(format!(
            "cosmology 不相容：副本卡 `{card_id}` 的道具 `{}`（体系 {:?} / 档位 {}）不被本容器的准入策略接受",
            item.id, item.origin.cosmology, item.origin.power_tier
        )),
        Err(e) => Err(format!("副本卡 `{card_id}` 的道具 `{}` 准入校验失败：{e}", item.id)),
    }
}

/// 合并后全局 id 唯一性复核（同类目内不得重复）。命名空间前缀在构造上已保证，
/// 这里当**断言**用：撞了说明前缀逻辑被改坏了，必须在建房期炸出来而不是运行时静默采样出鬼东西。
fn assert_unique_ids(sk: &Skeleton) -> Result<(), String> {
    let dup = |kind: &str, ids: Vec<&str>| -> Result<(), String> {
        let mut seen: std::collections::BTreeSet<&str> = Default::default();
        for id in ids {
            if !id.is_empty() && !seen.insert(id) {
                return Err(format!("{kind} id 冲突：`{id}` 在合并后的容器里出现多次"));
            }
        }
        Ok(())
    };
    dup("mainlineNodes", sk.mainline_nodes.iter().map(|n| n.id.as_str()).collect())?;
    dup("storylines", sk.storylines.iter().map(|s| s.id.as_str()).collect())?;
    dup(
        "内容池",
        sk.hidden_content_pool
            .iter()
            .chain(sk.side_hook_pool.iter())
            .map(|p| p.id.as_str())
            .collect(),
    )?;
    dup("endingPool", sk.ending_pool.iter().map(|e| e.id.as_str()).collect())?;
    dup("worldItems", sk.world_items.iter().map(|i| i.id.as_str()).collect())?;
    dup("locations", sk.locations.iter().map(|l| l.id.as_str()).collect())?;
    dup("worldCharacters", sk.world_characters.iter().map(|w| w.card.id.as_str()).collect())?;
    Ok(())
}

/// 解引用容器模板声明的副本卡（DB 侧）。返回空 Vec = **非容器形态**（走原路径）。
///
/// 关闭开关或模板未声明 `subplotCardRefs` → 直接空（读取侧降级，产物逐字节不变）。
/// 其余任何一条不成立都是**建房期拒绝**（400），不静默跳过某张卡：
/// 少装一张卡 = 内容不同 = 种子不同，静默容忍等于给了"卡不合法就退化重刷"的旁路。
async fn load_container_cards(
    db: &AnyPool,
    world: &crate::worlds::WorldRow,
    skeleton: &Skeleton,
) -> Result<Vec<ContainerCard>, ApiError> {
    if skeleton.subplot_card_refs.is_empty()
        || !container_assembly_enabled(db, Some(&world.id)).await
    {
        return Ok(Vec::new());
    }
    // 房主：无交易红线（§10「玩家间交易暂不开」）下，只能装自己的卡。
    let Some(host) = world.host_user_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(ApiError::BadRequest(
            "容器房缺少房主：副本卡只能由卡主本人装入自己的房".into(),
        ));
    };

    let mut out: Vec<ContainerCard> = Vec::new();
    for card_ref in &skeleton.subplot_card_refs {
        let card_id = card_ref.card_id.trim();
        if card_id.is_empty() {
            return Err(ApiError::BadRequest("subplotCardRefs 含空 cardId".into()));
        }
        let row = sqlx::query(
            "SELECT owner_id, star_rating, status, source_template_id, source_template_version \
             FROM subplot_cards WHERE id = $1",
        )
        .bind(card_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::BadRequest(format!("副本卡 `{card_id}` 不存在")))?;

        let owner: String = row.try_get("owner_id")?;
        if owner != host {
            // 不泄露"这张卡存在但不是你的"，与卡不存在同一措辞。
            return Err(ApiError::BadRequest(format!("副本卡 `{card_id}` 不存在")));
        }
        let status: String = row.try_get("status")?;
        if status != "owned" {
            return Err(ApiError::BadRequest(format!(
                "副本卡 `{card_id}` 已作为合成材料销毁，不可装入"
            )));
        }
        let star_rating: i64 = row.try_get("star_rating")?;
        let source_template_id: Option<String> = row.try_get("source_template_id")?;
        let source_template_version: Option<i64> = row.try_get("source_template_version")?;
        let (Some(tpl_id), Some(tpl_ver)) = (source_template_id, source_template_version) else {
            return Err(ApiError::BadRequest(format!(
                "副本卡 `{card_id}` 没有内容蓝图（来源模板缺失），不可作自定义房的内容燃料"
            )));
        };
        // 版本钉住：客户端声明了就必须与服务端一致；进指纹的恒是服务端值。
        if let Some(declared) = card_ref.card_version {
            if declared != tpl_ver {
                return Err(ApiError::BadRequest(format!(
                    "副本卡 `{card_id}` 版本不匹配：声明 {declared}，实际 {tpl_ver}（卡发新版不自动生效，请发容器新版本）"
                )));
            }
        }
        // 蓝图解引用：卡的内容 = 来源模板骨架。下架/未过审的来源 → 停止后续建房（§3.1）。
        let tpl = sqlx::query(
            "SELECT skeleton_json, moderation, withdrawn FROM world_templates WHERE id = $1",
        )
        .bind(&tpl_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest(format!("副本卡 `{card_id}` 的内容蓝图已不存在"))
        })?;
        let moderation: String = tpl.try_get("moderation")?;
        let withdrawn: i64 = tpl.try_get("withdrawn")?;
        if moderation != "approved" || withdrawn != 0 {
            return Err(ApiError::BadRequest(format!(
                "副本卡 `{card_id}` 的内容蓝图已下架或未过审，不可装入"
            )));
        }
        let raw: String = tpl.try_get("skeleton_json")?;
        let fragment: Skeleton = serde_json::from_str(&raw).unwrap_or_default();

        let weight = if card_ref.weight.is_finite() { card_ref.weight.max(0.0) } else { 1.0 };
        out.push(ContainerCard {
            card_id: card_id.to_string(),
            card_version: tpl_ver,
            star_rating,
            weight,
            fragment,
        });
    }
    Ok(out)
}

/// 记录「本房装了哪几张卡」（migration 0033）。
///
/// 🔴 **装配不消耗卡**（§10【拍板 11】"永久蓝图：装入自定义房，房散卡在"）：
/// 本函数只在 `world_container_cards` 里 INSERT 引用行，**从不 UPDATE/DELETE `subplot_cards`**。
/// 幂等：先读已有 card_id 集合、只补缺失行；DB 唯一索引 (world_id, card_id) 是最后一道防线。
async fn record_container_cards(
    db: &AnyPool,
    world: &crate::worlds::WorldRow,
    cards: &[ContainerCard],
) -> Result<(), ApiError> {
    if cards.is_empty() {
        return Ok(());
    }
    let existing: Vec<String> =
        sqlx::query("SELECT card_id FROM world_container_cards WHERE world_id = $1")
            .bind(&world.id)
            .fetch_all(db)
            .await?
            .iter()
            .map(|r| r.try_get::<String, _>("card_id"))
            .collect::<Result<Vec<_>, _>>()?;
    let owner = world.host_user_id.clone().unwrap_or_default();
    let now = now_ms();
    for (slot, card) in cards.iter().enumerate() {
        if existing.iter().any(|c| c == &card.card_id) {
            continue;
        }
        sqlx::query(
            "INSERT INTO world_container_cards \
             (id, world_id, card_id, card_version, owner_id, template_id, slot_no, assembled_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(crate::db::new_id("wcc"))
        .bind(&world.id)
        .bind(&card.card_id)
        .bind(card.card_version)
        .bind(&owner)
        .bind(&world.template_id)
        .bind(slot as i64)
        .bind(now)
        .execute(db)
        .await?;
    }
    Ok(())
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
    let row = sqlx::query("SELECT assembled_json FROM worlds WHERE id = $1")
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
    sqlx::query("UPDATE worlds SET assembled_json = $1, updated_at = $2 WHERE id = $3")
        .bind(wrapper.to_string())
        .bind(now_ms())
        .bind(world_id)
        .execute(db)
        .await?;
    Ok(())
}

/// 仅测试可见的种子入口：让 `imprint` 模块能钉住「零烙印逐字节不变」这条接缝，
/// 而不必把 `instance_seed` / `resolve_instance_seed` 改成 `pub`
/// （它们的字节口径被黄金世界回归锁死，暴露出去等于给了改它的方便）。
#[cfg(test)]
pub(crate) mod testing {
    use super::{instance_seed, resolve_instance_seed, Skeleton};

    pub(crate) fn instance_seed_for_test(world_id: &str, fp: &str, ver: i64) -> u64 {
        instance_seed(world_id, fp, ver)
    }

    pub(crate) fn resolve_seed_for_test(world_id: &str, fp: &str, ver: i64, imprint_fp: &str) -> u64 {
        resolve_instance_seed(&Skeleton::default(), world_id, fp, ver, imprint_fp)
    }
}

// ---------- 核心：开局装配 ----------

/// 读取骨架 + 全体入场角色卡 → per-character 钩子 / 结局加权 / 阵容参数 / 主场标注。
/// 连接文本过机审后生效，结果写 worlds.assembled_json（钉住）。返回装配结果。
pub async fn assemble_instance(state: &AppState, world_id: &str) -> Result<AssembledInstance, ApiError> {
    let world = load_world(&state.db, world_id).await?;

    // 骨架（预审核池）：缺失/解析失败 → 空池（装配退化为无个性化，但不 panic）。
    // star_rating 同查读出：产出封顶输入 + 快照进 assembled_json（服务端留档）。
    // admission 同查读出：容器混装的 cosmology 相容判定输入（技术附录 §5.1）。
    let (skeleton, star_rating, admission) = load_skeleton(&state.db, &world.template_id).await?;

    // R2 自定义房：容器形态解引用 + 合并（开关关闭 / 模板未声明 subplotCardRefs → 空 → 原路径）。
    // 合并失败一律 400 **拒绝装配**（房开不起来），不静默退化——静默退化等于给了"内容不合法就
    // 退回普通装配再重刷"的旁路，且会让玩家在不知情下玩到缺内容的房。
    //
    // ⚠️ 「卡被下架 → 停止后续建房，**已钉住实例照旧运行**」靠调用方的前置条件成立：
    // 两个调用点（`chapters::start` 的 `assembly_of(&wrapper).is_none()`、`runtime` 的
    // `world.assembled_json.is_none()`）都只在**尚未装配**时进来，故已钉住的房永不重走本段。
    let container_cards = load_container_cards(&state.db, &world, &skeleton).await?;
    let skeleton = if container_cards.is_empty() {
        skeleton
    } else {
        let composed =
            compose_container_skeleton(skeleton, &container_cards, star_rating, &admission)
                .map_err(ApiError::BadRequest)?;
        // 🔴 装配**不消耗卡**：只在 world_container_cards 记引用行，subplot_cards 一个字节都不改。
        record_container_cards(&state.db, &world, &container_cards).await?;
        composed
    };

    // 全体在场成员卡。
    let cards = load_active_cards(&state.db, world_id).await?;

    let rules = &skeleton.assembly_rules;
    let profile = roster_profile(&cards);

    // 装配采样（防刷第二环）：固定实例种子 → 从超集各池采子集（退化路径 audit=None，全量装配）。
    // 种子由已钉住输入（world_id + 阵容指纹 + template_version）算出，纯函数、可 replay。
    let fingerprint = roster_fingerprint(&cards);

    // 世界线烙印指纹（提案 `spec-worldline-imprint.md` §2.2「共鸣 · 结构层」）：
    // 把这批卡各自带来的经历哈希进实例种子 ⇒ **内核相同但经历不同的两张卡，抽到的剧情不同**。
    //
    // 🔴 **零烙印 → 空串 → 种子逐字节不变**（`resolve_instance_seed` 里那一句早退）。
    // 全新库、全新卡没有烙印，黄金世界回归因此一个 bit 都不受影响——这一层是纯增量。
    //
    // ⚠️ 读失败一律降级为空串（按无烙印装配），不阻断建房：烙印是**叙事差异化**，
    // 不是资格判定；让一次读库抖动把整个房开不起来，代价与收益完全不成比例。
    //
    // 同一批行还派生**每张卡自己的**气运档与机缘档（`card_swing_levels`）——
    // 🔴 授予点数在这里恒为空（`no_swing_grants`）：道具加值是**预留接口，本轮不接线**。
    // 🔴 它**不进 `plan_sampling`**：气运机缘作用于角色不作用于世界，世界那一层的采样
    // 与接线前逐字节相同。落点是下面的 per-character 钩子。
    let (imprint_fp, swing_by_card) = {
        let cids: Vec<String> = cards.iter().map(|(c, _)| c.clone()).collect();
        match crate::imprint::load_imprints_for_cards(&state.db, &cids).await {
            Ok(rows) => (
                crate::imprint::imprint_fingerprint(&rows),
                crate::imprint::card_swing_levels(&rows, &cids, &crate::imprint::no_swing_grants()),
            ),
            Err(e) => {
                tracing::warn!(world_id, error = %e, "烙印读取失败，本次按无烙印装配");
                (String::new(), Default::default())
            }
        }
    };
    let agg_terms = aggregate_obsession_terms(&cards);
    let selection = plan_sampling(
        &skeleton,
        &fingerprint,
        world_id,
        world.template_version,
        &profile,
        &agg_terms,
        &cards,
        rules.ending_weight_threshold,
        star_rating,
        &imprint_fp,
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
        // 🔴 **这张卡自己的**气运与机缘（不是世界的、不是平均的）。无烙印 → (0,0) → 两处恒等。
        let (fortune_level, opportunity_level) = swing_by_card.get(cid).copied().unwrap_or((0, 0));
        // 气运改「先轮到哪些」（排序），机缘改「拿几个」（配额）——两个数各管一头。
        let candidates = rank_pool_items(&hidden_pool, &terms, fortune_level);
        let quota = personal_hook_quota(rules.hidden_per_character.max(1), opportunity_level);
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

    // 3) 结局：采样已按 storyline 约束 + 变体组互斥 + 阵容加权算出 enabled_endings（退化 = 全池加权），
    //    并在其中按权重掷点**定盘**一个 selected_ending（`DOMAIN_ENDING` 子流，随实例钉住）。
    //    两者语义分离：enabled = 台上有哪些；selected = 最终落到哪一个（`runtime::select_ending` 读后者）。
    let enabled_endings = selection.enabled_endings.clone();
    let selected_ending = selection.selected_ending.clone();

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
        selected_ending,
        lineup_params,
        difficulty_notes,
        home_advantages,
        world_character_entries,
        location_graph,
        resident_items,
        // 7) 身份池分配（§5 拍板 4、5）：随实例钉住，runtime 读回后作叙事层开局站位。
        //    **平权红线**：只描述"你在这个世界站哪个位"，不带任何数值 / 准入 / 产出 / 特权差异。
        identity_assignments: selection.identity_assignments,
        sampling: selection.audit,
        // 8) 公示产出表（§10 拍板 17）：骨架声明什么就钉什么（不做任何加权/挑选/随机），
        //    结算侧据此查表确定发放——"同贡献分必得同产出"的单一事实源。
        payout_table: skeleton.payout_table.clone(),
        // 9) 境界档（§6 拍板 3 戏服原则）：同样是"声明什么就钉什么"——**全员统一的一件戏服**，
        //    无分配、无抽样、无数值（见 `RealmTier` 三条设计约束）。
        //    未声明 → None → `skip_serializing_if` 不写键 → assembled_json 逐字节不变。
        //    钉住后由 `runtime::parse_realm_costume` 读回 briefing/flavorNotes 喂入场导演
        //    （§6「入场导演统一设定」）——仅影响描写，不进判定域。
        realm_tier: skeleton.realm_tier.clone(),
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
        "UPDATE worlds SET assembled_json = $1, updated_at = $2 WHERE id = $3 AND assembled_json IS NULL",
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

/// 读骨架 + 星级 + 准入策略（波次 3：star_rating 装配时从 world_templates 读出，供产出封顶并快照进
/// assembled_json；R2：admission_json 供容器混装的 cosmology 相容判定，技术附录 §5.1）。
/// 模板行缺失（测试/历史数据）→ (空骨架, 1★, 默认 open 策略)：退化装配且封顶按最保守档。
async fn load_skeleton(
    db: &AnyPool,
    template_id: &str,
) -> Result<(Skeleton, i64, crate::admission::WorldAdmissionPolicy), ApiError> {
    let row =
        sqlx::query("SELECT skeleton_json, star_rating, admission_json FROM world_templates WHERE id = $1")
            .bind(template_id)
            .fetch_optional(db)
            .await?;
    let Some(row) = row else {
        return Ok((Skeleton::default(), 1, Default::default()));
    };
    let raw: String = row.try_get("skeleton_json")?;
    let star_rating: i64 = row.try_get("star_rating")?;
    // 准入策略解析失败 → 默认 open（与 load_skeleton 对骨架的防御式 unwrap_or_default 同款）。
    let admission_raw: String = row.try_get("admission_json").unwrap_or_default();
    let admission = serde_json::from_str(&admission_raw).unwrap_or_default();
    Ok((serde_json::from_str(&raw).unwrap_or_default(), star_rating, admission))
}

async fn load_active_cards(db: &AnyPool, world_id: &str) -> Result<Vec<(String, CharacterCardV2)>, ApiError> {
    // 次级排序键 cloud_character_id 不可省：joined_at 是毫秒时间戳，两名成员并发 join 撞同一
    // 毫秒时行序就由 DB 决定，装配产物（perCharacterHooks / difficultyNotes 等按此序生成的数组）
    // 会在两次重放间漂移，破坏「同一实例可 replay」的确定性契约。
    // 该问题由黄金世界回归（runtime/golden.rs）首次运行时抓到。
    let rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, cc.card_json AS card \
         FROM world_members wm JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.world_id = $1 AND wm.status = 'active' \
         ORDER BY wm.joined_at ASC, wm.cloud_character_id ASC",
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

/// skeleton_json 的**全部合法顶层键**——本仓库唯一一处知道这个集合的地方。
///
/// 🔴 在此之前它谁也不知道：一半在 `struct Skeleton`（20 个字段，`Deserialize` + camelCase），
/// 另一半散在**手读点**——`runtime` 按字符串键读 `endgame`（min/maxWorldTicks，决定世界何时结束）
/// 与 `forbiddenPredicates`（禁止谓词，内容约束）。两边都不认识对方的键。
///
/// 于是拼错一个顶层键的后果是**全程零报错**：`Skeleton` 每个字段都 `#[serde(default)]`，
/// 未知字段被 serde 忽略（没有也不该有 `deny_unknown_fields`，那会让整个结构解析失败、
/// 反而触发下面那条「解析不出就放行」的防御分支，把校验整体关掉）；手读点则是
/// `.get(k).and_then(..)` 后 `unwrap_or_default`。所以：
///
/// | 拼错的键 | 静默后果 |
/// |---|---|
/// | `mainlineNodes` | 无主线大纲；`chapters::mainline_node_count` 归 0 → 通关判定退化 |
/// | `endgame` | 世界结束条件退回默认，运营设定的场次长度失效 |
/// | `forbiddenPredicates` | **禁止谓词失效**，失败方向是放行 |
/// | `payoutTable` / `identityPool` | 产出表 / 身份维度整个消失 |
///
/// 建模板期一次拦掉它，正是本文件既有的取舍（见下面那条 doc 的最后一段）。
/// 表里未列 = 不会被任何人读，因此**拒绝**而不是警告：一个没人读的键只可能是拼错或残留。
/// `__` 前缀的键（`__doc` 注释）豁免。
///
/// ⚠️ 加了新的手读键（`sk.get("xxx")`）而忘了往这张表里加，后果是**合法模板被拒**——响亮而不是静默，
/// 这个方向安全。反方向（往 `Skeleton` 加字段忘了加这里）由
/// `registered_key_sets_cover_every_struct_field` 在源码层钉住。
pub(crate) const SKELETON_TOP_LEVEL_KEYS: &[&str] = &[
    // ---- `struct Skeleton` 的字段（camelCase）----
    "sourceWork",
    "mainlineNodes",
    "endingPool",
    "hiddenContentPool",
    "sideHookPool",
    "worldItems",
    "worldCharacters",
    "locations",
    "assemblyRules",
    "storylines",
    "identityPool",
    "payoutTable",
    "realmTier",
    "sampling",
    "isSuperset",
    "subplotCardRefs",
    "seams",
    "nexus",
    "anchors",
    "container",
    // ---- 不在 `Skeleton` 里、由 runtime 按字符串键手读的 ----
    "endgame",             // runtime::…（结束条件：minWorldTicks / maxWorldTicks）
    "forbiddenPredicates", // runtime::…（禁止谓词）
];

/// `mainlineNodes[]` 元素的**全部合法键**——同 `SKELETON_TOP_LEVEL_KEYS`，是本仓库唯一知道这个集合的地方。
///
/// 🔴 这一层的知识裂得比顶层更开：`struct MainlineNode` 只认 `id` / `fated` / `variantGroup` / `arcTags`
/// 四个（装配层要的），而 `runtime` 从**同一批对象**上手读另外四个——`summary`（大纲文本）、
/// `constraint`、`threshold`（里程碑阈值）、`advanceWhen`（推进谓词）。两侧各测各的一半，都绿。
///
/// 拼错的失败方向全部朝坏的一边：
///
/// | 拼错的键 | 静默后果 |
/// |---|---|
/// | `constraint` | 落到 `_ => Soft` 分支 ⇒ **本该 `hard` 的硬约束静默降级** |
/// | `advanceWhen` | 推进谓词悄悄消失，节点退回纯阈值门 |
/// | `threshold` | 里程碑节点变回普通节点 |
/// | `summary` | 大纲节点文本为空——模型拿不到这个节点是干什么的 |
/// | `fated` | 宿命节点不再宿命；`variantGroup` | 互斥失效，同组变体可能同时出现 |
///
/// ⚠️ **只做了 `mainlineNodes[]` 这一层**。其余嵌套类型（结局池 / 内容池 / 地点 / 身份池……）
/// 同样是手读与类型化混用，尚未登记，见 `docs/VALIDATION.md` §3.8 的边界段。
/// 选它先做是因为：它是唯一被两个模块分头手读的嵌套类型，且 `constraint` 那条是 fail-open 的约束降级。
pub(crate) const MAINLINE_NODE_KEYS: &[&str] = &[
    // ---- `struct MainlineNode` 的字段 ----
    "id",
    "fated",
    "variantGroup",
    "arcTags",
    // ---- 不在 `MainlineNode` 里、由 runtime 手读的 ----
    "summary",     // 大纲节点文本
    "constraint",  // hard / soft / free
    "threshold",   // 里程碑阈值
    "advanceWhen", // 推进谓词
    // ---- 宿命时刻（2026-07-29）：把原著顺序搬上游戏时间轴 ----
    // 🔴 漏登记这两个键的后果**特别隐蔽**：模板作者写了 chapterOrder，
    // 建模板期会被「无人读取的键」400 掉（那还算好的，至少当场报错）；
    // 而若有人把它们从这张表里删掉，存量模板的宿命时刻会**静默失效**——
    // 主线退回「等人来推」，世界看起来照常跑，只是原著再也不会自己发生。
    "chapterOrder", // 原著里的先后 → due_at = chapterOrder × MUSE_FATED_TICK_SPACING
    "atLocation",   // 这件事发生在哪儿（碰撞的另一半坐标：在场者亲历、不在场者只听说）
];

/// `forbiddenPredicates[]` 元素的全部合法键。**没有任何类型化读者**——`Skeleton` 压根没有这个字段，
/// 只有 `runtime` 手读这三个。
///
/// 🔴 这一层的失败方向是三层里最坏的：`expression` 拼错 → `runtime` 那里
/// `let (Some(id), Some(expr)) = … else { continue }` **整条禁止谓词被丢弃**，
/// 世界照常开，只是那条内容约束从来没生效过，且没有任何日志。
pub(crate) const FORBIDDEN_PREDICATE_KEYS: &[&str] = &["id", "expression", "reason"];

/// **骨架每一层的合法键集，按路径索引** —— 全仓唯一一处知道 `skeleton_json` 的形状。
///
/// 路径语法：`""` = 顶层；`a.b` = 对象字段；`a[]` = 数组元素。
/// 例：`locations[].gate` = `locations` 数组里每个元素的 `gate` 对象。
///
/// 🔴 `validate_skeleton_refs`（建模板期拦）与 `admin_api::calibration::skeleton_shape`
/// （存量模板的发现面）**都遍历这一张表**。各抄一份的话，加一层就得记得改两处，
/// 而漏改的表现是「运营台说这个模板没问题」——那正是这套校验在修的缺陷本身。
///
/// **未登记的路径不校验**（不是「拒绝」）。这个方向是刻意的：这是一道**会拒请求**的闸，
/// 对着看不懂的结构乱拒是生产事故，而漏检一层只是维持现状、不构成回归。
/// 漏登记由 `every_skeleton_struct_is_registered` 在**源码层**变响——它从 `struct Skeleton`
/// 出发做可达性闭包，任何新结构体没进这张表即 CI 红。
///
/// 各条目末尾注明键的来源：`Skeleton` 侧的结构体名，或「手读」及其读者。
pub(crate) const SKELETON_KEY_SETS: &[(&str, &str, &[&str])] = &[
    ("", "Skeleton", SKELETON_TOP_LEVEL_KEYS),                          // Skeleton + runtime 手读
    ("sourceWork", "SkeletonSource", &["sourceId", "title"]),                 // SkeletonSource
    ("mainlineNodes[]", "MainlineNode", MAINLINE_NODE_KEYS),                // MainlineNode + runtime 手读
    ("forbiddenPredicates[]", "", FORBIDDEN_PREDICATE_KEYS),    // 无结构体，全靠 runtime 手读
    ("endgame", "", ENDGAME_KEYS),                              // 无结构体，全靠 runtime 手读
    ("endingPool[]", "EndingCandidate", &["id", "affinity", "baseWeight", "variantGroup", "arcTags"]), // EndingCandidate
    ("hiddenContentPool[]", "PoolItem", POOL_ITEM_KEYS),                // PoolItem
    ("hiddenContentPool[].rewardItem", "ItemDefinition", ITEM_DEFINITION_KEYS),
    ("hiddenContentPool[].rewardItem.origin", "ItemOrigin", ITEM_ORIGIN_KEYS),
    ("sideHookPool[]", "PoolItem", POOL_ITEM_KEYS),                     // PoolItem（与上面同型）
    ("sideHookPool[].rewardItem", "ItemDefinition", ITEM_DEFINITION_KEYS),
    ("sideHookPool[].rewardItem.origin", "ItemOrigin", ITEM_ORIGIN_KEYS),
    ("worldItems[]", "ItemDefinition", ITEM_DEFINITION_KEYS),                 // ItemDefinition（admission，跨模块）
    ("worldItems[].origin", "ItemOrigin", ITEM_ORIGIN_KEYS),              // ItemOrigin（admission，跨模块）
    ("worldCharacters[]", "WorldCharacter", &["card", "homeLocation", "carriedItemIds", "agendaNodes"]), // WorldCharacter
    ("locations[]", "LocationSpec", &["id", "name", "connections", "isSecretRealm", "gate", "residentItemIds"]), // LocationSpec
    // LocationGate 属 muse-engine（跨 crate 契约，§3.7 那一类）
    ("locations[].gate", "LocationGate", &["requiredItemIds", "requiredEffectTags", "requiredCosmologies", "maxPowerTier"]),
    ("assemblyRules", "AssemblyRules", &["hiddenPerCharacter", "endingWeightThreshold"]), // AssemblyRules
    // StorylineSpec + `assets::worlds::world_scan_text` 手读的 `summary`（结构体里没有这个字段）
    ("storylines[]", "StorylineSpec", &["id", "mainlineNodeIds", "hiddenPoolIds", "endingIds", "affinity", "summary"]),
    ("identityPool[]", "IdentitySpec", &["id", "label", "quota", "themes", "hookAffinity", "isLead"]), // IdentitySpec
    ("payoutTable", "PayoutTable", &["worldlineTiers", "contributionWeights", "collapse"]), // PayoutTable
    ("payoutTable.worldlineTiers[]", "PayoutTier", &["label", "minScore", "item", "mileage"]), // PayoutTier
    ("payoutTable.worldlineTiers[].item", "ItemDefinition", ITEM_DEFINITION_KEYS),
    ("payoutTable.worldlineTiers[].item.origin", "ItemOrigin", ITEM_ORIGIN_KEYS),
    // IntensityWeights 属 muse-engine（跨 crate）
    ("payoutTable.contributionWeights", "IntensityWeights", &["success", "partial", "failure", "speak"]),
    ("payoutTable.collapse", "CollapsePolicy", &["baselineFactor", "worldlineFactor"]), // CollapsePolicy
    ("realmTier", "RealmTier", REALM_TIER_KEYS),                         // RealmTier
    // ⚠️ `sampling` 有**两个读者**，各认各的：装配层 `SamplingSpec` 认前五个（每副本抽多少），
    // 创作者发布端 `assets::worlds::SamplingView` 认 `redundancyRatio`（超集冗余门，§防刷 ①）。
    // 两边都不知道并集——这正是本套校验反复撞见的形状，故并集只在这里写一次。
    ("sampling", "SamplingSpec", &[
        "instanceStorylineCount",
        "instanceMainlineCount",
        "instanceHiddenCount",
        "instanceNpcCount",
        "instanceLocationCount",
        "redundancyRatio",
    ]),
    ("subplotCardRefs[]", "SubplotCardRef", &["cardId", "cardVersion", "weight"]), // SubplotCardRef
    ("seams[]", "Seam", &["from", "to"]),                            // Seam
    ("nexus", "NexusSpec", &["name"]),                                    // NexusSpec
    ("container", "ContainerPlan", &["fingerprint", "weights", "pinnedLocations", "cardIds"]), // ContainerPlan
];

/// 不下钻的子树：schema 归别人所有，键集不在本仓库这一侧维护。
///
/// `worldCharacters[].card` 是 `muse_engine::character::types::CharacterCardV2`——
/// 拿骨架这一侧的键集去校验它，只会把合法的角色卡拒掉。它的送审同样另走
/// `safety::card_scan_text` 的语义口径。
pub(crate) const SKELETON_OPAQUE_PATHS: &[&str] = &["worldCharacters[].card"];

/// `endgame`（放置房终局策略）。**无结构体**，`runtime` 全靠字符串键手读。
pub(crate) const ENDGAME_KEYS: &[&str] =
    &["minWorldTicks", "maxWorldTicks", "worldTimeLimit", "keyCharacterIds"];

/// `PoolItem`（隐藏内容池 / 支线钩子池的元素）。
pub(crate) const POOL_ITEM_KEYS: &[&str] = &[
    "id",
    "themes",
    "template",
    "difficultyBase",
    "rewardItemRef",
    "rewardItem",
    "variantGroup",
    "arcTags",
];

/// `admission::ItemDefinition` / `ItemOrigin`：道具目录与内联奖励共用同一形状
/// （目录里那份与内联那份是同一类型，这也是它们的送审必须一视同仁的原因，见 VALIDATION §3.9）。
pub(crate) const ITEM_DEFINITION_KEYS: &[&str] = &["id", "narrative", "effectTags", "origin"];
pub(crate) const ITEM_ORIGIN_KEYS: &[&str] = &["worldTemplateId", "cosmology", "powerTier"];

/// `RealmTier`（境界档，总规格 §6「戏服」）。
pub(crate) const REALM_TIER_KEYS: &[&str] =
    &["id", "label", "cosmology", "genre", "conflictIntensity", "briefing", "flavorNotes"];

/// 一处「无人读取的键」。
pub(crate) struct UnknownKey {
    /// 人读路径，形如 `mainlineNodes[mn-1].constrait`（数组元素有 `id` 报 id，无则报 `#序号`）。
    pub(crate) display: String,
    /// 编辑距离最近的合法键；无近似项则 `None`（不硬凑建议）。
    pub(crate) hint: Option<&'static str>,
}

/// 逐层扫出骨架里**一切无人读取的键**（`SKELETON_KEY_SETS` 之外的）。
///
/// 两个消费方共用这一份实现：`validate_skeleton_refs` 取第一条拒请求，
/// `admin_api::calibration::skeleton_shape` 取全部给运营看。各写一遍就会漂。
///
/// 未登记路径不检查（理由见 `SKELETON_KEY_SETS`）；`SKELETON_OPAQUE_PATHS` 的子树不下钻；
/// `__` 前缀（注释键）豁免。
pub(crate) fn unknown_skeleton_keys(skeleton: &Value) -> Vec<UnknownKey> {
    let mut out = Vec::new();
    walk_unknown_keys(skeleton, "", String::new(), &mut out);
    out
}

fn walk_unknown_keys(v: &Value, path: &str, display: String, out: &mut Vec<UnknownKey>) {
    if SKELETON_OPAQUE_PATHS.contains(&path) {
        return;
    }
    match v {
        Value::Array(arr) => {
            let child_path = format!("{path}[]");
            for (i, item) in arr.iter().enumerate() {
                // 点名到具体那一条：有 `id` 报 id，无 `id` 报序号——否则运营不知道该改哪一行。
                let who = item
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("#{}", i + 1));
                walk_unknown_keys(item, &child_path, format!("{display}[{who}]"), out);
            }
        }
        Value::Object(map) => {
            let allowed = SKELETON_KEY_SETS.iter().find(|(p, _, _)| *p == path).map(|(_, _, k)| *k);
            for (k, child) in map {
                if k.starts_with("__") {
                    continue;
                }
                let child_display =
                    if display.is_empty() { k.clone() } else { format!("{display}.{k}") };
                if let Some(allowed) = allowed {
                    if !allowed.contains(&k.as_str()) {
                        out.push(UnknownKey {
                            display: child_display,
                            hint: nearest_key(k, allowed),
                        });
                        continue; // 键本身就不认识，再往下钻没有意义。
                    }
                }
                let child_path = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                walk_unknown_keys(child, &child_path, child_display, out);
            }
        }
        _ => {}
    }
}

/// 键名归一化：去掉大小写与非字母数字，于是 `mainLineNodes` / `mainline_nodes` / `MainlineNodes`
/// 都归到同一形——这三种正是最可能的写法错误（Rust 侧字段是 snake_case，线上格式是 camelCase）。
fn squash_key(k: &str) -> String {
    k.chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).collect()
}

/// 「是不是想写 X？」的候选：在归一化形上取编辑距离最近的一个，阈值 `max(1, len/4)`。
///
/// ⚠️ 只归一化是不够的——`constrait`（漏一个 `n`）归一化后仍不等于 `constraint`，
/// 而漏 / 多 / 错一个字母恰恰是最常见的拼法错误。没有这个提示，报错就只是「这个键没人读」，
/// 运营还得自己去猜正确写法是什么；提示才是这条错误信息真正有用的部分。
fn nearest_key<'a>(key: &str, allowed: &[&'a str]) -> Option<&'a str> {
    let a = squash_key(key);
    let budget = (a.len() / 4).max(1);
    allowed
        .iter()
        .map(|k| (edit_distance(&a, &squash_key(k)), *k))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, k)| (*d, k.len()))
        .map(|(_, k)| k)
}

/// Levenshtein 距离（滚动一行）。键名都是几十字符以内的短串，不值得引依赖。
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 建模板期引用完整性校验（Phase 3，`worlds_ops::create_template` 调用）：把 skeleton_json 试解析为
/// `Skeleton`，校验目录引用无悬空——`reward_item_ref`（无内联 fallback 时）/`connections`/`residentItemIds`/
/// `carried_item_ids`/`gate.requiredItemIds` 须能在对应目录（world_items / locations）解引用，
/// `gate.requiredCosmologies` 须 ∈ KNOWN_COSMOLOGIES。返回首个悬空引用的中文说明（Err）或通过（Ok）。
///
/// 另含三段**取值域**校验（不是引用完整性，但同属"建模板期一次拦掉，不留到运行时静默降级"）：
/// 身份池（§4 段）、公示产出表（§5 段）、境界档（§6 段：体系 / 题材 / 冲突烈度须在官方枚举内）。
///
/// 宽松边界：解析失败（类型不符）→ Ok（沿用 load_skeleton 的防御式 unwrap_or_default 语义，不因无关字段拦截合法模板）；
/// 只在结构成立时对「明确写了引用」的字段判悬空，避免误伤退化路径（空目录 / 无地点的老模板全部放行）。
pub(crate) fn validate_skeleton_refs(skeleton: &Value, container_on: bool) -> Result<(), String> {
    // 0) 未知键（逐层，见 `SKELETON_KEY_SETS`）。**必须在下面那句防御式解析之前**——
    //    它正是这一段要拦的东西：拼错的键不会让 `from_value::<Skeleton>` 失败，
    //    只会让对应字段静默取默认值。
    if let Some(u) = unknown_skeleton_keys(skeleton).into_iter().next() {
        // 与本函数其余各段一致，只报**第一个**冒犯者（键序由 serde_json 的有序 map 决定）。
        let hint = u.hint.map(|k| format!("（是不是想写 `{k}`？）")).unwrap_or_default();
        return Err(format!(
            "skeleton_json 的 `{}` 是无人读取的键{hint}——它不会报错，只会让对应功能静默退化到默认值",
            u.display
        ));
    }

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

    // 4) 身份池（§5【拍板 4、5】）：id 非空且唯一、quota ≥ 1、hookAffinity 指向存在的
    //    storyline / 隐藏池 / 支线钩子池 id。空 identityPool（老模板）直接放行。
    let storyline_ids: std::collections::BTreeSet<&str> =
        sk.storylines.iter().map(|s| s.id.as_str()).collect();
    let content_ids: std::collections::BTreeSet<&str> = sk
        .hidden_content_pool
        .iter()
        .chain(sk.side_hook_pool.iter())
        .map(|p| p.id.as_str())
        .collect();
    let mut seen_identity: std::collections::BTreeSet<&str> = Default::default();
    for spec in &sk.identity_pool {
        let id = spec.id.trim();
        if id.is_empty() {
            return Err(format!(
                "identityPool 缺少 id：身份 `{}` 没有稳定 id，无法被分配与审计",
                identity_display(spec)
            ));
        }
        if !seen_identity.insert(id) {
            return Err(format!("identityPool id 重复：`{id}`（身份 id 必须唯一，否则配额与分配无法对账）"));
        }
        if spec.quota == 0 {
            return Err(format!("identityPool quota 非法：身份 `{id}` 的 quota 必须 ≥ 1（0 等于这个站位不存在）"));
        }
        for hook in &spec.hook_affinity {
            let hook = hook.trim();
            if hook.is_empty() {
                continue;
            }
            if !storyline_ids.contains(hook) && !content_ids.contains(hook) {
                return Err(format!(
                    "hookAffinity 悬空：身份 `{id}` 引力指向不存在的 storyline / 内容池 id `{hook}`"
                ));
            }
        }
    }

    // 5) 公示产出表（§10【拍板 17】确定性产出）：门槛有限且非负、**门槛互不重复**（同分歧义即非确定性）、
    //    道具 id 非空、折算权重与崩塌系数取值合法。未声明 payoutTable（老模板）直接放行。
    if let Some(table) = &sk.payout_table {
        let w = &table.contribution_weights;
        for (name, v) in [
            ("success", w.success),
            ("partial", w.partial),
            ("failure", w.failure),
            ("speak", w.speak),
        ] {
            if !v.is_finite() || v < 0.0 {
                return Err(format!(
                    "payoutTable.contributionWeights.{name} 非法：权重须为非负有限数（当前 {v}）"
                ));
            }
        }
        for (name, v) in [
            ("baselineFactor", table.collapse.baseline_factor),
            ("worldlineFactor", table.collapse.worldline_factor),
        ] {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(format!(
                    "payoutTable.collapse.{name} 非法：崩塌系数须落在 [0,1]（当前 {v}）"
                ));
            }
        }
        let mut seen_thresholds: Vec<String> = Vec::new();
        for tier in &table.worldline_tiers {
            if !tier.min_score.is_finite() || tier.min_score < 0.0 {
                return Err(format!(
                    "payoutTable.worldlineTiers 门槛非法：档位 `{}` 的 minScore 须为非负有限数（当前 {}）",
                    tier.label, tier.min_score
                ));
            }
            // 定点化后比较（与结算侧同一 milli 口径），杜绝浮点表示差异导致的"看着不同实则同档"。
            let key = format!("{}", (tier.min_score * 1000.0).round() as i64);
            if seen_thresholds.contains(&key) {
                return Err(format!(
                    "payoutTable.worldlineTiers 门槛重复：minScore `{}` 出现多次（同一贡献分必须只对应一档，否则产出非确定）",
                    tier.min_score
                ));
            }
            seen_thresholds.push(key);
            if let Some(item) = &tier.item {
                if item.id.trim().is_empty() {
                    return Err(format!(
                        "payoutTable.worldlineTiers 道具 id 为空：档位 `{}` 无法发货与幂等去重",
                        tier.label
                    ));
                }
            }
        }
    }

    // 6) 境界档（§6【拍板 3】戏服原则）：id 非空、体系 / 题材 / 冲突烈度三项取值落在官方枚举内
    //    （**自由文本一律不收**，口径同 `gate.requiredCosmologies`）。三项都允许留空——
    //    空体系正是 §6 说的"无战力体系题材（都市/言情/历史），境界泛化为处境"。
    //    未声明 realmTier（老模板）直接放行，同 payoutTable 的「不填即零影响」。
    if let Some(rt) = &sk.realm_tier {
        if rt.id.trim().is_empty() {
            return Err("realmTier 缺少 id：境界档没有稳定 id，无法跨阶段对账与审计".to_string());
        }
        let cos = rt.cosmology.trim();
        if !cos.is_empty() && !crate::admission::KNOWN_COSMOLOGIES.contains(&cos) {
            return Err(format!(
                "realmTier.cosmology 非法：体系 `{cos}` 不在官方枚举内 {:?}（留空 = 无战力体系题材，是合法取值）",
                crate::admission::KNOWN_COSMOLOGIES
            ));
        }
        let genre = rt.genre.trim();
        if !genre.is_empty() && !KNOWN_GENRES.contains(&genre) {
            return Err(format!("realmTier.genre 非法：题材 `{genre}` 不在官方枚举内 {KNOWN_GENRES:?}"));
        }
        let ci = rt.conflict_intensity.trim();
        if !ci.is_empty() && !KNOWN_CONFLICT_INTENSITIES.contains(&ci) {
            return Err(format!(
                "realmTier.conflictIntensity 非法：冲突烈度 `{ci}` 不在官方枚举内 {KNOWN_CONFLICT_INTENSITIES:?}（文斗 civil / 武斗 martial / 生死 lethal）"
            ));
        }
    }

    // 7) 容器形态（R2 自定义房，技术附录 §3）：副本卡引用 / 缝合边 / 锚点的**建房期硬门**。
    //    未声明 subplotCardRefs（普通模板）直接放行——本段一个字节都不影响它们。
    validate_container_refs(&sk, container_on)?;

    Ok(())
}

/// 容器声明的建房期校验（`validate_skeleton_refs` 第 7 段，技术附录 §3.1/§3.3/§5）。
///
/// 这里只做**不依赖 DB 的静态门**（结构、格式、白名单、保留字）；需要卡内容才能判的
/// （卡内 id 含分隔符 / 卡内引用悬空 / cosmology 不相容 / 地点图不连通）由装配期的
/// `compose_container_skeleton` 拒绝，两道门合起来覆盖「建房期就拒绝，不留到运行时静默退化」。
fn validate_container_refs(sk: &Skeleton, container_on: bool) -> Result<(), String> {
    if sk.subplot_card_refs.is_empty() {
        // 普通模板：seams / nexus / anchors 单独声明而没有卡，是无意义配置，直接忽略（不拦老模板）。
        return Ok(());
    }
    // 🔴 前门拒绝（VALIDATION §0.1 未验证功能默认关闭）：开关未开时连声明都不许落库。
    // 读取侧另有降级（装配期忽略 refs 走原路径），两道合起来才是可逆急停阀。
    if !container_on {
        return Err(
            "自定义房装配（subplotCardRefs）尚未开放：本功能由运营开关 MUSE_CONTAINER_ASSEMBLY 控制，默认关闭"
                .to_string(),
        );
    }

    // 6a) 卡引用：id 非空 / 不含命名空间分隔符 / 不重复；weight 有限非负；cardVersion 非负。
    let mut seen_cards: std::collections::BTreeSet<&str> = Default::default();
    for r in &sk.subplot_card_refs {
        let cid = r.card_id.trim();
        if cid.is_empty() {
            return Err("subplotCardRefs 缺少 cardId：无法确定命名空间前缀".to_string());
        }
        if cid.contains(NS_SEP) {
            return Err(format!(
                "subplotCardRefs cardId 非法：`{cid}` 含保留分隔符 `{NS_SEP}`（它是命名空间前缀专用）"
            ));
        }
        if !seen_cards.insert(cid) {
            return Err(format!("subplotCardRefs 重复引用同一张卡：`{cid}`"));
        }
        if !r.weight.is_finite() || r.weight < 0.0 {
            return Err(format!("subplotCardRefs weight 非法：卡 `{cid}` 的权重须为非负有限数（当前 {}）", r.weight));
        }
        if let Some(v) = r.card_version {
            if v < 0 {
                return Err(format!("subplotCardRefs cardVersion 非法：卡 `{cid}` 的版本须 ≥ 0（当前 {v}）"));
            }
        }
    }

    // 6b) 容器本体 id 不得含命名空间分隔符（命名空间的前提：裸 id = 本体、带前缀 = 卡，两空间不相交）。
    for id in collect_id_like(sk) {
        if id.contains(NS_SEP) {
            return Err(format!(
                "容器本体 id `{id}` 含保留分隔符 `{NS_SEP}`：装卡的容器里 `{NS_SEP}` 是命名空间前缀专用（`卡id:原id`）"
            ));
        }
    }

    // 6c) 枢纽保留 id 不得被本体占用。
    if sk.locations.iter().any(|l| l.id.trim() == NEXUS_LOCATION_ID) {
        return Err(format!("地点 id `{NEXUS_LOCATION_ID}` 是容器枢纽保留字：请给本体地点换个 id"));
    }

    // 6d) 本体 anchors：须指向本体存在的地点，且**非秘境**（秘境不可作缝合口）。
    let loc_ids: std::collections::BTreeSet<&str> = sk.locations.iter().map(|l| l.id.as_str()).collect();
    // 本体锚点白名单（trim + 去空），6e 的裸端点判据要用。
    let own_anchors: std::collections::BTreeSet<&str> =
        sk.anchors.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).collect();
    for a in &sk.anchors {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if !loc_ids.contains(a) {
            return Err(format!("anchors 悬空：锚点 `{a}` 不是本容器的地点"));
        }
        if sk.locations.iter().any(|l| l.id == a && l.is_secret_realm) {
            return Err(format!("anchors 非法：秘境 `{a}` 不可作缝合口（gate 语义须完整保留在卡内）"));
        }
    }

    // 6e) 缝合边：两端非空且不相等；带前缀的端点其 cardId 必须在 refs 内（悬空引用）；
    //     裸 id 端点必须是本体地点、**且在本体 anchors 白名单内**。卡内那一端是否存在 /
    //     是否在卡的 anchors 白名单内，须待卡解引用后判，由 compose_container_skeleton 兜
    //     （同样是建房期，不是运行时）。
    //
    // 🔴 「且在本体 anchors 白名单内」这半条是 2026-07-29 补的，补的是一条**把错误推给别人**的缺陷：
    // `compose_container_skeleton` 步骤 4 要求缝合边的**每一个**端点都在白名单内
    // （见那里的「seams 越界」），而本段此前只查「是不是本体地点」。于是
    // 「anchors 留空 / 不含该地点 + seams 写本体地点」这种模板**能通过发布**，
    // 一直到有玩家去开房那一刻才 400 —— 撞上它的是玩家，不是写错的作者，
    // 而作者那边显示的是「发布成功」。
    //
    // 裸端点这一半在建模板期就能判死：合并后的白名单 = 本体 anchors ∪ 各卡 anchors（**带前缀**），
    // 而裸 id 不带前缀，因此它只可能由本体 anchors 放行——卡解不解引用都不改变这个结论。
    // 带前缀的那一半仍然只能留给 compose（要读卡蓝图），这条边界没有变。
    for seam in &sk.seams {
        let (from, to) = (seam.from.trim(), seam.to.trim());
        if from.is_empty() || to.is_empty() {
            return Err("seams 非法：缝合边两端都必须声明地点".to_string());
        }
        if from == to {
            return Err(format!("seams 非法：缝合边两端相同（`{from}`）"));
        }
        for end in [from, to] {
            match ns_owner(end) {
                Some(card) => {
                    if !seen_cards.contains(card) {
                        return Err(format!(
                            "seams 悬空：缝合口 `{end}` 指向未被 subplotCardRefs 引用的卡 `{card}`"
                        ));
                    }
                }
                None => {
                    if !loc_ids.contains(end) {
                        return Err(format!("seams 悬空：缝合口 `{end}` 不是本容器本体的地点"));
                    }
                    if !own_anchors.contains(end) {
                        return Err(format!(
                            "seams 越界：缝合口 `{end}` 不在本容器的 anchors 白名单内（缝合边只能落在锚点上）"
                        ));
                    }
                }
            }
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
        // 🔴 送审文本取不出来（卡序列化失败）→ 按**未通过**处理，不是按空文本送审：
        // 空串会被 provider 判过，等于让一张读不出来的卡直接进实例。
        let Some(scan_text) = npc_scan_text(&wc.card) else {
            tracing::warn!(world = %world_id, npc = %npc_id, "NPC 卡无法序列化为送审文本，按未通过处理");
            continue;
        };
        let verdict = crate::safety::moderate_and_queue(
            state,
            "assembly_npc",
            &format!("{world_id}:{npc_id}"),
            &scan_text,
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

/// NPC 卡的机审文本。**复用 `safety::card_scan_text`**（递归收全部字符串叶子）。
///
/// 🔴 它此前是**包含表**——只拼四个字段（名字 / 核心矛盾 / 表层目标 / 长期议程），
/// 而 `CharacterCardV2` 有约 50 个创作者可写的叙事文本字段（`plotSeeds` / `bottomLines` /
/// `stakes` / `hiddenNeed` / `outburstPattern` / `forbiddenPhrases` …），**整张卡都会进模型**
/// （`WorldCharacterEntry.card` → runtime 逐 tick 注入）。于是这道「未复核内容不进实例」的门
/// 漏看了其中约 46 个字段，且注释里只描述了选了哪四个、没给过理由。
///
/// 同一张卡在**模板发布**时走的是 `assets::worlds::world_scan_text` → `card_scan_text`（全量），
/// 也就是说同一判定本来就有两份实现，而窄的那份恰好在安全路径上。现收归一处。
/// 取舍与 VALIDATION §3.9 逐字相同：漏扫 = 内容绕过机审，多扫 = 送审文本长一点，两者不对称。
///
/// 🔴 序列化失败返回 `None` 而不是空串：空串会被 provider 判过 ⇒ **fail-open**。
/// 调用方按「未通过」处理（该 NPC 不钉入实例），与其它非 Approved 分支同一出口。
fn npc_scan_text(card: &CharacterCardV2) -> Option<String> {
    serde_json::to_value(card).ok().map(|v| crate::safety::card_scan_text(&v))
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
/// 按「执念/恐惧命中数 + 气运加成」给这个角色排候选钩子。
///
/// 🔴 **`fortune_level == 0` 时与只按命中数排序逐字节相同**：加成恒为 0.0，
/// 排序键退化为 `m as f32`，而按 `m as f32` 降序稳定排序与按 `m` 降序稳定排序
/// 产出**完全相同的序列**（f32 能精确表示这个量级的整数，平手序都由稳定排序保留）。
/// 无烙印的卡因此拿到与本层落地前一模一样的钩子。
///
/// 🔴 气运**加在排序上，不加在配额上**：它改「先轮到哪些」，不改「拿几个」——
/// 后者是机缘的事（`personal_hook_quota`）。两个数各管一头，不重叠。
fn rank_pool_items<'a>(
    pool: &'a [PoolItem],
    terms: &[String],
    fortune_level: i64,
) -> Vec<(&'a PoolItem, usize, Option<String>)> {
    let (mean, max_dev) = difficulty_center(pool);
    let mut scored: Vec<(&PoolItem, usize, Option<String>, f32)> = pool
        .iter()
        .map(|p| {
            let (m, term) = score_pool_item(p, terms);
            let score = m as f32 + fortune_bonus(p.difficulty_base, mean, max_dev, fortune_level);
            (p, m, term, score)
        })
        .collect();
    // 排序分降序；稳定排序保留池内原顺序作为平手序。
    scored.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(p, m, term, _)| (p, m, term)).collect()
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

/// 结局池按阵容加权（打分版）：weight = base_weight * (1 + 该倾向占比)；≥ 阈值则启用，
/// 保底至少启用权重最高者。输出 `(id, weight)` 并保持**模板池声明序**——
/// 权重跟着名单一起交出去，供下游 `pick_ending` 掷点定盘（筛与选的权重口径同一份，不可能漂移）。
fn weight_endings_scored(
    pool: &[EndingCandidate],
    profile: &(u32, u32, u32),
    threshold: f32,
) -> Vec<(String, f32)> {
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
    let enabled: Vec<(String, f32)> =
        weighted.iter().filter(|(_, w)| *w >= threshold).map(|(e, w)| (e.id.clone(), *w)).collect();
    if !enabled.is_empty() {
        return enabled;
    }
    // 保底：无一过阈值时启用权重最高的单个结局（副本必须可结束）。
    weighted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    weighted.first().map(|(e, w)| vec![(e.id.clone(), *w)]).unwrap_or_default()
}

/// 权重整数化（**精确档**，与 `scale_weight` 的差别是**不加保底 +1**）：
/// 用于「谁能被选中」的语义必须严格守住的场合——权重为 0 ⇒ 缩放后为 0 ⇒ 概率恒为 0。
/// 非正数 / NaN 一律归 0（一次比较即完成，掷点全程纯整数，无浮点 RNG）。
fn scale_weight_exact(w: f32) -> u64 {
    if !(w > 0.0) {
        return 0;
    }
    ((w as f64) * 1_000_000.0) as u64
}

/// 结局定盘（总规格 §5「一个模板，千个平行世界」）：在**已启用**的结局中按权重掷点选定一个。
///
/// 确定性契约：
/// - 入参 `sorted` 必须是**按 id 升序排好的切片**（debug_assert 守着）——掷点绝不吃迭代序；
/// - 全程整数权重（`scale_weight_exact`）+ `Rng::next_u64() % total`，无系统随机、无浮点 RNG；
/// - 同 seed 同输入恒等同输出（黄金世界重放的前提）。
///
/// 权重语义（不可被悄悄改掉，有专项用例锁）：
/// - **未启用**的结局根本不在 `sorted` 里 ⇒ 永不被选中；
/// - **权重为 0**（或负 / NaN）的结局缩放后为 0 ⇒ 累加区间长度为 0 ⇒ 永不被选中；
/// - 全部候选权重都为 0 的退化局面：不掷点，取排序后首个——「副本必须可结束」优先于零权语义，
///   否则世界将无结局可落。这是唯一的例外，也只在模板把整池权重配成 0 时才可能发生。
fn pick_ending(rng: &mut Rng, sorted: &[(String, f32)]) -> Option<String> {
    if sorted.is_empty() {
        return None;
    }
    debug_assert!(
        sorted.windows(2).all(|w| w[0].0 <= w[1].0),
        "pick_ending 入参必须按 id 升序：掷点不得依赖迭代序/声明序"
    );
    let scaled: Vec<u64> = sorted.iter().map(|(_, w)| scale_weight_exact(*w)).collect();
    let total: u64 = scaled.iter().copied().sum();
    if total == 0 {
        return sorted.first().map(|(id, _)| id.clone()); // 全零退化：副本必须可结束。
    }
    let mut r = rng.next_u64() % total;
    for (i, s) in scaled.iter().enumerate() {
        if *s == 0 {
            continue; // 零权项区间长度为 0，跳过（不消耗 r）。
        }
        if r < *s {
            return Some(sorted[i].0.clone());
        }
        r -= *s;
    }
    // 理论不可达（区间和 = total）；防御性回退到最后一个**正权**项，绝不回退到零权项。
    sorted.iter().rev().find(|(_, w)| scale_weight_exact(*w) > 0).map(|(id, _)| id.clone())
}

/// 掷点入参规范化：拷贝一份**按 id 升序**的候选切片（`enabled_endings` 输出仍保持模板池序）。
/// id 唯一性由模板引用完整性校验前置保证；万一重复，`sort_by` 稳定排序保留声明序 → 仍确定性。
fn sorted_for_pick(scored: &[(String, f32)]) -> Vec<(String, f32)> {
    let mut v = scored.to_vec();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
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
    use std::collections::{BTreeMap, BTreeSet};

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
        plan_sampling(sk, fp, world_id, 1, &PROFILE, &[], &[], 0.5, star, "")
    }

    /// 带阵容的规划（身份池分配需要卡本体做内核匹配）。
    fn plan_with_roster(sk: &Skeleton, world_id: &str, cards: &[(String, CharacterCardV2)]) -> Selection {
        let fp = roster_fingerprint(cards);
        plan_sampling(sk, &fp, world_id, 1, &PROFILE, &[], cards, 0.5, 5, "")
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

    // ================================================================
    // 结局定盘（任务 #41）：掷点消费 instance_seed + 权重语义不可被悄悄改掉
    // ================================================================

    /// 结局专用骨架（**非超集** → 走退化路径，即绝大多数存量模板所在的那条路）。
    /// 三个候选，权重拉开（3.0 / 1.0 / 0.5）且都 ≥ 默认阈值 0.5 ⇒ 三个都启用，有得选。
    /// 无 affinity ⇒ 阵容 boost = 0 ⇒ 权重就是 baseWeight，断言不必跟着阵容画像走。
    fn ending_pool_skeleton(pool: serde_json::Value) -> Skeleton {
        serde_json::from_value(serde_json::json!({
            "mainlineNodes": [ { "id": "n1" } ],
            "endingPool": pool,
        }))
        .unwrap()
    }

    fn three_weight_skeleton() -> Skeleton {
        ending_pool_skeleton(serde_json::json!([
            { "id": "e-hi",  "baseWeight": 3.0 },
            { "id": "e-mid", "baseWeight": 1.0 },
            { "id": "e-lo",  "baseWeight": 0.5 }
        ]))
    }

    /// 按给定阈值规划（`plan` 把阈值写死成 0.5，零权/未启用语义需要自定阈值）。
    fn plan_with_threshold(sk: &Skeleton, world_id: &str, threshold: f32) -> Selection {
        plan_sampling(sk, "cidA\ncidB", world_id, 1, &PROFILE, &[], &[], threshold, 5, "")
    }

    /// 同实例（同 world_id / 同阵容 / 同模板版本）反复规划 → 定盘结局逐字相同。
    /// 这是黄金世界重放能成立的前提：掷点只吃 instance_seed，不吃调用次数/时间/迭代序。
    #[test]
    fn ending_pick_is_deterministic_for_same_instance() {
        let sk = three_weight_skeleton();
        let a = plan(&sk, "world_end_fixed", "cidA\ncidB");
        let b = plan(&sk, "world_end_fixed", "cidA\ncidB");
        assert!(a.selected_ending.is_some(), "非空结局池必须定盘出一个结局");
        assert_eq!(a.selected_ending, b.selected_ending, "同实例两次规划必须落同一个结局");
        assert_eq!(a.enabled_endings, b.enabled_endings);

        // `pick_ending` 本身也是纯函数：同 Rng 状态 + 同输入 → 同输出。
        let scored = vec![("a".to_string(), 1.0f32), ("b".to_string(), 2.0f32)];
        assert_eq!(pick_ending(&mut Rng(0xdead_beef), &scored), pick_ending(&mut Rng(0xdead_beef), &scored));
        assert_eq!(pick_ending(&mut Rng(1), &[]), None, "空池 → None（世界仍可停机，只是无结局产出）");
    }

    /// 🔴 缺陷本体的回归锁：**只有 world_id 不同**的一批实例必须落到不止一个结局。
    /// 旧行为（取 enabledEndings 首个）在这里会得到 distinct == 1。
    #[test]
    fn ending_pick_varies_across_instances() {
        let sk = three_weight_skeleton();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for i in 0..32 {
            let s = plan(&sk, &format!("world_end_{i}"), "cidA\ncidB");
            let picked = s.selected_ending.expect("必须定盘");
            assert!(
                s.enabled_endings.contains(&picked),
                "掷点选出了未启用的结局 {picked}（enabled={:?}）",
                s.enabled_endings
            );
            seen.insert(picked);
        }
        assert!(seen.len() >= 2, "同模板同阵容的一批实例只落到一个结局 —— 掷点没吃到 instance_seed：{seen:?}");
    }

    /// 权重语义（一）：**未启用**（权重低于阈值被筛掉）的结局永不被选中。
    #[test]
    fn disabled_ending_is_never_selected() {
        let sk = ending_pool_skeleton(serde_json::json!([
            { "id": "e-weak", "baseWeight": 0.1 },
            { "id": "e-ok",   "baseWeight": 1.0 }
        ]));
        for i in 0..64 {
            let s = plan(&sk, &format!("world_disabled_{i}"), "cidA\ncidB");
            assert_eq!(s.enabled_endings, vec!["e-ok".to_string()], "低于阈值者不该启用");
            assert_eq!(s.selected_ending.as_deref(), Some("e-ok"), "未启用的 e-weak 绝不可被选中");
        }
    }

    /// 权重语义（二）：权重为 0 的结局**即使被启用**（阈值 0 时 0.0 >= 0.0 仍进名单）也永不被选中。
    /// 这条锁的是「筛的口径」和「选的口径」各管各的：进名单 ≠ 能上场。
    #[test]
    fn zero_weight_ending_is_never_selected() {
        let sk = ending_pool_skeleton(serde_json::json!([
            { "id": "e-zero", "baseWeight": 0.0 },
            { "id": "e-pos",  "baseWeight": 1.0 }
        ]));
        for i in 0..64 {
            let s = plan_with_threshold(&sk, &format!("world_zero_{i}"), 0.0);
            assert!(s.enabled_endings.contains(&"e-zero".to_string()), "阈值 0 时零权结局仍在启用名单里");
            assert_eq!(s.selected_ending.as_deref(), Some("e-pos"), "零权结局绝不可被选中");
        }
        // 直接对 pick_ending 施压：负权 / NaN 与 0 同档，一律出局。
        let scored = vec![
            ("e-nan".to_string(), f32::NAN),
            ("e-neg".to_string(), -5.0f32),
            ("e-pos".to_string(), 1.0f32),
        ];
        for seed in 0..256u64 {
            assert_eq!(pick_ending(&mut Rng(seed), &scored).as_deref(), Some("e-pos"));
        }
        // 全零退化：不掷点，取排序后首个 —— 「副本必须可结束」优先，且仍是确定性的。
        let all_zero = vec![("z-a".to_string(), 0.0f32), ("z-b".to_string(), 0.0f32)];
        assert_eq!(pick_ending(&mut Rng(7), &all_zero).as_deref(), Some("z-a"));
    }

    /// 权重语义（三）：掷点**按权重**，不是等概率——权重 3.0 的结局必须显著多于权重 0.5 的。
    /// 固定种子集合 ⇒ 这组计数是确定值，不存在 flaky。
    #[test]
    fn ending_pick_follows_weights() {
        let sk = three_weight_skeleton();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for i in 0..300 {
            let s = plan(&sk, &format!("world_w_{i}"), "cidA\ncidB");
            *counts.entry(s.selected_ending.expect("必须定盘")).or_insert(0) += 1;
        }
        let hi = *counts.get("e-hi").unwrap_or(&0);
        let mid = *counts.get("e-mid").unwrap_or(&0);
        let lo = *counts.get("e-lo").unwrap_or(&0);
        assert!(hi > mid && mid > lo, "命中次数应随权重单调（3.0 / 1.0 / 0.5）：{counts:?}");
        assert!(lo > 0, "权重最低但已启用的结局仍须有机会上场：{counts:?}");
    }

    /// 超集路径同样定盘，且定盘结果落在该实例**已启用**的名单内（变体组互斥后的名单）。
    #[test]
    fn superset_path_pins_selected_ending_within_enabled() {
        let sk = superset();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for i in 0..32 {
            let s = plan(&sk, &format!("world_ss_end_{i}"), "cidA\ncidB");
            let picked = s.selected_ending.expect("超集路径同样必须定盘");
            assert!(
                s.enabled_endings.contains(&picked),
                "掷点越过了 storyline 约束 / 变体组互斥：{picked} ∉ {:?}",
                s.enabled_endings
            );
            seen.insert(picked);
        }
        assert!(seen.len() >= 2, "超集路径的结局也必须随实例种子分叉：{seen:?}");
    }

    /// 结局池为空 → 定盘为 None（世界仍可停机，只是无结局产出与荣誉奖励）。
    #[test]
    fn empty_ending_pool_pins_nothing() {
        let sk = ending_pool_skeleton(serde_json::json!([]));
        let s = plan(&sk, "world_no_ending", "cidA\ncidB");
        assert!(s.enabled_endings.is_empty());
        assert_eq!(s.selected_ending, None);
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

    /// 难度铺开的 `reward_superset`——气运只在难度**有差异**的世界里有着力点。
    ///
    /// 共享夹具 `reward_superset()` 的池项难度全取默认值（均匀），
    /// 那种世界里气运恒等（见 `a_world_of_uniform_difficulty_gives_fortune_nothing_to_grip`），
    /// 拿它测气运只会测到一个恒等函数。这里按真实数据的量级（0.2–0.5）铺开。
    fn reward_superset_with_spread_difficulty() -> Skeleton {
        let mut sk = reward_superset();
        for (i, p) in sk.hidden_content_pool.iter_mut().enumerate() {
            p.difficulty_base = 0.2 + 0.075 * i as f32; // 0.2 / 0.275 / 0.35 / 0.425 / 0.5
        }
        sk
    }

    /// 无奖励、难度铺开的隐藏池（6 条，0.2–0.5，均值 0.35）。
    fn plain_superset_with_spread_difficulty() -> Skeleton {
        let pool: Vec<serde_json::Value> = (0..6)
            .map(|i| serde_json::json!({
                "id": format!("hp-{i}"), "arcTags": ["arc-P"], "difficultyBase": 0.2 + 0.06 * i as f32
            }))
            .collect();
        let ids: Vec<String> = (0..6).map(|i| format!("hp-{i}")).collect();
        serde_json::from_value(serde_json::json!({
            "isSuperset": true,
            "storylines": [
                { "id": "arc-P", "mainlineNodeIds": ["mn-1"], "hiddenPoolIds": ids, "endingIds": ["end-1"] }
            ],
            "mainlineNodes": [ { "id": "mn-1", "fated": true, "arcTags": ["arc-P"] } ],
            "hiddenContentPool": pool,
            "endingPool": [ { "id": "end-1", "arcTags": ["arc-P"] } ],
            "sampling": { "instanceStorylineCount": 1, "instanceHiddenCount": 3 }
        }))
        .unwrap()
    }

    /// 🔴 **气运与机缘对世界没有影响**（产品 2026-07-29 二次澄清）。
    ///
    /// 这一条守的是「撤得干净」：世界这一层的采样**只认烙印指纹**（那是种子的第五段），
    /// 除此之外不该有任何气运机缘的痕迹。源码级——它不测某一次采样的结果，
    /// 测的是**这个函数体里根本没有那两个概念**。
    ///
    /// ⚠️ 为什么不用行为断言：「换气运档 → 产出不变」需要一个能传气运档的入口，
    /// 而正确的实现里**根本没有那个入口**——测不了一个不存在的参数。
    /// 这类「某个东西必须不存在」的红线，源码级是唯一测得了的形态。
    #[test]
    fn world_level_sampling_knows_nothing_about_fortune_or_opportunity() {
        let src = include_str!("mod.rs");
        let start = src.find("fn plan_sampling").expect("采样规划函数应存在");
        let end = src[start..].find("\n/// 读取骨架").map(|i| start + i).unwrap_or(src.len());
        let body = &src[start..end];
        // ⚠️ 只扫**代码标识符**，不扫「气运/机缘」这两个中文词：
        // 函数签名里那条注释正是在解释「它们不在这儿」，扫中文会把解释本身判成违规。
        for forbidden in ["fortune", "opportunity", "swing_", "SWING_"] {
            assert!(
                !body.contains(forbidden),
                "🔴 世界级采样里出现了 `{forbidden}`——这两个数只作用于角色，不作用于世界"
            );
        }
    }

    /// 🔴 **空隐藏池不得让装配 panic。**
    ///
    /// 只有主线、没有间隙内容的世界是合法的。世界级机缘那一版的 clamp 把下限写死成 1，
    /// 空池时成了 `clamp(1, 0)` —— Rust 的 clamp 在 min > max 时 panic，装配整条挂掉返 500。
    /// 机缘撤走之后这条依然要留着：它守的是「空池」这个合法输入，与气运本身无关。
    #[test]
    fn an_empty_hidden_pool_must_not_blow_up_assembly() {
        let mut sk = plain_superset_with_spread_difficulty();
        sk.hidden_content_pool.clear();
        for sl in sk.storylines.iter_mut() {
            sl.hidden_pool_ids.clear();
        }
        for i in 0..8 {
            let fp = format!("cid{i}:choice:pushed_a_milestone");
            let sel = plan_sampling(&sk, "cidA", "world_empty", 1, &PROFILE, &[], &[], 0.5, 5, &fp);
            assert!(sel.audit.unwrap().selected_hidden.is_empty(), "空池只能选出空集");
        }
    }

    /// 🔴 **平权红线：不管谁带着什么经历进来，产出封顶都不动一寸。**
    ///
    /// 20 组不同的烙印指纹（⇒ 20 个不同的实例种子）跑同一个 5★ 模板，
    /// 每一组的稀有奖励数都必须 ≤ `RARE_BUDGET`。
    #[test]
    fn no_roster_history_can_lift_the_output_cap() {
        let mut sk = reward_superset_with_spread_difficulty();
        sk.sampling.instance_hidden_count = Some(3);
        for i in 0..20 {
            let fp = format!("cid{i}:choice:pushed_a_milestone\ncid{i}:unfinished:x");
            let sel = plan_sampling(&sk, "cidA\ncidB", "world_equity", 1, &PROFILE, &[], &[], 0.5, 5, &fp);
            let audit = sel.audit.unwrap();
            let rare = audit.selected_hidden.iter().filter(|id| id.as_str() != "hr-1").count();
            assert!(rare <= RARE_BUDGET, "🔴 产出封顶被顶穿（第 {i} 组）：{:?}", audit.selected_hidden);
        }
    }

    // ---------- R1 身份池进采样域（总规格 §5【拍板 4、5】）：内核匹配 + 种子随机 ----------

    /// 测试用角色卡（只填内核匹配读到的字段：core_fear + plot_seeds）。
    fn card(id: &str, fear: &str, seeds: &[&str]) -> CharacterCardV2 {
        use muse_engine::character::types::{Agency, CardLifecycle, DramaticCore, Identity};
        CharacterCardV2 {
            schema_version: 2,
            id: id.into(),
            lifecycle: CardLifecycle::Ready,
            identity: Identity { name: id.into(), ..Default::default() },
            dramatic_core: DramaticCore { core_fear: fear.into(), ..Default::default() },
            decision_model: Default::default(),
            perception: Default::default(),
            emotion_dynamics: Default::default(),
            relation_grammar: Default::default(),
            expression_fingerprint: Default::default(),
            agency: Agency {
                plot_seeds: seeds.iter().map(|s| (*s).to_string()).collect(),
                ..Default::default()
            },
            growth_arc: Default::default(),
            world_adaptation: Default::default(),
            evidence_index: Default::default(),
            revision: 1,
            created_at: 0,
            updated_at: 0,
        }
    }

    // ---------- NPC 卡送审文本的覆盖面 ----------
    //
    // 🔴 `npc_scan_text` 此前是**包含表**（只拼四个字段），而整张卡都会进模型。
    // 这一组钉的是「送审文本必须覆盖创作者可写的全部叙事字段」。

    /// 逐字段探针：卡上各处的文字都必须出现在送审文本里。
    /// 改动前只有 `identity.name` / `coreContradiction` / `surfaceGoal` / `longTermAgenda` 四处在。
    #[test]
    fn npc_scan_text_covers_the_whole_card_not_four_fields() {
        use muse_engine::character::types::{Agency, DramaticCore, ExpressionFingerprint};
        let mut c = card("npc1", "恐惧探针", &["剧情种子探针"]);
        c.dramatic_core = DramaticCore {
            core_contradiction: "核心矛盾探针".into(),
            surface_goal: "表层目标探针".into(),
            hidden_need: "隐藏需求探针".into(),
            core_fear: "恐惧探针".into(),
            stakes: "赌注探针".into(),
            bottom_lines: vec!["底线探针".into()],
            ..Default::default()
        };
        c.agency = Agency {
            long_term_agenda: "长期议程探针".into(),
            plot_seeds: vec!["剧情种子探针".into()],
            refusal_rules: vec!["拒绝规则探针".into()],
            ..Default::default()
        };
        c.expression_fingerprint = ExpressionFingerprint {
            forbidden_phrases: vec!["禁用语探针".into()],
            ..Default::default()
        };

        let text = npc_scan_text(&c).expect("卡必须序列化得出送审文本");
        for probe in [
            "npc1",             // identity.name（旧口径已覆盖）
            "核心矛盾探针",     // 旧口径已覆盖
            "表层目标探针",     // 旧口径已覆盖
            "长期议程探针",     // 旧口径已覆盖
            "隐藏需求探针",     // ↓ 以下全是旧口径**漏掉**的
            "恐惧探针",
            "赌注探针",
            "底线探针",
            "剧情种子探针",
            "拒绝规则探针",
            "禁用语探针",
        ] {
            assert!(text.contains(probe), "NPC 送审文本漏了 `{probe}`——该字段的内容不过任何机审\n实际: {text}");
        }
    }

    /// 🔴 与云端角色卡、模板发布走的是**同一个实现**（`safety::card_scan_text`）。
    /// 谁把它改回自己拼几个字段，这一条立刻红——同一判定的第二份实现正是缺陷本身。
    #[test]
    fn npc_scan_text_is_the_same_implementation_as_card_scan_text() {
        let c = card("npc2", "怕黑", &["种子甲", "种子乙"]);
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            npc_scan_text(&c).unwrap(),
            crate::safety::card_scan_text(&v),
            "🔴 NPC 送审文本必须逐字复用 card_scan_text，不得另拼一套"
        );
    }

    /// 素阵容（无内核匹配词条）：只验分配协议本身。
    fn roster(ids: &[&str]) -> Vec<(String, CharacterCardV2)> {
        ids.iter().map(|id| ((*id).to_string(), card(id, "", &[]))).collect()
    }

    fn with_identities(mut sk: Skeleton, pool: serde_json::Value) -> Skeleton {
        sk.identity_pool = serde_json::from_value(pool).unwrap();
        sk
    }

    /// 标准身份池：官员×3 / 商贾×3 / 江湖客×3 / 被退婚主位×1（Σquota=10）。
    fn standard_pool() -> serde_json::Value {
        serde_json::json!([
            { "id": "official", "label": "户部主事",     "quota": 3, "themes": ["朝堂"], "hookAffinity": ["arc-A"] },
            { "id": "merchant", "label": "漕帮商贾",     "quota": 3, "themes": ["行商"] },
            { "id": "wanderer", "label": "江湖客",       "quota": 3, "themes": ["刀口"] },
            { "id": "jilted",   "label": "被退婚的嫡女", "quota": 1, "themes": ["退婚"], "isLead": true, "hookAffinity": ["hc-b1"] }
        ])
    }

    fn counts(assignments: &[(String, String)], identity: &str) -> usize {
        assignments.iter().filter(|(_, i)| i == identity).count()
    }

    // 确定性①：同 (world_id, 阵容, template_version) → 身份分配逐项一致。
    #[test]
    fn identity_assignment_is_deterministic() {
        let sk = with_identities(superset(), standard_pool());
        let cards = roster(&["cid-a", "cid-b", "cid-c", "cid-d"]);
        let a = plan_with_roster(&sk, "world_id_det", &cards);
        let b = plan_with_roster(&sk, "world_id_det", &cards);
        assert!(!a.identity_assignments.is_empty(), "声明了 identityPool 就必须分配出身份");
        assert_eq!(a.identity_assignments, b.identity_assignments, "同种子同阵容 → 身份分配必须完全一致");
        assert_eq!(
            a.audit.unwrap().identity_assignments,
            b.audit.unwrap().identity_assignments,
            "审计段身份分配同样可 replay"
        );
    }

    // 确定性②：入场先后（joined_at 顺序）不得影响分配 —— 与 roster_fingerprint 同一哲学。
    #[test]
    fn identity_assignment_ignores_join_order() {
        let sk = with_identities(superset(), standard_pool());
        let cards = roster(&["cid-a", "cid-b", "cid-c", "cid-d"]);
        let mut reversed = cards.clone();
        reversed.reverse();
        assert_eq!(
            plan_with_roster(&sk, "world_id_order", &cards).identity_assignments,
            plan_with_roster(&sk, "world_id_order", &reversed).identity_assignments,
            "同一阵容换入场顺序 → 身份分配必须不变"
        );
    }

    // 实例差异（防刷第二重）：不同 world_id、同阵容同模板 → 身份分布不同（攻略对不上盘）。
    #[test]
    fn identity_assignment_differs_across_instances() {
        let sk = with_identities(superset(), standard_pool());
        let cards = roster(&["cid-a", "cid-b", "cid-c", "cid-d"]);
        let sigs: BTreeSet<String> = (0..16)
            .map(|i| {
                let s = plan_with_roster(&sk, &format!("world_ident_{i}"), &cards);
                s.identity_assignments.iter().map(|(c, id)| format!("{c}={id}")).collect::<Vec<_>>().join(",")
            })
            .collect();
        assert!(sigs.len() >= 2, "不同实例应分出不同身份分布，实得 {} 种", sigs.len());
    }

    // 配额是硬上限：任意实例下每个身份的分配数 ≤ 其 quota（不超发）。
    #[test]
    fn identity_quota_never_oversubscribed() {
        let sk = with_identities(superset(), standard_pool());
        let cards = roster(&["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"]);
        for i in 0..16 {
            let a = plan_with_roster(&sk, &format!("world_quota_{i}"), &cards).identity_assignments;
            assert!(counts(&a, "official") <= 3, "official 超发: {a:?}");
            assert!(counts(&a, "merchant") <= 3, "merchant 超发: {a:?}");
            assert!(counts(&a, "wanderer") <= 3, "wanderer 超发: {a:?}");
            assert!(counts(&a, "jilted") <= 1, "主位 jilted 超发: {a:?}");
            let cids: BTreeSet<&str> = a.iter().map(|(c, _)| c.as_str()).collect();
            assert_eq!(cids.len(), a.len(), "同一角色不得被分配两个身份: {a:?}");
        }
    }

    // 边界①：人多于 Σquota → 配额用尽即止，多出的角色无身份（不超发、不报错）。
    #[test]
    fn identity_more_players_than_quota_leaves_remainder_unassigned() {
        let sk = with_identities(
            superset(),
            serde_json::json!([
                { "id": "official", "quota": 1 },
                { "id": "merchant", "quota": 1 }
            ]),
        );
        let cards = roster(&["c1", "c2", "c3", "c4", "c5"]);
        for i in 0..8 {
            let a = plan_with_roster(&sk, &format!("world_over_{i}"), &cards).identity_assignments;
            assert_eq!(a.len(), 2, "Σquota=2 → 至多 2 人有身份（其余走无站位路径）: {a:?}");
            assert_eq!(counts(&a, "official"), 1);
            assert_eq!(counts(&a, "merchant"), 1);
        }
    }

    // 边界②：人少于 Σquota → 每人一个身份，多余槽位空置（不足额不是错误）。
    #[test]
    fn identity_fewer_players_than_quota_assigns_everyone() {
        let sk = with_identities(superset(), standard_pool()); // Σquota=10
        let cards = roster(&["c1", "c2", "c3"]);
        let a = plan_with_roster(&sk, "world_under", &cards).identity_assignments;
        assert_eq!(a.len(), 3, "3 人 / Σquota=10 → 恰好 3 个分配: {a:?}");
        let cids: Vec<&str> = a.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(cids, vec!["c1", "c2", "c3"], "输出按 cid 升序");
    }

    // 内核匹配生效：主题词命中角色执念的角色，更容易站上对应身份（只影响抽取权重，不影响任何数值）。
    #[test]
    fn identity_kernel_match_biases_assignment() {
        let sk = with_identities(
            superset(),
            serde_json::json!([
                { "id": "jilted", "quota": 5, "themes": ["退婚"] },
                { "id": "plain",  "quota": 5 }
            ]),
        );
        // c1 内核贴「退婚」（core_fear + 1 个剧情种子命中），c2 完全不贴。
        let cards = vec![
            ("c1".to_string(), card("c1", "退婚之耻", &["退婚当日的羞辱", "账房算错了银子"])),
            ("c2".to_string(), card("c2", "怕黑", &["夜里赶路"])),
        ];
        let (mut c1_jilted, mut c2_jilted) = (0usize, 0usize);
        for i in 0..60 {
            let a = plan_with_roster(&sk, &format!("world_kernel_{i}"), &cards).identity_assignments;
            for (cid, id) in &a {
                if id == "jilted" {
                    if cid == "c1" {
                        c1_jilted += 1;
                    } else {
                        c2_jilted += 1;
                    }
                }
            }
        }
        assert!(
            c1_jilted > c2_jilted,
            "内核贴合者应更常站上对应身份（c1={c1_jilted} / c2={c2_jilted}）"
        );
        assert!(c2_jilted > 0, "不贴合者仍应有机会（+1 保底权重，不得锁死成血统制）");
    }

    // 老模板零影响①：超集模板未声明 identityPool → 无分配，其余维度逐字段不变。
    #[test]
    fn no_identity_pool_means_no_assignment_and_no_disturbance() {
        let bare = superset();
        let with_pool = with_identities(superset(), standard_pool());
        let cards = roster(&["c1", "c2", "c3"]);

        let a = plan_with_roster(&bare, "world_zero_impact", &cards);
        assert!(a.identity_assignments.is_empty(), "无 identityPool → 不产身份分配");
        let sa = a.audit.unwrap();
        assert!(sa.identity_assignments.is_empty(), "无 identityPool → 审计段身份为空");

        // 加了身份维度也不得扰动剧情采样（DOMAIN_IDENTITY 独立子流域 + 排在最后）。
        let sb = plan_with_roster(&with_pool, "world_zero_impact", &cards).audit.unwrap();
        assert_eq!(sa.selected_storylines, sb.selected_storylines);
        assert_eq!(sa.selected_mainline, sb.selected_mainline);
        assert_eq!(sa.selected_hidden, sb.selected_hidden);
        assert_eq!(sa.selected_endings, sb.selected_endings);
        assert_eq!(sa.selected_npcs, sb.selected_npcs);
        assert_eq!(sa.selected_locations, sb.selected_locations);
    }

    // 老模板零影响②：非超集老骨架（无 identityPool）→ 退化路径原样，身份为空。
    #[test]
    fn legacy_skeleton_has_no_identity_assignment() {
        let sk: Skeleton = serde_json::from_value(serde_json::json!({
            "mainlineNodes": [ { "id": "n1", "fated": true } ],
            "hiddenContentPool": [ { "id": "h1", "themes": ["x"] } ],
            "endingPool": [ { "id": "e1", "baseWeight": 1.0 } ]
        }))
        .unwrap();
        let s = plan_with_roster(&sk, "world_legacy_ident", &roster(&["c1", "c2"]));
        assert!(s.audit.is_none(), "退化路径不产采样审计段");
        assert!(s.identity_assignments.is_empty(), "老模板无 identityPool → 零身份分配");
    }

    // 身份池是独立维度：非超集模板只要声明了 identityPool 也照常分配（且仍无采样审计段）。
    #[test]
    fn identity_pool_works_on_non_superset_template() {
        let mut sk: Skeleton = serde_json::from_value(serde_json::json!({
            "mainlineNodes": [ { "id": "n1", "fated": true } ],
            "endingPool": [ { "id": "e1", "baseWeight": 1.0 } ]
        }))
        .unwrap();
        sk.identity_pool = serde_json::from_value(standard_pool()).unwrap();
        let cards = roster(&["c1", "c2"]);
        let s = plan_with_roster(&sk, "world_plain_ident", &cards);
        assert!(s.audit.is_none(), "非超集仍走退化采样路径");
        assert_eq!(s.identity_assignments.len(), 2, "身份池不搭超集判据的车: {:?}", s.identity_assignments);
        assert_eq!(
            s.identity_assignments,
            plan_with_roster(&sk, "world_plain_ident", &cards).identity_assignments,
            "退化路径的身份分配同样确定"
        );
    }

    // 空阵容不 panic（建房后未入场即装配的边角）。
    #[test]
    fn identity_assignment_with_empty_roster() {
        let sk = with_identities(superset(), standard_pool());
        let s = plan_with_roster(&sk, "world_empty_roster", &[]);
        assert!(s.identity_assignments.is_empty());
    }

    // ---------- 🔴 确定性契约：全仓「禁三样」----------

    /// 「禁三样」（系统随机 / 浮点 RNG / map 迭代序驱动 RNG）此前只有 **`ifline` 一个模块**
    /// 有源码级红线（`red_line_module_is_rng_free`），而这句话本身住在**本文件**——
    /// 装配与采样才是确定性最要紧的地方，却没有任何闸。
    ///
    /// 本条按「排除表而非包含表」扫**全仓生产码**：新模块默认被覆盖，不必有人记得加。
    /// 这个方向是刻意的，理由与 `world_scan_text` 那次相同——漏扫一个模块，
    /// 那个模块的不可复现随机不会报错，只会让黄金世界回归**偶发**变红，
    /// 而偶发红的第一反应通常是「重跑一下」。
    ///
    /// 禁的是**不可复现**的 API，不是「随机」本身：确定性随机走
    /// `assembly::fnv1a_64` + `Rng`（SplitMix64）并登记域常量。
    ///
    /// ⚠️ 也不禁 `HashMap`：契约禁的是「用 map 迭代序驱动 RNG」，而不是拿 map 做查表。
    /// 源码级扫描分不出这两者，一律禁会逼出一堆无意义的改写并让这道门被无视。
    /// 迭代序那一支由「同种子同结果」的行为用例负责（`same_seed_same_sampling` 等）。
    #[test]
    fn red_line_no_irreproducible_randomness_outside_the_exempt_list() {
        /// 不可复现的随机 / 时钟 API。
        const BANNED: &[&str] =
            &["thread_rng", "rand::random", "gen_range", "shuffle(", "SystemTime::now", "Instant::now"];

        /// 豁免，每条都必须说得出理由：
        /// - `auth/mod.rs`：会话 token 与短信验证码**必须**是密码学随机——
        ///   确定性 token 是安全漏洞，这里的「不可复现」正是要的性质。
        /// - `db.rs`：全仓**唯一的时间源**（`now_ms`）。别处要时间一律调它，
        ///   于是「现在几点」也只有一处定义（测试可据此整体控制时间）。
        const EXEMPT: &[&str] = &["auth/mod.rs", "db.rs"];

        let mut offenders: Vec<String> = Vec::new();
        for (path, src) in crate::testkit::production_sources() {
            if EXEMPT.iter().any(|e| path.ends_with(e)) {
                continue;
            }
            // 剥注释：本仓多处注释里逐字写着 `thread_rng` 来说明「禁的是它」。
            let code: String = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for b in BANNED {
                if code.contains(b) {
                    offenders.push(format!("{path} → {b}"));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "🔴 生产码出现不可复现的随机/时钟 API：{offenders:?}\n\
             确定性产出是合规红线（同一种子同一结果，黄金世界回归与录制回放都以此为前提）。\n\
             · 需要随机 → 走 `assembly::fnv1a_64` + `Rng`（SplitMix64）并登记域常量；\n\
             · 需要时间 → 走 `db::now_ms`（全仓唯一时间源）。\n\
             若确有必须不可复现的理由（如密码学随机），加进 EXEMPT 并写清楚，不要只把断言改绿。"
        );
        // 扫描器失效 = 红线静默失效：确认豁免名单里的文件**确实**含被禁 API，
        // 否则说明扫描根本没读到东西。
        for e in EXEMPT {
            let hit = crate::testkit::production_sources()
                .iter()
                .any(|(p, src)| p.ends_with(e) && BANNED.iter().any(|b| src.contains(b)));
            assert!(hit, "🔴 豁免项 `{e}` 里一个被禁 API 都没扫到——要么它不再需要豁免（请删掉），要么扫描器坏了");
        }
    }

    // ---------- 建模板期校验：未知顶层键 ----------
    //
    // 这一组防的是本文件里最不像 bug 的那类 bug：**拼错一个键，全程零报错**。
    // `Skeleton` 每个字段都 `#[serde(default)]`，未知字段被 serde 忽略；手读点则
    // `.get(k)…unwrap_or_default()`。所以拼错的直接后果是「模板建成功了，但世界是空的」。

    #[test]
    fn validate_rejects_misspelled_top_level_key() {
        // 大小写写错（camelCase 里最常见的一种）。
        let err = validate_skeleton_refs(&json!({ "mainLineNodes": [] }), false).unwrap_err();
        assert!(err.contains("mainLineNodes"), "错误须点名那个键: {err}");
        assert!(err.contains("是不是想写 `mainlineNodes`"), "须给出归一化后的近似建议: {err}");
    }

    #[test]
    fn validate_rejects_snake_case_top_level_key() {
        // 照着 Rust 侧字段名（snake_case）写线上格式，是另一种高频错法。
        let err = validate_skeleton_refs(&json!({ "forbidden_predicates": [] }), false).unwrap_err();
        assert!(err.contains("是不是想写 `forbiddenPredicates`"), "{err}");
    }

    #[test]
    fn validate_rejects_key_nobody_reads() {
        // 无近似项 → 仍然拒绝，只是不给建议（没人读的键只可能是残留或臆造）。
        let err = validate_skeleton_refs(&json!({ "difficultyCurve": 3 }), false).unwrap_err();
        assert!(err.contains("difficultyCurve"), "{err}");
        assert!(!err.contains("是不是想写"), "无近似项时不该硬凑建议: {err}");
    }

    // 🔴 这两个键**不在 `struct Skeleton` 里**，只被 runtime 按字符串键手读。
    // 若有人"清理"清单时按 `Skeleton` 的字段表重写它，真实模板会当场被拒——本例即那道闸。
    #[test]
    fn validate_accepts_hand_read_keys_absent_from_skeleton_struct() {
        let v = json!({
            "endgame": { "minWorldTicks": 10, "maxWorldTicks": 40 },
            "forbiddenPredicates": ["world.warDeclared == true"],
        });
        assert!(validate_skeleton_refs(&v, false).is_ok(), "{:?}", validate_skeleton_refs(&v, false));
    }

    /// 🔴 顺序钉子：未知键校验必须**先于**「解析不出结构化骨架就放行」那条防御分支。
    /// 下面这份骨架同时带了拼错的键**和**一个类型不符的字段（`worldItems` 应为数组）——
    /// 后者会让 `from_value::<Skeleton>` 失败。若把新校验挪到那句之后，这里会静默变成 Ok，
    /// 而这恰恰是最该拦的一类：写错一处的人通常不止写错一处。
    #[test]
    fn top_level_key_check_runs_before_the_lenient_parse_bailout() {
        let v = json!({ "mainLineNodes": [], "worldItems": "本该是数组" });
        assert!(
            serde_json::from_value::<Skeleton>(v.clone()).is_err(),
            "前提：这份骨架必须解析失败，否则本例证明不了顺序"
        );
        let err = validate_skeleton_refs(&v, false).unwrap_err();
        assert!(err.contains("mainLineNodes"), "{err}");
    }

    #[test]
    fn validate_accepts_double_underscore_comment_keys() {
        // golden fixture 用 `__doc` 放说明文字，是既有约定。
        assert!(validate_skeleton_refs(&json!({ "__doc": "本骨架用于回归", "__note": 1 }), false).is_ok());
    }


    // ---------- 建模板期校验：`mainlineNodes[]` 的节点级键 ----------

    #[test]
    fn validate_rejects_misspelled_mainline_node_key() {
        // `constraint` 拼错 → runtime 落到 `_ => Soft`，本该 hard 的约束静默降级。
        let v = json!({ "mainlineNodes": [ { "id": "mn-1", "constrait": "hard" } ] });
        let err = validate_skeleton_refs(&v, false).unwrap_err();
        assert!(err.contains("mn-1"), "须点名是哪个节点: {err}");
        assert!(err.contains("constrait"), "{err}");
        assert!(err.contains("是不是想写 `constraint`"), "{err}");
    }

    #[test]
    fn validate_names_node_by_index_when_id_missing() {
        let v = json!({ "mainlineNodes": [ { "id": "mn-1" }, { "summry": "缺 id 的节点" } ] });
        let err = validate_skeleton_refs(&v, false).unwrap_err();
        assert!(err.contains("mainlineNodes[#2].summry"), "无 id 时须给序号，否则运营找不到是哪一条: {err}");
    }

    /// 🔴 三层里失败方向最坏的一层：`expression` 拼错 → runtime 那里直接 `continue`，
    /// **整条禁止谓词被丢弃**，世界照常开，只是那条内容约束从来没生效过，且没有任何日志。
    #[test]
    fn validate_rejects_misspelled_forbidden_predicate_key() {
        let v = json!({ "forbiddenPredicates": [ { "id": "fp-1", "expresion": "x == 1" } ] });
        let err = validate_skeleton_refs(&v, false).unwrap_err();
        assert!(err.contains("fp-1"), "{err}");
        assert!(err.contains("是不是想写 `expression`"), "{err}");

        let ok = json!({ "forbiddenPredicates": [
            { "id": "fp-1", "expression": "x == 1", "reason": "剧透" }
        ] });
        assert!(validate_skeleton_refs(&ok, false).is_ok(), "{:?}", validate_skeleton_refs(&ok, false));
    }

    /// 装配层与 runtime 各读一半——两边的键都得放行，任何一侧被误拦都是线上事故。
    #[test]
    fn validate_accepts_the_union_of_both_readers_node_keys() {
        let v = json!({ "mainlineNodes": [ {
            "id": "mn-1", "fated": true, "variantGroup": "g1", "arcTags": ["arc-A"],   // 装配层读
            "summary": "开局", "constraint": "hard", "threshold": 3.0, "advanceWhen": "x == 1", // runtime 读
        } ] });
        assert!(validate_skeleton_refs(&v, false).is_ok(), "{:?}", validate_skeleton_refs(&v, false));
    }


    // ---------- 源码层红线：注册表 vs 结构体定义 ----------
    //
    // 这两条是整套逐层校验的护栏。没有它们，`SKELETON_KEY_SETS` 就是"靠人记得更新"的表——
    // 而"记得更新"这个假设，正是它下面每一层缺陷的共同成因。

    /// 三份源码：骨架的类型分散在这三处（后两处是**跨模块 / 跨 crate**契约，
    /// 引擎侧改一个字段名，server 这边只会表现为"读不到值"）。
    /// 路径写死是刻意的：布局变了就编译不过，响亮，好修。
    const SCHEMA_SOURCES: &[&str] = &[
        include_str!("mod.rs"),
        include_str!("../admission/mod.rs"),
        include_str!("../../../crates/muse-engine/src/narrative/types.rs"),
    ];

    fn camel_case(snake: &str) -> String {
        let mut out = String::new();
        let mut up = false;
        for c in snake.chars() {
            match c {
                '_' => up = true,
                _ if up => {
                    out.extend(c.to_uppercase());
                    up = false;
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// 从三份源码里找 `struct <name> { .. }`，返回 `(camelCase 字段名, 类型文本)`。
    /// 找不到 → `None`（外部类型，如引擎的 `CharacterCardV2`）。
    fn struct_fields(name: &str) -> Option<Vec<(String, String)>> {
        for src in SCHEMA_SOURCES {
            let needle = format!("struct {name} {{");
            let Some(i) = src.find(&needle) else { continue };
            let body_start = i + needle.len();
            let body = &src[body_start..body_start + src[body_start..].find("\n}")?];
            let mut fields = Vec::new();
            // 显式 `#[serde(rename = "X")]` 优先于 snake→camel 推导。
            // 🔴 不能假设「线上名一定是字段名的 camelCase」——这套红线的意义正是不靠假设。
            // 引擎里已有一处这样的字段（`DomainEvent.event_type` → `"type"`，当前从 `Skeleton` 不可达）。
            let mut pending_rename: Option<String> = None;
            for line in body.lines() {
                // 只取一级字段：恰好 4 空格缩进、非属性/注释、形如 `name: Type,`。
                if !line.starts_with("    ") || line.starts_with("     ") {
                    continue;
                }
                let t = line.trim();
                if t.starts_with('#') {
                    if let Some(r) = t.split("rename = \"").nth(1).and_then(|s| s.split('"').next()) {
                        if !t.contains("rename_all") {
                            pending_rename = Some(r.to_string());
                        }
                    }
                    continue;
                }
                if t.starts_with("//") {
                    continue;
                }
                let Some((n, ty)) = t.split_once(':') else { continue };
                let n = n.trim().trim_start_matches("pub(crate) ").trim_start_matches("pub ");
                if n.is_empty()
                    || !n.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
                {
                    continue;
                }
                let wire = pending_rename.take().unwrap_or_else(|| camel_case(n));
                fields.push((wire, ty.trim().trim_end_matches(',').to_string()));
            }
            return Some(fields);
        }
        None
    }

    /// 已登记的每一条，其结构体字段都必须在该条的键集里。
    /// 往结构体加字段而忘了加注册表 → 用了新字段的合法模板会被建模板期校验拒掉，本例先一步指出漏了哪个。
    #[test]
    fn registered_key_sets_cover_every_struct_field() {
        let mut checked = 0;
        for (path, struct_name, keys) in SKELETON_KEY_SETS {
            if struct_name.is_empty() {
                continue; // 无结构体（全靠手读），键集本身就是唯一定义。
            }
            let Some(fields) = struct_fields(struct_name) else {
                panic!("`{struct_name}`（路径 `{path}`）在三份 schema 源码里都找不到——是不是挪了文件/改了名？");
            };
            assert!(!fields.is_empty(), "`{struct_name}` 解析出 0 个字段，解析器疑似失效");
            for (f, _) in &fields {
                assert!(
                    keys.contains(&f.as_str()),
                    "`{struct_name}` 的字段 `{f}` 不在路径 `{path}` 的键集里——用到它的模板会被建模板期校验拒掉"
                );
            }
            checked += 1;
        }
        assert!(checked >= 25, "只校到 {checked} 条，注册表疑似被削了");
    }

    /// 🔴 可达性红线：从 `struct Skeleton` 出发的**每一个**结构体都必须在注册表里。
    ///
    /// 这条防的是「加了一层嵌套结构体但忘了登记」——那种情况下新那层不会被校验，
    /// 拼错继续静默，而且**没有任何征兆**（未登记路径按设计不检查）。
    /// 有了它，忘记登记会在 CI 上变红，而不是在某个模板上悄悄生效。
    #[test]
    fn every_skeleton_struct_is_registered() {
        // 不下钻：`worldCharacters[].card` 的 schema 归引擎所有（见 SKELETON_OPAQUE_PATHS）。
        const EXTERNAL_OPAQUE: &[&str] = &["CharacterCardV2", "Value"];

        let registered: std::collections::BTreeSet<&str> =
            SKELETON_KEY_SETS.iter().map(|(_, s, _)| *s).filter(|s| !s.is_empty()).collect();

        let mut seen = std::collections::BTreeSet::new();
        let mut queue = vec!["Skeleton".to_string()];
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) || EXTERNAL_OPAQUE.contains(&name.as_str()) {
                continue;
            }
            assert!(
                registered.contains(name.as_str()),
                "结构体 `{name}` 从 `Skeleton` 可达，却不在 SKELETON_KEY_SETS 里——\
                 它那一层的键不会被校验，拼错会继续静默。给它登记一条路径，或（若确属外部 schema）\
                 加进 SKELETON_OPAQUE_PATHS + EXTERNAL_OPAQUE 并说明理由。"
            );
            let Some(fields) = struct_fields(&name) else { continue };
            for (_, ty) in fields {
                // 从 `Vec<Option<Foo>>` / `Vec<(String, f32)>` 里挑出大写开头的类型名。
                for tok in ty.split(|c: char| !c.is_alphanumeric() && c != '_') {
                    if tok.chars().next().is_some_and(char::is_uppercase)
                        && !matches!(tok, "String" | "Vec" | "Option" | "BTreeMap" | "BTreeSet")
                    {
                        queue.push(tok.to_string());
                    }
                }
            }
        }
        assert!(seen.len() >= 20, "只走到 {} 个结构体，可达性遍历疑似失效", seen.len());
    }

    /// 端到端：黄金世界的真实骨架必须通过这道新校验。
    /// 它是仓库里唯一一份「运营真会写成那样」的完整模板，也是 `endgame` / `forbiddenPredicates` /
    /// `__doc` 三个非 `Skeleton` 键的现实出处。
    #[test]
    fn golden_skeleton_passes_top_level_key_check() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../runtime/golden/skeleton.json")).expect("golden 骨架 JSON");
        assert!(validate_skeleton_refs(&v, false).is_ok(), "{:?}", validate_skeleton_refs(&v, false));
    }

    // ---------- 建模板期校验：身份池引用完整性 ----------

    fn skeleton_with_pool(pool: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "storylines": [ { "id": "arc-A", "hiddenPoolIds": ["h1"] } ],
            "hiddenContentPool": [ { "id": "h1", "themes": ["x"] } ],
            "sideHookPool": [ { "id": "s1", "themes": ["y"] } ],
            "identityPool": pool
        })
    }

    #[test]
    fn validate_accepts_wellformed_identity_pool() {
        let sk = skeleton_with_pool(serde_json::json!([
            { "id": "official", "label": "户部主事", "quota": 3, "hookAffinity": ["arc-A", "h1", "s1"] },
            { "id": "jilted", "label": "被退婚的嫡女", "isLead": true }
        ]));
        assert!(
            validate_skeleton_refs(&sk, false).is_ok(),
            "合法身份池不得被拦：{:?}",
            validate_skeleton_refs(&sk, false)
        );
    }

    #[test]
    fn validate_rejects_duplicate_identity_id() {
        let sk = skeleton_with_pool(serde_json::json!([
            { "id": "official", "quota": 2 },
            { "id": "official", "quota": 1 }
        ]));
        let err = validate_skeleton_refs(&sk, false).unwrap_err();
        assert!(err.contains("id 重复"), "应报重复 id，实得：{err}");
    }

    #[test]
    fn validate_rejects_zero_quota() {
        let sk = skeleton_with_pool(serde_json::json!([{ "id": "official", "quota": 0 }]));
        let err = validate_skeleton_refs(&sk, false).unwrap_err();
        assert!(err.contains("quota"), "应报 quota 非法，实得：{err}");
    }

    #[test]
    fn validate_rejects_dangling_hook_affinity() {
        let sk = skeleton_with_pool(serde_json::json!([
            { "id": "official", "quota": 1, "hookAffinity": ["arc-NOPE"] }
        ]));
        let err = validate_skeleton_refs(&sk, false).unwrap_err();
        assert!(err.contains("hookAffinity 悬空"), "应报引力悬空，实得：{err}");
    }

    #[test]
    fn validate_rejects_blank_identity_id() {
        let sk = skeleton_with_pool(serde_json::json!([{ "id": "  ", "label": "无名位", "quota": 1 }]));
        let err = validate_skeleton_refs(&sk, false).unwrap_err();
        assert!(err.contains("缺少 id"), "应报缺少 id，实得：{err}");
    }

    // 老模板（无 identityPool）照旧放行。
    #[test]
    fn validate_passes_skeleton_without_identity_pool() {
        let sk = serde_json::json!({ "hiddenContentPool": [ { "id": "h1" } ] });
        assert!(validate_skeleton_refs(&sk, false).is_ok());
    }

    // ---------- 境界档（总规格 §6【拍板 3】戏服原则）：schema 落点 + 零影响 + 平权红线 ----------

    fn realm_json() -> serde_json::Value {
        serde_json::json!({
            "id": "tier-douwang",
            "label": "斗王档",
            "cosmology": "cultivation",
            "genre": "xuanhuan",
            "conflictIntensity": "martial",
            "briefing": "本篇全员领斗王档戏服：能御空短距、能扛一记斗皇余威，仅此而已。",
            "flavorNotes": ["魂技译为斗气招式风味，内核不变"]
        })
    }

    fn with_realm(mut sk: Skeleton, v: serde_json::Value) -> Skeleton {
        sk.realm_tier = serde_json::from_value(v).unwrap();
        sk
    }

    /// 🔴 **零影响契约**：声明与不声明 `realmTier`，采样产物的**七个维度逐项相同**。
    ///
    /// 这是境界档能安全落地的全部依据——它不掷骰子、不占 RNG 域、不参与任何选取，
    /// 所以加了这一维之后，storyline / mainline / hidden / ending / NPC / 地点 / 身份
    /// 必须一字不差。这条一旦红，说明有人把境界接进了采样，黄金世界基线随即失效。
    #[test]
    fn realm_tier_does_not_disturb_any_sampling_dimension() {
        let cards = roster(&["c1", "c2", "c3"]);
        let bare = with_identities(superset(), standard_pool());
        let dressed = with_realm(with_identities(superset(), standard_pool()), realm_json());

        let a = plan_with_roster(&bare, "world_realm_zero", &cards);
        let b = plan_with_roster(&dressed, "world_realm_zero", &cards);
        let (sa, sb) = (a.audit.unwrap(), b.audit.unwrap());
        assert_eq!(sa.selected_storylines, sb.selected_storylines);
        assert_eq!(sa.selected_mainline, sb.selected_mainline);
        assert_eq!(sa.selected_hidden, sb.selected_hidden);
        assert_eq!(sa.selected_endings, sb.selected_endings);
        assert_eq!(sa.selected_npcs, sb.selected_npcs);
        assert_eq!(sa.selected_locations, sb.selected_locations);
        assert_eq!(sa.identity_assignments, sb.identity_assignments);
        assert_eq!(sa.seed, sb.seed, "境界档不进种子：同 world_id 同阵容必得同一颗种子");
        assert_eq!(a.selected_ending, b.selected_ending, "结局定盘不受境界档影响");
    }

    /// 🔴 **未声明 → `assembled_json` 逐字节不变**：`skip_serializing_if` 必须让 `realmTier` 键
    /// 整个消失（不是 `"realmTier": null`）。同时把 `assembly` 段的**键序列**钉死——
    /// 加字段时若不小心插到中间、或忘了 skip，这条会立刻红，黄金世界快照就不会被悄悄改写。
    #[test]
    fn realm_tier_absent_keeps_assembled_json_byte_identical() {
        let base = AssembledInstance {
            per_character_hooks: vec![],
            enabled_endings: vec![],
            selected_ending: None,
            lineup_params: serde_json::json!({}),
            difficulty_notes: vec![],
            home_advantages: vec![],
            world_character_entries: vec![],
            location_graph: vec![],
            resident_items: vec![],
            identity_assignments: vec![],
            sampling: None,
            payout_table: None,
            realm_tier: None,
        };
        let raw = serde_json::to_string(&base).unwrap();
        assert!(!raw.contains("realmTier"), "未声明境界档时不得写出任何 realmTier 键：{raw}");

        // 🔴 直接钉死**原始字节串**（不能解析回 Map 再比——`serde_json::Map` 默认是 BTreeMap，
        // 一解析就把顺序洗成字典序，恰好把这条测试要抓的"字段插到中间"洗没了）。
        assert_eq!(
            raw,
            r#"{"perCharacterHooks":[],"enabledEndings":[],"lineupParams":{},"difficultyNotes":[],"homeAdvantages":[],"worldCharacterEntries":[],"locationGraph":[],"residentItems":[]}"#,
            "assembly 段的字节形态被改动 = 既有实例的 assembled_json 不再逐字节可比"
        );

        // 声明了才多出这一个键，且它**追加在最后**：既有前缀一字节不动，
        // 于是"声明境界档"对老字段是纯增量，不会挤动任何既有键的位置。
        let dressed = AssembledInstance { realm_tier: Some(serde_json::from_value(realm_json()).unwrap()), ..base };
        let dressed_raw = serde_json::to_string(&dressed).unwrap();
        let prefix = &raw[..raw.len() - 1]; // 去掉收尾的 `}`
        assert!(
            dressed_raw.starts_with(prefix),
            "境界档必须追加在末尾，既有字节前缀不得变动：\n{dressed_raw}"
        );
        assert!(
            dressed_raw[prefix.len()..].starts_with(r#","realmTier":{"#),
            "追加段应恰好是 realmTier：{}",
            &dressed_raw[prefix.len()..]
        );
    }

    /// 🔴 **平权红线锁**（§6「跨体系靠风味翻译，不靠数值换算」+ §0.1）：境界档序列化后
    /// **一个数字 / 布尔都不许有**，全部是字符串或字符串数组。
    ///
    /// 谁要给它加 `level` / `powerTier` / `combatBonus`，这条测试会先红——那不是"补一个字段"，
    /// 那是把「选阶段」变成「选强度」，等于用戏服开了数值侧门。
    #[test]
    fn realm_tier_carries_no_numeric_field() {
        let rt: RealmTier = serde_json::from_value(realm_json()).unwrap();
        let v = serde_json::to_value(&rt).unwrap();
        for (k, val) in v.as_object().expect("境界档应序列化为对象") {
            let ok = val.is_string()
                || val.as_array().is_some_and(|a| a.iter().all(serde_json::Value::is_string));
            assert!(ok, "境界档字段 `{k}` 不是字符串 / 字符串数组：{val} —— 数值化 = 平权红线违规");
        }
    }

    /// 建模板期取值域校验：三项枚举各自拦得住自由文本；留空一律放行。
    #[test]
    fn validate_realm_tier_enums() {
        let sk = |rt: serde_json::Value| serde_json::json!({ "realmTier": rt });

        assert!(validate_skeleton_refs(&sk(realm_json()), false).is_ok(), "合法境界档不得被拦");

        // 三项全留空 = 无战力体系题材（都市 / 言情 / 历史），§6 明说合法。
        assert!(
            validate_skeleton_refs(&sk(serde_json::json!({ "id": "tier-broke", "label": "破产后的第三个月" })), false).is_ok(),
            "空体系 / 空题材 / 空烈度是合法取值（境界泛化为处境）"
        );

        let err = validate_skeleton_refs(&sk(serde_json::json!({ "id": "  " })), false).unwrap_err();
        assert!(err.contains("realmTier 缺少 id"), "应报缺少 id，实得：{err}");

        let err = validate_skeleton_refs(&sk(serde_json::json!({ "id": "t", "cosmology": "斗气" })), false).unwrap_err();
        assert!(err.contains("cosmology 非法"), "自由文本体系必须被拦，实得：{err}");

        let err = validate_skeleton_refs(&sk(serde_json::json!({ "id": "t", "genre": "宫斗" })), false).unwrap_err();
        assert!(err.contains("genre 非法"), "自由文本题材必须被拦，实得：{err}");

        let err =
            validate_skeleton_refs(&sk(serde_json::json!({ "id": "t", "conflictIntensity": "很凶" })), false).unwrap_err();
        assert!(err.contains("conflictIntensity 非法"), "自由文本烈度必须被拦，实得：{err}");

        // 老模板（无 realmTier）照旧放行。
        assert!(validate_skeleton_refs(&serde_json::json!({ "hiddenContentPool": [] }), false).is_ok());
    }

    /// §6「角色带走道具与历练，**永不带走境界**」：境界档只存在于实例装配产物里，
    /// 装配层从不因它写任何角色侧的表。本文件的生产代码段内不得出现对 `cloud_characters` 的写入。
    #[test]
    fn realm_tier_never_written_back_to_card() {
        // 与 `red_line_assembly_never_writes_subplot_cards` 同款静态扫描：只扫生产代码段
        //（首个 test-only 编译属性之前），避免测试里的字面量造成假红。
        let src = include_str!("mod.rs");
        let cut = src.find(&concat!("#[cfg", "(test)]").to_string()).unwrap_or(src.len());
        let prod = &src[..cut];
        for verb in ["UPDATE cloud_characters", "INSERT INTO cloud_characters", "DELETE FROM cloud_characters"] {
            assert!(
                !prod.contains(verb),
                "装配层出现了对角色卡的写入（`{verb}`）：境界永不带走，卡侧一个字节都不该被装配改动"
            );
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

// ============================================================================
// R2 自定义房装配单测：命名空间 / 四段式种子 / 缝合 / 建房期校验 / 红线
// ============================================================================

#[cfg(test)]
mod container_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    const PROFILE: (u32, u32, u32) = (1, 1, 1);

    /// 容器装配开关的 RAII 夹具（进程级 env → 同一把锁串行化，Drop 时恢复原状）。
    /// 范式同 `worlds::DeathmatchSwitch` / `subplot::SubplotSwitch`。
    /// 定义在测试模块内：本文件的生产代码段不得出现 test-only 编译属性（见文件中部说明）。
    pub(crate) struct ContainerSwitch {
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    static CONTAINER_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 气运/机缘摆幅两个 env 的进程级串行化锁。
    ///
    /// 🔴 **只读那一条也必须进锁**。第一版只给「写 env」的两条端到端用例加了锁，
    /// 结果 `opportunity_stays_within_the_template` 只读不写、没进锁，
    /// 被并行跑的写方污染 → `hk` 从 2 变 3，症状表现为「气运看起来没生效」，
    /// 排查方向被整个带偏（去查 `choose_k` 和 scale_weight 了，那两处都没问题）。
    /// 口径与 `ContainerSwitch` 同源，理由见其上的注释。
    static FORTUNE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII：进作用域拿锁并设值，出作用域自动还原（含 panic 路径）。
    pub(crate) struct FortuneEnv {
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: Vec<(&'static str, Option<String>)>,
    }

    impl FortuneEnv {
        /// `pairs` 为 `(env 名, 值)`；传空即「只锁不改」，供只读用例用。
        pub(crate) fn set(pairs: &[(&'static str, &str)]) -> Self {
            let guard = FORTUNE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let keys = [super::ENV_OPPORTUNITY_EXTRA_HOOKS, super::ENV_FORTUNE_SWING_BP];
            let prev = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in keys {
                std::env::remove_var(k); // 先清干净，避免上一条用例的残留混进来
            }
            for (k, v) in pairs {
                std::env::set_var(k, v);
            }
            Self { _guard: guard, prev }
        }
    }

    impl Drop for FortuneEnv {
        fn drop(&mut self) {
            for (k, v) in &self.prev {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    impl ContainerSwitch {
        pub(crate) fn set(on: bool) -> Self {
            let sw = Self::take_lock();
            std::env::set_var(ENV_CONTAINER_ASSEMBLY, if on { "1" } else { "0" });
            sw
        }

        /// 🔴 **移除 env 也必须拿同一把锁**。此前「测默认值」那条用例直接
        /// `remove_var` + 手动存还，不进锁——它是 `#[test]` 时窗口极短还看不出来，
        /// 改成 `#[tokio::test]`（建池要 await）后窗口一下子变宽，
        /// 与并发的容器用例抢 env，表现为**偶发**红（单跑必绿，全跑偶尔红），最难查的一类。
        pub(crate) fn cleared() -> Self {
            let sw = Self::take_lock();
            std::env::remove_var(ENV_CONTAINER_ASSEMBLY);
            sw
        }

        fn take_lock() -> Self {
            let guard = CONTAINER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var(ENV_CONTAINER_ASSEMBLY).ok();
            Self { _guard: guard, prev }
        }
    }

    impl Drop for ContainerSwitch {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(ENV_CONTAINER_ASSEMBLY, v),
                None => std::env::remove_var(ENV_CONTAINER_ASSEMBLY),
            }
        }
    }

    fn sk(v: serde_json::Value) -> Skeleton {
        serde_json::from_value(v).expect("骨架 JSON")
    }

    fn open_policy() -> crate::admission::WorldAdmissionPolicy {
        Default::default()
    }

    /// 卡片段：**两张卡内部 id 完全相同**（arc/mn/hc/end/wi/gate/inner/secret）——
    /// 命名空间若失效，合并即撞车，这是本组测试的主要探针。
    fn frag_json(theme: &str, item_tier: u8) -> serde_json::Value {
        json!({
            "storylines": [
                { "id": "arc", "mainlineNodeIds": ["mn"], "hiddenPoolIds": ["hc"], "endingIds": ["end"] }
            ],
            "mainlineNodes": [ { "id": "mn", "fated": true, "arcTags": ["arc"] } ],
            "hiddenContentPool": [
                { "id": "hc", "arcTags": ["arc"], "themes": [theme], "rewardItemRef": "wi", "variantGroup": "vg" }
            ],
            "endingPool": [ { "id": "end", "arcTags": ["arc"] } ],
            "worldItems": [ {
                "id": "wi", "narrative": "卡内道具", "effectTags": ["advantage:combat"],
                "origin": { "worldTemplateId": "src", "cosmology": ["myth"], "powerTier": item_tier }
            } ],
            "locations": [
                { "id": "gate",   "connections": ["inner"], "residentItemIds": ["wi"] },
                { "id": "inner",  "connections": ["gate", "secret"] },
                { "id": "secret", "isSecretRealm": true, "connections": ["inner"] }
            ],
            "anchors": ["gate"]
        })
    }

    /// 容器本体：超集 + 一个孤立的本体地点（故意不连通，用来验证缝合）。
    fn container_json() -> serde_json::Value {
        json!({
            "isSuperset": true,
            "storylines": [
                { "id": "core-arc", "mainlineNodeIds": ["core-mn"], "hiddenPoolIds": ["core-hc"], "endingIds": ["core-end"] }
            ],
            "mainlineNodes": [ { "id": "core-mn", "fated": true, "arcTags": ["core-arc"] } ],
            "hiddenContentPool": [ { "id": "core-hc", "arcTags": ["core-arc"], "themes": ["core"] } ],
            "endingPool": [ { "id": "core-end", "arcTags": ["core-arc"] } ],
            "locations": [ { "id": "core-hub", "connections": [] } ],
            "anchors": ["core-hub"],
            "sampling": { "instanceStorylineCount": 2, "instanceHiddenCount": 2, "instanceLocationCount": 5 }
        })
    }

    fn card(id: &str, ver: i64, star: i64, weight: f32, frag: serde_json::Value) -> ContainerCard {
        ContainerCard {
            card_id: id.to_string(),
            card_version: ver,
            star_rating: star,
            weight,
            fragment: sk(frag),
        }
    }

    /// 标准两卡容器（卡星级 5，容器星级 5 → 不触发夹档；夹档另有专项）。
    fn two_card_container(refs: serde_json::Value) -> (Skeleton, Vec<ContainerCard>) {
        let mut c = container_json();
        c["subplotCardRefs"] = refs;
        (
            sk(c),
            vec![
                card("c1", 1, 5, 1.0, frag_json("甲", 1)),
                card("c2", 1, 5, 1.0, frag_json("乙", 1)),
            ],
        )
    }

    fn refs_json(ids: &[&str]) -> serde_json::Value {
        serde_json::Value::Array(ids.iter().map(|id| json!({ "cardId": id })).collect())
    }

    fn compose(container: Skeleton, cards: &[ContainerCard], star: i64) -> Result<Skeleton, String> {
        compose_container_skeleton(container, cards, star, &open_policy())
    }

    fn plan(sk: &Skeleton, world_id: &str, fp: &str, star: i64) -> Selection {
        plan_sampling(sk, fp, world_id, 1, &PROFILE, &[], &[], 0.5, star, "")
    }

    /// 一组地点 id 在给定图上是否连通（无向）。
    fn ids_connected(all: &[LocationSpec], ids: &[String]) -> bool {
        let subset: Vec<LocationSpec> = all
            .iter()
            .filter(|l| ids.iter().any(|i| i == &l.id))
            .map(|l| {
                let mut l = l.clone();
                l.connections.retain(|c| ids.iter().any(|i| i == c));
                l
            })
            .collect();
        location_components(&subset).len() <= 1
    }

    // ---------------- ① 开关：默认关闭 → 原路径、原公式 ----------------

    /// env 语义（解析链第 ④ 层）。接入 `flags` 体系后**行为一字未改**——
    /// 空 `runtime_flags` 表上解析必然落到 env，这正是「行为零变化」的回归保护。
    #[tokio::test]
    async fn switch_defaults_to_off() {
        let db = crate::testkit::test_pool().await;
        // env 未设置时必须是关闭（VALIDATION §0.1 未验证功能默认关闭）。
        // 🔴 经 `cleared()` 拿锁，不手搓 remove_var —— 理由见 `ContainerSwitch::cleared`。
        let _sw = ContainerSwitch::cleared();
        let off = container_assembly_enabled(&db, None).await;
        assert!(!off, "MUSE_CONTAINER_ASSEMBLY 默认必须关闭");
        assert!(!DEFAULT_CONTAINER_ASSEMBLY_ENABLED, "默认常量必须是 false");
        assert_eq!(
            crate::flags::declared_default(ENV_CONTAINER_ASSEMBLY),
            DEFAULT_CONTAINER_ASSEMBLY_ENABLED,
            "登记表与模块常量必须同源（编译期已由 const assert 钉死，此处运行期复述）"
        );
    }

    #[tokio::test]
    async fn switch_rejects_garbage_value_by_falling_back_to_off() {
        let db = crate::testkit::test_pool().await;
        let _sw = ContainerSwitch::set(false);
        std::env::set_var(ENV_CONTAINER_ASSEMBLY, "maybe");
        assert!(!container_assembly_enabled(&db, None).await, "配错不得静默开启未验证功能");
    }

    // ---------------- ② 四段式种子只在容器形态生效 ----------------

    #[test]
    fn four_part_seed_only_in_container_mode() {
        let plain = sk(container_json());
        assert!(plain.container.is_none(), "未合并的骨架不是容器形态");
        assert_eq!(
            resolve_instance_seed(&plain, "w1", "cidA\ncidB", 3, ""),
            instance_seed("w1", "cidA\ncidB", 3),
            "非容器形态必须走原三段式公式（byte 级不变）"
        );

        let (container, cards) = two_card_container(refs_json(&["c1", "c2"]));
        let composed = compose(container, &cards, 5).unwrap();
        let plan_ref = composed.container.as_ref().unwrap();
        assert_eq!(plan_ref.fingerprint, "c1@1\nc2@1", "卡集合指纹 = 排序去重的 cardId@version");
        assert_eq!(
            resolve_instance_seed(&composed, "w1", "cidA\ncidB", 3, ""),
            container_instance_seed("w1", "cidA\ncidB", 3, "c1@1\nc2@1"),
            "容器形态必须走四段式公式"
        );
        assert_ne!(
            resolve_instance_seed(&composed, "w1", "cidA\ncidB", 3, ""),
            instance_seed("w1", "cidA\ncidB", 3),
            "四段式必须与三段式不同，否则卡集合没进种子"
        );
    }

    #[test]
    fn card_set_fingerprint_is_order_independent() {
        let (c_a, cards_a) = two_card_container(refs_json(&["c1", "c2"]));
        let (c_b, _) = two_card_container(refs_json(&["c2", "c1"]));
        let cards_b = vec![cards_a[1].clone(), cards_a[0].clone()];
        let fa = compose(c_a, &cards_a, 5).unwrap().container.unwrap().fingerprint;
        let fb = compose(c_b, &cards_b, 5).unwrap().container.unwrap().fingerprint;
        assert_eq!(fa, fb, "卡集合指纹排序去重后与声明顺序无关（同一套卡 = 同一个房）");
    }

    // ---------------- ③ 防刷：换卡 / 换卡版本 → 种子变 → 采样变 ----------------

    #[test]
    fn swapping_a_card_changes_the_fingerprint_and_seed() {
        let (c_a, cards_a) = two_card_container(refs_json(&["c1", "c2"]));
        let mut c_b = container_json();
        c_b["subplotCardRefs"] = refs_json(&["c1", "c3"]);
        let cards_b =
            vec![card("c1", 1, 5, 1.0, frag_json("甲", 1)), card("c3", 1, 5, 1.0, frag_json("丙", 1))];
        let a = compose(c_a, &cards_a, 5).unwrap();
        let b = compose(sk(c_b), &cards_b, 5).unwrap();
        assert_ne!(
            a.container.as_ref().unwrap().fingerprint,
            b.container.as_ref().unwrap().fingerprint
        );
        assert_ne!(
            resolve_instance_seed(&a, "w_swap", "cidA", 1, ""),
            resolve_instance_seed(&b, "w_swap", "cidA", 1, ""),
            "换一张卡 → 种子必变（防「换卡组合刷同一世界」）"
        );
    }

    /// 🔴 防刷核心：**内容完全相同、只有卡版本不同** → 种子必变、采样必变。
    /// 内容相同保证了差异只可能来自种子（不是"换了内容当然选到不同东西"的假阳性）。
    #[test]
    fn same_content_different_card_version_changes_sampling() {
        let build = |ver: i64| {
            let (c, mut cards) = two_card_container(refs_json(&["c1", "c2"]));
            cards[0].card_version = ver;
            compose(c, &cards, 5).unwrap()
        };
        let (v1, v2) = (build(1), build(2));
        assert_ne!(
            v1.container.as_ref().unwrap().fingerprint,
            v2.container.as_ref().unwrap().fingerprint
        );

        let sig = |s: &Skeleton, w: &str| {
            let a = plan(s, w, "cidA\ncidB", 5).audit.unwrap();
            format!(
                "{}|{}|{}|{}",
                a.seed,
                a.selected_storylines.join(","),
                a.selected_hidden.join(","),
                a.selected_locations.join(",")
            )
        };
        let mut differ = 0usize;
        for i in 0..16 {
            let w = format!("world_cardver_{i}");
            assert_ne!(
                plan(&v1, &w, "cidA\ncidB", 5).audit.unwrap().seed,
                plan(&v2, &w, "cidA\ncidB", 5).audit.unwrap().seed,
                "同一实例、同一内容，仅卡版本不同 → 种子必须不同"
            );
            if sig(&v1, &w) != sig(&v2, &w) {
                differ += 1;
            }
        }
        assert!(differ > 0, "换卡集合后 16 个实例的采样竟然完全一致 —— 卡集合没有真正进采样");
    }

    // ---------------- ④ 确定性：同卡集合恒得同装配 ----------------

    #[test]
    fn same_card_set_yields_identical_assembly() {
        for i in 0..8 {
            let (c_a, cards_a) = two_card_container(refs_json(&["c1", "c2"]));
            let (c_b, cards_b) = two_card_container(refs_json(&["c1", "c2"]));
            let a = compose(c_a, &cards_a, 5).unwrap();
            let b = compose(c_b, &cards_b, 5).unwrap();
            // 合并本身确定：地点图逐项一致。
            assert_eq!(
                a.locations.iter().map(|l| (l.id.clone(), l.connections.clone())).collect::<Vec<_>>(),
                b.locations.iter().map(|l| (l.id.clone(), l.connections.clone())).collect::<Vec<_>>(),
                "合并器必须是纯函数（同输入同输出）"
            );
            let w = format!("world_det_{i}");
            let (sa, sb) =
                (plan(&a, &w, "cidA\ncidB", 5).audit.unwrap(), plan(&b, &w, "cidA\ncidB", 5).audit.unwrap());
            assert_eq!(sa.seed, sb.seed);
            assert_eq!(sa.selected_storylines, sb.selected_storylines);
            assert_eq!(sa.selected_mainline, sb.selected_mainline);
            assert_eq!(sa.selected_hidden, sb.selected_hidden);
            assert_eq!(sa.selected_endings, sb.selected_endings);
            assert_eq!(sa.selected_locations, sb.selected_locations);
            assert_eq!(sa.card_set_fingerprint, sb.card_set_fingerprint);
            assert_eq!(sa.selected_cards, vec!["c1".to_string(), "c2".to_string()]);
        }
    }

    // ---------------- ⑤ 命名空间：多卡混装不撞车 ----------------

    #[test]
    fn namespace_prevents_cross_card_id_collision() {
        let (container, cards) = two_card_container(refs_json(&["c1", "c2"]));
        let m = compose(container, &cards, 5).unwrap();

        // 两张卡内部 id 完全相同，合并后必须全部唯一。
        assert!(assert_unique_ids(&m).is_ok(), "命名空间失效：合并后出现重复 id");

        let ids: Vec<&str> = m.storylines.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["core-arc", "c1:arc", "c2:arc"], "卡内 id 必须带 `卡id:` 前缀，容器本体保持裸 id");
        assert!(m.world_items.iter().any(|i| i.id == "c1:wi"));
        assert!(m.world_items.iter().any(|i| i.id == "c2:wi"));

        // 引用位同步重写：奖励引用、驻留道具、地点连接、变体组、arcTags。
        let hc1 = m.hidden_content_pool.iter().find(|p| p.id == "c1:hc").expect("c1:hc");
        assert_eq!(hc1.reward_item_ref.as_deref(), Some("c1:wi"), "rewardItemRef 必须跟着重写");
        assert_eq!(hc1.variant_group.as_deref(), Some("c1:vg"), "变体组带前缀 → 互斥组天然不跨卡");
        assert_eq!(hc1.arc_tags, vec!["c1:arc".to_string()]);
        let gate1 = m.locations.iter().find(|l| l.id == "c1:gate").expect("c1:gate");
        assert_eq!(gate1.resident_item_ids, vec!["c1:wi".to_string()]);
        assert!(gate1.connections.contains(&"c1:inner".to_string()));

        // 归属映射：前缀即归属，容器本体裸 id 归属为 None。
        assert_eq!(ns_owner("c1:hc"), Some("c1"));
        assert_eq!(ns_owner("core-hc"), None);

        // 变体组不跨卡：两张卡的同名 vg 前缀化后互不干扰，两条 hc 可同时在演。
        let vgs: BTreeSet<&str> =
            m.hidden_content_pool.iter().filter_map(|p| p.variant_group.as_deref()).collect();
        assert_eq!(vgs, ["c1:vg", "c2:vg"].into_iter().collect::<BTreeSet<_>>());
    }

    #[test]
    fn card_content_resolves_across_cards_without_crosstalk() {
        let (container, cards) = two_card_container(refs_json(&["c1", "c2"]));
        let m = compose(container, &cards, 5).unwrap();
        // 解引用走合并后的目录：c1 的钩子只可能解出 c1 的道具。
        let hc1 = m.hidden_content_pool.iter().find(|p| p.id == "c1:hc").unwrap();
        let item = resolve_reward_item(hc1, &m.world_items).expect("解引用成功");
        assert_eq!(item.id, "c1:wi", "跨卡解引用必须落在本卡的道具上");
        // 地点驻留道具同理。
        let groups = distribute_resident_items(&m.locations, &m.world_items);
        let g1 = groups.iter().find(|g| g.location_id == "c1:gate").expect("c1:gate 有驻留道具");
        assert_eq!(g1.items.len(), 1);
        assert_eq!(g1.items[0].id, "c1:wi");
    }

    // ---------------- ⑥ 缝合：合并后地点图连通 ----------------

    #[test]
    fn seams_and_nexus_make_the_location_graph_connected() {
        let (container, cards) = two_card_container(refs_json(&["c1", "c2"]));
        // 合并前：本体孤点 + 两张卡各自一坨 = 3 个分量。
        let mut raw = sk(container_json());
        for c in &cards {
            merge_fragment(&mut raw, c, 5);
        }
        assert_eq!(location_components(&raw.locations).len(), 3, "未缝合时应当是 3 个孤岛");

        let m = compose(container, &cards, 5).unwrap();
        assert_eq!(location_components(&m.locations).len(), 1, "缝合后地点图必须连通");
        let nexus = m.locations.iter().find(|l| l.id == NEXUS_LOCATION_ID).expect("应生成枢纽");
        assert_eq!(nexus.name, NEXUS_DEFAULT_NAME);
        assert!(!nexus.is_secret_realm, "枢纽不得是秘境");
        // 枢纽只接锚点，不接秘境。
        assert!(nexus.connections.iter().all(|c| c == "core-hub" || c == "c1:gate" || c == "c2:gate"));
    }

    #[test]
    fn explicit_seam_links_two_cards_and_pins_endpoints() {
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1", "c2"]);
        c["seams"] = json!([{ "from": "c1:gate", "to": "c2:gate" }]);
        c["nexus"] = json!({ "name": "十字驿站" });
        let cards =
            vec![card("c1", 1, 5, 1.0, frag_json("甲", 1)), card("c2", 1, 5, 1.0, frag_json("乙", 1))];
        let m = compose(sk(c), &cards, 5).unwrap();
        let g1 = m.locations.iter().find(|l| l.id == "c1:gate").unwrap();
        assert!(g1.connections.contains(&"c2:gate".to_string()), "缝合边必须双向落地");
        let g2 = m.locations.iter().find(|l| l.id == "c2:gate").unwrap();
        assert!(g2.connections.contains(&"c1:gate".to_string()));
        assert_eq!(location_components(&m.locations).len(), 1);
        assert_eq!(
            m.locations.iter().find(|l| l.id == NEXUS_LOCATION_ID).map(|l| l.name.as_str()),
            Some("十字驿站"),
            "枢纽名取容器声明"
        );
        let pinned = &m.container.as_ref().unwrap().pinned_locations;
        for want in ["c1:gate", "c2:gate", NEXUS_LOCATION_ID] {
            assert!(pinned.iter().any(|p| p == want), "缝合端点与枢纽必须钉成地点采样必选种子");
        }
    }

    /// 缝合的目的在采样后仍成立：任意实例的被选地点集在合并图上连通。
    #[test]
    fn sampled_locations_stay_connected_in_container_mode() {
        let (container, cards) = two_card_container(refs_json(&["c1", "c2"]));
        let m = compose(container, &cards, 5).unwrap();
        for i in 0..16 {
            let s = plan(&m, &format!("world_conn_{i}"), "cidA", 5).audit.unwrap();
            assert!(
                ids_connected(&m.locations, &s.selected_locations),
                "实例 {i} 的地点子图裂了：{:?}",
                s.selected_locations
            );
            // 秘境保连通语义在跨卡场景同样成立。
            for secret in ["c1:secret", "c2:secret"] {
                if s.selected_locations.iter().any(|l| l == secret) {
                    let inner = secret.replace("secret", "inner");
                    assert!(
                        s.selected_locations.contains(&inner),
                        "秘境 {secret} 入选但通路 {inner} 未入选"
                    );
                }
            }
        }
    }

    // ---------------- ⑦ 平权红线：卡是内容燃料，不带规则、不加战力 ----------------

    /// 🔴 卡带入的道具档位被夹到 `min(容器星级, 卡星级)`：只降不升、effectTags 不变。
    #[test]
    fn card_items_are_capped_by_container_and_card_star() {
        // 5★ 卡的 tier5 道具装进 2★ 容器 → 夹到 2。
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let cards = vec![card("c1", 1, 5, 1.0, frag_json("甲", 5))];
        let m = compose(sk(c.clone()), &cards, 2).unwrap();
        let it = m.world_items.iter().find(|i| i.id == "c1:wi").unwrap();
        assert_eq!(it.origin.power_tier, 2, "容器星级封顶：借高星卡在低星容器刷高档道具必须被堵死");
        assert_eq!(it.effect_tags, vec!["advantage:combat".to_string()], "effectTags 恒不变（防转译后门）");

        // 1★ 卡的 tier5 道具装进 5★ 容器 → 夹到 1（低星卡不因装进高星容器而升档）。
        let low = vec![card("c1", 1, 1, 1.0, frag_json("甲", 5))];
        let m2 = compose(sk(c), &low, 5).unwrap();
        assert_eq!(m2.world_items.iter().find(|i| i.id == "c1:wi").unwrap().origin.power_tier, 1);
    }

    /// 🔴 产出封顶不被绕过：夹档（compose）+ 星级封顶剔除（plan_sampling）双保险，
    /// 内联奖励与 ref 奖励同口径（内联不是后门）。
    #[test]
    fn container_payout_cap_cannot_be_bypassed() {
        let mut frag = frag_json("甲", 5);
        // 再挂一条**内联** tier5 奖励的钩子：内联路径必须同样被夹。
        frag["hiddenContentPool"] = json!([
            { "id": "hc", "arcTags": ["arc"], "themes": ["甲"], "rewardItemRef": "wi" },
            { "id": "hc-inline", "arcTags": ["arc"], "themes": ["甲"], "rewardItem": {
                "id": "inline", "narrative": "内联神器", "effectTags": [],
                "origin": { "worldTemplateId": "src", "cosmology": ["myth"], "powerTier": 5 } } }
        ]);
        frag["storylines"] = json!([
            { "id": "arc", "mainlineNodeIds": ["mn"], "hiddenPoolIds": ["hc", "hc-inline"], "endingIds": ["end"] }
        ]);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let cards = vec![card("c1", 1, 5, 1.0, frag)];
        let m = compose(sk(c), &cards, 2).unwrap();

        for p in &m.hidden_content_pool {
            if let Some(t) = reward_tier(p, &m.world_items) {
                assert!(t <= 2, "钩子 {} 的奖励档位 {t} 越过 2★ 容器封顶", p.id);
            }
        }
        // 采样后仍不越界（星级封顶第二道）。
        for i in 0..8 {
            let s = plan(&m, &format!("world_cap_{i}"), "cidA", 2).audit.unwrap();
            for id in &s.selected_hidden {
                let p = m.hidden_content_pool.iter().find(|p| &p.id == id).unwrap();
                if let Some(t) = reward_tier(p, &m.world_items) {
                    assert!(t <= 2, "入选钩子 {id} 的奖励档位 {t} 越过封顶");
                }
            }
        }
    }

    /// 🔴 卡只贡献「内容」，不贡献规则维度：产出表 / 身份池 / 境界档 / 装配规则 / 采样计数 /
    /// 超集标记一律只认容器本体（卡不得带来产出加成、准入豁免或规则特权）。
    #[test]
    fn cards_never_contribute_rule_dimensions() {
        let mut frag = frag_json("甲", 1);
        frag["payoutTable"] = json!({ "worldlineTiers": [ { "label": "卡自带的产出表", "minScore": 0.0, "mileage": 9999 } ] });
        frag["identityPool"] = json!([ { "id": "card-lead", "quota": 9, "isLead": true } ]);
        // 境界档是**容器发的戏服**（§6）：卡自带一件 = "装上这张卡全场换套更高的水位"，
        // 与产出表同属规则维度的侧门，必须被合并器丢掉。
        frag["realmTier"] = json!({ "id": "card-tier", "label": "卡自带的斗帝档", "cosmology": "cultivation" });
        frag["assemblyRules"] = json!({ "hiddenPerCharacter": 9 });
        frag["sampling"] = json!({ "instanceHiddenCount": 99 });
        frag["isSuperset"] = json!(false);
        frag["subplotCardRefs"] = refs_json(&["c-nested"]); // 递归炸弹：卡里再引用卡。

        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let m = compose(sk(c), &[card("c1", 1, 5, 1.0, frag)], 5).unwrap();

        assert!(m.payout_table.is_none(), "卡不得带来产出表（产出加成 = 加战力的侧门）");
        assert!(m.identity_pool.is_empty(), "卡不得带来身份池（站位与配额是容器的规则维度）");
        assert!(m.realm_tier.is_none(), "卡不得带来境界档（戏服由容器发，§6「境界跟着副本走」）");
        assert_eq!(m.assembly_rules.hidden_per_character, 1, "装配规则只认容器本体");
        assert_eq!(m.sampling.instance_hidden_count, Some(2), "采样计数只认容器本体");
        assert!(m.is_superset, "超集标记只认容器本体");
        assert_eq!(m.subplot_card_refs.len(), 1, "卡内的 subplotCardRefs 不得被继承（禁止递归装卡）");
    }

    // ---------------- ⑧ 建房期拒绝（静态门 + 合并门） ----------------

    /// 建模板前门的纯校验。`container_on` 直接取自 `ContainerSwitch` 所设的 env——
    /// 端点侧真实的取法是 `container_assembly_enabled(&state.db, None).await`（global 档），
    /// 这里在无 DB 的纯函数用例里等价复现它的 env 层。
    fn validate(v: serde_json::Value) -> Result<(), String> {
        let container_on = matches!(
            std::env::var(ENV_CONTAINER_ASSEMBLY).as_deref().map(str::trim),
            Ok("1") | Ok("true") | Ok("on") | Ok("yes")
        );
        validate_skeleton_refs(&v, container_on)
    }

    #[test]
    fn front_door_rejects_container_declaration_while_switch_is_off() {
        let _sw = ContainerSwitch::set(false);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let err = validate(c).unwrap_err();
        assert!(err.contains("MUSE_CONTAINER_ASSEMBLY"), "应报未开放，实得：{err}");
        // 普通模板不受影响。
        assert!(validate(container_json()).is_ok(), "无 subplotCardRefs 的模板不得被本段拦住");
    }

    /// 🔴 **两处 ctx 口径**：装配期按 world、建模板前门只能按 global。
    /// 本用例只写一条 **world** 记录（env 与 global 都关着），验：
    /// ① 装配期按世界解析 ⇒ 那个房命中、别的房不命中；
    /// ② 建模板前门取 global ⇒ 仍然拒绝（世界还不存在，没有 world 可解析）。
    #[tokio::test]
    async fn container_grayscale_is_per_world_on_the_assembly_side_only() {
        let _sw = ContainerSwitch::set(false); // env 层：关
        let db = crate::testkit::test_pool().await;
        sqlx::query(
            "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
             updated_by, updated_at, reason, created_at) \
             VALUES ($1, 'MUSE_CONTAINER_ASSEMBLY', 'world', 'w_open', 1, 0, 0, 'test', $2, 'test', $3)",
        )
        .bind(crate::db::new_id("rf"))
        .bind(crate::db::now_ms())
        .bind(crate::db::now_ms())
        .execute(&db)
        .await
        .unwrap();

        assert!(
            container_assembly_enabled(&db, Some("w_open")).await,
            "🔴 装配期按世界解析：被灰度选中的房应当命中"
        );
        assert!(
            !container_assembly_enabled(&db, Some("w_other")).await,
            "🔴 没被选中的房不得跟着开"
        );
        assert!(
            !container_assembly_enabled(&db, None).await,
            "🔴 建模板前门取 global —— 给某个世界开闸不等于允许新模板声明 subplotCardRefs"
        );

        // 🔴 上面三条只验到函数本身。真正要验的是**装配期那一行传的是 world 还是 global**，
        // 所以再走一次 `load_container_cards`（故障注入实测：只有这一段能抓住把
        // `Some(&world.id)` 写成 `None` 的错误，上面三条对它完全无感）。
        //
        // 判据取「进没进到开关之后那一步」：被灰度选中的房会继续往下走，撞上「容器房缺少房主」
        // 而报错；没被选中的房在开关那一行就返回空。两种结果结构上不同，不会混淆。
        let wid = crate::worlds::create_world(
            &db,
            crate::worlds::CreateWorldParams::official("tpl_ct", 1, "容器房"),
        )
        .await
        .unwrap();
        // 给这个真实世界写一条 world 记录（世界 id 由 create_world 生成，故先建后写）。
        sqlx::query(
            "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
             updated_by, updated_at, reason, created_at) \
             VALUES ($1, 'MUSE_CONTAINER_ASSEMBLY', 'world', $2, 1, 0, 0, 'test', $3, 'test', $4)",
        )
        .bind(crate::db::new_id("rf"))
        .bind(&wid)
        .bind(crate::db::now_ms())
        .bind(crate::db::now_ms())
        .execute(&db)
        .await
        .unwrap();
        let world = crate::worlds::load_world(&db, &wid).await.unwrap();
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let skeleton = sk(c);

        let err = load_container_cards(&db, &world, &skeleton)
            .await
            .expect_err("被灰度选中的房应当走过开关那一行，并撞上「缺少房主」");
        assert!(
            format!("{err:?}").contains("房主"),
            "🔴 走过开关之后应当撞上房主校验；实得 {err:?} —— 若是 Ok(空) 说明装配期读的是 global"
        );
    }

    #[test]
    fn validate_accepts_wellformed_container() {
        let _sw = ContainerSwitch::set(true);
        let mut c = container_json();
        c["subplotCardRefs"] = json!([
            { "cardId": "c1", "cardVersion": 3, "weight": 0.5 },
            { "cardId": "c2" }
        ]);
        c["seams"] = json!([{ "from": "core-hub", "to": "c1:gate" }]);
        c["nexus"] = json!({ "name": "十字驿站" });
        assert!(validate(c.clone()).is_ok(), "合法容器不得被拦：{:?}", validate(c));
    }

    #[test]
    fn validate_rejects_duplicate_card_ref() {
        let _sw = ContainerSwitch::set(true);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1", "c1"]);
        assert!(validate(c).unwrap_err().contains("重复引用"));
    }

    #[test]
    fn validate_rejects_card_id_with_namespace_separator() {
        let _sw = ContainerSwitch::set(true);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1:evil"]);
        assert!(validate(c).unwrap_err().contains("保留分隔符"));
    }

    #[test]
    fn validate_rejects_container_body_id_with_separator() {
        let _sw = ContainerSwitch::set(true);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        c["hiddenContentPool"] = json!([{ "id": "core:hc", "themes": ["x"] }]);
        assert!(validate(c).unwrap_err().contains("容器本体 id"));
    }

    #[test]
    fn validate_rejects_reserved_nexus_id() {
        let _sw = ContainerSwitch::set(true);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        c["locations"] = json!([{ "id": NEXUS_LOCATION_ID }]);
        assert!(validate(c).unwrap_err().contains("保留字"));
    }

    #[test]
    fn validate_rejects_dangling_seam_endpoints() {
        let _sw = ContainerSwitch::set(true);
        // ① 指向未被引用的卡。
        let mut a = container_json();
        a["subplotCardRefs"] = refs_json(&["c1"]);
        a["seams"] = json!([{ "from": "core-hub", "to": "c9:gate" }]);
        assert!(validate(a).unwrap_err().contains("未被 subplotCardRefs 引用"));
        // ② 裸 id 不是本体地点。
        let mut b = container_json();
        b["subplotCardRefs"] = refs_json(&["c1"]);
        b["seams"] = json!([{ "from": "no-such-loc", "to": "c1:gate" }]);
        assert!(validate(b).unwrap_err().contains("不是本容器本体的地点"));
        // ③ 两端相同。
        let mut d = container_json();
        d["subplotCardRefs"] = refs_json(&["c1"]);
        d["seams"] = json!([{ "from": "core-hub", "to": "core-hub" }]);
        assert!(validate(d).unwrap_err().contains("两端相同"));
    }

    #[test]
    fn validate_rejects_bad_anchors() {
        let _sw = ContainerSwitch::set(true);
        let mut a = container_json();
        a["subplotCardRefs"] = refs_json(&["c1"]);
        a["anchors"] = json!(["nope"]);
        assert!(validate(a).unwrap_err().contains("anchors 悬空"));

        let mut b = container_json();
        b["subplotCardRefs"] = refs_json(&["c1"]);
        b["locations"] = json!([{ "id": "core-hub", "isSecretRealm": true }]);
        b["anchors"] = json!(["core-hub"]);
        assert!(validate(b).unwrap_err().contains("秘境"));
    }

    // ===== 气运与机缘（2026-07-29 二次澄清）：**角色自己的**两个数 =====
    /// 只填难度的池项夹具——从 JSON 反序列化，与真实来源同一条路径。
    fn pool_item_with_difficulty(id: &str, difficulty: f32) -> PoolItem {
        serde_json::from_value(serde_json::json!({ "id": id, "difficultyBase": difficulty }))
            .expect("池项应可从 JSON 构造")
    }

    /// 🔴 **零档 ⇒ 两处恒等 ⇒ 无烙印的卡拿到的钩子与本层落地前一模一样。**
    ///
    /// 这是它能作为纯增量上线的全部依据：全新库、全新卡都是零档。
    #[test]
    fn a_card_with_no_past_gets_exactly_what_it_got_before() {
        let _env = FortuneEnv::set(&[]);
        // 机缘零档：配额不动。
        for base in [1usize, 2, 5] {
            assert_eq!(personal_hook_quota(base, 0), base, "零档机缘改动了配额");
        }
        // 气运零档：排序加成恒 0 ⇒ 排序键退化为纯命中数。
        for d in [0.0_f32, 0.2, 0.35, 0.5, 1.0] {
            assert_eq!(fortune_bonus(d, 0.35, 0.15, 0), 0.0, "零档气运给出了非零加成（difficulty={d}）");
        }
    }

    /// 机缘：满档时**多分到**钩子，且单调不减。
    ///
    /// ⚠️ 为什么是「额外几个」而不是乘数：`hidden_per_character` 默认是 **1**，
    /// 1 × 1.2 四舍五入回 1——乘数在这个基数上等于没有，且**没有任何症状**。
    /// 这条用例就是钉住那件事：基数 1 时满档也必须真的多出来。
    #[test]
    fn opportunity_actually_adds_hooks_even_on_a_base_of_one() {
        let _env = FortuneEnv::set(&[]);
        let max = crate::imprint::SWING_MAX_LEVEL;
        assert!(personal_hook_quota(1, max) > 1, "🔴 基数 1 时满档机缘没有多出任何钩子——乘数陷阱");
        // 单调不减。
        let mut prev = 0usize;
        for level in 0..=max {
            let q = personal_hook_quota(1, level);
            assert!(q >= prev, "配额随档位下降了：{level} 档 → {q}");
            prev = q;
        }
        assert_eq!(personal_hook_quota(1, max), 1 + OPPORTUNITY_EXTRA_HOOKS as usize);
    }

    /// 🔴 **气运两极化，两个方向一起抬。**
    ///
    /// 这一条是它不违宪的关键：极温和的和极凶险的**一起**被排到前面。
    /// 若只抬高难度那一半，「高气运」就等于「更好的东西」，当场变成养成优势。
    #[test]
    fn fortune_lifts_both_extremes_never_only_the_good_half() {
        let _env = FortuneEnv::set(&[]);
        let (mean, dev) = (0.3_f32, 0.3_f32); // 这批候选难度铺在 0.0–0.6
        let max = crate::imprint::SWING_MAX_LEVEL;
        let calm = fortune_bonus(mean, mean, dev, max); // 正中常态
        let easy = fortune_bonus(0.0, mean, dev, max); // 温和的那一极
        let hard = fortune_bonus(0.6, mean, dev, max); // 凶险的那一极
        assert!(easy > calm, "温和的一端必须被抬起来");
        assert!(hard > calm, "凶险的一端必须被抬起来");
        assert!((easy - hard).abs() < 1e-6, "🔴 两端必须被抬得一样多——否则气运就是在偏袒某一边");
    }

    /// 常态取的是**候选集自己的均值**，不是写死的 0.5。
    ///
    /// 真实数据里 `difficultyBase` 落在 0.2–0.5，若把 0.5 当中心，
    /// 「凶险的那一极」在数据上根本不存在，两极化就只剩一边。
    #[test]
    fn the_baseline_is_this_pools_own_average_not_a_hardcoded_half() {
        let items = [
            pool_item_with_difficulty("a", 0.2),
            pool_item_with_difficulty("b", 0.3),
            pool_item_with_difficulty("c", 0.4),
        ];
        let (mean, dev) = difficulty_center(&items);
        assert!((mean - 0.3).abs() < 1e-6, "常态应是 0.3（这批的均值），实得 {mean}");
        assert!((dev - 0.1).abs() < 1e-6, "最大偏离应是 0.1，实得 {dev}");
    }

    /// 全体难度相同 ⇒ 无「极端」可言 ⇒ 气运无处着力，加成恒 0。
    #[test]
    fn a_pool_of_uniform_difficulty_gives_fortune_nothing_to_grip() {
        let items = [pool_item_with_difficulty("a", 0.4), pool_item_with_difficulty("b", 0.4)];
        let (mean, dev) = difficulty_center(&items);
        assert_eq!(dev, 0.0);
        assert_eq!(fortune_bonus(0.4, mean, dev, crate::imprint::SWING_MAX_LEVEL), 0.0);
    }

    /// 🔴 **气运不碰任何上限。**
    ///
    /// 稀有预算（`RARE_BUDGET`）、星级封顶都在**世界**这一层、在钩子分发**之前**执行；
    /// 气运只决定这个角色**先轮到哪些**。源码级钉住：两个函数体里不得出现那几个常量。
    #[test]
    fn fortune_never_touches_the_rare_budget_or_the_tier_cap() {
        let src = include_str!("mod.rs");
        for name in ["fn fortune_bonus", "fn personal_hook_quota"] {
            let start = src.find(name).unwrap_or_else(|| panic!("{name} 应存在"));
            // ⚠️ 边界用「下一条文档注释」而**不是** `\n` + 右花括号 + `\n`：
            // 后者在源码里是一个真实的右花括号字符，会把 `testkit::production_sources()`
            // 的花括号配平带歪 —— 那会让**别人的**红线误判（本仓已被咬过两次，见 VALIDATION §3.65）。
            let end = src[start..].find("\n/// ").map(|i| start + i).unwrap_or(src.len());
            let body = &src[start..end];
            for forbidden in ["RARE_BUDGET", "RARE_TIER", "star_rating"] {
                assert!(!body.contains(forbidden), "🔴 {name} 触碰了 {forbidden}");
            }
        }
    }

    /// 🔴 **气运改排序，机缘改配额——两个数各管一头，不重叠。**
    ///
    /// 重叠会让「气运高 = 拿得更多」，那正是产品原话里没有、而红线最怕的那个形态。
    #[test]
    fn fortune_changes_the_order_while_opportunity_changes_the_count() {
        let _env = FortuneEnv::set(&[]);
        let max = crate::imprint::SWING_MAX_LEVEL;
        // 气运不改配额。
        assert_eq!(personal_hook_quota(2, 0), personal_hook_quota(2, 0), "配额只该由机缘决定");
        // 源码级：配额函数里不得出现 fortune。
        let src = include_str!("mod.rs");
        let start = src.find("fn personal_hook_quota").expect("配额函数应存在");
        // 边界同上：不用带花括号的字面量。
        let end = src[start..].find("\n/// ").map(|i| start + i).unwrap_or(src.len());
        assert!(!src[start..end].contains("fortune"), "🔴 气运渗进了配额——它只该改排序");
        // 反向：气运确实改变了排序（同一批候选、同一份执念，只变档位）。
        let pool: Vec<PoolItem> = (0..6)
            .map(|i| pool_item_with_difficulty(&format!("hp-{i}"), 0.2 + 0.06 * i as f32))
            .collect();
        let ids = |lv: i64| -> Vec<String> {
            rank_pool_items(&pool, &[], lv).into_iter().map(|(p, _, _)| p.id.clone()).collect()
        };
        assert_ne!(ids(0), ids(max), "🔴 气运从零档到满档，候选顺序却没变——它没有生效");
    }

    // ---------- 点数与档位：产品要的「量化显示」+「很难增加」落在这里 ----------

    /// 🔴 **「很难增加」不是一句形容词，是这道几何阶梯。**
    #[test]
    fn the_ladder_doubles_every_step_so_the_numbers_climb_slowly() {
        use crate::imprint::{swing_level, swing_threshold, SWING_MAX_LEVEL};
        let steps: Vec<i64> = (1..=SWING_MAX_LEVEL).map(swing_threshold).collect();
        assert_eq!(steps, vec![4, 12, 28, 60, 124], "阶梯被改动过——「很难增加」这条产品约束在这里");
        let mut prev_gap = steps[0];
        for w in steps.windows(2) {
            let gap = w[1] - w[0];
            assert!(gap > prev_gap, "第 {}→{} 档的增量没有变大", w[0], w[1]);
            prev_gap = gap;
        }
        assert_eq!(swing_level(3), 0);
        assert_eq!(swing_level(4), 1);
        assert_eq!(swing_level(123), 4);
        assert_eq!(swing_level(124), 5);
    }

    /// 🔴 **档位封顶：老卡最终都撞到同一个顶，不会无限拉开。**
    ///
    /// 改成角色级之后这条更重要了——世界级时它是「世界更极端」，角色级时它是
    /// 「这个人分到更多」，而任何**无上限**的个人刻度都会被当成战力表。
    #[test]
    fn levels_are_capped_so_old_cards_stop_pulling_away() {
        use crate::imprint::{swing_level, SWING_MAX_LEVEL};
        for points in [124, 500, 10_000, i64::MAX / 2] {
            assert_eq!(swing_level(points), SWING_MAX_LEVEL, "点数 {points} 越过了封顶");
        }
    }

    /// 每一类烙印必须**明确**落在某一个方向上，或者明确不计点（fail-closed）。
    #[test]
    fn every_imprint_kind_lands_on_exactly_one_axis() {
        use crate::imprint::{no_swing_grants, swing_points_by_card};
        let rows: Vec<(String, String, String)> =
            ["choice", "witness", "bond", "circumstance", "unfinished"]
                .iter()
                .map(|k| ("c1".to_string(), k.to_string(), "x".to_string()))
                .collect();
        let s = swing_points_by_card(&rows, &no_swing_grants())["c1"];
        assert_eq!(s.opportunity, 3, "choice/witness/bond 应计机缘");
        assert_eq!(s.fortune, 2, "circumstance/unfinished 应计气运");
        let unknown = vec![("c1".into(), "某个将来才有的类目".to_string(), "x".into())];
        assert_eq!(swing_points_by_card(&unknown, &no_swing_grants())["c1"], Default::default());
    }

    /// 同一条烙印进种子只算一次（指纹会 dedup），但**计点不去重**：
    /// 一张卡在三个世界里都「走到了终局」是三段经历，不是一段。
    #[test]
    fn living_the_same_thing_three_times_counts_three_times() {
        use crate::imprint::{no_swing_grants, swing_points_by_card};
        let rows: Vec<(String, String, String)> = (0..3)
            .map(|_| ("c1".into(), "circumstance".into(), "walked_to_the_end".into()))
            .collect();
        assert_eq!(swing_points_by_card(&rows, &no_swing_grants())["c1"].fortune, 3);
    }

    /// 🔴 **一张卡的档位只由它自己的经历决定，同房别人一个字都改不了。**
    ///
    /// 这条是「对自己有影响」的正面表述，也是上一版做成世界平均时**恰恰不成立**的那条：
    /// 那时多拉几个新人进来就能把老手的世界拉回常态。
    #[test]
    fn a_cards_levels_depend_on_its_own_past_and_nobody_elses() {
        use crate::imprint::{card_swing_levels, no_swing_grants};
        let veteran: Vec<(String, String, String)> =
            (0..124).map(|_| ("vet".into(), "circumstance".into(), "x".into())).collect();
        let solo = card_swing_levels(&veteran, &["vet".to_string()], &no_swing_grants());
        let crowd_roster: Vec<String> =
            ["vet", "n1", "n2", "n3", "n4"].iter().map(|s| s.to_string()).collect();
        let crowd = card_swing_levels(&veteran, &crowd_roster, &no_swing_grants());
        assert_eq!(solo["vet"], crowd["vet"], "🔴 同房多了四个新人，老手的档位变了");
        assert_eq!(solo["vet"].0, crate::imprint::SWING_MAX_LEVEL, "满点应是满档");
        // 新人各自零档，且**都在表里**（调用方不必区分「没这张卡」和「零档」）。
        for n in ["n1", "n2", "n3", "n4"] {
            assert_eq!(crowd[n], (0, 0), "新卡应是零档");
        }
    }

    // ---------- 🔮 道具加值：预留接口的两条红线 ----------

    /// 🔴 **道具能加速到顶，不能突破顶。**
    ///
    /// 授予的点数与经历得来的点数走**同一道阶梯**，因此天量授予也只能到满档——
    /// 买到的是**更快到达**，到达之后与一个跑了六十局的人完全一样。
    #[test]
    fn granted_points_cannot_break_the_ceiling() {
        use crate::imprint::{card_swing_levels, Swing, SwingGrants, SWING_MAX_LEVEL};
        let mut grants = SwingGrants::new();
        grants.insert("buyer".to_string(), Swing { fortune: i64::MAX / 4, opportunity: i64::MAX / 4 });
        let lv = card_swing_levels(&[], &["buyer".to_string()], &grants);
        assert_eq!(lv["buyer"], (SWING_MAX_LEVEL, SWING_MAX_LEVEL), "🔴 道具把封顶顶穿了：{lv:?}");
    }

    /// 🔴 **给一张不在场的卡加点，不会凭空造出一个条目。**
    ///
    /// `grants` 是外部给的表。入参是名册本身而不是它的长度，于是不在场的卡结构上进不来。
    /// ⚠️ 这个洞今天不存在（生产恒传空授予），**预留接口的意义就是将来接线时它已经是安全的**。
    #[test]
    fn granting_points_to_an_absent_card_changes_nothing() {
        use crate::imprint::{card_swing_levels, Swing, SwingGrants};
        let roster = vec!["here".to_string()];
        let base = card_swing_levels(&[], &roster, &SwingGrants::new());
        let mut grants = SwingGrants::new();
        grants.insert("not_in_this_room".to_string(), Swing { fortune: 999, opportunity: 999 });
        let got = card_swing_levels(&[], &roster, &grants);
        assert_eq!(got, base, "🔴 不在场的卡影响了这个房间");
        assert!(!got.contains_key("not_in_this_room"), "🔴 不在场的卡出现在了结果里");
    }

    /// 🔴 **本轮没有任何生产路径给出非零授予**（未验证功能默认关闭，平台三约束第 1 条）。
    ///
    /// 扫描面走 `testkit::production_sources()`（**全仓**，花括号配平剥测试模块），不是手工列文件——
    /// 第一版手工列 + 土法剥离器，把扫描面悄悄砍成了半个文件，红线绿着但什么都没扫到。
    /// 🔵 抓住它的是最后那句 `calls > 0`：**红线必须扫到过东西**（VALIDATION §3.64 ⑥）。
    #[test]
    fn no_production_path_grants_swing_points_yet() {
        let mut calls = 0;
        for (path, src) in crate::testkit::production_sources() {
            for (i, _) in src.match_indices("card_swing_levels(") {
                if src[..i].ends_with("fn ") {
                    continue; // 函数定义本身，不是调用
                }
                let stmt = &src[i..][..src[i..].find(';').unwrap_or(src.len() - i)];
                assert!(
                    stmt.contains("no_swing_grants()"),
                    "🔴 {path} 有一处 card_swing_levels 传了非空授予——道具接口本轮只预留不接线：\n{stmt}"
                );
                calls += 1;
            }
            assert!(!src.contains("grants.insert"), "🔴 {path} 的生产代码在给某张卡加点");
        }
        assert!(calls > 0, "全仓一处生产调用都没扫到——这条红线失去了守护对象");
    }

    #[test]
    fn validate_rejects_negative_weight() {
        let _sw = ContainerSwitch::set(true);
        let mut c = container_json();
        c["subplotCardRefs"] = json!([{ "cardId": "c1", "weight": -1.0 }]);
        assert!(validate(c).unwrap_err().contains("weight 非法"));
    }

    /// 🔴 缝合口落在**没进 anchors 白名单**的本体地点上，必须在**建模板期**就拒绝。
    ///
    /// 这条 2026-07-29 补。此前建模板期只查「裸端点是不是本体地点」，
    /// 而 `compose_container_skeleton` 步骤 4 查的是「每个端点都得在白名单内」——
    /// 于是这种模板**发布时一路绿灯**，直到有玩家去开房才 400「seams 越界」。
    /// 错误被推给了撞上它的玩家，而写错的作者那边显示「发布成功」。
    ///
    /// 🔵 为什么裸端点这一半能在建模板期判死：合并后的白名单 =
    /// 本体 anchors ∪ 各卡 anchors（**命名空间化后恒带 `卡id:` 前缀**），
    /// 裸 id 不带前缀 ⇒ 只可能由本体 anchors 放行 ⇒ 卡解不解引用都不改变结论。
    /// 带前缀那一半仍归 compose，这条边界没动。
    #[test]
    fn validate_rejects_seam_on_a_body_location_outside_the_anchor_whitelist() {
        let _sw = ContainerSwitch::set(true);
        let two_locations = json!([
            { "id": "core-hub",  "connections": [] },
            { "id": "side-yard", "connections": [] }
        ]);

        // 缺陷形态：side-yard 是本体地点，但不在 anchors 里。
        let mut bad = container_json();
        bad["subplotCardRefs"] = refs_json(&["c1"]);
        bad["locations"] = two_locations.clone();
        bad["anchors"] = json!(["core-hub"]);
        bad["seams"] = json!([{ "from": "side-yard", "to": "c1:gate" }]);
        let err = validate(bad).unwrap_err();
        assert!(err.contains("seams 越界"), "实得：{err}");

        // 反向配对：把 side-yard 补进 anchors，同一份骨架必须照常放行——
        // 否则这条闸就是「把功能关掉」而不是「把缺陷挡掉」。
        let mut good = container_json();
        good["subplotCardRefs"] = refs_json(&["c1"]);
        good["locations"] = two_locations;
        good["anchors"] = json!(["core-hub", "side-yard"]);
        good["seams"] = json!([{ "from": "side-yard", "to": "c1:gate" }]);
        assert!(validate(good).is_ok(), "补进白名单后不该再拦");
    }

    /// 两道门口径一致：**建模板期放行的裸端点，compose 不得再以「越界」拒绝**。
    ///
    /// 这是上一条的**接缝断言**——单独看两边各自的用例都绿，漂开的恰恰是它们之间那条缝。
    #[test]
    fn the_two_gates_agree_on_bare_seam_endpoints() {
        let _sw = ContainerSwitch::set(true);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        c["seams"] = json!([{ "from": "core-hub", "to": "c1:gate" }]);

        // 建模板期放行（core-hub 在 container_json 的 anchors 里）。
        assert!(validate(c.clone()).is_ok(), "建模板期本应放行");

        // 同一份骨架进 compose，卡片段的 gate 也在卡自己的 anchors 里 ⇒ 不该出现「越界」。
        let composed = compose(sk(c), &[card("c1", 1, 5, 1.0, frag_json("甲", 1))], 5);
        match composed {
            Ok(_) => {}
            Err(e) => assert!(
                !e.contains("seams 越界"),
                "建模板期放行了、compose 却报越界——两道门的口径又漂开了：{e}"
            ),
        }
    }

    #[test]
    fn compose_rejects_card_internal_id_with_separator() {
        let mut frag = frag_json("甲", 1);
        frag["hiddenContentPool"] = json!([{ "id": "evil:hc", "arcTags": ["arc"] }]);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let err = compose(sk(c), &[card("c1", 1, 5, 1.0, frag)], 5).unwrap_err();
        assert!(err.contains("保留分隔符"), "实得：{err}");
    }

    #[test]
    fn compose_rejects_dangling_reference_inside_card() {
        // 卡内 connections 指向卡外地点（跨卡连接只能经 seams）。
        let mut frag = frag_json("甲", 1);
        frag["locations"] = json!([{ "id": "gate", "connections": ["somewhere-else"] }]);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let err = compose(sk(c), &[card("c1", 1, 5, 1.0, frag)], 5).unwrap_err();
        assert!(err.contains("引用悬空"), "实得：{err}");
    }

    #[test]
    fn compose_rejects_seam_outside_anchor_whitelist() {
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1", "c2"]);
        // c1:inner 不在 c1 的 anchors 白名单里（白名单只有 gate）。
        c["seams"] = json!([{ "from": "core-hub", "to": "c1:inner" }]);
        let cards =
            vec![card("c1", 1, 5, 1.0, frag_json("甲", 1)), card("c2", 1, 5, 1.0, frag_json("乙", 1))];
        let err = compose(sk(c), &cards, 5).unwrap_err();
        assert!(err.contains("anchors 白名单"), "实得：{err}");
    }

    #[test]
    fn compose_rejects_seam_into_secret_realm() {
        let mut frag = frag_json("甲", 1);
        frag["anchors"] = json!(["secret"]); // 故意把秘境声明成锚点。
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        c["seams"] = json!([{ "from": "core-hub", "to": "c1:secret" }]);
        let err = compose(sk(c), &[card("c1", 1, 5, 1.0, frag)], 5).unwrap_err();
        assert!(err.contains("秘境"), "实得：{err}");
    }

    #[test]
    fn compose_rejects_incompatible_cosmology() {
        // 容器 allowlist 只收 cultivation，卡内道具是 myth → 不相容，建房期拒绝。
        let policy: crate::admission::WorldAdmissionPolicy = serde_json::from_value(json!({
            "mode": "allowlist", "cosmologies": ["cultivation"]
        }))
        .unwrap();
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let err = compose_container_skeleton(
            sk(c),
            &[card("c1", 1, 5, 1.0, frag_json("甲", 1))],
            5,
            &policy,
        )
        .unwrap_err();
        assert!(err.contains("cosmology 不相容"), "实得：{err}");
    }

    #[test]
    fn compose_rejects_component_without_any_seam_anchor() {
        // 卡只有秘境地点 → 无合法缝合口 → 建房期拒绝（不留成运行时的孤岛）。
        let mut frag = frag_json("甲", 1);
        frag["locations"] = json!([{ "id": "gate", "isSecretRealm": true, "connections": [] }]);
        frag["anchors"] = json!([]);
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["c1"]);
        let err = compose(sk(c), &[card("c1", 1, 5, 1.0, frag)], 5).unwrap_err();
        assert!(err.contains("无合法缝合口"), "实得：{err}");
    }

    #[test]
    fn assert_unique_ids_catches_real_collision() {
        // 直接构造撞车骨架：命名空间逻辑若被改坏，必须在建房期炸出来。
        let bad = sk(json!({
            "hiddenContentPool": [ { "id": "dup" }, { "id": "dup" } ]
        }));
        assert!(assert_unique_ids(&bad).unwrap_err().contains("id 冲突"));
    }

    // ---------------- ⑨ DB 全链路：默认关闭字节不变 / 装配不消耗卡 ----------------

    async fn seed_tpl(db: &AnyPool, id: &str, skeleton: &serde_json::Value, star: i64) {
        sqlx::query(
            "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, \
             official, version, moderation, star_rating, created_at) \
             VALUES ($1, '容器模板', 'chapter', $2, '{\"mode\":\"open\"}', 1, 1, 'approved', $3, $4)",
        )
        .bind(id)
        .bind(skeleton.to_string())
        .bind(star)
        .bind(now_ms())
        .execute(db)
        .await
        .expect("seed template");
    }

    async fn seed_world_for(db: &AnyPool, world_id: &str, tpl: &str, host: &str) {
        sqlx::query(
            "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
             model_route_version, room_type, title, status, visibility, host_user_id, member_limit, \
             tick_per_day, state_revision, narrative_state_json, created_at, updated_at) \
             VALUES ($1, $2, 1, 'e1', 'p1', 'm1', 'chapter', '容器房', 'running', 'private', $3, 10, 3, 0, '{}', $4, $5)",
        )
        .bind(world_id)
        .bind(tpl)
        .bind(host)
        .bind(now_ms())
        .bind(now_ms())
        .execute(db)
        .await
        .expect("seed world");
    }

    async fn seed_subplot_card(db: &AnyPool, id: &str, owner: &str, star: i64, src_tpl: &str, ver: i64) {
        sqlx::query(
            "INSERT INTO subplot_cards (id, owner_id, star_rating, label, origin_kind, grant_key, \
             source_world_id, source_template_id, source_template_version, synthesized_from_json, \
             status, acquired_at) \
             VALUES ($1, $2, $3, '剧情结晶', 'settlement', $4, 'w_src', $5, $6, '[]', 'owned', $7)",
        )
        .bind(id)
        .bind(owner)
        .bind(star)
        .bind(format!("settlement:{id}"))
        .bind(src_tpl)
        .bind(ver)
        .bind(now_ms())
        .execute(db)
        .await
        .expect("seed subplot card");
    }

    /// 清掉已钉住的装配后重装（C-7 CAS 只允许首次写入）。
    async fn reassemble(state: &AppState, world_id: &str) -> String {
        sqlx::query("UPDATE worlds SET assembled_json = NULL WHERE id = $1")
            .bind(world_id)
            .execute(&state.db)
            .await
            .unwrap();
        let a = assemble_instance(state, world_id).await.expect("装配成功");
        serde_json::to_string(&a).expect("序列化装配产物")
    }

    /// 🔴 **最重要的回归保护**：开关默认关闭时，声明了 subplotCardRefs 的模板与
    /// 完全没有这些字段的模板，装配产物必须**逐字节相同**（同一 world_id / 同一阵容 / 同一版本）。
    /// 这同时证明了「关闭时走三段式原公式」——种子若变，采样与产物必变。
    #[tokio::test]
    async fn switch_off_keeps_assembly_byte_identical() {
        let state = crate::safety::testkit::test_state().await;
        let _sw = ContainerSwitch::set(false);

        let mut with_refs = container_json();
        with_refs["subplotCardRefs"] = refs_json(&["c1", "c2"]);
        with_refs["seams"] = json!([{ "from": "core-hub", "to": "c1:gate" }]);
        with_refs["nexus"] = json!({ "name": "十字驿站" });

        seed_tpl(&state.db, "tpl_off", &with_refs, 5).await;
        seed_world_for(&state.db, "w_off", "tpl_off", "u_host").await;
        let with_container = reassemble(&state, "w_off").await;

        // 同一个世界、同一模板 id，只把 skeleton 换成"没有容器字段"的版本 → 产物必须逐字节相同。
        sqlx::query("UPDATE world_templates SET skeleton_json = $1 WHERE id = 'tpl_off'")
            .bind(container_json().to_string())
            .execute(&state.db)
            .await
            .unwrap();
        let without_container = reassemble(&state, "w_off").await;

        assert_eq!(
            with_container, without_container,
            "开关关闭时容器声明必须完全无副作用（装配产物逐字节不变）"
        );
        assert!(!with_container.contains("cardSetFingerprint"), "关闭时不得写入卡集合审计段");
        assert!(!with_container.contains(NEXUS_LOCATION_ID), "关闭时不得生成枢纽地点");

        // 关闭时也绝不写引用表。
        let n = crate::safety::testkit::count(
            &state.db,
            "SELECT COUNT(*) FROM world_container_cards WHERE world_id = 'w_off'",
        )
        .await;
        assert_eq!(n, 0, "开关关闭时不得记录任何装卡引用");
    }

    /// 🔴 **装配不消耗卡**（§10【拍板 11】永久蓝图："装入自定义房，房散卡在"）：
    /// 装配后卡仍是 `owned`、`consumed_into` 仍为空；只在 `world_container_cards` 多出引用行。
    #[tokio::test]
    async fn assembly_never_consumes_cards() {
        let state = crate::safety::testkit::test_state().await;
        let _sw = ContainerSwitch::set(true);

        seed_tpl(&state.db, "tpl_src1", &frag_json("甲", 1), 5).await;
        seed_tpl(&state.db, "tpl_src2", &frag_json("乙", 1), 5).await;
        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["card_a", "card_b"]);
        seed_tpl(&state.db, "tpl_box", &c, 5).await;
        seed_subplot_card(&state.db, "card_a", "u_host", 5, "tpl_src1", 1).await;
        seed_subplot_card(&state.db, "card_b", "u_host", 5, "tpl_src2", 1).await;
        seed_world_for(&state.db, "w_box", "tpl_box", "u_host").await;

        let first = reassemble(&state, "w_box").await;
        assert!(first.contains("cardSetFingerprint"), "容器形态必须写入卡集合审计段");
        assert!(first.contains("card_a"), "审计段应含被装入的卡");

        // 卡仍在手，一个字节都没被动过。
        for id in ["card_a", "card_b"] {
            let row = sqlx::query("SELECT status, consumed_into FROM subplot_cards WHERE id = $1")
                .bind(id)
                .fetch_one(&state.db)
                .await
                .unwrap();
            let status: String = row.try_get("status").unwrap();
            let consumed: Option<String> = row.try_get("consumed_into").unwrap();
            assert_eq!(status, "owned", "装配消耗了卡 {id} —— 违反「永久蓝图」红线");
            assert!(consumed.is_none(), "装配给卡 {id} 写了合成血缘");
        }
        // 引用表记账：两张卡两行。
        assert_eq!(
            crate::safety::testkit::count(
                &state.db,
                "SELECT COUNT(*) FROM world_container_cards WHERE world_id = 'w_box'"
            )
            .await,
            2
        );

        // 重复装配幂等：不重复记账、卡仍不被消耗。
        let second = reassemble(&state, "w_box").await;
        assert_eq!(first, second, "同一 (world, 阵容, 版本, 卡集合) 必须得到同一份装配");
        assert_eq!(
            crate::safety::testkit::count(
                &state.db,
                "SELECT COUNT(*) FROM world_container_cards WHERE world_id = 'w_box'"
            )
            .await,
            2,
            "重复装配不得重复记账"
        );

        // 同一张卡可以同时装进另一个房（蓝图可复制，资产不转移）。
        seed_world_for(&state.db, "w_box2", "tpl_box", "u_host").await;
        assemble_instance(&state, "w_box2").await.expect("第二个房照样能装同一批卡");
        assert_eq!(
            crate::safety::testkit::count(
                &state.db,
                "SELECT COUNT(*) FROM world_container_cards WHERE card_id = 'card_a'"
            )
            .await,
            2,
            "一卡多房是正常形态（房散卡在）"
        );
    }

    /// 别人的卡 / 已熔的卡 / 无蓝图的卡 / 版本对不上 → 建房期拒绝（400），不静默跳过。
    #[tokio::test]
    async fn container_rejects_unusable_cards_at_assembly() {
        let state = crate::safety::testkit::test_state().await;
        let _sw = ContainerSwitch::set(true);
        seed_tpl(&state.db, "tpl_src", &frag_json("甲", 1), 5).await;

        let mut c = container_json();
        c["subplotCardRefs"] = refs_json(&["card_x"]);
        seed_tpl(&state.db, "tpl_reject", &c, 5).await;

        // ① 卡不存在。
        seed_world_for(&state.db, "w_r1", "tpl_reject", "u_host").await;
        assert!(assemble_instance(&state, "w_r1").await.is_err(), "引用不存在的卡必须拒绝装配");

        // ② 卡属于别人（无交易红线：只能装自己的卡）。
        seed_subplot_card(&state.db, "card_x", "u_other", 5, "tpl_src", 1).await;
        assert!(assemble_instance(&state, "w_r1").await.is_err(), "别人的卡不得被装入");

        // ③ 已作为合成材料销毁的卡。
        sqlx::query("UPDATE subplot_cards SET owner_id = 'u_host', status = 'consumed' WHERE id = 'card_x'")
            .execute(&state.db)
            .await
            .unwrap();
        assert!(assemble_instance(&state, "w_r1").await.is_err(), "已熔的卡不得被装入");

        // ④ 版本钉住对不上。
        sqlx::query("UPDATE subplot_cards SET status = 'owned' WHERE id = 'card_x'")
            .execute(&state.db)
            .await
            .unwrap();
        let mut c2 = container_json();
        c2["subplotCardRefs"] = json!([{ "cardId": "card_x", "cardVersion": 7 }]);
        sqlx::query("UPDATE world_templates SET skeleton_json = $1 WHERE id = 'tpl_reject'")
            .bind(c2.to_string())
            .execute(&state.db)
            .await
            .unwrap();
        assert!(assemble_instance(&state, "w_r1").await.is_err(), "卡版本对不上必须拒绝（版本钉住）");

        // ⑤ 来源蓝图被下架 → 停止后续建房。
        sqlx::query("UPDATE world_templates SET skeleton_json = $1 WHERE id = 'tpl_reject'")
            .bind(c.to_string())
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE world_templates SET withdrawn = 1 WHERE id = 'tpl_src'")
            .execute(&state.db)
            .await
            .unwrap();
        assert!(assemble_instance(&state, "w_r1").await.is_err(), "蓝图下架后不得再装入");

        // 全程一张引用行都不该落。
        assert_eq!(
            crate::safety::testkit::count(&state.db, "SELECT COUNT(*) FROM world_container_cards").await,
            0,
            "被拒的装配不得留下记账"
        );
    }

    /// 🔴 源码级断言：装配层**永不改写副本卡资产**。
    /// 唯一的销毁语义（合成）归 `subplot/` 独占；装配只在 `world_container_cards` 记引用。
    #[test]
    fn red_line_assembly_never_writes_subplot_cards() {
        // ⚠️ include_str! 会把本测试自己读进来，故按 test-only 编译属性的首次出现截断，
        //    只扫生产代码段（体例同 member_order_tests::order_by_clause_pins_secondary_key）。
        let cut_marker = concat!("#[cfg", "(test)]");
        let src = include_str!("mod.rs");
        let src = src.split(cut_marker).next().unwrap_or(src).to_ascii_uppercase();
        for stmt in ["UPDATE SUBPLOT_CARDS", "DELETE FROM SUBPLOT_CARDS", "INSERT INTO SUBPLOT_CARDS"] {
            assert!(
                !src.contains(stmt),
                "装配层出现 `{stmt}`：副本卡是**永久蓝图**（装入自定义房，房散卡在），\n\
                 装配只许在 world_container_cards 记一行引用；资产的写入路径归 subplot/ 独占（§0.2）"
            );
        }
        // 只读一次卡（解引用蓝图）是允许的，且必须是 SELECT。
        assert!(
            src.contains("FROM SUBPLOT_CARDS WHERE ID = $1"),
            "装配层应当只以 SELECT 方式解引用副本卡（若本断言失败，多半是扫描截断点被前移了）"
        );
    }
}

/// `load_active_cards` 的定序回归。
///
/// 背景：本查询原先只按 `joined_at ASC` 排序。`joined_at` 是毫秒时间戳，两名成员并发 join
/// 撞同一毫秒时行序就由 DB 决定，装配产物（`per_character_hooks` 等按成员序生成的数组）
/// 会在两次重放间漂移，破坏「同一实例可 replay」的确定性契约。
/// 该问题由黄金世界回归（`runtime/golden.rs`）首次运行时抓到——它当时用固定错开的
/// `joined_at` 绕开，所以撞毫秒这条路径没有回归保护，本模块补上。
///
/// ⚠️ **为什么必须有源码级断言**：下面两个行为测试在 dev 的 SQLite 上**证伪不了这个 bug**——
/// 该查询会走 `world_members` 的 `UNIQUE(world_id, cloud_character_id)` 索引，扫描序恰好就是
/// cid 序，所以即便删掉次级键、行为测试照样全绿（实测验证过）。而 Postgres 无此保证，
/// 生产上问题是真实的。行为测试只能作为**口径文档**，真正防回归的是 `order_by_clause_pins_secondary_key`。
/// 这条经验适用于所有「靠 DB 返回序」的断言：**在一种引擎上碰巧通过，不等于契约成立**。
#[cfg(test)]
mod member_order_tests {
    use super::*;
    use crate::safety::testkit::test_state;

    /// 造一张可反序列化的 CharacterCardV2。
    /// 注意：`load_active_cards` 对解析失败是**静默跳过**（`if let Ok(card)`），
    /// 手写简化 JSON 会让成员凭空消失、测试假绿——必须用结构体构造后序列化。
    async fn seed_char(db: &AnyPool, id: &str) {
        use muse_engine::character::types::{CardLifecycle, Identity};
        let card = serde_json::to_string(&CharacterCardV2 {
            schema_version: 2,
            id: id.into(),
            lifecycle: CardLifecycle::Ready,
            identity: Identity { name: id.into(), ..Default::default() },
            dramatic_core: Default::default(),
            decision_model: Default::default(),
            perception: Default::default(),
            emotion_dynamics: Default::default(),
            relation_grammar: Default::default(),
            expression_fingerprint: Default::default(),
            agency: Default::default(),
            growth_arc: Default::default(),
            world_adaptation: Default::default(),
            evidence_index: Default::default(),
            revision: 1,
            created_at: 0,
            updated_at: 0,
        })
        .expect("card json");
        sqlx::query(
            "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
             rights_declaration, moderation, withdrawn, created_at) \
             VALUES ($1, 'u1', 'local', 1, $2, 'original', 'approved', 0, 0)",
        )
        .bind(id)
        .bind(card)
        .execute(db)
        .await
        .expect("seed char");
    }

    /// 同一毫秒的成员，**无论插入顺序如何**，都必须按 cloud_character_id 稳定定序。
    #[tokio::test]
    async fn same_joined_at_orders_by_character_id_regardless_of_insert_order() {
        let state = test_state().await;
        // 两个世界的成员完全相同、joined_at 完全相同，只有 INSERT 先后相反。
        // 若查询缺次级键，两边拿到的行序就会取决于 DB 的物理返回序。
        for (world_id, first, second) in [("w_fwd", "c_alpha", "c_beta"), ("w_rev", "c_beta", "c_alpha")] {
            crate::safety::testkit::seed_world(&state.db, world_id, 0, "running").await;
            for cid in [first, second] {
                if sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM cloud_characters WHERE id = $1")
                    .bind(cid)
                    .fetch_one(&state.db)
                    .await
                    .unwrap()
                    == 0
                {
                    seed_char(&state.db, cid).await;
                }
                // joined_at 恒为 777：模拟并发 join 撞同一毫秒。
                sqlx::query(
                    "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, \
                     boundary_json, status, joined_at) VALUES ($1, $2, 'u1', $3, '{}', 'active', 777)",
                )
                .bind(format!("m_{world_id}_{cid}"))
                .bind(world_id)
                .bind(cid)
                .execute(&state.db)
                .await
                .expect("seed member");
            }
        }

        let fwd: Vec<String> =
            load_active_cards(&state.db, "w_fwd").await.unwrap().into_iter().map(|(cid, _)| cid).collect();
        let rev: Vec<String> =
            load_active_cards(&state.db, "w_rev").await.unwrap().into_iter().map(|(cid, _)| cid).collect();

        assert_eq!(fwd, vec!["c_alpha".to_string(), "c_beta".to_string()], "应按 cid 字典序，与插入顺序无关");
        assert_eq!(fwd, rev, "插入顺序相反时行序必须仍然一致——这正是重放漂移的来源");
    }

    /// 次级键不得喧宾夺主：joined_at 不同时仍按时间先后，保持「先来后到」的既有语义。
    #[tokio::test]
    async fn distinct_joined_at_still_orders_by_time_first() {
        let state = test_state().await;
        crate::safety::testkit::seed_world(&state.db, "w_time", 0, "running").await;
        // 字典序靠后的 c_zeta 先加入，字典序靠前的 c_alpha 后加入。
        for (cid, joined) in [("c_zeta", 100i64), ("c_alpha", 200i64)] {
            seed_char(&state.db, cid).await;
            sqlx::query(
                "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, \
                 boundary_json, status, joined_at) VALUES ($1, 'w_time', 'u1', $2, '{}', 'active', $3)",
            )
            .bind(format!("m_{cid}"))
            .bind(cid)
            .bind(joined)
            .execute(&state.db)
            .await
            .expect("seed member");
        }

        let order: Vec<String> =
            load_active_cards(&state.db, "w_time").await.unwrap().into_iter().map(|(cid, _)| cid).collect();
        assert_eq!(
            order,
            vec!["c_zeta".to_string(), "c_alpha".to_string()],
            "joined_at 不同时先来后到优先，次级键只在撞毫秒时生效"
        );
    }

    /// 🔴 真正的防回归闸：源码级断言 ORDER BY 必须带次级键。
    ///
    /// 上面两个行为测试在 SQLite 下删掉次级键也会绿（索引扫描恰好有序），
    /// 只有本断言能拦住「有人图省事把次级键删了」。体例同
    /// `progression::tests::red_line_engine_decision_paths_never_reference_mileage`。
    #[test]
    fn order_by_clause_pins_secondary_key() {
        // ⚠️ `include_str!` 会把**本测试自己**也读进来，断言里的字面量会匹配到自身：
        //    「必须存在」的断言因此永远为真（假绿）。
        //
        // 🔴 曾用「按第一个测试段标记截断、只扫生产段」来规避，但那个办法两次栽在同一件事上：
        //    生产段一旦出现该标记的字面量（哪怕在注释里、哪怕在文件很靠前处新增了一个测试子模块），
        //    截断点就前移，断言随即静默失效——第二次是 runtime 在 :593 新增子模块、
        //    而目标 SQL 在 :2053，整段被切掉，断言直接假红。
        //
        // 现在改用**拆写 needle**：源码里只存在两个片段，拼接后的完整串在文件中出现且仅出现在
        // 被检查的那条 SQL 里。不再需要截断，也就不存在"截断点被谁挪走"这回事。
        let needle = concat!("ORDER BY wm.joined_at ASC, ", "wm.cloud_character_id ASC");
        // 拆点必须选在「拼接后完整、但源码文本里不连续」的位置——
        // 上一版拆在 ASC 与引号之间，源码里恰好构成 `ASC",` 而匹配到自身（假红）。
        let bad = concat!("ORDER BY wm.", "joined_at ASC\",");

        let src = include_str!("mod.rs");
        assert!(
            src.contains(needle),
            "assembly::load_active_cards 的 ORDER BY 必须带 cloud_character_id 次级键：\n\
             joined_at 撞毫秒时行序由 DB 决定，装配产物会在重放间漂移（Postgres 上尤其如此，\n\
             SQLite 因走唯一索引恰好有序，行为测试证伪不了）"
        );
        assert!(!src.contains(bad), "不得存在只按 joined_at 排序的成员查询");

        let runtime_src = include_str!("../runtime/mod.rs");
        assert!(
            runtime_src.contains(needle),
            "runtime 的成员卡查询必须与 assembly 同口径——它构造的 other_cards_brief \n\
             与 principal 投影会直接喂给引擎，漂移影响比装配层更直接"
        );
        assert!(!runtime_src.contains(bad), "runtime 侧同样不得只按 joined_at 排序");
    }
}

