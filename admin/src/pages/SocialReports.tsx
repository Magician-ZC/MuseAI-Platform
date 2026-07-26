// 社交举报队列（真人社交解锁 R3，migration 0040；总规格 §14【拍板 22】恨隔面具原则）。
// 后端：server/src/social/mod.rs 的 `/admin/social/reports*` 三个端点（reviewer / support / admin）。
//
// 🔴 这个页面必须说清楚、且在结构上守住的三件事：
//
// ① **本页只改举报单的状态，不执行处置**。`resolve` 写的是 pending → actioned/dismissed + 备注 + 审计；
//    封禁 / 内容驳回 / 改判走各自既有入口（详情抽屉里的跳转按钮）。原作者刻意没把处置塞进举报接口——
//    塞进来等于给封禁开一条**绕过既有权限矩阵**的侧门。前端因此只做「跳转 + 回填」，
//    一条新的写路径都不开；跳转按钮还要**受前端 RBAC 收敛**：reviewer 看得见举报队列但进不去用户管理，
//    那他就不该有一个能点的封禁按钮（后端仍会二次校验，前端做的是纵深与诚实）。
//
// ② **§14 恨隔面具原则**：玩家侧任何接口都不下发被举报人的真人 id（连举报回执都不给）。
//    真人 id 与举报正文只在**这一处**运营复核档出现，走 reviewer/support 鉴权，处置全程写
//    audit_logs('social.report_resolved')。界面上要把这件事写出来，而不是默默展示。
//
// ③ **未验证功能默认关闭**（VALIDATION.md §0.1）：整块能力由 MUSE_SOCIAL_IDENTITY_UNLOCK 控制，
//    默认关。关闭时后端整体 404 —— 那是「功能不存在」，不是故障，本页据此渲染空态而不是报错。
//
// 数据诚实纪律（设计文档 §9.1）：只渲染接口真实返回的字段；积压计数一律取 /summary 的全量聚合，
// **不拿已加载的那一页自己数**——分页页内统计出来的「待处理 12 条」会被读成队列只剩 12 条。
import { useCallback, useEffect, useState } from 'react';
import {
  Alert,
  Button,
  Descriptions,
  Drawer,
  Empty,
  Input,
  Modal,
  Select,
  Space,
  Table,
  Tag,
  Tooltip,
  Typography,
  message,
} from 'antd';
import type { TableColumnsType } from 'antd';
import { Link } from 'react-router-dom';
import { AdminApiError, adminFetch, getRole } from '../api';
import { canAccess, roleLabel } from '../rbac';
import { ErrorAlert, formatNumber, formatTime, friendlyError, usePagedList } from '../components/shared';
import './SocialReports.css';

// ---------------- 类型（对齐 server/src/social/mod.rs 的响应） ----------------

interface ReportRow {
  id: string;
  reporterUserId: string;
  subjectKind: string;
  subjectId: string;
  /** 🔴 服务端内部解析出的被举报真人 id。玩家侧任何响应体都不下发它，只有本复核档能看到。 */
  subjectUserId: string;
  worldId: string;
  category: string;
  detail: string;
  status: string;
  /** 处置人（admin user id）；未处置为空串。 */
  handledBy: string;
  /** 处置理由；未处置为空串。 */
  resolution: string;
  createdAt: number;
  /** 未处置为 0（不是 null）——服务端 DEFAULT 0，故这里按 0 判空。 */
  resolvedAt: number;
}

/** 复合游标：`(created_at, id)` 两段缺一不可，见 server/src/pagination.rs。 */
interface ReportCursor {
  cursor: number;
  cursorId: string;
}

interface ReportListRes {
  reports: ReportRow[];
  nextCursor: number | null;
  nextCursorId: string | null;
  status: string;
  category: string;
  subjectKind: string;
  pageSize: number;
}

interface DistributionRow {
  key: string;
  pending: number;
  actioned: number;
  dismissed: number;
  total: number;
}

interface SummaryRes {
  total: number;
  byStatus: Record<string, number>;
  byCategory: DistributionRow[];
  bySubjectKind: DistributionRow[];
  oldestPendingCreatedAt: number | null;
  escalateAt: number;
  escalatedSubjectCount: number;
  notes: string[];
}

