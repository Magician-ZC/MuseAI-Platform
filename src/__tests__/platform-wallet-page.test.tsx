import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router-dom';

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
import Wallet from '../pages/platform/Wallet';
import { useWalletStore } from '../stores/useWalletStore';

const fetchMock = cloudFetch as unknown as Mock;

function renderWallet() {
  return render(
    <MemoryRouter initialEntries={['/platform/wallet']}>
      <Routes>
        <Route path="/platform/wallet" element={<Wallet />} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  fetchMock.mockReset();
  useWalletStore.setState({ balanceCents: 0, loaded: false, loading: false, error: null, orders: [] });
});

describe('Wallet 页面 — 余额 / 充值 / 红线', () => {
  it('展示余额与「余额不可提现不可转账」红线', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/billing/balance') return { balanceCents: 8888 };
      throw new Error(`unexpected ${path}`);
    });
    const { container } = renderWallet();
    // 红线明示
    expect(await screen.findByText('余额不可提现、不可转账')).toBeInTheDocument();
    // 余额到账后展示 88.88
    await waitFor(() => expect(container.textContent).toContain('88.88'));
  });

  it('充值：确认后 POST /billing/orders（idempotent），成功回写并提示', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/billing/balance') return { balanceCents: 0 };
      if (path === '/api/billing/orders') return { orderId: 'order_9', balanceCents: 3000 };
      throw new Error(`unexpected ${path}`);
    });
    renderWallet();
    // 默认金额 ¥30；点击确认充值
    const btn = await screen.findByRole('button', { name: /确认充值/ });
    fireEvent.click(btn);
    expect(await screen.findByText(/充值成功/)).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/billing/orders',
      expect.objectContaining({ method: 'POST', idempotent: true, body: { kind: 'recharge', amountCents: 3000 } }),
    );
    // 订单回执出现
    expect(await screen.findByText('已到账')).toBeInTheDocument();
  });

  it('未成年拒充（403）→ 未成年人保护友好提示', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/billing/balance') return { balanceCents: 0 };
      if (path === '/api/billing/orders') throw new CloudError('forbidden', 'forbidden', 403);
      throw new Error(`unexpected ${path}`);
    });
    renderWallet();
    fireEvent.click(await screen.findByRole('button', { name: /确认充值/ }));
    expect(await screen.findByText('未成年人保护：暂不支持充值')).toBeInTheDocument();
  });

  it('云端故障优雅降级：余额加载失败显示错误卡（页面不崩）', async () => {
    fetchMock.mockImplementation(async () => {
      throw new Error('network down');
    });
    renderWallet();
    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
    // 红线文案仍在（页面结构完整未崩）
    expect(screen.getByText('余额不可提现、不可转账')).toBeInTheDocument();
  });
});
