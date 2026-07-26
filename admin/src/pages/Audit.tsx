// 内容审核：Tab「审核队列」（机审预标注 + 人审 + 详情抽屉 + approve/reject）
// + Tab「申诉复审」（components/AuditAppeals，被驳回内容的申诉裁决）
// + Tab「已过审内容处置」（components/ContentDisposal，再审 / 下架 / 恢复，migration 0044）。
// #10b（§10）：详情抽屉展示「卡片全文 cardJson + 机审命中点 + 同作者历史」。
// 卡片全文/历史由审核详情端点（G-ASSETS #10a 契约）提供；端点未就绪时优雅降级——
// 仍展示机审命中并标注「卡片全文需后端支持」，不崩溃。
//
//
// world_event 主体（§15 第 2/3 层送进队列的运行时投影事件，migration 0047）：
//   · 详情抽屉附「事件内容」面板 —— 没有它人审是在盲审（只有一行 kind 和一个 {"layer":3} 载荷）；
//   · 🔴 两档按钮：「通过」只推翻**机器**收紧（reviewer）；被人审驳回过的事件走
//     「恢复放行（admin）」，理由必填。两档不共用一个按钮，且台阶在抽屉里就可见——
//     否则运营点了才收到 409。
//
// 🔴 前两个 Tab 只作用于**仍在人审队列里**的条目；第三个 Tab 作用于**已经在线上**的内容——
// 这正是举报队列（社交举报 → 详情抽屉 → 跳转清单）指过来的那条路径。深链形如
// `/audit?tab=disposal&kind=character&subject=cchar_x`，落地即自动查出目标主体，
// 免得运营从举报单里手抄一遍 id。
import { useEffect, useRef, useState } from 'react';
import { Alert, Button, Descriptions, Drawer, message, Select, Space, Spin, Table, Tabs, Tag, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { useLocation } from 'react-router-dom';
import { adminFetch, getRole } from '../api';
import AuditAppeals from '../components/AuditAppeals';
import ContentDisposal from '../components/ContentDisposal';
import { ErrorAlert, formatTime, friendlyError, ReasonModal, usePagedList } from '../components/shared';

interface AuditRow {
  id: string;
  subjectKind: string;
  subjectId: string;
  /** world_event 主体的世界维度（其余主体为 null）。subjectId 是 domainEventId，跨世界重名。 */
  subjectWorldId?: string | null;
  machineVerdict: string;
  machineHits: unknown;
  status: string;
  reviewerId: string | null;
  reviewedAt: number | null;
  createdAt: number;
}

/** world_event 主体的事件本体（详情端点随 subjectEvent 下发）。 */
interface SubjectEvent {
  eventId?: string;
  worldId?: string;
  moderation?: string;
  tickNo?: number;
  sequence?: number;
  eventType?: string;
  visibility?: string;
  publicProjection?: string | null;
  privateProjections?: string | null;
  arbiterNote?: string | null;
  /** 🔴 决定这一行该点「通过」还是该走「恢复放行」——台阶在工作台上就该可见。 */
  humanRejectedBefore?: boolean;
  /** 定位不到事件时的原因（世界已清理 / 存量行跨世界重名）。 */
  unresolved?: string;
}

/** 同作者历史发布项（G-ASSETS 契约：authorHistory:[{id,version,moderation,createdAt}]）。 */
interface AuthorHistoryEntry {
  id: string;
  version?: number | string | null;
  moderation?: string | null;
  createdAt?: number | null;
}

/** 审核详情（列表行 + 卡片全文 + 同作者历史）。cardJson/authorHistory 端点未就绪时缺省。 */
interface AuditDetail extends AuditRow {
  cardJson?: unknown;
  authorHistory?: AuthorHistoryEntry[];
  subjectEvent?: SubjectEvent | null;
}

/** 运行时世界事件主体的 subject_kind（与 server 端 `safety::WORLD_EVENT_SUBJECT` 同一字面量）。 */
const WORLD_EVENT = 'world_event';

const STATUS_OPTIONS = [
  { value: 'open', label: '待审核' },
  { value: 'approved', label: '已通过' },
  { value: 'rejected', label: '已驳回' },
];

const SUBJECT_TEXT: Record<string, string> = {
  character: '角色卡',
  character_avatar: '角色立绘',
  world_cover: '世界封面',
  template: '世界模板',
  world_template: '世界模板',
  intervention: '干预文本',
  event: '世界事件',
  // §15 第 2/3 层送进队列的运行时投影事件（migration 0047 起裁决可回写）。
  world_event: '世界事件（运行时）',
};

const VERDICT_TAG: Record<string, { color: string; text: string }> = {
  pass: { color: 'green', text: '机审通过' },
  pending: { color: 'default', text: '待机审' },
  flag: { color: 'orange', text: '机审存疑' },
  block: { color: 'red', text: '机审拦截' },
};

const STATUS_TAG: Record<string, { color: string; text: string }> = {
  open: { color: 'blue', text: '待审核' },
  approved: { color: 'green', text: '已通过' },
  rejected: { color: 'red', text: '已驳回' },
};

const MODERATION_TAG: Record<string, { color: string; text: string }> = {
  approved: { color: 'green', text: '已通过' },
  rejected: { color: 'red', text: '已驳回' },
  pending: { color: 'gold', text: '待审核' },
  open: { color: 'blue', text: '待审核' },
  draft: { color: 'default', text: '草稿' },
};

/** 机审命中：兼容字符串数组或对象数组，统一渲染。 */
function MachineHits({ hits }: { hits: unknown }) {
  if (!Array.isArray(hits) || hits.length === 0) {
    return <Typography.Text type="secondary">无机审命中</Typography.Text>;
  }
  if (hits.every((h) => typeof h === 'string')) {
    return (
      <Space wrap>
        {(hits as string[]).map((h, i) => (
          <Tag key={i} color="orange">{h}</Tag>
        ))}
      </Space>
    );
  }
  return (
    <pre style={{ maxHeight: 260, overflow: 'auto', background: '#0000000a', padding: 12, borderRadius: 6, margin: 0 }}>
      {JSON.stringify(hits, null, 2)}
    </pre>
  );
}

/** 卡片全文：对象序列化为 JSON，字符串原样展示。 */
function CardFullText({ cardJson }: { cardJson: unknown }) {
  if (cardJson == null || (typeof cardJson === 'object' && Object.keys(cardJson as object).length === 0)) {
    return <Typography.Text type="secondary">该主体无卡片全文，或后端未随详情返回。</Typography.Text>;
  }
  const text = typeof cardJson === 'string' ? cardJson : JSON.stringify(cardJson, null, 2);
  return (
    <pre style={{ maxHeight: 340, overflow: 'auto', background: '#0000000a', padding: 12, borderRadius: 6, margin: 0, whiteSpace: 'pre-wrap' }}>
      {text}
    </pre>
  );
}

/**
 * world_event 主体的事件本体。
 *
 * 🔴 没有这一段，人审是在**盲审**：工作台上只有一行 `world_event` 和一个 `{"layer":3}` 的
 * 机审载荷，无从判断该通过还是驳回（理由与位图主体附图逐字相同）。
 */
function WorldEventPanel({ ev }: { ev: SubjectEvent }) {
  if (ev.unresolved) {
    return (
      <Alert
        type="warning"
        showIcon
        title="定位不到对应的世界事件"
        description={`${ev.unresolved} 该队列行仍可驳回关闭，但无法回写事件审核态。`}
      />
    );
  }
  const block = (label: string, text?: string | null) =>
    text ? (
      <>
        <Typography.Text type="secondary">{label}</Typography.Text>
        <pre style={{ maxHeight: 220, overflow: 'auto', background: '#0000000a', padding: 12, borderRadius: 6, margin: '4px 0 12px', whiteSpace: 'pre-wrap' }}>
          {text}
        </pre>
      </>
    ) : null;
  return (
    <>
      <Descriptions
        column={2}
        bordered
        size="small"
        items={[
          { key: 'w', label: '世界', children: <Typography.Text code copyable>{ev.worldId ?? '—'}</Typography.Text> },
          { key: 'e', label: '事件行 ID', children: <Typography.Text code copyable>{ev.eventId ?? '—'}</Typography.Text> },
          { key: 'tick', label: '拍 / 序号', children: `${ev.tickNo ?? '—'} / ${ev.sequence ?? '—'}` },
          { key: 'vis', label: '可见性', children: ev.visibility ?? '—' },
          {
            key: 'm',
            label: '当前审核态',
            children: (() => {
              const t = MODERATION_TAG[ev.moderation ?? ''] ?? { color: 'default', text: ev.moderation ?? '—' };
              return <Tag color={t.color}>{t.text}</Tag>;
            })(),
          },
          { key: 'type', label: '事件类型', children: ev.eventType ?? '—' },
        ]}
      />
      <div style={{ marginTop: 12 }}>
        {block('公共投影', ev.publicProjection)}
        {block('私有投影', ev.privateProjections)}
        {block('仲裁备注', ev.arbiterNote)}
      </div>
      <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
        裁决只改事件的**可见性**（moderation 一列），正文一个字节不动（§0.3 公共事实不可回滚）。
      </Typography.Paragraph>
    </>
  );
}

const HISTORY_COLUMNS: TableColumnsType<AuthorHistoryEntry> = [
  { title: '版本', dataIndex: 'version', key: 'version', width: 90, render: (v: AuthorHistoryEntry['version']) => v ?? '—' },
  {
    title: '审核状态',
    dataIndex: 'moderation',
    key: 'moderation',
    width: 100,
    render: (m: AuthorHistoryEntry['moderation']) => {
      if (!m) return '—';
      const t = MODERATION_TAG[m] ?? { color: 'default', text: m };
      return <Tag color={t.color}>{t.text}</Tag>;
    },
  },
  { title: '提交时间', dataIndex: 'createdAt', key: 'createdAt', render: (v: AuthorHistoryEntry['createdAt']) => formatTime(v) },
  { title: 'ID', dataIndex: 'id', key: 'id', render: (v: string) => <Typography.Text code>{v}</Typography.Text> },
];

export default function Audit() {
  // 深链参数（举报队列的跳转清单会带上它们）。只读一次初值：之后的 Tab 切换由用户控制，
  // 不该被地址栏里那次跳转反复拽回去。
  const search = new URLSearchParams(useLocation().search);
  const [tab, setTab] = useState(search.get('tab') === 'disposal' ? 'disposal' : 'queue');
  const deepLinkKind = search.get('kind');
  const deepLinkSubject = search.get('subject');
  const [status, setStatus] = useState('open');
  const [detail, setDetail] = useState<AuditRow | null>(null);
  const [enriched, setEnriched] = useState<AuditDetail | null>(null);
  const [enrichLoading, setEnrichLoading] = useState(false);
  const [enrichUnavailable, setEnrichUnavailable] = useState(false);
  const [action, setAction] = useState<{ row: AuditRow; kind: 'approve' | 'reject' | 'reinstate' } | null>(null);
  const [acting, setActing] = useState(false);
  // 当前打开详情的 id，用于丢弃切换后到达的过期响应。
  const openIdRef = useRef<string | null>(null);

  const list = usePagedList<AuditRow>(async (cursor) => {
    const qs = new URLSearchParams({ status });
    if (cursor) qs.set('cursor', cursor);
    qs.set('limit', '20');
    const res = await adminFetch<{ items: AuditRow[]; nextCursor: string | null }>(
      `/admin/audit-queue?${qs.toString()}`,
    );
    return { items: res.items, nextCursor: res.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  const closeDetail = () => {
    openIdRef.current = null;
    setDetail(null);
    setEnriched(null);
    setEnrichUnavailable(false);
    setEnrichLoading(false);
  };

  const openDetail = (row: AuditRow) => {
    setAction(null);
    setDetail(row);
    openIdRef.current = row.id;

    // 后端可能已在列表行内联返回卡片全文/历史；有则直接用，免二次请求。
    const inline = row as AuditDetail;
    if (inline.cardJson !== undefined || inline.authorHistory !== undefined) {
      setEnriched(inline);
      setEnrichUnavailable(false);
      setEnrichLoading(false);
      return;
    }

    // 否则拉取审核详情端点（G-ASSETS #10a 契约）。端点未就绪 → 优雅降级。
    setEnriched(null);
    setEnrichUnavailable(false);
    setEnrichLoading(true);
    adminFetch<AuditDetail>(`/admin/audit-queue/${row.id}`)
      .then((d) => {
        if (openIdRef.current !== row.id) return; // 期间已切换详情，丢弃过期响应
        setEnriched(d);
      })
      .catch(() => {
        if (openIdRef.current !== row.id) return;
        setEnrichUnavailable(true); // 端点未就绪 / 404 / 网络失败 → 降级
      })
      .finally(() => {
        if (openIdRef.current !== row.id) return;
        setEnrichLoading(false);
      });
  };

  const doAction = async (reason: string) => {
    if (!action) return;
    // reinstate 走请求体 + 理由必填（admin 专属的更高台阶）；approve/reject 沿用 query 形态。
    if (action.kind === 'reinstate' && !reason) {
      message.error('恢复放行必须填写理由');
      return;
    }
    setActing(true);
    try {
      if (action.kind === 'reinstate') {
        await adminFetch(`/admin/audit-queue/${action.row.id}/reinstate`, 'POST', { reason });
      } else {
        await adminFetch(
          `/admin/audit-queue/${action.row.id}/${action.kind}?reason=${encodeURIComponent(reason)}`,
          'POST',
        );
      }
      message.success(
        action.kind === 'approve' ? '已通过' : action.kind === 'reject' ? '已驳回' : '已恢复放行',
      );
      setAction(null);
      closeDetail();
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setActing(false);
    }
  };

  const columns: TableColumnsType<AuditRow> = [
    { title: '主体类型', dataIndex: 'subjectKind', key: 'subjectKind', width: 110, render: (v: string) => SUBJECT_TEXT[v] ?? v },
    {
      title: '主体 ID',
      dataIndex: 'subjectId',
      key: 'subjectId',
      // world_event 的 subjectId 是 domainEventId，跨世界重名（引擎按 patch-{revision}-ev-{seq} 生成）。
      // 不把世界摆出来，两条不同世界的队列项在列表上长得一模一样。
      render: (v: string, r: AuditRow) => (
        <Space direction="vertical" size={0}>
          <Typography.Text code>{v}</Typography.Text>
          {r.subjectWorldId && (
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>世界 {r.subjectWorldId}</Typography.Text>
          )}
        </Space>
      ),
    },
    {
      title: '机审结果',
      dataIndex: 'machineVerdict',
      key: 'machineVerdict',
      width: 110,
      render: (v: string) => {
        const t = VERDICT_TAG[v] ?? { color: 'default', text: v };
        return <Tag color={t.color}>{t.text}</Tag>;
      },
    },
    {
      title: '命中数',
      dataIndex: 'machineHits',
      key: 'hits',
      width: 80,
      render: (h: unknown) => (Array.isArray(h) ? h.length : 0),
    },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 100,
      render: (v: string) => {
        const t = STATUS_TAG[v] ?? { color: 'default', text: v };
        return <Tag color={t.color}>{t.text}</Tag>;
      },
    },
    { title: '审核人', dataIndex: 'reviewerId', key: 'reviewerId', render: (v: string | null) => v ?? '—' },
    { title: '提交时间', dataIndex: 'createdAt', key: 'createdAt', render: formatTime },
    {
      title: '操作',
      key: 'op',
      fixed: 'right',
      width: 90,
      render: (_, r) => (
        <Button size="small" onClick={() => openDetail(r)}>
          详情
        </Button>
      ),
    },
  ];

  const machineHits = enriched?.machineHits ?? detail?.machineHits;
  const subjectEvent = enriched?.subjectEvent ?? null;
  const isWorldEvent = detail?.subjectKind === WORLD_EVENT;
  // 🔴 台阶可见化：被人审驳回过的事件，reviewer 点「通过」必定 409（推翻人不是队列的本职）。
  // 与其让运营点了才知道，不如在按钮上就分开——口径同「已过审内容处置」的两档。
  const humanRejected = !!subjectEvent?.humanRejectedBefore;
  const canReinstate = isWorldEvent && detail?.status !== 'approved' && !subjectEvent?.unresolved;

  // Tab「审核队列」：原有筛选 + 列表（详情抽屉与理由 Modal 为浮层，保持在 Tabs 外）。
  const queuePane = (
    <>
      <Space style={{ marginBottom: 16 }}>
        <span>状态筛选：</span>
        <Select style={{ width: 160 }} value={status} onChange={setStatus} options={STATUS_OPTIONS} />
        <Button onClick={reload}>刷新</Button>
      </Space>

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={list.items}
        loading={list.loading}
        pagination={false}
        scroll={{ x: 1000 }}
      />

      {list.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={list.loadMore} loading={list.loading}>加载更多</Button>
        </div>
      )}
    </>
  );

  return (
    <div>
      <Typography.Title level={4}>内容审核</Typography.Title>
      <Tabs
        activeKey={tab}
        onChange={setTab}
        items={[
          { key: 'queue', label: '审核队列', children: queuePane },
          // 申诉复审：被驳回内容的申诉裁决（改判通过 / 维持原判），惰性挂载，切到该 Tab 才拉取。
          { key: 'appeals', label: '申诉复审', children: <AuditAppeals /> },
          // 已过审内容处置：前两个 Tab 够不着的那一半——内容一旦过审就离开了队列，
          // 此后出问题只能从这里再审 / 下架 / 恢复。
          {
            key: 'disposal',
            label: '已过审内容处置',
            children: (
              <ContentDisposal initialKind={deepLinkKind} initialSubjectId={deepLinkSubject} />
            ),
          },
        ]}
      />

      <Drawer
        title="审核详情"
        width={680}
        open={!!detail}
        onClose={closeDetail}
        extra={
          detail && (detail.status === 'open' || canReinstate) ? (
            <Space>
              {detail.status === 'open' && (
                <Button danger onClick={() => setAction({ row: detail, kind: 'reject' })}>驳回</Button>
              )}
              {detail.status === 'open' && (
                <Button
                  type="primary"
                  disabled={humanRejected}
                  title={humanRejected ? '该事件已被人审驳回过，「通过」只能推翻机器判定；请用「恢复放行」' : undefined}
                  onClick={() => setAction({ row: detail, kind: 'approve' })}
                >
                  通过
                </Button>
              )}
              {canReinstate && (
                <Button
                  type={humanRejected ? 'primary' : 'default'}
                  disabled={getRole() !== 'admin'}
                  title={getRole() !== 'admin' ? '恢复放行是 admin 专属（推翻人审终判的台阶更高）' : undefined}
                  onClick={() => setAction({ row: detail, kind: 'reinstate' })}
                >
                  恢复放行（admin）
                </Button>
              )}
            </Space>
          ) : undefined
        }
      >
        {detail && (
          <>
            <Descriptions
              column={1}
              bordered
              size="small"
              items={[
                { key: 'kind', label: '主体类型', children: SUBJECT_TEXT[detail.subjectKind] ?? detail.subjectKind },
                { key: 'sid', label: '主体 ID', children: <Typography.Text code copyable>{detail.subjectId}</Typography.Text> },
                ...(detail.subjectWorldId
                  ? [{ key: 'swid', label: '所属世界', children: <Typography.Text code copyable>{detail.subjectWorldId}</Typography.Text> }]
                  : []),
                { key: 'verdict', label: '机审结果', children: (VERDICT_TAG[detail.machineVerdict]?.text) ?? detail.machineVerdict },
                { key: 'status', label: '当前状态', children: (STATUS_TAG[detail.status]?.text) ?? detail.status },
                { key: 'reviewer', label: '审核人', children: detail.reviewerId ?? '—' },
                { key: 'reviewedAt', label: '审核时间', children: formatTime(detail.reviewedAt) },
                { key: 'createdAt', label: '提交时间', children: formatTime(detail.createdAt) },
              ]}
            />

            <Typography.Title level={5} style={{ marginTop: 20 }}>机审命中点</Typography.Title>
            <MachineHits hits={machineHits} />

            {isWorldEvent && subjectEvent && (
              <>
                <Typography.Title level={5} style={{ marginTop: 20 }}>事件内容</Typography.Title>
                <WorldEventPanel ev={subjectEvent} />
                {humanRejected && (
                  <Alert
                    type="warning"
                    showIcon
                    style={{ marginBottom: 16 }}
                    title="该事件已被人审驳回过"
                    description="「通过」只推翻机器收紧，对已被人驳回的事件不生效（会 409）。确需放行请用「恢复放行（admin）」——推翻人审终判是更高一档的权限，且理由必填。"
                  />
                )}
              </>
            )}

            {/* #10b 卡片全文 + 同作者历史（§10）。端点未就绪时优雅降级。 */}
            {enrichLoading && (
              <div style={{ marginTop: 20 }}>
                <Spin size="small" />{' '}
                <Typography.Text type="secondary">加载卡片全文与同作者历史…</Typography.Text>
              </div>
            )}

            {enrichUnavailable && (
              <Alert
                type="info"
                showIcon
                style={{ marginTop: 20 }}
                title="卡片全文需后端支持"
                description="审核详情端点（卡片全文 + 同作者历史）尚未就绪，当前仅展示机审命中与主体引用。端点上线后此处将自动呈现完整内容（§10）。"
              />
            )}

            {enriched && !enrichLoading && (
              <>
                <Typography.Title level={5} style={{ marginTop: 20 }}>卡片全文</Typography.Title>
                <CardFullText cardJson={enriched.cardJson} />

                <Typography.Title level={5} style={{ marginTop: 20 }}>同作者历史</Typography.Title>
                {Array.isArray(enriched.authorHistory) && enriched.authorHistory.length > 0 ? (
                  <Table
                    rowKey="id"
                    size="small"
                    columns={HISTORY_COLUMNS}
                    dataSource={enriched.authorHistory}
                    pagination={false}
                    scroll={{ x: 420 }}
                  />
                ) : (
                  <Typography.Text type="secondary">无同作者历史发布记录。</Typography.Text>
                )}

                <Typography.Paragraph type="secondary" style={{ marginTop: 20 }}>
                  卡片全文与同作者历史仅供人审裁决使用，访问记录已纳入审计（§10 / §14）。
                </Typography.Paragraph>
              </>
            )}
          </>
        )}
      </Drawer>

      <ReasonModal
        open={!!action}
        title={
          action?.kind === 'approve' ? '通过审核' : action?.kind === 'reject' ? '驳回审核' : '恢复放行（推翻人审终判）'
        }
        danger={action?.kind === 'reject'}
        okText={
          action?.kind === 'approve' ? '确认通过' : action?.kind === 'reject' ? '确认驳回' : '确认恢复放行'
        }
        placeholder={
          action?.kind === 'reinstate'
            ? '填写恢复理由（必填，1-500 字，将写入审计日志与风控留痕）'
            : '填写审核意见（可选，将写入审计日志）'
        }
        loading={acting}
        onOk={doAction}
        onCancel={() => setAction(null)}
      />
    </div>
  );
}
