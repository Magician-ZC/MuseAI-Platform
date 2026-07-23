// 提取任务 UI 状态（规格 §9.3 / §10.2）：当前 taskId、ExtractionTask 快照、按 revision 去重的
// 任务事件订阅（listen 'engine-event' 的 Task kind）、start/get/confirmRoster/synthesize/cancel 动作。
// createDiskStorage 持久化「进行中任务 id 列表」。命令名/参数与 src-tauri/src/commands/character_v2.rs 对齐。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
import { listen } from '@tauri-apps/api/event';
import { appInvoke } from '../utils/runtime';
import { createDiskStorage } from './diskStorage';

// ---------- 引擎 DTO 镜像（camelCase，与 muse-engine serde 形态一致） ----------

export type ModelInterface = 'OpenAI-compatible' | 'Anthropic-compatible';

/** muse-engine ModelProfile 的 TS 镜像（一次模型调用所需凭据与参数）。集中于此，其余 store 以 type-only 复用。 */
export interface ModelProfile {
  interface: ModelInterface;
  baseUrl: string;
  apiKey: string;
  model: string;
}

export type ChapterStatus = 'pending' | 'running' | 'scanned' | 'failed' | 'cancelled';
export type RosterTier = 'core' | 'major' | 'functional' | 'extra';
export type DnaStatus = 'pending' | 'generated' | 'failed' | 'skipped';
export type TaskStage =
  | 'preprocess'
  | 'scan'
  | 'merge'
  | 'tiering'
  | 'synthesis'
  | 'review'
  | 'done'
  | 'cancelled';

export interface TaskError {
  code: string;
  message: string;
  retryable: boolean;
}

export interface SourceFingerprint {
  size: number;
  modifiedAt: number;
  contentHash: string;
}

export interface ChapterEntry {
  id: string;
  index: number;
  title: string;
  charRange: [number, number];
  status: ChapterStatus;
  attempt: number;
  discoveryStoreKey?: string;
  error?: TaskError;
}

export interface RosterEntry {
  key: string;
  canonicalName: string;
  aliases: string[];
  tier: RosterTier;
  mergedFrom: string[];
  userConfirmed: boolean;
  dnaStatus: DnaStatus;
}

export interface ExtractionTask {
  schemaVersion: number;
  taskId: string;
  workTitle: string;
  sourcePath: string;
  sourceFingerprint: SourceFingerprint;
  pipelineVersion: string;
  chapters: ChapterEntry[];
  roster: RosterEntry[];
  stage: TaskStage;
  revision: number;
  createdAt: number;
  updatedAt: number;
}

/** ExtractionRequestDto（camelCase）。profile 为 ModelProfile；prompts 逐环节独立。 */
export interface ExtractionRequestInput {
  workTitle: string;
  sourcePath: string;
  profile: ModelProfile;
  scanPrompt: string;
  mergePrompt: string;
  tieringPrompt: string;
  synthesisPrompt: string;
  promptVersion?: string;
  temperature?: number;
  maxOutputTokens?: number;
  concurrency?: number;
}

export interface StartedTask {
  taskId: string;
}

export interface SynthesisStarted {
  runId: string;
}

export interface CoverageReport {
  scannedChapters: number;
  totalChapters: number;
  failedChapters: number[];
  rosterSize: number;
  unresolvedAliases: string[];
  lowConfidenceFields: string[];
}

/** `engine-event` 的 Task kind 载荷（EngineEvent::Task，规格 §10.2 至少含这些字段）。 */
export interface TaskEvent {
  kind: 'task';
  taskId: string;
  revision: number;
  stage: string;
  itemId: string | null;
  progress: number;
  error: TaskError | null;
}

// appInvoke 命令签名扩展（不改 runtime.ts；命令名/参数与 character_v2.rs 精确对齐）。
declare module '../utils/runtime' {
  interface AppInvokeCommands {
    start_character_extraction: { args: { request: ExtractionRequestInput }; result: StartedTask };
    get_character_extraction_task: { args: { taskId: string }; result: ExtractionTask };
    confirm_character_roster: {
      args: { taskId: string; expectedRevision: number; roster: RosterEntry[] };
      result: ExtractionTask;
    };
    start_character_dna_synthesis: {
      args: { taskId: string; request: ExtractionRequestInput; keys: string[] };
      result: SynthesisStarted;
    };
    cancel_character_extraction: { args: { taskId: string }; result: boolean };
    get_extraction_coverage_report: { args: { taskId: string }; result: CoverageReport };
  }
}

type EngineEventPayload = { kind?: string;[key: string]: unknown };

interface ExtractionStoreState {
  currentTaskId: string | null;
  task: ExtractionTask | null;
  /** 进行中任务 id 列表（持久化，断线重连的恢复清单）。 */
  activeTaskIds: string[];
  /** 每个任务最近一次已接受的事件（按 revision 去重后的结果）。 */
  taskEvents: Record<string, TaskEvent>;
  /** 每个任务已处理的最高 revision（去重水位，快照拉取时一并抬升）。 */
  lastRevisionByTask: Record<string, number>;
  lastError: string | null;

