//! 行动仲裁（规格 §12.3）：规则层优先（无模型），规则不能裁决的交模型（0–1 次调用）。
//! 文件所有权：agent-E4。
//!
//! 边界：不改写角色意图原文；输出含规则依据；硬节点与角色底线冲突时可调整事件实现或
//! 返回 Blocked，不能悄悄替角色改主意。状态变化统一交 reducer（本模块不产生 StatePatch）。

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::character::types::CharacterCardV2;
use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{
    ArbiterOutcome, ArbiterResult, ConstraintLevel, LocationDef, LocationGate, NarrativeState,
    NodeStatus, RoleDecision,
};

/// 移动伪目标前缀（Phase 2）：decision.targets 中 `loc:<id>` 表示「移动到地点 id」的意图。
/// 与角色目标区分——R2 在场校验跳过它，R6 据它判定连通/准入。
pub const LOC_TARGET_PREFIX: &str = "loc:";

/// 解析移动目标：targets 中首个 `loc:<id>` 的 `<id>`（无则非移动决策）。
fn move_dest(d: &RoleDecision) -> Option<String> {
    d.targets.iter().find_map(|t| t.strip_prefix(LOC_TARGET_PREFIX).map(|s| s.to_string()))
}

fn is_loc_target(t: &str) -> bool {
    t.starts_with(LOC_TARGET_PREFIX)
}

/// R6b 秘境准入（引擎侧纯函数）：required_item_ids ⊆ held ∧ required_effect_tags ⊆ held。
/// held = 角色 resources（约定道具以 `item:<id>`、标签以 `tag:<t>` 或裸值承载）。
/// 体系(cosmology)/强度(power_tier)闸门需 per-item 元数据，引擎无此上下文——留待 server 侧
/// check_location_admission 在物化 held cosmology/tier 后强化（Phase 3）；此处只判引擎可确定性验证的持有闸。
fn gate_admits(gate: &LocationGate, held: &[String]) -> bool {
    let holds = |needle: &str| {
        held.iter().any(|h| {
            let h = h.as_str();
            h == needle
                || h.strip_prefix("item:").map(|x| x == needle).unwrap_or(false)
                || h.strip_prefix("tag:").map(|x| x == needle).unwrap_or(false)
        })
    };
    gate.required_item_ids.iter().all(|id| holds(id))
        && gate.required_effect_tags.iter().all(|t| holds(t))
}

pub struct ArbiterPrompts {
    pub system: String,
    pub prompt_version: String,
}

/// 仲裁模型层默认输出上限（结构化裁决，不需要长文本）。
const ARBITER_MAX_TOKENS: u32 = 1500;

fn outcome(d: &RoleDecision, result: ArbiterResult, rule: &str, consequence: &str) -> ArbiterOutcome {
    ArbiterOutcome {
        decision_id: d.decision_id.clone(),
        character_id: d.character_id.clone(),
        result,
        rule_refs: vec![rule.to_string()],
        consequence: consequence.to_string(),
    }
}

/// R1 资源约束：捕捉「动用/消耗/花费…X」类明确的资源消耗声明，若 X 与角色 resources 均不匹配 → 违规。
/// 保守：仅匹配明确的耗用动词；无匹配则不判违规（交后续规则/模型）。
fn violates_resource(state: &NarrativeState, d: &RoleDecision, res_re: &Regex) -> bool {
    let owned: &[String] = state
        .characters
        .get(&d.character_id)
        .map(|c| c.resources.as_slice())
        .unwrap_or(&[]);
    for cap in res_re.captures_iter(&d.action) {
        let object = cap.get(2).map(|m| m.as_str()).unwrap_or("").trim();
        if object.is_empty() {
            continue;
        }
        let matched = owned.iter().any(|r| {
            let r = r.trim();
            !r.is_empty() && (r.contains(object) || object.contains(r))
        });
        if !matched {
            return true; // 声明动用了一项并不持有的资源。
        }
    }
    false
}

/// R3 读心/强制他人：直接获取他人内心/秘密，或强迫他人吐露私密。保守匹配明确句式。
fn violates_mind_control(d: &RoleDecision, coerce_re: &Regex, read_re: &Regex) -> bool {
    coerce_re.is_match(&d.action) || read_re.is_match(&d.action)
}

