// 羁绊（C1，规格 §2.5 关系 / §9.4 信息边界）：跨世界聚合各世界 state-summary.relations 中含我角色的有向边。
// 复用服务端已做的 principal 隔离（events/mod.rs 只返回 from==我 或 known_to 含我 的边）；前端只做展示层聚合。
// 取舍：state-summary 只给当前值（非历史序列），故展示为"当前羁绊强度"，不做趋势线。
import React, { useEffect, useMemo, useState } from 'react';
import { Typography, Card, Tag, Alert, Spin, Empty, Space, Tooltip, Button } from 'antd';
import { HeartOutlined, GlobalOutlined } from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import {
  usePlatformStore,
  type BondEdge,
  type WorldStateSummary,
  type WorldDetail,
  type Membership,
} from '../../stores/usePlatformStore';

const { Title, Text } = Typography;

/** 有限并发 map（仿 enrichWorldTitles：单项失败由 fn 内部静默兜底，不整体失败）。 */
async function runLimited<T, R>(items: T[], limit: number, fn: (item: T) => Promise<R>): Promise<R[]> {
  const results: R[] = new Array(items.length);
  let cursor = 0;
  const worker = async () => {
    while (cursor < items.length) {
      const idx = cursor;
      cursor += 1;
      results[idx] = await fn(items[idx]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return results;
}

/** 由一个世界的关系快照 + 我在该世界的角色集，抽出含我角色的羁绊边。 */
function edgesFromWorld(
  worldId: string,
  worldTitle: string,
  relations: WorldStateSummary['relations'],
  mineSet: Set<string>,
  nameOf: Map<string, string>,
): BondEdge[] {
  const edges: BondEdge[] = [];
  for (const rel of relations ?? []) {
    const base = { worldId, worldTitle, trust: rel.trust, affinity: rel.affinity, fear: rel.fear, debt: rel.debt };
    if (mineSet.has(rel.from)) {
      edges.push({
        ...base,
        myCharacterId: rel.from,
        otherCharacterId: rel.to,
        otherName: nameOf.get(rel.to) || rel.to,
        direction: 'out',
      });
    } else if (mineSet.has(rel.to)) {
      edges.push({
        ...base,
        myCharacterId: rel.to,
        otherCharacterId: rel.from,
        otherName: nameOf.get(rel.from) || rel.from,
        direction: 'in',
      });
    }
  }
  return edges;
}

/** 羁绊强度条：亲和值按当前集合归一（scale 无关），绿=亲近 红=疏离。 */
const AffinityBar: React.FC<{ affinity: number; max: number }> = ({ affinity, max }) => {
  const pct = Math.min(100, (Math.abs(affinity) / Math.max(1, max)) * 100);
  const positive = affinity >= 0;
  return (
    <div style={{ height: 8, borderRadius: 4, background: '#eae6df', overflow: 'hidden', width: '100%' }}>
      <div style={{ height: '100%', width: `${pct}%`, background: positive ? '#7cae7a' : '#d98b8b' }} />
    </div>
  );
};

const Bonds: React.FC = () => {
  const navigate = useNavigate();
  const loadMemberships = usePlatformStore((s) => s.loadMemberships);
  const membershipsError = usePlatformStore((s) => s.membershipsError);
  const [bonds, setBonds] = useState<BondEdge[]>([]);
  const [myNameOf, setMyNameOf] = useState<Map<string, string>>(new Map());
  const [loading, setLoading] = useState(true);

  const loadBonds = async () => {
    setLoading(true);
    const ms: Membership[] = await loadMemberships();
    // 我的角色名映射（供羁绊左侧显示"我的哪个角色"）。
    const nameMap = new Map<string, string>();
    const myByWorld = new Map<string, Set<string>>();
    const titleByWorld = new Map<string, string>();
    for (const m of ms) {
      nameMap.set(m.cloudCharacterId, m.characterName || m.cloudCharacterId);
      if (!myByWorld.has(m.worldId)) myByWorld.set(m.worldId, new Set());
      myByWorld.get(m.worldId)!.add(m.cloudCharacterId);
      titleByWorld.set(m.worldId, m.worldTitle || m.worldId);
    }
    setMyNameOf(nameMap);

    const worldIds = [...myByWorld.keys()];
    // 每世界并发拉 state-summary（关系）+ world 详情（他方角色名）；限 6 并发，单世界失败静默。
    const perWorld = await runLimited(worldIds, 6, async (wid) => {
      try {
        const [summary, detail] = await Promise.all([
          cloudFetch<WorldStateSummary>(`/api/worlds/${wid}/state-summary`),
          cloudFetch<WorldDetail>(`/api/worlds/${wid}`).catch(() => null),
        ]);
        const rosterNames = new Map<string, string>();
        for (const r of detail?.roster ?? []) rosterNames.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
        return edgesFromWorld(wid, titleByWorld.get(wid) || wid, summary.relations, myByWorld.get(wid)!, rosterNames);
      } catch {
        return [] as BondEdge[];
      }
    });

    const all = perWorld.flat().sort((a, b) => Math.abs(b.affinity) - Math.abs(a.affinity));
    setBonds(all);
    setLoading(false);
  };

  useEffect(() => {
    void loadBonds();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const maxAff = useMemo(() => Math.max(1, ...bonds.map((b) => Math.abs(b.affinity))), [bonds]);

  return (
    <div style={{ padding: '32px 40px', maxWidth: 900, margin: '0 auto' }}>
      <div style={{ marginBottom: 24 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <HeartOutlined style={{ color: '#d97757', marginRight: 10 }} />
          羁绊
        </Title>
        <Text type="secondary">你的角色在各世界结下的关系——只呈现你有权知道的那些（当事或知情），按亲和强度排序。</Text>
      </div>

      {membershipsError ? (
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={membershipsError}
          action={
            <Button size="small" onClick={() => void loadBonds()}>
              重试
            </Button>
          }
        />
      ) : loading ? (
        <div style={{ textAlign: 'center', padding: 60 }}>
          <Spin />
        </div>
      ) : bonds.length === 0 ? (
        <Empty description="还没有结下任何羁绊——让角色在世界里相遇、相知，关系会在这里显现" style={{ padding: 60 }}>
          <Button type="primary" onClick={() => navigate('/platform/characters')}>
            看看我的角色
          </Button>
        </Empty>
      ) : (
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          {bonds.map((b, i) => {
            const positive = b.affinity >= 0;
            return (
              <Card
                key={`${b.worldId}-${b.myCharacterId}-${b.otherCharacterId}-${i}`}
                size="small"
                style={{ borderRadius: 10, border: '1px solid #eae6df' }}
                styles={{ body: { padding: 16 } }}
              >
                <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start" wrap>
                  <Space size={8} wrap>
                    <Text strong style={{ color: '#33312e' }}>
                      {myNameOf.get(b.myCharacterId) || b.myCharacterId}
                    </Text>
                    <Tag color={b.direction === 'out' ? 'orange' : 'blue'}>
                      {b.direction === 'out' ? '我对 TA' : 'TA 对我'}
                    </Tag>
                    <Text strong style={{ color: '#8b7355' }}>
                      {b.otherName}
                    </Text>
                    <Tag color={positive ? 'green' : 'red'}>{positive ? '亲近' : '疏离'}</Tag>
                  </Space>
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    <GlobalOutlined /> {b.worldTitle}
                  </Text>
                </Space>
                <div style={{ margin: '10px 0 8px' }}>
                  <AffinityBar affinity={b.affinity} max={maxAff} />
                </div>
                <Space size={6} wrap>
                  <Tooltip title="信任">
                    <Tag color="geekblue">信任 {b.trust}</Tag>
                  </Tooltip>
                  <Tooltip title="亲和（正=亲近 负=疏离）">
                    <Tag color={positive ? 'green' : 'red'}>亲和 {b.affinity}</Tag>
                  </Tooltip>
                  <Tooltip title="恐惧">
                    <Tag color="purple">恐惧 {b.fear}</Tag>
                  </Tooltip>
                  <Tooltip title="亏欠">
                    <Tag color="gold">亏欠 {b.debt}</Tag>
                  </Tooltip>
                </Space>
              </Card>
            );
          })}
        </Space>
      )}
    </div>
  );
};

export default Bonds;
