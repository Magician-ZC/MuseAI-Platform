// 世界提取与发布（P3，桌面平台页）：上传小说原文 → 本地引擎逐章提取世界超集 →
// 分维度展示/编辑四条 roster（NPC/反派、地点/秘境、道具、剧情线/结局预览）→ 确认清单 →
// 合成内容超集（WorldSkeletonDraft）→ cloudFetch 发布到 server /assets/worlds（经机审）。
//
// 提取交互复用 ExtractionWizard 的骨架（Steps/STAGE_TO_STEP/roster 表格/useModelProfile/buildRequest），
// 云发布交互复用 CharacterPublish 的骨架（权利声明/发布/mine 列表/status/withdraw）。
// ⚠️ WorldSkeletonDraft 是**只读产物**：一切编辑收敛到 Review 阶段的三条 roster，引擎合成时统一收口，
//    避免深编辑击穿 server 超集校验（悬空引用 / redundancyRatio<3.0 → 400）。
import React from 'react';
import {
  Steps,
  Table,
  Tabs,
  Progress,
  Button,
  Input,
  Select,
  Checkbox,
  Switch,
  Radio,
  Space,
  Typography,
  Tag,
  Alert,
  Empty,
  Card,
  Descriptions,
  Popconfirm,
  message,
} from 'antd';
import {
  GlobalOutlined,
  FolderOpenOutlined,
  ThunderboltOutlined,
  StopOutlined,
  CloudUploadOutlined,
  ReloadOutlined,
} from '@ant-design/icons';
import {
  useWorldExtractionStore,
  type WorldExtractionRequestInput,
  type ModelProfile,
  type RosterEntry,
  type RosterTier,
  type WorldRosterEntry,
  type WorldSkeletonDraft,
  type WorldCoverageReport,
} from '../../stores/useWorldExtractionStore';
import { useSettingsStore } from '../../stores/useSettingsStore';
import { cloudFetch } from '../../utils/cloudApi';
import { describeCloudError, moderationMeta } from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

type Rights = 'original' | 'public_domain_adaptation';
type RoomType = 'idle' | 'chapter' | 'arena';

/** server WorldTemplateView（GET /assets/worlds/mine 与 POST 响应）。 */
interface CloudWorld {
  id: string;
  title: string;
  version: number;
  rightsDeclaration: string;
  moderation: string;
  withdrawn: boolean;
  createdAt: number;
}

const WIZARD_STEPS = ['上传原文', '逐章提取', '清单确认', '合成超集', '发布到平台'];

/** 任务 stage → 向导 step 索引。 */
const STAGE_TO_STEP: Record<string, number> = {
  scan: 1,
  merge: 1,
  tiering: 1,
  review: 2,
  synthesis: 3,
  assembled: 3,
  done: 3,
  cancelled: 0,
};

const TIER_OPTIONS: Array<{ value: RosterTier; label: string }> = [
  { value: 'core', label: '核心角色' },
  { value: 'major', label: '重要配角' },
  { value: 'functional', label: '功能角色' },
  { value: 'extra', label: '过场人物' },
];

const ROOM_TYPE_OPTIONS: Array<{ value: RoomType; label: string; hint: string }> = [
  { value: 'idle', label: '日常放置', hint: '角色在世界中自行生活，产出日报，对内容量要求最低。' },
  { value: 'chapter', label: '章节推进', hint: '按剧情线推进，需要足量主线段与冗余变体。' },
  { value: 'arena', label: '竞技场', hint: '对抗性玩法，侧重反派议程与结局分化。' },
];

const rightsLabel = (r: string): string =>
  r === 'original' ? '原创' : r === 'public_domain_adaptation' ? '公有领域改编' : r;

/** 从当前选中模型构造引擎调用凭据（镜像 ExtractionWizard.useModelProfile）。 */
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