/// 规则层（纯函数）：
/// R1 资源约束：action 引用了 resources 中不存在的资源 → Invalid("rule:resource")
/// R2 目标在场：targets 必须都在活跃角色集合 → 越界 Invalid("rule:target")
/// R3 读心/强制他人：action 含对他人内心/秘密的直接获取或强制他人行动 → Invalid("rule:mind_control")
///    （启发式：正则匹配「让/命令/迫使 X 说出/交出 + 秘密/心里」类模式；保守宁可漏判交模型层）
/// R4 同目标冲突：多个决策争夺同一独占目标 → 全部标记 needs_model
/// R5 硬节点保护：action 明显使当前 Pending 硬节点不可能发生 → needs_model（模型层裁决实现调整或 Blocked）
/// 返回：已裁决结果 + 需模型层的决策列表。
///
/// 设计：R1/R2/R3 命中即 Invalid（进 resolved）；干净且无冲突/无硬节点威胁的决策由规则层直接判 Success；
/// 只有 R4（冲突）或 R5（硬节点威胁）的决策进入 pending（交模型层），保证仲裁调用 0–1 次。
///
/// Phase 2 分组语义：调用方逐地点组调用——`active_character_ids` 为**同组在场集**（R2 据此判「目标不在场」，
/// 跨地点角色目标即越界）；`locations` 为本回合地点图（R6 移动合法性据此判连通/准入）。
/// `locations` 空 = 无地点维度，退化为「无移动决策」——R6 不触发，行为与 Phase 1 一致。
pub fn rule_arbitrate(
    state: &NarrativeState,
    decisions: &[RoleDecision],
    active_character_ids: &[String],
    locations: &BTreeMap<String, LocationDef>,
) -> (Vec<ArbiterOutcome>, Vec<RoleDecision>) {
    // 本函数内 4 个 `Regex::new(...).unwrap()` 的 pattern 全是字面量、不含任何运行期输入
    //（决策文本只进 `is_match`/`captures`）→ 失败仅可能是模式写错，属编译期可知，unwrap 静态安全。
    let res_re = Regex::new(r"(动用|消耗|花费|拿出|掏出|支付)([^\s，。、；：！？…,.!?（）()]{1,8})").unwrap();
    let coerce_re = Regex::new(
        r"(让|命令|迫使|逼迫|逼|强迫|胁迫).{0,12}(说出|交出|供出|坦白|招供|吐露).{0,8}(秘密|真相|心里|心事|底细|隐私|下落)",
    )
    .unwrap();
    let read_re =
        Regex::new(r"(读取|窥探|看穿|洞悉|读心|偷看).{0,8}(内心|心里|想法|秘密|心思)").unwrap();

    let active: BTreeSet<&str> = active_character_ids.iter().map(|s| s.as_str()).collect();

    // R4 预计算：出现在 ≥2 个决策 targets 中的目标视为被争夺（移动伪目标 loc:<id> 不计——
    // 多人同赴一地不是资源争夺）。
    let mut target_count: BTreeMap<&str, usize> = BTreeMap::new();
    for d in decisions {
        for t in &d.targets {
            if is_loc_target(t) {
                continue;
            }
            *target_count.entry(t.as_str()).or_default() += 1;
        }
    }
    let conflict_targets: BTreeSet<&str> =
        target_count.iter().filter(|(_, c)| **c >= 2).map(|(t, _)| *t).collect();

    // R5 预计算：是否存在待推进的硬节点。
    let has_pending_hard = state
        .narrative
        .outline_nodes
        .iter()
        .any(|n| n.constraint == ConstraintLevel::Hard && n.status == NodeStatus::Pending);
    let irreversible_re = Regex::new(
        r"(杀死|杀掉|杀了|处死|毒死|毁掉|摧毁|炸毁|烧毁|销毁|终止|放弃|背叛|叛变|叛逃|自尽|同归于尽)",
    )
    .unwrap();

    // 定序：按 character_id、decision_id 排序，保证确定性输出（§12.5.3）。
    let mut ordered: Vec<&RoleDecision> = decisions.iter().collect();
    ordered.sort_by(|a, b| a.character_id.cmp(&b.character_id).then(a.decision_id.cmp(&b.decision_id)));

    let mut resolved: Vec<ArbiterOutcome> = Vec::new();
    let mut pending: Vec<RoleDecision> = Vec::new();

    for d in ordered {
        // R1
        if violates_resource(state, d, &res_re) {
            resolved.push(outcome(d, ArbiterResult::Invalid, "rule:resource", "行动动用了未持有的资源，无法执行"));
            continue;
        }
        // R2 目标在场：仅校验角色目标；移动伪目标 loc:<id> 交 R6，跨地点角色目标（不在同组在场集）判越界。
        if d.targets.iter().filter(|t| !is_loc_target(t)).any(|t| !active.contains(t.as_str())) {
            resolved.push(outcome(d, ArbiterResult::Invalid, "rule:target", "行动目标不在场，无法执行"));
            continue;
        }
        // R3
        if violates_mind_control(d, &coerce_re, &read_re) {
            resolved.push(outcome(
                d,
                ArbiterResult::Invalid,
                "rule:mind_control",
                "不能直接读取或强取他人私密（信息边界）",
            ));
            continue;
        }

        // R6 移动合法性（Phase 2）：移动是终态裁决（不落入 R4/R5）。
        // R6a 连通：目标 ∈ 当前地点 connections；R6b 准入：目标 gate（秘境）须放行。
        if let Some(dest) = move_dest(d) {
            let cur_loc =
                state.characters.get(&d.character_id).map(|c| c.location.as_str()).unwrap_or("");
            let connected = locations
                .get(cur_loc)
                .map(|l| l.connections.iter().any(|c| c == &dest))
                .unwrap_or(false);
            if !connected {
                resolved.push(outcome(
                    d,
                    ArbiterResult::Invalid,
                    "rule:move_unreachable",
                    "无法从当前位置抵达该地点",
                ));
                continue;
            }
            if let Some(gate) = locations.get(&dest).and_then(|l| l.gate.as_ref()) {
                let held = state
                    .characters
                    .get(&d.character_id)
                    .map(|c| c.resources.as_slice())
                    .unwrap_or(&[]);
                if !gate_admits(gate, held) {
                    resolved.push(outcome(
                        d,
                        ArbiterResult::Invalid,
                        "rule:move_admission",
                        "未满足秘境准入条件",
                    ));
                    continue;
                }
            }
            resolved.push(outcome(d, ArbiterResult::Success, "rule:move", &format!("移动到「{dest}」")));
            continue;
        }

        // R4 / R5：需模型层裁决结果与意外后果。
        let conflict = d.targets.iter().any(|t| conflict_targets.contains(t.as_str()));
        let threatens_hard = has_pending_hard && irreversible_re.is_match(&d.action);
        if conflict || threatens_hard {
            pending.push(d.clone());
        } else {
            // 干净决策：规则层直接判可行，避免不必要的模型调用。
            resolved.push(outcome(d, ArbiterResult::Success, "rule:clear", "行动可行，照常推进"));
        }
    }

    (resolved, pending)
}

// ============================================================================
// R7 底线硬约束（总规格 §7「人设保险」三级出口第 1 级 · 事前预防）
// ============================================================================
//
// 规格原文：卡的 `bottomLines` / `refusalRules` / `immutableCore` 升级为仲裁硬约束——
// 角色行为违反自己卡的底线 → 仲裁拒绝该提案、重新生成。
//
// **为什么检测放在规则层而不是模型层**：拦不拦得住必须**可 replay**（同一份脚本重跑两次
// 逐字节相同）。把「是否违反底线」交给模型判，等于把一个会改变提交内容的分支挂在模型输出上，
// replay 契约当场破裂；而「重新生成」本身才是模型该干的活（在底线硬约束下重出提案），
// 它的结果又会被同一个确定性闸重新校验一遍。故分工是：
//   **规则层判「拦不拦」（确定性、零新增成本）→ 模型层做「怎么改」（重新生成，有上限）**。
//
// **为什么不做成 `rule_arbitrate` 的第七条 if**：`rule_arbitrate` 的入参里没有角色卡
// （它只吃 `NarrativeState`），加参数会让全部既有调用点与用例被迫改签名，违反
// 「卡没声明底线时行为与用例一个字都不变」。故独立成纯函数，由 `run_round` 在仲裁步骤起手调用，
// 命中者直接落 `Invalid` 并**不再进入** R1–R6（提案已死，不必再判资源/冲突，也不进模型层 pending）。
//
// **误伤控制**（底线是玩家写的自由文本，可能写得极宽）三道闸，见各常量注释：
//   ①过宽护栏（含「任何/所有/一切…」的条目不进规则层）②最短片段门槛 ③否定回看窗口。
// 三闸的共同取向与 R1/R3 一致——**宁可漏判，绝不误伤**：漏掉的交模型重新生成时的底线回执、
// 交 critic 事后建议（第 2/3 级出口），而误伤会让角色什么都做不了、世界卡死。

