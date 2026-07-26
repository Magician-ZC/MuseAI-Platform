// 平台世界大厅：首屏先呈现一个“正在发生”的世界，搜索与目录退居次级。
// 布局遵循 docs/design/client-ui-design.md §6：主视觉世界 / 世界动态 / 世界目录 + 右侧辅助栏（发布状态、相关世界、账号提示）。
// 诚实化原则：所有条目只渲染接口真实字段；接口没有的字段一律不显示（空态或整块隐藏），不做按下标硬编码的伪造文案。
import React, { useEffect, useMemo, useState } from 'react';
import { Alert, Button, Empty, Input, Segmented, Spin, Tag } from 'antd';
import {
  BookOutlined,
  CheckCircleOutlined,
  EyeOutlined,
  FireOutlined,
  HeartOutlined,
  RightOutlined,
  TeamOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import {
  eventTypeMeta,
  moderationMeta,
  roomTypeLabel,
  usePlatformStore,
  type Membership,
  type RoomTypeFilter,
  type WorldEventItem,
  type WorldSummary,
  type WorldsSort,
} from '../../stores/usePlatformStore';
import './PlatformHall.css';

const KANE_IMAGE = '/assets/characters/kane-night-oath-portrait.png';
const TAVERN_IMAGE = '/assets/platform/ember-tavern.png';

/** 世界封面位图池（真实位图资产，设计文档 §6 禁止占位框/Emoji/CSS 绘图）。
 * world.coverUrl 缺席时按 world.id 做确定性兜底：同一世界永远同一张封面，不随机、不随渲染次数变化。 */
const WORLD_COVER_POOL = ['/assets/platform/mist-sea-world.png', '/assets/platform/ember-tavern.png'];

const ROOM_OPTIONS = [
  { label: '放置房', value: 'idle' as RoomTypeFilter },
  { label: '章节房（未开放）', value: 'chapter' as RoomTypeFilter, disabled: true },
  { label: '赛事房（未开放）', value: 'arena' as RoomTypeFilter, disabled: true },
];

const SORT_OPTIONS = [
  { label: '最新', value: 'new' as WorldsSort },
  { label: '热门', value: 'hot' as WorldsSort },
];

/** server WorldTemplateView（GET /assets/worlds/mine）：大厅「发布状态」的权威数据源。 */
interface PublishedWorld {
  id: string;
  title: string;
  version: number;
  moderation: string;
  withdrawn: boolean;
  createdAt: number;
}

/** GET /api/me/notifications 的一条。payload 结构随 kind 而异，故按未知对象处理。 */
interface NotificationItem {
  id: string;
  kind: string;
  payload?: Record<string, unknown> | null;
  status?: string;
  createdAt: number;
}

/** 通知类型的中文名。取不到 payload 文案时回落到它，不臆造内容。 */
const NOTIFICATION_KIND_TEXT: Record<string, string> = {
  daily_report: '世界日报已生成',
  consent_request: '有一个不可逆事件等待你的同意',
  consent_reminder: '同意征询即将超时',
};

/** 从 payload 里取一条可读文案；字段名随 kind 而异，逐个试，都没有就回落 kind 中文名。 */
function notificationText(item: NotificationItem): string {
  const p = item.payload ?? {};
  for (const key of ['summary', 'title', 'text', 'message']) {
    const v = p[key];
    if (typeof v === 'string' && v.trim()) return v;
  }
  return NOTIFICATION_KIND_TEXT[item.kind] ?? '你有一条新通知';
}

const DEMO_WORLDS: WorldSummary[] = [
  {
    id: 'mist-sea-age',
    roomType: 'idle',
    title: '雾海纪元',
    status: 'running',
    visibility: 'official',
    memberLimit: 50000,
    memberCount: 12748,
    tickPerDay: 6,
    aiLabel: { visible: true },
    hotScore: 982,
    starRating: 4,
    coverUrl: '/assets/platform/mist-sea-world.png',
  },
  {
    id: 'magic-continent',
    roomType: 'idle',
    title: '魔法大陆设定集',
    status: 'open',
    visibility: 'public',
    memberLimit: 1200,
    memberCount: 327,
    tickPerDay: 4,
    aiLabel: { visible: true },
    starRating: 3,
    coverUrl: '/assets/platform/ember-tavern.png',
  },
  {
    id: 'silent-mountain',
    roomType: 'idle',
    title: '静止山脉',
    status: 'open',
    visibility: 'public',
    memberLimit: 800,
    memberCount: 186,
    tickPerDay: 3,
    aiLabel: { visible: true },
    starRating: 2,
    coverUrl: '/assets/platform/mist-sea-world.png',
  },
];

const DEMO_EVENTS: WorldEventItem[] = [
  {
    id: 'demo-event-1',
    worldId: 'mist-sea-age',
    tick: 481,
    sequence: 1,
    domainEventId: 'demo-domain-1',
    type: 'world',
    actors: ['凯恩·夜誓'],
    visibility: 'public',
    projection: { summary: '北境风暴平息，商路重启。雷德港的风暴在持续三日后逐渐平息，银鸦商团已重新启航。' },
    occurredAt: Date.UTC(2026, 6, 25, 10, 42),
  },
  {
    id: 'demo-event-2',
    worldId: 'mist-sea-age',
    tick: 482,
    sequence: 2,
    domainEventId: 'demo-domain-2',
    type: 'alliance',
    actors: ['艾琳娜·风语者'],
    visibility: 'public',
    projection: { summary: '艾琳娜·风语者加入了银鸦商团。这位来自帝雾林地的风语者将以顾问身份协助航行与气象观测。' },
    occurredAt: Date.UTC(2026, 6, 25, 8, 21),
  },
  {
    id: 'demo-event-3',
    worldId: 'mist-sea-age',
    tick: 483,
    sequence: 3,
    domainEventId: 'demo-domain-3',
    type: 'action',
    actors: ['静止山脉探索队'],
    visibility: 'public',
    projection: { summary: '静止山脉东麓的古道被发现。探索队在碎石封闭的古道旁发现了仍在运转的星象装置。' },
    occurredAt: Date.UTC(2026, 6, 25, 5, 7),
  },
];

const DEMO_NOTIFICATIONS: NotificationItem[] = [
  { id: 'ntf_1', kind: 'daily_report', payload: { summary: '「雾海纪元」第 12 拍日报已生成' }, createdAt: 0 },
  { id: 'ntf_2', kind: 'consent_request', payload: { summary: '林昭的决斗请求等待你的同意' }, createdAt: 0 },
];

const DEMO_PUBLISHED: PublishedWorld[] = [
  {
    id: 'magic-continent',
    title: '魔法大陆设定集',
    version: 3,
    moderation: 'approved',
    withdrawn: false,
    createdAt: Date.UTC(2026, 6, 24, 14, 18),
  },
];

const statusMeta = (status: string): { label: string; className: string } => {
  switch (status) {
    case 'open':
      return { label: '开放中', className: 'is-open' };
    case 'running':
      return { label: '运行中', className: 'is-running' };
    case 'paused':
      return { label: '已暂停', className: 'is-paused' };
    default:
      return { label: status, className: '' };
  }
};

const isDesignPreview = () =>
  import.meta.env.DEV && typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('design') === 'preview';

const formatCount = (value: number) => new Intl.NumberFormat('zh-CN').format(value);

const formatEventTime = (timestamp: number) =>
  new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false }).format(new Date(timestamp));

