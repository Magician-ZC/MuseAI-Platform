import { describe, expect, it } from 'vitest';
import {
  buildRelationGraph,
  buildCooccurrenceGraph,
  buildPowerHierarchy,
  ARC_STAGE_COLOR,
  FACTION_PALETTE,
  ALLIANCE_COLOR,
  CONFLICT_COLOR,
  MINE_RING_COLOR,
  OTHER_NODE_COLOR,
  RELATION_POSITIVE_COLOR,
  RELATION_NEGATIVE_COLOR,
  RELATION_NEUTRAL_COLOR,
} from '../components/graph/model';
import type {
  WorldRosterEntry,
  WorldRelation,
  WorldCharacterState,
  WorldEventItem,
} from '../stores/usePlatformStore';

const ROSTER: WorldRosterEntry[] = [
  { cloudCharacterId: 'a', name: '甲' },
  { cloudCharacterId: 'b', name: '乙' },
  { cloudCharacterId: 'c', name: '丙' },
];

const CHARACTERS: WorldCharacterState[] = [
  { id: 'a', arcStage: 'rising', activity: 5 },
  { id: 'b', arcStage: 'climax', activity: 2 },
  { id: 'c', arcStage: 'setup', activity: 0 },
];

const RELATIONS: WorldRelation[] = [
  { from: 'a', to: 'b', trust: 60, affinity: 40, fear: 0, debt: -10 },
  { from: 'b', to: 'c', trust: -20, affinity: -30, fear: 15, debt: 0 },
];

function ev(type: string, actors: string[], seq: number): WorldEventItem {
  return {
    id: `e${seq}`,
    worldId: 'w',
    tick: seq,
    sequence: seq,
    domainEventId: `d${seq}`,
    type,
    actors,
    visibility: 'public',
    occurredAt: seq,
  };
}

describe('buildRelationGraph', () => {
  it('节点：size∝activity、色=arcStage 五色、mine 打标', () => {
    const g = buildRelationGraph({
      roster: ROSTER,
      relations: RELATIONS,
      characters: CHARACTERS,
      myIds: ['a'],
      dimension: 'affinity',
    });
    const byId = new Map(g.nodes.map((n) => [n.id, n]));

    const a = byId.get('a')!;
    expect(a.color).toBe(ARC_STAGE_COLOR.rising);
    expect(a.size).toBe(16 + 5 * 4); // 36
    expect(a.mine).toBe(true);
    expect(a.arcStage).toBe('rising');
    expect(a.activity).toBe(5);

    const b = byId.get('b')!;
    expect(b.color).toBe(ARC_STAGE_COLOR.climax);
    expect(b.mine).toBe(false);

    // activity 0 → 保底最小尺寸 16
    expect(byId.get('c')!.size).toBe(16);
  });

  it('边：所选维度绿(正)红(负)配色 + weight=|值|', () => {
    const g = buildRelationGraph({
      roster: ROSTER,
      relations: RELATIONS,
      characters: CHARACTERS,
      myIds: ['a'],
      dimension: 'affinity',
    });
    const ab = g.links.find((l) => l.source === 'a' && l.target === 'b')!;
    expect(ab.weight).toBe(40);
    expect(ab.color).toBe(RELATION_POSITIVE_COLOR);
    expect(ab.dim).toBe('affinity');
    expect(ab.kind).toBe('relation');

    const bc = g.links.find((l) => l.source === 'b' && l.target === 'c')!;
    expect(bc.weight).toBe(30);
    expect(bc.color).toBe(RELATION_NEGATIVE_COLOR); // affinity -30
  });

  it('维度切换改变配色/权重：debt 负→红、fear 零→中性灰', () => {
    const debt = buildRelationGraph({
      roster: ROSTER,
      relations: RELATIONS,
      characters: CHARACTERS,
      myIds: ['a'],
      dimension: 'debt',
    });
    const abDebt = debt.links.find((l) => l.source === 'a' && l.target === 'b')!;
    expect(abDebt.weight).toBe(10); // |−10|
    expect(abDebt.color).toBe(RELATION_NEGATIVE_COLOR);

    const fear = buildRelationGraph({
      roster: ROSTER,
      relations: RELATIONS,
      characters: CHARACTERS,
      myIds: ['a'],
      dimension: 'fear',
    });
    const abFear = fear.links.find((l) => l.source === 'a' && l.target === 'b')!;
    expect(abFear.weight).toBe(0); // fear 0
    expect(abFear.color).toBe(RELATION_NEUTRAL_COLOR);
  });

  it('观众空 relations（principal 空集）：无边，节点为 roster public 子集', () => {
    const g = buildRelationGraph({
      roster: ROSTER,
      relations: [],
      characters: CHARACTERS,
      myIds: [],
      dimension: 'trust',
    });
    expect(g.links).toHaveLength(0);
    expect(g.nodes.map((n) => n.id).sort()).toEqual(['a', 'b', 'c']);
    // 无 principal → 无我方描边
    expect(g.nodes.every((n) => !n.mine)).toBe(true);
    expect(g.categories).toEqual([]);
  });
});

