// 知识包管理器（规格 §4 / §9.4 / §11）：包列表 + 导入（权利基础/允许用途/保留策略）+ 四模式蒸馏 +
// 检索预览 + 绑定角色（influence 滑块 + 冲突策略）+ 删除 + 启停对照。用 useKnowledgePackStore。
import React from 'react';
import {
  Modal,
  Tabs,
  Table,
  Button,
  Input,
  Select,
  Slider,
  Switch,
  Space,
  Typography,
  Tag,
  Alert,
  Empty,
  List,
  Popconfirm,
  message,
  Form,
} from 'antd';
import { ImportOutlined, ExperimentOutlined, SearchOutlined, DeleteOutlined } from '@ant-design/icons';
import {
  useKnowledgePackStore,
  type KnowledgePack,
  type KnowledgeBinding,
  type PackMode,
  type RightsBasis,
  type AllowedUse,
  type Retention,
  type ConflictPolicy,
  type ImportKnowledgeRequest,
} from '../stores/useKnowledgePackStore';
import type { ModelProfile } from '../stores/useExtractionStore';
import { useSettingsStore } from '../stores/useSettingsStore';
import { usePartnerStore } from '../stores/usePartnerStore';

const { Text, Paragraph } = Typography;

export interface KnowledgePackManagerProps {
  open: boolean;
  onClose: () => void;
}

const MODE_OPTIONS: Array<{ value: PackMode; label: string }> = [
  { value: 'knowledge', label: '知识包（知道什么）' },
  { value: 'mind', label: '思维包（如何分析）' },
  { value: 'value', label: '价值包（价值尺度）' },
  { value: 'expression', label: '表达包（语言组织）' },
];

const RIGHTS_OPTIONS: Array<{ value: RightsBasis; label: string }> = [
  { value: 'owned', label: '自有版权' },
  { value: 'licensed', label: '已获授权' },
  { value: 'public_domain', label: '公有领域' },
  { value: 'personal_use', label: '个人使用' },
  { value: 'unknown', label: '未知' },
];

const ALLOWED_USE_OPTIONS: Array<{ value: AllowedUse; label: string }> = [
  { value: 'extract', label: '提取' },
  { value: 'retrieve', label: '检索' },
  { value: 'generate', label: '生成' },
  { value: 'send_to_remote_model', label: '发送给远程模型' },
  { value: 'publish', label: '发布' },
];

const RETENTION_OPTIONS: Array<{ value: Retention; label: string }> = [
  { value: 'index_only', label: '仅索引（不留副本）' },
  { value: 'reference_original', label: '引用原文件' },
  { value: 'managed_copy', label: '受控副本' },
];

const CONFLICT_OPTIONS: Array<{ value: ConflictPolicy; label: string }> = [
  { value: 'character_core_wins', label: '角色内核优先' },
  { value: 'ask_user', label: '询问用户' },
];

function useModelProfile(): ModelProfile | null {
  const models = useSettingsStore((s) => s.models);
  const selectedModelId = useSettingsStore((s) => s.selectedModelId);
  const model = models?.find((m) => m.id === selectedModelId) || models?.[0];
  if (!model) return null;
  return {
    interface: model.modelInterface,
    baseUrl: model.baseUrl,
    apiKey: model.apiKey,
    model: model.model,
  };
}

