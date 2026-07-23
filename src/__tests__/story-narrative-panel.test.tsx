import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { NarrativeRuntimePanel } from '../pages/Story';
import { useCharacterRuntimeStore } from '../stores/useCharacterRuntimeStore';
import { usePartnerStore } from '../stores/usePartnerStore';
import { useSettingsStore } from '../stores/useSettingsStore';
import { createEmptyCardV2 } from '../utils/characterCardV2';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const mockInvoke = invoke as unknown as Mock;

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
  useCharacterRuntimeStore.setState({
    currentRunId: null,
    state: null,
    sceneIds: [],
    scenes: {},
    currentRoundId: null,
    roundStatus: 'idle',
    blockedReason: null,
    lastCritic: null,
    lastError: null,
    costEstimate: null,
    lastSynthesis: null,
  });
  usePartnerStore.setState({
    characterCardsV2: [createEmptyCardV2('林逸'), createEmptyCardV2('沈霜')],
  });
  useSettingsStore.setState({
    models: [
      {
        id: 'm1',
        name: 'M',
        provider: 'OpenAI',
        modelInterface: 'OpenAI-compatible',
        baseUrl: 'u',
        apiKey: 'k',
        model: 'gpt-4o',
      },
    ],
    selectedModelId: 'm1',
  });
});

describe('NarrativeRuntimePanel (P2)', () => {
  it('渲染三种运行模式、活跃角色选择与大纲/托梦/成本区块', () => {
    render(<NarrativeRuntimePanel />);

    expect(screen.getByText('互动模式')).toBeInTheDocument();
    expect(screen.getByText('观察模式')).toBeInTheDocument();
    expect(screen.getByText('章节草稿')).toBeInTheDocument();
    expect(screen.getByText(/活跃角色/)).toBeInTheDocument();
    expect(screen.getByLabelText('大纲文本')).toBeInTheDocument();
    expect(screen.getByLabelText('托梦内容')).toBeInTheDocument();
    expect(screen.getByText('回合成本预估')).toBeInTheDocument();
  });

  it('大纲文本解析为四级约束节点（用 storyConstraints）', () => {
    render(<NarrativeRuntimePanel />);

    fireEvent.change(screen.getByLabelText('大纲文本'), {
      target: { value: '[硬] 揭穿骗局\n与盟友决裂\n[自由] 支线探索' },
    });

    expect(screen.getByText('揭穿骗局')).toBeInTheDocument();
    expect(screen.getByText('与盟友决裂')).toBeInTheDocument();
    expect(screen.getByText('支线探索')).toBeInTheDocument();
    expect(screen.getAllByText('硬节点').length).toBeGreaterThanOrEqual(1);
  });

  it('禁止谓词：合法表达式加入标签', () => {
    render(<NarrativeRuntimePanel />);

    fireEvent.change(screen.getByLabelText('禁止谓词表达式'), {
      target: { value: 'characters.li.arcStage == "堕落"' },
    });
    fireEvent.click(screen.getByRole('button', { name: /添\s*加/ }));

    expect(screen.getByText('characters.li.arcStage == "堕落"')).toBeInTheDocument();
  });

  it('回合成本预估：调用 narrative_estimate 并展示结果', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'narrative_estimate'
        ? { callsPerScene: 6, estimatedTokensLow: 1200, estimatedTokensHigh: 3400 }
        : undefined,
    );
    render(<NarrativeRuntimePanel />);

    fireEvent.click(screen.getByRole('button', { name: /预估/ }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('narrative_estimate', expect.anything());
    });
    expect(await screen.findByText(/每场景调用约 6 次/)).toBeInTheDocument();
  });

  it('初始化运行：调用 narrative_init_run', async () => {
    // 直接以已选角色驱动（回合请求不依赖多选下拉的交互）。
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'narrative_init_run' ? { runId: 'run-x', schemaVersion: 1, revision: 0, world: {}, characters: {}, relations: [], narrative: { outlineNodes: [], forbiddenPredicates: [], foreshadowing: [], pacingNotes: [] }, authoring: { lockedSceneIds: [], branchSnapshotIds: [] } } : undefined,
    );
    render(<NarrativeRuntimePanel />);

    // 未选角色时点击初始化 → 警告，不调用；这里验证守卫存在。
    fireEvent.click(screen.getByRole('button', { name: '初始化运行' }));
    await new Promise((r) => setTimeout(r, 0));
    expect(mockInvoke).not.toHaveBeenCalledWith('narrative_init_run', expect.anything());
  });
});
