import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import ExtractionWizard from '../components/ExtractionWizard';
import { useExtractionStore, type ExtractionTask, type RosterEntry } from '../stores/useExtractionStore';
import { useSettingsStore } from '../stores/useSettingsStore';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const mockInvoke = invoke as unknown as Mock;
const mockListen = listen as unknown as Mock;

const makeTask = (overrides: Partial<ExtractionTask> = {}): ExtractionTask => ({
  schemaVersion: 1,
  taskId: 't1',
  workTitle: '星穹之诗',
  sourcePath: '/books/star.txt',
  sourceFingerprint: { size: 10, modifiedAt: 1, contentHash: 'h' },
  pipelineVersion: 'v1',
  chapters: [
    { id: 'c1', index: 0, title: '第一章', charRange: [0, 100], status: 'scanned', attempt: 1 },
    { id: 'c2', index: 1, title: '第二章', charRange: [100, 200], status: 'pending', attempt: 0 },
  ],
  roster: [],
  stage: 'scan',
  revision: 3,
  createdAt: 0,
  updatedAt: 0,
  ...overrides,
});

const roster = (): RosterEntry[] => [
  {
    key: 'k1',
    canonicalName: '林逸',
    aliases: ['小林'],
    tier: 'core',
    mergedFrom: [],
    userConfirmed: true,
    dnaStatus: 'pending',
  },
];

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
  mockListen.mockReset();
  mockListen.mockImplementation(async () => () => {});
  useExtractionStore.setState({
    currentTaskId: null,
    task: null,
    activeTaskIds: [],
    taskEvents: {},
    lastRevisionByTask: {},
    lastError: null,
  });
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
});

describe('ExtractionWizard', () => {
  it('挂载即订阅 engine-event（集成缝）并渲染八阶段', async () => {
    render(<ExtractionWizard open onClose={() => {}} />);

    expect(screen.getByText('全书角色提取向导')).toBeInTheDocument();
    expect(screen.getByText('文件检查')).toBeInTheDocument();
    expect(screen.getByText('章节扫描')).toBeInTheDocument();
    expect(screen.getByText('别名合并')).toBeInTheDocument();
    expect(screen.getByText('DNA 生成')).toBeInTheDocument();
    expect(screen.getByText('入库')).toBeInTheDocument();

    await waitFor(() => {
      expect(mockListen).toHaveBeenCalledWith('engine-event', expect.any(Function));
    });
  });

  it('填写源路径后开始提取调用 start_character_extraction', async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'start_character_extraction') return { taskId: 't1' };
      if (cmd === 'get_character_extraction_task') return makeTask({ stage: 'scan' });
      return undefined;
    });

    render(<ExtractionWizard open onClose={() => {}} />);
    fireEvent.change(screen.getByLabelText('源文件路径'), { target: { value: '/books/star.txt' } });
    fireEvent.click(screen.getByRole('button', { name: /开始提取/ }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('start_character_extraction', expect.anything());
    });
    const call = mockInvoke.mock.calls.find(([cmd]) => cmd === 'start_character_extraction');
    expect(call?.[1].request.sourcePath).toBe('/books/star.txt');
    expect(call?.[1].request.profile.model).toBe('gpt-4o');
  });

  it('章节扫描阶段展示进度', () => {
    useExtractionStore.setState({ currentTaskId: 't1', task: makeTask({ stage: 'scan' }) });
    render(<ExtractionWizard open onClose={() => {}} />);
    expect(screen.getByText(/章节扫描进度：1 \/ 2 章/)).toBeInTheDocument();
  });

  it('merge 阶段渲染可编辑角色清单并可确认（调 confirmRoster）', async () => {
    const task = makeTask({ stage: 'merge', roster: roster() });
    useExtractionStore.setState({ currentTaskId: 't1', task });
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'confirm_character_roster'
        ? { ...task, stage: 'synthesis', revision: task.revision + 1 }
        : undefined,
    );

    render(<ExtractionWizard open onClose={() => {}} />);
    expect(screen.getByDisplayValue('林逸')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: '确认清单' }));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        'confirm_character_roster',
        expect.objectContaining({ taskId: 't1', expectedRevision: 3 }),
      );
    });
  });

  it('synthesis 阶段勾选入库后开始合成（调 start_character_dna_synthesis，传对应 keys）', async () => {
    const task = makeTask({ stage: 'synthesis', roster: roster() });
    useExtractionStore.setState({ currentTaskId: 't1', task });
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'start_character_dna_synthesis' ? { runId: 'r1' } : undefined,
    );

    render(<ExtractionWizard open onClose={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /开始 \/ 重试合成/ }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        'start_character_dna_synthesis',
        expect.objectContaining({ taskId: 't1', keys: ['k1'] }),
      );
    });
  });

  it('review 阶段可拉取覆盖报告', async () => {
    useExtractionStore.setState({ currentTaskId: 't1', task: makeTask({ stage: 'review' }) });
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'get_extraction_coverage_report'
        ? {
            scannedChapters: 2,
            totalChapters: 2,
            failedChapters: [],
            rosterSize: 3,
            unresolvedAliases: [],
            lowConfidenceFields: ['林逸.coreFear'],
          }
        : undefined,
    );

    render(<ExtractionWizard open onClose={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: '查看覆盖报告' }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('get_extraction_coverage_report', { taskId: 't1' });
    });
    expect(await screen.findByText('林逸.coreFear')).toBeInTheDocument();
  });
});
