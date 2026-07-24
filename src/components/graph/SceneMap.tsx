// 场景地图（P2 #5）：把世界的「地点图 + 角色驻留」画成一张静态场景地图。
// - 地点节点：大小 ∝ connections 度数；秘境（isSecretRealm）以菱形 + 虚线环 + 🔒 标记区分。
// - 角色节点：小圆，吸附到其所在地点（我方角色 #d97757 描边环）。
// - 边：地点-地点 connections（实线）+ 角色-地点驻留（虚线）。
// - 布局：用 d3-force 预计算稳定坐标（地点 forceLink(connections)+forceManyBody+forceCollide；
//   角色 forceX/forceY 吸附所属地点），输出坐标喂 echarts graph layout:'none' 静态渲染——不每帧仿真，防抖动。
// - 交互：hover 地点高亮内部角色 + 连通地点（emphasis focus:'adjacency'）；click 地点 → 按 location 筛事件流。
// - 缺 locations（老服务端/空世界）优雅降级为空态「暂无地点数据」。
// 表现形式借鉴通用可视化范式（d3-force 预布局 + echarts graph），不复用任何 novel-fan-graph 代码。
// 防剧透靠服务端 principal 投影（events/mod.rs：秘境内位置仅角色主人可见、gate 细节不下发），前端不做人工遮罩。
// React 19 + echarts 6：组件内 ref 独占 init/dispose，getInstanceByDom 抗 StrictMode 双挂载；数据更新走增量 setOption。
import React, { useEffect, useMemo, useRef, useState } from 'react';
import * as echarts from 'echarts';
import {
  forceSimulation,
  forceLink,
  forceManyBody,
  forceCollide,
  forceX,
  forceY,
  forceCenter,
  type SimulationNodeDatum,
} from 'd3-force';
import { Card, Empty, List, Space, Tag, Typography } from 'antd';
import { EnvironmentOutlined, LockOutlined } from '@ant-design/icons';
import type { WorldLocation, WorldRosterEntry, WorldEventItem } from '../../stores/usePlatformStore';
import { MINE_RING_COLOR } from './model';
import { locationIconDataUri } from './glyphs';

const { Text, Paragraph } = Typography;

// ---------- 布局（纯计算，可测）：d3-force 预布局，输出稳定坐标 ----------

/** 一个已定坐标的场景节点。location 节点 id = 地点 id；character 节点 id = 角色 id（另带 locationId）。 */
export interface SceneNodePos {
  id: string;
  kind: 'location' | 'character';
  x: number;
  y: number;
  /** character 节点：所属地点 id。 */
  locationId?: string;
}

export interface SceneLayout {
  /** 键：location → 地点 id；character → `char:<角色 id>`（避免与地点 id 撞键）。 */
  positions: Record<string, SceneNodePos>;
  /** 地点-地点连边（无向去重）。 */
  locationLinks: Array<{ source: string; target: string }>;
  /** 角色-地点驻留边。 */
  residencyLinks: Array<{ source: string; target: string }>;
}

/** 确定性 PRNG（mulberry32）：喂给 d3-force randomSource，保证同输入同布局（可测、无抖动）。 */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface LocSimNode extends SimulationNodeDatum {
  id: string;
}
interface CharSimNode extends SimulationNodeDatum {
  id: string;
  charId: string;
  location: string;
}

/**
 * 两阶段 d3-force 预布局（纯函数，固定 tick，确定性）：
 * - 阶段一：地点按 connections 连边（forceLink）+ 斥力（forceManyBody）+ 碰撞（forceCollide）+ 居中收敛；
 *   连通地点被弹簧拉近、非连通地点被斥力推远，无连接的秘境自然漂成孤岛。
 * - 阶段二：角色以 forceX/forceY 吸附到所属地点的固定坐标（地点坐标此时已定），碰撞散开避免叠死。
 */
