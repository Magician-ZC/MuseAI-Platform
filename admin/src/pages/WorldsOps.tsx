// 世界运营：活跃世界监控 + 脱敏诊断 + 暂停/恢复 + 官方建房 + 世界模板库（含星级 curation 定档）。
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
  Tag,
  Typography,
} from 'antd';
import type { TableColumnsType } from 'antd';
import { useLocation, useNavigate } from 'react-router-dom';
import { adminFetch, AdminApiError } from '../api';
import { ErrorAlert, formatTime, friendlyError, usePagedList } from '../components/shared';
import WorldsMonitorConsole from './WorldsMonitorConsole';
import Calibration from './Calibration';

// ---------------- 类型 ----------------

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
  /** 星级 1-5（列表投影新增字段；后端未投影时缺省显示占位）。 */
  starRating?: number;
  /** 星级来源：auto 自动评估 / curated 人工定档。 */
  starSource?: 'auto' | 'curated';
}

const ROOM_TYPE_TEXT: Record<string, string> = { idle: '放置世界', chapter: '章节房', arena: '赛事房' };
const MOD_TAG: Record<string, { color: string; text: string }> = {
  pending: { color: 'blue', text: '待审核' },
  approved: { color: 'green', text: '已通过' },
  rejected: { color: 'red', text: '已驳回' },
};
const STAR_SOURCE_TAG: Record<string, { color: string; text: string }> = {
  auto: { color: 'default', text: '自动评估' },
  curated: { color: 'gold', text: '人工定档' },
};

/** 金色星级徽标 + 来源 Tag；后端尚未投影星级字段时显示占位符。 */
function StarBadge({ rating, source }: { rating?: number | null; source?: string | null }) {
  if (rating == null) return <>—</>;
  const s = source ? (STAR_SOURCE_TAG[source] ?? { color: 'default', text: source }) : null;
  return (
    <Space size={4}>
      <span style={{ color: '#faad14', fontWeight: 600 }}>{rating}★</span>
      {s && <Tag color={s.color}>{s.text}</Tag>}
    </Space>
  );
}

/**
 * 定档接口错误 → 提示文案：400（星级/理由非法）、403（无权限）、404（模板不存在）
 * 按契约优先透出服务端文案；无结构化文案（如断网、空响应体）时回退通用友好提示。
 */
function starActionError(e: unknown): string {
  if (e instanceof AdminApiError && e.message && !/^HTTP \d+$/.test(e.message)) return e.message;
  return friendlyError(e);
}

// ⚠️ 这里原本还有一个 `WorldsMonitor` 组件（约 340 行）。`WorldsMonitorConsole` 上线后它就
//    **不再被任何路由引用**，全仓零命中，2026-07-29 删除。触发点是根 `tsc` 把它报成未使用——
//    根 tsconfig 的 `noUnusedLocals` 比 admin 自己的严，而后台组件一旦被根 `src/__tests__/` 的
//    用例 import 就会进根那道检查（admin 至今没有自己的 npm test，见 VALIDATION §3.47 A5）。

// ================= 世界模板库 =================

/** 服务端 `STAGE_NO_MAX`（`admin_api::worlds_ops`）。超出即 400。 */
const STAGE_NO_MAX = 999;

/**
 * 从 URL 带来的建模板预填——人工校准页「按此坐标建模板」跳过来时用。
 *
 * 🔴 这条链路存在的理由要写清楚：人工校准面（`/admin/sagas` 等六个端点）**恒为只读**
 * （`editable: false`），它的 `editPath` 一直在说「唯一写入路径是 POST /admin/world-templates」，
 * 而那个端点的后台表单**此前根本没有 `sagaId` / `stageNo` 两个字段**
 * ——`grep sagaId admin/src` 在 2026-07-29 之前一次都不命中。
 * 于是「唯一写入路径」在后台是**走不通的**：运营看得见「这个系列缺 3 号阶段」，
 * 却只能自己去调 API 补。本预填 + 下面那两个表单字段补的是这一格。
 *
 * ⚠️ 刻意**不**给校准面加写端点：模板是 append-only 的（改模板 = 建一条新行、新 id，
 * admin 侧连 version 都恒为 1，两条模板之间没有任何血缘字段），
 * 新开一条写路径会同时带出「血缘怎么表达」这个未决问题，且必然与既有写入面的校验链漂移。
 * 让唯一的那条路可达，比再造一条便宜也诚实。
 */
interface TemplatePrefill {
  sagaId: string;
  stageNo?: number;
}