/// 底线拦截写入 `ArbiterOutcome.rule_refs` 的规则标记（透明战报可见「为什么这一步没发生」）。
pub const BOTTOM_LINE_RULE_REF: &str = "rule:bottom_line";

/// 规则层可判定的「禁止行为」最短片段（Unicode 字符数）。**误伤控制闸②**：
/// 低于此长度的底线（「不逃」→「逃」、「不杀人」→「杀人」）字面匹配会大面积误伤
/// （「逃出火场」也算「逃」），一律不进规则层。**参数化集中点**（VALIDATION §0.2）。
const MIN_FORBIDDEN_CHARS: usize = 3;

/// 单角色进入规则层的底线条目上限。**参数化集中点**：防有人往卡里堆几千条底线拖垮每拍筛查
/// （成本工程 §17），也防「底线堆量 = 拦得更多 = 变相优势」。超出部分按声明序截断（确定性）。
const MAX_BOTTOM_LINES_PER_CHARACTER: usize = 32;

/// 否定回看窗口（Unicode 字符数）。**误伤控制闸③**：命中片段之前这么多个字里若出现否定字，
/// 视为「角色正在**拒绝**做这件事」（"我不会做伪证"/"拒绝做伪证"），不算违反底线——
/// 恰恰相反，那是底线在起作用。**参数化集中点**。
const NEGATION_WINDOW_CHARS: usize = 3;

/// 底线里引出「被禁止行为」的否定标记（按长度降序匹配，取最长）。
/// 只有以否定式书写的底线才进规则层：正向承诺式（「答应过的路一定走完」）无法确定性判定
/// 「违反」，一律交模型层与 critic。
/// 注：刻意不收「不做」「不肯」这类会把动词一并吃掉的组合（「不做伪证」→ 片段应是「做伪证」
/// 而不是「伪证」），只收纯否定成分。
const NEGATION_MARKERS: &[&str] = &[
    "绝对不能",
    "绝对不会",
    "绝对不",
    "永远不会",
    "永远不",
    "从来不会",
    "从来不",
    "无论如何不",
    "绝不会",
    "绝不能",
    "决不会",
    "决不能",
    "不可以",
    "再也不",
    "绝不",
    "决不",
    "永不",
    "从不",
    "不得",
    "不能",
    "不会",
    "不可",
    "不许",
    "不准",
    "不该",
    "不应",
    "不再",
    "拒绝",
    "禁止",
    "不",
];

/// 行动文本里表示「没有做/拒绝做」的否定字（用于否定回看窗口）。
/// 刻意不收「非」（"非要做…" 是坚持要做，收了会漏判）。
const NEGATION_CHARS: &[char] = &['不', '没', '未', '别', '莫', '勿', '拒', '绝', '甭', '无'];

/// 过宽限定词。**误伤控制闸①**：含这些词的底线（"绝不伤害任何人"）覆盖面等于全世界，
/// 一旦按字面拦截，角色将什么都做不了、世界卡死。这类条目**不进规则层**——
/// 它仍然会作为角色自己卡上的内容进决策上下文（角色自己会照着演），也仍然被 critic 事后审校，
/// 只是不作为确定性硬拦截的依据。**参数化集中点**。
const OVERBROAD_MARKERS: &[&str] =
    &["任何", "所有", "一切", "全部", "任谁", "谁都", "什么都", "凡是", "无论"];

/// 可剥离的前置虚词（单字）：把「在背后出刀」收窄为「背后出刀」，令「从背后出刀」也能命中。
/// 只剥一个字、且剥完仍须满足 `MIN_FORBIDDEN_CHARS`，不改变行为语义。
const STRIPPABLE_LEADING: &[char] = &[
    '在', '从', '对', '向', '跟', '同', '与', '把', '将', '往', '朝', '给', '替', '为', '去', '要',
    '会', '能', '肯', '愿', '再', '被', '让',
];

/// 一条被违反的底线（事前筛查产物）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BottomLineHit {
    pub character_id: String,
    pub decision_id: String,
    /// 被违反的底线**原文**（回注给角色重新决策，也进战报）。
    pub line: String,
    /// 命中的禁止行为片段（诊断用，便于运营看误伤）。
    pub matched: String,
}

/// 取一张卡声明的全部底线（规格 §7 的三处合一）：
/// `dramaticCore.bottomLines` → `agency.refusalRules` → `growthArc.immutableCore`。
/// 顺序固定（字段序 → 各字段内声明序），去重保留首次出现，截断到 `MAX_BOTTOM_LINES_PER_CHARACTER`。
/// 纯函数、全序可复现。
pub fn card_bottom_lines(card: &CharacterCardV2) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in card
        .dramatic_core
        .bottom_lines
        .iter()
        .chain(card.agency.refusal_rules.iter())
        .chain(card.growth_arc.immutable_core.iter())
    {
        let line = raw.trim();
        if line.is_empty() || out.iter().any(|x| x == line) {
            continue;
        }
        out.push(line.to_string());
        if out.len() >= MAX_BOTTOM_LINES_PER_CHARACTER {
            break;
        }
    }
    out
}

/// 活跃卡集合 → 底线表（`character_id → 底线原文`，BTreeMap 键有序）。
/// **没有任何一张卡声明底线 → 返回空表**，`run_round` 据此整段短路，默认路径零开销、零行为变化。
pub fn collect_bottom_lines(
    cards: &BTreeMap<String, CharacterCardV2>,
) -> BTreeMap<String, Vec<String>> {
    cards
        .iter()
        .filter_map(|(cid, card)| {
            let lines = card_bottom_lines(card);
            if lines.is_empty() {
                None
            } else {
                Some((cid.clone(), lines))
            }
        })
        .collect()
}

/// 在 `line` 中定位第一个否定标记，返回 `(字节起点, 标记)`；同一位置取最长匹配。
fn first_negation(line: &str) -> Option<(usize, &'static str)> {
    for (idx, _) in line.char_indices() {
        // NEGATION_MARKERS 已按长度降序书写：同一位置命中的第一条即最长匹配。
        if let Some(m) = NEGATION_MARKERS.iter().find(|m| line[idx..].starts_with(**m)) {
            return Some((idx, m));
        }
    }
    None
}

