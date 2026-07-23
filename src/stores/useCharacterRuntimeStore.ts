// 叙事运行 UI 状态（规格 §9.4 / §12）：当前 runId、NarrativeState 快照、场景列表、
// 回合事件订阅（'engine-event' 的 Narrative kind：roundDone/roundBlocked/roundFailed/synthesisDone），
// init/estimate/startRound/cancel/lock/branch 封装 appInvoke（命令名见 narrative.rs）。
// 合成完成（synthesisDone）时把角色卡送入 partner store（规格 §9.2/§9.3）。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
import { listen } from '@tauri-apps/api/event';
import { appInvoke } from '../utils/runtime';
import { createDiskStorage } from './diskStorage';
import { usePartnerStore } from './usePartnerStore';
import type { CharacterCardV2 } from '../utils/characterCardV2';
import type { ModelProfile } from './useExtractionStore';
import type { RetrievedFragment } from './useKnowledgePackStore';

// ---------- 五层状态镜像（narrative/types.rs，camelCase） ----------

export type RunMode = 'interactive' | 'observe' | 'chapterDraft';
export type ConstraintLevel = 'hard' | 'soft' | 'free';
export type NodeStatus = 'pending' | 'done' | 'bypassed' | 'blocked';
export type PatchOp = 'set' | 'append' | 'remove' | 'increment';
export type ArbiterResult = 'success' | 'partialSuccess' | 'failure' | 'invalid' | 'blocked';

export interface EmotionEntry {
  name: string;
  intensity: number;
  cause?: string;
}

export interface CharacterState {
  goals: string[];
  emotions: EmotionEntry[];
  resources: string[];
  secrets: string[];
  misconceptions: string[];
  plans: string[];
  arcStage: string;
}

export interface RelationState {
  from: string;
  to: string;
  trust: number;
  affinity: number;
  fear: number;
  debt: number;
  knownTo: string[];
  notes: string[];
}

export interface OutlineNode {
  id: string;
  summary: string;
  constraint: ConstraintLevel;
  status: NodeStatus;
}

export interface ForbiddenPredicate {
  id: string;
  expression: string;
  reason: string;
}

export interface NarrativeLayer {
  outlineNodes: OutlineNode[];
  forbiddenPredicates: ForbiddenPredicate[];
  foreshadowing: string[];
  pacingNotes: string[];
}

export interface AuthoringLayer {
  lockedSceneIds: string[];
  branchSnapshotIds: string[];
}

export interface NarrativeState {
  schemaVersion: number;
  runId: string;
  revision: number;
  world: Record<string, unknown>;
  characters: Record<string, CharacterState>;
  relations: RelationState[];
  narrative: NarrativeLayer;
  authoring: AuthoringLayer;
}

export interface PatchOperation {
  op: PatchOp;
  path: string;
  value?: unknown;
  precondition?: unknown;
}

export interface StatePatch {
  id: string;
  baseRevision: number;
  sourceDecisionIds: string[];
  operations: PatchOperation[];
}

export interface SpeakIntent {
  willSpeak: boolean;
  purpose: string;
}

export interface Prediction {
  characterId: string;
  expected: string;
  confidence: number;
}

export interface RoleDecision {
  decisionId: string;
  characterId: string;
  intent: string;
  action: string;
  speak: SpeakIntent;
  targets: string[];
  acceptableCosts: string[];
  predictions: Prediction[];
}

export interface ArbiterOutcome {
  decisionId: string;
  characterId: string;
  result: ArbiterResult;
  ruleRefs: string[];
  consequence: string;
}

export type DomainEventType =
  | 'action_resolved'
  | 'dialogue_spoken'
  | 'relation_changed'
  | 'resource_changed'
  | 'outline_progressed'
  | 'consent_requested';

export type EventVisibility =
  | { scope: 'public' }
  | { scope: 'private'; audienceCharacterIds: string[] };

