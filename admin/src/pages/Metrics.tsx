// 数据看板：/admin/metrics/overview 聚合 → antd Statistic + echarts（token 成本 / tick 分布）
// + /admin/metrics/trends 按天趋势（近 7/14/30 天折线，独立加载不影响 overview）。
import { useEffect, useRef, useState } from 'react';
import { Button, Card, Col, Empty, Row, Segmented, Space, Spin, Statistic, Typography } from 'antd';
import ReactECharts from 'echarts-for-react';
import { adminFetch } from '../api';
import { ErrorAlert, formatNumber, formatPercent, friendlyError } from '../components/shared';

interface MetricsOverview {
  users: { total: number; banned: number };
  dailyReports: { total: number; opened: number; openRate: number };
  ticks: { total: number; done: number; failed: number; successRate: number };
  tokenCostByWorld: { worldId: string; tokens: number }[];
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