  start: (request: ExtractionRequestInput) => Promise<string>;
  get: (taskId?: string) => Promise<ExtractionTask | null>;
  confirmRoster: (
    taskId: string,
    expectedRevision: number,
    roster: RosterEntry[],
  ) => Promise<ExtractionTask>;
  synthesize: (
    taskId: string,
    request: ExtractionRequestInput,
    keys: string[],
  ) => Promise<string>;
  cancel: (taskId: string) => Promise<boolean>;
  getCoverageReport: (taskId: string) => Promise<CoverageReport>;
  /** 纯事件归约：按 revision 去重后写入 taskEvents（listen 与测试共用）。 */
  applyTaskEvent: (evt: TaskEvent) => void;
  /** 订阅 `engine-event` 的 Task kind，返回取消订阅函数（参考 runtime.ts listenStream 模式）。 */
  subscribe: () => () => void;
  setCurrentTask: (taskId: string | null) => void;
  removeActiveTask: (taskId: string) => void;
  reset: () => void;
}

const raiseWatermark = (
  map: Record<string, number>,
  taskId: string,
  revision: number,
): Record<string, number> => ({
  ...map,
  [taskId]: Math.max(map[taskId] ?? -1, revision),
});

export const useExtractionStore = create<ExtractionStoreState>()(
  persist(
    (set, get) => ({
      currentTaskId: null,
      task: null,
      activeTaskIds: [],
      taskEvents: {},
      lastRevisionByTask: {},
      lastError: null,

      start: async (request) => {
        set({ lastError: null });
        try {
          const { taskId } = await appInvoke('start_character_extraction', { request });
          set((state) => ({
            currentTaskId: taskId,
            activeTaskIds: state.activeTaskIds.includes(taskId)
              ? state.activeTaskIds
              : [...state.activeTaskIds, taskId],
          }));
          return taskId;
        } catch (e) {
          set({ lastError: String(e) });
          throw e;
        }
      },

      get: async (taskId) => {
        const id = taskId ?? get().currentTaskId;
        if (!id) return null;
        try {
          const task = await appInvoke('get_character_extraction_task', { taskId: id });
          set((state) => ({
            task,
            currentTaskId: id,
            // 快照 revision 抬升去重水位：早于/等于快照的迟到事件一律丢弃（规格 §10.2）。
            lastRevisionByTask: raiseWatermark(state.lastRevisionByTask, id, task.revision),
          }));
          return task;
        } catch (e) {
          set({ lastError: String(e) });
          throw e;
        }
      },

      confirmRoster: async (taskId, expectedRevision, roster) => {
        const task = await appInvoke('confirm_character_roster', {
          taskId,
          expectedRevision,
          roster,
        });
        set((state) => ({
          task: state.currentTaskId === taskId ? task : state.task,
          lastRevisionByTask: raiseWatermark(state.lastRevisionByTask, taskId, task.revision),
        }));
        return task;
      },

      synthesize: async (taskId, request, keys) => {
        const { runId } = await appInvoke('start_character_dna_synthesis', {
          taskId,
          request,
          keys,
        });
        return runId;
      },

      cancel: async (taskId) => {
        const cancelled = await appInvoke('cancel_character_extraction', { taskId });
        set((state) => ({
          activeTaskIds: state.activeTaskIds.filter((t) => t !== taskId),
        }));
        return cancelled;
      },

      getCoverageReport: (taskId) => appInvoke('get_extraction_coverage_report', { taskId }),

      applyTaskEvent: (evt) =>
        set((state) => {
          if (!evt || evt.kind !== 'task' || typeof evt.taskId !== 'string') return {};
          const watermark = state.lastRevisionByTask[evt.taskId] ?? -1;
          if (evt.revision <= watermark) return {}; // 去重：重复/迟到事件不推进
          return {
            taskEvents: { ...state.taskEvents, [evt.taskId]: evt },
            lastRevisionByTask: { ...state.lastRevisionByTask, [evt.taskId]: evt.revision },
          };
        }),

      subscribe: () => {
        let active = true;
        let unlisten: (() => void) | null = null;
        listen<EngineEventPayload>('engine-event', (event) => {
          if (!active) return;
          const payload = event?.payload;
          if (payload && payload.kind === 'task') {
            get().applyTaskEvent(payload as unknown as TaskEvent);
          }
        }).then((fn) => {
          unlisten = fn;
          if (!active) fn();
        });
        return () => {
          active = false;
          if (unlisten) unlisten();
        };
      },

      setCurrentTask: (currentTaskId) => set({ currentTaskId }),

      removeActiveTask: (taskId) =>
        set((state) => ({ activeTaskIds: state.activeTaskIds.filter((t) => t !== taskId) })),

      reset: () => set({ currentTaskId: null, task: null, lastError: null }),
    }),
    {
      name: 'museai-extraction-storage',
      version: 1,
      // 旧数据原样通过（补默认字段）。
      migrate: (persisted) => {
        if (persisted && typeof persisted === 'object') {
          const p = persisted as Partial<ExtractionStoreState>;
          return { ...(p as ExtractionStoreState), activeTaskIds: p.activeTaskIds ?? [] };
        }
        return persisted as ExtractionStoreState;
      },
      storage: createJSONStorage(() => createDiskStorage('extraction-store')),
      partialize: (state) => ({ activeTaskIds: state.activeTaskIds }) as ExtractionStoreState,
    },
  ),
);
