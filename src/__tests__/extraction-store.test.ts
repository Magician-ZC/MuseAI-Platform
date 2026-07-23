import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  useExtractionStore,
  type ExtractionTask,
  type TaskEvent,
} from '../stores/useExtractionStore';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

const mockInvoke = invoke as unknown as Mock;
const mockListen = listen as unknown as Mock;

const flushMicrotasks = () => new Promise((resolve) => setTimeout(resolve, 0));

const makeTask = (overrides: Partial<ExtractionTask> = {}): ExtractionTask => ({
  schemaVersion: 1,
  taskId: 'task-1',
  workTitle: '示例长篇',
  sourcePath: '/books/demo.txt',
  sourceFingerprint: { size: 100, modifiedAt: 1, contentHash: 'h' },
  pipelineVersion: 'v1',
  chapters: [],
  roster: [],
  stage: 'scan',
  revision: 4,
  createdAt: 0,
  updatedAt: 0,
  ...overrides,
});

const makeEvent = (revision: number, overrides: Partial<TaskEvent> = {}): TaskEvent => ({
  kind: 'task',
  taskId: 'task-1',
  revision,
  stage: 'scan',
  itemId: null,
  progress: revision / 10,
  error: null,
  ...overrides,
});

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async () => undefined);
  mockListen.mockReset();
  useExtractionStore.setState({
    currentTaskId: null,
    task: null,
    activeTaskIds: [],
    taskEvents: {},
    lastRevisionByTask: {},
    lastError: null,
  });
});

describe('useExtractionStore actions', () => {
  it('start 调用 start_character_extraction，登记 currentTaskId 与进行中任务列表', async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'start_character_extraction' ? { taskId: 'task-9' } : undefined,
    );

    const request = {
      workTitle: 'W',
      sourcePath: '/p',
      profile: { interface: 'OpenAI-compatible' as const, baseUrl: 'u', apiKey: 'k', model: 'm' },
      scanPrompt: 's',
      mergePrompt: 'm',
      tieringPrompt: 't',
      synthesisPrompt: 'y',
    };
    const taskId = await useExtractionStore.getState().start(request);

    expect(taskId).toBe('task-9');
    expect(mockInvoke).toHaveBeenCalledWith('start_character_extraction', { request });
    expect(useExtractionStore.getState().currentTaskId).toBe('task-9');
    expect(useExtractionStore.getState().activeTaskIds).toEqual(['task-9']);
  });

  it('get 拉取快照并按快照 revision 抬升去重水位', async () => {
    const task = makeTask({ taskId: 'task-1', revision: 6 });
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'get_character_extraction_task' ? task : undefined,
    );

    const result = await useExtractionStore.getState().get('task-1');

    expect(result).toEqual(task);
    expect(mockInvoke).toHaveBeenCalledWith('get_character_extraction_task', { taskId: 'task-1' });
    expect(useExtractionStore.getState().task).toEqual(task);
    // 早于/等于快照 revision 的迟到事件被丢弃
    useExtractionStore.getState().applyTaskEvent(makeEvent(6));
    expect(useExtractionStore.getState().taskEvents['task-1']).toBeUndefined();
    useExtractionStore.getState().applyTaskEvent(makeEvent(7));
    expect(useExtractionStore.getState().taskEvents['task-1'].revision).toBe(7);
  });

  it('confirmRoster 传入 expectedRevision 与 roster 并回写任务', async () => {
    const updated = makeTask({ revision: 8, stage: 'synthesis' });
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'confirm_character_roster' ? updated : undefined,
    );
    useExtractionStore.setState({ currentTaskId: 'task-1' });

    const roster = [
      {
        key: 'k1',
        canonicalName: '林逸',
        aliases: ['小林'],
        tier: 'core' as const,
        mergedFrom: [],
        userConfirmed: true,
        dnaStatus: 'pending' as const,
      },
    ];
    const result = await useExtractionStore.getState().confirmRoster('task-1', 4, roster);

    expect(result).toEqual(updated);
    expect(mockInvoke).toHaveBeenCalledWith('confirm_character_roster', {
      taskId: 'task-1',
      expectedRevision: 4,
      roster,
    });
    expect(useExtractionStore.getState().task).toEqual(updated);
  });

  it('synthesize 返回 runId，cancel 从进行中列表移除任务', async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'start_character_dna_synthesis') return { runId: 'synth-task-1' };
      if (cmd === 'cancel_character_extraction') return true;
      return undefined;
    });
    useExtractionStore.setState({ activeTaskIds: ['task-1', 'task-2'] });

    const request = {
      workTitle: 'W',
      sourcePath: '/p',
      profile: { interface: 'Anthropic-compatible' as const, baseUrl: 'u', apiKey: 'k', model: 'm' },
      scanPrompt: 's',
      mergePrompt: 'm',
      tieringPrompt: 't',
      synthesisPrompt: 'y',
    };
    const runId = await useExtractionStore.getState().synthesize('task-1', request, ['k1']);
    expect(runId).toBe('synth-task-1');
    expect(mockInvoke).toHaveBeenCalledWith('start_character_dna_synthesis', {
      taskId: 'task-1',
      request,
      keys: ['k1'],
    });

    const cancelled = await useExtractionStore.getState().cancel('task-1');
    expect(cancelled).toBe(true);
    expect(useExtractionStore.getState().activeTaskIds).toEqual(['task-2']);
  });
});