export interface DomainEvent {
  schemaVersion: number;
  id: string;
  runId: string;
  sequence: number;
  type: DomainEventType;
  actorIds: string[];
  targetIds?: string[];
  fact: unknown;
  statePatchId: string;
  causedBy: string[];
  visibility: EventVisibility;
}

export interface SceneRecord {
  sceneId: string;
  tick: number;
  situation: string;
  decisions: RoleDecision[];
  outcomes: ArbiterOutcome[];
  prose: string;
  events: DomainEvent[];
  statePatch: StatePatch;
  locked: boolean;
  createdAt: number;
}

export interface RoundBudget {
  maxTotalTokens: number;
  spentTokens: number;
  maxScenes: number;
}

export interface CostEstimate {
  callsPerScene: number;
  estimatedTokensLow: number;
  estimatedTokensHigh: number;
}

export interface Snapshot {
  schemaVersion: number;
  snapshotId: string;
  runId: string;
  atSceneId: string;
  state: NarrativeState;
  createdAt: number;
}

// ---------- 回合请求 DTO（RoundRequestDto，camelCase） ----------

export interface NarrativePromptsInput {
  director: string;
  decide: string;
  arbiter: string;
  writer: string;
  critic: string;
  promptVersion?: string;
}

export interface ModelRoutesInput {
  default: ModelProfile;
  decide?: ModelProfile;
  arbiter?: ModelProfile;
  writer?: ModelProfile;
  critic?: ModelProfile;
  director?: ModelProfile;
}

export interface RoundRequestInput {
  runId: string;
  mode: RunMode;
  routes: ModelRoutesInput;
  prompts: NarrativePromptsInput;
  activeCards: Record<string, CharacterCardV2>;
  otherCardsBrief: Record<string, string>;
  whispers: Record<string, string>;
  fragments: Record<string, RetrievedFragment[]>;
  temperatureDecide?: number;
  temperatureWriter?: number;
  maxOutputTokens?: number;
  budget: RoundBudget;
}

export interface RoundStarted {
  roundId: string;
}

// ---------- Narrative kind 事件载荷（命令壳内 serde_json 手工构造的 kind） ----------

export type NarrativePayload =
  | { kind: 'roundDone'; runId: string; sceneId: string; scene: SceneRecord; critic: unknown; spentTokens: number }
  | { kind: 'roundBlocked'; runId: string; reason: string }
  | { kind: 'roundFailed'; runId: string; code: string; message: string }
  | { kind: 'synthesisDone'; taskId: string; cards: CharacterCardV2[] }
  | { kind: 'synthesisFailed'; taskId: string; code: string; message: string };

/** `engine-event` 的 Narrative kind 外层（EngineEvent::Narrative）。 */
export interface NarrativeEnvelope {
  kind: 'narrative';
  runId: string;
  payload: NarrativePayload;
}

// appInvoke 命令签名扩展（命令名/参数与 narrative.rs 精确对齐）。
declare module '../utils/runtime' {
  interface AppInvokeCommands {
    narrative_init_run: { args: { state: NarrativeState }; result: NarrativeState };
    narrative_get_state: { args: { runId: string }; result: NarrativeState };
    narrative_estimate: {
      args: { activeCount: number; maxOutputTokens: number; scenes: number };
      result: CostEstimate;
    };
    start_narrative_round: { args: { request: RoundRequestInput }; result: RoundStarted };
    cancel_narrative_round: { args: { roundId: string }; result: boolean };
    narrative_list_scenes: { args: { runId: string }; result: string[] };
    narrative_get_scene: { args: { runId: string; sceneId: string }; result: SceneRecord };
    narrative_lock_scenes: { args: { runId: string; sceneIds: string[] }; result: NarrativeState };
    narrative_take_snapshot: { args: { runId: string; atSceneId: string }; result: Snapshot };
    narrative_branch: {
      args: { snapshotId: string; sourceRunId: string; newRunId: string };
      result: NarrativeState;
    };
    narrative_list_snapshots: { args: { runId: string }; result: Snapshot[] };
  }
}

