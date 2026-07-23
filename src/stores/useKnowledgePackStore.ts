// 知识包与绑定 UI 状态（规格 §9.4 / §11.2）：包/绑定列表，import/distill/search/delete + 绑定 CRUD
// 封装 appInvoke（命令名见 knowledge.rs）。createDiskStorage 持久化 UI 偏好。
// 注意：rightsBasis/allowedUses/retention/conflictPolicy 枚举在后端为 snake_case 序列化；PackMode 为 camelCase。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
import { appInvoke } from '../utils/runtime';
import { createDiskStorage } from './diskStorage';
import type { ModelProfile } from './useExtractionStore';

// ---------- 引擎 DTO 镜像（camelCase，值枚举大小写与后端 serde 一致） ----------

export type RightsBasis = 'owned' | 'licensed' | 'public_domain' | 'personal_use' | 'unknown';
export type AllowedUse = 'extract' | 'retrieve' | 'generate' | 'send_to_remote_model' | 'publish';
export type Retention = 'reference_original' | 'managed_copy' | 'index_only';
export type PackMode = 'knowledge' | 'mind' | 'value' | 'expression';
export type ConflictPolicy = 'character_core_wins' | 'ask_user';

export interface Heuristic {
  when: string;
  prefer: string;
  avoid?: string;
}

export interface Distilled {
  principles: string[];
  decisionHeuristics?: Heuristic[];
  evidenceStandards?: string[];
  expressionRules?: string[];
}

export interface PackSource {
  path: string;
  author?: string;
  contentHash: string;
  rightsBasis: RightsBasis;
  allowedUses: AllowedUse[];
  userAttestedAt?: number;
  retention: Retention;
}

export interface KnowledgePack {
  schemaVersion: number;
  id: string;
  title: string;
  source: PackSource;
  mode: PackMode;
  distilled: Distilled;
  timeBoundary?: string;
  chunkIndexStoreKey: string;
  indexVersion: string;
  revision: number;
}

export interface KnowledgeBinding {
  id: string;
  packId: string;
  characterId: string;
  storyId?: string;
  /** 0.0–1.0 影响强度 */
  influence: number;
  enabled: boolean;
  conflictPolicy: ConflictPolicy;
}

/** 检索片段（叙事回合注入的输入单元，跨 store 复用）。 */
export interface RetrievedFragment {
  packId: string;
  packTitle: string;
  chunkId: string;
  ordinal: number;
  text: string;
  score: number;
}

export interface ChunkStats {
  chunkCount: number;
  totalChars: number;
}

export interface ImportKnowledgeRequest {
  sourcePath: string;
  title: string;
  rightsBasis: RightsBasis;
  allowedUses: AllowedUse[];
  retention: Retention;
}

export interface ImportKnowledgeResponse {
  pack: KnowledgePack;
  chunkStats: ChunkStats;
}

export interface DistillRequest {
  packId: string;
  mode: PackMode;
  profile: ModelProfile;
  /** key: knowledge/mind/value/expression → system prompt */
  promptsByMode: Record<string, string>;
  promptVersion?: string;
}

// appInvoke 命令签名扩展（命令名/参数与 knowledge.rs 精确对齐）。
declare module '../utils/runtime' {
  interface AppInvokeCommands {
    import_knowledge_source: { args: { request: ImportKnowledgeRequest }; result: ImportKnowledgeResponse };
    distill_knowledge_pack: { args: { request: DistillRequest }; result: KnowledgePack };
    search_knowledge: {
      args: { packIds: string[]; query: string; limit?: number };
      result: RetrievedFragment[];
    };
    list_knowledge_packs: { args: void; result: KnowledgePack[] };
    delete_knowledge_pack: { args: { packId: string }; result: void };
    list_knowledge_bindings: { args: void; result: KnowledgeBinding[] };
    upsert_knowledge_binding: { args: { binding: KnowledgeBinding }; result: void };
    remove_knowledge_binding: { args: { bindingId: string }; result: void };
  }
}

interface KnowledgeStoreState {
  packs: KnowledgePack[];
  bindings: KnowledgeBinding[];
  /** 最近一次检索结果（UI 预览用，不持久化）。 */
  fragments: RetrievedFragment[];
  lastError: string | null;

  // UI 偏好（持久化）
  selectedPackIds: string[];
  searchLimit: number;

