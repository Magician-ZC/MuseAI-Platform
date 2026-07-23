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
import DailyReport from '../pages/platform/DailyReport';
import { usePlatformStore } from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.getState().reset();
});

describe('DailyReport — 阅读态', () => {
  it('详情：高光/关系/独白区分公开事实·私密视角·模型推断，打开即拉取（北极星埋点）', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/reports/rpt1') {
        return {
          id: 'rpt1',
          worldId: 'w1',
          characterId: 'cA',
          openedAt: null,
          createdAt: 1,
          content: {
            reportDay: '2026-07-22',
            characterId: 'cA',
            highlights: [
              { eventId: 'e1', type: 'action', summary: '在集市赢得一场辩论', kind: 'public_fact' },
              { eventId: 'e2', type: 'dialogue', summary: '独自记下一个秘密', kind: 'private_view' },
            ],
            relationChanges: [{ eventId: 'e3', type: 'alliance', summary: '与游侠结盟', kind: 'public_fact' }],
            monologue: { text: '我把今天悄悄收进了心里', kind: 'model_inference' },
            provenanceLegend: {
              public_fact: '公开事实',
              private_view: '角色私密视角（仅你可见）',
              model_inference: '模型推断',
            },
          },
        };
      }
      throw new Error(`unexpected ${path}`);
    });

    render(
      <MemoryRouter initialEntries={['/platform/reports/rpt1']}>
        <Routes>
          <Route path="/platform/reports/:id" element={<DailyReport />} />
        </Routes>
      </MemoryRouter>,
    );

    // 独白被排版引号包裹（“…”），用子串匹配。
    expect(await screen.findByText(/我把今天悄悄收进了心里/)).toBeInTheDocument();
    expect(screen.getByText('角色独白')).toBeInTheDocument();
    expect(screen.getByText('在集市赢得一场辩论')).toBeInTheDocument();
    // 三类来源标签均出现（至少各一次）
    expect(screen.getAllByText('公开事实').length).toBeGreaterThan(0);
    expect(screen.getAllByText('角色私密视角').length).toBeGreaterThan(0);
    expect(screen.getAllByText('模型推断').length).toBeGreaterThan(0);
    // 打开即 GET（服务端回写 opened_at）
    expect(fetchMock).toHaveBeenCalledWith('/api/me/reports/rpt1');
  });

  it('列表：展示日报与未读标记', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/reports') {
        return {
          reports: [{ id: 'rpt1', worldId: 'w1', characterId: 'cA', reportDay: '2026-07-22', opened: false, createdAt: 1 }],
          nextCursor: null,
        };
      }
      if (path === '/api/worlds/w1') return { title: '云州世界' };
      throw new Error(`unexpected ${path}`);
    });

    render(
      <MemoryRouter initialEntries={['/platform/reports']}>
        <Routes>
          <Route path="/platform/reports" element={<DailyReport />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(await screen.findByText('2026-07-22')).toBeInTheDocument();
    expect(screen.getByText('未读')).toBeInTheDocument();
  });

  it('详情加载失败：优雅降级', async () => {
    fetchMock.mockImplementation(async () => {
      throw new TypeError('offline');
    });

    render(
      <MemoryRouter initialEntries={['/platform/reports/rptX']}>
        <Routes>
          <Route path="/platform/reports/:id" element={<DailyReport />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
  });
});
