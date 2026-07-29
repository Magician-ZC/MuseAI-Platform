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

/// 析取分隔符：`A || B` = 「A 或 B，任一条成立即成立」。
const OR: &str = "||";

/// 按**顶层** `||` 切分——字符串字面量内部的 `||` 不算分隔符。
///
/// ⚠️ 不做这一步的后果很具体：`characters.a.goals contains "打通北门||南门"` 会被从中间劈开，
/// 两半都不是合法谓词 ⇒ 建模板期报一个跟作者写的东西对不上的错。
fn split_disjuncts(expr: &str) -> Vec<&str> {
    let b = expr.as_bytes();
    let (mut out, mut start, mut i, mut in_str, mut escaped) = (Vec::new(), 0usize, 0usize, false, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        // ⚠️ 从 `OR` 派生，**不写死** `b'|'`：写死的话这个常量就成了摆设——
        // 改它不会改变任何行为，而下一个人会以为改了。
        // （做故障注入时当场撞见：把 OR 换成 `&&` 竟然什么都没变。）
        if b[i..].starts_with(OR.as_bytes()) {
            out.push(&expr[start..i]);
            i += OR.len();
            start = i;
            continue;
        }
        i += 1;
    }
    out.push(&expr[start..]);
    out
}

/// 解析一个推进门表达式：一条或多条以 `||` 相连的谓词。
///
/// ══════════════════════════════════════════════════════════════════════════
/// 🔴 **只支持析取（或），刻意不支持合取（与）**
/// ══════════════════════════════════════════════════════════════════════════
/// 这不是「还没做」，是一个设计决定，有两个理由：
///
/// 1. **产品上：没有 DM 兜底。** 剧场有导演在现场调节奏；这里只有 endgame 策略和仲裁，
///    两者都不会看着场上说「这组人卡住了，我放点水」。合取门（全都要）会让「差一条」
///    变成「卡死」，而那条差的可能**根本没人分到**——钩子是私有的、按执念分配的，
///    没人执念对得上的那条支线就没人会去做。
/// 2. **工程上：失败方向不对称。** 合取门的失败是**静默**的——世界照常跑、照常结算，
///    只是那扇门永远不开，没有任何报错；而析取门只会「更容易开」，
///    失败方向是安全的（顶多是主线推得比预期快，那看得见）。
///
/// 🔵 真需要「全都要」时，正确的表达是**拆成多个连续的主线节点**——
/// 那本来就是大纲的表达方式，而且每一步都看得见推没推动。把它压进一个谓词只是把
/// 「三件事」写成一行，代价是失去了中间的可观测性。
///
/// 单条谓词（无 `||`）⇒ 恰好一个析取项 ⇒ 与本层落地前**逐字节等价**。
fn parse_gate(expression: &str) -> Result<Vec<Predicate>, EngineError> {
    let parts = split_disjuncts(expression);
    let mut out = Vec::with_capacity(parts.len());
    for (i, raw) in parts.iter().enumerate() {
        let t = raw.trim();
        if t.is_empty() {
            return Err(err(format!(
                "推进门第 {} 个析取项为空（`{OR}` 两侧都要有谓词）：`{expression}`",
                i + 1
            )));
        }
        out.push(parse(t)?);
    }
    Ok(out)
}

/// 创建时校验谓词表达式语法；解析失败 → Validation（运行时不应再失败）。
///
/// 支持 `A || B || C`（任一成立即成立）。理由与「为何不支持合取」见 [`parse_gate`]。
pub fn parse_predicate(expression: &str) -> Result<(), EngineError> {
    parse_gate(expression).map(|_| ())
}

