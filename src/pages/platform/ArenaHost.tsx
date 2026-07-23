// 赛事房主播控制台（P6，FE1 所有；规格 §2.5）：赛制状态 + 触发回合 + 淘汰(同意门控) + 结算 + 复活资格。
// 对接 server arena：host/tick、report、eliminate、settle、revive-match（AuthUser + host 守卫）。
// 红线 UI（写进文案）：买过程不买结果、无免死道具、胜者奖励荣誉非强度、淘汰不可逆需当事人同意。
// Local-first：仅平台路由；云端故障显示错误卡不崩；非主播（403）友好提示并禁用控制。
import React, { useEffect, useMemo, useState } from 'react';
import {
  Typography,
  Card,
  Button,
  Space,
  Alert,
  Tag,
  Select,
  Spin,
  Statistic,
  Divider,
  List,
  Empty,
} from 'antd';
import {
  TrophyOutlined,
  ThunderboltOutlined,
  StopOutlined,
  ReloadOutlined,
  SafetyCertificateOutlined,
  TeamOutlined,
  EyeOutlined,
  LeftOutlined,
} from '@ant-design/icons';
import { useParams, useNavigate } from 'react-router-dom';
import { cloudFetch, CloudError } from '../../utils/cloudApi';
import {
  describeCloudError,
  arenaPhaseMeta,
  type WorldDetail,
  type ArenaReport,
} from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

type Feedback = { type: 'success' | 'error' | 'warning' | 'info'; text: string };

