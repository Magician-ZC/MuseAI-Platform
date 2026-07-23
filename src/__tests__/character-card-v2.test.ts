import { describe, expect, it } from 'vitest';
import {
  createEmptyCardV2,
  migrateV1ToV2,
  validateCard,
  isV2,
  type CharacterCardV2,
  type EvidenceRef,
} from '../utils/characterCardV2';
import {
  isSameCardContent,
  cardContentHash,
  buildSwapTestRequest,
  buildStressTestRequest,
  type EvalConfig,
} from '../utils/characterEvaluation';
import type { PartnerItem } from '../stores/usePartnerStore';

const EVAL_CONFIG: EvalConfig = {
  profile: { interface: 'OpenAI-compatible', baseUrl: 'https://x/v1', apiKey: 'k', model: 'gpt-4o' },
  swapPrompt: 'SWAP-SYS',
  stressPrompt: 'STRESS-SYS',
  promptVersion: 'v1',
};

const makeEvidence = (id: string, overrides: Partial<EvidenceRef> = {}): EvidenceRef => ({
  id,
  sourceId: 'src-1',
  chapterIndex: 1,
  locator: { start: 0, end: 10 },
  quotePreview: '示例原文',
  kind: 'action',
  confidence: 'high',
  ...overrides,
});

// 行为字段齐全、引用 ev-1 的卡（用于 ready 相关校验）。
const makeReadyCard = (): CharacterCardV2 => {
  const card = createEmptyCardV2('测试角色');
  card.dramaticCore.coreContradiction = '想自由又渴望被认可';
  card.dramaticCore.surfaceGoal = '夺回家族权柄';
  card.dramaticCore.coreFear = '被当作异类清除';
  card.dramaticCore.stakes = '暴露即死';
  card.decisionModel.valuePriorities = ['生存', '真相', '同伴'];
  card.decisionModel.decisionRules = [
    { when: '被威胁时', then: '先示弱后反击', because: '避免正面硬碰', evidenceIds: ['ev-1'] },
  ];
  card.agency.plotSeeds = ['隐藏的穿越者身份'];
  return card;
};

describe('createEmptyCardV2', () => {
  it('生成 schemaVersion=2 的 draft 空卡', () => {
    const card = createEmptyCardV2('林逸');
    expect(card.schemaVersion).toBe(2);
    expect(card.lifecycle).toBe('draft');
    expect(card.identity.name).toBe('林逸');
    expect(card.identity.importance).toBe('functional');
    expect(card.dramaticCore.coreContradiction).toBe('');
    expect(card.decisionModel.decisionRules).toEqual([]);
    expect(card.agency.plotSeeds).toEqual([]);
    expect(card.evidenceIndex).toEqual({ storeKey: '', contentHash: '', count: 0 });
  });

  it('每次生成唯一 id', () => {
    expect(createEmptyCardV2('a').id).not.toBe(createEmptyCardV2('b').id);
  });
});

