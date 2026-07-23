import { beforeEach, describe, expect, it } from 'vitest';
import { usePartnerStore, type PartnerItem } from '../stores/usePartnerStore';
import { createEmptyCardV2, isV2 } from '../utils/characterCardV2';

const v1Card = (): PartnerItem => ({
  id: 'cc-v1',
  name: '沈霜',
  type: 'character_card',
  worldBookId: 'wb-1',
  content: '# 角色卡：沈霜',
  fields: {
    name: '沈霜',
    age: '27',
    speakingStyle: '言简意赅，很少寒暄',
    typicalReactions: '被威胁时：先示弱后反击；被夸奖时：礼貌自谦',
    customFields: [{ id: 'cf-1', moduleId: 'char_basic', label: '门派', value: '寒山' }],
  },
});

beforeEach(() => {
  usePartnerStore.setState({
    worldBooks: [],
    characterCards: [v1Card()],
    characterCardsV2: [],
    selectedId: 'cc-v1',
    selectedType: 'character_card',
  });
});

describe('usePartnerStore V2 分流 — 升级', () => {
  it('upgradeToV2 生成 V2 新版本且不改动源 V1 卡', () => {
    const before = JSON.parse(JSON.stringify(usePartnerStore.getState().characterCards));

    const newId = usePartnerStore.getState().upgradeToV2('cc-v1');

    expect(newId).toBeTruthy();
    // 源卡原样保留（数量、内容、id 均不变）
    expect(usePartnerStore.getState().characterCards).toEqual(before);
    expect(usePartnerStore.getState().characterCards[0].id).toBe('cc-v1');

    const cards = usePartnerStore.getState().characterCardsV2;
    expect(cards).toHaveLength(1);
    const card = cards[0];
    expect(isV2(card)).toBe(true);
    expect(card.schemaVersion).toBe(2);
    expect(card.lifecycle).toBe('draft');
    // 生成新 id，不复用源卡 id
    expect(card.id).not.toBe('cc-v1');
    expect(newId).toBe(card.id);
    // V1 字段无损保留，迁移种子落到对应层
    expect(card.identity.name).toBe('沈霜');
    expect(card.identity.legacyV1Fields).toEqual(v1Card().fields);
    expect(card.expressionFingerprint.sentenceRhythm).toBe('言简意赅，很少寒暄');
    expect(card.decisionModel.decisionRules.length).toBeGreaterThan(0);
  });

  it('V1 与 V2 共存：升级后两个集合都在', () => {
    usePartnerStore.getState().upgradeToV2('cc-v1');
    expect(usePartnerStore.getState().characterCards).toHaveLength(1); // V1 仍在
    expect(usePartnerStore.getState().characterCardsV2).toHaveLength(1); // V2 新增
  });

  it('两次升级生成两张独立的 V2 卡（新 id 共存，不覆盖）', () => {
    const id1 = usePartnerStore.getState().upgradeToV2('cc-v1');
    const id2 = usePartnerStore.getState().upgradeToV2('cc-v1');
    expect(id1).not.toBe(id2);
    expect(usePartnerStore.getState().characterCardsV2).toHaveLength(2);
  });

  it('对不存在的源卡返回 null 且不新增 V2 卡', () => {
    const result = usePartnerStore.getState().upgradeToV2('missing');
    expect(result).toBeNull();
    expect(usePartnerStore.getState().characterCardsV2).toHaveLength(0);
  });
});

describe('usePartnerStore V2 分流 — 增改', () => {
  it('addV2Card 新增，且同 id 幂等覆盖（不产生重复）', () => {
    const card = createEmptyCardV2('新角色');
    usePartnerStore.getState().addV2Card(card);
    expect(usePartnerStore.getState().characterCardsV2).toHaveLength(1);

    // 相同 id 再次写入 → 覆盖而非追加
    usePartnerStore.getState().addV2Card({ ...card, lifecycle: 'reviewed' });
    expect(usePartnerStore.getState().characterCardsV2).toHaveLength(1);
    expect(usePartnerStore.getState().characterCardsV2[0].lifecycle).toBe('reviewed');
  });

  it('updateV2Card 浅合并并刷新 updatedAt', () => {
    const card = createEmptyCardV2('待改');
    usePartnerStore.setState({ characterCardsV2: [{ ...card, updatedAt: 0 }] });

    usePartnerStore.getState().updateV2Card(card.id, { lifecycle: 'ready' });
    const updated = usePartnerStore.getState().characterCardsV2[0];
    expect(updated.lifecycle).toBe('ready');
    expect(updated.updatedAt).toBeGreaterThan(0);
  });
});

describe('usePartnerStore V1 行为不回归', () => {
  it('新增 V1 角色卡仍走原逻辑，characterCardsV2 不受影响', () => {
    usePartnerStore.getState().addCharacterCard();
    expect(usePartnerStore.getState().characterCards.length).toBe(2);
    expect(usePartnerStore.getState().characterCardsV2).toHaveLength(0);
    expect(usePartnerStore.getState().selectedType).toBe('character_card');
  });
});
