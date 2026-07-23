// 经济运营：P4a 未启用计费，读 /admin/economy/overview 显示占位卡（§2.6）。
import { useEffect, useState } from 'react';
import { Alert, Card, Descriptions, List, Result, Spin, Tag, Typography } from 'antd';
import { adminFetch } from '../api';
import { ErrorAlert, friendlyError } from '../components/shared';

interface EconomyOverview {
  stage: string;
  billingEnabled: boolean;
  message: string;
  orders: { total: number; paid: number; refunded: number };
  userBalances: { totalCents: number };
  creatorSettlement: { enabled: boolean };
  notes: string[];
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

  return (
    <div>
      <Typography.Title level={4}>经济运营</Typography.Title>
      {error && <ErrorAlert message={error} onRetry={load} />}
      {loading ? (
        <div style={{ textAlign: 'center', marginTop: 80 }}>
          <Spin />
        </div>
      ) : (
        data && (
          <Card>
            <Result
              status="info"
              title={`当前阶段：${data.stage}　${data.billingEnabled ? '计费已启用' : '未启用计费'}`}
              subTitle={data.message}
            />
            <Descriptions
              bordered
              column={2}
              size="small"
              style={{ marginTop: 8 }}
              items={[
                { key: 'billing', label: '计费开关', children: <Tag color={data.billingEnabled ? 'green' : 'default'}>{data.billingEnabled ? '开启' : '关闭'}</Tag> },
                { key: 'settle', label: '创作者结算', children: <Tag color={data.creatorSettlement.enabled ? 'green' : 'default'}>{data.creatorSettlement.enabled ? '启用' : '未启用'}</Tag> },
                { key: 'orders', label: '订单(总/已付/退款)', children: `${data.orders.total} / ${data.orders.paid} / ${data.orders.refunded}` },
                { key: 'balance', label: '用户余额合计(分)', children: data.userBalances.totalCents },
              ]}
            />
            <Alert
              type="warning"
              showIcon
              style={{ marginTop: 16 }}
              message="P4b 获批后再增加订单 / 退款 / 对账（feature=billing 编译）"
            />
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
          </Card>
        )
      )}
    </div>
  );
}