/// 由一条底线原文解析出可供字面匹配的「禁止行为」片段（可能为空 = 该条不进规则层）。
/// 纯函数：过宽护栏 → 定位否定标记 → 取其后的行为片段 → 生成 ≤2 个片段（原片段 + 剥前置虚词）。
fn forbidden_needles(line: &str) -> Vec<String> {
    // 闸①：过宽条目不进规则层。
    if OVERBROAD_MARKERS.iter().any(|m| line.contains(m)) {
        return Vec::new();
    }
    // 只认否定式书写；正向承诺式无法确定性判定「违反」。
    let Some((idx, marker)) = first_negation(line) else {
        return Vec::new();
    };
    let pred = line[idx + marker.len()..].trim();
    let mut needles: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        // 闸②：最短片段门槛。
        if s.chars().count() >= MIN_FORBIDDEN_CHARS && !needles.iter().any(|x| x == s) {
            needles.push(s.to_string());
        }
    };
    push(pred);
    let mut chars = pred.chars();
    if let Some(first) = chars.next() {
        if STRIPPABLE_LEADING.contains(&first) {
            push(chars.as_str());
        }
    }
    needles
}

/// 命中片段之前 `NEGATION_WINDOW_CHARS` 个字内是否出现否定字（闸③）。
fn negated_before(action: &str, pos: usize) -> bool {
    action[..pos].chars().rev().take(NEGATION_WINDOW_CHARS).any(|c| NEGATION_CHARS.contains(&c))
}

/// `action` 是否**明白无误地重述**了某个被禁止的行为：命中片段且该处未被否定。
/// 返回命中的片段。确定性：片段按固定顺序遍历，出现位置从左到右。
fn action_restates(action: &str, needles: &[String]) -> Option<String> {
    for needle in needles {
        let mut from = 0usize;
        while let Some(rel) = action[from..].find(needle.as_str()) {
            let pos = from + rel;
            if !negated_before(action, pos) {
                return Some(needle.clone());
            }
            from = pos + needle.len();
        }
    }
    None
}

/// 事前底线筛查（纯函数，确定性，无模型调用）。
///
/// 只看 `action`（规格说的是「角色**行为**违反自己卡的底线」）——不看 `intent`：
/// 内心动过念头不等于做了，按念头拦会把「挣扎」这种最好的戏也拦掉。
///
/// 每条决策至多产 1 条命中（取该角色**首条**被违反的底线，按 `card_bottom_lines` 的固定序），
/// 输出按 `(character_id, decision_id)` 定序（§12.5.3）。
pub fn screen_bottom_lines(
    bottom_lines: &BTreeMap<String, Vec<String>>,
    decisions: &[RoleDecision],
) -> Vec<BottomLineHit> {
    let mut hits: Vec<BottomLineHit> = Vec::new();
    for d in decisions {
        let Some(lines) = bottom_lines.get(&d.character_id) else {
            continue;
        };
        for line in lines {
            let needles = forbidden_needles(line);
            if needles.is_empty() {
                continue;
            }
            if let Some(matched) = action_restates(&d.action, &needles) {
                hits.push(BottomLineHit {
                    character_id: d.character_id.clone(),
                    decision_id: d.decision_id.clone(),
                    line: line.clone(),
                    matched,
                });
                break;
            }
        }
    }
    hits.sort_by(|a, b| a.character_id.cmp(&b.character_id).then(a.decision_id.cmp(&b.decision_id)));
    hits
}

/// 底线拦截后的 `consequence` 文案（**参数化集中点**，与 `SANCTUARY_DOWNGRADE_CONSEQUENCE` 同规格）。
///
/// 该文本同时是**给写作环节的指令**——`call_writer` 把 `consequence` 原样喂给写手，若不显式否定，
/// 写手会顺着决策原文把违背底线的行为写成已发生；而正文即公共事实，一旦写出就不可回滚（§0.3）。
/// 它还会随 `SceneRecord.outcomes` 进战报供玩家直读（但**不**进 `pacingNotes`／`DomainEvent`——
/// `Invalid` 在 `build_patch`/`build_events` 里被整条跳过，被拦的行动不留任何公共事实），
/// 故写成可直读的中文陈述句，不夹排版记号。
///
/// 🔴 平权（§0.1）：末句显式声明「底线不是优势」——拦截只否决提案，绝不改判成功率。
fn bottom_line_consequence(line: &str) -> String {
    format!(
        "此行动触到该角色自己的底线「{line}」，因而并未发生：他在做下去之前收住了。\
正文不得把该行动写成已经发生，只可写他临到跟前又停下\
（底线说的是这个角色不会做什么，不是他更强、更容易成功或能少付代价）"
    )
}

/// 命中 → 仲裁拒绝裁决。`Invalid` 在下游是**完全惰性**的：不产 `ActionResolved`、不进 `StatePatch`、
/// 不推关系、不计里程碑强度、不进生死降级与同意门控——正好等于「拒绝该提案」，且**不改判成功率**。
pub fn bottom_line_outcome(hit: &BottomLineHit) -> ArbiterOutcome {
    ArbiterOutcome {
        decision_id: hit.decision_id.clone(),
        character_id: hit.character_id.clone(),
        result: ArbiterResult::Invalid,
        rule_refs: vec![BOTTOM_LINE_RULE_REF.to_string()],
        consequence: bottom_line_consequence(&hit.line),
    }
}

/// 模型层输出（宽松解析：result 缺省视为 success）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOutcome {
    #[serde(default)]
    decision_id: String,
    #[serde(default)]
    result: Option<ArbiterResult>,
    #[serde(default)]
    consequence: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArbiterBatch {
    #[serde(default)]
    outcomes: Vec<RawOutcome>,
}

