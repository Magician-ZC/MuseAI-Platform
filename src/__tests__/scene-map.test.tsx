import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';

// echarts 在 jsdom 无 canvas，替身化 init/dispose（SceneMap 用 raw echarts + ref）。
vi.mock('echarts', () => {
  const chart = {
    setOption: vi.fn(),
    resize: vi.fn(),
    dispose: vi.fn(),
    on: vi.fn(),
    off: vi.fn(),
    dispatchAction: vi.fn(),
  };
  return {
    init: vi.fn(() => chart),
    getInstanceByDom: vi.fn(() => undefined),
  };
});

import SceneMap, { computeSceneLayout } from '../components/graph/SceneMap';
import type { WorldLocation } from '../stores/usePlatformStore';

function dist(
  layout: ReturnType<typeof computeSceneLayout>,
  a: string,
  b: string,
): number {
  const pa = layout.positions[a];
  const pb = layout.positions[b];
  return Math.hypot(pa.x - pb.x, pa.y - pb.y);
}

describe('computeSceneLayout（d3-force 预布局，纯计算）', () => {
  // A-B-C 链 + 孤立秘境 S（无 connections）。
  const locations: WorldLocation[] = [
    { id: 'A', name: '前厅', connections: ['B'], isSecretRealm: false },
    { id: 'B', name: '回廊', connections: ['A', 'C'], isSecretRealm: false },
    { id: 'C', name: '内院', connections: ['B'], isSecretRealm: false },
    { id: 'S', name: '密室', connections: [], isSecretRealm: true },
  ];
  const positions = { c1: 'A', c2: 'A', c3: 'C', cS: 'S' };

  it('连通地点距离 < 非连通地点距离（弹簧拉近相邻、斥力推远间接）', () => {
    const layout = computeSceneLayout({ locations, positions }, { ticks: 400, seed: 1 });
    // A-B、B-C 直接连通；A-C 仅经 B 间接 → dist(A,C) 应更大。
    expect(dist(layout, 'A', 'B')).toBeLessThan(dist(layout, 'A', 'C'));
    expect(dist(layout, 'B', 'C')).toBeLessThan(dist(layout, 'A', 'C'));
  });

  it('无连接的秘境成孤岛（离主簇远于任一连通对）', () => {
    const layout = computeSceneLayout({ locations, positions }, { ticks: 400, seed: 1 });
    const ab = dist(layout, 'A', 'B');
    // S 无连边，仅受斥力 → 离连通簇任一节点都远于最近的连通对距离。
    expect(dist(layout, 'S', 'A')).toBeGreaterThan(ab);
    expect(dist(layout, 'S', 'B')).toBeGreaterThan(ab);
    expect(dist(layout, 'S', 'C')).toBeGreaterThan(ab);
  });

  it('确定性：同输入同种子产出同布局', () => {
    const l1 = computeSceneLayout({ locations, positions }, { ticks: 200, seed: 7 });
    const l2 = computeSceneLayout({ locations, positions }, { ticks: 200, seed: 7 });
    expect(l1.positions.A).toEqual(l2.positions.A);
    expect(l1.positions['char:c1']).toEqual(l2.positions['char:c1']);
  });

  it('角色节点吸附所属地点（落点靠近地点而非其它地点）', () => {
    const layout = computeSceneLayout({ locations, positions }, { ticks: 400, seed: 1 });
    // c1 驻留 A：到 A 的距离应小于到 C 的距离。
    expect(dist(layout, 'char:c1', 'A')).toBeLessThan(dist(layout, 'char:c1', 'C'));
    // 悬空位置（地点不存在）不产生角色节点。
    const dangling = computeSceneLayout(
      { locations, positions: { cx: 'NONEXIST' } },
      { ticks: 50, seed: 1 },
    );
    expect(dangling.positions['char:cx']).toBeUndefined();
  });
});

describe('SceneMap 渲染', () => {
  it('缺 locations 时优雅降级为空态，不崩溃', () => {
    render(<SceneMap roster={[]} />);
    expect(screen.getByText('暂无地点数据')).toBeInTheDocument();
    // 空态不应渲染图容器。
    expect(screen.queryByTestId('scene-map')).toBeNull();
  });

  it('有 locations 时渲染图容器', () => {
    render(
      <SceneMap
        roster={[{ cloudCharacterId: 'c1', name: '沈霜' }]}
        locations={[{ id: 'A', name: '前厅', connections: [], isSecretRealm: false }]}
        positions={{ c1: 'A' }}
      />,
    );
    expect(screen.getByTestId('scene-map')).toBeInTheDocument();
  });
});
