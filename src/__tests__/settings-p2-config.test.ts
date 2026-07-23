import { describe, it, expect } from 'vitest';
import {
  defaultAgentConfigs,
  defaultCharacterScanPrompt,
  defaultCharacterMergePrompt,
  defaultCharacterTieringPrompt,
  defaultCharacterSynthesisPrompt,
  defaultCharacterSwapTestPrompt,
  defaultCharacterStressTestPrompt,
  defaultKnowledgeDistillMindPrompt,
  defaultKnowledgeDistillValuePrompt,
  defaultKnowledgeDistillExpressionPrompt,
  defaultNarrativeDirectorPrompt,
  defaultNarrativeDecidePrompt,
  defaultNarrativeArbiterPrompt,
  defaultNarrativeWriterPrompt,
  defaultNarrativeCriticPrompt,
  useSettingsStore,
} from '../stores/useSettingsStore';

// agent-D5：P0–P2 新 AI 环节的默认 prompt、采样配置与按环节模型路由。

const extractionConfig = (maxContext: number, maxOutput = 8192) => ({
  temperature: 0,
  maxOutputTokens: maxOutput,
  maxContextTokens: maxContext,
  thinkingDepth: 'off' as const,
});

describe('Settings P2 stage agent configs (agent-D5)', () => {
  it('registers character-pipeline extraction configs with temperature 0', () => {
    expect(defaultAgentConfigs.characterScan).toEqual(extractionConfig(200000));
    expect(defaultAgentConfigs.characterMerge).toEqual(extractionConfig(128000));
    expect(defaultAgentConfigs.characterTiering).toEqual(extractionConfig(128000));
    expect(defaultAgentConfigs.characterSynthesis).toEqual(extractionConfig(200000, 16384));
    expect(defaultAgentConfigs.characterSwapTest).toEqual(extractionConfig(128000));
    expect(defaultAgentConfigs.characterStressTest).toEqual(extractionConfig(128000));
  });

  it('registers knowledge distillation configs with temperature 0', () => {
    expect(defaultAgentConfigs.knowledgeDistillMind).toEqual(extractionConfig(128000));
    expect(defaultAgentConfigs.knowledgeDistillValue).toEqual(extractionConfig(128000));
    expect(defaultAgentConfigs.knowledgeDistillExpression).toEqual(extractionConfig(128000));
  });

  it('registers narrative round configs: writer creative, others deterministic', () => {
    expect(defaultAgentConfigs.narrativeDirector).toEqual(extractionConfig(128000));
    expect(defaultAgentConfigs.narrativeDecide).toEqual(extractionConfig(128000, 4096));
    expect(defaultAgentConfigs.narrativeArbiter).toEqual(extractionConfig(128000, 4096));
    expect(defaultAgentConfigs.narrativeCritic).toEqual(extractionConfig(128000, 4096));
    // 场景写作是唯一的创作环节：temperature ≈ 0.8（与 narrative.rs 默认 temperature_writer 对齐）。
    expect(defaultAgentConfigs.narrativeWriter).toEqual({
      temperature: 0.8,
      maxOutputTokens: 8192,
      maxContextTokens: 128000,
      thinkingDepth: 'off',
    });
    expect(defaultAgentConfigs.narrativeDecide.temperature).toBe(0);
    expect(defaultAgentConfigs.narrativeArbiter.temperature).toBe(0);
  });

  it('exposes the new configs on the live store state', () => {
    const { agentConfigs } = useSettingsStore.getState();
    expect(agentConfigs.characterScan).toEqual(defaultAgentConfigs.characterScan);
    expect(agentConfigs.narrativeWriter).toEqual(defaultAgentConfigs.narrativeWriter);
    expect(agentConfigs.knowledgeDistillMind).toEqual(defaultAgentConfigs.knowledgeDistillMind);
  });
});

describe('AgentConfig optional modelId routing (§12.4)', () => {
  it('leaves modelId unset by default so agents fall back to global selectedModelId', () => {
    Object.values(defaultAgentConfigs).forEach((config) => {
      expect(config.modelId).toBeUndefined();
    });
  });

  it('accepts a per-agent modelId as a pure additive override', () => {
    const store = useSettingsStore.getState();
    store.setAgentConfig('narrativeWriter', { modelId: 'model-abc' });

    const after = useSettingsStore.getState().agentConfigs.narrativeWriter;
    expect(after.modelId).toBe('model-abc');
    // 其它字段不受影响。
    expect(after.temperature).toBe(0.8);
    expect(after.maxContextTokens).toBe(128000);

    // 复位，避免污染其它用例。
    store.setAgentConfig('narrativeWriter', { modelId: undefined });
  });
});