// ---------------- 展示映射（后端白名单的中文名；未知取值一律原样回显，不吞） ----------------

const STATUS_TEXT: Record<string, { color: string; text: string }> = {
  pending: { color: 'blue', text: '待处理' },
  actioned: { color: 'green', text: '已处置' },
  dismissed: { color: 'default', text: '不予支持' },
};

const CATEGORY_TEXT: Record<string, string> = {
  harassment: '骚扰',
  impersonation: '冒充他人',
  minor_risk: '未成年风险',
  sexual: '色情内容',
  violence: '暴力威胁',
  fraud: '诈骗',
  other: '其他',
};

/** 类别底色按严重度分档；文字始终在场（设计文档 §5 末段：状态不得只用颜色表达）。 */
const CATEGORY_COLOR: Record<string, string> = {
  harassment: 'volcano',
  impersonation: 'orange',
  minor_risk: 'red',
  sexual: 'magenta',
  violence: 'red',
  fraud: 'gold',
  other: 'default',
};

const SUBJECT_TEXT: Record<string, string> = {
  character: '角色面具',
  unlock_request: '解锁请求',
};

const STATUS_FILTER = [
  { value: 'pending', label: '待处理' },
  { value: 'actioned', label: '已处置' },
  { value: 'dismissed', label: '不予支持' },
  { value: 'all', label: '全部状态' },
];

function statusTag(value: string) {
  const t = STATUS_TEXT[value] ?? { color: 'default', text: value };
  return <Tag color={t.color}>{t.text}</Tag>;
}

function categoryTag(value: string) {
  return <Tag color={CATEGORY_COLOR[value] ?? 'default'}>{CATEGORY_TEXT[value] ?? value}</Tag>;
}