const WorldPublish: React.FC = () => {
  const task = useWorldExtractionStore((s) => s.task);
  const currentTaskId = useWorldExtractionStore((s) => s.currentTaskId);
  const taskEvents = useWorldExtractionStore((s) => s.taskEvents);
  const lastError = useWorldExtractionStore((s) => s.lastError);
  const lastAssembledDraft = useWorldExtractionStore((s) => s.lastAssembledDraft);
  const lastAssembledTaskId = useWorldExtractionStore((s) => s.lastAssembledTaskId);
  const start = useWorldExtractionStore((s) => s.start);
  const getTask = useWorldExtractionStore((s) => s.get);
  const confirmRosters = useWorldExtractionStore((s) => s.confirmRosters);
  const synthesize = useWorldExtractionStore((s) => s.synthesize);
  const cancel = useWorldExtractionStore((s) => s.cancel);
  const getCoverageReport = useWorldExtractionStore((s) => s.getCoverageReport);
  const resetTask = useWorldExtractionStore((s) => s.reset);

  const settings = useSettingsStore();
  const profile = useModelProfile();

  const [workTitle, setWorkTitle] = React.useState('');
  const [sourcePath, setSourcePath] = React.useState('');
  const [charDraft, setCharDraft] = React.useState<RosterEntry[]>([]);
  const [locDraft, setLocDraft] = React.useState<WorldRosterEntry[]>([]);
  const [itemDraft, setItemDraft] = React.useState<WorldRosterEntry[]>([]);
  const [coverage, setCoverage] = React.useState<WorldCoverageReport | null>(null);
  const [busy, setBusy] = React.useState(false);

  // 发布表单
  const [roomType, setRoomType] = React.useState<RoomType>('idle');
  const [rights, setRights] = React.useState<Rights>('original');
  const [agreed, setAgreed] = React.useState(false);
  const [publishing, setPublishing] = React.useState(false);
  const [feedback, setFeedback] = React.useState<{ type: 'success' | 'error'; text: string } | null>(
    null,
  );

  // 我发布的世界
  const [mine, setMine] = React.useState<CloudWorld[]>([]);
  const [mineLoading, setMineLoading] = React.useState(false);
  const [mineError, setMineError] = React.useState<string | null>(null);

  // 集成缝：挂载即订阅 engine-event（Task 进度 + world 合成 Narrative），卸载退订。
  React.useEffect(() => {
    const unsub = useWorldExtractionStore.getState().subscribe();
    return () => unsub();
  }, []);

  // 若已有进行中任务，拉一次快照恢复。
  React.useEffect(() => {
    if (currentTaskId && !task) {
      getTask(currentTaskId).catch(() => undefined);
    }
  }, [currentTaskId, task, getTask]);

  // 任务 roster 变化时同步到可编辑草稿（随 revision 抬升）。
  React.useEffect(() => {
    if (task) {
      setCharDraft((task.characterRoster ?? []).map((e) => ({ ...e })));
      setLocDraft((task.locationRoster ?? []).map((e) => ({ ...e })));
      setItemDraft((task.itemRoster ?? []).map((e) => ({ ...e })));
    }
  }, [task?.taskId, task?.revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const loadMine = React.useCallback(async () => {
    setMineLoading(true);
    setMineError(null);
    try {
      const data = await cloudFetch<CloudWorld[]>('/api/assets/worlds/mine');
      setMine(Array.isArray(data) ? data : []);
    } catch (e) {
      setMineError(describeCloudError(e));
    } finally {
      setMineLoading(false);
    }
  }, []);

  React.useEffect(() => {
    void loadMine();
  }, [loadMine]);

  const buildRequest = React.useCallback((): WorldExtractionRequestInput | null => {
    if (!profile) return null;
    return {
      workTitle: workTitle.trim() || task?.workTitle || '未命名世界',
      sourcePath: sourcePath.trim() || task?.sourcePath || '',
      profile,
      scanPrompt: settings.worldScanPrompt,
      charMergePrompt: settings.worldCharMergePrompt,
      locMergePrompt: settings.worldLocMergePrompt,
      itemMergePrompt: settings.worldItemMergePrompt,
      charTieringPrompt: settings.worldCharTieringPrompt,
      charSynthesisPrompt: settings.worldCharSynthesisPrompt,
      locationSynthesisPrompt: settings.worldLocationSynthesisPrompt,
      itemSynthesisPrompt: settings.worldItemSynthesisPrompt,
      plotSynthesisPrompt: settings.worldPlotSynthesisPrompt,
      endingSynthesisPrompt: settings.worldEndingSynthesisPrompt,
      temperature: settings.agentConfigs?.worldScan?.temperature ?? 0,
      maxOutputTokens: settings.agentConfigs?.worldSynthesis?.maxOutputTokens ?? 8192,
      concurrency: settings.agentConfigs?.backgroundExtraction?.concurrency ?? 3,
    };
  }, [profile, workTitle, sourcePath, task, settings]);

  const stage = task?.stage ?? null;
  const draftReady = !!lastAssembledDraft && lastAssembledTaskId === (task?.taskId ?? currentTaskId);
  const currentStep = draftReady ? 4 : task ? STAGE_TO_STEP[task.stage] ?? 0 : 0;
  const latestEvent = currentTaskId ? taskEvents[currentTaskId] : undefined;

  const scannedCount = task?.chapters.filter((c) => c.status === 'scanned').length ?? 0;
  const totalChapters = task?.chapters.length ?? 0;
  const scanPercent = totalChapters > 0 ? Math.round((scannedCount / totalChapters) * 100) : 0;
  const failedChapters = task?.chapters.filter((c) => c.status === 'failed') ?? [];

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
      message.success('世界提取任务已创建，开始逐章扫描');
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

  const handleCancel = async () => {
    if (!currentTaskId) return;
    try {
      await cancel(currentTaskId);
      message.info('已请求取消提取任务');
    } catch (e) {
      message.error(`取消失败：${String(e)}`);
    }
  };

  const handleReset = () => {
    resetTask();
    setWorkTitle('');
    setSourcePath('');
    setCharDraft([]);
    setLocDraft([]);
    setItemDraft([]);
    setCoverage(null);
    setFeedback(null);
  };

  const updateChar = (key: string, patch: Partial<RosterEntry>) =>
    setCharDraft((prev) => prev.map((e) => (e.key === key ? { ...e, ...patch } : e)));
  const updateLoc = (key: string, patch: Partial<WorldRosterEntry>) =>
    setLocDraft((prev) => prev.map((e) => (e.key === key ? { ...e, ...patch } : e)));
  const updateItem = (key: string, patch: Partial<WorldRosterEntry>) =>
    setItemDraft((prev) => prev.map((e) => (e.key === key ? { ...e, ...patch } : e)));

  const handleConfirm = async () => {
    if (!task) return;
    setBusy(true);
    try {
      await confirmRosters(task.taskId, task.revision, charDraft, locDraft, itemDraft);
      message.success('世界清单已确认，可开始合成超集');
    } catch (e) {
      message.error(`确认清单失败（如提示修订冲突请刷新后重试）：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleSynthesize = async () => {
    if (!task) return;
    const request = buildRequest();
    if (!request) return;
    setBusy(true);
    try {
      await synthesize(task.taskId, request);
      await getTask(task.taskId);
      message.success('已开始合成世界内容超集（长任务，进度见下方，可取消）');
    } catch (e) {
      message.error(`启动合成失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleLoadCoverage = async () => {
    if (!task) return;
    try {
      setCoverage(await getCoverageReport(task.taskId));
    } catch (e) {
      message.error(`获取覆盖报告失败：${String(e)}`);
    }
  };

  const publish = async () => {
    if (!lastAssembledDraft) return;
    setFeedback(null);
    setPublishing(true);
    try {
      const view = await cloudFetch<CloudWorld>('/api/assets/worlds', {
        method: 'POST',
        idempotent: true,
        body: {
          workTitle: workTitle.trim() || task?.workTitle || '未命名世界',
          roomType,
          skeletonJson: lastAssembledDraft,
          rightsDeclaration: rights,
        },
      });
      const m = moderationMeta(view.moderation);
      setFeedback({
        type: 'success',
        text: `已提交发布：${view.title}（第 ${view.version} 版），当前审核态：${m.label}`,
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
        `/api/assets/worlds/${id}/status`,
      );
      setMine((prev) =>
        prev.map((w) => (w.id === id ? { ...w, moderation: s.moderation, withdrawn: s.withdrawn } : w)),
      );
    } catch (e) {
      setMineError(describeCloudError(e));
    }
  };

  const withdraw = async (id: string) => {
    try {
      await cloudFetch(`/api/assets/worlds/${id}/withdraw`, { method: 'POST', idempotent: true });
      await loadMine();
    } catch (e) {
      setMineError(describeCloudError(e));
    }
  };

  // ---------- roster 列定义 ----------

  const charColumns = [
    {
      title: '入库',
      dataIndex: 'userConfirmed',
      width: 56,
      render: (_: unknown, row: RosterEntry) => (
        <Checkbox
          checked={row.userConfirmed}
          disabled={row.tier === 'extra'}
          onChange={(e) => updateChar(row.key, { userConfirmed: e.target.checked })}
          aria-label={`入库 ${row.canonicalName}`}
        />
      ),
    },
    {
      title: 'NPC / 反派',
      dataIndex: 'canonicalName',
      render: (_: unknown, row: RosterEntry) => (
        <Input
          size="small"
          value={row.canonicalName}
          onChange={(e) => updateChar(row.key, { canonicalName: e.target.value })}
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
            updateChar(row.key, {
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
      width: 96,
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

  const locColumns = [
    {
      title: '入库',
      dataIndex: 'userConfirmed',
      width: 56,
      render: (_: unknown, row: WorldRosterEntry) => (
        <Checkbox
          checked={row.userConfirmed}
          onChange={(e) => updateLoc(row.key, { userConfirmed: e.target.checked })}
          aria-label={`入库 ${row.canonicalName}`}
        />
      ),
    },
    {
      title: '地点 / 秘境',
      dataIndex: 'canonicalName',
      render: (_: unknown, row: WorldRosterEntry) => (
        <Input
          size="small"
          value={row.canonicalName}
          onChange={(e) => updateLoc(row.key, { canonicalName: e.target.value })}
          aria-label={`地点名 ${row.key}`}
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
      title: '秘境',
      dataIndex: 'isSecretRealm',
      width: 90,
      render: (_: unknown, row: WorldRosterEntry) => (
        <Switch
          size="small"
          checked={row.isSecretRealm}
          onChange={(checked) => updateLoc(row.key, { isSecretRealm: checked })}
          aria-label={`秘境 ${row.canonicalName}`}
        />
      ),
    },
  ];

  const itemColumns = [
    {
      title: '入库',
      dataIndex: 'userConfirmed',
      width: 56,
      render: (_: unknown, row: WorldRosterEntry) => (
        <Checkbox
          checked={row.userConfirmed}
          onChange={(e) => updateItem(row.key, { userConfirmed: e.target.checked })}
          aria-label={`入库 ${row.canonicalName}`}
        />
      ),
    },
    {
      title: '道具 / 法宝',
      dataIndex: 'canonicalName',
      render: (_: unknown, row: WorldRosterEntry) => (
        <Input
          size="small"
          value={row.canonicalName}
          onChange={(e) => updateItem(row.key, { canonicalName: e.target.value })}
          aria-label={`道具名 ${row.key}`}
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
  ];

  const cardStyle: React.CSSProperties = {
    borderRadius: 12,
    border: 'none',
    boxShadow: '0 1px 3px rgba(0,0,0,0.05)',
    marginBottom: 20,
  };

  const draftSummary = (draft: WorldSkeletonDraft) => {
    const n = (arr?: unknown[]) => (Array.isArray(arr) ? arr.length : 0);
    return (
      <Descriptions size="small" bordered column={2}>
        <Descriptions.Item label="世界固有角色">{n(draft.worldCharacters)}</Descriptions.Item>
        <Descriptions.Item label="地点 / 秘境">{n(draft.locations)}</Descriptions.Item>
        <Descriptions.Item label="道具目录">{n(draft.worldItems)}</Descriptions.Item>
        <Descriptions.Item label="主线段">{n(draft.mainlineNodes)}</Descriptions.Item>
        <Descriptions.Item label="隐藏内容池">{n(draft.hiddenContentPool)}</Descriptions.Item>
        <Descriptions.Item label="结局候选">{n(draft.endingPool)}</Descriptions.Item>
        <Descriptions.Item label="剧情线">{n(draft.storylines)}</Descriptions.Item>
        <Descriptions.Item label="冗余倍率">
          {draft.sampling?.redundancyRatio != null
            ? draft.sampling.redundancyRatio.toFixed(2)
            : '—'}
        </Descriptions.Item>
      </Descriptions>
    );
  };

  return (
    <div style={{ padding: '32px 40px', maxWidth: 1100, margin: '0 auto' }}>
      <div style={{ marginBottom: 24 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <GlobalOutlined style={{ color: '#d97757', marginRight: 10 }} />
          发布世界到云端
        </Title>
        <Text type="secondary">
          从本地小说原文提取世界内容超集（NPC/反派、地点/秘境、道具、剧情线、结局），确认清单后合成并发布到平台世界库（经机审）。
        </Text>
      </div>

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
          message="提取 / 合成出错"
          description={lastError}
          style={{ marginBottom: 16 }}
          closable
        />
      )}

      {/* 步骤一：上传原文 + 启动 */}
      {(!task || stage === 'cancelled') && (
        <Card title="上传小说原文" style={cardStyle} styles={{ body: { padding: 20 } }}>
          <Paragraph type="secondary">
            首版支持可读取的 TXT / Markdown 全书文件。系统会逐章扫描世界实体、归并并分层，最后由你确认清单再合成内容超集。
          </Paragraph>
          <Space direction="vertical" style={{ width: '100%' }} size={12}>
            <Input
              addonBefore="世界 / 作品名称"
              placeholder="例如：九霄仙域"
              value={workTitle}
              onChange={(e) => setWorkTitle(e.target.value)}
              aria-label="世界名称"
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
            <Button type="primary" icon={<ThunderboltOutlined />} loading={busy} onClick={handleStart}>
              开始提取
            </Button>
          </Space>
        </Card>
      )}

      {/* 步骤二：逐章提取进度 */}
      {task && (stage === 'scan' || stage === 'merge' || stage === 'tiering') && (
        <Card title="逐章提取进度" style={cardStyle} styles={{ body: { padding: 20 } }}>
          <Descriptions size="small" column={2} style={{ marginBottom: 12 }}>
            <Descriptions.Item label="世界">{task.workTitle}</Descriptions.Item>
            <Descriptions.Item label="任务修订">r{task.revision}</Descriptions.Item>
            <Descriptions.Item label="当前阶段">
              {stage === 'scan' ? '逐章扫描' : stage === 'merge' ? '实体归并' : '重要度分层'}
            </Descriptions.Item>
            <Descriptions.Item label="已扫描">
              {scannedCount} / {totalChapters} 章
            </Descriptions.Item>
          </Descriptions>
          <Progress percent={scanPercent} status="active" />
          {latestEvent && (
            <Text type="secondary" style={{ display: 'block', marginTop: 8 }}>
              最新事件：{latestEvent.stage} · 进度 {Math.round(latestEvent.progress * 100)}%
              {latestEvent.error ? ` · 错误：${latestEvent.error.message}` : ''}
            </Text>
          )}
          {failedChapters.length > 0 && (
            <Alert
              type="warning"
              showIcon
              style={{ marginTop: 12 }}
              message={`有 ${failedChapters.length} 个章节扫描失败`}
              description="点击「刷新进度」触发幂等续跑，失败章会被重试。"
            />
          )}
          <Space style={{ marginTop: 16 }}>
            <Button icon={<ReloadOutlined />} onClick={handleRefresh}>
              刷新进度
            </Button>
            <Button danger icon={<StopOutlined />} onClick={handleCancel}>
              取消任务
            </Button>
          </Space>
        </Card>
      )}

      {/* 步骤三：分维度 roster 确认 */}
      {task && stage === 'review' && (
        <Card title="世界清单确认（分维度）" style={cardStyle} styles={{ body: { padding: 20 } }}>
          <Paragraph type="secondary">
            核对归并结果、调整重要度与秘境标记、勾选要入库的实体。剧情线与结局为自动派生的全书草稿（只读），确认后由合成阶段统一收口为内容超集。
          </Paragraph>
          <Tabs
            items={[
              {
                key: 'chars',
                label: `NPC / 反派 (${charDraft.length})`,
                children: (
                  <Table<RosterEntry>
                    size="small"
                    rowKey="key"
                    columns={charColumns}
                    dataSource={charDraft}
                    pagination={false}
                    locale={{ emptyText: <Empty description="尚无归并后的人物" /> }}
                    scroll={{ y: 300 }}
                  />
                ),
              },
              {
                key: 'locs',
                label: `地点 / 秘境 (${locDraft.length})`,
                children: (
                  <Table<WorldRosterEntry>
                    size="small"
                    rowKey="key"
                    columns={locColumns}
                    dataSource={locDraft}
                    pagination={false}
                    locale={{ emptyText: <Empty description="尚无归并后的地点" /> }}
                    scroll={{ y: 300 }}
                  />
                ),
              },
              {
                key: 'items',
                label: `道具 (${itemDraft.length})`,
                children: (
                  <Table<WorldRosterEntry>
                    size="small"
                    rowKey="key"
                    columns={itemColumns}
                    dataSource={itemDraft}
                    pagination={false}
                    locale={{ emptyText: <Empty description="尚无归并后的道具" /> }}
                    scroll={{ y: 300 }}
                  />
                ),
              },
              {
                key: 'plot',
                label: `剧情线 / 结局 (${(task.plotBeats?.length ?? 0) + (task.endingClues?.length ?? 0)})`,
                children: (
                  <div>
                    <Alert
                      type="info"
                      showIcon
                      style={{ marginBottom: 12 }}
                      message="剧情线与结局为 Review 阶段自动派生的草稿，用户不确认；确认清单后将合成为剧情线与结局池。"
                    />
                    <Text strong>剧情节拍（{task.plotBeats?.length ?? 0}）</Text>
                    <div style={{ maxHeight: 140, overflowY: 'auto', margin: '8px 0 16px' }}>
                      {(task.plotBeats ?? []).length === 0 ? (
                        <Text type="secondary">—</Text>
                      ) : (
                        (task.plotBeats ?? []).map((b, i) => (
                          <div key={i} style={{ fontSize: 13, marginBottom: 4 }}>
                            <Tag>{`第 ${b.chapterIndex + 1} 章`}</Tag>
                            {b.isHidden && <Tag color="purple">隐藏</Tag>}
                            {b.surface}
                          </div>
                        ))
                      )}
                    </div>
                    <Text strong>结局线索（{task.endingClues?.length ?? 0}）</Text>
                    <div style={{ maxHeight: 140, overflowY: 'auto', marginTop: 8 }}>
                      {(task.endingClues ?? []).length === 0 ? (
                        <Text type="secondary">—</Text>
                      ) : (
                        (task.endingClues ?? []).map((c, i) => (
                          <div key={i} style={{ fontSize: 13, marginBottom: 4 }}>
                            <Tag>{`第 ${c.chapterIndex + 1} 章`}</Tag>
                            {c.affinityHint && <Tag color="blue">{c.affinityHint}</Tag>}
                            {c.surface}
                          </div>
                        ))
                      )}
                    </div>
                  </div>
                ),
              },
            ]}
          />
          <Space style={{ marginTop: 16 }} wrap>
            <Button type="primary" loading={busy} onClick={handleConfirm}>
              确认清单
            </Button>
            <Button icon={<ReloadOutlined />} onClick={handleRefresh}>
              刷新
            </Button>
            <Button
              type="primary"
              ghost
              icon={<ThunderboltOutlined />}
              loading={busy}
              onClick={handleSynthesize}
            >
              确认无误，开始合成超集
            </Button>
            <Text type="secondary">
              已勾选入库：NPC {charDraft.filter((e) => e.userConfirmed).length} · 地点{' '}
              {locDraft.filter((e) => e.userConfirmed).length} · 道具{' '}
              {itemDraft.filter((e) => e.userConfirmed).length}
            </Text>
          </Space>
        </Card>
      )}

      {/* 步骤四：合成超集 */}
      {task && (stage === 'synthesis' || stage === 'assembled') && (
        <Card title="合成世界内容超集" style={cardStyle} styles={{ body: { padding: 20 } }}>
          {stage === 'synthesis' && !draftReady ? (
            <div>
              <Paragraph type="secondary">
                正在为已确认的实体逐个合成（NPC DNA、地点/秘境、道具、剧情线、结局），这是长任务，请耐心等待。完成后会自动带出超集摘要。
              </Paragraph>
              <Progress
                percent={latestEvent ? Math.round(latestEvent.progress * 100) : 0}
                status="active"
              />
              {latestEvent && (
                <Text type="secondary" style={{ display: 'block', marginTop: 8 }}>
                  最新事件：{latestEvent.stage} · {Math.round(latestEvent.progress * 100)}%
                </Text>
              )}
              <Space style={{ marginTop: 16 }}>
                <Button icon={<ReloadOutlined />} onClick={handleRefresh}>
                  刷新进度
                </Button>
                <Button danger icon={<StopOutlined />} onClick={handleCancel}>
                  取消合成
                </Button>
              </Space>
            </div>
          ) : (
            <div>
              <Alert
                type="success"
                showIcon
                message="内容超集合成完成，可发布到平台。"
                style={{ marginBottom: 16 }}
              />
              {lastAssembledDraft && draftSummary(lastAssembledDraft)}
              <Space style={{ marginTop: 16 }}>
                <Button onClick={handleLoadCoverage}>查看覆盖报告</Button>
                {!draftReady && (
                  <Button
                    type="primary"
                    ghost
                    icon={<ThunderboltOutlined />}
                    loading={busy}
                    onClick={handleSynthesize}
                  >
                    重新合成
                  </Button>
                )}
              </Space>
              {coverage && (
                <Descriptions size="small" bordered column={2} style={{ marginTop: 16 }}>
                  <Descriptions.Item label="已扫描章节">
                    {coverage.scannedChapters} / {coverage.totalChapters}
                  </Descriptions.Item>
                  <Descriptions.Item label="失败章节">
                    {coverage.failedChapters.length > 0 ? coverage.failedChapters.join('、') : '无'}
                  </Descriptions.Item>
                  <Descriptions.Item label="人物数">{coverage.characterRosterSize}</Descriptions.Item>
                  <Descriptions.Item label="地点数">{coverage.locationRosterSize}</Descriptions.Item>
                  <Descriptions.Item label="道具数">{coverage.itemRosterSize}</Descriptions.Item>
                </Descriptions>
              )}
            </div>
          )}
        </Card>
      )}

      {/* 步骤五：发布到平台 */}
      {draftReady && lastAssembledDraft && (
        <Card title="发布到平台" style={cardStyle} styles={{ body: { padding: 20 } }}>
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            <div>
              <Text strong>房型</Text>
              <div style={{ marginTop: 8 }}>
                <Select<RoomType>
                  value={roomType}
                  style={{ width: 220 }}
                  onChange={setRoomType}
                  options={ROOM_TYPE_OPTIONS.map((o) => ({ value: o.value, label: o.label }))}
                />
                <Text type="secondary" style={{ marginLeft: 12, fontSize: 12 }}>
                  {ROOM_TYPE_OPTIONS.find((o) => o.value === roomType)?.hint}
                </Text>
              </div>
            </div>

            <div>
              <Text strong>权利基础</Text>
              <div style={{ marginTop: 8 }}>
                <Radio.Group value={rights} onChange={(e) => setRights(e.target.value)}>
                  <Radio value="original">原创</Radio>
                  <Radio value="public_domain_adaptation">公有领域改编</Radio>
                </Radio.Group>
              </div>
            </div>

            <Alert
              type="info"
              showIcon
              message="本次发布上传的是合成产物（世界超集快照）。发布内容按当前超集不可变入库；本地后续重新合成会产生新版本。"
            />

            <Checkbox checked={agreed} onChange={(e) => setAgreed(e.target.checked)}>
              我确认对该世界拥有相应权利，并同意接受平台内容与安全审核。
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

            <Space>
              <Button
                type="primary"
                icon={<CloudUploadOutlined />}
                loading={publishing}
                disabled={!agreed}
                onClick={() => void publish()}
              >
                发布到平台
              </Button>
              <Button onClick={handleReset}>开始新的世界提取</Button>
            </Space>
          </Space>
        </Card>
      )}

      {/* 我发布的世界 */}
      <Card
        title="我发布的世界"
        style={cardStyle}
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
          <Table<CloudWorld>
            dataSource={mine}
            rowKey="id"
            loading={mineLoading}
            pagination={false}
            size="small"
            locale={{ emptyText: '尚无已发布世界' }}
            columns={[
              { title: '标题', dataIndex: 'title', ellipsis: true },
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
                width: 160,
                render: (_: unknown, r: CloudWorld) => (
                  <Space size={4}>
                    <Button size="small" type="link" onClick={() => void refreshStatus(r.id)}>
                      刷新态
                    </Button>
                    {!r.withdrawn && (
                      <Popconfirm title="撤回后停止后续建房投放，确认？" onConfirm={() => void withdraw(r.id)}>
                        <Button size="small" type="link">
                          撤回
                        </Button>
                      </Popconfirm>
                    )}
                  </Space>
                ),
              },
            ]}
          />
        )}
      </Card>

      <Paragraph type="secondary" style={{ fontSize: 12 }}>
        世界超集只描述「可采样的内容池」，建房时由平台按房型采样出内容不同的副本。深度编辑请回到清单确认阶段，合成产物本身视为只读，以保证超集校验通过。
      </Paragraph>
    </div>
  );
};

export default WorldPublish;
