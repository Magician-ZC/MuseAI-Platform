// 渐进式捏人向导（总规格 docs/build/spec-world-ecosystem.md §7【拍板 21】）：
// 三句话开卡（名字 + 执念 + 底线）→ AI 展开 18 字段草稿（3 句原话 + 15 项 AI 猜测，逐项标注可改）
// → 深层字段渐进披露（默认折叠，展开后即完整 CharacterCardV2Editor）。
//
// 落点是本地轨：调用用户自己的模型 Key（useSettingsStore.models），产物落本地 partner store，
// 全程不依赖平台账号、不做联网校验。平台的角色卡是从本地成品卡发布上云的，捏人发生在这里。
//
// 【诚实划界】§7 描述的「深层字段随剧情触发式解锁」（"你的角色刚才在秘境门口犹豫了——
// 想调他的决策模型吗？"）需要世界运行时把剧情事件回流到客户端。该触发源尚未接入，
// 本文件只实现前端的渐进披露骨架：深层字段默认折叠 + 手动解锁，UI 文案如实说明这一点。
import React from 'react';
import {
  Modal,
  Steps,
  Input,
  Button,
  Space,
  Typography,
  Tag,
  Alert,
  Collapse,
  Divider,
  message,
} from 'antd';
import { ThunderboltOutlined, ReloadOutlined, SaveOutlined } from '@ant-design/icons';
import { appInvoke } from '../utils/runtime';
import { useSettingsStore } from '../stores/useSettingsStore';
import { usePartnerStore } from '../stores/usePartnerStore';
import CharacterCardV2Editor from './CharacterCardV2Editor';
import { createEmptyCardV2, type CharacterCardV2, type DecisionRule } from '../utils/characterCardV2';

const { Text, Paragraph } = Typography;
const { TextArea } = Input;

// ---------- 命令契约（与 src-tauri/src/agent/sessions.rs::generate_quick_character_draft 对齐）----------

export const QUICK_CARD_COMMAND = 'generate_quick_character_draft' as const;

/** AI 展开出的 15 个字段（另 3 个字段是用户原话，不经模型）。 */
export interface QuickCardDraftFields {
  narrativeRole: string;
  coreContradiction: string;
  surfaceGoal: string;
  hiddenNeed: string;
  coreFear: string;
  stakes: string;
  valuePriorities: string[];
  riskAppetite: string;
  decisionRules: DecisionRule[];
  attributionStyle: string;
  triggers: string[];
  trustBuilding: string;
  sentenceRhythm: string;
  plotSeeds: string[];
  immutableCore: string[];
}

export interface QuickCardDraftResponse {
  fields: QuickCardDraftFields;
  variationSeed: string;
  variationAxes: string[];
  sourceFingerprint: string;
  missingFields: string[];
}

export interface QuickCardDraftRequestDto {
  modelInterface: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  name: string;
  obsession: string;
  bottomLine: string;
  variationSeed?: string;
  temperature?: number;
  maxOutputTokens?: number;
  thinkingDepth?: string;
  systemPrompt?: string;
  taskId: string;
}

// 桌面专用命令：入口挂在「背景设定」页（手机端没有该页，MobileShell 只有聊天/羁绊/故事/首页），
// 因此不进 mobile_server 路由与 appInvoke 的 HTTP 分支；沿用 CharacterCardV2Editor 的模块增强写法。
declare module '../utils/runtime' {
  interface AppInvokeCommands {
    generate_quick_character_draft: {
      args: { request: QuickCardDraftRequestDto };
      result: QuickCardDraftResponse;
    };
  }
}

// ---------- 灵魂三句 ----------

export interface QuickCardSoul {
  name: string;
  obsession: string;
  bottomLine: string;
}

/**
 * 灵魂三句在卡上的落点（§7「卡是灵魂，境界是世界发的戏服」）。
 * 这三条是用户原话，永远不标「AI 猜的」。
 */
