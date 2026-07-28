import React, { useEffect, useMemo, useState } from 'react';
import { Alert, Button, Input, InputNumber, Select, Space, Tag } from 'antd';
import {
  CheckOutlined,
  CloseOutlined,
  GiftOutlined,
  LockOutlined,
  PlayCircleOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../../utils/cloudApi';
import {
  formatJourneyTime,
  journeyError,
  previewContext,
  previewInvitations,
  type Invitation,
} from './journeyData';
import {
  JourneyContextBar,
  JourneyPage,
  JourneyState,
  journeyHref,
  useJourneyContext,
  useJourneyPreview,
} from './JourneyShared';

interface OnboardingPreset {
  presetId: string;
  name: string;
  tagline: string;
  isDefault?: boolean;
}

interface OnboardingStatus {
  claimed: boolean;
  joined?: boolean;
  ticksDone?: number;
  presetId?: string;
  cloudCharacterId?: string;
  worldId?: string;
  claimedAt?: number;
  microworld?: { templateId: string; title: string; starRating: number; lethality: string };
  next?: { method?: string; path?: string; description?: string };
}

const previewPresets: OnboardingPreset[] = [
  { presetId: 'watcher', name: '灰塔守望者', tagline: '冷静、克制，擅长从细节里读出真相。', isDefault: true },
  { presetId: 'wanderer', name: '雾海旅人', tagline: '好奇、敏锐，愿意为陌生人踏入未知。' },
  { presetId: 'keeper', name: '余烬守火人', tagline: '温柔、坚韧，珍惜关系也守得住边界。' },
];

export const JourneyOnboarding: React.FC = () => {
  const preview = useJourneyPreview();
  const navigate = useNavigate();
  const [presets, setPresets] = useState<OnboardingPreset[]>(preview ? previewPresets : []);
  const [selected, setSelected] = useState(previewPresets[0].presetId);
  const [status, setStatus] = useState<OnboardingStatus | null>(preview ? {
    claimed: false,
    microworld: { templateId: 'mist-first-step', title: '雾海的第一封信', starRating: 1, lethality: 'safe' },
  } : null);
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    if (preview) return;
    setLoading(true);
    setError(null);
    try {
      const [presetData, statusData] = await Promise.all([
        cloudFetch<{ presets: OnboardingPreset[]; defaultPresetId?: string }>('/api/onboarding/presets'),
        cloudFetch<OnboardingStatus>('/api/me/onboarding'),
      ]);
      setPresets(presetData.presets ?? []);
      setSelected(statusData.presetId || presetData.defaultPresetId || presetData.presets?.[0]?.presetId || '');
      setStatus(statusData);
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => { void load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const claim = async () => {
    setBusy(true);
    setError(null);
    try {
      if (preview) {
        setStatus({
          claimed: true,
          joined: false,
          presetId: selected,
          cloudCharacterId: previewContext.characterId,
          worldId: previewContext.worldId,
          claimedAt: Date.now(),
          microworld: { templateId: 'mist-first-step', title: '雾海的第一封信', starRating: 1, lethality: 'safe' },
        });
      } else {
        const result = await cloudFetch<OnboardingStatus>('/api/me/onboarding/gift', {
          method: 'POST', idempotent: true, body: { presetId: selected },
        });
        setStatus({ ...result, claimed: true });
      }
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setBusy(false);
    }
  };

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      if (!preview) {
        await cloudFetch('/api/me/onboarding/microworld/start', { method: 'POST', idempotent: true });
      }
      navigate(journeyHref(`/platform/worlds/${status?.worldId || previewContext.worldId}`, preview));
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <JourneyPage title="从第一段旅程开始" description="选择一个角色起点，领取开场礼，并在安全的微世界里完成第一次体验。">
      <JourneyState loading={loading} error={error && !status ? error : null} onRetry={load}>
        {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
        <div className="journey-grid">
          <section className="journey-panel journey-panel--7">
            <h2>选择你的起点</h2>
            <p className="journey-panel__intro">这不是永久职业，只决定第一段故事如何向你伸手。</p>
            <div className="journey-radio-grid" role="radiogroup" aria-label="开场角色预设">
              {presets.map((preset) => (
                <label className="journey-radio-card" key={preset.presetId}>
                  <input type="radio" name="onboarding-preset" value={preset.presetId} checked={selected === preset.presetId} onChange={() => setSelected(preset.presetId)} disabled={Boolean(status?.claimed)} />
                  <span className="journey-radio-card__content">
                    <strong>{preset.name}</strong>
                    <small>{preset.tagline}</small>
                  </span>
                </label>
              ))}
            </div>
            <div className="journey-panel__footer">
              {!status?.claimed ? (
                <Button type="primary" size="large" icon={<GiftOutlined />} loading={busy} disabled={!selected} onClick={() => void claim()}>
                  领取开场礼
                </Button>
              ) : (
                <Button type="primary" size="large" icon={<PlayCircleOutlined />} loading={busy} onClick={() => void start()}>
                  开启第一段微世界
                </Button>
              )}
            </div>
          </section>
          <aside className="journey-panel journey-panel--5">
            <div className="journey-portrait-card">
              <img src="/assets/characters/kane-night-oath-portrait.png" alt="凯恩·夜誓角色肖像" />
              <div>
                <Tag color={status?.claimed ? 'success' : 'orange'}>{status?.claimed ? '已领取' : '等待领取'}</Tag>
                <h2>{presets.find((item) => item.presetId === selected)?.name || '你的开场角色'}</h2>
                <p className="journey-panel__intro">系统会创建一张只属于你的云端角色卡；后续仍可发布自己的完整角色。</p>
                <div className="journey-portrait-card__meta">
                  <div><span>微世界</span><strong>{status?.microworld?.title || '安全微世界'}</strong></div>
                  <div><span>烈度</span><strong>{status?.microworld?.lethality === 'safe' ? '无永久死亡' : '按世界规则'}</strong></div>
                </div>
              </div>
            </div>
            <div className="journey-notice" style={{ marginTop: 18 }}>
              <SafetyCertificateOutlined /> 领取开场礼不会自动把角色投入世界；正式入场仍需经过世界边界协议与准入校验。
            </div>
          </aside>
        </div>
      </JourneyState>
    </JourneyPage>
  );
};

export const JourneyInvitations: React.FC = () => {
  const preview = useJourneyPreview();
  const navigate = useNavigate();
  const [items, setItems] = useState<Invitation[]>(preview ? previewInvitations : []);
  const [resolved, setResolved] = useState<Record<string, 'accepted' | 'declined'>>({});
  const [loading, setLoading] = useState(!preview);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    if (preview) return;
    setLoading(true);
    setError(null);
    try {
      const data = await cloudFetch<{ invitations: Invitation[] }>('/api/me/invitations?status=pending');
      setItems(data.invitations ?? []);
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => { void load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const respond = async (item: Invitation, accept: boolean) => {
    setBusyId(item.id);
    setError(null);
    try {
      if (!preview) {
        await cloudFetch(`/api/me/invitations/${item.id}/respond`, {
          method: 'POST', idempotent: true, body: { accept },
        });
      }
      setResolved((current) => ({ ...current, [item.id]: accept ? 'accepted' : 'declined' }));
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <JourneyPage title="房间邀请" description="邀请来自角色面具，而不暴露真人身份。接受只代表同意邀请，真正入场仍由你最后确认。">
      {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
      <JourneyState loading={loading} error={!items.length ? error : null} empty={!items.length} emptyText="暂时没有待处理邀请" onRetry={load}>
        <section className="journey-panel">
          <div className="journey-list">
            {items.map((item, index) => {
              const result = resolved[item.id];
              const cover = index % 2 ? '/assets/platform/mist-sea-world.png' : '/assets/platform/ember-tavern.png';
              return (
                <article className="journey-list-item" key={item.id}>
                  <img className="journey-list-item__image" src={cover} alt={`${item.worldTitle}世界封面`} />
                  <div className="journey-list-item__body">
                    <Space size={8} wrap>
                      <h3>{item.worldTitle}</h3>
                      {result && <Tag color={result === 'accepted' ? 'success' : 'default'}>{result === 'accepted' ? '已接受' : '已婉拒'}</Tag>}
                    </Space>
                    <p>{item.inviterCharacterName} 邀请 {item.myCharacterName} 进入房间 · 有效至 {formatJourneyTime(item.expiresAt)}</p>
                  </div>
                  <div className="journey-list-item__actions">
                    {result === 'accepted' ? (
                      <Button type="primary" onClick={() => navigate(journeyHref(`/platform/worlds/${item.worldId}`, preview))}>前往世界并确认入场</Button>
                    ) : result ? null : (
                      <>
                        <Button icon={<CloseOutlined />} disabled={busyId === item.id} onClick={() => void respond(item, false)}>婉拒</Button>
                        <Button type="primary" icon={<CheckOutlined />} loading={busyId === item.id} onClick={() => void respond(item, true)}>接受邀请</Button>
                      </>
                    )}
                  </div>
                </article>
              );
            })}
          </div>
          <div className="journey-notice" style={{ marginTop: 16 }}><LockOutlined /> 接受邀请不会绕过世界人数、星级、生死边界或未成年保护校验。</div>
        </section>
      </JourneyState>
    </JourneyPage>
  );
};

interface AppealItem {
  id: string;
  worldId: string;
  tickNo: number;
  characterId: string;
  reasonCode: string;
  reasonText: string;
  status: string;
  annotation?: { body?: string } | null;
  worldFactChanged: boolean;
  createdAt: number;
}

export const JourneyOoc: React.FC = () => {
  const preview = useJourneyPreview();
  const { context, memberships, loading: contextLoading, error: contextError, changeWorld, reload } = useJourneyContext();
  const [appeals, setAppeals] = useState<AppealItem[]>(preview ? [{
    id: 'appeal-01', worldId: previewContext.worldId, tickNo: 47, characterId: previewContext.characterId,
    reasonCode: 'ooc', reasonText: '凯恩不会在没有确认同伴安全前独自离开灯塔。', status: 'pending',
    annotation: { body: '他真正害怕的不是雾，而是又一次来不及回头。' }, worldFactChanged: false, createdAt: Date.now() - 3_600_000,
  }] : []);
  const [tickNo, setTickNo] = useState<number | null>(47);
  const [reasonCode, setReasonCode] = useState('ooc');
  const [reasonText, setReasonText] = useState('');
  const [annotation, setAnnotation] = useState('');
  const [loading, setLoading] = useState(!preview);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    if (preview) return;
    setLoading(true);
    setError(null);
    try {
      const data = await cloudFetch<{ items: AppealItem[] }>('/api/me/ooc-appeals?limit=50&offset=0');
      setAppeals(data.items ?? []);
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => { if (context) void load(); }, [context?.worldId]); // eslint-disable-line react-hooks/exhaustive-deps

  const submit = async () => {
    if (!context || !tickNo || !reasonText.trim()) return;
    setBusy(true);
    setError(null);
    try {
      let created: AppealItem;
      if (preview) {
        created = { id: `appeal-${Date.now()}`, worldId: context.worldId, tickNo, characterId: context.characterId, reasonCode, reasonText: reasonText.trim(), status: 'pending', annotation: annotation.trim() ? { body: annotation.trim() } : null, worldFactChanged: false, createdAt: Date.now() };
      } else {
        created = await cloudFetch<AppealItem>(`/api/worlds/${context.worldId}/ooc-appeals`, {
          method: 'POST', idempotent: true,
          body: { tickNo, characterId: context.characterId, reasonCode, reasonText: reasonText.trim(), annotation: annotation.trim() || undefined },
        });
      }
      setAppeals((items) => [created, ...items.filter((item) => item.id !== created.id)]);
      setReasonText('');
      setAnnotation('');
    } catch (err) {
      setError(journeyError(err));
    } finally {
      setBusy(false);
    }
  };

  const filtered = useMemo(() => appeals.filter((item) => !context || item.worldId === context.worldId), [appeals, context]);
  return (
    <JourneyPage title="角色解释权" description="当落定的一拍不符合你的角色理解，可提交 OOC 申诉并保存只有自己可见的内心批注。" wide>
      <JourneyState loading={contextLoading} error={contextError} empty={!context} emptyText="你还没有参与过可申诉的世界" onRetry={reload}>
        <JourneyContextBar context={context} memberships={memberships} onChange={changeWorld} />
        {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
        <div className="journey-grid">
          <section className="journey-panel journey-panel--7">
            <h2>提交一条角色异议</h2>
            <p className="journey-panel__intro">申诉提供解释权与可能的梦境额度补偿，不会回滚公共世界事实。</p>
            <div className="journey-list-item" style={{ marginBottom: 16 }}>
              <img className="journey-list-item__image" src="/assets/platform/mist-sea-world.png" alt="第 47 拍雾海灯塔事件画面" />
              <div className="journey-list-item__body">
                <Tag color="orange">第 {tickNo || '—'} 拍</Tag>
                <h3>灯塔求援信号</h3>
                <p>凯恩在雾墙裂开后独自转向旧港；请说明这一步为什么不符合你的角色理解。</p>
              </div>
            </div>
            <div className="journey-form-stack">
              <div className="journey-radio-grid">
                <label className="journey-label">落定节拍<InputNumber min={1} value={tickNo} onChange={setTickNo} style={{ width: '100%' }} /></label>
                <label className="journey-label">异议类别<Select value={reasonCode} onChange={setReasonCode} options={[{ value: 'ooc', label: '角色失真' }, { value: 'tone', label: '语气不符' }, { value: 'boundary', label: '边界冲突' }]} /></label>
              </div>
              <label className="journey-label">为什么不符合角色？<Input.TextArea rows={4} maxLength={1000} showCount value={reasonText} onChange={(event) => setReasonText(event.target.value)} placeholder="说明角色一贯的选择、关系或边界…" /></label>
              <label className="journey-label">私人内心批注（可选）<small>只对你可见，不进入公共事件流。</small><Input.TextArea rows={3} maxLength={1000} showCount value={annotation} onChange={(event) => setAnnotation(event.target.value)} placeholder="记录你对这个角色更深一层的理解…" /></label>
            </div>
            <div className="journey-panel__footer"><Button type="primary" size="large" loading={busy} disabled={!tickNo || !reasonText.trim()} onClick={() => void submit()}>提交申诉与批注</Button></div>
          </section>
          <aside className="journey-panel journey-panel--5">
            <h2>我的申诉记录</h2>
            <JourneyState loading={loading} empty={!filtered.length} emptyText="当前世界还没有申诉记录">
              <div className="journey-timeline">
                {filtered.map((item) => (
                  <div className="journey-timeline__item" key={item.id}>
                    <span className="journey-timeline__dot" />
                    <div className="journey-timeline__content">
                      <Space size={7} wrap><h3>第 {item.tickNo} 拍</h3><Tag color={item.status === 'pending' ? 'processing' : 'success'}>{item.status === 'pending' ? '复核中' : item.status}</Tag></Space>
                      <p>{item.reasonText}</p>
                      {item.annotation?.body && <p><LockOutlined /> {item.annotation.body}</p>}
                    </div>
                  </div>
                ))}
              </div>
            </JourneyState>
          </aside>
        </div>
      </JourneyState>
    </JourneyPage>
  );
};
