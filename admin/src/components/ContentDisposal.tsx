// 已过审内容处置（audit 模块内分区，reviewer/admin）：
//   GET  /admin/content/takedowns?state=&kind=&cursor=&limit=   处置台账
//   GET  /admin/content/{kind}/{id}                             单主体处置态
//   POST /admin/content/{kind}/{id}/recheck|takedown|restore    再审 / 下架 / 恢复
//   GET  /admin/content/appeals?status=&cursor=&limit=          处置申诉队列（migration 0045）
//   POST /admin/content/appeals/{id}/resolve                    处置申诉裁决（uphold / overturn）
//
// 🔴 这个分区必须说清楚、且在结构上守住的四件事：
//
// ① **下架的是「展示」，不是「已发生的世界事实」**。处置只改主体的审核态列；`world_events`
//    等已落定的事实表一个字节都不动（服务端红线，回执里的 `worldlineUntouched` 就是它的自述）。
//    这句话是常驻横幅，不是提示语——运营在按下按钮的那一刻就该知道自己改的是什么、没改什么。
//
// ② **运行中的世界不受影响，且必须说出来**。入场闸只在入场时判一次，因此下架不会把卡从运行中的
//    世界里赶出去。服务端把受影响世界清单直接放进回执，本页原样渲染并给出既有的暂停入口——
//    界面不替产品决定「要不要中止运行中的世界」，但把决定所需的事实摆齐。
//
// ③ **可逆性两档，界面必须让人看出区别**。可恢复下架是常规动作；永久移除不可逆、admin 专属，
//    因此它是一个**默认不勾**的复选框 + 变红的按钮 + 明写「不可恢复」的确认文案。
//    非 admin 角色下该复选框不可勾（Tooltip 说明原因），后端仍会二次校验。
//
// ④ **数据诚实（设计文档 §9.1）**：只渲染接口真实返回的字段；空台账明写「真的没有处置记录」，
//    与取数失败可区分；「加载更多」仅在 nextCursor 非空时出现（末页服务端回 null）。
import { useCallback, useEffect, useState } from 'react';
import {
  Alert,
  Button,
  Checkbox,
  Descriptions,
  Input,
  Modal,
  Select,
  Space,
  Table,
  Tag,
  Tooltip,
  Typography,
  message,
} from 'antd';
import type { TableColumnsType } from 'antd';
import { Link } from 'react-router-dom';
import { adminFetch, getRole } from '../api';
import { ErrorAlert, formatTime, friendlyError, usePagedList } from './shared';

// ---------------- 类型（对齐 server/src/admin_api/takedown.rs 的响应） ----------------

interface TakedownRow {
  id: string;
  subjectKind: string;
  subjectId: string;
  /** restricted（可恢复）/ removed（永久移除）/ restored（已恢复） */
  state: string;
  /** 服务端派生：只有 restricted 可恢复。前端不自己记规则。 */
  reversible: boolean;
  prevModeration: string;
  reason: string;
  actorId: string;
  actorRole: string;
  bytesPurged: boolean;
  createdAt: number;
  restoredAt: number | null;
  restoredBy: string | null;
  restoreReason: string | null;
}

interface AffectedWorld {
  id: string;
  title: string;
  status: string;
}

interface SubjectStatus {
  subjectKind: string;
  subjectLabel: string;
  subjectId: string;
  /** 当前展示态；无该项资产（如未上传立绘）时为 null。 */
  moderation: string | null;
  takenDown: boolean;
  canTakedown: boolean;
  canRecheck: boolean;
  takedown: TakedownRow | null;
  affectedRunningWorldCount: number;
  affectedRunningWorlds: AffectedWorld[];
  worldlineUntouched: boolean;
  notes: string[];
}

/** 处置申诉行（对齐 server/src/admin_api/takedown/appeals.rs，migration 0045）。 */
interface DisposalAppealRow {
  id: string;
  takedownId: string;
  disposalAt: number;
  subjectKind: string;
  subjectId: string;
  ownerId: string;
  appealText: string;
  /** 提交时的处置态快照（restricted / removed）。 */
  disposalState: string;
  /** pending / upheld（维持处置）/ overturned（改判恢复） */
  status: string;
  /** 🔴 复审人写给**作者**的答复（会回显给作者）。不是下架时填的内部理由。 */
  resolutionReason: string | null;
  reviewerId: string | null;
  createdAt: number;
  resolvedAt: number | null;
  /** 后台专属：当前处置台账全行，含运营内部处置理由。作者侧永不下发。 */
  disposal: TakedownRow | null;
}

