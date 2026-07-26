// 渐进式捏人（总规格 §7【拍板 21】）：三句话开卡 → AI 展开 18 字段草稿 → 深层字段渐进披露。
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import QuickCardWizard, {
  buildQuickCardDraft,
  describeQuickCardError,
  normalizeQuickCardResponse,
  AI_FIELD_SPECS,
  QUICK_CARD_FIELD_COUNT,
  type QuickCardDraftResponse,
} from '../components/QuickCardWizard';
import CharacterCardV2Editor from '../components/CharacterCardV2Editor';
import Background from '../pages/Background';
import { validateCard } from '../utils/characterCardV2';
import { useSettingsStore } from '../stores/useSettingsStore';
import { usePartnerStore } from '../stores/usePartnerStore';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const mockInvoke = invoke as unknown as Mock;

const SOUL = {
  name: '沈砚',
  obsession: '找到那年雪夜里失踪的妹妹',
  bottomLine: '绝不拿孩子做筹码',
};

const makeResponse = (overrides: Partial<QuickCardDraftResponse> = {}): QuickCardDraftResponse => ({
  fields: {
    narrativeRole: '主角',
    coreContradiction: '既想救人又怕再失去',
    surfaceGoal: '查清那年雪夜的真相',
    hiddenNeed: '被允许放下',
    coreFear: '再看见一双空着的鞋',
    stakes: '最后一个亲人',
    valuePriorities: ['亲人', '承诺', '自己'],
    riskAppetite: '涉及孩子时一步都不赌',
    decisionRules: [{ when: '有人拿孩子要挟', then: '宁可暴露自己', because: '底线在此' }],
    attributionStyle: '先假设对方有难处',
    triggers: ['听见雪落的声音'],
    trustBuilding: '看对方怎么对待比自己弱的人',
    sentenceRhythm: '短句，话尾常吞掉',
    plotSeeds: ['身上带着一封没寄出的信', '欠了城南药铺一条命'],
    immutableCore: ['不拿孩子做筹码'],
    ...overrides.fields,
  },
  variationSeed: 'seed-a',
  variationAxes: ['气质底色：外冷内热', '出身位置：从底层一路爬上来'],
  sourceFingerprint: 'qc1-0123456789abcdef',
  missingFields: [],
  ...overrides,
});

/** 填三句话并点「AI 展开草稿」。 */
const fillSoulAndGenerate = () => {
  fireEvent.change(screen.getByLabelText('名字'), { target: { value: SOUL.name } });
  fireEvent.change(screen.getByLabelText('执念'), { target: { value: SOUL.obsession } });
  fireEvent.change(screen.getByLabelText('底线'), { target: { value: SOUL.bottomLine } });
  fireEvent.click(screen.getByRole('button', { name: /AI 展开草稿/ }));
};

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
  useSettingsStore.setState({
    models: [
      {
        id: 'm1',
        name: '测试模型',
        provider: 'OpenAI',
        modelInterface: 'OpenAI-compatible',
        baseUrl: 'https://x/v1',
        apiKey: 'k',
        model: 'gpt-4o',
      },
    ],
    selectedModelId: 'm1',
  });
  usePartnerStore.setState({ characterCardsV2: [] });
});

describe('QuickCardWizard · 三句话开卡', () => {
  it('只填名字/执念/底线三项即可生成——没有第四个必填项', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft' ? makeResponse() : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    // 第一屏只有三个输入框（三句话即全部必填项）
    expect(screen.getByLabelText('名字')).toBeInTheDocument();
    expect(screen.getByLabelText('执念')).toBeInTheDocument();
    expect(screen.getByLabelText('底线')).toBeInTheDocument();

    fillSoulAndGenerate();

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        'generate_quick_character_draft',
        expect.anything(),
      );
    });
    const call = mockInvoke.mock.calls.find(([cmd]) => cmd === 'generate_quick_character_draft');
    expect(call?.[1].request.name).toBe(SOUL.name);
    expect(call?.[1].request.obsession).toBe(SOUL.obsession);
    expect(call?.[1].request.bottomLine).toBe(SOUL.bottomLine);
    expect(call?.[1].request.model).toBe('gpt-4o');
    // 草稿到位
    expect(await screen.findByText('AI 展开的草稿')).toBeInTheDocument();
  });

  it('三句话缺一句就不发请求（门槛只有这三句，但这三句是灵魂）', async () => {
    render(<QuickCardWizard open onClose={() => {}} />);
    fireEvent.change(screen.getByLabelText('名字'), { target: { value: SOUL.name } });
    fireEvent.click(screen.getByRole('button', { name: /AI 展开草稿/ }));

    await waitFor(() => {
      expect(
        mockInvoke.mock.calls.filter(([cmd]) => cmd === 'generate_quick_character_draft'),
      ).toHaveLength(0);
    });
  });

  it('「换一种变奏」重新展开时会换一个变奏种子（同三句话不产出同一个人）', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft' ? makeResponse() : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();
    await screen.findByText('AI 展开的草稿');

    fireEvent.click(screen.getByRole('button', { name: /换一种变奏/ }));
    await waitFor(() => {
      const calls = mockInvoke.mock.calls.filter(([cmd]) => cmd === 'generate_quick_character_draft');
      expect(calls).toHaveLength(2);
      // 首次交给后端生成种子；「换一种变奏」显式重掷
      expect(calls[0][1].request.variationSeed).toBeUndefined();
      expect(typeof calls[1][1].request.variationSeed).toBe('string');
      expect(calls[1][1].request.variationSeed.length).toBeGreaterThan(0);
    });
    // 变奏轴要展示出来，用户才知道「这一版偏向什么」
    expect(screen.getAllByText('气质底色：外冷内热').length).toBeGreaterThan(0);
  });
});

