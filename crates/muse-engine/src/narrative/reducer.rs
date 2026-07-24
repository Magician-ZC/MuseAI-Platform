//! 确定性 reducer：StatePatch 校验与应用（规格 §9.4 末段 + §12.5 拒绝矩阵）。
//! 文件所有权：agent-E3。纯函数 + 少量存储调用，无模型调用。
//!
//! 拒绝矩阵（必测，§12.5.5）：
//! - base_revision ≠ 当前 revision → Conflict
//! - path 不在白名单 → Validation
//! - precondition 不满足 → Conflict
//! - 应用后命中任一 ForbiddenPredicate → Validation（整个 patch 拒绝，不部分提交）
//! - 同 patch id 重复提交 → 幂等返回已提交结果（不重复应用）
//! - 引用不存在的 characterId/关系端点 → Validation
//!   （例外：关系**边**不存在但 from/to 均为已知角色时，写入路径以零值自动建边
//!   （known_to=[from,to]）——关系演化 A 依赖；precondition 读取仍要求边已存在）

use serde_json::Value;

use super::types::{
    CharacterState, EmotionEntry, NarrativeState, NodeStatus, OutlineNode, PatchOp, PatchOperation,
    RelationState, StatePatch,
};
use crate::EngineError;

/// 路径白名单（前缀匹配 + 段结构校验）。
/// 形如：`world.<key>`、`characters.<id>.(goals|emotions|resources|secrets|misconceptions|plans|arcStage)`、
/// `relations[<from>-><to>].(trust|affinity|fear|debt|knownTo|notes)`、
/// `narrative.(outlineNodes[<id>].status|foreshadowing|pacingNotes)`、
/// `authoring.(lockedSceneIds|branchSnapshotIds)`。
pub const PATH_WHITELIST_DOC: &str = "见模块注释；实现为 parse_path() 的合法产物集合";

/// 幂等账在 world 中的保留键；禁止经 patch 直接写，只由 reducer 维护。
const APPLIED_KEY: &str = "appliedPatchIds";
/// 幂等账保留上限。
const APPLIED_MAX: usize = 200;

/// 解析并校验单个路径；返回结构化路径供 apply 使用。
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedPath {
    World(String),
    Character { id: String, field: String },
    Relation { from: String, to: String, field: String },
    OutlineNodeStatus { node_id: String },
    NarrativeList { field: String },
    AuthoringList { field: String },
}

fn deny(path: &str, reason: &str) -> EngineError {
    EngineError::Validation(format!("路径不在白名单 [{reason}]: {path}"))
}

pub fn parse_path(path: &str) -> Result<ParsedPath, EngineError> {
    // world.<key>：单段 key，不含 . 与 [，且不得触碰内部保留键。
    if let Some(key) = path.strip_prefix("world.") {
        if key.is_empty() || key.contains('.') || key.contains('[') {
            return Err(deny(path, "world 键非法"));
        }
        if key == APPLIED_KEY {
            return Err(deny(path, "world 保留键不可写"));
        }
        return Ok(ParsedPath::World(key.to_string()));
    }

    // characters.<id>.<field>
    if let Some(rest) = path.strip_prefix("characters.") {
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(deny(path, "characters 需 <id>.<field>"));
        }
        let (id, field) = (parts[0], parts[1]);
        match field {
            "goals" | "emotions" | "resources" | "secrets" | "misconceptions" | "plans"
            | "arcStage" | "location" => {
                return Ok(ParsedPath::Character {
                    id: id.to_string(),
                    field: field.to_string(),
                });
            }
            _ => return Err(deny(path, "未知角色字段")),
        }
    }

    // relations[<from>-><to>].<field>
    if let Some(rest) = path.strip_prefix("relations[") {
        let (from, to, field) = parse_bracket_field(rest).ok_or_else(|| deny(path, "关系语法"))?;
        match field.as_str() {
            "trust" | "affinity" | "fear" | "debt" | "knownTo" | "notes" => {
                return Ok(ParsedPath::Relation { from, to, field });
            }
            _ => return Err(deny(path, "未知关系字段")),
        }
    }

    // narrative.outlineNodes[<id>].status | narrative.foreshadowing | narrative.pacingNotes
    if let Some(rest) = path.strip_prefix("narrative.") {
        if let Some(inner) = rest.strip_prefix("outlineNodes[") {
            // 复用括号解析：outlineNodes[<id>].status（无 -> 分隔，用同一函数解析出 field）
            let idx = inner.find(']').ok_or_else(|| deny(path, "outlineNodes 缺 ]"))?;
            let node_id = &inner[..idx];
            let after = &inner[idx + 1..];
            let field = after.strip_prefix('.').ok_or_else(|| deny(path, "outlineNodes 缺字段"))?;
            if node_id.is_empty() || field != "status" {
                return Err(deny(path, "outlineNodes 仅 status 可写"));
            }
            return Ok(ParsedPath::OutlineNodeStatus { node_id: node_id.to_string() });
        }
        match rest {
            "foreshadowing" | "pacingNotes" => {
                return Ok(ParsedPath::NarrativeList { field: rest.to_string() });
            }
            _ => return Err(deny(path, "未知叙事字段")),
        }
    }

    // authoring.<field>
    if let Some(rest) = path.strip_prefix("authoring.") {
        match rest {
            "lockedSceneIds" | "branchSnapshotIds" => {
                return Ok(ParsedPath::AuthoringList { field: rest.to_string() });
            }
            _ => return Err(deny(path, "未知创作字段")),
        }
    }

    Err(deny(path, "未知根段"))
}