describe('P2 default prompts content and contracts', () => {
  it('character scan demands verbatim quotes and strict per-chapter JSON', () => {
    expect(defaultCharacterScanPrompt).toContain('逐章扫描器');
    expect(defaultCharacterScanPrompt).toContain('逐字');
    expect(defaultCharacterScanPrompt).toContain('chapterIndex');
    expect(defaultCharacterScanPrompt).toContain('mentions');
    expect(defaultCharacterScanPrompt).toContain('quote');
  });

  it('character merge stays conservative about aliases', () => {
    expect(defaultCharacterMergePrompt).toContain('别名归并器');
    expect(defaultCharacterMergePrompt).toContain('canonicalName');
    expect(defaultCharacterMergePrompt).toContain('宁可漏并');
  });

  it('character tiering restricts tiers to the four-level enum', () => {
    expect(defaultCharacterTieringPrompt).toContain('重要度分层');
    expect(defaultCharacterTieringPrompt).toContain('core');
    expect(defaultCharacterTieringPrompt).toContain('major');
    expect(defaultCharacterTieringPrompt).toContain('functional');
    expect(defaultCharacterTieringPrompt).toContain('extra');
    expect(defaultCharacterTieringPrompt).toContain('adjustments');
  });

  it('character synthesis emits the ten-layer DNA and runs contradiction review', () => {
    expect(defaultCharacterSynthesisPrompt).toContain('DNA 合成师');
    expect(defaultCharacterSynthesisPrompt).toContain('行为不可替换性');
    expect(defaultCharacterSynthesisPrompt).toContain('dramaticCore');
    expect(defaultCharacterSynthesisPrompt).toContain('decisionModel');
    expect(defaultCharacterSynthesisPrompt).toContain('expressionFingerprint');
    expect(defaultCharacterSynthesisPrompt).toContain('矛盾审查');
  });

  it('evaluation prompts target swap-rejection and decision-consistency metrics', () => {
    expect(defaultCharacterSwapTestPrompt).toContain('互换测试评审员');
    expect(defaultCharacterSwapTestPrompt).toContain('incompatible');
    expect(defaultCharacterStressTestPrompt).toContain('压力测试评审员');
    expect(defaultCharacterStressTestPrompt).toContain('决策一致性');
  });

  it('knowledge distillation prompts enforce required fields per pack mode', () => {
    expect(defaultKnowledgeDistillMindPrompt).toContain('Mind Pack');
    expect(defaultKnowledgeDistillMindPrompt).toContain('decisionHeuristics');
    expect(defaultKnowledgeDistillValuePrompt).toContain('Value Pack');
    expect(defaultKnowledgeDistillValuePrompt).toContain('principles');
    expect(defaultKnowledgeDistillExpressionPrompt).toContain('Expression Pack');
    expect(defaultKnowledgeDistillExpressionPrompt).toContain('expressionRules');
  });

  it('narrative prompts encode the round protocol and role boundaries', () => {
    expect(defaultNarrativeDirectorPrompt).toContain('导演');
    expect(defaultNarrativeDirectorPrompt).toContain('绝不替角色做决定');
    // 决策器：第一人称结构化提案 JSON。
    expect(defaultNarrativeDecidePrompt).toContain('角色决策器');
    expect(defaultNarrativeDecidePrompt).toContain('第一人称');
    expect(defaultNarrativeDecidePrompt).toContain('提案');
    expect(defaultNarrativeDecidePrompt).toContain('willSpeak');
    expect(defaultNarrativeDecidePrompt).toContain('predictions');
    // 仲裁器：只裁不改写。
    expect(defaultNarrativeArbiterPrompt).toContain('仲裁');
    expect(defaultNarrativeArbiterPrompt).toContain('绝不重写角色意图');
    // 写手：正文而非 JSON。
    expect(defaultNarrativeWriterPrompt).toContain('场景写手');
    // 审校：只建议不改状态。
    expect(defaultNarrativeCriticPrompt).toContain('一致性审校器');
    expect(defaultNarrativeCriticPrompt).toContain('revisionSuggestions');
  });

  it('mirrors every default prompt onto the live store state', () => {
    const state = useSettingsStore.getState();
    expect(state.characterScanPrompt).toBe(defaultCharacterScanPrompt);
    expect(state.characterMergePrompt).toBe(defaultCharacterMergePrompt);
    expect(state.characterTieringPrompt).toBe(defaultCharacterTieringPrompt);
    expect(state.characterSynthesisPrompt).toBe(defaultCharacterSynthesisPrompt);
    expect(state.characterSwapTestPrompt).toBe(defaultCharacterSwapTestPrompt);
    expect(state.characterStressTestPrompt).toBe(defaultCharacterStressTestPrompt);
    expect(state.knowledgeDistillMindPrompt).toBe(defaultKnowledgeDistillMindPrompt);
    expect(state.knowledgeDistillValuePrompt).toBe(defaultKnowledgeDistillValuePrompt);
    expect(state.knowledgeDistillExpressionPrompt).toBe(defaultKnowledgeDistillExpressionPrompt);
    expect(state.narrativeDirectorPrompt).toBe(defaultNarrativeDirectorPrompt);
    expect(state.narrativeDecidePrompt).toBe(defaultNarrativeDecidePrompt);
    expect(state.narrativeArbiterPrompt).toBe(defaultNarrativeArbiterPrompt);
    expect(state.narrativeWriterPrompt).toBe(defaultNarrativeWriterPrompt);
    expect(state.narrativeCriticPrompt).toBe(defaultNarrativeCriticPrompt);
  });
});

