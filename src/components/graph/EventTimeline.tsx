// 图谱化时间轴（P1 #2）：把 antd Timeline 升级为二维事件图谱。
// - 横轴 = tick（拍），纵轴 = 角色泳道；每个事件是所属角色泳道上、对应 tick 的一个节点。
// - 同一 tick 多名 actor 参与的事件 → 纵向连线把这些泳道点连起来（对抗/结盟一眼可见）。
// - 虚拟时钟播放器（播放/暂停/倍速/拖拽，迁自 ArenaReplay 模式）：revealUpToTick 游标只点亮 ≤ 游标的事件。
// - 点击事件节点 → 右侧事件详情卡。
// 表现形式借鉴通用可视化范式（echarts scatter + lines，cartesian2d 手动定坐标），不复用任何 novel-fan-graph 代码。
// React 19 + echarts 6：组件内 ref 独占 init/dispose，getInstanceByDom 抗 StrictMode 双挂载；数据更新走增量 setOption。
import React, { useEffect, useMemo, useRef, useState } from 'react';
import * as echarts from 'echarts';
import { Card, Empty, Segmented, Slider, Space, Tag, Button, Typography } from 'antd';
import {
  PlayCircleOutlined,
  PauseCircleOutlined,
  StepBackwardOutlined,
  TeamOutlined,
} from '@ant-design/icons';
import type { WorldEventItem, WorldRosterEntry } from '../../stores/usePlatformStore';
import { MINE_RING_COLOR } from './model';

const { Text, Paragraph } = Typography;

// ---------- 通用时间轴事件（对外契约；WorldEventItem / ArenaReplayEvent 皆可映射） ----------

export interface TimelineEvent {
  id: string;
  sequence: number;
  tick: number;
  /** 事件类型（action/dialogue/conflict/alliance/status/arena_elim/...）。决定节点配色。 */
  type: string;
  actors: string[];
  summary?: string;
  /** 非 'public' 时标「仅你可见」；观众投影下服务端已过滤，前端不做人工遮罩。 */
  visibility?: string;
}

/** 把 WorldEventItem[] 映射成 TimelineEvent[]（summary 取 projection.summary/narrative）。 */
export function toTimelineEvents(events: WorldEventItem[]): TimelineEvent[] {
  return events.map((ev) => ({
    id: ev.id,
    sequence: ev.sequence,
    tick: ev.tick,
    type: ev.type,
    actors: ev.actors,
    summary: ev.projection?.summary || ev.projection?.narrative || '',
    visibility: ev.visibility,
  }));
}

// ---------- 游标：只保留 ≤ revealUpToTick 的事件（纯函数，可测） ----------

/** 过滤出 tick ≤ 游标的事件并按 sequence 升序（一生按发生顺序读）。 */
export function revealEventsUpTo<T extends { tick: number; sequence: number }>(
  events: T[],
  uptoTick: number,
): T[] {
  return events
    .filter((e) => e.tick <= uptoTick)
    .slice()
    .sort((a, b) => a.sequence - b.sequence);
}

// ---------- 事件类型配色（暖调，与主题协调；对抗=红、结盟=绿、淘汰=深红、胜者=金） ----------

const EVENT_TYPE_COLOR: Record<string, string> = {
  action: '#8b7355',
  dialogue: '#6b8fae',
  conflict: '#c15b5b',
  alliance: '#5b9a6f',
  item: '#c9a15a',
  status: '#a89b8c',
  arbiter: '#b07aa1',
  world: '#7a9a9a',
  arena_elim: '#a4322f',
  arena_winner: '#d4a017',
  arena_gift: '#b58bbf',
};

const EVENT_TYPE_FALLBACK_COLOR = '#8b7355';

export function eventTimelineColor(type: string): string {
  return EVENT_TYPE_COLOR[type] ?? EVENT_TYPE_FALLBACK_COLOR;
}

function eventTypeLabel(type: string): string {
  switch (type) {
    case 'action':
      return '行动';
    case 'dialogue':
      return '对话';
    case 'conflict':
      return '冲突';
    case 'alliance':
      return '结盟';
    case 'item':
      return '道具';
    case 'status':
      return '状态';
    case 'arbiter':
      return '仲裁';
    case 'world':
      return '世界';
    case 'arena_elim':
      return '淘汰';
    case 'arena_winner':
      return '胜者';
    case 'arena_gift':
      return '打赏';
    default:
      return type;
  }
}

// ---------- echarts 选项 ----------

interface ScatterDatum {
  value: [number, number];
  itemStyle: { color: string; borderColor?: string; borderWidth?: number };
  symbolSize: number;
  __event: TimelineEvent;
}

