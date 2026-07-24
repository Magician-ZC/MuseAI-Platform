//! 关系演化（A. relation dynamics）：由本回合仲裁结果**确定性**推导 `relations[<from>-><to>].*`
//! 数值变更（纯函数，无模型调用、无随机源、无时钟依赖）。
//!
//! 背景：此前 `build_patch` 从不产生任何 `relations` 操作，关系图恒无边、恒为 0，
//! 软主线 `advance_when` 关系谓词（如 `relations[a->b].affinity > 0.2`）永远无法命中。
//! 本模块以 action+intent 文本的关键词正则分类（沿用 `IrreversibleRules` 的既有先例）
//! 推导每回合的关系增量，经 `build_patch` 并入同一 StatePatch 原子提交。
//!
//! 输出规约（确定性铁律）：
//! - 同一回合同一 `(from, to, field)` 的多笔增量**先累加**，再读当前状态值算终值并 clamp 到
//!   [-1.0, 1.0]，发 **Set（带 clamp 后终值）**而非 Increment——clamp 语义只能这样做确定；
//! - 操作按 `(from, to, field)` 的 BTreeMap 序输出，保证同输入恒同输出（可 replay）；
//! - 边不存在时终值以 0 为基（reducer 写入时对已知角色端点零值自动建边，见 reducer.rs）。

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde_json::json;

use super::arbiter::LOC_TARGET_PREFIX;
use super::types::{
    ArbiterOutcome, ArbiterResult, NarrativeState, PatchOp, PatchOperation, RoleDecision,
};

// ---------- 平衡参数（集中一处，可调） ----------
// 命名约定：<类别>_<方向>_<字段>。方向 T2A = target→actor 边、A2T = actor→target 边、BI = 双向。

/// 敌对：target→actor fear 增量（被攻击者对行动者生畏）。
const HOSTILE_T2A_FEAR: f64 = 0.10;
/// 敌对：target→actor trust 增量（负——信任受损）。
const HOSTILE_T2A_TRUST: f64 = -0.06;
/// 敌对：target→actor affinity 增量（负——好感受损）。
const HOSTILE_T2A_AFFINITY: f64 = -0.08;
/// 敌对：actor→target affinity 增量（负——出手者对目标好感亦降）。
const HOSTILE_A2T_AFFINITY: f64 = -0.05;

/// 友善：双向 affinity 增量。
const FRIENDLY_BI_AFFINITY: f64 = 0.08;
/// 友善：双向 trust 增量。
const FRIENDLY_BI_TRUST: f64 = 0.06;
/// 友善且含「救」类语义：target→actor debt 另加（救命之恩记欠）。
const RESCUE_T2A_DEBT: f64 = 0.10;

/// 关系破裂（背叛/决裂/绝交/反目家族）：双向 trust 增量（覆盖敌对普通值，取更强者）。
const RUPTURE_BI_TRUST: f64 = -0.50;
/// 关系破裂：双向 affinity 增量。
const RUPTURE_BI_AFFINITY: f64 = -0.50;
/// 关系破裂：target→actor fear 增量（强于敌对普通值 0.10）。
const RUPTURE_T2A_FEAR: f64 = 0.15;

/// 中性互动（有角色目标但敌对/友善皆未命中）：双向 affinity 微增（熟悉度）。
const NEUTRAL_BI_AFFINITY: f64 = 0.02;
/// 中性互动：双向 trust 微增。
const NEUTRAL_BI_TRUST: f64 = 0.01;

/// willSpeak 且有角色目标：actor→target affinity 另加（主动交流微增好感）。
const SPEAK_A2T_AFFINITY: f64 = 0.02;

/// 结果缩放：Success 全额。
const SCALE_SUCCESS: f64 = 1.0;
/// 结果缩放：PartialSuccess 打六折。
const SCALE_PARTIAL: f64 = 0.6;
/// 结果缩放：Failure 两五折。
const SCALE_FAILURE: f64 = 0.25;
/// 例外：敌对-Failure 的 fear 项按此缩放（未遂的敌意仍暴露敌意，强于普通 Failure 折扣）。
const HOSTILE_FAILURE_FEAR_SCALE: f64 = 0.5;

