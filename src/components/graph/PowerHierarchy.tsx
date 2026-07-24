// 势力图（P0 #4）：由 WorldRoom 内联的 FactionMap 迁出并升级。
// - 保留并查集聚类（buildPowerHierarchy，逻辑同旧 buildFactionMap）。
// - 势力分区着色（category 配色）+ 图例点击隔离单势力 + hover 高亮同势力（ForceGraph highlightCategory）。
// - 地点拓扑 seam 未就绪，仍以阵营聚合替代真实地图布局（保留说明）。
import React, { useMemo } from 'react';
import { Alert, Empty, Space, Tag, Typography } from 'antd';
import type { WorldRelation, WorldRosterEntry, WorldEventItem } from '../../stores/usePlatformStore';
import { resolveObjectUrl } from '../../utils/cloudApi';
import ForceGraph from './ForceGraph';
import { buildPowerHierarchy, FACTION_PALETTE } from './model';

const { Text } = Typography;

export const PowerHierarchy: React.FC<{
  roster: WorldRosterEntry[];
  events: WorldEventItem[];
  relations?: WorldRelation[];
  myIds?: Set<string>;
}> = ({ roster, events, relations, myIds }) => {
  const mine = myIds ?? new Set<string>();
  // 相对 avatarUrl 预解析为完整 URL（同 RelationForceGraph），保持 model.ts 纯函数无 base 依赖。
  const resolvedRoster = useMemo(
    () => roster.map((r) => (r.avatarUrl ? { ...r, avatarUrl: resolveObjectUrl(r.avatarUrl) } : r)),
    [roster],
  );
  const model = useMemo(
    () => buildPowerHierarchy({ roster: resolvedRoster, events, relations, myIds: mine }),
    [resolvedRoster, events, relations, mine],
  );

  // 按 category 归组，供底部势力成员列表。
  const factions = useMemo(() => {
    const groups = new Map<number, Array<{ id: string; name: string; mine: boolean }>>();
    for (const n of model.nodes) {
      const cat = n.category ?? 0;
      const arr = groups.get(cat) ?? [];
      arr.push({ id: n.id, name: n.label, mine: !!n.mine });
      groups.set(cat, arr);
    }
    return model.categories.map((c, i) => ({ name: c.name, members: groups.get(i) ?? [] }));
  }, [model]);

  if (model.nodes.length === 0) {
    return <Empty description="暂无角色，无法绘制势力图" />;
  }

  return (
    <div>
      <Alert
        type="info"
        showIcon
        style={{ marginBottom: 12 }}
        message="按阵营聚合呈现"
        description="世界模板的地点拓扑与角色坐标尚未由服务端下发（拓扑数据 seam）；当前依据结盟/冲突与权威关系将角色聚合为势力簇。图例可点击隔离单个势力，悬停节点高亮同势力成员。"
      />
      <ForceGraph
        nodes={model.nodes}
        links={model.links}
        categories={model.categories}
        highlightCategory
        highlightNeighbors={false}
        legend
        repulsion={220}
        edgeLength={90}
        height={380}
      />
      <Space direction="vertical" size={8} style={{ width: '100%', marginTop: 8 }}>
        {factions.map((f, i) => (
          <Space key={f.name} size={6} wrap>
            <Tag color="geekblue" style={{ borderColor: FACTION_PALETTE[i % FACTION_PALETTE.length] }}>
              {f.name}
            </Tag>
            <Text type="secondary" style={{ fontSize: 12 }}>
              {f.members.length} 名成员
            </Text>
            {f.members.map((m) => (
              <Tag key={m.id} color={m.mine ? 'orange' : 'default'}>
                {m.name}
              </Tag>
            ))}
          </Space>
        ))}
      </Space>
    </div>
  );
};

export default PowerHierarchy;
