// 用户钱包（P4b，FE1 所有）：对接 server billing —— 余额 / 充值 / 退款。
// 经济红线（规格 §2.6，写进实现）：余额**不可提现、不可转账、不可兑换胜负结果**；
//   本 store 只调用 /billing/{orders,balance,refunds}，**绝无** withdraw / transfer / cashout。
// 两套账：用户钱包与创作者结算不混用同一 wallet 概念，本 store 只做用户侧。
// 权威边界：余额以服务端为权威（每次进入重新拉取，**不持久化金额**）；
//   订单为**客户端本地回执**（server 未提供订单列表端点），仅用于退款入口与展示，非权威账本。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
import { cloudFetch, CloudError } from '../utils/cloudApi';
import { describeCloudError } from './usePlatformStore';

/** 单笔充值上限（分）——与 server MAX_RECHARGE_CENTS 对齐，仅为更友好的前置提示；最终以服务端为准。 */
export const MAX_RECHARGE_CENTS = 10_000_000; // 10 万元

/** 未成年拒充（server 对 age_declared==2 返回 403）→ 面向用户的未成年人保护提示。 */
export const MINOR_FORBIDDEN_HINT =
  '按未成年人保护要求，当前账号暂不支持充值。若为误判，请核对账号资料中的年龄声明。';

/** 分 → 人民币元（展示用；余额不可提现，仅作阅读态金额呈现）。 */
export function formatYuan(cents: number): string {
  return `¥${(cents / 100).toFixed(2)}`;
}

export interface WalletOrder {
  orderId: string;
  amountCents: number;
  kind: 'recharge';
  status: 'fulfilled' | 'refunded';
  createdAt: number;
}

export interface WalletActionResult {
  ok: boolean;
  error?: string;
  /** 未成年拒充（403）：供页面给出更明确的未成年人保护提示。 */
  minorForbidden?: boolean;
}

interface WalletState {
  /** 余额（分）；服务端权威，不持久化。 */
  balanceCents: number;
  /** 是否已成功读到服务端余额（区分「未加载」与「余额为 0」）。 */
  loaded: boolean;
  loading: boolean;
  error: string | null;
  /** 本地订单回执（充值 / 退款）；非权威账本，仅本机可见。 */
  orders: WalletOrder[];

  /** GET /billing/balance —— 拉取当前余额（服务端权威）。 */
  loadBalance: () => Promise<void>;
  /** POST /billing/orders {kind:"recharge",amountCents} + Idempotency-Key —— DevPayment 立即成功后回写余额。 */
  recharge: (amountCents: number) => Promise<WalletActionResult>;
  /** POST /billing/refunds {orderId} + Idempotency-Key —— 仅已履约订单可退，幂等。 */
  refund: (orderId: string) => Promise<WalletActionResult>;
  reset: () => void;
  // 红线：本 store 不存在 withdraw / transfer / cashout —— 余额不可提现、不可转账。
}

const initialState = {
  balanceCents: 0,
  loaded: false,
  loading: false,
  error: null as string | null,
  orders: [] as WalletOrder[],
};

export const useWalletStore = create<WalletState>()(
  persist(
    (set, get) => ({
      ...initialState,

      loadBalance: async () => {
        set({ loading: true, error: null });
        try {
          const resp = await cloudFetch<{ balanceCents: number }>('/api/billing/balance');
          set({ balanceCents: resp.balanceCents ?? 0, loaded: true, loading: false });
        } catch (e) {
          // 云端故障优雅降级：保留本地已知状态，仅记错误，页面显示错误卡不崩溃。
          set({ loading: false, error: describeCloudError(e) });
        }
      },

      recharge: async (amountCents) => {
        if (!Number.isInteger(amountCents) || amountCents <= 0) {
          const error = '请输入有效的充值金额';
          set({ error });
          return { ok: false, error };
        }
        if (amountCents > MAX_RECHARGE_CENTS) {
          const error = `单笔充值不可超过 ${formatYuan(MAX_RECHARGE_CENTS)}`;
          set({ error });
          return { ok: false, error };
        }
        set({ loading: true, error: null });
        try {
          // idempotent:true → cloudFetch 自动附 Idempotency-Key，失败重试不双扣；DevPayment 立即成功。
          const resp = await cloudFetch<{ orderId: string; balanceCents: number }>('/api/billing/orders', {
            method: 'POST',
            idempotent: true,
            body: { kind: 'recharge', amountCents },
          });
          const order: WalletOrder = {
            orderId: resp.orderId,
            amountCents,
            kind: 'recharge',
            status: 'fulfilled',
            createdAt: Date.now(),
          };
          set((s) => ({
            balanceCents: resp.balanceCents,
            loaded: true,
            loading: false,
            orders: [order, ...s.orders],
          }));
          return { ok: true };
        } catch (e) {
          // 未成年拒充：server 返回 403（forbidden）→ 专用未成年人保护提示。
          const minorForbidden = e instanceof CloudError && e.code === 'forbidden';
          const error = minorForbidden ? MINOR_FORBIDDEN_HINT : describeCloudError(e);
          set({ loading: false, error });
          return { ok: false, error, minorForbidden };
        }
      },

      refund: async (orderId) => {
        if (!orderId.trim()) {
          const error = '缺少订单号';
          set({ error });
          return { ok: false, error };
        }
        set({ loading: true, error: null });
        try {
          const resp = await cloudFetch<{ orderId: string; status: string; balanceCents: number }>(
            '/api/billing/refunds',
            { method: 'POST', idempotent: true, body: { orderId } },
          );
          set((s) => ({
            balanceCents: resp.balanceCents,
            loaded: true,
            loading: false,
            orders: s.orders.map((o) =>
              o.orderId === orderId && resp.status === 'refunded' ? { ...o, status: 'refunded' } : o,
            ),
          }));
          return { ok: true };
        } catch (e) {
          const error = describeCloudError(e);
          set({ loading: false, error });
          return { ok: false, error };
        }
      },

      reset: () => set({ ...initialState, orders: get().orders }),
    }),
    {
      name: 'museai-wallet',
      version: 2,
      storage: createJSONStorage(() => localStorage),
      // 仅持久化本地订单回执；绝不缓存余额（服务端权威）。
      partialize: (s) => ({ orders: s.orders }) as WalletState,
      // v1 占位（enabled/balance/status）→ v2 真实钱包：丢弃占位字段，订单从空开始。
      migrate: (persisted, version) => {
        if (version < 2 || !persisted || typeof persisted !== 'object') {
          return { orders: [] } as unknown as WalletState;
        }
        const p = persisted as { orders?: WalletOrder[] };
        return { orders: Array.isArray(p.orders) ? p.orders : [] } as unknown as WalletState;
      },
    },
  ),
);
