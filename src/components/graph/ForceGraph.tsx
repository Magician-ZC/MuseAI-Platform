// 通用力导向图受控封装（P0）：echarts `graph` + force 布局。
// React 19 + echarts 6：组件内用 ref 管 init/dispose，StrictMode 双挂载不重复 init（getInstanceByDom 复用）。
// 数据更新走增量 setOption(notMerge:false)，保留 force 布局已固定的节点坐标，避免每拍炸开重排。
// 交互：roam（缩放/平移）、draggable、tooltip、emphasis focus:'adjacency'（hover 高亮节点+邻居、淡化其余）。
import React, { useEffect, useMemo, useRef } from 'react';
import * as echarts from 'echarts';
import { Empty } from 'antd';
import type { GraphNode, GraphLink, GraphCategory } from './model';
import { MINE_RING_COLOR } from './model';

export interface ForceGraphProps {
  nodes: GraphNode[];
  links: GraphLink[];
  categories?: GraphCategory[];
  /** 点击节点回调（传回原始 GraphNode）。 */
  onNodeClick?: (node: GraphNode) => void;
  /** hover 高亮节点 + 直接邻居边、淡化其余（echarts focus:'adjacency'）。默认 true。 */
  highlightNeighbors?: boolean;
  /** hover 时高亮同 category 的全部节点（势力图用）。与 highlightNeighbors 二选一语义。 */
  highlightCategory?: boolean;
  /** 展示图例（category 名称，点击可隔离单类）。默认按 categories 是否存在。 */
  legend?: boolean;
  /** 画布高度。默认 380。 */
  height?: number | string;
  /** 力导向斥力（默认 180）。 */
  repulsion?: number;
  /** 力导向边长（默认 110）。 */
  edgeLength?: number;
  /** 供测试定位；默认 'echarts-graph'。 */
  testId?: string;
  /** 空数据文案。 */
  emptyText?: string;
}

interface EchartsNodeDatum {
  id: string;
  name: string;
  value: number;
  symbolSize: number;
  category?: number;
  itemStyle: { color: string; borderColor?: string; borderWidth?: number };
  __node: GraphNode;
}

function buildOption(
  nodes: GraphNode[],
  links: GraphLink[],
  categories: GraphCategory[],
  opts: { highlightNeighbors: boolean; legend: boolean; repulsion: number; edgeLength: number },
): echarts.EChartsCoreOption {
  const maxWeight = Math.max(1, ...links.map((l) => Math.abs(l.weight)));
  const data: EchartsNodeDatum[] = nodes.map((n) => ({
    id: n.id,
    name: n.label,
    value: n.size,
    symbolSize: n.size,
    category: n.category,
    itemStyle: {
      color: n.color,
      borderColor: n.mine ? MINE_RING_COLOR : undefined,
      borderWidth: n.mine ? 3 : 0,
    },
    __node: n,
  }));

  const edgeData = links.map((l) => ({
    source: l.source,
    target: l.target,
    value: l.weight,
    lineStyle: {
      color: l.color ?? '#cbb7a3',
      width: l.width ?? 1 + (Math.abs(l.weight) / maxWeight) * 5,
      type: l.dashed ? 'dashed' : 'solid',
      curveness: 0.08,
    },
  }));

  return {
    tooltip: {
      formatter: (p: { dataType?: string; data?: unknown }) => {
        if (p.dataType === 'edge') {
          const e = p.data as { source: string; target: string; value: number };
          return `${e.source} → ${e.target}<br/>强度 ${Math.round(Math.abs(e.value) * 100) / 100}`;
        }
        const n = (p.data as EchartsNodeDatum | undefined)?.__node;
        if (!n) return '';
        const bits = [n.label];
        if (typeof n.activity === 'number') bits.push(`活跃度 ${n.activity}`);
        return bits.join('<br/>');
      },
    },
    legend: opts.legend && categories.length > 0
      ? [{ data: categories.map((c) => c.name), bottom: 0, textStyle: { color: '#8c857b' } }]
      : undefined,
    series: [
      {
        type: 'graph',
        layout: 'force',
        roam: true,
        draggable: true,
        force: { repulsion: opts.repulsion, edgeLength: opts.edgeLength, gravity: 0.06 },
        categories: categories.length > 0 ? categories : undefined,
        label: {
          show: true,
          position: 'right',
          color: '#33312e',
          formatter: (p: { data?: { name?: string } }) => p.data?.name ?? '',
        },
        lineStyle: { color: '#cbb7a3', curveness: 0.08 },
        emphasis: opts.highlightNeighbors
          ? { focus: 'adjacency', label: { show: true }, lineStyle: { width: 4 } }
          : { focus: 'none', label: { show: true } },
        blur: { itemStyle: { opacity: 0.18 }, lineStyle: { opacity: 0.08 } },
        data,
        links: edgeData,
      },
    ],
  };
}

