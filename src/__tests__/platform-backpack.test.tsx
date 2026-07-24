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
import Backpack from '../pages/platform/Backpack';
import { usePlatformStore } from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;

const BACKPACK = {
  items: [
    {
      backpackId: 'bp1',
      status: 'carried',
      acquiredWorldId: 'w0',
      carriedWorldId: 'w1',
      item: {
        id: 'itm_key',
        narrative: '一枚泛着幽光的铜钥',
        effectTags: ['unlock', 'ward'],
        origin: { worldTemplateId: 't', cosmology: ['xuanhuan'], powerTier: 3 },
      },
    },
    {
      backpackId: 'bp2',
      status: 'owned',
      acquiredWorldId: 'w2',
      carriedWorldId: null,
      item: {
        id: 'itm_gem',
        narrative: '半块温热的玉',
        effectTags: [],
        origin: { worldTemplateId: 't2', cosmology: [], powerTier: 1 },
      },
    },
    {
      backpackId: 'bp3',
      status: 'sealed',
      acquiredWorldId: 'w0',
      carriedWorldId: 'w3',
      item: {
        id: 'itm_blade',
        narrative: '被降档封印的凶刃',
        effectTags: ['attack'],
        origin: { worldTemplateId: 't', cosmology: ['wuxia'], powerTier: 9 },
      },
    },
  ],
};

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.getState().reset();
});

const renderBackpack = () =>
  render(
    <MemoryRouter>
      <Backpack />
    </MemoryRouter>,
  );

describe('Backpack（跨世界背包）', () => {
  it('按状态分组渲染，展示 narrative / powerTier / effectTags', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/backpack') return BACKPACK;
      throw new Error(`unexpected ${path}`);
    });
    renderBackpack();

    // narrative
    expect(await screen.findByText('一枚泛着幽光的铜钥')).toBeInTheDocument();
    expect(screen.getByText('半块温热的玉')).toBeInTheDocument();
    // 分组标签（随身 / 在库 / 封印）——分组标题与物品状态标签各出现，用 getAllByText 断言存在
    expect(screen.getAllByText('随身').length).toBeGreaterThan(0);
    expect(screen.getAllByText('在库').length).toBeGreaterThan(0);
    expect(screen.getAllByText('封印').length).toBeGreaterThan(0);
    // powerTier 与 effectTags
    expect(screen.getByText('强度 3')).toBeInTheDocument();
    expect(screen.getByText('unlock')).toBeInTheDocument();
    expect(screen.getByText('ward')).toBeInTheDocument();
    // 道具干预未接线的防误导文案
    expect(screen.getByText('携带经入场生效，主动投放后续开放')).toBeInTheDocument();
  });

  it('空背包：引导去世界赢取信物', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/backpack') return { items: [] };
      throw new Error(`unexpected ${path}`);
    });
    renderBackpack();
    expect(await screen.findByText(/背包还是空的/)).toBeInTheDocument();
  });

  it('加载失败：优雅降级为连接平台失败', async () => {
    fetchMock.mockImplementation(async () => {
      throw new TypeError('offline');
    });
    renderBackpack();
    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
  });
});
