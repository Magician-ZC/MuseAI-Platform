// 回归测试: useWorldStateSummary hook 曾只取 relations/characters, 漏传 locations/positions,
// 导致场景地图(SceneMap)拿不到地点数据、恒显示空态。组件层单元测试测不到(直接传 props),
// 只有 hook→组件链路的集成才暴露 —— 本测试锁定 hook 把四个字段都带进 summary。
import { describe, expect, it, vi, type Mock } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';

vi.mock('../utils/cloudApi', () => ({
  cloudFetch: vi.fn(),
  cloudStream: vi.fn(() => () => {}),
  getPlatformBase: vi.fn(() => 'http://test'),
  setPlatformBase: vi.fn(),
  CloudError: class extends Error {},
}));

// 导入 useWorldStateSummary 会加载整个 WorldRoom 模块(含 SceneMap→echarts)；jsdom 无 canvas，替身化。
vi.mock('echarts', () => ({
  init: vi.fn(() => ({ setOption: vi.fn(), resize: vi.fn(), dispose: vi.fn(), on: vi.fn(), off: vi.fn(), dispatchAction: vi.fn() })),
  getInstanceByDom: vi.fn(() => undefined),
}));

import { cloudFetch } from '../utils/cloudApi';
import { useWorldStateSummary } from '../pages/platform/WorldRoom';

const fetchMock = cloudFetch as unknown as Mock;

describe('useWorldStateSummary', () => {
  it('把响应的 locations/positions 带进 summary（回归：曾只取 relations/characters 致场景地图空态）', async () => {
    fetchMock.mockImplementation((path: string) => {
      if (path === '/api/worlds/w1/state-summary')
        return Promise.resolve({
          characters: [{ id: 'c1', arcStage: '', activity: 0 }],
          relations: [],
          locations: [{ id: 'loc_hall', name: '正厅', connections: [], isSecretRealm: false }],
          positions: { c1: 'loc_hall' },
        });
      return Promise.reject(new Error('unexpected ' + path));
    });

    const { result } = renderHook(() => useWorldStateSummary('w1'));
    await waitFor(() => expect(result.current.summary).not.toBeNull());

    expect(result.current.summary?.locations).toHaveLength(1);
    expect(result.current.summary?.locations?.[0]).toMatchObject({ id: 'loc_hall', name: '正厅' });
    expect(result.current.summary?.positions).toEqual({ c1: 'loc_hall' });
    expect(result.current.summary?.characters).toHaveLength(1);
  });

  it('缺 locations/positions(老服务端)时优雅降级为空数组/空对象，不为 undefined', async () => {
    fetchMock.mockImplementation((path: string) => {
      if (path === '/api/worlds/w1/state-summary')
        return Promise.resolve({ characters: [], relations: [] }); // 老响应无这两字段
      return Promise.reject(new Error('unexpected ' + path));
    });

    const { result } = renderHook(() => useWorldStateSummary('w1'));
    await waitFor(() => expect(result.current.summary).not.toBeNull());

    expect(result.current.summary?.locations).toEqual([]);
    expect(result.current.summary?.positions).toEqual({});
  });
});
