// 人工校准面（总规格 §79/§83 内容生产流水线第一环：人工校准 → 仿真试跑 → 世界质量回归）。
//
// 本页做**三维**（§79 里「人工校准」明列的那三项）：阶段切分（saga_id / stage_no）、
// 身份池（skeleton.identityPool → 实例分配）、境界档（skeleton.realmTier → 实例钉住，总规格 §6）。
//
// 三维不是同构的，界面上也不该长得一样：
//   · 阶段切分 = 坐标 → 看连续性（缺号 / 重号）；
//   · 身份池   = 各不相同的开局站位 → 看分布（填充率 / 基尼）；
//   · 境界档   = 全员统一的一件戏服 → 看「有没有 / 各阶是不是在换 / 实例钉住没有」。
//     **境界档没有「分布」可看**——它零抽样、无配额，谁要在这一维画分布图就是没读 §6。
//
// 🔴 两条必须在界面上说清楚、不得靠运营自行推断的事：
//   ① **只可视化，不可编辑**：后端六个端点全只读，恒下发 `editable:false` + `editPath`。
//      页面把这两个字段渲染出来，而不是自己写死一句「只读」——后端哪天开了写入面，界面自动跟上。
//   ② **每一维的真实效力**：身份池的分配层与叙事感知层都已落地（但永不进数值层）；
//      境界档的**叙事感知层是缺的**——runtime 不读 realmTier，这件戏服目前没人穿。
//      这些层状态一律由后端 `effect` 段下发，本页原样渲染（`EffectPanel`），
//      **不得只画一张「已配置」的绿标**。
//
// 数据诚实纪律（设计文档 §9.1）：只渲染接口真实返回的字段，null 一律显示 `—`，
// 比率为 null 时不得当 0% 渲染（formatPercent 已处理）。
import { useCallback, useEffect, useState } from 'react';
import { Alert, Button, Drawer, Empty, Space, Table, Tabs, Tag, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { useLocation } from 'react-router-dom';
import { adminFetch } from '../api';
import { ErrorAlert, formatNumber, formatPercent, formatTime, friendlyError } from '../components/shared';
import './Calibration.css';

// ---------------- 类型（对齐 server/src/admin_api/calibration.rs 的响应） ----------------

interface SagaRow {
  sagaId: string;
  templateCount: number;
  stageCount: number;
  minStageNo: number | null;
  maxStageNo: number | null;
  missingStageNos: number[];
  missingStageNosTruncated: boolean;
  duplicateStageNos: number[];
  unnumberedTemplateCount: number;
  contiguous: boolean;
  moderationCounts: Record<string, number>;
  roomTypes: string[];
  starMin: number | null;
  starMax: number | null;
  worldCount: number;
  liveWorldCount: number;
  firstCreatedAt: number | null;
  lastCreatedAt: number | null;
}

interface SagaListRes {
  sagas: SagaRow[];
  scannedTemplates: number;
  truncated: boolean;
  standaloneTemplateCount: number;
  editable: boolean;
  editPath: string;
  notes: string[];
}

interface StageShape {
  parsed: boolean;
  skeletonBytes: number;
  mainlineNodes?: number;
  endingPool?: number;
  hiddenContentPool?: number;
  worldCharacters?: number;
  locations?: number;
  storylines?: number;
  subplotCardRefs?: number;
  seams?: number;
  identityPoolSize?: number;
  identityQuotaTotal?: number;
  identityLeadCount?: number;
  hasPayoutTable?: boolean;
  /** 境界档是「有 / 无」，不是规模——§6 全员统一，没有「几个境界」这回事。 */
  hasRealmTier?: boolean;
  realmTierLabel?: string | null;
  hasEndgame?: boolean;
  isSuperset?: boolean;
}

interface StageTemplate {
  id: string;
  title: string;
  roomType: string;
  moderation: string;
  starRating: number;
  starSource: string;
  official: boolean;
  version: number;
  createdAt: number;
  worldCount: number;
  liveWorldCount: number;
  shape: StageShape;
}

interface SagaDetailRes {
  sagaId: string;
  templateCount: number;
  stageCount: number;
  stages: { stageNo: number; templates: StageTemplate[] }[];
  continuity: {
    minStageNo: number | null;
    maxStageNo: number | null;
    missingStageNos: number[];
    missingStageNosTruncated: boolean;
    duplicateStageNos: number[];
    unnumberedTemplateCount: number;
    contiguous: boolean;
  };
  truncated: boolean;
  editable: boolean;
  editPath: string;
  notes: string[];
}

interface IdentityPoolRow {
  templateId: string;
  title: string;
  roomType: string;
  moderation: string;
  sagaId: string;
  stageNo: number;
  poolSize: number;
  quotaTotal: number;
  leadCount: number;
  worldCount: number;
  liveWorldCount: number;
}

interface IdentityDirectoryRes {
  templates: IdentityPoolRow[];
  scannedTemplates: number;
  truncated: boolean;
  editable: boolean;
  editPath: string;
  notes: string[];
}

interface IdentityStat {
  identityId: string;
  label: string;
  quota: number;
  isLead: boolean;
  themes: string[];
  hookAffinity: string[];
  assignedCount: number;
  worldCount: number;
  quotaCapacity: number;
  /** 0..1 小数；无有效分母 → null（显示 —，不得当 0% 读）。 */
  fillRatio: number | null;
}

/**
 * 效力自述段：后端下发「哪一层已经生效、哪一层永不生效、哪一层根本没有」。
 * 层的**数量与名字按维度不同**（身份池四层、境界档五层），故这里用索引签名，
 * 由各维度自己给 `layers` 顺序表 —— 前端不猜、不补、不合并。
 */
interface EffectScope {
  summary: string;
  warning: string;
  [layer: string]: string;
}

interface IdentityDetailRes {
  templateId: string;
  title: string;
  roomType: string;
  version: number;
  moderation: string;
  sagaId: string;
  stageNo: number;
  declared: boolean;
  poolSize: number;
  quotaTotal: number;
  leadCount: number;
  poolIssues: { duplicateIds: string[]; blankIdCount: number; nonPositiveQuotaIds: string[] };
  distribution: {
    worldsScanned: number;
    worldsTruncated: boolean;
    worldsAssembled: number;
    worldsWithAssignments: number;
    assignmentTotal: number;
    activeMemberTotal: number;
    activeMembersWithoutIdentity: number;
    byIdentity: IdentityStat[];
    unknownIdentityIds: { identityId: string; assignedCount: number }[];
    neverAssignedIdentityIds: string[];
    gini: number | null;
    worlds: {
      id: string;
      title: string;
      status: string;
      assembled: boolean;
      assignmentCount: number;
      activeMemberCount: number;
      activeMembersWithoutIdentity: number;
    }[];
  };
  effect: EffectScope;
  editable: boolean;
  editPath: string;
  notes: string[];
}

// ---- 维度三：境界档（总规格 §6【拍板 3】戏服原则） ----

interface RealmRow {
  templateId: string;
  title: string;
  roomType: string;
  moderation: string;
  sagaId: string;
  stageNo: number;
  tierId: string;
  label: string;
  cosmology: string;
  genre: string;
  conflictIntensity: string;
  flavorNoteCount: number;
  /** 填了官方枚举外的自由文本的字段名（建模板端点会拦，出现在此 = 历史数据 / 直写库）。 */
  invalidEnumFields: string[];
  worldCount: number;
  liveWorldCount: number;
}

interface RealmDirectoryRes {
  templates: RealmRow[];
  scannedTemplates: number;
  truncated: boolean;
  /** 归属某个 Saga 却没有戏服的模板数 = 真正的校准缺口（§6「阶段天然携带境界档」）。 */
  undeclaredInSagaCount: number;
  /** 独立模板没戏服，只作对照，不是缺口。 */
  undeclaredStandaloneCount: number;
  effect: EffectScope;
  editable: boolean;
  editPath: string;
  notes: string[];
}

interface RealmStage {
  templateId: string;
  stageNo: number;
  title: string;
  declared: boolean;
  tierId: string | null;
  label: string | null;
  cosmology: string | null;
  isSelf: boolean;
}

interface RealmDetailRes {
  templateId: string;
  title: string;
  roomType: string;
  version: number;
  moderation: string;
  sagaId: string;
  stageNo: number;
  declared: boolean;
  /** 未声明 → null（不是空对象）。 */
  declaration: {
    tierId: string;
    label: string;
    cosmology: string;
    genre: string;
    conflictIntensity: string;
    briefing: string;
    flavorNotes: string[];
    invalidEnumFields: string[];
    blankTierId: boolean;
    /** §6 历史题材严审提示。🔴 只是提示，未接进任何审核链路。 */
    stricterModerationHint: boolean;
  } | null;
  sagaStages: {
    stages: RealmStage[];
    stagesWithoutRealmTier: number[];
    reusedTierIds: { tierId: string; stageNos: number[] }[];
    distinctCosmologies: string[];
    distinctGenres: string[];
  };
  pinning: {
    worldsScanned: number;
    worldsTruncated: boolean;
    worldsAssembled: number;
    worldsWithRealmTier: number;
    staleTierIds: { tierId: string; worldCount: number }[];
    worlds: {
      id: string;
      title: string;
      status: string;
      assembled: boolean;
      pinnedTierId: string | null;
      pinnedLabel: string | null;
      /** null = 模板未声明境界档，「是否一致」这个问题不成立——不得当 false 渲染。 */
      matchesTemplate: boolean | null;
    }[];
  };
  effect: EffectScope;
  editable: boolean;
  editPath: string;
  notes: string[];
}

// ---------------- 展示映射 ----------------

const ROOM_TYPE_TEXT: Record<string, string> = { idle: '放置世界', chapter: '章节房', arena: '赛事房' };
const MOD_TEXT: Record<string, { color: string; text: string }> = {
  pending: { color: 'blue', text: '待审核' },
  approved: { color: 'green', text: '已通过' },
  rejected: { color: 'red', text: '已驳回' },
};
const WORLD_STATUS_TEXT: Record<string, string> = {
  open: '开放',
  running: '运行中',
  paused: '已暂停',
  ended: '已结束',
};

/**
 * 境界档三项枚举的中文映射（与 server/src/assembly/mod.rs 的 KNOWN_COSMOLOGIES /
 * KNOWN_GENRES / KNOWN_CONFLICT_INTENSITIES 一一对应）。
 * 🔴 后端加了新取值而这里没跟上时，一律**原样回显英文** —— 绝不 fallback 成"其他"把新值吞掉。
 */
const COSMOLOGY_TEXT: Record<string, string> = {
  magic: '魔法',
  tech: '科技',
  cultivation: '修真 / 斗气',
  mundane: '凡俗',
  psychic: '异能',
  myth: '神话',
};
const GENRE_TEXT: Record<string, string> = {
  xuanhuan: '玄幻',
  xianxia: '仙侠',
  wuxia: '武侠',
  urban: '都市',
  romance: '言情',
  history: '历史',
  scifi: '科幻',
  mystery: '悬疑',
  other: '其他',
};
/** §6 原文三档：文斗 / 武斗 / 生死。 */
const INTENSITY_TEXT: Record<string, { color: string; text: string }> = {
  civil: { color: 'blue', text: '文斗' },
  martial: { color: 'orange', text: '武斗' },
  lethal: { color: 'red', text: '生死' },
};

/** 枚举值渲染：空 → `—`（留空是合法的，不是缺数据）；未知取值 → 原样回显并标红。 */
function EnumText({ value, map, invalid }: { value: string; map: Record<string, string>; invalid?: boolean }) {
  if (!value) return <Typography.Text type="secondary">—</Typography.Text>;
  if (invalid) return <Tag color="red">{value}（非官方取值）</Tag>;
  return <>{map[value] ?? value}</>;
}

/**
 * 状态语言七档 → 中文标签与语气（VALIDATION §0.3）。
 * 🔴 后端下发什么就显示什么：这里只做翻译，**不做推断、不做美化**。
 * 未知取值原样回显（后端改口径时界面立刻露出，而不是被 fallback 悄悄吞掉）。
 */
const LAYER_STATE: Record<string, { text: string; tone: 'ok' | 'never' | 'missing'; note: string }> = {
  Implemented: {
    text: '已实现（Implemented）',
    tone: 'ok',
    note: '代码已落地并有测试覆盖；这不等于「已验证值得上线」。',
  },
  NeverByDesign: {
    text: '设计上永不生效',
    tone: 'never',
    note: '平权红线（VALIDATION §0.1）：不改判定、不改发奖、不开权限、不调难度、不改准入。',
  },
  Missing: {
    text: '缺失',
    tone: 'missing',
    note: '没有任何指标能证明这一维调得对不对。',
  },
};

/** 身份池四层（§5）。 */
const IDENTITY_LAYERS: { key: string; label: string }[] = [
  { key: 'assignmentLayer', label: '分配层' },
  { key: 'narrativeLayer', label: '叙事感知层' },
  { key: 'numericLayer', label: '数值层' },
  { key: 'calibrationLoop', label: '校准闭环' },
];

/** 境界档五层（§6）。比身份池多一层，且**叙事感知层是缺的**——这正是要让运营看见的事。 */
const REALM_LAYERS: { key: string; label: string }[] = [
  { key: 'declarationLayer', label: '声明层' },
  { key: 'pinningLayer', label: '钉住层' },
  { key: 'narrativeLayer', label: '叙事感知层' },
  { key: 'numericLayer', label: '数值层' },
  { key: 'calibrationLoop', label: '校准闭环' },
];

function ModTag({ value }: { value: string }) {
  const t = MOD_TEXT[value] ?? { color: 'default', text: value };
  return <Tag color={t.color}>{t.text}</Tag>;
}

/** 只读徽标 + 唯一写入路径。文案取自接口的 editPath，后端开了写入面时界面自动跟上。 */
function ReadOnlyBanner({ editable, editPath }: { editable?: boolean; editPath?: string }) {
  if (editable === undefined) return null;
  if (editable) {
    // 目前后端恒为 false；真开了写入面时不该继续显示「只读」，故这里如实反映而不是写死。
    return <Alert type="info" showIcon style={{ marginBottom: 16 }} title="本页支持编辑。" />;
  }
  return (
    <Alert
      type="warning"
      showIcon
      style={{ marginBottom: 16 }}
      title="本页只可视化，不可编辑"
      description={editPath ?? '校准参数的写入路径不在本页。'}
    />
  );
}

/**
 * 效力自述面板：这一维「哪一层已经生效、哪一层永不生效、哪一层根本没有」。
 * 🔴 这一段不是装饰——没有它，运营会把下面的数据误读成「调了就会生效」的证据。
 * 标题与层列表由调用方给（各维度的层不同名、也不同数），本组件只负责如实渲染后端下发的状态。
 */
function EffectPanel({
  title,
  effect,
  layers,
}: {
  title: string;
  effect: EffectScope;
  layers: { key: string; label: string }[];
}) {
  return (
    <div className="calibration__effect">
      <h5>{title}</h5>
      <p>{effect.summary}</p>
      <dl className="calibration__layers">
        {layers.map(({ key, label }) => {
          const raw = String(effect[key] ?? '');
          const s = LAYER_STATE[raw];
          return (
            <div className="calibration__layer" key={key}>
              <dt>{label}</dt>
              <dd className={s ? `is-${s.tone}` : undefined}>
                {s?.text ?? raw ?? '—'}
                {s && <small>{s.note}</small>}
              </dd>
            </div>
          );
        })}
      </dl>
      <p className="is-warning" style={{ marginTop: 10, marginBottom: 0 }}>
        {effect.warning}
      </p>
    </div>
  );
}

/** 阶段号带：把「切成了几段、哪一段缺、哪一段重号」画成一条可扫视的带子。 */
function StageStrip({
  maxStageNo,
  missing,
  duplicates,
}: {
  maxStageNo: number | null;
  missing: number[];
  duplicates: number[];
}) {
  if (!maxStageNo) return <Typography.Text type="secondary">该系列没有任何已编号的阶段</Typography.Text>;
  const missingSet = new Set(missing);
  const dupSet = new Set(duplicates);
  const chips = [];
  for (let n = 1; n <= maxStageNo; n += 1) {
    const cls = missingSet.has(n) ? 'is-missing' : dupSet.has(n) ? 'is-duplicate' : '';
    // 状态必须同时有文字与颜色（设计文档 §5）：缺号写「缺」，重号写「重」。
    const suffix = missingSet.has(n) ? ' 缺' : dupSet.has(n) ? ' 重' : '';
    chips.push(
      <span className={`calibration__stage-chip ${cls}`} key={n} title={`阶段 ${n}${suffix}`}>
        {n}
        {suffix}
      </span>,
    );
  }
  return <div className="calibration__stage-strip">{chips}</div>;
}

/** 连续性结论标签（文字 + 颜色，不用彩色圆点）。 */
function ContinuityTags({ row }: { row: Pick<SagaRow, 'contiguous' | 'missingStageNos' | 'duplicateStageNos' | 'unnumberedTemplateCount'> }) {
  if (row.contiguous) return <Tag color="green">阶段齐整</Tag>;
  return (
    <Space size={4} wrap>
      {row.missingStageNos.length > 0 && <Tag color="red">缺 {row.missingStageNos.length} 阶</Tag>}
      {row.duplicateStageNos.length > 0 && <Tag color="orange">重号 {row.duplicateStageNos.length}</Tag>}
      {row.unnumberedTemplateCount > 0 && <Tag color="orange">未编号 {row.unnumberedTemplateCount}</Tag>}
    </Space>
  );
}

function Notes({ notes }: { notes?: string[] }) {
  if (!notes?.length) return null;
  return (
    <ul className="calibration__notes">
      {notes.map((n) => (
        <li key={n}>{n}</li>
      ))}
    </ul>
  );
}

// ================= 维度一：阶段切分 =================

function StageSplitting({ deepLink }: { deepLink?: string }) {
  const [data, setData] = useState<SagaListRes | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [detail, setDetail] = useState<SagaDetailRes | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setData(await adminFetch<SagaListRes>('/admin/sagas'));
    } catch (e) {
      setError(friendlyError(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const openDetail = useCallback(async (sagaId: string) => {
    setDetailId(sagaId);
    setDetail(null);
    setDetailError(null);
    setDetailLoading(true);
    try {
      setDetail(await adminFetch<SagaDetailRes>(`/admin/sagas/${encodeURIComponent(sagaId)}`));
    } catch (e) {
      setDetailError(friendlyError(e));
    } finally {
      setDetailLoading(false);
    }
  }, []);

  // 深链 `?saga=xxx`：运营把「这个系列缺了两阶」的链接直接贴给内容同事，对方打开即是同一屏。
  useEffect(() => {
    if (deepLink) openDetail(deepLink);
  }, [deepLink, openDetail]);

  const columns: TableColumnsType<SagaRow> = [
    {
      // 固定最小宽：其余各列宽度合计已达 ~1140px，不给首列留宽会把表头压成竖排单字。
      title: '世界系列（sagaId）',
      dataIndex: 'sagaId',
      key: 'sagaId',
      width: 220,
      render: (v: string) => <Typography.Text code>{v}</Typography.Text>,
    },
    {
      title: '阶段',
      key: 'stages',
      width: 150,
      render: (_, r) => (
        <>
          {r.stageCount} 阶
          {r.minStageNo != null && r.maxStageNo != null && (
            <Typography.Text type="secondary">
              （{r.minStageNo}–{r.maxStageNo}）
            </Typography.Text>
          )}
        </>
      ),
    },
    { title: '模板数', dataIndex: 'templateCount', key: 'templateCount', width: 80 },
    { title: '连续性', key: 'continuity', width: 200, render: (_, r) => <ContinuityTags row={r} /> },
    {
      title: '审核态',
      key: 'moderation',
      width: 200,
      render: (_, r) => (
        <Space size={4} wrap>
          {Object.entries(r.moderationCounts).map(([k, v]) => (
            <Tag key={k} color={MOD_TEXT[k]?.color ?? 'default'}>
              {MOD_TEXT[k]?.text ?? k} {v}
            </Tag>
          ))}
        </Space>
      ),
    },
    {
      title: '星级跨度',
      key: 'star',
      width: 100,
      render: (_, r) =>
        r.starMin == null ? (
          '—'
        ) : (
          <span style={{ color: '#bb7623', fontWeight: 600 }}>
            {r.starMin === r.starMax ? `${r.starMin}★` : `${r.starMin}–${r.starMax}★`}
          </span>
        ),
    },
    {
      title: '世界（在跑/总）',
      key: 'worlds',
      width: 130,
      render: (_, r) => `${formatNumber(r.liveWorldCount)} / ${formatNumber(r.worldCount)}`,
    },
    { title: '最近录入', dataIndex: 'lastCreatedAt', key: 'lastCreatedAt', width: 170, render: formatTime },
    {
      title: '操作',
      key: 'op',
      width: 110,
      fixed: 'right',
      render: (_, r) => (
        <Button size="small" onClick={() => openDetail(r.sagaId)}>
          查看阶段
        </Button>
      ),
    },
  ];

  const problemCount = data?.sagas.filter((s) => !s.contiguous).length ?? null;

  return (
    <div>
      <ReadOnlyBanner editable={data?.editable} editPath={data?.editPath} />

      <dl className="calibration__stats">
        <div className="calibration__stat">
          <dt>世界系列数</dt>
          <dd>{data ? formatNumber(data.sagas.length) : '—'}</dd>
        </div>
        <div className={`calibration__stat${problemCount ? ' is-attention' : ''}`}>
          <dt>阶段坐标有问题的系列</dt>
          <dd>{problemCount == null ? '—' : formatNumber(problemCount)}</dd>
        </div>
        <div className="calibration__stat">
          <dt>已切分模板</dt>
          <dd>
            {data ? formatNumber(data.scannedTemplates) : '—'}
            {data?.truncated && <small>已截断</small>}
          </dd>
        </div>
        <div className="calibration__stat">
          <dt>未归入系列的模板</dt>
          <dd>
            {data ? formatNumber(data.standaloneTemplateCount) : '—'}
            <small>待切分</small>
          </dd>
        </div>
      </dl>

      <Space style={{ marginBottom: 12 }}>
        <Button onClick={load} loading={loading}>
          刷新
        </Button>
        {data?.truncated && (
          <Typography.Text type="warning">
            扫描已达上限，末尾一个可能被切断的系列已整组丢弃（半个系列的连续性诊断是错的）。
          </Typography.Text>
        )}
      </Space>

      {error && <ErrorAlert message={error} onRetry={load} />}

      <Table
        rowKey="sagaId"
        size="small"
        columns={columns}
        dataSource={data?.sagas ?? []}
        loading={loading}
        pagination={false}
        scroll={{ x: 1360 }}
        locale={{
          emptyText: (
            <Empty
              description={
                error
                  ? '未能取到数据'
                  : '还没有任何世界系列。阶段切分靠建模板时录入 sagaId + stageNo，本页只读不产生数据。'
              }
            />
          ),
        }}
      />

      <Notes notes={data?.notes} />

      <Drawer
        title={`世界系列阶段结构：${detailId ?? ''}`}
        width={860}
        open={!!detailId}
        onClose={() => setDetailId(null)}
        loading={detailLoading}
      >
        {detailError && <ErrorAlert message={detailError} onRetry={() => detailId && openDetail(detailId)} />}
        {detail && (
          <>
            <ReadOnlyBanner editable={detail.editable} editPath={detail.editPath} />
            <div className="calibration__section">阶段号分布</div>
            <StageStrip
              maxStageNo={detail.continuity.maxStageNo}
              missing={detail.continuity.missingStageNos}
              duplicates={detail.continuity.duplicateStageNos}
            />
            <p className="calibration__hint">
              共 {detail.stageCount} 阶 / {detail.templateCount} 个模板。
              缺号从 1 起算（「缺开篇」也会被报出来）；重号 = 同一阶段挂了多个模板。
              {detail.continuity.unnumberedTemplateCount > 0 &&
                ` 另有 ${detail.continuity.unnumberedTemplateCount} 个模板属于本系列却没有阶段号。`}
            </p>

            <div className="calibration__section">逐阶段形状（按剧情顺序，不是录入时间）</div>
            {detail.stages.map((stage) => (
              <div key={stage.stageNo} style={{ marginBottom: 18 }}>
                <Typography.Text strong>
                  阶段 {stage.stageNo}
                  {stage.templates.length > 1 && (
                    <Tag color="orange" style={{ marginLeft: 8 }}>
                      重号：{stage.templates.length} 个模板
                    </Tag>
                  )}
                </Typography.Text>
                <Table
                  style={{ marginTop: 8 }}
                  rowKey="id"
                  size="small"
                  pagination={false}
                  dataSource={stage.templates}
                  columns={[
                    { title: '模板', dataIndex: 'title', key: 'title' },
                    {
                      title: '房型',
                      dataIndex: 'roomType',
                      key: 'roomType',
                      width: 90,
                      render: (v: string) => ROOM_TYPE_TEXT[v] ?? v,
                    },
                    {
                      title: '审核态',
                      dataIndex: 'moderation',
                      key: 'moderation',
                      width: 90,
                      render: (v: string) => <ModTag value={v} />,
                    },
                    {
                      title: '星级',
                      key: 'star',
                      width: 70,
                      render: (_: unknown, r: StageTemplate) => (
                        <span style={{ color: '#bb7623', fontWeight: 600 }}>{r.starRating}★</span>
                      ),
                    },
                    {
                      title: '骨架形状',
                      key: 'shape',
                      render: (_: unknown, r: StageTemplate) =>
                        r.shape.parsed ? (
                          <Space size={4} wrap>
                            <Tag>主线 {r.shape.mainlineNodes}</Tag>
                            <Tag>结局 {r.shape.endingPool}</Tag>
                            <Tag>隐藏池 {r.shape.hiddenContentPool}</Tag>
                            <Tag>地点 {r.shape.locations}</Tag>
                            <Tag>NPC {r.shape.worldCharacters}</Tag>
                            {(r.shape.identityPoolSize ?? 0) > 0 && (
                              <Tag color="gold">
                                身份池 {r.shape.identityPoolSize}（配额 {r.shape.identityQuotaTotal}）
                              </Tag>
                            )}
                            {r.shape.hasRealmTier && (
                              <Tag color="purple">境界档 {r.shape.realmTierLabel}</Tag>
                            )}
                            {r.shape.hasPayoutTable && <Tag color="green">产出表</Tag>}
                            {r.shape.hasEndgame && <Tag color="green">终局策略</Tag>}
                          </Space>
                        ) : (
                          // 骨架不是合法 JSON → 明说，不拿 0 冒充（数据诚实纪律）。
                          <Tag color="red">骨架 JSON 无法解析（{formatNumber(r.shape.skeletonBytes)} 字节）</Tag>
                        ),
                    },
                    {
                      title: '世界（在跑/总）',
                      key: 'worlds',
                      width: 120,
                      render: (_: unknown, r: StageTemplate) =>
                        `${formatNumber(r.liveWorldCount)} / ${formatNumber(r.worldCount)}`,
                    },
                  ]}
                />
              </div>
            ))}
            <Notes notes={detail.notes} />
          </>
        )}
      </Drawer>
    </div>
  );
}

// ================= 维度二：身份池 =================

function IdentityPools({ deepLink }: { deepLink?: string }) {
  const [data, setData] = useState<IdentityDirectoryRes | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [detail, setDetail] = useState<IdentityDetailRes | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setData(await adminFetch<IdentityDirectoryRes>('/admin/identity-pools'));
    } catch (e) {
      setError(friendlyError(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const openDetail = useCallback(async (templateId: string) => {
    setDetailId(templateId);
    setDetail(null);
    setDetailError(null);
    setDetailLoading(true);
    try {
      setDetail(
        await adminFetch<IdentityDetailRes>(
          `/admin/world-templates/${encodeURIComponent(templateId)}/identity-pool`,
        ),
      );
    } catch (e) {
      setDetailError(friendlyError(e));
    } finally {
      setDetailLoading(false);
    }
  }, []);

  // 深链 `?pool=tpl_xxx`（见 StageSplitting 同处说明）。
  useEffect(() => {
    if (deepLink) openDetail(deepLink);
  }, [deepLink, openDetail]);

  const columns: TableColumnsType<IdentityPoolRow> = [
    { title: '模板', dataIndex: 'title', key: 'title' },
    {
      title: '系列 / 阶段',
      key: 'saga',
      width: 200,
      render: (_, r) =>
        r.sagaId ? (
          <>
            <Typography.Text code>{r.sagaId}</Typography.Text>
            <Typography.Text type="secondary"> · 第 {r.stageNo} 阶</Typography.Text>
          </>
        ) : (
          <Typography.Text type="secondary">独立模板</Typography.Text>
        ),
    },
    {
      title: '房型',
      dataIndex: 'roomType',
      key: 'roomType',
      width: 90,
      render: (v: string) => ROOM_TYPE_TEXT[v] ?? v,
    },
    {
      title: '审核态',
      dataIndex: 'moderation',
      key: 'moderation',
      width: 90,
      render: (v: string) => <ModTag value={v} />,
    },
    { title: '身份数', dataIndex: 'poolSize', key: 'poolSize', width: 80 },
    {
      title: '配额合计',
      key: 'quota',
      width: 110,
      render: (_, r) => (
        <>
          {formatNumber(r.quotaTotal)}
          {r.leadCount > 0 && <Tag color="gold" style={{ marginLeft: 6 }}>戏眼 {r.leadCount}</Tag>}
        </>
      ),
    },
    {
      title: '世界（在跑/总）',
      key: 'worlds',
      width: 130,
      render: (_, r) => `${formatNumber(r.liveWorldCount)} / ${formatNumber(r.worldCount)}`,
    },
    {
      title: '操作',
      key: 'op',
      width: 110,
      fixed: 'right',
      render: (_, r) => (
        <Button size="small" onClick={() => openDetail(r.templateId)}>
          查看分布
        </Button>
      ),
    },
  ];

  const d = detail?.distribution;

  return (
    <div>
      <ReadOnlyBanner editable={data?.editable} editPath={data?.editPath} />

      <Table
        rowKey="templateId"
        size="small"
        columns={columns}
        dataSource={data?.templates ?? []}
        loading={loading}
        pagination={false}
        scroll={{ x: 1080 }}
        title={() => (
          <Space>
            <Button size="small" onClick={load} loading={loading}>
              刷新
            </Button>
            <Typography.Text type="secondary">
              只列出声明了 identityPool 的模板（已扫描 {data ? formatNumber(data.scannedTemplates) : '—'} 个模板
              {data?.truncated ? '，已达扫描上限' : ''}）。
            </Typography.Text>
          </Space>
        )}
        locale={{
          emptyText: (
            <Empty
              description={
                error
                  ? '未能取到数据'
                  : '没有任何模板声明了身份池。未声明 = 该模板在身份这一维上零影响，属正常状态。'
              }
            />
          ),
        }}
      />

      {error && <ErrorAlert message={error} onRetry={load} />}
      <Notes notes={data?.notes} />

      <Drawer
        title={`身份池分配分布：${detail?.title ?? detailId ?? ''}`}
        width={980}
        open={!!detailId}
        onClose={() => setDetailId(null)}
        loading={detailLoading}
      >
        {detailError && <ErrorAlert message={detailError} onRetry={() => detailId && openDetail(detailId)} />}
        {detail && d && (
          <>
            {/* 🔴 效力自述必须在分布图**之前**：先说清楚它有什么用，再给数。 */}
            <EffectPanel title="身份池现在到底有什么用" effect={detail.effect} layers={IDENTITY_LAYERS} />
            <ReadOnlyBanner editable={detail.editable} editPath={detail.editPath} />

            {!detail.declared && (
              <Alert
                type="info"
                showIcon
                style={{ marginBottom: 16 }}
                title="该模板未声明 identityPool"
                description="未声明 = 装配时不做身份分配，叙事层完全退化为只显示角色名。这是正常状态，不是缺数据。"
              />
            )}

            {(detail.poolIssues.duplicateIds.length > 0 ||
              detail.poolIssues.blankIdCount > 0 ||
              detail.poolIssues.nonPositiveQuotaIds.length > 0) && (
              <Alert
                type="error"
                showIcon
                style={{ marginBottom: 16 }}
                title="身份池存在脏数据"
                description={
                  <>
                    {detail.poolIssues.duplicateIds.length > 0 && (
                      <div>重复 id：{detail.poolIssues.duplicateIds.join('、')}</div>
                    )}
                    {detail.poolIssues.blankIdCount > 0 && <div>空 id 条目：{detail.poolIssues.blankIdCount} 条</div>}
                    {detail.poolIssues.nonPositiveQuotaIds.length > 0 && (
                      <div>配额 ≤ 0：{detail.poolIssues.nonPositiveQuotaIds.join('、')}</div>
                    )}
                    <div>建模板端点会拦下这些条目，出现在此说明是历史数据或绕过端点直写库。</div>
                  </>
                }
              />
            )}

            <dl className="calibration__stats">
              <div className="calibration__stat">
                <dt>已扫描世界</dt>
                <dd>
                  {formatNumber(d.worldsScanned)}
                  {d.worldsTruncated && <small>已截断</small>}
                </dd>
              </div>
              <div className="calibration__stat">
                <dt>发生过分配的世界</dt>
                <dd>
                  {formatNumber(d.worldsWithAssignments)}
                  <small>已装配 {formatNumber(d.worldsAssembled)}</small>
                </dd>
              </div>
              <div className="calibration__stat">
                <dt>分配人次</dt>
                <dd>{formatNumber(d.assignmentTotal)}</dd>
              </div>
              <div className={`calibration__stat${d.gini != null && d.gini > 0.35 ? ' is-attention' : ''}`}>
                <dt>分配集中度（基尼）</dt>
                <dd>{d.gini == null ? '—' : d.gini.toFixed(3)}</dd>
              </div>
            </dl>

            <div className="calibration__section">逐身份分配</div>
            <p className="calibration__hint">
              {/* JSX 文本按字面渲染：强调必须用 <strong>，写 Markdown 星号会在界面上露出两个 `**`。 */}
              填充率 = 分配人次 ÷（配额 × 发生过分配的世界数）。配额是<strong>上限不是保底</strong>：人少于配额合计时槽位空置，
              人多于配额合计时余下角色不分配身份，两者都不是错误。分母为 0 时显示 <code>—</code>，不当 0% 读。
            </p>
            <Table
              rowKey="identityId"
              size="small"
              pagination={false}
              dataSource={d.byIdentity}
              locale={{ emptyText: <Empty description="该模板未声明任何身份" /> }}
              columns={[
                {
                  title: '身份',
                  key: 'identity',
                  render: (_: unknown, r: IdentityStat) => (
                    <>
                      {r.label}
                      {r.isLead && (
                        <Tag color="gold" style={{ marginLeft: 6 }}>
                          戏眼主位
                        </Tag>
                      )}
                      <div>
                        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                          {r.identityId || '（缺 id）'}
                        </Typography.Text>
                      </div>
                    </>
                  ),
                },
                { title: '配额', dataIndex: 'quota', key: 'quota', width: 70 },
                { title: '分配人次', dataIndex: 'assignedCount', key: 'assignedCount', width: 90 },
                { title: '覆盖世界', dataIndex: 'worldCount', key: 'worldCount', width: 90 },
                {
                  title: '填充率',
                  key: 'fill',
                  width: 110,
                  render: (_: unknown, r: IdentityStat) => (
                    <span
                      style={
                        r.fillRatio == null
                          ? undefined
                          : { color: r.fillRatio === 0 ? '#c64d40' : '#38342f', fontWeight: 600 }
                      }
                    >
                      {formatPercent(r.fillRatio)}
                    </span>
                  ),
                },
                {
                  title: '主题词（内核匹配依据）',
                  key: 'themes',
                  render: (_: unknown, r: IdentityStat) =>
                    r.themes.length ? (
                      <Space size={4} wrap>
                        {r.themes.map((t) => (
                          <Tag key={t}>{t}</Tag>
                        ))}
                      </Space>
                    ) : (
                      '—'
                    ),
                },
                {
                  title: '钩子引力',
                  key: 'hooks',
                  width: 160,
                  render: (_: unknown, r: IdentityStat) =>
                    r.hookAffinity.length ? (
                      <Space size={4} wrap>
                        {r.hookAffinity.map((h) => (
                          <Tag key={h} color="blue">
                            {h}
                          </Tag>
                        ))}
                      </Space>
                    ) : (
                      '—'
                    ),
                },
              ]}
            />

            {d.neverAssignedIdentityIds.length > 0 && (
              <Alert
                type="warning"
                showIcon
                style={{ marginTop: 12 }}
                title={`${d.neverAssignedIdentityIds.length} 个身份从未被分到过`}
                description={`${d.neverAssignedIdentityIds.join('、')} —— 已发生过分配但这些站位一次都没被抽中，多半是主题词与入场卡内核不重叠，或配额在它之前就被用尽。`}
              />
            )}
            {d.unknownIdentityIds.length > 0 && (
              <Alert
                type="error"
                showIcon
                style={{ marginTop: 12 }}
                title="有实例钉着模板里已不存在的身份"
                description={`${d.unknownIdentityIds
                  .map((u) => `${u.identityId}（${u.assignedCount} 人次）`)
                  .join('、')} —— 模板改版后老实例不会回溯改写；叙事层对这些角色退化为只显示名字。`}
              />
            )}

            <div className="calibration__section">逐世界明细</div>
            <p className="calibration__hint">
              「在场无身份」含<strong>装配之后才入场</strong>的成员——他们本就不在那次分配的名单里，不是分配失败。
              全局合计：在场 {formatNumber(d.activeMemberTotal)} 人，其中 {formatNumber(d.activeMembersWithoutIdentity)} 人无身份。
            </p>
            <Table
              rowKey="id"
              size="small"
              pagination={false}
              dataSource={d.worlds}
              locale={{ emptyText: <Empty description="该模板还没有开出任何世界" /> }}
              columns={[
                { title: '世界', dataIndex: 'title', key: 'title' },
                {
                  title: '状态',
                  dataIndex: 'status',
                  key: 'status',
                  width: 90,
                  render: (v: string) => WORLD_STATUS_TEXT[v] ?? v,
                },
                {
                  title: '装配',
                  dataIndex: 'assembled',
                  key: 'assembled',
                  width: 90,
                  render: (v: boolean) => (v ? <Tag color="green">已装配</Tag> : <Tag>未装配</Tag>),
                },
                { title: '分配人次', dataIndex: 'assignmentCount', key: 'assignmentCount', width: 90 },
                { title: '在场成员', dataIndex: 'activeMemberCount', key: 'activeMemberCount', width: 90 },
                {
                  title: '在场无身份',
                  dataIndex: 'activeMembersWithoutIdentity',
                  key: 'without',
                  width: 100,
                },
              ]}
            />

            <Notes notes={detail.notes} />
          </>
        )}
      </Drawer>
    </div>
  );
}

// ================= 维度三：境界档 =================

/**
 * 戏服带：把「同一系列各阶段各发什么戏服」画成一条可扫视的带子。
 * 这是境界档这一维的核心视图 —— §6「你选阶段，就是在选境界」，
 * 若各阶同档（复用）或多阶缺档，这句话在那几阶之间就不成立，带子上一眼可见。
 * 状态一律「文字 + 颜色」：缺档写「缺」，复用写「复用」（设计文档 §5，不用彩色圆点）。
 */
function RealmStrip({ stages, reused }: { stages: RealmStage[]; reused: { tierId: string }[] }) {
  if (!stages.length) {
    return <Typography.Text type="secondary">该模板不属于任何世界系列，没有同系列各阶可对照。</Typography.Text>;
  }
  const reusedSet = new Set(reused.map((r) => r.tierId));
  return (
    <div className="calibration__realm-strip">
      {stages.map((s) => {
        const missing = !s.declared || !s.tierId;
        const isReused = !missing && reusedSet.has(s.tierId as string);
        const cls = missing ? 'is-missing' : isReused ? 'is-duplicate' : '';
        return (
          <span
            className={`calibration__realm-chip ${cls}${s.isSelf ? ' is-self' : ''}`}
            key={s.templateId}
            title={`${s.title}（阶段 ${s.stageNo}）`}
          >
            <b>阶段 {s.stageNo}</b>
            <span>{missing ? '缺戏服' : s.label}</span>
            {isReused && <small>与其他阶复用同一档</small>}
          </span>
        );
      })}
    </div>
  );
}

function RealmTiers({ deepLink }: { deepLink?: string }) {
  const [data, setData] = useState<RealmDirectoryRes | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [detail, setDetail] = useState<RealmDetailRes | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setData(await adminFetch<RealmDirectoryRes>('/admin/realm-tiers'));
    } catch (e) {
      setError(friendlyError(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const openDetail = useCallback(async (templateId: string) => {
    setDetailId(templateId);
    setDetail(null);
    setDetailError(null);
    setDetailLoading(true);
    try {
      setDetail(
        await adminFetch<RealmDetailRes>(`/admin/world-templates/${encodeURIComponent(templateId)}/realm-tier`),
      );
    } catch (e) {
      setDetailError(friendlyError(e));
    } finally {
      setDetailLoading(false);
    }
  }, []);

  // 深链 `?realm=tpl_xxx`（见 StageSplitting 同处说明）。
  useEffect(() => {
    if (deepLink) openDetail(deepLink);
  }, [deepLink, openDetail]);

  const columns: TableColumnsType<RealmRow> = [
    { title: '模板', dataIndex: 'title', key: 'title' },
    {
      title: '系列 / 阶段',
      key: 'saga',
      width: 200,
      render: (_, r) =>
        r.sagaId ? (
          <>
            <Typography.Text code>{r.sagaId}</Typography.Text>
            <Typography.Text type="secondary"> · 第 {r.stageNo} 阶</Typography.Text>
          </>
        ) : (
          <Typography.Text type="secondary">独立模板</Typography.Text>
        ),
    },
    {
      title: '境界档',
      key: 'tier',
      width: 200,
      render: (_, r) => (
        <>
          {r.label}
          <div>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              {r.tierId || '（缺 id）'}
            </Typography.Text>
          </div>
        </>
      ),
    },
    {
      title: '体系',
      key: 'cosmology',
      width: 120,
      render: (_, r) => (
        <EnumText value={r.cosmology} map={COSMOLOGY_TEXT} invalid={r.invalidEnumFields.includes('cosmology')} />
      ),
    },
    {
      title: '题材',
      key: 'genre',
      width: 100,
      render: (_, r) => (
        <EnumText value={r.genre} map={GENRE_TEXT} invalid={r.invalidEnumFields.includes('genre')} />
      ),
    },
    {
      title: '冲突烈度',
      key: 'intensity',
      width: 110,
      render: (_, r) => {
        if (!r.conflictIntensity) return <Typography.Text type="secondary">—</Typography.Text>;
        if (r.invalidEnumFields.includes('conflictIntensity')) {
          return <Tag color="red">{r.conflictIntensity}（非官方取值）</Tag>;
        }
        const t = INTENSITY_TEXT[r.conflictIntensity];
        return <Tag color={t?.color ?? 'default'}>{t?.text ?? r.conflictIntensity}</Tag>;
      },
    },
    {
      title: '世界（在跑/总）',
      key: 'worlds',
      width: 130,
      render: (_, r) => `${formatNumber(r.liveWorldCount)} / ${formatNumber(r.worldCount)}`,
    },
    {
      title: '操作',
      key: 'op',
      width: 110,
      fixed: 'right',
      render: (_, r) => (
        <Button size="small" onClick={() => openDetail(r.templateId)}>
          查看戏服
        </Button>
      ),
    },
  ];

  const dec = detail?.declaration;
  const st = detail?.sagaStages;
  const pin = detail?.pinning;

  return (
    <div>
      {/* 🔴 效力自述放在最上面：这一维的叙事感知层是缺的，运营在列表页就该知道，
          而不是点进详情才发现「配了半天玩家看不到」。 */}
      {data && <EffectPanel title="境界档现在到底有什么用" effect={data.effect} layers={REALM_LAYERS} />}
      <ReadOnlyBanner editable={data?.editable} editPath={data?.editPath} />

      <dl className="calibration__stats">
        <div className="calibration__stat">
          <dt>已配戏服的模板</dt>
          <dd>{data ? formatNumber(data.templates.length) : '—'}</dd>
        </div>
        <div className={`calibration__stat${data?.undeclaredInSagaCount ? ' is-attention' : ''}`}>
          <dt>系列里缺戏服的阶段</dt>
          <dd>
            {data ? formatNumber(data.undeclaredInSagaCount) : '—'}
            <small>校准缺口</small>
          </dd>
        </div>
        <div className="calibration__stat">
          <dt>没戏服的独立模板</dt>
          <dd>
            {data ? formatNumber(data.undeclaredStandaloneCount) : '—'}
            <small>只作对照</small>
          </dd>
        </div>
        <div className="calibration__stat">
          <dt>已扫描模板</dt>
          <dd>
            {data ? formatNumber(data.scannedTemplates) : '—'}
            {data?.truncated && <small>已截断</small>}
          </dd>
        </div>
      </dl>

      <Table
        rowKey="templateId"
        size="small"
        columns={columns}
        dataSource={data?.templates ?? []}
        loading={loading}
        pagination={false}
        scroll={{ x: 1180 }}
        title={() => (
          <Space>
            <Button size="small" onClick={load} loading={loading}>
              刷新
            </Button>
            <Typography.Text type="secondary">
              只列出声明了 realmTier 的模板。境界档全员统一，故本维没有「分布」可看。
            </Typography.Text>
          </Space>
        )}
        locale={{
          emptyText: (
            <Empty
              description={
                error
                  ? '未能取到数据'
                  : '没有任何模板声明了境界档。归属世界系列的阶段本应各有一件戏服（总规格 §6），独立模板没有则属正常。'
              }
            />
          ),
        }}
      />

      {error && <ErrorAlert message={error} onRetry={load} />}
      <Notes notes={data?.notes} />

      <Drawer
        title={`境界档：${detail?.title ?? detailId ?? ''}`}
        width={980}
        open={!!detailId}
        onClose={() => setDetailId(null)}
        loading={detailLoading}
      >
        {detailError && <ErrorAlert message={detailError} onRetry={() => detailId && openDetail(detailId)} />}
        {detail && st && pin && (
          <>
            <EffectPanel title="境界档现在到底有什么用" effect={detail.effect} layers={REALM_LAYERS} />
            <ReadOnlyBanner editable={detail.editable} editPath={detail.editPath} />

            {!detail.declared && (
              <Alert
                type="warning"
                showIcon
                style={{ marginBottom: 16 }}
                title="该模板未声明 realmTier"
                description={
                  detail.sagaId
                    ? '它是一个世界系列的阶段，按总规格 §6「阶段天然携带境界档」本应配一件戏服 —— 这是校准缺口。'
                    : '独立模板（不属于任何世界系列）没有戏服是正常状态，不是缺数据。'
                }
              />
            )}

            {dec && (dec.blankTierId || dec.invalidEnumFields.length > 0) && (
              <Alert
                type="error"
                showIcon
                style={{ marginBottom: 16 }}
                title="境界档存在脏数据"
                description={
                  <>
                    {dec.blankTierId && <div>缺少档位 id：无法跨阶段对账与审计。</div>}
                    {dec.invalidEnumFields.length > 0 && (
                      <div>填了官方枚举外的自由文本：{dec.invalidEnumFields.join('、')}</div>
                    )}
                    <div>建模板端点会拦下这些取值，出现在此说明是历史数据或绕过端点直写库。</div>
                  </>
                }
              />
            )}

            {dec?.stricterModerationHint && (
              <Alert
                type="info"
                showIcon
                style={{ marginBottom: 16 }}
                title="历史题材：按总规格 §6 应走更严审核档（合规）"
                description="这只是一条提示：本仓库尚未把题材接进任何审核链路，标了 history 不会自动改变审核档位，仍需人工按更严标准复核。"
              />
            )}

            {dec && (
              <>
                <div className="calibration__section">这一阶发的戏服</div>
                <dl className="calibration__kv">
                  <div>
                    <dt>档名</dt>
                    <dd>
                      {dec.label}
                      <small>{dec.tierId || '（缺 id）'}</small>
                    </dd>
                  </div>
                  <div>
                    <dt>体系</dt>
                    <dd>
                      <EnumText
                        value={dec.cosmology}
                        map={COSMOLOGY_TEXT}
                        invalid={dec.invalidEnumFields.includes('cosmology')}
                      />
                      {!dec.cosmology && <small>留空 = 无战力体系题材，境界泛化为处境</small>}
                    </dd>
                  </div>
                  <div>
                    <dt>题材</dt>
                    <dd>
                      <EnumText
                        value={dec.genre}
                        map={GENRE_TEXT}
                        invalid={dec.invalidEnumFields.includes('genre')}
                      />
                    </dd>
                  </div>
                  <div>
                    <dt>冲突烈度</dt>
                    <dd>
                      {dec.conflictIntensity ? (
                        <Tag color={INTENSITY_TEXT[dec.conflictIntensity]?.color ?? 'default'}>
                          {INTENSITY_TEXT[dec.conflictIntensity]?.text ?? dec.conflictIntensity}
                        </Tag>
                      ) : (
                        <Typography.Text type="secondary">—</Typography.Text>
                      )}
                      <small>叙事标注，不是死亡开关（世界是否致命由建房参数决定）</small>
                    </dd>
                  </div>
                </dl>
                <p className="calibration__hint">
                  入场导演统一设定：{dec.briefing || <Typography.Text type="secondary">（未填写）</Typography.Text>}
                </p>
                {dec.flavorNotes.length > 0 && (
                  <p className="calibration__hint">
                    跨体系风味翻译提示：
                    <Space size={4} wrap>
                      {dec.flavorNotes.map((n) => (
                        <Tag key={n}>{n}</Tag>
                      ))}
                    </Space>
                  </p>
                )}
              </>
            )}

            <div className="calibration__section">同系列各阶对照（你选阶段，就是在选境界）</div>
            <RealmStrip stages={st.stages} reused={st.reusedTierIds} />
            {st.reusedTierIds.length > 0 && (
              <Alert
                type="warning"
                showIcon
                style={{ marginTop: 8 }}
                title="同一系列里有多个阶段发同一件戏服"
                description={`${st.reusedTierIds
                  .map((r) => `${r.tierId}（阶段 ${r.stageNos.join('、')}）`)
                  .join('；')} —— 在这几阶之间「选阶段 = 选境界」不成立，值得复核是不是漏改。`}
              />
            )}
            {st.stagesWithoutRealmTier.length > 0 && (
              <Alert
                type="warning"
                showIcon
                style={{ marginTop: 8 }}
                title={`${st.stagesWithoutRealmTier.length} 个阶段没有戏服`}
                description={`阶段 ${st.stagesWithoutRealmTier.join('、')} —— 按总规格 §6，系列的每一阶都应携带自己的境界档。`}
              />
            )}
            {st.distinctCosmologies.length > 1 && (
              <Alert
                type="info"
                showIcon
                style={{ marginTop: 8 }}
                title="同一系列跨了多个体系"
                description={`${st.distinctCosmologies
                  .map((c) => COSMOLOGY_TEXT[c] ?? c)
                  .join('、')} —— 按 §6，跨体系应靠风味翻译而不是换一套数值，值得复核是不是标错。`}
              />
            )}

            <div className="calibration__section">实例钉住情况</div>
            <p className="calibration__hint">
              境界档在装配那一刻就钉死在实例的 <code>assembled_json</code> 里，改模板<strong>不会</strong>回溯改写已开出的世界。
              因此「已钉住」少于「已装配」是正常的 —— 那些实例是在这个模板声明戏服<strong>之前</strong>装配的。
            </p>
            <dl className="calibration__stats">
              <div className="calibration__stat">
                <dt>已扫描世界</dt>
                <dd>
                  {formatNumber(pin.worldsScanned)}
                  {pin.worldsTruncated && <small>已截断</small>}
                </dd>
              </div>
              <div className="calibration__stat">
                <dt>已钉住戏服</dt>
                <dd>
                  {formatNumber(pin.worldsWithRealmTier)}
                  <small>已装配 {formatNumber(pin.worldsAssembled)}</small>
                </dd>
              </div>
              <div className={`calibration__stat${pin.staleTierIds.length ? ' is-attention' : ''}`}>
                <dt>钉着旧档的实例</dt>
                <dd>{formatNumber(pin.staleTierIds.reduce((a, b) => a + b.worldCount, 0))}</dd>
              </div>
              <div className="calibration__stat">
                <dt>模板版本</dt>
                <dd>v{detail.version}</dd>
              </div>
            </dl>
            {pin.staleTierIds.length > 0 && (
              <Alert
                type="warning"
                showIcon
                style={{ marginBottom: 12 }}
                title="有实例钉着与当前模板不一致的境界档"
                description={`${pin.staleTierIds
                  .map((s) => `${s.tierId}（${s.worldCount} 个世界）`)
                  .join('、')} —— 模板改版后老实例保持原样，这不是故障，但复盘那几个世界时要按它们各自钉住的档来读。`}
              />
            )}
            <Table
              rowKey="id"
              size="small"
              pagination={false}
              dataSource={pin.worlds}
              locale={{ emptyText: <Empty description="该模板还没有开出任何世界" /> }}
              columns={[
                { title: '世界', dataIndex: 'title', key: 'title' },
                {
                  title: '状态',
                  dataIndex: 'status',
                  key: 'status',
                  width: 90,
                  render: (v: string) => WORLD_STATUS_TEXT[v] ?? v,
                },
                {
                  title: '装配',
                  dataIndex: 'assembled',
                  key: 'assembled',
                  width: 90,
                  render: (v: boolean) => (v ? <Tag color="green">已装配</Tag> : <Tag>未装配</Tag>),
                },
                {
                  title: '钉住的戏服',
                  key: 'pinned',
                  render: (_: unknown, r: RealmDetailRes['pinning']['worlds'][number]) =>
                    r.pinnedTierId ? (
                      <>
                        {r.pinnedLabel}
                        <Typography.Text type="secondary" style={{ fontSize: 12, marginLeft: 6 }}>
                          {r.pinnedTierId}
                        </Typography.Text>
                      </>
                    ) : (
                      <Typography.Text type="secondary">未钉住</Typography.Text>
                    ),
                },
                {
                  title: '与模板一致',
                  key: 'matches',
                  width: 120,
                  // null（模板未声明 / 实例未钉住）必须显示 —，不得当「不一致」渲染。
                  render: (_: unknown, r: RealmDetailRes['pinning']['worlds'][number]) =>
                    r.matchesTemplate == null ? (
                      <Typography.Text type="secondary">—</Typography.Text>
                    ) : r.matchesTemplate ? (
                      <Tag color="green">一致</Tag>
                    ) : (
                      <Tag color="orange">钉着旧档</Tag>
                    ),
                },
              ]}
            />

            <Notes notes={detail.notes} />
          </>
        )}
      </Drawer>
    </div>
  );
}

// ================= 主页面 =================

export default function Calibration() {
  // 深链：`?saga=<sagaId>` 打开阶段结构抽屉，`?pool=<templateId>` 切到身份池并打开分布抽屉，
  // `?realm=<templateId>` 切到境界档并打开戏服抽屉。
  // 运营排查时贴一条链接就能让对方落到同一屏，不必口述「点第几行的按钮」。
  const params = new URLSearchParams(useLocation().search);
  const saga = params.get('saga') ?? undefined;
  const pool = params.get('pool') ?? undefined;
  const realm = params.get('realm') ?? undefined;

  return (
    <div className="calibration">
      <div className="calibration__head">
        <h4>人工校准面</h4>
        <span className="calibration__readonly">只读 · 不可调参</span>
        <small>
          内容生产流水线第一环：人工校准 → 仿真试跑 → 世界质量回归。三维分别看三件事：
          阶段切分看坐标是否齐整，身份池看站位分布是否失衡，境界档看这一阶有没有戏服、各阶是不是在换。
          境界档目前只到「已声明 / 已钉住」，引擎尚未读取它 —— 详见各页顶部的效力自述。
        </small>
      </div>

      <Tabs
        defaultActiveKey={realm ? 'realm' : pool ? 'identity' : 'stages'}
        items={[
          { key: 'stages', label: '阶段切分', children: <StageSplitting deepLink={saga} /> },
          { key: 'identity', label: '身份池', children: <IdentityPools deepLink={pool} /> },
          { key: 'realm', label: '境界档', children: <RealmTiers deepLink={realm} /> },
        ]}
      />
    </div>
  );
}
