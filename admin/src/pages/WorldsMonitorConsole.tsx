import { useEffect, useMemo, useState } from 'react';
import {
  AlertOutlined,
  CalendarOutlined,
  DownOutlined,
  ReloadOutlined,
} from '@ant-design/icons';
import { App as AntdApp, Button, Progress, Segmented, Select, Spin, Tag } from 'antd';
import ReactECharts from 'echarts-for-react';
import { useLocation, useNavigate } from 'react-router-dom';
import { adminFetch } from '../api';
import { ErrorAlert, friendlyError, ReasonModal, usePagedList } from '../components/shared';
import './WorldsMonitorConsole.css';

interface WorldApiRow {
  id: string;
  title: string;
  roomType: string;
  status: string;
  visibility: string;
  memberLimit: number;
  tickPerDay: number;
  engineVersion: string;
  promptSetVersion: string;
  modelRouteVersion: string;
  stateRevision: number;
  spentTokensToday: number;
  dailyTokenBudget: number;
  fused: boolean;
  createdAt: number;
}

interface TickMeta {
  tickNo: number;
  status: string;
  error: string | null;
  costTokens: number;
  startedAt: number | null;
  finishedAt: number | null;
  createdAt: number;
}

interface Diagnostics {
  world: Record<string, unknown> & { id: string; title: string; status: string };
  ticks: TickMeta[];
  budget: {
    dailyTokenBudget: number;
    dailyCnyBudgetCents: number;
    spentTokensToday: number;
    budgetDay: string;
    fused: boolean;
  } | null;
  riskEventCounts: { kind: string; count: number }[];
  eventStats: { total: number; byModeration: { moderation: string; count: number }[] };
  redactionNote: string;
}

interface ConsoleWorld extends WorldApiRow {
  thumbnail: string;
  roomTypeLabel: string;
  participantCount: number | null;
  successRate: number | null;
  todayCostCny: number | null;
  moderationLatency: number | null;
  lastActivityLabel: string;
}

const ROOM_TYPE_TEXT: Record<string, string> = {
  idle: '开放世界',
  chapter: '章节房',
  arena: '赛事房',
  exploration: '探索房',
  social: '社交房',
  quest: '任务房',
  story: '剧情房',
  side: '副本房',
};

const STATUS_TEXT: Record<string, string> = {
  open: '运行中',
  running: '运行中',
  attention: '需关注',
  paused: '已暂停',
  fused: '已熔断',
  ended: '已结束',
};

