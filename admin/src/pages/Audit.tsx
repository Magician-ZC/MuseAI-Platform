// 内容审核：Tab「审核队列」（机审预标注 + 人审 + 详情抽屉 + approve/reject）
// + Tab「申诉复审」（components/AuditAppeals，被驳回内容的申诉裁决）。
// #10b（§10）：详情抽屉展示「卡片全文 cardJson + 机审命中点 + 同作者历史」。
// 卡片全文/历史由审核详情端点（G-ASSETS #10a 契约）提供；端点未就绪时优雅降级——
// 仍展示机审命中并标注「卡片全文需后端支持」，不崩溃。
import { useEffect, useRef, useState } from 'react';
import { Alert, Button, Descriptions, Drawer, message, Select, Space, Spin, Table, Tabs, Tag, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { adminFetch } from '../api';
import AuditAppeals from '../components/AuditAppeals';
import { ErrorAlert, formatTime, friendlyError, ReasonModal, usePagedList } from '../components/shared';

interface AuditRow {
  id: string;
  subjectKind: string;
  subjectId: string;
  machineVerdict: string;
  machineHits: unknown;
  status: string;
  reviewerId: string | null;
  reviewedAt: number | null;
  createdAt: number;
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
}

const STATUS_OPTIONS = [
  { value: 'open', label: '待审核' },
  { value: 'approved', label: '已通过' },
  { value: 'rejected', label: '已驳回' },
];

const SUBJECT_TEXT: Record<string, string> = {
  character: '角色卡',
  template: '世界模板',
  intervention: '干预文本',
  event: '世界事件',
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
  const [status, setStatus] = useState('open');
  const [detail, setDetail] = useState<AuditRow | null>(null);
  const [enriched, setEnriched] = useState<AuditDetail | null>(null);
  const [enrichLoading, setEnrichLoading] = useState(false);
  const [enrichUnavailable, setEnrichUnavailable] = useState(false);
  const [action, setAction] = useState<{ row: AuditRow; kind: 'approve' | 'reject' } | null>(null);
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
    setActing(true);
    try {
      await adminFetch(
        `/admin/audit-queue/${action.row.id}/${action.kind}?reason=${encodeURIComponent(reason)}`,
        'POST',
      );
      message.success(action.kind === 'approve' ? '已通过' : '已驳回');
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
    { title: '主体 ID', dataIndex: 'subjectId', key: 'subjectId', render: (v: string) => <Typography.Text code>{v}</Typography.Text> },
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
        defaultActiveKey="queue"
        items={[
          { key: 'queue', label: '审核队列', children: queuePane },
          // 申诉复审：被驳回内容的申诉裁决（改判通过 / 维持原判），惰性挂载，切到该 Tab 才拉取。
          { key: 'appeals', label: '申诉复审', children: <AuditAppeals /> },
        ]}
      />

      <Drawer
        title="审核详情"
        width={680}
        open={!!detail}
        onClose={closeDetail}
        extra={
          detail?.status === 'open' && (
            <Space>
              <Button danger onClick={() => setAction({ row: detail, kind: 'reject' })}>驳回</Button>
              <Button type="primary" onClick={() => setAction({ row: detail, kind: 'approve' })}>通过</Button>
            </Space>
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
                { key: 'kind', label: '主体类型', children: SUBJECT_TEXT[detail.subjectKind] ?? detail.subjectKind },
                { key: 'sid', label: '主体 ID', children: <Typography.Text code copyable>{detail.subjectId}</Typography.Text> },
                { key: 'verdict', label: '机审结果', children: (VERDICT_TAG[detail.machineVerdict]?.text) ?? detail.machineVerdict },
                { key: 'status', label: '当前状态', children: (STATUS_TAG[detail.status]?.text) ?? detail.status },
                { key: 'reviewer', label: '审核人', children: detail.reviewerId ?? '—' },
                { key: 'reviewedAt', label: '审核时间', children: formatTime(detail.reviewedAt) },
                { key: 'createdAt', label: '提交时间', children: formatTime(detail.createdAt) },
              ]}
            />

            <Typography.Title level={5} style={{ marginTop: 20 }}>机审命中点</Typography.Title>
            <MachineHits hits={machineHits} />

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
                message="卡片全文需后端支持"
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
        title={action?.kind === 'approve' ? '通过审核' : '驳回审核'}
        danger={action?.kind === 'reject'}
        okText={action?.kind === 'approve' ? '确认通过' : '确认驳回'}
        placeholder="填写审核意见（可选，将写入审计日志）"
        loading={acting}
        onOk={doAction}
        onCancel={() => setAction(null)}
      />
    </div>
  );
}