describe('migrateV1ToV2', () => {
  const fields = {
    name: '林逸',
    age: '18岁',
    gender: '男',
    speakingStyle: '用词严谨，语气不温不火',
    typicalReactions: '遭遇危机时：瞳孔微缩但绝不惊慌；被夸奖时：礼貌微笑自谦',
    identityTags: ['穿越者', '魔法天才'],
    customFields: [{ id: 'cf-1', moduleId: 'char_basic', label: '昵称', value: '小逸' }],
  };
  const item: PartnerItem = {
    id: 'cc-1',
    name: '林逸',
    type: 'character_card',
    content: '',
    fields,
    worldBookId: null,
  };

  it('legacyV1Fields 无损保留全部 V1 字段（含 customFields）', () => {
    const card = migrateV1ToV2(item);
    expect(card.identity.legacyV1Fields).toEqual(fields);
    // 深拷贝，不与源共享引用
    expect(card.identity.legacyV1Fields).not.toBe(fields);
    const legacyTags = (card.identity.legacyV1Fields as { identityTags?: string[] }).identityTags;
    expect(legacyTags).toEqual(['穿越者', '魔法天才']);
  });

  it('产物一律 lifecycle=draft，name 映射到 identity', () => {
    const card = migrateV1ToV2(item);
    expect(card.lifecycle).toBe('draft');
    expect(card.schemaVersion).toBe(2);
    expect(card.identity.name).toBe('林逸');
  });

  it('speakingStyle → expressionFingerprint.sentenceRhythm 种子', () => {
    const card = migrateV1ToV2(item);
    expect(card.expressionFingerprint.sentenceRhythm).toBe('用词严谨，语气不温不火');
  });

  it('typicalReactions → decisionModel.decisionRules 种子（when/then + 固定 because）', () => {
    const card = migrateV1ToV2(item);
    expect(card.decisionModel.decisionRules).toHaveLength(2);
    expect(card.decisionModel.decisionRules[0]).toEqual({
      when: '遭遇危机时',
      then: '瞳孔微缩但绝不惊慌',
      because: '迁移自 V1 典型反应',
    });
    expect(card.decisionModel.decisionRules[1].when).toBe('被夸奖时');
    expect(card.decisionModel.decisionRules.every((r) => r.because === '迁移自 V1 典型反应')).toBe(true);
  });

  it('其余层留空待补', () => {
    const card = migrateV1ToV2(item);
    expect(card.dramaticCore.coreContradiction).toBe('');
    expect(card.agency.plotSeeds).toEqual([]);
    expect(card.perception.trustOrder).toEqual([]);
  });

  it('容忍 fields 缺失', () => {
    const bare: PartnerItem = { id: 'cc-2', name: '无名', type: 'character_card', content: '' };
    const card = migrateV1ToV2(bare);
    expect(card.identity.legacyV1Fields).toEqual({});
    expect(card.decisionModel.decisionRules).toEqual([]);
    expect(card.expressionFingerprint.sentenceRhythm).toBe('');
    expect(card.identity.name).toBe('无名');
  });
});

describe('validateCard', () => {
  it('空卡报缺失、不可达 ready', () => {
    const result = validateCard(createEmptyCardV2('空'));
    expect(result.ready).toBe(false);
    expect(result.reachableLifecycle).toBe('draft');
    expect(result.missing).toContain('dramaticCore.coreContradiction');
    expect(result.missing).toContain('dramaticCore.surfaceGoal');
    expect(result.missing).toContain('dramaticCore.coreFear');
    expect(result.missing).toContain('dramaticCore.stakes');
    expect(result.missing).toContain('decisionModel.valuePriorities');
    expect(result.missing).toContain('decisionModel.decisionRules');
    expect(result.missing).toContain('agency.plotSeeds');
    expect(result.danglingEvidenceIds).toEqual([]);
  });

  it('悬空 evidenceIds 报错、降级为 reviewed', () => {
    const result = validateCard(makeReadyCard(), []);
    expect(result.danglingEvidenceIds).toEqual(['ev-1']);
    expect(result.reachableLifecycle).toBe('reviewed');
    expect(result.ready).toBe(false);
  });

  it('证据可解析（EvidenceRef）→ ready', () => {
    const result = validateCard(makeReadyCard(), [makeEvidence('ev-1')]);
    expect(result.danglingEvidenceIds).toEqual([]);
    expect(result.missing).toEqual([]);
    expect(result.ready).toBe(true);
    expect(result.reachableLifecycle).toBe('ready');
  });

  it('证据可解析（id 字符串集合）→ ready', () => {
    const result = validateCard(makeReadyCard(), ['ev-1']);
    expect(result.ready).toBe(true);
  });

  it('低置信证据未确认 → 不 ready；确认后 ready', () => {
    const card = makeReadyCard();
    const unresolved = validateCard(card, [makeEvidence('ev-1', { confidence: 'low' })]);
    expect(unresolved.ready).toBe(false);
    expect(unresolved.reachableLifecycle).toBe('reviewed');
    expect(unresolved.missing).toContain('evidence:lowConfidenceUnresolved');

    const resolved = validateCard(card, [
      makeEvidence('ev-1', { confidence: 'low', userConfirmed: true }),
    ]);
    expect(resolved.ready).toBe(true);
  });
});