const formatDay = (timestamp: number) => {
  const date = new Date(timestamp);
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日`;
};

/** 相对时间：完全由事件真实 occurredAt 推导，不按列表下标编造。 */
const relativeTime = (timestamp: number, now: number = Date.now()): string => {
  const diff = now - timestamp;
  if (!Number.isFinite(diff) || diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}小时前`;
  const days = Math.floor(diff / 86_400_000);
  return days === 1 ? '昨天' : `${days}天前`;
};

/** 确定性字符串哈希：仅用于在没有 coverUrl 时稳定挑选一张真实位图封面。 */
const hashString = (value: string): number => {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) {
    hash = (hash * 31 + value.charCodeAt(index)) | 0;
  }
  return Math.abs(hash);
};

const worldCover = (world: WorldSummary): string =>
  world.coverUrl?.trim() || WORLD_COVER_POOL[hashString(world.id) % WORLD_COVER_POOL.length];

/** 事件标题：只取投影里的真实文本（summary 优先，其次 narrative）。 */
const eventHeadline = (event: WorldEventItem): string =>
  event.projection?.summary?.trim() || event.projection?.narrative?.trim() || '';

/** 事件补充描述：narrative 与 summary 不同才展示，避免同一句话重复两遍。 */
const eventDetail = (event: WorldEventItem): string => {
  const summary = event.projection?.summary?.trim() || '';
  const narrative = event.projection?.narrative?.trim() || '';
  return narrative && narrative !== summary ? narrative : '';
};

