// 图谱共享数据层（P0）：把世界权威状态（relations / characters）与事件流映射成通用力导向图模型。
// 纯函数、无 React / echarts 依赖，供关系图谱、势力图，以及 P1/P2 的其它可视化复用。
// 表现形式借鉴通用 force-directed graph（echarts force / d3-force）范式，不复用任何 novel-fan-graph 代码。
import type {
  WorldRelation,
  WorldCharacterState,
  WorldRosterEntry,
  WorldEventItem,
} from '../../stores/usePlatformStore';
import { charAvatarDataUri } from './glyphs';

// ---------- 通用图模型（对外契约，供 ForceGraph 与 P1/P2 复用） ----------

/** 一个图节点：{id,label,size,color,kind} 为契约必备，其余为呈现/侧栏可选增强。 */
export interface GraphNode {
  id: string;
  label: string;
  /** 视觉直径（px）。关系图 ∝ 活跃度；共现回退 ∝ 参与次数。 */
  size: number;
  /** 填充色。关系图按弧光阶段五色；势力图按势力配色。 */
  color: string;
  /** 语义类别：弧光阶段（setup/rising/...）、'coocc'（共现回退）、'faction'（势力）等。 */
  kind: string;
  /** 是否为「我方」角色（true 时 ForceGraph 加 #d97757 描边环）。 */
  mine?: boolean;
  /** echarts category 序号（势力图用于图例隔离 / 同类高亮）。 */
  category?: number;
  /** 侧栏角色状态卡用：弧光阶段原始码。 */
  arcStage?: string;
  /** 侧栏角色状态卡用：活跃度。 */
  activity?: number;
  /** echarts `image://` symbol（角色首字头像 / 地点图标）；缺省时 ForceGraph 用圆点。 */
  symbol?: string;
}

/** 一条图边：{source,target,weight,kind,dim} 为契约必备，其余为呈现可选增强。 */
export interface GraphLink {
  source: string;
  target: string;
  /** 边强度（∝ 线宽）。关系图取所选维度绝对值；共现取共同参与次数。 */
  weight: number;
  /** 语义类别：'relation'（关系维度）、'coocc'（共现）、'alliance' / 'conflict'（势力）。 */
  kind: string;
  /** 关系维度码（trust/affinity/fear/debt）或聚合依据，供 tooltip / 调试。 */
  dim?: string;
  /** 线色（正=绿 负=红 中性=灰；势力：盟好=绿 敌对=红）。 */
  color?: string;
  /** 显式线宽（势力图区分盟好/敌对）；缺省时 ForceGraph 按 weight 归一。 */
  width?: number;
  /** 虚线（势力图敌对边）。 */
  dashed?: boolean;
}

export interface GraphCategory {
  name: string;
}

export interface GraphModel {
  nodes: GraphNode[];
  links: GraphLink[];
  categories: GraphCategory[];
}

// ---------- 弧光阶段：标签 + 五色 ----------

/** 弧光阶段代码 → 中文标签（未知值回退原文）。关系图与状态面板共用。 */
export function arcStageLabel(stage: string): string {
  switch (stage) {
    case 'setup':
      return '铺垫';
    case 'rising':
      return '上升';
    case 'climax':
      return '高潮';
    case 'falling':
      return '回落';
    case 'resolution':
      return '收束';
    default:
      return stage;
  }
}

/** 弧光阶段五色（暖调，与主题 #d97757 / #8b7355 协调）。未知阶段回退中性棕。 */
export const ARC_STAGE_COLOR: Record<string, string> = {
  setup: '#a89b8c', // 铺垫 · 灰褐
  rising: '#6f9e6f', // 上升 · 绿
  climax: '#d9772f', // 高潮 · 橙
  falling: '#b07aa1', // 回落 · 藕紫
  resolution: '#6b8fae', // 收束 · 青蓝
};

const ARC_STAGE_FALLBACK_COLOR = '#8b7355';

export function arcStageColor(stage: string | undefined): string {
  if (!stage) return ARC_STAGE_FALLBACK_COLOR;
  return ARC_STAGE_COLOR[stage] ?? ARC_STAGE_FALLBACK_COLOR;
}

/** 我方角色描边环色（ForceGraph 与图例复用）。 */
export const MINE_RING_COLOR = '#d97757';
/** 非我方共现回退节点填充色。 */
export const OTHER_NODE_COLOR = '#8b7355';

