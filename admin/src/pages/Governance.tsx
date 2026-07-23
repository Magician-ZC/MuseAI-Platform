// 模型与 Prompt 治理：Prompt 版本 + diff 视图 + 激活(互斥) + 灰度(canary)；模型路由 + 激活(一键回滚)。
import { useEffect, useState } from 'react';
import {
  Alert,
  Button,
  Drawer,
  Form,
  Input,
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
import { ErrorAlert, formatTime, friendlyError, ReasonModal, usePagedList } from '../components/shared';

const SCOPES = ['director', 'decide', 'arbiter', 'writer', 'critic', 'report'];
const SCOPE_TEXT: Record<string, string> = {
  director: '导演/大纲',
  decide: '角色决策',
  arbiter: '仲裁',
  writer: '叙事写作',
  critic: '一致性评审',
  report: '日报生成',
};

interface PromptRow {
  id: string;
  scope: string;
  version: string;
  content: string;
  active: boolean;
  canaryWorldIds: string[];
  createdAt: number;
}

interface RouteRow {
  id: string;
  version: string;
  routesJson: unknown;
  active: boolean;
  createdAt: number;
}

// ---------------- 行级 LCS diff（按行） ----------------

type DiffLine = { type: 'eq' | 'del' | 'add'; text: string };

function diffLines(oldText: string, newText: string): DiffLine[] {
  const a = oldText.split('\n');
  const b = newText.split('\n');
  const n = a.length;
  const m = b.length;
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ type: 'eq', text: a[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ type: 'del', text: a[i] });
      i++;
    } else {
      out.push({ type: 'add', text: b[j] });
      j++;
    }
  }
  while (i < n) out.push({ type: 'del', text: a[i++] });
  while (j < m) out.push({ type: 'add', text: b[j++] });
  return out;
}

function DiffView({ oldText, newText }: { oldText: string; newText: string }) {
  const lines = diffLines(oldText, newText);
  const bg: Record<DiffLine['type'], string> = {
    eq: 'transparent',
    del: 'rgba(255,77,79,0.14)',
    add: 'rgba(82,196,26,0.16)',
  };
  const sign: Record<DiffLine['type'], string> = { eq: ' ', del: '-', add: '+' };
  return (
    <div style={{ fontFamily: 'monospace', fontSize: 12, border: '1px solid #f0f0f0', borderRadius: 6, overflow: 'auto', maxHeight: 520 }}>
      {lines.map((l, idx) => (
        <div key={idx} style={{ background: bg[l.type], padding: '1px 8px', whiteSpace: 'pre-wrap' }}>
          <span style={{ userSelect: 'none', opacity: 0.6, marginRight: 8 }}>{sign[l.type]}</span>
          {l.text || ' '}
        </div>
      ))}
    </div>
  );
}

// ================= Prompt 治理 =================

