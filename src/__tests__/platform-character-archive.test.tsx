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

import { cloudFetch } from '../utils/cloudApi';
import CharacterArchive from '../pages/platform/CharacterArchive';
import { usePlatformStore } from '../stores/usePlatformStore';
import { usePartnerStore } from '../stores/usePartnerStore';
import { createEmptyCardV2 } from '../utils/characterCardV2';

const fetchMock = cloudFetch as unknown as Mock;

const mkMembership = (worldId: string, worldTitle: string, cid: string, name: string) => ({
  worldId,
  worldTitle,
  roomType: 'idle',
  worldStatus: 'running',
  stateRevision: 1,
  cloudCharacterId: cid,
  characterName: name,
  membershipStatus: 'active',
  joinedAt: 100,
});

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.getState().reset();
  usePartnerStore.setState({ characterCardsV2: [{ ...createEmptyCardV2('沈霜'), id: 'lc1' }] });
});

const renderArchive = () =>
  render(
    <MemoryRouter initialEntries={['/platform/characters/cc1']}>
      <Routes>
        <Route path="/platform/characters/:cid" element={<CharacterArchive />} />
      </Routes>
    </MemoryRouter>,
  );

describe('CharacterArchive（角色一生档案）', () => {
  it('fan-out 组合：按 characterId 过滤世界/日报，近似归因物品，聚合羁绊，moderation 标签', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/memberships') {
        return {
          memberships: [
            mkMembership('w1', '云州世界', 'cc1', '沈霜'),
            mkMembership('w2', '雾都', 'cc1', '沈霜'),
            mkMembership('w3', '别人的世界', 'cc2', '游侠'), // 非 cc1，应排除
          ],
        };
      }
      if (path === '/api/me/reports') {
        return {
          reports: [
            { id: 'r1', worldId: 'w1', characterId: 'cc1', reportDay: '2026-07-22', opened: false, createdAt: 200 },
            { id: 'r9', worldId: 'w3', characterId: 'cc2', reportDay: '2026-07-20', opened: true, createdAt: 100 },
          ],
          nextCursor: null,
        };
      }
      if (path === '/api/me/backpack') {
        return {
          items: [
            {
              backpackId: 'bp1',
              status: 'owned',
              acquiredWorldId: 'w1', // ∈ cc1 所在世界 → 归因给 cc1
              carriedWorldId: null,
              item: { id: 'itm1', narrative: '通关所获的信物', effectTags: ['relic'], origin: { worldTemplateId: 't', cosmology: [], powerTier: 4 } },
            },
            {
              backpackId: 'bp2',
              status: 'owned',
              acquiredWorldId: 'w9', // 非 cc1 世界 → 排除
              carriedWorldId: null,
              item: { id: 'itm2', narrative: '无关的杂物', effectTags: [], origin: { worldTemplateId: 't', cosmology: [], powerTier: 1 } },
            },
          ],
        };
      }
      if (path === '/api/assets/characters/mine') {
        return [{ id: 'cc1', localCardId: 'lc1', version: 1, rightsDeclaration: 'original', moderation: 'approved', withdrawn: false, createdAt: 1 }];
      }
      if (path === '/api/worlds/w1/state-summary') {
        return { relations: [{ from: 'cc1', to: 'ccOther', trust: 30, affinity: 55, fear: 0, debt: 0 }], characters: [] };
      }
      if (path === '/api/worlds/w2/state-summary') return { relations: [], characters: [] };
      if (path === '/api/worlds/w1') return { roster: [{ cloudCharacterId: 'cc1', name: '沈霜' }, { cloudCharacterId: 'ccOther', name: '游侠' }] };
      if (path === '/api/worlds/w2') return { roster: [{ cloudCharacterId: 'cc1', name: '沈霜' }] };
      throw new Error(`unexpected ${path}`);
    });

    renderArchive();

    // 头部身份卡：本地卡名 + moderation 标签
    expect(await screen.findByText('沈霜')).toBeInTheDocument();
    expect(screen.getByText('已通过')).toBeInTheDocument();
    // 走过的世界：cc1 的 w1/w2，排除他人 cc2 的 w3（云州世界在世界时间线 + 日报行各出现一次）
    expect(screen.getAllByText('云州世界').length).toBeGreaterThan(0);
    expect(screen.getByText('雾都')).toBeInTheDocument();
    expect(screen.queryByText('别人的世界')).not.toBeInTheDocument();
    // 逐日人生：仅 cc1 的日报（按 characterId 过滤）
    expect(screen.getByText('2026-07-22')).toBeInTheDocument();
    expect(screen.queryByText('2026-07-20')).not.toBeInTheDocument();
    // 带来的信物：近似归因（得自 cc1 所在世界），排除 w9
    expect(screen.getByText('通关所获的信物')).toBeInTheDocument();
    expect(screen.queryByText('无关的杂物')).not.toBeInTheDocument();
    // 羁绊：聚合含 cc1 的边（游侠）
    expect(screen.getByText('游侠')).toBeInTheDocument();
  });

  it('加载失败：优雅降级 + 返回入口', async () => {
    fetchMock.mockImplementation(async () => {
      throw new TypeError('offline');
    });
    renderArchive();
    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
  });
});
