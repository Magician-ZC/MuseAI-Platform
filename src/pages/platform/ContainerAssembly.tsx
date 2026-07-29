// 自定义房装配（总规格 §10「自定义房闭环」）的**玩家端入口**：
// 打官方世界 → 结算得卡 → 合成 → **装进自提取的世界容器开房**。
//
// 前三步早就有界面（journey 的副本卡页 + 本页所在的提取发布向导），只有最后这一步没有：
// 服务端 `assembly::compose_container_skeleton`、`world_container_cards`（迁移 0033）、
// 三道校验闸全部就位，而 `src/` 全域对 `subplotCardRefs` / `seams` / `anchors` 的引用数是 **0**
// （2026-07-29 实测）。本文件补的就是那一格。
//
// ══════════════════════════════════════════════════════════════════════════════
// 🔴 这是**声明面**，不是骨架编辑器 —— 两条硬边界，都不是偷懒
// ══════════════════════════════════════════════════════════════════════════════
// ① **不提供 `seams`（跨卡缝合边）编辑。**
//    缝合边的卡内那一端形如 `卡id:地点id`，而玩家端**拿不到卡的蓝图内容**——
//    `GET /me/subplot-cards` 只下发 `source.templateId` + `source.templateVersion`，不下发 skeleton；
//    全仓唯一暴露 `skeletonJson` 的读接口是 operator 权限的 `GET /api/admin/world-templates`。
//    给玩家一个只能盲填的输入框，等于造一个必然写错的入口。
//
//    不声明缝合边是**合法且完整**的路径，不是退化：服务端 compose 步骤 5 在合并后地图仍不连通时，
//    会自动生成枢纽地点 `loc-nexus`（默认名「交汇之地」，可由下方「枢纽地点名」改）把各连通分量接起来。
//    跨卡显式缝合属运营能力，仍走 admin 建模板路径。
//
// ② **不做穷尽的提交前校验。**
//    服务端三道闸里只有第一道的**一部分**是「用手上这份 draft 就能算」的：
//    建模板期 `validate_container_refs`（部分可算）、装配期 `load_container_cards`（卡归属 / 状态 /
//    蓝图版本 / 蓝图审核态，全要查库）、装配期 `compose_container_skeleton`（要解引用卡蓝图）。
//    本面因此只做一件事：**不把明显选不了的东西摆出来**，其余交给服务端那些写得很清楚的中文 400。
//
//    🔴 绝不在这里重抄服务端的 `SKELETON_KEY_SETS` / `collect_id_like`——那是同一判定的第二份拷贝，
//    而它的漂移方向恰恰是最坏的那个：**前端说没问题、后端 400**。
//    下面 `containerBlockers` 只覆盖**地点**这一类 id（服务端的扫描面是 11 处采集点），
//    因为那正是锚点选择器直接摆在用户眼前的那一类；覆盖不到的由服务端兜，且这个方向是安全的
//    （前端拦的是服务端必拦集合的子集，永远不会出现「前端放行、后端也放行」的假绿）。
//
// ══════════════════════════════════════════════════════════════════════════════
// 可见性：为什么没有「该功能未开放」的提示
// ══════════════════════════════════════════════════════════════════════════════
// 副本卡由结算铸出，而铸卡受 `MUSE_SUBPLOT_CARDS` 控制 ⇒ 开关关着的环境里玩家**手上没有卡**
// ⇒ 整段自然不渲染。这与平台「关闭即 404 而非 403、不向外泄露平台有这个未开放功能」的口径一致
// （见 server `livestage` / `annotations` / `ifline` 各自模块头的同型注释）。
// 因此读卡失败时同样**静默隐藏**，刻意不区分「功能没开」与「你没有卡」——那正是要保护的东西。
//
// ⚠️ 另一半如实说：`MUSE_CONTAINER_ASSEMBLY` 是**另一个**开关，且玩家端没有任何接口能查它。
// 「有卡但装配开关没开」时，本面照常显示，提交才被服务端前门拒绝（400，文案里写明是运营开关控制）。
// 这不是疏漏——补一个玩家可见的开关查询端点会一次性暴露全部未发布功能的存在性，与上面那条红线冲突。
import React from 'react';
import { Table, Checkbox, InputNumber, Select, Input, Alert, Tag, Space, Typography, Tooltip } from 'antd';
import type { WorldSkeletonDraft } from '../../stores/useWorldExtractionStore';
import { cloudFetch, CloudError } from '../../utils/cloudApi';

