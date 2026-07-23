// 八阶段全书角色提取向导（规格 §3.3 / §10.2）：
// 文件检查 → 章节扫描 → 角色发现 → 别名合并（确认）→ 重要度分层（勾选入库）→ DNA 生成 → 低置信确认 → 入库。
// 用 useExtractionStore 的动作与 Task 事件；合成走 useCharacterRuntimeStore（synthesisDone 落 partner store）。
// 关键集成缝：组件挂载即激活 useCharacterRuntimeStore.subscribe()（否则合成卡不会自动入库），卸载时退订。
import React from 'react';
import {
  Modal,
  Steps,
  Table,
  Progress,
  Button,
  Input,
  Select,
  Checkbox,
  Space,
  Typography,
  Tag,
  Alert,
  Empty,
  message,
  Descriptions,
} from 'antd';
import {
  FolderOpenOutlined,
  ThunderboltOutlined,
  StopOutlined,
} from '@ant-design/icons';
import {
  useExtractionStore,
  type ExtractionRequestInput,
  type ModelProfile,
  type RosterEntry,
  type RosterTier,
  type CoverageReport,
} from '../stores/useExtractionStore';
import { useCharacterRuntimeStore } from '../stores/useCharacterRuntimeStore';
import { useSettingsStore } from '../stores/useSettingsStore';
import { usePartnerStore } from '../stores/usePartnerStore';
import CharacterCardV2Editor from './CharacterCardV2Editor';
import type { CharacterCardV2 } from '../utils/characterCardV2';

const { Text, Paragraph } = Typography;

export interface ExtractionWizardProps {
  open: boolean;
  onClose: () => void;
}

// 8 个产品阶段（规格 §3.3）
const WIZARD_STEPS = [
  '文件检查',
  '章节扫描',
  '角色发现',
  '别名合并',
  '重要度分层',
  'DNA 生成',
  '低置信确认',
  '入库',
];

// 任务 stage → 向导 step 索引
const STAGE_TO_STEP: Record<string, number> = {
  preprocess: 0,
  scan: 1,
  merge: 3,
  tiering: 4,
  synthesis: 5,
  review: 6,
  done: 7,
  cancelled: 0,
};

const TIER_OPTIONS: Array<{ value: RosterTier; label: string }> = [
  { value: 'core', label: '核心角色' },
  { value: 'major', label: '重要配角' },
  { value: 'functional', label: '功能角色' },
  { value: 'extra', label: '过场人物' },
];

/** 从当前选中模型构造引擎调用凭据。 */
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