/** 关系边正/负/中性配色。 */
export const RELATION_POSITIVE_COLOR = '#5b9a6f';
export const RELATION_NEGATIVE_COLOR = '#c15b5b';
export const RELATION_NEUTRAL_COLOR = '#cbb7a3';

/** 势力盟好/敌对配色。 */
export const ALLIANCE_COLOR = '#7cae7a';
export const CONFLICT_COLOR = '#d98b8b';

/** 势力分区配色盘（category 顺序取用）。 */
export const FACTION_PALETTE = [
  '#d97757',
  '#8b7355',
  '#6f8fae',
  '#7cae7a',
  '#b58bbf',
  '#c9a15a',
  '#7a9a9a',
  '#a9736b',
];

// ---------- 关系维度 ----------

export type RelationDimension = 'trust' | 'affinity' | 'fear' | 'debt';

export const RELATION_DIMENSION_LABEL: Record<RelationDimension, string> = {
  trust: '信任',
  affinity: '亲和',
  fear: '恐惧',
  debt: '负债',
};

function toIdSet(myIds: Set<string> | string[] | undefined): Set<string> {
  if (!myIds) return new Set();
  return myIds instanceof Set ? myIds : new Set(myIds);
}

function relationValue(rel: WorldRelation, dim: RelationDimension): number {
  const v = rel[dim];
  return typeof v === 'number' && Number.isFinite(v) ? v : 0;
}

function relationLinkColor(value: number): string {
  if (value > 0) return RELATION_POSITIVE_COLOR;
  if (value < 0) return RELATION_NEGATIVE_COLOR;
  return RELATION_NEUTRAL_COLOR;
}

function relationNodeSize(activity: number | undefined): number {
  // 头像节点需足够大以看清首字（≥38）。
  if (typeof activity !== 'number' || !Number.isFinite(activity)) return 42;
  return Math.max(38, Math.min(40 + activity * 4, 62));
}

/** 非我方角色头像描边环色（暖白，与主题一致）。我方用 MINE_RING_COLOR。 */
const OTHER_RING_COLOR = '#f3ece0';

/** roster → 已过审头像的完整 URL 映射（缺席即未过审 → 回退首字头像）。
 * roster.avatarUrl 由消费层预解析为完整 URL 后传入，本模块保持纯函数、不依赖 getPlatformBase。 */
function avatarUrlMap(roster: WorldRosterEntry[]): Map<string, string> {
  const m = new Map<string, string>();
  for (const r of roster) if (r.avatarUrl) m.set(r.cloudCharacterId, r.avatarUrl);
  return m;
}

/** 角色节点 echarts symbol：有过审头像（完整 URL）→ `image://<url>`；否则回退首字头像 SVG。 */
function characterNodeSymbol(
  avatarUrl: string | undefined,
  fallback: { name: string; fill: string; ring: string },
): string {
  if (avatarUrl) return `image://${avatarUrl}`;
  return charAvatarDataUri(fallback);
}

// ---------- 权威关系图（#3）：节点∝活跃度·弧光五色，边按所选维度绿正红负 ----------

/**
 * 由权威 relations + characters 构建关系图（纯函数）。
 * - 节点：阵容 ∪ 关系端点；size ∝ activity；color 按 arcStage 五色；mine 打标（ForceGraph 描边）。
 * - 边：每条关系一条，weight = |所选维度值|，color 按符号（正绿/负红/零灰），dim = 维度码。
 * 观众投影下 relations/characters 已由服务端按 principal 过滤（events/mod.rs），前端不做人工遮罩。
 */
