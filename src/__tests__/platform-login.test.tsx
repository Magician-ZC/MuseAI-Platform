import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';

vi.mock('../utils/cloudApi', () => {
  class CloudError extends Error {
    constructor(public code: string, message: string, public status: number) {
      super(message);
    }
  }
  return {
    cloudFetch: vi.fn(),
    cloudStream: vi.fn(() => () => {}),
    getPlatformBase: vi.fn(() => 'http://127.0.0.1:8787'),
    setPlatformBase: vi.fn(),
    CloudError,
  };
});

import { cloudFetch, CloudError } from '../utils/cloudApi';
import PlatformLogin from '../pages/platform/PlatformLogin';
import { useAuthStore } from '../stores/useAuthStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  useAuthStore.getState().logout();
});

const renderLogin = () =>
  render(
    <MemoryRouter initialEntries={['/platform/login']}>
      <PlatformLogin />
    </MemoryRouter>,
  );

describe('PlatformLogin', () => {
  it('获取验证码：调用 /auth/challenge，dev 模式回显验证码', async () => {
    fetchMock.mockResolvedValueOnce({ challengeId: 'chal1', expiresAt: Date.now() + 300000, devCode: '123456' });
    renderLogin();

    fireEvent.change(screen.getByLabelText('手机号'), { target: { value: '13800000000' } });
    fireEvent.click(screen.getByRole('button', { name: /获取验证码/ }));

    expect(await screen.findByText(/123456/)).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/auth/challenge',
      expect.objectContaining({ method: 'POST', body: { phone: '13800000000' } }),
    );
  });

  it('登录成功：写入 useAuthStore 会话', async () => {
    fetchMock.mockResolvedValueOnce({
      accessToken: 'at-1',
      refreshToken: 'rt-1',
      user: { id: 'u1', phone: '13800000000', nickname: '', ageDeclared: 0, status: 'active' },
    });
    renderLogin();

    fireEvent.change(screen.getByLabelText('手机号'), { target: { value: '13800000000' } });
    fireEvent.change(screen.getByLabelText('验证码'), { target: { value: '654321' } });
    fireEvent.click(screen.getByRole('button', { name: /登录 \/ 注册/ }));

    await waitFor(() => {
      expect(useAuthStore.getState().accessToken).toBe('at-1');
    });
    expect(useAuthStore.getState().user?.phone).toBe('13800000000');
  });

  it('验证码错误：优雅提示，不崩溃', async () => {
    fetchMock.mockRejectedValueOnce(new CloudError('bad_request', '请求无效: 验证码错误', 400));
    renderLogin();

    fireEvent.change(screen.getByLabelText('手机号'), { target: { value: '13800000000' } });
    fireEvent.change(screen.getByLabelText('验证码'), { target: { value: '000000' } });
    fireEvent.click(screen.getByRole('button', { name: /登录 \/ 注册/ }));

    expect(await screen.findByText(/验证码错误/)).toBeInTheDocument();
    expect(useAuthStore.getState().accessToken).toBeNull();
  });
});
