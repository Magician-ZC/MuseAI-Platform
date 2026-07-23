// 世界运营：活跃世界监控 + 脱敏诊断 + 暂停/恢复 + 官方建房 + 世界模板库。
import { useEffect, useState } from 'react';
import {
  Alert,
  Button,
  Descriptions,
  Drawer,
  Form,
  Input,
  InputNumber,
  message,
  Modal,
  Select,
  Space,
  Table,
  Tabs,
  Tag,
  Typography,
} from 'antd';
import type { TableColumnsType } from 'antd';
import { adminFetch } from '../api';
import { ErrorAlert, formatNumber, formatTime, friendlyError, ReasonModal, usePagedList } from '../components/shared';

// ---------------- 类型 ----------------

interface WorldRow {
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

interface Diagnostics {
  world: Record<string, unknown> & { id: string; title: string; status: string };
  ticks: TickMeta[];
  budget: {
    dailyTokenBudget: number;
    dailyCnyBudgetCents: number;
    spentTokensToday: number;
    budgetDay: string;
    fused: boolean;
  } | null;
  riskEventCounts: { kind: string; count: number }[];
  eventStats: { total: number; byModeration: { moderation: string; count: number }[] };
  redactionNote: string;
}

interface TemplateRow {
  id: string;
  title: string;
  roomType: string;
  skeletonJson: unknown;
  admissionJson: unknown;
  official: boolean;
  version: number;
  moderation: string;
  createdAt: number;
}

const ROOM_TYPE_TEXT: Record<string, string> = { idle: '放置世界', chapter: '章节房', arena: '赛事房' };
const WORLD_STATUS_TAG: Record<string, { color: string; text: string }> = {
  open: { color: 'blue', text: '开放' },
  running: { color: 'green', text: '运行中' },
  paused: { color: 'orange', text: '已暂停' },
  ended: { color: 'default', text: '已结束' },
};
const MOD_TAG: Record<string, { color: string; text: string }> = {
  pending: { color: 'blue', text: '待审核' },
  approved: { color: 'green', text: '已通过' },
  rejected: { color: 'red', text: '已驳回' },
};

// ================= 世界监控 =================

function WorldsMonitor() {
  const [status, setStatus] = useState<string | undefined>(undefined);
  const [action, setAction] = useState<{ row: WorldRow; kind: 'pause' | 'resume' } | null>(null);
  const [acting, setActing] = useState(false);
  const [diagId, setDiagId] = useState<string | null>(null);
  const [diag, setDiag] = useState<Diagnostics | null>(null);
  const [diagLoading, setDiagLoading] = useState(false);
  const [diagError, setDiagError] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [form] = Form.useForm();

  const list = usePagedList<WorldRow>(async (cursor) => {
    const qs = new URLSearchParams();
    if (status) qs.set('status', status);
    if (cursor) qs.set('cursor', cursor);
    qs.set('limit', '20');
    const res = await adminFetch<{ worlds: WorldRow[]; nextCursor: string | null }>(
      `/admin/worlds?${qs.toString()}`,
    );
    return { items: res.worlds, nextCursor: res.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  const loadDiagnostics = async (id: string) => {
    setDiagId(id);
    setDiag(null);
    setDiagError(null);
    setDiagLoading(true);
    try {
      const res = await adminFetch<Diagnostics>(`/admin/worlds/${id}/diagnostics`);
      setDiag(res);
    } catch (e) {
      setDiagError(friendlyError(e));
    } finally {
      setDiagLoading(false);
    }
  };

  const doAction = async (reason: string) => {
    if (!action) return;
    setActing(true);
    try {
      await adminFetch(
        `/admin/worlds/${action.row.id}/${action.kind}?reason=${encodeURIComponent(reason)}`,
        'POST',
      );
      message.success(action.kind === 'pause' ? '已暂停' : '已恢复');
      setAction(null);
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setActing(false);
    }
  };

  const submitCreate = async () => {
    let values: Record<string, unknown>;
    try {
      values = await form.validateFields();
    } catch {
      return;
    }
    setCreating(true);
    try {
      const res = await adminFetch<{ worldId: string }>('/admin/worlds', 'POST', values);
      message.success(`官方世界已创建：${res.worldId}`);
      setCreateOpen(false);
      form.resetFields();
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setCreating(false);
    }
  };

  const columns: TableColumnsType<WorldRow> = [
    { title: '标题', dataIndex: 'title', key: 'title' },
    { title: '房型', dataIndex: 'roomType', key: 'roomType', width: 100, render: (v: string) => ROOM_TYPE_TEXT[v] ?? v },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 90,
      render: (v: string) => {
        const t = WORLD_STATUS_TAG[v] ?? { color: 'default', text: v };
        return <Tag color={t.color}>{t.text}</Tag>;
      },
    },
    { title: '可见性', dataIndex: 'visibility', key: 'visibility', width: 90 },
    {
      title: '预算(今日/上限)',
      key: 'budget',
      width: 150,
      render: (_, r) => `${formatNumber(r.spentTokensToday)} / ${r.dailyTokenBudget ? formatNumber(r.dailyTokenBudget) : '∞'}`,
    },
    {
      title: '熔断',
      dataIndex: 'fused',
      key: 'fused',
      width: 80,
      render: (v: boolean) => (v ? <Tag color="red">已熔断</Tag> : <Tag color="green">正常</Tag>),
    },
    { title: 'tick/日', dataIndex: 'tickPerDay', key: 'tickPerDay', width: 80 },
    { title: '引擎版本', dataIndex: 'engineVersion', key: 'engineVersion', width: 110 },
    { title: '创建时间', dataIndex: 'createdAt', key: 'createdAt', render: formatTime },
    {
      title: '操作',
      key: 'op',
      fixed: 'right',
      width: 170,
      render: (_, r) => (
        <Space size="small">
          <Button size="small" onClick={() => loadDiagnostics(r.id)}>诊断</Button>
          {r.status === 'paused' ? (
            <Button size="small" type="primary" onClick={() => setAction({ row: r, kind: 'resume' })}>恢复</Button>
          ) : (
            <Button
              size="small"
              disabled={!['open', 'running'].includes(r.status)}
              onClick={() => setAction({ row: r, kind: 'pause' })}
            >
              暂停
            </Button>
          )}
        </Space>
      ),
    },
  ];

  return (
    <div>
      <Space style={{ marginBottom: 16 }} wrap>
        <span>状态筛选：</span>
        <Select
          style={{ width: 160 }}
          allowClear
          placeholder="全部状态"
          value={status}
          onChange={(v) => setStatus(v)}
          options={[
            { value: 'open', label: '开放' },
            { value: 'running', label: '运行中' },
            { value: 'paused', label: '已暂停' },
            { value: 'ended', label: '已结束' },
          ]}
        />
        <Button onClick={reload}>刷新</Button>
        <Button type="primary" onClick={() => setCreateOpen(true)}>建官方世界</Button>
      </Space>

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={list.items}
        loading={list.loading}
        pagination={false}
        scroll={{ x: 1200 }}
      />

      {list.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={list.loadMore} loading={list.loading}>加载更多</Button>
        </div>
      )}

      {/* 脱敏诊断抽屉 */}
      <Drawer title="世界诊断（脱敏）" width={720} open={!!diagId} onClose={() => setDiagId(null)} loading={diagLoading}>
        {diagError && <ErrorAlert message={diagError} onRetry={() => diagId && loadDiagnostics(diagId)} />}
        {diag && (
          <>
            <Alert type="warning" showIcon style={{ marginBottom: 16 }} message={diag.redactionNote} />
            <Descriptions
              title="世界元数据"
              column={2}
              bordered
              size="small"
              items={[
                { key: 'id', label: 'ID', children: String(diag.world.id) },
                { key: 'title', label: '标题', children: String(diag.world.title) },
                { key: 'status', label: '状态', children: (WORLD_STATUS_TAG[diag.world.status]?.text) ?? String(diag.world.status) },
                { key: 'roomType', label: '房型', children: ROOM_TYPE_TEXT[String(diag.world.roomType)] ?? String(diag.world.roomType) },
                { key: 'rev', label: '状态修订', children: String(diag.world.stateRevision ?? '—') },
                { key: 'engine', label: '引擎版本', children: String(diag.world.engineVersion ?? '—') },
                { key: 'prompt', label: 'Prompt 版本', children: String(diag.world.promptSetVersion ?? '—') },
                { key: 'route', label: '模型路由', children: String(diag.world.modelRouteVersion ?? '—') },
              ]}
            />

            <Descriptions
              title="预算 / 熔断"
              column={2}
              bordered
              size="small"
              style={{ marginTop: 20 }}
              items={
                diag.budget
                  ? [
                      { key: 'tb', label: 'token 日预算', children: formatNumber(diag.budget.dailyTokenBudget) },
                      { key: 'sp', label: '今日已耗 token', children: formatNumber(diag.budget.spentTokensToday) },
                      { key: 'cny', label: '人民币日预算(分)', children: formatNumber(diag.budget.dailyCnyBudgetCents) },
                      { key: 'day', label: '预算日', children: diag.budget.budgetDay },
                      { key: 'fused', label: '熔断', children: diag.budget.fused ? '已熔断' : '正常' },
                    ]
                  : [{ key: 'none', label: '预算', children: '未配置' }]
              }
            />

            <Typography.Title level={5} style={{ marginTop: 20 }}>最近 tick（含错误码）</Typography.Title>
            <Table
              rowKey="tickNo"
              size="small"
              pagination={false}
              dataSource={diag.ticks}
              columns={[
                { title: 'tick', dataIndex: 'tickNo', key: 'tickNo', width: 70 },
                { title: '状态', dataIndex: 'status', key: 'status', width: 90, render: (v: string) => <Tag color={v === 'done' ? 'green' : v === 'failed' ? 'red' : 'default'}>{v}</Tag> },
                { title: '错误码', dataIndex: 'error', key: 'error', render: (v: string | null) => v ?? '—' },
                { title: 'token', dataIndex: 'costTokens', key: 'costTokens', width: 90, render: formatNumber },
                { title: '完成时间', dataIndex: 'finishedAt', key: 'finishedAt', render: formatTime },
              ]}
            />

            <Typography.Title level={5} style={{ marginTop: 20 }}>风控命中计数</Typography.Title>
            {diag.riskEventCounts.length ? (
              <Space wrap>
                {diag.riskEventCounts.map((r) => (
                  <Tag key={r.kind} color="volcano">{r.kind}: {r.count}</Tag>
                ))}
              </Space>
            ) : (
              <Typography.Text type="secondary">无风控命中</Typography.Text>
            )}

            <Typography.Title level={5} style={{ marginTop: 20 }}>事件审核态（共 {diag.eventStats.total}）</Typography.Title>
            <Space wrap>
              {diag.eventStats.byModeration.map((m) => (
                <Tag key={m.moderation}>{m.moderation}: {m.count}</Tag>
              ))}
            </Space>
          </>
        )}
      </Drawer>

      {/* 建官方世界 */}
      <Modal
        title="创建官方放置世界"
        open={createOpen}
        onOk={submitCreate}
        confirmLoading={creating}
        onCancel={() => setCreateOpen(false)}
        okText="创建"
        cancelText="取消"
        width={560}
      >
        <Form
          form={form}
          layout="vertical"
          initialValues={{
            templateVersion: 1,
            roomType: 'idle',
            visibility: 'official',
            memberLimit: 10,
            tickPerDay: 3,
            dailyTokenBudget: 0,
            dailyCnyBudgetCents: 0,
            status: 'open',
          }}
        >
          <Form.Item name="title" label="世界标题" rules={[{ required: true, message: '请输入标题' }]}>
            <Input placeholder="官方世界标题" />
          </Form.Item>
          <Space size="large" style={{ display: 'flex' }}>
            <Form.Item name="templateId" label="模板 ID" rules={[{ required: true, message: '请输入模板 ID' }]} style={{ flex: 1 }}>
              <Input placeholder="tpl_..." />
            </Form.Item>
            <Form.Item name="templateVersion" label="模板版本">
              <InputNumber min={1} style={{ width: 120 }} />
            </Form.Item>
          </Space>
          <Space size="large" style={{ display: 'flex' }}>
            <Form.Item name="roomType" label="房型" style={{ flex: 1 }}>
              <Select options={[
                { value: 'idle', label: '放置世界' },
                { value: 'chapter', label: '章节房' },
                { value: 'arena', label: '赛事房' },
              ]} />
            </Form.Item>
            <Form.Item name="visibility" label="可见性" style={{ flex: 1 }}>
              <Select options={[
                { value: 'official', label: '官方' },
                { value: 'public', label: '公开' },
                { value: 'private', label: '私有' },
              ]} />
            </Form.Item>
            <Form.Item name="status" label="初始状态" style={{ flex: 1 }}>
              <Select options={[
                { value: 'open', label: '开放' },
                { value: 'running', label: '运行中' },
                { value: 'paused', label: '暂停' },
              ]} />
            </Form.Item>
          </Space>
          <Space size="large" style={{ display: 'flex' }}>
            <Form.Item name="memberLimit" label="成员上限">
              <InputNumber min={1} style={{ width: 120 }} />
            </Form.Item>
            <Form.Item name="tickPerDay" label="每日 tick">
              <InputNumber min={0} style={{ width: 120 }} />
            </Form.Item>
            <Form.Item name="dailyTokenBudget" label="token 日预算(0=不限)">
              <InputNumber min={0} style={{ width: 160 }} />
            </Form.Item>
            <Form.Item name="dailyCnyBudgetCents" label="人民币日预算(分)">
              <InputNumber min={0} style={{ width: 160 }} />
            </Form.Item>
          </Space>
        </Form>
      </Modal>

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

// ================= 世界模板库 =================

function Templates() {
  const [moderation, setModeration] = useState<string | undefined>(undefined);
  const [createOpen, setCreateOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [view, setView] = useState<TemplateRow | null>(null);
  const [form] = Form.useForm();

  const list = usePagedList<TemplateRow>(async (cursor) => {
    const qs = new URLSearchParams();
    if (moderation) qs.set('moderation', moderation);
    if (cursor) qs.set('cursor', cursor);
    qs.set('limit', '20');
    const res = await adminFetch<{ templates: TemplateRow[]; nextCursor: string | null }>(
      `/admin/world-templates?${qs.toString()}`,
    );
    return { items: res.templates, nextCursor: res.nextCursor };
  });

  const { reload } = list;
  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [moderation]);

  const submitCreate = async () => {
    let values: { title: string; roomType: string; skeletonJson: string; admissionJson?: string };
    try {
      values = await form.validateFields();
    } catch {
      return;
    }
    let skeletonJson: unknown;
    let admissionJson: unknown | undefined;
    try {
      skeletonJson = JSON.parse(values.skeletonJson);
    } catch {
      message.error('骨架 JSON 解析失败，请检查格式');
      return;
    }
    if (values.admissionJson && values.admissionJson.trim()) {
      try {
        admissionJson = JSON.parse(values.admissionJson);
      } catch {
        message.error('准入 JSON 解析失败，请检查格式');
        return;
      }
    }
    setCreating(true);
    try {
      const res = await adminFetch<{ templateId: string; moderation: string }>(
        '/admin/world-templates',
        'POST',
        { title: values.title, roomType: values.roomType, skeletonJson, admissionJson },
      );
      message.success(`模板已创建并进入审核队列：${res.templateId}`);
      setCreateOpen(false);
      form.resetFields();
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setCreating(false);
    }
  };

  const columns: TableColumnsType<TemplateRow> = [
    { title: '标题', dataIndex: 'title', key: 'title' },
    { title: '房型', dataIndex: 'roomType', key: 'roomType', width: 100, render: (v: string) => ROOM_TYPE_TEXT[v] ?? v },
    { title: '官方', dataIndex: 'official', key: 'official', width: 70, render: (v: boolean) => (v ? <Tag color="gold">官方</Tag> : '—') },
    { title: '版本', dataIndex: 'version', key: 'version', width: 70 },
    {
      title: '审核态',
      dataIndex: 'moderation',
      key: 'moderation',
      width: 90,
      render: (v: string) => {
        const t = MOD_TAG[v] ?? { color: 'default', text: v };
        return <Tag color={t.color}>{t.text}</Tag>;
      },
    },
    { title: '创建时间', dataIndex: 'createdAt', key: 'createdAt', render: formatTime },
    { title: '操作', key: 'op', width: 90, render: (_, r) => <Button size="small" onClick={() => setView(r)}>查看</Button> },
  ];

  return (
    <div>
      <Space style={{ marginBottom: 16 }} wrap>
        <span>审核态筛选：</span>
        <Select
          style={{ width: 160 }}
          allowClear
          placeholder="全部"
          value={moderation}
          onChange={(v) => setModeration(v)}
          options={[
            { value: 'pending', label: '待审核' },
            { value: 'approved', label: '已通过' },
            { value: 'rejected', label: '已驳回' },
          ]}
        />
        <Button onClick={reload}>刷新</Button>
        <Button type="primary" onClick={() => setCreateOpen(true)}>新建模板</Button>
      </Space>

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={list.items}
        loading={list.loading}
        pagination={false}
        scroll={{ x: 800 }}
      />

      {list.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={list.loadMore} loading={list.loading}>加载更多</Button>
        </div>
      )}

      <Modal
        title="新建世界模板"
        open={createOpen}
        onOk={submitCreate}
        confirmLoading={creating}
        onCancel={() => setCreateOpen(false)}
        okText="创建"
        cancelText="取消"
        width={640}
      >
        <Alert type="info" showIcon style={{ marginBottom: 12 }} message="新模板进入待审核态，登记到审核队列，由审核工作台裁决。" />
        <Form form={form} layout="vertical" initialValues={{ roomType: 'idle', admissionJson: '{ "mode": "open" }' }}>
          <Form.Item name="title" label="模板标题" rules={[{ required: true, message: '请输入标题' }]}>
            <Input />
          </Form.Item>
          <Form.Item name="roomType" label="房型" rules={[{ required: true }]}>
            <Select options={[
              { value: 'idle', label: '放置世界' },
              { value: 'chapter', label: '章节房' },
              { value: 'arena', label: '赛事房' },
            ]} />
          </Form.Item>
          <Form.Item
            name="skeletonJson"
            label="骨架 JSON（对象：主线硬节点 / 结局池 / 隐藏内容池 / 装配规则）"
            rules={[{ required: true, message: '请输入骨架 JSON' }]}
          >
            <Input.TextArea rows={6} placeholder='{ "hardNodes": [], "endings": [] }' />
          </Form.Item>
          <Form.Item name="admissionJson" label="准入 JSON（可选，默认 open）">
            <Input.TextArea rows={3} />
          </Form.Item>
        </Form>
      </Modal>

      <Drawer title="模板详情" width={640} open={!!view} onClose={() => setView(null)}>
        {view && (
          <>
            <Descriptions
              column={1}
              bordered
              size="small"
              items={[
                { key: 'id', label: 'ID', children: <Typography.Text code copyable>{view.id}</Typography.Text> },
                { key: 'title', label: '标题', children: view.title },
                { key: 'roomType', label: '房型', children: ROOM_TYPE_TEXT[view.roomType] ?? view.roomType },
                { key: 'official', label: '官方', children: view.official ? '是' : '否' },
                { key: 'version', label: '版本', children: view.version },
                { key: 'moderation', label: '审核态', children: (MOD_TAG[view.moderation]?.text) ?? view.moderation },
                { key: 'createdAt', label: '创建时间', children: formatTime(view.createdAt) },
              ]}
            />
            <Typography.Title level={5} style={{ marginTop: 20 }}>骨架 JSON</Typography.Title>
            <pre style={{ maxHeight: 220, overflow: 'auto', background: '#0000000a', padding: 12, borderRadius: 6 }}>
              {JSON.stringify(view.skeletonJson, null, 2)}
            </pre>
            <Typography.Title level={5} style={{ marginTop: 12 }}>准入 JSON</Typography.Title>
            <pre style={{ maxHeight: 160, overflow: 'auto', background: '#0000000a', padding: 12, borderRadius: 6 }}>
              {JSON.stringify(view.admissionJson, null, 2)}
            </pre>
          </>
        )}
      </Drawer>
    </div>
  );
}

// ================= 主页面 =================

export default function WorldsOps() {
  return (
    <div>
      <Typography.Title level={4}>世界运营</Typography.Title>
      <Tabs
        defaultActiveKey="monitor"
        items={[
          { key: 'monitor', label: '世界监控', children: <WorldsMonitor /> },
          { key: 'templates', label: '世界模板', children: <Templates /> },
        ]}
      />
    </div>
  );
}
