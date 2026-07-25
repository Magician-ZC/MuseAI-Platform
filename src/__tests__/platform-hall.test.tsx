import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
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

  // 以下三条对齐 docs/design/client-ui-design.md §6（大厅五个组成 + 真实位图封面 + 诚实数据）。

  it('世界目录卡片使用真实位图封面，且同一 world.id 的兜底封面是确定性的', async () => {
    const base = {
      roomType: 'idle',
      status: 'open',
      visibility: 'public',
      memberLimit: 10,
      memberCount: 4,
      tickPerDay: 3,
    };
    fetchMock.mockImplementation(async (path: string) => {
      if (path.startsWith('/api/worlds?')) {
        return {
          worlds: [
            { ...base, id: 'w1', title: '主视觉世界' },
            { ...base, id: 'w2', title: '目录世界甲' },
          ],
          nextCursor: null,
        };
      }
      if (path === '/api/assets/worlds/mine') return [];
      return { events: [] };
    });

    const first = renderHall();
    const cover = await screen.findByAltText('目录世界甲的世界封面');
    const src = cover.getAttribute('src');
    expect(src).toMatch(/^\/assets\/platform\/.+\.png$/);
    // 主视觉世界同样是真实位图，不使用占位框 / Emoji / CSS 绘图。
    expect(screen.getByAltText('主视觉世界的世界封面').getAttribute('src')).toMatch(/^\/assets\/platform\/.+\.png$/);
    // 辅助栏「相关世界」的缩略封面同样是真实位图。
    const railThumb = screen.getByRole('complementary').querySelector('.related-worlds img');
    expect(railThumb?.getAttribute('src')).toMatch(/^\/assets\/platform\/.+\.png$/);

    // 确定性兜底：重渲染后同一 world.id 仍是同一张图（不随机）。
    first.unmount();
    usePlatformStore.getState().reset();
    renderHall();
    const again = await screen.findByAltText('目录世界甲的世界封面');
    expect(again.getAttribute('src')).toBe(src);
  });

  it('发布状态与相关世界位于右侧辅助栏，而非主栏', async () => {
    const base = {
      roomType: 'idle',
      status: 'open',
      visibility: 'public',
      memberLimit: 10,
      memberCount: 4,
      tickPerDay: 3,
    };
    fetchMock.mockImplementation(async (path: string) => {
      if (path.startsWith('/api/worlds?')) {
        return {
          worlds: [
            { ...base, id: 'w1', title: '主视觉世界' },
            { ...base, id: 'w2', title: '目录世界甲' },
          ],
          nextCursor: null,
        };
      }
      if (path === '/api/assets/worlds/mine') {
        return [
          { id: 'tpl-1', title: '我的世界书', version: 2, moderation: 'approved', withdrawn: false, createdAt: Date.UTC(2026, 6, 24, 12, 0) },
        ];
      }
      return { events: [] };
    });

    renderHall();
    const rail = await screen.findByRole('complementary');
    expect(await within(rail).findByText('发布状态')).toBeInTheDocument();
    expect(within(rail).getByText('相关世界')).toBeInTheDocument();
    // 发布状态读的是 /assets/worlds/mine 真实字段（标题 + 审核态 + 版本）。
    expect(within(rail).getByText('我的世界书')).toBeInTheDocument();
    expect(within(rail).getByText(/已通过 · 第 2 版/)).toBeInTheDocument();
    // 相关世界复用已加载的同房型世界，不造数据。
    expect(within(rail).getByText('目录世界甲')).toBeInTheDocument();

    const main = screen.getByRole('main');
    expect(within(main).queryByText('发布状态')).toBeNull();
    expect(within(main).queryByText('你的发布状态')).toBeNull();
  });

  it('世界动态条目只渲染事件真实字段：标题取 summary，相对时间取 occurredAt', async () => {
    const occurredAt = Date.now() - 2 * 60 * 60 * 1000;
    fetchMock.mockImplementation(async (path: string) => {
      if (path.startsWith('/api/worlds?')) {
        return {
          worlds: [
            { id: 'w1', roomType: 'idle', title: '云州放置世界', status: 'open', visibility: 'public', memberLimit: 10, memberCount: 4, tickPerDay: 3 },
          ],
          nextCursor: null,
        };
      }
      if (path === '/api/assets/worlds/mine') return [];
      if (path === '/api/worlds/w1/events') {
        return {
          events: [
            {
              id: 'e1',
              worldId: 'w1',
              tick: 7,
              sequence: 1,
              domainEventId: 'd1',
              type: 'alliance',
              actors: ['青岚'],
              visibility: 'public',
              projection: { summary: '青岚与商队结盟。' },
              occurredAt,
            },
          ],
        };
      }
      throw new Error(`unexpected ${path}`);
    });

    renderHall();
    // 标题与主视觉「最近活动」都来自同一条真实事件摘要。
    expect((await screen.findAllByText('青岚与商队结盟。')).length).toBeGreaterThan(0);
    // 元信息行由 type / actors / tick 拼装，全部是接口字段。
    expect(screen.getByText('结盟 · 青岚 · 第 7 拍')).toBeInTheDocument();
    // 相对时间由 occurredAt 推导，而不是按列表下标写死。
    expect(screen.getByText('2小时前')).toBeInTheDocument();
    // 旧实现按 index 硬编码的伪造文案必须消失。
    expect(screen.queryByText('北境风暴平息，商路重启')).toBeNull();
    expect(screen.queryByText('1小时前')).toBeNull();
  });
});
