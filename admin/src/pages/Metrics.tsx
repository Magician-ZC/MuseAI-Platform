// 数据看板：/admin/metrics/overview 聚合 → antd Statistic + echarts（token 成本 / tick 分布
// / 错峰调度成本仪表）+ /admin/metrics/trends 按天趋势（近 7/14/30 天折线，独立加载不影响 overview）。
import { useEffect, useRef, useState } from 'react';
import { Button, Card, Col, Empty, Row, Segmented, Space, Spin, Statistic, Table, Typography } from 'antd';
import ReactECharts from 'echarts-for-react';
import { adminFetch } from '../api';
import { ErrorAlert, formatNumber, formatPercent, friendlyError } from '../components/shared';

/**
 * 错峰调度可观测面（`cost.offPeak`，migration 0038 三列的唯一读出口）。
 *
 * 🔴 **单位口径**（服务端 `admin_api/dashboards.rs` 同名注释，docs/API.md 登记）：
 * - `tickRatio` / `tokenRatio` / `savedRatio` 一律 **0..1 小数**（同 successRate/usageRatio），
 *   渲染必须走 `formatPercent`（内部 ×100）；窗口内一拍都没有 → `null`，显示 `—` 而非 0%。
 * - `priceRatioPct` 是**百分数整数**（100=原价、50=5 折），`priceRatio` 是它的 0..1 形态。
 *   两者绝不可混用——把 `priceRatioPct` 丢进 `formatPercent` 会渲染成 5000%。
 * - 金额账面口径是整数分（`*Cents`），`*Cny` 为展示派生值。
 */
interface OffPeakMeter {
  windowDays: number;
  ticks: number;
  tokens: number;
  offPeakTicks: number;
  offPeakTokens: number;
  /** 0..1 小数，无 tick 时为 null。 */
  tickRatio: number | null;
  tokenRatio: number | null;
  savedRatio: number | null;
  nominalCents: number;
  nominalCny: number;
  savedCents: number;
  savedCny: number;
  effectiveCents: number;
  effectiveCny: number;
  deferredTicks: number;
  deferMsTotal: number;
  deferMsMax: number;
  /** 平均延后毫秒，分母只含被延后过的拍；无被延后拍 → null。 */
  avgDeferMs: number | null;
  byRatio: OffPeakRatioBucket[];
}

interface OffPeakRatioBucket {
  /** 百分数整数（100=原价）。 */
  priceRatioPct: number;
  /** 同一个数的 0..1 形态。 */
  priceRatio: number;
  ticks: number;
  tokens: number;
  savedCents: number;
  savedCny: number;
}

/** 平台成本仪表（`/admin/metrics/overview` 顶层 `cost`）。金额一律整数分为账面，`cny` 为展示派生。 */
interface CostOverview {
  centsPer1kTokens: number;
  today: { day: string; tokens: number; cents: number; cny: number };
  trendDays: number;
  /** `offPeakTokens` 是当日 `tokens` 里走了折扣时段的那部分，两者是**包含**关系，不可相加。 */
  trend: { day: string; tokens: number; cents: number; cny: number; offPeakTokens?: number }[];
  offPeak?: OffPeakMeter | null;
}

interface MetricsOverview {
  users: { total: number; banned: number };
  dailyReports: { total: number; opened: number; openRate: number };
  ticks: { total: number; done: number; failed: number; successRate: number };
  tokenCostByWorld: { worldId: string; tokens: number }[];
  /** 旧版 server 可能不下发；缺席时错峰区整段走空态，不报错。 */
  cost?: CostOverview | null;
  auditBacklog: number;
  worlds: { active: number; fused: number };
  riskEvents: number;
  dataRequestsPending: number;
}

/** 按天趋势（GET /admin/metrics/trends）：UTC 日界、升序、末位为今天、空天补零。 */
interface TrendDay {
  day: string;
  newUsers: number;
  activeWorlds: number;
  events: number;
  tickTokens: number;
  giftCount: number;
  revenueCents: number;
}

// 趋势系列色（4 系列 categorical，已过色觉/对比校验；固定顺序分配，不轮转）。
const TREND_COLORS = ['#1677ff', '#389e0d', '#722ed1', '#ad6800'];

const TREND_DAY_OPTIONS = [
  { label: '近 7 天', value: 7 },
  { label: '近 14 天', value: 14 },
  { label: '近 30 天', value: 30 },
];

// 'YYYY-MM-DD' → 'MM-DD'（x 轴刻度更紧凑，tooltip 仍显示全量）。
const shortDay = (d: string): string => d.slice(5);