export const SOUL_FIELD_SPECS: Array<{
  key: keyof QuickCardSoul;
  label: string;
  path: string;
  hint: string;
}> = [
  { key: 'name', label: '名字', path: 'identity.name', hint: '叫什么。不必解释，一个名字就够。' },
  {
    key: 'obsession',
    label: '执念',
    path: 'agency.longTermAgenda',
    hint: '他放不下的那件事——一句话，不写背景不写来龙去脉。',
  },
  {
    key: 'bottomLine',
    label: '底线',
    path: 'dramaticCore.bottomLines[0]',
    hint: '他绝不肯做的事。这条会成为仲裁硬约束的种子。',
  },
];

// ---------- AI 展开的 15 项：浅层 8 项即时可见，深层 7 项渐进披露 ----------

type FieldKind = 'text' | 'list' | 'rules';

export interface AiFieldSpec {
  /** 卡上的字段路径，同时作为「AI 猜的」标注的唯一 key */
  path: string;
  label: string;
  kind: FieldKind;
  /** shallow：开卡即见；deep：随剧情解锁（触发源未接入，可手动展开） */
  depth: 'shallow' | 'deep';
}

export const AI_FIELD_SPECS: AiFieldSpec[] = [
  // —— 浅层：开卡即见，决定「这个人是谁」——
  { path: 'identity.narrativeRole', label: '叙事角色', kind: 'text', depth: 'shallow' },
  { path: 'dramaticCore.coreContradiction', label: '核心矛盾', kind: 'text', depth: 'shallow' },
  { path: 'dramaticCore.surfaceGoal', label: '表层目标', kind: 'text', depth: 'shallow' },
  { path: 'dramaticCore.hiddenNeed', label: '隐藏需求', kind: 'text', depth: 'shallow' },
  { path: 'dramaticCore.coreFear', label: '核心恐惧', kind: 'text', depth: 'shallow' },
  { path: 'dramaticCore.stakes', label: '赌注', kind: 'text', depth: 'shallow' },
  { path: 'agency.plotSeeds', label: '剧情种子', kind: 'list', depth: 'shallow' },
  { path: 'growthArc.immutableCore', label: '不可变内核', kind: 'list', depth: 'shallow' },
  // —— 深层：§7 举的例子正是「想调他的决策模型吗」，故决策/感知/情绪/关系/表达归为深层 ——
  { path: 'decisionModel.valuePriorities', label: '价值排序', kind: 'list', depth: 'deep' },
  { path: 'decisionModel.riskAppetite', label: '风险偏好', kind: 'text', depth: 'deep' },
  { path: 'decisionModel.decisionRules', label: '决策规则', kind: 'rules', depth: 'deep' },
  { path: 'perception.attributionStyle', label: '归因风格', kind: 'text', depth: 'deep' },
  { path: 'emotionDynamics.triggers', label: '情绪触发点', kind: 'list', depth: 'deep' },
  { path: 'relationGrammar.trustBuilding', label: '如何建立信任', kind: 'text', depth: 'deep' },
  { path: 'expressionFingerprint.sentenceRhythm', label: '句式节奏', kind: 'text', depth: 'deep' },
];

/** 18 字段 = 3 句用户原话 + 15 项 AI 展开。 */
export const QUICK_CARD_FIELD_COUNT = SOUL_FIELD_SPECS.length + AI_FIELD_SPECS.length;

// ---------- 宽容归一化：模型/宿主返回什么形状都不许把界面打崩 ----------

const asString = (value: unknown): string => (typeof value === 'string' ? value.trim() : '');

const asList = (value: unknown): string[] => {
  if (Array.isArray(value)) {
    return value.filter((item): item is string => typeof item === 'string').map((s) => s.trim()).filter(Boolean);
  }
  if (typeof value === 'string') {
    return value
      .split(/[\n、；;]/)
      .map((s) => s.trim())
      .filter(Boolean);
  }
  return [];
};

const asRules = (value: unknown): DecisionRule[] => {
  if (!Array.isArray(value)) return [];
  return value
    .map((item): DecisionRule | null => {
      if (typeof item === 'string' && item.trim()) {
        return { when: '', then: item.trim(), because: '' };
      }
      if (item && typeof item === 'object') {
        const raw = item as Record<string, unknown>;
        const rule = {
          when: asString(raw.when),
          then: asString(raw.then),
          because: asString(raw.because),
        };
        return rule.when || rule.then ? rule : null;
      }
      return null;
    })
    .filter((rule): rule is DecisionRule => rule !== null);
};

