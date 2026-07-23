// 数据看板：/admin/metrics/overview 聚合 → antd Statistic + echarts（token 成本 / tick 分布）。
import { useEffect, useState } from 'react';
import { Button, Card, Col, Empty, Row, Space, Spin, Statistic, Typography } from 'antd';
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

function StatCard({ children }: { children: React.ReactNode }) {
  return <Card size="small">{children}</Card>;
}

export default function Metrics() {
  const [data, setData] = useState<MetricsOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

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

  useEffect(() => {
    load();
  }, []);

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
    </div>
  );
}
