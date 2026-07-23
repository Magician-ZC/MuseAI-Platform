//! 大纲约束与禁止谓词（规格 §5.2 / §12.3）。文件所有权：agent-E3。

use serde_json::Value;

use super::types::{ConstraintLevel, ForbiddenPredicate, NarrativeState, NodeStatus, OutlineNode};
use crate::EngineError;

/// 受限谓词 DSL（MVP）的四种形态：
/// 1. `characters.<id>.<listField> contains "<literal>"`（listField ∈ goals/resources/secrets/misconceptions/plans）
/// 2. `characters.<id>.arcStage == "<literal>"`
/// 3. `world.<key> == <json literal>`
/// 4. `relations[<from>-><to>].<numField> (<|>|==) <number>`（numField ∈ trust/affinity/fear/debt）
#[derive(Debug, Clone, PartialEq)]
enum Predicate {
    CharListContains { id: String, field: String, literal: String },
    CharArcEq { id: String, literal: String },
    WorldEq { key: String, value: Value },
    RelNumCmp { from: String, to: String, field: String, op: Cmp, num: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    Lt,
    Gt,
    Eq,
}

const CHAR_LIST_FIELDS: &[&str] = &["goals", "resources", "secrets", "misconceptions", "plans"];
const REL_NUM_FIELDS: &[&str] = &["trust", "affinity", "fear", "debt"];

fn err(msg: impl Into<String>) -> EngineError {
    EngineError::Validation(msg.into())
}

/// 拆分 `<lhs> <op> <rhs>`：op 两侧带空格，lhs 无空格，rhs 可含空格（引号串）。
fn split_op(expr: &str) -> Result<(&str, &str, &str), EngineError> {
    // 顺序敏感：先找 contains / ==，再找 < / >（单字符）。
    for token in ["contains", "==", "<", ">"] {
        let pat = format!(" {token} ");
        if let Some(idx) = expr.find(&pat) {
            let lhs = expr[..idx].trim();
            let rhs = expr[idx + pat.len()..].trim();
            return Ok((lhs, token, rhs));
        }
    }
    Err(err(format!("谓词缺少操作符 (contains|==|<|>)，无法定位: `{expr}`")))
}

/// 解析引号字符串字面量（支持转义，走 serde）。
fn parse_string_literal(rhs: &str) -> Result<String, EngineError> {
    if !(rhs.starts_with('"') && rhs.ends_with('"') && rhs.len() >= 2) {
        return Err(err(format!("期望字符串字面量（双引号包裹），实际 token: `{rhs}`")));
    }
    serde_json::from_str::<String>(rhs).map_err(|e| err(format!("字符串字面量非法 `{rhs}`: {e}")))
}

/// 解析 `<from>-><to>].<field>`（已消费前缀 `relations[`）。
fn parse_rel_lhs(rest: &str) -> Result<(String, String, String), EngineError> {
    let idx = rest.find(']').ok_or_else(|| err(format!("关系左值缺 ]，token: `{rest}`")))?;
    let bracket = &rest[..idx];
    let field = rest[idx + 1..]
        .strip_prefix('.')
        .ok_or_else(|| err(format!("关系左值缺字段，token: `{rest}`")))?;
    let (from, to) = bracket
        .split_once("->")
        .ok_or_else(|| err(format!("关系左值需 <from>-><to>，token: `{bracket}`")))?;
    if from.is_empty() || to.is_empty() {
        return Err(err(format!("关系端点为空，token: `{bracket}`")));
    }
    Ok((from.to_string(), to.to_string(), field.to_string()))
}

fn parse(expression: &str) -> Result<Predicate, EngineError> {
    let (lhs, op, rhs) = split_op(expression.trim())?;

    if let Some(rest) = lhs.strip_prefix("characters.") {
        let (id, field) = rest
            .split_once('.')
            .ok_or_else(|| err(format!("角色左值需 <id>.<field>，token: `{lhs}`")))?;
        if id.is_empty() {
            return Err(err(format!("角色 id 为空，token: `{lhs}`")));
        }
        if field == "arcStage" {
            if op != "==" {
                return Err(err(format!("arcStage 仅支持 ==，token: `{op}`")));
            }
            return Ok(Predicate::CharArcEq { id: id.into(), literal: parse_string_literal(rhs)? });
        }
        if CHAR_LIST_FIELDS.contains(&field) {
            if op != "contains" {
                return Err(err(format!("列表字段仅支持 contains，token: `{op}`")));
            }
            return Ok(Predicate::CharListContains {
                id: id.into(),
                field: field.into(),
                literal: parse_string_literal(rhs)?,
            });
        }
        return Err(err(format!("未知角色字段，token: `{field}`")));
    }

    if let Some(key) = lhs.strip_prefix("world.") {
        if op != "==" {
            return Err(err(format!("world 谓词仅支持 ==，token: `{op}`")));
        }
        if key.is_empty() || key.contains('.') {
            return Err(err(format!("world 键非法，token: `{key}`")));
        }
        let value =
            serde_json::from_str::<Value>(rhs).map_err(|e| err(format!("world 右值需 JSON 字面量 `{rhs}`: {e}")))?;
        return Ok(Predicate::WorldEq { key: key.into(), value });
    }

    if let Some(rest) = lhs.strip_prefix("relations[") {
        let (from, to, field) = parse_rel_lhs(rest)?;
        if !REL_NUM_FIELDS.contains(&field.as_str()) {
            return Err(err(format!("未知关系数值字段，token: `{field}`")));
        }
        let cmp = match op {
            "<" => Cmp::Lt,
            ">" => Cmp::Gt,
            "==" => Cmp::Eq,
            _ => return Err(err(format!("关系谓词仅支持 <|>|==，token: `{op}`"))),
        };
        let num = rhs.parse::<f64>().map_err(|_| err(format!("关系右值需数字，token: `{rhs}`")))?;
        return Ok(Predicate::RelNumCmp { from, to, field, op: cmp, num });
    }

    Err(err(format!("无法识别的谓词左值，token: `{lhs}`")))
}

/// 创建时校验谓词表达式语法；解析失败 → Validation（运行时不应再失败）。
pub fn parse_predicate(expression: &str) -> Result<(), EngineError> {
    parse(expression).map(|_| ())
}

/// 求值：状态命中谓词返回 true。表达式非法 → Validation；引用的实体缺失视为「未命中」（false）。
pub fn eval_predicate(state: &NarrativeState, predicate: &ForbiddenPredicate) -> Result<bool, EngineError> {
    let pred = parse(&predicate.expression)?;
    Ok(match pred {
        Predicate::CharListContains { id, field, literal } => match state.characters.get(&id) {
            None => false,
            Some(c) => {
                let list = match field.as_str() {
                    "goals" => &c.goals,
                    "resources" => &c.resources,
                    "secrets" => &c.secrets,
                    "misconceptions" => &c.misconceptions,
                    _ => &c.plans,
                };
                list.iter().any(|x| x == &literal)
            }
        },
        Predicate::CharArcEq { id, literal } => {
            state.characters.get(&id).map(|c| c.arc_stage == literal).unwrap_or(false)
        }
        Predicate::WorldEq { key, value } => {
            state.world.get(&key).map(|v| json_num_eq(v, &value)).unwrap_or(false)
        }
        Predicate::RelNumCmp { from, to, field, op, num } => {
            match state.relations.iter().find(|r| r.from == from && r.to == to) {
                None => false,
                Some(r) => {
                    let lhs = match field.as_str() {
                        "trust" => r.trust,
                        "affinity" => r.affinity,
                        "fear" => r.fear,
                        _ => r.debt,
                    } as f64;
                    match op {
                        Cmp::Lt => lhs < num,
                        Cmp::Gt => lhs > num,
                        Cmp::Eq => (lhs - num).abs() < 1e-6,
                    }
                }
            }
        }
    })
}

/// JSON 值相等：数值按 f64 容差比较，其余精确比较。
fn json_num_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(xf), Some(yf)) => (xf - yf).abs() < 1e-6,
            _ => a == b,
        },
        _ => a == b,
    }
}

