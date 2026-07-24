// 申诉复审（audit 模块内分区，reviewer/admin）：
// GET /admin/appeals?status=… 列表 + POST /admin/appeals/{id}/resolve {decision, reason}。
// 改判通过（overturn）只翻转裁决时仍处于「已驳回」的维度（卡优先、其次头像），维持原判（uphold）不动主体；
// 复审理由必填（1..500 字符），写入申诉行与审计日志；非 pending 重复裁决后端返回 409。
import { useEffect, useRef, useState } from 'react';
import { Button, Input, message, Modal, Select, Space, Table, Tag, Tooltip, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { AdminApiError, adminFetch } from '../api';
import { ErrorAlert, formatTime, friendlyError } from './shared';

/** 主体摘要（当前仅 character）；主体已删除时整体为 null，申诉行仍可见。 */
interface AppealSubject {
  name: string;
  moderation: string;
  avatarModeration: string | null;
  ownerId: string;
}

interface AppealRow {
  id: string;
  subjectKind: string;
  subjectId: string;
  ownerId: string;
  appealText: string;
  status: string;
  resolutionReason: string | null;
  reviewerId: string | null;
  createdAt: number;
  resolvedAt: number | null;
  subject: AppealSubject | null;
}

const STATUS_OPTIONS = [
  { value: 'pending', label: '待复审' },
  { value: 'overturned', label: '已改判' },
  { value: 'upheld', label: '已维持' },
  { value: 'all', label: '全部' },
];

const APPEAL_STATUS_TAG: Record<string, { color: string; text: string }> = {
  pending: { color: 'blue', text: '待复审' },
  overturned: { color: 'green', text: '已改判' },
  upheld: { color: 'default', text: '已维持' },
};

const SUBJECT_TEXT: Record<string, string> = {
  character: '角色卡',
  template: '世界模板',
  intervention: '干预文本',
  event: '世界事件',
};

/** 卡 / 头像审核维度状态（cloud_characters.moderation / avatar_moderation）。 */
const MOD_TAG: Record<string, { color: string; text: string }> = {
  approved: { color: 'green', text: '已通过' },
  rejected: { color: 'red', text: '已驳回' },
  pending: { color: 'gold', text: '待审核' },
};

function modTag(prefix: string, value: string) {
  const t = MOD_TAG[value] ?? { color: 'default', text: value };
  return <Tag color={t.color}>{`${prefix}·${t.text}`}</Tag>;
}

export default function AuditAppeals() {
  const [status, setStatus] = useState('pending');
  const [items, setItems] = useState<AppealRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [action, setAction] = useState<{ row: AppealRow; decision: 'overturn' | 'uphold' } | null>(null);
  const [reason, setReason] = useState('');
  const [acting, setActing] = useState(false);
  // 请求序号：筛选快速切换时丢弃过期响应。
  const reqRef = useRef(0);

  const load = async () => {
    const seq = ++reqRef.current;
    setLoading(true);
    setError(null);
    try {
      const res = await adminFetch<{ items: AppealRow[] }>(`/admin/appeals?status=${status}`);
      if (seq !== reqRef.current) return;
      setItems(res.items);
    } catch (e) {
      if (seq !== reqRef.current) return;
      setError(friendlyError(e));
      setItems([]);
    } finally {
      if (seq === reqRef.current) setLoading(false);
    }
  };

  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  const openAction = (row: AppealRow, decision: 'overturn' | 'uphold') => {
    setReason('');
    setAction({ row, decision });
  };

  const doResolve = async () => {
    if (!action) return;
    const text = reason.trim();
    if (!text) {
      message.warning('请填写复审理由（必填，最多 500 字）');
      return;
    }
    setActing(true);
    try {
      await adminFetch(`/admin/appeals/${action.row.id}/resolve`, 'POST', {
        decision: action.decision,
        reason: text,
      });
      message.success(action.decision === 'overturn' ? '已改判通过' : '已维持原判');
      setAction(null);
      load();
    } catch (e) {
      // 409（已被处理，不可重复裁决）/ 400（参数校验）直接展示服务端文案；409 后刷新列表同步最新状态。
      if (e instanceof AdminApiError && (e.code === 'conflict' || e.code === 'bad_request')) {
        message.error(e.message || friendlyError(e));
        if (e.code === 'conflict') {
          setAction(null);
          load();
        }
      } else {
        message.error(friendlyError(e));
      }
    } finally {
      setActing(false);
    }
  };

  const columns: TableColumnsType<AppealRow> = [
    { title: '申诉时间', dataIndex: 'createdAt', key: 'createdAt', width: 165, render: formatTime },
    {
      title: '主体名',
      key: 'subject',
      width: 180,
      render: (_, r) => (
        <Space direction="vertical" size={0}>
          {r.subject ? (
            <Typography.Text>{r.subject.name || '（未命名）'}</Typography.Text>
          ) : (
            <Tag>已删除</Tag>
          )}
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            {SUBJECT_TEXT[r.subjectKind] ?? r.subjectKind}
          </Typography.Text>
        </Space>
      ),
    },
    {
      title: '卡 / 头像审核态',
      key: 'moderation',
      width: 190,
      render: (_, r) =>
        r.subject ? (
          <Space size={4} wrap>
            {modTag('卡', r.subject.moderation)}
            {r.subject.avatarModeration != null && modTag('头像', r.subject.avatarModeration)}
          </Space>
        ) : (
          '—'
        ),
    },
    {
      title: '申诉理由',
      dataIndex: 'appealText',
      key: 'appealText',
      ellipsis: { showTitle: false },
      render: (v: string) => (
        <Tooltip
          placement="topLeft"
          title={<div style={{ maxHeight: 300, overflow: 'auto', whiteSpace: 'pre-wrap' }}>{v}</div>}
        >
          <span>{v}</span>
        </Tooltip>
      ),
    },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 100,
      render: (v: string, r) => {
        const t = APPEAL_STATUS_TAG[v] ?? { color: 'default', text: v };
        const tag = <Tag color={t.color}>{t.text}</Tag>;
        if (!r.resolutionReason) return tag;
        return (
          <Tooltip
            placement="topLeft"
            title={`复审理由：${r.resolutionReason}${r.reviewerId ? `（复审人 ${r.reviewerId}，${formatTime(r.resolvedAt)}）` : ''}`}
          >
            {tag}
          </Tooltip>
        );
      },
    },
    {
      title: '操作',
      key: 'op',
      fixed: 'right',
      width: 190,
      render: (_, r) =>
        r.status === 'pending' ? (
          <Space>
            <Button size="small" type="primary" onClick={() => openAction(r, 'overturn')}>
              改判通过
            </Button>
            <Button size="small" onClick={() => openAction(r, 'uphold')}>
              维持原判
            </Button>
          </Space>
        ) : (
          <Typography.Text type="secondary">—</Typography.Text>
        ),
    },
  ];

  return (
    <div>
      <Space style={{ marginBottom: 16 }}>
        <span>状态筛选：</span>
        <Select style={{ width: 160 }} value={status} onChange={setStatus} options={STATUS_OPTIONS} />
        <Button onClick={() => load()} loading={loading}>刷新</Button>
      </Space>

      {error && <ErrorAlert message={error} onRetry={() => load()} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={items}
        loading={loading}
        pagination={{ pageSize: 20, hideOnSinglePage: true, showSizeChanger: false }}
        scroll={{ x: 1000 }}
      />

      <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>
        改判通过仅放行裁决时仍处于「已驳回」的维度（卡与头像分开审、分开改判，卡被驳回优先放行卡）；
        维持原判不改变主体审核状态。复审理由写入申诉记录与审计日志，裁决后不可重复处理。
      </Typography.Paragraph>

      <Modal
        open={!!action}
        title={action?.decision === 'overturn' ? '改判通过' : '维持原判'}
        okText={action?.decision === 'overturn' ? '确认改判' : '确认维持'}
        cancelText="取消"
        confirmLoading={acting}
        okButtonProps={{ disabled: !reason.trim() }}
        onOk={doResolve}
        onCancel={() => setAction(null)}
      >
        <Typography.Paragraph type="secondary" style={{ marginBottom: 8 }}>
          {action?.decision === 'overturn'
            ? '改判通过将放行主体当前被驳回的维度（卡被驳回则放行卡；否则仅放行被驳回的头像），并记录审计日志。'
            : '维持原判不改变主体审核状态，仅记录复审结论与审计日志。'}
        </Typography.Paragraph>
        <Input.TextArea
          rows={4}
          maxLength={500}
          showCount
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder="复审理由（必填，最多 500 字，将写入申诉记录与审计日志）"
        />
      </Modal>
    </div>
  );
}