/// 解析 `<from>-><to>].<field>` 形式（已消费前缀 `relations[`）。
fn parse_bracket_field(rest: &str) -> Option<(String, String, String)> {
    let idx = rest.find(']')?;
    let bracket = &rest[..idx];
    let after = rest[idx + 1..].strip_prefix('.')?;
    let (from, to) = bracket.split_once("->")?;
    if from.is_empty() || to.is_empty() || after.is_empty() {
        return None;
    }
    Some((from.to_string(), to.to_string(), after.to_string()))
}

/// 校验 + 应用（不落盘）：成功返回新状态（revision+1）与被应用的操作数。
/// 任何一步失败即整体拒绝，输入状态不变（clone-on-apply）。
pub fn validate_and_apply(
    state: &NarrativeState,
    patch: &StatePatch,
) -> Result<NarrativeState, EngineError> {
    // 幂等：同 patch id 已应用 → 直接返回当前状态（不重复应用、不再 bump revision）。
    if already_applied(state, &patch.id) {
        return Ok(state.clone());
    }
    // 旧 revision 拒绝。
    if patch.base_revision != state.revision {
        return Err(EngineError::Conflict(format!(
            "base_revision {} ≠ 当前 revision {}",
            patch.base_revision, state.revision
        )));
    }

    // clone-on-apply：全部在副本上进行，失败时输入状态不可变。
    let mut next = state.clone();
    for op in &patch.operations {
        apply_operation(&mut next, op)?;
    }

    // 禁止谓词后校验：应用后命中任一 → 整个 patch 拒绝。
    for pred in &next.narrative.forbidden_predicates {
        if super::constraints::eval_predicate(&next, pred)? {
            return Err(EngineError::Validation(format!(
                "命中禁止谓词 {}: {}",
                pred.id, pred.reason
            )));
        }
    }

    next.revision += 1;
    record_applied(&mut next, &patch.id);
    Ok(next)
}