/// 从用户大纲文本解析节点（TS 端 storyConstraints.ts 亦有同构实现，前端为编辑器体验，
/// 引擎端为最终事实；两端契约：一行一节点，前缀 [硬]/[软]/[自由]，缺省软）。
/// 空行忽略；节点 id 按出现顺序确定性生成（node-1, node-2, …）。
pub fn parse_outline(text: &str) -> Result<Vec<OutlineNode>, EngineError> {
    let mut nodes = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let (level, summary) = if let Some(rest) = line.strip_prefix("[硬]") {
            (ConstraintLevel::Hard, rest.trim())
        } else if let Some(rest) = line.strip_prefix("[软]") {
            (ConstraintLevel::Soft, rest.trim())
        } else if let Some(rest) = line.strip_prefix("[自由]") {
            (ConstraintLevel::Free, rest.trim())
        } else {
            (ConstraintLevel::Soft, line) // 缺省软
        };
        if summary.is_empty() {
            return Err(err(format!("大纲第 {} 行缺少节点描述", lineno + 1)));
        }
        nodes.push(OutlineNode {
            id: format!("node-{}", nodes.len() + 1),
            summary: summary.to_string(),
            constraint: level,
            status: NodeStatus::Pending,
        });
    }
    Ok(nodes)
}