fn build_arbiter_user_prompt(state: &NarrativeState, situation: &str, pending: &[RoleDecision]) -> String {
    let hard_nodes: Vec<Value> = state
        .narrative
        .outline_nodes
        .iter()
        .filter(|n| n.constraint == ConstraintLevel::Hard && n.status == NodeStatus::Pending)
        .map(|n| json!({ "id": n.id, "summary": n.summary }))
        .collect();
    let items: Vec<Value> = pending
        .iter()
        .map(|d| {
            json!({
                "decisionId": d.decision_id,
                "characterId": d.character_id,
                "intent": d.intent,
                "action": d.action,
                "targets": d.targets,
            })
        })
        .collect();
    format!(
        "局势：{situation}\n待推进硬节点：{hard}\n待裁决行动（互相冲突或可能危及硬节点）：{items}\n\n\
你是行动仲裁器：只裁决可行性、冲突结果与意外后果，绝不改写任何角色的 intent 原文。\
result 取值：success/partialSuccess/failure/invalid/blocked。\
若某行动与硬节点或角色底线冲突且无法调整实现，则该项 result=blocked 并在 consequence 说明冲突。\
每个 decisionId 必须来自上面给定集合，一一给出裁决。严格输出 JSON：\n\
{{\"outcomes\":[{{\"decisionId\":\"...\",\"result\":\"success\",\"consequence\":\"简述结果与后果\"}}]}}",
        hard = serde_json::to_string(&hard_nodes).unwrap_or_default(),
        items = serde_json::to_string(&items).unwrap_or_default(),
    )
}

