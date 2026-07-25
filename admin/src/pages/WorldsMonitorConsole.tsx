// 世界运行监控指挥台。规格来源：docs/design/admin-ui-design.md（§4 布局 / §5 视觉 / §6 监控 / §7 诊断栏）。
//
// 数据纪律（docs/VALIDATION.md §0 状态语言）：
//   正式模式（designPreview=false）只渲染 /admin/worlds 与 /admin/worlds/{id}/diagnostics 真实返回的字段；
//   后端没有的字段一律显示 —／空态，并就地留 `TODO(接口缺字段)` 注释，禁止用常量冒充真实能力。
//   design=preview 仅在开发环境注入样本数据用于视觉验收，不作为接口已稳定的证据。
import { useCallback, useEffect, useMemo, useState } from 'react';
import { AlertOutlined, CalendarOutlined, ReloadOutlined } from '@ant-design/icons';
import { App as AntdApp, Button, Progress, Segmented, Select, Spin, Tag } from 'antd';
import ReactECharts from 'echarts-for-react';
import { useLocation, useNavigate } from 'react-router-dom';
import { adminFetch } from '../api';
import { ErrorAlert, formatPercent, formatTime, friendlyError, ReasonModal, usePagedList } from '../components/shared';
import './WorldsMonitorConsole.css';

interface WorldApiRow {
  id: string;
  title: string;
  roomType: string;
  status: string;
  visibility: string;
  memberLimit: number;
  tickPerDay: number;
  engineVersion: string;
  promptSetVersion: string;
  modelRouteVersion: string;
  stateRevision: number;
  spentTokensToday: number;
  dailyTokenBudget: number;
  fused: boolean;
  createdAt: number;
  // ---- 成本仪表列（server/src/admin_api/worlds_ops.rs::list_worlds）----
  /** active 成员数；无成员即 0（0 是真实答案，不是缺数据）。 */
  participantCount: number | null;
  /** 已终结 tick 中 done 的占比，**0..1 小数**（不是百分数）；无已终结 tick → null，显示 — 而非 0%。 */
  successRate: number | null;
  /** 今日（UTC 日界）tick 消耗 token。 */
  todayTokens: number | null;
  /** 今日成本：cents 为账面整数分，cny 为展示派生值。 */
  todayCostCents: number | null;
  todayCostCny: number | null;
}

interface TickMeta {
  tickNo: number;
  status: string;
  error: string | null;
  costTokens: number;
  startedAt: number | null;
  finishedAt: number | null;
  createdAt: number;
}

/**
 * 世界预算/熔断态（diagnostics.budget）。
 * 金额账面口径是整数分（`*Cents`），`*Cny` 为展示派生；用量比一律 0..1 小数。
 * 跨日陈旧计数器由后端归零（`spentTokensTodayEffective`），前端不再自行判断日界。
 */
interface DiagnosticsBudget {
  dailyTokenBudget: number;
  dailyCnyBudgetCents: number;
  /** 今日金额上限（元）；<=0 表示未设金额上限，此时 cnyUsageRatio 为 null。 */
  dailyCnyBudget: number;
  /** 库中原始计数器（可能属于过去某天，展示一律用 Effective 版本）。 */
  spentTokensToday: number;
  spentTokensTodayEffective: number;
  spentCnyCents: number;
  spentCny: number;
  centsPer1kTokens: number;
  budgetDay: string;
  budgetDayIsToday: boolean;
  /** 三个用量比均为 0..1 小数；对应维度未设上限时为 null。usageRatio 取 token 与金额两维的较大者。 */
  tokenUsageRatio: number | null;
  cnyUsageRatio: number | null;
  usageRatio: number | null;
  fused: boolean;
}

interface Diagnostics {
  world: Record<string, unknown> & { id: string; title: string; status: string };
  ticks: TickMeta[];
  budget: DiagnosticsBudget | null;
  riskEventCounts: { kind: string; count: number }[];
  eventStats: { total: number; byModeration: { moderation: string; count: number }[] };
  redactionNote: string;
}

/** 平台成本仪表（GET /admin/metrics/overview 的顶层 `cost`）。 */
interface CostOverview {
  centsPer1kTokens: number;
  today: { day: string; tokens: number; cents: number; cny: number };
  trendDays: number;
  trend: { day: string; tokens: number; cents: number; cny: number }[];
}

interface ConsoleWorld extends WorldApiRow {
  thumbnail: string;
  roomTypeLabel: string;
  /** 以下两项接口暂未提供，正式模式恒 null → 表格显示空态。 */
  moderationLatency: number | null;
  /** 最后活动时间（毫秒）；接口暂未提供，正式模式恒 null。 */
  lastActivityAt: number | null;
}

interface TimelineEvent {
  key: string;
  at: number;
  level: 'info' | 'warning' | 'danger';
  levelLabel: string;
  title: string;
  detail: string;
  source: string;
  ref: string;
}

const EMPTY = '—';

const ROOM_TYPE_TEXT: Record<string, string> = {
  idle: '开放世界',
  chapter: '章节房',
  arena: '赛事房',
  exploration: '探索房',
  social: '社交房',
  quest: '任务房',
  story: '剧情房',
  side: '副本房',
};

const STATUS_TEXT: Record<string, string> = {
  open: '运行中',
  running: '运行中',
  attention: '需关注',
  paused: '已暂停',
  fused: '已熔断',
  ended: '已结束',
};

/** tick 错误码 → 中文说明；未收录的错误码原样透出，不臆造语义。 */
const TICK_ERROR_TEXT: Record<string, string> = {
  moderation_latency_high: '风控审核延迟超过阈值',
  budget_exhausted: '今日预算已耗尽',
  model_error: '模型调用失败',
  timeout: '引擎执行超时',
  fused: '世界已熔断，tick 停止调度',
};

/**
 * 世界封面占位图。
 * TODO(接口缺字段): coverUrl —— /admin/worlds 未返回封面，正式模式按 world.id 哈希确定性取图
 *（同一世界恒定同一张，既不全站同图也不随机）。接口补齐后直接改读 coverUrl。
 */
const WORLD_THUMBNAILS = [
  '/assets/worlds/mist-sea-world.png',
  '/assets/worlds/still-mountains.png',
  '/assets/worlds/ember-tavern.png',
  '/assets/worlds/mechanical-city.png',
  '/assets/worlds/desert-journey.png',
  '/assets/worlds/evernight-realm.png',
];

