// 数据看板：/admin/metrics/overview 聚合 → antd Statistic + echarts（token 成本 / tick 分布
// / 错峰调度成本仪表）+ /admin/metrics/trends 按天趋势（近 7/14/30 天折线，独立加载不影响 overview）。
import { useEffect, useRef, useState } from 'react';
import { Button, Card, Col, Empty, Row, Segmented, Space, Spin, Statistic, Table, Tag, Tooltip, Typography } from 'antd';
import ReactECharts from 'echarts-for-react';
import { adminFetch } from '../api';
import { ErrorAlert, formatNumber, formatPercent, friendlyError } from '../components/shared';

/**
 * 错峰调度可观测面（`cost.offPeak`，migration 0038 三列的唯一读出口）。
 *
 * 🔴 **单位口径**（服务端 `admin_api/dashboards.rs` 同名注释，docs/API.md 登记）：
 * - `tickRatio` / `tokenRatio` / `savedRatio` 一律 **0..1 小数**（同 successRate/usageRatio），
 *   渲染必须走 `formatPercent`（内部 ×100）；窗口内一拍都没有 → `null`，显示 `—` 而非 0%。
 * - `priceRatioPct` 是**百分数整数**（100=原价、50=5 折），`priceRatio` 是它的 0..1 形态。
 *   两者绝不可混用——把 `priceRatioPct` 丢进 `formatPercent` 会渲染成 5000%。
 * - 金额账面口径是整数分（`*Cents`），`*Cny` 为展示派生值。
 */
interface OffPeakMeter {
  windowDays: number;
  ticks: number;
  tokens: number;
  offPeakTicks: number;
  offPeakTokens: number;
  /** 0..1 小数，无 tick 时为 null。 */
  tickRatio: number | null;
  tokenRatio: number | null;
  savedRatio: number | null;
  nominalCents: number;
  nominalCny: number;
  savedCents: number;
  savedCny: number;
  effectiveCents: number;
  effectiveCny: number;
  deferredTicks: number;
  deferMsTotal: number;
  deferMsMax: number;
  /** 平均延后毫秒，分母只含被延后过的拍；无被延后拍 → null。 */
  avgDeferMs: number | null;
  byRatio: OffPeakRatioBucket[];
}

interface OffPeakRatioBucket {
  /** 百分数整数（100=原价）。 */
  priceRatioPct: number;
  /** 同一个数的 0..1 形态。 */
  priceRatio: number;
  ticks: number;
  tokens: number;
  savedCents: number;
  savedCny: number;
}

/** 平台成本仪表（`/admin/metrics/overview` 顶层 `cost`）。金额一律整数分为账面，`cny` 为展示派生。 */
interface CostOverview {
  centsPer1kTokens: number;
  today: { day: string; tokens: number; cents: number; cny: number };
  trendDays: number;
  /** `offPeakTokens` 是当日 `tokens` 里走了折扣时段的那部分，两者是**包含**关系，不可相加。 */
  trend: { day: string; tokens: number; cents: number; cny: number; offPeakTokens?: number }[];
  offPeak?: OffPeakMeter | null;
}

/**
 * **一个校准读数的信封**（服务端 `slo/calibration.rs` §0.5，随 `narrativeSlo.calibration` 下发）。
 *
 * 🔴 `value` 与 `n` 同处一个对象是**刻意的**：取值必须穿过这层信封，于是
 * 「拿到一个数却不知道它压在几个观察上」在结构上不可能发生。前端这一侧对应的纪律是
 * `ReadingCell` —— 渲染读数**只准**走它，它永远把 n 一起画出来。
 */
interface Reading {
  /** 四态见 `ReadingStatus`。 */
  status: ReadingStatus;
  /** **可据此调参的读数**；`status ≠ ok` 时恒为 null。🔴 不得回落成 `pointEstimate` 渲染。 */
  value: number | null;
  /** 原始算术，永远给。只在明确标着「样本不足」的地方出现（tooltip），不进正文。 */
  pointEstimate: number | null;
  /** 观察数。`unit` 说明它数的是世界 / 拍 / 事件 / 观察。 */
  n: number;
  /** 生效的最小样本量门槛（服务端 `MUSE_SLO_CALIBRATION_MIN_N`）。 */
  minN: number;
  unit?: string;
  /** 比例类读数带 95% Wilson 区间；基尼与均值类恒为 null，理由见 `ciNoteRef`。 */
  ci95?: { low: number; high: number; method: string; level: number } | null;
  /** 区间说明的**短码**，全文在 `calibration.ciNotes[code]`（逐条重发会让被轮询的端点胖上百 KB）。 */
  ciNoteRef?: string | null;
  worlds?: number;
  sampleN?: number;
}

/**
 * 🔴 **四态绝不可长成同一个样子**：
 * `entry_not_open`（这一维从未被配置过，块级）/ `no_data_in_window`（分母为 0）/
 * `insufficient_sample`（**有样本但 n < minN**）/ `ok`（真数，可以是 0）。
 * 前两者显示 `—`；第三者显示「样本不足（n=…）」——它既不是 0 也不是 `—`。
 */
type ReadingStatus = 'ok' | 'insufficient_sample' | 'no_data_in_window' | 'entry_not_open';

interface IdentityBucket {
  identityId: string;
  status: ReadingStatus;
  observations: number;
  worlds: number;
  meanRelativeShare: Reading;
  zeroScoreObservations: number;
  zeroScoreRate: Reading;
}

interface RealmBucket {
  tierId: string;
  status: ReadingStatus;
  worlds: number;
  completion: { completionRate: Reading; natural: number; unfinished: number };
  blocking: { blockedRate: Reading; withheldRate: Reading };
  endings: { distinctEndings: number; concentrationGini: Reading };
}