function PromptGovernance() {
  const [scope, setScope] = useState<string | undefined>(undefined);
  const [selected, setSelected] = useState<React.Key[]>([]);
  const [createOpen, setCreateOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [form] = Form.useForm();
  const [activateTarget, setActivateTarget] = useState<PromptRow | null>(null);
  const [activating, setActivating] = useState(false);
  const [canary, setCanary] = useState<PromptRow | null>(null);
  const [canaryText, setCanaryText] = useState('');
  const [canarySaving, setCanarySaving] = useState(false);
  const [diffOpen, setDiffOpen] = useState(false);

  const list = usePagedList<PromptRow>(async () => {
    const qs = new URLSearchParams();
    if (scope) qs.set('scope', scope);
    const res = await adminFetch<{ prompts: PromptRow[] }>(`/admin/prompts?${qs.toString()}`);
    return { items: res.prompts, nextCursor: null };
  });

  const { reload } = list;
  useEffect(() => {
    setSelected([]);
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope]);

  const submitCreate = async () => {
    let values: { scope: string; version: string; content: string };
    try {
      values = await form.validateFields();
    } catch {
      return;
    }
    setCreating(true);
    try {
      await adminFetch('/admin/prompts', 'POST', values);
      message.success('新版本已登记（默认未激活，不影响线上）');
      setCreateOpen(false);
      form.resetFields();
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setCreating(false);
    }
  };

  const doActivate = async (reason: string) => {
    if (!activateTarget) return;
    setActivating(true);
    try {
      await adminFetch(`/admin/prompts/${activateTarget.id}/activate?reason=${encodeURIComponent(reason)}`, 'POST');
      message.success(`已激活 ${activateTarget.scope}@${activateTarget.version}（同环节其余版本自动停用）`);
      setActivateTarget(null);
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setActivating(false);
    }
  };

  const openCanary = (row: PromptRow) => {
    setCanary(row);
    setCanaryText((row.canaryWorldIds ?? []).join('\n'));
  };

  const saveCanary = async () => {
    if (!canary) return;
    const worldIds = canaryText.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
    setCanarySaving(true);
    try {
      await adminFetch(`/admin/prompts/${canary.id}/canary`, 'POST', { worldIds });
      message.success(`灰度名单已更新（${worldIds.length} 个世界）`);
      setCanary(null);
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setCanarySaving(false);
    }
  };

  // 选中恰好两版本 → 计算 diff（按 createdAt 旧→新）。
  const selRows = list.items.filter((p) => selected.includes(p.id));
  const [older, newer] = [...selRows].sort((a, b) => a.createdAt - b.createdAt);

  const columns: TableColumnsType<PromptRow> = [
    { title: '环节', dataIndex: 'scope', key: 'scope', width: 120, render: (v: string) => SCOPE_TEXT[v] ?? v },
    { title: '版本', dataIndex: 'version', key: 'version', width: 120, render: (v: string) => <Typography.Text code>{v}</Typography.Text> },
    { title: '状态', dataIndex: 'active', key: 'active', width: 90, render: (v: boolean) => (v ? <Tag color="green">已激活</Tag> : <Tag>未激活</Tag>) },
    { title: '灰度世界', dataIndex: 'canaryWorldIds', key: 'canary', width: 90, render: (v: string[]) => (v?.length ? <Tag color="blue">{v.length}</Tag> : '—') },
    { title: '内容预览', dataIndex: 'content', key: 'content', ellipsis: true, render: (v: string) => v.slice(0, 60) || '—' },
    { title: '创建时间', dataIndex: 'createdAt', key: 'createdAt', width: 170, render: formatTime },
    {
      title: '操作',
      key: 'op',
      width: 150,
      fixed: 'right',
      render: (_, r) => (
        <Space size="small">
          <Button size="small" disabled={r.active} onClick={() => setActivateTarget(r)}>激活</Button>
          <Button size="small" onClick={() => openCanary(r)}>灰度</Button>
        </Space>
      ),
    },
  ];

  return (
    <div>
      <Space style={{ marginBottom: 16 }} wrap>
        <span>环节筛选：</span>
        <Select
          style={{ width: 180 }}
          allowClear
          placeholder="全部环节"
          value={scope}
          onChange={(v) => setScope(v)}
          options={SCOPES.map((s) => ({ value: s, label: `${SCOPE_TEXT[s]}（${s}）` }))}
        />
        <Button onClick={reload}>刷新</Button>
        <Button type="primary" onClick={() => setCreateOpen(true)}>新建版本</Button>
        <Button disabled={selected.length !== 2} onClick={() => setDiffOpen(true)}>
          对比所选两版本（{selected.length}/2）
        </Button>
      </Space>

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={list.items}
        loading={list.loading}
        pagination={false}
        scroll={{ x: 900 }}
        rowSelection={{
          selectedRowKeys: selected,
          onChange: (keys) => setSelected(keys.slice(-2)),
        }}
      />

      {/* 新建版本 */}
      <Modal
        title="登记新 Prompt 版本"
        open={createOpen}
        onOk={submitCreate}
        confirmLoading={creating}
        onCancel={() => setCreateOpen(false)}
        okText="登记"
        cancelText="取消"
        width={640}
      >
        <Alert type="info" showIcon style={{ marginBottom: 12 }} message="新版本默认未激活，不影响线上；需在列表中显式激活。" />
        <Form form={form} layout="vertical" initialValues={{ scope: 'writer' }}>
          <Space size="large" style={{ display: 'flex' }}>
            <Form.Item name="scope" label="环节" rules={[{ required: true }]} style={{ flex: 1 }}>
              <Select options={SCOPES.map((s) => ({ value: s, label: `${SCOPE_TEXT[s]}（${s}）` }))} />
            </Form.Item>
            <Form.Item name="version" label="版本号" rules={[{ required: true, message: '请输入版本号' }]} style={{ flex: 1 }}>
              <Input placeholder="如 v3 / 2026-07-20" />
            </Form.Item>
          </Space>
          <Form.Item name="content" label="Prompt 内容" rules={[{ required: true, message: '请输入内容' }]}>
            <Input.TextArea rows={10} />
          </Form.Item>
        </Form>
      </Modal>

      {/* diff 抽屉 */}
      <Drawer
        title={older && newer ? `Diff：${older.scope}@${older.version} → ${newer.scope}@${newer.version}` : 'Diff'}
        width={860}
        open={diffOpen && selRows.length === 2}
        onClose={() => setDiffOpen(false)}
      >
        {older && newer && (
          <>
            {older.scope !== newer.scope && (
              <Alert type="warning" showIcon style={{ marginBottom: 12 }} message="所选两版本环节不同，diff 仅作文本对比参考。" />
            )}
            <Space style={{ marginBottom: 12 }}>
              <Tag color="red">- 旧：{older.version}</Tag>
              <Tag color="green">+ 新：{newer.version}</Tag>
            </Space>
            <DiffView oldText={older.content} newText={newer.content} />
          </>
        )}
      </Drawer>

      {/* 灰度名单 */}
      <Modal
        title={canary ? `灰度发布：${canary.scope}@${canary.version}` : '灰度发布'}
        open={!!canary}
        onOk={saveCanary}
        confirmLoading={canarySaving}
        onCancel={() => setCanary(null)}
        okText="保存灰度名单"
        cancelText="取消"
      >
        <Typography.Paragraph type="secondary">
          按世界灰度：填写世界 ID（空格 / 逗号 / 换行分隔）。留空即清空灰度名单。
        </Typography.Paragraph>
        <Input.TextArea rows={6} value={canaryText} onChange={(e) => setCanaryText(e.target.value)} placeholder="world_abc&#10;world_def" />
      </Modal>

      <ReasonModal
        open={!!activateTarget}
        title={activateTarget ? `激活 ${activateTarget.scope}@${activateTarget.version}` : '激活'}
        okText="确认激活"
        placeholder="激活/回滚理由（可选，写入审计日志）"
        loading={activating}
        onOk={doActivate}
        onCancel={() => setActivateTarget(null)}
      />
    </div>
  );
}

// ================= 模型路由 =================

function ModelRoutes() {
  const [createOpen, setCreateOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [form] = Form.useForm();
  const [activateTarget, setActivateTarget] = useState<RouteRow | null>(null);
  const [activating, setActivating] = useState(false);
  const [view, setView] = useState<RouteRow | null>(null);

  const list = usePagedList<RouteRow>(async () => {
    const res = await adminFetch<{ modelRoutes: RouteRow[] }>('/admin/model-routes');
    return { items: res.modelRoutes, nextCursor: null };
  });

  const { reload } = list;
  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const submitCreate = async () => {
    let values: { version: string; routesJson: string };
    try {
      values = await form.validateFields();
    } catch {
      return;
    }
    let routesJson: unknown;
    try {
      routesJson = JSON.parse(values.routesJson);
    } catch {
      message.error('路由 JSON 解析失败，请检查格式');
      return;
    }
    setCreating(true);
    try {
      await adminFetch('/admin/model-routes', 'POST', { version: values.version, routesJson });
      message.success('新路由版本已登记（默认未激活）');
      setCreateOpen(false);
      form.resetFields();
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setCreating(false);
    }
  };

  const doActivate = async (reason: string) => {
    if (!activateTarget) return;
    setActivating(true);
    try {
      await adminFetch(`/admin/model-routes/${activateTarget.id}/activate?reason=${encodeURIComponent(reason)}`, 'POST');
      message.success(`已激活路由 ${activateTarget.version}（其余版本自动停用）`);
      setActivateTarget(null);
      reload();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setActivating(false);
    }
  };

  const columns: TableColumnsType<RouteRow> = [
    { title: '版本', dataIndex: 'version', key: 'version', width: 160, render: (v: string) => <Typography.Text code>{v}</Typography.Text> },
    { title: '状态', dataIndex: 'active', key: 'active', width: 100, render: (v: boolean) => (v ? <Tag color="green">当前生效</Tag> : <Tag>历史版本</Tag>) },
    { title: '路由映射', dataIndex: 'routesJson', key: 'routesJson', ellipsis: true, render: (v: unknown) => JSON.stringify(v).slice(0, 80) },
    { title: '创建时间', dataIndex: 'createdAt', key: 'createdAt', width: 170, render: formatTime },
    {
      title: '操作',
      key: 'op',
      width: 160,
      render: (_, r) => (
        <Space size="small">
          <Button size="small" onClick={() => setView(r)}>查看</Button>
          <Button size="small" type={r.active ? 'default' : 'primary'} disabled={r.active} onClick={() => setActivateTarget(r)}>
            激活/回滚
          </Button>
        </Space>
      ),
    },
  ];

  return (
    <div>
      <Space style={{ marginBottom: 16 }}>
        <Button onClick={reload}>刷新</Button>
        <Button type="primary" onClick={() => setCreateOpen(true)}>新建路由版本</Button>
      </Space>
      <Alert type="info" showIcon style={{ marginBottom: 16 }} message="全局单活跃路由：激活某版本即互斥切换；一键回滚 = 激活旧版本。" />

      {list.error && <ErrorAlert message={list.error} onRetry={reload} />}

      <Table rowKey="id" size="small" columns={columns} dataSource={list.items} loading={list.loading} pagination={false} scroll={{ x: 800 }} />

      <Modal
        title="登记新模型路由版本"
        open={createOpen}
        onOk={submitCreate}
        confirmLoading={creating}
        onCancel={() => setCreateOpen(false)}
        okText="登记"
        cancelText="取消"
        width={640}
      >
        <Form form={form} layout="vertical">
          <Form.Item name="version" label="版本号" rules={[{ required: true, message: '请输入版本号' }]}>
            <Input placeholder="如 route-v2" />
          </Form.Item>
          <Form.Item
            name="routesJson"
            label="路由映射 JSON（对象：环节 → ModelProfile）"
            rules={[{ required: true, message: '请输入路由 JSON' }]}
          >
            <Input.TextArea rows={10} placeholder='{ "writer": { "modelId": "..." } }' />
          </Form.Item>
        </Form>
      </Modal>

      <Drawer title={view ? `路由版本 ${view.version}` : '路由详情'} width={640} open={!!view} onClose={() => setView(null)}>
        {view && (
          <pre style={{ background: '#0000000a', padding: 12, borderRadius: 6, overflow: 'auto' }}>
            {JSON.stringify(view.routesJson, null, 2)}
          </pre>
        )}
      </Drawer>

      <ReasonModal
        open={!!activateTarget}
        title={activateTarget ? `激活路由 ${activateTarget.version}` : '激活'}
        okText="确认激活"
        placeholder="激活/回滚理由（可选，写入审计日志）"
        loading={activating}
        onOk={doActivate}
        onCancel={() => setActivateTarget(null)}
      />
    </div>
  );
}

export default function Governance() {
  return (
    <div>
      <Typography.Title level={4}>模型与 Prompt 治理</Typography.Title>
      <Tabs
        defaultActiveKey="prompts"
        items={[
          { key: 'prompts', label: 'Prompt 版本 / 灰度', children: <PromptGovernance /> },
          { key: 'routes', label: '模型路由', children: <ModelRoutes /> },
        ]}
      />
    </div>
  );
}
