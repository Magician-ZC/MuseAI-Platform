// 角色测试：互换/压力测试的报告类型 + invoke 参数组装 + 同卡短路判断。
// 报告类型与 crates/muse-engine/src/character/types.rs 的 camelCase 序列化形态一致。
import type { CharacterCardV2 } from './characterCardV2';
import type { ModelProfile } from '../stores/useExtractionStore';

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

/**
 * 互换/压力测试共用的完整请求 DTO，与 Rust `SwapTestRequestDto`
 * （src-tauri/src/commands/character_v2.rs:138-149）camelCase 逐字段对齐。
 * 两个命令都要求携带模型 profile 与两段评测 system prompt（swap/stress）；
 * 互换用 cardA/cardB/scenario，压力用 cardA/scenarios。
 * 调用时须以 `{ request }` 包裹（tauri command 形参名为 `request`）。
 */
export interface SwapTestRequestDto {
  profile: ModelProfile;
  swapPrompt: string;
  stressPrompt: string;
  promptVersion?: string;
  cardA: CharacterCardV2;
  cardB?: CharacterCardV2;
  scenario?: string;
  scenarios?: string[];
}

/** 评测前置配置：从 useSettingsStore 取选中模型 profile + 两段测试提示词。 */
export interface EvalConfig {
  profile: ModelProfile;
  swapPrompt: string;
  stressPrompt: string;
  promptVersion?: string;
}

/** 组装 run_character_swap_test 的完整 request（补齐 profile + 两段 prompt）。 */
export function buildSwapTestRequest(
  config: EvalConfig,
  cardA: CharacterCardV2,
  cardB: CharacterCardV2,
  scenario: string,
): SwapTestRequestDto {
  return {
    profile: config.profile,
    swapPrompt: config.swapPrompt,
    stressPrompt: config.stressPrompt,
    promptVersion: config.promptVersion,
    cardA,
    cardB,
    scenario,
  };
}

/** 组装 run_character_stress_test 的完整 request（补齐 profile + 两段 prompt）。 */
export function buildStressTestRequest(
  config: EvalConfig,
  card: CharacterCardV2,
  scenarios: string[],
): SwapTestRequestDto {
  return {
    profile: config.profile,
    swapPrompt: config.swapPrompt,
    stressPrompt: config.stressPrompt,
    promptVersion: config.promptVersion,
    cardA: card,
    scenarios: [...scenarios],
  };
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