// ---------------- 展示映射（未知取值一律原样回显，不吞） ----------------

const KIND_OPTIONS = [
  { value: 'character', label: '角色卡' },
  { value: 'character_avatar', label: '角色立绘' },
  { value: 'world_cover', label: '世界封面' },
  { value: 'world_template', label: '世界模板' },
];

const KIND_TEXT: Record<string, string> = Object.fromEntries(
  KIND_OPTIONS.map((o) => [o.value, o.label]),
);

const STATE_TEXT: Record<string, { color: string; text: string }> = {
  restricted: { color: 'orange', text: '已下架（可恢复）' },
  removed: { color: 'red', text: '已永久移除' },
  restored: { color: 'green', text: '已恢复' },
};

/** 主体展示态。`takedown` 是本批次引入的处置态，与「发布时被驳回」是两回事，不可混用文案。 */
const MODERATION_TEXT: Record<string, { color: string; text: string }> = {
  approved: { color: 'green', text: '已过审（在线）' },
  pending: { color: 'gold', text: '待审核' },
  rejected: { color: 'red', text: '发布时被驳回' },
  takedown: { color: 'volcano', text: '已被下架' },
};

const STATE_FILTER = [
  { value: 'all', label: '全部处置' },
  { value: 'restricted', label: '已下架（可恢复）' },
  { value: 'removed', label: '已永久移除' },
  { value: 'restored', label: '已恢复' },
];

function stateTag(value: string) {
  const t = STATE_TEXT[value] ?? { color: 'default', text: value };
  return <Tag color={t.color}>{t.text}</Tag>;
}

function moderationTag(value: string | null) {
  if (value == null) return <Tag>无该项资产</Tag>;
  const t = MODERATION_TEXT[value] ?? { color: 'default', text: value };
  return <Tag color={t.color}>{t.text}</Tag>;
}

// ---------------- 处置动作 Modal（理由必填；永久移除单独一档） ----------------

type ActionKind = 'recheck' | 'takedown' | 'restore';

const ACTION_META: Record<ActionKind, { title: string; ok: string; placeholder: string }> = {
  recheck: {
    title: '送回人审队列（再审）',
    ok: '确认送审',
    placeholder: '填写再审理由，例如「收到 3 条骚扰举报，请人审复看」（必填，写入审计日志）',
  },
  takedown: {
    title: '下架该内容',
    ok: '确认下架',
    placeholder: '填写下架理由（必填，写入审计日志与风控留痕；不会展示给作者）',
  },
  restore: {
    title: '恢复该内容',
    ok: '确认恢复',
    placeholder: '填写恢复理由，例如「复核后认为不构成违规」（必填，写入审计日志）',
  },
};

