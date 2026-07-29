//! 角色决策（规格 §12.2）：白名单上下文组装 + role_decide 调用。文件所有权：agent-E4。
//!
//! 信息边界铁律：给角色 X 组装的上下文只允许包含——
//! 公共 world 层、X 自己的 CharacterState、from==X 或 to==X 且 known_to 含 X 的关系、
//! 公开场景描述、X 的 DNA 卡、绑定到 X 的知识片段、主人托梦（平台注入，低优先层）、
//! **X 自己的开局站位（身份）**（平台注入，纯展示层，见 `assemble_visible_context` 第 7 段）。
//! 其他角色的 DNA 内容只能以第三人称一句话摘要出现（防卡片注入，平台规格 §14.1）。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::character::types::CharacterCardV2;
use crate::host::{CancelFlag, EngineHost};
use crate::knowledge::types::RetrievedFragment;
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{NarrativeState, RoleDecision};

pub struct DecidePrompts {
    pub system: String,
    pub prompt_version: String,
}

/// world 层内部保留键，绝不进入任何角色可见上下文。
///
/// 🔴 **不再在这里另写字面量**：它与 `reducer` 的「不可经 patch 写」用**同一个数组**
/// （`reducer::RESERVED_WORLD_KEYS`）。此前两处各写一份 `"appliedPatchIds"`，
/// 而漂开的后果是**内部账静默出现在每个角色的 prompt 里**——不报错、不告警。
/// 加第二个内部键时只需改那一处，两边一起生效。
use super::reducer::RESERVED_WORLD_KEYS;

/// 从 DNA 卡中裁出「行为层」视图（角色自己的卡，全部对自己可见）；
/// 剔除存储/版本元数据，仅保留决策相关层，供角色本人推理。
/// 🔴 **进模型可见面的卡字段，用白名单而不是黑名单。**
///
/// 这里此前是**排除表**：`to_value(card)` 之后 `remove` 掉几个已知的元数据键，
/// 其余**自动**进入每个角色的 prompt。于是给 `CharacterCardV2` 加一个新字段，
/// 它**默认就被喂给模型了**——加字段的人不需要、也不会想到要为此做任何决定。
/// 若那个新字段是内部的、存储用的、或含未公开信息，后果是**静默泄露**。
///
/// ⚠️ 与 `world` 那个排除表（`reducer::RESERVED_WORLD_KEYS`）的区别要说清楚：
/// `world` 是**开放键空间**（叙事可写任意键），只能靠排除表挡；
/// 而 `CharacterCardV2` 是**固定 struct**，完全可以用白名单——
/// 既然可以，就不该把「新字段默认泄露」这条路留着。
///
/// 由 `dna_view_covers_every_card_field_by_an_explicit_decision` 钉住：
/// 卡的每个字段必须**要么在包含表、要么在排除表**，加字段时不做选择就红。
const DNA_VIEW_INCLUDE: &[&str] = &[
    "identity",
    "dramaticCore",
    "decisionModel",
    "perception",
    "emotionDynamics",
    "relationGrammar",
    "expressionFingerprint",
    "agency",
    "growthArc",
    "worldAdaptation",
];

/// 世界记忆窗口：可见上下文里最多带多少条**自己的**历史条目（§0.2 参数化的产品口径）。
///
/// 取 12 的理由不是拍脑袋：它要同时满足两件相反的事——够长到让「上一场冲突」还在视野里
/// （一场冲突通常产出 2-4 条），又短到不让上下文随世界长度无界增长（那会直接变成 token 账单）。
/// ⚠️ 真实值应当在有真实模型账单之后再调，本仓至今一次真实模型调用都没发生过。
const MEMORY_WINDOW: usize = 12;

/// 明确**不进**角色可见上下文的卡字段：存储/版本元数据与证据索引。
/// 与 [`DNA_VIEW_INCLUDE`] 的并集必须恰好等于卡的全部字段（用例钉住）。
const DNA_VIEW_EXCLUDE: &[&str] =
    &["schemaVersion", "id", "lifecycle", "evidenceIndex", "revision", "createdAt", "updatedAt"];