/**
 * 延迟告警阈值（秒）。设计文档 §7.2 要求延迟图渲染告警阈值。
 * 这是前端展示分档，不是产品规则；服务端下发阈值后应改读接口（平台三约束②：规则参数化）。
 */
const LATENCY_ALERT_THRESHOLD_S = 1.5;

/**
 * 「需关注」判定：今日 token 预算消耗比例达到该值且未熔断。
 * 同上，属前端展示分档；TODO(接口缺字段): healthState —— 世界健康档位应由 server 统一裁定后下发。
 */
const ATTENTION_BUDGET_RATIO = 0.9;

/**
 * 表格「成功率」转告警色的分档（0..1 小数，与接口同单位）。
 * 同为前端展示分档，不是产品规则；服务端下发阈值后应改读接口。
 */
const SUCCESS_RATE_WARN_RATIO = 0.92;

/** 时间线一屏最多渲染的事件条数，超出只提示总数，避免长窗口把主区拉成长滚动条。 */
const TIMELINE_MAX_ROWS = 10;

interface RangeOption {
  value: string;
  label: string;
  windowMs: number;
  windowLabel: string;
}

/** 时间范围（设计文档 §6.2 核心操作）：决定延迟曲线与事件时间线的取数窗口。 */
const RANGE_OPTIONS: RangeOption[] = [
  { value: 'live', label: '实时', windowMs: 30 * 60 * 1000, windowLabel: '最近 30 分钟' },
  { value: '1h', label: '1小时', windowMs: 60 * 60 * 1000, windowLabel: '最近 1 小时' },
  { value: '6h', label: '6小时', windowMs: 6 * 60 * 60 * 1000, windowLabel: '最近 6 小时' },
  { value: '24h', label: '24小时', windowMs: 24 * 60 * 60 * 1000, windowLabel: '最近 24 小时' },
  { value: '7d', label: '7天', windowMs: 7 * 24 * 60 * 60 * 1000, windowLabel: '最近 7 天' },
];

/**
 * 可下推服务端的状态筛选（对应 worlds.status 列）。
 * running 覆盖 open/running 两个库值、attention/fused 为前端派生态，三者只能本地过滤。
 */
const SERVER_STATUS_PARAM: Record<string, string> = { paused: 'paused', ended: 'ended' };

const STATUS_FILTER_OPTIONS = [
  { value: 'running', label: '运行中' },
  { value: 'attention', label: '需关注' },
  { value: 'paused', label: '已暂停' },
  { value: 'fused', label: '已熔断' },
  { value: 'ended', label: '已结束' },
];

// ---------------- 预览样本（仅 design=preview） ----------------

// 注意：样本必须与接口同单位（successRate 为 0..1 小数、成本以分为账面口径），
// 否则预览会验收出一条正式模式根本不存在的渲染路径。
const PREVIEW_WORLDS: ConsoleWorld[] = [
  {
    id: 'world_1001', title: '雾海纪元', roomType: 'idle', roomTypeLabel: '开放世界', status: 'running', visibility: 'official',
    memberLimit: 2000, participantCount: 1248, tickPerDay: 4, engineVersion: 'v3.8.2', promptSetVersion: 'v2.13.0',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 1842391, spentTokensToday: 642000, dailyTokenBudget: 1000000,
    fused: false, createdAt: 1753392753000, successRate: 0.962, todayTokens: 642000, todayCostCents: 12864,
    todayCostCny: 128.64, moderationLatency: 1.6,
    lastActivityAt: 1753428000000, thumbnail: '/assets/worlds/mist-sea-world.png',
  },
  {
    id: 'world_1002', title: '静止山脉', roomType: 'exploration', roomTypeLabel: '探索房', status: 'running', visibility: 'public',
    memberLimit: 1000, participantCount: 612, tickPerDay: 3, engineVersion: 'v3.8.2', promptSetVersion: 'v2.12.6',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 812104, spentTokensToday: 483000, dailyTokenBudget: 800000,
    fused: false, createdAt: 1753392100000, successRate: 0.978, todayTokens: 483000, todayCostCents: 9671,
    todayCostCny: 96.71, moderationLatency: 1.2,
    lastActivityAt: 1753427880000, thumbnail: '/assets/worlds/still-mountains.png',
  },
  {
    id: 'world_1003', title: '星火酒馆', roomType: 'social', roomTypeLabel: '社交房', status: 'attention', visibility: 'public',
    memberLimit: 600, participantCount: 386, tickPerDay: 6, engineVersion: 'v3.8.1', promptSetVersion: 'v2.12.8',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 302768, spentTokensToday: 321000, dailyTokenBudget: 500000,
    fused: false, createdAt: 1753391800000, successRate: 0.906, todayTokens: 321000, todayCostCents: 6433,
    todayCostCny: 64.33, moderationLatency: 3.8,
    lastActivityAt: 1753427940000, thumbnail: '/assets/worlds/ember-tavern.png',
  },
  {
    id: 'world_1004', title: '机械之城', roomType: 'quest', roomTypeLabel: '任务房', status: 'running', visibility: 'official',
    memberLimit: 1400, participantCount: 932, tickPerDay: 4, engineVersion: 'v3.8.2', promptSetVersion: 'v2.13.0',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 1231009, spentTokensToday: 1051000, dailyTokenBudget: 1400000,
    fused: false, createdAt: 1753391600000, successRate: 0.951, todayTokens: 1051000, todayCostCents: 21038,
    todayCostCny: 210.38, moderationLatency: 1.4,
    lastActivityAt: 1753428000000, thumbnail: '/assets/worlds/mechanical-city.png',
  },
  {
    id: 'world_1005', title: '沙海旅途', roomType: 'story', roomTypeLabel: '剧情房', status: 'attention', visibility: 'public',
    memberLimit: 500, participantCount: 274, tickPerDay: 3, engineVersion: 'v3.8.1', promptSetVersion: 'v2.12.6',
    modelRouteVersion: 'qwen-max-backup', stateRevision: 213557, spentTokensToday: 210000, dailyTokenBudget: 400000,
    fused: false, createdAt: 1753391300000, successRate: 0.883, todayTokens: 210000, todayCostCents: 4219,
    todayCostCny: 42.19, moderationLatency: 4.5,
    lastActivityAt: 1753427820000, thumbnail: '/assets/worlds/desert-journey.png',
  },
  {
    id: 'world_1006', title: '永夜之境', roomType: 'side', roomTypeLabel: '副本房', status: 'fused', visibility: 'official',
    memberLimit: 400, participantCount: 0, tickPerDay: 2, engineVersion: 'v3.8.0', promptSetVersion: 'v2.11.9',
    modelRouteVersion: 'qwen-max-backup', stateRevision: 87662, spentTokensToday: 0, dailyTokenBudget: 300000,
    fused: true, createdAt: 1753390900000, successRate: null, todayTokens: 0, todayCostCents: 0,
    todayCostCny: 0, moderationLatency: null,
    lastActivityAt: 1753426980000, thumbnail: '/assets/worlds/evernight-realm.png',
  },
];