/// 应用单个操作：解析路径 → 校验 precondition → 按字段类型分派。
fn apply_operation(next: &mut NarrativeState, op: &PatchOperation) -> Result<(), EngineError> {
    let parsed = parse_path(&op.path)?;

    // 乐观前置条件：当前值必须等于 precondition 才应用。
    if let Some(pre) = &op.precondition {
        let cur = read_current(next, &parsed)?;
        if !values_equal(&cur, pre) {
            return Err(EngineError::Conflict(format!(
                "precondition 不满足: 路径 {} 当前值 {} ≠ 期望 {}",
                op.path, cur, pre
            )));
        }
    }

    match &parsed {
        ParsedPath::World(key) => apply_world(next, key, op),
        ParsedPath::Character { id, field } => {
            let c = next.characters.get_mut(id).ok_or_else(|| dangling_char(id))?;
            apply_character(c, field, op)
        }
        ParsedPath::Relation { from, to, field } => {
            // 引用完整性：两端角色必须存在（悬空端点仍整 patch 拒绝——ghost 类未知角色行为不变）。
            if !next.characters.contains_key(from) {
                return Err(dangling_char(from));
            }
            if !next.characters.contains_key(to) {
                return Err(dangling_char(to));
            }
            // 关系演化（A. relation_dynamics）：写入时若边不存在且 from/to 均为已知角色，
            // 以零值自动建边（known_to=[from,to]）——新世界 relations 初始为空，首笔关系写入
            // 即可落定。仅放宽「边必须先存在」这一点；白名单路径/端点校验机制不变，
            // precondition 读取（read_current）仍要求边已存在。
            if !next.relations.iter().any(|r| &r.from == from && &r.to == to) {
                let mut known_to = vec![from.clone()];
                if to != from {
                    known_to.push(to.clone());
                }
                next.relations.push(RelationState {
                    from: from.clone(),
                    to: to.clone(),
                    trust: 0.0,
                    affinity: 0.0,
                    fear: 0.0,
                    debt: 0.0,
                    known_to,
                    notes: vec![],
                });
            }
            let r = next
                .relations
                .iter_mut()
                .find(|r| &r.from == from && &r.to == to)
                .expect("边已存在或刚建边，必可取到");
            apply_relation(r, field, op)
        }
        ParsedPath::OutlineNodeStatus { node_id } => {
            let n = next
                .narrative
                .outline_nodes
                .iter_mut()
                .find(|n| &n.id == node_id)
                .ok_or_else(|| EngineError::Validation(format!("大纲节点不存在: {node_id}")))?;
            apply_status(n, op)
        }
        ParsedPath::NarrativeList { field } => match field.as_str() {
            "foreshadowing" => apply_str_list(&mut next.narrative.foreshadowing, op),
            _ => apply_str_list(&mut next.narrative.pacing_notes, op),
        },
        ParsedPath::AuthoringList { field } => match field.as_str() {
            "lockedSceneIds" => apply_str_list(&mut next.authoring.locked_scene_ids, op),
            _ => apply_str_list(&mut next.authoring.branch_snapshot_ids, op),
        },
    }
}

fn dangling_char(id: &str) -> EngineError {
    EngineError::Validation(format!("悬空 characterId: {id}"))
}

fn op_kind_err(op: PatchOp, note: &str) -> EngineError {
    EngineError::Validation(format!("操作 {op:?} 不适用: {note}"))
}

// ---------- 取值（供 precondition 比较） ----------

fn read_current(state: &NarrativeState, parsed: &ParsedPath) -> Result<Value, EngineError> {
    match parsed {
        ParsedPath::World(key) => Ok(state.world.get(key).cloned().unwrap_or(Value::Null)),
        ParsedPath::Character { id, field } => {
            let c = state.characters.get(id).ok_or_else(|| dangling_char(id))?;
            char_field_value(c, field)
        }
        ParsedPath::Relation { from, to, field } => {
            let r = state
                .relations
                .iter()
                .find(|r| &r.from == from && &r.to == to)
                .ok_or_else(|| EngineError::Validation(format!("关系不存在: {from}->{to}")))?;
            rel_field_value(r, field)
        }
        ParsedPath::OutlineNodeStatus { node_id } => {
            let n = state
                .narrative
                .outline_nodes
                .iter()
                .find(|n| &n.id == node_id)
                .ok_or_else(|| EngineError::Validation(format!("大纲节点不存在: {node_id}")))?;
            Ok(Value::String(status_str(n.status).to_string()))
        }
        ParsedPath::NarrativeList { field } => Ok(match field.as_str() {
            "foreshadowing" => serde_json::to_value(&state.narrative.foreshadowing)?,
            _ => serde_json::to_value(&state.narrative.pacing_notes)?,
        }),
        ParsedPath::AuthoringList { field } => Ok(match field.as_str() {
            "lockedSceneIds" => serde_json::to_value(&state.authoring.locked_scene_ids)?,
            _ => serde_json::to_value(&state.authoring.branch_snapshot_ids)?,
        }),
    }
}

fn char_field_value(c: &CharacterState, field: &str) -> Result<Value, EngineError> {
    Ok(match field {
        "goals" => serde_json::to_value(&c.goals)?,
        "emotions" => serde_json::to_value(&c.emotions)?,
        "resources" => serde_json::to_value(&c.resources)?,
        "secrets" => serde_json::to_value(&c.secrets)?,
        "misconceptions" => serde_json::to_value(&c.misconceptions)?,
        "plans" => serde_json::to_value(&c.plans)?,
        "location" => Value::String(c.location.clone()),
        _ => Value::String(c.arc_stage.clone()), // arcStage
    })
}

