// Character DNA V2 编辑器（规格 §9.1）：十层查看/编辑 + 证据溯源展示 + validateCard 提示
// （lifecycle 可达性 / 缺失字段 / 悬空 evidenceIds）+ 互换测试触发（buildSwapTestRequest → run_character_swap_test）。
import React from 'react';
import {
  Collapse,
  Input,
  Select,
  Tag,
  Space,
  Typography,
  Alert,
  Button,
  Descriptions,
  message,
  Divider,
  Empty,
} from 'antd';
import { ExperimentOutlined, SafetyCertificateOutlined } from '@ant-design/icons';
import {
  validateCard,
  type CharacterCardV2,
  type EvidenceRef,
  type Importance,
  type CardLifecycle,
} from '../utils/characterCardV2';
import {
  buildSwapTestRequest,
  isSameCardContent,
  buildIdenticalSwapReport,
  SWAP_TEST_COMMAND,
  type SwapTestReport,
} from '../utils/characterEvaluation';
import { appInvoke } from '../utils/runtime';

const { Text, Paragraph } = Typography;
const { TextArea } = Input;

// 注册互换/压力测试命令签名（与 crates/muse-engine 薄壳对齐；其它命令由各 store 声明）。
declare module '../utils/runtime' {
  interface AppInvokeCommands {
    run_character_swap_test: {
      args: { cardA: CharacterCardV2; cardB: CharacterCardV2; scenario: string };
      result: SwapTestReport;
    };
  }
}

export interface CharacterCardV2EditorProps {
  card: CharacterCardV2;
  onChange?: (card: CharacterCardV2) => void;
  /** 该卡的证据集合（用于 validateCard 与溯源展示）。 */
  evidence?: EvidenceRef[];
  /** 互换测试候选卡（除当前卡外的其它 V2 卡）。 */
  otherCards?: CharacterCardV2[];
}

const IMPORTANCE_OPTIONS: Array<{ value: Importance; label: string }> = [
  { value: 'core', label: '核心' },
  { value: 'major', label: '重要' },
  { value: 'functional', label: '功能' },
];

const LIFECYCLE_LABEL: Record<CardLifecycle, string> = {
  draft: '草稿',
  reviewed: '已复核',
  ready: '就绪',
};

const LIFECYCLE_COLOR: Record<CardLifecycle, string> = {
  draft: 'default',
  reviewed: 'blue',
  ready: 'green',
};

// ---------- 通用字段编辑器 ----------

const TextField: React.FC<{
  label: string;
  value: string;
  onChange: (v: string) => void;
  textarea?: boolean;
}> = ({ label, value, onChange, textarea }) => (
  <div style={{ marginBottom: 12 }}>
    <Text type="secondary" style={{ fontSize: 12, display: 'block', marginBottom: 4 }}>
      {label}
    </Text>
    {textarea ? (
      <TextArea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        autoSize={{ minRows: 2, maxRows: 6 }}
        aria-label={label}
      />
    ) : (
      <Input value={value} onChange={(e) => onChange(e.target.value)} aria-label={label} />
    )}
  </div>
);

/** 字符串数组：一行一项编辑。 */
const ListField: React.FC<{
  label: string;
  value: string[];
  onChange: (v: string[]) => void;
}> = ({ label, value, onChange }) => (
  <div style={{ marginBottom: 12 }}>
    <Text type="secondary" style={{ fontSize: 12, display: 'block', marginBottom: 4 }}>
      {label}（每行一项）
    </Text>
    <TextArea
      value={value.join('\n')}
      onChange={(e) => onChange(e.target.value.split('\n').map((s) => s.trim()).filter(Boolean))}
      autoSize={{ minRows: 2, maxRows: 6 }}
      aria-label={label}
    />
  </div>
);