/** 按校准维度分组的只读读数（身份维 = 组内分布，戏服维 = 跨世界对比）。 */
interface CalibrationReadings {
  status: string;
  windowDays?: number;
  worldsScanned?: number;
  sampleFloor?: { minN: number; minGroups: number };
  /** 短码 → 区间说明全文。读数只带短码，全文在这里给一次。 */
  ciNotes?: Record<string, string>;
  dimensions: {
    identityShareBalance?: {
      status: ReadingStatus;
      title?: string;
      notes?: string[];
      observations?: number;
      worldsCounted?: number;
      identitiesObserved?: number;
      meanShareGini?: Reading;
      byIdentity?: IdentityBucket[];
    };
    realmTierWorldQuality?: {
      status: ReadingStatus;
      title?: string;
      notes?: string[];
      worldsScanned?: number;
      tiersObserved?: number;
      tiersWithSufficientSample?: number;
      byRealmTier?: RealmBucket[];
    };
  };
}

/**
 * **一个 SLO 指标块**（服务端 `slo/mod.rs`，随 `narrativeSlo.metrics` 下发）。
 *
 * 🔴 `status` 是一套**七态**，只有 `ok` 该显示数值，其余一律显示 `—`：
 * `ok`（有样本，**可以是真的 0%**）/ `no_data_in_window`（分窗口，本窗口零样本）/
 * `no_data_yet`（全生命周期口径，至今零样本）/ `entry_not_open`（入口从未开过）/
 * `no_data_source`（口径未拍板，压根算不出）/ `skipped_too_large`（超扫描上限）/
 * `skipped_by_request`（调用方传了 `?slo=0`）。
 *
 * 显示 `—` 与显示 `0%` 是两个完全不同的经营判断。服务端 2026-07-28 之前有四项在空平台上
 * 报 `0%`（见 `docs/VALIDATION.md` §3.36）——`forcedRate: 0` 像好消息、
 * `repeatEntryRate: 0` 像事故，而它们来自同一个空库。前端这一侧的纪律就是本注释这条：
 * **status ≠ ok 一律不画数字**。
 */
interface SloMetricBlock {
  metric?: string;
  /** 中文标题**由服务端给**（不在前端另维护一张名字表，见 `SLO_HEADLINE` 的注释）。 */
  title?: string;
  status: string;
  value?: number | null;
  notes?: string[];
  reason?: string;
  [key: string]: unknown;
}

interface NarrativeSlo {
  status: string;
  windowDays?: number;
  /** 七项指标，键 = metric 名。**前端不假设有哪几项**，见 `sloRows`。 */
  metrics?: Record<string, SloMetricBlock> | null;
  /** 「没有数据源」的指标名单，与 `metrics` 里那几项的 `no_data_source` 一致。 */
  unavailable?: string[];
  calibration?: CalibrationReadings | null;
}

interface MetricsOverview {
  users: { total: number; banned: number };
  dailyReports: { total: number; opened: number; openRate: number };
  ticks: { total: number; done: number; failed: number; successRate: number };
  tokenCostByWorld: { worldId: string; tokens: number }[];
  /** 旧版 server 可能不下发；缺席时错峰区整段走空态，不报错。 */
  cost?: CostOverview | null;
  auditBacklog: number;
  worlds: { active: number; fused: number };
  riskEvents: number;
  dataRequestsPending: number;
  /** 旧版 server 可能不下发；缺席时校准区整段走空态，不报错。 */
  narrativeSlo?: NarrativeSlo | null;
}

/** 按天趋势（GET /admin/metrics/trends）：UTC 日界、升序、末位为今天、空天补零。 */
interface TrendDay {
  day: string;
  newUsers: number;
  activeWorlds: number;
  events: number;
  tickTokens: number;
  giftCount: number;
  revenueCents: number;
}

// 趋势系列色（4 系列 categorical，已过色觉/对比校验；固定顺序分配，不轮转）。
const TREND_COLORS = ['#1677ff', '#389e0d', '#722ed1', '#ad6800'];

const TREND_DAY_OPTIONS = [
  { label: '近 7 天', value: 7 },
  { label: '近 14 天', value: 14 },
  { label: '近 30 天', value: 30 },
];

// 'YYYY-MM-DD' → 'MM-DD'（x 轴刻度更紧凑，tooltip 仍显示全量）。
const shortDay = (d: string): string => d.slice(5);

// 错峰构成图的两个系列色：取 TREND_COLORS 的第 1 / 第 4 位。该对已对本页卡片底色
// （antd token colorBgContainer = #fffdfa）校验：亮度带 / 色度下限 / 对比度全过，
// CVD 分离 protan ΔE 32.1 · tritan 24.5、常视 ΔE 34.7。折扣段贴基线，便于逐日横比。
const OFF_PEAK_COLOR = TREND_COLORS[0];
const FULL_PRICE_COLOR = TREND_COLORS[3];
/** 堆叠段之间留 2px 卡片底色缝，避免两段糊成一根柱子。值须跟随 main.tsx 的 colorBgContainer。 */
const CARD_SURFACE = '#fffdfa';

