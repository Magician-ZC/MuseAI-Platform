// 内容审核：审核队列（机审预标注 + 人审）+ 详情抽屉（机审命中全文）+ approve/reject。
// 说明：S6 队列返回主体引用与机审命中；主体原文需另行授权，抽屉展示可得信息并给出脱敏说明。
import { useEffect, useState } from 'react';
import { Button, Descriptions, Drawer, message, Select, Space, Table, Tag, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { adminFetch } from '../api';
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

export default function Audit() {
  const [status, setStatus] = useState('open');
  const [detail, setDetail] = useState<AuditRow | null>(null);
  const [action, setAction] = useState<{ row: AuditRow; kind: 'approve' | 'reject' } | null>(null);
  const [acting, setActing] = useState(false);

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
      setDetail(null);
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
        <Button size="small" onClick={() => setDetail(r)}>
          详情
        </Button>
      ),
    },
  ];

  return (
    <div>
      <Typography.Title level={4}>内容审核</Typography.Title>
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

      <Drawer
        title="审核详情"
        width={640}
        open={!!detail}
        onClose={() => setDetail(null)}
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
            <MachineHits hits={detail.machineHits} />
            <Typography.Paragraph type="secondary" style={{ marginTop: 20 }}>
              主体原文（如角色卡全文 / 同作者历史）默认脱敏，查看必要内容需另行授权（§10）。此处仅呈现机审命中与主体引用，供人审裁决。
            </Typography.Paragraph>
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
