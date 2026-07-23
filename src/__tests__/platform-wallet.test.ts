import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

// 保留真实 CloudError 类，让 store 与 describeCloudError 的 instanceof 判定一致。
vi.mock('../utils/cloudApi', () => {
  class CloudError extends Error {
    constructor(public code: string, message: string, public status: number) {
      super(message);
    }
  }
  return {
    cloudFetch: vi.fn(),
    cloudStream: vi.fn(() => () => {}),
    getPlatformBase: vi.fn(() => 'http://test'),
    setPlatformBase: vi.fn(),
    CloudError,
  };
});

import { cloudFetch, CloudError } from '../utils/cloudApi';
import { useWalletStore, type WalletOrder } from '../stores/useWalletStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  useWalletStore.setState({ balanceCents: 0, loaded: false, loading: false, error: null, orders: [] });
});

describe('useWalletStore — P4b 真实钱包（对接 billing）', () => {
  it('loadBalance：读取服务端余额（GET /billing/balance，服务端权威）', async () => {
    fetchMock.mockResolvedValueOnce({ balanceCents: 12345 });
    await useWalletStore.getState().loadBalance();
    const s = useWalletStore.getState();
    expect(s.balanceCents).toBe(12345);
    expect(s.loaded).toBe(true);
    expect(fetchMock).toHaveBeenCalledWith('/api/billing/balance');
  });

  it('recharge：POST /billing/orders 带 idempotent，DevPayment 成功后回写余额 + 记本地订单回执', async () => {
    fetchMock.mockResolvedValueOnce({ orderId: 'order_1', balanceCents: 5000 });
    const r = await useWalletStore.getState().recharge(5000);
    expect(r.ok).toBe(true);
    const s = useWalletStore.getState();
    expect(s.balanceCents).toBe(5000);
    expect(s.orders[0]).toMatchObject({ orderId: 'order_1', amountCents: 5000, status: 'fulfilled', kind: 'recharge' });
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/billing/orders',
      expect.objectContaining({ method: 'POST', idempotent: true, body: { kind: 'recharge', amountCents: 5000 } }),
    );
  });

  it('recharge：未成年拒充（403 forbidden）→ minorForbidden + 未成年人保护提示，不改余额', async () => {
    fetchMock.mockRejectedValueOnce(new CloudError('forbidden', 'forbidden', 403));
    const r = await useWalletStore.getState().recharge(5000);
    expect(r.ok).toBe(false);
    expect(r.minorForbidden).toBe(true);
    const s = useWalletStore.getState();
    expect(s.error).toContain('未成年');
    expect(s.balanceCents).toBe(0);
  });

  it('recharge：金额非法前置拦截（不发起请求）', async () => {
    const r = await useWalletStore.getState().recharge(0);
    expect(r.ok).toBe(false);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('refund：POST /billing/refunds 带 idempotent，退款成功后订单标记 refunded + 回写余额', async () => {
    const seed: WalletOrder = { orderId: 'order_1', amountCents: 5000, kind: 'recharge', status: 'fulfilled', createdAt: 1 };
    useWalletStore.setState({ orders: [seed], balanceCents: 5000, loaded: true });
    fetchMock.mockResolvedValueOnce({ orderId: 'order_1', status: 'refunded', balanceCents: 0 });
    const r = await useWalletStore.getState().refund('order_1');
    expect(r.ok).toBe(true);
    const s = useWalletStore.getState();
    expect(s.orders[0].status).toBe('refunded');
    expect(s.balanceCents).toBe(0);
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/billing/refunds',
      expect.objectContaining({ method: 'POST', idempotent: true, body: { orderId: 'order_1' } }),
    );
  });

  it('云端故障优雅降级：loadBalance 失败只记错误、不抛异常', async () => {
    fetchMock.mockRejectedValueOnce(new Error('network down'));
    await expect(useWalletStore.getState().loadBalance()).resolves.toBeUndefined();
    expect(useWalletStore.getState().error).toBeTruthy();
  });

  it('红线：store 绝不暴露 withdraw / transfer / cashout（余额不可提现不可转账）', () => {
    const keys = Object.keys(useWalletStore.getState());
    expect(keys).not.toContain('withdraw');
    expect(keys).not.toContain('transfer');
    expect(keys).not.toContain('cashout');
  });
});
