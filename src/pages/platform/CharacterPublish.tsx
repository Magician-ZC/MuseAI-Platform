// 发布本地角色到云端（C1，规格 §2.3）：选卡 + 权利声明 + 最小发布清单 → POST /assets/characters → 审核态查询。
// 三分离：本地模板永远归用户；此处只发布「不可变快照」进审核队列，本地后续编辑不回写已发布版本。
import React, { useEffect, useState } from 'react';
import {
  Row,
  Col,
  Card,
  List,
  Radio,
  Checkbox,
  Button,
  Tag,
  Alert,
  Table,
  Space,
  Typography,
  Empty,
  Popconfirm,
  Avatar,
  Upload,
  Select,
  Modal,
  Tooltip,
  Input,
  message,
} from 'antd';
import {
  CloudUploadOutlined,
  FileTextOutlined,
  ReloadOutlined,
  UploadOutlined,
  UserOutlined,
} from '@ant-design/icons';
import { usePartnerStore } from '../../stores/usePartnerStore';
import type { CharacterCardV2 } from '../../utils/characterCardV2';
import { cloudFetch, CloudError, uploadAvatar, resolveObjectUrl } from '../../utils/cloudApi';
import { compressAvatarImage, ACCEPTED_AVATAR_MIME } from '../../utils/imageAvatar';
import {
  describeCloudError,
  moderationMeta,
  appealStatusMeta,
  type CloudCharacter,
  type CloudCharacterStatus,
  type CharacterAppeal,
} from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

type Rights = 'original' | 'public_domain_adaptation';

const rightsLabel = (r: string): string =>
  r === 'original' ? '原创' : r === 'public_domain_adaptation' ? '公有领域改编' : r;

const lifecycleMeta = (lc: string): { label: string; color: string } => {
  switch (lc) {
    case 'ready':
      return { label: '已就绪', color: 'green' };
    case 'reviewed':
      return { label: '已复核', color: 'blue' };
    default:
      return { label: '草稿', color: 'default' };
  }
};

