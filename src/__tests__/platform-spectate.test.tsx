import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { render, screen, act, waitFor } from '@testing-library/react';
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

// ForceGraph / EventTimeline 用 raw echarts + ref；jsdom 无 canvas，替身化 init/dispose 并记录 setOption。
const echartsSetOptionCalls: unknown[] = [];
vi.mock('echarts', () => {
  const chart = {
    setOption: vi.fn((opt: unknown) => {
      echartsSetOptionCalls.push(opt);
    }),
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
import WorldSpectate from '../pages/platform/WorldSpectate';
import { usePlatformStore } from '../stores/usePlatformStore';
import { RELATION_POSITIVE_COLOR, RELATION_NEGATIVE_COLOR } from '../components/graph/model';

const fetchMock = cloudFetch as unknown as Mock;
const streamMock = cloudStream as unknown as Mock;

const WORLD_OBJ = {
  id: 'w1',
  title: '云州世界',
  roomType: 'idle',
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
  echartsSetOptionCalls.length = 0;
  usePlatformStore.setState({ roomView: 'stream' });
  usePlatformStore.getState().reset();
});

describe('WorldSpectate — 只读观战席', () => {
  it('渲染只读事件流，且无干预/同意面板', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/worlds/w1') {
        return {
          id: 'w1',
          title: '云州世界',
          roomType: 'idle',
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
          roster: [{ cloudCharacterId: 'cA', name: '沈霜' }],
          compliance: { aiGenerated: true, arbitrationPublic: true },
        };
      }
      if (path === '/api/worlds/w1/events') {
        return {
          events: [
            { id: 'ev1', worldId: 'w1', tick: 1, sequence: 1, domainEventId: 'd1', type: 'action', actors: ['cA'], visibility: 'public', projection: { summary: '沈霜独自远行' }, occurredAt: 1 },
          ],
          nextCursor: 1,
        };
      }
      throw new Error(`unexpected ${path}`);
    });

    render(
      <MemoryRouter initialEntries={['/platform/worlds/w1/spectate']}>
        <Routes>
          <Route path="/platform/worlds/:id/spectate" element={<WorldSpectate />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(await screen.findByText('云州世界')).toBeInTheDocument();
    expect(screen.getByText('观战席')).toBeInTheDocument();
    expect(await screen.findByText('沈霜独自远行')).toBeInTheDocument();
    // 观战席无干预与同意面板
    expect(screen.queryByRole('button', { name: /提交托梦/ })).toBeNull();
    expect(screen.queryByText('待处理的同意请求')).toBeNull();
    // 未请求「我的角色」（无需鉴别归属）
    expect(fetchMock).not.toHaveBeenCalledWith('/api/assets/characters/mine');
  });

  it('观众视角空 relations（principal 过滤后）：关系图谱不泄私密关系边（无绿/红关系边）', async () => {
    usePlatformStore.setState({ roomView: 'graph' });
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/worlds/w1') return WORLD_OBJ;
      if (path === '/api/worlds/w1/events') {
        return {
          events: [
            { id: 'ev1', worldId: 'w1', tick: 1, sequence: 1, domainEventId: 'd1', type: 'dialogue', actors: ['cA', 'cB'], visibility: 'public', projection: { summary: '公开对谈' }, occurredAt: 1 },
          ],
          nextCursor: 1,
        };
      }
      // 观众投影：服务端把私密关系过滤为空集（只保留 public 子集，此处为空）。
      if (path === '/api/worlds/w1/state-summary') return { relations: [], characters: [{ id: 'cA', arcStage: 'rising', activity: 2 }] };
      throw new Error(`unexpected ${path}`);
    });

    render(
      <MemoryRouter initialEntries={['/platform/worlds/w1/spectate']}>
        <Routes>
          <Route path="/platform/worlds/:id/spectate" element={<WorldSpectate />} />
        </Routes>
      </MemoryRouter>,
    );

    // 关系图谱渲染（回退到公开事件共现，因权威 relations 为空）。
    expect(await screen.findByTestId('echarts-graph')).toBeInTheDocument();
    await waitFor(() => expect(echartsSetOptionCalls.length).toBeGreaterThan(0));

    // 断言：所有已下发到 echarts 的关系图连线，均无「关系维度」正/负配色（绿/红）——即未泄露私密关系边。
    const relationColored = echartsSetOptionCalls.flatMap((opt) => {
      const series = (opt as { series?: Array<{ type?: string; links?: Array<{ lineStyle?: { color?: string } }> }> }).series ?? [];
      const graph = series.find((s) => s.type === 'graph');
      return (graph?.links ?? []).map((l) => l.lineStyle?.color);
    });
    expect(relationColored).not.toContain(RELATION_POSITIVE_COLOR);
    expect(relationColored).not.toContain(RELATION_NEGATIVE_COLOR);
  });

  it('实时演化：收到 status 类事件后去抖（~2s）重拉 state-summary', async () => {
    const callbacks: Array<(e: unknown) => void> = [];
    streamMock.mockImplementation((_id: string, cb: (e: unknown) => void) => {
      callbacks.push(cb);
      return () => {};
    });
    let summaryCalls = 0;
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/worlds/w1') return WORLD_OBJ;
      if (path === '/api/worlds/w1/events') return { events: [], nextCursor: null };
      if (path === '/api/worlds/w1/state-summary') {
        summaryCalls += 1;
        return { relations: [], characters: [] };
      }
      throw new Error(`unexpected ${path}`);
    });

    render(
      <MemoryRouter initialEntries={['/platform/worlds/w1/spectate']}>
        <Routes>
          <Route path="/platform/worlds/:id/spectate" element={<WorldSpectate />} />
        </Routes>
      </MemoryRouter>,
    );

    await screen.findByText('云州世界');
    // 首次快照拉取。
    await waitFor(() => expect(summaryCalls).toBe(1));

    // 注入一条 status 事件（模拟 WS 实时下发）→ 去抖后应重拉一次快照。
    act(() => {
      callbacks.forEach((cb) =>
        cb({ id: 'ev9', worldId: 'w1', tick: 5, sequence: 9, domainEventId: 'd9', type: 'status', actors: ['cA'], visibility: 'public', occurredAt: 5 }),
      );
    });

    await waitFor(() => expect(summaryCalls).toBe(2), { timeout: 3000 });
  });
});