const { Text, Paragraph } = Typography;

/** 服务端为容器枢纽保留的地点 id（`assembly::NEXUS_LOCATION_ID`）。本体不得占用。 */
export const NEXUS_RESERVED_ID = 'loc-nexus';
/** 命名空间分隔符（`assembly::NS_SEP`）。装卡后卡内 id 一律重写为 `卡id:原id`。 */
export const NS_SEPARATOR = ':';

/** `GET /api/me/subplot-cards` 的单张卡投影（对齐 `subplot::project_card`，只取本面用得到的字段）。 */
export interface SubplotCardSummary {
  id: string;
  starRating: number;
  label: string;
  originKind: string;
  status: string;
  source?: { worldId?: string | null; templateId?: string | null; templateVersion?: number | null };
}

/** 玩家在本面上做出的全部声明。空 `cards` = 不装卡 = 普通世界。 */
export interface ContainerSelection {
  cards: { cardId: string; cardVersion?: number; weight: number }[];
  /** 本体对外缝合口白名单（地点 id）。空 = 不声明，由服务端在需要枢纽时自行挑代表。 */
  anchors: string[];
  /** 枢纽地点名。空 = 用服务端默认「交汇之地」。 */
  nexusName: string;
}

export const EMPTY_SELECTION: ContainerSelection = { cards: [], anchors: [], nexusName: '' };

interface DraftLocationRaw {
  id?: unknown;
  isSecretRealm?: unknown;
}

/** 从只读的合成产物里取出地点清单（draft 的数组元素类型是 `unknown`，此处逐项收窄）。 */
export function draftLocations(draft: WorldSkeletonDraft): { id: string; isSecretRealm: boolean }[] {
  const raw = Array.isArray(draft.locations) ? draft.locations : [];
  const out: { id: string; isSecretRealm: boolean }[] = [];
  for (const l of raw) {
    if (!l || typeof l !== 'object') continue;
    const id = (l as DraftLocationRaw).id;
    if (typeof id !== 'string' || !id.trim()) continue;
    out.push({ id: id.trim(), isSecretRealm: (l as DraftLocationRaw).isSecretRealm === true });
  }
  return out;
}

/**
 * 这份合成产物**根本不能**当装卡容器的理由。非空即整段禁用。
 *
 * 只覆盖地点这一类 id，理由见文件头 ②：这里要的是「别让用户白填一遍」，不是「替服务端把关」。
 */
export function containerBlockers(draft: WorldSkeletonDraft): string[] {
  const locs = draftLocations(draft);
  const blockers: string[] = [];
  if (locs.some((l) => l.id === NEXUS_RESERVED_ID)) {
    blockers.push(
      `本世界有地点占用了保留 id「${NEXUS_RESERVED_ID}」——装卡时服务端要用它生成枢纽地点。请回到「清单确认」改掉这个地点 id 后重新合成。`,
    );
  }
  const colliding = locs.filter((l) => l.id.includes(NS_SEPARATOR)).map((l) => l.id);
  if (colliding.length > 0) {
    blockers.push(
      `本世界的地点 id 含保留分隔符「${NS_SEPARATOR}」：${colliding.join('、')}。装卡后「${NS_SEPARATOR}」是命名空间前缀专用（卡id${NS_SEPARATOR}原id），本体 id 不得占用。`,
    );
  }
  return blockers;
}

/**
 * 可以拿来当对外缝合口的本体地点。
 *
 * 🔴 **秘境永不入选**：秘境的 gate 语义必须完整保留在它自己那一片，接一条缝合边进去等于绕开 gate。
 * 服务端在建模板期（`anchors 非法：秘境…`）与 compose 步骤 4 各拦一次，这里做的是不把它摆出来。
 * 含命名空间分隔符的 id 也不列——那种骨架整段都装不了卡，见 `containerBlockers`。
 */
export function anchorCandidates(draft: WorldSkeletonDraft): { id: string; isSecretRealm: boolean }[] {
  return draftLocations(draft).filter((l) => !l.isSecretRealm && !l.id.includes(NS_SEPARATOR));
}

/** 这张卡能不能装（服务端 `load_container_cards` 里唯一一条前端能提前算的判据）。 */
export function cardUsable(c: SubplotCardSummary): boolean {
  return (
    c.status === 'owned' &&
    typeof c.source?.templateId === 'string' &&
    !!c.source.templateId &&
    typeof c.source?.templateVersion === 'number'
  );
}

