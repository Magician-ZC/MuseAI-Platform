// 角色测试：互换/压力测试的报告类型 + invoke 参数组装 + 同卡短路判断。
// 报告类型与 crates/muse-engine/src/character/types.rs 的 camelCase 序列化形态一致。
import type { CharacterCardV2 } from './characterCardV2';

// ---------- 报告类型（镜像 types.rs） ----------

export interface SwapFinding {
  dimension: string;
  aBehavior: string;
  bBehavior: string;
  distinct: boolean;
}

export interface SwapTestReport {
  cardA: string;
  cardB: string;
  scenario: string;
  /** 各维度的差异描述与是否可互换判定 */
  findings: SwapFinding[];
  interchangeable: boolean;
  summary: string;
}

export interface StressScenarioResult {
  scenario: string;
  predictedChoice: string;
  rationale: string;
  consistentWithCore: boolean;
}

export interface StressTestReport {
  cardId: string;
  scenarios: StressScenarioResult[];
  consistent: boolean;
  summary: string;
}

// ---------- invoke 参数组装 ----------

/** Rust 薄壳命令名（规格 §10.1） */
export const SWAP_TEST_COMMAND = 'run_character_swap_test' as const;
export const STRESS_TEST_COMMAND = 'run_character_stress_test' as const;

export interface SwapTestRequest {
  cardA: CharacterCardV2;
  cardB: CharacterCardV2;
  scenario: string;
}

export interface StressTestRequest {
  card: CharacterCardV2;
  scenarios: string[];
}

/** 组装 run_character_swap_test 的 invoke 参数对象。 */
export function buildSwapTestRequest(
  cardA: CharacterCardV2,
  cardB: CharacterCardV2,
  scenario: string,
): SwapTestRequest {
  return { cardA, cardB, scenario };
}

/** 组装 run_character_stress_test 的 invoke 参数对象。 */
export function buildStressTestRequest(
  card: CharacterCardV2,
  scenarios: string[],
): StressTestRequest {
  return { card, scenarios: [...scenarios] };
}

// ---------- 同卡短路判断 ----------

// 内容比较时排除的易变/标识字段（id/时间戳/revision）。
const VOLATILE_KEYS: ReadonlyArray<keyof CharacterCardV2> = [
  'id',
  'createdAt',
  'updatedAt',
  'revision',
];

// 稳定序列化：对象键排序、跳过 undefined；数组保序（valuePriorities 等顺序有意义）。
function stableStringify(value: unknown): string {
  if (value === null || typeof value !== 'object') {
    return JSON.stringify(value ?? null);
  }
  if (Array.isArray(value)) {
    return `[${value.map(stableStringify).join(',')}]`;
  }
  const obj = value as Record<string, unknown>;
  const keys = Object.keys(obj)
    .filter((key) => obj[key] !== undefined)
    .sort();
  return `{${keys.map((key) => `${JSON.stringify(key)}:${stableStringify(obj[key])}`).join(',')}}`;
}

function canonicalContent(card: CharacterCardV2): string {
  const clone: Record<string, unknown> = { ...card };
  for (const key of VOLATILE_KEYS) {
    delete clone[key];
  }
  return stableStringify(clone);
}

// FNV-1a 32 位，输出 8 位十六进制。
function fnv1a(input: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i += 1) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
}

/** 卡内容哈希（排除 id/时间戳/revision），用于缓存键与短路判断。 */
export function cardContentHash(card: CharacterCardV2): string {
  return fnv1a(canonicalContent(card));
}

/** 两张卡的行为内容是否一致（排除 id/时间戳/revision）；互换测试同卡短路用。 */
export function isSameCardContent(a: CharacterCardV2, b: CharacterCardV2): boolean {
  return canonicalContent(a) === canonicalContent(b);
}

/** 同卡短路时直接给出「无差异、可互换」的互换报告，无需调用模型。 */
export function buildIdenticalSwapReport(
  cardA: CharacterCardV2,
  cardB: CharacterCardV2,
  scenario: string,
): SwapTestReport {
  return {
    cardA: cardA.id,
    cardB: cardB.id,
    scenario,
    findings: [],
    interchangeable: true,
    summary: '两张卡内容一致（同卡复制），无需调用模型即判定为可互换。',
  };
}