describe('QuickCardWizard · AI 猜测标注', () => {
  it('灵魂三句标「我写的」，AI 展开的字段标「AI 猜的」', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft' ? makeResponse() : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();
    await screen.findByText('AI 展开的草稿');

    // 三句话是用户原话
    expect(screen.getAllByText('我写的')).toHaveLength(3);
    // 浅层 AI 字段逐条标注「AI 猜的」
    const shallowCount = AI_FIELD_SPECS.filter((s) => s.depth === 'shallow').length;
    expect(screen.getAllByText('AI 猜的')).toHaveLength(shallowCount);
    // 明确告诉用户 18 个字段里哪些不是他写的
    expect(
      screen.getByText(new RegExp(`这张草稿共 ${QUICK_CARD_FIELD_COUNT} 个字段`)),
    ).toBeInTheDocument();
  });

  it('用户改动某个 AI 字段后，该字段标注变为「我改过的」', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft' ? makeResponse() : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();
    await screen.findByText('AI 展开的草稿');

    expect(screen.queryByText('我改过的')).toBeNull();
    fireEvent.change(screen.getByLabelText('核心恐惧'), { target: { value: '再听见门被从外面锁上' } });

    expect(await screen.findByText('我改过的')).toBeInTheDocument();
  });

  it('模型漏掉的字段标红为「模型没给 · 得你自己写」，不冒充 AI 猜测', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft'
        ? makeResponse({ fields: { ...makeResponse().fields, coreFear: '', plotSeeds: [] } })
        : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();
    await screen.findByText('AI 展开的草稿');

    expect(screen.getAllByText('模型没给 · 得你自己写')).toHaveLength(2);
    expect(screen.getByText(/有 2 项模型没给出内容/)).toBeInTheDocument();
  });

  it('存入角色卡库时把来源标注随卡持久化（含触发源未接入的诚实标记）', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft' ? makeResponse() : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();
    await screen.findByText('AI 展开的草稿');
    fireEvent.click(screen.getByRole('button', { name: /存入角色卡库/ }));

    await waitFor(() => {
      expect(usePartnerStore.getState().characterCardsV2).toHaveLength(1);
    });
    const saved = usePartnerStore.getState().characterCardsV2[0];
    const provenance = (saved.identity.legacyV1Fields as any).quickCard;
    expect(provenance.source).toBe('quickCardWizard');
    expect(provenance.sourceFingerprint).toBe('qc1-0123456789abcdef');
    expect(provenance.aiGuessedPaths).toContain('dramaticCore.coreFear');
    expect(provenance.userWrittenPaths).toContain('agency.longTermAgenda');
    expect(provenance.triggerSourceWired).toBe(false);
  });
});

describe('QuickCardWizard · 非法 JSON 降级', () => {
  it('后端抛出「非合法 JSON」错误时不崩，给出可读中文错误并留在第一步', async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'generate_quick_character_draft') {
        throw JSON.stringify({
          message: '模型没有返回合法 JSON，请重新分析：expected value at line 1',
          rawOutput: '我先来聊聊这个角色吧……',
        });
      }
      return undefined;
    });

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();

    expect(await screen.findByText('展开失败')).toBeInTheDocument();
    expect(screen.getByText(/模型没有返回合法 JSON/)).toBeInTheDocument();
    // 没有半张草稿留在界面上
    expect(screen.queryByText('AI 展开的草稿')).toBeNull();
    // 三句话还在，可以直接重试
    expect(screen.getByLabelText('名字')).toHaveValue(SOUL.name);
  });

  it('后端返回形状怪异（字段包不是对象/列表写成字符串）也不崩，降级成空字段提示', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft'
        ? { fields: 'not-an-object', variationAxes: null, missingFields: undefined }
        : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();

    expect(await screen.findByText('AI 展开的草稿')).toBeInTheDocument();
    expect(
      screen.getByText(new RegExp(`有 ${AI_FIELD_SPECS.length} 项模型没给出内容`)),
    ).toBeInTheDocument();
  });

  it('describeQuickCardError 把 {message,rawOutput} 与纯文本都转成可读中文', () => {
    const structured = describeQuickCardError(
      JSON.stringify({ message: '模型没有返回合法 JSON，请重新分析：x', rawOutput: '原始输出片段' }),
    );
    expect(structured).toContain('模型没有返回合法 JSON');
    expect(structured).toContain('原始输出片段');
    expect(describeQuickCardError(new Error('网络超时'))).toBe('网络超时');
  });
});