/** 把后端（或任意宿主）返回的字段包归一化成确定形状；缺项一律降级为空，不抛错。 */
export function normalizeQuickCardFields(raw: unknown): QuickCardDraftFields {
  const source = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  return {
    narrativeRole: asString(source.narrativeRole),
    coreContradiction: asString(source.coreContradiction),
    surfaceGoal: asString(source.surfaceGoal),
    hiddenNeed: asString(source.hiddenNeed),
    coreFear: asString(source.coreFear),
    stakes: asString(source.stakes),
    valuePriorities: asList(source.valuePriorities),
    riskAppetite: asString(source.riskAppetite),
    decisionRules: asRules(source.decisionRules),
    attributionStyle: asString(source.attributionStyle),
    triggers: asList(source.triggers),
    trustBuilding: asString(source.trustBuilding),
    sentenceRhythm: asString(source.sentenceRhythm),
    plotSeeds: asList(source.plotSeeds),
    immutableCore: asList(source.immutableCore),
  };
}

export function normalizeQuickCardResponse(raw: unknown): QuickCardDraftResponse {
  const source = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  return {
    fields: normalizeQuickCardFields(source.fields),
    variationSeed: asString(source.variationSeed),
    variationAxes: asList(source.variationAxes),
    sourceFingerprint: asString(source.sourceFingerprint),
    missingFields: asList(source.missingFields),
  };
}

// ---------- 草稿装配 ----------

/** 草稿的来源标注：随卡持久化，入库后仍能看出哪些是 AI 猜的。 */
export interface QuickCardProvenance {
  source: 'quickCardWizard';
  sourceFingerprint: string;
  variationSeed: string;
  variationAxes: string[];
  /** 用户原话所在路径（灵魂三句 + 被用户改过的 AI 字段） */
  userWrittenPaths: string[];
  /** 仍是 AI 猜测、未经用户确认的路径 */
  aiGuessedPaths: string[];
  /** 深层字段是否已解锁（触发源未接入时由用户手动解锁） */
  deepUnlocked: boolean;
  /** 诚实标注：剧情触发源尚未接入 */
  triggerSourceWired: false;
}

export interface QuickCardDraft {
  card: CharacterCardV2;
  /** AI 实际写出了内容的路径（= 需要标注「AI 猜的」的字段） */
  aiGuessedPaths: string[];
  /** 模型没能给出的字段路径（如实告诉用户「这几项得你自己写」） */
  emptyPaths: string[];
  response: QuickCardDraftResponse;
}

/** 取某个 AI 字段在字段包里的值（path → fields 的映射写在这里，避免散落各处）。 */
function readAiField(fields: QuickCardDraftFields, path: string): string | string[] | DecisionRule[] {
  switch (path) {
    case 'identity.narrativeRole':
      return fields.narrativeRole;
    case 'dramaticCore.coreContradiction':
      return fields.coreContradiction;
    case 'dramaticCore.surfaceGoal':
      return fields.surfaceGoal;
    case 'dramaticCore.hiddenNeed':
      return fields.hiddenNeed;
    case 'dramaticCore.coreFear':
      return fields.coreFear;
    case 'dramaticCore.stakes':
      return fields.stakes;
    case 'agency.plotSeeds':
      return fields.plotSeeds;
    case 'growthArc.immutableCore':
      return fields.immutableCore;
    case 'decisionModel.valuePriorities':
      return fields.valuePriorities;
    case 'decisionModel.riskAppetite':
      return fields.riskAppetite;
    case 'decisionModel.decisionRules':
      return fields.decisionRules;
    case 'perception.attributionStyle':
      return fields.attributionStyle;
    case 'emotionDynamics.triggers':
      return fields.triggers;
    case 'relationGrammar.trustBuilding':
      return fields.trustBuilding;
    case 'expressionFingerprint.sentenceRhythm':
      return fields.sentenceRhythm;
    default:
      return '';
  }
}

