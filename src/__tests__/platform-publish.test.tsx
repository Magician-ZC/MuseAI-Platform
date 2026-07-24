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
    getPlatformBase: vi.fn(() => 'http://test'),
    setPlatformBase: vi.fn(),
    uploadAvatar: vi.fn(),
    resolveObjectUrl: vi.fn((u?: string | null) => (u ? `http://test${u}` : undefined)),
    CloudError,
  };
});

// 压缩用真实 canvas 在 jsdom 不可用 → 整体 mock，返回固定 base64，测试不依赖真实渲染。
vi.mock('../utils/imageAvatar', () => ({
  ACCEPTED_AVATAR_MIME: ['image/png', 'image/jpeg', 'image/webp'],
  compressAvatarImage: vi.fn(async () => ({ imageBase64: 'BASE64DATA', mime: 'image/png' })),
}));

import { cloudFetch, uploadAvatar } from '../utils/cloudApi';
import CharacterPublish from '../pages/platform/CharacterPublish';
import { usePartnerStore } from '../stores/usePartnerStore';
import { createEmptyCardV2 } from '../utils/characterCardV2';

const fetchMock = cloudFetch as unknown as Mock;
const uploadAvatarMock = uploadAvatar as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  uploadAvatarMock.mockReset();
  const card = createEmptyCardV2('沈霜');
  usePartnerStore.setState({ characterCardsV2: [card] });
});

const renderPublish = () =>
  render(
    <MemoryRouter>
      <CharacterPublish />
    </MemoryRouter>,
  );

describe('CharacterPublish', () => {
  it('选卡 → 权利声明 → 发布，展示审核态', async () => {
    fetchMock.mockImplementation(async (path: string, opts?: { method?: string }) => {
      if (path === '/api/assets/characters/mine') return [];
      if (path === '/api/assets/characters' && opts?.method === 'POST') {
        return {
          id: 'cc1',
          localCardId: 'lc',
          version: 1,
          rightsDeclaration: 'original',
          moderation: 'pending',
          withdrawn: false,
          createdAt: 1,
        };
      }
      throw new Error(`unexpected ${path}`);
    });

    renderPublish();

    // 选中左侧角色卡
    fireEvent.click(await screen.findByText('沈霜'));
    // 勾选权利声明
    fireEvent.click(screen.getByRole('checkbox'));
    // 发布
    fireEvent.click(screen.getByRole('button', { name: /发布此版本/ }));

    // 审核态回显（moderation=pending → 审核中）
    expect(await screen.findByText(/审核中/)).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(
        ([p, o]) => p === '/api/assets/characters' && (o as { method?: string })?.method === 'POST',
      ),
    ).toBe(true);
  });

  it('我的云端版本加载失败：优雅降级', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/assets/characters/mine') throw new TypeError('offline');
      throw new Error(`unexpected ${path}`);
    });

    renderPublish();
    expect(await screen.findByText('连接平台失败')).toBeInTheDocument();
  });

  it('头像上传：压缩 → uploadAvatar 携带 base64 → 过审后回显', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/assets/characters/mine') {
        return [
          {
            id: 'cc1',
            localCardId: '沈霜卡',
            version: 1,
            rightsDeclaration: 'original',
            moderation: 'approved',
            withdrawn: false,
            createdAt: 1,
          },
        ];
      }
      throw new Error(`unexpected ${path}`);
    });
    uploadAvatarMock.mockResolvedValue({
      avatarUrl: '/api/assets/objects/avatars/cc1.png',
      moderation: 'approved',
    });

    const { container } = renderPublish();

    // 等云端角色加载出来（表格「本地卡」单元格精确文本）→ 头像区 effect 默认选中它、上传按钮可用。
    await screen.findByText('沈霜卡');
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /上传头像/ })).not.toBeDisabled(),
    );

    const fileInput = container.querySelector('input[type="file"]') as HTMLInputElement;
    expect(fileInput).toBeTruthy();
    const file = new File([new Uint8Array([1, 2, 3])], 'a.png', { type: 'image/png' });
    fireEvent.change(fileInput, { target: { files: [file] } });

    // uploadAvatar 收到目标 id + 压缩后的纯 base64 + mime。
    await screen.findByText('头像已通过审核并更新');
    expect(uploadAvatarMock).toHaveBeenCalledWith('cc1', 'BASE64DATA', 'image/png');
  });
});