export function buildRelationGraph(input: {
  roster: WorldRosterEntry[];
  relations: WorldRelation[];
  characters?: WorldCharacterState[];
  myIds?: Set<string> | string[];
  dimension: RelationDimension;
}): GraphModel {
  const { roster, relations, characters, dimension } = input;
  const mine = toIdSet(input.myIds);
  const stateOf = new Map<string, WorldCharacterState>();
  for (const c of characters ?? []) stateOf.set(c.id, c);

  const nodes = new Map<string, GraphNode>();
  const nameOf = new Map<string, string>();
  for (const r of roster) nameOf.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
  const avatarOf = avatarUrlMap(roster);

  const ensure = (id: string): GraphNode => {
    let n = nodes.get(id);
    if (!n) {
      const st = stateOf.get(id);
      const label = nameOf.get(id) || id;
      const color = arcStageColor(st?.arcStage);
      const isMine = mine.has(id);
      n = {
        id,
        label,
        size: relationNodeSize(st?.activity),
        color,
        kind: st?.arcStage ?? 'unknown',
        mine: isMine,
        arcStage: st?.arcStage,
        activity: st?.activity,
        symbol: characterNodeSymbol(avatarOf.get(id), {
          name: label,
          fill: color,
          ring: isMine ? MINE_RING_COLOR : OTHER_RING_COLOR,
        }),
      };
      nodes.set(id, n);
    }
    return n;
  };

  for (const r of roster) ensure(r.cloudCharacterId);

  const links: GraphLink[] = [];
  for (const rel of relations) {
    ensure(rel.from);
    ensure(rel.to);
    const value = relationValue(rel, dimension);
    links.push({
      source: rel.from,
      target: rel.to,
      weight: Math.abs(value),
      kind: 'relation',
      dim: dimension,
      color: relationLinkColor(value),
    });
  }

  return { nodes: [...nodes.values()], links, categories: [] };
}

// ---------- 共现回退图：缺权威 summary 时用事件共同参与推导 ----------

/**
 * 事件共现启发式（回退路径）：节点 size ∝ 参与次数，同一事件的参与者两两连边（weight=共现次数）。
 * mine 节点填 #d97757、其余填 #8b7355；边为中性灰。缺 state-summary 时替代 buildRelationGraph。
 */
export function buildCooccurrenceGraph(input: {
  roster: WorldRosterEntry[];
  events: WorldEventItem[];
  myIds?: Set<string> | string[];
}): GraphModel {
  const { roster, events } = input;
  const mine = toIdSet(input.myIds);
  const avatarOf = avatarUrlMap(roster);
  const nameOf = new Map<string, string>();
  const weight = new Map<string, number>();
  const order: string[] = [];
  const seen = (id: string) => {
    if (!weight.has(id)) {
      weight.set(id, 1);
      order.push(id);
    }
  };
  for (const r of roster) {
    nameOf.set(r.cloudCharacterId, r.name || r.cloudCharacterId);
    seen(r.cloudCharacterId);
  }

  const linkMap = new Map<string, GraphLink>();
  for (const ev of events) {
    for (const a of ev.actors) {
      seen(a);
      weight.set(a, (weight.get(a) ?? 1) + 1);
    }
    for (let i = 0; i < ev.actors.length; i += 1) {
      for (let j = i + 1; j < ev.actors.length; j += 1) {
        const [s, t] = [ev.actors[i], ev.actors[j]].sort();
        if (s === t) continue;
        const key = `${s}__${t}`;
        const existing = linkMap.get(key);
        if (existing) existing.weight += 1;
        else
          linkMap.set(key, {
            source: s,
            target: t,
            weight: 1,
            kind: 'coocc',
            color: RELATION_NEUTRAL_COLOR,
          });
      }
    }
  }

  const nodes: GraphNode[] = order.map((id) => {
    const w = weight.get(id) ?? 1;
    const label = nameOf.get(id) || id;
    const isMine = mine.has(id);
    const color = isMine ? MINE_RING_COLOR : OTHER_NODE_COLOR;
    return {
      id,
      label,
      size: Math.max(38, Math.min(38 + w * 3, 58)),
      color,
      kind: 'coocc',
      mine: isMine,
      symbol: characterNodeSymbol(avatarOf.get(id), {
        name: label,
        fill: color,
        ring: isMine ? MINE_RING_COLOR : OTHER_RING_COLOR,
      }),
    };
  });

  return { nodes, links: [...linkMap.values()], categories: [] };
}

// ---------- 势力图（#4）：并查集聚类 + 势力分区着色 ----------

/**
 * 势力聚类（并查集）：结盟事件 / 正亲和把角色并入同簇；冲突事件 / 负亲和 / 高恐惧作为跨簇敌对边。
 * 输出通用图模型：node.category = 势力序号、node.color = 势力配色、mine 打标；
 * 边 alliance=绿实线 / conflict=红虚线。categories 供 echarts 图例点击隔离与同势力高亮。
 * 地点拓扑数据 seam 未就绪（world-template 级坐标未下发），故以阵营聚合替代真实地图布局。
 */
