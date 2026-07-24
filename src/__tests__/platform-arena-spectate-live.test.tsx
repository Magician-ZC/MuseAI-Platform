// 赛事观战席实时化：注入 arena_elim 流事件 → 实时动态合并显示淘汰 + 角色名（对齐 spec 测试项）。
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { render, screen, act } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router-dom';

vi.mock('../utils/cloudApi', () => {
  class CloudError extends Error {
    constructor(public code: string, message: string, public status: number) {
      super(message);
    }
  }
  return {
    cloudFetch: vi.fn(),
    cloudStream: vi.fn(() => () => {}),
    getPlatformBase: vi.fn(() => 'http://test'),
    setPlatformBase: vi.fn(),
    CloudError,
  };
});

vi.mock('echarts-for-react', () => ({
  default: () => <div data-testid="echarts-graph" />,
}));

// ForceGraph / EventTimeline 用 raw echarts + ref；jsdom 无 canvas，替身化 init/dispose。
vi.mock('echarts', () => {
  const chart = {
    setOption: vi.fn(),
    resize: vi.fn(),
    dispose: vi.fn(),
    on: vi.fn(),
    off: vi.fn(),
    dispatchAction: vi.fn(),
  };
  return {
    init: vi.fn(() => chart),
    getInstanceByDom: vi.fn(() => undefined),
  };
});

import { cloudFetch, cloudStream } from '../utils/cloudApi';
import ArenaSpectate from '../pages/platform/ArenaSpectate';

const fetchMock = cloudFetch as unknown as Mock;
const streamMock = cloudStream as unknown as Mock;

const REPORT = {
  worldId: 'w1',
  match: { phase: 'running', alliances: [], eliminations: [], winnerCharId: null },
  rounds: [],
  environment: [],
  compliance: { arbitrationPublic: true, aiGenerated: true },
};
const WORLD = {
  id: 'w1',
  title: '龙争虎斗',
  roomType: 'arena',
  status: 'running',
  visibility: 'official',
  memberLimit: 10,
  memberCount: 2,
  tickPerDay: 3,
  templateId: 't',
  templateVersion: 1,
  engineVersion: 'e1',
  promptSetVersion: 'p1',
  modelRouteVersion: 'm1',
  roster: [
    { cloudCharacterId: 'cA', name: '沈霜' },
    { cloudCharacterId: 'cB', name: '云起' },
  ],
  compliance: { aiGenerated: true, arbitrationPublic: true },
};

beforeEach(() => {
  fetchMock.mockReset();
  streamMock.mockReset();
  streamMock.mockReturnValue(() => {});
  fetchMock.mockImplementation(async (path: string) => {
    if (path === '/api/arena/w1/report') return REPORT;
    if (path === '/api/worlds/w1') return WORLD;
    throw new Error(`unexpected ${path}`);
  });
});

describe('ArenaSpectate — 实时观战合并赛制事件', () => {
  it('注入 arena_elim 流事件 → 实时动态合并显示「淘汰」+ 角色名', async () => {
    let onEvent: ((e: unknown) => void) | null = null;
    streamMock.mockImplementation((_id: string, cb: (e: unknown) => void) => {
      onEvent = cb;
      return () => {};
    });

    render(
      <MemoryRouter initialEntries={['/platform/arena/w1/spectate']}>
        <Routes>
          <Route path="/platform/arena/:worldId/spectate" element={<ArenaSpectate />} />
        </Routes>
      </MemoryRouter>,
    );

    // 战报加载后头部标题可见。
    expect(await screen.findByText('龙争虎斗')).toBeInTheDocument();
    // 注入前实时动态为空：无「淘汰」标签（"阵容 / 淘汰" 卡片标题非精确匹配 '淘汰'）。
    expect(screen.queryByText('淘汰')).toBeNull();
    expect(streamMock).toHaveBeenCalled();

    // 注入一条淘汰系统事件（模拟 WS 实时下发）。
    await act(async () => {
      onEvent?.({
        id: 'we_1',
        sequence: 7,
        tick: 2,
        occurredAt: 100,
        type: 'arena_elim',
        actors: ['cA'],
        summary: '角色 cA 已淘汰（当事人同意，不可逆）',
        ruleRefs: [],
        arenaKind: 'elim',
        characterId: 'cA',
      });
    });

    // 实时动态合并：出现「淘汰」标签 + 角色名（沈霜，经 roster 解析）。
    expect(await screen.findByText('淘汰')).toBeInTheDocument();
    expect(screen.getByText('沈霜')).toBeInTheDocument();
  });
});
