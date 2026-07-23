// 世界提取任务 UI 状态（P3 世界内容超集提取）：当前 taskId、WorldExtractionTask 快照、按 revision 去重的
// 任务事件订阅（listen 'engine-event' 的 Task kind）、合成完成的 Narrative 事件（worldAssembled）落 draft。
// start/get/confirmRosters/synthesize/cancel 动作与 src-tauri/src/commands/world_v2.rs 逐字对齐。
// createDiskStorage 持久化「进行中任务 id 列表」。整体镜像 useExtractionStore，差异见各处注释。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
import { listen } from '@tauri-apps/api/event';
import { appInvoke } from '../utils/runtime';
import { createDiskStorage } from './diskStorage';
// 与 character 提取共享的基础 DTO（凭据、章节、角色 roster、任务事件）type-only 复用，避免重复定义。
import type {
  ModelProfile,
  ChapterEntry,
  RosterEntry,
  SourceFingerprint,
  TaskEvent,
} from './useExtractionStore';

export type { ModelProfile, RosterEntry, RosterTier, DnaStatus } from './useExtractionStore';

// ---------- 引擎 DTO 镜像（camelCase，与 muse-engine world::types serde 形态一致） ----------

/** 世界提取阶段（对齐引擎 WorldStage）。 */
export type WorldStage =
  | 'scan'
  | 'merge'
  | 'tiering'
  | 'review'
  | 'synthesis'
  | 'assembled'
  | 'done'
  | 'cancelled';

/** location/item 归并条目（对齐引擎 WorldRosterEntry：实体无角色分层语义）。 */
export interface WorldRosterEntry {
  key: string;
  canonicalName: string;
  aliases: string[];
  mergedFrom: string[];
  userConfirmed: boolean;
  /** location：秘境标记（item 恒 false）。 */
  isSecretRealm: boolean;
}

/** 全书级剧情节拍草稿（Review 派生，用户不确认、随合成透传）。 */
export interface PlotBeatDraft {
  surface: string;
  chapterIndex: number;
  links: string[];
  tension: string;
  isHidden: boolean;
}

/** 全书级结局线索草稿（Review 派生）。 */
export interface EndingClueDraft {
  surface: string;
  affinityHint: string;
  chapterIndex: number;
}

export interface WorldExtractionTask {
  schemaVersion: number;
  taskId: string;
  workTitle: string;
  sourcePath: string;
  sourceFingerprint: SourceFingerprint;
  pipelineVersion: string;
  chapters: ChapterEntry[];
  /** 四条平行 roster：character 复用 RosterEntry（带 tier/dnaStatus）；location/item 用 WorldRosterEntry。 */
  characterRoster: RosterEntry[];
  locationRoster: WorldRosterEntry[];
  itemRoster: WorldRosterEntry[];
  /** plot/ending 是全书级派生，Review 前才产；确认后合成，只读。 */
  plotBeats: PlotBeatDraft[];
  endingClues: EndingClueDraft[];
  stage: WorldStage;
  revision: number;
  createdAt: number;
  updatedAt: number;
}

/** 采样提示（超集防刷元数据）。 */
export interface SamplingHints {
  instanceMainlineCount: number;
  instanceHiddenCount: number;
  instanceNpcCount: number;
  instanceLocationCount: number;
  /** 超集量 ÷ 单副本量（server 校验 ≥ 3.0 才过审）。 */
  redundancyRatio: number;
}

/**
 * 世界内容超集草稿（合成产物）。字段名对齐 server assembly::Skeleton（camelCase）。
 * ⚠️ 前端把它视为**只读产物**：一切编辑收敛到 Review 阶段的三条 roster，引擎在合成时统一收口。
 * 深编辑地点/道具/剧情线极易击穿服务端超集校验（悬空引用 / redundancyRatio<3.0），被 400 拒。
 * 因此这里用宽松结构：仅登记发布 + 摘要展示需要的字段。
 */
export interface WorldSkeletonDraft {
  sourceWork?: { sourceId?: string; title?: string };
  worldCharacters?: unknown[];
  locations?: unknown[];
  worldItems?: unknown[];
  mainlineNodes?: unknown[];
  hiddenContentPool?: unknown[];
  sideHookPool?: unknown[];
  endingPool?: unknown[];
  storylines?: unknown[];
  sampling?: SamplingHints;
  isSuperset?: boolean;
  [key: string]: unknown;
}

