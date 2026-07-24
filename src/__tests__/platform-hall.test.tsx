import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
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
            { id: 'w1', roomType: 'idle', title: '云州放置世界', status: 'open', visibility: 'official', memberLimit: 10, memberCount: 4, tickPerDay: 3, starRating: 3 },
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
    // 星级徽标：starRating=3 → 金色「3★」
    expect(screen.getByText('3★')).toBeInTheDocument();
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

  it('搜索 + 切热门：请求带 q 与 sort=hot，热门条目渲染热度徽标且无「加载更多」', async () => {
    const world = {
      id: 'w1',
      roomType: 'idle',
      title: '云州放置世界',
      status: 'open',
      visibility: 'official',
      memberLimit: 10,
      memberCount: 4,
      tickPerDay: 3,
    };
    fetchMock.mockImplementation(async (path: string) => {
      if (path.startsWith('/api/worlds?')) {
        // 热门：快照榜附 hotScore、nextCursor 恒 null；最新：cursor 分页有下一页。
        if (path.includes('sort=hot')) {
          return { worlds: [{ ...world, hotScore: 128 }], nextCursor: null };
        }
        return { worlds: [world], nextCursor: 'cur-next' };
      }
      if (path === '/api/me/reports') return { reports: [], nextCursor: null };
      throw new Error(`unexpected ${path}`);
    });

    renderHall();
    // 最新模式：有 nextCursor → 显示加载更多
    expect(await screen.findByRole('button', { name: '加载更多' })).toBeInTheDocument();

    // 输入搜索词（含 %，验证 URL 编码）并回车触发
    const input = screen.getByPlaceholderText('搜索世界标题');
    fireEvent.change(input, { target: { value: '云州100%' } });
    fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
    const encodedQ = `q=${encodeURIComponent('云州100%')}`;
    await waitFor(() =>
      expect(fetchMock.mock.calls.some(([p]) => typeof p === 'string' && p.includes(encodedQ))).toBe(true),
    );

    // 切到热门：请求同时携带 q 与 sort=hot
    fireEvent.click(screen.getByText('热门'));
    await waitFor(() => {
      const hotCall = fetchMock.mock.calls
        .map(([p]) => p as string)
        .find((p) => typeof p === 'string' && p.includes('sort=hot'));
      expect(hotCall).toBeTruthy();
      expect(hotCall).toContain(encodedQ);
    });

    // 热门条目渲染热度徽标；快照榜不分页 → 无加载更多
    expect(await screen.findByText(/热度 128/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '加载更多' })).toBeNull();
  });
});