function buildOption(
  revealed: TimelineEvent[],
  laneIds: string[],
  laneNames: string[],
  laneMine: boolean[],
  maxTick: number,
): echarts.EChartsCoreOption {
  const laneIndex = new Map<string, number>();
  laneIds.forEach((id, i) => laneIndex.set(id, i));

  const points: ScatterDatum[] = [];
  const connectors: Array<{ coords: [[number, number], [number, number]]; lineStyle: { color: string } }> = [];

  for (const ev of revealed) {
    const color = eventTimelineColor(ev.type);
    const ys: number[] = [];
    for (const actor of ev.actors) {
      const y = laneIndex.get(actor);
      if (y === undefined) continue;
      ys.push(y);
      points.push({
        value: [ev.tick, y],
        symbolSize: 14,
        itemStyle: {
          color,
          borderColor: laneMine[y] ? MINE_RING_COLOR : undefined,
          borderWidth: laneMine[y] ? 2.5 : 0,
        },
        __event: ev,
      });
    }
    // 同一 tick 多名 actor：把泳道点两两纵向连起来（min↔max 已足够勾勒对抗/结盟）。
    if (ys.length >= 2) {
      const lo = Math.min(...ys);
      const hi = Math.max(...ys);
      connectors.push({ coords: [[ev.tick, lo], [ev.tick, hi]], lineStyle: { color } });
    }
  }

  return {
    tooltip: {
      trigger: 'item',
      formatter: (p: { data?: unknown }) => {
        const ev = (p.data as ScatterDatum | undefined)?.__event;
        if (!ev) return '';
        const bits = [`第 ${ev.tick} 拍 · ${eventTypeLabel(ev.type)}`];
        if (ev.summary) bits.push(ev.summary);
        return bits.join('<br/>');
      },
    },
    grid: { left: 90, right: 24, top: 16, bottom: 40 },
    xAxis: {
      type: 'value',
      name: '拍',
      min: 0,
      max: Math.max(1, maxTick) + 0.5,
      minInterval: 1,
      axisLabel: { color: '#8c857b' },
      splitLine: { lineStyle: { color: '#f0ece5' } },
    },
    yAxis: {
      type: 'value',
      min: -0.5,
      max: Math.max(0.5, laneIds.length - 0.5),
      interval: 1,
      axisTick: { show: false },
      axisLine: { show: false },
      axisLabel: {
        color: '#33312e',
        formatter: (v: number) => laneNames[v] ?? '',
      },
      splitLine: { lineStyle: { color: '#f5f2ec' } },
    },
    series: [
      {
        type: 'lines',
        coordinateSystem: 'cartesian2d',
        silent: true,
        data: connectors,
        lineStyle: { width: 2, opacity: 0.55, curveness: 0 },
        z: 1,
      },
      {
        type: 'scatter',
        coordinateSystem: 'cartesian2d',
        data: points,
        z: 2,
        emphasis: { scale: 1.4 },
      },
    ],
  };
}

// ---------- 播放器 + 图谱容器 ----------

export interface EventTimelineProps {
  events: TimelineEvent[];
  roster: WorldRosterEntry[];
  myIds?: Set<string>;
  height?: number;
  testId?: string;
}

const SPEED_OPTIONS = [
  { label: '0.5×', value: 0.5 },
  { label: '1×', value: 1 },
  { label: '2×', value: 2 },
  { label: '4×', value: 4 },
];