const PREVIEW_WORLDS: ConsoleWorld[] = [
  {
    id: 'world_1001', title: '雾海纪元', roomType: 'idle', roomTypeLabel: '开放世界', status: 'running', visibility: 'official',
    memberLimit: 2000, participantCount: 1248, tickPerDay: 4, engineVersion: 'v3.8.2', promptSetVersion: 'v2.13.0',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 1842391, spentTokensToday: 642000, dailyTokenBudget: 1000000,
    fused: false, createdAt: 1753392753000, successRate: 96.2, todayCostCny: 128.64, moderationLatency: 1.6,
    lastActivityLabel: '1 分钟前', thumbnail: '/assets/worlds/mist-sea-world.png',
  },
  {
    id: 'world_1002', title: '静止山脉', roomType: 'exploration', roomTypeLabel: '探索房', status: 'running', visibility: 'public',
    memberLimit: 1000, participantCount: 612, tickPerDay: 3, engineVersion: 'v3.8.2', promptSetVersion: 'v2.12.6',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 812104, spentTokensToday: 483000, dailyTokenBudget: 800000,
    fused: false, createdAt: 1753392100000, successRate: 97.8, todayCostCny: 96.71, moderationLatency: 1.2,
    lastActivityLabel: '3 分钟前', thumbnail: '/assets/worlds/still-mountains.png',
  },
  {
    id: 'world_1003', title: '星火酒馆', roomType: 'social', roomTypeLabel: '社交房', status: 'attention', visibility: 'public',
    memberLimit: 600, participantCount: 386, tickPerDay: 6, engineVersion: 'v3.8.1', promptSetVersion: 'v2.12.8',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 302768, spentTokensToday: 321000, dailyTokenBudget: 500000,
    fused: false, createdAt: 1753391800000, successRate: 90.6, todayCostCny: 64.33, moderationLatency: 3.8,
    lastActivityLabel: '2 分钟前', thumbnail: '/assets/worlds/ember-tavern.png',
  },
  {
    id: 'world_1004', title: '机械之城', roomType: 'quest', roomTypeLabel: '任务房', status: 'running', visibility: 'official',
    memberLimit: 1400, participantCount: 932, tickPerDay: 4, engineVersion: 'v3.8.2', promptSetVersion: 'v2.13.0',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 1231009, spentTokensToday: 1051000, dailyTokenBudget: 1400000,
    fused: false, createdAt: 1753391600000, successRate: 95.1, todayCostCny: 210.38, moderationLatency: 1.4,
    lastActivityLabel: '1 分钟前', thumbnail: '/assets/worlds/mechanical-city.png',
  },
  {
    id: 'world_1005', title: '沙海旅途', roomType: 'story', roomTypeLabel: '剧情房', status: 'attention', visibility: 'public',
    memberLimit: 500, participantCount: 274, tickPerDay: 3, engineVersion: 'v3.8.1', promptSetVersion: 'v2.12.6',
    modelRouteVersion: 'qwen-max-backup', stateRevision: 213557, spentTokensToday: 210000, dailyTokenBudget: 400000,
    fused: false, createdAt: 1753391300000, successRate: 88.3, todayCostCny: 42.19, moderationLatency: 4.5,
    lastActivityLabel: '4 分钟前', thumbnail: '/assets/worlds/desert-journey.png',
  },
  {
    id: 'world_1006', title: '永夜之境', roomType: 'side', roomTypeLabel: '副本房', status: 'fused', visibility: 'official',
    memberLimit: 400, participantCount: 0, tickPerDay: 2, engineVersion: 'v3.8.0', promptSetVersion: 'v2.11.9',
    modelRouteVersion: 'qwen-max-backup', stateRevision: 87662, spentTokensToday: 0, dailyTokenBudget: 300000,
    fused: true, createdAt: 1753390900000, successRate: null, todayCostCny: 0, moderationLatency: null,
    lastActivityLabel: '18 分钟前', thumbnail: '/assets/worlds/evernight-realm.png',
  },
];

const PREVIEW_DIAGNOSTICS: Diagnostics = {
  world: {
    id: 'world_1001', title: '雾海纪元', status: 'running', roomType: 'idle', modelRouteVersion: 'qwen-max-primary',
    promptSetVersion: 'v2.13.0', startedAt: '2026-07-25 08:12:33',
  },
  ticks: Array.from({ length: 24 }, (_, index) => ({
    tickNo: 1842368 + index,
    status: index === 17 ? 'failed' : 'done',
    error: index === 17 ? 'moderation_latency_high' : null,
    costTokens: 1100 + index * 17,
    startedAt: 1753428000000 + index * 75000,
    finishedAt: 1753428000000 + index * 75000 + (index === 17 ? 2100 : 900 + (index % 7) * 115),
    createdAt: 1753428000000 + index * 75000,
  })),
  budget: { dailyTokenBudget: 1000000, dailyCnyBudgetCents: 20000, spentTokensToday: 642000, budgetDay: '2026-07-25', fused: false },
  riskEventCounts: [{ kind: 'prompt_injection', count: 1 }, { kind: 'moderation_delay', count: 1 }],
  eventStats: { total: 327, byModeration: [{ moderation: 'approved', count: 324 }, { moderation: 'pending', count: 3 }] },
  redactionNote: '诊断信息已脱敏，不展示用户私密内容与模型链式推理。',
};

