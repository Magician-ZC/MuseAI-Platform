// 赛事房观战席 + 透明战报（P6，FE1 所有；规格 §2.5 / §2.7 / §9.4）：只读，无干预/同意面板。
// GET /arena/{id}/report —— 事件时间轴 + 判定依据 ruleRefs（对抗「是不是剧本」质疑）+ 礼物/环境日志。
// echarts 阵容/淘汰图；胜者奖励荣誉性展示；观众买过程不买结果。
// Local-first：仅平台路由；云端故障显示错误卡不崩；角色名 best-effort（取不到回退角色 ID）。
import React, { useEffect, useMemo, useRef, useState } from 'react';
import { Typography, Card, Tag, Space, Alert, Spin, Empty, Button, Timeline, Divider, message } from 'antd';
import {
  TrophyOutlined,
  SafetyCertificateOutlined,
  RobotOutlined,
  GiftOutlined,
  EyeOutlined,
  LeftOutlined,
  ReloadOutlined,
  PlayCircleOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons';
import { useParams, useNavigate } from 'react-router-dom';
import ReactECharts from 'echarts-for-react';
import { cloudFetch, cloudStream } from '../../utils/cloudApi';
import {
  describeCloudError,
  arenaPhaseMeta,
  arenaEventKindMeta,
  eventTypeMeta,
  type WorldDetail,
  type ArenaReport,
  type ArenaEnvEvent,
  type ArenaReplayEvent,
  type ArenaGiftResult,
} from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

function envKindLabel(kind: string): string {
  switch (kind) {
    case 'gift_boon':
      return '礼物加成';
    default:
      return kind;
  }
}

/** 环境 / 礼物事件行（礼物经网关映射为环境通道，不走玩家道具干预）。 */
const EnvEventRow: React.FC<{ env: ArenaEnvEvent }> = ({ env }) => {
  const payloadLabel =
    (typeof env.payload?.label === 'string' && env.payload.label) ||
    (typeof env.payload?.name === 'string' && env.payload.name) ||
    '';
  return (
    <Space size={6} wrap>
      <Tag icon={<GiftOutlined />} color="magenta">
        {envKindLabel(env.kind)}
      </Tag>
      {payloadLabel && <Text style={{ color: '#33312e' }}>{payloadLabel}</Text>}
      {env.aggregatedCount > 1 && <Text type="secondary">× {env.aggregatedCount}</Text>}
      <Text type="secondary" style={{ fontSize: 12 }}>
        {env.appliedTick != null ? `已应用于第 ${env.appliedTick} 拍` : '待应用'}
      </Text>
    </Space>
  );
};

/** 站内打赏快捷 SKU（映射见 server 0008 gift_sku_map；均为「买过程」的环境/道具增益，无免死/最终判定）。 */
const GIFT_SKUS: Array<{ sku: string; label: string; icon: React.ReactNode }> = [
  { sku: 'rose', label: '玫瑰·助战', icon: <GiftOutlined /> },
  { sku: 'rocket', label: '火箭·重掷', icon: <ThunderboltOutlined /> },
  { sku: 'crown', label: '皇冠·情报', icon: <TrophyOutlined /> },
  { sku: 'shield', label: '护盾·掩体', icon: <SafetyCertificateOutlined /> },
];

/** 打赏入口：SKU 快捷键 → POST /arena/{id}/gift（幂等）。买过程不买结果——经系统频道注入场内环境。 */
const GiftBar: React.FC<{ worldId: string; onGifted: () => void }> = ({ worldId, onGifted }) => {
  const [busy, setBusy] = useState<string | null>(null);

  const send = async (sku: string, label: string) => {
    setBusy(sku);
    try {
      const res = await cloudFetch<ArenaGiftResult>(`/api/arena/${worldId}/gift`, {
        method: 'POST',
        idempotent: true,
        body: { sku, count: 1 },
      });
      if (res.mapped) {
        message.success(`已打赏「${label}」，注入场内环境（系统代投）`);
      } else {
        message.info(`「${label}」已记账；该 SKU 暂无对应场内增益`);
      }
      onGifted();
    } catch (e) {
      message.error(describeCloudError(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Card
      title="打赏入口"
      size="small"
      style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
      styles={{ body: { padding: 12 } }}
    >
      <Space size={8} wrap>
        {GIFT_SKUS.map((g) => (
          <Button
            key={g.sku}
            icon={g.icon}
            loading={busy === g.sku}
            disabled={busy !== null && busy !== g.sku}
            onClick={() => void send(g.sku, g.label)}
          >
            {g.label}
          </Button>
        ))}
      </Space>
      <Divider style={{ margin: '10px 0' }} />
      <Text type="secondary" style={{ fontSize: 12 }}>
        观众买的是过程（环境/道具增益），不买结果——无免死、最终判定不可购买；礼物经系统频道代投，不走玩家道具干预。
      </Text>
    </Card>
  );
};

/** 实时动态条：把流里的赛制系统事件（淘汰/胜者/打赏）按到达顺序倒序展示。 */
const LiveTicker: React.FC<{ events: ArenaReplayEvent[]; nameOf: (id: string) => string }> = ({ events, nameOf }) => {
  if (events.length === 0) {
    return <Empty description="等待实时赛况…" image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }
  return (
    <Space direction="vertical" size={8} style={{ width: '100%' }}>
      {events
        .slice()
        .reverse()
        .slice(0, 20)
        .map((ev) => {
          const meta = arenaEventKindMeta(ev.type);
          const who = ev.characterId ? nameOf(ev.characterId) : '';
          return (
            <Space key={`${ev.sequence}-${ev.id}`} size={6} wrap>
              <Tag color={meta.color}>{meta.label}</Tag>
              {who && <Text style={{ color: '#33312e' }}>{who}</Text>}
              <Text type="secondary" style={{ fontSize: 12 }}>
                {ev.summary}
              </Text>
            </Space>
          );
        })}
    </Space>
  );
};

const ArenaSpectate: React.FC = () => {
  const { worldId } = useParams<{ worldId: string }>();
  const navigate = useNavigate();

  const [report, setReport] = useState<ArenaReport | null>(null);
  const [world, setWorld] = useState<WorldDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // 实时赛制事件（arena_elim/winner/gift）：由 cloudStream 合并，去重按 sequence。
  const [liveEvents, setLiveEvents] = useState<ArenaReplayEvent[]>([]);

  const loadReport = async () => {
    if (!worldId) return;
    setLoading(true);
    setError(null);
    try {
      const rep = await cloudFetch<ArenaReport>(`/api/arena/${worldId}/report`);
      setReport(rep);
    } catch (e) {
      setError(describeCloudError(e));
    } finally {
      setLoading(false);
    }
  };

  const loadWorld = async () => {
    if (!worldId) return;
    try {
      // best-effort：仅用于把角色 ID 显示为名字；失败静默回退 ID。
      const w = await cloudFetch<WorldDetail>(`/api/worlds/${worldId}`);
      setWorld(w);
    } catch {
      /* 名字非关键 */
    }
  };

  useEffect(() => {
    void loadReport();
    void loadWorld();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [worldId]);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of world?.roster ?? []) m.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    return (id: string) => m.get(id) || id;
  }, [world]);

  // 让流处理闭包始终读到最新 nameOf（world 异步加载后才有名字映射）。
  const nameOfRef = useRef(nameOf);
  nameOfRef.current = nameOf;

  // 实时观战：订阅世界流，只合并赛制系统事件（arena_*）。淘汰/胜者弹 toast；打赏/胜者补拉权威快照。
  useEffect(() => {
    if (!worldId) return;
    let unsub: (() => void) | null = null;
    try {
      unsub = cloudStream(
        worldId,
        (raw) => {
          const ev = raw as ArenaReplayEvent;
          if (!ev || typeof ev.sequence !== 'number') return;
          if (ev.type !== 'arena_elim' && ev.type !== 'arena_winner' && ev.type !== 'arena_gift') return;
          setLiveEvents((prev) => {
            if (prev.some((e) => e.sequence === ev.sequence)) return prev; // 按 sequence 去重
            return [...prev, ev].sort((a, b) => a.sequence - b.sequence);
          });
          const who = ev.characterId ? nameOfRef.current(ev.characterId) : '';
          if (ev.type === 'arena_elim') message.warning(`${who || '有角色'} 被淘汰`);
          if (ev.type === 'arena_winner') message.success(`唯一胜者：${who || '已产生'}`);
          // 打赏/胜者落定后补拉权威快照（环境日志 / eliminations / winner）。
          if (ev.type === 'arena_gift' || ev.type === 'arena_winner') void loadReport();
        },
        () => {
          // 实时流异常不致命：保留已加载战报，等待 cloudStream 自动重连补偿。
        },
      );
    } catch {
      // WebSocket 不可用（离线等）：降级为手动刷新，页面不崩。
    }
    return () => {
      if (unsub) unsub();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [worldId]);

  const eliminations = report?.match.eliminations ?? [];
  const rosterCount = world?.roster.length ?? 0;
  const remaining = Math.max(rosterCount - eliminations.length, 0);
  const winner = report?.match.winnerCharId ?? null;
  const phase = report?.match.phase ?? 'lobby';
  const phaseMeta = arenaPhaseMeta(phase);

  const chartOption = useMemo(() => {
    const data: Array<{ value: number; name: string; itemStyle: { color: string } }> = [];
    if (rosterCount > 0) data.push({ value: remaining, name: '现役在场', itemStyle: { color: '#d97757' } });
    if (eliminations.length > 0) data.push({ value: eliminations.length, name: '已淘汰', itemStyle: { color: '#cbb7a3' } });
    return {
      tooltip: { trigger: 'item' },
      legend: { bottom: 0 },
      series: [
        {
          type: 'pie',
          radius: ['45%', '70%'],
          avoidLabelOverlap: true,
          label: { formatter: '{b}: {c}' },
          data,
        },
      ],
    };
  }, [rosterCount, remaining, eliminations.length]);

  const hasChartData = rosterCount > 0 || eliminations.length > 0;

  if (loading && !report) {
    return (
      <div style={{ textAlign: 'center', padding: 80 }}>
        <Spin />
      </div>
    );
  }

  if (error && !report) {
    return (
      <div style={{ padding: '32px 40px', maxWidth: 900, margin: '0 auto' }}>
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={error}
          action={
            <Space>
              <Button size="small" onClick={() => void loadReport()}>
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

  if (!report) return null;

  const rounds = report.rounds ?? [];
  const environment = report.environment ?? [];

  return (
    <div style={{ padding: '24px 40px', maxWidth: 900, margin: '0 auto' }}>
      <Button
        type="text"
        icon={<LeftOutlined />}
        onClick={() => navigate('/platform')}
        style={{ marginBottom: 8, color: '#8c857b' }}
      >
        大厅
      </Button>

      {/* 头部：观战席 + 合规标识 */}
      <Card
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start" wrap>
          <Space direction="vertical" size={4}>
            <Space size={10} wrap>
              <Title level={3} style={{ margin: 0, color: '#33312e' }}>
                {world?.title || '赛事房'}
              </Title>
              <Tag icon={<EyeOutlined />} color="blue">
                观战席（只读）
              </Tag>
              <Tag color={phaseMeta.color}>{phaseMeta.label}</Tag>
            </Space>
            <Text type="secondary" style={{ fontSize: 12 }}>
              透明战报 · 只出结果与判定依据，不含模型隐藏推理
            </Text>
          </Space>
          <Space direction="vertical" size={4} align="end">
            <Tag icon={<RobotOutlined />} color="orange">
              AI 生成内容
            </Tag>
            {report.compliance?.arbitrationPublic && (
              <Tag icon={<SafetyCertificateOutlined />} color="green">
                仲裁公开
              </Tag>
            )}
            <Space size={4}>
              <Button size="small" type="text" icon={<ReloadOutlined />} onClick={() => void loadReport()}>
                刷新
              </Button>
              {phase === 'concluded' && (
                <Button
                  size="small"
                  type="text"
                  icon={<PlayCircleOutlined />}
                  onClick={() => navigate(`/platform/arena/${worldId}/replay`)}
                >
                  回放
                </Button>
              )}
            </Space>
          </Space>
        </Space>
      </Card>

      {/* 红线：透明战报的意义 + 付费边界 */}
      <Alert
        type="info"
        showIcon
        style={{ marginBottom: 16, background: '#faf9f5', border: '1px solid #eae6df' }}
        message="透明战报：对抗「是不是剧本」的质疑"
        description="每回合仲裁器输出可查战报——谁做了什么、判定依据（ruleRefs）、道具与环境生效记录。观众买的是过程（道具、复活赛资格），不买结果；无免死道具，最终判定不可购买；胜者奖励为荣誉性。"
      />

      {/* 胜者荣誉展示（非强度） */}
      {winner && (
        <Card
          style={{ marginBottom: 16, borderRadius: 12, border: '1px solid #f0d9c8', background: '#fff7f0' }}
          styles={{ body: { padding: 16 } }}
        >
          <Space size={10} align="center">
            <TrophyOutlined style={{ color: '#d4a017', fontSize: 22 }} />
            <Space direction="vertical" size={0}>
              <Text strong style={{ color: '#33312e' }}>
                唯一胜者：{nameOf(winner)}
              </Text>
              <Text type="secondary" style={{ fontSize: 12 }}>
                荣誉性奖励（称号 / 立绘框 / 赛季榜），非强度加成。
              </Text>
            </Space>
          </Space>
        </Card>
      )}

      <div style={{ display: 'flex', gap: 16, alignItems: 'flex-start', flexWrap: 'wrap' }}>
        {/* 战报时间轴 */}
        <div style={{ flex: '1 1 480px', minWidth: 0 }}>
          <Card
            title="战报时间轴"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 20 } }}
          >
            {rounds.length === 0 ? (
              <Empty description="赛事尚未产生可公开的战报事件" />
            ) : (
              <Timeline
                items={rounds.map((round) => ({
                  color: '#d97757',
                  children: (
                    <div>
                      <Text strong style={{ color: '#33312e' }}>
                        第 {round.tick} 回合
                      </Text>
                      <Space direction="vertical" size={10} style={{ width: '100%', marginTop: 8 }}>
                        {round.events.map((ev) => {
                          const meta = eventTypeMeta(ev.type);
                          return (
                            <Card
                              key={ev.sequence}
                              size="small"
                              style={{ borderRadius: 8, border: '1px solid #eae6df' }}
                            >
                              <Space size={6} wrap style={{ marginBottom: 4 }}>
                                <Tag color={meta.color}>{meta.label}</Tag>
                                {ev.actors.length > 0 && (
                                  <Text type="secondary" style={{ fontSize: 12 }}>
                                    {ev.actors.map(nameOf).join('、')}
                                  </Text>
                                )}
                              </Space>
                              <Paragraph style={{ margin: '2px 0', color: '#33312e' }}>
                                {String(ev.summary) || '（无摘要）'}
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
                              {round.env.length > 0 && (
                                <div style={{ marginTop: 6 }}>
                                  {round.env.map((e, i) => (
                                    <EnvEventRow key={i} env={e} />
                                  ))}
                                </div>
                              )}
                            </Card>
                          );
                        })}
                      </Space>
                    </div>
                  ),
                }))}
              />
            )}
          </Card>
        </div>

        {/* 侧栏：实时动态 + 打赏入口 + 阵容/淘汰图 + 礼物/环境日志 */}
        <div style={{ flex: '0 1 320px', minWidth: 280, display: 'flex', flexDirection: 'column', gap: 16 }}>
          <Card
            title={
              <Space size={6}>
                <span>实时动态</span>
                <Tag color="processing" style={{ marginInlineEnd: 0 }}>
                  LIVE
                </Tag>
              </Space>
            }
            size="small"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 12 } }}
          >
            <LiveTicker events={liveEvents} nameOf={nameOf} />
          </Card>

          {worldId && <GiftBar worldId={worldId} onGifted={() => void loadReport()} />}

          <Card
            title="阵容 / 淘汰"
            size="small"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 12 } }}
          >
            {hasChartData ? (
              <ReactECharts option={chartOption} style={{ height: 240 }} notMerge />
            ) : (
              <Empty description="暂无阵容数据" image={Empty.PRESENTED_IMAGE_SIMPLE} />
            )}
          </Card>

          <Card
            title="礼物 / 环境日志"
            size="small"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 12 } }}
          >
            {environment.length === 0 ? (
              <Empty description="暂无礼物 / 环境事件" image={Empty.PRESENTED_IMAGE_SIMPLE} />
            ) : (
              <Space direction="vertical" size={8} style={{ width: '100%' }}>
                {environment.map((e, i) => (
                  <div key={i}>
                    <EnvEventRow env={e} />
                  </div>
                ))}
              </Space>
            )}
            <Divider style={{ margin: '10px 0' }} />
            <Text type="secondary" style={{ fontSize: 12 }}>
              观众礼物经平台网关映射为场内环境事件（专用系统通道），不走玩家道具干预。
            </Text>
          </Card>
        </div>
      </div>
    </div>
  );
};

export default ArenaSpectate;
