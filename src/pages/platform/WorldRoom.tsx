// 世界房间（C1，规格 §2.5/§2.4/§2.7）：L0 事件流 + L1 事件卡/关系图谱/状态面板 + 干预三环 UI + 同意请求。
// 读写分离：事件流是只读投影（cloudStream 订阅 + GET events 补偿）；一切状态变更只提交意图（interventions/consents）。
// 导出 useWorldEvents 与 L1 组件供 WorldSpectate 复用。
import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Typography,
  Tag,
  Card,
  Space,
  Alert,
  Spin,
  Empty,
  Button,
  Input,
  Select,
  Segmented,
  List,
  Divider,
  Tooltip,
  Timeline,
  Checkbox,
} from 'antd';
import {
  RobotOutlined,
  SafetyCertificateOutlined,
  BulbOutlined,
  GiftOutlined,
  TeamOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons';
import { useParams, useNavigate, useSearchParams } from 'react-router-dom';
import RelationForceGraph from '../../components/graph/RelationForceGraph';
import PowerHierarchy from '../../components/graph/PowerHierarchy';
import EventTimeline, { toTimelineEvents } from '../../components/graph/EventTimeline';
import SceneMap from '../../components/graph/SceneMap';
import { arcStageLabel } from '../../components/graph/model';
import { cloudFetch, cloudStream } from '../../utils/cloudApi';
import { usePartnerStore } from '../../stores/usePartnerStore';
import {
  usePlatformStore,
  describeCloudError,
  roomTypeLabel,
  eventTypeMeta,
  type WorldDetail,
  type WorldEventItem,
  type WorldRosterEntry,
  type ConsentRequest,
  type InterventionRecord,
  type CloudCharacter,
  type RoomView,
  type WorldStateSummary,
  type WorldCharacterState,
} from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

const WHISPER_MAX = 100;

// ---------- 事件流数据：初始补偿 + WS 订阅 + 去重 ----------

function upsertEvent(list: WorldEventItem[], ev: WorldEventItem): WorldEventItem[] {
  const idx = list.findIndex((e) => e.id === ev.id || e.sequence === ev.sequence);
  const next = idx >= 0 ? list.map((e, i) => (i === idx ? ev : e)) : [...list, ev];
  return next.sort((a, b) => a.sequence - b.sequence);
}

/** 拉取历史事件（当前 principal 投影）并订阅实时流；断线由 cloudStream 内部重连补偿。 */
export function useWorldEvents(worldId: string | undefined): {
  events: WorldEventItem[];
  loading: boolean;
  error: string | null;
} {
  const [events, setEvents] = useState<WorldEventItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!worldId) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    setEvents([]);

    cloudFetch<{ events: WorldEventItem[]; nextCursor: number | null }>(`/api/worlds/${worldId}/events`)
      .then((data) => {
        if (cancelled) return;
        const list = (data.events ?? []).slice().sort((a, b) => a.sequence - b.sequence);
        setEvents(list);
      })
      .catch((e) => {
        if (!cancelled) setError(describeCloudError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    let unsub: (() => void) | null = null;
    try {
      unsub = cloudStream(
        worldId,
        (raw) => {
          const ev = raw as WorldEventItem;
          if (!ev || typeof ev.sequence !== 'number' || !ev.id) return;
          setEvents((prev) => upsertEvent(prev, ev));
        },
        () => {
          // 实时流异常不致命：保留已有历史事件，等待自动重连
        },
      );
    } catch {
      // WebSocket 不可用（如离线）：仅用历史补偿，页面不崩
    }

    return () => {
      cancelled = true;
      if (unsub) unsub();
    };
  }, [worldId]);

  return { events, loading, error };
}

// ---------- 权威状态快照（#6b）：relations / characters ----------

/**
 * 拉取世界权威状态快照（GET /worlds/{id}/state-summary，server 端 G-RUNTIME 提供）。
 * 端点未就绪（尚未上线）或云端故障时优雅降级为 null——由消费组件回退到事件共现启发式，页面不崩。
 */
export function useWorldStateSummary(worldId: string | undefined): {
  summary: WorldStateSummary | null;
  loading: boolean;
  error: string | null;
  /** 手动重拉权威快照（观战席实时演化：收到 relation/status 事件后去抖调用）。 */
  reload: () => void;
} {
  const [summary, setSummary] = useState<WorldStateSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadToken, setReloadToken] = useState(0);

  // 世界切换时才清空旧快照；实时重拉（reloadToken 递增）保留上一帧，避免关系图周期性闪空。
  useEffect(() => {
    setSummary(null);
  }, [worldId]);

  useEffect(() => {
    if (!worldId) return;
    let cancelled = false;
    setLoading(true);
    setError(null);

    cloudFetch<WorldStateSummary>(`/api/worlds/${worldId}/state-summary`)
      .then((data) => {
        if (cancelled) return;
        setSummary({
          relations: data.relations ?? [],
          characters: data.characters ?? [],
          locations: data.locations ?? [],
          positions: data.positions ?? {},
        });
        setError(null);
      })
      .catch((e) => {
        // 权威快照非关键：端点未就绪 / 网络失败时保持上一帧（或 null），组件回退启发式。
        if (!cancelled) setError(describeCloudError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [worldId, reloadToken]);

  const reload = useCallback(() => setReloadToken((t) => t + 1), []);

  return { summary, loading, error, reload };
}

// ---------- L0 文字流 ----------

export const EventStream: React.FC<{ events: WorldEventItem[] }> = ({ events }) => {
  if (events.length === 0) {
    return <Empty description="世界尚未产生事件，等待下一个节拍" />;
  }
  return (
    <Timeline
      items={events.map((ev) => {
        const meta = eventTypeMeta(ev.type);
        return {
          color: meta.color === 'default' ? 'gray' : meta.color,
          children: (
            <div>
              <Space size={6} wrap>
                <Tag color={meta.color}>{meta.label}</Tag>
                <Text type="secondary" style={{ fontSize: 12 }}>
                  第 {ev.tick} 拍
                </Text>
                {ev.visibility !== 'public' && <Tag color="purple">仅你可见</Tag>}
              </Space>
              <Paragraph style={{ margin: '6px 0 0', color: '#33312e' }}>
                {ev.projection?.summary || ev.projection?.narrative || '（无摘要）'}
              </Paragraph>
              {ev.actors.length > 0 && (
                <Text type="secondary" style={{ fontSize: 12 }}>
                  参与：{ev.actors.join('、')}
                </Text>
              )}
            </div>
          ),
        };
      })}
    />
  );
};

// ---------- L1 事件卡 ----------

export const EventCards: React.FC<{ events: WorldEventItem[] }> = ({ events }) => {
  if (events.length === 0) return <Empty description="暂无事件卡" />;
  return (
    <Space direction="vertical" size={12} style={{ width: '100%' }}>
      {events
        .slice()
        .reverse()
        .map((ev) => {
          const meta = eventTypeMeta(ev.type);
          return (
            <Card key={ev.id} size="small" style={{ borderRadius: 10, border: '1px solid #eae6df' }}>
              <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start">
                <Space size={6}>
                  <Tag color={meta.color}>{meta.label}</Tag>
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    第 {ev.tick} 拍 · #{ev.sequence}
                  </Text>
                </Space>
                <Space size={4}>
                  {ev.visibility !== 'public' && <Tag color="purple">仅你可见</Tag>}
                  {ev.aiLabel?.visible !== false && <Tag>AI</Tag>}
                </Space>
              </Space>
              <Paragraph style={{ margin: '8px 0 4px', color: '#33312e' }}>
                {ev.projection?.summary || '（无摘要）'}
              </Paragraph>
              {ev.actors.length > 0 && (
                <Text type="secondary" style={{ fontSize: 12 }}>
                  <TeamOutlined /> {ev.actors.join('、')}
                </Text>
              )}
            </Card>
          );
        })}
    </Space>
  );
};

// ---------- L1 状态面板 ----------

export const StatusPanel: React.FC<{
  roster: WorldRosterEntry[];
  events: WorldEventItem[];
  /** 权威角色状态（#6b）；提供时以 arcStage/activity 为准，事件派生仅作近况补充。 */
  characters?: WorldCharacterState[];
  myIds?: Set<string>;
}> = ({ roster, events, characters, myIds }) => {
  const mine = myIds ?? new Set<string>();
  const stats = useMemo(() => {
    const map = new Map<string, { count: number; lastSummary?: string; lastTick?: number }>();
    for (const ev of events) {
      for (const a of ev.actors) {
        const cur = map.get(a) ?? { count: 0 };
        cur.count += 1;
        cur.lastSummary = ev.projection?.summary || cur.lastSummary;
        cur.lastTick = ev.tick;
        map.set(a, cur);
      }
    }
    return map;
  }, [events]);

  const authMap = useMemo(() => {
    const m = new Map<string, WorldCharacterState>();
    for (const c of characters ?? []) m.set(c.id, c);
    return m;
  }, [characters]);
  const hasAuthoritative = (characters?.length ?? 0) > 0;

  if (roster.length === 0) return <Empty description="暂无角色阵容" />;

  return (
    <Space direction="vertical" size={12} style={{ width: '100%' }}>
      <Text type="secondary" style={{ fontSize: 12 }}>
        {hasAuthoritative ? '数据源：权威状态快照（弧光阶段 / 活跃度）' : '数据源：由观测事件推导（尚无权威状态快照）'}
      </Text>
      {roster.map((r) => {
        const s = stats.get(r.cloudCharacterId);
        const a = authMap.get(r.cloudCharacterId);
        return (
          <Card key={r.cloudCharacterId} size="small" style={{ borderRadius: 10, border: '1px solid #eae6df' }}>
            <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start">
              <Space size={8} wrap>
                <Text strong>{r.name || r.cloudCharacterId}</Text>
                {mine.has(r.cloudCharacterId) && <Tag color="orange">我的角色</Tag>}
                {a && <Tag color="blue">弧光 · {arcStageLabel(a.arcStage)}</Tag>}
              </Space>
              <Text type="secondary" style={{ fontSize: 12 }}>
                {a ? `活跃度 ${a.activity}` : `活跃 ${s?.count ?? 0} 次`}
              </Text>
            </Space>
            {s?.lastSummary && (
              <Paragraph type="secondary" style={{ margin: '6px 0 0', fontSize: 12 }} ellipsis={{ rows: 2 }}>
                最近（第 {s.lastTick} 拍）：{s.lastSummary}
              </Paragraph>
            )}
          </Card>
        );
      })}
    </Space>
  );
};

// ---------- L1 世界线（只看我这个角色的故事线；情感留存核心） ----------

/**
 * 单角色世界线：把已取的全量投影事件（useWorldEvents）过滤到「该角色作为 actor 参与」的事件，
 * 按 sequence 升序（一生按发生顺序读）叙事化渲染。纯前端过滤，无新请求。
 * private 事件标「仅你可见」；同场其他角色以名解析显示。
 */
export const CharacterWorldline: React.FC<{
  events: WorldEventItem[];
  characterId: string | undefined;
  roster?: WorldRosterEntry[];
}> = ({ events, characterId, roster }) => {
  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of roster ?? []) m.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    return m;
  }, [roster]);
  const mine = useMemo(
    () =>
      characterId
        ? events
            .filter((ev) => ev.actors.includes(characterId))
            .slice()
            .sort((a, b) => a.sequence - b.sequence)
        : [],
    [events, characterId],
  );

  if (!characterId) {
    return <Empty description="先选择一个你的角色，查看 TA 的世界线" image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }
  if (mine.length === 0) {
    return (
      <Empty description="TA 还没在这个世界留下故事，等待下一个节拍" image={Empty.PRESENTED_IMAGE_SIMPLE} />
    );
  }
  return (
    <Timeline
      items={mine.map((ev) => {
        const meta = eventTypeMeta(ev.type);
        const others = ev.actors.filter((a) => a !== characterId).map((a) => nameOf.get(a) || a);
        return {
          color: meta.color === 'default' ? 'gray' : meta.color,
          children: (
            <div>
              <Space size={6} wrap>
                <Tag color="orange">我的角色</Tag>
                <Tag color={meta.color}>{meta.label}</Tag>
                <Text type="secondary" style={{ fontSize: 12 }}>
                  第 {ev.tick} 拍
                </Text>
                {ev.visibility !== 'public' && <Tag color="purple">仅你可见</Tag>}
              </Space>
              <Paragraph style={{ margin: '6px 0 0', color: '#33312e' }}>
                {ev.projection?.narrative || ev.projection?.summary || '（无摘要）'}
              </Paragraph>
              {others.length > 0 && (
                <Text type="secondary" style={{ fontSize: 12 }}>
                  同场：{others.join('、')}
                </Text>
              )}
            </div>
          ),
        };
      })}
    />
  );
};

// ---------- L1 视图容器（房间与观战席共用） ----------

const L1_OPTIONS = [
  { label: '事件流', value: 'stream' as RoomView },
  { label: '事件卡', value: 'cards' as RoomView },
  { label: '时间线', value: 'timeline' as RoomView },
  { label: '关系图谱', value: 'graph' as RoomView },
  { label: '势力地图', value: 'map' as RoomView },
  { label: '场景地图', value: 'scene' as RoomView },
  { label: '状态面板', value: 'status' as RoomView },
];

const WORLDLINE_OPTION = { label: '世界线', value: 'worldline' as RoomView };

export const WorldViewPanel: React.FC<{
  view: RoomView;
  onViewChange: (v: RoomView) => void;
  events: WorldEventItem[];
  roster: WorldRosterEntry[];
  myIds?: Set<string>;
  /** 权威状态快照（#6b）；缺省（如观战席未拉取）时 L1 组件回退事件启发式。 */
  summary?: WorldStateSummary | null;
  loading?: boolean;
  error?: string | null;
  /** 我在本世界的角色（提供且非空时解锁「世界线」视图 + 角色选择器）。观战席不传 → 无此视图。 */
  myChars?: WorldRosterEntry[];
  selectedCharId?: string;
  onSelectChar?: (id: string) => void;
}> = ({ view, onViewChange, events, roster, myIds, summary, loading, error, myChars, selectedCharId, onSelectChar }) => {
  const hasWorldline = (myChars?.length ?? 0) > 0;
  const options = hasWorldline ? [...L1_OPTIONS, WORLDLINE_OPTION] : L1_OPTIONS;
  // roomView 持久化且与观战席共享：无我方角色（观战/无角色世界）时回退 stream，避免 Segmented 值越界。
  const effectiveView: RoomView = view === 'worldline' && !hasWorldline ? 'stream' : view;
  const currentChar = selectedCharId ?? myChars?.[0]?.cloudCharacterId;
  return (
    <Card
      style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
      styles={{ body: { padding: 20 } }}
      title={
        <Segmented options={options} value={effectiveView} onChange={(v) => onViewChange(v as RoomView)} />
      }
    >
      {error ? (
        <Alert type="error" showIcon message="事件流加载失败" description={error} />
      ) : loading ? (
        <div style={{ textAlign: 'center', padding: 40 }}>
          <Spin />
        </div>
      ) : effectiveView === 'stream' ? (
        <EventStream events={events} />
      ) : effectiveView === 'cards' ? (
        <EventCards events={events} />
      ) : effectiveView === 'timeline' ? (
        <EventTimeline events={toTimelineEvents(events)} roster={roster} myIds={myIds} />
      ) : effectiveView === 'graph' ? (
        <RelationForceGraph
          roster={roster}
          events={events}
          relations={summary?.relations}
          characters={summary?.characters}
          myIds={myIds}
        />
      ) : effectiveView === 'map' ? (
        <PowerHierarchy roster={roster} events={events} relations={summary?.relations} myIds={myIds} />
      ) : effectiveView === 'scene' ? (
        <SceneMap
          locations={summary?.locations}
          positions={summary?.positions}
          roster={roster}
          events={events}
          myIds={myIds}
        />
      ) : effectiveView === 'worldline' ? (
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          {(myChars?.length ?? 0) > 1 && (
            <Select
              style={{ width: '100%', maxWidth: 320 }}
              value={currentChar}
              onChange={onSelectChar}
              options={(myChars ?? []).map((c) => ({ label: c.name || c.cloudCharacterId, value: c.cloudCharacterId }))}
              aria-label="选择要查看世界线的角色"
            />
          )}
          <Text type="secondary" style={{ fontSize: 12 }}>
            只看这个角色的故事线，按发生顺序叙事化排列（仅你可见的私密事件也在其中）。
          </Text>
          <CharacterWorldline events={events} characterId={currentChar} roster={roster} />
        </Space>
      ) : (
        <StatusPanel roster={roster} events={events} characters={summary?.characters} myIds={myIds} />
      )}
    </Card>
  );
};

// ---------- 世界详情头 ----------

export const WorldHeader: React.FC<{ world: WorldDetail; spectate?: boolean }> = ({ world, spectate }) => (
  <Card
    style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
    styles={{ body: { padding: 20 } }}
  >
    <Space direction="vertical" size={8} style={{ width: '100%' }}>
      <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start">
        <Space direction="vertical" size={2}>
          <Space size={10}>
            <Title level={3} style={{ margin: 0, color: '#33312e' }}>
              {world.title}
            </Title>
            {spectate && <Tag color="blue">观战席</Tag>}
          </Space>
          <Space size={16} style={{ color: '#8c857b', fontSize: 13 }}>
            <Tag color="orange">{roomTypeLabel(world.roomType)}</Tag>
            {/* 星级（1-5）：≥3 的世界要求投放角色历练达标（join 由服务端权威校验并返回文案）。 */}
            {typeof world.starRating === 'number' && <Tag color="gold">{world.starRating}★</Tag>}
            <span>
              <TeamOutlined /> {world.memberCount}/{world.memberLimit}
            </span>
            <span>
              <ThunderboltOutlined /> 每日 {world.tickPerDay} 拍
            </span>
          </Space>
        </Space>
        <Space direction="vertical" size={4} align="end">
          <Tag icon={<RobotOutlined />} color="orange">
            AI 生成内容
          </Tag>
          {world.compliance?.arbitrationPublic && (
            <Tag icon={<SafetyCertificateOutlined />} color="green">
              仲裁公开
            </Tag>
          )}
        </Space>
      </Space>
      <Text type="secondary" style={{ fontSize: 11 }}>
        引擎 {world.engineVersion} · Prompt {world.promptSetVersion} · 模型 {world.modelRouteVersion} · 模板 v
        {world.templateVersion}
      </Text>
    </Space>
  </Card>
);

// ---------- 投放面板（构筑环：选角色 + 边界协议 → join） ----------

const JoinPanel: React.FC<{
  worldId: string;
  candidates: Array<{ id: string; name: string }>;
  full: boolean;
  onJoined: () => void;
}> = ({ worldId, candidates, full, onJoined }) => {
  const [charId, setCharId] = useState<string | undefined>(candidates[0]?.id);
  const [agreed, setAgreed] = useState(false);
  const [allowIrreversible, setAllowIrreversible] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [feedback, setFeedback] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

  useEffect(() => {
    if (!charId && candidates[0]) setCharId(candidates[0].id);
  }, [candidates, charId]);

  if (candidates.length === 0) return null;

  const submit = async () => {
    if (!charId) {
      setFeedback({ type: 'error', text: '请选择要投放的角色' });
      return;
    }
    if (!agreed) {
      setFeedback({ type: 'error', text: '请先确认入场边界协议' });
      return;
    }
    setSubmitting(true);
    setFeedback(null);
    try {
      await cloudFetch(`/api/worlds/${worldId}/join`, {
        method: 'POST',
        idempotent: true,
        body: { cloudCharacterId: charId, boundary: { acknowledged: true, allowIrreversible } },
      });
      setFeedback({ type: 'success', text: '投放成功，角色将在下一个节拍登场' });
      setAgreed(false);
      onJoined();
    } catch (e) {
      setFeedback({ type: 'error', text: describeCloudError(e) });
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Card
      title="投放角色"
      size="small"
      style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
      styles={{ body: { padding: 16 } }}
    >
      {full ? (
        <Alert type="warning" showIcon message="世界人数已满，暂无法投放新角色" />
      ) : (
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          <Select
            style={{ width: '100%' }}
            value={charId}
            onChange={setCharId}
            options={candidates.map((c) => ({ label: c.name, value: c.id }))}
            aria-label="选择要投放的角色"
          />
          <Checkbox checked={agreed} onChange={(e) => setAgreed(e.target.checked)}>
            我已阅读并同意入场边界协议：死亡 / 永久退场 / 关系与亲密变化等不可逆事件，会在触发时请求我确认。
          </Checkbox>
          <Checkbox checked={allowIrreversible} onChange={(e) => setAllowIrreversible(e.target.checked)}>
            预授权可逆范围内的剧情推进（不可逆事件仍单独请求确认）
          </Checkbox>
          {feedback && <Alert type={feedback.type} showIcon message={feedback.text} />}
          <Button type="primary" block loading={submitting} onClick={() => void submit()}>
            确认投放
          </Button>
        </Space>
      )}
    </Card>
  );
};

// ---------- 干预面板（托梦 + 道具） ----------

const InterventionPanel: React.FC<{
  worldId: string;
  myChars: WorldRosterEntry[];
  revision: number;
  onRevisionStale: () => void;
}> = ({ worldId, myChars, revision, onRevisionStale }) => {
  const [mode, setMode] = useState<'whisper' | 'item'>('whisper');
  const [charId, setCharId] = useState<string | undefined>(myChars[0]?.cloudCharacterId);
  const [text, setText] = useState('');
  const [itemId, setItemId] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [feedback, setFeedback] = useState<{ type: 'success' | 'error' | 'warning'; text: string } | null>(null);
  const [records, setRecords] = useState<InterventionRecord[]>([]);

  useEffect(() => {
    if (!charId && myChars[0]) setCharId(myChars[0].cloudCharacterId);
  }, [myChars, charId]);

  const loadRecords = async () => {
    try {
      const data = await cloudFetch<{ interventions: InterventionRecord[] }>(
        `/api/worlds/${worldId}/interventions/mine`,
      );
      setRecords(data.interventions ?? []);
    } catch {
      // 记录列表非关键，失败静默
    }
  };

  useEffect(() => {
    void loadRecords();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [worldId]);

  const submit = async () => {
    if (!charId) {
      setFeedback({ type: 'warning', text: '请选择要干预的角色' });
      return;
    }
    if (mode === 'whisper' && !text.trim()) {
      setFeedback({ type: 'warning', text: '请输入托梦内容' });
      return;
    }
    if (mode === 'item' && !itemId.trim()) {
      setFeedback({ type: 'warning', text: '请输入道具 ID' });
      return;
    }
    setSubmitting(true);
    setFeedback(null);
    try {
      const payload = mode === 'whisper' ? { text: text.trim() } : { itemId: itemId.trim() };
      const resp = await cloudFetch<{ status: string; rejectReason?: string | null }>(
        `/api/worlds/${worldId}/interventions`,
        {
          method: 'POST',
          idempotent: true,
          body: { kind: mode, characterId: charId, payload, expectedWorldRevision: revision },
        },
      );
      if (resp.status === 'accepted') {
        setFeedback({ type: 'success', text: '已提交，角色将在下一个节拍收到（它可依本性忽略）' });
        setText('');
        setItemId('');
      } else {
        const reason =
          resp.rejectReason === 'quota'
            ? '本节拍干预额度已用完'
            : resp.rejectReason === 'moderation'
              ? '内容未通过安全审核'
              : resp.rejectReason || '未接受';
        setFeedback({ type: 'warning', text: `未被接受：${reason}` });
      }
      await loadRecords();
    } catch (e) {
      const msg = describeCloudError(e);
      setFeedback({ type: 'error', text: msg });
      if (msg.includes('世界状态已更新')) onRevisionStale();
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Card
      title="干预"
      size="small"
      style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
      styles={{ body: { padding: 16 } }}
    >
      {myChars.length === 0 ? (
        <Empty description="你在本世界没有可干预的角色" image={Empty.PRESENTED_IMAGE_SIMPLE} />
      ) : (
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          <Segmented
            block
            value={mode}
            onChange={(v) => setMode(v as 'whisper' | 'item')}
            options={[
              { label: '托梦', value: 'whisper', icon: <BulbOutlined /> },
              { label: '道具', value: 'item', icon: <GiftOutlined /> },
            ]}
          />

          {myChars.length > 1 && (
            <Select
              style={{ width: '100%' }}
              value={charId}
              onChange={setCharId}
              options={myChars.map((c) => ({ label: c.name || c.cloudCharacterId, value: c.cloudCharacterId }))}
              aria-label="选择角色"
            />
          )}

          {mode === 'whisper' ? (
            <>
              <Input.TextArea
                value={text}
                onChange={(e) => setText(e.target.value)}
                maxLength={WHISPER_MAX}
                showCount
                rows={3}
                placeholder="给角色一条心声 / 直觉 / 执念（≤100 字）"
                aria-label="托梦内容"
              />
              <Text type="secondary" style={{ fontSize: 11 }}>
                托梦是低优先层的外来声音，角色可依本性忽略；抗命是特性，会进日报高光。
              </Text>
            </>
          ) : (
            <>
              <Input
                value={itemId}
                onChange={(e) => setItemId(e.target.value)}
                placeholder="道具 ID（须在你的背包中）"
                aria-label="道具 ID"
              />
              <Text type="secondary" style={{ fontSize: 11 }}>
                道具改变局面不改变意志；P4a 仅免费测试，跨世界背包体系将于后续开放。
              </Text>
            </>
          )}

          {feedback && <Alert type={feedback.type} showIcon message={feedback.text} />}

          <Button type="primary" block loading={submitting} onClick={() => void submit()}>
            提交{mode === 'whisper' ? '托梦' : '道具'}
          </Button>

          {records.length > 0 && (
            <>
              <Divider style={{ margin: '4px 0' }} />
              <Text type="secondary" style={{ fontSize: 12 }}>
                最近干预
              </Text>
              <List
                size="small"
                dataSource={records.slice(0, 5)}
                rowKey={(r) => r.id}
                renderItem={(r) => (
                  <List.Item style={{ paddingInline: 0 }}>
                    <Space size={8}>
                      <Tag>{r.kind === 'whisper' ? '托梦' : '道具'}</Tag>
                      <Tag color={r.status === 'accepted' || r.status === 'applied' ? 'green' : 'red'}>
                        {r.status === 'accepted' ? '已接受' : r.status === 'applied' ? '已生效' : '被拒'}
                      </Tag>
                      {r.rejectReason && <Text type="secondary" style={{ fontSize: 12 }}>{r.rejectReason}</Text>}
                    </Space>
                  </List.Item>
                )}
              />
            </>
          )}
        </Space>
      )}
    </Card>
  );
};

// ---------- 同意请求面板 ----------

const ConsentPanel: React.FC<{ worldId: string }> = ({ worldId }) => {
  const [consents, setConsents] = useState<ConsentRequest[]>([]);
  const [error, setError] = useState<string | null>(null);
  const busyRef = useRef<Record<string, boolean>>({});
  const [, force] = useState(0);

  const load = async () => {
    try {
      const data = await cloudFetch<{ consents: ConsentRequest[] }>('/api/me/consents?status=pending');
      setConsents((data.consents ?? []).filter((c) => c.worldId === worldId));
      setError(null);
    } catch (e) {
      setError(describeCloudError(e));
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [worldId]);

  const respond = async (cid: string, approve: boolean) => {
    busyRef.current[cid] = true;
    force((n) => n + 1);
    try {
      await cloudFetch(`/api/worlds/${worldId}/consents/${cid}/respond`, {
        method: 'POST',
        idempotent: true,
        body: { approve },
      });
      await load();
    } catch (e) {
      setError(describeCloudError(e));
    } finally {
      busyRef.current[cid] = false;
      force((n) => n + 1);
    }
  };

  if (error) {
    return (
      <Card title="同意请求" size="small" style={{ borderRadius: 12, border: 'none' }}>
        <Alert type="error" showIcon message={error} />
      </Card>
    );
  }
  if (consents.length === 0) return null;

  return (
    <Card
      title="待处理的同意请求"
      size="small"
      style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
      styles={{ body: { padding: 16 } }}
    >
      <Space direction="vertical" size={12} style={{ width: '100%' }}>
        <Text type="secondary" style={{ fontSize: 12 }}>
          不可逆事件（死亡 / 永久退场 / 关系变化等）需要你确认；未响应默认走更保守、可逆的结果。
        </Text>
        {consents.map((c) => (
          <Card key={c.id} size="small" style={{ background: '#fff7f0', border: '1px solid #f0d9c8' }}>
            <Space direction="vertical" size={8} style={{ width: '100%' }}>
              <Space size={8}>
                <Tag color="orange">{c.eventKind}</Tag>
                <Tooltip title="仅展示规则与后果，不含模型隐藏推理">
                  <Text strong>{c.detail}</Text>
                </Tooltip>
              </Space>
              <Space>
                <Button
                  type="primary"
                  size="small"
                  loading={busyRef.current[c.id]}
                  onClick={() => void respond(c.id, true)}
                >
                  同意
                </Button>
                <Button size="small" danger loading={busyRef.current[c.id]} onClick={() => void respond(c.id, false)}>
                  拒绝
                </Button>
              </Space>
            </Space>
          </Card>
        ))}
      </Space>
    </Card>
  );
};

// ---------- 页面 ----------

const WorldRoom: React.FC = () => {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const characterParam = searchParams.get('character') ?? undefined;
  const roomView = usePlatformStore((s) => s.roomView);
  const setRoomView = usePlatformStore((s) => s.setRoomView);

  const localCards = usePartnerStore((s) => s.characterCardsV2);
  const [selectedCharId, setSelectedCharId] = useState<string | undefined>(characterParam);
  const deepLinkAppliedRef = useRef(false);

  const [world, setWorld] = useState<WorldDetail | null>(null);
  const [worldError, setWorldError] = useState<string | null>(null);
  const [worldLoading, setWorldLoading] = useState(true);
  const [revision, setRevision] = useState(0);
  const [myCloudChars, setMyCloudChars] = useState<CloudCharacter[]>([]);

  const { events, loading: eventsLoading, error: eventsError } = useWorldEvents(id);
  // 权威状态快照（#6b）：驱动关系图谱/势力地图/状态面板；端点未就绪时组件自动回退启发式。
  const { summary } = useWorldStateSummary(id);

  const loadWorld = async () => {
    if (!id) return;
    setWorldLoading(true);
    setWorldError(null);
    try {
      const d = await cloudFetch<WorldDetail>(`/api/worlds/${id}`);
      setWorld(d);
      setRevision(d.stateRevision ?? 0);
    } catch (e) {
      setWorldError(describeCloudError(e));
    } finally {
      setWorldLoading(false);
    }
  };

  const loadMine = () => {
    cloudFetch<CloudCharacter[]>('/api/assets/characters/mine')
      .then((chars) => setMyCloudChars(chars ?? []))
      .catch(() => setMyCloudChars([]));
  };

  useEffect(() => {
    void loadWorld();
    loadMine();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  const myIds = useMemo(() => new Set(myCloudChars.map((c) => c.id)), [myCloudChars]);

  // 我在本世界的角色 = 我的云端角色 ∩ 当前阵容（干预对象 + 世界线主角）。
  const myChars = useMemo(
    () => (world ? world.roster.filter((r) => myIds.has(r.cloudCharacterId)) : []),
    [world, myIds],
  );

  // 我方角色就绪后校正世界线选中角色：?character= 深链优先，否则首个；深链时一次性切到「世界线」视图。
  useEffect(() => {
    if (myChars.length === 0) return;
    const ids = myChars.map((c) => c.cloudCharacterId);
    setSelectedCharId((cur) =>
      cur && ids.includes(cur) ? cur : characterParam && ids.includes(characterParam) ? characterParam : ids[0],
    );
    if (!deepLinkAppliedRef.current && characterParam && ids.includes(characterParam)) {
      deepLinkAppliedRef.current = true;
      setRoomView('worldline');
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [myChars, characterParam]);

  // 可投放候选 = 已过审、未撤回、且尚未在本世界的我的角色（本地卡名解析友好显示）。
  const joinCandidates = useMemo(() => {
    if (!world) return [];
    const nameByLocalId = new Map(localCards.map((c) => [c.id, c.identity.name]));
    const rosterIds = new Set(world.roster.map((r) => r.cloudCharacterId));
    return myCloudChars
      .filter((c) => c.moderation === 'approved' && !c.withdrawn && !rosterIds.has(c.id))
      .map((c) => ({ id: c.id, name: nameByLocalId.get(c.localCardId) || c.localCardId }));
  }, [world, myCloudChars, localCards]);

  if (worldLoading && !world) {
    return (
      <div style={{ textAlign: 'center', padding: 80 }}>
        <Spin />
      </div>
    );
  }

  if (worldError && !world) {
    return (
      <div style={{ padding: '32px 40px', maxWidth: 1180, margin: '0 auto' }}>
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={worldError}
          action={
            <Space>
              <Button size="small" onClick={() => void loadWorld()}>
                重试
              </Button>
              <Button size="small" type="text" onClick={() => navigate('/platform')}>
                返回大厅
              </Button>
            </Space>
          }
        />
      </div>
    );
  }

  if (!world) return null;

  return (
    <div style={{ padding: '24px 40px', maxWidth: 1240, margin: '0 auto' }}>
      <WorldHeader world={world} />
      <div style={{ display: 'flex', gap: 16, alignItems: 'flex-start', flexWrap: 'wrap' }}>
        <div style={{ flex: '1 1 560px', minWidth: 0 }}>
          <WorldViewPanel
            view={roomView}
            onViewChange={setRoomView}
            events={events}
            roster={world.roster}
            myIds={myIds}
            summary={summary}
            loading={eventsLoading}
            error={eventsError}
            myChars={myChars}
            selectedCharId={selectedCharId}
            onSelectChar={setSelectedCharId}
          />
        </div>
        <div style={{ flex: '0 1 340px', minWidth: 280, display: 'flex', flexDirection: 'column', gap: 16 }}>
          <ConsentPanel worldId={world.id} />
          <JoinPanel
            worldId={world.id}
            candidates={joinCandidates}
            full={world.memberCount >= world.memberLimit}
            onJoined={() => {
              void loadWorld();
              loadMine();
            }}
          />
          <InterventionPanel
            worldId={world.id}
            myChars={myChars}
            revision={revision}
            onRevisionStale={() => void loadWorld()}
          />
        </div>
      </div>
    </div>
  );
};

export default WorldRoom;
