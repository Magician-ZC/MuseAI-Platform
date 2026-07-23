import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

// mock 云端通道：提供可控 cloudFetch + 真实形态 CloudError（describeCloudError 依赖 instanceof）。
vi.mock('../utils/cloudApi', () => {
  class CloudError extends Error {
    constructor(
      public code: string,
      message: string,
      public status: number,
    ) {
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
import {
  usePlatformStore,
  describeCloudError,
  roomTypeLabel,
  eventTypeMeta,
  moderationMeta,
  provenanceMeta,
} from '../stores/usePlatformStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
  usePlatformStore.setState({ roomTypeFilter: 'idle', roomView: 'stream' });
  usePlatformStore.getState().reset();
});

describe('usePlatformStore — 世界大厅', () => {
  it('loadWorlds 成功：写入列表与 cursor', async () => {
    fetchMock.mockResolvedValueOnce({
      worlds: [{ id: 'w1', roomType: 'idle', title: '云州', status: 'open', visibility: 'official', memberLimit: 10, memberCount: 3, tickPerDay: 3 }],
      nextCursor: 'cur1',
    });
    await usePlatformStore.getState().loadWorlds(true);
    const s = usePlatformStore.getState();
    expect(s.worlds).toHaveLength(1);
    expect(s.worlds[0].title).toBe('云州');
    expect(s.worldsHasMore).toBe(true);
    expect(s.worldsError).toBeNull();
    // 默认 idle：请求带 type=idle
    expect(fetchMock).toHaveBeenCalledWith('/api/worlds?type=idle');
  });

  it('loadWorlds(false) 追加而非替换', async () => {
    fetchMock.mockResolvedValueOnce({ worlds: [{ id: 'w1' }], nextCursor: 'c1' });
    await usePlatformStore.getState().loadWorlds(true);
    fetchMock.mockResolvedValueOnce({ worlds: [{ id: 'w2' }], nextCursor: null });
    await usePlatformStore.getState().loadWorlds(false);
    const s = usePlatformStore.getState();
    expect(s.worlds.map((w) => w.id)).toEqual(['w1', 'w2']);
    expect(s.worldsHasMore).toBe(false);
  });

  it('loadWorlds 失败：优雅降级为友好错误，不抛出、不崩溃', async () => {
    fetchMock.mockRejectedValueOnce(new TypeError('network down'));
    await usePlatformStore.getState().loadWorlds(true);
    const s = usePlatformStore.getState();
    expect(s.worldsLoading).toBe(false);
    expect(s.worldsError).toContain('连接平台失败');
    expect(s.worlds).toHaveLength(0);
  });
});

describe('usePlatformStore — 我的世界 / 日报聚合', () => {
  it('loadReports 按世界聚合，未读角标与 unreadTotal 正确', async () => {
    fetchMock.mockImplementation(async (path: string) => {
      if (path === '/api/me/reports') {
        return {
          reports: [
            { id: 'r3', worldId: 'w1', characterId: 'cA', reportDay: '2026-07-22', opened: false, createdAt: 300 },
            { id: 'r2', worldId: 'w1', characterId: 'cB', reportDay: '2026-07-21', opened: true, createdAt: 200 },
            { id: 'r1', worldId: 'w2', characterId: 'cC', reportDay: '2026-07-20', opened: false, createdAt: 100 },
          ],
          nextCursor: null,
        };
      }
      // enrichWorldTitles
      if (path === '/api/worlds/w1') return { title: '云州世界' };
      if (path === '/api/worlds/w2') return { title: '雾都' };
      throw new Error(`unexpected ${path}`);
    });

    await usePlatformStore.getState().loadReports();
    const s = usePlatformStore.getState();
    expect(s.myWorlds).toHaveLength(2);
    const w1 = s.myWorlds.find((w) => w.worldId === 'w1')!;
    expect(w1.characterIds.sort()).toEqual(['cA', 'cB']);
    expect(w1.totalReports).toBe(2);
    expect(w1.unreadCount).toBe(1);
    // reports 已按 createdAt DESC，最新一份 = r3
    expect(w1.latestReportId).toBe('r3');
    expect(usePlatformStore.getState().unreadTotal()).toBe(2);
  });

  it('loadReports 失败：优雅降级', async () => {
    fetchMock.mockRejectedValueOnce(new Error('boom'));
    await usePlatformStore.getState().loadReports();
    expect(usePlatformStore.getState().reportsError).toContain('连接平台失败');
  });
});

describe('describeCloudError — 稳定错误码 → 友好中文', () => {
  it('鉴权失效', () => {
    expect(describeCloudError(new CloudError('unauthorized', 'x', 401))).toContain('重新登录');
  });
  it('revision 冲突（Conflict 子原因识别）', () => {
    expect(describeCloudError(new CloudError('conflict', '状态冲突: revision', 409))).toContain('世界状态已更新');
  });
  it('world_full 冲突', () => {
    expect(describeCloudError(new CloudError('conflict', '状态冲突: world_full', 409))).toContain('人数已满');
  });
  it('风控拦截', () => {
    expect(describeCloudError(new CloudError('risk_blocked', 'x', 403))).toContain('风控');
  });
  it('非 CloudError（网络失败）→ 连接平台失败', () => {
    expect(describeCloudError(new TypeError('fetch failed'))).toContain('连接平台失败');
  });
});

describe('展示层助手', () => {
  it('roomTypeLabel', () => {
    expect(roomTypeLabel('idle')).toBe('放置房');
    expect(roomTypeLabel('chapter')).toBe('章节房');
    expect(roomTypeLabel('arena')).toBe('赛事房');
  });
  it('eventTypeMeta', () => {
    expect(eventTypeMeta('dialogue').label).toBe('对话');
    expect(eventTypeMeta('consent_request').label).toBe('同意请求');
  });
  it('moderationMeta', () => {
    expect(moderationMeta('approved').label).toBe('已通过');
    expect(moderationMeta('pending').label).toBe('审核中');
  });
  it('provenanceMeta 区分三类来源', () => {
    expect(provenanceMeta('public_fact').label).toBe('公开事实');
    expect(provenanceMeta('private_view').label).toBe('角色私密视角');
    expect(provenanceMeta('model_inference').label).toBe('模型推断');
  });
});
