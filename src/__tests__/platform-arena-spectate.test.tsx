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

vi.mock('echarts-for-react', () => ({
  default: () => <div data-testid="echarts-graph" />,
}));

import { cloudFetch } from '../utils/cloudApi';
import ArenaSpectate from '../pages/platform/ArenaSpectate';

const fetchMock = cloudFetch as unknown as Mock;

const REPORT = {
  worldId: 'w1',
  match: { phase: 'running', alliances: [], eliminations: ['cB'], winnerCharId: null },
  rounds: [
    {
      tick: 1,
      events: [
        {
          sequence: 1,
          type: 'conflict',
          actors: ['cA', 'cB'],
          summary: '沈霜与陆沉在擂台正面交锋',
          ruleRefs: ['规则R1：先手判定', '道具A 已生效'],
        },
      ],
      env: [{ appliedTick: 1, kind: 'gift_boon', payload: { label: '烈焰增益' }, aggregatedCount: 3 }],
    },
  ],
  environment: [{ appliedTick: 1, kind: 'gift_boon', payload: { label: '烈焰增益' }, aggregatedCount: 3 }],
  compliance: { arbitrationPublic: true, aiGenerated: true },
};

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

function renderSpectate() {
  return render(
    <MemoryRouter initialEntries={['/platform/arena/w1/spectate']}>
      <Routes>
        <Route path="/platform/arena/:worldId/spectate" element={<ArenaSpectate />} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  fetchMock.mockReset();
  fetchMock.mockImplementation(async (path: string) => {
    if (path === '/api/arena/w1/report') return REPORT;
    if (path === '/api/worlds/w1') return WORLD;
    throw new Error(`unexpected ${path}`);
  });
});

describe('ArenaSpectate — 观战 + 透明战报', () => {
  it('渲染事件时间轴 + 判定依据 ruleRefs + 礼物/环境日志', async () => {
    renderSpectate();
    expect(await screen.findByText('沈霜与陆沉在擂台正面交锋')).toBeInTheDocument();
    // 判定依据（对抗剧本质疑）
    expect(screen.getByText('判定依据：')).toBeInTheDocument();
    expect(screen.getByText('规则R1：先手判定')).toBeInTheDocument();
    // 礼物/环境日志
    expect(screen.getAllByText('礼物加成').length).toBeGreaterThan(0);
    // 透明战报红线
    expect(screen.getByText('透明战报：对抗「是不是剧本」的质疑')).toBeInTheDocument();
  });

  it('echarts 阵容/淘汰图渲染，且为只读（无干预/淘汰面板）', async () => {
    renderSpectate();
    await screen.findByText('沈霜与陆沉在擂台正面交锋');
    expect(screen.getByTestId('echarts-graph')).toBeInTheDocument();
    // 只读：无主播/干预控制
    expect(screen.queryByRole('button', { name: /触发一个回合/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /裁定淘汰/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /提交托梦/ })).toBeNull();
  });

  it('胜者荣誉性展示（非强度）', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/arena/w1/report') {
        return { ...REPORT, match: { ...REPORT.match, phase: 'concluded', winnerCharId: 'cA' } };
      }
      if (path === '/api/worlds/w1') return WORLD;
      throw new Error(`unexpected ${path}`);
    });
    renderSpectate();
    expect(await screen.findByText('唯一胜者：沈霜')).toBeInTheDocument();
    expect(screen.getByText(/荣誉性奖励/)).toBeInTheDocument();
  });

  it('云端故障优雅降级：战报加载失败显示错误卡（不崩）', async () => {
    fetchMock.mockImplementation(async () => {
      throw new Error('network down');
    });
    renderSpectate();
    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
  });
});