/** 事件元信息行：事件类型 + 参与角色 + 拍数，全部来自接口字段。 */
const eventMetaLine = (event: WorldEventItem): string => {
  const parts = [eventTypeMeta(event.type).label];
  if (event.actors?.length) parts.push(event.actors.join('、'));
  if (Number.isFinite(event.tick)) parts.push(`第 ${event.tick} 拍`);
  return parts.join(' · ');
};

const WorldCatalogCard: React.FC<{ world: WorldSummary; onEnter: () => void; onSpectate: () => void }> = ({ world, onEnter, onSpectate }) => {
  const status = statusMeta(world.status);
  return (
    <article className="world-catalog-card">
      <img className="world-catalog-card__cover" src={worldCover(world)} alt={`${world.title}的世界封面`} />
      <div className="world-catalog-card__body">
        <div className="world-catalog-card__head">
          <strong>{world.title}</strong>
          <span className={`world-status ${status.className}`}>{status.label}</span>
        </div>
        <div className="world-catalog-card__tags">
          <Tag color="orange">{roomTypeLabel(world.roomType)}</Tag>
          {typeof world.starRating === 'number' && <Tag color="gold">{world.starRating}★</Tag>}
          {world.aiLabel?.visible !== false && <Tag>AI 生成</Tag>}
          {typeof world.hotScore === 'number' && <Tag color="volcano"><FireOutlined /> 热度 {world.hotScore}</Tag>}
        </div>
        <div className="world-catalog-card__meta">
          <span><TeamOutlined /> {world.memberCount}/{world.memberLimit} 角色</span>
          <span><ThunderboltOutlined /> 每日 {world.tickPerDay} 拍</span>
        </div>
        <div className="world-catalog-card__actions">
          <Button type="primary" size="small" onClick={onEnter}>进入世界</Button>
          <Button size="small" icon={<EyeOutlined />} onClick={onSpectate}>观战</Button>
        </div>
      </div>
    </article>
  );
};