export function computeSceneLayout(
  input: { locations: WorldLocation[]; positions: Record<string, string> },
  opts?: { ticks?: number; seed?: number },
): SceneLayout {
  const ticks = opts?.ticks ?? 300;
  const rand = mulberry32(opts?.seed ?? 20240719);
  const locations = input.locations ?? [];
  const charPositions = input.positions ?? {};
  const locIds = new Set(locations.map((l) => l.id));

  // 地点-地点连边：无向去重，丢弃悬空目标与自环。
  const seen = new Set<string>();
  const locationLinks: Array<{ source: string; target: string }> = [];
  for (const l of locations) {
    for (const c of l.connections ?? []) {
      if (c === l.id || !locIds.has(c)) continue;
      const key = [l.id, c].sort().join('\u0000');
      if (seen.has(key)) continue;
      seen.add(key);
      locationLinks.push({ source: l.id, target: c });
    }
  }

  // ---- 阶段一：地点力布局 ----
  const locNodes: LocSimNode[] = locations.map((l) => ({ id: l.id }));
  if (locNodes.length > 0) {
    const sim = forceSimulation<LocSimNode>(locNodes)
      .randomSource(rand)
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      .force('link', forceLink<LocSimNode, any>(locationLinks.map((l) => ({ ...l })))
        .id((d) => d.id)
        .distance(160)
        .strength(0.6))
      .force('charge', forceManyBody<LocSimNode>().strength(-340))
      .force('collide', forceCollide<LocSimNode>(42))
      .force('center', forceCenter(0, 0))
      .stop();
    for (let i = 0; i < ticks; i += 1) sim.tick();
  }
  const locPos = new Map<string, { x: number; y: number }>();
  for (const n of locNodes) locPos.set(n.id, { x: n.x ?? 0, y: n.y ?? 0 });

  // ---- 阶段二：角色吸附到所属地点 ----
  const charNodes: CharSimNode[] = Object.entries(charPositions)
    .filter(([, lid]) => locIds.has(lid))
    .map(([cid, lid]) => {
      const base = locPos.get(lid) ?? { x: 0, y: 0 };
      // 初始置于地点附近（确定性抖动）利于快速收敛。
      return { id: cid, charId: cid, location: lid, x: base.x + (rand() - 0.5) * 24, y: base.y + (rand() - 0.5) * 24 };
    });
  const residencyLinks = charNodes.map((n) => ({ source: n.charId, target: n.location }));
  if (charNodes.length > 0) {
    const sim2 = forceSimulation<CharSimNode>(charNodes)
      .randomSource(rand)
      .force('x', forceX<CharSimNode>((d) => locPos.get(d.location)?.x ?? 0).strength(0.9))
      .force('y', forceY<CharSimNode>((d) => locPos.get(d.location)?.y ?? 0).strength(0.9))
      .force('collide', forceCollide<CharSimNode>(15))
      .force('charge', forceManyBody<CharSimNode>().strength(-24))
      .stop();
    for (let i = 0; i < ticks; i += 1) sim2.tick();
  }

  const out: Record<string, SceneNodePos> = {};
  for (const n of locNodes) out[n.id] = { id: n.id, kind: 'location', x: n.x ?? 0, y: n.y ?? 0 };
  for (const n of charNodes)
    out[`char:${n.charId}`] = { id: n.charId, kind: 'character', x: n.x ?? 0, y: n.y ?? 0, locationId: n.location };

  return { positions: out, locationLinks, residencyLinks };
}

// ---------- echarts 选项 ----------

interface SceneNodeMeta {
  kind: 'location' | 'character';
  id: string;
  locationId?: string;
}

const LOCATION_COLOR = '#8b7355';
const SECRET_COLOR = '#b58bbf';
const CHARACTER_COLOR = '#d9a441';

function locName(loc: WorldLocation | undefined, id: string): string {
  return loc?.name || id;
}