function ActionModal({
  action,
  subject,
  onClose,
  onDone,
}: {
  action: ActionKind | null;
  subject: SubjectStatus;
  onClose: () => void;
  onDone: (result: unknown) => void;
}) {
  const [reason, setReason] = useState('');
  const [permanent, setPermanent] = useState(false);
  const [busy, setBusy] = useState(false);
  const role = getRole();
  const canPermanent = role === 'admin';

  useEffect(() => {
    if (action) {
      setReason('');
      // 🔴 每次打开都复位：不可逆动作绝不能「记住上次的勾选」。
      setPermanent(false);
    }
  }, [action]);

  if (!action) return null;
  const meta = ACTION_META[action];
  const danger = action === 'takedown';

  const submit = async () => {
    const trimmed = reason.trim();
    if (!trimmed) {
      message.error('处置理由必填');
      return;
    }
    setBusy(true);
    try {
      const body =
        action === 'takedown' ? { reason: trimmed, permanent } : { reason: trimmed };
      const res = await adminFetch(
        `/admin/content/${subject.subjectKind}/${encodeURIComponent(subject.subjectId)}/${action}`,
        'POST',
        body,
      );
      message.success(
        action === 'takedown'
          ? permanent
            ? '已永久移除'
            : '已下架（可恢复）'
          : action === 'restore'
            ? '已恢复'
            : '已送回人审队列',
      );
      onDone(res);
      onClose();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open
      title={permanent ? '永久移除该内容（不可恢复）' : meta.title}
      onOk={submit}
      onCancel={onClose}
      confirmLoading={busy}
      okText={permanent ? '确认永久移除' : meta.ok}
      cancelText="取消"
      okButtonProps={{ danger: danger || permanent }}
    >
      <Descriptions
        column={1}
        size="small"
        style={{ marginBottom: 12 }}
        items={[
          { key: 'k', label: '主体', children: `${subject.subjectLabel}　${subject.subjectId}` },
          { key: 'm', label: '当前状态', children: moderationTag(subject.moderation) },
        ]}
      />

      {action === 'takedown' && (
        <>
          <Tooltip
            title={
              canPermanent
                ? undefined
                : '永久移除是不可逆处置，仅超级管理员可执行——不可逆的动作必须比可逆的更难触发。后端仍会二次校验。'
            }
          >
            <span>
              <Checkbox
                checked={permanent}
                disabled={!canPermanent}
                onChange={(e) => setPermanent(e.target.checked)}
              >
                永久移除（<b>不可恢复</b>；立绘 / 封面会连对象存储字节一并删除）
              </Checkbox>
            </span>
          </Tooltip>
          <Alert
            style={{ margin: '12px 0' }}
            type={permanent ? 'error' : 'warning'}
            showIcon
            title={permanent ? '此操作不可撤销' : '此操作可恢复'}
            description={
              permanent
                ? '永久移除后「恢复」接口将恒定拒绝，作者需重新发布。用于被要求永久移除的合规场景。'
                : '可恢复下架只关闭展示与新入场；如判断有误，可在本页原地恢复到下架前的状态。'
            }
          />
        </>
      )}

      {action === 'recheck' && (
        <Alert
          style={{ marginBottom: 12 }}
          type="info"
          showIcon
          title="再审不改变展示态"
          description="内容会被送回人审队列，但在人审给出裁决前照常在线。需要立刻断掉展示请改用「下架」——两个动作正交。"
        />
      )}

      {subject.affectedRunningWorldCount > 0 && action !== 'restore' && (
        <Alert
          style={{ marginBottom: 12 }}
          type="warning"
          showIcon
          title={`该主体仍在 ${subject.affectedRunningWorldCount} 个运行中的世界里`}
          description="入场闸只在入场时判一次，因此处置后它会继续参演存量世界。要立即中止请另行暂停对应世界（世界运营 → 暂停世界）；本入口不代做强制离场——那会改动世界线相关表。"
        />
      )}

      <Input.TextArea
        rows={3}
        value={reason}
        placeholder={meta.placeholder}
        onChange={(e) => setReason(e.target.value)}
      />
    </Modal>
  );
}

// ---------------- 单主体处置面板 ----------------

function SubjectPanel({
  subject,
  onAction,
  onRefresh,
}: {
  subject: SubjectStatus;
  onAction: (a: ActionKind) => void;
  onRefresh: () => void;
}) {
  const td = subject.takedown;
  const canRestore = !!td && td.state === 'restricted';

  return (
    <div style={{ marginBottom: 24 }}>
      <Descriptions
        bordered
        column={2}
        size="small"
        title={
          <Space>
            <span>{subject.subjectLabel}</span>
            <Typography.Text code copyable>
              {subject.subjectId}
            </Typography.Text>
            <Button size="small" onClick={onRefresh}>
              刷新
            </Button>
          </Space>
        }
        items={[
          { key: 'm', label: '当前展示态', children: moderationTag(subject.moderation) },
          {
            key: 'td',
            label: '处置状态',
            children: td ? stateTag(td.state) : <Typography.Text type="secondary">未被处置</Typography.Text>,
          },
          ...(td
            ? [
                { key: 'r', label: '处置理由', children: td.reason },
                { key: 'a', label: '处置人', children: `${td.actorId}（${td.actorRole}）` },
                { key: 'c', label: '处置时间', children: formatTime(td.createdAt) },
                {
                  key: 'p',
                  label: '对象字节',
                  children: td.bytesPurged
                    ? '已删除'
                    : td.state === 'removed'
                      ? '未删除（文本主体不删字节：运行中的世界仍引用该不可变快照）'
                      : '保留',
                },
                ...(td.restoredAt
                  ? [
                      {
                        key: 'rs',
                        label: '恢复记录',
                        children: `${formatTime(td.restoredAt)}　${td.restoredBy ?? '—'}　${td.restoreReason ?? ''}`,
                      },
                    ]
                  : []),
              ]
            : []),
          {
            key: 'w',
            label: '受影响的运行中世界',
            span: 2,
            children:
              subject.affectedRunningWorldCount === 0 ? (
                <Typography.Text type="secondary">无（0 是真实答案，不是缺数据）</Typography.Text>
              ) : (
                <Space direction="vertical" size={2}>
                  <span>
                    共 {subject.affectedRunningWorldCount} 个。处置<b>不会</b>把它从这些世界里赶出去
                    —— 入场闸只在入场时判一次。要立即中止请暂停对应世界。
                  </span>
                  <Space wrap>
                    {subject.affectedRunningWorlds.map((w) => (
                      <Link key={w.id} to={`/worlds?world=${encodeURIComponent(w.id)}`}>
                        <Tag color="blue">{w.title || w.id}</Tag>
                      </Link>
                    ))}
                  </Space>
                </Space>
              ),
          },
        ]}
      />

      <Space style={{ marginTop: 12 }} wrap>
        {/* 四类主体（含立绘 / 世界封面两类位图）现在都有再审通道：位图走图片机审入队 +
            人审工作台回写。此前位图只能下架不能再审，是 migration 0027 记着的那处缺口。 */}
        <Tooltip title={subject.canRecheck ? undefined : '仅可对已过审内容发起再审'}>
          <span>
            <Button disabled={!subject.canRecheck} onClick={() => onAction('recheck')}>
              送回人审队列（再审）
            </Button>
          </span>
        </Tooltip>
        <Tooltip title={subject.canTakedown ? undefined : '仅可下架当前处于「已过审」的内容'}>
          <span>
            <Button danger disabled={!subject.canTakedown} onClick={() => onAction('takedown')}>
              下架
            </Button>
          </span>
        </Tooltip>
        <Tooltip
          title={
            canRestore
              ? undefined
              : td?.state === 'removed'
                ? '已永久移除的内容不可恢复'
                : '该主体当前不处于可恢复的下架状态'
          }
        >
          <span>
            <Button type="primary" disabled={!canRestore} onClick={() => onAction('restore')}>
              恢复
            </Button>
          </span>
        </Tooltip>
      </Space>
    </div>
  );
}

// ---------------- 主组件 ----------------

export default function ContentDisposal({
  initialKind,
  initialSubjectId,
}: {
  initialKind?: string | null;
  initialSubjectId?: string | null;
}) {
  const [kind, setKind] = useState(initialKind || 'character');
  const [subjectId, setSubjectId] = useState(initialSubjectId || '');
  const [subject, setSubject] = useState<SubjectStatus | null>(null);
  const [lookupError, setLookupError] = useState<string | null>(null);
  const [looking, setLooking] = useState(false);
  const [action, setAction] = useState<ActionKind | null>(null);
  const [stateFilter, setStateFilter] = useState('all');

  const ledger = usePagedList<TakedownRow>(async (cursor) => {
    const qs = new URLSearchParams({ limit: '20' });
    if (stateFilter !== 'all') qs.set('state', stateFilter);
    if (cursor) qs.set('cursor', cursor);
    const res = await adminFetch<{ items: TakedownRow[]; nextCursor: string | null }>(
      `/admin/content/takedowns?${qs.toString()}`,
    );
    return { items: res.items, nextCursor: res.nextCursor };
  });
  const { reload } = ledger;

  const lookup = useCallback(
    async (k: string, id: string) => {
      const trimmed = id.trim();
      if (!trimmed) {
        setSubject(null);
        setLookupError(null);
        return;
      }
      setLooking(true);
      setLookupError(null);
      try {
        const res = await adminFetch<SubjectStatus>(
          `/admin/content/${k}/${encodeURIComponent(trimmed)}`,
        );
        setSubject(res);
      } catch (e) {
        setSubject(null);
        setLookupError(friendlyError(e));
      } finally {
        setLooking(false);
      }
    },
    [],
  );

  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stateFilter]);

  // 从举报队列跳转过来时（?kind=&subject=）自动落到目标主体上，免得运营再手抄一遍 id。
  useEffect(() => {
    if (initialSubjectId) lookup(initialKind || 'character', initialSubjectId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialKind, initialSubjectId]);

  const columns: TableColumnsType<TakedownRow> = [
    {
      title: '主体类型',
      dataIndex: 'subjectKind',
      key: 'kind',
      width: 110,
      render: (v: string) => KIND_TEXT[v] ?? v,
    },
    {
      title: '主体 ID',
      dataIndex: 'subjectId',
      key: 'sid',
      render: (v: string, r) => (
        <Button
          type="link"
          size="small"
          style={{ padding: 0 }}
          onClick={() => {
            setKind(r.subjectKind);
            setSubjectId(v);
            lookup(r.subjectKind, v);
          }}
        >
          <Typography.Text code>{v}</Typography.Text>
        </Button>
      ),
    },
    { title: '处置状态', dataIndex: 'state', key: 'state', width: 150, render: stateTag },
    { title: '处置理由', dataIndex: 'reason', key: 'reason', ellipsis: true },
    {
      title: '处置人',
      dataIndex: 'actorId',
      key: 'actor',
      width: 160,
      render: (v: string, r) => `${v}（${r.actorRole}）`,
    },
    { title: '处置时间', dataIndex: 'createdAt', key: 'createdAt', width: 180, render: formatTime },
  ];

  return (
    <div>
      <Alert
        type="warning"
        showIcon
        style={{ marginBottom: 16 }}
        title="🔴 处置边界：下架的是「展示」，不是「已发生的世界事实」"
        description={
          <>
            处置只改主体的审核态列（不再可被选入新世界、不再随读取面下发）。已落定的世界事实
            —— <code>world_events</code> / <code>world_ticks</code> / <code>world_members</code> /
            <code> world_contributions</code> / <code>world_biographies</code> —— <b>一个字节都不改</b>
            （§0.3 公共事实不可回滚）。一张卡被下架，不意味着它参演过的世界事实要被抹掉。
            <br />
            因此<b>已在进行中的世界不受影响</b>：入场闸只在入场时判一次，被下架的卡会继续参演存量世界。
            需要立即中止请走「世界运营 → 暂停世界」；本入口不代做强制离场（那会改动世界线相关表，需红线评审）。
          </>
        }
      />

      <Space style={{ marginBottom: 16 }} wrap>
        <span>主体类型：</span>
        <Select style={{ width: 150 }} value={kind} onChange={setKind} options={KIND_OPTIONS} />
        <Input
          style={{ width: 340 }}
          value={subjectId}
          placeholder="粘贴主体 ID（角色卡 / 世界 / 模板 id）"
          onChange={(e) => setSubjectId(e.target.value)}
          onPressEnter={() => lookup(kind, subjectId)}
          allowClear
        />
        <Button type="primary" loading={looking} onClick={() => lookup(kind, subjectId)}>
          查询处置态
        </Button>
      </Space>

      {lookupError && <ErrorAlert message={lookupError} onRetry={() => lookup(kind, subjectId)} />}

      {subject && (
        <SubjectPanel
          subject={subject}
          onAction={setAction}
          onRefresh={() => lookup(subject.subjectKind, subject.subjectId)}
        />
      )}

      {subject && (
        <ActionModal
          action={action}
          subject={subject}
          onClose={() => setAction(null)}
          onDone={() => {
            lookup(subject.subjectKind, subject.subjectId);
            reload();
          }}
        />
      )}

      <Typography.Title level={5}>处置台账</Typography.Title>
      <Space style={{ marginBottom: 12 }}>
        <span>处置状态：</span>
        <Select
          style={{ width: 180 }}
          value={stateFilter}
          onChange={setStateFilter}
          options={STATE_FILTER}
        />
        <Button onClick={reload}>刷新</Button>
      </Space>

      {ledger.error && <ErrorAlert message={ledger.error} onRetry={reload} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={ledger.items}
        loading={ledger.loading}
        pagination={false}
        scroll={{ x: 960 }}
        locale={{
          emptyText: ledger.error
            ? '取数失败，见上方错误提示'
            : '该筛选条件下没有处置记录（这是真实结果，不是加载失败）',
        }}
      />

      {/* 「加载更多」仅在服务端回了非空 nextCursor 时出现；末页回 null，因此按钮消失 = 真的翻完了。 */}
      {ledger.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={ledger.loadMore} loading={ledger.loading}>
            加载更多
          </Button>
        </div>
      )}

      <DisposalAppealQueue onResolved={reload} />

      <Typography.Paragraph type="secondary" style={{ marginTop: 20 }}>
        处置全程写审计日志（<code>content.takedown</code> / <code>content.takedown_permanent</code> /
        <code> content.restore</code> / <code>content.recheck</code>）与风控留痕
        （<code>risk_events.kind = 'content_disposal' / 'content_recheck'</code>），与状态改动同事务。
        永久移除为超级管理员专属；可恢复下架与恢复为审核角色。作者侧只看到「已被下架 / 已被永久移除」
        与时间，<b>看不到此处填写的内部理由</b>。
      </Typography.Paragraph>
    </div>
  );
}

