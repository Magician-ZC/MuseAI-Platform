import React, { useEffect, useState } from 'react';
import { Alert, Button, Input, Space, Tag } from 'antd';
import {
  CheckOutlined,
  CloseOutlined,
  EyeInvisibleOutlined,
  LinkOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
  SendOutlined,
  ShoppingOutlined,
  UserSwitchOutlined,
} from '@ant-design/icons';
import { cloudFetch } from '../../../utils/cloudApi';
import { formatJourneyTime, journeyError, previewContext } from './journeyData';
import {
  JourneyContextBar,
  JourneyPage,
  JourneyState,
  useJourneyContext,
  useJourneyPreview,
} from './JourneyShared';

interface SocialBond {
  characterId: string;
  characterName: string;
  eligible?: boolean;
  eligibility?: boolean;
  unlockStatus?: string;
  reasons?: string[];
  positiveBond?: boolean;
  sharedDeath?: boolean;
}

interface UnlockRequest {
  id: string;
  worldId: string;
  fromCharacterId: string;
  fromCharacterName: string;
  status: string;
  expiresAt: number;
  createdAt: number;
}

interface IdentityItem {
  unlockId: string;
  worldId: string;
  counterpartCharacterId: string;
  counterpartCharacterName: string;
  identity: { userId: string; nickname: string };
  unlockedAt: number;
}