/**
 * 三句话 + AI 字段包 → 一张合法的 CharacterCardV2 草稿（lifecycle 恒为 draft）。
 * 产物可直接喂给 CharacterCardV2Editor。
 */
export function buildQuickCardDraft(soul: QuickCardSoul, response: QuickCardDraftResponse): QuickCardDraft {
  const f = response.fields;
  const card = createEmptyCardV2(soul.name.trim());

  // 灵魂三句（用户原话）
  card.identity.importance = 'core';
  card.agency.longTermAgenda = soul.obsession.trim();
  const bottomLine = soul.bottomLine.trim();
  card.dramaticCore.bottomLines = bottomLine ? [bottomLine] : [];

  // AI 展开的 15 项
  card.identity.narrativeRole = f.narrativeRole;
  card.dramaticCore.coreContradiction = f.coreContradiction;
  card.dramaticCore.surfaceGoal = f.surfaceGoal;
  card.dramaticCore.hiddenNeed = f.hiddenNeed;
  card.dramaticCore.coreFear = f.coreFear;
  card.dramaticCore.stakes = f.stakes;
  card.decisionModel.valuePriorities = f.valuePriorities;
  card.decisionModel.riskAppetite = f.riskAppetite;
  card.decisionModel.decisionRules = f.decisionRules;
  card.perception.attributionStyle = f.attributionStyle;
  card.emotionDynamics.triggers = f.triggers;
  card.relationGrammar.trustBuilding = f.trustBuilding;
  card.expressionFingerprint.sentenceRhythm = f.sentenceRhythm;
  card.agency.plotSeeds = f.plotSeeds;
  card.growthArc.immutableCore = f.immutableCore;

  // 底线同时成为不可变内核的一部分（§7 人设保险：底线 → 仲裁硬约束）
  if (bottomLine && !card.growthArc.immutableCore.includes(bottomLine)) {
    card.growthArc.immutableCore = [bottomLine, ...card.growthArc.immutableCore];
  }

  const aiGuessedPaths: string[] = [];
  const emptyPaths: string[] = [];
  for (const spec of AI_FIELD_SPECS) {
    const value = readAiField(f, spec.path);
    const filled = Array.isArray(value) ? value.length > 0 : value.trim().length > 0;
    (filled ? aiGuessedPaths : emptyPaths).push(spec.path);
  }

  card.lifecycle = 'draft';
  card.identity.legacyV1Fields = {
    // 借 legacyV1Fields 这块无损自由区随卡携带来源标注（serde 侧为 Option<Value>，可原样往返）。
    quickCard: buildProvenance(response, aiGuessedPaths, [], false),
  } as Record<string, unknown>;

  return { card, aiGuessedPaths, emptyPaths, response };
}

function buildProvenance(
  response: QuickCardDraftResponse,
  aiGuessedPaths: string[],
  userEditedPaths: string[],
  deepUnlocked: boolean,
): QuickCardProvenance {
  return {
    source: 'quickCardWizard',
    sourceFingerprint: response.sourceFingerprint,
    variationSeed: response.variationSeed,
    variationAxes: response.variationAxes,
    userWrittenPaths: [...SOUL_FIELD_SPECS.map((s) => s.path), ...userEditedPaths],
    aiGuessedPaths: aiGuessedPaths.filter((p) => !userEditedPaths.includes(p)),
    deepUnlocked,
    triggerSourceWired: false,
  };
}

/** 后端错误既可能是纯文本，也可能是 {message, rawOutput} 的 JSON；一律转成可读中文。 */
export function describeQuickCardError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  try {
    const parsed = JSON.parse(text) as { message?: unknown; rawOutput?: unknown };
    if (parsed && typeof parsed === 'object' && typeof parsed.message === 'string') {
      const raw = typeof parsed.rawOutput === 'string' ? parsed.rawOutput.slice(0, 200) : '';
      return raw ? `${parsed.message}\n模型原始输出：${raw}` : parsed.message;
    }
  } catch {
    // 不是 JSON，按纯文本处理
  }
  return text || '未知错误';
}