// ---------------- 处置申诉队列（migration 0045） ----------------
//
// 🔴 与上方「申诉复审」分区（`/admin/appeals`）是**两件事**，界面上不合并：
//   - 那条受理「发布时被驳回」，改判 = 直接放行；
//   - 这条受理「过审后被处置」，改判 = 走恢复台阶写回下架前的状态、并把处置台账翻成已恢复。
// 合并会诱导运营用错的入口做错的改判，而错的那条会绕过恢复台阶。
//
// 🔴 这里**显示**下架时填的运营内部理由（`disposal.reason`）——人审要据以判断当初为什么下架。
// 它只出现在本后台面；作者侧看到的永远只有下方「答复」框里写的那段话。

const APPEAL_STATUS_TEXT: Record<string, { color: string; text: string }> = {
  pending: { color: 'gold', text: '待裁决' },
  upheld: { color: 'default', text: '维持处置' },
  overturned: { color: 'green', text: '改判 · 已恢复' },
};

function ResolveAppealModal({
  appeal,
  onClose,
  onDone,
}: {
  appeal: DisposalAppealRow | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const [reason, setReason] = useState('');
  const [decision, setDecision] = useState<'uphold' | 'overturn'>('uphold');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (appeal) {
      setReason('');
      setDecision('uphold');
    }
  }, [appeal]);

  if (!appeal) return null;
  // 永久移除不可恢复：改判在服务端恒 409，界面提前说清楚而不是让人点了才吃报错。
  const permanentlyRemoved = appeal.disposal?.state === 'removed';

  const submit = async () => {
    if (!reason.trim()) {
      message.warning('答复必填');
      return;
    }
    setBusy(true);
    try {
      await adminFetch(`/admin/content/appeals/${encodeURIComponent(appeal.id)}/resolve`, 'POST', {
        decision,
        reason: reason.trim(),
      });
      message.success(decision === 'overturn' ? '已改判并恢复' : '已维持处置');
      onDone();
      onClose();
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open
      title="裁决处置申诉"
      okText={decision === 'overturn' ? '改判并恢复' : '维持处置'}
      confirmLoading={busy}
      onOk={submit}
      onCancel={onClose}
    >
      <Descriptions size="small" column={1} bordered style={{ marginBottom: 12 }}>
        <Descriptions.Item label="主体">
          {KIND_TEXT[appeal.subjectKind] || appeal.subjectKind} · <code>{appeal.subjectId}</code>
        </Descriptions.Item>
        <Descriptions.Item label="作者申诉">{appeal.appealText}</Descriptions.Item>
        {/* 内部理由：本页可见，作者侧永不下发。 */}
        <Descriptions.Item label="当初的下架理由（内部）">
          {appeal.disposal?.reason ?? '—'}
        </Descriptions.Item>
      </Descriptions>

      <Space direction="vertical" style={{ width: '100%' }} size="middle">
        <Select
          style={{ width: '100%' }}
          value={decision}
          onChange={(v) => setDecision(v)}
          options={[
            { value: 'uphold', label: '维持处置（内容保持下架）' },
            {
              value: 'overturn',
              label: '改判并恢复（写回下架前的展示态）',
              disabled: permanentlyRemoved,
            },
          ]}
        />
        {permanentlyRemoved && (
          <Alert
            type="warning"
            showIcon
            message="该内容已被永久移除，不可恢复"
            description="永久移除是不可逆处置（位图连对象字节一并删除）。此处只能维持，并在答复里向作者说明；如需重新上线，由作者重新发布。"
          />
        )}
        <Input.TextArea
          rows={4}
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder="填写答复（必填，最长 500 字）。🔴 这段话会原样展示给作者，请按对外口径书写。"
        />
      </Space>
    </Modal>
  );
}