function buildSceneOption(
  layout: SceneLayout,
  locations: WorldLocation[],
  nameOf: Map<string, string>,
  mine: Set<string>,
): echarts.EChartsCoreOption {
  const locById = new Map(locations.map((l) => [l.id, l]));
  const degree = new Map<string, number>();
  for (const lk of layout.locationLinks) {
    degree.set(lk.source, (degree.get(lk.source) ?? 0) + 1);
    degree.set(lk.target, (degree.get(lk.target) ?? 0) + 1);
  }

  interface GraphDatum {
    name: string;
    x: number;
    y: number;
    symbol: string;
    symbolSize: number;
    itemStyle: Record<string, unknown>;
    label: Record<string, unknown>;
    __meta: SceneNodeMeta;
  }
  const data: GraphDatum[] = [];
  for (const key of Object.keys(layout.positions)) {
    const p = layout.positions[key];
    if (p.kind === 'location') {
      const loc = locById.get(p.id);
      const secret = !!loc?.isSecretRealm;
      const d = degree.get(p.id) ?? 0;
      data.push({
        name: `L:${p.id}`,
        x: p.x,
        y: p.y,
        symbol: locationIconDataUri({ name: locName(loc, p.id), secret }),
        symbolSize: Math.min(58, 40 + d * 5),
        itemStyle: {},
        label: {
          show: true,
          position: 'bottom',
          color: '#33312e',
          fontSize: 12,
          formatter: locName(loc, p.id),
        },
        __meta: { kind: 'location', id: p.id },
      });
    } else {
      const isMine = mine.has(p.id);
      data.push({
        name: `C:${p.id}`,
        x: p.x,
        y: p.y,
        symbol: 'circle',
        symbolSize: 12,
        itemStyle: {
          color: CHARACTER_COLOR,
          borderColor: isMine ? MINE_RING_COLOR : '#fffdfa',
          borderWidth: isMine ? 3 : 1,
        },
        label: { show: false },
        __meta: { kind: 'character', id: p.id, locationId: p.locationId },
      });
    }
  }

  const links = [
    ...layout.locationLinks.map((l) => ({
      source: `L:${l.source}`,
      target: `L:${l.target}`,
      lineStyle: { color: '#cbb7a3', width: 2, type: 'solid', curveness: 0 },
    })),
    ...layout.residencyLinks.map((l) => ({
      source: `C:${l.source}`,
      target: `L:${l.target}`,
      lineStyle: { color: '#d8c9b3', width: 1, type: 'dashed', opacity: 0.75, curveness: 0 },
    })),
  ];

  return {
    tooltip: {
      formatter: (p: { dataType?: string; data?: unknown }) => {
        if (p.dataType !== 'node') return '';
        const meta = (p.data as GraphDatum | undefined)?.__meta;
        if (!meta) return '';
        if (meta.kind === 'location') {
          const loc = locById.get(meta.id);
          return `${loc?.isSecretRealm ? '🔒 秘境 · ' : '地点 · '}${locName(loc, meta.id)}`;
        }
        const at = meta.locationId ? locName(locById.get(meta.locationId), meta.locationId) : '';
        return `${nameOf.get(meta.id) || meta.id}${at ? `<br/>所在：${at}` : ''}`;
      },
    },
    series: [
      {
        type: 'graph',
        layout: 'none',
        roam: true,
        draggable: true,
        // hover 地点 → 高亮其邻接（内部角色经驻留边、连通地点经 connections 边）。
        emphasis: { focus: 'adjacency', label: { show: true }, lineStyle: { width: 3.5 } },
        blur: { itemStyle: { opacity: 0.2 }, lineStyle: { opacity: 0.08 } },
        lineStyle: { color: '#cbb7a3', curveness: 0 },
        data,
        links,
      },
    ],
  };
}

// ---------- 组件 ----------

export interface SceneMapProps {
  /** 地点图（public 投影）。缺省/空 → 优雅降级空态。 */
  locations?: WorldLocation[];
  /** 角色当前位置 {characterId: locationId}（principal 过滤后）。 */
  positions?: Record<string, string>;
  roster: WorldRosterEntry[];
  /** 事件流：click 地点后按该地点在场角色筛出的事件在下方列出。 */
  events?: WorldEventItem[];
  myIds?: Set<string>;
  height?: number;
  testId?: string;
}

