// Character DNA V2：类型定义（规格 §9.1，逐字段照抄）+ V1→V2 迁移（§10.1）+ draft/reviewed/ready 校验。
// 与 crates/muse-engine/src/character/types.rs 的 serde camelCase 序列化形态字段级一致。
import type { PartnerItem, PartnerItemFields } from '../stores/usePartnerStore';

// ---------- 枚举/字面量类型 ----------

export type CardLifecycle = 'draft' | 'reviewed' | 'ready';
export type Importance = 'core' | 'major' | 'functional';
export type EvidenceKind = 'description' | 'action' | 'otherView' | 'inference';
export type Confidence = 'high' | 'medium' | 'low';

// ---------- 证据与规则 ----------

export interface EvidenceLocator {
  start: number;
  end: number;
  heading?: string;
}

export interface EvidenceRef {
  id: string;
  sourceId: string;
  chapterIndex: number; // 全书章节位置
  locator: EvidenceLocator;
  quotePreview: string; // UI 预览，≤200 字；非完整原文副本
  kind: EvidenceKind;
  confidence: Confidence;
  userConfirmed?: boolean;
  conflictsWith?: string[]; // 互相矛盾的证据 id
}

export interface DecisionRule {
  when: string; // 当……时
  then: string; // 通常会……
  because: string; // 因为……
  evidenceIds?: string[];
}

// ---------- 十层结构 ----------

export interface SourceWork {
  sourceId: string;
  title: string;
  version?: string;
}

// A 基础身份层（V1 字段迁移目的地；含别名与指代）
export interface Identity {
  name: string;
  aliases: string[];
  narrativeRole?: string; // 主角/对手/盟友/导师/催化者
  importance: Importance;
  sourceWork?: SourceWork;
  legacyV1Fields?: Record<string, unknown>; // V1 原样保留区，禁止类型收窄丢数据
}

// B 戏剧内核层
export interface DramaticCore {
  coreContradiction: string;
  surfaceGoal: string;
  hiddenNeed: string;
  deniedDesire?: string;
  coreFear: string;
  stakes: string;
  bottomLines: string[];
  selfDeception?: string;
}

// C 决策模型层
export interface DecisionModel {
  valuePriorities: string[]; // 冲突时从高到低
  riskAppetite: string;
  defaultStrategies: string[]; // 谈判/试探/欺骗/对抗/退让/牺牲/拖延
  escalationPath: string[]; // 克制 → 失控的阶段
  sacrificeOrder: string[]; // 资源/名誉/关系/身体/信念
  knownBiases: string[];
  decisionRules: DecisionRule[];
}

// D 感知与认知层
export interface Perception {
  firstNotices: string[];
  blindSpots: string[];
  attributionStyle: string; // 判断他人动机的默认归因
  trustOrder: string[]; // 证据/权威/直觉/经验/情感
}

// E 情绪动力层
export interface EmotionDynamics {
  triggers: string[];
  maskingStyle: string;
  outburstPattern: string;
  recoveryConditions: string;
  pressureShift?: string; // 长期压力下的性格变形
}

// F 关系语法层
export interface RelationGrammar {
  trustBuilding: string;
  trustRepair: string;
  modesByRelation: Record<string, string>; // 盟友/爱人/权威/陌生人/敌人…
  attractedBy: string[];
  provokedBy: string[];
}

// G 表达与行为指纹层（只管「怎样表现」，不替代决策内核）
export interface ExpressionFingerprint {
  sentenceRhythm: string;
  metaphorSources: string[];
  questioningStyle?: string;
  lyingStyle?: string;
  humorStyle?: string;
  sayVsThinkGap: string; // 口头表达与内心真实的距离
  signatureGestures: string[];
  stateVariants?: Record<string, string>; // 平静/危险/羞耻/愤怒下的表达差异
  forbiddenPhrases: string[]; // 禁用的通用 AI 式表达
}

// H 行动力与剧情种子层（自主推动剧情的关键）
export interface Agency {
  initiativeTriggers: string[];
  defaultPlans: string[];
  longTermAgenda: string;
  leverage: string[]; // 影响他人与局势的筹码
  plotSeeds: string[]; // 天然携带的冲突/秘密/承诺/未完成事项
  refusalRules: string[]; // 会拒绝哪些剧情安排
}

// I 成长弧层（模板侧只存弧线定义，运行状态另存）
export interface GrowthArc {
  immutableCore: string[];
  mutableBeliefs: string[];
  breakPoints: string[];
  awakeningPoints: string[];
}

