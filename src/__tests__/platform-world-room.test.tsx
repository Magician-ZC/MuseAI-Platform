import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
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

// echarts 力导向图在 jsdom 无 canvas，替身化以隔离渲染。
vi.mock('echarts-for-react', () => ({
  default: () => <div data-testid="echarts-graph" />,
}));

import { cloudFetch, cloudStream } from '../utils/cloudApi';
import WorldRoom from '../pages/platform/WorldRoom';
import { usePlatformStore } from '../stores/usePlatformStore';
import { usePartnerStore } from '../stores/usePartnerStore';
import { createEmptyCardV2 } from '../utils/characterCardV2';

const fetchMock = cloudFetch as unknown as Mock;
const streamMock = cloudStream as unknown as Mock;

const WORLD = {
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
    { cloudCharacterId: 'cMine', name: '沈霜' },
    { cloudCharacterId: 'cOther', name: '游侠' },
  ],
  compliance: { aiGenerated: true, arbitrationPublic: true },
};

const EVENTS = {
  events: [
    {
      id: 'ev1',
      worldId: 'w1',
      tick: 1,
      sequence: 1,
      domainEventId: 'd1',
      type: 'dialogue',
      actors: ['cMine', 'cOther'],
      visibility: 'public',
      projection: { summary: '沈霜与游侠交谈' },
      occurredAt: 1,
    },
  ],
  nextCursor: 1,
};

function happyPath(path: string, opts?: { method?: string }) {
  if (path === '/api/worlds/w1') return Promise.resolve(WORLD);
  if (path === '/api/worlds/w1/events') return Promise.resolve(EVENTS);
  if (path === '/api/assets/characters/mine') {
    return Promise.resolve([
      { id: 'cMine', localCardId: 'lc', version: 1, rightsDeclaration: 'original', moderation: 'approved', withdrawn: false, createdAt: 1 },
    ]);
  }
  if (path === '/api/worlds/w1/interventions/mine') return Promise.resolve({ interventions: [] });
  if (path === '/api/me/consents?status=pending') return Promise.resolve({ consents: [] });
  if (path === '/api/worlds/w1/interventions' && opts?.method === 'POST') {
    return Promise.resolve({ status: 'accepted' });
  }
  return Promise.reject(new Error(`unexpected ${path}`));
}

beforeEach(() => {
  fetchMock.mockReset();
  streamMock.mockReset();
  streamMock.mockReturnValue(() => {});
  usePlatformStore.setState({ roomView: 'stream' });
  usePlatformStore.getState().reset();
});

const renderRoom = () =>
  render(
    <MemoryRouter initialEntries={['/platform/worlds/w1']}>
      <Routes>
        <Route path="/platform/worlds/:id" element={<WorldRoom />} />
      </Routes>
    </MemoryRouter>,
  );