/** 世界提取覆盖报告（对齐引擎 WorldCoverageReport）。 */
export interface WorldCoverageReport {
  scannedChapters: number;
  totalChapters: number;
  failedChapters: number[];
  characterRosterSize: number;
  locationRosterSize: number;
  itemRosterSize: number;
}

/** WorldExtractionRequestDto（camelCase）。10 段 prompt 逐环节独立，对齐 world_v2.rs。 */
export interface WorldExtractionRequestInput {
  workTitle: string;
  sourcePath: string;
  profile: ModelProfile;
  scanPrompt: string;
  charMergePrompt: string;
  locMergePrompt: string;
  itemMergePrompt: string;
  charTieringPrompt: string;
  charSynthesisPrompt: string;
  locationSynthesisPrompt: string;
  itemSynthesisPrompt: string;
  plotSynthesisPrompt: string;
  endingSynthesisPrompt: string;
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

// appInvoke 命令签名扩展（不改 runtime.ts；命令名/参数与 world_v2.rs 精确对齐）。
declare module '../utils/runtime' {
  interface AppInvokeCommands {
    start_world_extraction: { args: { request: WorldExtractionRequestInput }; result: StartedTask };
    get_world_extraction_task: { args: { taskId: string }; result: WorldExtractionTask };
    confirm_world_rosters: {
      args: {
        taskId: string;
        expectedRevision: number;
        characters: RosterEntry[];
        locations: WorldRosterEntry[];
        items: WorldRosterEntry[];
      };
      result: WorldExtractionTask;
    };
    start_world_synthesis: {
      args: { taskId: string; request: WorldExtractionRequestInput };
      result: SynthesisStarted;
    };
    cancel_world_extraction: { args: { taskId: string }; result: boolean };
    get_world_coverage_report: { args: { taskId: string }; result: WorldCoverageReport };
  }
}

type EngineEventPayload = { kind?: string; [key: string]: unknown };

/** 合成完成的 Narrative 载荷（world_v2.rs 命令壳内 serde_json 手工构造，run_id=`wsynth-{taskId}`）。 */
type WorldNarrativePayload =
  | { kind: 'worldAssembled'; taskId: string; draft: WorldSkeletonDraft }
  | { kind: 'worldSynthesisFailed'; taskId: string; code: string; message: string };

interface WorldNarrativeEnvelope {
  kind: 'narrative';
  runId: string;
  payload: WorldNarrativePayload;
}

interface WorldExtractionStoreState {
  currentTaskId: string | null;
  task: WorldExtractionTask | null;
  /** 进行中任务 id 列表（持久化，断线重连的恢复清单）。 */
  activeTaskIds: string[];
  /** 每个任务最近一次已接受的事件（按 revision 去重后的结果）。 */
  taskEvents: Record<string, TaskEvent>;
  /** 每个任务已处理的最高 revision（去重水位，快照拉取时一并抬升）。 */
  lastRevisionByTask: Record<string, number>;
  /** 合成完成回传的世界超集草稿（发布步的入参；仅内存态，丢事件需重合成）。 */
  lastAssembledDraft: WorldSkeletonDraft | null;
  /** 最近一次合成完成对应的 taskId（校验 draft 归属）。 */
  lastAssembledTaskId: string | null;
  lastError: string | null;