function parseTemplatePrefill(search: string): TemplatePrefill | null {
  const q = new URLSearchParams(search);
  if (q.get('newTemplate') !== '1') return null;
  const sagaId = (q.get('sagaId') ?? '').trim();
  if (!sagaId) return null;
  const rawStage = q.get('stageNo');
  const stageNo =
    rawStage != null && /^\d+$/.test(rawStage) && Number(rawStage) >= 1 && Number(rawStage) <= STAGE_NO_MAX
      ? Number(rawStage)
      : undefined;
  return { sagaId, stageNo };
}

function Templates() {
  const [moderation, setModeration] = useState<string | undefined>(undefined);
  const [createOpen, setCreateOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [view, setView] = useState<TemplateRow | null>(null);
  const [starTarget, setStarTarget] = useState<TemplateRow | null>(null);
  const [starring, setStarring] = useState(false);
  const [form] = Form.useForm();
  const [starForm] = Form.useForm();
  const location = useLocation();
  const navigate = useNavigate();

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

  // 校准页跳过来时：预填阶段坐标并直接把建模板框弹开。
  useEffect(() => {
    const prefill = parseTemplatePrefill(location.search);
    if (!prefill) return;
    form.setFieldsValue({ sagaId: prefill.sagaId, stageNo: prefill.stageNo });
    setCreateOpen(true);
    // 用过即从地址栏消费掉：留着的话「刷新一下页面」会再弹一次建模板框，
    // 而运营多半以为那是上次没提交成功。其余 query（design=preview / view）原样保留。
    const q = new URLSearchParams(location.search);
    for (const k of ['newTemplate', 'sagaId', 'stageNo']) q.delete(k);
    const suffix = q.toString();
    navigate(`/worlds${suffix ? `?${suffix}` : ''}`, { replace: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.search]);

  const submitCreate = async () => {
    let values: {
      title: string;
      roomType: string;
      skeletonJson: string;
      admissionJson?: string;
      sagaId?: string;
      stageNo?: number;
    };
    try {
      values = await form.validateFields();
    } catch {
      return;
    }
    // 阶段坐标必须成对：服务端只给 stageNo 会 400，只给 sagaId 会被当成「阶段号缺失」拒绝。
    // 在这里先拦一次，是为了让运营看到一句说人话的提示，而不是一条 `请求无效: …` 的透传。
    const sagaId = (values.sagaId ?? '').trim();
    const hasStage = typeof values.stageNo === 'number';
    if ((sagaId.length > 0) !== hasStage) {
      message.error('世界系列 ID 与阶段号必须成对填写：要么都填，要么都留空（留空 = 独立模板，不归入任何 Saga）');
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
        {
          title: values.title,
          roomType: values.roomType,
          skeletonJson,
          admissionJson,
          // 二者成对下发或都不下发（上面已拦成对性）。都不填 = 独立模板，与接线前行为逐字相同。
          ...(sagaId.length > 0 ? { sagaId, stageNo: values.stageNo } : {}),
        },
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

  /** 打开定档 Modal：星级预填当前值（无则 3★），理由每次清空。 */
  const openStar = (row: TemplateRow) => {
    setStarTarget(row);
    starForm.setFieldsValue({ star: row.starRating ?? 3, reason: '' });
  };

  const submitStar = async () => {
    if (!starTarget) return;
    let values: { star: number; reason: string };
    try {
      values = await starForm.validateFields();
    } catch {
      return;
    }
    setStarring(true);
    try {
      // POST /api/admin/world-templates/{id}/star，body { star: 1..5, reason: 1..500 必填 }。
      await adminFetch(`/admin/world-templates/${starTarget.id}/star`, 'POST', {
        star: values.star,
        reason: values.reason.trim(),
      });
      message.success(`模板已定档为 ${values.star}★`);
      setStarTarget(null);
      reload();
    } catch (e) {
      message.error(starActionError(e));
    } finally {
      setStarring(false);
    }
  };

  const columns: TableColumnsType<TemplateRow> = [
    { title: '标题', dataIndex: 'title', key: 'title' },
    { title: '房型', dataIndex: 'roomType', key: 'roomType', width: 100, render: (v: string) => ROOM_TYPE_TEXT[v] ?? v },
    { title: '官方', dataIndex: 'official', key: 'official', width: 70, render: (v: boolean) => (v ? <Tag color="gold">官方</Tag> : '—') },
    {
      title: '星级',
      key: 'star',
      width: 150,
      render: (_, r) => <StarBadge rating={r.starRating} source={r.starSource} />,
    },
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
    {
      title: '操作',
      key: 'op',
      width: 140,
      render: (_, r) => (
        <Space size="small">
          <Button size="small" onClick={() => setView(r)}>查看</Button>
          <Button size="small" onClick={() => openStar(r)}>定档</Button>
        </Space>
      ),
    },
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
        scroll={{ x: 1000 }}
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
        <Alert type="info" showIcon style={{ marginBottom: 12 }} title="新模板进入待审核态，登记到审核队列，由审核工作台裁决。" />
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
          <Form.Item label="Saga 阶段坐标（可选；两格要么都填、要么都留空）">
            <Space align="start">
              <Form.Item name="sagaId" noStyle>
                <Input placeholder="世界系列 ID，如 douluo" style={{ width: 260 }} allowClear />
              </Form.Item>
              <Form.Item name="stageNo" noStyle>
                <InputNumber
                  min={1}
                  max={STAGE_NO_MAX}
                  precision={0}
                  placeholder={`阶段号 1-${STAGE_NO_MAX}`}
                  style={{ width: 160 }}
                />
              </Form.Item>
            </Space>
            <Typography.Paragraph type="secondary" style={{ fontSize: 12, margin: '6px 0 0' }}>
              留空 = 独立模板，不归入任何 Saga。填了则该模板成为这个世界系列的第 N 阶段——
              <b>一个世界实例 = 一个阶段</b>，「连载」由开新实例 + 阶段间继承实现。
              人工校准页的缺号 / 重号诊断读的就是这两个字段。
            </Typography.Paragraph>
          </Form.Item>
        </Form>
      </Modal>

      {/* 模板星级定档 */}
      <Modal
        title={`模板定档：${starTarget?.title ?? ''}`}
        open={!!starTarget}
        onOk={submitStar}
        confirmLoading={starring}
        onCancel={() => setStarTarget(null)}
        okText="确认定档"
        cancelText="取消"
        width={480}
      >
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 12 }}
          title="人工定档后来源标记为「人工定档」，覆盖自动评估结果；理由将写入审计日志。"
        />
        <Form form={starForm} layout="vertical">
          <Form.Item name="star" label="目标星级" rules={[{ required: true, message: '请选择星级' }]}>
            <Select
              options={[5, 4, 3, 2, 1].map((n) => ({
                value: n,
                label: <span style={{ color: '#faad14', fontWeight: 600 }}>{'★'.repeat(n)}（{n} 星）</span>,
              }))}
            />
          </Form.Item>
          <Form.Item
            name="reason"
            label="定档理由（必填）"
            rules={[
              { required: true, whitespace: true, message: '请填写定档理由' },
              { max: 500, message: '理由不能超过 500 字' },
            ]}
          >
            <Input.TextArea rows={4} maxLength={500} showCount placeholder="说明定档依据（如内容质量、玩家反馈、运营策略），将写入审计日志" />
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
                { key: 'star', label: '星级', children: <StarBadge rating={view.starRating} source={view.starSource} /> },
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

/** 世界运营下的低频子视图（设计文档 §3「更多模块：世界模板及低频管理入口」）。 */
type WorldsView = 'monitor' | 'templates' | 'calibration';

function parseView(search: string): WorldsView {
  const v = new URLSearchParams(search).get('view');
  return v === 'templates' || v === 'calibration' ? v : 'monitor';
}

export default function WorldsOps() {
  const location = useLocation();
  const navigate = useNavigate();
  const requestedView = parseView(location.search);
  const [activeView, setActiveView] = useState<WorldsView>(requestedView);

  useEffect(() => setActiveView(requestedView), [requestedView]);

  if (activeView === 'monitor') {
    return <WorldsMonitorConsole />;
  }

  /** 切换子视图（`view` 为空即回监控页），其余 query（如 design=preview）原样保留。 */
  const goView = (view: WorldsView) => {
    const query = new URLSearchParams(location.search);
    if (view === 'monitor') query.delete('view');
    else query.set('view', view);
    const suffix = query.toString();
    navigate(`/worlds${suffix ? `?${suffix}` : ''}`);
    setActiveView(view);
  };

  return (
    <div style={{ padding: activeView === 'calibration' ? 0 : 24 }}>
      <Space style={{ margin: activeView === 'calibration' ? '24px 24px 0' : '0 0 16px' }} wrap>
        <Button onClick={() => goView('monitor')}>返回世界监控</Button>
        <Button
          type={activeView === 'templates' ? 'primary' : 'default'}
          onClick={() => goView('templates')}
        >
          世界模板
        </Button>
        <Button
          type={activeView === 'calibration' ? 'primary' : 'default'}
          onClick={() => goView('calibration')}
        >
          人工校准
        </Button>
      </Space>
      {activeView === 'calibration' ? (
        <Calibration />
      ) : (
        <>
          <Typography.Title level={4} style={{ margin: '0 0 16px' }}>世界模板</Typography.Title>
          <Templates />
        </>
      )}
    </div>
  );
}