/// 模型层：一次调用裁决剩余决策的结果与意外后果；输出 decision_id 必须 ⊆ 输入集合（引用完整性）。
#[allow(clippy::too_many_arguments)] // 签名由骨架固定
pub async fn model_arbitrate(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &ArbiterPrompts,
    run_id: &str,
    state: &NarrativeState,
    situation: &str,
    pending: &[RoleDecision],
    cancel: &CancelFlag,
) -> Result<Vec<ArbiterOutcome>, EngineError> {
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    let spec = ModelCallSpec {
        max_retries: None,
        profile: profile.clone(),
        system: prompts.system.clone(),
        user: build_arbiter_user_prompt(state, situation, pending),
        temperature: 0.0, // 裁决类：确定性
        max_output_tokens: ARBITER_MAX_TOKENS,
        agent: "arbiter".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };

    let batch: ArbiterBatch =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;

    // 引用完整性：只接受 decision_id ∈ pending 的裁决；其余丢弃。
    let pending_ids: BTreeSet<&str> = pending.iter().map(|d| d.decision_id.as_str()).collect();
    let mut by_id: BTreeMap<String, (ArbiterResult, String)> = BTreeMap::new();
    for o in batch.outcomes {
        if pending_ids.contains(o.decision_id.as_str()) {
            by_id.insert(o.decision_id.clone(), (o.result.unwrap_or(ArbiterResult::Success), o.consequence));
        }
    }

    // 覆盖每个 pending 决策（模型漏判则回退 Success）；character_id 以本地决策为准，防篡改。
    let mut out: Vec<ArbiterOutcome> = Vec::with_capacity(pending.len());
    for d in pending {
        let (result, consequence) =
            by_id.get(&d.decision_id).cloned().unwrap_or((ArbiterResult::Success, String::new()));
        out.push(ArbiterOutcome {
            decision_id: d.decision_id.clone(),
            character_id: d.character_id.clone(),
            result,
            rule_refs: vec!["model:arbiter".to_string()],
            consequence,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use crate::narrative::types::{
        CharacterState, OutlineNode, SpeakIntent,
    };
    use std::sync::Arc;

    fn decision(id: &str, cid: &str, action: &str, targets: Vec<&str>) -> RoleDecision {
        RoleDecision {
            decision_id: id.to_string(),
            character_id: cid.to_string(),
            intent: "意图".into(),
            action: action.to_string(),
            speak: SpeakIntent { will_speak: false, purpose: String::new() },
            targets: targets.into_iter().map(String::from).collect(),
            acceptable_costs: vec![],
            predictions: vec![],
            duration: 0,
        }
    }

    fn base_state() -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        s.characters.insert("li".into(), CharacterState::default());
        s.characters.insert("wang".into(), CharacterState::default());
        s
    }

    fn active() -> Vec<String> {
        vec!["li".to_string(), "wang".to_string()]
    }

    /// 无地点维度（退化路径）：locations 空 → R6 不触发，行为与 Phase 1 一致。
    fn no_locations() -> BTreeMap<String, LocationDef> {
        BTreeMap::new()
    }

    fn move_decision(id: &str, cid: &str, dest: &str) -> RoleDecision {
        decision(id, cid, &format!("前往{dest}"), vec![&format!("loc:{dest}")])
    }

    // ===== R1 资源约束 =====

    #[test]
    fn r1_rejects_unowned_resource() {
        let s = base_state(); // li 无任何 resources
        let d = decision("d1", "li", "动用禁军包围皇宫", vec![]);
        let (resolved, pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert!(pending.is_empty());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:resource".to_string()]);
    }

    #[test]
    fn r1_allows_owned_resource() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().resources.push("禁军".into());
        let d = decision("d1", "li", "动用禁军包围皇宫", vec![]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        // 持有该资源 → 不因 R1 违规；干净决策判 Success。
        assert_eq!(resolved[0].result, ArbiterResult::Success);
    }

    // ===== R2 目标在场 =====

    #[test]
    fn r2_rejects_offscene_target() {
        let s = base_state();
        let d = decision("d1", "li", "攻击对方", vec!["ghost"]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:target".to_string()]);
    }

    // ===== R3 读心 / 强制他人 =====

    #[test]
    fn r3_rejects_coercing_secret() {
        let s = base_state();
        let d = decision("d1", "li", "命令王五说出他的秘密", vec![]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:mind_control".to_string()]);
    }

    #[test]
    fn r3_rejects_mind_reading() {
        let s = base_state();
        let d = decision("d1", "li", "窥探对方的内心想法", vec![]);
        let (resolved, _pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:mind_control".to_string()]);
    }

    // ===== R4 同目标冲突 =====

    #[test]
    fn r4_conflicting_target_goes_to_model() {
        let s = base_state();
        let d1 = decision("d1", "li", "抢夺王座", vec!["throne_holder"]);
        let d2 = decision("d2", "wang", "抢夺王座", vec!["throne_holder"]);
        // 目标须在场，加入 active 集合。
        let act = vec!["li".to_string(), "wang".to_string(), "throne_holder".to_string()];
        let (resolved, pending) = rule_arbitrate(&s, &[d1, d2], &act, &no_locations());
        assert!(resolved.is_empty(), "冲突决策不应被规则层直接判定");
        assert_eq!(pending.len(), 2);
    }

    // ===== R5 硬节点保护 =====

    #[test]
    fn r5_irreversible_near_hard_node_goes_to_model() {
        let mut s = base_state();
        s.narrative.outline_nodes.push(OutlineNode {
            id: "n1".into(),
            summary: "主角与对手决战".into(),
            constraint: ConstraintLevel::Hard,
            status: NodeStatus::Pending,
            threshold: None,
            advance_when: None,
            weights: None,
        });
        let d = decision("d1", "li", "杀死关键人物王五", vec![]);
        let (resolved, pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert!(resolved.is_empty());
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn clean_decision_auto_success_without_model() {
        let s = base_state();
        let d = decision("d1", "li", "礼貌地上前问候", vec![]);
        let (resolved, pending) = rule_arbitrate(&s, &[d], &active(), &no_locations());
        assert!(pending.is_empty(), "干净决策不需要模型层");
        assert_eq!(resolved[0].result, ArbiterResult::Success);
    }

    #[test]
    fn deterministic_ordering_of_resolved() {
        let s = base_state();
        // 乱序输入，输出应按 character_id 定序。
        let d2 = decision("d2", "wang", "问候", vec![]);
        let d1 = decision("d1", "li", "问候", vec![]);
        let (resolved, _p) = rule_arbitrate(&s, &[d2, d1], &active(), &no_locations());
        assert_eq!(resolved[0].character_id, "li");
        assert_eq!(resolved[1].character_id, "wang");
    }

    // ===== R6 移动合法性（Phase 2）：连通 + 秘境准入 =====

    fn locmap(defs: Vec<LocationDef>) -> BTreeMap<String, LocationDef> {
        defs.into_iter().map(|d| (d.id.clone(), d)).collect()
    }

    fn loc(id: &str, connections: &[&str]) -> LocationDef {
        LocationDef {
            id: id.into(),
            name: id.into(),
            connections: connections.iter().map(|s| s.to_string()).collect(),
            is_secret_realm: false,
            gate: None,
        }
    }

    #[test]
    fn r6_move_to_connected_location_succeeds() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().location = "前厅".into();
        let locs = locmap(vec![loc("前厅", &["密室"]), loc("密室", &["前厅"])]);
        let d = move_decision("d1", "li", "密室");
        let (resolved, pending) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert!(pending.is_empty(), "移动是终态裁决，不进模型层");
        assert_eq!(resolved[0].result, ArbiterResult::Success);
        assert_eq!(resolved[0].rule_refs, vec!["rule:move".to_string()]);
    }

    #[test]
    fn r6_move_to_unconnected_location_is_invalid() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().location = "前厅".into();
        // 前厅只连密室，不连塔顶。
        let locs = locmap(vec![loc("前厅", &["密室"]), loc("塔顶", &[])]);
        let d = move_decision("d1", "li", "塔顶");
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:move_unreachable".to_string()]);
    }

    #[test]
    fn r6_secret_realm_admission_denied_without_item() {
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().location = "前厅".into();
        // li 无 resources；秘境 gate 需玉钥。
        let mut secret = loc("秘境", &[]);
        secret.is_secret_realm = true;
        secret.gate =
            Some(LocationGate { required_item_ids: vec!["玉钥".into()], ..Default::default() });
        let locs = locmap(vec![loc("前厅", &["秘境"]), secret]);
        let d = move_decision("d1", "li", "秘境");
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:move_admission".to_string()]);
    }

    #[test]
    fn r6_secret_realm_admission_passes_with_item() {
        let mut s = base_state();
        {
            let li = s.characters.get_mut("li").unwrap();
            li.location = "前厅".into();
            li.resources.push("item:玉钥".into()); // 持有玉钥
        }
        let mut secret = loc("秘境", &[]);
        secret.is_secret_realm = true;
        secret.gate =
            Some(LocationGate { required_item_ids: vec!["玉钥".into()], ..Default::default() });
        let locs = locmap(vec![loc("前厅", &["秘境"]), secret]);
        let d = move_decision("d1", "li", "秘境");
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &locs);
        assert_eq!(resolved[0].result, ArbiterResult::Success, "持有准入道具 → 放行");
    }

    #[test]
    fn r6_cross_group_character_target_is_invalid() {
        // R2 同组在场重定义：active = 同组 {li}，li 攻击不在同组的 wang（跨地点目标）→ 越界 Invalid。
        let s = base_state();
        let d = decision("d1", "li", "攻击王五", vec!["wang"]);
        let (resolved, _p) = rule_arbitrate(&s, &[d], &["li".to_string()], &no_locations());
        assert_eq!(resolved[0].result, ArbiterResult::Invalid);
        assert_eq!(resolved[0].rule_refs, vec!["rule:target".to_string()]);
    }

    // ===== 模型层 =====

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

    fn prompts() -> ArbiterPrompts {
        ArbiterPrompts { system: "你是仲裁器".into(), prompt_version: "v1".into() }
    }

    #[tokio::test]
    async fn model_arbitrate_no_call_when_empty() {
        let (host, ev) = test_host(vec![]);
        let s = base_state();
        let out = model_arbitrate(
            host.as_ref(),
            &dummy_profile(),
            &prompts(),
            "run-1",
            &s,
            "局势",
            &[],
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert!(out.is_empty());
        // 无 pending 时不发任何模型调用。
        assert_eq!(ev.0.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn model_arbitrate_covers_all_and_enforces_integrity() {
        // 模型返回一个越界 decisionId（不在 pending）+ 漏掉 d2。
        let resp = r#"{"outcomes":[
            {"decisionId":"d1","result":"failure","consequence":"被拦下"},
            {"decisionId":"ghost","result":"success","consequence":"不该出现"}
        ]}"#;
        let (host, _ev) = test_host(vec![Ok(resp.to_string())]);
        let s = base_state();
        let pending = vec![decision("d1", "li", "a", vec![]), decision("d2", "wang", "b", vec![])];
        let out = model_arbitrate(
            host.as_ref(),
            &dummy_profile(),
            &prompts(),
            "run-1",
            &s,
            "局势",
            &pending,
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        // 每个 pending 决策都被覆盖；ghost 被丢弃。
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].decision_id, "d1");
        assert_eq!(out[0].result, ArbiterResult::Failure);
        assert_eq!(out[1].decision_id, "d2");
        assert_eq!(out[1].result, ArbiterResult::Success); // 漏判回退
        assert!(out.iter().all(|o| o.decision_id != "ghost"));
    }

    // ===== R7 底线硬约束（人设保险第 1 级·事前，总规格 §7）=====

    fn card_with(bottom: &[&str], refusal: &[&str], immutable: &[&str]) -> CharacterCardV2 {
        use crate::character::types::{CardLifecycle, Identity};
        let vs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        CharacterCardV2 {
            schema_version: 2,
            id: "c".into(),
            lifecycle: CardLifecycle::Draft,
            identity: Identity::default(),
            dramatic_core: crate::character::types::DramaticCore {
                bottom_lines: vs(bottom),
                ..Default::default()
            },
            decision_model: Default::default(),
            perception: Default::default(),
            emotion_dynamics: Default::default(),
            relation_grammar: Default::default(),
            expression_fingerprint: Default::default(),
            agency: crate::character::types::Agency {
                refusal_rules: vs(refusal),
                ..Default::default()
            },
            growth_arc: crate::character::types::GrowthArc {
                immutable_core: vs(immutable),
                ..Default::default()
            },
            world_adaptation: Default::default(),
            evidence_index: Default::default(),
            revision: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn lines_of(cid: &str, card: &CharacterCardV2) -> BTreeMap<String, Vec<String>> {
        let cards: BTreeMap<String, CharacterCardV2> =
            [(cid.to_string(), card.clone())].into_iter().collect();
        collect_bottom_lines(&cards)
    }

    /// 🔴 回归护栏（最重要）：卡上一条底线都没声明 → 筛查恒空、底线表恒空，
    /// `run_round` 据此整段短路，默认路径行为与接线前逐字节一致。
    #[test]
    fn bottom_line_absent_screens_nothing() {
        let card = card_with(&[], &[], &[]);
        let lines = lines_of("li", &card);
        assert!(lines.is_empty(), "无声明 → 底线表必须为空（run_round 据此短路）");
        let d = decision("d1", "li", "拔刀当场杀死对手", vec![]);
        assert!(screen_bottom_lines(&lines, &[d]).is_empty());
    }

    /// 字面重述自己卡上的禁止行为 → 命中，并落成 `Invalid` + `rule:bottom_line`。
    #[test]
    fn bottom_line_literal_restatement_is_rejected() {
        let card = card_with(&["不替人做伪证"], &[], &["不做伪证"]);
        let lines = lines_of("li", &card);
        let d = decision("d1", "li", "在堂上替裴照做伪证，一口咬定他当夜不在场", vec![]);
        let hits = screen_bottom_lines(&lines, &[d]);
        assert_eq!(hits.len(), 1, "应命中：{hits:?}");
        assert_eq!(hits[0].character_id, "li");
        assert_eq!(hits[0].line, "不做伪证", "命中的是 immutableCore 那一条（更短、更通用）");
        assert_eq!(hits[0].matched, "做伪证");

        let o = bottom_line_outcome(&hits[0]);
        assert_eq!(o.result, ArbiterResult::Invalid, "拦截 = 拒绝提案");
        assert_eq!(o.rule_refs, vec![BOTTOM_LINE_RULE_REF.to_string()]);
        // consequence 同时是给写手的指令：必须显式否定「已发生」，否则正文会把 OOC 写成公共事实。
        assert!(o.consequence.contains("并未发生"), "必须显式告诉写手它没发生：{}", o.consequence);
        // 🔴 平权（§0.1）：文案必须否掉「底线 = 优势」的联想。
        assert!(o.consequence.contains("不是他更强"), "文案须声明底线不是优势：{}", o.consequence);
    }

    /// 三处字段合一（规格 §7 明列三处），去重且顺序固定。
    #[test]
    fn bottom_line_merges_three_card_fields_in_fixed_order() {
        let card = card_with(&["不牵连无辜", "不做伪证"], &["不做伪证", "不篡改记录"], &["不背后出刀"]);
        let lines = lines_of("li", &card);
        assert_eq!(
            lines["li"],
            vec![
                "不牵连无辜".to_string(),
                "不做伪证".to_string(),
                "不篡改记录".to_string(),
                "不背后出刀".to_string(),
            ],
            "bottomLines → refusalRules → immutableCore，重复项只留首次出现"
        );
        // refusalRules / immutableCore 单独也能拦（不是只认 bottomLines）。
        let only_refusal = lines_of("li", &card_with(&[], &["不篡改记录"], &[]));
        let d = decision("d1", "li", "连夜篡改记录，把那一行抹掉", vec![]);
        assert_eq!(
            screen_bottom_lines(&only_refusal, std::slice::from_ref(&d)).len(),
            1,
            "refusalRules 也是硬约束"
        );
        let only_immutable = lines_of("li", &card_with(&[], &[], &["不篡改记录"]));
        assert_eq!(screen_bottom_lines(&only_immutable, &[d]).len(), 1, "immutableCore 也是硬约束");
    }

    /// 误伤控制闸③：角色**拒绝**做这件事 → 不算违反底线（那正是底线在起作用）。
    #[test]
    fn bottom_line_refusing_the_act_is_not_a_violation() {
        let lines = lines_of("li", &card_with(&["不做伪证"], &[], &[]));
        for action in [
            "当堂拒绝做伪证，把笔推回去",
            "他不肯做伪证，只是沉默",
            "我不会做伪证",
            "任凭如何相逼，也没有做伪证",
        ] {
            let d = decision("d1", "li", action, vec![]);
            assert!(
                screen_bottom_lines(&lines, &[d]).is_empty(),
                "拒绝/否定语境不得判违规：{action}"
            );
        }
    }

    /// 误伤控制闸①：过宽底线（"绝不伤害任何人"）不进规则层——按字面拦会让角色什么都做不了。
    #[test]
    fn bottom_line_overbroad_declaration_is_not_enforced() {
        let lines = lines_of("li", &card_with(&["绝不伤害任何人", "不做任何事"], &[], &[]));
        assert_eq!(lines["li"].len(), 2, "条目仍原样留在卡上（角色自己看得见、critic 也照审）");
        for action in ["出手伤害任何人挡路的", "做任何事都要先问过", "伤害任何人"] {
            let d = decision("d1", "li", action, vec![]);
            assert!(
                screen_bottom_lines(&lines, &[d]).is_empty(),
                "过宽条目不得作为确定性硬拦截依据：{action}"
            );
        }
    }

    /// 误伤控制闸②：过短底线（"不逃"/"不杀人"）不进规则层——字面匹配会大面积误伤。
    #[test]
    fn bottom_line_too_short_declaration_is_not_enforced() {
        let lines = lines_of("li", &card_with(&["不逃", "不杀人"], &[], &[]));
        for action in ["转身就逃出火场", "杀人偿命，他还是动了手"] {
            let d = decision("d1", "li", action, vec![]);
            assert!(screen_bottom_lines(&lines, &[d]).is_empty(), "过短条目不得进规则层：{action}");
        }
    }

    /// 正向承诺式底线（无否定标记）不进规则层：无法确定性判定「违反」。
    #[test]
    fn bottom_line_positive_pledge_is_not_enforced() {
        let lines = lines_of("li", &card_with(&["答应过的路一定走完", "认错时如实报数目"], &[], &[]));
        let d = decision("d1", "li", "半路撇下同行的人，独自走了", vec![]);
        assert!(screen_bottom_lines(&lines, &[d]).is_empty(), "正向承诺交模型层与 critic");
    }

    /// 前置虚词剥离：「不在背后出刀」也能拦住「从背后出刀」。
    #[test]
    fn bottom_line_strips_one_leading_function_word() {
        let lines = lines_of("li", &card_with(&["不在背后出刀"], &[], &[]));
        let d = decision("d1", "li", "趁其不备，从背后出刀", vec![]);
        let hits = screen_bottom_lines(&lines, &[d]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].matched, "背后出刀");
    }

    /// 确定性：乱序输入 → 输出按 (character_id, decision_id) 定序；连跑两次逐字节相同。
    #[test]
    fn bottom_line_screen_is_deterministic() {
        let cards: BTreeMap<String, CharacterCardV2> = [
            ("wang".to_string(), card_with(&["不做伪证"], &[], &[])),
            ("li".to_string(), card_with(&["不篡改记录"], &[], &[])),
        ]
        .into_iter()
        .collect();
        let lines = collect_bottom_lines(&cards);
        let ds = vec![
            decision("d2", "wang", "替人做伪证", vec![]),
            decision("d1", "li", "连夜篡改记录", vec![]),
        ];
        let a = screen_bottom_lines(&lines, &ds);
        let b = screen_bottom_lines(&lines, &ds);
        assert_eq!(a, b, "同输入必须同输出");
        assert_eq!(a.iter().map(|h| h.character_id.as_str()).collect::<Vec<_>>(), vec!["li", "wang"]);
    }

    /// 每条决策至多一条命中（不因底线堆量而放大战报噪声），且条目数有上限。
    #[test]
    fn bottom_line_caps_lines_and_reports_first_hit_only() {
        let many: Vec<String> = (0..100).map(|i| format!("不做第{i}号勾当")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let lines = lines_of("li", &card_with(&refs, &[], &[]));
        assert_eq!(lines["li"].len(), MAX_BOTTOM_LINES_PER_CHARACTER, "条目数须截断到上限");

        let two = lines_of("li", &card_with(&["不篡改记录", "不做伪证"], &[], &[]));
        let d = decision("d1", "li", "先篡改记录，再做伪证", vec![]);
        let hits = screen_bottom_lines(&two, &[d]);
        assert_eq!(hits.len(), 1, "一条决策至多产一条命中");
        assert_eq!(hits[0].line, "不篡改记录", "取卡上首条被违反的底线");
    }

    /// 🔴 黄金世界护栏：把 `server/src/runtime/golden/cards.json` 的底线文案与剧本 action
    /// 原样抄进引擎单测 —— golden 是逐字节比对的回归基线，规则层一旦在它身上误伤，
    /// 12 项 golden 会集体变红。这条用例让误伤在引擎层就炸，而不是等到 server 侧。
    #[test]
    fn bottom_line_never_false_positives_on_golden_world_cards() {
        let cards: BTreeMap<String, CharacterCardV2> = [
            (
                "shenyan".to_string(),
                card_with(
                    &["不牵连不知情的下属", "不在酒席上说一句自己不认的话"],
                    &["不供出不知情的人"],
                    &["不牵连无辜"],
                ),
            ),
            (
                "peizhao".to_string(),
                card_with(&["不动手无寸铁的人", "不在背后出刀"], &["不在背后出刀"], &["不背后出刀"]),
            ),
            (
                "cuie".to_string(),
                card_with(&["不替人做伪证", "不把旧信交出去"], &["不做伪证"], &["不做伪证"]),
            ),
            (
                "lu".to_string(),
                card_with(&["不多记一个字"], &["不替任何人隐去一句话"], &["不篡改记录"]),
            ),
        ]
        .into_iter()
        .collect();
        let lines = collect_bottom_lines(&cards);
        let script: &[(&str, &str)] = &[
            ("shenyan", "举杯向裴照道谢，谢他当年那一次相助"),
            ("shenyan", "抢先把话头引开，挡在裴照面前"),
            ("shenyan", "把半枚鱼符赠与崔萼，托她保管"),
            ("peizhao", "坐直身子，向沈砚举杯回礼"),
            ("peizhao", "拔刀出鞘半寸，逼沈砚正面回话"),
            ("peizhao", "收刀入鞘，长出一口气"),
            ("cuie", "起身替满席斟酒，唱一支旧曲"),
            ("cuie", "出言相助，替裴照解围"),
            ("cuie", "出手相救，把裴照护在身后"),
            ("lu", "在灯下把席间言语记下"),
            ("lu", "在灯下把方才的争执记下"),
            ("lu", "在灯下把这一段记下"),
            // 致死剧情测试点 + 剧本兜底响应（同样不得误伤）。
            ("shenyan", "拔剑当场杀死裴照"),
            ("shenyan", "端起酒盏，不接话头"),
            ("peizhao", "端起酒盏，不接话头"),
            ("cuie", "端起酒盏，不接话头"),
            ("lu", "端起酒盏，不接话头"),
        ];
        for (cid, action) in script {
            let d = decision("d1", cid, action, vec![]);
            let hits = screen_bottom_lines(&lines, &[d]);
            assert!(hits.is_empty(), "黄金世界剧本被误伤：{cid} / {action} → {hits:?}");
        }
    }

    #[tokio::test]
    async fn model_arbitrate_propagates_blocked() {
        let resp = r#"{"outcomes":[{"decisionId":"d1","result":"blocked","consequence":"与硬节点冲突"}]}"#;
        let (host, _ev) = test_host(vec![Ok(resp.to_string())]);
        let s = base_state();
        let pending = vec![decision("d1", "li", "a", vec![])];
        let out = model_arbitrate(
            host.as_ref(),
            &dummy_profile(),
            &prompts(),
            "run-1",
            &s,
            "局势",
            &pending,
            &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(out[0].result, ArbiterResult::Blocked);
    }
}
