import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
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

import { cloudFetch, CloudError } from '../utils/cloudApi';
import ArenaHost from '../pages/platform/ArenaHost';

const fetchMock = cloudFetch as unknown as Mock;

const WORLD = {
  id: 'w1',
  title: '赛季总决赛',
  roomType: 'arena',
  status: 'running',
  visibility: 'official',
  memberLimit: 8,
  memberCount: 2,
  tickPerDay: 6,
  templateId: 't',
  templateVersion: 1,
  engineVersion: 'e1',
  promptSetVersion: 'p1',
  modelRouteVersion: 'm1',
  roster: [
    { cloudCharacterId: 'cA', name: '沈霜' },
    { cloudCharacterId: 'cB', name: '陆沉' },
  ],
};

const REPORT = {
  worldId: 'w1',
  match: { phase: 'running', alliances: [], eliminations: [], winnerCharId: null },
  rounds: [],
  environment: [],
  compliance: { arbitrationPublic: true, aiGenerated: true },
};

function renderHost() {
  return render(
    <MemoryRouter initialEntries={['/platform/arena/w1/host']}>
      <Routes>
        <Route path="/platform/arena/:worldId/host" element={<ArenaHost />} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  fetchMock.mockReset();
});

describe('ArenaHost 主播控制台', () => {
  it('展示赛制状态、阵容与红线（无免死 / 荣誉奖励 / 同意门控）', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/worlds/w1') return WORLD;
      if (path === '/api/arena/w1/report') return REPORT;
      throw new Error(`unexpected ${path}`);
    });
    renderHost();
    expect(await screen.findByText('赛季总决赛')).toBeInTheDocument();
    expect(screen.getAllByText('沈霜').length).toBeGreaterThan(0);
    // 淘汰同意门控说明
    expect(screen.getByText('淘汰的同意门控')).toBeInTheDocument();
    // 红线文案
    expect(screen.getByText(/免死与最终判定不可买/)).toBeInTheDocument();
    expect(screen.getByText(/胜者奖励为荣誉性/)).toBeInTheDocument();
  });

  it('触发一个回合 → POST /arena/w1/host/tick（idempotent）', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/worlds/w1') return WORLD;
      if (path === '/api/arena/w1/report') return REPORT;
      if (path === '/api/arena/w1/host/tick') return { worldId: 'w1', tickNo: 1, scheduled: true };
      throw new Error(`unexpected ${path}`);
    });
    renderHost();
    fireEvent.click(await screen.findByRole('button', { name: /触发一个回合/ }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/api/arena/w1/host/tick',
        expect.objectContaining({ method: 'POST', idempotent: true }),
      ),
    );
  });

  it('裁定淘汰 → POST /arena/w1/eliminate，落定前先发起同意提案', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/worlds/w1') return WORLD;
      if (path === '/api/arena/w1/report') return REPORT;
      if (path === '/api/arena/w1/eliminate') return { worldId: 'w1', characterId: 'cA', status: 'pending_consent', consentId: 'c1' };
      throw new Error(`unexpected ${path}`);
    });
    renderHost();
    fireEvent.click(await screen.findByRole('button', { name: /裁定淘汰/ }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/api/arena/w1/eliminate',
        expect.objectContaining({ method: 'POST', idempotent: true, body: { cloudCharacterId: 'cA' } }),
      ),
    );
    expect(await screen.findByText('淘汰提案已发起')).toBeInTheDocument();
  });

  it('非主播（403）→ 友好提示并锁定控制', async () => {
    fetchMock.mockImplementation(async (path: string, opts?: { method?: string }) => {
      if (path === '/api/worlds/w1') return WORLD;
      if (path === '/api/arena/w1/report') return REPORT;
      if (opts?.method === 'POST') throw new CloudError('forbidden', 'forbidden', 403);
      throw new Error(`unexpected ${path}`);
    });
    renderHost();
    fireEvent.click(await screen.findByRole('button', { name: /触发一个回合/ }));
    expect(await screen.findByText('你不是本世界主播')).toBeInTheDocument();
  });
});