/**
 * 把玩家的声明合进待提交的骨架。
 *
 * 🔴 **没装卡时返回 draft 本身（引用相等）**，不是一份浅拷贝：
 * 「默认路径逐字节不变」在本仓是反复出现的约束（服务端那侧同样如此——
 * `Skeleton.container.is_none()` 是容器逻辑唯一的开关点）。返回同一个引用让这条约束
 * 在前端也变成**类型层面**成立的事实，而不是「拷贝恰好没改任何键」这种要靠人读的保证。
 */
export function buildContainerSkeleton(
  draft: WorldSkeletonDraft,
  selection: ContainerSelection,
): WorldSkeletonDraft {
  const cards = selection.cards.filter((c) => c.cardId.trim().length > 0);
  if (cards.length === 0) return draft;

  const out: WorldSkeletonDraft = {
    ...draft,
    subplotCardRefs: cards.map((c) => ({
      cardId: c.cardId,
      // 版本钉住：服务端 `load_container_cards` 会拿它与卡实际的 source_template_version 比对，
      // 不一致直接 400（「卡发新版不自动生效，请发容器新版本」）。拿不到版本号的卡进不了候选表。
      ...(typeof c.cardVersion === 'number' ? { cardVersion: c.cardVersion } : {}),
      weight: c.weight,
    })),
  };
  const anchors = selection.anchors.map((a) => a.trim()).filter((a) => a.length > 0);
  if (anchors.length > 0) out.anchors = anchors;
  const nexusName = selection.nexusName.trim();
  if (nexusName.length > 0) out.nexus = { name: nexusName };
  return out;
}

interface Props {
  draft: WorldSkeletonDraft;
  value: ContainerSelection;
  onChange: (next: ContainerSelection) => void;
}

