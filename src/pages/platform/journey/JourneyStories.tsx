import React, { useEffect, useMemo, useState } from 'react';
import { Alert, Button, Checkbox, Input, Modal, Select, Space, Tag } from 'antd';
import {
  ApartmentOutlined,
  BookOutlined,
  FireOutlined,
  HourglassOutlined,
  LockOutlined,
  PlusOutlined,
} from '@ant-design/icons';
import { cloudFetch, resolveObjectUrl } from '../../../utils/cloudApi';
import {
  formatJourneyTime,
  journeyError,
  previewCards,
  previewContext,
  type SubplotCard,
} from './journeyData';
import {
  JourneyContextBar,
  JourneyPage,
  JourneyState,
  StarRating,
  useJourneyContext,
  useJourneyPreview,
} from './JourneyShared';

interface ForkPoints {
  eligible: boolean;
  ineligibleReason?: string | null;
  supportedForkPoints: Array<{ kind: string; tickNo: number; stateRevision: number; stateFidelity: string; desc?: string }>;
  cost: { subplotCards: number; note?: string };
}

interface IflineItem {
  id: string;
  status: string;
  characterId: string;
  premise?: string | null;
  createdAt: number;
  forkPoint?: { kind: string; tickNo: number; stateFidelity: string };
  progress?: { beatCount?: number; maxBeats?: number };
  advance?: { pending?: boolean; lastError?: string | null };
  ending?: { label?: string | null };
  affectsOriginWorld: boolean;
}

const previewFork: ForkPoints = {
  eligible: true,
  supportedForkPoints: [{ kind: 'terminal', tickNo: 47, stateRevision: 47, stateFidelity: 'origin_terminal', desc: '原世界终局态的完整复制。' }],
  cost: { subplotCards: 1, note: '显式消耗一张在手副本卡。' },
};