const SceneMap: React.FC<SceneMapProps> = ({
  locations,
  positions,
  roster,
  events = [],
  myIds,
  height = 420,
  testId = 'scene-map',
}) => {
  const mine = myIds ?? new Set<string>();
  const locs = locations ?? [];
  const pos = positions ?? {};
  const hasLocations = locs.length > 0;

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of roster) m.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    return m;
  }, [roster]);

  const layout = useMemo(
    () => (hasLocations ? computeSceneLayout({ locations: locs, positions: pos }) : null),
    // 依赖以 JSON 指纹稳定，避免 props 引用变动导致每次重算布局。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [hasLocations, JSON.stringify(locs), JSON.stringify(pos)],
  );

  const option = useMemo(
    () => (layout ? buildSceneOption(layout, locs, nameOf, mine) : null),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [layout, nameOf, mine],
  );

  const [selectedLoc, setSelectedLoc] = useState<string | null>(null);

  // 选中地点 → 该地点在场角色集合 → 筛出这些角色参与的事件（按 sequence 逆序，最近在前）。
  const filteredEvents = useMemo(() => {
    if (!selectedLoc) return [];
    const here = new Set(
      Object.entries(pos)
        .filter(([, lid]) => lid === selectedLoc)
        .map(([cid]) => cid),
    );
    if (here.size === 0) return [];
    return events
      .filter((ev) => ev.actors.some((a) => here.has(a)))
      .slice()
      .sort((a, b) => b.sequence - a.sequence);
  }, [selectedLoc, pos, events]);

  // ---- echarts 受控生命周期 ----
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const initializedRef = useRef(false);
  const selectRef = useRef(setSelectedLoc);
  selectRef.current = setSelectedLoc;

  useEffect(() => {
    const el = containerRef.current;
    if (!el || !option) return;

    let chart = echarts.getInstanceByDom(el);
    if (!chart) chart = echarts.init(el);
    chartRef.current = chart;
    chart.setOption(option, { notMerge: true });
    initializedRef.current = true;

    const onClick = (params: echarts.ECElementEvent) => {
      if (params.dataType !== 'node') return;
      const meta = (params.data as { __meta?: SceneNodeMeta } | undefined)?.__meta;
      if (meta?.kind === 'location') selectRef.current((cur) => (cur === meta.id ? null : meta.id));
    };
    chart.on('click', onClick);

    const ro = new ResizeObserver(() => chart?.resize());
    ro.observe(el);

    return () => {
      ro.disconnect();
      chart?.off('click', onClick);
      chart?.dispose();
      chartRef.current = null;
      initializedRef.current = false;
    };
    // 仅在有无 option（挂载）时建图；option 内容变更由下方增量 effect 处理。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [!!option]);

  useEffect(() => {
    if (!initializedRef.current || !option) return;
    chartRef.current?.setOption(option, { notMerge: false });
  }, [option]);

  if (!hasLocations) {
    return <Empty description="暂无地点数据" image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }

  const selectedLocObj = locs.find((l) => l.id === selectedLoc);

  return (
    <div>
      <Space direction="vertical" size={12} style={{ width: '100%' }}>
        <div
          ref={containerRef}
          data-testid={testId}
          style={{ width: '100%', height: typeof height === 'number' ? `${height}px` : height }}
        />
        <Space size={16} wrap>
          <Text type="secondary" style={{ fontSize: 12 }}>
            <EnvironmentOutlined style={{ color: LOCATION_COLOR }} /> 地点（大小∝连通度）
          </Text>
          <Text type="secondary" style={{ fontSize: 12 }}>
            <LockOutlined style={{ color: SECRET_COLOR }} /> 秘境（菱形·虚线环）
          </Text>
          <Text type="secondary" style={{ fontSize: 12 }}>
            <span style={{ color: CHARACTER_COLOR }}>●</span> 角色（吸附所在地点，
            <span style={{ color: MINE_RING_COLOR }}>◎</span> 为我的角色）
          </Text>
          <Text type="secondary" style={{ fontSize: 12 }}>
            点击地点可筛出该地点在场角色的事件。
          </Text>
        </Space>
        {selectedLoc && (
          <Card
            size="small"
            style={{ borderRadius: 10, border: '1px solid #eae6df', background: '#fffdfa' }}
            title={
              <Space size={8} wrap>
                <Text strong>{locName(selectedLocObj, selectedLoc)}</Text>
                {selectedLocObj?.isSecretRealm && <Tag color="purple">秘境</Tag>}
                <Text type="secondary" style={{ fontSize: 12 }}>
                  {filteredEvents.length} 条相关事件
                </Text>
              </Space>
            }
          >
            {filteredEvents.length === 0 ? (
              <Text type="secondary" style={{ fontSize: 12 }}>
                该地点暂无在场角色的可见事件
              </Text>
            ) : (
              <List
                size="small"
                dataSource={filteredEvents.slice(0, 20)}
                renderItem={(ev) => (
                  <List.Item style={{ paddingLeft: 0, paddingRight: 0 }}>
                    <Space direction="vertical" size={2} style={{ width: '100%' }}>
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        第 {ev.tick} 拍 · {ev.actors.map((a) => nameOf.get(a) || a).join('、')}
                      </Text>
                      <Paragraph style={{ margin: 0, color: '#33312e', fontSize: 13 }}>
                        {ev.projection?.summary || ev.projection?.narrative || '（无摘要）'}
                      </Paragraph>
                    </Space>
                  </List.Item>
                )}
              />
            )}
          </Card>
        )}
      </Space>
    </div>
  );
};

export default SceneMap;