type EngineEventPayload = { kind?: string;[key: string]: unknown };
type RoundStatus = 'idle' | 'running' | 'done' | 'blocked' | 'failed';

interface RuntimeStoreState {
  currentRunId: string | null; // 持久化：便于重载后恢复上次运行
  state: NarrativeState | null;
  sceneIds: string[];
  scenes: Record<string, SceneRecord>;
  currentRoundId: string | null;
  roundStatus: RoundStatus;
  blockedReason: string | null;
  lastCritic: unknown | null;
  lastError: { code: string; message: string } | null;
  costEstimate: CostEstimate | null;
  lastSynthesis: { taskId: string; cards: CharacterCardV2[] } | null;

  init: (state: NarrativeState) => Promise<NarrativeState>;
  getState: (runId?: string) => Promise<NarrativeState | null>;
  estimate: (activeCount: number, maxOutputTokens: number, scenes: number) => Promise<CostEstimate>;
  startRound: (request: RoundRequestInput) => Promise<string>;
  cancel: (roundId?: string) => Promise<boolean>;
  listScenes: (runId?: string) => Promise<string[]>;
  getScene: (runId: string, sceneId: string) => Promise<SceneRecord>;
  lock: (runId: string, sceneIds: string[]) => Promise<NarrativeState>;
  takeSnapshot: (runId: string, atSceneId: string) => Promise<Snapshot>;
  branch: (snapshotId: string, sourceRunId: string, newRunId: string) => Promise<NarrativeState>;
  listSnapshots: (runId: string) => Promise<Snapshot[]>;
  /** 纯事件归约：按 payload.kind 分派（listen 与测试共用）。 */
  applyNarrativeEvent: (evt: NarrativeEnvelope) => void;
  /** 订阅 `engine-event` 的 Narrative kind，返回取消订阅函数。 */
  subscribe: () => () => void;
  setCurrentRun: (runId: string | null) => void;
  reset: () => void;
}

const mergeScene = (scenes: Record<string, SceneRecord>, scene: SceneRecord) => ({
  ...scenes,
  [scene.sceneId]: scene,
});