  start: (request: WorldExtractionRequestInput) => Promise<string>;
  get: (taskId?: string) => Promise<WorldExtractionTask | null>;
  confirmRosters: (
    taskId: string,
    expectedRevision: number,
    characters: RosterEntry[],
    locations: WorldRosterEntry[],
    items: WorldRosterEntry[],
  ) => Promise<WorldExtractionTask>;
  synthesize: (taskId: string, request: WorldExtractionRequestInput) => Promise<string>;
  cancel: (taskId: string) => Promise<boolean>;
  getCoverageReport: (taskId: string) => Promise<WorldCoverageReport>;
  /** 纯事件归约：按 revision 去重后写入 taskEvents（listen 与测试共用）。 */
  applyTaskEvent: (evt: TaskEvent) => void;
  /** 纯事件归约：合成完成落 draft / 失败落 error（listen 与测试共用）。 */
  applyNarrativeEvent: (env: WorldNarrativeEnvelope) => void;
  /** 订阅 `engine-event`（Task + world Narrative），返回退订函数。 */
  subscribe: () => () => void;
  setCurrentTask: (taskId: string | null) => void;
  removeActiveTask: (taskId: string) => void;
  clearAssembled: () => void;
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

export const useWorldExtractionStore = create<WorldExtractionStoreState>()(
  persist(
    (set, get) => ({
      currentTaskId: null,
      task: null,
      activeTaskIds: [],
      taskEvents: {},
      lastRevisionByTask: {},
      lastAssembledDraft: null,
      lastAssembledTaskId: null,
      lastError: null,

      start: async (request) => {
        set({ lastError: null });
        try {
          const { taskId } = await appInvoke('start_world_extraction', { request });
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
          const task = await appInvoke('get_world_extraction_task', { taskId: id });
          set((state) => ({
            task,
            currentTaskId: id,
            // 快照 revision 抬升去重水位：早于/等于快照的迟到事件一律丢弃。
            lastRevisionByTask: raiseWatermark(state.lastRevisionByTask, id, task.revision),
          }));
          return task;
        } catch (e) {
          set({ lastError: String(e) });
          throw e;
        }
      },

      confirmRosters: async (taskId, expectedRevision, characters, locations, items) => {
        const task = await appInvoke('confirm_world_rosters', {
          taskId,
          expectedRevision,
          characters,
          locations,
          items,
        });
        set((state) => ({
          task: state.currentTaskId === taskId ? task : state.task,
          lastRevisionByTask: raiseWatermark(state.lastRevisionByTask, taskId, task.revision),
        }));
        return task;
      },

      synthesize: async (taskId, request) => {
        const { runId } = await appInvoke('start_world_synthesis', { taskId, request });
        return runId;
      },

      cancel: async (taskId) => {
        const cancelled = await appInvoke('cancel_world_extraction', { taskId });
        set((state) => ({
          activeTaskIds: state.activeTaskIds.filter((t) => t !== taskId),
        }));
        return cancelled;
      },

      getCoverageReport: (taskId) => appInvoke('get_world_coverage_report', { taskId }),

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

      applyNarrativeEvent: (env) =>
        set(() => {
          const payload = env?.payload;
          if (!payload || typeof payload.kind !== 'string') return {};
          if (payload.kind === 'worldAssembled') {
            return {
              lastAssembledDraft: payload.draft ?? null,
              lastAssembledTaskId: payload.taskId ?? null,
              lastError: null,
            };
          }
          if (payload.kind === 'worldSynthesisFailed') {
            return { lastError: `合成失败（${payload.code}）：${payload.message}` };
          }
          return {};
        }),

      subscribe: () => {
        let active = true;
        let unlisten: (() => void) | null = null;
        listen<EngineEventPayload>('engine-event', (event) => {
          if (!active) return;
          const payload = event?.payload;
          if (!payload) return;
          if (payload.kind === 'task') {
            get().applyTaskEvent(payload as unknown as TaskEvent);
          } else if (payload.kind === 'narrative') {
            const env = payload as unknown as WorldNarrativeEnvelope;
            const inner = env.payload?.kind;
            // 仅处理世界合成的两类 Narrative；角色回合事件（roundDone 等）不属于本 store。
            if (inner === 'worldAssembled' || inner === 'worldSynthesisFailed') {
              get().applyNarrativeEvent(env);
            }
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

      clearAssembled: () => set({ lastAssembledDraft: null, lastAssembledTaskId: null }),

      reset: () =>
        set({
          currentTaskId: null,
          task: null,
          lastError: null,
          lastAssembledDraft: null,
          lastAssembledTaskId: null,
        }),
    }),
    {
      name: 'museai-world-extraction-storage',
      version: 1,
      migrate: (persisted) => {
        if (persisted && typeof persisted === 'object') {
          const p = persisted as Partial<WorldExtractionStoreState>;
          return { ...(p as WorldExtractionStoreState), activeTaskIds: p.activeTaskIds ?? [] };
        }
        return persisted as WorldExtractionStoreState;
      },
      storage: createJSONStorage(() => createDiskStorage('world-extraction-store')),
      partialize: (state) => ({ activeTaskIds: state.activeTaskIds }) as WorldExtractionStoreState,
    },
  ),
);
