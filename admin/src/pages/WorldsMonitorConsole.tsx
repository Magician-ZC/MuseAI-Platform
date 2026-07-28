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
  /**
   * 世界封面（迁移 0027）。**仅机审 approved 才会下发**；无封面 / 未过审 → 后端**不写该键**
   *（不是空串、不是 null），故此处可选；缺席时走 `thumbnailFor` 的确定性兜底图。
   */
  coverUrl?: string | null;
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
  world: Record<string, unknown> & {
    id: string;
    title: string;
    status: string;
    /** 开始跑的时刻 = **首拍被排期**（server: `MIN(world_ticks.created_at)`）。
     *  没排过拍 → null；**不回落 createdAt**：「还没开演」与「很早就开演了」必须分得开。
     *
     *  ⚠️ 类型是 `number | string`：正式模式来自接口，是毫秒时间戳；
     *  设计预览模式喂的是**已格式化好的展示串**（预览数据不经接口，见 `previewDiag`）。
     *  两种都要能渲染，故此处如实放宽，而不是在预览数据里硬凑一个假时间戳。 */
    startedAt?: number | string | null;
  };
  ticks: TickMeta[];
  budget: DiagnosticsBudget | null;
  riskEventCounts: { kind: string; count: number }[];
  /** 风控日环比（UTC 日界，口径同成本看板）。
   *  🔴 服务端**不给百分比**：昨日可能为 0，0 做分母不是「涨了无穷」是「没有可比基数」。 */
  riskEventDaily?: { today: number; yesterday: number; delta: number } | null;
  /** Prompt 版本治理留痕。⚠️ `activatedBy/At` 来自 audit_logs 的 prompt.activate，
   *  不是 prompt_versions 上的列；从未经端点激活过 → null，**不猜**。 */
  promptSet?: {
    version?: string | null;
    createdAt?: number | null;
    activatedBy?: string | null;
    activatedAt?: number | null;
  } | null;
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
  roomTypeLabel: string;
  /** 以下两项接口暂未提供，正式模式恒 null → 表格显示空态。 */
  moderationLatency: number | null;
  /** 最后活动时间（毫秒）= 最后一拍**跑完**的时刻；从未跑完过任何一拍 → null（不是 0）。 */
  lastActivityAt: number | null;
}

/** 平台级健康汇总（`GET /admin/worlds/summary`）。分档规则见 server 端同名函数。 */
interface WorldsSummary {
  total: number;
  running: number;
  attention: number;
  fused: number;
  paused: number;
  ended: number;
  /**
   * 命中**任意一条已启用规则**的世界数（去重）。新维度全关时恒等于 `attention`。
   *
   * 🔴 与 `attention` 的区别是刻意的：`attention` 的口径**永远只是预算那一条**，
   * 于是运营开一个新维度不会让一个既有数字的含义悄悄变掉；新维度进的是这个字段。
   */
  attentionAny?: number;
  /**
   * 「需关注」拆到是被哪条规则命中的。
   *
   * 🔴 各条**互相重叠**（一个世界可以同时预算逼近上限、失败率高、又停摆），
   * **不可相加**——要总数请用 `attentionAny`（server 侧 UNION 去重）。
   * 🔴 `enabled: false` 的维度 `count` 是 `null` 而不是 0：0 会被读成「一个都没有」，
   * 而真相是「这一维压根没开过」。
   */
  attentionReasons: {
    code: string;
    count: number | null;
    meaning?: string;
    thresholdBp?: number;
    enabled?: boolean;
    note?: string;
  }[];
}

/** 原因码 → 中文短名。表里没有的码原样显示（server 新加一条不会被静默吞掉）。 */
const ATTENTION_REASON_LABEL: Record<string, string> = {
  budget_ratio: '预算逼近上限',
  tick_failure_rate: 'tick 失败率',
  blocked_streak: '连续被拦',
  stalled: '停摆',
};

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

/** 诊断一次拉多少拍。服务端另有 500 的硬顶（被轮询的端点必须封顶），此处取其一半留余量：
 *  曲线与时间线都只做展示，200 个点已远超肉眼分辨率，再多只是白拉数据。 */
