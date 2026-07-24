import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
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
import MyCharacters from '../pages/platform/MyCharacters';
import { usePlatformStore } from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;

const mkMembership = (worldId: string, worldTitle: string, cid: string, name: string, joinedAt: number) => ({
  worldId,
  worldTitle,
  roomType: 'idle',
  worldStatus: 'running',
  stateRevision: 3,
  cloudCharacterId: cid,
  characterName: name,
  membershipStatus: 'active',
  joinedAt,
});

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.getState().reset();
});

const renderPage = () =>
  render(
    <MemoryRouter>
      <MyCharacters />
    </MemoryRouter>,
  );

describe('MyCharacters（我的角色 · 各世界）', () => {
  it('按角色分组：同一角色的多个世界合并为一组，未读日报角标', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/memberships') {
        return {
          memberships: [
            mkMembership('w1', '云州世界', 'cc1', '沈霜', 300),
            mkMembership('w2', '雾都', 'cc1', '沈霜', 200),
            mkMembership('w1', '云州世界', 'cc2', '游侠', 100),
          ],
        };
      }
      if (path === '/api/me/reports') {
        return {
          reports: [
            { id: 'r2', worldId: 'w1', characterId: 'cc1', reportDay: '2026-07-22', opened: false, createdAt: 200 },
            { id: 'r1', worldId: 'w1', characterId: 'cc1', reportDay: '2026-07-21', opened: true, createdAt: 100 },
          ],
          nextCursor: null,
        };
      }
      throw new Error(`unexpected ${path}`);
    });
    renderPage();

    // 两个角色分组（沈霜聚合 w1+w2，游侠单独）
    expect(await screen.findByText('沈霜')).toBeInTheDocument();
    expect(screen.getByText('游侠')).toBeInTheDocument();
    expect(screen.getByText('2 个世界')).toBeInTheDocument(); // 沈霜横跨 2 个世界
    // cc1/w1 有 1 份未读 → 角标
    expect(await screen.findByText('1')).toBeInTheDocument();
  });

  it('离场按钮：确认后调用 POST /worlds/{id}/leave', async () => {
    fetchMock.mockImplementation(async (path: string, opts?: { method?: string }) => {
      if (path === '/api/me/memberships') {
        return { memberships: [mkMembership('w1', '云州世界', 'cc1', '沈霜', 300)] };
      }
      if (path === '/api/me/reports') return { reports: [], nextCursor: null };
      if (path === '/api/worlds/w1/leave' && opts?.method === 'POST') {
        return { worldId: 'w1', cloudCharacterId: 'cc1', status: 'left' };
      }
      throw new Error(`unexpected ${path}`);
    });
    renderPage();

    // 触发离场 → Popconfirm → 确认（触发按钮含 logout 图标，无障碍名为「logout 离场」→ 用子串匹配）
    fireEvent.click(await screen.findByRole('button', { name: /离场/ }));
    fireEvent.click(await screen.findByRole('button', { name: '确认离场' }));

    await vi.waitFor(() => {
      expect(
        fetchMock.mock.calls.some(
          ([p, o]) => p === '/api/worlds/w1/leave' && (o as { method?: string })?.method === 'POST',
        ),
      ).toBe(true);
    });
  });

  it('无成员关系：空态引导去大厅投放', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/memberships') return { memberships: [] };
      if (path === '/api/me/reports') return { reports: [], nextCursor: null };
      throw new Error(`unexpected ${path}`);
    });
    renderPage();
    expect(await screen.findByText('你还没有把角色投进任何世界')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /去大厅投放角色/ })).toBeInTheDocument();
  });
});