fn rel_field_value(r: &RelationState, field: &str) -> Result<Value, EngineError> {
    Ok(match field {
        "trust" => serde_json::to_value(r.trust)?,
        "affinity" => serde_json::to_value(r.affinity)?,
        "fear" => serde_json::to_value(r.fear)?,
        "debt" => serde_json::to_value(r.debt)?,
        "knownTo" => serde_json::to_value(&r.known_to)?,
        _ => serde_json::to_value(&r.notes)?, // notes
    })
}

/// 数值按 f64 容差比较，其余按 JSON 值精确比较（避免 f32→f64 精度误差）。
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(xf), Some(yf)) => (xf - yf).abs() < 1e-6,
            _ => a == b,
        },
        _ => a == b,
    }
}

// ---------- 应用（按字段类型分派） ----------

fn apply_character(c: &mut CharacterState, field: &str, op: &PatchOperation) -> Result<(), EngineError> {
    match field {
        "goals" => apply_str_list(&mut c.goals, op),
        "resources" => apply_str_list(&mut c.resources, op),
        "secrets" => apply_str_list(&mut c.secrets, op),
        "misconceptions" => apply_str_list(&mut c.misconceptions, op),
        "plans" => apply_str_list(&mut c.plans, op),
        "emotions" => apply_emotions(&mut c.emotions, op),
        // location（Phase 2）：标量 Set 单值（非 list append），与 arcStage 同款；movement 落定路径。
        "location" => apply_scalar_string(&mut c.location, op),
        _ => apply_scalar_string(&mut c.arc_stage, op), // arcStage
    }
}

fn apply_relation(r: &mut RelationState, field: &str, op: &PatchOperation) -> Result<(), EngineError> {
    match field {
        "trust" => apply_num(&mut r.trust, op),
        "affinity" => apply_num(&mut r.affinity, op),
        "fear" => apply_num(&mut r.fear, op),
        "debt" => apply_num(&mut r.debt, op),
        "knownTo" => apply_str_list(&mut r.known_to, op),
        _ => apply_str_list(&mut r.notes, op), // notes
    }
}

/// 字符串列表：Set 整表替换；Append 按集合语义去重追加；Remove 按值删除；Increment 非法。
fn apply_str_list(list: &mut Vec<String>, op: &PatchOperation) -> Result<(), EngineError> {
    match op.op {
        PatchOp::Set => {
            *list = require_str_array(op)?;
            Ok(())
        }
        PatchOp::Append => {
            let v = require_string(op)?;
            if !list.contains(&v) {
                list.push(v);
            }
            Ok(())
        }
        PatchOp::Remove => {
            let v = require_string(op)?;
            list.retain(|x| x != &v);
            Ok(())
        }
        PatchOp::Increment => Err(op_kind_err(op.op, "列表字段不支持 increment")),
    }
}

/// 数值字段：仅 Set / Increment 合法。
fn apply_num(field: &mut f32, op: &PatchOperation) -> Result<(), EngineError> {
    match op.op {
        PatchOp::Set => {
            *field = require_number(op)? as f32;
            Ok(())
        }
        PatchOp::Increment => {
            *field += require_number(op)? as f32;
            Ok(())
        }
        PatchOp::Append | PatchOp::Remove => Err(op_kind_err(op.op, "数值字段仅支持 set/increment")),
    }
}

/// 标量字符串（arcStage）：仅 Set 合法。
fn apply_scalar_string(s: &mut String, op: &PatchOperation) -> Result<(), EngineError> {
    match op.op {
        PatchOp::Set => {
            *s = require_string(op)?;
            Ok(())
        }
        _ => Err(op_kind_err(op.op, "字符串字段仅支持 set")),
    }
}

/// 大纲节点 status：仅 Set 且值须为合法 NodeStatus。
fn apply_status(node: &mut OutlineNode, op: &PatchOperation) -> Result<(), EngineError> {
    match op.op {
        PatchOp::Set => {
            node.status = parse_status(&require_string(op)?)?;
            Ok(())
        }
        _ => Err(op_kind_err(op.op, "status 字段仅支持 set")),
    }
}