export function buildPowerHierarchy(input: {
  roster: WorldRosterEntry[];
  events: WorldEventItem[];
  relations?: WorldRelation[];
  myIds?: Set<string> | string[];
}): GraphModel {
  const { roster, events, relations } = input;
  const mine = toIdSet(input.myIds);
  const avatarOf = avatarUrlMap(roster);

  const nameOf = new Map<string, string>();
  const ids: string[] = [];
  const parent = new Map<string, string>();
  const weight = new Map<string, number>();

  const add = (id: string, name?: string) => {
    if (!parent.has(id)) {
      parent.set(id, id);
      ids.push(id);
      weight.set(id, 1);
    }
    if (name && !nameOf.has(id)) nameOf.set(id, name);
    else if (!nameOf.has(id)) nameOf.set(id, id);
  };
  const find = (x: string): string => {
    let root = x;
    while (parent.get(root) !== root) root = parent.get(root) as string;
    let cur = x;
    while (parent.get(cur) !== root) {
      const nxt = parent.get(cur) as string;
      parent.set(cur, root);
      cur = nxt;
    }
    return root;
  };
  const union = (a: string, b: string) => {
    add(a);
    add(b);
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) parent.set(ra, rb);
  };

  for (const r of roster) add(r.cloudCharacterId, r.name || r.cloudCharacterId);

  const rawLinks: Array<{ source: string; target: string; kind: 'alliance' | 'conflict' }> = [];
  for (const ev of events) {
    for (const a of ev.actors) {
      add(a);
      weight.set(a, (weight.get(a) ?? 1) + 1);
    }
    if (ev.type === 'alliance' || ev.type === 'conflict') {
      for (let i = 0; i < ev.actors.length; i += 1) {
        for (let j = i + 1; j < ev.actors.length; j += 1) {
          if (ev.actors[i] === ev.actors[j]) continue;
          if (ev.type === 'alliance') {
            union(ev.actors[i], ev.actors[j]);
            rawLinks.push({ source: ev.actors[i], target: ev.actors[j], kind: 'alliance' });
          } else {
            rawLinks.push({ source: ev.actors[i], target: ev.actors[j], kind: 'conflict' });
          }
        }
      }
    }
  }
  for (const rel of relations ?? []) {
    add(rel.from);
    add(rel.to);
    if (rel.affinity > 0) {
      union(rel.from, rel.to);
      rawLinks.push({ source: rel.from, target: rel.to, kind: 'alliance' });
    } else if (rel.affinity < 0 || rel.fear > 0) {
      rawLinks.push({ source: rel.from, target: rel.to, kind: 'conflict' });
    }
  }

  const rootToFaction = new Map<string, number>();
  for (const id of ids) {
    const root = find(id);
    if (!rootToFaction.has(root)) rootToFaction.set(root, rootToFaction.size);
  }
  const factionCount = rootToFaction.size;
  const categories: GraphCategory[] = Array.from({ length: factionCount }, (_, i) => ({
    name: `势力 ${i + 1}`,
  }));

  const nodes: GraphNode[] = ids.map((id) => {
    const category = rootToFaction.get(find(id)) as number;
    const w = weight.get(id) ?? 1;
    const label = nameOf.get(id) as string;
    const isMine = mine.has(id);
    const color = FACTION_PALETTE[category % FACTION_PALETTE.length];
    return {
      id,
      label,
      size: Math.max(38, Math.min(38 + w * 3, 56)),
      color,
      kind: 'faction',
      category,
      mine: isMine,
      symbol: characterNodeSymbol(avatarOf.get(id), {
        name: label,
        fill: color,
        ring: isMine ? MINE_RING_COLOR : OTHER_RING_COLOR,
      }),
    };
  });

  const links: GraphLink[] = rawLinks.map((l) => ({
    source: l.source,
    target: l.target,
    weight: l.kind === 'alliance' ? 2 : 1.5,
    kind: l.kind,
    color: l.kind === 'alliance' ? ALLIANCE_COLOR : CONFLICT_COLOR,
    width: l.kind === 'alliance' ? 2 : 1.5,
    dashed: l.kind === 'conflict',
  }));

  return { nodes, links, categories };
}