/** 预览样本：平台健康指标 + 成本趋势（对齐验收参考图，仅 design=preview 使用）。 */
const PREVIEW_HEALTH = { running: 38, attention: 4, fused: 1, costCny: 1284 };
const PREVIEW_COST_SPARK = [1080, 1190, 1135, 1270, 1205, 1350, 1284];
const PREVIEW_PROMPT_EDITOR = { editor: '林逸', updatedAt: '12:02:11' };
const PREVIEW_BACKUP_ROUTE = { name: 'qwen-max（备）', errorRate: '错误率 2.1%' };

function hashOf(text: string): number {
  let hash = 7;
  for (let i = 0; i < text.length; i += 1) hash = (hash * 33 + text.charCodeAt(i)) >>> 0;
  return hash;
}

function thumbnailFor(id: string): string {
  return WORLD_THUMBNAILS[hashOf(id) % WORLD_THUMBNAILS.length];
}

/**
 * 预览样本诊断：按当前时间窗口生成 24 个 tick（随选中世界与时间范围变化），
 * 使「时间范围切换」「时间线跟随选中世界」在预览模式同样可见。仅 design=preview 调用。
 */
function makePreviewDiagnostics(world: ConsoleWorld, windowMs: number): Diagnostics {
  const now = Date.now();
  const previewCnyBudgetCents = 20000;
  const previewSpentCents = world.todayCostCents ?? 0;
  const previewTokenRatio = world.dailyTokenBudget > 0 ? world.spentTokensToday / world.dailyTokenBudget : null;
  const previewCnyRatio = previewSpentCents / previewCnyBudgetCents;
  const step = Math.max(1000, Math.round(windowMs / 24));
  const seed = hashOf(world.id);
  const failedIndex = world.status === 'running' ? 17 + (seed % 5) : 20;
  const ticks: TickMeta[] = Array.from({ length: 24 }, (_, index) => {
    const startedAt = now - windowMs + index * step;
    const failed = index === failedIndex;
    const duration = failed ? 2100 + (seed % 400) : 900 + ((seed >>> (index % 9)) % 620);
    return {
      tickNo: 1842368 + index,
      status: failed ? 'failed' : 'done',
      error: failed ? 'moderation_latency_high' : null,
      costTokens: 1100 + index * 17,
      startedAt,
      finishedAt: startedAt + duration,
      createdAt: startedAt,
    };
  });
  return {
    // 接口按 tick_no DESC 返回，预览保持同一顺序，避免前端只在预览下"恰好正序"。
    ticks: ticks.reverse(),
    world: {
      id: world.id,
      title: world.title,
      status: world.status,
      roomType: world.roomType,
      modelRouteVersion: world.modelRouteVersion,
      promptSetVersion: world.promptSetVersion,
      startedAt: '2026-07-25 08:12:33',
    },
    budget: {
      dailyTokenBudget: world.dailyTokenBudget,
      dailyCnyBudgetCents: previewCnyBudgetCents,
      dailyCnyBudget: previewCnyBudgetCents / 100,
      spentTokensToday: world.spentTokensToday,
      spentTokensTodayEffective: world.spentTokensToday,
      spentCnyCents: previewSpentCents,
      spentCny: previewSpentCents / 100,
      centsPer1kTokens: 20,
      budgetDay: '2026-07-25',
      budgetDayIsToday: true,
      tokenUsageRatio: previewTokenRatio,
      cnyUsageRatio: previewCnyRatio,
      usageRatio: previewTokenRatio == null ? previewCnyRatio : Math.max(previewTokenRatio, previewCnyRatio),
      fused: world.fused,
    },
    riskEventCounts: [{ kind: 'prompt_injection', count: 1 }, { kind: 'moderation_delay', count: 1 }],
    eventStats: { total: 327, byModeration: [{ moderation: 'approved', count: 324 }, { moderation: 'pending', count: 3 }] },
    redactionNote: '诊断信息已脱敏，不展示用户私密内容与模型链式推理。',
  };
}

// ---------------- 数据映射 ----------------

function budgetRatio(row: { spentTokensToday: number; dailyTokenBudget: number }): number {
  if (!row.dailyTokenBudget || row.dailyTokenBudget <= 0) return 0;
  return row.spentTokensToday / row.dailyTokenBudget;
}

/** 表格展示态：熔断 > 库状态（paused/ended）> 预算逼近上限（需关注）> 运行中。 */
function deriveStatus(row: WorldApiRow): string {
  if (row.fused) return 'fused';
  if (row.status === 'paused' || row.status === 'ended') return row.status;
  if (budgetRatio(row) >= ATTENTION_BUDGET_RATIO) return 'attention';
  return row.status;
}

function apiRowToConsole(row: WorldApiRow): ConsoleWorld {
  return {
    ...row,
    roomTypeLabel: ROOM_TYPE_TEXT[row.roomType] ?? row.roomType,
    // participantCount / successRate / todayTokens / todayCost* 由 /admin/worlds 直接下发（成本仪表），
    // 无需在此兜底：缺失（旧版 server）时为 undefined，各渲染点按 == null 走空态。
    // TODO(接口缺字段): moderationLatency —— 全仓没有任何一处记录**机审调用耗时**：
    // ModerationProvider 调用不打点，risk_events / world_events 无耗时列；audit_queue 的
    // created_at/reviewed_at 是**人审周转**（小时/天量级），与此处按秒渲染、>3s 报警的语义完全不同，
    // 不得拿来充数。补齐路径见 server/src/admin_api/worlds_ops.rs::list_worlds 同名注释。
    moderationLatency: null,
    // TODO(接口缺字段): lastActivityAt —— worlds.updated_at 已存在但未投影；createdAt ≠ 最后活动，不可顶替。
    lastActivityAt: null,
    thumbnail: thumbnailFor(row.id),
    status: deriveStatus(row),
  };
}