function DisposalAppealQueue({ onResolved }: { onResolved: () => void }) {
  const [statusFilter, setStatusFilter] = useState('pending');
  const [target, setTarget] = useState<DisposalAppealRow | null>(null);

  const queue = usePagedList<DisposalAppealRow>(async (cursor) => {
    const qs = new URLSearchParams({ limit: '20', status: statusFilter });
    if (cursor) qs.set('cursor', cursor);
    const res = await adminFetch<{ items: DisposalAppealRow[]; nextCursor: string | null }>(
      `/admin/content/appeals?${qs.toString()}`,
    );
    return { items: res.items, nextCursor: res.nextCursor };
  });
  const { reload } = queue;
  useEffect(() => {
    reload();
  }, [statusFilter, reload]);

  const columns: TableColumnsType<DisposalAppealRow> = [
    {
      title: '主体',
      key: 'subject',
      render: (_, r) => (
        <Space direction="vertical" size={0}>
          <span>{KIND_TEXT[r.subjectKind] || r.subjectKind}</span>
          <code style={{ fontSize: 12 }}>{r.subjectId}</code>
        </Space>
      ),
    },
    { title: '作者申诉', dataIndex: 'appealText', width: 260 },
    {
      title: '处置态',
      key: 'disposalState',
      render: (_, r) => stateTag(r.disposal?.state ?? r.disposalState),
    },
    {
      title: '状态',
      key: 'status',
      render: (_, r) => {
        const meta = APPEAL_STATUS_TEXT[r.status];
        return <Tag color={meta?.color}>{meta?.text ?? r.status}</Tag>;
      },
    },
    {
      title: '答复（作者可见）',
      key: 'resolution',
      render: (_, r) => r.resolutionReason ?? '—',
    },
    { title: '提交时刻', key: 'createdAt', render: (_, r) => formatTime(r.createdAt) },
    {
      title: '操作',
      key: 'op',
      render: (_, r) =>
        r.status === 'pending' ? (
          <Button size="small" onClick={() => setTarget(r)}>
            裁决
          </Button>
        ) : (
          '—'
        ),
    },
  ];

  return (
    <>
      <Typography.Title level={5} style={{ marginTop: 28 }}>
        处置申诉
      </Typography.Title>
      <Typography.Paragraph type="secondary">
        被下架的作者对<b>处置本身</b>提的异议。与上方「申诉复审」分属两条路径：那条受理「发布时被驳回」、
        改判即放行；这条受理「过审后被处置」、改判走恢复台阶写回下架前的展示态并把台账翻成已恢复。
        每<b>一次处置</b>受理一次申诉——内容恢复后又被重新下架的，作者重新获得申诉权。
      </Typography.Paragraph>
      <Space style={{ marginBottom: 12 }}>
        <span>申诉状态：</span>
        <Select
          style={{ width: 180 }}
          value={statusFilter}
          onChange={setStatusFilter}
          options={[
            { value: 'pending', label: '待裁决' },
            { value: 'upheld', label: '维持处置' },
            { value: 'overturned', label: '改判 · 已恢复' },
            { value: 'all', label: '全部' },
          ]}
        />
        <Button onClick={reload}>刷新</Button>
      </Space>

      {queue.error && <ErrorAlert message={queue.error} onRetry={reload} />}

      <Table
        rowKey="id"
        size="small"
        columns={columns}
        dataSource={queue.items}
        loading={queue.loading}
        pagination={false}
        scroll={{ x: 1100 }}
        locale={{
          emptyText: queue.error
            ? '取数失败，见上方错误提示'
            : '该筛选条件下没有处置申诉（这是真实结果，不是加载失败）',
        }}
      />

      {queue.hasMore && (
        <div style={{ textAlign: 'center', marginTop: 16 }}>
          <Button onClick={queue.loadMore} loading={queue.loading}>
            加载更多
          </Button>
        </div>
      )}

      <ResolveAppealModal
        appeal={target}
        onClose={() => setTarget(null)}
        onDone={() => {
          reload();
          onResolved();
        }}
      />
    </>
  );
}