/**
 * 受控力导向图。首拍 setOption(notMerge:true) 建图，后续数据变更 setOption(notMerge:false) 增量合并，
 * 保留 force 已收敛的节点坐标。init/dispose 由挂载 effect 独占，回调用 ref 取最新值避免重复 init。
 */
const ForceGraph: React.FC<ForceGraphProps> = ({
  nodes,
  links,
  categories = [],
  onNodeClick,
  highlightNeighbors = true,
  highlightCategory = false,
  legend,
  height = 380,
  repulsion = 180,
  edgeLength = 110,
  testId = 'echarts-graph',
  emptyText = '暂无数据',
}) => {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const initializedRef = useRef(false);

  const showLegend = legend ?? categories.length > 0;

  const option = useMemo(
    () =>
      buildOption(nodes, links, categories, {
        highlightNeighbors,
        legend: showLegend,
        repulsion,
        edgeLength,
      }),
    [nodes, links, categories, highlightNeighbors, showLegend, repulsion, edgeLength],
  );

  // 回调 ref：init effect 只跑一次，事件里始终读到最新 onNodeClick / highlightCategory / nodes。
  const clickRef = useRef(onNodeClick);
  clickRef.current = onNodeClick;
  const hlCategoryRef = useRef(highlightCategory);
  hlCategoryRef.current = highlightCategory;
  const nodesRef = useRef(nodes);
  nodesRef.current = nodes;

  const hasData = nodes.length > 0;

  // 挂载/卸载：init 一次（复用已存在实例以抗 StrictMode 双挂载），dispose 收尾。
  useEffect(() => {
    const el = containerRef.current;
    if (!el || !hasData) return;

    let chart = echarts.getInstanceByDom(el);
    if (!chart) chart = echarts.init(el);
    chartRef.current = chart;
    chart.setOption(option, { notMerge: true });
    initializedRef.current = true;

    const onClick = (params: echarts.ECElementEvent) => {
      const data = params.data as { __node?: GraphNode } | undefined;
      if (params.dataType === 'node' && data?.__node) {
        clickRef.current?.(data.__node);
      }
    };
    chart.on('click', onClick);

    // 同势力高亮（势力图）：hover 某节点 → 高亮同 category 的全部节点。
    const onMouseOver = (params: echarts.ECElementEvent) => {
      if (!hlCategoryRef.current || params.dataType !== 'node') return;
      const cat = (params.data as { category?: number } | undefined)?.category;
      if (cat === undefined) return;
      const idx = nodesRef.current.reduce<number[]>((acc, n, i) => {
        if (n.category === cat) acc.push(i);
        return acc;
      }, []);
      chart?.dispatchAction({ type: 'highlight', seriesIndex: 0, dataIndex: idx });
    };
    const onMouseOut = () => {
      if (hlCategoryRef.current) chart?.dispatchAction({ type: 'downplay', seriesIndex: 0 });
    };
    chart.on('mouseover', onMouseOver);
    chart.on('mouseout', onMouseOut);

    const ro = new ResizeObserver(() => chart?.resize());
    ro.observe(el);

    return () => {
      ro.disconnect();
      chart?.off('click', onClick);
      chart?.off('mouseover', onMouseOver);
      chart?.off('mouseout', onMouseOut);
      chart?.dispose();
      chartRef.current = null;
      initializedRef.current = false;
    };
    // 仅在挂载时建图；option 变更由下方增量 effect 处理。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasData]);

  // 增量更新：保留已固定坐标，避免每次 props 变化全量重排（#5）。
  useEffect(() => {
    if (!initializedRef.current) return;
    chartRef.current?.setOption(option, { notMerge: false });
  }, [option]);

  if (!hasData) {
    return <Empty description={emptyText} image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }

  return (
    <div
      ref={containerRef}
      data-testid={testId}
      style={{ width: '100%', height: typeof height === 'number' ? `${height}px` : height }}
    />
  );
};

export default ForceGraph;