const INCIDENTS = [
  { time: '12:45:21', level: 'warning', label: '警告', title: '风控延迟升高', detail: '近 5 分钟风控平均延迟 1.6s，超过阈值 1.5s', source: '系统', ref: 'world_1001' },
  { time: '12:30:07', level: 'info', label: '信息', title: '世界心跳正常', detail: '心跳检测通过，延迟 482ms', source: '系统', ref: 'world_1001' },
  { time: '12:15:33', level: 'danger', label: '报警', title: '内容风控命中', detail: '检测到疑似边界内容（低风险），已拦截并提示', source: '风控引擎', ref: 'R-0302' },
  { time: '12:02:11', level: 'info', label: '信息', title: '模型路由切换', detail: '从 qwen-max 切换到 qwen-max（备）', source: '调度系统', ref: '错误率升高' },
  { time: '11:45:58', level: 'info', label: '信息', title: '世界启动', detail: '世界 雾海纪元 已启动', source: '系统', ref: '运营：林逸' },
];

function apiRowToConsole(row: WorldApiRow): ConsoleWorld {
  return {
    ...row,
    roomTypeLabel: ROOM_TYPE_TEXT[row.roomType] ?? row.roomType,
    participantCount: null,
    successRate: null,
    todayCostCny: null,
    moderationLatency: null,
    lastActivityLabel: new Date(row.createdAt).toLocaleString('zh-CN', { hour12: false }),
    thumbnail: '/assets/worlds/mist-sea-world.png',
    status: row.fused ? 'fused' : row.status,
  };
}

function formatCompact(value: number | null): string {
  if (value == null) return '—';
  return value.toLocaleString('zh-CN');
}

function statusClass(status: string): string {
  if (status === 'attention') return 'is-attention';
  if (status === 'fused' || status === 'ended') return 'is-danger';
  if (status === 'paused') return 'is-paused';
  return 'is-running';
}