describe('P2 prompt set/reset actions', () => {
  const cases: Array<[string, string, string, string]> = [
    ['setCharacterScanPrompt', 'resetCharacterScanPrompt', 'characterScanPrompt', defaultCharacterScanPrompt],
    ['setCharacterMergePrompt', 'resetCharacterMergePrompt', 'characterMergePrompt', defaultCharacterMergePrompt],
    ['setCharacterTieringPrompt', 'resetCharacterTieringPrompt', 'characterTieringPrompt', defaultCharacterTieringPrompt],
    ['setCharacterSynthesisPrompt', 'resetCharacterSynthesisPrompt', 'characterSynthesisPrompt', defaultCharacterSynthesisPrompt],
    ['setCharacterSwapTestPrompt', 'resetCharacterSwapTestPrompt', 'characterSwapTestPrompt', defaultCharacterSwapTestPrompt],
    ['setCharacterStressTestPrompt', 'resetCharacterStressTestPrompt', 'characterStressTestPrompt', defaultCharacterStressTestPrompt],
    ['setKnowledgeDistillMindPrompt', 'resetKnowledgeDistillMindPrompt', 'knowledgeDistillMindPrompt', defaultKnowledgeDistillMindPrompt],
    ['setKnowledgeDistillValuePrompt', 'resetKnowledgeDistillValuePrompt', 'knowledgeDistillValuePrompt', defaultKnowledgeDistillValuePrompt],
    ['setKnowledgeDistillExpressionPrompt', 'resetKnowledgeDistillExpressionPrompt', 'knowledgeDistillExpressionPrompt', defaultKnowledgeDistillExpressionPrompt],
    ['setNarrativeDirectorPrompt', 'resetNarrativeDirectorPrompt', 'narrativeDirectorPrompt', defaultNarrativeDirectorPrompt],
    ['setNarrativeDecidePrompt', 'resetNarrativeDecidePrompt', 'narrativeDecidePrompt', defaultNarrativeDecidePrompt],
    ['setNarrativeArbiterPrompt', 'resetNarrativeArbiterPrompt', 'narrativeArbiterPrompt', defaultNarrativeArbiterPrompt],
    ['setNarrativeWriterPrompt', 'resetNarrativeWriterPrompt', 'narrativeWriterPrompt', defaultNarrativeWriterPrompt],
    ['setNarrativeCriticPrompt', 'resetNarrativeCriticPrompt', 'narrativeCriticPrompt', defaultNarrativeCriticPrompt],
  ];

  it.each(cases)('%s writes and %s restores default', (setter, resetter, key, def) => {
    const store = useSettingsStore.getState() as Record<string, any>;

    store[setter]('自定义内容-测试用');
    expect((useSettingsStore.getState() as Record<string, any>)[key]).toBe('自定义内容-测试用');

    store[resetter]();
    expect((useSettingsStore.getState() as Record<string, any>)[key]).toBe(def);
  });
});