export const JourneySocial: React.FC = () => {
  const preview = useJourneyPreview();
  const { context, memberships, loading: contextLoading, error: contextError, changeWorld, reload } = useJourneyContext();
  const [bonds, setBonds] = useState<SocialBond[]>(preview ? [{ characterId: 'char-elena', characterName: '艾琳娜·风语', eligible: true, unlockStatus: 'none', positiveBond: true, sharedDeath: true }] : []);
  const [requests, setRequests] = useState<UnlockRequest[]>(preview ? [{ id: 'unlock-01', worldId: previewContext.worldId, fromCharacterId: 'char-elena', fromCharacterName: '艾琳娜·风语', status: 'pending', createdAt: Date.now() - 1_800_000, expiresAt: Date.now() + 86_400_000 }] : []);
  const [identities, setIdentities] = useState<IdentityItem[]>([]);
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = async () => {
    if (!context || preview) return;
    setLoading(true); setError(null);
    try {
      const [bondData, requestData, identityData] = await Promise.all([
        cloudFetch<{ bonds: SocialBond[] }>(`/api/worlds/${context.worldId}/social/bonds`),
        cloudFetch<{ requests: UnlockRequest[] }>('/api/me/social/unlock-requests?status=pending'),
        cloudFetch<{ identities: IdentityItem[] }>('/api/me/social/identities'),
      ]);
      setBonds(bondData.bonds ?? []); setRequests(requestData.requests ?? []); setIdentities(identityData.identities ?? []);
    } catch (err) { setError(journeyError(err)); } finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, [context?.worldId]); // eslint-disable-line react-hooks/exhaustive-deps

  const requestUnlock = async (bond: SocialBond) => {
    if (!context) return;
    setBusy(bond.characterId); setError(null);
    try {
      if (!preview) await cloudFetch(`/api/worlds/${context.worldId}/social/unlock-requests`, { method: 'POST', idempotent: true, body: { targetCharacterId: bond.characterId } });
      setBonds((items) => items.map((item) => item.characterId === bond.characterId ? { ...item, unlockStatus: 'pending' } : item));
      setNotice(`已向 ${bond.characterName} 发出双向身份解锁请求。对方接受前，你们仍只看得到角色面具。`);
    } catch (err) { setError(journeyError(err)); } finally { setBusy(null); }
  };

  const respond = async (request: UnlockRequest, accept: boolean) => {
    setBusy(request.id); setError(null);
    try {
      if (!preview) await cloudFetch(`/api/me/social/unlock-requests/${request.id}/respond`, { method: 'POST', idempotent: true, body: { accept } });
      setRequests((items) => items.filter((item) => item.id !== request.id));
      if (accept) {
        setIdentities((items) => [{ unlockId: request.id, worldId: request.worldId, counterpartCharacterId: request.fromCharacterId, counterpartCharacterName: request.fromCharacterName, identity: { userId: 'private-preview', nickname: '苏晚' }, unlockedAt: Date.now() }, ...items]);
        setNotice('双方身份已解锁。任意一方拉黑后，身份可见性会立即收回。');
      }
    } catch (err) { setError(journeyError(err)); } finally { setBusy(null); }
  };

  const selected = bonds[0];
  return (
    <JourneyPage title="解锁真实身份" description="只有成年用户、达成社交资格且双方明确同意后，角色背后的昵称才会彼此可见。" wide>
      <JourneyState loading={contextLoading} error={contextError} empty={!context} emptyText="参与多人世界并建立羁绊后，身份解锁会在这里出现" onRetry={reload}>
        <JourneyContextBar context={context} memberships={memberships} onChange={changeWorld} />
        {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
        {notice && <Alert type="success" showIcon title={notice} closable onClose={() => setNotice(null)} style={{ marginBottom: 16 }} />}
        <JourneyState loading={loading} error={!bonds.length ? error : null} empty={!bonds.length} emptyText="当前世界没有符合社交资格的角色关系">
          <div className="journey-grid">
            <section className="journey-panel journey-panel--8">
              <div className="journey-social-pair">
                <div className="journey-social-profile"><img src="/assets/characters/kane-night-oath-portrait.png" alt={`${context?.characterName}角色肖像`} /><strong>{context?.characterName}</strong><span>你的角色面具</span></div>
                <div className="journey-social-link"><LinkOutlined /></div>
                <div className="journey-social-profile"><img src="/assets/characters/elena-windwhisper-portrait.png" alt={`${selected?.characterName || '对端角色'}角色肖像`} /><strong>{selected?.characterName || '等待羁绊'}</strong><span>对方角色面具</span></div>
              </div>
              {selected && (
                <div style={{ marginTop: 24 }}>
                  <div className="journey-radio-grid">
                    <div className="journey-notice"><CheckOutlined /> 正向羁绊已达成</div>
                    <div className="journey-notice"><CheckOutlined /> 共同经历满足资格</div>
                    <div className="journey-notice"><EyeInvisibleOutlined /> 当前仍隐藏真人身份</div>
                  </div>
                  <div className="journey-panel__footer"><Button type="primary" size="large" icon={<UserSwitchOutlined />} loading={busy === selected.characterId} disabled={selected.unlockStatus === 'pending'} onClick={() => void requestUnlock(selected)}>{selected.unlockStatus === 'pending' ? '等待对方回应' : '请求双向解锁'}</Button></div>
                </div>
              )}
            </section>
            <aside className="journey-panel journey-panel--4">
              <h2>待我回应</h2>
              {requests.length ? <div className="journey-list">{requests.map((request) => <article className="journey-list-item" key={request.id}><div className="journey-list-item__body"><h3>{request.fromCharacterName}</h3><p>希望解锁双方昵称 · {formatJourneyTime(request.createdAt)}</p></div><div className="journey-list-item__actions"><Button icon={<CloseOutlined />} onClick={() => void respond(request, false)}>拒绝</Button><Button type="primary" icon={<CheckOutlined />} loading={busy === request.id} onClick={() => void respond(request, true)}>接受</Button></div></article>)}</div> : <p className="journey-panel__intro">暂时没有新的身份解锁请求。</p>}
              {identities.length > 0 && <><h2 style={{ marginTop: 24 }}>已解锁</h2>{identities.map((item) => <div className="journey-list-item" key={item.unlockId}><div className="journey-list-item__body"><h3>{item.counterpartCharacterName}</h3><p>昵称：{item.identity.nickname || '未设置'} · {formatJourneyTime(item.unlockedAt)}</p></div><Tag color="success">双方同意</Tag></div>)}</>}
              <div className="journey-notice" style={{ marginTop: 18 }}>不会显示手机号等强身份信息；任意一方拉黑都会即时撤回身份可见性。</div>
            </aside>
          </div>
        </JourneyState>
      </JourneyState>
    </JourneyPage>
  );
};

interface OfflineGain { id?: string; kind?: string; summary?: string; createdAt?: number; characterId?: string }
interface BackpackEntry { backpackId: string; status: string; item: { id: string; narrative?: string; effectTags?: string[] } }

export const JourneyChapters: React.FC = () => {
  const preview = useJourneyPreview();
  const { context, memberships, loading: contextLoading, error: contextError, changeWorld, reload } = useJourneyContext();
  const [gains, setGains] = useState<OfflineGain[]>(preview ? [{ id: 'gain-01', kind: 'training', summary: '在离线夹层里完成了三次潮汐辨向训练，记住了灯塔守望者留下的暗号。', createdAt: Date.now() - 3_600_000 }] : []);
  const [items, setItems] = useState<BackpackEntry[]>(preview ? [{ backpackId: 'bp-compass', status: 'owned', item: { id: 'mist-sea-compass', narrative: '雾海罗盘：靠近被遗忘的航路时，赤铜指针会轻轻震动。', effectTags: ['导航', '雾海'] } }] : []);
  const [selectedItems, setSelectedItems] = useState<string[]>(preview ? ['mist-sea-compass'] : []);
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = async () => {
    if (!context || preview) return;
    setLoading(true); setError(null);
    try {
      const [gainData, backpackData] = await Promise.all([
        cloudFetch<{ gains: OfflineGain[] }>(`/api/worlds/${context.worldId}/offline-gains`),
        cloudFetch<{ items?: BackpackEntry[]; backpack?: BackpackEntry[] }>('/api/me/backpack'),
      ]);
      setGains(gainData.gains ?? []); setItems(backpackData.items ?? backpackData.backpack ?? []);
    } catch (err) { setError(journeyError(err)); } finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, [context?.worldId]); // eslint-disable-line react-hooks/exhaustive-deps

  const action = async (kind: 'start' | 'finish' | 'carry') => {
    if (!context) return;
    setBusy(kind); setError(null); setNotice(null);
    try {
      if (!preview) {
        const path = kind === 'carry' ? `/api/worlds/${context.worldId}/carry` : `/api/worlds/${context.worldId}/chapters/${kind}`;
        await cloudFetch(path, { method: 'POST', idempotent: true, body: kind === 'carry' ? { itemIds: selectedItems } : undefined });
      }
      setNotice(kind === 'start' ? '章节会话已开始，世界已安排下一拍。' : kind === 'finish' ? '本章已结算；新的离线夹层训练已开始。' : '携带声明已提交，服务端会按目标世界规则逐件判定。');
    } catch (err) { setError(journeyError(err)); } finally { setBusy(null); }
  };

  const compass = items.find((item) => item.item.id === 'mist-sea-compass') ?? items[0];
  return (
    <JourneyPage title="欢迎回到故事里" description="章节房把离线成长、主线推进与跨世界物品重新接回同一段旅程。" wide>
      <JourneyState loading={contextLoading} error={contextError} empty={!context} emptyText="加入一个章节房后即可查看离线成长" onRetry={reload}>
        <JourneyContextBar context={context} memberships={memberships} onChange={changeWorld} />
        {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
        {notice && <Alert type="success" showIcon title={notice} closable onClose={() => setNotice(null)} style={{ marginBottom: 16 }} />}
        <JourneyState loading={loading}>
          <div className="journey-grid">
            <section className="journey-panel journey-panel--7">
              <div className="journey-item-showcase">
                <img src="/assets/journey/mist-sea-compass.png" alt="雾海罗盘物品图" />
                <div><Tag color="gold">跨世界物品</Tag><h2>{compass?.item.id === 'mist-sea-compass' ? '雾海罗盘' : compass?.item.id || '旅程信物'}</h2><p>{compass?.item.narrative || '这个章节还没有可携带的物品。'}</p><Space wrap>{(compass?.item.effectTags ?? []).map((tag) => <Tag key={tag}>{tag}</Tag>)}</Space></div>
              </div>
              {items.length > 0 && <div style={{ marginTop: 20 }}><h3>选择入场携带</h3><div className="journey-list">{items.map((item) => <label className="journey-list-item" key={item.backpackId} style={{ cursor: 'pointer' }}><input type="checkbox" checked={selectedItems.includes(item.item.id)} onChange={() => setSelectedItems((current) => current.includes(item.item.id) ? current.filter((id) => id !== item.item.id) : [...current, item.item.id])} /><div className="journey-list-item__body"><h3>{item.item.id}</h3><p>{item.item.narrative}</p></div></label>)}</div><div className="journey-panel__footer"><Button icon={<ShoppingOutlined />} loading={busy === 'carry'} disabled={!selectedItems.length} onClick={() => void action('carry')}>声明携带 {selectedItems.length} 件物品</Button></div></div>}
            </section>
            <aside className="journey-panel journey-panel--5">
              <h2>离线期间</h2>
              <p className="journey-panel__intro">离线收益是角色自动训练与探索的可读摘要，不会替你自动作出重大剧情选择。</p>
              <div className="journey-timeline">{gains.map((gain, index) => <div className="journey-timeline__item" key={gain.id || index}><span className="journey-timeline__dot" /><div className="journey-timeline__content"><h3>{gain.kind === 'training' ? '角色训练' : '离线探索'}</h3><p>{gain.summary || '角色在离线夹层里继续积累经验。'}</p></div></div>)}</div>
              {!gains.length && <p className="journey-panel__intro">尚无离线收益。完成一次章节结算后，这里会开始记录。</p>}
              <Space style={{ marginTop: 18 }} wrap><Button type="primary" icon={<PlayCircleOutlined />} loading={busy === 'start'} onClick={() => void action('start')}>继续章节</Button><Button icon={<CheckOutlined />} loading={busy === 'finish'} onClick={() => void action('finish')}>结算本章</Button></Space>
            </aside>
          </div>
        </JourneyState>
      </JourneyState>
    </JourneyPage>
  );
};

interface LiveSession { id: string; worldId: string; title: string; status: string; startsAt: number; endsAt?: number; delayTicks: number; capacity: number; viewerCount?: number; broadcast?: { publishedThroughTick?: number | null; worldTickNow?: number | null; pendingTicks?: number } }
interface LiveEvent { id: string; tick: number; sequence?: number; type: string; actors?: string[]; summary?: string; projection?: { summary?: string }; occurredAt?: number }
interface Danmaku { id: string; displayName?: string; body: string; anchorTick?: number; createdAt: number }

const previewSession: LiveSession = { id: 'live-mist-01', worldId: previewContext.worldId, title: '雾海纪元 · 灯塔终夜', status: 'live', startsAt: Date.now() - 2_700_000, delayTicks: 2, capacity: 500, viewerCount: 168, broadcast: { publishedThroughTick: 47, worldTickNow: 49, pendingTicks: 2 } };
const previewEvents: LiveEvent[] = [
  { id: 'event-45', tick: 45, type: 'world', actors: ['凯恩·夜誓'], projection: { summary: '旧港上空的雾墙第一次出现了裂缝，远处灯塔亮起三短一长的求援信号。' } },
  { id: 'event-46', tick: 46, type: 'dialogue', actors: ['艾琳娜·风语'], projection: { summary: '艾琳娜把那封未寄出的信塞进凯恩手中：“别让潮汐替我们决定结局。”' } },
  { id: 'event-47', tick: 47, type: 'choice', actors: ['凯恩·夜誓'], projection: { summary: '凯恩转身走向灯塔。他没有回头，但把罗盘留在了旧港的界碑上。' } },
];

export const JourneyLive: React.FC = () => {
  const preview = useJourneyPreview();
  const [sessions, setSessions] = useState<LiveSession[]>(preview ? [previewSession] : []);
  const [selectedId, setSelectedId] = useState(preview ? previewSession.id : '');
  const [detail, setDetail] = useState<LiveSession | null>(preview ? previewSession : null);
  const [events, setEvents] = useState<LiveEvent[]>(preview ? previewEvents : []);
  const [danmaku, setDanmaku] = useState<Danmaku[]>(preview ? [{ id: 'dan-01', displayName: '旅人 17', body: '那封信终于到了。', anchorTick: 46, createdAt: Date.now() - 120_000 }] : []);
  const [text, setText] = useState('');
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadSessions = async () => {
    if (preview) return;
    setLoading(true); setError(null);
    try {
      const data = await cloudFetch<{ sessions: LiveSession[] }>('/api/live/sessions?status=all&limit=20');
      const list = data.sessions ?? []; setSessions(list); setSelectedId((current) => current || list[0]?.id || '');
    } catch (err) { setError(journeyError(err)); } finally { setLoading(false); }
  };
  useEffect(() => { void loadSessions(); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const loadDetail = async () => {
    if (!selectedId || preview) return;
    setLoading(true); setError(null);
    try {
      const [sessionData, feedData, danmakuData] = await Promise.all([
        cloudFetch<LiveSession>(`/api/live/sessions/${selectedId}`),
        cloudFetch<{ events: LiveEvent[] }>(`/api/live/sessions/${selectedId}/feed?limit=50`),
        cloudFetch<{ danmaku: Danmaku[] }>(`/api/live/sessions/${selectedId}/danmaku?limit=50`),
      ]);
      setDetail(sessionData); setEvents(feedData.events ?? []); setDanmaku(danmakuData.danmaku ?? []);
    } catch (err) { setError(journeyError(err)); } finally { setLoading(false); }
  };
  useEffect(() => { void loadDetail(); }, [selectedId]); // eslint-disable-line react-hooks/exhaustive-deps

  const send = async () => {
    if (!selectedId || !text.trim()) return;
    setBusy(true); setError(null);
    try {
      const created = preview ? { id: `dan-${Date.now()}`, displayName: '林逸', body: text.trim(), anchorTick: detail?.broadcast?.publishedThroughTick ?? undefined, createdAt: Date.now() } : await cloudFetch<Danmaku>(`/api/live/sessions/${selectedId}/danmaku`, { method: 'POST', idempotent: true, body: { body: text.trim() } });
      setDanmaku((items) => [...items, created]); setText('');
    } catch (err) { setError(journeyError(err)); } finally { setBusy(false); }
  };

  const current = detail ?? sessions.find((session) => session.id === selectedId) ?? null;
  const eventSummary = (event: LiveEvent) => event.projection?.summary || event.summary || '这一拍的公开投影已落定。';
  return (
    <JourneyPage title="今夜开演" description="公开舞台使用延迟缓冲播出已落定的世界投影；观众看到的是有审核窗口的节目流，而不是伪装成零延迟的世界事实。" wide action={<Button icon={<ReloadOutlined />} onClick={() => void (selectedId ? loadDetail() : loadSessions())}>刷新播出</Button>}>
      {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
      <JourneyState loading={loading && !current} error={!current ? error : null} empty={!current} emptyText="当前没有已预告的公开舞台" onRetry={loadSessions}>
        <div className="journey-grid">
          <section className="journey-panel journey-panel--8 journey-stage">
            <div className="journey-stage__media">
              <img src="/assets/platform/mist-sea-world.png" alt="雾海纪元直播舞台" />
              <div className="journey-stage__content"><span className="journey-stage__pulse">{current?.status === 'live' ? '正在播出' : '节目单'}</span><h2>{current?.title}</h2><p>已播至第 {current?.broadcast?.publishedThroughTick ?? '—'} 拍 · 世界当前第 {current?.broadcast?.worldTickNow ?? '—'} 拍 · 内容审核缓冲 {current?.delayTicks ?? 0} 拍</p><Space wrap><Tag color="volcano">{current?.viewerCount ?? 0} 人观看</Tag><Tag color="default">AI 生成内容</Tag></Space></div>
            </div>
          </section>
          <aside className="journey-panel journey-panel--4">
            <h2>观众席</h2>
            <div className="journey-list" style={{ maxHeight: 240, overflowY: 'auto' }}>{danmaku.map((item) => <div className="journey-list-item" key={item.id}><div className="journey-list-item__body"><h3>{item.displayName || '匿名旅人'}</h3><p>{item.body}</p></div>{item.anchorTick !== undefined && <Tag>第 {item.anchorTick} 拍</Tag>}</div>)}</div>
            <Space.Compact style={{ width: '100%', marginTop: 14 }}><Input value={text} maxLength={120} onChange={(event) => setText(event.target.value)} onPressEnter={() => void send()} placeholder="发送一条公开弹幕…" aria-label="公开弹幕" /><Button type="primary" icon={<SendOutlined />} loading={busy} disabled={!text.trim()} onClick={() => void send()} /></Space.Compact>
            <div className="journey-notice" style={{ marginTop: 14 }}>弹幕绑定已公开拍号，经过内容安全检查；不会带出你的真人身份。</div>
          </aside>
          <section className="journey-panel journey-panel--8">
            <h2>播出事件</h2>
            <div className="journey-stage__feed">{events.map((event) => <article className="journey-stage-event" key={event.id}><span>第 {event.tick} 拍</span><div><strong>{event.actors?.join('、') || '世界'}</strong><p className="journey-panel__intro" style={{ margin: '4px 0 0' }}>{eventSummary(event)}</p></div></article>)}</div>
          </section>
          <aside className="journey-panel journey-panel--4">
            <h2>节目单</h2>
            <div className="journey-list">{sessions.map((session) => <button type="button" className="journey-list-item" key={session.id} onClick={() => setSelectedId(session.id)} style={{ width: '100%', textAlign: 'left', cursor: 'pointer' }}><div className="journey-list-item__body"><h3>{session.title}</h3><p>{formatJourneyTime(session.startsAt)} · 延迟 {session.delayTicks} 拍</p></div><Tag color={session.status === 'live' ? 'volcano' : 'default'}>{session.status === 'live' ? '直播中' : session.status}</Tag></button>)}</div>
          </aside>
        </div>
      </JourneyState>
    </JourneyPage>
  );
};
