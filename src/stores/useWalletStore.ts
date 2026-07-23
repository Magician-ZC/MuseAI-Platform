// 钱包占位（C1，agent-C1 所有）：P4a 无充值、无平台币、无分成（规格 §2.6 商业化阶段门）。
// 本 store 仅提供「未启用」的只读占位状态，明确不做任何余额写入 / 支付接口；
// 真实钱包在 P4b 支付立项后另行设计（用户余额与创作者结算是两套账，不混用同一 wallet 概念）。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';

export type WalletStatus = 'disabled';

interface WalletState {
  /** P4a 恒为 false：钱包能力未启用。 */
  enabled: boolean;
  /** 占位余额，恒为 0（不可提现、不可转账、不可兑换）。 */
  balance: number;
  status: WalletStatus;
  /** 面向用户的状态文案。 */
  statusLabel: () => string;
}

export const useWalletStore = create<WalletState>()(
  persist(
    (): WalletState => ({
      enabled: false,
      balance: 0,
      status: 'disabled',
      statusLabel: () => '未启用',
    }),
    {
      name: 'museai-wallet',
      version: 1,
      storage: createJSONStorage(() => localStorage),
      // 只持久化占位标记；不缓存任何金额（P4a 无经济系统）。
      partialize: () => ({ enabled: false, balance: 0, status: 'disabled' }) as WalletState,
      migrate: (persisted) => persisted as WalletState,
    },
  ),
);
