// 赛事房观战席 + 透明战报（P6，FE1 所有；规格 §2.5 / §2.7 / §9.4）：只读，无干预/同意面板。
// GET /arena/{id}/report —— 事件时间轴 + 判定依据 ruleRefs（对抗「是不是剧本」质疑）+ 礼物/环境日志。
// echarts 阵容/淘汰图；胜者奖励荣誉性展示；观众买过程不买结果。
// Local-first：仅平台路由；云端故障显示错误卡不崩；角色名 best-effort（取不到回退角色 ID）。
import React, { useEffect, useMemo, useState } from 'react';
import { Typography, Card, Tag, Space, Alert, Spin, Empty, Button, Timeline, Divider } from 'antd';
import {
  TrophyOutlined,
  SafetyCertificateOutlined,
  RobotOutlined,
  GiftOutlined,
  EyeOutlined,
  LeftOutlined,
  ReloadOutlined,
} from '@ant-design/icons';
import { useParams, useNavigate } from 'react-router-dom';
import ReactECharts from 'echarts-for-react';
import { cloudFetch } from '../../utils/cloudApi';
import {
  describeCloudError,
  arenaPhaseMeta,
  eventTypeMeta,
  type WorldDetail,
  type ArenaReport,
  type ArenaEnvEvent,
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

const ArenaSpectate: React.FC = () => {
  const { worldId } = useParams<{ worldId: string }>();
  const navigate = useNavigate();

  const [report, setReport] = useState<ArenaReport | null>(null);
  const [world, setWorld] = useState<WorldDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

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
            <Button size="small" type="text" icon={<ReloadOutlined />} onClick={() => void loadReport()}>
              刷新
            </Button>
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

        {/* 侧栏：阵容/淘汰图 + 礼物/环境日志 */}
        <div style={{ flex: '0 1 320px', minWidth: 280, display: 'flex', flexDirection: 'column', gap: 16 }}>
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
