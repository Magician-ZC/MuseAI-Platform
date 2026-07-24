// 关系图谱（P0 #3）：由 WorldRoom 内联的 RelationGraph 迁出并升级。
// - 节点 size ∝ activity（不再用共现 weight）、按 arcStage 五色、我方角色 #d97757 描边环。
// - 维度切换器（信任/亲和/恐惧/负债）决定边的绿(正)红(负)配色与粗细。
// - emphasis focus:'adjacency' 邻居高亮；点击节点 → 右侧角色状态卡（arcStage/activity/与我方关系数值）。
// - 缺权威 summary（relations 空）时回退共现图（buildCooccurrenceGraph）。
import React, { useMemo, useState } from 'react';
import { Card, Empty, Segmented, Space, Tag, Typography } from 'antd';
import type {
  WorldRelation,
  WorldCharacterState,
  WorldRosterEntry,
  WorldEventItem,
} from '../../stores/usePlatformStore';
import ForceGraph from './ForceGraph';
import {
  arcStageColor,
  arcStageLabel,
  buildCooccurrenceGraph,
  buildRelationGraph,
  MINE_RING_COLOR,
  OTHER_NODE_COLOR,
  RELATION_DIMENSION_LABEL,
  RELATION_NEGATIVE_COLOR,
  RELATION_POSITIVE_COLOR,
  type GraphNode,
  type RelationDimension,
} from './model';

const { Text } = Typography;

const DIMENSION_OPTIONS: Array<{ label: string; value: RelationDimension }> = (
  ['trust', 'affinity', 'fear', 'debt'] as RelationDimension[]
).map((v) => ({ label: RELATION_DIMENSION_LABEL[v], value: v }));

/** 我方 ↔ 选中角色的关系数值卡（双向都取，缺失记 —）。 */
function relationsWithMine(
  charId: string,
  relations: WorldRelation[],
  myIds: Set<string>,
  nameOf: Map<string, string>,
): Array<{ other: string; otherName: string; outgoing?: WorldRelation; incoming?: WorldRelation }> {
  const out: Array<{ other: string; otherName: string; outgoing?: WorldRelation; incoming?: WorldRelation }> = [];
  const byOther = new Map<string, { outgoing?: WorldRelation; incoming?: WorldRelation }>();
  for (const rel of relations) {
    if (rel.from === charId && myIds.has(rel.to)) {
      const e = byOther.get(rel.to) ?? {};
      e.outgoing = rel;
      byOther.set(rel.to, e);
    }
    if (rel.to === charId && myIds.has(rel.from)) {
      const e = byOther.get(rel.from) ?? {};
      e.incoming = rel;
      byOther.set(rel.from, e);
    }
  }
  for (const [other, e] of byOther) {
    out.push({ other, otherName: nameOf.get(other) || other, ...e });
  }
  return out;
}

const CharacterStatePanel: React.FC<{
  node: GraphNode;
  relations: WorldRelation[];
  myIds: Set<string>;
  nameOf: Map<string, string>;
}> = ({ node, relations, myIds, nameOf }) => {
  const rels = useMemo(
    () => (node.mine ? [] : relationsWithMine(node.id, relations, myIds, nameOf)),
    [node, relations, myIds, nameOf],
  );
  return (
    <Card size="small" style={{ borderRadius: 10, border: '1px solid #eae6df', background: '#fffdfa' }}>
      <Space direction="vertical" size={8} style={{ width: '100%' }}>
        <Space size={8} wrap>
          <Text strong>{node.label}</Text>
          {node.mine && <Tag color="orange">我的角色</Tag>}
          {node.arcStage && (
            <Tag color="blue" style={{ borderColor: arcStageColor(node.arcStage) }}>
              弧光 · {arcStageLabel(node.arcStage)}
            </Tag>
          )}
        </Space>
        <Text type="secondary" style={{ fontSize: 12 }}>
          活跃度 {typeof node.activity === 'number' ? node.activity : '—'}
        </Text>
        {!node.mine && (
          <>
            <Text type="secondary" style={{ fontSize: 12 }}>
              与我方关系
            </Text>
            {rels.length === 0 ? (
              <Text type="secondary" style={{ fontSize: 12 }}>
                暂无与你角色的权威关系数据
              </Text>
            ) : (
              rels.map((r) => {
                const rel = r.outgoing ?? r.incoming;
                if (!rel) return null;
                return (
                  <Space key={r.other} size={6} wrap style={{ fontSize: 12 }}>
                    <Tag>{r.otherName}</Tag>
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      信任 {rel.trust} · 亲和 {rel.affinity} · 恐惧 {rel.fear} · 负债 {rel.debt}
                    </Text>
                  </Space>
                );
              })
            )}
          </>
        )}
      </Space>
    </Card>
  );
};

