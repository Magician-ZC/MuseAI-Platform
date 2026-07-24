// 角色一生档案（C1，规格 §2.5）：单角色跨世界的一生——身份卡 + 走过的世界 + 逐日人生 + 带来的信物 + 羁绊。
// 纯前端组合已有端点（fan-out，无新后端）；各分区错误相互隔离，任一失败不拖垮整页。
import React, { useEffect, useMemo, useState } from 'react';
import { Typography, Card, Tag, Alert, Spin, Empty, Space, Button, Timeline, List, Divider } from 'antd';
import {
  ReadOutlined,
  BranchesOutlined,
  GlobalOutlined,
  ShoppingOutlined,
  HeartOutlined,
  RobotOutlined,
  ArrowLeftOutlined,
} from '@ant-design/icons';
import { useParams, useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import { usePartnerStore } from '../../stores/usePartnerStore';
import {
  usePlatformStore,
  describeCloudError,
  roomTypeLabel,
  moderationMeta,
  type Membership,
  type ReportListItem,
  type BackpackItem,
  type CloudCharacter,
  type BondEdge,
  type WorldStateSummary,
  type WorldDetail,
} from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

interface ArchiveData {
  worlds: Membership[];
  reports: ReportListItem[];
  items: BackpackItem[];
  cloudChar?: CloudCharacter;
  bonds: BondEdge[];
}

const CharacterArchive: React.FC = () => {
  const { cid } = useParams<{ cid: string }>();
  const navigate = useNavigate();
  const loadMemberships = usePlatformStore((s) => s.loadMemberships);
  const localCards = usePartnerStore((s) => s.characterCardsV2);

  const [data, setData] = useState<ArchiveData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    if (!cid) return;
    setLoading(true);
    setError(null);
    try {
      // 主脊：memberships（世界清单来源）。失败且无数据 → 页面级错误。
      const ms = await loadMemberships();
      const memErr = usePlatformStore.getState().membershipsError;
      if (memErr && ms.length === 0) {
        setError(memErr);
        return;
      }
      // 次级分区（日报 / 背包 / 云端角色）各自失败降级为空，避免单点拖垮档案。
      const [reports, items, chars] = await Promise.all([
        cloudFetch<{ reports: ReportListItem[] }>('/api/me/reports')
          .then((d) => d.reports ?? [])
          .catch(() => [] as ReportListItem[]),
        cloudFetch<{ items: BackpackItem[] }>('/api/me/backpack')
          .then((d) => d.items ?? [])
          .catch(() => [] as BackpackItem[]),
        cloudFetch<CloudCharacter[]>('/api/assets/characters/mine').catch(() => [] as CloudCharacter[]),
      ]);

      const worlds = ms.filter((m) => m.cloudCharacterId === cid);
      const worldIds = new Set(worlds.map((w) => w.worldId));
      const reportsOfChar = reports.filter((r) => r.characterId === cid);
      // 取舍：backpacks 表按 user 归属、无 characterId → 按"获得世界 ∈ 该角色所在世界"近似归因。
      const itemsOfChar = items.filter((b) => worldIds.has(b.acquiredWorldId));
      const cloudChar = chars.find((c) => c.id === cid);

      // 羁绊：仅该角色所在世界（数量小）拉 state-summary + world 详情，抽含 cid 的边。
      const titleByWorld = new Map(worlds.map((w) => [w.worldId, w.worldTitle || w.worldId]));
      const bondLists = await Promise.all(
        [...worldIds].map(async (wid) => {
          try {
            const [summary, detail] = await Promise.all([
              cloudFetch<WorldStateSummary>(`/api/worlds/${wid}/state-summary`),
              cloudFetch<WorldDetail>(`/api/worlds/${wid}`).catch(() => null),
            ]);
            const nameOf = new Map<string, string>();
            for (const r of detail?.roster ?? []) nameOf.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
            const out: BondEdge[] = [];
            for (const rel of summary.relations ?? []) {
              const base = {
                worldId: wid,
                worldTitle: titleByWorld.get(wid) || wid,
                trust: rel.trust,
                affinity: rel.affinity,
                fear: rel.fear,
                debt: rel.debt,
              };
              if (rel.from === cid) {
                out.push({ ...base, myCharacterId: cid, otherCharacterId: rel.to, otherName: nameOf.get(rel.to) || rel.to, direction: 'out' });
              } else if (rel.to === cid) {
                out.push({ ...base, myCharacterId: cid, otherCharacterId: rel.from, otherName: nameOf.get(rel.from) || rel.from, direction: 'in' });
              }
            }
            return out;
          } catch {
            return [] as BondEdge[];
          }
        }),
      );
      const bonds = bondLists.flat().sort((a, b) => Math.abs(b.affinity) - Math.abs(a.affinity));

      setData({ worlds, reports: reportsOfChar, items: itemsOfChar, cloudChar, bonds });
    } catch (e) {
      setError(describeCloudError(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cid]);

  // 身份：本地卡（identity）优先，回退 membership 名 / cid。
  const localCard = useMemo(
    () => (data?.cloudChar ? localCards.find((c) => c.id === data.cloudChar!.localCardId) : undefined),
    [data, localCards],
  );
  const displayName =
    localCard?.identity?.name || data?.worlds[0]?.characterName || cid || '未知角色';
  const aliases = localCard?.identity?.aliases ?? [];
  const narrativeRole = localCard?.identity?.narrativeRole;

  if (loading && !data) {
    return (
      <div style={{ textAlign: 'center', padding: 80 }}>
        <Spin />
      </div>
    );
  }

  if (error && !data) {
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
              <Button size="small" type="text" onClick={() => navigate('/platform/characters')}>
                返回我的角色
              </Button>
            </Space>
          }
        />
      </div>
    );
  }

  if (!data) return null;
  const mod = data.cloudChar ? moderationMeta(data.cloudChar.moderation) : null;

  return (
    <div style={{ padding: '24px 40px', maxWidth: 900, margin: '0 auto' }}>
      <Button
        type="text"
        size="small"
        icon={<ArrowLeftOutlined />}
        onClick={() => navigate('/platform/characters')}
        style={{ color: '#8c857b', marginBottom: 12 }}
      >
        我的角色
      </Button>

      {/* 头部身份卡 */}
      <Card
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start" wrap>
          <Space direction="vertical" size={8}>
            <Title level={3} style={{ margin: 0, color: '#33312e' }}>
              {displayName}
            </Title>
            {(aliases.length > 0 || narrativeRole) && (
              <Space size={4} wrap>
                {narrativeRole && <Tag color="purple">{narrativeRole}</Tag>}
                {aliases.map((t) => (
                  <Tag key={t} color="geekblue">
                    {t}
                  </Tag>
                ))}
              </Space>
            )}
            <Text type="secondary" style={{ fontSize: 12 }}>
              走过 {data.worlds.length} 个世界 · {data.reports.length} 份日记 · {data.items.length} 件信物 ·{' '}
              {data.bonds.length} 段羁绊
            </Text>
          </Space>
          <Space direction="vertical" size={6} align="end">
            <Tag icon={<RobotOutlined />} color="orange">
              AI 生成内容
            </Tag>
            {mod && <Tag color={mod.color}>{mod.label}</Tag>}
          </Space>
        </Space>
      </Card>

      {/* 走过的世界 */}
      <Card
        title={
          <Space>
            <GlobalOutlined style={{ color: '#d97757' }} /> 走过的世界
          </Space>
        }
        size="small"
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 18 } }}
      >
        {data.worlds.length === 0 ? (
          <Empty description="TA 还未踏入任何世界" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <Timeline
            items={data.worlds.map((w) => ({
              color: '#d97757',
              children: (
                <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start" wrap>
                  <Space size={8} wrap>
                    <Text strong style={{ color: '#33312e' }}>
                      {w.worldTitle || w.worldId}
                    </Text>
                    <Tag color="orange">{roomTypeLabel(w.roomType)}</Tag>
                  </Space>
                  <Space size={6}>
                    <Button
                      size="small"
                      icon={<BranchesOutlined />}
                      onClick={() => navigate(`/platform/worlds/${w.worldId}?character=${cid}`)}
                    >
                      世界线
                    </Button>
                    <Button size="small" type="text" onClick={() => navigate(`/platform/worlds/${w.worldId}`)}>
                      进入
                    </Button>
                  </Space>
                </Space>
              ),
            }))}
          />
        )}
      </Card>

      {/* 逐日人生（日报） */}
      <Card
        title={
          <Space>
            <ReadOutlined style={{ color: '#d97757' }} /> 逐日人生
          </Space>
        }
        size="small"
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 18 } }}
      >
        {data.reports.length === 0 ? (
          <Empty description="还没有日记——过完第一天，TA 的昨日人生会写在这里" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <List
            dataSource={data.reports}
            rowKey={(r) => r.id}
            renderItem={(r) => (
              <List.Item
                style={{ paddingInline: 0, cursor: 'pointer' }}
                onClick={() => navigate(`/platform/reports/${r.id}`)}
                actions={[
                  <Button key="open" size="small" type="link">
                    阅读
                  </Button>,
                ]}
              >
                <Space size={8} wrap>
                  <Tag>{r.reportDay}</Tag>
                  {!r.opened && <Tag color="red">未读</Tag>}
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {data.worlds.find((w) => w.worldId === r.worldId)?.worldTitle || r.worldId}
                  </Text>
                </Space>
              </List.Item>
            )}
          />
        )}
      </Card>

      {/* TA 带来的信物 */}
      <Card
        title={
          <Space>
            <ShoppingOutlined style={{ color: '#d97757' }} /> TA 带来的信物
          </Space>
        }
        size="small"
        style={{ marginBottom: 16, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 18 } }}
      >
        <Text type="secondary" style={{ fontSize: 12 }}>
          按"获得世界属于该角色所在世界"近似归因（背包按账号归属，非按角色）。
        </Text>
        <Divider style={{ margin: '10px 0' }} />
        {data.items.length === 0 ? (
          <Empty description="TA 还没在世界里赢得信物" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <Space direction="vertical" size={10} style={{ width: '100%' }}>
            {data.items.map((it) => (
              <Card key={it.backpackId} size="small" style={{ borderRadius: 8, border: '1px solid #eae6df' }}>
                <Space size={8} wrap>
                  <Text strong>{it.item.id}</Text>
                  <Tag color="gold">强度 {it.item.origin.powerTier}</Tag>
                  {it.item.effectTags.map((t) => (
                    <Tag key={t} color="geekblue">
                      {t}
                    </Tag>
                  ))}
                </Space>
                <Paragraph style={{ margin: '6px 0 0', color: '#33312e' }}>{it.item.narrative}</Paragraph>
              </Card>
            ))}
          </Space>
        )}
      </Card>

      {/* TA 的羁绊 */}
      <Card
        title={
          <Space>
            <HeartOutlined style={{ color: '#d97757' }} /> TA 的羁绊
          </Space>
        }
        size="small"
        style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 18 } }}
      >
        {data.bonds.length === 0 ? (
          <Empty description="尚未结下你有权知道的羁绊" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <Space direction="vertical" size={8} style={{ width: '100%' }}>
            {data.bonds.map((b, i) => {
              const positive = b.affinity >= 0;
              return (
                <Space key={`${b.worldId}-${b.otherCharacterId}-${i}`} size={8} wrap>
                  <Tag color={b.direction === 'out' ? 'orange' : 'blue'}>
                    {b.direction === 'out' ? '我对 TA' : 'TA 对我'}
                  </Tag>
                  <Text strong style={{ color: '#8b7355' }}>
                    {b.otherName}
                  </Text>
                  <Tag color={positive ? 'green' : 'red'}>亲和 {b.affinity}</Tag>
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {b.worldTitle}
                  </Text>
                </Space>
              );
            })}
          </Space>
        )}
      </Card>
    </div>
  );
};

export default CharacterArchive;
