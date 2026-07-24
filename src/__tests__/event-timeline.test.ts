import { describe, expect, it } from 'vitest';
import {
  revealEventsUpTo,
  toTimelineEvents,
  eventTimelineColor,
  type TimelineEvent,
} from '../components/graph/EventTimeline';
import type { WorldEventItem } from '../stores/usePlatformStore';

const EVENTS: TimelineEvent[] = [
  { id: 'e1', sequence: 1, tick: 1, type: 'action', actors: ['a'] },
  { id: 'e3', sequence: 3, tick: 3, type: 'conflict', actors: ['a', 'b'] },
  { id: 'e2', sequence: 2, tick: 2, type: 'alliance', actors: ['a', 'b'] },
  { id: 'e4', sequence: 4, tick: 5, type: 'arena_elim', actors: ['b'] },
];

describe('revealEventsUpTo — 游标只保留 ≤ 上界的事件', () => {
  it('游标=2：仅保留 tick ≤ 2 的事件，且按 sequence 升序', () => {
    const revealed = revealEventsUpTo(EVENTS, 2);
    expect(revealed.map((e) => e.id)).toEqual(['e1', 'e2']);
  });

  it('游标=3：纳入 tick=3 的事件（边界含等号）', () => {
    const revealed = revealEventsUpTo(EVENTS, 3);
    expect(revealed.map((e) => e.id)).toEqual(['e1', 'e2', 'e3']);
  });

  it('游标=0：空（尚未点亮任何事件）', () => {
    expect(revealEventsUpTo(EVENTS, 0)).toEqual([]);
  });

  it('游标=最大 tick：点亮全部（含 tick=5 的淘汰）', () => {
    const revealed = revealEventsUpTo(EVENTS, 5);
    expect(revealed.map((e) => e.id)).toEqual(['e1', 'e2', 'e3', 'e4']);
  });

  it('不改动入参数组（返回副本排序）', () => {
    const input = EVENTS.slice();
    revealEventsUpTo(input, 5);
    expect(input.map((e) => e.id)).toEqual(['e1', 'e3', 'e2', 'e4']);
  });
});

describe('toTimelineEvents — WorldEventItem → TimelineEvent', () => {
  it('summary 取 projection.summary，回退 narrative；保留 tick/actors/visibility', () => {
    const world: WorldEventItem[] = [
      { id: 'w1', worldId: 'W', tick: 2, sequence: 1, domainEventId: 'd1', type: 'dialogue', actors: ['a', 'b'], visibility: 'public', projection: { summary: '摘要甲' }, occurredAt: 1 },
      { id: 'w2', worldId: 'W', tick: 3, sequence: 2, domainEventId: 'd2', type: 'status', actors: ['a'], visibility: 'private', projection: { narrative: '仅叙事' }, occurredAt: 2 },
    ];
    const mapped = toTimelineEvents(world);
    expect(mapped[0]).toMatchObject({ id: 'w1', tick: 2, type: 'dialogue', summary: '摘要甲', visibility: 'public' });
    expect(mapped[1].summary).toBe('仅叙事');
    expect(mapped[1].visibility).toBe('private');
  });
});

describe('eventTimelineColor — 类型配色', () => {
  it('对抗=红、结盟=绿、淘汰=深红；未知回退中性棕', () => {
    expect(eventTimelineColor('conflict')).toBe('#c15b5b');
    expect(eventTimelineColor('alliance')).toBe('#5b9a6f');
    expect(eventTimelineColor('arena_elim')).toBe('#a4322f');
    expect(eventTimelineColor('unknown-type')).toBe('#8b7355');
  });
});
