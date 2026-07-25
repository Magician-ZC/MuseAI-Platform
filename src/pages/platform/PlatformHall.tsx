// 平台世界大厅：首屏先呈现一个“正在发生”的世界，搜索与目录退居次级。
import React, { useEffect, useMemo, useState } from 'react';
import { Alert, Button, Empty, Input, Segmented, Spin, Tag } from 'antd';
import {
  BookOutlined,
  CheckCircleOutlined,
  ClockCircleOutlined,
  EyeOutlined,
  FireOutlined,
  HeartOutlined,
  MessageOutlined,
  RightOutlined,
  TeamOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import {
  roomTypeLabel,
  usePlatformStore,
  type MyWorldEntry,
  type RoomTypeFilter,
  type WorldEventItem,
  type WorldSummary,
  type WorldsSort,
} from '../../stores/usePlatformStore';
import './PlatformHall.css';

const HERO_IMAGE = '/assets/platform/mist-sea-world.png';
const KANE_IMAGE = '/assets/characters/kane-night-oath-portrait.png';
const ELENA_IMAGE = '/assets/characters/elena-windwhisper-portrait.png';
const TAVERN_IMAGE = '/assets/platform/ember-tavern.png';

const ROOM_OPTIONS = [
  { label: '放置房', value: 'idle' as RoomTypeFilter },
  { label: '章节房（未开放）', value: 'chapter' as RoomTypeFilter, disabled: true },
  { label: '赛事房（未开放）', value: 'arena' as RoomTypeFilter, disabled: true },
];

const SORT_OPTIONS = [
  { label: '最新', value: 'new' as WorldsSort },
  { label: '热门', value: 'hot' as WorldsSort },
];

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

const DEMO_MY_WORLD: MyWorldEntry = {
  worldId: 'magic-continent',
  characterIds: ['kane-night-oath'],
  unreadCount: 0,
  totalReports: 12,
  latestReportId: 'report-demo',
  latestReportDay: '2026-07-24',
};

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

const nextWorldTickLabel = () => {
  const now = new Date();
  return `${now.getFullYear()}年${now.getMonth() + 1}月${now.getDate()}日 20:00`;
};

const eventTitle = (event: WorldEventItem, index: number) => {
  if (index === 0) return '北境风暴平息，商路重启';
  if (index === 1) return `${event.actors[0] || '新角色'}加入了「银鸦商团」`;
  return '静止山脉东麓的古道被发现';
};

const eventImage = (index: number) => (index === 0 ? KANE_IMAGE : index === 1 ? ELENA_IMAGE : HERO_IMAGE);

const WorldCatalogCard: React.FC<{ world: WorldSummary; onEnter: () => void; onSpectate: () => void }> = ({ world, onEnter, onSpectate }) => {
  const status = statusMeta(world.status);
  return (
    <article className="world-catalog-card">
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
    myWorlds,
    worldTitles,
    memberships,
    setRoomTypeFilter,
    setWorldsQuery,
    setWorldsSort,
    loadWorlds,
    loadReports,
    loadMemberships,
  } = usePlatformStore();
  const [searchText, setSearchText] = useState(worldsQuery);
  const [events, setEvents] = useState<WorldEventItem[]>([]);

  useEffect(() => {
    if (previewMode) return;
    void loadWorlds(true);
    void loadReports();
    void loadMemberships();
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
  const myWorld = previewMode ? DEMO_MY_WORLD : myWorlds[0];
  const myWorldTitle = previewMode ? '魔法大陆设定集' : myWorld ? worldTitles[myWorld.worldId] || myWorld.worldId : '';
  const bondedCharacter = previewMode ? '凯恩·夜誓' : memberships[0]?.characterName;
  const featuredStatus = statusMeta(featuredWorld?.status || 'open');

  const catalogWorlds = useMemo(() => visibleWorlds.slice(1), [visibleWorlds]);

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
            <img src={HERO_IMAGE} alt="雾海中矗立的奇幻城堡与山脉" />
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
                  <dd>{formatCount(featuredWorld.memberCount)}</dd>
                </div>
                <div>
                  <dt><ClockCircleOutlined /> 下次世界时刻</dt>
                  <dd>{nextWorldTickLabel()}</dd>
                </div>
              </dl>
              <p>你离开后，北境的风暴平息，商团重新打通了雷德港的航线。</p>
              <div className="featured-world__actions">
                <Button type="primary" onClick={() => navigate(`/platform/worlds/${featuredWorld.id}`)}>进入世界</Button>
                <Button onClick={() => navigate('/background')}>继续编辑</Button>
                <Button onClick={() => navigate('/platform/my')}>查看我的发布</Button>
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
                {recentEvents.slice(0, 3).map((event, index) => (
                  <button key={event.id} type="button" className="world-event" onClick={() => navigate(`/platform/worlds/${featuredWorld.id}/spectate`)}>
                    <img src={eventImage(index)} alt="" />
                    <span className="world-event__copy">
                      <strong>{eventTitle(event, index)}</strong>
                      <span>{event.projection?.summary || event.projection?.narrative || '世界发生了新的变化。'}</span>
                      <small>{index === 0 ? '世界事件 · 雷德港 · 商团' : index === 1 ? '角色动态 · 风语者' : '探索发现 · 静止山脉'}</small>
                    </span>
                    <time dateTime={new Date(event.occurredAt).toISOString()}>
                      {index === 0 ? '1小时前' : index === 1 ? '3小时前' : '6小时前'}
                      <span>{formatEventTime(event.occurredAt)}</span>
                    </time>
                  </button>
                ))}
              </div>
            )}
          </section>

          <section className="publish-status" aria-labelledby="publish-status-title">
            <h2 id="publish-status-title">你的发布状态</h2>
            {myWorld ? (
              <div className="publish-status__row">
                <span className="publish-status__icon"><BookOutlined /></span>
                <span className="publish-status__title">
                  <strong>{myWorldTitle}</strong>
                  <small>世界书 · 来源于你的工作室</small>
                </span>
                <span className="publish-status__state"><CheckCircleOutlined /> 已发布<small>公开可见</small></span>
                <span className="publish-status__metric"><small>浏览</small>{previewMode ? 327 : myWorld.totalReports}</span>
                <span className="publish-status__metric"><small>收藏</small>{previewMode ? 68 : myWorld.characterIds.length}</span>
                <span className="publish-status__updated">更新于 2026年7月24日 22:18</span>
                <Button onClick={() => navigate('/background')}>继续编辑</Button>
              </div>
            ) : (
              <div className="publish-status__empty">
                <span>还没有发布世界，把本地设定整理好后再来。</span>
                <Button type="primary" onClick={() => navigate('/platform/worlds/publish')}>发布世界</Button>
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
          <section className="related-section">
            <h3>你的羁绊角色</h3>
            {bondedCharacter ? (
              <div className="bonded-character">
                <img src={KANE_IMAGE} alt={`${bondedCharacter}的角色头像`} />
                <div>
                  <strong>{bondedCharacter}</strong>
                  <span><HeartOutlined /> 羁绊值 {previewMode ? 68 : '—'}</span>
                  <small>上次互动：今天 14:32</small>
                  <Button onClick={() => navigate('/chat')}>继续对话</Button>
                </div>
              </div>
            ) : (
              <p className="related-empty">投放角色后，与你相关的羁绊会出现在这里。</p>
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

          <section className="related-section related-section--activity">
            <h3>最新互动</h3>
            <ul>
              <li><MessageOutlined /><span>有人在你的房间留言</span><time>2小时前</time></li>
              <li><HeartOutlined /><span>艾琳娜·风语者关注了你</span><time>5小时前</time></li>
              <li><BookOutlined /><span>你的发布获得 {previewMode ? 327 : myWorld?.totalReports || 0} 次浏览</span><time>昨日</time></li>
            </ul>
          </section>
        </aside>
      </div>
    </div>
  );
};

export default PlatformHall;