const ContainerAssemblyPanel: React.FC<Props> = ({ draft, value, onChange }) => {
  const [cards, setCards] = React.useState<SubplotCardSummary[] | null>(null);
  // 读卡失败但**不是** 404 时的提示。404 = 功能未开放/无此路由，静默隐藏（见文件头）。
  const [loadWarning, setLoadWarning] = React.useState<string | null>(null);

  React.useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await cloudFetch<{ cards?: SubplotCardSummary[] }>('/api/me/subplot-cards?status=owned');
        if (!cancelled) setCards(res.cards ?? []);
      } catch (e) {
        if (cancelled) return;
        setCards([]);
        if (e instanceof CloudError && e.status !== 404) {
          setLoadWarning('副本卡读取失败，本次发布将不装卡。这不影响世界本身的发布。');
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const usable = React.useMemo(() => (cards ?? []).filter(cardUsable), [cards]);
  const blockers = React.useMemo(() => containerBlockers(draft), [draft]);
  const anchors = React.useMemo(() => anchorCandidates(draft), [draft]);

  // 一张卡都没有 ⇒ 整段不存在（含「功能没开」的情形，两者刻意不可区分）。
  if (cards === null || usable.length === 0) {
    return loadWarning ? <Alert type="warning" showIcon message={loadWarning} /> : null;
  }

  const selectedIds = new Set(value.cards.map((c) => c.cardId));

  const toggle = (card: SubplotCardSummary, on: boolean) => {
    if (on) {
      if (selectedIds.has(card.id)) return;
      onChange({
        ...value,
        cards: [
          ...value.cards,
          {
            cardId: card.id,
            cardVersion: card.source?.templateVersion ?? undefined,
            weight: 1,
          },
        ],
      });
    } else {
      onChange({ ...value, cards: value.cards.filter((c) => c.cardId !== card.id) });
    }
  };

  const setWeight = (cardId: string, weight: number) => {
    onChange({
      ...value,
      cards: value.cards.map((c) => (c.cardId === cardId ? { ...c, weight } : c)),
    });
  };

  const zeroWeighted = value.cards.filter((c) => c.weight === 0).map((c) => c.cardId);

  const columns = [
    {
      title: '装入',
      dataIndex: 'id',
      width: 64,
      render: (_: unknown, r: SubplotCardSummary) => (
        <Checkbox
          aria-label={`装入副本卡 ${r.label}`}
          checked={selectedIds.has(r.id)}
          onChange={(e) => toggle(r, e.target.checked)}
        />
      ),
    },
    {
      title: '副本卡',
      dataIndex: 'label',
      render: (v: string, r: SubplotCardSummary) => (
        <Space size={6}>
          <Text>{v}</Text>
          <Tag color="gold">{r.starRating}★</Tag>
          {r.originKind === 'synthesis' && <Tag>合成</Tag>}
        </Space>
      ),
    },
    {
      title: '内容蓝图版本',
      dataIndex: 'source',
      width: 130,
      render: (_: unknown, r: SubplotCardSummary) => (
        <Tooltip title="装配时按此版本钉住；卡的来源模板发了新版不会自动生效，需要重新发布容器新版本。">
          <Text type="secondary">v{r.source?.templateVersion}</Text>
        </Tooltip>
      ),
    },
    {
      title: '内容权重',
      dataIndex: 'weight',
      width: 140,
      render: (_: unknown, r: SubplotCardSummary) => (
        <InputNumber
          aria-label={`副本卡 ${r.label} 的内容权重`}
          min={0}
          step={0.1}
          disabled={!selectedIds.has(r.id)}
          value={value.cards.find((c) => c.cardId === r.id)?.weight ?? 1}
          onChange={(v) => setWeight(r.id, typeof v === 'number' && Number.isFinite(v) ? Math.max(0, v) : 1)}
          style={{ width: 110 }}
        />
      ),
    },
  ];

  return (
    <Space direction="vertical" size={12} style={{ width: '100%' }}>
      <div>
        <Text strong>装入副本卡（自定义房）</Text>
        <Paragraph type="secondary" style={{ fontSize: 12, marginTop: 4, marginBottom: 0 }}>
          副本卡是永久蓝图：装进房里不会被消耗，一张卡可以同时装在多个房里。
          不装卡时本世界就是普通世界，提交内容与不打开这一段时完全一致。
        </Paragraph>
      </div>

      {loadWarning && <Alert type="warning" showIcon message={loadWarning} />}

      {blockers.length > 0 ? (
        <Alert
          type="warning"
          showIcon
          message="这份世界超集暂时不能装卡"
          description={
            <ul style={{ margin: 0, paddingLeft: 18 }}>
              {blockers.map((b) => (
                <li key={b}>{b}</li>
              ))}
            </ul>
          }
        />
      ) : (
        <>
          <Table<SubplotCardSummary>
            size="small"
            rowKey="id"
            dataSource={usable}
            columns={columns}
            pagination={false}
          />

          {zeroWeighted.length > 0 && (
            <Alert
              type="warning"
              showIcon
              message={`权重为 0 的卡：${zeroWeighted.join('、')}`}
              description="权重 0 是合法值，装配会成功，但这张卡的剧情线永远抽不中——等于装了个空壳。除非你就是要占个位，否则把它调回 1。"
            />
          )}

          {value.cards.length > 0 && (
            <>
              <div>
                <Text strong>对外缝合口（可选）</Text>
                <div style={{ marginTop: 6 }}>
                  <Select
                    mode="multiple"
                    allowClear
                    aria-label="对外缝合口"
                    style={{ width: '100%', maxWidth: 520 }}
                    placeholder="不选=由服务端自行挑选各片区代表"
                    value={value.anchors}
                    onChange={(v: string[]) => onChange({ ...value, anchors: v })}
                    options={anchors.map((l) => ({ value: l.id, label: l.id }))}
                  />
                </div>
                <Text type="secondary" style={{ fontSize: 12 }}>
                  只列非秘境地点——秘境的 gate 语义必须完整留在它自己那一片，不可作缝合口。
                </Text>
              </div>

              <div>
                <Text strong>枢纽地点名（可选）</Text>
                <div style={{ marginTop: 6 }}>
                  <Input
                    aria-label="枢纽地点名"
                    style={{ width: 260 }}
                    placeholder="交汇之地"
                    maxLength={40}
                    value={value.nexusName}
                    onChange={(e) => onChange({ ...value, nexusName: e.target.value })}
                  />
                </div>
              </div>

              <Alert
                type="info"
                showIcon
                message={`本次将装入 ${value.cards.length} 张副本卡`}
                description="卡与容器的地图若接不到一起，服务端会自动生成一个枢纽地点把各片区连起来。跨卡的显式缝合边不在本页声明——那需要卡内部的地点 id，而玩家端按设计拿不到卡的蓝图内容。"
              />
            </>
          )}
        </>
      )}
    </Space>
  );
};

export default ContainerAssemblyPanel;