/// 当前待推进节点（首个 Pending）；硬节点 Blocked 判定辅助。
pub fn next_pending(nodes: &[OutlineNode]) -> Option<&OutlineNode> {
    nodes.iter().find(|n| matches!(n.status, super::types::NodeStatus::Pending))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narrative::types::{CharacterState, RelationState};
    use serde_json::json;

    fn state_with() -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
        let mut li = CharacterState::default();
        li.secrets.push("身世".into());
        li.arc_stage = "觉醒".into();
        s.characters.insert("li".into(), li);
        s.characters.insert("wang".into(), CharacterState::default());
        s.relations.push(RelationState {
            from: "li".into(),
            to: "wang".into(),
            trust: 0.5,
            affinity: 0.0,
            fear: 0.0,
            debt: 0.0,
            known_to: vec![],
            notes: vec![],
        });
        s.world.insert("phase".into(), json!("night"));
        s
    }

    fn pred(expr: &str) -> ForbiddenPredicate {
        ForbiddenPredicate { id: "f".into(), expression: expr.into(), reason: "r".into() }
    }

    // ---- 大纲解析 ----

    #[test]
    fn parse_outline_prefixes_default_and_blank() {
        let text = "[硬]主角登场\n\n找到线索\n[软] 遇到旧友 \n[自由]闲聊\n\n";
        let nodes = parse_outline(text).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].constraint, ConstraintLevel::Hard);
        assert_eq!(nodes[0].summary, "主角登场");
        assert_eq!(nodes[1].constraint, ConstraintLevel::Soft); // 缺省软
        assert_eq!(nodes[1].summary, "找到线索");
        assert_eq!(nodes[2].constraint, ConstraintLevel::Soft);
        assert_eq!(nodes[2].summary, "遇到旧友");
        assert_eq!(nodes[3].constraint, ConstraintLevel::Free);
        // 确定性 id
        assert_eq!(nodes[0].id, "node-1");
        assert_eq!(nodes[3].id, "node-4");
        assert!(nodes.iter().all(|n| n.status == NodeStatus::Pending));
    }

    #[test]
    fn parse_outline_rejects_empty_summary() {
        assert_eq!(parse_outline("[硬]\n").unwrap_err().code(), "validation");
    }

    // ---- 谓词解析（四形态） ----

    #[test]
    fn parse_predicate_four_forms() {
        parse_predicate("characters.li.secrets contains \"身世\"").unwrap();
        parse_predicate("characters.li.arcStage == \"觉醒\"").unwrap();
        parse_predicate("world.phase == \"night\"").unwrap();
        parse_predicate("relations[li->wang].trust < 0.3").unwrap();
        parse_predicate("relations[li->wang].debt > 1").unwrap();
    }

    #[test]
    fn parse_predicate_errors_locate_token() {
        // 缺操作符
        assert!(parse_predicate("characters.li.secrets 身世").is_err());
        // 未知字段
        let e = parse_predicate("characters.li.charm contains \"x\"").unwrap_err();
        assert!(e.to_string().contains("charm"), "错误应含 token: {e}");
        // 字符串字面量缺引号
        let e = parse_predicate("characters.li.arcStage == 觉醒").unwrap_err();
        assert!(e.to_string().contains("觉醒"), "错误应含 token: {e}");
        // 关系右值非数字
        let e = parse_predicate("relations[li->wang].trust < abc").unwrap_err();
        assert!(e.to_string().contains("abc"), "错误应含 token: {e}");
        // arcStage 用 contains
        assert!(parse_predicate("characters.li.arcStage contains \"x\"").is_err());
    }

    // ---- 求值 ----

    #[test]
    fn eval_contains_hit_and_miss() {
        let s = state_with();
        assert!(eval_predicate(&s, &pred("characters.li.secrets contains \"身世\"")).unwrap());
        assert!(!eval_predicate(&s, &pred("characters.li.secrets contains \"财宝\"")).unwrap());
        // 引用缺失角色 → 未命中
        assert!(!eval_predicate(&s, &pred("characters.ghost.secrets contains \"身世\"")).unwrap());
    }

    #[test]
    fn eval_arc_and_world_eq() {
        let s = state_with();
        assert!(eval_predicate(&s, &pred("characters.li.arcStage == \"觉醒\"")).unwrap());
        assert!(!eval_predicate(&s, &pred("characters.li.arcStage == \"沉睡\"")).unwrap());
        assert!(eval_predicate(&s, &pred("world.phase == \"night\"")).unwrap());
        assert!(!eval_predicate(&s, &pred("world.phase == \"day\"")).unwrap());
    }

    #[test]
    fn eval_relation_numeric_boundary() {
        let s = state_with(); // trust = 0.5
        assert!(!eval_predicate(&s, &pred("relations[li->wang].trust < 0.5")).unwrap()); // 边界不含
        assert!(eval_predicate(&s, &pred("relations[li->wang].trust < 0.6")).unwrap());
        assert!(eval_predicate(&s, &pred("relations[li->wang].trust == 0.5")).unwrap());
        assert!(!eval_predicate(&s, &pred("relations[li->wang].trust > 0.5")).unwrap());
        // 关系缺失 → 未命中
        assert!(!eval_predicate(&s, &pred("relations[wang->li].trust < 0.9")).unwrap());
    }

    #[test]
    fn eval_unknown_path_is_validation() {
        let s = state_with();
        // 未知字段的表达式在 eval 时经 parse 返回 Validation。
        assert_eq!(eval_predicate(&s, &pred("characters.li.charm contains \"x\"")).unwrap_err().code(), "validation");
    }
}
