// 客服与工单：data_requests（数据导出/删除）检索 + run 执行（理由走 query，写审计）。
import { useEffect, useState } from 'react';
import { Button, message, Select, Space, Table, Tag, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { adminFetch } from '../api';
import { ErrorAlert, formatTime, friendlyError, ReasonModal, usePagedList } from '../components/shared';

interface DataRequestRow {
  id: string;
  userId: string;
  kind: string;
  status: string;
  resultKey: string | null;
  createdAt: number;
  updatedAt: number;
}

const STATUS_TAG: Record<string, { color: string; text: string }> = {
  pending: { color: 'blue', text: '待处理' },
  running: { color: 'gold', text: '执行中' },
  done: { color: 'green', text: '已完成' },
  failed: { color: 'red', text: '失败' },
};

const KIND_TEXT: Record<string, string> = { export: '数据导出', delete: '数据删除' };

export default function Tickets() {
  const [status, setStatus] = useState<string | undefined>(undefined);
  const [runTarget, setRunTarget] = useState<DataRequestRow | null>(null);
  const [running, setRunning] = useState(false);

  const list = usePagedList<DataRequestRow>(async (cursor) => {
    const qs = new URLSearchParams();
    if (status) qs.set('status', status);
    if (cursor) qs.set('cursor', cursor);
    qs.set('limit', '20');
    const res = await adminFetch<{ requests: DataRequestRow[]; nextCursor: string | null }>(
      `/admin/data-requests?${qs.toString()}`,
    );
    return { items: res.requests, nextCursor: res.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  const doRun = async (reason: string) => {
    if (!runTarget) return;
    setRunning(true);
    try {
      await adminFetch(`/admin/data-requests/${runTarget.id}/run?reason=${encodeURIComponent(reason)}`, 'POST');
      message.success('工单已执行');
      setRunTarget(null);
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setRunning(false);
    }
  };

  const columns: TableColumnsType<DataRequestRow> = [
    { title: '工单 ID', dataIndex: 'id', key: 'id', render: (v: string) => <Typography.Text code>{v}</Typography.Text> },
    { title: '用户', dataIndex: 'userId', key: 'userId', render: (v: string) => <Typography.Text code>{v}</Typography.Text> },
    { title: '类型', dataIndex: 'kind', key: 'kind', width: 100, render: (v: string) => KIND_TEXT[v] ?? v },
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
    { title: '结果', dataIndex: 'resultKey', key: 'resultKey', render: (v: string | null) => v ?? '—' },
    { title: '创建时间', dataIndex: 'createdAt', key: 'createdAt', width: 170, render: formatTime },
    { title: '更新时间', dataIndex: 'updatedAt', key: 'updatedAt', width: 170, render: formatTime },
    {
      title: '操作',
      key: 'op',
      fixed: 'right',
      width: 90,
      render: (_, r) => (
        <Button size="small" type="primary" disabled={r.status === 'done'} onClick={() => setRunTarget(r)}>
          执行
        </Button>
      ),
    },
  ];

  return (
    <div>
      <Typography.Title level={4}>客服与工单</Typography.Title>
      <Space style={{ marginBottom: 16 }} wrap>
        <span>状态筛选：</span>
        <Select
          style={{ width: 160 }}
          allowClear
          placeholder="全部状态"
          value={status}
          onChange={(v) => setStatus(v)}
          options={[
            { value: 'pending', label: '待处理' },
            { value: 'running', label: '执行中' },
            { value: 'done', label: '已完成' },
          ]}
        />
        <Button onClick={reload}>刷新</Button>
      </Space>

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table rowKey="id" size="small" columns={columns} dataSource={list.items} loading={list.loading} pagination={false} scroll={{ x: 1100 }} />

      {list.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={list.loadMore} loading={list.loading}>加载更多</Button>
        </div>
      )}

      <ReasonModal
        open={!!runTarget}
        title={runTarget ? `执行工单（${KIND_TEXT[runTarget.kind] ?? runTarget.kind}）` : '执行工单'}
        okText="确认执行"
        placeholder="执行理由（可选，写入审计日志）"
        loading={running}
        onOk={doRun}
        onCancel={() => setRunTarget(null)}
      />
    </div>
  );
}