describe('isV2', () => {
  it('V2 卡判定为真', () => {
    expect(isV2(createEmptyCardV2('x'))).toBe(true);
  });

  it('V1 卡与非法值判定为假', () => {
    const v1: PartnerItem = { id: 'cc-1', name: '旧卡', type: 'character_card', content: '' };
    expect(isV2(v1)).toBe(false);
    expect(isV2(null)).toBe(false);
    expect(isV2(undefined)).toBe(false);
    expect(isV2({ schemaVersion: 1 })).toBe(false);
  });
});

describe('isSameCardContent / cardContentHash', () => {
  it('同卡复制（仅 id/时间戳/revision 不同）判定内容一致（阳性对照）', () => {
    const a = makeReadyCard();
    const b: CharacterCardV2 = JSON.parse(JSON.stringify(a));
    b.id = 'ccv2-copy';
    b.createdAt = a.createdAt + 10_000;
    b.updatedAt = a.updatedAt + 20_000;
    b.revision = a.revision + 5;
    expect(isSameCardContent(a, b)).toBe(true);
    expect(cardContentHash(a)).toBe(cardContentHash(b));
  });

  it('行为内容不同判定不一致（阴性对照）', () => {
    const a = makeReadyCard();
    const b: CharacterCardV2 = JSON.parse(JSON.stringify(a));
    b.dramaticCore.surfaceGoal = '截然不同的目标';
    expect(isSameCardContent(a, b)).toBe(false);
    expect(cardContentHash(a)).not.toBe(cardContentHash(b));
  });

  it('内容比较对键顺序不敏感', () => {
    const a = makeReadyCard();
    const reordered: CharacterCardV2 = {
      ...JSON.parse(JSON.stringify(a)),
      // 打乱构造顺序不应影响内容判定
    };
    expect(isSameCardContent(a, reordered)).toBe(true);
  });
});

describe('buildSwapTestRequest / buildStressTestRequest（完整 DTO：补 profile + 两段 prompt）', () => {
  it('互换请求含 profile/swapPrompt/stressPrompt + cardA/cardB/scenario', () => {
    const a = makeReadyCard();
    const b = makeReadyCard();
    const req = buildSwapTestRequest(EVAL_CONFIG, a, b, '盟友背叛');
    expect(req.profile).toEqual(EVAL_CONFIG.profile);
    expect(req.swapPrompt).toBe('SWAP-SYS');
    expect(req.stressPrompt).toBe('STRESS-SYS');
    expect(req.promptVersion).toBe('v1');
    expect(req.cardA).toBe(a);
    expect(req.cardB).toBe(b);
    expect(req.scenario).toBe('盟友背叛');
    // 互换不带 scenarios
    expect(req.scenarios).toBeUndefined();
  });

  it('压力请求含 profile/两段 prompt + cardA/scenarios（拷贝而非引用）', () => {
    const a = makeReadyCard();
    const scenarios = ['情境一', '情境二'];
    const req = buildStressTestRequest(EVAL_CONFIG, a, scenarios);
    expect(req.profile).toEqual(EVAL_CONFIG.profile);
    expect(req.swapPrompt).toBe('SWAP-SYS');
    expect(req.stressPrompt).toBe('STRESS-SYS');
    expect(req.cardA).toBe(a);
    expect(req.scenarios).toEqual(['情境一', '情境二']);
    expect(req.scenarios).not.toBe(scenarios);
    // 压力不带 cardB/scenario
    expect(req.cardB).toBeUndefined();
    expect(req.scenario).toBeUndefined();
  });
});
