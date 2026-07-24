// 赛事回放页（P6 观战直播 + 回放）：从 GET /arena/{id}/replay 分页拉取 public 时间线，
// 虚拟时钟 + 播放/暂停/倍速/可拖动进度条重建可 seek 的赛况回放。
// 只读、可公开验证：与透明战报同源（world_events public 行），私有投影永不进入回放。
// Local-first：仅平台路由；云端故障显示错误卡不崩；角色名 best-effort（取不到回退角色 ID）。
import React, { useEffect, useMemo, useRef, useState } from 'react';
import { Typography, Card, Tag, Space, Alert, Spin, Empty, Button, Timeline, Slider, Segmented } from 'antd';
import {
  TrophyOutlined,
  RobotOutlined,
  SafetyCertificateOutlined,
  LeftOutlined,
  PlayCircleOutlined,
  PauseCircleOutlined,
  StepBackwardOutlined,
} from '@ant-design/icons';
import { useParams, useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import {
  describeCloudError,
  arenaPhaseMeta,
  arenaEventKindMeta,
  type WorldDetail,
  type ArenaReplay as ArenaReplayData,
  type ArenaReplayEvent,
} from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

const SPEED_OPTIONS = [
  { label: '0.5×', value: 0.5 },
  { label: '1×', value: 1 },
  { label: '2×', value: 2 },
  { label: '4×', value: 4 },
];

/** 分页拉取整条 public 时间线（seek by sequence，直到没有更多）。 */
async function fetchAllReplay(worldId: string): Promise<ArenaReplayData> {
  const first = await cloudFetch<ArenaReplayData>(`/api/arena/${worldId}/replay?limit=200`);
  let all: ArenaReplayEvent[] = [...(first.events ?? [])];
  let cursor = first.nextCursor;
  // server 每页返回 nextCursor=末条 sequence（含末页）；再拉一次得空页即停止（sequence 单调，必收敛）。
  while (cursor != null && all.length > 0) {
    const page = await cloudFetch<ArenaReplayData>(`/api/arena/${worldId}/replay?cursor=${cursor}&limit=200`);
    if (!page.events || page.events.length === 0) break;
    all = all.concat(page.events);
    if (page.nextCursor == null || page.nextCursor === cursor) break;
    cursor = page.nextCursor;
  }
  return { ...first, events: all };
}

function formatClock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const mm = Math.floor(s / 60);
  const ss = s % 60;
  return `${mm}:${String(ss).padStart(2, '0')}`;
}

