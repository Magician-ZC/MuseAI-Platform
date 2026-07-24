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
import Bonds from '../pages/platform/Bonds';
import { usePlatformStore } from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.getState().reset();
});

const renderPage = () =>
  render(
    <MemoryRouter>
      <Bonds />
    </MemoryRouter>,
  );

describe('Bonds（羁绊）', () => {
  it('只显含我角色的边、direction 判定、按 |affinity| 排序、排除非我边', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/memberships') {
        return {
          memberships: [
            {
              worldId: 'w1',
              worldTitle: '云州世界',
              roomType: 'idle',
              worldStatus: 'running',
              stateRevision: 1,
              cloudCharacterId: 'cc1',
              characterName: '沈霜',
              membershipStatus: 'active',
              joinedAt: 100,
            },
          ],
        };
      }
      if (path === '/api/worlds/w1/state-summary') {
        return {
          relations: [
            // 我是 from → 我对 TA（out），affinity 40
            { from: 'cc1', to: 'ccOther', trust: 50, affinity: 40, fear: 0, debt: 0 },
            // 我是 to → TA 对我（in），affinity -30
            { from: 'ccOther2', to: 'cc1', trust: 10, affinity: -30, fear: 20, debt: 5 },
            // 与我无关（前端应过滤掉，即便 |affinity| 最大）
            { from: 'ccX', to: 'ccY', trust: 0, affinity: 99, fear: 0, debt: 0 },
          ],
          characters: [],
        };
      }
      if (path === '/api/worlds/w1') {
        return {
          roster: [
            { cloudCharacterId: 'cc1', name: '沈霜' },
            { cloudCharacterId: 'ccOther', name: '游侠' },
            { cloudCharacterId: 'ccOther2', name: '刺客' },
            { cloudCharacterId: 'ccX', name: '路人甲' },
            { cloudCharacterId: 'ccY', name: '路人乙' },
          ],
        };
      }
      throw new Error(`unexpected ${path}`);
    });
    renderPage();

    // 含我角色的两条边：游侠（out）、刺客（in）
    expect(await screen.findByText('游侠')).toBeInTheDocument();
    expect(screen.getByText('刺客')).toBeInTheDocument();
    // 与我无关的边被前端过滤（路人不出现）
    expect(screen.queryByText('路人甲')).not.toBeInTheDocument();
    expect(screen.queryByText('路人乙')).not.toBeInTheDocument();
    // direction 判定
    expect(screen.getByText('我对 TA')).toBeInTheDocument();
    expect(screen.getByText('TA 对我')).toBeInTheDocument();
    // 按 |affinity| 排序：40 在 30 之前
    const affinityTags = screen.getAllByText(/^亲和 /).map((n) => n.textContent);
    expect(affinityTags[0]).toBe('亲和 40');
    expect(affinityTags[1]).toBe('亲和 -30');
  });

  it('无成员关系：空态引导看我的角色', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/memberships') return { memberships: [] };
      throw new Error(`unexpected ${path}`);
    });
    renderPage();
    expect(await screen.findByText(/还没有结下任何羁绊/)).toBeInTheDocument();
  });

  it('单世界 state-summary 失败静默：不整页崩溃，退化为空羁绊', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/memberships') {
        return {
          memberships: [
            {
              worldId: 'w1',
              worldTitle: '云州世界',
              roomType: 'idle',
              worldStatus: 'running',
              stateRevision: 1,
              cloudCharacterId: 'cc1',
              characterName: '沈霜',
              membershipStatus: 'active',
              joinedAt: 100,
            },
          ],
        };
      }
      // state-summary 抛错 → 该世界静默跳过
      if (path === '/api/worlds/w1/state-summary') throw new TypeError('offline');
      if (path === '/api/worlds/w1') return { roster: [] };
      throw new Error(`unexpected ${path}`);
    });
    renderPage();
    expect(await screen.findByText(/还没有结下任何羁绊/)).toBeInTheDocument();
  });
});