/// 情绪列表：Set 整表替换；Append 按 name 覆盖或追加；Remove 按 name 删除；Increment 非法。
fn apply_emotions(list: &mut Vec<EmotionEntry>, op: &PatchOperation) -> Result<(), EngineError> {
    match op.op {
        PatchOp::Set => {
            *list = serde_json::from_value(require_value(op)?.clone())?;
            Ok(())
        }
        PatchOp::Append => {
            let e: EmotionEntry = serde_json::from_value(require_value(op)?.clone())?;
            if let Some(slot) = list.iter_mut().find(|x| x.name == e.name) {
                *slot = e;
            } else {
                list.push(e);
            }
            Ok(())
        }
        PatchOp::Remove => {
            let name = require_string(op)?;
            list.retain(|x| x.name != name);
            Ok(())
        }
        PatchOp::Increment => Err(op_kind_err(op.op, "情绪列表不支持 increment")),
    }
}

/// world 层灵活值：Set/Remove 通用；Append 要求数组；Increment 要求数值。
fn apply_world(next: &mut NarrativeState, key: &str, op: &PatchOperation) -> Result<(), EngineError> {
    match op.op {
        PatchOp::Set => {
            next.world.insert(key.to_string(), require_value(op)?.clone());
            Ok(())
        }
        PatchOp::Remove => {
            next.world.remove(key);
            Ok(())
        }
        PatchOp::Append => {
            let entry = next.world.entry(key.to_string()).or_insert_with(|| Value::Array(vec![]));
            match entry {
                Value::Array(a) => {
                    a.push(require_value(op)?.clone());
                    Ok(())
                }
                _ => Err(op_kind_err(op.op, "world.append 要求目标为数组")),
            }
        }
        PatchOp::Increment => {
            let delta = require_number(op)?;
            let entry = next.world.entry(key.to_string()).or_insert_with(|| Value::from(0.0));
            let cur = entry.as_f64().ok_or_else(|| op_kind_err(op.op, "world.increment 要求目标为数值"))?;
            *entry = Value::from(cur + delta);
            Ok(())
        }
    }
}

fn parse_status(s: &str) -> Result<NodeStatus, EngineError> {
    match s {
        "pending" => Ok(NodeStatus::Pending),
        "done" => Ok(NodeStatus::Done),
        "bypassed" => Ok(NodeStatus::Bypassed),
        "blocked" => Ok(NodeStatus::Blocked),
        _ => Err(EngineError::Validation(format!("非法节点状态: {s}"))),
    }
}

fn status_str(s: NodeStatus) -> &'static str {
    match s {
        NodeStatus::Pending => "pending",
        NodeStatus::Done => "done",
        NodeStatus::Bypassed => "bypassed",
        NodeStatus::Blocked => "blocked",
    }
}

// ---------- value 取值助手 ----------

fn require_value(op: &PatchOperation) -> Result<&Value, EngineError> {
    op.value
        .as_ref()
        .ok_or_else(|| EngineError::Validation(format!("操作缺少 value: {}", op.path)))
}

fn require_string(op: &PatchOperation) -> Result<String, EngineError> {
    match require_value(op)? {
        Value::String(s) => Ok(s.clone()),
        v => Err(EngineError::Validation(format!("期望字符串 value, 实际 {v}"))),
    }
}

fn require_number(op: &PatchOperation) -> Result<f64, EngineError> {
    match require_value(op)? {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| EngineError::Validation("数值 value 无法转 f64".into())),
        v => Err(EngineError::Validation(format!("期望数值 value, 实际 {v}"))),
    }
}

fn require_str_array(op: &PatchOperation) -> Result<Vec<String>, EngineError> {
    match require_value(op)? {
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(String::from)
                    .ok_or_else(|| EngineError::Validation("数组元素须为字符串".into()))
            })
            .collect(),
        v => Err(EngineError::Validation(format!("期望字符串数组 value, 实际 {v}"))),
    }
}

// ---------- 幂等账 ----------

/// 幂等账：已应用 patch id 记录在 world['appliedPatchIds']（有界，保留最近 200 条）。
pub fn already_applied(state: &NarrativeState, patch_id: &str) -> bool {
    state
        .world
        .get(APPLIED_KEY)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().any(|x| x.as_str() == Some(patch_id)))
        .unwrap_or(false)
}