function formatCompact(value: number | null | undefined): string {
  if (value == null) return EMPTY;
  return value.toLocaleString('zh-CN');
}

/** 金额（元）→ `¥1,234.56`；null 走空态。账面口径是整数分，这里只做展示格式化。 */
function formatCny(value: number | null | undefined): string {
  if (value == null) return EMPTY;
  return `¥${value.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

/** 0..1 用量比 → 进度条百分比（整数，封顶 100）；无上限（null）时返回 null，不得回退成 0%。 */
function ratioToPercent(ratio: number | null | undefined): number | null {
  if (ratio == null || !Number.isFinite(ratio)) return null;
  return Math.min(100, Math.round(ratio * 100));
}

function statusClass(status: string): string {
  if (status === 'attention') return 'is-attention';
  if (status === 'fused' || status === 'ended') return 'is-danger';
  if (status === 'paused') return 'is-paused';
  return 'is-running';
}

/** 窗口 ≤24 小时显示时分秒，跨天窗口补月日。 */
function formatEventTime(at: number, windowMs: number): string {
  const date = new Date(at);
  const clock = date.toLocaleTimeString('zh-CN', { hour12: false });
  if (windowMs <= 24 * 60 * 60 * 1000) return clock;
  return `${date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' })} ${clock.slice(0, 5)}`;
}

function formatAxisTime(at: number, windowMs: number): string {
  const date = new Date(at);
  const hh = String(date.getHours()).padStart(2, '0');
  const mm = String(date.getMinutes()).padStart(2, '0');
  if (windowMs <= 24 * 60 * 60 * 1000) return `${hh}:${mm}`;
  return `${date.getMonth() + 1}-${date.getDate()} ${hh}:${mm}`;
}

/** tick 元数据 → 事件时间线（唯一真实来源：diagnostics.ticks，随选中世界变化）。 */
function ticksToTimeline(ticks: TickMeta[], windowMs: number, worldTitle: string): TimelineEvent[] {
  const since = Date.now() - windowMs;
  return ticks
    .filter((tick) => (tick.finishedAt ?? tick.createdAt) >= since)
    .map((tick) => {
      const at = tick.finishedAt ?? tick.createdAt;
      const seconds = tick.startedAt != null && tick.finishedAt != null
        ? (tick.finishedAt - tick.startedAt) / 1000
        : null;
      if (tick.status === 'failed') {
        const code = tick.error ?? 'unknown';
        return {
          key: `tick-${tick.tickNo}`,
          at,
          level: 'danger' as const,
          levelLabel: '报警',
          title: `Tick #${tick.tickNo} 执行失败`,
          detail: `${TICK_ERROR_TEXT[code] ?? '错误码'}（${code}）`,
          source: '引擎调度',
          ref: worldTitle,
        };
      }
      if (seconds != null && seconds > LATENCY_ALERT_THRESHOLD_S) {
        return {
          key: `tick-${tick.tickNo}`,
          at,
          level: 'warning' as const,
          levelLabel: '警告',
          title: `Tick #${tick.tickNo} 延迟偏高`,
          detail: `耗时 ${seconds.toFixed(2)}s，超过告警阈值 ${LATENCY_ALERT_THRESHOLD_S}s`,
          source: '引擎调度',
          ref: worldTitle,
        };
      }
      return {
        key: `tick-${tick.tickNo}`,
        at,
        level: 'info' as const,
        levelLabel: '信息',
        title: `Tick #${tick.tickNo} ${tick.status === 'done' ? '执行完成' : `状态 ${tick.status}`}`,
        detail: `${seconds == null ? '耗时未记录' : `耗时 ${seconds.toFixed(2)}s`} · 消耗 ${formatCompact(tick.costTokens)} tokens`,
        source: '引擎调度',
        ref: worldTitle,
      };
    })
    .sort((a, b) => b.at - a.at);
}

export default function WorldsMonitorConsole() {
  const { message: messageApi } = AntdApp.useApp();
  const location = useLocation();
  const navigate = useNavigate();
  const designPreview = (import.meta as any).env?.DEV && new URLSearchParams(location.search).get('design') === 'preview';
  const [statusFilter, setStatusFilter] = useState<string | undefined>();
  const [range, setRange] = useState<string>('24h');
  const [levelFilter, setLevelFilter] = useState<'all' | 'abnormal'>('all');
  const [previewWorlds, setPreviewWorlds] = useState(PREVIEW_WORLDS);
  const [selectedId, setSelectedId] = useState(designPreview ? 'world_1001' : '');
  const [diag, setDiag] = useState<Diagnostics | null>(null);
  const [diagLoading, setDiagLoading] = useState(false);
  const [diagError, setDiagError] = useState<string | null>(null);
  const [diagReloadKey, setDiagReloadKey] = useState(0);
  const [cost, setCost] = useState<CostOverview | null>(null);
  const [costReloadKey, setCostReloadKey] = useState(0);
  const [action, setAction] = useState<{ row: ConsoleWorld; kind: 'pause' | 'resume' } | null>(null);
  const [acting, setActing] = useState(false);

  const rangeOption = RANGE_OPTIONS.find((item) => item.value === range) ?? RANGE_OPTIONS[3];

  const list = usePagedList<WorldApiRow>(async (cursor) => {
    const query = new URLSearchParams();
    const serverStatus = statusFilter ? SERVER_STATUS_PARAM[statusFilter] : undefined;
    if (serverStatus) query.set('status', serverStatus);
    if (cursor) query.set('cursor', cursor);
    query.set('limit', '20');
    const result = await adminFetch<{ worlds: WorldApiRow[]; nextCursor: string | null }>(`/admin/worlds?${query.toString()}`);
    return { items: result.worlds, nextCursor: result.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    if (!designPreview) reload();
  }, [designPreview, reload, statusFilter]);

  const allWorlds = useMemo(
    () => (designPreview ? previewWorlds : list.items.map(apiRowToConsole)),
    [designPreview, list.items, previewWorlds],
  );

  const worlds = useMemo(() => {
    if (!statusFilter) return allWorlds;
    return allWorlds.filter((world) => (
      world.status === statusFilter || (statusFilter === 'running' && world.status === 'open')
    ));
  }, [allWorlds, statusFilter]);

  const selected = worlds.find((world) => world.id === selectedId) ?? worlds[0] ?? null;

  useEffect(() => {
    if (worlds.length && !worlds.some((world) => world.id === selectedId)) {
      setSelectedId(worlds[0].id);
    }
  }, [selectedId, worlds]);

  // 正式模式：选中世界变化（含首次落位）即拉一次脱敏诊断；预览模式用样本诊断。
  useEffect(() => {
    if (designPreview || !selectedId) return undefined;
    let cancelled = false;
    setDiagLoading(true);
    setDiagError(null);
    adminFetch<Diagnostics>(`/admin/worlds/${selectedId}/diagnostics`)
      .then((result) => {
        if (!cancelled) setDiag(result);
      })
      .catch((error) => {
        if (cancelled) return;
        setDiag(null);
        setDiagError(friendlyError(error));
      })
      .finally(() => {
        if (!cancelled) setDiagLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [designPreview, selectedId, diagReloadKey]);

  // 平台成本仪表：健康指标条的「今日成本」与迷你趋势唯一数据源（近 7 日，末桶即今天）。
  // 取数失败不弹错、不阻断监控主流程——成本位退回空态即可，监控页的主职责是世界与 tick。
  useEffect(() => {
    if (designPreview) return undefined;
    let cancelled = false;
    adminFetch<{ cost?: CostOverview | null }>('/admin/metrics/overview')
      .then((result) => {
        if (!cancelled) setCost(result.cost ?? null);
      })
      .catch(() => {
        if (!cancelled) setCost(null);
      });
    return () => {
      cancelled = true;
    };
  }, [designPreview, costReloadKey]);

  const previewDiag = useMemo(
    () => (designPreview && selected ? makePreviewDiagnostics(selected, rangeOption.windowMs) : null),
    [designPreview, selected, rangeOption.windowMs],
  );
  const diagnostics = designPreview ? previewDiag : diag;

  const refreshAll = useCallback(() => {
    if (designPreview) return;
    reload();
    setDiagReloadKey((key) => key + 1);
    setCostReloadKey((key) => key + 1);
  }, [designPreview, reload]);

  const doAction = async (reason: string) => {
    if (!action) return;
    setActing(true);
    try {
      if (designPreview) {
        setPreviewWorlds((current) => current.map((world) => (
          world.id === action.row.id
            ? { ...world, status: action.kind === 'pause' ? 'paused' : 'running', fused: false }
            : world
        )));
      } else {
        await adminFetch(`/admin/worlds/${action.row.id}/${action.kind}?reason=${encodeURIComponent(reason)}`, 'POST');
        refreshAll();
      }
      messageApi.success(action.kind === 'pause' ? '世界已暂停' : '世界已恢复');
      setAction(null);
    } catch (error) {
      messageApi.error(friendlyError(error));
    } finally {
      setActing(false);
    }
  };

  // ---------- 时间范围驱动的取数窗口（设计文档 §6.2 / §7.2） ----------
  // 注：/admin/worlds/{id}/diagnostics 只返回最近 10 个 tick 且不接受时间窗参数，
  // TODO(接口缺字段): diagnostics 需支持 ?since=/?window= 与更大的 tick 采样，否则长窗口只能看到最近 10 次。
  const windowedTicks = useMemo(() => {
    const since = Date.now() - rangeOption.windowMs;
    return (diagnostics?.ticks ?? []).filter((tick) => (tick.finishedAt ?? tick.createdAt) >= since);
  }, [diagnostics, rangeOption.windowMs]);

  const latencyPoints = useMemo(() => (
    windowedTicks
      .filter((tick) => tick.startedAt != null && tick.finishedAt != null)
      .map((tick) => ({
        at: tick.finishedAt as number,
        seconds: Number((((tick.finishedAt as number) - (tick.startedAt as number)) / 1000).toFixed(2)),
      }))
      .sort((a, b) => a.at - b.at)
  ), [windowedTicks]);

  const currentLatency = latencyPoints.length ? latencyPoints[latencyPoints.length - 1].seconds : null;

  const latencyMax = useMemo(() => {
    const peak = latencyPoints.reduce((max, point) => Math.max(max, point.seconds), 0);
    return Math.max(LATENCY_ALERT_THRESHOLD_S * 2, Math.ceil(peak * 1.3 * 2) / 2);
  }, [latencyPoints]);

  const latencyOption = useMemo(() => ({
    animation: false,
    grid: { left: 35, right: 16, top: 14, bottom: 26 },
    xAxis: {
      type: 'category',
      data: latencyPoints.map((point) => formatAxisTime(point.at, rangeOption.windowMs)),
      boundaryGap: false,
      axisLine: { lineStyle: { color: '#e4ded6' } },
      axisTick: { show: false },
      axisLabel: { color: '#928b83', fontSize: 10, hideOverlap: true },
    },
    yAxis: {
      type: 'value',
      min: 0,
      max: latencyMax,
      axisLabel: { color: '#928b83', fontSize: 10, formatter: '{value}s' },
      splitLine: { lineStyle: { color: '#e4ded6' } },
    },
    tooltip: { trigger: 'axis', valueFormatter: (value: number) => `${value}s` },
    series: [{
      type: 'line',
      name: '执行延迟',
      data: latencyPoints.map((point) => point.seconds),
      smooth: 0.22,
      symbol: 'none',
      lineStyle: { color: '#5d8750', width: 1.6 },
      areaStyle: { color: 'rgba(93,135,80,0.05)' },
      // 设计文档 §7.2：延迟图必须渲染告警阈值。
      markLine: {
        silent: true,
        symbol: 'none',
        precision: 2,
        data: [{ yAxis: LATENCY_ALERT_THRESHOLD_S }],
        lineStyle: { color: '#c64d40', type: 'dashed', width: 1 },
        label: {
          formatter: `告警阈值 ${LATENCY_ALERT_THRESHOLD_S}s`,
          position: 'insideStartTop',
          color: '#c64d40',
          fontSize: 10,
        },
      },
      markArea: {
        silent: true,
        itemStyle: { color: 'rgba(198,77,64,0.05)' },
        data: [[{ yAxis: LATENCY_ALERT_THRESHOLD_S }, { yAxis: latencyMax }]],
      },
    }],
  }), [latencyPoints, latencyMax, rangeOption.windowMs]);

  // ---------- 平台今日成本与近 N 日趋势（设计文档 §6.1） ----------
  // 正式模式全部来自 /admin/metrics/overview 的 cost 对象（cny 已由 server 从整数分折算）。
  const costTodayCny = designPreview ? PREVIEW_HEALTH.costCny : cost?.today.cny ?? null;
  const costSpark = useMemo(
    () => (designPreview ? PREVIEW_COST_SPARK : (cost?.trend ?? []).map((point) => point.cny)),
    [designPreview, cost],
  );
  const costTrendDays = designPreview ? PREVIEW_COST_SPARK.length : cost?.trendDays ?? 0;
  // 单点画不出走势，只在有 ≥2 个日桶时才渲染 sparkline（否则退回单列布局）。
  const showCostSpark = costTodayCny != null && costSpark.length > 1;

  const costSparkOption = useMemo(() => ({
    animation: false,
    grid: { left: 2, right: 2, top: 4, bottom: 4 },
    xAxis: { type: 'category', show: false, data: costSpark.map((_, index) => index) },
    yAxis: { type: 'value', show: false },
    series: [{ type: 'line', data: costSpark, symbol: 'none', smooth: 0.25, lineStyle: { color: '#b18a72', width: 1.6 } }],
  }), [costSpark]);

  // ---------- 事件时间线（跟随选中世界，设计文档 §6.3） ----------
  const timelineEvents = useMemo(() => {
    if (!diagnostics || !selected) return [];
    return ticksToTimeline(diagnostics.ticks, rangeOption.windowMs, selected.title);
  }, [diagnostics, rangeOption.windowMs, selected]);

  const visibleEvents = useMemo(
    () => (levelFilter === 'abnormal' ? timelineEvents.filter((event) => event.level !== 'info') : timelineEvents),
    [levelFilter, timelineEvents],
  );

  const latestIncident = timelineEvents.find((event) => event.level !== 'info') ?? null;

  // ---------- 健康指标条（设计文档 §6.1） ----------
  // TODO(接口缺字段): 平台级聚合 —— 运行中/需关注/熔断三档现按"已加载的世界"统计（翻页未完时偏小）；
  // /admin/metrics/overview 的 worlds.active/fused 是另一套口径（库状态 + 熔断标记，无"需关注"档，
  // 且与本页 deriveStatus 的派生态不一致），不能直接顶替。需 server 提供与本页同口径的全量汇总。
  // 今日成本已改读 cost.today（见上方 costTodayCny），不再走这里的 token 合计。
  const health = useMemo(() => {
    let running = 0;
    let attention = 0;
    let fused = 0;
    let tokens = 0;
    for (const world of allWorlds) {
      if (world.status === 'fused') fused += 1;
      else if (world.status === 'attention') attention += 1;
      else if (world.status === 'running' || world.status === 'open') running += 1;
      tokens += world.spentTokensToday ?? 0;
    }
    return { running, attention, fused, tokens };
  }, [allWorlds]);

  const budget = diagnostics?.budget ?? null;
  // 进度取 server 的 usageRatio（token 与金额两维的较大者，即先触线的那条），两维都没设上限时为 null，
  // 此时不画进度条——0% 会被读成"今天几乎没花钱"，而真相是"根本没有上限可比"。
  const budgetPercent = ratioToPercent(budget?.usageRatio);
  const budgetRatioNote = budget
    ? [
      budget.cnyUsageRatio != null ? `金额 ${formatPercent(budget.cnyUsageRatio)}` : null,
      budget.tokenUsageRatio != null ? `Token ${formatPercent(budget.tokenUsageRatio)}` : null,
    ].filter(Boolean).join(' · ')
    : '';
  const riskTotal = diagnostics
    ? diagnostics.riskEventCounts.reduce((sum, item) => sum + item.count, 0)
    : null;

  const failedTicks = windowedTicks.filter((tick) => tick.status === 'failed').length;
  const tickFailureText = windowedTicks.length
    ? `Tick 失败率 ${((failedTicks / windowedTicks.length) * 100).toFixed(1)}%（近 ${windowedTicks.length} 次）`
    : '暂无 Tick 数据';

  // 统计日期：只在后端预算日确为今日（budgetDayIsToday）时显示它；陈旧预算日说明该世界今天还没跑拍，
  // 此时展示的消耗已被后端归零到今日口径，再挂一个过去的日期会自相矛盾，改显示本地今日。
  const statDate = budget?.budgetDayIsToday
    ? budget.budgetDay
    : new Date().toLocaleDateString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' }).replace(/\//g, '-');
  const startedAtText = diagnostics?.world.startedAt ? String(diagnostics.world.startedAt) : EMPTY;

  return (
    <div className="world-console">
      <div className="world-console__toolbar">
        <Segmented
          size="small"
          value={range}
          onChange={(value) => setRange(String(value))}
          options={RANGE_OPTIONS.map((item) => ({ label: item.label, value: item.value }))}
          aria-label="时间范围"
        />
        {/* 统计日期为只读展示（后端按自然日聚合预算），不做成可点按钮以免暗示不存在的选择能力。 */}
        <span className="world-console__date" title="统计日期（后端预算日；该世界今日尚未跑拍时显示本地今日）">
          <CalendarOutlined />
          {statDate}
        </span>
      </div>

      <div className="world-console__grid">
        <main className="world-console__main">
          <section className="world-health-strip" aria-label="世界运行概览">
            <div className="world-health-strip__metric is-healthy">
              <span className="world-health-strip__dot" /><span>运行中</span>
              <strong>{designPreview ? PREVIEW_HEALTH.running : health.running}</strong>
            </div>
            <div className="world-health-strip__metric is-warning">
              <span className="world-health-strip__dot" /><span>需关注</span>
              <strong>{designPreview ? PREVIEW_HEALTH.attention : health.attention}</strong>
            </div>
            <div className="world-health-strip__metric is-danger">
              <span className="world-health-strip__dot" /><span>已熔断</span>
              <strong>{designPreview ? PREVIEW_HEALTH.fused : health.fused}</strong>
            </div>
            <div
              className={`world-health-strip__metric is-cost${showCostSpark ? '' : ' has-no-chart'}`}
              title={costTodayCny != null && costTrendDays > 0 ? `平台今日成本（UTC 日界）· 迷你曲线为近 ${costTrendDays} 日趋势` : undefined}
            >
              <span>今日成本</span>
              <strong>{formatCny(costTodayCny)}</strong>
              {showCostSpark && <ReactECharts option={costSparkOption} style={{ width: 82, height: 38 }} />}
              {costTodayCny == null && (
                <p className="world-health-strip__note">
                  今日 Token {formatCompact(health.tokens)} · 成本接口暂未返回
                </p>
              )}
            </div>
          </section>

          <section className="world-table-panel" aria-labelledby="active-worlds-title">
            <header className="world-table-panel__header">
              <div>
                <h2 id="active-worlds-title">活跃世界</h2>
                <span>共 {worlds.length} 个{!designPreview && list.hasMore ? '（未加载完）' : ''}</span>
              </div>
              <div>
                <Select
                  size="small"
                  allowClear
                  placeholder="全部状态"
                  value={statusFilter}
                  onChange={setStatusFilter}
                  options={STATUS_FILTER_OPTIONS}
                  aria-label="状态筛选"
                />
                <Button size="small" icon={<ReloadOutlined />} onClick={refreshAll} disabled={designPreview}>刷新</Button>
              </div>
            </header>

            {list.error && !designPreview && <ErrorAlert message={list.error} onRetry={reload} />}
            <div className="world-table-panel__scroll">
              <table className="world-table">
                <thead>
                  <tr>
                    <th>世界名称</th><th>状态</th><th>房间类型</th><th>参与人数</th><th>当前 Tick</th><th>成功率</th><th>今日成本</th><th>风控延迟</th><th>最后活动</th>
                  </tr>
                </thead>
                <tbody>
                  {worlds.map((world) => (
                    // 设计文档 §6.2：点击整行即为主入口，右侧诊断栏随之更新。
                    <tr
                      key={world.id}
                      className={selected?.id === world.id ? 'is-selected' : ''}
                      tabIndex={0}
                      aria-selected={selected?.id === world.id}
                      onClick={() => setSelectedId(world.id)}
                      onKeyDown={(event) => {
                        if (event.key !== 'Enter' && event.key !== ' ') return;
                        event.preventDefault();
                        setSelectedId(world.id);
                      }}
                    >
                      <td>
                        <span className="world-table__world">
                          <img src={world.thumbnail} alt="" />
                          <strong>{world.title}</strong>
                        </span>
                      </td>
                      <td><span className={`world-status ${statusClass(world.status)}`}><i />{STATUS_TEXT[world.status] ?? world.status}</span></td>
                      <td>{world.roomTypeLabel}</td>
                      <td>{formatCompact(world.participantCount)}</td>
                      <td>{formatCompact(world.stateRevision)}</td>
                      {/* successRate 是 0..1 小数（已终结 tick 中 done 的占比），必须 ×100 再渲染；
                          null = 窗口内无已终结 tick，属"暂无数据"，显示 — 且不着色，不得当 0% 读。 */}
                      <td className={world.successRate == null
                        ? 'world-value'
                        : world.successRate < SUCCESS_RATE_WARN_RATIO ? 'world-value is-warning' : 'world-value is-good'}
                      >
                        {formatPercent(world.successRate)}
                      </td>
                      <td title={world.todayTokens != null ? `今日 ${formatCompact(world.todayTokens)} tokens` : undefined}>
                        {formatCny(world.todayCostCny)}
                      </td>
                      <td className={world.moderationLatency != null && world.moderationLatency > 3 ? 'world-value is-danger' : 'world-value'}>{world.moderationLatency == null ? EMPTY : `${world.moderationLatency}s`}</td>
                      <td>{world.lastActivityAt == null ? EMPTY : formatTime(world.lastActivityAt)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {list.loading && !designPreview && <div className="world-table-panel__loading"><Spin /></div>}
              {!list.loading && worlds.length === 0 && <div className="world-table-panel__empty">暂无世界</div>}
            </div>
            {!designPreview && list.hasMore && (
              <div className="world-table-panel__more">
                <Button size="small" onClick={list.loadMore} loading={list.loading}>加载更多</Button>
              </div>
            )}
          </section>

          <section className="world-timeline" aria-labelledby="world-timeline-title">
            <header>
              <div>
                <h2 id="world-timeline-title">事件时间线</h2>
                <span>（{selected?.title ?? '未选择世界'} · {rangeOption.windowLabel}）</span>
              </div>
              <div>
                <Select
                  size="small"
                  value={levelFilter}
                  onChange={(value) => setLevelFilter(value)}
                  options={[{ value: 'all', label: '级别：全部' }, { value: 'abnormal', label: '仅异常' }]}
                  aria-label="事件级别筛选"
                />
              </div>
            </header>
            <div className="world-timeline__rows">
              {visibleEvents.slice(0, TIMELINE_MAX_ROWS).map((event) => (
                <div className="world-timeline__row" key={event.key}>
                  <time>{formatEventTime(event.at, rangeOption.windowMs)}</time>
                  <Tag color={event.level === 'danger' ? 'red' : event.level === 'warning' ? 'orange' : 'green'}>{event.levelLabel}</Tag>
                  <div><strong>{event.title}</strong><span>{event.detail}</span></div>
                  <div><span>{event.source}</span><small>{event.ref}</small></div>
                </div>
              ))}
              {visibleEvents.length > TIMELINE_MAX_ROWS && (
                <div className="world-timeline__more">仅显示最近 {TIMELINE_MAX_ROWS} 条，{rangeOption.windowLabel}内共 {visibleEvents.length} 条</div>
              )}
              {!visibleEvents.length && (
                <div className="world-timeline__empty">
                  {selected ? `${rangeOption.windowLabel}内暂无事件记录` : '请选择一个世界查看事件'}
                </div>
              )}
            </div>
          </section>
        </main>

        <aside className="world-inspector" aria-label="世界诊断详情">
          {selected ? (
            <>
              <header className="world-inspector__identity">
                <img src={selected.thumbnail} alt={`${selected.title}缩略图`} />
                <div>
                  <div><h2>{selected.title}</h2><span className={`world-status ${statusClass(selected.status)}`}><i />{STATUS_TEXT[selected.status] ?? selected.status}</span></div>
                  <p>世界ID：{selected.id}　 房间类型：{selected.roomTypeLabel}</p>
                  {/* TODO(接口缺字段): startedAt —— 世界启动时间未落库/未投影，正式模式只能给创建时间。 */}
                  <p>启动时间：{startedAtText}</p>
                  <p>创建时间：{formatTime(selected.createdAt)}</p>
                </div>
              </header>

              <section className="world-inspector__latency">
                <h3>实时延迟 <span>（{rangeOption.windowLabel}，告警阈值 {LATENCY_ALERT_THRESHOLD_S}s）</span></h3>
                {latencyPoints.length ? (
                  <>
                    <ReactECharts option={latencyOption} style={{ height: 142 }} />
                    <strong className={currentLatency != null && currentLatency > LATENCY_ALERT_THRESHOLD_S ? 'is-alert' : ''}>
                      当前 {currentLatency}s
                    </strong>
                  </>
                ) : (
                  <div className="world-inspector__chart-empty">{diagLoading ? <Spin size="small" /> : `${rangeOption.windowLabel}内暂无 Tick 延迟数据`}</div>
                )}
              </section>

              <section className="world-inspector__metrics">
                <div>
                  <span>今日预算（金额）</span>
                  <strong>
                    {budget ? formatCny(budget.spentCny) : EMPTY}
                    <small> / {budget && budget.dailyCnyBudget > 0 ? formatCny(budget.dailyCnyBudget) : '未设金额上限'}</small>
                  </strong>
                  {budgetPercent != null
                    ? <Progress percent={budgetPercent} showInfo={false} strokeColor="#d37a57" railColor="#e4ded6" size="small" />
                    : budget && <p>未设配额上限，无用量比</p>}
                  {/* 已消耗一律取 server 的 Effective 值：跨日未跑拍时旧计数器已被归零，不会把昨天的量报成今日。 */}
                  <p>
                    {`Token ${budget ? formatCompact(budget.spentTokensTodayEffective) : EMPTY} / ${budget && budget.dailyTokenBudget > 0 ? formatCompact(budget.dailyTokenBudget) : '不限'}`}
                    {budgetRatioNote ? `　${budgetRatioNote}` : ''}
                  </p>
                </div>
                <div>
                  {/* 接口按 kind 聚合全量风控事件，没有按日切分，故口径写"累计"而非"今日"。 */}
                  <span>风险命中（累计）</span>
                  <strong>{riskTotal == null ? EMPTY : riskTotal} <small>次</small></strong>
                  {/* TODO(接口缺字段): riskEventCountsToday / 日环比 —— 需 risk_events 按日聚合。 */}
                  <p>日环比 {EMPTY}（接口未提供按日聚合）</p>
                </div>
              </section>

              {diagError && <div className="world-inspector__error"><ErrorAlert message={diagError} onRetry={() => setDiagReloadKey((key) => key + 1)} /></div>}

              <section className="world-inspector__incident">
                <h3>最新异常</h3>
                {latestIncident ? (
                  <div>
                    <AlertOutlined />
                    <p>
                      <strong>{latestIncident.title}</strong>
                      <span>{latestIncident.detail}</span>
                      {/* TODO(接口缺字段): impact / handlingHint —— 影响面与处置线索需 server 侧事件模型补充。 */}
                      <span>来源：{latestIncident.source}</span>
                      <small>发生时间：{formatEventTime(latestIncident.at, rangeOption.windowMs)}</small>
                    </p>
                  </div>
                ) : (
                  <div className="world-inspector__incident-empty">{rangeOption.windowLabel}内无异常记录</div>
                )}
              </section>

              <section className="world-inspector__route">
                <h3>模型路由</h3>
                <div>
                  <span><i />当前路由</span>
                  <strong>{String(diagnostics?.world.modelRouteVersion ?? selected.modelRouteVersion ?? EMPTY)}</strong>
                  <small>{tickFailureText}</small>
                </div>
                {/* TODO(接口缺字段): backupRoute / routeErrorRate —— 备用路由与路由级错误率无数据源。 */}
                <div>
                  <span><i className="is-idle" />备用路由</span>
                  <strong>{designPreview ? PREVIEW_BACKUP_ROUTE.name : EMPTY}</strong>
                  <small>{designPreview ? PREVIEW_BACKUP_ROUTE.errorRate : '暂无数据'}</small>
                </div>
              </section>

              <section className="world-inspector__prompt">
                <h3>Prompt 版本</h3>
                <div>
                  <span>当前版本</span>
                  <strong>{String(diagnostics?.world.promptSetVersion ?? selected.promptSetVersion ?? EMPTY)}</strong>
                  <Tag color="green">已生效</Tag>
                </div>
                {/* TODO(接口缺字段): promptSetUpdatedBy / promptSetUpdatedAt —— Prompt 发布元数据未投影。 */}
                <p>
                  {`更新人：${designPreview ? PREVIEW_PROMPT_EDITOR.editor : EMPTY}　更新时间：${designPreview ? PREVIEW_PROMPT_EDITOR.updatedAt : EMPTY}`}
                </p>
              </section>

              <div className="world-inspector__actions">
                <Button type="primary" size="large" loading={diagLoading} onClick={() => setDiagReloadKey((key) => key + 1)}>查看诊断</Button>
                <div>
                  <Button danger onClick={() => setAction({ row: selected, kind: selected.status === 'paused' ? 'resume' : 'pause' })}>
                    {selected.status === 'paused' ? '恢复世界' : '暂停世界'}
                  </Button>
                  <Button danger onClick={() => navigate(`/risk${designPreview ? '?design=preview' : ''}`)}>进入应急处置</Button>
                </div>
              </div>
            </>
          ) : (
            <div className="world-inspector__empty">请选择一个世界查看诊断</div>
          )}
        </aside>
      </div>

      <ReasonModal
        open={!!action}
        title={action?.kind === 'pause' ? `暂停世界 ${action?.row.title ?? ''}` : `恢复世界 ${action?.row.title ?? ''}`}
        okText={action?.kind === 'pause' ? '确认暂停' : '确认恢复'}
        danger={action?.kind === 'pause'}
        loading={acting}
        onOk={doAction}
        onCancel={() => setAction(null)}
      />

    </div>
  );
}