export const RelationForceGraph: React.FC<{
  roster: WorldRosterEntry[];
  events: WorldEventItem[];
  /** 权威关系（#6b）；提供且非空时优先，否则退回事件共现启发式。 */
  relations?: WorldRelation[];
  /** 权威角色状态（弧光阶段 / 活跃度）。 */
  characters?: WorldCharacterState[];
  myIds?: Set<string>;
  /** 透传给 ForceGraph 的测试定位（多图同页时用以区分；默认 'echarts-graph'）。 */
  testId?: string;
}> = ({ roster, events, relations, characters, myIds, testId }) => {
  const mine = myIds ?? new Set<string>();
  const authoritative = !!relations && relations.length > 0;
  const [dimension, setDimension] = useState<RelationDimension>('affinity');
  const [selected, setSelected] = useState<GraphNode | null>(null);

  const model = useMemo(
    () =>
      authoritative
        ? buildRelationGraph({
            roster,
            relations: relations as WorldRelation[],
            characters,
            myIds: mine,
            dimension,
          })
        : buildCooccurrenceGraph({ roster, events, myIds: mine }),
    [authoritative, roster, relations, characters, events, mine, dimension],
  );

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of roster) m.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    return m;
  }, [roster]);

  if (model.nodes.length === 0) {
    return <Empty description="暂无角色，无法绘制关系图谱" />;
  }

  return (
    <div>
      {authoritative && (
        <Space style={{ marginBottom: 12 }} size={10} wrap>
          <Text type="secondary" style={{ fontSize: 12 }}>
            关系维度
          </Text>
          <Segmented
            size="small"
            options={DIMENSION_OPTIONS}
            value={dimension}
            onChange={(v) => setDimension(v as RelationDimension)}
          />
        </Space>
      )}
      <ForceGraph
        nodes={model.nodes}
        links={model.links}
        onNodeClick={(n) => setSelected(n)}
        height={440}
        labelPosition="bottom"
        testId={testId}
      />
      {selected && (
        <div style={{ marginTop: 12 }}>
          <CharacterStatePanel node={selected} relations={relations ?? []} myIds={mine} nameOf={nameOf} />
        </div>
      )}
      <Space size={16} style={{ marginTop: 8 }} wrap>
        <Text type="secondary" style={{ fontSize: 12 }}>
          <span style={{ color: MINE_RING_COLOR }}>◎</span> 我的角色（描边环）
        </Text>
        {authoritative ? (
          <>
            <Text type="secondary" style={{ fontSize: 12 }}>
              <span style={{ color: RELATION_POSITIVE_COLOR }}>—</span> 正向{' '}
              <span style={{ color: RELATION_NEGATIVE_COLOR }}>—</span> 负向
            </Text>
            <Text type="secondary" style={{ fontSize: 12 }}>
              数据源：权威关系状态（节点大小∝活跃度·配色=弧光阶段；边={RELATION_DIMENSION_LABEL[dimension]}，绿正红负，粗细∝强度）
            </Text>
          </>
        ) : (
          <Text type="secondary" style={{ fontSize: 12 }}>
            <span style={{ color: OTHER_NODE_COLOR }}>●</span> 其他角色 · 数据源：由观测事件共现推导（连线粗细=共同参与次数）
          </Text>
        )}
      </Space>
    </div>
  );
};

export default RelationForceGraph;
