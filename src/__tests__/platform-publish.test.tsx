import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
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
    CloudError,
  };
});

import { cloudFetch } from '../utils/cloudApi';
import CharacterPublish from '../pages/platform/CharacterPublish';
import { usePartnerStore } from '../stores/usePartnerStore';
import { createEmptyCardV2 } from '../utils/characterCardV2';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
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
});