  listPacks: () => Promise<KnowledgePack[]>;
  importSource: (request: ImportKnowledgeRequest) => Promise<ImportKnowledgeResponse>;
  distill: (request: DistillRequest) => Promise<KnowledgePack>;
  search: (packIds: string[], query: string, limit?: number) => Promise<RetrievedFragment[]>;
  deletePack: (packId: string) => Promise<void>;
  listBindings: () => Promise<KnowledgeBinding[]>;
  upsertBinding: (binding: KnowledgeBinding) => Promise<void>;
  removeBinding: (bindingId: string) => Promise<void>;

  setSelectedPackIds: (ids: string[]) => void;
  togglePackSelected: (id: string) => void;
  setSearchLimit: (limit: number) => void;
}

export const useKnowledgePackStore = create<KnowledgeStoreState>()(
  persist(
    (set, get) => ({
      packs: [],
      bindings: [],
      fragments: [],
      lastError: null,
      selectedPackIds: [],
      searchLimit: 5,

      listPacks: async () => {
        try {
          const packs = await appInvoke('list_knowledge_packs');
          set({ packs });
          return packs;
        } catch (e) {
          set({ lastError: String(e) });
          throw e;
        }
      },

      importSource: async (request) => {
        set({ lastError: null });
        try {
          const response = await appInvoke('import_knowledge_source', { request });
          set((state) => ({
            packs: [...state.packs.filter((p) => p.id !== response.pack.id), response.pack],
          }));
          return response;
        } catch (e) {
          set({ lastError: String(e) });
          throw e;
        }
      },

      distill: async (request) => {
        const pack = await appInvoke('distill_knowledge_pack', { request });
        set((state) => ({
          packs: state.packs.some((p) => p.id === pack.id)
            ? state.packs.map((p) => (p.id === pack.id ? pack : p))
            : [...state.packs, pack],
        }));
        return pack;
      },

      search: async (packIds, query, limit) => {
        const fragments = await appInvoke('search_knowledge', {
          packIds,
          query,
          limit: limit ?? get().searchLimit,
        });
        set({ fragments });
        return fragments;
      },

      deletePack: async (packId) => {
        await appInvoke('delete_knowledge_pack', { packId });
        // 级联：后端删切块/索引/使用正文，本地镜像同步移除包、其绑定与选中项。
        set((state) => ({
          packs: state.packs.filter((p) => p.id !== packId),
          bindings: state.bindings.filter((b) => b.packId !== packId),
          selectedPackIds: state.selectedPackIds.filter((id) => id !== packId),
        }));
      },

      listBindings: async () => {
        try {
          const bindings = await appInvoke('list_knowledge_bindings');
          set({ bindings });
          return bindings;
        } catch (e) {
          set({ lastError: String(e) });
          throw e;
        }
      },

      upsertBinding: async (binding) => {
        await appInvoke('upsert_knowledge_binding', { binding });
        set((state) => ({
          bindings: state.bindings.some((b) => b.id === binding.id)
            ? state.bindings.map((b) => (b.id === binding.id ? binding : b))
            : [...state.bindings, binding],
        }));
      },

      removeBinding: async (bindingId) => {
        await appInvoke('remove_knowledge_binding', { bindingId });
        set((state) => ({ bindings: state.bindings.filter((b) => b.id !== bindingId) }));
      },

      setSelectedPackIds: (selectedPackIds) => set({ selectedPackIds }),

      togglePackSelected: (id) =>
        set((state) => ({
          selectedPackIds: state.selectedPackIds.includes(id)
            ? state.selectedPackIds.filter((p) => p !== id)
            : [...state.selectedPackIds, id],
        })),

      setSearchLimit: (searchLimit) => set({ searchLimit }),
    }),
    {
      name: 'museai-knowledge-storage',
      version: 1,
      // 旧数据原样通过（补默认 UI 偏好）。
      migrate: (persisted) => {
        if (persisted && typeof persisted === 'object') {
          const p = persisted as Partial<KnowledgeStoreState>;
          return {
            ...(p as KnowledgeStoreState),
            selectedPackIds: p.selectedPackIds ?? [],
            searchLimit: p.searchLimit ?? 5,
          };
        }
        return persisted as KnowledgeStoreState;
      },
      storage: createJSONStorage(() => createDiskStorage('knowledge-store')),
      partialize: (state) =>
        ({
          selectedPackIds: state.selectedPackIds,
          searchLimit: state.searchLimit,
        }) as KnowledgeStoreState,
    },
  ),
);