describe('QuickCardWizard · 渐进披露', () => {
  it('深层字段默认折叠，展开后才出现（触发源未接入，如实说明）', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft' ? makeResponse() : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();
    await screen.findByText('AI 展开的草稿');

    // 折叠态：深层字段不可见，但入口与「未接入」标记可见
    expect(screen.getByText('深层字段 · 随剧情解锁')).toBeInTheDocument();
    expect(screen.getByText('触发源尚未接入')).toBeInTheDocument();
    expect(screen.queryByText('归因风格')).toBeNull();
    expect(screen.queryByText('风险偏好')).toBeNull();

    fireEvent.click(screen.getByText('深层字段 · 随剧情解锁'));

    await waitFor(() => {
      expect(screen.getAllByText('归因风格').length).toBeGreaterThan(0);
    });
    expect(screen.getAllByText('风险偏好').length).toBeGreaterThan(0);
    expect(screen.getByText('这些字段本应由剧情触发解锁')).toBeInTheDocument();
  });

  it('展开后草稿直接喂进 CharacterCardV2Editor（完整十层）', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'generate_quick_character_draft' ? makeResponse() : undefined,
    );

    render(<QuickCardWizard open onClose={() => {}} />);
    fillSoulAndGenerate();
    await screen.findByText('AI 展开的草稿');
    fireEvent.click(screen.getByText('深层字段 · 随剧情解锁'));

    expect(await screen.findByText('完整十层编辑器')).toBeInTheDocument();
    // 编辑器自身的标志性区块
    expect(screen.getByText('当前生命周期')).toBeInTheDocument();
    expect(screen.getByText('证据溯源')).toBeInTheDocument();
    expect(screen.getAllByText('J · 跨世界适配').length).toBeGreaterThan(0);
  });
});

describe('入口 · 背景设定页', () => {
  it('「三句话捏人」按钮可打开向导（本地入口，不经平台登录）', async () => {
    mockInvoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === 'get_workspace_dir') return `/tmp/MuseAI/${args?.dirType ?? 'articles'}`;
      if (cmd === 'list_dir') return [];
      if (cmd === 'read_file') return '';
      return undefined;
    });

    render(<Background />);
    fireEvent.click(screen.getByRole('button', { name: /三句话捏人/ }));

    expect(await screen.findByText('三句话捏人', { selector: 'div' })).toBeInTheDocument();
    expect(screen.getByLabelText('执念')).toBeInTheDocument();
    expect(screen.getByLabelText('底线')).toBeInTheDocument();
  });
});

describe('buildQuickCardDraft · 草稿装配', () => {
  it('三句话原样落卡，15 项 AI 字段落到对应层，lifecycle 恒为 draft', () => {
    const { card, aiGuessedPaths, emptyPaths } = buildQuickCardDraft(SOUL, makeResponse());

    expect(card.schemaVersion).toBe(2);
    expect(card.lifecycle).toBe('draft');
    expect(card.identity.name).toBe('沈砚');
    expect(card.agency.longTermAgenda).toBe(SOUL.obsession);
    expect(card.dramaticCore.bottomLines).toEqual([SOUL.bottomLine]);
    expect(card.dramaticCore.coreFear).toBe('再看见一双空着的鞋');
    expect(card.decisionModel.decisionRules[0].then).toBe('宁可暴露自己');
    expect(card.perception.attributionStyle).toBe('先假设对方有难处');
    // 底线同时进不可变内核（§7 人设保险：底线 → 仲裁硬约束的种子）
    expect(card.growthArc.immutableCore).toContain(SOUL.bottomLine);
    // 15 项全给出 → 全部标注为 AI 猜的，没有缺项
    expect(aiGuessedPaths).toHaveLength(AI_FIELD_SPECS.length);
    expect(emptyPaths).toHaveLength(0);
  });

  it('产物是合法 CharacterCardV2，可直接渲染 CharacterCardV2Editor 且通过关键字段校验', () => {
    const { card } = buildQuickCardDraft(SOUL, makeResponse());

    // validateCard 的关键行为字段应当齐全（三句话 + AI 展开已覆盖）
    const validation = validateCard(card, []);
    expect(validation.missing).toEqual([]);

    render(<CharacterCardV2Editor card={card} />);
    expect(screen.getByText('A · 基础身份')).toBeInTheDocument();
    expect(screen.getByDisplayValue('沈砚')).toBeInTheDocument();
  });

  it('normalizeQuickCardResponse 容忍缺字段与错类型，不抛错', () => {
    const normalized = normalizeQuickCardResponse({
      fields: { valuePriorities: '亲人、承诺', decisionRules: ['遇事先扛下来'], coreFear: 42 },
    });

    expect(normalized.fields.valuePriorities).toEqual(['亲人', '承诺']);
    expect(normalized.fields.decisionRules[0].then).toBe('遇事先扛下来');
    expect(normalized.fields.coreFear).toBe('');
    expect(normalized.variationAxes).toEqual([]);
    expect(normalized.missingFields).toEqual([]);

    expect(() => normalizeQuickCardResponse(null)).not.toThrow();
    expect(() => normalizeQuickCardResponse('nonsense')).not.toThrow();
  });
});