const CharacterPublish: React.FC = () => {
  const cards = usePartnerStore((s) => s.characterCardsV2);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [rights, setRights] = useState<Rights>('original');
  const [agreed, setAgreed] = useState(false);
  const [publishing, setPublishing] = useState(false);
  const [feedback, setFeedback] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

  const [mine, setMine] = useState<CloudCharacter[]>([]);
  const [mineLoading, setMineLoading] = useState(false);
  const [mineError, setMineError] = useState<string | null>(null);

  // 角色头像：目标为已发布的云端角色（头像挂在云端角色上，需其 id）。
  const [avatarCharId, setAvatarCharId] = useState<string | null>(null);
  const [avatarUploading, setAvatarUploading] = useState(false);
  const [avatarFeedback, setAvatarFeedback] = useState<{ type: 'success' | 'info' | 'error'; text: string } | null>(
    null,
  );

  // 驳回申诉：目标行 + 申诉正文（Modal 内 TextArea，trim 后 1..=500 必填）。
  const [appealTarget, setAppealTarget] = useState<CloudCharacter | null>(null);
  const [appealText, setAppealText] = useState('');
  const [appealSubmitting, setAppealSubmitting] = useState(false);

  const selected: CharacterCardV2 | undefined = cards.find((c) => c.id === selectedId);
  const avatarChar = mine.find((c) => c.id === avatarCharId);
  const currentAvatarUrl = resolveObjectUrl(avatarChar?.avatarUrl);

  // 卸载守卫：status 补拉是浮动 promise（loadMine 逐行 best-effort + refreshStatus），
  // 组件卸载后 resolve 再 setState 会在测试里触发环境销毁后的调度（window is not defined）。
  const aliveRef = React.useRef(true);
  useEffect(() => {
    aliveRef.current = true;
    return () => {
      aliveRef.current = false;
    };
  }, []);

  // 把 status 端点回读（审核态 + 驳回理由 + 申诉状态）合并进对应行。
  const mergeStatus = (id: string, s: CloudCharacterStatus) => {
    if (!aliveRef.current) return; // 卸载后丢弃迟到的回读
    setMine((prev) =>
      prev.map((c) =>
        c.id === id
          ? { ...c, moderation: s.moderation, withdrawn: s.withdrawn, rejectReason: s.rejectReason, appeal: s.appeal }
          : c,
      ),
    );
  };

  const loadMine = async () => {
    setMineLoading(true);
    setMineError(null);
    try {
      const data = await cloudFetch<CloudCharacter[]>('/api/assets/characters/mine');
      const list = Array.isArray(data) ? data : [];
      setMine(list);
      // 驳回理由与申诉状态仅 status 端点下发：对 rejected 行补拉（best-effort，单行失败静默不阻塞列表）。
      for (const c of list.filter((x) => x.moderation === 'rejected')) {
        void cloudFetch<CloudCharacterStatus>(`/api/assets/characters/${c.id}/status`)
          .then((s) => mergeStatus(c.id, s))
          .catch(() => {});
      }
    } catch (e) {
      setMineError(describeCloudError(e));
    } finally {
      setMineLoading(false);
    }
  };

  useEffect(() => {
    void loadMine();
  }, []);

  // mine 变化后维持一个有效的头像目标：保留当前选择，否则回落到首个云端角色。
  useEffect(() => {
    if (mine.length === 0) {
      setAvatarCharId(null);
      return;
    }
    setAvatarCharId((cur) => (cur && mine.some((c) => c.id === cur) ? cur : mine[0].id));
  }, [mine]);

  // 处理头像文件：校验 MIME → 压缩（最长边≤256）→ 纯 base64 → 上传 → 按审核态提示。
  const handleAvatarFile = async (file: File) => {
    if (!avatarCharId) return;
    if (!(ACCEPTED_AVATAR_MIME as readonly string[]).includes(file.type)) {
      setAvatarFeedback({ type: 'error', text: '仅支持 PNG / JPEG / WebP 格式的图片' });
      return;
    }
    setAvatarUploading(true);
    setAvatarFeedback(null);
    try {
      const { imageBase64, mime } = await compressAvatarImage(file);
      const res = await uploadAvatar(avatarCharId, imageBase64, mime);
      if (res.moderation === 'approved' && res.avatarUrl) {
        const url = res.avatarUrl;
        setMine((prev) => prev.map((c) => (c.id === avatarCharId ? { ...c, avatarUrl: url } : c)));
        setAvatarFeedback({ type: 'success', text: '头像已通过审核并更新' });
      } else if (res.moderation === 'pending') {
        setAvatarFeedback({ type: 'info', text: '头像已提交，审核中；通过后将自动展示' });
      } else {
        setAvatarFeedback({ type: 'error', text: '头像未通过审核，请更换图片后重试' });
      }
    } catch (e) {
      setAvatarFeedback({ type: 'error', text: describeCloudError(e) });
    } finally {
      setAvatarUploading(false);
    }
  };

  const publish = async () => {
    if (!selected) return;
    setFeedback(null);
    setPublishing(true);
    try {
      const view = await cloudFetch<CloudCharacter>('/api/assets/characters', {
        method: 'POST',
        idempotent: true,
        body: {
          localCardId: selected.id,
          cardJson: selected,
          rightsDeclaration: rights,
        },
      });
      const m = moderationMeta(view.moderation);
      setFeedback({
        type: 'success',
        text: `已提交发布：${selected.identity.name}（第 ${view.version} 版），当前审核态：${m.label}`,
      });
      setAgreed(false);
      await loadMine();
    } catch (e) {
      setFeedback({ type: 'error', text: describeCloudError(e) });
    } finally {
      setPublishing(false);
    }
  };

  const refreshStatus = async (id: string) => {
    try {
      const s = await cloudFetch<CloudCharacterStatus>(`/api/assets/characters/${id}/status`);
      mergeStatus(id, s);
    } catch (e) {
      setMineError(describeCloudError(e));
    }
  };

  // 提交申诉：仅驳回内容可申诉；红线——提交不改 moderation，改判仅由后台复核完成。
  const submitAppeal = async () => {
    if (!appealTarget) return;
    const text = appealText.trim();
    if (!text) return;
    setAppealSubmitting(true);
    try {
      const row = await cloudFetch<CharacterAppeal>(`/api/assets/characters/${appealTarget.id}/appeal`, {
        method: 'POST',
        idempotent: true,
        body: { text },
      });
      setMine((prev) =>
        prev.map((c) =>
          c.id === appealTarget.id
            ? {
                ...c,
                appeal: {
                  status: row.status,
                  appealText: row.appealText,
                  resolutionReason: row.resolutionReason,
                  createdAt: row.createdAt,
                  resolvedAt: row.resolvedAt,
                },
              }
            : c,
        ),
      );
      setAppealTarget(null);
      setAppealText('');
      message.success('申诉已提交，复核结果将在此处更新');
    } catch (e) {
      // 409（已申诉过）/ 400（非驳回态、字数不合规）等：直接透出服务端中文文案。
      message.error(describeCloudError(e));
      if (e instanceof CloudError && e.status === 409) {
        // 已存在申诉（如另一端提交过）：拉回该行申诉状态并收起弹窗。
        void refreshStatus(appealTarget.id);
        setAppealTarget(null);
      }
    } finally {
      setAppealSubmitting(false);
    }
  };

  const withdraw = async (id: string) => {
    try {
      await cloudFetch(`/api/assets/characters/${id}/withdraw`, { method: 'POST', idempotent: true });
      await loadMine();
    } catch (e) {
      setMineError(describeCloudError(e));
    }
  };

  const remove = async (id: string) => {
    try {
      await cloudFetch(`/api/assets/characters/${id}`, { method: 'DELETE', idempotent: true });
      await loadMine();
    } catch (e) {
      // 404（已删）视为成功刷新
      if (e instanceof CloudError && e.code === 'not_found') {
        await loadMine();
        return;
      }
      setMineError(describeCloudError(e));
    }
  };

  return (
    <div style={{ padding: '32px 40px', maxWidth: 1100, margin: '0 auto' }}>
      <div style={{ marginBottom: 24 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <CloudUploadOutlined style={{ color: '#d97757', marginRight: 10 }} />
          发布角色到云端
        </Title>
        <Text type="secondary">
          发布的是不可变快照，进审核队列（机审 + 风险分层人审）。本地模板永远归你，后续编辑不影响已发布版本。
        </Text>
      </div>

      <Row gutter={[20, 20]}>
        {/* 选卡 */}
        <Col xs={24} md={9}>
          <Card
            title="选择本地角色（V2）"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: cards.length ? 8 : 24 } }}
          >
            {cards.length === 0 ? (
              <Empty description="暂无 V2 角色卡，请先在本地「背景」中生成或升级" />
            ) : (
              <List
                dataSource={cards}
                rowKey={(c) => c.id}
                renderItem={(c) => {
                  const lm = lifecycleMeta(c.lifecycle);
                  return (
                    <List.Item
                      onClick={() => setSelectedId(c.id)}
                      style={{
                        cursor: 'pointer',
                        padding: '10px 12px',
                        borderRadius: 8,
                        background: c.id === selectedId ? '#f2e8dc' : 'transparent',
                      }}
                    >
                      <Space style={{ justifyContent: 'space-between', width: '100%' }}>
                        <Text strong>{c.identity.name || '未命名角色'}</Text>
                        <Tag color={lm.color}>{lm.label}</Tag>
                      </Space>
                    </List.Item>
                  );
                }}
              />
            )}
          </Card>
        </Col>

        {/* 发布表单 */}
        <Col xs={24} md={15}>
          <Card
            title="权利声明与发布"
            style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
            styles={{ body: { padding: 20 } }}
          >
            {!selected ? (
              <Empty description="从左侧选择一张角色卡" />
            ) : (
              <Space direction="vertical" size={16} style={{ width: '100%' }}>
                <Space size={10}>
                  <Text strong style={{ fontSize: 16 }}>
                    {selected.identity.name || '未命名角色'}
                  </Text>
                  <Tag color={lifecycleMeta(selected.lifecycle).color}>
                    {lifecycleMeta(selected.lifecycle).label}
                  </Tag>
                </Space>

                {selected.lifecycle !== 'ready' && (
                  <Alert
                    type="warning"
                    showIcon
                    message="该卡尚未达到「就绪」，仍可发布，但建议先在本地补全关键行为字段与证据。"
                  />
                )}

                <div>
                  <Text strong>权利基础</Text>
                  <div style={{ marginTop: 8 }}>
                    <Radio.Group value={rights} onChange={(e) => setRights(e.target.value)}>
                      <Radio value="original">原创</Radio>
                      <Radio value="public_domain_adaptation">公有领域改编</Radio>
                    </Radio.Group>
                  </div>
                </div>

                <Card size="small" style={{ background: '#faf9f5', border: '1px solid #eae6df' }}>
                  <Space direction="vertical" size={4}>
                    <Text strong style={{ fontSize: 13 }}>
                      <FileTextOutlined /> 最小发布清单（本次上传内容）
                    </Text>
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      · 角色版本 DNA（十层结构，运行所需）
                    </Text>
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      · 权利元数据（{rightsLabel(rights)}）
                    </Text>
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      不上传：本地证据账本原文、用户关系记忆、故事历史
                    </Text>
                  </Space>
                </Card>

                <Checkbox checked={agreed} onChange={(e) => setAgreed(e.target.checked)}>
                  我确认对该角色拥有相应权利，并同意接受平台内容与安全审核。
                </Checkbox>

                {feedback && (
                  <Alert
                    type={feedback.type}
                    showIcon
                    message={feedback.text}
                    closable
                    onClose={() => setFeedback(null)}
                  />
                )}

                <Button
                  type="primary"
                  icon={<CloudUploadOutlined />}
                  loading={publishing}
                  disabled={!agreed}
                  onClick={() => void publish()}
                >
                  发布此版本
                </Button>
              </Space>
            )}
          </Card>
        </Col>
      </Row>

      {/* 角色头像（挂在已发布的云端角色上） */}
      <Card
        title="角色头像"
        style={{ marginTop: 20, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 20 } }}
      >
        {mine.length === 0 ? (
          <Empty description="先发布角色到云端，再为其上传头像" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <Space direction="vertical" size={14} style={{ width: '100%' }}>
            <Text type="secondary" style={{ fontSize: 12 }}>
              头像会经过内容审核，仅通过后才对外展示（未过审时各处自动回退为首字头像）。图片将压缩至最长边 256px 后上传。
            </Text>
            <Space size={16} wrap>
              <Text strong style={{ fontSize: 13 }}>
                选择云端角色
              </Text>
              <Select
                value={avatarCharId ?? undefined}
                onChange={(v) => {
                  setAvatarCharId(v);
                  setAvatarFeedback(null);
                }}
                style={{ minWidth: 280 }}
                placeholder="选择一个已发布的角色"
                options={mine.map((c) => ({
                  value: c.id,
                  label: `${c.localCardId} · v${c.version}（${moderationMeta(c.moderation).label}）`,
                }))}
              />
            </Space>
            <Space size={16} align="center" wrap>
              <Avatar size={72} src={currentAvatarUrl} icon={<UserOutlined />} shape="circle">
                {!currentAvatarUrl ? (avatarChar?.localCardId?.[0] ?? '角') : null}
              </Avatar>
              <Space direction="vertical" size={4}>
                <Text type="secondary" style={{ fontSize: 12 }}>
                  {currentAvatarUrl ? '当前头像（已过审）' : '该角色暂无已过审头像'}
                </Text>
                <Upload
                  accept="image/png,image/jpeg,image/webp"
                  showUploadList={false}
                  disabled={!avatarCharId || avatarUploading}
                  beforeUpload={(file) => {
                    // 返回 false 阻止 antd 自动上传，自己走压缩 + uploadAvatar。
                    void handleAvatarFile(file as File);
                    return false;
                  }}
                >
                  <Button icon={<UploadOutlined />} loading={avatarUploading} disabled={!avatarCharId}>
                    上传头像
                  </Button>
                </Upload>
              </Space>
            </Space>
            {avatarFeedback && (
              <Alert
                type={avatarFeedback.type}
                showIcon
                message={avatarFeedback.text}
                closable
                onClose={() => setAvatarFeedback(null)}
              />
            )}
          </Space>
        )}
      </Card>

      {/* 我的云端版本 */}
      <Card
        title="我的云端版本"
        style={{ marginTop: 20, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
        styles={{ body: { padding: 12 } }}
        extra={
          <Button size="small" icon={<ReloadOutlined />} onClick={() => void loadMine()} loading={mineLoading}>
            刷新
          </Button>
        }
      >
        {mineError ? (
          <Alert type="error" showIcon message="连接平台失败" description={mineError} />
        ) : (
          <Table<CloudCharacter>
            dataSource={mine}
            rowKey="id"
            loading={mineLoading}
            pagination={false}
            size="small"
            locale={{ emptyText: '尚无已发布版本' }}
            columns={[
              {
                title: '头像',
                key: 'avatar',
                width: 60,
                render: (_: unknown, r: CloudCharacter) => {
                  const url = resolveObjectUrl(r.avatarUrl);
                  return (
                    <Avatar size={32} src={url} icon={<UserOutlined />} shape="circle">
                      {!url ? (r.localCardId?.[0] ?? '角') : null}
                    </Avatar>
                  );
                },
              },
              { title: '本地卡', dataIndex: 'localCardId', ellipsis: true },
              { title: '版本', dataIndex: 'version', width: 70, render: (v: number) => `v${v}` },
              { title: '权利', dataIndex: 'rightsDeclaration', width: 120, render: rightsLabel },
              {
                title: '审核态',
                dataIndex: 'moderation',
                width: 150,
                render: (m: string, r: CloudCharacter) => {
                  const meta = moderationMeta(m);
                  return (
                    <Space direction="vertical" size={2}>
                      <Tag color={meta.color}>{meta.label}</Tag>
                      {m === 'rejected' && r.rejectReason && (
                        <Tooltip title={`驳回理由：${r.rejectReason}`}>
                          <Text type="danger" style={{ fontSize: 12 }}>
                            {r.rejectReason}
                          </Text>
                        </Tooltip>
                      )}
                    </Space>
                  );
                },
              },
              {
                title: '状态',
                dataIndex: 'withdrawn',
                width: 80,
                render: (w: boolean) => (w ? <Tag color="red">已撤回</Tag> : <Tag color="green">在用</Tag>),
              },
              {
                title: '操作',
                key: 'ops',
                width: 250,
                render: (_: unknown, r: CloudCharacter) => {
                  const appealMeta = r.appeal ? appealStatusMeta(r.appeal.status) : null;
                  const appealTag = appealMeta && r.appeal && (
                    <Tag color={appealMeta.color} style={{ marginInlineEnd: 0 }}>
                      {appealMeta.label}
                    </Tag>
                  );
                  return (
                    <Space size={4}>
                      <Button size="small" type="link" onClick={() => void refreshStatus(r.id)}>
                        刷新态
                      </Button>
                      {!r.withdrawn && (
                        <Popconfirm title="撤回后停止后续投放，确认？" onConfirm={() => void withdraw(r.id)}>
                          <Button size="small" type="link">
                            撤回
                          </Button>
                        </Popconfirm>
                      )}
                      <Popconfirm title="删除云端资产？运行中世界按入场协议处理" onConfirm={() => void remove(r.id)}>
                        <Button size="small" type="link" danger>
                          删除
                        </Button>
                      </Popconfirm>
                      {/* 申诉入口：仅驳回且尚未申诉时展示；已有申诉只展示状态（每主体终身一次）。 */}
                      {r.appeal ? (
                        r.appeal.resolutionReason ? (
                          <Tooltip title={`复核理由：${r.appeal.resolutionReason}`}>{appealTag}</Tooltip>
                        ) : (
                          appealTag
                        )
                      ) : r.moderation === 'rejected' ? (
                        <Button
                          size="small"
                          type="link"
                          onClick={() => {
                            setAppealTarget(r);
                            setAppealText('');
                          }}
                        >
                          申诉
                        </Button>
                      ) : null}
                    </Space>
                  );
                },
              },
            ]}
          />
        )}
      </Card>

      <Paragraph type="secondary" style={{ fontSize: 12, marginTop: 16 }}>
        私密房只降低发现与传播范围，不豁免平台的数据、内容与版权义务；公开分发会叠加更严格的权利证明与人审。
      </Paragraph>

      {/* 申诉弹窗：提交进入人工复核队列；改判前原审核结果继续生效（提交不改 moderation）。 */}
      <Modal
        title="对审核结果发起申诉"
        open={!!appealTarget}
        onCancel={() => setAppealTarget(null)}
        onOk={() => void submitAppeal()}
        okText="提交申诉"
        cancelText="取消"
        confirmLoading={appealSubmitting}
        okButtonProps={{ disabled: !appealText.trim() }}
      >
        <Space direction="vertical" size={10} style={{ width: '100%' }}>
          {appealTarget?.rejectReason && (
            <Alert type="warning" showIcon message={`驳回理由：${appealTarget.rejectReason}`} />
          )}
          <Text type="secondary" style={{ fontSize: 12 }}>
            每个内容仅可申诉一次，提交后进入人工复核；复核改判前，原审核结果继续生效。
          </Text>
          <Input.TextArea
            rows={4}
            maxLength={500}
            showCount
            placeholder="请说明理由（必填，500 字以内），如权利证明、误判说明等"
            value={appealText}
            onChange={(e) => setAppealText(e.target.value)}
          />
        </Space>
      </Modal>
    </div>
  );
};

export default CharacterPublish;
