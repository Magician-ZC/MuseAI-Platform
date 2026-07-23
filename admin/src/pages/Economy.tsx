// 经济运营：/admin/economy/overview 真实只读聚合（充值/退款/余额/礼物/订单状态）。
// 数据来自账本双录 + 钱包余额 + 礼物流水，只读，不建结算、不碰创作者分成（§2.6）。
import { useEffect, useState } from 'react';
import { Button, Card, Col, Empty, List, Row, Space, Spin, Statistic, Tag, Typography } from 'antd';
import ReactECharts from 'echarts-for-react';
import { adminFetch } from '../api';
import { ErrorAlert, formatNumber, friendlyError } from '../components/shared';

interface EconomyOverview {
  billingEnabled: boolean;
  recharge: { totalCents: number; count: number };
  refund: { totalCents: number; count: number };
  balance: { totalCents: number; wallets: number };
  ledgerNetCents: number;
  orders: { total: number; byStatus: Record<string, number> };
  gifts: { events: number; giftCount: number; worlds: number };
  creatorSettlement: { enabled: boolean };
  notes: string[];
}

// 分 → 元（保留两位）。
const yuan = (cents: number): number => cents / 100;

const ORDER_STATUS_TEXT: Record<string, string> = {
  created: '待支付',
  paid: '已支付',
  fulfilled: '已履约',
  refunded: '已退款',
  failed: '失败',
};
const ORDER_STATUS_COLOR: Record<string, string> = {
  created: '#8c8c8c',
  paid: '#1677ff',
  fulfilled: '#52c41a',
  refunded: '#faad14',
  failed: '#ff4d4f',
};

function MoneyCard({ title, cents, footnote }: { title: string; cents: number; footnote?: string }) {
  return (
    <Card size="small">
      <Statistic title={title} value={yuan(cents)} precision={2} prefix="¥" />
      {footnote && (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          {footnote}
        </Typography.Text>
      )}
    </Card>
  );
}

export default function Economy() {
  const [data, setData] = useState<EconomyOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      setData(await adminFetch<EconomyOverview>('/admin/economy/overview'));
    } catch (e) {
      setError(friendlyError(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  // 订单状态分布（饼图）——仅展示计数 > 0 的状态。
  const statusEntries = data
    ? Object.entries(data.orders.byStatus).filter(([, n]) => n > 0)
    : [];
  const orderPieOption = data && {
    tooltip: { trigger: 'item' as const },
    legend: { bottom: 0 },
    series: [
      {
        type: 'pie' as const,
        radius: ['40%', '68%'],
        data: statusEntries.map(([status, n]) => ({
          name: ORDER_STATUS_TEXT[status] ?? status,
          value: n,
          itemStyle: { color: ORDER_STATUS_COLOR[status] ?? '#bfbfbf' },
        })),
      },
    ],
  };

  // 资金流向（柱状，单位元）：充值 / 退款 / 当前余额。
  const fundBarOption = data && {
    tooltip: { trigger: 'axis' as const, valueFormatter: (v: number) => `¥${v.toFixed(2)}` },
    grid: { left: 60, right: 20, top: 20, bottom: 30 },
    xAxis: { type: 'category' as const, data: ['充值', '退款', '当前余额'] },
    yAxis: { type: 'value' as const, name: '元' },
    series: [
      {
        type: 'bar' as const,
        data: [
          { value: yuan(data.recharge.totalCents), itemStyle: { color: '#52c41a' } },
          { value: yuan(data.refund.totalCents), itemStyle: { color: '#ff4d4f' } },
          { value: yuan(data.balance.totalCents), itemStyle: { color: '#1677ff' } },
        ],
      },
    ],
  };

  return (
    <div>
      <Space style={{ marginBottom: 16, width: '100%', justifyContent: 'space-between' }}>
        <Space>
          <Typography.Title level={4} style={{ margin: 0 }}>
            经济运营
          </Typography.Title>
          {data && (
            <Tag color={data.billingEnabled ? 'green' : 'default'}>
              {data.billingEnabled ? '计费进行中' : '暂无充值'}
            </Tag>
          )}
        </Space>
        <Button onClick={load} loading={loading}>
          刷新
        </Button>
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
              <Col xs={12} md={8} lg={6}>
                <MoneyCard title="充值总额" cents={data.recharge.totalCents} footnote={`${formatNumber(data.recharge.count)} 笔充值`} />
              </Col>
              <Col xs={12} md={8} lg={6}>
                <MoneyCard title="退款总额" cents={data.refund.totalCents} footnote={`${formatNumber(data.refund.count)} 笔退款`} />
              </Col>
              <Col xs={12} md={8} lg={6}>
                <MoneyCard title="用户余额合计" cents={data.balance.totalCents} footnote={`${formatNumber(data.balance.wallets)} 个钱包`} />
              </Col>
              <Col xs={12} md={8} lg={6}>
                <MoneyCard
                  title="账本净额"
                  cents={data.ledgerNetCents}
                  footnote={data.ledgerNetCents === data.balance.totalCents ? '双录一致 ✓' : '与余额不一致，请核对'}
                />
              </Col>
              <Col xs={12} md={8} lg={6}>
                <Card size="small">
                  <Statistic title="礼物流水" value={data.gifts.events} suffix="条" />
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    礼物量 {formatNumber(data.gifts.giftCount)} · 覆盖 {formatNumber(data.gifts.worlds)} 个世界
                  </Typography.Text>
                </Card>
              </Col>
              <Col xs={12} md={8} lg={6}>
                <Card size="small">
                  <Statistic title="订单总数" value={data.orders.total} suffix="单" />
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    已履约 {formatNumber(data.orders.byStatus.fulfilled ?? 0)} · 已退款 {formatNumber(data.orders.byStatus.refunded ?? 0)}
                  </Typography.Text>
                </Card>
              </Col>
              <Col xs={12} md={8} lg={6}>
                <Card size="small">
                  <Statistic
                    title="创作者结算"
                    value={data.creatorSettlement.enabled ? '启用' : '未启用'}
                    valueStyle={{ color: '#8c8c8c' }}
                  />
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    另一套账，不在此聚合
                  </Typography.Text>
                </Card>
              </Col>
            </Row>

            <Row gutter={[16, 16]} style={{ marginTop: 16 }}>
              <Col xs={24} lg={12}>
                <Card size="small" title="资金流向（充值 / 退款 / 余额）">
                  {data.recharge.totalCents || data.refund.totalCents || data.balance.totalCents ? (
                    <ReactECharts option={fundBarOption} style={{ height: 320 }} notMerge />
                  ) : (
                    <Empty description="暂无充值数据" />
                  )}
                </Card>
              </Col>
              <Col xs={24} lg={12}>
                <Card size="small" title="订单状态分布">
                  {statusEntries.length ? (
                    <ReactECharts option={orderPieOption} style={{ height: 320 }} notMerge />
                  ) : (
                    <Empty description="暂无订单数据" />
                  )}
                </Card>
              </Col>
            </Row>

            {data.notes?.length > 0 && (
              <List
                style={{ marginTop: 16 }}
                header={<Typography.Text strong>说明</Typography.Text>}
                size="small"
                bordered
                dataSource={data.notes}
                renderItem={(n) => <List.Item>{n}</List.Item>}
              />
            )}
            <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>
              数据为服务端 SQL 只读聚合（账本双录 / 钱包余额 / 礼物流水 / 订单状态）；不进行结算，创作者分成为另一套账（§2.6）。
            </Typography.Paragraph>
          </>
        )
      )}
    </div>
  );
}
