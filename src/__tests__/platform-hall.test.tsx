import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';

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

import { cloudFetch } from '../utils/cloudApi';
import PlatformHall from '../pages/platform/PlatformHall';
import { usePlatformStore } from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.setState({ roomTypeFilter: 'idle' });
  usePlatformStore.getState().reset();
});

const renderHall = () =>
  render(
    <MemoryRouter>
      <PlatformHall />
    </MemoryRouter>,
  );

describe('PlatformHall', () => {
  it('渲染精选世界卡片', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path.startsWith('/api/worlds?')) {
        return {
          worlds: [
            { id: 'w1', roomType: 'idle', title: '云州放置世界', status: 'open', visibility: 'official', memberLimit: 10, memberCount: 4, tickPerDay: 3 },
          ],
          nextCursor: null,
        };
      }
      if (path === '/api/me/reports') return { reports: [], nextCursor: null };
      throw new Error(`unexpected ${path}`);
    });

    renderHall();
    expect(await screen.findByText('云州放置世界')).toBeInTheDocument();
    // 「放置房」同时出现在房型筛选与世界卡标签，故用 getAllByText。
    expect(screen.getAllByText('放置房').length).toBeGreaterThan(0);
  });

  it('云端不可用：优雅降级为「连接平台失败」而非崩溃', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path.startsWith('/api/worlds?')) throw new TypeError('network down');
      if (path === '/api/me/reports') return { reports: [], nextCursor: null };
      throw new Error(`unexpected ${path}`);
    });

    renderHall();
    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
  });
});
