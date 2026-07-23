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
import MyWorlds from '../pages/platform/MyWorlds';
import { usePlatformStore } from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.getState().reset();
});

const renderMy = () =>
  render(
    <MemoryRouter>
      <MyWorlds />
    </MemoryRouter>,
  );

describe('MyWorlds', () => {
  it('展示已投放世界与未读日报角标', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/reports') {
        return {
          reports: [
            { id: 'r2', worldId: 'w1', characterId: 'cA', reportDay: '2026-07-22', opened: false, createdAt: 200 },
            { id: 'r1', worldId: 'w1', characterId: 'cA', reportDay: '2026-07-21', opened: true, createdAt: 100 },
          ],
          nextCursor: null,
        };
      }
      if (path === '/api/worlds/w1') return { title: '云州世界' };
      throw new Error(`unexpected ${path}`);
    });

    renderMy();
    expect(await screen.findByText('云州世界')).toBeInTheDocument();
    // 1 份未读
    expect(await screen.findByText('1')).toBeInTheDocument();
  });

  it('无投放世界：空态引导', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/reports') return { reports: [], nextCursor: null };
      throw new Error(`unexpected ${path}`);
    });

    renderMy();
    expect(await screen.findByText('你还没有把角色投进任何世界')).toBeInTheDocument();
  });
});