fn record_applied(state: &mut NarrativeState, patch_id: &str) {
    let entry = state.world.entry(APPLIED_KEY.to_string()).or_insert_with(|| Value::Array(vec![]));
    if let Value::Array(a) = entry {
        a.push(Value::String(patch_id.to_string()));
        let len = a.len();
        if len > APPLIED_MAX {
            a.drain(0..len - APPLIED_MAX);
        }
    } else {
        *entry = Value::Array(vec![Value::String(patch_id.to_string())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narrative::types::{ConstraintLevel, ForbiddenPredicate};
    use serde_json::json;

    fn op(op: PatchOp, path: &str, value: Option<Value>) -> PatchOperation {
        PatchOperation { op, path: path.into(), value, precondition: None }
    }

    /// 含两个角色 li/wang、一条 li->wang 关系、一个 pending 节点、一条禁止谓词的基态。
    fn base_state() -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: "run1".into(), ..Default::default() };
        s.characters.insert("li".into(), CharacterState::default());
        s.characters.insert("wang".into(), CharacterState::default());
        s.relations.push(RelationState {
            from: "li".into(),
            to: "wang".into(),
            trust: 0.5,
            affinity: 0.2,
            fear: 0.0,
            debt: 0.0,
            known_to: vec!["li".into()],
            notes: vec![],
        });
        s.narrative.outline_nodes.push(OutlineNode {
            id: "n1".into(),
            summary: "开场".into(),
            constraint: ConstraintLevel::Hard,
            status: NodeStatus::Pending,
            threshold: None,
            advance_when: None,
            weights: None,
        });
        s.narrative.forbidden_predicates.push(ForbiddenPredicate {
            id: "f1".into(),
            expression: "characters.li.secrets contains \"身世\"".into(),
            reason: "身世不得泄露".into(),
        });
        s
    }

    fn patch(id: &str, base: u64, ops: Vec<PatchOperation>) -> StatePatch {
        StatePatch { id: id.into(), base_revision: base, source_decision_ids: vec![], operations: ops }
    }

    // ---- parse_path 白名单 ----

    #[test]
    fn parse_path_accepts_whitelist() {
        assert_eq!(parse_path("world.weather").unwrap(), ParsedPath::World("weather".into()));
        assert_eq!(
            parse_path("characters.li.goals").unwrap(),
            ParsedPath::Character { id: "li".into(), field: "goals".into() }
        );
        assert_eq!(
            parse_path("characters.li.arcStage").unwrap(),
            ParsedPath::Character { id: "li".into(), field: "arcStage".into() }
        );
        assert_eq!(
            parse_path("characters.li.location").unwrap(),
            ParsedPath::Character { id: "li".into(), field: "location".into() }
        );
        assert_eq!(
            parse_path("relations[li->wang].trust").unwrap(),
            ParsedPath::Relation { from: "li".into(), to: "wang".into(), field: "trust".into() }
        );
        assert_eq!(
            parse_path("narrative.outlineNodes[n1].status").unwrap(),
            ParsedPath::OutlineNodeStatus { node_id: "n1".into() }
        );
        assert_eq!(
            parse_path("narrative.foreshadowing").unwrap(),
            ParsedPath::NarrativeList { field: "foreshadowing".into() }
        );
        assert_eq!(
            parse_path("authoring.lockedSceneIds").unwrap(),
            ParsedPath::AuthoringList { field: "lockedSceneIds".into() }
        );
    }

    #[test]
    fn parse_path_rejects_offpath() {
        for bad in [
            "foo.bar",
            "characters.li",
            "characters.li.unknown",
            "characters.li.goals.extra",
            "relations[li].trust",
            "relations[li->wang].charm",
            "narrative.outlineNodes[n1].summary",
            "narrative.random",
            "authoring.secret",
            "world.appliedPatchIds", // 保留键
            "world.a.b",             // 多段
        ] {
            assert_eq!(parse_path(bad).unwrap_err().code(), "validation", "应拒绝: {bad}");
        }
    }

    // ---- 拒绝矩阵 ----

    #[test]
    fn stale_revision_rejected() {
        let s = base_state(); // revision 0
        let p = patch("p1", 5, vec![op(PatchOp::Append, "characters.li.goals", Some(json!("逃跑")))]);
        let err = validate_and_apply(&s, &p).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn unknown_path_rejected() {
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Set, "characters.li.unknown", Some(json!("x")))]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
    }

    #[test]
    fn precondition_mismatch_rejected_and_input_unchanged() {
        let s = base_state();
        let mut o = op(PatchOp::Set, "relations[li->wang].trust", Some(json!(0.9)));
        o.precondition = Some(json!(0.1)); // 实际 0.5，不满足
        let p = patch("p1", 0, vec![o]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "conflict");
        // 输入不变
        assert_eq!(s.relations[0].trust, 0.5);
        assert_eq!(s.revision, 0);
    }

    #[test]
    fn precondition_match_applies() {
        let s = base_state();
        let mut o = op(PatchOp::Set, "relations[li->wang].trust", Some(json!(0.9)));
        o.precondition = Some(json!(0.5));
        let p = patch("p1", 0, vec![o]);
        let next = validate_and_apply(&s, &p).unwrap();
        assert!((next.relations[0].trust - 0.9).abs() < 1e-6);
    }

    #[test]
    fn forbidden_predicate_rejects_whole_patch() {
        let s = base_state();
        // 一次 patch 两步：先加无害目标，再触发禁止谓词 → 整体拒绝，两步都不生效。
        let p = patch(
            "p1",
            0,
            vec![
                op(PatchOp::Append, "characters.li.goals", Some(json!("求生"))),
                op(PatchOp::Append, "characters.li.secrets", Some(json!("身世"))),
            ],
        );
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
        assert!(s.characters["li"].goals.is_empty());
        assert!(s.characters["li"].secrets.is_empty());
    }

    #[test]
    fn idempotent_same_patch_id() {
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Increment, "relations[li->wang].trust", Some(json!(0.1)))]);
        let s1 = validate_and_apply(&s, &p).unwrap();
        assert_eq!(s1.revision, 1);
        assert!((s1.relations[0].trust - 0.6).abs() < 1e-6);
        assert!(already_applied(&s1, "p1"));
        // 同 id 重复提交（base_revision 仍写 0）：不重复应用、不再 bump。
        let s2 = validate_and_apply(&s1, &p).unwrap();
        assert_eq!(s2.revision, 1);
        assert!((s2.relations[0].trust - 0.6).abs() < 1e-6);
    }

    #[test]
    fn dangling_character_rejected() {
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Append, "characters.ghost.goals", Some(json!("x")))]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
    }

    #[test]
    fn dangling_relation_endpoint_rejected() {
        let s = base_state();
        // 关系两端角色缺失
        let p = patch("p1", 0, vec![op(PatchOp::Set, "relations[li->ghost].trust", Some(json!(0.3)))]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
        // 反向亦然：from 为未知角色同样拒绝（自动建边仅限两端皆已知角色）。
        let p2 = patch("p2", 0, vec![op(PatchOp::Set, "relations[ghost->li].trust", Some(json!(0.3)))]);
        assert_eq!(validate_and_apply(&s, &p2).unwrap_err().code(), "validation");
    }

    // ---- 关系演化（A）：边不存在 + 两端已知角色 → 零值自动建边 ----

    #[test]
    fn relation_write_auto_creates_edge_with_known_to() {
        // base_state 只有 li->wang；wang->li 不存在。Set 写入 → 零值自动建边后落定。
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Set, "relations[wang->li].fear", Some(json!(0.1)))]);
        let next = validate_and_apply(&s, &p).unwrap();
        let r = next.relations.iter().find(|r| r.from == "wang" && r.to == "li").expect("应自动建边");
        assert!((r.fear - 0.1).abs() < 1e-6);
        // 其余字段零值基线；known_to=[from,to]。
        assert_eq!(r.trust, 0.0);
        assert_eq!(r.affinity, 0.0);
        assert_eq!(r.debt, 0.0);
        assert_eq!(r.known_to, vec!["wang".to_string(), "li".to_string()]);
        assert!(r.notes.is_empty());
        // 既有 li->wang 边不受影响。
        assert!((next.relations.iter().find(|r| r.from == "li" && r.to == "wang").unwrap().trust - 0.5).abs() < 1e-6);
    }

    #[test]
    fn relation_increment_auto_creates_edge_from_zero() {
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Increment, "relations[wang->li].trust", Some(json!(0.06)))]);
        let next = validate_and_apply(&s, &p).unwrap();
        let r = next.relations.iter().find(|r| r.from == "wang" && r.to == "li").unwrap();
        assert!((r.trust - 0.06).abs() < 1e-6, "零值建边后 Increment 以 0 为基");
    }

    #[test]
    fn relation_precondition_on_missing_edge_still_rejected() {
        // 自动建边只放宽写入路径；precondition 读取仍要求边已存在（保持乐观校验语义）。
        let s = base_state();
        let mut o = op(PatchOp::Set, "relations[wang->li].trust", Some(json!(0.3)));
        o.precondition = Some(json!(0.0));
        let p = patch("p1", 0, vec![o]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
        // 输入不变：未因失败留下半建的边。
        assert!(s.relations.iter().all(|r| !(r.from == "wang" && r.to == "li")));
    }

    // ---- 类型合法性 ----

    #[test]
    fn increment_on_list_rejected() {
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Increment, "characters.li.goals", Some(json!(1)))]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
    }

    #[test]
    fn append_on_numeric_rejected() {
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Append, "relations[li->wang].trust", Some(json!(0.1)))]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
    }

    #[test]
    fn set_append_remove_apply_correctly() {
        let s = base_state();
        let p = patch(
            "p1",
            0,
            vec![
                op(PatchOp::Set, "characters.li.arcStage", Some(json!("觉醒"))),
                op(PatchOp::Append, "characters.li.goals", Some(json!("复仇"))),
                op(PatchOp::Append, "characters.li.goals", Some(json!("复仇"))), // 去重
                op(PatchOp::Set, "narrative.outlineNodes[n1].status", Some(json!("done"))),
            ],
        );
        let next = validate_and_apply(&s, &p).unwrap();
        assert_eq!(next.characters["li"].arc_stage, "觉醒");
        assert_eq!(next.characters["li"].goals, vec!["复仇".to_string()]);
        assert_eq!(next.narrative.outline_nodes[0].status, NodeStatus::Done);
        assert_eq!(next.revision, 1);
    }

    // ---- Phase 2：location 标量 Set（movement 落定路径） ----

    #[test]
    fn location_set_applies_as_scalar() {
        let s = base_state(); // li/wang location 默认 ""
        let p = patch("p1", 0, vec![op(PatchOp::Set, "characters.li.location", Some(json!("密室")))]);
        let next = validate_and_apply(&s, &p).unwrap();
        assert_eq!(next.characters["li"].location, "密室");
        assert_eq!(next.revision, 1);
    }

    #[test]
    fn location_rejects_non_set_ops() {
        // location 是标量字符串：Append/Remove/Increment 一律非法。
        let s = base_state();
        for bad in [PatchOp::Append, PatchOp::Remove, PatchOp::Increment] {
            let p = patch("p1", 0, vec![op(bad, "characters.li.location", Some(json!("密室")))]);
            assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation", "应拒绝 {bad:?}");
        }
    }

    #[test]
    fn location_precondition_gates_move() {
        // 乐观前置：仅当当前 location 等于期望才落定（movement 幂等/CAS 佐证）。
        let mut s = base_state();
        s.characters.get_mut("li").unwrap().location = "前厅".into();
        let mut o = op(PatchOp::Set, "characters.li.location", Some(json!("密室")));
        o.precondition = Some(json!("前厅"));
        let next = validate_and_apply(&s, &patch("p1", 0, vec![o])).unwrap();
        assert_eq!(next.characters["li"].location, "密室");
        // 前置不满足 → Conflict，输入不变。
        let mut o2 = op(PatchOp::Set, "characters.li.location", Some(json!("密室")));
        o2.precondition = Some(json!("别处"));
        assert_eq!(validate_and_apply(&s, &patch("p2", 0, vec![o2])).unwrap_err().code(), "conflict");
        assert_eq!(s.characters["li"].location, "前厅");
    }

    #[test]
    fn applied_patch_ids_bounded() {
        let mut s = base_state();
        for i in 0..(APPLIED_MAX as u64 + 10) {
            let p = patch(&format!("p{i}"), i, vec![]);
            s = validate_and_apply(&s, &p).unwrap();
        }
        let len = s.world.get(APPLIED_KEY).unwrap().as_array().unwrap().len();
        assert_eq!(len, APPLIED_MAX);
        // 最早的应已被裁掉，最近的保留。
        assert!(!already_applied(&s, "p0"));
        assert!(already_applied(&s, &format!("p{}", APPLIED_MAX as u64 + 9)));
    }

    #[test]
    fn invalid_node_status_rejected() {
        let s = base_state();
        let p = patch("p1", 0, vec![op(PatchOp::Set, "narrative.outlineNodes[n1].status", Some(json!("finished")))]);
        assert_eq!(validate_and_apply(&s, &p).unwrap_err().code(), "validation");
    }
}