/// 求值：状态命中谓词返回 true。表达式非法 → Validation；引用的实体缺失视为「未命中」（false）。
///
/// 多个析取项时**任一命中即 true**，且**短路**：前一条成立就不再算后面的。
/// 🔵 短路在这里不只是性能——它让「把最可能的那条路写在前面」成为一种可用的表达。
pub fn eval_predicate(state: &NarrativeState, predicate: &ForbiddenPredicate) -> Result<bool, EngineError> {
    for pred in parse_gate(&predicate.expression)? {
        if eval_one(state, pred) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 单个谓词求值。引用的实体缺失视为「未命中」（false）。
fn eval_one(state: &NarrativeState, pred: Predicate) -> bool {
    match pred {
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
    }
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
            // 文本大纲不带阈值/谓词/权重配置（老式节点，走 build_patch 兼容路径）。
            threshold: None,
            advance_when: None,
            weights: None,
            // 也不带宿命时刻：手写大纲没有「原著第几章」这个坐标，故恒等人来推。
            due_at: None,
            at_location: None,
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

    // ═══════════════════════════════════════════════════════════════════════
    // 析取门（第 3 步）：多条路任一条通
    // ═══════════════════════════════════════════════════════════════════════

    /// 🔴 **任一析取项成立即成立。**
    ///
    /// 这是「没有 DM」这条约束在语法上的落点：主线门必须能写「多条路任一条通」，
    /// 否则难度会两极分化——没做支线的人觉得主线不可能，做了的人觉得太简单。
    #[test]
    fn any_one_branch_opens_the_gate() {
        let s = state_with(); // trust = 0.5，world.flag 见 state_with
        // 第一条不成立、第二条成立 → 开。
        assert!(eval_predicate(&s, &pred("relations[li->wang].trust > 0.9 || relations[li->wang].trust == 0.5")).unwrap());
        // 第一条成立、第二条不成立 → 开（且短路，不会因为后一条非法而失败——见下一条用例）。
        assert!(eval_predicate(&s, &pred("relations[li->wang].trust == 0.5 || relations[li->wang].trust > 0.9")).unwrap());
        // 全不成立 → 不开。
        assert!(!eval_predicate(&s, &pred("relations[li->wang].trust > 0.9 || relations[li->wang].trust < 0.1")).unwrap());
    }

    /// 🔴 **单条谓词（无 `||`）的行为逐字节不变。**
    ///
    /// 这是这一层能作为纯增量上线的全部依据：全部存量模板写的都是单条谓词。
    #[test]
    fn a_single_predicate_behaves_exactly_as_before() {
        let s = state_with();
        for (expr, want) in [
            ("relations[li->wang].trust == 0.5", true),
            ("relations[li->wang].trust > 0.9", false),
            ("characters.li.secrets contains \"身世\"", true),
            ("characters.li.secrets contains \"不存在的秘密\"", false),
            ("characters.li.arcStage == \"觉醒\"", true),
            ("world.phase == \"night\"", true),
        ] {
            assert_eq!(eval_predicate(&s, &pred(expr)).unwrap(), want, "表达式 `{expr}`");
            assert!(parse_predicate(expr).is_ok(), "表达式 `{expr}` 应可通过建模板期校验");
        }
    }

    /// 🔴 **字符串字面量里的 `||` 不是分隔符。**
    ///
    /// 不做这一步的后果很具体：`contains "打通北门||南门"` 会被从中间劈开，
    /// 两半都不是合法谓词 ⇒ 建模板期报一个跟作者写的东西对不上的错。
    #[test]
    fn a_pipe_inside_a_string_literal_is_not_a_separator() {
        assert!(parse_predicate("characters.li.secrets contains \"打通北门||南门\"").is_ok());
        let parts = super::split_disjuncts("characters.li.secrets contains \"a||b\"");
        assert_eq!(parts.len(), 1, "引号内的 || 不得切分：{parts:?}");
        // 引号外的照切。
        assert_eq!(super::split_disjuncts("a == 1 || b == 2").len(), 2);
        // 混合：引号内不切、引号外切。
        assert_eq!(
            super::split_disjuncts("characters.li.secrets contains \"a||b\" || world.x == true").len(),
            2
        );
    }

    /// 空析取项直接拒（`A || ` / ` || B` / `A |||| B`）——建模板期就该看见。
    #[test]
    fn an_empty_branch_is_rejected_at_authoring_time() {
        for bad in ["world.x == true || ", " || world.x == true", "world.x == true |||| world.y == true"] {
            let e = parse_predicate(bad).unwrap_err();
            assert_eq!(e.code(), "validation", "`{bad}` 应在建模板期被拒");
            assert!(format!("{e}").contains("析取项为空"), "报错要说清是哪种问题：{e}");
        }
    }

    /// 🔴 **刻意不支持合取（`&&`）**——它不是被漏掉的，是被排除的。
    ///
    /// 理由见 `parse_gate` 的注释：没有 DM 兜底时，合取门的失败是**静默**的
    /// （世界照常跑，只是那扇门永远不开）。真需要「全都要」时应当拆成多个连续的主线节点。
    /// 这条用例把那个决定钉死——将来有人顺手加上 `&&` 会当场红，逼一次显式评审。
    #[test]
    fn conjunction_is_deliberately_unsupported() {
        let e = parse_predicate("world.x == true && world.y == true").unwrap_err();
        assert_eq!(e.code(), "validation", "`&&` 必须被拒，而不是被当成某种意思悄悄接受");
    }

    #[test]
    fn eval_unknown_path_is_validation() {
        let s = state_with();
        // 未知字段的表达式在 eval 时经 parse 返回 Validation。
        assert_eq!(eval_predicate(&s, &pred("characters.li.charm contains \"x\"")).unwrap_err().code(), "validation");
    }
}