/// 关系数值统一 clamp 区间。
const CLAMP_MIN: f64 = -1.0;
const CLAMP_MAX: f64 = 1.0;

// ---------- 语义分类（预编译正则，仿 IrreversibleRules 先例） ----------

/// 行动语义类别。分类互斥，优先级：破裂 > 敌对 > 友善 > 中性——
/// 破裂语义天然是最强的敌意（fear/trust/affinity 皆取更强者），文本同时命中破裂与敌对时只按破裂计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Rupture,
    Hostile,
    Friendly,
    Neutral,
}

/// 关系演化关键词规则（预编译；作用于 action+intent 拼接文本）。
struct RelationRules {
    /// 关系破裂：与 mod.rs `IrreversibleRules.relation` 同源的背叛/决裂/绝交/反目家族。
    rupture: Regex,
    hostile: Regex,
    friendly: Regex,
    /// 「救」类子语义（友善命中时另加 debt）。
    rescue: Regex,
}

impl RelationRules {
    fn new() -> Self {
        Self {
            rupture: Regex::new(r"(背叛|叛变|叛逃|反目成仇|反目|决裂|绝交|断绝)").unwrap(),
            hostile: Regex::new(
                r"(攻击|袭|杀|伤|威胁|抢|夺|斥|骗|偷|揭穿|对抗|阻拦|挡|逼|囚)",
            )
            .unwrap(),
            friendly: Regex::new(
                r"(帮|助|救|护|赠|送|安慰|陪|合作|结盟|道谢|致歉|坦白|信任|分享)",
            )
            .unwrap(),
            rescue: Regex::new(r"救").unwrap(),
        }
    }

    fn classify(&self, text: &str) -> Category {
        if self.rupture.is_match(text) {
            Category::Rupture
        } else if self.hostile.is_match(text) {
            Category::Hostile
        } else if self.friendly.is_match(text) {
            Category::Friendly
        } else {
            Category::Neutral
        }
    }
}

// ---------- 推导主流程 ----------

/// 关系字段名（&'static str 令 (from,to,field) 键可 BTreeMap 排序且零分配）。
const F_TRUST: &str = "trust";
const F_AFFINITY: &str = "affinity";
const F_FEAR: &str = "fear";
const F_DEBT: &str = "debt";

