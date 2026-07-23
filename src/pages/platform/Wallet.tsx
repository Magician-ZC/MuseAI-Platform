// 钱包（P4b，FE1 所有；规格 §2.6 经济红线）：余额展示 + 充值 + 订单/退款记录。
// 红线 UI（写进文案）：余额**不可提现、不可转账、不可兑换胜负结果**；仅用于平台内过程性消费。
// 充值走 POST /billing/orders（cloudFetch idempotent:true 带 Idempotency-Key，DevPayment 即时成功→回写余额）。
// 未成年拒充（server 403）→ 未成年人保护友好提示。Local-first：云端故障显示错误卡，页面不崩。
import React, { useEffect, useState } from 'react';
import {
  Typography,
  Card,
  Button,
  InputNumber,
  Space,
  Alert,
  Tag,
  List,
  Spin,
  Statistic,
  Divider,
  Empty,
} from 'antd';
import {
  WalletOutlined,
  ReloadOutlined,
  LockOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons';
import { useWalletStore, formatYuan, type WalletOrder } from '../../stores/useWalletStore';

const { Title, Text, Paragraph } = Typography;

const PRESETS_YUAN = [6, 30, 68, 128, 328];

function fmtTime(ms: number): string {
  try {
    return new Date(ms).toLocaleString('zh-CN', { hour12: false });
  } catch {
    return '';
  }
}

const OrderRow: React.FC<{
  order: WalletOrder;
  refunding: boolean;
  onRefund: (id: string) => void;
}> = ({ order, refunding, onRefund }) => {
  const refunded = order.status === 'refunded';
  return (
    <List.Item
      style={{ paddingInline: 0 }}
      actions={[
        refunded ? (
          <Tag key="s" color="default">
            已退款
          </Tag>
        ) : (
          <Button key="r" size="small" danger loading={refunding} onClick={() => onRefund(order.orderId)}>
            申请退款
          </Button>
        ),
      ]}
    >
      <List.Item.Meta
        title={
          <Space size={8}>
            <Text strong>{formatYuan(order.amountCents)}</Text>
            <Tag color={refunded ? 'default' : 'green'}>{refunded ? '已退款' : '已到账'}</Tag>
          </Space>
        }
        description={
          <Text type="secondary" style={{ fontSize: 12 }}>
            充值 · 订单 {order.orderId} · {fmtTime(order.createdAt)}
          </Text>
        }
      />
    </List.Item>
  );
};

const Wallet: React.FC = () => {
  const balanceCents = useWalletStore((s) => s.balanceCents);
  const loaded = useWalletStore((s) => s.loaded);
  const loading = useWalletStore((s) => s.loading);
  const error = useWalletStore((s) => s.error);
  const orders = useWalletStore((s) => s.orders);
  const loadBalance = useWalletStore((s) => s.loadBalance);
  const recharge = useWalletStore((s) => s.recharge);
  const refund = useWalletStore((s) => s.refund);

  const [amountYuan, setAmountYuan] = useState<number | null>(30);
  const [submitting, setSubmitting] = useState(false);
  const [feedback, setFeedback] = useState<{ type: 'success' | 'error' | 'warning'; text: string } | null>(null);
  const [minorBlocked, setMinorBlocked] = useState(false);
  const [refundingId, setRefundingId] = useState<string | null>(null);

  useEffect(() => {
    void loadBalance();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const submitRecharge = async () => {
    if (!amountYuan || amountYuan <= 0) {
      setFeedback({ type: 'warning', text: '请输入有效的充值金额' });
      return;
    }
    setSubmitting(true);
    setFeedback(null);
    setMinorBlocked(false);
    const cents = Math.round(amountYuan * 100);
    const r = await recharge(cents);
    if (r.ok) {
      setFeedback({ type: 'success', text: `充值成功，${formatYuan(cents)} 已到账` });
    } else if (r.minorForbidden) {
      setMinorBlocked(true);
    } else {
      setFeedback({ type: 'error', text: r.error || '充值失败，请稍后重试' });
    }
    setSubmitting(false);
  };

  const submitRefund = async (orderId: string) => {
    setRefundingId(orderId);
    setFeedback(null);
    const r = await refund(orderId);
    if (r.ok) {
      setFeedback({ type: 'success', text: '退款成功，金额已原路退回余额' });
    } else {
      setFeedback({ type: 'error', text: r.error || '退款失败，请稍后重试' });
    }
    setRefundingId(null);
  };

  return (
    <div style={{ padding: '28px 40px', maxWidth: 760, margin: '0 auto' }}>
      <div style={{ marginBottom: 16 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <WalletOutlined style={{ color: '#d97757', marginRight: 10 }} />
          钱包
        </Title>
        <Text type="secondary">用于平台内过程性消费的账户余额。</Text>
      </div>

      {/* 红线：余额不可提现不可转账（始终明示） */}
      <Alert
        type="info"
        showIcon
        icon={<LockOutlined />}
        style={{ marginBottom: 16, background: '#faf9f5', border: '1px solid #eae6df' }}
        message="余额不可提现、不可转账"
        description="平台余额仅用于世界内过程性消费（如道具、复活赛资格等），不可提现、不可转账，也不可兑换随机或胜负结果。充值即代表你已知悉本条边界。"
      />

      {/* 未成年拒充友好提示 */}
      {minorBlocked && (
        <Alert
          type="warning"
          showIcon
          icon={<SafetyCertificateOutlined />}
          style={{ marginBottom: 16 }}
          message="未成年人保护：暂不支持充值"
          description="按未成年人保护要求，当前账号暂不支持充值。若为误判，请核对账号资料中的年龄声明后再试。"
          closable
          onClose={() => setMinorBlocked(false)}
        />
      )}

      {/* 云端故障优雅降级：错误卡而非崩溃 */}
      {error && !minorBlocked && (
        <Alert
          type="error"
          showIcon
          style={{ marginBottom: 16 }}
          message="连接平台失败"
          description={error}
          action={
            <Button size="small" onClick={() => void loadBalance()}>
              重试
            </Button>
          }
        />
      )}

      {/* 余额 */}
      <Card
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        <Space style={{ justifyContent: 'space-between', width: '100%' }} align="center">
          <Statistic
            title="当前余额"
            value={loaded ? balanceCents / 100 : 0}
            precision={2}
            prefix="¥"
            valueStyle={{ color: '#33312e' }}
          />
          <Button
            icon={<ReloadOutlined />}
            loading={loading}
            onClick={() => void loadBalance()}
            aria-label="刷新余额"
          >
            刷新
          </Button>
        </Space>
      </Card>

      {/* 充值 */}
      <Card
        title="充值"
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        <Space direction="vertical" size={14} style={{ width: '100%' }}>
          <Space size={8} wrap>
            {PRESETS_YUAN.map((y) => (
              <Button
                key={y}
                type={amountYuan === y ? 'primary' : 'default'}
                onClick={() => setAmountYuan(y)}
              >
                {formatYuan(y * 100)}
              </Button>
            ))}
          </Space>
          <Space size={10} align="center" wrap>
            <Text type="secondary">自定义金额</Text>
            <InputNumber
              min={1}
              max={100000}
              precision={2}
              value={amountYuan}
              onChange={(v) => setAmountYuan(typeof v === 'number' ? v : null)}
              prefix="¥"
              style={{ width: 160 }}
              aria-label="充值金额"
            />
          </Space>

          {feedback && <Alert type={feedback.type} showIcon message={feedback.text} />}

          <Button
            type="primary"
            loading={submitting}
            disabled={!amountYuan || amountYuan <= 0}
            onClick={() => void submitRecharge()}
          >
            确认充值 {amountYuan ? formatYuan(Math.round(amountYuan * 100)) : ''}
          </Button>
          <Text type="secondary" style={{ fontSize: 12 }}>
            由第三方支付渠道履约；开发态即时到账。你购买的是平台内的过程性消费额度，不是任何结果或胜负。
          </Text>
        </Space>
      </Card>

      {/* 订单 / 退款记录（本机回执） */}
      <Card
        title="充值与退款记录"
        style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        {loading && orders.length === 0 ? (
          <div style={{ textAlign: 'center', padding: 24 }}>
            <Spin />
          </div>
        ) : orders.length === 0 ? (
          <Empty description="还没有充值记录" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <List
            dataSource={orders}
            rowKey={(o) => o.orderId}
            renderItem={(o) => (
              <OrderRow order={o} refunding={refundingId === o.orderId} onRefund={submitRefund} />
            )}
          />
        )}
        <Divider style={{ margin: '12px 0' }} />
        <Paragraph type="secondary" style={{ margin: 0, fontSize: 12 }}>
          记录为本机回执，仅供查询与退款入口；权威账目以平台服务端为准。仅已履约（未消费）的充值订单可原路退款。
        </Paragraph>
      </Card>
    </div>
  );
};

export default Wallet;
