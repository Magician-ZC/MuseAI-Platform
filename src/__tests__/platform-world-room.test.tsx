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

  it('切换到关系图谱：渲染 echarts 力导向图', async () => {
    fetchMock.mockImplementation(happyPath);
    renderRoom();
    await screen.findByText('云州世界');

    fireEvent.click(screen.getByText('关系图谱'));
    expect(await screen.findByTestId('echarts-graph')).toBeInTheDocument();
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