/// 白名单过滤本体。**单独抽出来是为了能被直接喂一个「多了一个字段」的 map** ——
/// 那正是「给卡加字段」时会发生的事，而它不该需要真去改 struct 才测得了
///（改 struct 的注入会牵动几十个构造点，编译都过不去，什么也证明不了）。
fn dna_view_from_map(obj: &serde_json::Map<String, Value>) -> Value {
    Value::Object(
        DNA_VIEW_INCLUDE
            .iter()
            .filter_map(|k| obj.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect(),
    )
}

fn dna_view(card: &CharacterCardV2) -> Result<Value, EngineError> {
    let v = serde_json::to_value(card)?;
    let obj = v.as_object().ok_or_else(|| {
        EngineError::Validation("角色卡序列化结果不是对象，无法裁出行为层视图".into())
    })?;
    Ok(dna_view_from_map(obj))
}

/// 组装角色可见上下文（纯函数，必测隔离性：B 的 secrets 永不出现在 A 的产物中）。
///
/// 铁律实现要点：只读 `state.characters[character_id]` 这一格自身私有状态，
/// 绝不遍历其他角色的 secrets/misconceptions/plans；他人仅以调用方给定的第三人称摘要出现。
///
/// `self_identity` = **本角色自己**的开局站位展示名（如 `户部主事`，平台由 runtime 回灌）。
/// 传 `None`/空白 → 上下文里根本不出现该字段，与接线前逐字节一致。调用方只允许传本人那一条，
/// 他人的自身身份绝不进来（他人身份如何被感知仍走且只走 `other_cards_brief`）。
#[allow(clippy::too_many_arguments)] // 白名单上下文的全量入参，逐项都是信息边界的一条通道
pub fn assemble_visible_context(
    state: &NarrativeState,
    character_id: &str,
    card: &CharacterCardV2,
    other_cards_brief: &BTreeMap<String, String>,
    situation: &str,
    fragments: &[RetrievedFragment],
    whisper: Option<&str>,
    self_identity: Option<&str>,
    ambient: &[crate::narrative::AmbientEvent],
) -> Result<String, EngineError> {
    // 1) 自己的私有状态：只取自己这一格（缺失视为空态），不触碰任何他人条目。
    let own = state.characters.get(character_id).cloned().unwrap_or_default();
    let own_v = serde_json::to_value(&own)?;

    // 2) 与自己相关且自己知情的关系：
    //    - from==X：X 是关系主体（自身对外的信任/情感），本人天然知情；
    //    - to==X 且 known_to 含 X：X 是关系客体且已被告知，才可见。
    //    其他角色之间的关系一律不进入（最大程度杜绝泄漏）。
    let relations: Vec<Value> = state
        .relations
        .iter()
        .filter(|r| {
            r.from == character_id
                || (r.to == character_id && r.known_to.iter().any(|k| k == character_id))
        })
        .map(|r| {
            json!({
                "from": r.from,
                "to": r.to,
                "trust": r.trust,
                "affinity": r.affinity,
                "fear": r.fear,
                "debt": r.debt,
                "notes": r.notes,
            })
        })
        .collect();

    // 3) 公共 world 层（剔除引擎内部保留键）。
    let world: BTreeMap<&String, &Value> =
        state.world.iter().filter(|(k, _)| !RESERVED_WORLD_KEYS.contains(&k.as_str())).collect();
    let world_v = serde_json::to_value(&world)?;

    // 4) 他人仅以第三人称一句话摘要出现（不含自己；防卡片/原文 DNA 注入）。
    let others: BTreeMap<&String, &String> =
        other_cards_brief.iter().filter(|(k, _)| k.as_str() != character_id).collect();
    let others_v = serde_json::to_value(&others)?;

    // 5) 绑定到自己的知识片段（来源可溯，供审校 100% 追踪）。
    let knowledge: Vec<Value> =
        fragments.iter().map(|f| json!({ "pack": f.pack_title, "text": f.text })).collect();

    // 6) 自己的 DNA 行为层。
    let dna_v = dna_view(card)?;

    let mut ctx = json!({
        "you": character_id,
        "situation": situation,
        "yourDna": dna_v,
        "yourState": own_v,
        "yourRelations": Value::Array(relations),
        "world": world_v,
        "others": others_v,
        "knowledge": Value::Array(knowledge),
    });
    // 7) 自身开局站位（平台总规格 §5【拍板 4、5】：身份 = 开局站位）。
    //
    // 为何要单开一条通道：上面第 4 段的 `others` 恒**剔除自己**，于是「你在这个世界是户部主事」
    // 只有别人看得见、角色本人反而看不见——玩家感知不到自己的开局站位。此处把本人那一条单独喂入。
    //
    // 🔴 平权宪法（总规格 §0.1 真红线 1）：身份**不携带任何数值差异、准入门槛、产出加成、
    //    难度优待或叙事特权**。本字段是**纯展示层**，只影响角色如何自述与自我定位；
    //    引擎内**没有任何判定读它**——仲裁（rule/model）、确定性不变量、reducer/StatePatch、
    //    同意门控、关系演化、里程碑强度全都不引用它，它也绝不写回 `NarrativeState`。
    //    下游任何「据身份改判定 / 改发奖 / 开权限 / 调难度」都是红线违规。
    //
    // 🔒 不进 DNA：身份只挂在上下文顶层这一格，`yourDna`（来自 `active_cards` 的不可变卡快照）
    //    一个字节都不改（红线由 `self_identity_never_touches_dna_redline` 守着）。
    //
    // 退化：`None` 或空白展示名 → 该字段完全不出现，产物与接线前逐字节一致。
    // 确定性：纯函数、单值拼接，无随机、不依赖任何迭代序。
    if let Some(d) = self_identity.map(str::trim).filter(|d| !d.is_empty()) {
        if let Some(obj) = ctx.as_object_mut() {
            obj.insert(
                "yourIdentity".to_string(),
                json!({
                    "display": d,
                    "note": "这是你在这个世界的开局站位：你就是以这个身份出场的，别人也这样看你、这样称呼你。\
它只说明你从哪里起步、与谁相熟、手边有什么，不给你任何额外优势、特权、豁免或更高的成功率——\
所有人一律平等，戏份要靠你自己玩出来。",
                }),
            );
        }
    }

    // 8) 环境事件（观众礼物等，平台总规格红线 1「不卖胜负与数值平权」）。
    //
    // 🔴 **它买到的是「被看见」，不是「影响力」**（产品 2026-07-28 拍板，open-decisions §5 选项 A）。
    //    与 `yourIdentity` 同款处理：**纯展示层**，只影响场景里出现了什么、角色可以提到什么；
    //    引擎内**没有任何判定读它**——仲裁（rule/model）、确定性不变量、reducer/StatePatch、
    //    同意门控、关系演化、里程碑强度全都不引用它，它也绝不写回 `NarrativeState`。
    //
    // 🔴 **所有角色看到同一份**：礼物是公开的、场上的，不按角色分发。
    //    按角色分发会立刻造出「谁的观众多谁看到更多」这条可优化的差异通道。
    //
    // 🔴 `count` 是**聚合计数不是强度**：note 里明写「送得多不等于更管用」，
    //    因为模型是会自己脑补数值语义的——不写死这句话，「×5」就会被演成「效果更强」。
    //
    // 退化：空切片 → 该字段完全不出现，产物与接线前逐字节一致。
    // 确定性：按调用方给定的顺序原样拼接，无随机、不排序、不依赖任何 map 迭代序。
    if !ambient.is_empty() {
        if let Some(obj) = ctx.as_object_mut() {
            let items: Vec<Value> = ambient
                .iter()
                .filter(|e| !e.label.trim().is_empty())
                .map(|e| json!({ "what": e.label.trim(), "times": e.count }))
                .collect();
            if !items.is_empty() {
                obj.insert(
                    "ambient".to_string(),
                    json!({
                        "items": items,
                        "note": "这些是场外的人送进来的东西，场上确实出现了，你可以看见、可以提到、可以用它做戏。它**不给任何人优势**：不改变成败判定、不提高成功率、不带来豁免或特权，也不代表谁更重要。times 只是同一样东西被送了几次，**送得多不等于更管用**。谁也没有因为它变强，戏还是要自己演。",
                    }),
                );
            }
        }
    }

    // 9) 世界记忆（2026-07-29）：**这个角色记得自己在这个世界里做过什么、结果如何**。
    //
    // ══════════════════════════════════════════════════════════════════════════
    // 🔴 它补的是一个此前没人注意到的空洞：**角色是失忆的**
    // ══════════════════════════════════════════════════════════════════════════
    // 在这一段落地之前，可见上下文里**没有任何历史**——没有上一拍正文、没有事件记录、
    // 没有游戏时钟。角色每一拍都是从「同一份静态卡 + 同一份状态」重新开始面对世界的。
    //
    // 而 `narrative.pacingNotes` 一直在记录每一条仲裁结果（`build_patch` 每拍 Append），
    // 却**全仓没有任何生产代码读它**——系统把「发生过什么」写下来，然后扔掉了。
    // 本段就是那个缺失的读取方。
    //
    // ══════════════════════════════════════════════════════════════════════════
    // 🔴 只给**自己的**条目：信息隔离铁律不容许任何例外
    // ══════════════════════════════════════════════════════════════════════════
    // `pacingNotes` 是**全局**流水（含所有角色的结果），而本函数的铁律是
    // 「B 的私有状态永不出现在 A 的产物中」（`context_isolation` 系列用例守着）。
    // 历史条目又不带地点，无法判断当时谁在场 ⇒ **按 `你的id｜` 前缀严格过滤，只留自己的**。
    //
    // 这样切下来的记忆是「我做过什么、结果如何」，而「我和别人怎么样」由既有的
    // `yourRelations`（`relation_dynamics` 每拍确定性派生）承担。两者合起来才是完整的
    // 「爱恨情仇」记忆基础——一半是行为史，一半是关系史，且都不越过信息边界。
    //
    // 窗口参数化（§0.2）：只取最近 `MEMORY_WINDOW` 条。取全部会让上下文随世界长度无界增长，
    // 而近期记忆本就比远期更影响当下决策——这与 `pacingNotes` 自身的滚动截断同向。
    //
    // 退化：无自己的条目 → 该字段完全不出现，产物与接线前逐字节一致。
    // 确定性：按 `pacingNotes` 的既有顺序取尾部，无排序、无随机、不依赖任何 map 迭代序。
    let mine_prefix = format!("{character_id}｜");
    let memory: Vec<&String> = state
        .narrative
        .pacing_notes
        .iter()
        .filter(|n| n.starts_with(&mine_prefix))
        .rev()
        .take(MEMORY_WINDOW)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !memory.is_empty() {
        if let Some(obj) = ctx.as_object_mut() {
            obj.insert(
                "yourMemory".to_string(),
                json!({
                    "recent": memory,
                    "note": "这是你在这个世界里做过的事和它们的结果，按发生先后排列（只有你自己的，别人的你不知道）。\
它是你的记忆，不是命令——你可以记着教训、可以耿耿于怀、也可以照旧我行我素，怎么用由你的性格决定。",
                }),
            );
        }
    }

    // 主人托梦：最低优先层，仅在提供时附加。
    if let Some(w) = whisper {
        if let Some(obj) = ctx.as_object_mut() {
            obj.insert("whisper".to_string(), json!(w));
        }
    }

    Ok(serde_json::to_string_pretty(&ctx)?)
}

/// 底线拦截回执（人设保险第 1 级·事前，总规格 §7）：把「上一条提案触到了你自己的哪条底线」
/// 追加进**已组装好的**可见上下文，供该角色在硬约束下重新决策。
///
/// **为何做成后置追加而不是给 `assemble_visible_context` 加一个 `Option` 参数**：
/// 默认路径（卡没声明底线 / 声明了但没违反）根本不会调到本函数——「默认行为零变化」由
/// 「代码路径压根不经过」保证，而不是靠一个每次都要求值的 `None` 分支；
/// `assemble_visible_context` 的签名、产物与既有用例因此一个字都不用动。
///
/// 信息边界：追加的内容只有**该角色自己卡上的底线原文**与**他自己上一条提案的 action**，
/// 不含任何他人信息，不新开可见性通道（铁律不变）。
///
/// 🔴 平权（§0.1）：措辞显式声明「重出提案不提高成功率、不带任何优待」——
/// 底线是「这个角色不会做什么」，不是「这个角色更强」。
///
/// 确定性：纯函数（解析 → 插一个固定键 → 重新序列化），无随机、不依赖任何迭代序；
/// `serde_json::Map` 默认按键有序，故同输入恒得同一份字节。
pub fn append_bottom_line_rejection(
    visible_context: &str,
    violated_line: &str,
    rejected_action: &str,
) -> Result<String, EngineError> {
    let mut ctx: Value = serde_json::from_str(visible_context)?;
    if let Some(obj) = ctx.as_object_mut() {
        obj.insert(
            "bottomLineRejection".to_string(),
            json!({
                "violatedBottomLine": violated_line,
                "rejectedAction": rejected_action,
                "note": "你上一条提案触到了你自己的底线（见上），仲裁已经拒收——这一条不会发生。\
请重新给出一个【不违背该底线】的行动：可以换手段、换目标、换时机，也可以选择按兵不动。\
重出提案不会提高你的成功率，也不给你任何优待、豁免或额外资源；\
底线说的是「你不会做什么」，不是「你更强」。",
            }),
        );
    }
    Ok(serde_json::to_string_pretty(&ctx)?)
}

/// 世界线烙印（`spec-worldline-imprint.md` §2.2 b「共鸣 · 表现层」，第 5 步）：
/// 把这张卡**在别的世界里**经历过什么，追加进它已组装好的可见上下文。
///
/// ══════════════════════════════════════════════════════════════════════════
/// 🔴 它与 `yourMemory` 是**两件事**，别合并
/// ══════════════════════════════════════════════════════════════════════════
/// | | 记的是 | 来源 | 跨世界吗 |
/// |---|---|---|---|
/// | `yourMemory` | 我在**这个**世界里做过什么 | `narrative.pacingNotes` | ❌ |
/// | `yourPast`（本函数） | 我在**别的**世界里经历过什么 | 服务端 `character_imprints` | ✅ |
///
/// 合并会毁掉后者的全部意义：烙印之所以能兑现「复刻内核也复刻不了这张卡」，
/// 正是因为它**不在卡里、不在本世界状态里**，只能由服务端按这张卡的编号取出来。
///
/// **为何是后置追加而不是给 `assemble_visible_context` 加参数**：同
/// [`append_bottom_line_rejection`] 的理由——桌面壳与新卡根本不会调到本函数，
/// 「默认行为零变化」由「代码路径压根不经过」保证，而不是靠一个每次都要求值的空分支。
///
/// 🔴 平权（§0.1）：措辞显式声明「这是你带过来的过去，不是你的能力」。
/// 烙印**不含优势语义**——同一条「他退过一次」落在不同内核上会产生相反的行为，
/// 方向由卡自己的决策模型决定，所以它强化的恰恰是「内核决定一切」。
///
/// ⚠️ **这一步的效果不可证伪**：能测的只有「注入了什么」，测不了「模型因此怎么变」。
/// 真要验它得做 A/B——同内核、同世界、同种子，只差烙印，跑 N 次比较决策分布，
/// 而那需要真实模型凭据（本仓至今一次真实模型调用都没发生过）。
/// 因此本步状态是 `Integrated`，**不是** `Validated`（状态语言七档）。
///
/// 空表 → 原样返回（连解析都不做，逐字节恒等）。
/// 确定性：纯函数（解析 → 插一个固定键 → 重新序列化），无随机、不依赖任何迭代序。
pub fn append_worldline_imprints(
    visible_context: &str,
    past: &[String],
) -> Result<String, EngineError> {
    let lines: Vec<&String> = past.iter().filter(|s| !s.trim().is_empty()).collect();
    if lines.is_empty() {
        return Ok(visible_context.to_string());
    }
    let mut ctx: Value = serde_json::from_str(visible_context)?;
    if let Some(obj) = ctx.as_object_mut() {
        obj.insert(
            "yourPast".to_string(),
            json!({
                "lived": lines,
                "note": "这些是你在别的世界里经历过的事，越靠后的越久远、越模糊。\
它是你带过来的过去，不是你的能力：它不让你更强、不提高任何成功率、不给你豁免或特权，\
也不决定你现在该怎么做。你可以被它塑造，也可以偏偏不认它——怎么用由你的性格决定。",
            }),
        );
    }
    Ok(serde_json::to_string_pretty(&ctx)?)
}

/// 私有线索（per-character 钩子的投放）：把「这张卡手上还没了结的事」追加进它的可见上下文。
///
/// ══════════════════════════════════════════════════════════════════════════
/// 🔴 这是「钩子私有制」第一次真的成立
/// ══════════════════════════════════════════════════════════════════════════
/// 平台侧的装配器一直在按每张卡的执念挑钩子、参数化、过机审、落库，
/// 而那段文字**全仓没有任何消费方**——模型没见过，玩家也没见过。
/// 于是「按执念绑定归属，偷不走别人的」这句话在实现上是空的：
/// 没有任何东西可以被偷，因为从没有人拥有过任何东西。本函数是那条投放通道。
///
/// 🔴 **只给自己那一条**，信息隔离铁律不容许例外——这也正是「私有」二字的字面含义。
///
/// 🔴 **它是牵挂，不是牌**：措辞显式声明「不给你任何优势，也不保证你做得成」。
/// 一条私有线索让这个角色**有事可做、有方向**，不让他**更容易赢**——
/// 差别在于前者改变的是「他会去干什么」，后者改变的是「他干成的概率」。
///
/// ⚠️ 关于完成标记那句话：它是**机制说明混进叙事上下文**，我知道这不干净。
/// 替代方案（服务端按事件流反推「这条钩子了结了没有」）需要一条从仲裁结果回到钩子 id 的链路，
/// 而那条链路今天不存在，凭空造一条比让模型记一个键更重。
/// 🔵 真正兜住风险的不是这句话写得多好，而是**默认不启用**：
/// 模板不显式声明 `hookCompletionRequired` 时，结算走老路径（通关就发），
/// 模型不写这个键也不会有任何人少拿东西。
///
/// **为何是后置追加**：同 [`append_bottom_line_rejection`]——桌面壳与无钩子的世界
/// 根本不会调到本函数，「默认行为零变化」由「代码路径压根不经过」保证。
///
/// 空表 → 原样返回（连解析都不做，逐字节恒等）。纯函数，无随机、不依赖任何迭代序。
pub fn append_personal_threads(
    visible_context: &str,
    threads: &[(String, String)],
) -> Result<String, EngineError> {
    let items: Vec<Value> = threads
        .iter()
        .filter(|(what, key)| !what.trim().is_empty() && !key.trim().is_empty())
        .map(|(what, key)| json!({ "what": what.trim(), "doneKey": key.trim() }))
        .collect();
    if items.is_empty() {
        return Ok(visible_context.to_string());
    }
    let mut ctx: Value = serde_json::from_str(visible_context)?;
    if let Some(obj) = ctx.as_object_mut() {
        obj.insert(
            "yourThreads".to_string(),
            json!({
                "unfinished": items,
                "note": "这些是只属于你的线索——别人不知道你惦记着它们，你也不知道别人惦记着什么。\
它们不给你任何优势、不提高成功率、不保证你做得成，也不是非做不可：\
它只说明这个世界上有几件事和你有关，去不去、怎么去，由你的性格决定。",
                "howToClose": "其中一条真的有了了结（做成了，或者彻底断了念想）时，\
在本回合的状态更新里把它的 doneKey 记为 true；没了结就不要写。",
            }),
        );
    }
    Ok(serde_json::to_string_pretty(&ctx)?)
}

/// 决策用户提示：可见上下文 + 严格 JSON 输出契约。
fn build_decide_user_prompt(character_id: &str, visible_context: &str) -> String {
    format!(
        "以下是【仅你（{character_id}）可见】的信息，其它角色的私密一概不在其中：\n{visible_context}\n\n\
请完全代入该角色，基于上述信息做出本回合决策。你的输出只是【提案】，不直接改变世界状态。\
严格输出如下 JSON（不要输出多余文本或解释）：\n\
{{\"intent\":\"你的真实意图\",\"action\":\"你要采取的具体行动\",\
\"speak\":{{\"willSpeak\":true,\"purpose\":\"若发言，目的是什么\"}},\
\"targets\":[\"你行动指向的在场角色id\"],\
\"acceptableCosts\":[\"你愿意为此付出的代价\"],\
\"predictions\":[{{\"characterId\":\"某在场角色id\",\"expected\":\"你预测他会如何反应\",\"confidence\":0.6}}],\
\"duration\":本行动预计耗时（正整数，游戏时间单位；越大则你越晚再次行动）}}"
    )
}

/// 单角色决策调用：严格 JSON → RoleDecision；decision_id/character_id 由代码补齐；
/// targets 白名单校验（只能指向在场角色），越界目标丢弃并记录。
#[allow(clippy::too_many_arguments)] // 签名由骨架固定：注入宿主 + 决策上下文全量入参
pub async fn role_decide(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &DecidePrompts,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    // DES 调度时刻 T（P2 Phase 1）：并入 decision_id 时间段，防同角色跨步撞 id。interval 模式为 0。
    now_hint: i64,
    character_id: &str,
    visible_context: &str,
    active_character_ids: &[String],
    cancel: &CancelFlag,
) -> Result<RoleDecision, EngineError> {
    let spec = ModelCallSpec {
        max_retries: None,
        profile: profile.clone(),
        system: prompts.system.clone(),
        user: build_decide_user_prompt(character_id, visible_context),
        temperature,
        max_output_tokens,
        agent: "roleDecide".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };

    let mut decision: RoleDecision =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;

    // 代码补齐不可信字段：决策 id 确定性派生。加入时间段 now_hint（P2 DES）——同角色异步多次行动
    // （跨 event_step，T 不同）不再撞 id；interval 模式 now_hint=0，形如 `dec:{run}:0:{cid}` 稳定。
    decision.decision_id = format!("dec:{run_id}:{now_hint}:{character_id}");
    decision.character_id = character_id.to_string();

    // duration 补齐（P2 DES）：模型缺省/给 0 或负 → 兜底 DEFAULT_DURATION；再 clamp 到 [MIN,MAX]。
    // 防 duration<=0 导致该角色 next_time 不前进、永远抢占最小 T 而饿死其它角色。
    if decision.duration <= 0 {
        decision.duration = super::DEFAULT_DURATION;
    }
    decision.duration = decision.duration.clamp(super::MIN_DURATION, super::MAX_DURATION);

    // targets 白名单：只保留同组在场角色（active_character_ids 为同组子集，Phase 2 分组收窄），
    // 越界角色目标丢弃；移动伪目标 `loc:<id>` 保留（合法性交仲裁 R6 判连通/准入，非角色白名单范畴）。
    decision
        .targets
        .retain(|t| t.starts_with(super::arbiter::LOC_TARGET_PREFIX) || active_character_ids.iter().any(|a| a == t));

    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::{CardLifecycle, Identity};
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use crate::narrative::types::{CharacterState, RelationState};
    use std::sync::Arc;

    fn minimal_card(name: &str) -> CharacterCardV2 {
        CharacterCardV2 {
            schema_version: 2,
            id: name.to_string(),
            lifecycle: CardLifecycle::Draft,
            identity: Identity { name: name.to_string(), ..Default::default() },
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
            revision: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn state_with_secrets() -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        let mut a = CharacterState::default();
        a.secrets.push("我是国王的私生子".into());
        a.plans.push("暗杀公爵".into());
        a.misconceptions.push("误以为王后已死".into());
        a.goals.push("夺回王位".into());
        s.characters.insert("A".into(), a);
        s.characters.insert("B".into(), CharacterState::default());
        // A→B 的关系，仅 A 知情（known_to=[A]），B 不应看到其 notes。
        s.relations.push(RelationState {
            from: "A".into(),
            to: "B".into(),
            trust: 0.9,
            affinity: 0.1,
            fear: 0.0,
            debt: 0.0,
            known_to: vec!["A".into()],
            notes: vec!["秘密同盟标记".into()],
        });
        s.world.insert("phase".into(), serde_json::json!("夜晚"));
        // 内部保留键不得泄漏。
        s.world.insert(RESERVED_WORLD_KEYS[0].into(), serde_json::json!(["patch-x"]));
        s
    }

    // ===== 信息边界铁律（§12.2，最高优先级，必测）=====

    #[test]
    fn iron_law_b_context_never_contains_a_private_fields() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let brief: BTreeMap<String, String> =
            [("A".to_string(), "A 是一名沉默寡言的侍卫。".to_string())].into_iter().collect();

        let ctx =
            assemble_visible_context(&s, "B", &card_b, &brief, "宫廷大厅", &[], None, None, &[]).unwrap();

        // A 的 secrets / plans / misconceptions / goals 原文一律不得出现在 B 的上下文里。
        assert!(!ctx.contains("私生子"), "泄漏了 A 的 secret：{ctx}");
        assert!(!ctx.contains("暗杀公爵"), "泄漏了 A 的 plan：{ctx}");
        assert!(!ctx.contains("王后已死"), "泄漏了 A 的 misconception：{ctx}");
        assert!(!ctx.contains("夺回王位"), "泄漏了 A 的 goal：{ctx}");
        // A→B 关系仅 A 知情，B 不得看到其 notes。
        assert!(!ctx.contains("秘密同盟标记"), "泄漏了 B 未知情的关系：{ctx}");
        // 引擎内部保留键不得泄漏。
        assert!(!ctx.contains(RESERVED_WORLD_KEYS[0]), "泄漏了内部保留键：{ctx}");

        // B 应能看到：他人第三人称摘要、公共 world、场景。
        assert!(ctx.contains("侍卫"), "缺少他人第三人称摘要");
        assert!(ctx.contains("夜晚"), "缺少公共 world 层");
        assert!(ctx.contains("宫廷大厅"), "缺少场景描述");
    }

    #[test]
    fn owner_can_see_own_private_and_own_relations() {
        let s = state_with_secrets();
        let card_a = minimal_card("A");
        let brief: BTreeMap<String, String> =
            [("B".to_string(), "B 是宫廷侍女。".to_string())].into_iter().collect();

        let ctx =
            assemble_visible_context(&s, "A", &card_a, &brief, "宫廷大厅", &[], None, None, &[]).unwrap();

        // A 自己能看到自己的私密（证明组装器确实注入了自身状态，只是不注入他人的）。
        assert!(ctx.contains("私生子"));
        assert!(ctx.contains("暗杀公爵"));
        // A 是关系主体（from==A），能看到该关系 notes。
        assert!(ctx.contains("秘密同盟标记"));
        // A 能看到他人（B）的第三人称摘要。
        assert!(ctx.contains("宫廷侍女"));
    }

    #[test]
    fn relation_visible_to_target_only_when_known() {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        s.characters.insert("A".into(), CharacterState::default());
        s.characters.insert("B".into(), CharacterState::default());
        // A→B，known_to 含 B：B 作为客体且已知情 → 可见。
        s.relations.push(RelationState {
            from: "A".into(),
            to: "B".into(),
            trust: 0.5,
            affinity: 0.0,
            fear: 0.0,
            debt: 0.0,
            known_to: vec!["A".into(), "B".into()],
            notes: vec!["公开的同僚关系".into()],
        });
        let card_b = minimal_card("B");
        let ctx = assemble_visible_context(&s, "B", &card_b, &BTreeMap::new(), "场景", &[], None, None, &[])
            .unwrap();
        assert!(ctx.contains("公开的同僚关系"), "已知情客体应能看到关系：{ctx}");
    }

    #[test]
    fn whisper_included_when_present() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let ctx = assemble_visible_context(
            &s,
            "B",
            &card_b,
            &BTreeMap::new(),
            "场景",
            &[],
            Some("主人提示：小心那个侍卫"),
            None, &[],)
        .unwrap();
        assert!(ctx.contains("小心那个侍卫"));
    }

    #[test]
    fn knowledge_fragments_included_with_source() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let frags = vec![RetrievedFragment {
            pack_id: "kp-1".into(),
            pack_title: "宫廷礼仪".into(),
            chunk_id: "c1".into(),
            ordinal: 0,
            text: "面见君主须先行躬身礼。".into(),
            score: 1.0,
        }];
        let ctx =
            assemble_visible_context(&s, "B", &card_b, &BTreeMap::new(), "场景", &frags, None, None, &[])
                .unwrap();
        assert!(ctx.contains("宫廷礼仪"));
        assert!(ctx.contains("躬身礼"));
    }

    // ===== 自身开局站位（身份池自身感知，总规格 §5【拍板 4、5】）=====

    /// 默认档（不传自身身份）：上下文里根本不出现 `yourIdentity`，顶层键集与接线前逐字段一致。
    /// 这是「默认行为零变化」的守卫——键集写死，将来任何人往里塞字段都会在这里炸。
    #[test]
    fn self_identity_absent_keeps_context_identical() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let brief: BTreeMap<String, String> =
            [("A".to_string(), "A 是一名沉默寡言的侍卫。".to_string())].into_iter().collect();

        let ctx =
            assemble_visible_context(&s, "B", &card_b, &brief, "宫廷大厅", &[], None, None, &[]).unwrap();

        assert!(!ctx.contains("yourIdentity"), "不传身份 → 该字段必须完全不出现：{ctx}");
        let v: Value = serde_json::from_str(&ctx).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["knowledge", "others", "situation", "world", "you", "yourDna", "yourRelations", "yourState"],
            "默认档顶层键集必须与接线前逐字段一致（serde_json Map 有序 → 断言确定性）"
        );

        // 空白展示名同样退化（与 server 侧 brief 拼接的空白口径一致）。
        let blank =
            assemble_visible_context(&s, "B", &card_b, &brief, "宫廷大厅", &[], None, Some("  "), &[])
                .unwrap();
        assert_eq!(blank, ctx, "空白身份 → 与不传时逐字节一致");
    }

    /// 传入后：角色**本人**看得见自己的开局站位，且措辞讲清「这是你在这个世界的开局站位」。
    #[test]
    fn self_identity_visible_to_owner_as_opening_position() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let ctx = assemble_visible_context(
            &s,
            "B",
            &card_b,
            &BTreeMap::new(),
            "宫廷大厅",
            &[],
            None,
            Some("户部主事"), &[],)
        .unwrap();

        let v: Value = serde_json::from_str(&ctx).unwrap();
        assert_eq!(v["yourIdentity"]["display"], json!("户部主事"), "本人必须看得见自己的身份");
        let note = v["yourIdentity"]["note"].as_str().unwrap_or_default();
        assert!(note.contains("开局站位"), "措辞必须讲明这是开局站位：{note}");
        // 🔴 平权（§0.1）：措辞本身必须否掉任何优待联想，绝不能被模型读成「我有特权」。
        assert!(note.contains("不给你任何额外优势"), "措辞必须显式声明零特权：{note}");
    }

    /// 🔴 红线：身份只挂上下文顶层，**绝不写进 DNA 卡视图**（`active_cards` 是不可变快照）。
    /// 🔴 **卡的每个字段都必须被显式表态：进 prompt，还是不进。**
    ///
    /// `dna_view` 此前是**排除表**——序列化整张卡再 `remove` 掉几个元数据键，
    /// 其余**自动**进入每个角色的 prompt。于是给 `CharacterCardV2` 加一个新字段，
    /// 它**默认就被喂给模型了**，而加字段的人根本不会想到要为此做决定。
    ///
    /// 这条用例把「不做决定」变成红：卡的字段集必须**恰好**等于
    /// 包含表 ∪ 排除表——多一个字段而两张表都没登记，立刻失败。
    ///
    /// ⚠️ 与 `world` 那个排除表的区别：`world` 是开放键空间，只能靠排除表；
    /// `CharacterCardV2` 是固定 struct，**能用白名单就不该留「默认泄露」这条路**。
    #[test]
    fn dna_view_covers_every_card_field_by_an_explicit_decision() {
        let card = minimal_card("A");
        let full = serde_json::to_value(&card).expect("卡可序列化");
        let all: std::collections::BTreeSet<&str> =
            full.as_object().expect("卡是对象").keys().map(String::as_str).collect();
        let decided: std::collections::BTreeSet<&str> =
            DNA_VIEW_INCLUDE.iter().chain(DNA_VIEW_EXCLUDE.iter()).copied().collect();

        let undecided: Vec<&&str> = all.iter().filter(|k| !decided.contains(*k)).collect();
        assert!(
            undecided.is_empty(),
            "🔴 卡上这些字段既不在 DNA_VIEW_INCLUDE 也不在 DNA_VIEW_EXCLUDE：{undecided:?}\n\
             改动前它们会**默认进入每个角色的 prompt**。请显式选一边——\
             这道门存在的全部意义就是不让「加字段」等于「顺手喂给模型」。"
        );
        let stale: Vec<&&str> = decided.iter().filter(|k| !all.contains(*k)).collect();
        assert!(stale.is_empty(), "🔴 两张表里登记了卡上已不存在的字段，请删掉：{stale:?}");

        // 行为面：包含表里的进得去，排除表里的一个都进不来。
        let view = dna_view(&card).expect("裁视图");
        let keys: std::collections::BTreeSet<&str> =
            view.as_object().expect("视图是对象").keys().map(String::as_str).collect();
        for k in DNA_VIEW_INCLUDE {
            assert!(keys.contains(k), "🔴 包含表里的 `{k}` 没进视图：{keys:?}");
        }
        for k in DNA_VIEW_EXCLUDE {
            assert!(!keys.contains(k), "🔴 排除表里的 `{k}` 混进了视图：{keys:?}");
        }

        // 🔴 **白名单的价值只在「卡多了一个字段」时才兑现**——今天两张表并集恰好覆盖全部字段，
        // 所以包含表与排除表**输出完全相同**，光看现有字段是分不出这两种实现的。
        // 这里直接喂一个多出字段的 map，模拟「有人给 CharacterCardV2 加了一列」：
        // 白名单实现下它进不来；排除表实现下它会**默认出现在每个角色的 prompt 里**。
        let mut with_new_field = full.as_object().expect("卡是对象").clone();
        with_new_field.insert("internalAuditNote".into(), serde_json::json!("内部批注，不该给模型看"));
        let view2 = dna_view_from_map(&with_new_field);
        let s2 = serde_json::to_string(&view2).expect("可序列化");
        assert!(
            !s2.contains("internalAuditNote") && !s2.contains("内部批注"),
            "🔴 卡上新增的字段**默认进了角色 prompt**。这正是排除表的失效方式：\n\
             加字段的人不会想到要为此做决定，而后果是静默泄露。实得：{s2}"
        );
    }

    /// 🔴 **每一个引擎内部保留键都必须被挡在所有角色的上下文之外——一个都不许漏。**
    ///
    /// # 这条门守的是一个「排除表」，而排除表的失败方向是泄露
    ///
    /// `world` 是**开放键空间**（叙事可以往里写任意键），所以内部键只能靠排除表挡。
    /// 而排除表漏一条的后果是：那个键**静默出现在每个角色的 prompt 里**——
    /// 不报错、不告警，只是每份上下文里多了一段谁也没打算给模型看的东西。
    ///
    /// 此前 `reducer`（不可写）与 `decide`（不可见）**各写了一份 `"appliedPatchIds"` 字面量**，
    /// 任一处改名或加键而另一处没跟上就会漂开。现在两处共用 `RESERVED_WORLD_KEYS`，
    /// 本用例**遍历那个数组**逐个验——加第二个内部键时它自动进入判据，
    /// 想漏都漏不掉（`docs/VALIDATION.md` §3.8.1 形态 (a)）。
    #[test]
    fn reserved_world_keys_are_filtered_from_every_visible_context() {
        assert!(!RESERVED_WORLD_KEYS.is_empty(), "保留键表不该是空的");
        let card_b = minimal_card("B");
        for key in RESERVED_WORLD_KEYS {
            let mut s = state_with_secrets();
            // 内部键的值取一个**一眼能认出来**的探针串。
            s.world.insert((*key).to_string(), serde_json::json!(["INTERNAL-PROBE-VALUE"]));
            // 同时放一个普通叙事键，确认过滤没有把正常的 world 层一起吃掉。
            s.world.insert("weather".into(), serde_json::json!("大雪"));

            let ctx = assemble_visible_context(
                &s, "B", &card_b, &BTreeMap::new(), "场景", &[], None, None, &[],
            )
            .unwrap();
            // 🔴 **导演 prompt 也是模型可见面**——它此前用的是另一个裸字面量（第三处）。
            let director_world =
                serde_json::to_string(&crate::narrative::public_world(&s)).unwrap();
            assert!(
                !director_world.contains(key) && !director_world.contains("INTERNAL-PROBE-VALUE"),
                "🔴 内部保留键 `{key}` 泄漏进了入场导演的 prompt：{director_world}"
            );
            assert!(director_world.contains("大雪"), "导演也该看得见正常的 world 层");
            assert!(
                !ctx.contains(key) && !ctx.contains("INTERNAL-PROBE-VALUE"),
                "🔴 内部保留键 `{key}` 泄漏进了角色可见上下文：{ctx}"
            );
            assert!(
                ctx.contains("大雪"),
                "🔴 过滤把正常的 world 层也吃掉了 —— 那比泄露更容易被当成「模型变笨了」：{ctx}"
            );
        }
    }

    /// 反向配对：保留键**同时**必须挡住 patch 写入（两件事共用同一张表）。
    #[test]
    fn reserved_world_keys_are_also_rejected_by_the_reducer() {
        for key in RESERVED_WORLD_KEYS {
            let err = crate::narrative::reducer::parse_path(&format!("world.{key}"))
                .expect_err("保留键必须不可经 patch 写");
            assert!(
                format!("{err}").contains("保留键"),
                "报错要说清它是保留键，否则作者只会以为路径写错了：{err}"
            );
        }
    }

    /// 🔴 **礼物只改上下文的那一格，别的一个字节都不动。**
    ///
    /// 源码扫描（`lib.rs::ambient_events_never_leave_the_presentation_layer`）证明的是
    /// 「没有别的地方**引用**它」；这条从行为侧补另一半：**同一份状态、加与不加礼物，
    /// 产出的上下文除了 `ambient` 这一格之外逐键相同**。
    ///
    /// 两条一起才封得住：只有源码扫描的话，「在 decide.rs 里顺手把礼物拼进 situation」
    /// 不会被它拦下（那仍然只出现在允许的文件里）。
    #[test]
    fn ambient_only_changes_the_ambient_key_of_the_context() {
        use crate::narrative::AmbientEvent;
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let brief = BTreeMap::new();

        let plain = assemble_visible_context(
            &s, "B", &card_b, &brief, "宫廷大厅", &[], None, None, &[],
        )
        .unwrap();
        let gifts = vec![
            AmbientEvent { label: "有人送上一束火把".into(), count: 3 },
            AmbientEvent { label: "  ".into(), count: 9 }, // 空白项应被剔除
        ];
        let with = assemble_visible_context(
            &s, "B", &card_b, &brief, "宫廷大厅", &[], None, None, &gifts,
        )
        .unwrap();

        let a: Value = serde_json::from_str(&plain).unwrap();
        let mut b: Value = serde_json::from_str(&with).unwrap();
        let amb = b.as_object_mut().unwrap().remove("ambient").expect("应有 ambient 一格");
        assert_eq!(
            a, b,
            "🔴 除 `ambient` 外还有别的键被礼物改动了 —— 那就不再是「纯展示层」。\n             最容易发生的形态是把礼物拼进 `situation` 或 `world`：源码扫描拦不住它（仍在允许的文件里），\n             这条用例才拦得住。"
        );

        // 空白 label 被剔除；`times` 是计数不是强度，且 note 必须把这句话说给模型听。
        let items = amb["items"].as_array().expect("items");
        assert_eq!(items.len(), 1, "空白展示文案不该出现在上下文里：{amb}");
        assert_eq!(items[0]["what"], "有人送上一束火把");
        assert_eq!(items[0]["times"], 3);
        let note = amb["note"].as_str().unwrap_or_default();
        for must in ["不给任何人优势", "送得多不等于更管用"] {
            assert!(
                note.contains(must),
                "🔴 note 里必须明说「{must}」。模型会自己脑补数值语义——\n                 不把这句话写死，`times: 5` 就会被演成「效果更强」，那就是在卖优势。实得：{note}"
            );
        }

        // 全空 / 全空白 → 该键完全不出现，与接线前逐字节一致。
        let blank = vec![AmbientEvent { label: "   ".into(), count: 1 }];
        let degraded = assemble_visible_context(
            &s, "B", &card_b, &brief, "宫廷大厅", &[], None, None, &blank,
        )
        .unwrap();
        assert_eq!(degraded, plain, "🔴 没有有效礼物时必须逐字节退化为接线前");
    }

    #[test]
    fn self_identity_never_touches_dna_redline() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let ctx = assemble_visible_context(
            &s,
            "B",
            &card_b,
            &BTreeMap::new(),
            "场景",
            &[],
            None,
            Some("户部主事"), &[],)
        .unwrap();
        let v: Value = serde_json::from_str(&ctx).unwrap();
        assert_eq!(v["yourDna"]["identity"]["name"], json!("B"), "卡上的名字必须原样");
        assert!(
            !v["yourDna"].to_string().contains("户部主事"),
            "红线：身份不得出现在 DNA 卡的任何字段里"
        );
        assert!(
            !v["yourState"].to_string().contains("户部主事"),
            "红线：身份不得渗进角色私有状态（那会随 reducer 落回世界状态）"
        );
    }

    /// 信息边界：调用方只喂本人那一条 → 他人的自身身份绝不出现在本角色上下文里。
    /// （他人身份如何被感知，仍走且只走 `other_cards_brief`。）
    #[test]
    fn other_players_self_identity_never_leaks() {
        let s = state_with_secrets();
        let card_a = minimal_card("A");
        let card_b = minimal_card("B");

        let ctx_a = assemble_visible_context(
            &s,
            "A",
            &card_a,
            &BTreeMap::new(),
            "场景",
            &[],
            None,
            Some("户部主事"), &[],)
        .unwrap();
        let ctx_b = assemble_visible_context(
            &s,
            "B",
            &card_b,
            &BTreeMap::new(),
            "场景",
            &[],
            None,
            Some("漕帮商贾"), &[],)
        .unwrap();

        assert!(ctx_a.contains("户部主事") && !ctx_a.contains("漕帮商贾"), "A 不得看见 B 的自身身份");
        assert!(ctx_b.contains("漕帮商贾") && !ctx_b.contains("户部主事"), "B 不得看见 A 的自身身份");
        // 本人有身份、他人没传 → 上下文里也不得凭空出现他人的身份字段。
        let plain =
            assemble_visible_context(&s, "B", &card_b, &BTreeMap::new(), "场景", &[], None, None, &[])
                .unwrap();
        assert!(!plain.contains("yourIdentity"), "未传身份的角色不得被别人的身份污染");
    }

    // ===== 底线拦截回执（人设保险第 1 级·事前，总规格 §7）=====

    /// 回执只带角色**自己**的东西：被违反的底线原文 + 他自己上一条提案；不新开任何可见性通道。
    /// 🔴 平权：措辞必须显式否掉「重出提案更容易成功」的联想。
    #[test]
    fn bottom_line_rejection_carries_only_own_card_content() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let brief: BTreeMap<String, String> =
            [("A".to_string(), "A 是一名沉默寡言的侍卫。".to_string())].into_iter().collect();
        let base =
            assemble_visible_context(&s, "B", &card_b, &brief, "宫廷大厅", &[], None, None, &[]).unwrap();

        let ctx = append_bottom_line_rejection(&base, "不替人做伪证", "替裴照做伪证").unwrap();
        let v: Value = serde_json::from_str(&ctx).unwrap();
        assert_eq!(v["bottomLineRejection"]["violatedBottomLine"], json!("不替人做伪证"));
        assert_eq!(v["bottomLineRejection"]["rejectedAction"], json!("替裴照做伪证"));
        let note = v["bottomLineRejection"]["note"].as_str().unwrap_or_default();
        assert!(note.contains("不违背该底线"), "必须给出可执行的重出指令：{note}");
        assert!(note.contains("不会提高你的成功率"), "必须显式声明零优待：{note}");

        // 信息边界：回执没有把任何他人私密带进来（原上下文的隔离性不被破坏）。
        assert!(!ctx.contains("私生子"));
        assert!(!ctx.contains("暗杀公爵"));

        // 除新增的这一个键外，其余键集与基线完全一致（不夹带任何别的东西）。
        let base_v: Value = serde_json::from_str(&base).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.retain(|k| *k != "bottomLineRejection");
        let base_keys: Vec<&str> = base_v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, base_keys, "回执只许多出 bottomLineRejection 一个键");
    }

    /// 确定性：同输入连跑两次逐字节相同（重生成路径必须可 replay）。
    #[test]
    fn bottom_line_rejection_is_byte_deterministic() {
        let s = state_with_secrets();
        let card_b = minimal_card("B");
        let base =
            assemble_visible_context(&s, "B", &card_b, &BTreeMap::new(), "场景", &[], None, None, &[])
                .unwrap();
        let a = append_bottom_line_rejection(&base, "不做伪证", "做伪证").unwrap();
        let b = append_bottom_line_rejection(&base, "不做伪证", "做伪证").unwrap();
        assert_eq!(a, b);
    }

    // ===== role_decide：补齐 id + targets 白名单 =====

    fn test_host(responses: Vec<Result<String, EngineError>>) -> (Arc<EngineHost>, Arc<CollectEvents>) {
        let events = Arc::new(CollectEvents::default());
        let host = Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1000)),
            events: events.clone(),
            model: Arc::new(ScriptedModel::new(responses)),
        });
        (host, events)
    }

    fn dummy_profile() -> ModelProfile {
        ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "http://x".into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    #[tokio::test]
    async fn role_decide_fills_ids_and_filters_targets() {
        // 模型返回一个含越界目标（ghost 不在场）的决策。
        let resp = r#"{"intent":"试探","action":"逼近王座","speak":{"willSpeak":true,"purpose":"表态"},"targets":["B","ghost"],"acceptableCosts":["名誉"],"predictions":[]}"#;
        let (host, _ev) = test_host(vec![Ok(resp.to_string())]);
        let prompts = DecidePrompts { system: "你是角色决策器".into(), prompt_version: "v1".into() };
        let active = vec!["A".to_string(), "B".to_string()];
        let d = role_decide(
            host.as_ref(),
            &dummy_profile(),
            &prompts,
            0.0,
            512,
            "run-1",
            0,
            "A",
            "（可见上下文）",
            &active,
            &CancelFlag::new(),
        )
        .await
        .unwrap();

        assert_eq!(d.decision_id, "dec:run-1:0:A");
        assert_eq!(d.character_id, "A");
        // ghost 越界被丢弃，仅保留在场的 B。
        assert_eq!(d.targets, vec!["B".to_string()]);
        assert!(d.speak.will_speak);
        // duration 缺省（模型未给）→ 兜底 DEFAULT_DURATION。
        assert_eq!(d.duration, crate::narrative::DEFAULT_DURATION);
    }

    #[tokio::test]
    async fn role_decide_keeps_move_target_drops_offscene() {
        // Phase 2：移动伪目标 loc:<id> 保留（交仲裁 R6），越界角色目标仍丢弃。
        let resp = r#"{"intent":"转移","action":"前往密室","speak":{"willSpeak":false,"purpose":""},"targets":["loc:密室","ghost"],"acceptableCosts":[],"predictions":[]}"#;
        let (host, _ev) = test_host(vec![Ok(resp.to_string())]);
        let prompts = DecidePrompts { system: "s".into(), prompt_version: "v1".into() };
        let active = vec!["A".to_string()];
        let d = role_decide(
            host.as_ref(),
            &dummy_profile(),
            &prompts,
            0.0,
            512,
            "run-1",
            0,
            "A",
            "ctx",
            &active,
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(d.targets, vec!["loc:密室".to_string()], "loc: 目标保留，ghost 丢弃");
    }

    #[tokio::test]
    async fn role_decide_fills_and_clamps_duration() {
        // P2 DES：模型给 0 / 负 → 兜底 DEFAULT_DURATION；超大 → clamp 到 MAX_DURATION。
        let zero = r#"{"intent":"i","action":"a","speak":{"willSpeak":false,"purpose":""},"targets":[],"duration":0}"#;
        let neg = r#"{"intent":"i","action":"a","speak":{"willSpeak":false,"purpose":""},"targets":[],"duration":-5}"#;
        let huge = r#"{"intent":"i","action":"a","speak":{"willSpeak":false,"purpose":""},"targets":[],"duration":999999999}"#;
        let (host, _ev) = test_host(vec![Ok(zero.into()), Ok(neg.into()), Ok(huge.into())]);
        let prompts = DecidePrompts { system: "s".into(), prompt_version: "v1".into() };
        let d0 = role_decide(
            host.as_ref(), &dummy_profile(), &prompts, 0.0, 512, "run-1", 0, "A", "ctx",
            &["A".to_string()], &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(d0.duration, crate::narrative::DEFAULT_DURATION, "0 应兜底 DEFAULT");
        let dn = role_decide(
            host.as_ref(), &dummy_profile(), &prompts, 0.0, 512, "run-1", 0, "A", "ctx",
            &["A".to_string()], &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(dn.duration, crate::narrative::DEFAULT_DURATION, "负值应兜底 DEFAULT");
        let dh = role_decide(
            host.as_ref(), &dummy_profile(), &prompts, 0.0, 512, "run-1", 0, "A", "ctx",
            &["A".to_string()], &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(dh.duration, crate::narrative::MAX_DURATION, "超大值应 clamp 到 MAX");
    }

    #[tokio::test]
    async fn role_decide_propagates_cancel() {
        let (host, _ev) = test_host(vec![Ok("{}".into())]);
        let prompts = DecidePrompts { system: "s".into(), prompt_version: "v1".into() };
        let cancel = CancelFlag::new();
        cancel.cancel();
        let err = role_decide(
            host.as_ref(),
            &dummy_profile(),
            &prompts,
            0.0,
            512,
            "run-1",
            0,
            "A",
            "ctx",
            &["A".to_string()],
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "cancelled");
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::character::types::{CardLifecycle, Identity};
    use crate::narrative::types::CharacterState;

    fn card(id: &str) -> CharacterCardV2 {
        CharacterCardV2 {
            schema_version: 2,
            id: id.to_string(),
            lifecycle: CardLifecycle::Draft,
            identity: Identity { name: id.to_string(), ..Default::default() },
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
            revision: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// 造一个带 pacingNotes 流水的状态。格式与 `build_patch` 的产出逐字一致：
    /// `{character_id}｜{result:?}｜{consequence}`。
    fn state_with_notes(notes: &[&str]) -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        s.characters.insert("A".into(), CharacterState::default());
        s.characters.insert("B".into(), CharacterState::default());
        s.narrative.pacing_notes = notes.iter().map(|n| n.to_string()).collect();
        s
    }

    fn ctx_of(s: &NarrativeState, who: &str) -> String {
        assemble_visible_context(s, who, &card(who), &BTreeMap::new(), "场景", &[], None, None, &[])
            .unwrap()
    }

    /// 🔴 **信息隔离铁律，记忆这一层同样一步不让**：A 的历史绝不出现在 B 的上下文里。
    ///
    /// `pacingNotes` 是**全局**流水，且历史条目不带地点（无法判断当时谁在场）。
    /// 所以这里只能按 `你的id｜` 前缀严格过滤——任何"顺便也给同组的人看看"的放宽，
    /// 都会让这条铁律出现第一个例外，而这类例外从来不会只有一个。
    #[test]
    fn iron_law_memory_never_leaks_another_characters_history() {
        let s = state_with_notes(&[
            "A｜Success｜A 推开了密室的门",
            "B｜Failure｜B 没能拦住他",
            "A｜Success｜A 拿走了那卷帛书",
        ]);
        let ctx_b = ctx_of(&s, "B");
        assert!(ctx_b.contains("B 没能拦住他"), "自己的历史必须在：{ctx_b}");
        assert!(!ctx_b.contains("推开了密室的门"), "🔴 A 的历史泄漏进了 B 的上下文：{ctx_b}");
        assert!(!ctx_b.contains("拿走了那卷帛书"), "🔴 A 的历史泄漏进了 B 的上下文：{ctx_b}");
    }

    /// 🔴 前缀匹配不得被「id 是另一个 id 的前缀」骗过。
    ///
    /// `A` 与 `AB` 两张卡：按 `starts_with("A")` 会把 AB 的历史算给 A。
    /// 分隔符 `｜` 必须进前缀，这条用例就是钉它的。
    #[test]
    fn a_character_id_that_prefixes_another_does_not_steal_its_memory() {
        let mut s = state_with_notes(&["A｜Success｜甲做的事", "AB｜Success｜乙做的事"]);
        s.characters.insert("AB".into(), CharacterState::default());
        let ctx_a = ctx_of(&s, "A");
        assert!(ctx_a.contains("甲做的事"));
        assert!(!ctx_a.contains("乙做的事"), "🔴 AB 的历史被算给了 A：{ctx_a}");
    }

    /// 反向配对：**没有历史时该字段整块不出现**，产物与接线前逐字节一致。
    ///
    /// 这一条守的是「纯增量」：没有记忆的世界（第一拍、老存档）上下文一个字节都不该变，
    /// 否则黄金世界回归与全部既有 prompt 指纹会跟着红——而那是这一层最不该付的代价。
    #[test]
    fn no_history_means_the_field_is_absent_entirely() {
        let empty = state_with_notes(&[]);
        assert!(!ctx_of(&empty, "A").contains("yourMemory"));
        // 有流水但没有自己的那一条 → 同样不出现（不给一个空数组）。
        let others_only = state_with_notes(&["B｜Success｜与我无关"]);
        assert!(!ctx_of(&others_only, "A").contains("yourMemory"));
    }

    /// 窗口：只带最近 `MEMORY_WINDOW` 条，且**保留的是最新的、丢的是最旧的**。
    ///
    /// 🔴 方向不能反：丢新留旧会把「刚刚发生的冲突」挤出视野，
    /// 而近期记忆恰恰是最影响当下决策的那一批（同 `pacingNotes` 自身滚动截断的取向）。
    #[test]
    fn the_window_keeps_the_newest_and_drops_the_oldest() {
        let notes: Vec<String> =
            (1..=MEMORY_WINDOW + 5).map(|i| format!("A｜Success｜第{i}件事")).collect();
        let refs: Vec<&str> = notes.iter().map(|s| s.as_str()).collect();
        let ctx = ctx_of(&state_with_notes(&refs), "A");
        assert!(ctx.contains(&format!("第{}件事", MEMORY_WINDOW + 5)), "最新的必须在：{ctx}");
        assert!(!ctx.contains("第1件事"), "🔴 最旧的应被丢掉，实得：{ctx}");
    }

    /// 顺序：按发生先后排列（旧 → 新）。
    ///
    /// 倒序会让模型把最后一条读成"最早发生的"，而因果次序是这段记忆的全部意义。
    #[test]
    fn memory_is_ordered_oldest_first() {
        let ctx = ctx_of(&state_with_notes(&["A｜Success｜先", "A｜Success｜后"]), "A");
        let i_first = ctx.find("先").expect("应含第一条");
        let i_second = ctx.find("后").expect("应含第二条");
        assert!(i_first < i_second, "记忆必须按发生先后排列：{ctx}");
    }

    /// 🔴 记忆是**记忆不是命令**：note 里必须明说"怎么用由你的性格决定"。
    ///
    /// 少了这句话，模型会把历史读成"上级指示"——「上次失败了，所以这次要成功」，
    /// 而那正好抹掉角色的性格：一个执拗的人本来就该在同一个地方栽第二次。
    #[test]
    fn memory_is_framed_as_recollection_not_instruction() {
        let ctx = ctx_of(&state_with_notes(&["A｜Failure｜没拦住"]), "A");
        assert!(ctx.contains("不是命令"), "note 必须把记忆与命令区分开：{ctx}");
    }
}