const ArenaHost: React.FC = () => {
  const { worldId } = useParams<{ worldId: string }>();
  const navigate = useNavigate();

  const [world, setWorld] = useState<WorldDetail | null>(null);
  const [report, setReport] = useState<ArenaReport | null>(null);
  const [worldError, setWorldError] = useState<string | null>(null);
  const [reportError, setReportError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const [notHost, setNotHost] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<Feedback | null>(null);
  const [consentNotice, setConsentNotice] = useState<string | null>(null);
  const [targetChar, setTargetChar] = useState<string | undefined>(undefined);

  const loadWorld = async () => {
    if (!worldId) return;
    setWorldError(null);
    try {
      const w = await cloudFetch<WorldDetail>(`/api/worlds/${worldId}`);
      setWorld(w);
      setTargetChar((prev) => prev ?? w.roster[0]?.cloudCharacterId);
    } catch (e) {
      setWorldError(describeCloudError(e));
    }
  };

  const refreshReport = async () => {
    if (!worldId) return;
    try {
      const rep = await cloudFetch<ArenaReport>(`/api/arena/${worldId}/report`);
      setReport(rep);
      setReportError(null);
    } catch (e) {
      // 战报非致命（如赛事未开赛 / 暂不可读）：降级为提示，控制台仍可用。
      setReportError(describeCloudError(e));
    }
  };

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void Promise.allSettled([loadWorld(), refreshReport()]).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [worldId]);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of world?.roster ?? []) m.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    return m;
  }, [world]);

  const rosterOptions = useMemo(
    () => (world?.roster ?? []).map((r) => ({ label: r.name || r.cloudCharacterId, value: r.cloudCharacterId })),
    [world],
  );

  const eliminations = report?.match.eliminations ?? [];
  const rosterCount = world?.roster.length ?? 0;
  const remaining = Math.max(rosterCount - eliminations.length, 0);
  const phase = report?.match.phase ?? 'lobby';
  const winner = report?.match.winnerCharId ?? null;
  const phaseMeta = arenaPhaseMeta(phase);

  // 统一动作错误处理：403 → 非主播友好提示并锁定控制；其余 → 友好中文。
  const handleError = (e: unknown) => {
    if (e instanceof CloudError && e.code === 'forbidden') {
      setNotHost(true);
      setFeedback({ type: 'warning', text: '只有本世界主播可控制赛事房。你可改用观战席查看透明战报。' });
    } else {
      setFeedback({ type: 'error', text: describeCloudError(e) });
    }
  };

  const doTick = async () => {
    if (!worldId) return;
    setBusy('tick');
    setFeedback(null);
    setConsentNotice(null);
    try {
      await cloudFetch(`/api/arena/${worldId}/host/tick`, { method: 'POST', idempotent: true });
      setFeedback({ type: 'success', text: '已触发一个回合，主播可在节拍间解说；引擎将在下一拍推进。' });
      await refreshReport();
    } catch (e) {
      handleError(e);
    } finally {
      setBusy(null);
    }
  };

  const doEliminate = async () => {
    if (!worldId || !targetChar) {
      setFeedback({ type: 'warning', text: '请先选择要裁定淘汰的参赛角色' });
      return;
    }
    setBusy('eliminate');
    setFeedback(null);
    setConsentNotice(null);
    try {
      const resp = await cloudFetch<{ status: string; consentId?: string }>(
        `/api/arena/${worldId}/eliminate`,
        { method: 'POST', idempotent: true, body: { cloudCharacterId: targetChar } },
      );
      const label = nameOf.get(targetChar) || targetChar;
      setConsentNotice(
        `已对「${label}」发起淘汰提案（状态：${resp.status}）。玩家角色淘汰不可逆，须当事人（角色主人）同意后，才会在「结算」时落定；` +
          `拒绝或超时未回应，将保守处理为免于淘汰。此处不出现任何免死道具或直接判定。`,
      );
      setFeedback({ type: 'info', text: '淘汰提案已提交，等待当事人同意。' });
      await refreshReport();
    } catch (e) {
      handleError(e);
    } finally {
      setBusy(null);
    }
  };

  const doSettle = async () => {
    if (!worldId) return;
    setBusy('settle');
    setFeedback(null);
    try {
      await cloudFetch(`/api/arena/${worldId}/settle`, { method: 'POST' });
      setFeedback({
        type: 'success',
        text: '已结算：仅当事人同意的淘汰才落定；拒绝或超时保守免淘汰。若现役仅剩 1 人则收敛为唯一胜者。',
      });
      await refreshReport();
    } catch (e) {
      handleError(e);
    } finally {
      setBusy(null);
    }
  };

  const doRevive = async () => {
    if (!worldId || !targetChar) {
      setFeedback({ type: 'warning', text: '请先选择要授予复活资格的参赛角色' });
      return;
    }
    setBusy('revive');
    setFeedback(null);
    setConsentNotice(null);
    try {
      await cloudFetch(`/api/arena/${worldId}/revive-match`, {
        method: 'POST',
        idempotent: true,
        body: { cloudCharacterId: targetChar },
      });
      const label = nameOf.get(targetChar) || targetChar;
      setFeedback({
        type: 'success',
        text: `已为「${label}」登记复活赛资格。买的是复活赛「资格」（过程），不是免死、也不改最终判定（结果）。`,
      });
      await refreshReport();
    } catch (e) {
      handleError(e);
    } finally {
      setBusy(null);
    }
  };

  if (loading && !world) {
    return (
      <div style={{ textAlign: 'center', padding: 80 }}>
        <Spin />
      </div>
    );
  }

  if (worldError && !world) {
    return (
      <div style={{ padding: '32px 40px', maxWidth: 960, margin: '0 auto' }}>
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
    <div style={{ padding: '24px 40px', maxWidth: 960, margin: '0 auto' }}>
      <Button
        type="text"
        icon={<LeftOutlined />}
        onClick={() => navigate('/platform')}
        style={{ marginBottom: 8, color: '#8c857b' }}
      >
        大厅
      </Button>

      {/* 头部：赛制状态 */}
      <Card
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start" wrap>
          <Space direction="vertical" size={4}>
            <Space size={10}>
              <Title level={3} style={{ margin: 0, color: '#33312e' }}>
                {world.title}
              </Title>
              <Tag color="orange">赛事房 · 主播控制台</Tag>
              <Tag color={phaseMeta.color}>{phaseMeta.label}</Tag>
            </Space>
            <Text type="secondary" style={{ fontSize: 12 }}>
              唯一胜者赛制 · 现役参赛角色收敛到 1 人即为胜者
            </Text>
          </Space>
          <Space>
            <Button icon={<EyeOutlined />} onClick={() => navigate(`/platform/arena/${world.id}/spectate`)}>
              查看透明战报
            </Button>
            <Button icon={<ReloadOutlined />} loading={busy === null && loading} onClick={() => void refreshReport()}>
              刷新
            </Button>
          </Space>
        </Space>

        <Divider style={{ margin: '16px 0' }} />

        <Space size={40} wrap>
          <Statistic title="阵容" value={rosterCount} suffix="人" valueStyle={{ color: '#33312e' }} />
          <Statistic title="已淘汰" value={eliminations.length} suffix="人" valueStyle={{ color: '#33312e' }} />
          <Statistic
            title="现役在场"
            value={remaining}
            suffix="人"
            valueStyle={{ color: '#d97757' }}
            prefix={<TeamOutlined />}
          />
          <div>
            <Text type="secondary" style={{ fontSize: 12, display: 'block', marginBottom: 4 }}>
              唯一胜者
            </Text>
            {winner ? (
              <Tag icon={<TrophyOutlined />} color="gold">
                {nameOf.get(winner) || winner}
              </Tag>
            ) : (
              <Text type="secondary">尚未产生</Text>
            )}
          </div>
        </Space>
      </Card>

      {/* 非主播 / 战报错误提示 */}
      {notHost && (
        <Alert
          type="warning"
          showIcon
          style={{ marginBottom: 16 }}
          message="你不是本世界主播"
          description="赛制控制（触发回合 / 淘汰 / 结算）仅限主播。你可改用观战席查看透明战报。"
          action={
            <Button size="small" onClick={() => navigate(`/platform/arena/${world.id}/spectate`)}>
              去观战
            </Button>
          }
        />
      )}
      {reportError && !notHost && (
        <Alert
          type="warning"
          showIcon
          style={{ marginBottom: 16 }}
          message="战报暂不可读"
          description={reportError}
        />
      )}

      <div style={{ display: 'flex', gap: 16, alignItems: 'flex-start', flexWrap: 'wrap' }}>
        {/* 控制区 */}
        <div style={{ flex: '1 1 520px', minWidth: 0, display: 'flex', flexDirection: 'column', gap: 16 }}>
          {/* 回合与结算 */}
          <Card
            title="回合 / 结算"
            size="small"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 16 } }}
          >
            <Space direction="vertical" size={12} style={{ width: '100%' }}>
              <Space wrap>
                <Button
                  type="primary"
                  icon={<ThunderboltOutlined />}
                  loading={busy === 'tick'}
                  disabled={notHost}
                  onClick={() => void doTick()}
                >
                  触发一个回合
                </Button>
                <Button
                  icon={<SafetyCertificateOutlined />}
                  loading={busy === 'settle'}
                  disabled={notHost}
                  onClick={() => void doSettle()}
                >
                  结算
                </Button>
              </Space>
              <Text type="secondary" style={{ fontSize: 12 }}>
                触发回合复用既有引擎节拍（主播控制节奏）；结算只落定「已同意」的淘汰，并在现役仅剩 1 人时收敛唯一胜者。
              </Text>
            </Space>
          </Card>

          {/* 淘汰 / 复活资格 */}
          <Card
            title="裁定 · 淘汰与复活资格"
            size="small"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 16 } }}
          >
            {rosterOptions.length === 0 ? (
              <Empty description="暂无参赛角色" image={Empty.PRESENTED_IMAGE_SIMPLE} />
            ) : (
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Select
                  style={{ width: '100%' }}
                  value={targetChar}
                  onChange={setTargetChar}
                  options={rosterOptions}
                  placeholder="选择参赛角色"
                  aria-label="选择参赛角色"
                />
                <Space wrap>
                  <Button
                    danger
                    icon={<StopOutlined />}
                    loading={busy === 'eliminate'}
                    disabled={notHost}
                    onClick={() => void doEliminate()}
                  >
                    裁定淘汰
                  </Button>
                  <Button
                    icon={<ReloadOutlined />}
                    loading={busy === 'revive'}
                    onClick={() => void doRevive()}
                  >
                    授予复活资格
                  </Button>
                </Space>

                {/* 同意门控说明（淘汰不可逆 → 当事人同意才落定） */}
                <Alert
                  type="info"
                  showIcon
                  message="淘汰的同意门控"
                  description="玩家角色淘汰不可逆：裁定后只发起「淘汰提案」，须当事人（角色主人）同意，才在结算时落定；拒绝或超时保守免于淘汰。此处不存在免死道具，最终判定不可购买。"
                />

                {consentNotice && <Alert type="warning" showIcon message="淘汰提案已发起" description={consentNotice} />}
              </Space>
            )}
          </Card>

          {feedback && <Alert type={feedback.type} showIcon message={feedback.text} />}
        </div>

        {/* 侧栏：阵容状态 + 红线 */}
        <div style={{ flex: '0 1 340px', minWidth: 280, display: 'flex', flexDirection: 'column', gap: 16 }}>
          <Card
            title="阵容"
            size="small"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 12 } }}
          >
            {rosterCount === 0 ? (
              <Empty description="暂无角色" image={Empty.PRESENTED_IMAGE_SIMPLE} />
            ) : (
              <List
                size="small"
                dataSource={world.roster}
                rowKey={(r) => r.cloudCharacterId}
                renderItem={(r) => {
                  const out = eliminations.includes(r.cloudCharacterId);
                  const isWinner = winner === r.cloudCharacterId;
                  return (
                    <List.Item style={{ paddingInline: 0 }}>
                      <Space size={8}>
                        <Text delete={out} style={{ color: out ? '#8c857b' : '#33312e' }}>
                          {r.name || r.cloudCharacterId}
                        </Text>
                        {isWinner && (
                          <Tag icon={<TrophyOutlined />} color="gold">
                            胜者
                          </Tag>
                        )}
                        {out && <Tag color="default">已淘汰</Tag>}
                      </Space>
                    </List.Item>
                  );
                }}
              />
            )}
          </Card>

          {/* 红线：付费边界 */}
          <Card
            title="付费边界（红线）"
            size="small"
            style={{ borderRadius: 12, border: '1px solid #f0d9c8', background: '#fff7f0' }}
            styles={{ body: { padding: 16 } }}
          >
            <Space direction="vertical" size={8}>
              <Text style={{ color: '#33312e' }}>· 买过程不买结果：道具与复活赛资格可买。</Text>
              <Text style={{ color: '#33312e' }}>· 免死与最终判定不可买，无免死道具。</Text>
              <Text style={{ color: '#33312e' }}>· 胜者奖励为荣誉性（称号 / 立绘框 / 赛季榜），非强度加成。</Text>
              <Paragraph type="secondary" style={{ margin: 0, fontSize: 12 }}>
                每回合仲裁输出可查战报（谁做了什么、判定依据），对抗「是不是剧本」质疑。
              </Paragraph>
            </Space>
          </Card>
        </div>
      </div>
    </div>
  );
};

export default ArenaHost;