describe('persist migration from an older version', () => {
  const getMigrate = () => {
    const migrate = useSettingsStore.persist.getOptions().migrate;
    if (!migrate) {
      throw new Error('settings store must define a migrate function');
    }
    return migrate;
  };

  it('is bumped to version 21', () => {
    expect(useSettingsStore.persist.getOptions().version).toBe(21);
  });

  it('injects new agent configs and prompts while preserving customized old data', () => {
    const migrate = getMigrate();

    // 模拟一个老用户的持久化快照（version 20），缺少全部新字段，
    // 但改过一个旧 prompt 和一个旧 agentConfig。
    const oldState = {
      llmProvider: 'OpenAI',
      modelInterface: 'OpenAI-compatible',
      llmBaseUrl: 'https://api.openai.com/v1',
      llmApiKey: 'sk-legacy',
      llmModel: 'gpt-4o',
      models: [
        {
          id: 'legacy-model',
          name: '老模型',
          provider: 'OpenAI',
          modelInterface: 'OpenAI-compatible',
          baseUrl: 'https://api.openai.com/v1',
          apiKey: 'sk-legacy',
          model: 'gpt-4o',
        },
      ],
      selectedModelId: 'legacy-model',
      chatArchivePrompt: '老用户改过的归档提示词-保留我',
      articleType: ['男频', '短篇'],
      agentConfigs: {
        writer: { temperature: 0.99, maxOutputTokens: 12345, maxContextTokens: 200000, thinkingDepth: 'high' as const },
      },
    };

    const migrated = migrate(oldState, 20) as any;

    // 1) 新 prompt 字段就位（等于默认值）。
    expect(migrated.characterScanPrompt).toBe(defaultCharacterScanPrompt);
    expect(migrated.characterSynthesisPrompt).toBe(defaultCharacterSynthesisPrompt);
    expect(migrated.knowledgeDistillMindPrompt).toBe(defaultKnowledgeDistillMindPrompt);
    expect(migrated.knowledgeDistillValuePrompt).toBe(defaultKnowledgeDistillValuePrompt);
    expect(migrated.knowledgeDistillExpressionPrompt).toBe(defaultKnowledgeDistillExpressionPrompt);
    expect(migrated.narrativeDirectorPrompt).toBe(defaultNarrativeDirectorPrompt);
    expect(migrated.narrativeDecidePrompt).toBe(defaultNarrativeDecidePrompt);
    expect(migrated.narrativeArbiterPrompt).toBe(defaultNarrativeArbiterPrompt);
    expect(migrated.narrativeWriterPrompt).toBe(defaultNarrativeWriterPrompt);
    expect(migrated.narrativeCriticPrompt).toBe(defaultNarrativeCriticPrompt);

    // 2) 新 agentConfig 字段就位（等于默认值）。
    expect(migrated.agentConfigs.characterScan).toEqual(defaultAgentConfigs.characterScan);
    expect(migrated.agentConfigs.characterSynthesis).toEqual(defaultAgentConfigs.characterSynthesis);
    expect(migrated.agentConfigs.knowledgeDistillMind).toEqual(defaultAgentConfigs.knowledgeDistillMind);
    expect(migrated.agentConfigs.narrativeWriter).toEqual(defaultAgentConfigs.narrativeWriter);
    expect(migrated.agentConfigs.narrativeDecide).toEqual(defaultAgentConfigs.narrativeDecide);

    // 3) 旧数据平滑保留：用户改过的旧 prompt 与旧 agentConfig 不被覆盖。
    expect(migrated.chatArchivePrompt).toBe('老用户改过的归档提示词-保留我');
    expect(migrated.agentConfigs.writer.temperature).toBe(0.99);
    expect(migrated.agentConfigs.writer.maxOutputTokens).toBe(12345);
    expect(migrated.articleType).toEqual(['男频', '短篇']);
    expect(migrated.selectedModelId).toBe('legacy-model');

    // 4) 其它既有 prompt 的默认值也补齐（老快照没有它们）。
    expect(migrated.narrativeCriticPrompt).toContain('一致性审校器');
  });
});