describe('useExtractionStore task-event dedup', () => {
  it('applyTaskEvent 按 revision 去重（重复/迟到不推进，前进接受）', () => {
    const store = useExtractionStore.getState();

    store.applyTaskEvent(makeEvent(1));
    expect(useExtractionStore.getState().taskEvents['task-1'].revision).toBe(1);

    store.applyTaskEvent(makeEvent(2));
    expect(useExtractionStore.getState().taskEvents['task-1'].revision).toBe(2);

    store.applyTaskEvent(makeEvent(1)); // 迟到
    expect(useExtractionStore.getState().taskEvents['task-1'].revision).toBe(2);

    store.applyTaskEvent(makeEvent(2)); // 重复
    expect(useExtractionStore.getState().taskEvents['task-1'].revision).toBe(2);

    store.applyTaskEvent(makeEvent(5)); // 前进
    expect(useExtractionStore.getState().taskEvents['task-1'].revision).toBe(5);
  });

  it('多任务各自独立去重', () => {
    const store = useExtractionStore.getState();
    store.applyTaskEvent(makeEvent(3, { taskId: 'A' }));
    store.applyTaskEvent(makeEvent(1, { taskId: 'B' }));
    store.applyTaskEvent(makeEvent(2, { taskId: 'A' })); // A 的迟到
    store.applyTaskEvent(makeEvent(2, { taskId: 'B' })); // B 前进
    expect(useExtractionStore.getState().taskEvents['A'].revision).toBe(3);
    expect(useExtractionStore.getState().taskEvents['B'].revision).toBe(2);
  });
});

describe('useExtractionStore subscribe', () => {
  it('订阅 engine-event，仅分派 Task kind，返回可用的取消订阅函数', async () => {
    let handler: ((event: { payload: unknown }) => void) | null = null;
    const unlistenSpy = vi.fn();
    mockListen.mockImplementation(async (_channel: string, cb: (event: { payload: unknown }) => void) => {
      handler = cb;
      return unlistenSpy;
    });

    const unsubscribe = useExtractionStore.getState().subscribe();
    await flushMicrotasks();

    expect(mockListen).toHaveBeenCalledWith('engine-event', expect.any(Function));
    expect(handler).not.toBeNull();

    handler!({ payload: makeEvent(3, { taskId: 'task-1' }) });
    expect(useExtractionStore.getState().taskEvents['task-1'].revision).toBe(3);

    // 非 Task kind 被忽略
    handler!({ payload: { kind: 'narrative', runId: 'r1', payload: { kind: 'roundDone' } } });
    expect(useExtractionStore.getState().taskEvents['r1']).toBeUndefined();

    unsubscribe();
    expect(unlistenSpy).toHaveBeenCalled();
  });
});