/// 由本回合 decisions/outcomes 推导关系数值 Set 操作（确定性纯函数）。
///
/// - 只处理 result ∈ {Success, PartialSuccess, Failure} 的 outcome（Invalid/Blocked 跳过；
///   Blocked 实际到不了这里——run_round 在 Blocked 时整回合不提交）；
/// - 目标过滤：剔除 `loc:` 移动伪目标、剔除 self、目标与行动者都必须存在于 `state.characters`
///   （reducer 对悬空端点整 patch 拒绝，这里前置滤掉以免拖垮全回合提交）；
/// - 同一 (from,to,field) 的多笔增量先累加，再以「当前值（边缺失视为 0）+ 累计增量」clamp 后发 Set。
pub fn derive_relation_ops(
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    state: &NarrativeState,
) -> Vec<PatchOperation> {
    let dmap: BTreeMap<&str, &RoleDecision> =
        decisions.iter().map(|d| (d.decision_id.as_str(), d)).collect();
    let rules = RelationRules::new();

    // (from, to, field) → 本回合累计增量。BTreeMap 保证输出有序（确定性）。
    let mut acc: BTreeMap<(String, String, &'static str), f64> = BTreeMap::new();
    let add = |acc: &mut BTreeMap<(String, String, &'static str), f64>,
               from: &str,
               to: &str,
               field: &'static str,
               delta: f64| {
        *acc.entry((from.to_string(), to.to_string(), field)).or_insert(0.0) += delta;
    };

    for o in outcomes {
        // 结果缩放；Invalid/Blocked 不产生关系变化。
        let scale = match o.result {
            ArbiterResult::Success => SCALE_SUCCESS,
            ArbiterResult::PartialSuccess => SCALE_PARTIAL,
            ArbiterResult::Failure => SCALE_FAILURE,
            ArbiterResult::Invalid | ArbiterResult::Blocked => continue,
        };
        let Some(d) = dmap.get(o.decision_id.as_str()) else { continue };
        let actor = o.character_id.as_str();
        if !state.characters.contains_key(actor) {
            continue; // 防御：行动者不在状态中（reducer 会拒悬空端点）
        }
        // 角色目标：滤 loc: 伪目标 / self / 不存在角色，去重且保首次出现序（确定性）。
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let targets: Vec<&str> = d
            .targets
            .iter()
            .map(|t| t.as_str())
            .filter(|t| !t.starts_with(LOC_TARGET_PREFIX))
            .filter(|t| *t != actor)
            .filter(|t| state.characters.contains_key(*t))
            .filter(|t| seen.insert(*t))
            .collect();
        if targets.is_empty() {
            continue;
        }

        let text = format!("{} {}", d.action, d.intent);
        let category = rules.classify(&text);
        for t in targets {
            match category {
                Category::Rupture => {
                    add(&mut acc, actor, t, F_TRUST, RUPTURE_BI_TRUST * scale);
                    add(&mut acc, t, actor, F_TRUST, RUPTURE_BI_TRUST * scale);
                    add(&mut acc, actor, t, F_AFFINITY, RUPTURE_BI_AFFINITY * scale);
                    add(&mut acc, t, actor, F_AFFINITY, RUPTURE_BI_AFFINITY * scale);
                    add(&mut acc, t, actor, F_FEAR, RUPTURE_T2A_FEAR * scale);
                }
                Category::Hostile => {
                    // 例外：未遂（Failure）的敌意仍暴露敌意——fear 项按 0.5 缩放而非 0.25。
                    let fear_scale = if o.result == ArbiterResult::Failure {
                        HOSTILE_FAILURE_FEAR_SCALE
                    } else {
                        scale
                    };
                    add(&mut acc, t, actor, F_FEAR, HOSTILE_T2A_FEAR * fear_scale);
                    add(&mut acc, t, actor, F_TRUST, HOSTILE_T2A_TRUST * scale);
                    add(&mut acc, t, actor, F_AFFINITY, HOSTILE_T2A_AFFINITY * scale);
                    add(&mut acc, actor, t, F_AFFINITY, HOSTILE_A2T_AFFINITY * scale);
                }
                Category::Friendly => {
                    add(&mut acc, actor, t, F_AFFINITY, FRIENDLY_BI_AFFINITY * scale);
                    add(&mut acc, t, actor, F_AFFINITY, FRIENDLY_BI_AFFINITY * scale);
                    add(&mut acc, actor, t, F_TRUST, FRIENDLY_BI_TRUST * scale);
                    add(&mut acc, t, actor, F_TRUST, FRIENDLY_BI_TRUST * scale);
                    if rules.rescue.is_match(&text) {
                        add(&mut acc, t, actor, F_DEBT, RESCUE_T2A_DEBT * scale);
                    }
                }
                Category::Neutral => {
                    add(&mut acc, actor, t, F_AFFINITY, NEUTRAL_BI_AFFINITY * scale);
                    add(&mut acc, t, actor, F_AFFINITY, NEUTRAL_BI_AFFINITY * scale);
                    add(&mut acc, actor, t, F_TRUST, NEUTRAL_BI_TRUST * scale);
                    add(&mut acc, t, actor, F_TRUST, NEUTRAL_BI_TRUST * scale);
                }
            }
            // 主动交流微增（与类别叠加，同受结果缩放）。
            if d.speak.will_speak {
                add(&mut acc, actor, t, F_AFFINITY, SPEAK_A2T_AFFINITY * scale);
            }
        }
    }

    // 终值 = clamp(当前值 + 累计增量)；发 Set（clamp 语义只能以终值 Set 做确定）。
    acc.into_iter()
        .map(|((from, to, field), delta)| {
            let cur = current_value(state, &from, &to, field);
            let fin = (cur + delta).clamp(CLAMP_MIN, CLAMP_MAX);
            PatchOperation {
                op: PatchOp::Set,
                path: format!("relations[{from}->{to}].{field}"),
                value: Some(json!(fin)),
                precondition: None,
            }
        })
        .collect()
}

/// 读当前关系字段值；边不存在 → 0.0（reducer 写入时对已知角色端点零值自动建边，语义一致）。
fn current_value(state: &NarrativeState, from: &str, to: &str, field: &str) -> f64 {
    state
        .relations
        .iter()
        .find(|r| r.from == from && r.to == to)
        .map(|r| match field {
            F_TRUST => r.trust as f64,
            F_AFFINITY => r.affinity as f64,
            F_FEAR => r.fear as f64,
            _ => r.debt as f64, // F_DEBT
        })
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narrative::types::{CharacterState, RelationState, SpeakIntent};

    fn state_with_chars(chars: &[&str]) -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        for c in chars {
            s.characters.insert((*c).into(), CharacterState::default());
        }
        s
    }

    fn decision(
        id: &str,
        cid: &str,
        action: &str,
        intent: &str,
        targets: &[&str],
        will_speak: bool,
    ) -> RoleDecision {
        RoleDecision {
            decision_id: id.into(),
            character_id: cid.into(),
            intent: intent.into(),
            action: action.into(),
            speak: SpeakIntent { will_speak, purpose: String::new() },
            targets: targets.iter().map(|t| t.to_string()).collect(),
            acceptable_costs: vec![],
            predictions: vec![],
            duration: 0,
        }
    }

    fn outcome(id: &str, cid: &str, result: ArbiterResult) -> ArbiterOutcome {
        ArbiterOutcome {
            decision_id: id.into(),
            character_id: cid.into(),
            result,
            rule_refs: vec![],
            consequence: "后果".into(),
        }
    }

    /// 从 ops 中取指定路径的 Set 终值。
    fn set_value(ops: &[PatchOperation], path: &str) -> Option<f64> {
        ops.iter()
            .find(|o| o.path == path)
            .map(|o| {
                assert_eq!(o.op, PatchOp::Set, "关系操作应为 Set：{path}");
                o.value.as_ref().and_then(|v| v.as_f64()).unwrap()
            })
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // ---- 敌对：各向增量正确 ----

    #[test]
    fn hostile_success_directions_and_values() {
        let s = state_with_chars(&["li", "wang"]);
        let d = vec![decision("d1", "li", "拔剑攻击对方", "压制", &["wang"], false)];
        let o = vec![outcome("d1", "li", ArbiterResult::Success)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[wang->li].fear").unwrap(), 0.10));
        assert!(approx(set_value(&ops, "relations[wang->li].trust").unwrap(), -0.06));
        assert!(approx(set_value(&ops, "relations[wang->li].affinity").unwrap(), -0.08));
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), -0.05));
        assert_eq!(ops.len(), 4, "敌对应恰产 4 笔：{ops:?}");
    }

    // ---- 友善：双向 + 救类 debt ----

    #[test]
    fn friendly_success_bidirectional_and_rescue_debt() {
        let s = state_with_chars(&["li", "wang"]);
        let d = vec![decision("d1", "li", "出手救下坠崖的同伴", "护住他", &["wang"], false)];
        let o = vec![outcome("d1", "li", ArbiterResult::Success)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), 0.08));
        assert!(approx(set_value(&ops, "relations[wang->li].affinity").unwrap(), 0.08));
        assert!(approx(set_value(&ops, "relations[li->wang].trust").unwrap(), 0.06));
        assert!(approx(set_value(&ops, "relations[wang->li].trust").unwrap(), 0.06));
        // 「救」类另加 target→actor debt。
        assert!(approx(set_value(&ops, "relations[wang->li].debt").unwrap(), 0.10));
        assert_eq!(ops.len(), 5);
    }

    // ---- 破裂：覆盖敌对，取更强者 ----

    #[test]
    fn rupture_overrides_hostile_with_stronger_values() {
        let s = state_with_chars(&["li", "wang"]);
        // 文本同时命中破裂（背叛）与敌对（杀）：只按破裂计（fear 0.15 > 0.10）。
        let d = vec![decision("d1", "li", "背叛盟友并动手追杀", "断绝往来", &["wang"], false)];
        let o = vec![outcome("d1", "li", ArbiterResult::Success)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[li->wang].trust").unwrap(), -0.50));
        assert!(approx(set_value(&ops, "relations[wang->li].trust").unwrap(), -0.50));
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), -0.50));
        assert!(approx(set_value(&ops, "relations[wang->li].affinity").unwrap(), -0.50));
        assert!(approx(set_value(&ops, "relations[wang->li].fear").unwrap(), 0.15), "破裂 fear 取更强者 0.15");
        assert_eq!(ops.len(), 5, "破裂不叠加敌对普通值：{ops:?}");
    }

    // ---- 中性：熟悉度微增 ----

    #[test]
    fn neutral_interaction_small_gain() {
        let s = state_with_chars(&["li", "wang"]);
        let d = vec![decision("d1", "li", "端详对方的神色", "观望", &["wang"], false)];
        let o = vec![outcome("d1", "li", ArbiterResult::Success)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), 0.02));
        assert!(approx(set_value(&ops, "relations[wang->li].affinity").unwrap(), 0.02));
        assert!(approx(set_value(&ops, "relations[li->wang].trust").unwrap(), 0.01));
        assert!(approx(set_value(&ops, "relations[wang->li].trust").unwrap(), 0.01));
        assert_eq!(ops.len(), 4);
    }

    // ---- willSpeak 加成 ----

    #[test]
    fn will_speak_adds_actor_to_target_affinity() {
        let s = state_with_chars(&["li", "wang"]);
        let d = vec![decision("d1", "li", "上前帮忙收拾行装", "结个善缘", &["wang"], true)];
        let o = vec![outcome("d1", "li", ArbiterResult::Success)];
        let ops = derive_relation_ops(&d, &o, &s);
        // 友善 0.08 + speak 0.02 = 0.10（同键先累加，单笔 Set）。
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), 0.10));
        assert!(approx(set_value(&ops, "relations[wang->li].affinity").unwrap(), 0.08));
    }

    // ---- 结果缩放 + 敌对-Failure 的 fear 例外 ----

    #[test]
    fn scaling_partial_and_failure() {
        let s = state_with_chars(&["li", "wang"]);
        let d = vec![decision("d1", "li", "伸手帮扶", "示好", &["wang"], false)];
        // PartialSuccess ×0.6。
        let o = vec![outcome("d1", "li", ArbiterResult::PartialSuccess)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), 0.08 * 0.6));
        assert!(approx(set_value(&ops, "relations[wang->li].trust").unwrap(), 0.06 * 0.6));
        // Failure ×0.25。
        let o = vec![outcome("d1", "li", ArbiterResult::Failure)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), 0.08 * 0.25));
    }

    #[test]
    fn hostile_failure_fear_uses_half_scale() {
        let s = state_with_chars(&["li", "wang"]);
        let d = vec![decision("d1", "li", "挥剑袭向对方", "除掉他", &["wang"], false)];
        let o = vec![outcome("d1", "li", ArbiterResult::Failure)];
        let ops = derive_relation_ops(&d, &o, &s);
        // fear 例外 ×0.5；其余项照常 ×0.25。
        assert!(approx(set_value(&ops, "relations[wang->li].fear").unwrap(), 0.10 * 0.5));
        assert!(approx(set_value(&ops, "relations[wang->li].trust").unwrap(), -0.06 * 0.25));
        assert!(approx(set_value(&ops, "relations[wang->li].affinity").unwrap(), -0.08 * 0.25));
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), -0.05 * 0.25));
    }

    // ---- clamp 上下界 ----

    #[test]
    fn clamp_upper_and_lower_bounds() {
        let mut s = state_with_chars(&["li", "wang"]);
        s.relations.push(RelationState {
            from: "li".into(),
            to: "wang".into(),
            trust: -0.98,
            affinity: 0.98,
            fear: 0.0,
            debt: 0.0,
            known_to: vec![],
            notes: vec![],
        });
        // 友善：li->wang affinity 0.98+0.08 → clamp 1.0。
        let d = vec![decision("d1", "li", "全力相助", "报恩", &["wang"], false)];
        let o = vec![outcome("d1", "li", ArbiterResult::Success)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), 1.0));
        // 破裂：li->wang trust -0.98-0.50 → clamp -1.0。
        let d = vec![decision("d1", "li", "当众宣布决裂", "断绝", &["wang"], false)];
        let ops = derive_relation_ops(&d, &o, &s);
        assert!(approx(set_value(&ops, "relations[li->wang].trust").unwrap(), -1.0));
    }

    // ---- 同回合同键累加去重（单笔 Set） ----

    #[test]
    fn same_round_same_key_accumulates_into_single_set() {
        let s = state_with_chars(&["li", "wang"]);
        // 双方互帮：li→wang affinity 获 0.08（li 主动）+ 0.08（wang 行动的双向回流）= 0.16。
        let d = vec![
            decision("d1", "li", "上前帮忙", "互助", &["wang"], false),
            decision("d2", "wang", "出手相助", "互助", &["li"], false),
        ];
        let o = vec![
            outcome("d1", "li", ArbiterResult::Success),
            outcome("d2", "wang", ArbiterResult::Success),
        ];
        let ops = derive_relation_ops(&d, &o, &s);
        let same_key = ops.iter().filter(|op| op.path == "relations[li->wang].affinity").count();
        assert_eq!(same_key, 1, "同键应先累加为单笔 Set");
        assert!(approx(set_value(&ops, "relations[li->wang].affinity").unwrap(), 0.16));
        assert!(approx(set_value(&ops, "relations[wang->li].affinity").unwrap(), 0.16));
        assert!(approx(set_value(&ops, "relations[li->wang].trust").unwrap(), 0.12));
    }

    // ---- 过滤：loc: 伪目标 / self / 不存在角色 / Invalid、Blocked ----

    #[test]
    fn filters_loc_self_unknown_and_skipped_results() {
        let s = state_with_chars(&["li", "wang"]);
        // loc: 伪目标 + self + 不存在角色：全部滤掉 → 无操作。
        let d = vec![decision("d1", "li", "帮忙", "互助", &["loc:密室", "li", "ghost"], false)];
        let o = vec![outcome("d1", "li", ArbiterResult::Success)];
        assert!(derive_relation_ops(&d, &o, &s).is_empty());
        // Invalid / Blocked 结果：跳过。
        let d = vec![decision("d1", "li", "攻击", "压制", &["wang"], false)];
        for r in [ArbiterResult::Invalid, ArbiterResult::Blocked] {
            let o = vec![outcome("d1", "li", r)];
            assert!(derive_relation_ops(&d, &o, &s).is_empty(), "{r:?} 不应产生关系操作");
        }
    }

    // ---- 确定性：同输入同输出、输出有序 ----

    #[test]
    fn deterministic_and_ordered_output() {
        let s = state_with_chars(&["an", "li", "wang"]);
        let d = vec![
            decision("d1", "li", "挥剑攻击", "压制", &["wang", "an"], true),
            decision("d2", "wang", "出手相救", "护住", &["an"], false),
        ];
        let o = vec![
            outcome("d1", "li", ArbiterResult::Success),
            outcome("d2", "wang", ArbiterResult::PartialSuccess),
        ];
        let a = derive_relation_ops(&d, &o, &s);
        let b = derive_relation_ops(&d, &o, &s);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "同输入应逐字节同输出"
        );
        // 输出按 (from,to,field) 有序。
        let keys: Vec<&str> = a.iter().map(|op| op.path.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "操作应按 (from,to,field) 字典序输出：{keys:?}");
    }
}
