// 用户管理：检索（脱敏由后端处理）+ cursor 分页 + 封禁/解封（理由走 query，写审计）。
import { useEffect, useState } from 'react';
import { Button, Input, message, Space, Table, Tag, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { adminFetch } from '../api';
import { ErrorAlert, formatTime, friendlyError, ReasonModal, usePagedList } from '../components/shared';

interface AdminUserRow {
  id: string;
  nickname: string;
  phone: string | null;
  email: string | null;
  ageDeclared: number;
  role: string;
  status: string;
  verificationStatus: string;
  createdAt: number;
}

const STATUS_TAG: Record<string, { color: string; text: string }> = {
  active: { color: 'green', text: '正常' },
  banned: { color: 'red', text: '已封禁' },
};

const VERIFY_TAG: Record<string, string> = {
  none: '未验证',
  pending: '验证中',
  verified: '已验证',
  rejected: '验证驳回',
};

export default function Users() {
  const [query, setQuery] = useState('');
  const [action, setAction] = useState<{ id: string; kind: 'ban' | 'unban'; name: string } | null>(null);
  const [acting, setActing] = useState(false);

  const list = usePagedList<AdminUserRow>(async (cursor) => {
    const qs = new URLSearchParams();
    if (query.trim()) qs.set('query', query.trim());
    if (cursor) qs.set('cursor', cursor);
    qs.set('limit', '20');
    const res = await adminFetch<{ users: AdminUserRow[]; nextCursor: string | null }>(
      `/admin/users?${qs.toString()}`,
    );
    return { items: res.users, nextCursor: res.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    reload();
    // 首屏加载；搜索通过按钮触发 reload。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const doAction = async (reason: string) => {
    if (!action) return;
    setActing(true);
    try {
      await adminFetch(
        `/admin/users/${action.id}/${action.kind}?reason=${encodeURIComponent(reason)}`,
        'POST',
      );
      message.success(action.kind === 'ban' ? '已封禁' : '已解封');
      setAction(null);
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setActing(false);
    }
  };

  const columns: TableColumnsType<AdminUserRow> = [
    { title: '昵称', dataIndex: 'nickname', key: 'nickname', render: (v: string) => v || '—' },
    { title: 'ID', dataIndex: 'id', key: 'id', render: (v: string) => <Typography.Text code copyable>{v}</Typography.Text> },
    { title: '手机号', dataIndex: 'phone', key: 'phone', render: (v: string | null) => v ?? '—' },
    { title: '邮箱', dataIndex: 'email', key: 'email', render: (v: string | null) => v ?? '—' },
    { title: '声明年龄', dataIndex: 'ageDeclared', key: 'ageDeclared', width: 90 },
    { title: '角色', dataIndex: 'role', key: 'role', width: 90, render: (v: string) => <Tag>{v}</Tag> },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 90,
      render: (v: string) => {
        const t = STATUS_TAG[v] ?? { color: 'default', text: v };
        return <Tag color={t.color}>{t.text}</Tag>;
      },
    },
    {
      title: '实名状态',
      dataIndex: 'verificationStatus',
      key: 'verificationStatus',
      width: 100,
      render: (v: string) => VERIFY_TAG[v] ?? v,
    },
    { title: '注册时间', dataIndex: 'createdAt', key: 'createdAt', render: formatTime },
    {
      title: '操作',
      key: 'op',
      fixed: 'right',
      width: 100,
      render: (_, r) =>
        r.status === 'banned' ? (
          <Button size="small" onClick={() => setAction({ id: r.id, kind: 'unban', name: r.nickname })}>
            解封
          </Button>
        ) : (
          <Button size="small" danger onClick={() => setAction({ id: r.id, kind: 'ban', name: r.nickname })}>
            封禁
          </Button>
        ),
    },
  ];

  return (
    <div>
      <Typography.Title level={4}>用户管理</Typography.Title>
      <Space style={{ marginBottom: 16 }}>
        <Input.Search
          allowClear
          style={{ width: 320 }}
          placeholder="按昵称 / 手机号 / 邮箱 / ID 检索"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onSearch={() => reload()}
          enterButton="搜索"
        />
        <Button onClick={() => { setQuery(''); setTimeout(reload, 0); }}>重置</Button>
      </Space>

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={list.items}
        loading={list.loading}
        pagination={false}
        scroll={{ x: 1100 }}
      />

      {list.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={list.loadMore} loading={list.loading}>
            加载更多
          </Button>
        </div>
      )}

      <ReasonModal
        open={!!action}
        title={action?.kind === 'ban' ? `封禁用户 ${action?.name || ''}` : `解封用户 ${action?.name || ''}`}
        danger={action?.kind === 'ban'}
        okText={action?.kind === 'ban' ? '确认封禁' : '确认解封'}
        loading={acting}
        onOk={doAction}
        onCancel={() => setAction(null)}
      />
    </div>
  );
}