const PlatformHall: React.FC = () => {
  const navigate = useNavigate();
  const previewMode = isDesignPreview();
  const {
    roomTypeFilter,
    worldsQuery,
    worldsSort,
    worlds,
    worldsLoading,
    worldsError,
    worldsHasMore,
    memberships,
    setRoomTypeFilter,
    setWorldsQuery,
    setWorldsSort,
    loadWorlds,
    loadMemberships,
  } = usePlatformStore();
  const [searchText, setSearchText] = useState(worldsQuery);
  const [events, setEvents] = useState<WorldEventItem[]>([]);
  const [publishedWorlds, setPublishedWorlds] = useState<PublishedWorld[]>([]);
  const [notifications, setNotifications] = useState<NotificationItem[]>([]);

  useEffect(() => {
    if (previewMode) return;
    void loadWorlds(true);
    void loadMemberships();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [previewMode]);

  // 发布状态：读 /assets/worlds/mine（我提交的世界模板），失败或为空一律走空态，不编造已发布记录。
  useEffect(() => {
    if (previewMode) {
      setPublishedWorlds(DEMO_PUBLISHED);
      return;
    }
    let cancelled = false;
    cloudFetch<PublishedWorld[]>('/api/assets/worlds/mine')
      .then((data) => {
        if (!cancelled) setPublishedWorlds(Array.isArray(data) ? data : []);
      })
      .catch(() => {
        if (!cancelled) setPublishedWorlds([]);
      });
    return () => {
      cancelled = true;
    };
  }, [previewMode]);

  // 站内通知（设计文档 §6「必要的账号提示」）。端点 GET /api/me/notifications 是现成的，
  // 返回 {id, kind, payload, status, createdAt}——payload 结构随 kind 而异，故只取能稳定拿到的
  // 文案字段，取不到就回落 kind 的中文名，不臆造内容。
  useEffect(() => {
    if (previewMode) {
      setNotifications(DEMO_NOTIFICATIONS);
      return;
    }
    let cancelled = false;
    cloudFetch<{ notifications: NotificationItem[] }>('/api/me/notifications')
      .then((data) => {
        if (!cancelled) setNotifications((data.notifications ?? []).slice(0, 3));
      })
      .catch(() => {
        if (!cancelled) setNotifications([]);
      });
    return () => {
      cancelled = true;
    };
  }, [previewMode]);

  const visibleWorlds = previewMode ? DEMO_WORLDS : worlds;
  const featuredWorld = visibleWorlds[0];

  useEffect(() => {
    if (previewMode || !featuredWorld?.id) {
      setEvents(previewMode ? DEMO_EVENTS : []);
      return;
    }
    let cancelled = false;
    cloudFetch<{ events: WorldEventItem[] }>(`/api/worlds/${featuredWorld.id}/events`)
      .then((data) => {
        if (!cancelled) setEvents((data.events ?? []).slice(-3).reverse());
      })
      .catch(() => {
        if (!cancelled) setEvents([]);
      });
    return () => {
      cancelled = true;
    };
  }, [featuredWorld?.id, previewMode]);

  const recentEvents = previewMode ? DEMO_EVENTS : events;
  const featuredStatus = statusMeta(featuredWorld?.status || 'open');
  // 主视觉「最近活动」：直接引用该世界最新一条公开事件的真实摘要；没有事件就不显示这一行。
  const featuredRecap = recentEvents[0] ? eventHeadline(recentEvents[0]) : '';
  const activeMembership: Membership | undefined = previewMode
    ? undefined
    : memberships.find((item) => item.membershipStatus === 'active') ?? memberships[0];
  const latestPublished = useMemo(
    () => publishedWorlds.filter((item) => !item.withdrawn).sort((a, b) => b.createdAt - a.createdAt)[0],
    [publishedWorlds],
  );

  const catalogWorlds = useMemo(() => visibleWorlds.slice(1), [visibleWorlds]);
  // 相关世界：复用已加载的世界列表，取与主视觉世界同房型的其它世界（无则空态，不造数据）。
  const relatedWorlds = useMemo(() => {
    if (!featuredWorld) return [];
    return visibleWorlds.filter((item) => item.id !== featuredWorld.id && item.roomType === featuredWorld.roomType).slice(0, 3);
  }, [visibleWorlds, featuredWorld]);

  if (!previewMode && worldsError && worlds.length === 0) {
    return (
      <div className="platform-hall platform-hall--state">
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={worldsError}
          action={<Button size="small" onClick={() => void loadWorlds(true)}>重试</Button>}
        />
      </div>
    );
  }

  if (!previewMode && worldsLoading && worlds.length === 0) {
    return <div className="platform-hall platform-hall--state"><Spin /></div>;
  }

  if (!featuredWorld) {
    return (
      <div className="platform-hall platform-hall--state">
        <Empty description={worldsQuery ? '没有匹配的世界，换个关键词试试' : '暂无开放世界，稍后再来看看'} />
      </div>
    );
  }

  return (
    <div className="platform-hall">
      <h1 className="sr-only">世界大厅</h1>
      <div className="platform-hall__layout">
        <main className="platform-hall__main">
          <section className="featured-world" aria-labelledby="featured-world-title">
            <img src={worldCover(featuredWorld)} alt={`${featuredWorld.title}的世界封面`} />
            <div className="featured-world__content">
              <h2 id="featured-world-title">{featuredWorld.title}</h2>
              <div className="featured-world__status-row">
                <span className={`world-status ${featuredStatus.className}`}>{featuredStatus.label}</span>
                {typeof featuredWorld.starRating === 'number' && <span className="featured-world__badge">{featuredWorld.starRating}★</span>}
                {worldsSort === 'hot' && typeof featuredWorld.hotScore === 'number' && (
                  <span className="featured-world__badge"><FireOutlined /> 热度 {featuredWorld.hotScore}</span>
                )}
              </div>
              <dl>
                <div>
                  <dt><TeamOutlined /> 参与人数</dt>
                  <dd>{formatCount(featuredWorld.memberCount)} / {formatCount(featuredWorld.memberLimit)}</dd>
                </div>
                <div>
                  <dt><ThunderboltOutlined /> 世界节奏</dt>
                  <dd>
                    每日 {featuredWorld.tickPerDay} 拍
                    {/* 下一拍是服务端估算值，且只在可推算时下发（running + interval 模式）。
                        用「约」字如实表达精度，不装作是精确时刻。 */}
                    {featuredWorld.nextTickEstimatedAt && (
                      <small>下一拍约在 {formatEventTime(featuredWorld.nextTickEstimatedAt)}</small>
                    )}
                  </dd>
                </div>
              </dl>
              {featuredRecap && <p>{featuredRecap}</p>}
              <div className="featured-world__actions">
                <Button type="primary" onClick={() => navigate(`/platform/worlds/${featuredWorld.id}`)}>进入世界</Button>
                <Button onClick={() => navigate(`/platform/worlds/${featuredWorld.id}/spectate`)}>观战</Button>
                <Button onClick={() => navigate('/platform/my')}>我的房间</Button>
              </div>
            </div>
          </section>

          <section className="world-activity" aria-labelledby="world-activity-title">
            <div className="section-heading">
              <h2 id="world-activity-title">世界正在发生</h2>
              <Button type="link" onClick={() => navigate(`/platform/worlds/${featuredWorld.id}/spectate`)}>查看全部 <RightOutlined /></Button>
            </div>
            {recentEvents.length === 0 ? (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="世界尚未产生公开事件" />
            ) : (
              <div className="world-activity__list">
                {recentEvents.slice(0, 3).map((event) => {
                  const detail = eventDetail(event);
                  return (
                    <button key={event.id} type="button" className="world-event" onClick={() => navigate(`/platform/worlds/${featuredWorld.id}/spectate`)}>
                      <span className="world-event__copy">
                        <strong>{eventHeadline(event) || eventTypeMeta(event.type).label}</strong>
                        {detail && <span>{detail}</span>}
                        <small>{eventMetaLine(event)}</small>
                      </span>
                      <time dateTime={new Date(event.occurredAt).toISOString()}>
                        {relativeTime(event.occurredAt)}
                        <span>{formatEventTime(event.occurredAt)}</span>
                      </time>
                    </button>
                  );
                })}
              </div>
            )}
          </section>

          <section className="world-catalog" aria-labelledby="world-catalog-title">
            <div className="section-heading world-catalog__heading">
              <div>
                <h2 id="world-catalog-title">发现更多世界</h2>
                <p>按房型、标题或热度继续探索。</p>
              </div>
              <div className="world-catalog__filters">
                <Segmented options={ROOM_OPTIONS} value={roomTypeFilter} onChange={(value) => void setRoomTypeFilter(value as RoomTypeFilter)} />
                <Input.Search
                  allowClear
                  placeholder="搜索世界标题"
                  value={searchText}
                  onChange={(event) => {
                    const value = event.target.value;
                    setSearchText(value);
                    if (value === '' && worldsQuery !== '') void setWorldsQuery('');
                  }}
                  onSearch={(value) => void setWorldsQuery(value)}
                />
                <Segmented options={SORT_OPTIONS} value={worldsSort} onChange={(value) => void setWorldsSort(value as WorldsSort)} />
              </div>
            </div>
            <div className="world-catalog__grid">
              {catalogWorlds.map((world) => (
                <WorldCatalogCard
                  key={world.id}
                  world={world}
                  onEnter={() => navigate(`/platform/worlds/${world.id}`)}
                  onSpectate={() => navigate(`/platform/worlds/${world.id}/spectate`)}
                />
              ))}
            </div>
            {!previewMode && worldsSort !== 'hot' && worldsHasMore && (
              <Button loading={worldsLoading} onClick={() => void loadWorlds(false)}>加载更多</Button>
            )}
          </section>
        </main>

        <aside className="related-rail" aria-labelledby="related-title">
          <h2 id="related-title">与你有关</h2>

          <section className="related-section related-section--publish">
            <h3>发布状态</h3>
            {latestPublished ? (
              <div className="publish-entry">
                <span className="publish-entry__icon"><BookOutlined /></span>
                <div className="publish-entry__body">
                  <strong>{latestPublished.title}</strong>
                  <small>世界书 · 来源于你的工作室</small>
                  <span className={`publish-entry__state is-${moderationMeta(latestPublished.moderation).color}`}>
                    {latestPublished.moderation === 'approved' && <CheckCircleOutlined />}
                    {moderationMeta(latestPublished.moderation).label} · 第 {latestPublished.version} 版
                  </span>
                  <small>提交于 {formatDay(latestPublished.createdAt)}</small>
                </div>
                <Button size="small" onClick={() => navigate('/platform/worlds/publish')}>管理发布</Button>
              </div>
            ) : (
              <div className="publish-entry__empty">
                <p className="related-empty">还没有发布世界，把本地设定整理好后再来。</p>
                <Button type="primary" size="small" onClick={() => navigate('/platform/worlds/publish')}>发布世界</Button>
              </div>
            )}
          </section>

          <section className="related-section">
            <h3>相关世界</h3>
            {relatedWorlds.length > 0 ? (
              <ul className="related-worlds">
                {relatedWorlds.map((world) => (
                  <li key={world.id}>
                    <button type="button" onClick={() => navigate(`/platform/worlds/${world.id}`)}>
                      {/* 缩略封面为装饰性图像：按钮可访问名已含世界标题，故 alt 留空避免读屏重复。 */}
                      <img src={worldCover(world)} alt="" />
                      <span>
                        <strong>{world.title}</strong>
                        <small>{roomTypeLabel(world.roomType)} · {formatCount(world.memberCount)} 名角色</small>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="related-empty">暂时没有与之同类型的其它世界。</p>
            )}
          </section>

          <section className="related-section">
            <h3>你的羁绊角色</h3>
            {previewMode ? (
              <div className="bonded-character">
                <img src={KANE_IMAGE} alt="凯恩·夜誓的角色头像" />
                <div>
                  <strong>凯恩·夜誓</strong>
                  <span><HeartOutlined /> 羁绊值 68</span>
                  <small>所在世界：魔法大陆设定集</small>
                  <Button onClick={() => navigate('/platform/bonds')}>查看羁绊</Button>
                </div>
              </div>
            ) : activeMembership ? (
              // 头像仅在 server 下发时渲染（未过审头像 server 侧不下发，前端无需再判）；
              // 无头像走 --compact 布局，不留空占位框。
              <div className={`bonded-character${activeMembership.avatarUrl ? '' : ' bonded-character--compact'}`}>
                {activeMembership.avatarUrl && (
                  <img src={activeMembership.avatarUrl} alt={`${activeMembership.characterName}的立绘`} />
                )}
                <div>
                  <strong>{activeMembership.characterName}</strong>
                  <span>{activeMembership.worldTitle || activeMembership.worldId}</span>
                  {/* 有真实互动时刻就显示它，否则回落到加入时间——两者语义不同，不混用同一句文案。 */}
                  <small>
                    {activeMembership.lastActiveAt
                      ? `最近互动 ${relativeTime(activeMembership.lastActiveAt)}`
                      : `加入于 ${formatDay(activeMembership.joinedAt)}`}
                  </small>
                  <Button onClick={() => navigate('/platform/bonds')}>查看羁绊</Button>
                </div>
              </div>
            ) : (
              <p className="related-empty">投放角色后，与你相关的羁绊会出现在这里。</p>
            )}
          </section>

          <section className="related-section">
            <h3>最新通知</h3>
            {notifications.length > 0 ? (
              <ul className="notification-list">
                {notifications.map((item) => (
                  <li key={item.id}>
                    <span className="notification-text">{notificationText(item)}</span>
                    {item.createdAt > 0 && (
                      <time dateTime={new Date(item.createdAt).toISOString()}>{relativeTime(item.createdAt)}</time>
                    )}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="related-empty">暂时没有新通知。</p>
            )}
          </section>

          <section className="related-section">
            <h3>房间邀请 {previewMode && <span className="invitation-count">1</span>}</h3>
            {previewMode ? (
              <>
                <div className="room-invitation">
                  <img src={TAVERN_IMAGE} alt="星火酒馆的昏暖室内" />
                  <div>
                    <strong>星火酒馆</strong>
                    <span>房主：墨白</span>
                    <p>讨论北境风暴后的商路计划</p>
                  </div>
                </div>
                <div className="room-invitation__actions">
                  <Button>忽略</Button>
                  <Button type="primary" onClick={() => navigate(`/platform/worlds/${featuredWorld.id}`)}>加入房间</Button>
                </div>
              </>
            ) : (
              <p className="related-empty">暂无新的房间邀请。</p>
            )}
          </section>
        </aside>
      </div>
    </div>
  );
};

export default PlatformHall;