export const JourneyIfline: React.FC = () => {
  const preview = useJourneyPreview();
  const { context, memberships, loading: contextLoading, error: contextError, changeWorld, reload } = useJourneyContext();
  const [fork, setFork] = useState<ForkPoints | null>(preview ? previewFork : null);
  const [cards, setCards] = useState<SubplotCard[]>(preview ? previewCards : []);
  const [iflines, setIflines] = useState<IflineItem[]>(preview ? [{
    id: 'ifline-01', status: 'running', characterId: previewContext.characterId,
    premise: '如果凯恩在终局前收到了那封迟到的信？', createdAt: Date.now() - 86_400_000,
    forkPoint: { kind: 'terminal', tickNo: 47, stateFidelity: 'origin_terminal' }, progress: { beatCount: 2, maxBeats: 12 },
    advance: { pending: false }, affectsOriginWorld: false,
  }] : []);
  const [premise, setPremise] = useState('如果凯恩在终局前收到了那封迟到的信？');
  const [selectedCards, setSelectedCards] = useState<string[]>(previewCards.slice(0, 1).map((card) => card.id));
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = async () => {
    if (!context || preview) return;
    setLoading(true);
    setError(null);
    try {
      const [forkData, lineData, cardData] = await Promise.all([
        cloudFetch<ForkPoints>(`/api/worlds/${context.worldId}/ifline-fork-points`),
        cloudFetch<{ items: IflineItem[] }>('/api/me/iflines?limit=50&offset=0'),
        cloudFetch<{ cards: SubplotCard[] }>('/api/me/subplot-cards?status=owned'),
      ]);
      setFork(forkData);
      setIflines((lineData.items ?? []).filter((item) => item.characterId === context.characterId));
      setCards(cardData.cards ?? []);
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => { void load(); }, [context?.worldId]); // eslint-disable-line react-hooks/exhaustive-deps

  const required = fork?.cost.subplotCards ?? 0;
  const toggleCard = (id: string) => {
    setSelectedCards((current) => current.includes(id) ? current.filter((item) => item !== id) : [...current, id].slice(-Math.max(required, 1)));
  };

  const open = async () => {
    if (!context || !fork?.eligible || selectedCards.length !== required) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      let line: IflineItem;
      if (preview) {
        line = { id: `ifline-${Date.now()}`, status: 'open', characterId: context.characterId, premise: premise.trim(), createdAt: Date.now(), forkPoint: { kind: 'terminal', tickNo: fork.supportedForkPoints[0]?.tickNo ?? 0, stateFidelity: 'origin_terminal' }, progress: { beatCount: 0, maxBeats: 12 }, advance: { pending: false }, affectsOriginWorld: false };
      } else {
        line = await cloudFetch<IflineItem>(`/api/worlds/${context.worldId}/iflines`, {
          method: 'POST', idempotent: true,
          body: { characterId: context.characterId, forkPoint: 'terminal', tickNo: fork.supportedForkPoints[0]?.tickNo, premise: premise.trim() || undefined, cardIds: selectedCards },
        });
      }
      setIflines((items) => [line, ...items]);
      setNotice('私人平行线已开启。原世界没有被改写，所选副本卡已作为内容燃料消耗。');
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setBusy(false);
    }
  };

  const advance = async (line: IflineItem) => {
    setBusy(true);
    setError(null);
    try {
      if (!preview) await cloudFetch(`/api/me/iflines/${line.id}/beats`, { method: 'POST', idempotent: true });
      setIflines((items) => items.map((item) => item.id === line.id ? { ...item, advance: { ...item.advance, pending: true } } : item));
      setNotice('推进请求已受理，故事会在后台生成。稍后刷新即可看到落定的新一拍。');
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setBusy(false);
    }
  };

  const active = iflines[0];
  return (
    <JourneyPage title="开启私人平行线" description="把“如果当时……”变成一条只属于你的故事。分叉必须忠于可还原状态，也绝不会改写原世界。" wide>
      <JourneyState loading={contextLoading} error={contextError} empty={!context} emptyText="参与并结束一个世界后，才能从它的终局开启平行线" onRetry={reload}>
        <JourneyContextBar context={context} memberships={memberships} onChange={changeWorld} />
        {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
        {notice && <Alert type="success" showIcon title={notice} closable onClose={() => setNotice(null)} style={{ marginBottom: 16 }} />}
        <JourneyState loading={loading} error={!fork ? error : null}>
          <div className="journey-grid">
            <section className="journey-panel journey-panel--4">
              <h2>1 · 选择分叉点</h2>
              <p className="journey-panel__intro">当前只展示服务端确认可完整还原的分叉点。</p>
              <div className="journey-timeline">
                {(fork?.supportedForkPoints ?? []).map((point) => (
                  <div className="journey-timeline__item" key={`${point.kind}-${point.tickNo}`}>
                    <span className="journey-timeline__dot" />
                    <div className="journey-timeline__content"><h3>第 {point.tickNo} 拍 · 原世界终局</h3><p>{point.desc}</p><Tag color="success">完整状态</Tag></div>
                  </div>
                ))}
              </div>
              {!fork?.eligible && <Alert type="warning" showIcon title="暂不可分叉" description={fork?.ineligibleReason || '当前世界不满足分叉条件'} />}
            </section>
            <section className="journey-panel journey-panel--4">
              <h2>2 · 写下另一种可能</h2>
              <div className="journey-portrait-card" style={{ gridTemplateColumns: '110px 1fr' }}>
                <img style={{ width: 110 }} src="/assets/characters/kane-night-oath-portrait.png" alt="凯恩·夜誓角色肖像" />
                <div><Tag color="orange">单人副本</Tag><h3>{context?.characterName}</h3><p className="journey-panel__intro">其他玩家角色会被剥离；只有你的主角进入平行线。</p></div>
              </div>
              <label className="journey-label" style={{ marginTop: 16 }}>如果……<Input.TextArea rows={5} maxLength={1000} showCount value={premise} onChange={(event) => setPremise(event.target.value)} /></label>
            </section>
            <section className="journey-panel journey-panel--4">
              <h2>3 · 选择内容燃料</h2>
              <p className="journey-panel__intro">需要显式选择 {required} 张在手副本卡。开启成功后不可恢复。</p>
              <div className="journey-list">
                {cards.slice(0, 6).map((card) => (
                  <label className={`journey-list-item${selectedCards.includes(card.id) ? ' is-selected' : ''}`} key={card.id} style={{ cursor: 'pointer' }}>
                    <Checkbox checked={selectedCards.includes(card.id)} onChange={() => toggleCard(card.id)} />
                    <div className="journey-list-item__body"><h3>{card.label}</h3><StarRating value={card.starRating} /></div>
                  </label>
                ))}
              </div>
              <div className="journey-panel__footer"><Button type="primary" icon={<FireOutlined />} loading={busy} disabled={!fork?.eligible || selectedCards.length !== required} onClick={() => void open()}>消耗 {required} 张卡并开启</Button></div>
            </section>
            {active && (
              <section className="journey-panel">
                <Space style={{ width: '100%', justifyContent: 'space-between' }} align="start" wrap>
                  <div><Tag color="purple">IF LINE</Tag><h2>{active.premise || '未命名平行线'}</h2><p className="journey-panel__intro">已推进 {active.progress?.beatCount ?? 0} 拍 · 创建于 {formatJourneyTime(active.createdAt)}</p></div>
                  <Button type="primary" icon={<ApartmentOutlined />} loading={busy || active.advance?.pending} disabled={active.status === 'ended'} onClick={() => void advance(active)}>{active.advance?.pending ? '后台生成中' : '由我推进下一拍'}</Button>
                </Space>
                <div className="journey-notice"><LockOutlined /> 这条线的内容只属于私人体验：不发历练、不铸新卡，也不会进入原世界结算。</div>
              </section>
            )}
          </div>
        </JourneyState>
      </JourneyState>
    </JourneyPage>
  );
};

export const JourneySubplot: React.FC = () => {
  const preview = useJourneyPreview();
  const [cards, setCards] = useState<SubplotCard[]>(preview ? previewCards : []);
  const [sourceCount, setSourceCount] = useState(3);
  const [selected, setSelected] = useState<string[]>(previewCards.slice(0, 3).map((card) => card.id));
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<SubplotCard | null>(null);

  const load = async () => {
    if (preview) return;
    setLoading(true);
    setError(null);
    try {
      const data = await cloudFetch<{ cards: SubplotCard[]; synthesisRule: { sourceCount: number } }>('/api/me/subplot-cards?status=owned');
      setCards(data.cards ?? []);
      setSourceCount(data.synthesisRule?.sourceCount ?? 3);
    } catch (err) { setError(journeyError(err)); } finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const selectedCards = useMemo(() => cards.filter((card) => selected.includes(card.id)), [cards, selected]);
  const sameStar = selectedCards.length < 2 || selectedCards.every((card) => card.starRating === selectedCards[0].starRating);
  const toggle = (card: SubplotCard) => {
    setSelected((current) => current.includes(card.id) ? current.filter((id) => id !== card.id) : [...current, card.id].slice(-sourceCount));
  };
  const synthesize = async () => {
    if (selected.length !== sourceCount || !sameStar) return;
    setBusy(true); setError(null);
    try {
      const card = preview
        ? { id: `subplot-${Date.now()}`, starRating: Math.min(5, (selectedCards[0]?.starRating ?? 1) + 1), label: '潮汐尽头的守望约定', originKind: 'synthesis', status: 'owned', synthesizedFrom: selected }
        : (await cloudFetch<{ card: SubplotCard }>('/api/me/subplot-cards/synthesize', { method: 'POST', idempotent: true, body: { cardIds: selected } })).card;
      setCards((items) => [card, ...items.filter((item) => !selected.includes(item.id))]);
      setResult(card);
      setSelected([]);
    } catch (err) { setError(journeyError(err)); } finally { setBusy(false); }
  };

  return (
    <JourneyPage title="我的副本卡" description="副本卡是旅程留下的内容燃料。选择同星卡合成更高星剧情蓝图，或保留它们用于开启 IF 线。" wide>
      {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
      <JourneyState loading={loading} error={!cards.length ? error : null} empty={!cards.length} emptyText="还没有副本卡；完成支持该产出的世界结算后会在这里出现" onRetry={load}>
        <div className="journey-grid">
          <section className="journey-panel journey-panel--8">
            <Space style={{ width: '100%', justifyContent: 'space-between' }} wrap><div><h2>在手卡片</h2><p className="journey-panel__intro">已选 {selected.length}/{sourceCount} 张 · 必须星级相同</p></div><Select defaultValue="owned" options={[{ value: 'owned', label: '在手' }, { value: 'consumed', label: '已消耗' }]} /></Space>
            <div className="journey-card-wall">
              {cards.map((card, index) => (
                <label className={`journey-asset-card${selected.includes(card.id) ? ' is-selected' : ''}`} key={card.id}>
                  <input type="checkbox" checked={selected.includes(card.id)} onChange={() => toggle(card)} />
                  <img src={index % 2 ? '/assets/platform/ember-tavern.png' : '/assets/platform/mist-sea-world.png'} alt={`${card.label}卡面`} />
                  <span className="journey-asset-card__body"><strong>{card.label}</strong><StarRating value={card.starRating} /></span>
                </label>
              ))}
            </div>
          </section>
          <aside className="journey-panel journey-panel--4">
            <h2>合成蓝图</h2>
            <p className="journey-panel__intro">消耗 {sourceCount} 张同星卡，合成一张更高星卡。血缘记录会永久保留。</p>
            <div className="journey-list">
              {selectedCards.map((card) => <div className="journey-list-item" key={card.id}><div className="journey-list-item__body"><h3>{card.label}</h3><StarRating value={card.starRating} /></div></div>)}
              {Array.from({ length: Math.max(0, sourceCount - selectedCards.length) }).map((_, index) => <div className="journey-list-item" key={`empty-${index}`}><PlusOutlined /><div className="journey-list-item__body"><p>再选择一张同星卡</p></div></div>)}
            </div>
            {!sameStar && <Alert type="warning" showIcon title="所选卡片星级不同" style={{ marginTop: 12 }} />}
            <div className="journey-panel__footer"><Button type="primary" icon={<FireOutlined />} loading={busy} disabled={selected.length !== sourceCount || !sameStar} onClick={() => void synthesize()}>确认合成</Button></div>
            {result && <Alert type="success" showIcon title={`已合成：${result.label}`} description={<StarRating value={result.starRating} />} style={{ marginTop: 16 }} />}
          </aside>
        </div>
      </JourneyState>
    </JourneyPage>
  );
};

interface MemorialCharacter {
  id: string;
  name: string;
  avatarUrl?: string | null;
  mileage: number;
  sealedAt?: number | null;
  sealedIn?: { worldId?: string | null; title?: string | null };
}

export const JourneyMemorial: React.FC = () => {
  const preview = useJourneyPreview();
  const { context, memberships, loading: contextLoading, error: contextError, changeWorld, reload } = useJourneyContext();
  const [hall, setHall] = useState<MemorialCharacter[]>(preview ? [{ id: 'legacy-elena', name: '艾琳娜·风语', avatarUrl: '/assets/characters/elena-windwhisper-portrait.png', mileage: 1280, sealedAt: Date.now() - 12 * 86_400_000, sealedIn: { title: '余烬酒馆' } }] : []);
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [understood, setUnderstood] = useState(false);
  const [sealed, setSealed] = useState(false);

  const load = async () => {
    if (preview) return;
    setLoading(true); setError(null);
    try { const data = await cloudFetch<{ characters: MemorialCharacter[] }>('/api/memorial/characters?limit=20&offset=0'); setHall(data.characters ?? []); }
    catch (err) { setError(journeyError(err)); } finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const seal = async () => {
    if (!context || !understood) return;
    setBusy(true); setError(null);
    try {
      if (!preview) await cloudFetch(`/api/me/characters/${context.characterId}/memorial`, { method: 'POST', idempotent: true });
      setSealed(true); setConfirmOpen(false);
    } catch (err) { setError(journeyError(err)); } finally { setBusy(false); }
  };

  return (
    <JourneyPage title={`为${context?.characterName || '角色'}封卷`} description="封卷把一张已有真实死亡证据的角色卡变成不可变的传世档案：只读、公开陈列、永不复活。" wide>
      <JourneyState loading={contextLoading} error={contextError} empty={!context} emptyText="没有可进入封卷流程的角色" onRetry={reload}>
        <JourneyContextBar context={context} memberships={memberships} onChange={changeWorld} />
        {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
        {sealed && <Alert type="success" showIcon title="封卷完成" description="角色已成为只读传世卡，道具已回到账户背包。" style={{ marginBottom: 16 }} />}
        <div className="journey-grid">
          <section className="journey-panel journey-panel--8 journey-memorial">
            <div className="journey-memorial__banner">
              <img src="/assets/platform/mist-sea-world.png" alt="雾海纪元世界终景" />
              <div className="journey-memorial__copy">
                <img src="/assets/characters/kane-night-oath-portrait.png" alt={`${context?.characterName || '角色'}肖像`} />
                <div><Tag color="volcano">传世封卷</Tag><h2>{context?.characterName}</h2><p>他的选择、历练、羁绊与足迹会成为公开可读的一生档案。封卷承接已经落定的死亡事实，不制造死亡，也不能撤回。</p></div>
              </div>
            </div>
            <div className="journey-memorial__body">
              <div className="journey-timeline">
                <div className="journey-timeline__item"><span className="journey-timeline__dot" /><div className="journey-timeline__content"><h3>确认公共死亡证据</h3><p>由服务端检查世界事件与同意记录，客户端不能自行声明。</p></div></div>
                <div className="journey-timeline__item"><span className="journey-timeline__dot" /><div className="journey-timeline__content"><h3>归还随身物品</h3><p>角色携带的合法物品回到账户背包，不随传世卡冻结。</p></div></div>
                <div className="journey-timeline__item"><span className="journey-timeline__dot" /><div className="journey-timeline__content"><h3>生成只读传世档案</h3><p>角色不再进入任何世界；同内核新卡被视为转世，而不是复活。</p></div></div>
              </div>
              <div className="journey-panel__footer"><Button danger type="primary" size="large" icon={<BookOutlined />} disabled={sealed} onClick={() => setConfirmOpen(true)}>{sealed ? '已封卷' : '审阅并确认封卷'}</Button></div>
            </div>
          </section>
          <aside className="journey-panel journey-panel--4">
            <h2>遗作馆新近封卷</h2>
            <JourneyState loading={loading} empty={!hall.length} emptyText="遗作馆目前还没有传世卡">
              <div className="journey-list">
                {hall.map((item) => (
                  <article className="journey-list-item" key={item.id}>
                    <img className="journey-list-item__image" style={{ width: 64, height: 82, objectPosition: 'top' }} src={preview ? item.avatarUrl || '/assets/characters/elena-windwhisper-portrait.png' : resolveObjectUrl(item.avatarUrl) || '/assets/characters/elena-windwhisper-portrait.png'} alt={`${item.name}传世肖像`} />
                    <div className="journey-list-item__body"><h3>{item.name}</h3><p>历练 {item.mileage} · {item.sealedIn?.title || '未知世界'}</p><p>{formatJourneyTime(item.sealedAt)}</p></div>
                  </article>
                ))}
              </div>
            </JourneyState>
          </aside>
        </div>
        <Modal title="确认不可逆封卷" open={confirmOpen} onCancel={() => setConfirmOpen(false)} footer={[
          <Button key="cancel" onClick={() => setConfirmOpen(false)}>再想想</Button>,
          <Button key="seal" danger type="primary" loading={busy} disabled={!understood} onClick={() => void seal()}>确认封卷</Button>,
        ]}>
          <p>封卷后，这张角色卡将永久只读、进入遗作馆，并且不能再加入任何世界。</p>
          <Checkbox checked={understood} onChange={(event) => setUnderstood(event.target.checked)}>我理解这是不可逆操作，也理解转世不等于复活。</Checkbox>
          <div className="journey-notice" style={{ marginTop: 16 }}><HourglassOutlined /> 如果世界里的死亡尚未真正落定，服务端会拒绝本次封卷，所有资产保持不变。</div>
        </Modal>
      </JourneyState>
    </JourneyPage>
  );
};