/** 毫秒时长 → 人读字符串；null（无被延后拍）走 `—`，不得渲染成 0。 */
function formatDuration(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return '—';
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)} 秒`;
  const minutes = seconds / 60;
  if (minutes < 60) return `${minutes.toFixed(1)} 分钟`;
  return `${(minutes / 60).toFixed(1)} 小时`;
}

/** 金额（元）→ `¥1,234.56`。账面口径是整数分，这里只做展示格式化。 */
function formatCny(cny?: number | null): string {
  if (cny == null) return '—';
  return `¥${cny.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

function StatCard({ children }: { children: React.ReactNode }) {
  return <Card size="small">{children}</Card>;
}

/** 读数的数值格式：比例走百分比，倍率 / 基尼走两位小数。null → `—`。 */
function readingText(v: number | null | undefined, kind: 'percent' | 'ratio'): string {
  if (v == null) return '—';
  return kind === 'percent' ? formatPercent(v) : v.toFixed(2);
}

/**
 * 🔴 **校准读数的唯一渲染入口。永远同时画出 n。**
 *
 * 由来：`meanShareGini` 曾在 3 个观察与 300 个观察上长得一模一样，运营会追着噪声调参。
 * 四态各有各的样子，绝不混同：
 * - `ok` → 数值 + n（比例类另附 95% 区间——**区间有多宽，这个数就有多不确定**）；
 * - `insufficient_sample` → 「样本不足」标签 + n，🔴 **正文里不出现数字**
 *   （点估计只在 tooltip 里，且明说不足以据此调参）。它既不是 0 也不是 `—`；
 * - `no_data_in_window` → `—` + 「零样本」（分母真的是 0）；
 * - `entry_not_open` → 由外层整块渲染，不会走到这里。
 *
 * 🔴 本组件**不做判断**：不标红、不比大小、不给「显著」结论。给数与 n，让人自己看。
 */
function ReadingCell({
  r,
  kind,
  notes,
}: {
  r?: Reading | null;
  kind: 'percent' | 'ratio';
  /** 短码 → 全文（服务端 `calibration.ciNotes`）。缺席时 tooltip 只少一句解释，不影响 n 与状态。 */
  notes?: Record<string, string>;
}) {
  if (!r) return <span>—</span>;
  const n = (
    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
      n={formatNumber(r.n)}
    </Typography.Text>
  );
  if (r.status === 'no_data_in_window') {
    return (
      <Space size={4}>
        <span>—</span>
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>零样本</Typography.Text>
      </Space>
    );
  }
  if (r.status === 'insufficient_sample') {
    return (
      <Tooltip
        title={`样本不足：n=${r.n} < minN=${r.minN}。点估计 ${readingText(r.pointEstimate, kind)} 只是原始算术，不足以据此调参。${(r.ciNoteRef && notes?.[r.ciNoteRef]) ?? ''}`}
      >
        <Space size={4}>
          <Tag color="warning" style={{ marginInlineEnd: 0 }}>样本不足</Tag>
          {n}
        </Space>
      </Tooltip>
    );
  }
  const body = (
    <Space size={4} wrap>
      <strong>{readingText(r.value, kind)}</strong>
      {n}
      {r.ci95 && (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          95% CI [{readingText(r.ci95.low, kind)}, {readingText(r.ci95.high, kind)}]
        </Typography.Text>
      )}
    </Space>
  );
  // 有区间的读数附「这个区间怎么读」，没区间的读数附「为什么不给区间」——两种都要看得见。
  const note = r.ciNoteRef ? notes?.[r.ciNoteRef] : null;
  return note ? <Tooltip title={note}>{body}</Tooltip> : body;
}

/** 七态 → 中文标签。`ok` 不在表里（它走数值分支）。未知状态原样显示，不吞掉。 */
const SLO_STATUS_LABEL: Record<string, string> = {
  no_data_in_window: '窗口内零样本',
  no_data_yet: '至今零样本',
  entry_not_open: '入口未开',
  no_data_source: '无数据源',
  skipped_too_large: '数据量超限，已跳过',
  skipped_by_request: '本次未计算',
};

/**
 * 每项指标的**头条数字与样本量**取哪个字段。
 *
 * 🔴 这张表**只做增强，不做筛选**：渲染遍历的是服务端实际下发的 `metrics` 的每一个键，
 * 表里没登记的指标照样出现在表格里（标题走服务端的 `title`，数值回落到通用的 `value`）。
 * 反过来写成「渲染这七项」的话，服务端上线第八项时前端会**静默地不显示它**——
 * 而这一段的全部意义就是让指标能被人看见。判据方向见 `docs/VALIDATION.md` §3.8.1。
 */
const SLO_HEADLINE: Record<
  string,
  { field: string; kind: 'percent' | 'ratio'; label: string; sample: string; unit: string }
> = {
  attentionGini: { field: 'overThresholdRate', kind: 'percent', label: '越门槛世界占比', sample: 'worldsCounted', unit: '个世界' },
  silentStreak: { field: 'overThresholdRate', kind: 'percent', label: '越门槛成员占比', sample: 'membersCounted', unit: '名成员' },
  forcedConclusionRate: { field: 'forcedRate', kind: 'percent', label: '强制收尾占比', sample: 'endedWorlds', unit: '个已收尾世界' },
  repeatEntryRate: { field: 'repeatEntryRate', kind: 'percent', label: '二次入世占比', sample: 'charactersTotal', unit: '张云角色卡' },
  stateTextContradictionRate: { field: 'value', kind: 'percent', label: '矛盾拍占比', sample: 'ticksTotal', unit: '拍' },
  oocAppealRate: { field: 'value', kind: 'percent', label: '申诉占比', sample: 'memberStagesCounted', unit: '人次·阶段' },
  plotRepetitionRate: { field: 'value', kind: 'percent', label: '重复占比', sample: '', unit: '' },
};

/** 从指标块里取一个数值字段；不是有限数就当没有（NaN/Inf 不许流到界面上）。 */
function sloNum(block: SloMetricBlock, field: string): number | null {
  const v = block[field];
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

/** 维度整块的空态：把服务端下发的第一条 note 原样显示（「没测过」不是「很均衡」）。 */
function DimensionEmpty({ status, notes }: { status: ReadingStatus; notes?: string[] }) {
  const fallback =
    status === 'entry_not_open'
      ? '这一维从未被任何模板配置过 —— 是「没测过」，不是「很均衡」。'
      : '窗口内零样本 —— 是「没测过」，不是「很均衡」。';
  return <Empty description={notes?.[0] ?? fallback} />;
}

export default function Metrics() {
  const [data, setData] = useState<MetricsOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 趋势区独立状态：切换天数 / 失败重试都不影响上方 overview。
  const [trendDays, setTrendDays] = useState(14);
  const [trend, setTrend] = useState<TrendDay[] | null>(null);
  const [trendLoading, setTrendLoading] = useState(true);
  const [trendError, setTrendError] = useState<string | null>(null);
  // 请求序号：快速切换 7/14/30 时丢弃过期响应。
  const trendReqRef = useRef(0);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      setData(await adminFetch<MetricsOverview>('/admin/metrics/overview'));
    } catch (e) {
      setError(friendlyError(e));
    } finally {
      setLoading(false);
    }
  };

  const loadTrends = async () => {
    const seq = ++trendReqRef.current;
    setTrendLoading(true);
    setTrendError(null);
    try {
      const res = await adminFetch<{ days: TrendDay[] }>(`/admin/metrics/trends?days=${trendDays}`);
      if (seq !== trendReqRef.current) return;
      setTrend(res.days);
    } catch (e) {
      if (seq !== trendReqRef.current) return;
      setTrendError(friendlyError(e));
    } finally {
      if (seq === trendReqRef.current) setTrendLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  useEffect(() => {
    loadTrends();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [trendDays]);

  const tokenBarOption = data && {
    tooltip: { trigger: 'axis' as const },
    grid: { left: 60, right: 20, top: 20, bottom: 70 },
    xAxis: {
      type: 'category' as const,
      data: data.tokenCostByWorld.map((w) => w.worldId),
      axisLabel: { rotate: 35, formatter: (v: string) => (v.length > 10 ? `${v.slice(0, 10)}…` : v) },
    },
    yAxis: { type: 'value' as const, name: 'token' },
    series: [{ type: 'bar' as const, data: data.tokenCostByWorld.map((w) => w.tokens), itemStyle: { color: '#1677ff' } }],
  };

  // 图一「规模」：新增用户 / 活跃世界 / 世界事件 / 礼物量多系列折线（同为「个数」量纲，共用一根 y 轴）。
  const scaleTrendOption = trend && {
    color: TREND_COLORS,
    tooltip: { trigger: 'axis' as const },
    legend: { bottom: 0 },
    grid: { left: 48, right: 24, top: 24, bottom: 56 },
    xAxis: {
      type: 'category' as const,
      boundaryGap: false,
      data: trend.map((d) => d.day),
      axisLabel: { formatter: shortDay },
    },
    yAxis: { type: 'value' as const, minInterval: 1 },
    series: [
      { name: '新增用户', type: 'line' as const, data: trend.map((d) => d.newUsers) },
      { name: '活跃世界', type: 'line' as const, data: trend.map((d) => d.activeWorlds) },
      { name: '世界事件', type: 'line' as const, data: trend.map((d) => d.events) },
      { name: '礼物量', type: 'line' as const, data: trend.map((d) => d.giftCount) },
    ],
  };

  // 图二「消耗与收入」：token 与「元」量纲不同，不做双 y 轴——上下两个联动子图共用时间轴
  // （axisPointer link 同步十字线），各自独立 y 轴，避免双轴比例误读。
  const costTrendOption = trend && {
    color: [TREND_COLORS[0], TREND_COLORS[1]],
    tooltip: { trigger: 'axis' as const },
    axisPointer: { link: [{ xAxisIndex: 'all' as const }] },
    legend: { bottom: 0 },
    grid: [
      { left: 64, right: 24, top: 20, height: '30%' },
      { left: 64, right: 24, top: '50%', height: '30%' },
    ],
    xAxis: [
      {
        type: 'category' as const,
        gridIndex: 0,
        boundaryGap: false,
        data: trend.map((d) => d.day),
        axisLabel: { show: false },
        axisTick: { show: false },
      },
      {
        type: 'category' as const,
        gridIndex: 1,
        boundaryGap: false,
        data: trend.map((d) => d.day),
        axisLabel: { formatter: shortDay },
      },
    ],
    yAxis: [
      { type: 'value' as const, gridIndex: 0, name: 'token' },
      { type: 'value' as const, gridIndex: 1, name: '元' },
    ],
    series: [
      {
        name: 'token 消耗',
        type: 'line' as const,
        xAxisIndex: 0,
        yAxisIndex: 0,
        data: trend.map((d) => d.tickTokens),
        tooltip: { valueFormatter: (v: unknown) => `${formatNumber(Number(v))} token` },
      },
      {
        name: '收入（元）',
        type: 'line' as const,
        xAxisIndex: 1,
        yAxisIndex: 1,
        // revenueCents 分 → 元。
        data: trend.map((d) => d.revenueCents / 100),
        tooltip: { valueFormatter: (v: unknown) => `¥${Number(v).toFixed(2)}` },
      },
    ],
  };

  // ---------- 错峰调度成本仪表（cost.offPeak，migration 0038） ----------
  const cost = data?.cost ?? null;
  const offPeak = cost?.offPeak ?? null;
  // 三档形态：接口没给 → 整段空态；窗口内无 tick → "暂无数据"；有 tick 但错峰拍为 0 → 真实的 0%（关闭态）。
  const offPeakHasTicks = !!offPeak && offPeak.ticks > 0;
  const offPeakActive = !!offPeak && offPeak.offPeakTicks > 0;

  // 逐日构成：折扣时段贴基线，原价段叠在上面（tokens 是全量，两者是包含关系，故原价 = 全量 − 折扣）。
  const offPeakTrendOption = cost && {
    color: [OFF_PEAK_COLOR, FULL_PRICE_COLOR],
    tooltip: { trigger: 'axis' as const, axisPointer: { type: 'shadow' as const } },
    legend: { bottom: 0 },
    grid: { left: 64, right: 24, top: 20, bottom: 48 },
    xAxis: {
      type: 'category' as const,
      data: cost.trend.map((d) => d.day),
      axisLabel: { formatter: shortDay },
    },
    yAxis: { type: 'value' as const, name: 'token' },
    series: [
      {
        name: '折扣时段',
        type: 'bar' as const,
        stack: 'tokens',
        data: cost.trend.map((d) => d.offPeakTokens ?? 0),
        itemStyle: { borderColor: CARD_SURFACE, borderWidth: 2 },
        tooltip: { valueFormatter: (v: unknown) => `${formatNumber(Number(v))} token` },
      },
      {
        name: '原价时段',
        type: 'bar' as const,
        stack: 'tokens',
        data: cost.trend.map((d) => Math.max(0, d.tokens - (d.offPeakTokens ?? 0))),
        // 顶段收 4px 圆角（堆叠柱的数据端），底段保持贴基线的直角。
        itemStyle: { borderColor: CARD_SURFACE, borderWidth: 2, borderRadius: [4, 4, 0, 0] as [number, number, number, number] },
        tooltip: { valueFormatter: (v: unknown) => `${formatNumber(Number(v))} token` },
      },
    ],
  };

  // ---------- 校准维度读数（narrativeSlo.calibration） ----------
  const slo = data?.narrativeSlo ?? null;
  // 🔴 遍历服务端**实际下发**的每一个键，不是前端写死的七项：新指标一上线就自动出现。
  // 排序取 SLO_HEADLINE 的登记顺序（可读顺序），未登记的排在后面、按名字定序。
  const sloOrder = Object.keys(SLO_HEADLINE);
  const sloRows = Object.entries(slo?.metrics ?? {})
    .map(([key, block]) => ({ key, block }))
    .sort((a, b) => {
      const ia = sloOrder.indexOf(a.key);
      const ib = sloOrder.indexOf(b.key);
      if (ia !== ib) return (ia < 0 ? 999 : ia) - (ib < 0 ? 999 : ib);
      return a.key.localeCompare(b.key);
    });

  const calibration = data?.narrativeSlo?.calibration ?? null;
  const identityDim = calibration?.dimensions?.identityShareBalance;
  const realmDim = calibration?.dimensions?.realmTierWorldQuality;

  const tickOther = data ? Math.max(0, data.ticks.total - data.ticks.done - data.ticks.failed) : 0;
  const tickPieOption = data && {
    tooltip: { trigger: 'item' as const },
    legend: { bottom: 0 },
    series: [
      {
        type: 'pie' as const,
        radius: ['40%', '68%'],
        data: [
          { name: '成功', value: data.ticks.done, itemStyle: { color: '#52c41a' } },
          { name: '失败', value: data.ticks.failed, itemStyle: { color: '#ff4d4f' } },
          { name: '其它', value: tickOther, itemStyle: { color: '#faad14' } },
        ],
      },
    ],
  };

  return (
    <div>
      <Space style={{ marginBottom: 16, width: '100%', justifyContent: 'space-between' }}>
        <Typography.Title level={4} style={{ margin: 0 }}>数据看板</Typography.Title>
        <Button onClick={load} loading={loading}>刷新</Button>
      </Space>

      {error && <ErrorAlert message={error} onRetry={load} />}

      {loading && !data ? (
        <div style={{ textAlign: 'center', marginTop: 80 }}>
          <Spin />
        </div>
      ) : (
        data && (
          <>
            <Row gutter={[16, 16]}>
              <Col xs={12} md={6}><StatCard><Statistic title="注册用户" value={data.users.total} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="封禁用户" value={data.users.banned} valueStyle={{ color: '#cf1322' }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="日报送达" value={data.dailyReports.total} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="日报打开率" value={formatPercent(data.dailyReports.openRate)} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="tick 成功率" value={formatPercent(data.ticks.successRate)} valueStyle={{ color: '#3f8600' }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="tick 失败数" value={data.ticks.failed} valueStyle={{ color: data.ticks.failed ? '#cf1322' : undefined }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="审核积压" value={data.auditBacklog} valueStyle={{ color: data.auditBacklog ? '#d46b08' : undefined }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="待处理工单" value={data.dataRequestsPending} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="活跃世界" value={data.worlds.active} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="熔断世界" value={data.worlds.fused} valueStyle={{ color: data.worlds.fused ? '#cf1322' : undefined }} /></StatCard></Col>
              <Col xs={12} md={6}><StatCard><Statistic title="风控事件" value={data.riskEvents} /></StatCard></Col>
            </Row>

            <Row gutter={[16, 16]} style={{ marginTop: 16 }}>
              <Col xs={24} lg={14}>
                <Card size="small" title="按世界 token 成本（Top 10）">
                  {data.tokenCostByWorld.length ? (
                    <ReactECharts option={tokenBarOption} style={{ height: 320 }} notMerge />
                  ) : (
                    <Empty description="暂无 tick 成本数据" />
                  )}
                </Card>
              </Col>
              <Col xs={24} lg={10}>
                <Card size="small" title="tick 状态分布">
                  {data.ticks.total ? (
                    <ReactECharts option={tickPieOption} style={{ height: 320 }} notMerge />
                  ) : (
                    <Empty description="暂无 tick 数据" />
                  )}
                </Card>
              </Col>
            </Row>
            <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>
              指标为服务端 SQL 聚合（注册数 / 日报打开率 / tick 成功率 / token 成本 / 审核积压等）；成本收入比在 P4b 收费后再引入。
            </Typography.Paragraph>

            {/* ---- 错峰调度（成本工程杠杆①，migration 0038 → cost.offPeak） ---- */}
            <Typography.Title level={5} style={{ margin: '24px 0 12px' }}>
              错峰调度
              {offPeak ? <Typography.Text type="secondary" style={{ fontWeight: 400, marginLeft: 8 }}>近 {offPeak.windowDays} 日（UTC 日界）</Typography.Text> : null}
            </Typography.Title>

            {!offPeak ? (
              <Card size="small"><Empty description="服务端未返回错峰数据（cost.offPeak）" /></Card>
            ) : !offPeakHasTicks ? (
              <Card size="small"><Empty description={`近 ${offPeak.windowDays} 日窗口内暂无 tick，错峰无从统计`} /></Card>
            ) : (
              <>
                <Row gutter={[16, 16]}>
                  {/* 比率字段是 0..1 小数，formatPercent 内部 ×100；null → 显示 —。 */}
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic title="错峰拍占比" value={formatPercent(offPeak.tickRatio)} />
                      <Typography.Text type="secondary">{formatNumber(offPeak.offPeakTicks)} / {formatNumber(offPeak.ticks)} 拍</Typography.Text>
                    </StatCard>
                  </Col>
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic title="错峰 token 占比" value={formatPercent(offPeak.tokenRatio)} />
                      <Typography.Text type="secondary">{formatNumber(offPeak.offPeakTokens)} / {formatNumber(offPeak.tokens)} token</Typography.Text>
                    </StatCard>
                  </Col>
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic
                        title="估算节省"
                        value={formatCny(offPeak.savedCny)}
                        valueStyle={{ color: offPeak.savedCents > 0 ? '#3f8600' : undefined }}
                      />
                      <Typography.Text type="secondary">原价 {formatCny(offPeak.nominalCny)} · 折让 {formatPercent(offPeak.savedRatio)}</Typography.Text>
                    </StatCard>
                  </Col>
                  <Col xs={12} md={6}>
                    <StatCard>
                      <Statistic title="平均延后时长" value={formatDuration(offPeak.avgDeferMs)} />
                      <Typography.Text type="secondary">
                        被延后 {formatNumber(offPeak.deferredTicks)} 拍 · 最长 {formatDuration(offPeak.deferMsMax || null)}
                      </Typography.Text>
                    </StatCard>
                  </Col>
                </Row>

                <Row gutter={[16, 16]} style={{ marginTop: 16 }}>
                  <Col xs={24} lg={14}>
                    <Card size="small" title={`逐日 token 构成（折扣时段 / 原价时段，近 ${offPeak.windowDays} 日）`}>
                      {offPeakActive ? (
                        <ReactECharts option={offPeakTrendOption} style={{ height: 320 }} notMerge />
                      ) : (
                        // 错峰默认关闭（MUSE_OFFPEAK_SCHEDULING）：全部落中性值，0% 是真实答案而非故障。
                        <Empty description={`近 ${offPeak.windowDays} 日内没有错峰拍：错峰调度默认关闭（MUSE_OFFPEAK_SCHEDULING），此时每拍按原价记账`} />
                      )}
                    </Card>
                  </Col>
                  <Col xs={24} lg={10}>
                    <Card size="small" title="名义档位分布">
                      <Table<OffPeakRatioBucket>
                        size="small"
                        rowKey={(r) => String(r.priceRatioPct)}
                        dataSource={offPeak.byRatio}
                        pagination={false}
                        locale={{ emptyText: <Empty description="窗口内无记账拍" /> }}
                        columns={[
                          {
                            title: '档位',
                            dataIndex: 'priceRatioPct',
                            key: 'pct',
                            // priceRatioPct 是百分数整数（100=原价），直接带 % 渲染；
                            // 绝不能走 formatPercent（那是给 0..1 小数用的）。
                            render: (pct: number) => (pct === 100 ? '原价（100%）' : `${pct}%`),
                          },
                          { title: '拍数', dataIndex: 'ticks', key: 'ticks', align: 'right', render: formatNumber },
                          { title: 'token', dataIndex: 'tokens', key: 'tokens', align: 'right', render: formatNumber },
                          { title: '估算节省', dataIndex: 'savedCny', key: 'saved', align: 'right', render: (v: number) => formatCny(v) },
                        ]}
                      />
                    </Card>
                  </Col>
                </Row>

                <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>
                  「估算节省」= Σ 按档位汇总的 token × (100 − 档位%) ÷ 100 × 单价（{cost?.centsPer1kTokens ?? '—'} 分 / 千 token）。
                  🔴 档位是<strong>运营配置的名义折扣</strong>，不是供应商账单结算价，仅用于估算错峰收益，<strong>不得当对账依据</strong>；
                  上方今日成本 / 趋势 / 每局成本一律按原价计，与此处折让不重复相减。
                  延后时长由进程内存态计时，server 重启会让在途延后账清零（方向为低估，不会虚报）。
                </Typography.Paragraph>
              </>
            )}

            {/* ---- 叙事质量 SLO（narrativeSlo.metrics，VALIDATION §4.2 八项表） ---- */}
            <Typography.Title level={5} style={{ margin: '24px 0 12px' }}>
              叙事质量 SLO
              {slo?.windowDays ? (
                <Typography.Text type="secondary" style={{ fontWeight: 400, marginLeft: 8 }}>
                  滚动窗口 {slo.windowDays} 日（强制收尾率与二次入世率是全生命周期口径，不切窗口）
                </Typography.Text>
              ) : null}
            </Typography.Title>

            {!slo || slo.status !== 'ok' ? (
              <Card size="small">
                <Empty description={slo ? `本次未计算叙事质量 SLO：${slo.status}` : '服务端未返回叙事质量 SLO（narrativeSlo）'} />
              </Card>
            ) : sloRows.length === 0 ? (
              <Card size="small"><Empty description="服务端返回了 SLO 段但一项指标都没有" /></Card>
            ) : (
              <Card size="small">
                <Table
                  size="small"
                  rowKey="key"
                  pagination={false}
                  dataSource={sloRows}
                  columns={[
                    {
                      title: '指标',
                      dataIndex: 'key',
                      render: (_: unknown, r: { key: string; block: SloMetricBlock }) => (
                        <Space size={4} wrap>
                          {/* 标题由服务端给：前端不另维护一张名字表，改口径时不会两边打架。 */}
                          <span>{r.block.title ?? r.key}</span>
                          {!SLO_HEADLINE[r.key] && (
                            <Tooltip title="服务端新上线的指标，前端还没给它配头条字段与样本口径；这里按通用形态显示，不会漏掉它。">
                              <Tag>新指标</Tag>
                            </Tooltip>
                          )}
                        </Space>
                      ),
                    },
                    {
                      title: '读数',
                      dataIndex: 'value',
                      width: 200,
                      render: (_: unknown, r: { key: string; block: SloMetricBlock }) => {
                        // 🔴 status ≠ ok 一律不画数字（见 SloMetricBlock 注释）。
                        if (r.block.status !== 'ok') {
                          const label = SLO_STATUS_LABEL[r.block.status] ?? r.block.status;
                          const why = r.block.reason ?? r.block.notes?.[0];
                          const cell = (
                            <Space size={4}>
                              <span>—</span>
                              <Typography.Text type="secondary" style={{ fontSize: 12 }}>{label}</Typography.Text>
                            </Space>
                          );
                          return why ? <Tooltip title={why}>{cell}</Tooltip> : cell;
                        }
                        const spec = SLO_HEADLINE[r.key];
                        const v = sloNum(r.block, spec?.field ?? 'value');
                        return (
                          <Space size={4} wrap>
                            <strong>{readingText(v, spec?.kind ?? 'percent')}</strong>
                            {spec?.label && (
                              <Typography.Text type="secondary" style={{ fontSize: 12 }}>{spec.label}</Typography.Text>
                            )}
                          </Space>
                        );
                      },
                    },
                    {
                      title: '样本',
                      dataIndex: 'sample',
                      width: 180,
                      render: (_: unknown, r: { key: string; block: SloMetricBlock }) => {
                        const spec = SLO_HEADLINE[r.key];
                        const n = spec?.sample ? sloNum(r.block, spec.sample) : null;
                        // 样本量永远画出来（同 ReadingCell 的纪律）：一个比例压在几个观察上，必须看得见。
                        return n == null ? <span>—</span> : (
                          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                            {formatNumber(n)} {spec?.unit}
                          </Typography.Text>
                        );
                      },
                    },
                    {
                      title: '门槛',
                      dataIndex: 'threshold',
                      width: 120,
                      render: (_: unknown, r: { key: string; block: SloMetricBlock }) => {
                        const t = sloNum(r.block, 'threshold') ?? sloNum(r.block, 'thresholdMax');
                        if (t == null) return <span>—</span>;
                        const over = r.block.overThreshold === true;
                        // 🔴 只标「越线了」这个事实，不作判断、不给结论（同 ReadingCell）。
                        return (
                          <Space size={4}>
                            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                              {r.key === 'silentStreak' ? `${formatNumber(t)} 拍` : readingText(t, 'ratio')}
                            </Typography.Text>
                            {over && <Tag color="warning" style={{ marginInlineEnd: 0 }}>越线</Tag>}
                          </Space>
                        );
                      },
                    },
                  ]}
                />
                <Typography.Paragraph type="secondary" style={{ margin: '12px 0 0' }}>
                  🔴 <strong>「—」和「0%」是两句完全不同的话</strong>：前者是「没测过」，后者是测出来的真数。
                  零样本、无数据源、超上限一律显示 —，把鼠标停在上面能看到服务端给的原因。
                  本段全部为<strong>只读观测</strong>，不写库、不回灌引擎。
                </Typography.Paragraph>
              </Card>
            )}

            {/* ---- 校准维度读数（narrativeSlo.calibration，只读观测，绝不回灌引擎） ---- */}
            <Typography.Title level={5} style={{ margin: '24px 0 12px' }}>
              校准维度读数
              {calibration?.windowDays ? (
                <Typography.Text type="secondary" style={{ fontWeight: 400, marginLeft: 8 }}>
                  近 {calibration.windowDays} 日开出的世界（cohort）· 已扫描 {formatNumber(calibration.worldsScanned)} 个
                  {calibration.sampleFloor ? ` · 最小样本量 minN=${calibration.sampleFloor.minN}` : ''}
                </Typography.Text>
              ) : null}
            </Typography.Title>

            {!calibration ? (
              <Card size="small"><Empty description="服务端未返回校准读数（narrativeSlo.calibration）" /></Card>
            ) : calibration.status !== 'ok' ? (
              // skipped_by_request / skipped_too_large：明说跳过，不给残缺数。
              <Card size="small"><Empty description={`本次未计算校准读数：${calibration.status}`} /></Card>
            ) : (
              <Row gutter={[16, 16]}>
                <Col xs={24} lg={12}>
                  <Card size="small" title="身份维：身份分配 × 戏份分布（组内分布）">
                    {identityDim?.status !== 'ok' ? (
                      <DimensionEmpty status={identityDim?.status ?? 'no_data_in_window'} notes={identityDim?.notes} />
                    ) : (
                      <>
                        <Space wrap style={{ marginBottom: 12 }}>
                          <Typography.Text type="secondary">
                            观察 {formatNumber(identityDim.observations)} · 世界 {formatNumber(identityDim.worldsCounted)} · 身份 {formatNumber(identityDim.identitiesObserved)}
                          </Typography.Text>
                          <span>
                            各身份均值之间的集中度：<ReadingCell r={identityDim.meanShareGini} kind="ratio" notes={calibration.ciNotes} />
                          </span>
                        </Space>
                        <Table<IdentityBucket>
                          size="small"
                          rowKey={(r) => r.identityId}
                          dataSource={identityDim.byIdentity ?? []}
                          pagination={false}
                          locale={{ emptyText: <Empty description="窗口内没有身份分桶" /> }}
                          columns={[
                            { title: '身份', dataIndex: 'identityId', key: 'id' },
                            { title: '观察数', dataIndex: 'observations', key: 'obs', align: 'right', render: formatNumber },
                            {
                              title: '相对均分倍率',
                              key: 'mean',
                              // 1.0 = 恰好拿到均分；这是倍率不是百分比，不能走 formatPercent。
                              render: (_: unknown, r: IdentityBucket) => <ReadingCell r={r.meanRelativeShare} kind="ratio" notes={calibration.ciNotes} />,
                            },
                            {
                              title: '零分率',
                              key: 'zero',
                              render: (_: unknown, r: IdentityBucket) => <ReadingCell r={r.zeroScoreRate} kind="percent" notes={calibration.ciNotes} />,
                            },
                          ]}
                        />
                      </>
                    )}
                  </Card>
                </Col>

                <Col xs={24} lg={12}>
                  <Card size="small" title="戏服维：境界档 × 世界质量（跨世界对比）">
                    {realmDim?.status !== 'ok' ? (
                      <DimensionEmpty status={realmDim?.status ?? 'no_data_in_window'} notes={realmDim?.notes} />
                    ) : (
                      <>
                        <Typography.Paragraph type="secondary" style={{ marginBottom: 12 }}>
                          戏服 {formatNumber(realmDim.tiersObserved)} 档 · 其中样本量达标 {formatNumber(realmDim.tiersWithSufficientSample)} 档
                          {(realmDim.tiersWithSufficientSample ?? 0) < 2
                            ? '：还不足 2 档，「跨戏服对比」这件事本身尚不成立，下表只是各桶各自的事实。'
                            : '。'}
                        </Typography.Paragraph>
                        <Table<RealmBucket>
                          size="small"
                          rowKey={(r) => r.tierId}
                          dataSource={realmDim.byRealmTier ?? []}
                          pagination={false}
                          locale={{ emptyText: <Empty description="窗口内没有戏服分桶" /> }}
                          columns={[
                            { title: '戏服', dataIndex: 'tierId', key: 'id' },
                            { title: '世界', dataIndex: 'worlds', key: 'worlds', align: 'right', render: formatNumber },
                            {
                              // 🔴 三个比率的 n 各不相同（世界 / 拍 / 事件），故各自渲染各自的 n，
                              // 不共用上面那一列「世界」。混着读会把最不可信的数当成最可信的。
                              title: '完读率',
                              key: 'completion',
                              render: (_: unknown, r: RealmBucket) => <ReadingCell r={r.completion?.completionRate} kind="percent" notes={calibration.ciNotes} />,
                            },
                            {
                              title: '阻断率',
                              key: 'blocked',
                              render: (_: unknown, r: RealmBucket) => <ReadingCell r={r.blocking?.blockedRate} kind="percent" notes={calibration.ciNotes} />,
                            },
                            {
                              title: '安全扣留率',
                              key: 'withheld',
                              render: (_: unknown, r: RealmBucket) => <ReadingCell r={r.blocking?.withheldRate} kind="percent" notes={calibration.ciNotes} />,
                            },
                          ]}
                        />
                      </>
                    )}
                  </Card>
                </Col>
              </Row>
            )}

            <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>
              读数<strong>只读</strong>：不回灌引擎、不进世界状态、不改任何判定（§0.1 平权红线）。
              每个数随身带 <strong>n</strong>（观察数）与 <strong>minN</strong>（最小样本量）：
              n 低于 minN 的读数显示「样本不足」而<strong>不给数</strong>——那既不是 0，也不是「很均衡」，
              是<strong>还不能据此调参</strong>；分母真为 0 的显示 <strong>—</strong>（零样本），
              这一维从没配置过的整块显示为空态。三者是三件不同的事。
              比例类读数附 95% 置信区间（Wilson）：<strong>区间有多宽，这个数就有多不确定</strong>；
              基尼与均值类不给区间，理由随数下发（鼠标悬停「样本不足」可见）。
              🔴 本区<strong>不给综合评分、不给「配得对不对」的判语、也不给「差异是否显著」的结论</strong>——
              给事实与不确定性，判断由人来做。读数建成 ≠ 校准闭环已验证。
            </Typography.Paragraph>
          </>
        )
      )}

      {/* 运营趋势（GET /admin/metrics/trends）：紧随 overview 之后，独立加载 / 错误 / 重试。 */}
      <Space style={{ margin: '24px 0 12px', width: '100%', justifyContent: 'space-between' }}>
        <Typography.Title level={5} style={{ margin: 0 }}>运营趋势</Typography.Title>
        <Segmented
          value={trendDays}
          onChange={(v) => setTrendDays(Number(v))}
          options={TREND_DAY_OPTIONS}
        />
      </Space>

      {trendError && <ErrorAlert message={trendError} onRetry={loadTrends} />}

      {trendLoading && !trend ? (
        <div style={{ textAlign: 'center', margin: '48px 0' }}>
          <Spin />
        </div>
      ) : (
        trend && (
          <Row gutter={[16, 16]}>
            <Col xs={24} lg={12}>
              <Card size="small" title="规模（新增用户 / 活跃世界 / 世界事件 / 礼物量）">
                {trend.length ? (
                  <ReactECharts option={scaleTrendOption} style={{ height: 320 }} notMerge />
                ) : (
                  <Empty description="暂无趋势数据" />
                )}
              </Card>
            </Col>
            <Col xs={24} lg={12}>
              <Card size="small" title="消耗与收入（token 消耗 / 收入·元）">
                {trend.length ? (
                  <ReactECharts option={costTrendOption} style={{ height: 320 }} notMerge />
                ) : (
                  <Empty description="暂无趋势数据" />
                )}
              </Card>
            </Col>
          </Row>
        )
      )}
    </div>
  );
}