// ---------- 组件 ----------

export interface QuickCardWizardProps {
  open: boolean;
  onClose: () => void;
  /** 存入角色卡库后回调（返回落库的卡） */
  onSaved?: (card: CharacterCardV2) => void;
  /** 采样温度：变奏需要自由度，故默认高于「提取型」任务。可被调用方覆盖（规则参数化） */
  temperature?: number;
  maxOutputTokens?: number;
}

/** 变奏需要采样自由度：提取求准，捏人求「同样三句话长出不一样的人」。 */
export const QUICK_CARD_DEFAULT_TEMPERATURE = 0.9;
export const QUICK_CARD_DEFAULT_MAX_OUTPUT_TOKENS = 4096;

const QuickCardWizard: React.FC<QuickCardWizardProps> = ({
  open,
  onClose,
  onSaved,
  temperature = QUICK_CARD_DEFAULT_TEMPERATURE,
  maxOutputTokens = QUICK_CARD_DEFAULT_MAX_OUTPUT_TOKENS,
}) => {
  const models = useSettingsStore((s) => s.models);
  const selectedModelId = useSettingsStore((s) => s.selectedModelId);
  const addV2Card = usePartnerStore((s) => s.addV2Card);

  const [name, setName] = React.useState('');
  const [obsession, setObsession] = React.useState('');
  const [bottomLine, setBottomLine] = React.useState('');
  const [generating, setGenerating] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [draft, setDraft] = React.useState<QuickCardDraft | null>(null);
  const [card, setCard] = React.useState<CharacterCardV2 | null>(null);
  const [userEditedPaths, setUserEditedPaths] = React.useState<string[]>([]);
  const [deepUnlocked, setDeepUnlocked] = React.useState(false);

  const profile = React.useMemo(() => {
    const m = models?.find((x) => x.id === selectedModelId) ?? models?.[0];
    if (!m) return null;
    return { interface: m.modelInterface, baseUrl: m.baseUrl, apiKey: m.apiKey, model: m.model };
  }, [models, selectedModelId]);

  const soul: QuickCardSoul = { name, obsession, bottomLine };
  const soulReady = [name, obsession, bottomLine].every((v) => v.trim().length > 0);

  const generate = async (reroll: boolean) => {
    if (!soulReady) {
      message.warning('三句话都要填：名字、执念、底线。只要这三样，别的等下再说。');
      return;
    }
    if (!profile) {
      message.error('尚未配置模型，请先在「设置」中添加并选择一个模型');
      return;
    }
    setGenerating(true);
    setError(null);
    try {
      const response = normalizeQuickCardResponse(
        await appInvoke(QUICK_CARD_COMMAND, {
          request: {
            modelInterface: profile.interface,
            baseUrl: profile.baseUrl,
            apiKey: profile.apiKey,
            model: profile.model,
            name: name.trim(),
            obsession: obsession.trim(),
            bottomLine: bottomLine.trim(),
            // 换一种变奏 = 换种子重掷；不传则由后端生成新种子
            variationSeed: reroll ? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}` : undefined,
            temperature,
            maxOutputTokens,
            taskId: `quick-card-${Date.now()}`,
          },
        }),
      );
      const next = buildQuickCardDraft(soul, response);
      setDraft(next);
      setCard(next.card);
      setUserEditedPaths([]);
      setDeepUnlocked(false);
    } catch (e) {
      setError(describeQuickCardError(e));
      setDraft(null);
      setCard(null);
    } finally {
      setGenerating(false);
    }
  };

  // 用户一改，这条就不再是「AI 猜的」。
  const markEdited = (path: string) => {
    setUserEditedPaths((prev) => (prev.includes(path) ? prev : [...prev, path]));
  };

  const patchCard = (path: string, mutate: (next: CharacterCardV2) => void) => {
    setCard((prev) => {
      if (!prev) return prev;
      const next: CharacterCardV2 = JSON.parse(JSON.stringify(prev)) as CharacterCardV2;
      mutate(next);
      next.updatedAt = Date.now();
      return next;
    });
    markEdited(path);
  };

  const readCardText = (path: string): string => {
    if (!card) return '';
    switch (path) {
      case 'identity.narrativeRole':
        return card.identity.narrativeRole ?? '';
      case 'dramaticCore.coreContradiction':
        return card.dramaticCore.coreContradiction;
      case 'dramaticCore.surfaceGoal':
        return card.dramaticCore.surfaceGoal;
      case 'dramaticCore.hiddenNeed':
        return card.dramaticCore.hiddenNeed;
      case 'dramaticCore.coreFear':
        return card.dramaticCore.coreFear;
      case 'dramaticCore.stakes':
        return card.dramaticCore.stakes;
      case 'decisionModel.riskAppetite':
        return card.decisionModel.riskAppetite;
      case 'perception.attributionStyle':
        return card.perception.attributionStyle;
      case 'relationGrammar.trustBuilding':
        return card.relationGrammar.trustBuilding;
      case 'expressionFingerprint.sentenceRhythm':
        return card.expressionFingerprint.sentenceRhythm;
      default:
        return '';
    }
  };

  const readCardList = (path: string): string[] => {
    if (!card) return [];
    switch (path) {
      case 'agency.plotSeeds':
        return card.agency.plotSeeds;
      case 'growthArc.immutableCore':
        return card.growthArc.immutableCore;
      case 'decisionModel.valuePriorities':
        return card.decisionModel.valuePriorities;
      case 'emotionDynamics.triggers':
        return card.emotionDynamics.triggers;
      default:
        return [];
    }
  };

  const writeCardText = (path: string, value: string) => {
    patchCard(path, (next) => {
      switch (path) {
        case 'identity.narrativeRole':
          next.identity.narrativeRole = value;
          break;
        case 'dramaticCore.coreContradiction':
          next.dramaticCore.coreContradiction = value;
          break;
        case 'dramaticCore.surfaceGoal':
          next.dramaticCore.surfaceGoal = value;
          break;
        case 'dramaticCore.hiddenNeed':
          next.dramaticCore.hiddenNeed = value;
          break;
        case 'dramaticCore.coreFear':
          next.dramaticCore.coreFear = value;
          break;
        case 'dramaticCore.stakes':
          next.dramaticCore.stakes = value;
          break;
        case 'decisionModel.riskAppetite':
          next.decisionModel.riskAppetite = value;
          break;
        case 'perception.attributionStyle':
          next.perception.attributionStyle = value;
          break;
        case 'relationGrammar.trustBuilding':
          next.relationGrammar.trustBuilding = value;
          break;
        case 'expressionFingerprint.sentenceRhythm':
          next.expressionFingerprint.sentenceRhythm = value;
          break;
        default:
          break;
      }
    });
  };

  const writeCardList = (path: string, value: string[]) => {
    patchCard(path, (next) => {
      switch (path) {
        case 'agency.plotSeeds':
          next.agency.plotSeeds = value;
          break;
        case 'growthArc.immutableCore':
          next.growthArc.immutableCore = value;
          break;
        case 'decisionModel.valuePriorities':
          next.decisionModel.valuePriorities = value;
          break;
        case 'emotionDynamics.triggers':
          next.emotionDynamics.triggers = value;
          break;
        default:
          break;
      }
    });
  };

  const renderAiField = (spec: AiFieldSpec) => {
    const edited = userEditedPaths.includes(spec.path);
    const missing = draft?.emptyPaths.includes(spec.path) ?? false;
    return (
      <div key={spec.path} style={{ marginBottom: 14 }}>
        <Space size={6} style={{ marginBottom: 4 }} wrap>
          <Text style={{ fontSize: 13 }}>{spec.label}</Text>
          {edited ? (
            <Tag color="green">我改过的</Tag>
          ) : missing ? (
            <Tag color="red">模型没给 · 得你自己写</Tag>
          ) : (
            <Tag color="orange">AI 猜的</Tag>
          )}
        </Space>
        {spec.kind === 'text' && (
          <TextArea
            value={readCardText(spec.path)}
            onChange={(e) => writeCardText(spec.path, e.target.value)}
            autoSize={{ minRows: 1, maxRows: 4 }}
            aria-label={spec.label}
          />
        )}
        {spec.kind === 'list' && (
          <TextArea
            value={readCardList(spec.path).join('\n')}
            onChange={(e) =>
              writeCardList(
                spec.path,
                e.target.value
                  .split('\n')
                  .map((s) => s.trim())
                  .filter(Boolean),
              )
            }
            autoSize={{ minRows: 2, maxRows: 5 }}
            aria-label={spec.label}
          />
        )}
        {spec.kind === 'rules' && (
          <div>
            {(card?.decisionModel.decisionRules ?? []).length === 0 ? (
              <Text type="secondary">暂无</Text>
            ) : (
              (card?.decisionModel.decisionRules ?? []).map((rule, i) => (
                <div key={i} style={{ padding: 8, background: '#faf9f5', borderRadius: 6, marginBottom: 6 }}>
                  <Text>
                    当 <Text strong>{rule.when || '—'}</Text> 则 <Text strong>{rule.then}</Text>
                  </Text>
                  {rule.because && (
                    <>
                      <br />
                      <Text type="secondary">因为 {rule.because}</Text>
                    </>
                  )}
                </div>
              ))
            )}
            <Text type="secondary" style={{ fontSize: 12 }}>
              决策规则可在下方「完整十层编辑器」中细调。
            </Text>
          </div>
        )}
      </div>
    );
  };

  const handleSave = () => {
    if (!card || !draft) return;
    const finalCard: CharacterCardV2 = {
      ...card,
      identity: {
        ...card.identity,
        legacyV1Fields: {
          ...(card.identity.legacyV1Fields ?? {}),
          quickCard: buildProvenance(draft.response, draft.aiGuessedPaths, userEditedPaths, deepUnlocked),
        } as Record<string, unknown>,
      },
      updatedAt: Date.now(),
    };
    addV2Card(finalCard);
    message.success(`「${finalCard.identity.name || '未命名'}」已存入角色卡库`);
    onSaved?.(finalCard);
    onClose();
  };

  const shallowSpecs = AI_FIELD_SPECS.filter((s) => s.depth === 'shallow');
  const deepSpecs = AI_FIELD_SPECS.filter((s) => s.depth === 'deep');

  return (
    <Modal
      open={open}
      onCancel={onClose}
      title="三句话捏人"
      width={860}
      footer={null}
      styles={{ body: { paddingTop: 16 } }}
    >
      <Steps
        size="small"
        current={draft ? 1 : 0}
        items={[{ title: '三句话开卡' }, { title: '复核 AI 展开的草稿' }]}
        style={{ marginBottom: 20 }}
      />

      {error && (
        <Alert
          type="error"
          showIcon
          message="展开失败"
          description={<span style={{ whiteSpace: 'pre-wrap' }}>{error}</span>}
          style={{ marginBottom: 16 }}
        />
      )}

      {/* 第一步：只问三件事。多问一项就多抬一分门槛。 */}
      <div>
        <Paragraph type="secondary" style={{ marginBottom: 12 }}>
          只要三句话：名字、执念、底线。剩下 15 项先由 AI 猜，逐条标注、随时可改——
          卡是灵魂，境界是世界发的戏服，这里不写世界设定。
        </Paragraph>
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          {SOUL_FIELD_SPECS.map((spec) => (
            <div key={spec.key}>
              <Space size={6} style={{ marginBottom: 4 }}>
                <Text style={{ fontSize: 13 }}>{spec.label}</Text>
                <Tag color="blue">我写的</Tag>
              </Space>
              {spec.key === 'name' ? (
                <Input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder={spec.hint}
                  aria-label={spec.label}
                />
              ) : (
                <TextArea
                  value={spec.key === 'obsession' ? obsession : bottomLine}
                  onChange={(e) =>
                    spec.key === 'obsession' ? setObsession(e.target.value) : setBottomLine(e.target.value)
                  }
                  placeholder={spec.hint}
                  autoSize={{ minRows: 1, maxRows: 3 }}
                  aria-label={spec.label}
                />
              )}
            </div>
          ))}
          {!profile && <Alert type="warning" showIcon message="未检测到可用模型，请先在设置页配置模型。" />}
          <Space>
            <Button
              type="primary"
              icon={<ThunderboltOutlined />}
              loading={generating}
              onClick={() => generate(false)}
            >
              {draft ? '重新展开' : 'AI 展开草稿'}
            </Button>
            {draft && (
              <Button icon={<ReloadOutlined />} loading={generating} onClick={() => generate(true)}>
                换一种变奏
              </Button>
            )}
          </Space>
        </Space>
      </div>

      {/* 第二步：草稿复核。哪些是你写的、哪些是 AI 猜的，一眼可辨。 */}
      {draft && card && (
        <div style={{ marginTop: 20 }}>
          <Divider titlePlacement="start">AI 展开的草稿</Divider>
          <Alert
            type="info"
            showIcon
            style={{ marginBottom: 12 }}
            message={`这张草稿共 ${QUICK_CARD_FIELD_COUNT} 个字段：3 句是你写的，其余 ${AI_FIELD_SPECS.length} 项是 AI 猜的`}
            description="标着「AI 猜的」的都不是你的角色，只是一个起点——改动任意一条，它就变成「我改过的」。"
          />

          {/* 人格变奏 + 同源指纹（§7：AI 展开注入人格变奏，配合同源指纹防同质） */}
          <div style={{ marginBottom: 12 }}>
            <Text type="secondary" style={{ fontSize: 12, display: 'block', marginBottom: 4 }}>
              本次人格变奏（同样三句话，换个变奏就长出另一个人）
            </Text>
            <Space size={4} wrap>
              {draft.response.variationAxes.map((axis) => (
                <Tag key={axis} color="purple">
                  {axis}
                </Tag>
              ))}
            </Space>
            <Paragraph type="secondary" style={{ fontSize: 12, marginTop: 6, marginBottom: 0 }}>
              同源指纹 {draft.response.sourceFingerprint || '—'}（由三句话算出，随卡记录；
              平台侧「同源卡同世界唯一」的校验尚未接入）
            </Paragraph>
          </div>

          {draft.emptyPaths.length > 0 && (
            <Alert
              type="warning"
              showIcon
              style={{ marginBottom: 12 }}
              message={`有 ${draft.emptyPaths.length} 项模型没给出内容，已标红，需要你自己写`}
            />
          )}

          {shallowSpecs.map(renderAiField)}

          {/* 渐进披露：深层字段默认折叠 */}
          <Collapse
            style={{ marginTop: 12 }}
            onChange={(keys) => setDeepUnlocked((Array.isArray(keys) ? keys : [keys]).includes('deep'))}
            items={[
              {
                key: 'deep',
                label: (
                  <Space wrap>
                    <Text strong>深层字段 · 随剧情解锁</Text>
                    <Tag color="default">{deepSpecs.length} 项</Tag>
                    <Tag color="gold">触发源尚未接入</Tag>
                  </Space>
                ),
                children: (
                  <div>
                    <Alert
                      type="info"
                      showIcon
                      style={{ marginBottom: 12 }}
                      message="这些字段本应由剧情触发解锁"
                      description="按总规格 §7，深层字段应当在剧情节点上被动弹出（例如「你的角色刚才在秘境门口犹豫了——想调他的决策模型吗？」）。该触发需要世界运行时把剧情事件回流到客户端，目前尚未接入，因此这里先提供手动解锁。"
                    />
                    {deepSpecs.map(renderAiField)}
                    <Divider titlePlacement="start">完整十层编辑器</Divider>
                    <CharacterCardV2Editor card={card} evidence={[]} otherCards={[]} onChange={setCard} />
                  </div>
                ),
              },
            ]}
          />

          <div style={{ marginTop: 16, textAlign: 'right' }}>
            <Space>
              <Button onClick={onClose}>取消</Button>
              <Button type="primary" icon={<SaveOutlined />} onClick={handleSave}>
                存入角色卡库
              </Button>
            </Space>
          </div>
        </div>
      )}
    </Modal>
  );
};

export default QuickCardWizard;
