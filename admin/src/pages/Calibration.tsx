// 人工校准面（总规格 §79/§83 内容生产流水线第一环：人工校准 → 仿真试跑 → 世界质量回归）。
//
// 本页只做**两维**：阶段切分（saga_id / stage_no）与身份池（skeleton.identityPool → 实例分配）。
// 境界档不在本页——它在 Skeleton 里没有任何字段落点，补 schema 会动到装配产物与黄金世界快照，
// 属独立评审项（见 server/src/admin_api/calibration.rs 模块头）。
//
// 🔴 两条必须在界面上说清楚、不得靠运营自行推断的事：
//   ① **只可视化，不可编辑**：后端四个端点全只读，恒下发 `editable:false` + `editPath`。
//      页面把这两个字段渲染出来，而不是自己写死一句「只读」——后端哪天开了写入面，界面自动跟上。
//   ② **身份池的真实效力**：分配层与叙事感知层都已落地，但按平权红线永不进数值层，
//      且没有任何指标度量「调身份池 → 戏份分布变化」。这四层状态由后端 `effect` 段下发，
//      本页原样渲染（`EffectPanel`），**不得只画分布图**。
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

interface EffectScope {
  assignmentLayer: string;
  narrativeLayer: string;
  numericLayer: string;
  calibrationLoop: string;
  summary: string;
  warning: string;
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

const LAYER_LABEL: { key: keyof EffectScope; label: string }[] = [
  { key: 'assignmentLayer', label: '分配层' },
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
 * 效力自述面板：身份池「哪一层已经生效、哪一层永不生效、哪一层根本没有」。
 * 🔴 这一段不是装饰——没有它，运营会把下面的分布图误读成「调了就会变强」的证据。
 */
function EffectPanel({ effect }: { effect: EffectScope }) {
  return (
    <div className="calibration__effect">
      <h5>身份池现在到底有什么用</h5>
      <p>{effect.summary}</p>
      <dl className="calibration__layers">
        {LAYER_LABEL.map(({ key, label }) => {
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
            <EffectPanel effect={detail.effect} />
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

// ================= 主页面 =================

export default function Calibration() {
  // 深链：`?saga=<sagaId>` 打开阶段结构抽屉，`?pool=<templateId>` 切到身份池并打开分布抽屉。
  // 运营排查时贴一条链接就能让对方落到同一屏，不必口述「点第几行的按钮」。
  const params = new URLSearchParams(useLocation().search);
  const saga = params.get('saga') ?? undefined;
  const pool = params.get('pool') ?? undefined;

  return (
    <div className="calibration">
      <div className="calibration__head">
        <h4>人工校准面</h4>
        <span className="calibration__readonly">只读 · 不可调参</span>
        <small>
          内容生产流水线第一环：人工校准 → 仿真试跑 → 世界质量回归。本期只做阶段切分与身份池两维；
          境界档在世界骨架里尚无字段落点，需先补 schema，故本页不展示。
        </small>
      </div>

      <Tabs
        defaultActiveKey={pool ? 'identity' : 'stages'}
        items={[
          { key: 'stages', label: '阶段切分', children: <StageSplitting deepLink={saga} /> },
          { key: 'identity', label: '身份池', children: <IdentityPools deepLink={pool} /> },
        ]}
      />
    </div>
  );
}