describe('buildCooccurrenceGraph（回退路径）', () => {
  it('size∝参与次数、mine/其他两色、共现两两连边', () => {
    const g = buildCooccurrenceGraph({
      roster: ROSTER.slice(0, 2),
      events: [ev('dialogue', ['a', 'b'], 1), ev('action', ['a'], 2)],
      myIds: ['a'],
    });
    const byId = new Map(g.nodes.map((n) => [n.id, n]));
    expect(byId.get('a')!.color).toBe(MINE_RING_COLOR);
    expect(byId.get('b')!.color).toBe(OTHER_NODE_COLOR);
    // a 参与 2 次 → weight 3 → size 18+3*3=27；b 参与 1 次 → 24
    expect(byId.get('a')!.size).toBe(27);
    expect(byId.get('b')!.size).toBe(24);
    const link = g.links.find((l) => l.source === 'a' && l.target === 'b')!;
    expect(link.kind).toBe('coocc');
    expect(link.weight).toBe(1);
  });
});

describe('buildPowerHierarchy', () => {
  it('并查集聚类：结盟同势力、冲突跨势力敌对边', () => {
    const g = buildPowerHierarchy({
      roster: ROSTER,
      events: [ev('alliance', ['a', 'b'], 1), ev('conflict', ['a', 'c'], 2)],
      myIds: ['a'],
    });
    const byId = new Map(g.nodes.map((n) => [n.id, n]));
    // a、b 结盟 → 同 category；c 独立 → 不同 category
    expect(byId.get('a')!.category).toBe(byId.get('b')!.category);
    expect(byId.get('a')!.category).not.toBe(byId.get('c')!.category);
    expect(g.categories.length).toBe(2);

    // 节点色取势力配色，mine 打标
    expect(byId.get('a')!.color).toBe(FACTION_PALETTE[byId.get('a')!.category! % FACTION_PALETTE.length]);
    expect(byId.get('a')!.mine).toBe(true);

    const alliance = g.links.find((l) => l.kind === 'alliance')!;
    expect(alliance.color).toBe(ALLIANCE_COLOR);
    expect(alliance.dashed).toBeFalsy();

    const conflict = g.links.find((l) => l.kind === 'conflict')!;
    expect(conflict.color).toBe(CONFLICT_COLOR);
    expect(conflict.dashed).toBe(true);
  });

  it('权威关系并入聚类：正亲和同势力、负亲和/高恐惧敌对', () => {
    const g = buildPowerHierarchy({
      roster: ROSTER,
      events: [],
      relations: RELATIONS, // a-b affinity 40 → 同势力；b-c affinity -30 → 敌对
      myIds: [],
    });
    const byId = new Map(g.nodes.map((n) => [n.id, n]));
    expect(byId.get('a')!.category).toBe(byId.get('b')!.category);
    expect(g.links.some((l) => l.kind === 'alliance')).toBe(true);
    expect(g.links.some((l) => l.kind === 'conflict')).toBe(true);
  });
});