export const useCharacterRuntimeStore = create<RuntimeStoreState>()(
  persist(
    (set, get) => ({
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

      init: async (state) => {
        const result = await appInvoke('narrative_init_run', { state });
        set({ currentRunId: result.runId, state: result, sceneIds: [], scenes: {} });
        return result;
      },

      getState: async (runId) => {
        const id = runId ?? get().currentRunId;
        if (!id) return null;
        const state = await appInvoke('narrative_get_state', { runId: id });
        set({ state, currentRunId: id });
        return state;
      },

      estimate: async (activeCount, maxOutputTokens, scenes) => {
        const costEstimate = await appInvoke('narrative_estimate', {
          activeCount,
          maxOutputTokens,
          scenes,
        });
        set({ costEstimate });
        return costEstimate;
      },

      startRound: async (request) => {
        set({ roundStatus: 'running', blockedReason: null, lastError: null });
        try {
          const { roundId } = await appInvoke('start_narrative_round', { request });
          set({ currentRoundId: roundId });
          return roundId;
        } catch (e) {
          set({ roundStatus: 'idle', lastError: { code: 'invoke', message: String(e) } });
          throw e;
        }
      },

      cancel: async (roundId) => {
        const id = roundId ?? get().currentRoundId;
        if (!id) return false;
        const cancelled = await appInvoke('cancel_narrative_round', { roundId: id });
        set((state) => ({
          currentRoundId: state.currentRoundId === id ? null : state.currentRoundId,
          roundStatus: state.roundStatus === 'running' ? 'idle' : state.roundStatus,
        }));
        return cancelled;
      },

      listScenes: async (runId) => {
        const id = runId ?? get().currentRunId;
        if (!id) return [];
        const sceneIds = await appInvoke('narrative_list_scenes', { runId: id });
        set({ sceneIds });
        return sceneIds;
      },

      getScene: async (runId, sceneId) => {
        const scene = await appInvoke('narrative_get_scene', { runId, sceneId });
        set((state) => ({
          scenes: mergeScene(state.scenes, scene),
          sceneIds: state.sceneIds.includes(sceneId) ? state.sceneIds : [...state.sceneIds, sceneId],
        }));
        return scene;
      },

      lock: async (runId, sceneIds) => {
        const state = await appInvoke('narrative_lock_scenes', { runId, sceneIds });
        set((prev) => ({ state: prev.currentRunId === runId ? state : prev.state }));
        return state;
      },

      takeSnapshot: (runId, atSceneId) => appInvoke('narrative_take_snapshot', { runId, atSceneId }),

      branch: (snapshotId, sourceRunId, newRunId) =>
        appInvoke('narrative_branch', { snapshotId, sourceRunId, newRunId }),

      listSnapshots: (runId) => appInvoke('narrative_list_snapshots', { runId }),

      applyNarrativeEvent: (evt) => {
        if (!evt || evt.kind !== 'narrative' || !evt.payload) return;
        const payload = evt.payload;
        const currentRunId = get().currentRunId;
        switch (payload.kind) {
          case 'roundDone': {
            // 运行隔离：仅接受当前运行的回合事件（未锁定当前运行时放行）。
            if (currentRunId && payload.runId !== currentRunId) return;
            set((state) => ({
              roundStatus: 'done',
              currentRoundId: null,
              lastCritic: payload.critic,
              scenes: mergeScene(state.scenes, payload.scene),
              sceneIds: state.sceneIds.includes(payload.sceneId)
                ? state.sceneIds
                : [...state.sceneIds, payload.sceneId],
            }));
            break;
          }
          case 'roundBlocked': {
            if (currentRunId && payload.runId !== currentRunId) return;
            set({ roundStatus: 'blocked', blockedReason: payload.reason, currentRoundId: null });
            break;
          }
          case 'roundFailed': {
            if (currentRunId && payload.runId !== currentRunId) return;
            set({
              roundStatus: 'failed',
              currentRoundId: null,
              lastError: { code: payload.code, message: payload.message },
            });
            break;
          }
          case 'synthesisDone': {
            // 角色卡合成完成：入 partner store（addV2Card 幂等），并留存最近一次结果。
            const cards = Array.isArray(payload.cards) ? payload.cards : [];
            const addV2Card = usePartnerStore.getState().addV2Card;
            for (const card of cards) addV2Card(card);
            set({ lastSynthesis: { taskId: payload.taskId, cards } });
            break;
          }
          case 'synthesisFailed': {
            set({ lastError: { code: payload.code, message: payload.message } });
            break;
          }
          default:
            break;
        }
      },

      subscribe: () => {
        let active = true;
        let unlisten: (() => void) | null = null;
        listen<EngineEventPayload>('engine-event', (event) => {
          if (!active) return;
          const payload = event?.payload;
          if (payload && payload.kind === 'narrative') {
            get().applyNarrativeEvent(payload as unknown as NarrativeEnvelope);
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

      setCurrentRun: (currentRunId) => set({ currentRunId }),

      reset: () =>
        set({
          state: null,
          sceneIds: [],
          scenes: {},
          currentRoundId: null,
          roundStatus: 'idle',
          blockedReason: null,
          lastCritic: null,
          lastError: null,
        }),
    }),
    {
      name: 'museai-character-runtime-storage',
      version: 1,
      // 旧数据原样通过。
      migrate: (persisted) => {
        if (persisted && typeof persisted === 'object') {
          return persisted as RuntimeStoreState;
        }
        return persisted as RuntimeStoreState;
      },
      storage: createJSONStorage(() => createDiskStorage('character-runtime-store')),
      partialize: (state) => ({ currentRunId: state.currentRunId }) as RuntimeStoreState,
    },
  ),
);