const ArenaReplay: React.FC = () => {
  const { worldId } = useParams<{ worldId: string }>();
  const navigate = useNavigate();

  const [replay, setReplay] = useState<ArenaReplayData | null>(null);
  const [world, setWorld] = useState<WorldDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // 虚拟时钟：cursor = 已揭示事件条数（0..total）；播放 = 按倍速逐条推进。
  const [cursor, setCursor] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);

  const load = async () => {
    if (!worldId) return;
    setLoading(true);
    setError(null);
    try {
      const data = await fetchAllReplay(worldId);
      setReplay(data);
      setCursor(0);
      setPlaying(false);
    } catch (e) {
      setError(describeCloudError(e));
    } finally {
      setLoading(false);
    }
  };

  const loadWorld = async () => {
    if (!worldId) return;
    try {
      const w = await cloudFetch<WorldDetail>(`/api/worlds/${worldId}`);
      setWorld(w);
    } catch {
      /* 名字非关键，失败静默回退 ID */
    }
  };

  useEffect(() => {
    void load();
    void loadWorld();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [worldId]);

  const total = replay?.events.length ?? 0;

  // 播放循环：按倍速逐条推进；不依赖 cursor 以免频繁重建定时器。
  const cursorRef = useRef(cursor);
  cursorRef.current = cursor;
  useEffect(() => {
    if (!playing) return;
    const id = setInterval(() => {
      setCursor((c) => (c >= total ? c : c + 1));
    }, Math.max(120, 800 / speed));
    return () => clearInterval(id);
  }, [playing, speed, total]);

  // 播放到末尾自动暂停。
  useEffect(() => {
    if (playing && total > 0 && cursor >= total) setPlaying(false);
  }, [playing, cursor, total]);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of world?.roster ?? []) m.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    return (id: string) => m.get(id) || id;
  }, [world]);

  const revealed = useMemo(() => (replay ? replay.events.slice(0, cursor) : []), [replay, cursor]);
  const lastShown = revealed[revealed.length - 1];
  const elapsedMs = lastShown && replay ? Math.max(0, lastShown.occurredAt - replay.startedAt) : 0;
  const durationMs = replay?.durationMs ?? 0;

  const togglePlay = () => {
    if (total === 0) return;
    if (!playing && cursor >= total) setCursor(0); // 从末尾重播
    setPlaying((p) => !p);
  };

  if (loading && !replay) {
    return (
      <div style={{ textAlign: 'center', padding: 80 }}>
        <Spin />
      </div>
    );
  }

  if (error && !replay) {
    return (
      <div style={{ padding: '32px 40px', maxWidth: 900, margin: '0 auto' }}>
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={error}
          action={
            <Space>
              <Button size="small" onClick={() => void load()}>
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

  if (!replay) return null;

  const phaseMeta = arenaPhaseMeta(replay.match.phase);
  const winner = replay.match.winnerCharId;

  return (
    <div style={{ padding: '24px 40px', maxWidth: 900, margin: '0 auto' }}>
      <Space style={{ marginBottom: 8 }} size={4}>
        <Button
          type="text"
          icon={<LeftOutlined />}
          onClick={() => navigate(`/platform/arena/${worldId}/spectate`)}
          style={{ color: '#8c857b' }}
        >
          观战席
        </Button>
      </Space>

      {/* 头部 */}
      <Card
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start" wrap>
          <Space direction="vertical" size={4}>
            <Space size={10} wrap>
              <Title level={3} style={{ margin: 0, color: '#33312e' }}>
                {world?.title || '赛事回放'}
              </Title>
              <Tag icon={<PlayCircleOutlined />} color="purple">
                回放
              </Tag>
              <Tag color={phaseMeta.color}>{phaseMeta.label}</Tag>
            </Space>
            <Text type="secondary" style={{ fontSize: 12 }}>
              从公开事件日志重建的可 seek 时间线 · 与透明战报同源，不含模型隐藏推理
            </Text>
          </Space>
          <Space direction="vertical" size={4} align="end">
            <Tag icon={<RobotOutlined />} color="orange">
              AI 生成内容
            </Tag>
            {replay.compliance?.arbitrationPublic && (
              <Tag icon={<SafetyCertificateOutlined />} color="green">
                仲裁公开
              </Tag>
            )}
          </Space>
        </Space>
      </Card>

      {/* 胜者荣誉展示（非强度） */}
      {winner && (
        <Card
          style={{ marginBottom: 16, borderRadius: 12, border: '1px solid #f0d9c8', background: '#fff7f0' }}
          styles={{ body: { padding: 16 } }}
        >
          <Space size={10} align="center">
            <TrophyOutlined style={{ color: '#d4a017', fontSize: 22 }} />
            <Text strong style={{ color: '#33312e' }}>
              唯一胜者：{nameOf(winner)}
            </Text>
          </Space>
        </Card>
      )}

      {/* 播放器控制条 */}
      <Card
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 16 } }}
      >
        {total === 0 ? (
          <Empty description="该赛事暂无可回放的公开事件" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <Space direction="vertical" size={12} style={{ width: '100%' }}>
            <Space size={12} wrap>
              <Button
                type="primary"
                icon={playing ? <PauseCircleOutlined /> : <PlayCircleOutlined />}
                onClick={togglePlay}
              >
                {playing ? '暂停' : cursor >= total ? '重播' : '播放'}
              </Button>
              <Button
                icon={<StepBackwardOutlined />}
                onClick={() => {
                  setPlaying(false);
                  setCursor(0);
                }}
              >
                回到开头
              </Button>
              <Segmented
                options={SPEED_OPTIONS}
                value={speed}
                onChange={(v) => setSpeed(v as number)}
                aria-label="播放倍速"
              />
              <Text type="secondary" style={{ fontSize: 12 }}>
                {formatClock(elapsedMs)} / {formatClock(durationMs)} · 第 {cursor} / {total} 条
              </Text>
            </Space>
            <Slider
              min={0}
              max={total}
              value={cursor}
              onChange={(v) => {
                setPlaying(false);
                setCursor(v);
              }}
              tooltip={{ formatter: (v) => `第 ${v} 条` }}
            />
          </Space>
        )}
      </Card>

      {/* 回放时间轴：已揭示事件 */}
      <Card
        title="回放时间轴"
        style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        {revealed.length === 0 ? (
          <Empty description="拖动进度条或点击播放开始回放" />
        ) : (
          <Timeline
            items={revealed.map((ev) => {
              const meta = arenaEventKindMeta(ev.type);
              const who = ev.actors.length > 0 ? ev.actors.map(nameOf).join('、') : ev.characterId ? nameOf(ev.characterId) : '';
              return {
                color: meta.color === 'default' ? 'gray' : meta.color,
                children: (
                  <div>
                    <Space size={6} wrap>
                      <Tag color={meta.color}>{meta.label}</Tag>
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        第 {ev.tick} 拍 · #{ev.sequence}
                      </Text>
                      {who && (
                        <Text type="secondary" style={{ fontSize: 12 }}>
                          {who}
                        </Text>
                      )}
                    </Space>
                    <Paragraph style={{ margin: '4px 0 0', color: '#33312e' }}>
                      {ev.summary || '（无摘要）'}
                    </Paragraph>
                    {ev.ruleRefs.length > 0 && (
                      <Space size={6} wrap style={{ marginTop: 4 }}>
                        <Text type="secondary" style={{ fontSize: 12 }}>
                          判定依据：
                        </Text>
                        {ev.ruleRefs.map((rr, i) => (
                          <Tag key={i} color="volcano" style={{ marginInlineEnd: 0 }}>
                            {rr}
                          </Tag>
                        ))}
                      </Space>
                    )}
                  </div>
                ),
              };
            })}
          />
        )}
      </Card>
    </div>
  );
};

export default ArenaReplay;
