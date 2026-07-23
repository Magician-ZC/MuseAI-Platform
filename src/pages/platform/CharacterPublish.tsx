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
} from 'antd';
import { CloudUploadOutlined, FileTextOutlined, ReloadOutlined } from '@ant-design/icons';
import { usePartnerStore } from '../../stores/usePartnerStore';
import type { CharacterCardV2 } from '../../utils/characterCardV2';
import { cloudFetch, CloudError } from '../../utils/cloudApi';
import { describeCloudError, moderationMeta, type CloudCharacter } from '../../stores/usePlatformStore';

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

  const selected: CharacterCardV2 | undefined = cards.find((c) => c.id === selectedId);

  const loadMine = async () => {
    setMineLoading(true);
    setMineError(null);
    try {
      const data = await cloudFetch<CloudCharacter[]>('/api/assets/characters/mine');
      setMine(Array.isArray(data) ? data : []);
    } catch (e) {
      setMineError(describeCloudError(e));
    } finally {
      setMineLoading(false);
    }
  };

  useEffect(() => {
    void loadMine();
  }, []);

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
      const s = await cloudFetch<{ id: string; moderation: string; version: number; withdrawn: boolean }>(
        `/api/assets/characters/${id}/status`,
      );
      setMine((prev) => prev.map((c) => (c.id === id ? { ...c, moderation: s.moderation, withdrawn: s.withdrawn } : c)));
    } catch (e) {
      setMineError(describeCloudError(e));
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
              { title: '本地卡', dataIndex: 'localCardId', ellipsis: true },
              { title: '版本', dataIndex: 'version', width: 70, render: (v: number) => `v${v}` },
              { title: '权利', dataIndex: 'rightsDeclaration', width: 120, render: rightsLabel },
              {
                title: '审核态',
                dataIndex: 'moderation',
                width: 90,
                render: (m: string) => {
                  const meta = moderationMeta(m);
                  return <Tag color={meta.color}>{meta.label}</Tag>;
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
                width: 200,
                render: (_: unknown, r: CloudCharacter) => (
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
                  </Space>
                ),
              },
            ]}
          />
        )}
      </Card>

      <Paragraph type="secondary" style={{ fontSize: 12, marginTop: 16 }}>
        私密房只降低发现与传播范围，不豁免平台的数据、内容与版权义务；公开分发会叠加更严格的权利证明与人审。
      </Paragraph>
    </div>
  );
};

export default CharacterPublish;