const CharacterCardV2Editor: React.FC<CharacterCardV2EditorProps> = ({
  card,
  onChange,
  evidence = [],
  otherCards = [],
}) => {
  const [draft, setDraft] = React.useState<CharacterCardV2>(card);
  const [swapTargetId, setSwapTargetId] = React.useState<string | undefined>(undefined);
  const [scenario, setScenario] = React.useState('');
  const [swapReport, setSwapReport] = React.useState<SwapTestReport | null>(null);
  const [testing, setTesting] = React.useState(false);

  React.useEffect(() => {
    setDraft(card);
  }, [card]);

  // 局部更新：合并 → 刷新 updatedAt → 回调。
  const update = (patch: Partial<CharacterCardV2>) => {
    setDraft((prev) => {
      const next = { ...prev, ...patch, updatedAt: Date.now() };
      onChange?.(next);
      return next;
    });
  };

  // 更新某一层的部分字段。
  const updateLayer = <K extends keyof CharacterCardV2>(
    layer: K,
    patch: Partial<CharacterCardV2[K]>,
  ) => {
    update({ [layer]: { ...(draft[layer] as object), ...patch } } as Partial<CharacterCardV2>);
  };

  const validation = React.useMemo(() => validateCard(draft, evidence), [draft, evidence]);

  // decisionRules 中引用的证据 id → 溯源展示。
  const referencedEvidence = React.useMemo(() => {
    const ids = new Set<string>();
    for (const rule of draft.decisionModel.decisionRules) {
      for (const id of rule.evidenceIds ?? []) ids.add(id);
    }
    return evidence.filter((ev) => ids.has(ev.id));
  }, [draft.decisionModel.decisionRules, evidence]);

  const handleSwapTest = async () => {
    const target = otherCards.find((c) => c.id === swapTargetId);
    if (!target) {
      message.warning('请选择用于互换测试的另一张角色卡');
      return;
    }
    // 同卡内容短路：无需调用模型直接给出「可互换」报告（阴性对照）。
    if (isSameCardContent(draft, target)) {
      setSwapReport(buildIdenticalSwapReport(draft, target, scenario));
      return;
    }
    setTesting(true);
    try {
      const request = buildSwapTestRequest(draft, target, scenario);
      const report = await appInvoke(SWAP_TEST_COMMAND, request);
      setSwapReport(report);
    } catch (e) {
      message.error(`互换测试失败：${String(e)}`);
    } finally {
      setTesting(false);
    }
  };

  const layers = [
    {
      key: 'identity',
      label: 'A · 基础身份',
      children: (
        <div>
          <TextField label="姓名" value={draft.identity.name} onChange={(v) => updateLayer('identity', { name: v })} />
          <ListField label="别名" value={draft.identity.aliases} onChange={(v) => updateLayer('identity', { aliases: v })} />
          <TextField
            label="叙事角色"
            value={draft.identity.narrativeRole ?? ''}
            onChange={(v) => updateLayer('identity', { narrativeRole: v })}
          />
          <div style={{ marginBottom: 12 }}>
            <Text type="secondary" style={{ fontSize: 12, display: 'block', marginBottom: 4 }}>
              重要度
            </Text>
            <Select<Importance>
              value={draft.identity.importance}
              options={IMPORTANCE_OPTIONS}
              style={{ width: 160 }}
              onChange={(v) => updateLayer('identity', { importance: v })}
              aria-label="重要度"
            />
          </div>
        </div>
      ),
    },
    {
      key: 'dramaticCore',
      label: 'B · 戏剧内核',
      children: (
        <div>
          <TextField label="核心矛盾" value={draft.dramaticCore.coreContradiction} onChange={(v) => updateLayer('dramaticCore', { coreContradiction: v })} textarea />
          <TextField label="表层目标" value={draft.dramaticCore.surfaceGoal} onChange={(v) => updateLayer('dramaticCore', { surfaceGoal: v })} />
          <TextField label="隐藏需求" value={draft.dramaticCore.hiddenNeed} onChange={(v) => updateLayer('dramaticCore', { hiddenNeed: v })} />
          <TextField label="核心恐惧" value={draft.dramaticCore.coreFear} onChange={(v) => updateLayer('dramaticCore', { coreFear: v })} />
          <TextField label="赌注" value={draft.dramaticCore.stakes} onChange={(v) => updateLayer('dramaticCore', { stakes: v })} />
          <ListField label="底线" value={draft.dramaticCore.bottomLines} onChange={(v) => updateLayer('dramaticCore', { bottomLines: v })} />
        </div>
      ),
    },
    {
      key: 'decisionModel',
      label: 'C · 决策模型',
      children: (
        <div>
          <ListField label="价值排序（从高到低）" value={draft.decisionModel.valuePriorities} onChange={(v) => updateLayer('decisionModel', { valuePriorities: v })} />
          <TextField label="风险偏好" value={draft.decisionModel.riskAppetite} onChange={(v) => updateLayer('decisionModel', { riskAppetite: v })} />
          <ListField label="默认策略" value={draft.decisionModel.defaultStrategies} onChange={(v) => updateLayer('decisionModel', { defaultStrategies: v })} />
          <ListField label="升级路径" value={draft.decisionModel.escalationPath} onChange={(v) => updateLayer('decisionModel', { escalationPath: v })} />
          <ListField label="牺牲顺序" value={draft.decisionModel.sacrificeOrder} onChange={(v) => updateLayer('decisionModel', { sacrificeOrder: v })} />
          <div style={{ marginTop: 8 }}>
            <Text type="secondary" style={{ fontSize: 12 }}>决策规则（当…则…因为…）</Text>
            {draft.decisionModel.decisionRules.length === 0 ? (
              <Text type="secondary" style={{ display: 'block' }}>暂无</Text>
            ) : (
              draft.decisionModel.decisionRules.map((rule, i) => (
                <div key={i} style={{ padding: 8, background: '#faf9f5', borderRadius: 6, marginTop: 6 }}>
                  <Text>当 <Text strong>{rule.when || '—'}</Text> 则 <Text strong>{rule.then}</Text></Text>
                  <br />
                  <Text type="secondary">因为 {rule.because}</Text>
                  {(rule.evidenceIds?.length ?? 0) > 0 && (
                    <div style={{ marginTop: 4 }}>
                      {rule.evidenceIds!.map((id) => (
                        <Tag key={id} color="blue" style={{ fontSize: 11 }}>{id}</Tag>
                      ))}
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      ),
    },
    {
      key: 'perception',
      label: 'D · 感知与认知',
      children: (
        <div>
          <ListField label="最先注意到" value={draft.perception.firstNotices} onChange={(v) => updateLayer('perception', { firstNotices: v })} />
          <ListField label="盲点" value={draft.perception.blindSpots} onChange={(v) => updateLayer('perception', { blindSpots: v })} />
          <TextField label="归因风格" value={draft.perception.attributionStyle} onChange={(v) => updateLayer('perception', { attributionStyle: v })} />
          <ListField label="信任顺序" value={draft.perception.trustOrder} onChange={(v) => updateLayer('perception', { trustOrder: v })} />
        </div>
      ),
    },
    {
      key: 'emotionDynamics',
      label: 'E · 情绪动力',
      children: (
        <div>
          <ListField label="触发点" value={draft.emotionDynamics.triggers} onChange={(v) => updateLayer('emotionDynamics', { triggers: v })} />
          <TextField label="掩饰方式" value={draft.emotionDynamics.maskingStyle} onChange={(v) => updateLayer('emotionDynamics', { maskingStyle: v })} />
          <TextField label="爆发模式" value={draft.emotionDynamics.outburstPattern} onChange={(v) => updateLayer('emotionDynamics', { outburstPattern: v })} />
          <TextField label="恢复条件" value={draft.emotionDynamics.recoveryConditions} onChange={(v) => updateLayer('emotionDynamics', { recoveryConditions: v })} />
        </div>
      ),
    },
    {
      key: 'relationGrammar',
      label: 'F · 关系语法',
      children: (
        <div>
          <TextField label="建立信任" value={draft.relationGrammar.trustBuilding} onChange={(v) => updateLayer('relationGrammar', { trustBuilding: v })} />
          <TextField label="修复信任" value={draft.relationGrammar.trustRepair} onChange={(v) => updateLayer('relationGrammar', { trustRepair: v })} />
          <ListField label="被什么吸引" value={draft.relationGrammar.attractedBy} onChange={(v) => updateLayer('relationGrammar', { attractedBy: v })} />
          <ListField label="被什么激怒" value={draft.relationGrammar.provokedBy} onChange={(v) => updateLayer('relationGrammar', { provokedBy: v })} />
        </div>
      ),
    },
    {
      key: 'expressionFingerprint',
      label: 'G · 表达指纹',
      children: (
        <div>
          <TextField label="句式节奏" value={draft.expressionFingerprint.sentenceRhythm} onChange={(v) => updateLayer('expressionFingerprint', { sentenceRhythm: v })} />
          <ListField label="比喻来源" value={draft.expressionFingerprint.metaphorSources} onChange={(v) => updateLayer('expressionFingerprint', { metaphorSources: v })} />
          <TextField label="言与心的距离" value={draft.expressionFingerprint.sayVsThinkGap} onChange={(v) => updateLayer('expressionFingerprint', { sayVsThinkGap: v })} />
          <ListField label="标志性动作" value={draft.expressionFingerprint.signatureGestures} onChange={(v) => updateLayer('expressionFingerprint', { signatureGestures: v })} />
          <ListField label="禁用表达（通用 AI 腔）" value={draft.expressionFingerprint.forbiddenPhrases} onChange={(v) => updateLayer('expressionFingerprint', { forbiddenPhrases: v })} />
        </div>
      ),
    },
    {
      key: 'agency',
      label: 'H · 行动力与剧情种子',
      children: (
        <div>
          <ListField label="主动触发" value={draft.agency.initiativeTriggers} onChange={(v) => updateLayer('agency', { initiativeTriggers: v })} />
          <ListField label="默认计划" value={draft.agency.defaultPlans} onChange={(v) => updateLayer('agency', { defaultPlans: v })} />
          <TextField label="长期议程" value={draft.agency.longTermAgenda} onChange={(v) => updateLayer('agency', { longTermAgenda: v })} />
          <ListField label="筹码" value={draft.agency.leverage} onChange={(v) => updateLayer('agency', { leverage: v })} />
          <ListField label="剧情种子" value={draft.agency.plotSeeds} onChange={(v) => updateLayer('agency', { plotSeeds: v })} />
          <ListField label="拒绝规则" value={draft.agency.refusalRules} onChange={(v) => updateLayer('agency', { refusalRules: v })} />
        </div>
      ),
    },
    {
      key: 'growthArc',
      label: 'I · 成长弧',
      children: (
        <div>
          <ListField label="不可变内核" value={draft.growthArc.immutableCore} onChange={(v) => updateLayer('growthArc', { immutableCore: v })} />
          <ListField label="可变信念" value={draft.growthArc.mutableBeliefs} onChange={(v) => updateLayer('growthArc', { mutableBeliefs: v })} />
          <ListField label="崩塌点" value={draft.growthArc.breakPoints} onChange={(v) => updateLayer('growthArc', { breakPoints: v })} />
          <ListField label="觉醒点" value={draft.growthArc.awakeningPoints} onChange={(v) => updateLayer('growthArc', { awakeningPoints: v })} />
        </div>
      ),
    },
    {
      key: 'worldAdaptation',
      label: 'J · 跨世界适配',
      children: (
        <div>
          <ListField label="必须保留" value={draft.worldAdaptation.mustPreserve} onChange={(v) => updateLayer('worldAdaptation', { mustPreserve: v })} />
          <ListField label="可本地化" value={draft.worldAdaptation.localizable} onChange={(v) => updateLayer('worldAdaptation', { localizable: v })} />
          <TextField
            label="冲突降级策略"
            value={draft.worldAdaptation.conflictFallback ?? ''}
            onChange={(v) => updateLayer('worldAdaptation', { conflictFallback: v })}
          />
        </div>
      ),
    },
  ];

  return (
    <div>
      {/* 校验状态条 */}
      <Alert
        type={validation.ready ? 'success' : 'warning'}
        showIcon
        icon={<SafetyCertificateOutlined />}
        style={{ marginBottom: 16 }}
        message={
          <Space wrap>
            <span>当前生命周期</span>
            <Tag color={LIFECYCLE_COLOR[draft.lifecycle]}>{LIFECYCLE_LABEL[draft.lifecycle]}</Tag>
            <span>可达</span>
            <Tag color={LIFECYCLE_COLOR[validation.reachableLifecycle]}>
              {LIFECYCLE_LABEL[validation.reachableLifecycle]}
            </Tag>
          </Space>
        }
        description={
          <div>
            {validation.missing.length > 0 && (
              <Paragraph style={{ marginBottom: 4 }}>
                待补充：{validation.missing.join('、')}
              </Paragraph>
            )}
            {validation.danglingEvidenceIds.length > 0 && (
              <Paragraph type="danger" style={{ marginBottom: 0 }}>
                悬空证据引用：{validation.danglingEvidenceIds.join('、')}
              </Paragraph>
            )}
            {validation.ready && <Text type="success">关键字段齐全、证据无悬空，可推进为「就绪」。</Text>}
          </div>
        }
      />

      <Collapse items={layers} defaultActiveKey={['identity', 'dramaticCore', 'decisionModel']} />

      {/* 证据溯源 */}
      <Divider titlePlacement="start" style={{ marginTop: 20 }}>证据溯源</Divider>
      <Descriptions size="small" column={3} style={{ marginBottom: 8 }}>
        <Descriptions.Item label="证据 storeKey">{draft.evidenceIndex.storeKey || '—'}</Descriptions.Item>
        <Descriptions.Item label="内容哈希">{draft.evidenceIndex.contentHash || '—'}</Descriptions.Item>
        <Descriptions.Item label="证据条数">{draft.evidenceIndex.count}</Descriptions.Item>
      </Descriptions>
      {referencedEvidence.length === 0 ? (
        <Empty description="决策规则暂未挂接可解析的证据" image={Empty.PRESENTED_IMAGE_SIMPLE} />
      ) : (
        referencedEvidence.map((ev) => (
          <div key={ev.id} style={{ padding: 8, background: '#faf9f5', borderRadius: 6, marginBottom: 6 }}>
            <Space size={6} wrap>
              <Tag color="blue">{ev.id}</Tag>
              <Tag>第 {ev.chapterIndex} 章</Tag>
              <Tag color={ev.kind === 'inference' ? 'purple' : 'cyan'}>
                {ev.kind === 'inference' ? '模型推断' : '原文事实'}
              </Tag>
              <Tag color={ev.confidence === 'low' ? 'red' : ev.confidence === 'medium' ? 'orange' : 'green'}>
                置信 {ev.confidence}
              </Tag>
              {ev.userConfirmed && <Tag color="green">已确认</Tag>}
            </Space>
            <Paragraph style={{ marginBottom: 0, marginTop: 4 }}>{ev.quotePreview}</Paragraph>
          </div>
        ))
      )}

      {/* 互换测试 */}
      <Divider titlePlacement="start" style={{ marginTop: 20 }}>互换测试</Divider>
      <Paragraph type="secondary">
        把甲的处境交给乙，看两人在同一局势下是否会做出不可替换的选择。选「同一张卡」可验证阴性对照（应判定为可互换）。
      </Paragraph>
      <Space direction="vertical" style={{ width: '100%' }} size={8}>
        <Select
          placeholder="选择另一张角色卡"
          style={{ width: '100%' }}
          value={swapTargetId}
          onChange={setSwapTargetId}
          options={otherCards.map((c) => ({ value: c.id, label: c.identity.name || c.id }))}
          aria-label="互换测试对象"
          notFoundContent={<Empty description="没有其它可选角色卡" image={Empty.PRESENTED_IMAGE_SIMPLE} />}
        />
        <TextArea
          placeholder="描述测试局势，例如：盟友背叛、必须在救人与保守秘密之间抉择"
          value={scenario}
          onChange={(e) => setScenario(e.target.value)}
          autoSize={{ minRows: 2, maxRows: 4 }}
          aria-label="互换测试局势"
        />
        <Button type="primary" icon={<ExperimentOutlined />} loading={testing} onClick={handleSwapTest}>
          运行互换测试
        </Button>
      </Space>

      {swapReport && (
        <div style={{ marginTop: 16 }}>
          <Alert
            type={swapReport.interchangeable ? 'warning' : 'success'}
            showIcon
            message={swapReport.interchangeable ? '两角色可互换（差异不足）' : '两角色不可互换（选择有实质差异）'}
            description={swapReport.summary}
            style={{ marginBottom: 8 }}
          />
          {swapReport.findings.map((f, i) => (
            <div key={i} style={{ padding: 8, background: '#faf9f5', borderRadius: 6, marginBottom: 6 }}>
              <Space wrap>
                <Text strong>{f.dimension}</Text>
                <Tag color={f.distinct ? 'green' : 'default'}>{f.distinct ? '有差异' : '无差异'}</Tag>
              </Space>
              <Paragraph style={{ marginBottom: 0, marginTop: 4 }}>
                甲：{f.aBehavior}　乙：{f.bBehavior}
              </Paragraph>
            </div>
          ))}
        </div>
      )}
    </div>
  );
};

export default CharacterCardV2Editor;