/** 距今等待时长的粗粒度表达（分钟 / 小时 / 天）。原始时间戳同时展示，粗粒度只是给一眼看的。 */
function waitedText(since: number | null): string {
  if (since == null) return '—';
  const ms = Date.now() - since;
  if (!Number.isFinite(ms) || ms < 0) return '—';
  const minutes = Math.floor(ms / 60000);
  if (minutes < 60) return `${Math.max(0, minutes)} 分钟`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours} 小时`;
  return `${Math.floor(hours / 24)} 天`;
}

// ---------------- 处置动作：跳转到既有入口（本页不新开任何写路径） ----------------

interface JumpTarget {
  key: string;
  /** 目标所属 RBAC 模块 key —— 决定按钮可不可点（后端仍会二次校验）。 */
  module: string;
  label: string;
  to: string;
  /** 落地后真正执行处置的既有端点，写在界面上，避免「这个按钮到底会干什么」靠猜。 */
  endpoint: string;
  hint: string;
}

function jumpTargets(row: ReportRow, escalateAt: number | null): JumpTarget[] {
  const targets: JumpTarget[] = [
    {
      key: 'ban-subject',
      module: 'users',
      label: '用户管理 → 封禁 / 解封被举报人',
      to: `/users?query=${encodeURIComponent(row.subjectUserId)}`,
      endpoint: 'POST /admin/users/{id}/ban（require_role(support)，理由写审计）',
      hint: '搜索框已回填被举报人 id，落地即是目标行。',
    },
    {
      key: 'risk',
      module: 'risk',
      label: '风控 → 累计升级事件',
      to: '/risk?kind=social_report_threshold',
      endpoint: "GET /admin/risk-events?kind=social_report_threshold",
      hint:
        escalateAt != null
          ? `同一被举报人待处理举报数达 ${escalateAt} 条时，服务端写一条升级事件到风控面；达阈值对象的名单只在那里，本页不复制一份。`
          : '同一被举报人待处理举报数达阈值时，服务端写一条升级事件到风控面。',
    },
    {
      key: 'reporter',
      module: 'users',
      label: '用户管理 → 查看举报人（疑似滥用举报时）',
      to: `/users?query=${encodeURIComponent(row.reporterUserId)}`,
      endpoint: 'POST /admin/users/{id}/ban（require_role(support)）',
      hint: '举报本身也可能是骚扰手段；处置举报人走的是同一条既有路径。',
    },
  ];
  if (row.subjectKind === 'character') {
    targets.push({
      key: 'audit',
      module: 'audit',
      label: '内容审核 → 人审队列',
      to: '/audit',
      endpoint: 'POST /admin/audit-queue/{id}/reject（require_role(reviewer)）',
      hint:
        '仅当这张角色卡仍在人审队列中才能在那里驳回；已过审的卡，后台目前没有「再审 / 下架」入口（诚实空缺，不是本页藏起来了）。',
    });
  }
  return targets;
}

function JumpList({ row, escalateAt }: { row: ReportRow; escalateAt: number | null }) {
  const role = getRole();
  return (
    <ul className="social-reports__jumps">
      {jumpTargets(row, escalateAt).map((t) => {
        const allowed = canAccess(role, t.module);
        const button = (
          <Button size="small" disabled={!allowed}>
            前往
          </Button>
        );
        return (
          <li className="social-reports__jump" key={t.key}>
            <div className="social-reports__jump-main">
              <strong>{t.label}</strong>
              <small>{t.hint}</small>
              <code>{t.endpoint}</code>
            </div>
            {allowed ? (
              <Link to={t.to}>{button}</Link>
            ) : (
              <Tooltip
                title={`当前角色「${roleLabel(role)}」无权进入该模块。处置权限不在举报队列里放宽——举报单只负责记录结论。`}
              >
                {/* 禁用按钮不接收指针事件，套一层容器才能让 Tooltip 触发。 */}
                <span>{button}</span>
              </Tooltip>
            )}
          </li>
        );
      })}
    </ul>
  );
}

// ---------------- 功能未开启空态 ----------------

function FeatureOff({ onRetry }: { onRetry: () => void }) {
  return (
    <div className="social-reports__off">
      <Empty
        image={Empty.PRESENTED_IMAGE_SIMPLE}
        description={
          <div className="social-reports__off-text">
            <strong>真人社交解锁尚未开启，举报队列当前不存在</strong>
            <p>
              整块真人社交能力（解锁 / 拉黑 / 举报）由运行时开关 <code>MUSE_SOCIAL_IDENTITY_UNLOCK</code> 控制，
              <b>默认关闭</b>（VALIDATION.md §0.1「未验证功能默认关闭」）。开关从未对任何作用域开启过时，
              相关端点整体返回 404 —— 这是刻意的「功能不存在」，不是后台故障，也不代表没有举报。
            </p>
            <p>
              可见性判据是「入口<b>曾经</b>对任何人开放过」，而不是「此刻全局是开的」：
              若按世界灰度开过几个世界，那几个世界里产生的举报会真实落库，用全局口径判就会变成
              <b>举报进得来、处置进不去</b>的死结。因此只要开过一次，本页立即可用。
            </p>
            <p>开关按 用户 / 世界 / 全局 三级作用域灰度，由运营开关面（runtime_flags）或环境变量设置。</p>
          </div>
        }
      >
        <Button onClick={onRetry}>重新检测</Button>
      </Empty>
    </div>
  );
}

// ---------------- 主页面 ----------------

export default function SocialReports() {
  const [status, setStatus] = useState('pending');
  const [category, setCategory] = useState('all');
  const [subjectKind, setSubjectKind] = useState('all');
  const [detail, setDetail] = useState<ReportRow | null>(null);

  // 开关闸：'loading' 未知 / 'on' 可用 / 'off' 功能未开启（后端 404）。
  // 🔴 只有 404 才判「关」——其它错误（网络 / 500 / 鉴权）都不是「功能不存在」，
  // 把它们也渲染成「功能未开启」，等于在后端抽风时告诉运营「没有举报要处理」。
  const [gate, setGate] = useState<'loading' | 'on' | 'off'>('loading');
  const [summaryError, setSummaryError] = useState<string | null>(null);
  const [summary, setSummary] = useState<SummaryRes | null>(null);
  const [pageSize, setPageSize] = useState<number | null>(null);

  const [action, setAction] = useState<{ row: ReportRow; kind: 'actioned' | 'dismissed' } | null>(null);
  const [reason, setReason] = useState('');
  const [acting, setActing] = useState(false);

  const loadSummary = useCallback(async () => {
    setSummaryError(null);
    try {
      const res = await adminFetch<SummaryRes>('/admin/social/reports/summary');
      setSummary(res);
      setGate('on');
    } catch (e) {
      // 🔴 404 = 功能未开启（整块能力不存在），不是错误：渲染空态而不是红色报错条。
      if (e instanceof AdminApiError && e.code === 'not_found') {
        setSummary(null);
        setGate('off');
        return;
      }
      // 其它失败：**队列形状取不到 ≠ 队列不可用**。指标退回 `—`，队列本身照常渲染并各自报错——
      // 不能因为一个聚合接口挂了就把待处理的举报单藏起来。
      setSummary(null);
      setSummaryError(friendlyError(e));
      setGate('on');
    }
  }, []);

  useEffect(() => {
    loadSummary();
  }, [loadSummary]);

  const list = usePagedList<ReportRow, ReportCursor>(async (cursor) => {
    const qs = new URLSearchParams({ status, category, subjectKind });
    if (cursor) {
      qs.set('cursor', String(cursor.cursor));
      qs.set('cursorId', cursor.cursorId);
    }
    try {
      const res = await adminFetch<ReportListRes>(`/admin/social/reports?${qs.toString()}`);
      setPageSize(res.pageSize ?? null);
      // 🔴 两段缺一不可：只带 created_at 的单列游标会在同毫秒并列组横跨页边界时**永久丢行**
      //（server/src/pagination.rs）。缺第二段时宁可判「没有下一页」，也不退化成会丢举报的翻页。
      const next =
        res.nextCursor != null && res.nextCursorId != null
          ? { cursor: res.nextCursor, cursorId: res.nextCursorId }
          : null;
      return { items: res.reports, nextCursor: next };
    } catch (e) {
      // 会话期间开关被关掉 → 整页切到「功能未开启」，而不是留一条看不懂的报错。
      if (e instanceof AdminApiError && e.code === 'not_found') {
        setGate('off');
        return { items: [], nextCursor: null };
      }
      throw e;
    }
  });

  const { reload } = list;
  useEffect(() => {
    if (gate === 'on') reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gate, status, category, subjectKind]);

  const refreshAll = useCallback(() => {
    loadSummary();
    reload();
  }, [loadSummary, reload]);

  const openAction = (row: ReportRow, kind: 'actioned' | 'dismissed') => {
    setReason('');
    setAction({ row, kind });
  };

  const doResolve = async () => {
    if (!action) return;
    const text = reason.trim();
    if (!text) {
      message.warning('请填写复核结论（必填，最多 500 字，将写入举报单与审计日志）');
      return;
    }
    setActing(true);
    try {
      await adminFetch(`/admin/social/reports/${encodeURIComponent(action.row.id)}/resolve`, 'POST', {
        action: action.kind,
        reason: text,
      });
      message.success(action.kind === 'actioned' ? '已标记为「已处置」' : '已标记为「不予支持」');
      setAction(null);
      setDetail(null);
      refreshAll();
    } catch (e) {
      // 409（已被别人处理，CAS 不覆盖）/ 400（理由校验）直接展示服务端文案，并刷新回真实状态。
      message.error(friendlyError(e));
      if (e instanceof AdminApiError && (e.code === 'conflict' || e.code === 'not_found')) refreshAll();
    } finally {
      setActing(false);
    }
  };

  // 筛选项优先用 /summary 下发的键（白名单键恒出现，哪怕计数是 0，故新类别一上线就能筛）；
  // 形状取不到时退回本地白名单（与 server 的 REPORT_CATEGORIES / SUBJECT_KINDS 同一份），
  // 只是没有计数——**筛选能力不该因为一个聚合接口失败而消失**。
  const categoryOptions = [
    { value: 'all', label: '全部类别' },
    ...(summary
      ? summary.byCategory.map((c) => ({
          value: c.key,
          label: `${CATEGORY_TEXT[c.key] ?? c.key}（待处理 ${c.pending}）`,
        }))
      : Object.keys(CATEGORY_TEXT).map((k) => ({ value: k, label: CATEGORY_TEXT[k] }))),
  ];
  const subjectOptions = [
    { value: 'all', label: '全部主体' },
    ...(summary
      ? summary.bySubjectKind.map((k) => ({
          value: k.key,
          label: `${SUBJECT_TEXT[k.key] ?? k.key}（待处理 ${k.pending}）`,
        }))
      : Object.keys(SUBJECT_TEXT).map((k) => ({ value: k, label: SUBJECT_TEXT[k] }))),
  ];

  const columns: TableColumnsType<ReportRow> = [
    { title: '举报时间', dataIndex: 'createdAt', key: 'createdAt', width: 170, render: formatTime },
    { title: '类别', dataIndex: 'category', key: 'category', width: 120, render: categoryTag },
    // 主体与双方各占一列、每列两行（设计文档 §5 的 58px 行高正好容得下）：
    // 六列铺开会把「举报说明」挤成十来个字，而摘要那一列才是分诊时真正在扫的东西。
    {
      title: '主体',
      key: 'subject',
      width: 190,
      render: (_, r) => (
        <div className="social-reports__cell">
          <span>{SUBJECT_TEXT[r.subjectKind] ?? r.subjectKind}</span>
          <small>
            <Typography.Text code>{r.subjectId}</Typography.Text>
          </small>
        </div>
      ),
    },
    {
      title: '被举报人 / 举报人',
      key: 'parties',
      width: 210,
      render: (_, r) => (
        <div className="social-reports__cell">
          <Typography.Text code>{r.subjectUserId || '—'}</Typography.Text>
          <small>
            举报人 <Typography.Text code>{r.reporterUserId || '—'}</Typography.Text>
          </small>
        </div>
      ),
    },
    {
      title: '举报说明',
      dataIndex: 'detail',
      key: 'detail',
      ellipsis: true,
      render: (v: string) => v || <Typography.Text type="secondary">未填写</Typography.Text>,
    },
    { title: '状态', dataIndex: 'status', key: 'status', width: 100, render: statusTag },
    {
      title: '操作',
      key: 'op',
      fixed: 'right',
      width: 90,
      render: (_, r) => (
        <Button size="small" onClick={() => setDetail(r)}>
          详情
        </Button>
      ),
    },
  ];

  const pending = summary?.byStatus?.pending ?? null;
  const oldest = summary?.oldestPendingCreatedAt ?? null;

  return (
    <div className="social-reports">
      <div className="social-reports__head">
        <h4>社交举报队列</h4>
        <span className="social-reports__badge">运营复核档 · 全程审计</span>
        <small>
          真人社交解锁（§14 恨隔面具原则）的治理闭环环节。玩家侧所有接口都不下发被举报人的真人 id，
          这里是唯一能同时看到真人 id 与举报正文的地方；处置动作不在本页发生，跳转到各自既有入口执行。
        </small>
      </div>

      {gate === 'loading' && <Alert type="info" showIcon title="正在检测真人社交解锁开关…" />}

      {gate === 'off' && <FeatureOff onRetry={loadSummary} />}

      {gate === 'on' && (
        <>
          {/* 🔴 处置边界：这一条不是提示语，是本页的设计前提。 */}
          <Alert
            type="warning"
            showIcon
            style={{ marginBottom: 16 }}
            title="本页只改举报单状态，不执行处置"
            description="复核处置写的是这张举报单的「待处理 → 已处置 / 不予支持」+ 结论备注 + 审计留痕。封禁、内容驳回、申诉改判走各自既有入口（详情抽屉内的跳转按钮），各带自己的权限校验与审计。举报接口刻意不包办处置——包办等于给封禁开一条绕过既有权限矩阵的侧门。"
          />

          {summaryError && (
            <Alert
              type="warning"
              showIcon
              style={{ marginBottom: 16 }}
              title="队列形状（积压 / 类别分布）取数失败，指标显示为 —"
              description={`${summaryError}　队列本身照常可用；指标是聚合口径，取不到时宁可显示 — 也不拿当前这一页的条数充数。`}
              action={
                <Button size="small" onClick={loadSummary}>
                  重试
                </Button>
              }
            />
          )}

          <dl className="social-reports__stats">
            <div className={`social-reports__stat${pending ? ' is-attention' : ''}`}>
              <dt>待处理</dt>
              <dd>{formatNumber(pending)}</dd>
            </div>
            <div className="social-reports__stat">
              <dt>已处置</dt>
              <dd>{formatNumber(summary?.byStatus?.actioned ?? null)}</dd>
            </div>
            <div className="social-reports__stat">
              <dt>不予支持</dt>
              <dd>{formatNumber(summary?.byStatus?.dismissed ?? null)}</dd>
            </div>
            <div
              className={`social-reports__stat${summary?.escalatedSubjectCount ? ' is-critical' : ''}`}
            >
              <dt>已达升级阈值的对象</dt>
              <dd>
                {formatNumber(summary?.escalatedSubjectCount ?? null)}
                {summary?.escalateAt != null && <small>阈值 {summary.escalateAt} 条</small>}
              </dd>
            </div>
            <div className={`social-reports__stat${oldest != null ? ' is-attention' : ''}`}>
              <dt>最久未处理</dt>
              <dd>
                {waitedText(oldest)}
                {oldest != null && <small>{formatTime(oldest)}</small>}
              </dd>
            </div>
          </dl>

          <Space style={{ marginBottom: 16 }} wrap>
            <span>状态：</span>
            <Select style={{ width: 140 }} value={status} onChange={setStatus} options={STATUS_FILTER} />
            <span>类别：</span>
            <Select
              style={{ width: 200 }}
              value={category}
              onChange={setCategory}
              options={categoryOptions}
            />
            <span>主体：</span>
            <Select
              style={{ width: 200 }}
              value={subjectKind}
              onChange={setSubjectKind}
              options={subjectOptions}
            />
            <Button onClick={refreshAll}>刷新</Button>
          </Space>

          {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

          <Table
            rowKey="id"
            size="small"
            columns={columns}
            dataSource={list.items}
            loading={list.loading}
            pagination={false}
            scroll={{ x: 1080 }}
            locale={{
              emptyText: list.loading ? ' ' : '该筛选条件下没有举报单（这是真实结果，不是加载失败）。',
            }}
          />

          {list.hasMore && (
            <div style={{ textAlign: 'center', marginTop: 16 }}>
              <Button onClick={list.loadMore} loading={list.loading}>
                加载更多
              </Button>
            </div>
          )}

          <ul className="social-reports__notes">
            <li>
              计数取自 <code>/admin/social/reports/summary</code> 的全量聚合，不受分页与筛选影响；
              列表按 <code>(举报时间, id)</code> 复合游标翻页
              {pageSize != null ? `，每页 ${pageSize} 条（服务端配置 MUSE_SOCIAL_PAGE_SIZE）` : ''}。
              复合游标是必需的：同毫秒批量到达的举报若用单列游标翻页，横跨页边界的那几条会被永久跳过——
              运营看不见 = 永远不会被处置。
            </li>
            <li>
              🔴 举报与拉黑<b>不设年龄门</b>：把未成年的举报能力关掉，等于把「保护未成年」做成
              「让未成年无法自保」。本页不对举报人做任何年龄相关的过滤、降权或排序。
            </li>
            <li>
              达阈值对象的<b>名单</b>在风控面（<code>risk_events.kind = 'social_report_threshold'</code>），
              本页只给数量、不复制名单——一处事实一个去处。
            </li>
            {(summary?.notes ?? []).map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        </>
      )}

      <Drawer
        title="举报详情（运营复核档）"
        width={720}
        open={!!detail}
        onClose={() => setDetail(null)}
        extra={
          detail?.status === 'pending' ? (
            <Space>
              <Button onClick={() => openAction(detail, 'dismissed')}>不予支持</Button>
              <Button type="primary" danger onClick={() => openAction(detail, 'actioned')}>
                标记为已处置
              </Button>
            </Space>
          ) : (
            detail && statusTag(detail.status)
          )
        }
      >
        {detail && (
          <>
            <Descriptions
              column={1}
              bordered
              size="small"
              items={[
                {
                  key: 'id',
                  label: '举报单 ID',
                  children: (
                    <Typography.Text code copyable>
                      {detail.id}
                    </Typography.Text>
                  ),
                },
                { key: 'status', label: '状态', children: statusTag(detail.status) },
                { key: 'category', label: '类别', children: categoryTag(detail.category) },
                {
                  key: 'kind',
                  label: '主体类型',
                  children: SUBJECT_TEXT[detail.subjectKind] ?? detail.subjectKind,
                },
                {
                  key: 'subjectId',
                  label: '主体 ID',
                  children: (
                    <Typography.Text code copyable>
                      {detail.subjectId}
                    </Typography.Text>
                  ),
                },
                {
                  key: 'subjectUser',
                  label: '被举报人（真人 id）',
                  children: detail.subjectUserId ? (
                    <Typography.Text code copyable>
                      {detail.subjectUserId}
                    </Typography.Text>
                  ) : (
                    '—'
                  ),
                },
                {
                  key: 'reporter',
                  label: '举报人（真人 id）',
                  children: detail.reporterUserId ? (
                    <Typography.Text code copyable>
                      {detail.reporterUserId}
                    </Typography.Text>
                  ) : (
                    '—'
                  ),
                },
                {
                  key: 'world',
                  label: '世界',
                  children: detail.worldId ? (
                    <Typography.Text code copyable>
                      {detail.worldId}
                    </Typography.Text>
                  ) : (
                    '—'
                  ),
                },
                { key: 'createdAt', label: '举报时间', children: formatTime(detail.createdAt) },
                {
                  key: 'resolvedAt',
                  label: '处置时间',
                  children: detail.resolvedAt ? formatTime(detail.resolvedAt) : '—',
                },
                { key: 'handledBy', label: '处置人', children: detail.handledBy || '—' },
              ]}
            />

            <Typography.Title level={5} style={{ marginTop: 20 }}>
              举报说明（举报人填写）
            </Typography.Title>
            {detail.detail ? (
              <pre className="social-reports__text">{detail.detail}</pre>
            ) : (
              <Typography.Text type="secondary">举报人未填写说明。</Typography.Text>
            )}

            {detail.resolution && (
              <>
                <Typography.Title level={5} style={{ marginTop: 20 }}>
                  复核结论
                </Typography.Title>
                <pre className="social-reports__text">{detail.resolution}</pre>
              </>
            )}

            <Typography.Title level={5} style={{ marginTop: 20 }}>
              处置动作（跳转到既有入口）
            </Typography.Title>
            <p className="social-reports__hint">
              下面每一项都是<b>既有</b>的处置路径，各自带自己的权限矩阵与审计。本页不复制、也不绕过它们；
              无权进入的模块，按钮就是不可点的（后端仍会二次校验）。
            </p>
            <JumpList row={detail} escalateAt={summary?.escalateAt ?? null} />

            <p className="social-reports__masked">
              §14 恨隔面具原则：玩家侧任何接口都不下发被举报人的真人 id（连举报回执都不给）。
              本档是唯一同时展示真人 id 与举报正文的读取面，受 reviewer / support 鉴权，
              处置写入 <code>audit_logs('social.report_resolved')</code>。请勿把此处的真人 id 带到任何玩家可见的地方。
            </p>
          </>
        )}
      </Drawer>

      <Modal
        open={!!action}
        title={action?.kind === 'actioned' ? '标记为「已处置」' : '标记为「不予支持」'}
        okText="提交复核结论"
        cancelText="取消"
        confirmLoading={acting}
        okButtonProps={{ danger: action?.kind === 'actioned' }}
        onOk={doResolve}
        onCancel={() => setAction(null)}
      >
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 12 }}
          title="这一步只改举报单状态"
          description={
            action?.kind === 'actioned'
              ? '「已处置」表示这条举报已有结论并已在对应模块执行处置。它本身不封禁、不下架任何东西——实处置请在跳转入口完成。'
              : '「不予支持」表示经复核不构成违规。举报人不会收到被举报人的任何身份信息（§14）。'
          }
        />
        <Input.TextArea
          rows={4}
          maxLength={500}
          showCount
          value={reason}
          placeholder="填写复核结论（必填，最多 500 字）：依据了什么、在哪个入口执行了什么处置。将写入举报单与审计日志。"
          onChange={(e) => setReason(e.target.value)}
        />
      </Modal>
    </div>
  );
}
