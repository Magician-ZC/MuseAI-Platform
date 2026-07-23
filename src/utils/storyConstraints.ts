// 大纲约束解析/序列化 + 禁止谓词表达式校验。
// 契约与 crates/muse-engine/src/narrative/{types,constraints}.rs 一致：
// - 大纲：一行一节点，前缀 [硬]/[软]/[自由]，缺省软，空行忽略。
// - 禁止谓词：受限 DSL 四形态，此处做正则级（结构）校验，与 Rust DSL 同构。

// ---------- 类型（镜像 narrative/types.rs 的 camelCase 序列化形态） ----------

export type ConstraintLevel = 'hard' | 'soft' | 'free';
export type NodeStatus = 'pending' | 'done' | 'bypassed' | 'blocked';

export interface OutlineNode {
  id: string;
  summary: string;
  constraint: ConstraintLevel;
  status: NodeStatus;
}

export interface ForbiddenPredicate {
  id: string;
  /** 受限 DSL 表达式，见 validateForbiddenExpression */
  expression: string;
  reason: string;
}

// ---------- 大纲 ⇄ 文本 ----------

const PREFIX_TO_LEVEL: Record<string, ConstraintLevel> = {
  硬: 'hard',
  软: 'soft',
  自由: 'free',
};

const LEVEL_TO_PREFIX: Record<ConstraintLevel, string> = {
  hard: '[硬]',
  soft: '[软]',
  free: '[自由]',
};

// 容忍半角 [] 与全角 【】括号，前缀后可有空白。
const OUTLINE_PREFIX_RE = /^[[【]\s*(硬|软|自由)\s*[\]】]\s*(.*)$/;

export interface ParseOutlineOptions {
  /** 生成节点 id 的前缀，默认 'node'（node-1、node-2…） */
  idPrefix?: string;
}

/** 用户大纲文本 → OutlineNode[]（新节点一律 pending，缺省软，空行忽略）。 */
export function parseOutline(text: string, options: ParseOutlineOptions = {}): OutlineNode[] {
  const idPrefix = options.idPrefix ?? 'node';
  const nodes: OutlineNode[] = [];
  for (const line of (text ?? '').split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue; // 空行忽略
    const match = trimmed.match(OUTLINE_PREFIX_RE);
    let constraint: ConstraintLevel = 'soft'; // 缺省软
    let summary = trimmed;
    if (match) {
      constraint = PREFIX_TO_LEVEL[match[1]];
      summary = match[2].trim();
    }
    nodes.push({
      id: `${idPrefix}-${nodes.length + 1}`,
      summary,
      constraint,
      status: 'pending',
    });
  }
  return nodes;
}

export interface SerializeOutlineOptions {
  /** true 时软节点也带 [软] 前缀；默认 false（沿用「缺省软」，软节点不写前缀）。 */
  explicitSoft?: boolean;
}

/** OutlineNode[] → 文本，与 parseOutline 互逆（默认软节点省略前缀，往返幂等）。 */
export function serializeOutline(
  nodes: OutlineNode[],
  options: SerializeOutlineOptions = {},
): string {
  const explicitSoft = options.explicitSoft ?? false;
  return nodes
    .map((node) => {
      const summary = node.summary.trim();
      if (node.constraint === 'soft' && !explicitSoft) return summary;
      return `${LEVEL_TO_PREFIX[node.constraint]} ${summary}`.trim();
    })
    .join('\n');
}

// ---------- 禁止谓词表达式校验（四形态） ----------

export type PredicateForm = 'contains' | 'arcStage' | 'world' | 'relation';

export interface PredicateValidation {
  valid: boolean;
  form?: PredicateForm;
  /** 简体中文错误说明（valid=false 时给出） */
  error?: string;
}

const ID_RE = '[A-Za-z0-9_\\-]+';
const FIELD_RE = '[A-Za-z_][A-Za-z0-9_]*';

// characters.<id>.<listField> contains "<literal>"
const CONTAINS_RE = new RegExp(`^characters\\.(${ID_RE})\\.(${FIELD_RE})\\s+contains\\s+"([^"]*)"$`);
// characters.<id>.arcStage == "<literal>"
const ARC_STAGE_RE = new RegExp(`^characters\\.(${ID_RE})\\.arcStage\\s*==\\s*"([^"]*)"$`);
// world.<key> == <json literal>
const WORLD_RE = new RegExp(`^world\\.(${ID_RE})\\s*==\\s*(.+)$`);
// relations[<from>-><to>].<numField> (<|>|==) <number>
const RELATION_RE = new RegExp(
  `^relations\\[(${ID_RE})->(${ID_RE})\\]\\.(${FIELD_RE})\\s*(<|>|==)\\s*(-?\\d+(?:\\.\\d+)?)$`,
);

/**
 * 校验禁止谓词表达式是否符合四形态之一（结构/正则级，与 Rust 受限 DSL 同构）：
 * 1. characters.<id>.<字段> contains "值"
 * 2. characters.<id>.arcStage == "值"
 * 3. world.<键> == <JSON 字面量>
 * 4. relations[<from>-><to>].<数值字段> (<|>|==) <数字>
 */
export function validateForbiddenExpression(expression: string): PredicateValidation {
  const expr = (expression ?? '').trim();
  if (!expr) return { valid: false, error: '表达式为空' };

  if (expr.startsWith('characters.')) {
    if (ARC_STAGE_RE.test(expr)) return { valid: true, form: 'arcStage' };
    if (CONTAINS_RE.test(expr)) return { valid: true, form: 'contains' };
    return {
      valid: false,
      error: 'characters 谓词须为 `characters.<id>.<字段> contains "值"` 或 `characters.<id>.arcStage == "值"`',
    };
  }

  if (expr.startsWith('world.')) {
    const match = expr.match(WORLD_RE);
    if (!match) return { valid: false, error: 'world 谓词须为 `world.<键> == <JSON 字面量>`' };
    try {
      JSON.parse(match[2].trim());
    } catch {
      return { valid: false, error: 'world 谓词右侧不是合法 JSON 字面量' };
    }
    return { valid: true, form: 'world' };
  }

  if (expr.startsWith('relations[')) {
    if (RELATION_RE.test(expr)) return { valid: true, form: 'relation' };
    return {
      valid: false,
      error: 'relations 谓词须为 `relations[<from>-><to>].<数值字段> (<|>|==) <数字>`',
    };
  }

  return { valid: false, error: '无法识别的谓词形态（应以 characters. / world. / relations[ 开头）' };
}