// 错峰构成图的两个系列色：取 TREND_COLORS 的第 1 / 第 4 位。该对已对本页卡片底色
// （antd token colorBgContainer = #fffdfa）校验：亮度带 / 色度下限 / 对比度全过，
// CVD 分离 protan ΔE 32.1 · tritan 24.5、常视 ΔE 34.7。折扣段贴基线，便于逐日横比。
const OFF_PEAK_COLOR = TREND_COLORS[0];
const FULL_PRICE_COLOR = TREND_COLORS[3];
/** 堆叠段之间留 2px 卡片底色缝，避免两段糊成一根柱子。值须跟随 main.tsx 的 colorBgContainer。 */
const CARD_SURFACE = '#fffdfa';

/** 毫秒时长 → 人读字符串；null（无被延后拍）走 `—`，不得渲染成 0。 */
function formatDuration(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return '—';
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)} 秒`;
  const minutes = seconds / 60;
  if (minutes < 60) return `${minutes.toFixed(1)} 分钟`;
  return `${(minutes / 60).toFixed(1)} 小时`;
}

/** 金额（元）→ `¥1,234.56`。账面口径是整数分，这里只做展示格式化。 */
function formatCny(cny?: number | null): string {
  if (cny == null) return '—';
  return `¥${cny.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

function StatCard({ children }: { children: React.ReactNode }) {
  return <Card size="small">{children}</Card>;
}

export default function Metrics() {
  const [data, setData] = useState<MetricsOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 趋势区独立状态：切换天数 / 失败重试都不影响上方 overview。
  const [trendDays, setTrendDays] = useState(14);
  const [trend, setTrend] = useState<TrendDay[] | null>(null);
  const [trendLoading, setTrendLoading] = useState(true);
  const [trendError, setTrendError] = useState<string | null>(null);
  // 请求序号：快速切换 7/14/30 时丢弃过期响应。
  const trendReqRef = useRef(0);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      setData(await adminFetch<MetricsOverview>('/admin/metrics/overview'));
    } catch (e) {
      setError(friendlyError(e));
    } finally {
      setLoading(false);
    }
  };

  const loadTrends = async () => {
    const seq = ++trendReqRef.current;
    setTrendLoading(true);
    setTrendError(null);
    try {
      const res = await adminFetch<{ days: TrendDay[] }>(`/admin/metrics/trends?days=${trendDays}`);
      if (seq !== trendReqRef.current) return;
      setTrend(res.days);
    } catch (e) {
      if (seq !== trendReqRef.current) return;
      setTrendError(friendlyError(e));
    } finally {
      if (seq === trendReqRef.current) setTrendLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  useEffect(() => {
    loadTrends();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [trendDays]);

  const tokenBarOption = data && {
    tooltip: { trigger: 'axis' as const },
    grid: { left: 60, right: 20, top: 20, bottom: 70 },
    xAxis: {
      type: 'category' as const,
      data: data.tokenCostByWorld.map((w) => w.worldId),
      axisLabel: { rotate: 35, formatter: (v: string) => (v.length > 10 ? `${v.slice(0, 10)}…` : v) },
    },
    yAxis: { type: 'value' as const, name: 'token' },
    series: [{ type: 'bar' as const, data: data.tokenCostByWorld.map((w) => w.tokens), itemStyle: { color: '#1677ff' } }],
  };

  // 图一「规模」：新增用户 / 活跃世界 / 世界事件 / 礼物量多系列折线（同为「个数」量纲，共用一根 y 轴）。
  const scaleTrendOption = trend && {
    color: TREND_COLORS,
    tooltip: { trigger: 'axis' as const },
    legend: { bottom: 0 },
    grid: { left: 48, right: 24, top: 24, bottom: 56 },
    xAxis: {
      type: 'category' as const,
      boundaryGap: false,
      data: trend.map((d) => d.day),
      axisLabel: { formatter: shortDay },
    },
    yAxis: { type: 'value' as const, minInterval: 1 },
    series: [
      { name: '新增用户', type: 'line' as const, data: trend.map((d) => d.newUsers) },
      { name: '活跃世界', type: 'line' as const, data: trend.map((d) => d.activeWorlds) },
      { name: '世界事件', type: 'line' as const, data: trend.map((d) => d.events) },
      { name: '礼物量', type: 'line' as const, data: trend.map((d) => d.giftCount) },
    ],
  };

  // 图二「消耗与收入」：token 与「元」量纲不同，不做双 y 轴——上下两个联动子图共用时间轴
  // （axisPointer link 同步十字线），各自独立 y 轴，避免双轴比例误读。
  const costTrendOption = trend && {
    color: [TREND_COLORS[0], TREND_COLORS[1]],
    tooltip: { trigger: 'axis' as const },
    axisPointer: { link: [{ xAxisIndex: 'all' as const }] },
    legend: { bottom: 0 },
    grid: [
      { left: 64, right: 24, top: 20, height: '30%' },
      { left: 64, right: 24, top: '50%', height: '30%' },
    ],
    xAxis: [
      {
        type: 'category' as const,
        gridIndex: 0,
        boundaryGap: false,
        data: trend.map((d) => d.day),
        axisLabel: { show: false },
        axisTick: { show: false },
      },
      {
        type: 'category' as const,
        gridIndex: 1,
        boundaryGap: false,
        data: trend.map((d) => d.day),
        axisLabel: { formatter: shortDay },
      },
    ],
    yAxis: [
      { type: 'value' as const, gridIndex: 0, name: 'token' },
      { type: 'value' as const, gridIndex: 1, name: '元' },
    ],
    series: [
      {
        name: 'token 消耗',
        type: 'line' as const,
        xAxisIndex: 0,
        yAxisIndex: 0,
        data: trend.map((d) => d.tickTokens),
        tooltip: { valueFormatter: (v: unknown) => `${formatNumber(Number(v))} token` },
      },
      {
        name: '收入（元）',
        type: 'line' as const,
        xAxisIndex: 1,
        yAxisIndex: 1,
        // revenueCents 分 → 元。
        data: trend.map((d) => d.revenueCents / 100),
        tooltip: { valueFormatter: (v: unknown) => `¥${Number(v).toFixed(2)}` },
      },
    ],
  };

  // ---------- 错峰调度成本仪表（cost.offPeak，migration 0038） ----------
  const cost = data?.cost ?? null;
  const offPeak = cost?.offPeak ?? null;
  // 三档形态：接口没给 → 整段空态；窗口内无 tick → "暂无数据"；有 tick 但错峰拍为 0 → 真实的 0%（关闭态）。
  const offPeakHasTicks = !!offPeak && offPeak.ticks > 0;
  const offPeakActive = !!offPeak && offPeak.offPeakTicks > 0;

  // 逐日构成：折扣时段贴基线，原价段叠在上面（tokens 是全量，两者是包含关系，故原价 = 全量 − 折扣）。
  const offPeakTrendOption = cost && {
    color: [OFF_PEAK_COLOR, FULL_PRICE_COLOR],
    tooltip: { trigger: 'axis' as const, axisPointer: { type: 'shadow' as const } },
    legend: { bottom: 0 },
    grid: { left: 64, right: 24, top: 20, bottom: 48 },
    xAxis: {
      type: 'category' as const,
      data: cost.trend.map((d) => d.day),
      axisLabel: { formatter: shortDay },
    },
    yAxis: { type: 'value' as const, name: 'token' },
    series: [
      {
        name: '折扣时段',
        type: 'bar' as const,
        stack: 'tokens',
        data: cost.trend.map((d) => d.offPeakTokens ?? 0),
        itemStyle: { borderColor: CARD_SURFACE, borderWidth: 2 },
        tooltip: { valueFormatter: (v: unknown) => `${formatNumber(Number(v))} token` },
      },
      {
        name: '原价时段',
        type: 'bar' as const,
        stack: 'tokens',
        data: cost.trend.map((d) => Math.max(0, d.tokens - (d.offPeakTokens ?? 0))),
        // 顶段收 4px 圆角（堆叠柱的数据端），底段保持贴基线的直角。
        itemStyle: { borderColor: CARD_SURFACE, borderWidth: 2, borderRadius: [4, 4, 0, 0] as [number, number, number, number] },
        tooltip: { valueFormatter: (v: unknown) => `${formatNumber(Number(v))} token` },
      },
    ],
  };

  const tickOther = data ? Math.max(0, data.ticks.total - data.ticks.done - data.ticks.failed) : 0;
  const tickPieOption = data && {
    tooltip: { trigger: 'item' as const },
    legend: { bottom: 0 },
    series: [
      {
        type: 'pie' as const,
        radius: ['40%', '68%'],
        data: [
          { name: '成功', value: data.ticks.done, itemStyle: { color: '#52c41a' } },
          { name: '失败', value: data.ticks.failed, itemStyle: { color: '#ff4d4f' } },
          { name: '其它', value: tickOther, itemStyle: { color: '#faad14' } },
        ],
      },
    ],
  };

  return (
    <div>
      <Space style={{ marginBottom: 16, width: '100%', justifyContent: 'space-between' }}>
        <Typography.Title level={4} style={{ margin: 0 }}>数据看板</Typography.Title>
        <Button onClick={load} loading={loading}>刷新</Button>
      </Space>

      {error && <ErrorAlert message={error} onRetry={load} />}

      {loading && !data ? (
        <div style={{ textAlign: 'center', marginTop: 80 }}>
          <Spin />
        </div>
      ) : (
        data && (
          <>
            <Row gutter={[16, 16]}>
              <Col xs={12} md={6}><StatCard><Statistic title="注册用户" value={data.users.total} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="封禁用户" value={data.users.banned} valueStyle={{ color: '#cf1322' }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="日报送达" value={data.dailyReports.total} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="日报打开率" value={formatPercent(data.dailyReports.openRate)} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="tick 成功率" value={formatPercent(data.ticks.successRate)} valueStyle={{ color: '#3f8600' }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="tick 失败数" value={data.ticks.failed} valueStyle={{ color: data.ticks.failed ? '#cf1322' : undefined }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="审核积压" value={data.auditBacklog} valueStyle={{ color: data.auditBacklog ? '#d46b08' : undefined }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="待处理工单" value={data.dataRequestsPending} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="活跃世界" value={data.worlds.active} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="熔断世界" value={data.worlds.fused} valueStyle={{ color: data.worlds.fused ? '#cf1322' : undefined }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="风控事件" value={data.riskEvents} /></StatCard></Col>
            </Row>

            <Row gutter={[16, 16]} style={{ marginTop: 16 }}>
              <Col xs={24} lg={14}>
                <Card size="small" title="按世界 token 成本（Top 10）">
                  {data.tokenCostByWorld.length ? (
                    <ReactECharts option={tokenBarOption} style={{ height: 320 }} notMerge />
                  ) : (
                    <Empty description="暂无 tick 成本数据" />
                  )}
                </Card>
              </Col>
              <Col xs={24} lg={10}>
                <Card size="small" title="tick 状态分布">
                  {data.ticks.total ? (
                    <ReactECharts option={tickPieOption} style={{ height: 320 }} notMerge />
                  ) : (
                    <Empty description="暂无 tick 数据" />
                  )}
                </Card>
              </Col>
            </Row>
            <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>
              指标为服务端 SQL 聚合（注册数 / 日报打开率 / tick 成功率 / token 成本 / 审核积压等）；成本收入比在 P4b 收费后再引入。
            </Typography.Paragraph>

            {/* ---- 错峰调度（成本工程杠杆①，migration 0038 → cost.offPeak） ---- */}
            <Typography.Title level={5} style={{ margin: '24px 0 12px' }}>
              错峰调度
              {offPeak ? <Typography.Text type="secondary" style={{ fontWeight: 400, marginLeft: 8 }}>近 {offPeak.windowDays} 日（UTC 日界）</Typography.Text> : null}
            </Typography.Title>

            {!offPeak ? (
              <Card size="small"><Empty description="服务端未返回错峰数据（cost.offPeak）" /></Card>
            ) : !offPeakHasTicks ? (
              <Card size="small"><Empty description={`近 ${offPeak.windowDays} 日窗口内暂无 tick，错峰无从统计`} /></Card>
            ) : (
              <>
                <Row gutter={[16, 16]}>
                  {/* 比率字段是 0..1 小数，formatPercent 内部 ×100；null → 显示 —。 */}
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic title="错峰拍占比" value={formatPercent(offPeak.tickRatio)} />
                      <Typography.Text type="secondary">{formatNumber(offPeak.offPeakTicks)} / {formatNumber(offPeak.ticks)} 拍</Typography.Text>
                    </StatCard>
                  </Col>
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic title="错峰 token 占比" value={formatPercent(offPeak.tokenRatio)} />
                      <Typography.Text type="secondary">{formatNumber(offPeak.offPeakTokens)} / {formatNumber(offPeak.tokens)} token</Typography.Text>
                    </StatCard>
                  </Col>
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic
                        title="估算节省"
                        value={formatCny(offPeak.savedCny)}
                        valueStyle={{ color: offPeak.savedCents > 0 ? '#3f8600' : undefined }}
                      />
                      <Typography.Text type="secondary">原价 {formatCny(offPeak.nominalCny)} · 折让 {formatPercent(offPeak.savedRatio)}</Typography.Text>
                    </StatCard>
                  </Col>
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic title="平均延后时长" value={formatDuration(offPeak.avgDeferMs)} />
                      <Typography.Text type="secondary">
                        被延后 {formatNumber(offPeak.deferredTicks)} 拍 · 最长 {formatDuration(offPeak.deferMsMax || null)}
                      </Typography.Text>
                    </StatCard>
                  </Col>
                </Row>

                <Row gutter={[16, 16]} style={{ marginTop: 16 }}>
                  <Col xs={24} lg={14}>
                    <Card size="small" title={`逐日 token 构成（折扣时段 / 原价时段，近 ${offPeak.windowDays} 日）`}>
                      {offPeakActive ? (
                        <ReactECharts option={offPeakTrendOption} style={{ height: 320 }} notMerge />
                      ) : (
                        // 错峰默认关闭（MUSE_OFFPEAK_SCHEDULING）：全部落中性值，0% 是真实答案而非故障。
                        <Empty description={`近 ${offPeak.windowDays} 日内没有错峰拍：错峰调度默认关闭（MUSE_OFFPEAK_SCHEDULING），此时每拍按原价记账`} />
                      )}
                    </Card>
                  </Col>
                  <Col xs={24} lg={10}>
                    <Card size="small" title="名义档位分布">
                      <Table<OffPeakRatioBucket>
                        size="small"
                        rowKey={(r) => String(r.priceRatioPct)}
                        dataSource={offPeak.byRatio}
                        pagination={false}
                        locale={{ emptyText: <Empty description="窗口内无记账拍" /> }}
                        columns={[
                          {
                            title: '档位',
                            dataIndex: 'priceRatioPct',
                            key: 'pct',
                            // priceRatioPct 是百分数整数（100=原价），直接带 % 渲染；
                            // 绝不能走 formatPercent（那是给 0..1 小数用的）。
                            render: (pct: number) => (pct === 100 ? '原价（100%）' : `${pct}%`),
                          },
                          { title: '拍数', dataIndex: 'ticks', key: 'ticks', align: 'right', render: formatNumber },
                          { title: 'token', dataIndex: 'tokens', key: 'tokens', align: 'right', render: formatNumber },
                          { title: '估算节省', dataIndex: 'savedCny', key: 'saved', align: 'right', render: (v: number) => formatCny(v) },
                        ]}
                      />
                    </Card>
                  </Col>
                </Row>

                <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>
                  「估算节省」= Σ 按档位汇总的 token × (100 − 档位%) ÷ 100 × 单价（{cost?.centsPer1kTokens ?? '—'} 分 / 千 token）。
                  🔴 档位是<strong>运营配置的名义折扣</strong>，不是供应商账单结算价，仅用于估算错峰收益，<strong>不得当对账依据</strong>；
                  上方今日成本 / 趋势 / 每局成本一律按原价计，与此处折让不重复相减。
                  延后时长由进程内存态计时，server 重启会让在途延后账清零（方向为低估，不会虚报）。
                </Typography.Paragraph>
              </>
            )}
          </>
        )
      )}

      {/* 运营趋势（GET /admin/metrics/trends）：紧随 overview 之后，独立加载 / 错误 / 重试。 */}
      <Space style={{ margin: '24px 0 12px', width: '100%', justifyContent: 'space-between' }}>
        <Typography.Title level={5} style={{ margin: 0 }}>运营趋势</Typography.Title>
        <Segmented
          value={trendDays}
          onChange={(v) => setTrendDays(Number(v))}
          options={TREND_DAY_OPTIONS}
        />
      </Space>

      {trendError && <ErrorAlert message={trendError} onRetry={loadTrends} />}

      {trendLoading && !trend ? (
        <div style={{ textAlign: 'center', margin: '48px 0' }}>
          <Spin />
        </div>
      ) : (
        trend && (
          <Row gutter={[16, 16]}>
            <Col xs={24} lg={12}>
              <Card size="small" title="规模（新增用户 / 活跃世界 / 世界事件 / 礼物量）">
                {trend.length ? (
                  <ReactECharts option={scaleTrendOption} style={{ height: 320 }} notMerge />
                ) : (
                  <Empty description="暂无趋势数据" />
                )}
              </Card>
            </Col>
            <Col xs={24} lg={12}>
              <Card size="small" title="消耗与收入（token 消耗 / 收入·元）">
                {trend.length ? (
                  <ReactECharts option={costTrendOption} style={{ height: 320 }} notMerge />
                ) : (
                  <Empty description="暂无趋势数据" />
                )}
              </Card>
            </Col>
          </Row>
        )
      )}
    </div>
  );
}