const KnowledgePackManager: React.FC<KnowledgePackManagerProps> = ({ open, onClose }) => {
  const packs = useKnowledgePackStore((s) => s.packs);
  const bindings = useKnowledgePackStore((s) => s.bindings);
  const fragments = useKnowledgePackStore((s) => s.fragments);
  const lastError = useKnowledgePackStore((s) => s.lastError);
  const listPacks = useKnowledgePackStore((s) => s.listPacks);
  const listBindings = useKnowledgePackStore((s) => s.listBindings);
  const importSource = useKnowledgePackStore((s) => s.importSource);
  const distill = useKnowledgePackStore((s) => s.distill);
  const search = useKnowledgePackStore((s) => s.search);
  const deletePack = useKnowledgePackStore((s) => s.deletePack);
  const upsertBinding = useKnowledgePackStore((s) => s.upsertBinding);
  const removeBinding = useKnowledgePackStore((s) => s.removeBinding);

  const settings = useSettingsStore();
  const profile = useModelProfile();
  const charactersV2 = usePartnerStore((s) => s.characterCardsV2);
  const charactersV1 = usePartnerStore((s) => s.characterCards);

  const [importForm] = Form.useForm();
  const [query, setQuery] = React.useState('');
  const [searchPackIds, setSearchPackIds] = React.useState<string[]>([]);
  const [busy, setBusy] = React.useState(false);

  React.useEffect(() => {
    if (open) {
      listPacks().catch(() => undefined);
      listBindings().catch(() => undefined);
    }
  }, [open, listPacks, listBindings]);

  const characterOptions = React.useMemo(() => {
    const v2 = charactersV2.map((c) => ({ value: c.id, label: `${c.identity.name}（V2）` }));
    const v1 = charactersV1.map((c) => ({ value: c.id, label: c.name }));
    return [...v2, ...v1];
  }, [charactersV2, charactersV1]);

  const characterName = React.useCallback(
    (id: string) =>
      charactersV2.find((c) => c.id === id)?.identity.name ||
      charactersV1.find((c) => c.id === id)?.name ||
      id,
    [charactersV1, charactersV2],
  );

  const handleImport = async (values: {
    sourcePath: string;
    title: string;
    rightsBasis: RightsBasis;
    allowedUses: AllowedUse[];
    retention: Retention;
  }) => {
    setBusy(true);
    try {
      const request: ImportKnowledgeRequest = {
        sourcePath: values.sourcePath.trim(),
        title: values.title.trim(),
        rightsBasis: values.rightsBasis,
        allowedUses: values.allowedUses ?? [],
        retention: values.retention,
      };
      const res = await importSource(request);
      message.success(`已导入知识源：切块 ${res.chunkStats.chunkCount} 段`);
      importForm.resetFields();
    } catch (e) {
      message.error(`导入失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleDistill = async (pack: KnowledgePack, mode: PackMode) => {
    if (!profile) {
      message.error('未配置可用模型');
      return;
    }
    setBusy(true);
    try {
      await distill({
        packId: pack.id,
        mode,
        profile,
        promptsByMode: {
          mind: settings.knowledgeDistillMindPrompt,
          value: settings.knowledgeDistillValuePrompt,
          expression: settings.knowledgeDistillExpressionPrompt,
        },
      });
      message.success('蒸馏完成');
    } catch (e) {
      message.error(`蒸馏失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleSearch = async () => {
    if (searchPackIds.length === 0) {
      message.warning('请先选择要检索的知识包');
      return;
    }
    setBusy(true);
    try {
      await search(searchPackIds, query);
    } catch (e) {
      message.error(`检索失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async (packId: string) => {
    try {
      await deletePack(packId);
      message.success('已删除知识包及其索引、绑定');
    } catch (e) {
      message.error(`删除失败：${String(e)}`);
    }
  };

  const handleAddBinding = async (values: {
    packId: string;
    characterId: string;
    influence: number;
    conflictPolicy: ConflictPolicy;
    enabled: boolean;
  }) => {
    const binding: KnowledgeBinding = {
      id: `bind-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
      packId: values.packId,
      characterId: values.characterId,
      influence: values.influence,
      enabled: values.enabled ?? true,
      conflictPolicy: values.conflictPolicy,
    };
    try {
      await upsertBinding(binding);
      message.success('已绑定知识包到角色');
    } catch (e) {
      message.error(`绑定失败：${String(e)}`);
    }
  };

  const toggleBinding = async (binding: KnowledgeBinding, enabled: boolean) => {
    try {
      await upsertBinding({ ...binding, enabled });
    } catch (e) {
      message.error(`更新绑定失败：${String(e)}`);
    }
  };

  const packColumns = [
    { title: '标题', dataIndex: 'title' },
    {
      title: '类型',
      dataIndex: 'mode',
      width: 110,
      render: (mode: PackMode) => (
        <Tag color="#d97757">{MODE_OPTIONS.find((m) => m.value === mode)?.label.slice(0, 3)}</Tag>
      ),
    },
    {
      title: '权利',
      dataIndex: ['source', 'rightsBasis'],
      width: 100,
      render: (rb: RightsBasis) => RIGHTS_OPTIONS.find((r) => r.value === rb)?.label ?? rb,
    },
    {
      title: '远程发送',
      key: 'remote',
      width: 90,
      render: (_: unknown, pack: KnowledgePack) =>
        pack.source.allowedUses.includes('send_to_remote_model') ? (
          <Tag color="orange">允许</Tag>
        ) : (
          <Tag>本地</Tag>
        ),
    },
    {
      title: '蒸馏',
      key: 'distill',
      width: 150,
      render: (_: unknown, pack: KnowledgePack) => (
        <Select<PackMode>
          size="small"
          placeholder="蒸馏为…"
          style={{ width: '100%' }}
          options={MODE_OPTIONS.filter((m) => m.value !== 'knowledge')}
          onChange={(mode) => handleDistill(pack, mode)}
          aria-label={`蒸馏 ${pack.title}`}
          value={undefined}
        />
      ),
    },
    {
      title: '操作',
      key: 'ops',
      width: 70,
      render: (_: unknown, pack: KnowledgePack) => (
        <Popconfirm title="删除该知识包及其索引与绑定？" onConfirm={() => handleDelete(pack.id)}>
          <Button size="small" type="text" danger icon={<DeleteOutlined />} aria-label={`删除 ${pack.title}`} />
        </Popconfirm>
      ),
    },
  ];

  const tabs = [
    {
      key: 'packs',
      label: '知识包',
      children: (
        <div>
          <Form
            form={importForm}
            layout="inline"
            onFinish={handleImport}
            initialValues={{
              rightsBasis: 'owned',
              allowedUses: ['extract', 'retrieve'],
              retention: 'index_only',
            }}
            style={{ marginBottom: 16, rowGap: 8, flexWrap: 'wrap' }}
          >
            <Form.Item name="title" rules={[{ required: true, message: '请输入标题' }]}>
              <Input placeholder="知识包标题" style={{ width: 150 }} aria-label="知识包标题" />
            </Form.Item>
            <Form.Item name="sourcePath" rules={[{ required: true, message: '请输入源路径' }]}>
              <Input placeholder="源文件路径" style={{ width: 180 }} aria-label="源文件路径" />
            </Form.Item>
            <Form.Item name="rightsBasis" label="权利基础">
              <Select options={RIGHTS_OPTIONS} style={{ width: 120 }} />
            </Form.Item>
            <Form.Item name="allowedUses" label="允许用途">
              <Select mode="multiple" options={ALLOWED_USE_OPTIONS} style={{ minWidth: 180 }} />
            </Form.Item>
            <Form.Item name="retention" label="保留">
              <Select options={RETENTION_OPTIONS} style={{ width: 150 }} />
            </Form.Item>
            <Form.Item>
              <Button type="primary" htmlType="submit" icon={<ImportOutlined />} loading={busy}>
                导入
              </Button>
            </Form.Item>
          </Form>
          <Alert
            type="info"
            showIcon
            style={{ marginBottom: 12 }}
            message="本地保存 ≠ 不发送给模型：勾选「发送给远程模型」的知识包才会随对话上传；未勾选时组装器只走本地路径。"
          />
          <Table<KnowledgePack>
            size="small"
            rowKey="id"
            columns={packColumns}
            dataSource={packs}
            pagination={false}
            locale={{ emptyText: <Empty description="暂无知识包" /> }}
          />
        </div>
      ),
    },
    {
      key: 'search',
      label: '检索预览',
      children: (
        <div>
          <Space direction="vertical" style={{ width: '100%' }} size={12}>
            <Select
              mode="multiple"
              placeholder="选择要检索的知识包"
              style={{ width: '100%' }}
              value={searchPackIds}
              onChange={setSearchPackIds}
              options={packs.map((p) => ({ value: p.id, label: p.title }))}
              aria-label="选择检索知识包"
            />
            <Space.Compact style={{ width: '100%' }}>
              <Input
                placeholder="输入场景查询，例如：如何应对背叛"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onPressEnter={handleSearch}
                aria-label="检索查询"
              />
              <Button type="primary" icon={<SearchOutlined />} loading={busy} onClick={handleSearch}>
                检索
              </Button>
            </Space.Compact>
          </Space>
          <List
            style={{ marginTop: 16 }}
            dataSource={fragments}
            locale={{ emptyText: <Empty description="暂无检索结果" /> }}
            renderItem={(frag) => (
              <List.Item>
                <List.Item.Meta
                  title={
                    <Space>
                      <Text strong>{frag.packTitle}</Text>
                      <Tag>#{frag.ordinal}</Tag>
                      <Text type="secondary">相关度 {frag.score.toFixed(2)}</Text>
                    </Space>
                  }
                  description={<Paragraph style={{ marginBottom: 0 }}>{frag.text}</Paragraph>}
                />
              </List.Item>
            )}
          />
        </div>
      ),
    },
    {
      key: 'bindings',
      label: '角色绑定',
      children: (
        <div>
          <Form
            layout="inline"
            onFinish={handleAddBinding}
            initialValues={{ influence: 0.5, conflictPolicy: 'character_core_wins', enabled: true }}
            style={{ marginBottom: 16, rowGap: 8, flexWrap: 'wrap', alignItems: 'center' }}
          >
            <Form.Item name="packId" rules={[{ required: true, message: '选择知识包' }]}>
              <Select
                placeholder="知识包"
                style={{ width: 160 }}
                options={packs.map((p) => ({ value: p.id, label: p.title }))}
                aria-label="绑定知识包"
              />
            </Form.Item>
            <Form.Item name="characterId" rules={[{ required: true, message: '选择角色' }]}>
              <Select placeholder="角色" style={{ width: 150 }} options={characterOptions} aria-label="绑定角色" />
            </Form.Item>
            <Form.Item name="influence" label="影响强度" style={{ minWidth: 200 }}>
              <Slider min={0} max={1} step={0.05} style={{ width: 120 }} />
            </Form.Item>
            <Form.Item name="conflictPolicy" label="冲突">
              <Select options={CONFLICT_OPTIONS} style={{ width: 150 }} />
            </Form.Item>
            <Form.Item name="enabled" label="启用" valuePropName="checked">
              <Switch />
            </Form.Item>
            <Form.Item>
              <Button type="primary" htmlType="submit" icon={<ExperimentOutlined />}>
                绑定
              </Button>
            </Form.Item>
          </Form>
          <List
            dataSource={bindings}
            locale={{ emptyText: <Empty description="暂无绑定" /> }}
            renderItem={(binding) => {
              const pack = packs.find((p) => p.id === binding.packId);
              return (
                <List.Item
                  actions={[
                    <Switch
                      key="toggle"
                      size="small"
                      checked={binding.enabled}
                      onChange={(v) => toggleBinding(binding, v)}
                      checkedChildren="启"
                      unCheckedChildren="停"
                      aria-label={`启停 ${binding.id}`}
                    />,
                    <Popconfirm
                      key="del"
                      title="解除绑定？"
                      onConfirm={() => removeBinding(binding.id)}
                    >
                      <Button size="small" type="text" danger icon={<DeleteOutlined />} aria-label={`解绑 ${binding.id}`} />
                    </Popconfirm>,
                  ]}
                >
                  <List.Item.Meta
                    title={
                      <Space>
                        <Text strong>{pack?.title ?? binding.packId}</Text>
                        <Text type="secondary">→ {characterName(binding.characterId)}</Text>
                      </Space>
                    }
                    description={
                      <Space size={16}>
                        <Text type="secondary">影响强度 {binding.influence.toFixed(2)}</Text>
                        <Text type="secondary">
                          {CONFLICT_OPTIONS.find((c) => c.value === binding.conflictPolicy)?.label}
                        </Text>
                        <Tag color={binding.enabled ? 'green' : 'default'}>
                          {binding.enabled ? '启用中' : '已停用'}
                        </Tag>
                      </Space>
                    }
                  />
                </List.Item>
              );
            }}
          />
          <Alert
            type="info"
            showIcon
            style={{ marginTop: 12 }}
            message="启停对照：切换某个绑定的启用状态后，重新生成同一场景即可并排对比角色表现差异。"
          />
        </div>
      ),
    },
  ];

  return (
    <Modal open={open} onCancel={onClose} title="知识包管理" width={900} footer={null}>
      {lastError && (
        <Alert type="error" showIcon message={lastError} style={{ marginBottom: 12 }} />
      )}
      <Tabs items={tabs} />
    </Modal>
  );
};

export default KnowledgePackManager;