// J 跨世界适配层
export interface WorldAdaptation {
  identityMapping?: string;
  capabilityMapping?: string;
  mustPreserve: string[];
  localizable: string[];
  conflictFallback?: string; // 与目标世界规则冲突时的降级策略
}

// 证据全量外置，各层仅以 evidenceIds 引用
export interface EvidenceIndex {
  storeKey: string;
  contentHash: string;
  count: number;
}

export interface CharacterCardV2 {
  schemaVersion: 2;
  id: string;
  lifecycle: CardLifecycle;
  identity: Identity;
  dramaticCore: DramaticCore;
  decisionModel: DecisionModel;
  perception: Perception;
  emotionDynamics: EmotionDynamics;
  relationGrammar: RelationGrammar;
  expressionFingerprint: ExpressionFingerprint;
  agency: Agency;
  growthArc: GrowthArc;
  worldAdaptation: WorldAdaptation;
  evidenceIndex: EvidenceIndex;
  revision: number;
  createdAt: number;
  updatedAt: number;
}

// ---------- 工具 ----------

let cardIdCounter = 0;

function generateCardId(): string {
  cardIdCounter += 1;
  return `ccv2-${Date.now().toString(36)}-${cardIdCounter.toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

function deepClone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

/** 生成一张各层留空的 draft 卡（新建/迁移的基座）。 */
export function createEmptyCardV2(name: string): CharacterCardV2 {
  const now = Date.now();
  return {
    schemaVersion: 2,
    id: generateCardId(),
    lifecycle: 'draft',
    identity: { name: name ?? '', aliases: [], importance: 'functional' },
    dramaticCore: {
      coreContradiction: '',
      surfaceGoal: '',
      hiddenNeed: '',
      coreFear: '',
      stakes: '',
      bottomLines: [],
    },
    decisionModel: {
      valuePriorities: [],
      riskAppetite: '',
      defaultStrategies: [],
      escalationPath: [],
      sacrificeOrder: [],
      knownBiases: [],
      decisionRules: [],
    },
    perception: { firstNotices: [], blindSpots: [], attributionStyle: '', trustOrder: [] },
    emotionDynamics: {
      triggers: [],
      maskingStyle: '',
      outburstPattern: '',
      recoveryConditions: '',
    },
    relationGrammar: {
      trustBuilding: '',
      trustRepair: '',
      modesByRelation: {},
      attractedBy: [],
      provokedBy: [],
    },
    expressionFingerprint: {
      sentenceRhythm: '',
      metaphorSources: [],
      sayVsThinkGap: '',
      signatureGestures: [],
      forbiddenPhrases: [],
    },
    agency: {
      initiativeTriggers: [],
      defaultPlans: [],
      longTermAgenda: '',
      leverage: [],
      plotSeeds: [],
      refusalRules: [],
    },
    growthArc: { immutableCore: [], mutableBeliefs: [], breakPoints: [], awakeningPoints: [] },
    worldAdaptation: { mustPreserve: [], localizable: [] },
    evidenceIndex: { storeKey: '', contentHash: '', count: 0 },
    revision: 0,
    createdAt: now,
    updatedAt: now,
  };
}

const MIGRATED_REACTION_BECAUSE = '迁移自 V1 典型反应';

/** 把 V1 典型反应文本切成 decisionRules 种子：一行/一分句一条，首个冒号分 when/then。 */
function seedDecisionRulesFromReactions(text?: string): DecisionRule[] {
  const raw = (text ?? '').trim();
  if (!raw) return [];
  return raw
    .split(/[\n；;]+/)
    .map((segment) => segment.trim())
    .filter((segment) => segment.length > 0)
    .map((segment) => {
      const match = segment.match(/^(.*?)[：:](.*)$/);
      if (match && match[1].trim() && match[2].trim()) {
        return { when: match[1].trim(), then: match[2].trim(), because: MIGRATED_REACTION_BECAUSE };
      }
      return { when: '', then: segment, because: MIGRATED_REACTION_BECAUSE };
    });
}

/**
 * V1 PartnerItem → V2（§10.1）：
 * - fields 全量原样存入 identity.legacyV1Fields（含 customFields，无损）
 * - name → identity.name；speakingStyle → expressionFingerprint.sentenceRhythm 种子
 * - typicalReactions → decisionModel.decisionRules 种子（because 固定标注）
 * - 其余层留空待补；产物一律 lifecycle='draft'
 */
export function migrateV1ToV2(item: PartnerItem): CharacterCardV2 {
  const fields: PartnerItemFields = item.fields ?? {};
  const name = (item.name ?? '').trim() || (fields.name ?? '').trim() || '未命名角色';
  const card = createEmptyCardV2(name);

  card.identity.legacyV1Fields = deepClone(fields) as Record<string, unknown>;

  const speakingStyle = (fields.speakingStyle ?? '').trim();
  if (speakingStyle) {
    card.expressionFingerprint.sentenceRhythm = speakingStyle;
  }

  card.decisionModel.decisionRules = seedDecisionRulesFromReactions(fields.typicalReactions);
  card.lifecycle = 'draft';
  return card;
}

export interface CardValidation {
  /** 是否满足全部 ready 判据 */
  ready: boolean;
  /** 生命周期可达性：当前内容可推进到的最高阶段 */
  reachableLifecycle: CardLifecycle;
  /** 缺失/未处理项（行为字段路径 + 证据问题码） */
  missing: string[];
  /** 引用了但在证据集合中找不到的 evidence id */
  danglingEvidenceIds: string[];
}

/**
 * ready 校验（§9.1 末段）：
 * - 关键行为字段非空（dramaticCore 四项、valuePriorities、decisionRules、plotSeeds）
 * - 所有 evidenceIds 可解析（对照传入的证据集合；元素可为 EvidenceRef 或 id 字符串）
 * - 低置信/矛盾证据须已 userConfirmed
 * 生命周期可达性：行为字段齐全→至少 reviewed；再叠加证据无悬空/低置信已处理→ready。
 */
export function validateCard(
  card: CharacterCardV2,
  evidence: ReadonlyArray<EvidenceRef | string> = [],
): CardValidation {
  const missing: string[] = [];

  const requireString = (value: string, path: string) => {
    if (!value || !value.trim()) missing.push(path);
  };
  const requireNonEmpty = (arr: unknown[], path: string) => {
    if (!Array.isArray(arr) || arr.length === 0) missing.push(path);
  };

  requireString(card.dramaticCore.coreContradiction, 'dramaticCore.coreContradiction');
  requireString(card.dramaticCore.surfaceGoal, 'dramaticCore.surfaceGoal');
  requireString(card.dramaticCore.coreFear, 'dramaticCore.coreFear');
  requireString(card.dramaticCore.stakes, 'dramaticCore.stakes');
  requireNonEmpty(card.decisionModel.valuePriorities, 'decisionModel.valuePriorities');
  requireNonEmpty(card.decisionModel.decisionRules, 'decisionModel.decisionRules');
  requireNonEmpty(card.agency.plotSeeds, 'agency.plotSeeds');

  const behavioralComplete = missing.length === 0;

  // 卡内唯一携带 evidenceIds 的位置是 DecisionRule（§9.1：各层仅以 evidenceIds 引用）
  const referencedIds = new Set<string>();
  for (const rule of card.decisionModel.decisionRules) {
    for (const id of rule.evidenceIds ?? []) {
      if (id) referencedIds.add(id);
    }
  }

  const knownIds = new Set<string>();
  const refObjects: EvidenceRef[] = [];
  for (const entry of evidence) {
    if (typeof entry === 'string') {
      knownIds.add(entry);
    } else if (entry && typeof entry === 'object') {
      knownIds.add(entry.id);
      refObjects.push(entry);
    }
  }

  const danglingEvidenceIds = [...referencedIds].filter((id) => !knownIds.has(id));

  const lowConfidenceUnresolved = refObjects.some(
    (ref) =>
      referencedIds.has(ref.id) &&
      (ref.confidence === 'low' || (ref.conflictsWith?.length ?? 0) > 0) &&
      ref.userConfirmed !== true,
  );
  if (lowConfidenceUnresolved) missing.push('evidence:lowConfidenceUnresolved');

  const evidenceClean = danglingEvidenceIds.length === 0 && !lowConfidenceUnresolved;
  const reachableLifecycle: CardLifecycle = behavioralComplete
    ? evidenceClean
      ? 'ready'
      : 'reviewed'
    : 'draft';

  return {
    ready: reachableLifecycle === 'ready',
    reachableLifecycle,
    missing,
    danglingEvidenceIds,
  };
}

/** schemaVersion 分流判定：是否为 V2 卡。 */
export function isV2(item: unknown): item is CharacterCardV2 {
  return (
    !!item &&
    typeof item === 'object' &&
    (item as { schemaVersion?: unknown }).schemaVersion === 2
  );
}