const ExtractionWizard: React.FC<ExtractionWizardProps> = ({ open, onClose }) => {
  const task = useExtractionStore((s) => s.task);
  const currentTaskId = useExtractionStore((s) => s.currentTaskId);
  const taskEvents = useExtractionStore((s) => s.taskEvents);
  const lastError = useExtractionStore((s) => s.lastError);
  const start = useExtractionStore((s) => s.start);
  const getTask = useExtractionStore((s) => s.get);
  const confirmRoster = useExtractionStore((s) => s.confirmRoster);
  const synthesize = useExtractionStore((s) => s.synthesize);
  const cancel = useExtractionStore((s) => s.cancel);
  const getCoverageReport = useExtractionStore((s) => s.getCoverageReport);
  const lastSynthesis = useCharacterRuntimeStore((s) => s.lastSynthesis);
  const addV2Card = usePartnerStore((s) => s.addV2Card);

  const settings = useSettingsStore();
  const profile = useModelProfile();

  const [workTitle, setWorkTitle] = React.useState('');
  const [sourcePath, setSourcePath] = React.useState('');
  const [rosterDraft, setRosterDraft] = React.useState<RosterEntry[]>([]);
  const [coverage, setCoverage] = React.useState<CoverageReport | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [editingCard, setEditingCard] = React.useState<CharacterCardV2 | null>(null);

  // 集成缝：挂载即订阅运行时（synthesisDone → partner store）与任务事件；卸载退订。
  React.useEffect(() => {
    const unsubRuntime = useCharacterRuntimeStore.getState().subscribe();
    const unsubTask = useExtractionStore.getState().subscribe();
    return () => {
      unsubRuntime();
      unsubTask();
    };
  }, []);

  // 打开时若已有进行中任务，拉一次快照。
  React.useEffect(() => {
    if (open && currentTaskId && !task) {
      getTask(currentTaskId).catch(() => undefined);
    }
  }, [open, currentTaskId, task, getTask]);

  // 任务 roster 变化时同步到可编辑草稿。
  React.useEffect(() => {
    if (task?.roster) {
      setRosterDraft(task.roster.map((entry) => ({ ...entry })));
    }
  }, [task?.taskId, task?.revision, task?.roster]);

  const buildRequest = React.useCallback((): ExtractionRequestInput | null => {
    if (!profile) return null;
    return {
      workTitle: workTitle.trim() || task?.workTitle || '未命名作品',
      sourcePath: sourcePath.trim() || task?.sourcePath || '',
      profile,
      scanPrompt: settings.characterScanPrompt,
      mergePrompt: settings.characterMergePrompt,
      tieringPrompt: settings.characterTieringPrompt,
      synthesisPrompt: settings.characterSynthesisPrompt,
      temperature: settings.agentConfigs?.characterScan?.temperature ?? 0,
      maxOutputTokens: settings.agentConfigs?.characterSynthesis?.maxOutputTokens ?? 16384,
      concurrency: settings.agentConfigs?.backgroundExtraction?.concurrency ?? 5,
    };
  }, [profile, workTitle, sourcePath, task, settings]);

  const currentStep = task ? STAGE_TO_STEP[task.stage] ?? 0 : 0;
  const latestEvent = currentTaskId ? taskEvents[currentTaskId] : undefined;

  const scannedCount = task?.chapters.filter((c) => c.status === 'scanned').length ?? 0;
  const totalChapters = task?.chapters.length ?? 0;
  const scanPercent = totalChapters > 0 ? Math.round((scannedCount / totalChapters) * 100) : 0;

  const handleStart = async () => {
    if (!profile) {
      message.error('未配置可用模型，请先在设置中添加模型');
      return;
    }
    if (!sourcePath.trim()) {
      message.error('请填写作品源文件路径（TXT / Markdown）');
      return;
    }
    const request = buildRequest();
    if (!request) return;
    setBusy(true);
    try {
      const taskId = await start(request);
      await getTask(taskId);
      message.success('提取任务已创建，开始逐章扫描');
    } catch (e) {
      message.error(`创建提取任务失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleRefresh = async () => {
    if (!currentTaskId) return;
    try {
      await getTask(currentTaskId);
    } catch (e) {
      message.error(`刷新任务失败：${String(e)}`);
    }
  };

  const updateRosterEntry = (key: string, patch: Partial<RosterEntry>) => {
    setRosterDraft((prev) => prev.map((e) => (e.key === key ? { ...e, ...patch } : e)));
  };

  const handleConfirmRoster = async () => {
    if (!task) return;
    setBusy(true);
    try {
      await confirmRoster(task.taskId, task.revision, rosterDraft);
      message.success('角色清单已确认');
    } catch (e) {
      message.error(`确认清单失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleSynthesize = async () => {
    if (!task) return;
    const request = buildRequest();
    if (!request) return;
    // 只合成已勾选入库且非过场的角色。
    const keys = rosterDraft
      .filter((e) => e.userConfirmed && e.tier !== 'extra')
      .map((e) => e.key);
    if (keys.length === 0) {
      message.warning('请先在清单中勾选要入库的角色');
      return;
    }
    setBusy(true);
    try {
      await synthesize(task.taskId, request, keys);
      message.success(`已开始为 ${keys.length} 个角色并发合成 DNA`);
    } catch (e) {
      message.error(`启动 DNA 合成失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleLoadCoverage = async () => {
    if (!task) return;
    try {
      const report = await getCoverageReport(task.taskId);
      setCoverage(report);
    } catch (e) {
      message.error(`获取覆盖报告失败：${String(e)}`);
    }
  };

  const handleCancel = async () => {
    if (!currentTaskId) return;
    try {
      await cancel(currentTaskId);
      message.info('已请求取消提取任务');
    } catch (e) {
      message.error(`取消失败：${String(e)}`);
    }
  };

  const confirmedCount = rosterDraft.filter((e) => e.userConfirmed && e.tier !== 'extra').length;
  const generatedCount = rosterDraft.filter((e) => e.dnaStatus === 'generated').length;

  const rosterColumns = [
    {
      title: '入库',
      dataIndex: 'userConfirmed',
      width: 56,
      render: (_: unknown, row: RosterEntry) => (
        <Checkbox
          checked={row.userConfirmed}
          disabled={row.tier === 'extra'}
          onChange={(e) => updateRosterEntry(row.key, { userConfirmed: e.target.checked })}
          aria-label={`入库 ${row.canonicalName}`}
        />
      ),
    },
    {
      title: '角色名',
      dataIndex: 'canonicalName',
      render: (_: unknown, row: RosterEntry) => (
        <Input
          size="small"
          value={row.canonicalName}
          onChange={(e) => updateRosterEntry(row.key, { canonicalName: e.target.value })}
          aria-label={`角色名 ${row.key}`}
        />
      ),
    },
    {
      title: '别名',
      dataIndex: 'aliases',
      render: (aliases: string[]) =>
        aliases.length > 0 ? (
          <Space size={4} wrap>
            {aliases.map((a) => (
              <Tag key={a} style={{ margin: 0 }}>
                {a}
              </Tag>
            ))}
          </Space>
        ) : (
          <Text type="secondary">—</Text>
        ),
    },
    {
      title: '重要度',
      dataIndex: 'tier',
      width: 130,
      render: (_: unknown, row: RosterEntry) => (
        <Select<RosterTier>
          size="small"
          value={row.tier}
          style={{ width: '100%' }}
          options={TIER_OPTIONS}
          onChange={(value) =>
            updateRosterEntry(row.key, {
              tier: value,
              userConfirmed: value === 'extra' ? false : row.userConfirmed,
            })
          }
          aria-label={`重要度 ${row.canonicalName}`}
        />
      ),
    },
    {
      title: '合成状态',
      dataIndex: 'dnaStatus',
      width: 100,
      render: (dnaStatus: RosterEntry['dnaStatus']) => {
        const map: Record<RosterEntry['dnaStatus'], { color: string; label: string }> = {
          pending: { color: 'default', label: '待生成' },
          generated: { color: 'green', label: '已生成' },
          failed: { color: 'red', label: '失败' },
          skipped: { color: 'default', label: '跳过' },
        };
        const s = map[dnaStatus];
        return <Tag color={s.color}>{s.label}</Tag>;
      },
    },
  ];

  return (
    <Modal
      open={open}
      onCancel={onClose}
      title="全书角色提取向导"
      width={860}
      footer={null}
      styles={{ body: { paddingTop: 16 } }}
    >
      <Steps
        size="small"
        current={currentStep}
        items={WIZARD_STEPS.map((title) => ({ title }))}
        style={{ marginBottom: 24 }}
      />

      {lastError && (
        <Alert
          type="error"
          showIcon
          message="提取出错"
          description={lastError}
          style={{ marginBottom: 16 }}
        />
      )}

      {/* 阶段一：文件检查 + 启动 */}
      {(!task || task.stage === 'preprocess' || task.stage === 'cancelled') && (
        <div>
          <Paragraph type="secondary">
            首版仅支持可读取的 TXT / Markdown 全书文件。系统会扫描全部章节、发现角色并分层，最后由你确认清单再批量生成角色卡。
          </Paragraph>
          <Space direction="vertical" style={{ width: '100%' }} size={12}>
            <Input
              addonBefore="作品名称"
              placeholder="例如：星穹之诗"
              value={workTitle}
              onChange={(e) => setWorkTitle(e.target.value)}
              aria-label="作品名称"
            />
            <Input
              addonBefore="源文件路径"
              placeholder="/path/to/book.txt"
              prefix={<FolderOpenOutlined />}
              value={sourcePath}
              onChange={(e) => setSourcePath(e.target.value)}
              aria-label="源文件路径"
            />
            {!profile && (
              <Alert type="warning" showIcon message="未检测到可用模型，请先在设置页配置模型。" />
            )}
            <Button
              type="primary"
              icon={<ThunderboltOutlined />}
              loading={busy}
              onClick={handleStart}
            >
              开始提取
            </Button>
          </Space>
        </div>
      )}

      {/* 阶段二/三：章节扫描 + 角色发现 */}
      {task && (task.stage === 'scan') && (
        <div>
          <Descriptions size="small" column={2} style={{ marginBottom: 12 }}>
            <Descriptions.Item label="作品">{task.workTitle}</Descriptions.Item>
            <Descriptions.Item label="任务修订">r{task.revision}</Descriptions.Item>
          </Descriptions>
          <Text>
            章节扫描进度：{scannedCount} / {totalChapters} 章
          </Text>
          <Progress percent={scanPercent} status="active" />
          {latestEvent && (
            <Text type="secondary" style={{ display: 'block', marginTop: 8 }}>
              最新事件：{latestEvent.stage} · 进度 {Math.round(latestEvent.progress * 100)}%
            </Text>
          )}
          <Space style={{ marginTop: 16 }}>
            <Button onClick={handleRefresh}>刷新进度</Button>
            <Button danger icon={<StopOutlined />} onClick={handleCancel}>
              取消任务
            </Button>
          </Space>
        </div>
      )}

      {/* 阶段四/五：别名合并 + 重要度分层（用户确认清单） */}
      {task && (task.stage === 'merge' || task.stage === 'tiering') && (
        <div>
          <Paragraph type="secondary">
            请确认归并后的角色清单：核对合并是否正确、调整重要度、勾选要入库生成完整卡的角色（过场人物默认仅入索引，不生成卡）。
          </Paragraph>
          <Table<RosterEntry>
            size="small"
            rowKey="key"
            columns={rosterColumns}
            dataSource={rosterDraft}
            pagination={false}
            locale={{ emptyText: <Empty description="尚无归并后的角色" /> }}
            scroll={{ y: 320 }}
          />
          <Space style={{ marginTop: 16 }}>
            <Button type="primary" loading={busy} onClick={handleConfirmRoster}>
              确认清单
            </Button>
            <Text type="secondary">已勾选入库：{confirmedCount} 个</Text>
          </Space>
        </div>
      )}

      {/* 阶段六：DNA 生成 */}
      {task && task.stage === 'synthesis' && (
        <div>
          <Paragraph type="secondary">
            为已确认的角色并发合成 Character DNA V2。合成完成的角色卡会自动入库（可在背景页查看），失败的角色可单独重试。
          </Paragraph>
          <Table<RosterEntry>
            size="small"
            rowKey="key"
            columns={rosterColumns}
            dataSource={rosterDraft}
            pagination={false}
            scroll={{ y: 280 }}
          />
          <Space style={{ marginTop: 16 }}>
            <Button
              type="primary"
              icon={<ThunderboltOutlined />}
              loading={busy}
              onClick={handleSynthesize}
            >
              开始 / 重试合成
            </Button>
            <Button onClick={handleRefresh}>刷新状态</Button>
            <Text type="secondary">
              已生成 {generatedCount} / {confirmedCount}
            </Text>
          </Space>
        </div>
      )}

      {/* 阶段七/八：低置信确认 + 覆盖报告 + 入库 */}
      {task && (task.stage === 'review' || task.stage === 'done') && (
        <div>
          <Alert
            type="success"
            showIcon
            message={
              task.stage === 'done'
                ? '提取完成，角色卡已入库'
                : '合成完成，请复核低置信字段与覆盖报告后入库'
            }
            style={{ marginBottom: 16 }}
          />
          {lastSynthesis && lastSynthesis.cards.length > 0 && (
            <div style={{ marginBottom: 16 }}>
              <Paragraph>
                本次合成入库角色卡：<Text strong>{lastSynthesis.cards.length}</Text> 张。可逐张复核低置信字段与证据溯源。
              </Paragraph>
              <Space wrap>
                {lastSynthesis.cards.map((card) => (
                  <Button key={card.id} onClick={() => setEditingCard(card)}>
                    复核「{card.identity.name || card.id}」
                  </Button>
                ))}
              </Space>
            </div>
          )}
          <Button onClick={handleLoadCoverage} style={{ marginBottom: 12 }}>
            查看覆盖报告
          </Button>
          {coverage && (
            <Descriptions size="small" bordered column={1}>
              <Descriptions.Item label="已扫描章节">
                {coverage.scannedChapters} / {coverage.totalChapters}
              </Descriptions.Item>
              <Descriptions.Item label="失败章节">
                {coverage.failedChapters.length > 0 ? coverage.failedChapters.join('、') : '无'}
              </Descriptions.Item>
              <Descriptions.Item label="角色数">{coverage.rosterSize}</Descriptions.Item>
              <Descriptions.Item label="未决别名">
                {coverage.unresolvedAliases.length > 0
                  ? coverage.unresolvedAliases.join('、')
                  : '无'}
              </Descriptions.Item>
              <Descriptions.Item label="低置信字段">
                {coverage.lowConfidenceFields.length > 0
                  ? coverage.lowConfidenceFields.join('、')
                  : '无'}
              </Descriptions.Item>
            </Descriptions>
          )}
          <div style={{ marginTop: 16, textAlign: 'right' }}>
            <Button type="primary" onClick={onClose}>
              完成
            </Button>
          </div>
        </div>
      )}

      {/* 低置信确认：逐张复核合成出的 V2 卡（十层 + 证据溯源 + validateCard），编辑即回写 partner store。 */}
      <Modal
        open={!!editingCard}
        onCancel={() => setEditingCard(null)}
        title={editingCard ? `复核角色卡 · ${editingCard.identity.name || editingCard.id}` : ''}
        width={820}
        footer={null}
        destroyOnHidden
      >
        {editingCard && (
          <CharacterCardV2Editor
            card={editingCard}
            evidence={[]}
            otherCards={(lastSynthesis?.cards ?? []).filter((c) => c.id !== editingCard.id)}
            onChange={(updated) => addV2Card(updated)}
          />
        )}
      </Modal>
    </Modal>
  );
};

export default ExtractionWizard;