const EventTimeline: React.FC<EventTimelineProps> = ({
  events,
  roster,
  myIds,
  height = 360,
  testId = 'event-timeline',
}) => {
  const mine = myIds ?? new Set<string>();

  // 泳道：阵容顺序 ∪ 事件中出现的其他 actor（保证事件都有落点）。
  const { laneIds, laneNames, laneMine } = useMemo(() => {
    const ids: string[] = [];
    const names: string[] = [];
    const mineFlags: boolean[] = [];
    const seen = new Set<string>();
    const nameOf = new Map<string, string>();
    for (const r of roster) nameOf.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    const push = (id: string) => {
      if (seen.has(id)) return;
      seen.add(id);
      ids.push(id);
      names.push(nameOf.get(id) || id);
      mineFlags.push(mine.has(id));
    };
    for (const r of roster) push(r.cloudCharacterId);
    for (const ev of events) for (const a of ev.actors) push(a);
    return { laneIds: ids, laneNames: names, laneMine: mineFlags };
  }, [roster, events, mine]);

  const maxTick = useMemo(() => events.reduce((m, e) => Math.max(m, e.tick), 0), [events]);

  // 虚拟时钟：cursor = 已揭示到的 tick 上界（0..maxTick）；播放按倍速逐拍推进。
  const [cursor, setCursor] = useState(maxTick);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  const [selected, setSelected] = useState<TimelineEvent | null>(null);
  // 跟随实时：cursor 停在末尾时，新事件推高 maxTick 自动跟进；用户回拖后停止跟随。
  const followingRef = useRef(true);
  const prevMaxRef = useRef(maxTick);

  useEffect(() => {
    if (maxTick !== prevMaxRef.current) {
      if (followingRef.current && !playing) setCursor(maxTick);
      prevMaxRef.current = maxTick;
    }
  }, [maxTick, playing]);

  // 播放循环：逐拍推进；到末尾自动停。
  useEffect(() => {
    if (!playing) return;
    const timer = setInterval(() => {
      setCursor((c) => (c >= maxTick ? c : c + 1));
    }, Math.max(200, 900 / speed));
    return () => clearInterval(timer);
  }, [playing, speed, maxTick]);

  useEffect(() => {
    if (playing && cursor >= maxTick) setPlaying(false);
  }, [playing, cursor, maxTick]);

  const revealed = useMemo(() => revealEventsUpTo(events, cursor), [events, cursor]);

  const option = useMemo(
    () => buildOption(revealed, laneIds, laneNames, laneMine, maxTick),
    [revealed, laneIds, laneNames, laneMine, maxTick],
  );

  // ---- echarts 受控生命周期 ----
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const initializedRef = useRef(false);
  const selectRef = useRef(setSelected);
  selectRef.current = setSelected;

  const hasLanes = laneIds.length > 0;

  useEffect(() => {
    const el = containerRef.current;
    if (!el || !hasLanes) return;

    let chart = echarts.getInstanceByDom(el);
    if (!chart) chart = echarts.init(el);
    chartRef.current = chart;
    chart.setOption(option, { notMerge: true });
    initializedRef.current = true;

    const onClick = (params: echarts.ECElementEvent) => {
      const ev = (params.data as ScatterDatum | undefined)?.__event;
      if (ev) selectRef.current(ev);
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
    // 仅挂载时建图；option 变更由下方增量 effect 处理。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasLanes]);

  useEffect(() => {
    if (!initializedRef.current) return;
    chartRef.current?.setOption(option, { notMerge: false });
  }, [option]);

  if (!hasLanes) {
    return <Empty description="暂无角色，无法绘制时间线" image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }

  const togglePlay = () => {
    if (maxTick === 0) return;
    if (!playing && cursor >= maxTick) {
      setCursor(0);
      followingRef.current = true;
    }
    setPlaying((p) => !p);
  };

  return (
    <div>
      <Space direction="vertical" size={12} style={{ width: '100%' }}>
        <Space size={12} wrap>
          <Button
            type="primary"
            size="small"
            icon={playing ? <PauseCircleOutlined /> : <PlayCircleOutlined />}
            onClick={togglePlay}
            disabled={maxTick === 0}
          >
            {playing ? '暂停' : cursor >= maxTick ? '重播' : '播放'}
          </Button>
          <Button
            size="small"
            icon={<StepBackwardOutlined />}
            disabled={maxTick === 0}
            onClick={() => {
              setPlaying(false);
              followingRef.current = false;
              setCursor(0);
            }}
          >
            回到开头
          </Button>
          <Segmented
            size="small"
            options={SPEED_OPTIONS}
            value={speed}
            onChange={(v) => setSpeed(v as number)}
            aria-label="播放倍速"
          />
          <Text type="secondary" style={{ fontSize: 12 }}>
            第 {cursor} / {maxTick} 拍 · 已点亮 {revealed.length} 事件
          </Text>
        </Space>
        <Slider
          min={0}
          max={Math.max(1, maxTick)}
          value={cursor}
          disabled={maxTick === 0}
          onChange={(v) => {
            setPlaying(false);
            followingRef.current = v >= maxTick;
            setCursor(v);
          }}
          tooltip={{ formatter: (v) => `第 ${v} 拍` }}
        />
        <div
          ref={containerRef}
          data-testid={testId}
          style={{ width: '100%', height: typeof height === 'number' ? `${height}px` : height }}
        />
        {selected && (
          <Card size="small" style={{ borderRadius: 10, border: '1px solid #eae6df', background: '#fffdfa' }}>
            <Space direction="vertical" size={6} style={{ width: '100%' }}>
              <Space size={8} wrap>
                <Tag color="geekblue" style={{ borderColor: eventTimelineColor(selected.type) }}>
                  {eventTypeLabel(selected.type)}
                </Tag>
                <Text type="secondary" style={{ fontSize: 12 }}>
                  第 {selected.tick} 拍 · #{selected.sequence}
                </Text>
                {selected.visibility && selected.visibility !== 'public' && <Tag color="purple">仅你可见</Tag>}
              </Space>
              {selected.summary && (
                <Paragraph style={{ margin: 0, color: '#33312e' }}>{selected.summary}</Paragraph>
              )}
              {selected.actors.length > 0 && (
                <Text type="secondary" style={{ fontSize: 12 }}>
                  <TeamOutlined />{' '}
                  {selected.actors.map((a) => laneNames[laneIds.indexOf(a)] || a).join('、')}
                </Text>
              )}
            </Space>
          </Card>
        )}
        <Text type="secondary" style={{ fontSize: 12 }}>
          横轴为拍序、每行是一名角色的泳道；同一拍多名角色的事件以纵向连线相连（<span style={{ color: EVENT_TYPE_COLOR.conflict }}>红=冲突</span> ·{' '}
          <span style={{ color: EVENT_TYPE_COLOR.alliance }}>绿=结盟</span>）。
          <span style={{ color: MINE_RING_COLOR }}> ◎ </span>为我的角色（描边环）。拖动游标或播放以逐拍点亮。
        </Text>
      </Space>
    </div>
  );
};

export default EventTimeline;