describe('WorldRoom', () => {
  it('渲染世界头 + L0 事件流；订阅实时流', async () => {
    fetchMock.mockImplementation(happyPath);
    renderRoom();

    expect(await screen.findByText('云州世界')).toBeInTheDocument();
    expect(await screen.findByText('沈霜与游侠交谈')).toBeInTheDocument();
    expect(screen.getByText('仲裁公开')).toBeInTheDocument();
    // 订阅了世界事件流
    expect(streamMock).toHaveBeenCalledWith('w1', expect.any(Function), expect.any(Function));
  });

  it('托梦：提交后展示已接受', async () => {
    fetchMock.mockImplementation(happyPath);
    renderRoom();

    const textarea = await screen.findByLabelText('托梦内容');
    fireEvent.change(textarea, { target: { value: '记得完成今天的画作' } });
    fireEvent.click(screen.getByRole('button', { name: /提交托梦/ }));

    expect(await screen.findByText(/已提交/)).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(
        ([p, o]) => p === '/api/worlds/w1/interventions' && (o as { method?: string })?.method === 'POST',
      ),
    ).toBe(true);
  });

  it('切换到关系图谱：渲染 echarts 力导向图（state-summary 未就绪时回退启发式）', async () => {
    // happyPath 对 state-summary 抛未知路径 → 优雅降级，图谱仍以事件共现渲染。
    fetchMock.mockImplementation(happyPath);
    renderRoom();
    await screen.findByText('云州世界');

    fireEvent.click(screen.getByText('关系图谱'));
    expect(await screen.findByTestId('echarts-graph')).toBeInTheDocument();
    expect(await screen.findByText(/由观测事件共现推导/)).toBeInTheDocument();
  });

  it('切换到势力地图：渲染 echarts + 阵营聚合 seam 说明', async () => {
    fetchMock.mockImplementation(happyPath);
    renderRoom();
    await screen.findByText('云州世界');

    fireEvent.click(screen.getByText('势力地图'));
    expect(await screen.findByTestId('echarts-graph')).toBeInTheDocument();
    expect(screen.getByText('按阵营聚合呈现')).toBeInTheDocument();
  });

  it('关系图谱/状态面板：消费 state-summary 权威 relations/characters（#6b）', async () => {
    const withSummary = async (path: string, opts?: { method?: string }) => {
      if (path === '/api/worlds/w1/state-summary') {
        return {
          relations: [{ from: 'cMine', to: 'cOther', trust: 60, affinity: 40, fear: 0, debt: 0 }],
          characters: [
            { id: 'cMine', arcStage: 'rising', activity: 5 },
            { id: 'cOther', arcStage: 'setup', activity: 2 },
          ],
        };
      }
      return happyPath(path, opts);
    };
    fetchMock.mockImplementation(withSummary);
    renderRoom();
    await screen.findByText('云州世界');

    // 关系图谱标注为权威数据源
    fireEvent.click(screen.getByText('关系图谱'));
    expect(await screen.findByText(/权威关系状态/)).toBeInTheDocument();

    // 状态面板展示权威弧光阶段（rising → 上升）
    fireEvent.click(screen.getByText('状态面板'));
    expect(await screen.findByText('弧光 · 上升')).toBeInTheDocument();
  });

  it('世界加载失败：优雅降级为「连接平台失败」', async () => {
    fetchMock.mockImplementation(async () => {
      throw new TypeError('offline');
    });
    renderRoom();
    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
  });

  it('投放角色：选角色 + 确认边界协议 → join', async () => {
    usePartnerStore.setState({ characterCardsV2: [{ ...createEmptyCardV2('沈霜'), id: 'lc1' }] });
    fetchMock.mockImplementation(async (path: string, opts?: { method?: string }) => {
      if (path === '/api/worlds/w1') {
        return { ...WORLD, roster: [{ cloudCharacterId: 'cOther', name: '游侠' }], memberCount: 1 };
      }
      if (path === '/api/worlds/w1/events') return { events: [], nextCursor: null };
      if (path === '/api/assets/characters/mine') {
        return [{ id: 'cNew', localCardId: 'lc1', version: 1, rightsDeclaration: 'original', moderation: 'approved', withdrawn: false, createdAt: 1 }];
      }
      if (path === '/api/worlds/w1/interventions/mine') return { interventions: [] };
      if (path === '/api/me/consents?status=pending') return { consents: [] };
      if (path === '/api/worlds/w1/join' && opts?.method === 'POST') {
        return { membershipId: 'm1', worldId: 'w1', cloudCharacterId: 'cNew', status: 'active' };
      }
      throw new Error(`unexpected ${path}`);
    });

    renderRoom();

    // 候选角色（本地卡名解析）出现
    expect(await screen.findByText('投放角色')).toBeInTheDocument();
    fireEvent.click(await screen.findByRole('checkbox', { name: /入场边界协议/ }));
    fireEvent.click(screen.getByRole('button', { name: /确认投放/ }));

    expect(await screen.findByText(/投放成功/)).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(
        ([p, o]) => p === '/api/worlds/w1/join' && (o as { method?: string })?.method === 'POST',
      ),
    ).toBe(true);
  });
});

describe('WorldRoom — 世界线视图（按角色过滤 + 深链）', () => {
  const renderRoomAt = (path: string) =>
    render(
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path="/platform/worlds/:id" element={<WorldRoom />} />
        </Routes>
      </MemoryRouter>,
    );

  it('有我方角色时出现「世界线」视图：过滤到我角色参与的事件并叙事化', async () => {
    fetchMock.mockImplementation(happyPath);
    renderRoomAt('/platform/worlds/w1');
    await screen.findByText('云州世界');

    // 我方角色 cMine 在阵容 → 「世界线」视图可用
    fireEvent.click(screen.getByText('世界线'));
    // cMine 参与了 ev1 → 叙事化呈现该事件，并标「我的角色」
    expect(await screen.findByText('我的角色')).toBeInTheDocument();
    expect(screen.getByText('沈霜与游侠交谈')).toBeInTheDocument();
    // 同场其他角色以名解析（cOther → 游侠）
    expect(screen.getByText(/同场：游侠/)).toBeInTheDocument();
  });

  it('?character= 深链：预选角色并自动切到世界线视图', async () => {
    fetchMock.mockImplementation(happyPath);
    renderRoomAt('/platform/worlds/w1?character=cMine');

    // 无需手动点击，深链直接进入世界线（出现「我的角色」标注 + 事件）
    expect(await screen.findByText('我的角色')).toBeInTheDocument();
    expect(screen.getByText('沈霜与游侠交谈')).toBeInTheDocument();
  });

  it('我角色在本世界尚无事件：世界线空态', async () => {
    fetchMock.mockImplementation(async (path: string, opts?: { method?: string }) => {
      if (path === '/api/worlds/w1/events') return { events: [], nextCursor: null };
      return happyPath(path, opts);
    });
    renderRoomAt('/platform/worlds/w1');
    await screen.findByText('云州世界');

    fireEvent.click(screen.getByText('世界线'));
    expect(await screen.findByText(/TA 还没在这个世界留下故事/)).toBeInTheDocument();
  });
});
