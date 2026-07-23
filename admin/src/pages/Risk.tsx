// 风控：risk_events 检索（按 kind 筛选）+ cursor 分页 + 详情抽屉。
import { useEffect, useState } from 'react';
import { Button, Drawer, Descriptions, Input, Select, Space, Table, Tag, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { adminFetch } from '../api';
import { ErrorAlert, formatTime, usePagedList } from '../components/shared';

interface RiskRow {
  id: string;
  userId: string | null;
  worldId: string | null;
  kind: string;
  detail: unknown;
  createdAt: number;
}

// 常见风控类型（对齐 safety 模块；亦支持自定义输入）。
const KIND_OPTIONS = [
  { value: 'injection', label: '提示注入' },
  { value: 'forged_state', label: '伪造状态' },
  { value: 'unauthorized', label: '越权访问' },
  { value: 'batch_register', label: '批量注册' },
  { value: 'abuse', label: '滥用' },
];

const KIND_COLOR: Record<string, string> = {
  injection: 'red',
  forged_state: 'volcano',
  unauthorized: 'orange',
  batch_register: 'gold',
  abuse: 'magenta',
};

export default function Risk() {
  const [kind, setKind] = useState<string | undefined>(undefined);
  const [detail, setDetail] = useState<RiskRow | null>(null);

  const list = usePagedList<RiskRow>(async (cursor) => {
    const qs = new URLSearchParams();
    if (kind) qs.set('kind', kind);
    if (cursor) qs.set('cursor', cursor);
    qs.set('limit', '20');
    const res = await adminFetch<{ events: RiskRow[]; nextCursor: string | null }>(
      `/admin/risk-events?${qs.toString()}`,
    );
    return { items: res.events, nextCursor: res.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind]);

  const columns: TableColumnsType<RiskRow> = [
    { title: '类型', dataIndex: 'kind', key: 'kind', width: 130, render: (v: string) => <Tag color={KIND_COLOR[v] ?? 'default'}>{v}</Tag> },
    { title: '用户', dataIndex: 'userId', key: 'userId', render: (v: string | null) => (v ? <Typography.Text code>{v}</Typography.Text> : '—') },
    { title: '世界', dataIndex: 'worldId', key: 'worldId', render: (v: string | null) => (v ? <Typography.Text code>{v}</Typography.Text> : '—') },
    { title: '摘要', dataIndex: 'detail', key: 'detail', ellipsis: true, render: (v: unknown) => JSON.stringify(v).slice(0, 80) },
    { title: '时间', dataIndex: 'createdAt', key: 'createdAt', width: 170, render: formatTime },
    { title: '操作', key: 'op', width: 90, render: (_, r) => <Button size="small" onClick={() => setDetail(r)}>详情</Button> },
  ];

  return (
    <div>
      <Typography.Title level={4}>风控</Typography.Title>
      <Space style={{ marginBottom: 16 }} wrap>
        <span>类型筛选：</span>
        <Select
          style={{ width: 200 }}
          allowClear
          showSearch
          placeholder="全部类型（可自定义搜索）"
          value={kind}
          onChange={(v) => setKind(v)}
          options={KIND_OPTIONS}
        />
        <Input.Search
          style={{ width: 220 }}
          allowClear
          placeholder="或输入自定义 kind 精确筛选"
          onSearch={(v) => setKind(v.trim() || undefined)}
          enterButton="筛选"
        />
        <Button onClick={reload}>刷新</Button>
      </Space>

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table rowKey="id" size="small" columns={columns} dataSource={list.items} loading={list.loading} pagination={false} scroll={{ x: 900 }} />

      {list.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={list.loadMore} loading={list.loading}>加载更多</Button>
        </div>
      )}

      <Drawer title="风控事件详情" width={560} open={!!detail} onClose={() => setDetail(null)}>
        {detail && (
          <>
            <Descriptions
              column={1}
              bordered
              size="small"
              items={[
                { key: 'id', label: '事件 ID', children: <Typography.Text code copyable>{detail.id}</Typography.Text> },
                { key: 'kind', label: '类型', children: <Tag color={KIND_COLOR[detail.kind] ?? 'default'}>{detail.kind}</Tag> },
                { key: 'userId', label: '用户', children: detail.userId ?? '—' },
                { key: 'worldId', label: '世界', children: detail.worldId ?? '—' },
                { key: 'time', label: '时间', children: formatTime(detail.createdAt) },
              ]}
            />
            <Typography.Title level={5} style={{ marginTop: 20 }}>detail</Typography.Title>
            <pre style={{ background: '#0000000a', padding: 12, borderRadius: 6, overflow: 'auto', maxHeight: 360 }}>
              {JSON.stringify(detail.detail, null, 2)}
            </pre>
          </>
        )}
      </Drawer>
    </div>
  );
}
