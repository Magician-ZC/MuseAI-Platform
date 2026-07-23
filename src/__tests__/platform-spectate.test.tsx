import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { render, screen } from '@testing-library/react';
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

import { cloudFetch, cloudStream } from '../utils/cloudApi';
import WorldSpectate from '../pages/platform/WorldSpectate';
import { usePlatformStore } from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;
const streamMock = cloudStream as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  streamMock.mockReset();
  streamMock.mockReturnValue(() => {});
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
});