const DIAG_TICK_LIMIT = 200;

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
 * 世界封面**兜底图**（不是假数据，是"这个世界没有可展示封面"的确定性占位）。
 *
 * 真实封面走 `/admin/worlds` 的 `coverUrl`：后端只在机审 approved 时下发该键，
 * 无封面 / 待人审 / 被拒 → 键缺席，此时按 `world.id` 哈希取本表中的一张——
 * 同一世界恒定同一张，既不全站同图（分不清哪行是哪个世界）也不随机（每次刷新换图）。
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
    lastActivityAt: 1753428000000, coverUrl: '/assets/worlds/mist-sea-world.png',
  },
  {
    id: 'world_1002', title: '静止山脉', roomType: 'exploration', roomTypeLabel: '探索房', status: 'running', visibility: 'public',
    memberLimit: 1000, participantCount: 612, tickPerDay: 3, engineVersion: 'v3.8.2', promptSetVersion: 'v2.12.6',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 812104, spentTokensToday: 483000, dailyTokenBudget: 800000,
    fused: false, createdAt: 1753392100000, successRate: 0.978, todayTokens: 483000, todayCostCents: 9671,
    todayCostCny: 96.71, moderationLatency: 1.2,
    lastActivityAt: 1753427880000, coverUrl: '/assets/worlds/still-mountains.png',
  },
  {
    id: 'world_1003', title: '星火酒馆', roomType: 'social', roomTypeLabel: '社交房', status: 'attention', visibility: 'public',
    memberLimit: 600, participantCount: 386, tickPerDay: 6, engineVersion: 'v3.8.1', promptSetVersion: 'v2.12.8',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 302768, spentTokensToday: 321000, dailyTokenBudget: 500000,
    fused: false, createdAt: 1753391800000, successRate: 0.906, todayTokens: 321000, todayCostCents: 6433,
    todayCostCny: 64.33, moderationLatency: 3.8,
    lastActivityAt: 1753427940000, coverUrl: '/assets/worlds/ember-tavern.png',
  },
  {
    id: 'world_1004', title: '机械之城', roomType: 'quest', roomTypeLabel: '任务房', status: 'running', visibility: 'official',
    memberLimit: 1400, participantCount: 932, tickPerDay: 4, engineVersion: 'v3.8.2', promptSetVersion: 'v2.13.0',
    modelRouteVersion: 'qwen-max-primary', stateRevision: 1231009, spentTokensToday: 1051000, dailyTokenBudget: 1400000,
    fused: false, createdAt: 1753391600000, successRate: 0.951, todayTokens: 1051000, todayCostCents: 21038,
    todayCostCny: 210.38, moderationLatency: 1.4,
    lastActivityAt: 1753428000000, coverUrl: '/assets/worlds/mechanical-city.png',
  },
  {
    id: 'world_1005', title: '沙海旅途', roomType: 'story', roomTypeLabel: '剧情房', status: 'attention', visibility: 'public',
    memberLimit: 500, participantCount: 274, tickPerDay: 3, engineVersion: 'v3.8.1', promptSetVersion: 'v2.12.6',
    modelRouteVersion: 'qwen-max-backup', stateRevision: 213557, spentTokensToday: 210000, dailyTokenBudget: 400000,
    fused: false, createdAt: 1753391300000, successRate: 0.883, todayTokens: 210000, todayCostCents: 4219,
    todayCostCny: 42.19, moderationLatency: 4.5,
    lastActivityAt: 1753427820000, coverUrl: '/assets/worlds/desert-journey.png',
  },
  {
    id: 'world_1006', title: '永夜之境', roomType: 'side', roomTypeLabel: '副本房', status: 'fused', visibility: 'official',
    memberLimit: 400, participantCount: 0, tickPerDay: 2, engineVersion: 'v3.8.0', promptSetVersion: 'v2.11.9',
    modelRouteVersion: 'qwen-max-backup', stateRevision: 87662, spentTokensToday: 0, dailyTokenBudget: 300000,
    fused: true, createdAt: 1753390900000, successRate: null, todayTokens: 0, todayCostCents: 0,
    todayCostCny: 0, moderationLatency: null,
    lastActivityAt: 1753426980000, coverUrl: '/assets/worlds/evernight-realm.png',
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
 * 世界缩略图取图：**有真实过审封面就用真封面**，否则按 id 哈希取确定性兜底图。
 * 正式模式与预览模式共用本函数——预览样本同样只给 `coverUrl`，不额外造一条渲染路径。
 * 后端对未过审封面是"不下发该键"，这里再挡一次空串/空白，避免任何来源的空值渲染成碎图。
 */
function coverSrc(world: { id: string; coverUrl?: string | null }): string {
  return world.coverUrl?.trim() || thumbnailFor(world.id);
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
    // moderationLatency：**仍恒 null（显示 —）**，但理由已经和当初不一样了，故整条重写。
    // 原文写「全仓没有任何一处记录机审调用耗时」——那句话已经不成立：server 侧
    // migration 0049 加了 `safety_recheck_runs.provider_ms`（只累加 check_text 两端时钟差），
    // 读取面在 `GET /admin/safety/recheck` 的 `providerLatency.avgMsPerCall`。
    //
    // 🔴 数有了仍不在本列下发，是因为另外三件事一件都没成立：
    // ① provider 仍是 Dev 桩，耗时恒 ~0 —— 恒 0 的「审核延迟」在本列上与「审核非常快」长得一样；
    // ② §15 第 3 层默认关闭，多数世界一行台账都没有 —— 按世界聚合会得到一片 null；
    // ③ 该列只覆盖运行时投影这条链，静态审核（角色卡/模板/托梦信）不落那张表，
    //    摆在世界列表里会被读成那个世界的机审总体 SLA。
    // ⚠️ 另有两个**不得充数**的近似：audit_queue 的 created_at/reviewed_at 是人审周转
    //（小时/天量级，一眼可辨）；同表的 latency_ms 是一次尝试全程（含 DB 与记账，
    // 系统性偏大却看起来完全合理，更难识破）。逐条见 worlds_ops.rs::list_worlds 同名注释。
    moderationLatency: null,
    // lastActivityAt 由 /admin/worlds 直接下发（= MAX(world_ticks.finished_at)，最后一拍**跑完**的时刻）。
    // 🔴 服务端刻意不用 worlds.updated_at：那一列任何一次写世界行都会动（暂停、改预算……），
    // 会把**运营自己的操作**记成世界活动。旧版 server 无此键 → undefined → 各渲染点按 == null 走空态。
    lastActivityAt: (row as { lastActivityAt?: number | null }).lastActivityAt ?? null,
    // coverUrl 由 ...row 原样带入（后端只在机审 approved 时下发该键），取图统一走 coverSrc。
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

  // 平台级健康汇总：与列表分开取，因为它**不随分页/筛选变化**（永远是全量）。
  // 跟着 statusFilter 重取是刻意的——运营切了筛选之后仍应看到平台全貌，
  // 但重取的代价只有一条聚合 SQL。
  const [summary, setSummary] = useState<WorldsSummary | null>(null);
  useEffect(() => {
    if (designPreview) return undefined;
    let cancelled = false;
    adminFetch<WorldsSummary>('/admin/worlds/summary')
      .then((r) => {
        if (!cancelled) setSummary(r);
      })
      // 🔴 拿不到就置 null → 显示空态，**绝不回落到本页现算**：
      // 那个数会以「全量」的名义显示一个页内的值，比空着更误导。
      .catch(() => {
        if (!cancelled) setSummary(null);
      });
    return () => {
      cancelled = true;
    };
  }, [designPreview, statusFilter]);

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

  // 正式模式：选中世界变化（含首次落位）或**时间范围变化**即拉一次脱敏诊断；预览模式用样本诊断。
  //
  // 🔵 取数窗口**下推到服务端**（`?sinceMs=` + `?limit=`）。此前端点固定只回最近 10 拍，
  // 前端只能在客户端按时间过滤——窗口一拉长就没数据了（曲线越选越长、点越选越少）。
  // limit 取一个与窗口相称的上限：服务端另有 500 的硬顶（它是被轮询的端点，不封顶等于
  // 留了一条「拉一个跑了半年的世界就把整张表扫回来」的路），这里不必也不该自己再猜一个更大的数。
  useEffect(() => {
    if (designPreview || !selectedId) return undefined;
    let cancelled = false;
    setDiagLoading(true);
    setDiagError(null);
    const sinceMs = Date.now() - rangeOption.windowMs;
    adminFetch<Diagnostics>(
      `/admin/worlds/${selectedId}/diagnostics?sinceMs=${sinceMs}&limit=${DIAG_TICK_LIMIT}`,
    )
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
  }, [designPreview, selectedId, rangeOption.windowMs, diagReloadKey]);

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
  // 取数窗口已**下推到服务端**（上面的 `?sinceMs=&limit=`）。这里保留一次客户端过滤是因为
  // ① 预览模式的样本诊断不经接口；② 用户改时间范围到请求回来之间，屏幕上还是上一批数据，
  // 不过滤会看到窗口外的点。它是**二次收窄**，不再是唯一的收窄手段。
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
  // ✅ 三档改读 **平台级全量汇总** `GET /admin/worlds/summary`（2026-07-27）。
  //
  // 此前这里按"已加载的世界"现算，于是**翻页没翻完时三档全部偏小**——而运营看这条指标
  // 恰恰是为了「现在有几个世界要管」。一个系统性偏小、且偏小幅度取决于用户翻到第几页的
  // 数字，比没有这个数字更糟。这不是前端能修的：本页永远只持有当前页。
  //
  // 分档规则由 server 逐条保留本页原有的 deriveStatus（熔断 > 库状态 > 预算 > 运行中），
  // 阈值也原样搬过去（0.9 → MUSE_ATTENTION_BUDGET_BP=9000），**没有改判定**。
  // ⚠️ 一处故意差异：server 只认 budget_day = 今天的计数器，于是「昨天烧光、今天没跑」的
  // 世界不再被算成需关注——那是修 bug（口径同 diagnostics 的 spentTokensTodayEffective）。
  //
  // 🔴 汇总拿不到时**不回落到本页现算**：那个数会以「全量」的名义显示一个页内的值，
  // 比空着更误导。拿不到就显示空态。
  // 今日成本已改读 cost.today（见上方 costTodayCny），不再走这里的 token 合计。
  const health = useMemo(() => {
    if (designPreview) {
      let running = 0;
      let attention = 0;
      let fused = 0;
      for (const world of allWorlds) {
        if (world.status === 'fused') fused += 1;
        else if (world.status === 'attention') attention += 1;
        else if (world.status === 'running' || world.status === 'open') running += 1;
      }
      return { running, attention, fused };
    }
    if (!summary) return null;
    return { running: summary.running, attention: summary.attention, fused: summary.fused };
  }, [designPreview, allWorlds, summary]);

  /**
   * 「需关注」的构成。遍历 server **实际下发**的每一条原因，不是前端写死的四项——
   * server 新加一维时后台会自动显示它（码没登记就原样显示码），而不是静默漏掉。
   * 这是 `docs/VALIDATION.md` §3.37 那条教训的同款：算了、发了、却没人显示，等于没算。
   */
  const attentionBreakdown = useMemo(
    () =>
      (summary?.attentionReasons ?? []).map((r) => ({
        code: r.code,
        label: ATTENTION_REASON_LABEL[r.code] ?? r.code,
        // enabled === false → 「未启用」；count 为 null 同样不画数字（两者都不是 0）。
        text: r.enabled === false || r.count == null ? '未启用' : String(r.count),
      })),
    [summary],
  );

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
  // 日环比：服务端只给 today / yesterday / delta（**不给百分比**，理由见 Diagnostics 类型注释）。
  // 旧版 server 无此块 → 保持原来的空态文案，不假装有数。
  const riskDailyText = (() => {
    const d = diagnostics?.riskEventDaily;
    if (!d) return `日环比 ${EMPTY}（接口未提供按日聚合）`;
    const sign = d.delta > 0 ? '+' : '';
    return `今日 ${d.today} 次 · 昨日 ${d.yesterday} 次 · 环比 ${sign}${d.delta}`;
  })();

  const failedTicks = windowedTicks.filter((tick) => tick.status === 'failed').length;
  const tickFailureText = windowedTicks.length
    ? `Tick 失败率 ${((failedTicks / windowedTicks.length) * 100).toFixed(1)}%（近 ${windowedTicks.length} 次）`
    : '暂无 Tick 数据';

  // 统计日期：只在后端预算日确为今日（budgetDayIsToday）时显示它；陈旧预算日说明该世界今天还没跑拍，
  // 此时展示的消耗已被后端归零到今日口径，再挂一个过去的日期会自相矛盾，改显示本地今日。
  const statDate = budget?.budgetDayIsToday
    ? budget.budgetDay
    : new Date().toLocaleDateString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' }).replace(/\//g, '-');
  // 启动时间 = 首拍被排期的时刻（server 口径）。设计模式下预览数据给的是已格式化的字符串，
  // 正式模式给的是毫秒时间戳——两者都要能渲染，且**都不回落创建时间**（下面单独一行显示创建时间）。
  const promptTraceText = (() => {
    if (designPreview) {
      return `激活人：${PREVIEW_PROMPT_EDITOR.editor}　激活时间：${PREVIEW_PROMPT_EDITOR.updatedAt}`;
    }
    const ps = diagnostics?.promptSet;
    const by = ps?.activatedBy ?? EMPTY;
    const at = ps?.activatedAt != null ? formatTime(ps.activatedAt) : EMPTY;
    const created = ps?.createdAt != null ? formatTime(ps.createdAt) : EMPTY;
    return `激活人：${by}　激活时间：${at}　版本创建：${created}`;
  })();

  const startedAtRaw = diagnostics?.world.startedAt;
  const startedAtText =
    typeof startedAtRaw === 'number' ? formatTime(startedAtRaw)
    : typeof startedAtRaw === 'string' ? startedAtRaw
    : EMPTY;

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
              <strong>{designPreview ? PREVIEW_HEALTH.running : health?.running ?? EMPTY}</strong>
            </div>
            <div className="world-health-strip__metric is-warning">
              <span className="world-health-strip__dot" /><span>需关注</span>
              <strong>{designPreview ? PREVIEW_HEALTH.attention : health?.attention ?? EMPTY}</strong>
            </div>
            <div className="world-health-strip__metric is-danger">
              <span className="world-health-strip__dot" /><span>已熔断</span>
              <strong>{designPreview ? PREVIEW_HEALTH.fused : health?.fused ?? EMPTY}</strong>
            </div>
            {/* 「需关注」的构成。🔴 各条重叠、不可相加，总数用 attentionAny（server 已去重）。
                未启用的维度显示「未启用」而不是 0——0 会被读成「一个都没有」。 */}
            {!designPreview && attentionBreakdown.length > 0 && (
              <div className="world-health-strip__metric is-warning" title="各条规则互相重叠，不可相加；总数见括号里的去重值">
                <span>需关注构成</span>
                <strong style={{ fontSize: 13, fontWeight: 500 }}>
                  {attentionBreakdown.map((r) => `${r.label} ${r.text}`).join(' · ')}
                  {summary?.attentionAny != null && summary.attentionAny !== summary.attention
                    ? `（合计 ${summary.attentionAny}）`
                    : ''}
                </strong>
              </div>
            )}
            <div
              className={`world-health-strip__metric is-cost${showCostSpark ? '' : ' has-no-chart'}`}
              title={costTodayCny != null && costTrendDays > 0 ? `平台今日成本（UTC 日界）· 迷你曲线为近 ${costTrendDays} 日趋势` : undefined}
            >
              <span>今日成本</span>
              <strong>{formatCny(costTodayCny)}</strong>
              {showCostSpark && <ReactECharts option={costSparkOption} style={{ width: 82, height: 38 }} />}
              {costTodayCny == null && (
                <p className="world-health-strip__note">
                  {/* 🔴 这里**不再**给「今日 Token 合计」：三档已改读平台级全量汇总，
                      而 token 合计只有当前页的值——两个口径混在同一栏里，
                      读者会以为它们是同一个范围的数。成本接口没返回就直说没返回。 */}
                  成本接口暂未返回
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
                          <img src={coverSrc(world)} alt="" />
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
                <img src={coverSrc(selected)} alt={`${selected.title}缩略图`} />
                <div>
                  <div><h2>{selected.title}</h2><span className={`world-status ${statusClass(selected.status)}`}><i />{STATUS_TEXT[selected.status] ?? selected.status}</span></div>
                  <p>世界ID：{selected.id}　 房间类型：{selected.roomTypeLabel}</p>
                  {/* 启动时间 = 首拍被排期的时刻（server: MIN(world_ticks.created_at)）。
                      没排过拍 → —，**不拿创建时间冒充**（创建时间在下一行单独给）。 */}
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
                  {/* 日环比（UTC 日界，口径同成本看板）。🔴 只给绝对差值，**不给百分比**：
                      昨日可能为 0，而 0 做分母得到的不是「涨了无穷」，是「没有可比基数」。 */}
                  <p>{riskDailyText}</p>
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
                {/* 🔴 措辞是「激活人 / 激活时间」而不是「更新人 / 更新时间」：
                    prompt_versions 表上**没有** updated_by 列，服务端给的是 audit_logs 里
                    prompt.activate 的留痕。叫「更新人」会让人以为那张表上真有这么一列。
                    从未经激活端点生效过（如直接播种进库）→ —，**不拿创建时间冒充**。 */}
                <p>{promptTraceText}</p>
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