export default function WorldsMonitorConsole() {
  const { message: messageApi } = AntdApp.useApp();
  const location = useLocation();
  const navigate = useNavigate();
  const designPreview = (import.meta as any).env?.DEV && new URLSearchParams(location.search).get('design') === 'preview';
  const [statusFilter, setStatusFilter] = useState<string | undefined>();
  const [range, setRange] = useState<string>('24小时');
  const [previewWorlds, setPreviewWorlds] = useState(PREVIEW_WORLDS);
  const [selectedId, setSelectedId] = useState(designPreview ? 'world_1001' : '');
  const [diag, setDiag] = useState<Diagnostics | null>(designPreview ? PREVIEW_DIAGNOSTICS : null);
  const [diagLoading, setDiagLoading] = useState(false);
  const [diagError, setDiagError] = useState<string | null>(null);
  const [action, setAction] = useState<{ row: ConsoleWorld; kind: 'pause' | 'resume' } | null>(null);
  const [acting, setActing] = useState(false);

  const list = usePagedList<WorldApiRow>(async (cursor) => {
    const query = new URLSearchParams();
    if (statusFilter) query.set('status', statusFilter);
    if (cursor) query.set('cursor', cursor);
    query.set('limit', '20');
    const result = await adminFetch<{ worlds: WorldApiRow[]; nextCursor: string | null }>(`/admin/worlds?${query.toString()}`);
    return { items: result.worlds, nextCursor: result.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    if (!designPreview) reload();
  }, [designPreview, reload, statusFilter]);

  const worlds = useMemo(
    () => (designPreview ? previewWorlds : list.items.map(apiRowToConsole)),
    [designPreview, list.items, previewWorlds],
  );
  const selected = worlds.find((world) => world.id === selectedId) ?? worlds[0] ?? null;

  useEffect(() => {
    if (!selectedId && worlds[0]) setSelectedId(worlds[0].id);
  }, [selectedId, worlds]);

  const loadDiagnostics = async (world: ConsoleWorld) => {
    setSelectedId(world.id);
    setDiagError(null);
    if (designPreview) {
      setDiag({ ...PREVIEW_DIAGNOSTICS, world: { ...PREVIEW_DIAGNOSTICS.world, id: world.id, title: world.title, status: world.status } });
      return;
    }
    setDiagLoading(true);
    try {
      setDiag(await adminFetch<Diagnostics>(`/admin/worlds/${world.id}/diagnostics`));
    } catch (error) {
      setDiagError(friendlyError(error));
    } finally {
      setDiagLoading(false);
    }
  };

  const doAction = async (reason: string) => {
    if (!action) return;
    setActing(true);
    try {
      if (designPreview) {
        setPreviewWorlds((current) => current.map((world) => (
          world.id === action.row.id
            ? { ...world, status: action.kind === 'pause' ? 'paused' : 'running', fused: false }
            : world
        )));
      } else {
        await adminFetch(`/admin/worlds/${action.row.id}/${action.kind}?reason=${encodeURIComponent(reason)}`, 'POST');
        reload();
      }
      messageApi.success(action.kind === 'pause' ? '世界已暂停' : '世界已恢复');
      setAction(null);
    } catch (error) {
      messageApi.error(friendlyError(error));
    } finally {
      setActing(false);
    }
  };

  const latencyData = useMemo(() => {
    const ticks = diag?.ticks ?? PREVIEW_DIAGNOSTICS.ticks;
    return ticks.map((tick) => {
      if (tick.startedAt == null || tick.finishedAt == null) return null;
      return Number(((tick.finishedAt - tick.startedAt) / 1000).toFixed(2));
    });
  }, [diag]);

  const latencyOption = {
    animation: false,
    grid: { left: 35, right: 16, top: 14, bottom: 26 },
    xAxis: {
      type: 'category',
      data: latencyData.map((_, index) => index),
      boundaryGap: false,
      axisLine: { lineStyle: { color: '#ddd7cf' } },
      axisTick: { show: false },
      axisLabel: { color: '#928b83', fontSize: 10, formatter: (value: number) => (value === 0 ? '12:15' : value === 8 ? '12:30' : value === 16 ? '12:45' : value === 23 ? '现在' : '') },
    },
    yAxis: {
      type: 'value', min: 0, max: 3,
      axisLabel: { color: '#928b83', fontSize: 10, formatter: '{value}s' },
      splitLine: { lineStyle: { color: '#eee9e2' } },
    },
    tooltip: { trigger: 'axis', valueFormatter: (value: number) => `${value}s` },
    series: [{
      type: 'line', data: latencyData, smooth: 0.22, symbol: 'none',
      lineStyle: { color: '#638f55', width: 1.6 },
      areaStyle: { color: 'rgba(99,143,85,0.04)' },
    }],
  };

  const costSparkOption = {
    animation: false,
    grid: { left: 2, right: 2, top: 4, bottom: 4 },
    xAxis: { type: 'category', show: false, data: [1, 2, 3, 4, 5, 6, 7] },
    yAxis: { type: 'value', show: false },
    series: [{ type: 'line', data: [1080, 1190, 1135, 1270, 1205, 1350, 1284], symbol: 'none', smooth: 0.25, lineStyle: { color: '#b18a72', width: 1.6 } }],
  };

  return (
    <div className="world-console">
      <div className="world-console__toolbar">
        <Segmented
          size="small"
          value={range}
          onChange={(value) => setRange(String(value))}
          options={['实时', '1小时', '6小时', '24小时', '7天']}
        />
        <Button icon={<CalendarOutlined />}>2026-07-25 <DownOutlined /></Button>
      </div>

      <div className="world-console__grid">
        <main className="world-console__main">
          <section className="world-health-strip" aria-label="世界运行概览">
            <div className="world-health-strip__metric is-healthy"><span className="world-health-strip__dot" /><span>运行中</span><strong>38</strong></div>
            <div className="world-health-strip__metric is-warning"><span className="world-health-strip__dot" /><span>需关注</span><strong>4</strong></div>
            <div className="world-health-strip__metric is-danger"><span className="world-health-strip__dot" /><span>已熔断</span><strong>1</strong></div>
            <div className="world-health-strip__metric is-cost">
              <span>今日成本</span><strong>¥ 1,284</strong>
              <ReactECharts option={costSparkOption} style={{ width: 82, height: 38 }} />
            </div>
          </section>

          <section className="world-table-panel" aria-labelledby="active-worlds-title">
            <header className="world-table-panel__header">
              <div><h2 id="active-worlds-title">活跃世界</h2><span>共 {worlds.length} 个</span></div>
              <div>
                <Select
                  size="small"
                  allowClear
                  placeholder="全部状态"
                  value={statusFilter}
                  onChange={setStatusFilter}
                  options={[
                    { value: 'running', label: '运行中' },
                    { value: 'paused', label: '已暂停' },
                    { value: 'ended', label: '已结束' },
                  ]}
                />
                <Button size="small" icon={<ReloadOutlined />} onClick={reload} disabled={designPreview}>刷新</Button>
              </div>
            </header>

            {list.error && !designPreview && <ErrorAlert message={list.error} onRetry={reload} />}
            <div className="world-table-panel__scroll">
              <table className="world-table">
                <thead>
                  <tr>
                    <th>世界名称</th><th>状态</th><th>房间类型</th><th>参与人数</th><th>当前 Tick</th><th>成功率</th><th>今日成本</th><th>风控延迟</th><th>最后活动</th>
                  </tr>
                </thead>
                <tbody>
                  {worlds.map((world) => (
                    <tr key={world.id} className={selected?.id === world.id ? 'is-selected' : ''}>
                      <td>
                        <button className="world-table__world" type="button" onClick={() => void loadDiagnostics(world)}>
                          <img src={world.thumbnail} alt="" />
                          <strong>{world.title}</strong>
                        </button>
                      </td>
                      <td><span className={`world-status ${statusClass(world.status)}`}><i />{STATUS_TEXT[world.status] ?? world.status}</span></td>
                      <td>{world.roomTypeLabel}</td>
                      <td>{formatCompact(world.participantCount)}</td>
                      <td>{formatCompact(world.stateRevision)}</td>
                      <td className={world.successRate != null && world.successRate < 92 ? 'world-value is-warning' : 'world-value is-good'}>{world.successRate == null ? '—' : `${world.successRate}%`}</td>
                      <td>{world.todayCostCny == null ? `${formatCompact(world.spentTokensToday)} tok` : `¥${world.todayCostCny.toFixed(2)}`}</td>
                      <td className={world.moderationLatency != null && world.moderationLatency > 3 ? 'world-value is-danger' : 'world-value'}>{world.moderationLatency == null ? '—' : `${world.moderationLatency}s`}</td>
                      <td>{world.lastActivityLabel}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {list.loading && !designPreview && <div className="world-table-panel__loading"><Spin /></div>}
              {!list.loading && worlds.length === 0 && <div className="world-table-panel__empty">暂无世界</div>}
            </div>
          </section>

          <section className="world-timeline" aria-labelledby="world-timeline-title">
            <header>
              <div><h2 id="world-timeline-title">事件时间线</h2><span>（{selected?.title ?? '未选择世界'}）</span></div>
              <div>
                <Select size="small" defaultValue="all" options={[{ value: 'all', label: '级别：全部' }, { value: 'warning', label: '仅异常' }]} />
              </div>
            </header>
            <div className="world-timeline__rows">
              {INCIDENTS.map((incident) => (
                <div className="world-timeline__row" key={`${incident.time}-${incident.title}`}>
                  <time>{incident.time}</time>
                  <Tag color={incident.level === 'danger' ? 'red' : incident.level === 'warning' ? 'orange' : 'green'}>{incident.label}</Tag>
                  <div><strong>{incident.title}</strong><span>{incident.detail}</span></div>
                  <div><span>{incident.source}</span><small>{incident.ref}</small></div>
                </div>
              ))}
            </div>
          </section>
        </main>

        <aside className="world-inspector" aria-label="世界诊断详情">
          {selected ? (
            <>
              <header className="world-inspector__identity">
                <img src={selected.thumbnail} alt={`${selected.title}缩略图`} />
                <div>
                  <div><h2>{selected.title}</h2><span className={`world-status ${statusClass(selected.status)}`}><i />{STATUS_TEXT[selected.status] ?? selected.status}</span></div>
                  <p>世界ID：{selected.id}　 房间类型：{selected.roomTypeLabel}</p>
                  <p>启动时间：2026-07-25 08:12:33</p>
                </div>
              </header>

              <section className="world-inspector__latency">
                <h3>实时延迟 <span>（最近 30 分钟）</span></h3>
                <ReactECharts option={latencyOption} style={{ height: 142 }} />
                <strong>{selected.moderationLatency ?? '—'}{selected.moderationLatency != null ? 's' : ''}</strong>
              </section>

              <section className="world-inspector__metrics">
                <div>
                  <span>今日预算</span>
                  <strong>¥128 <small>/ ¥200</small></strong>
                  <Progress percent={64} showInfo={false} strokeColor="#cf704d" railColor="#eee8e1" size="small" />
                </div>
                <div>
                  <span>风险命中（今日）</span>
                  <strong>{diag?.riskEventCounts.reduce((sum, item) => sum + item.count, 0) ?? 0} <small>次</small></strong>
                  <p>较昨日 <b>+1</b></p>
                </div>
              </section>

              {diagError && <div className="world-inspector__error"><ErrorAlert message={diagError} onRetry={() => void loadDiagnostics(selected)} /></div>}

              <section className="world-inspector__incident">
                <h3>最新异常</h3>
                <div>
                  <AlertOutlined />
                  <p><strong>风控延迟偏高</strong><span>近 5 分钟平均延迟 1.6s，超过阈值 1.5s</span><span>影响：可能导致响应变慢、排队增加</span><small>发生时间：12:45:21</small></p>
                </div>
              </section>

              <section className="world-inspector__route">
                <h3>模型路由</h3>
                <div><span><i />当前路由</span><strong>{String(diag?.world.modelRouteVersion ?? selected.modelRouteVersion ?? '—')}</strong><small>错误率 1.8%</small></div>
                <div><span><i />备路由</span><strong>qwen-max（备）</strong><small>错误率 2.1%</small></div>
              </section>

              <section className="world-inspector__prompt">
                <h3>Prompt 版本</h3>
                <div><span>当前版本</span><strong>{String(diag?.world.promptSetVersion ?? selected.promptSetVersion ?? '—')}</strong><Tag color="green">已生效</Tag></div>
                <p>更新人：林逸　更新时间：12:02:11</p>
              </section>

              <div className="world-inspector__actions">
                <Button type="primary" size="large" loading={diagLoading} onClick={() => void loadDiagnostics(selected)}>查看诊断</Button>
                <div>
                  <Button danger onClick={() => setAction({ row: selected, kind: selected.status === 'paused' ? 'resume' : 'pause' })}>
                    {selected.status === 'paused' ? '恢复世界' : '暂停世界'}
                  </Button>
                  <Button danger onClick={() => navigate(`/risk${designPreview ? '?design=preview' : ''}`)}>进入应急处置</Button>
                </div>
              </div>
            </>
          ) : (
            <div className="world-inspector__empty">请选择一个世界查看诊断</div>
          )}
        </aside>
      </div>

      <ReasonModal
        open={!!action}
        title={action?.kind === 'pause' ? `暂停世界 ${action?.row.title ?? ''}` : `恢复世界 ${action?.row.title ?? ''}`}
        okText={action?.kind === 'pause' ? '确认暂停' : '确认恢复'}
        danger={action?.kind === 'pause'}
        loading={acting}
        onOk={doAction}
        onCancel={() => setAction(null)}
      />

    </div>
  );
}
