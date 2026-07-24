// 平台模式 UI 状态（C1，agent-C1 所有）：世界大厅 / 我的世界 / 日报列表 / 房间视图切换。
// 只承载平台侧的列表与 UI 偏好；房间事件流、日报详情、发布等页面级数据留在各页面内。
// 所有云端调用走 cloudApi（cloudFetch），错误经 describeCloudError 统一为友好中文，与本地能力物理隔离。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
import { cloudFetch, CloudError } from '../utils/cloudApi';

// ---------- 云端契约镜像（camelCase，与 server 端 JSON 形态一致） ----------

export type RoomType = 'idle' | 'chapter' | 'arena';
export type RoomTypeFilter = RoomType | 'all';

export interface AiLabel {
  visible: boolean;
  metadataRef?: string;
}

export interface WorldSummary {
  id: string;
  roomType: string;
  title: string;
  status: string;
  visibility: string;
  memberLimit: number;
  memberCount: number;
  tickPerDay: number;
  aiLabel?: AiLabel;
}

export interface WorldRosterEntry {
  cloudCharacterId: string;
  name: string;
  aiLabel?: AiLabel;
}

export interface WorldDetail {
  id: string;
  title: string;
  roomType: string;
  status: string;
  visibility: string;
  memberLimit: number;
  memberCount: number;
  tickPerDay: number;
  templateId: string;
  templateVersion: number;
  engineVersion: string;
  promptSetVersion: string;
  modelRouteVersion: string;
  roster: WorldRosterEntry[];
  aiLabel?: AiLabel;
  compliance?: { aiGenerated: boolean; arbitrationPublic: boolean };
  /** 服务端当前未在详情返回；留字段以便前向兼容 revision CAS（见 WorldRoom 干预面板）。 */
  stateRevision?: number;
}

export interface WorldEventItem {
  id: string;
  worldId: string;
  tick: number;
  sequence: number;
  domainEventId: string;
  type: string; // action | dialogue | conflict | alliance | item | status | arbiter | world | consent_request
  actors: string[];
  targets?: string[];
  visibility: string;
  projection?: { summary?: string; narrative?: string; quote?: string };
  aiLabel?: AiLabel;
  occurredAt: number;
}

// ---------- 世界权威状态快照（#6b；GET /worlds/{id}/state-summary，由 narrative_state 派生、按 principal 过滤） ----------

/** 一条权威关系边（有向）：数值区间由服务端定义，前端只按相对量/符号呈现（scale 无关）。 */
export interface WorldRelation {
  from: string;
  to: string;
  trust: number;
  affinity: number;
  fear: number;
  debt: number;
}

/** 一名角色的权威状态：弧光阶段 + 活跃度。 */
export interface WorldCharacterState {
  id: string;
  arcStage: string;
  activity: number;
}

/** state-summary 端点返回体：权威关系 + 角色状态。 */
export interface WorldStateSummary {
  relations: WorldRelation[];
  characters: WorldCharacterState[];
}

export interface CloudCharacter {
  id: string;
  localCardId: string;
  version: number;
  rightsDeclaration: string;
  moderation: string; // pending | approved | rejected
  withdrawn: boolean;
  createdAt: number;
}

export interface ReportListItem {
  id: string;
  worldId: string;
  characterId: string;
  reportDay: string;
  opened: boolean;
  createdAt: number;
}

export type ProvenanceKind = 'public_fact' | 'private_view' | 'model_inference' | string;

export interface ReportHighlight {
  eventId?: string;
  type: string;
  summary: string;
  kind: ProvenanceKind;
}

export interface ReportContent {
  reportDay: string;
  characterId: string;
  highlights: ReportHighlight[];
  relationChanges: ReportHighlight[];
  monologue: { text: string; kind: ProvenanceKind };
  provenanceLegend: Record<string, string>;
}

export interface ReportDetail {
  id: string;
  worldId: string;
  characterId: string;
  content: ReportContent;
  openedAt: number | null;
  createdAt: number;
}

export interface ConsentRequest {
  id: string;
  worldId: string;
  eventKind: string;
  detail: string;
  options: string[];
  status: string;
  mySubjects: string[];
  responded: boolean;
  expiresAt: number;
  createdAt: number;
}

export interface InterventionRecord {
  id: string;
  kind: string;
  characterId: string;
  status: string; // accepted | rejected | applied
  rejectReason?: string | null;
  createdAt: number;
}

/** 「我的世界」条目：从 /me/reports 按世界聚合（已投放角色 + 未读日报角标）。 */
export interface MyWorldEntry {
  worldId: string;
  characterIds: string[];
  unreadCount: number;
  totalReports: number;
  latestReportId?: string;
  latestReportDay?: string;
}

/** 「我的角色 × 世界」权威成员关系（GET /me/memberships）：补日报反推的盲区（刚投放没日报也在场）。 */
export interface Membership {
  worldId: string;
  worldTitle: string;
  roomType: string;
  worldStatus: string;
  stateRevision: number;
  cloudCharacterId: string;
  characterName: string;
  membershipStatus: string; // active | left
  joinedAt: number;
}

/** 跨世界背包物品（GET /me/backpack；镜像 backpack/mod.rs::my_backpack）。 */
export interface BackpackItem {
  backpackId: string;
  status: string; // owned | carried | sealed | consumed
  acquiredWorldId: string;
  carriedWorldId: string | null;
  item: {
    id: string;
    narrative: string;
    effectTags: string[];
    origin: { worldTemplateId: string; cosmology: string[]; powerTier: number };
  };
}

/** 羁绊边（前端派生，非端点）：跨世界聚合各世界 state-summary.relations 中含我角色的有向边。 */
export interface BondEdge {
  worldId: string;
  worldTitle: string;
  myCharacterId: string;
  otherCharacterId: string;
  otherName: string;
  trust: number;
  affinity: number;
  fear: number;
  debt: number;
  direction: 'out' | 'in'; // 我的角色是 from(out) 还是被 known_to(in)
}

// ---------- 赛事房契约镜像（P6，FE1 追加；GET /arena/{id}/report 形态） ----------

export type ArenaPhase = 'lobby' | 'running' | 'concluded' | string;

/** 透明战报的单条事件：只出结果摘要 + 判定依据 ruleRefs，绝不含模型隐藏推理（§9.4）。 */
export interface ArenaRuleEvent {
  sequence: number;
  type: string;
  actors: string[];
  summary: string;
  ruleRefs: string[];
}

/** 环境 / 礼物事件（观众礼物经网关映射为环境通道，不走玩家道具干预）。 */
export interface ArenaEnvEvent {
  appliedTick: number | null;
  kind: string;
  payload: Record<string, unknown>;
  aggregatedCount: number;
}

export interface ArenaRound {
  tick: number;
  events: ArenaRuleEvent[];
  env: ArenaEnvEvent[];
}

export interface ArenaMatchState {
  phase: ArenaPhase;
  alliances: unknown[];
  eliminations: string[];
  winnerCharId: string | null;
}

export interface ArenaReport {
  worldId: string;
  match: ArenaMatchState;
  rounds: ArenaRound[];
  environment: ArenaEnvEvent[];
  compliance?: { arbitrationPublic: boolean; aiGenerated: boolean };
}

// ---------- 回放 / 直播统一事件（GET /arena/{id}/replay + WS 实时流 arena_* 事件） ----------

/** 回放/直播统一事件：public 时间线的一条（含引擎回合事件 + arena_* 系统事件）。 */
export interface ArenaReplayEvent {
  id: string;
  sequence: number;
  tick: number;
  occurredAt: number;
  type: string; // action|dialogue|status|arena_elim|arena_winner|arena_gift|...
  actors: string[];
  summary: string;
  ruleRefs: string[];
  arenaKind?: 'elim' | 'winner' | 'gift' | null;
  characterId?: string | null;
  sku?: string | null;
  aggregatedCount?: number | null;
}

/** GET /arena/{id}/replay 返回：可 seek 的 public 时间线 + 赛制快照 + 时长。 */
export interface ArenaReplay {
  worldId: string;
  match: ArenaMatchState;
  events: ArenaReplayEvent[];
  nextCursor: number | null;
  durationMs: number;
  startedAt: number;
  endedAt: number;
  compliance?: { arbitrationPublic: boolean; aiGenerated: boolean };
}

/** POST /arena/{id}/gift 返回：站内打赏结果 + 付费边界（买过程不买结果）。 */
export interface ArenaGiftResult {
  worldId: string;
  sku: string;
  count: number;
  mapped: boolean;
  boon: unknown;
  envEventId?: string;
  aggregatedCount?: number;
  boundary: { buys: string; notImmunity: boolean; notFinalVerdict: boolean };
}

// ---------- 错误友好化（所有平台页面复用；键在稳定 error code + Conflict 子原因） ----------

/**
 * 把 cloudFetch 抛出的 CloudError / 网络异常转成面向用户的中文提示。
 * server 端 Conflict 统一 code='conflict'，子原因在 message（如「状态冲突: revision」），此处二次识别。
 * 非 CloudError（网络失败 / 非 JSON 响应）一律降级为「连接平台失败」。
 */
export function describeCloudError(err: unknown): string {
  if (err instanceof CloudError) {
    const raw = err.message || '';
    switch (err.code) {
      case 'unauthorized':
        return '登录已过期，请重新登录';
      case 'forbidden':
        return '你没有权限执行此操作';
      case 'risk_blocked':
        return '该操作已被安全风控拦截';
      case 'not_found':
        return '资源不存在或已被移除';
      case 'bad_request':
        return raw.replace(/^请求无效:\s*/, '') || '请求无效';
      case 'idempotency_mismatch':
        return '重复请求但内容不一致，请稍后重试';
      case 'conflict': {
        if (raw.includes('revision')) return '世界状态已更新，请刷新后重试';
        if (raw.includes('world_full')) return '世界人数已满';
        if (raw.includes('character_not_approved')) return '角色尚未通过审核，暂不能投放';
        if (raw.includes('character_withdrawn')) return '角色已撤回，暂不能投放';
        if (raw.includes('world_not_joinable') || raw.includes('world_not_running'))
          return '世界当前不可加入或已停止运行';
        return raw.replace(/^状态冲突:\s*/, '') || '操作冲突，请刷新后重试';
      }
      default:
        return raw || '操作失败，请稍后重试';
    }
  }
  return '连接平台失败，请检查网络或平台地址';
}

/** 判断是否鉴权失效（供页面决定是否引导重新登录）。 */
export function isAuthError(err: unknown): boolean {
  return err instanceof CloudError && err.code === 'unauthorized';
}

// ---------- 展示层小助手（各平台页面复用；集中在平台模块内） ----------

export function roomTypeLabel(rt: string): string {
  switch (rt) {
    case 'idle':
      return '放置房';
    case 'chapter':
      return '章节房';
    case 'arena':
      return '赛事房';
    default:
      return rt;
  }
}

/** 赛事赛制阶段 → 展示标签与色彩（§2.5 唯一胜者赛制）。 */
export function arenaPhaseMeta(phase: string): { label: string; color: string } {
  switch (phase) {
    case 'lobby':
      return { label: '待开赛', color: 'default' };
    case 'running':
      return { label: '进行中', color: 'processing' };
    case 'concluded':
      return { label: '已结束', color: 'success' };
    default:
      return { label: phase, color: 'default' };
  }
}

/** WorldEvent.type → 展示标签与色彩（§9.4 事件类型枚举）。 */
export function eventTypeMeta(type: string): { label: string; color: string } {
  switch (type) {
    case 'action':
      return { label: '行动', color: 'geekblue' };
    case 'dialogue':
      return { label: '对话', color: 'cyan' };
    case 'conflict':
      return { label: '冲突', color: 'red' };
    case 'alliance':
      return { label: '结盟', color: 'green' };
    case 'item':
      return { label: '道具', color: 'gold' };
    case 'status':
      return { label: '状态', color: 'purple' };
    case 'arbiter':
      return { label: '仲裁', color: 'volcano' };
    case 'world':
      return { label: '世界', color: 'magenta' };
    case 'consent_request':
      return { label: '同意请求', color: 'orange' };
    case 'arena_elim':
      return { label: '淘汰', color: 'red' };
    case 'arena_winner':
      return { label: '胜者', color: 'gold' };
    case 'arena_gift':
      return { label: '打赏', color: 'magenta' };
    default:
      return { label: type, color: 'default' };
  }
}

/** 赛事系统事件 type（arena_elim/winner/gift）→ 展示标签与色彩（观战/回放时间线高亮）。 */
export function arenaEventKindMeta(type: string): { label: string; color: string } {
  switch (type) {
    case 'arena_elim':
      return { label: '淘汰', color: 'red' };
    case 'arena_winner':
      return { label: '胜者', color: 'gold' };
    case 'arena_gift':
      return { label: '打赏', color: 'magenta' };
    default:
      return eventTypeMeta(type);
  }
}

/** 云端角色审核态 → 展示标签与色彩。 */
export function moderationMeta(m: string): { label: string; color: string } {
  switch (m) {
    case 'approved':
      return { label: '已通过', color: 'green' };
    case 'pending':
      return { label: '审核中', color: 'gold' };
    case 'rejected':
      return { label: '未通过', color: 'red' };
    case 'quarantined':
      return { label: '已隔离', color: 'volcano' };
    default:
      return { label: m, color: 'default' };
  }
}

/** 日报来源分层（公开事实 / 私密视角 / 模型推断）→ 展示元数据（§2.5 必须明确区分）。 */
export function provenanceMeta(kind: ProvenanceKind): { label: string; color: string; hint: string } {
  switch (kind) {
    case 'public_fact':
      return { label: '公开事实', color: 'blue', hint: '世界内可公开观测的事实' };
    case 'private_view':
      return { label: '角色私密视角', color: 'purple', hint: '仅你（角色主人）可见' };
    case 'model_inference':
      return { label: '模型推断', color: 'orange', hint: '由模型生成的推断，非确定事实' };
    default:
      return { label: kind, color: 'default', hint: '' };
  }
}

// ---------- store ----------

export type RoomView = 'stream' | 'cards' | 'graph' | 'map' | 'status' | 'worldline';

interface PlatformState {
  // 世界大厅
  roomTypeFilter: RoomTypeFilter;
  worlds: WorldSummary[];
  worldsCursor: string | null;
  worldsHasMore: boolean;
  worldsLoading: boolean;
  worldsError: string | null;

  // 我的世界 / 日报列表
  reports: ReportListItem[];
  myWorlds: MyWorldEntry[];
  worldTitles: Record<string, string>;
  reportsLoading: boolean;
  reportsError: string | null;

  // 我的角色 × 世界（权威 memberships；补日报反推盲区）
  memberships: Membership[];
  membershipsLoading: boolean;
  membershipsError: string | null;

  // 跨世界背包
  backpack: BackpackItem[];
  backpackLoading: boolean;
  backpackError: string | null;

  // 房间 L1 视图切换（偏好，持久化）
  roomView: RoomView;

  setRoomTypeFilter: (filter: RoomTypeFilter) => Promise<void>;
  loadWorlds: (reset?: boolean) => Promise<void>;
  loadReports: () => Promise<void>;
  loadMemberships: () => Promise<Membership[]>;
  loadBackpack: () => Promise<void>;
  enrichWorldTitles: (ids: string[]) => Promise<void>;
  setRoomView: (view: RoomView) => void;
  unreadTotal: () => number;
  reset: () => void;
}

const initialListState = {
  worlds: [] as WorldSummary[],
  worldsCursor: null as string | null,
  worldsHasMore: false,
  worldsLoading: false,
  worldsError: null as string | null,
  reports: [] as ReportListItem[],
  myWorlds: [] as MyWorldEntry[],
  worldTitles: {} as Record<string, string>,
  reportsLoading: false,
  reportsError: null as string | null,
  memberships: [] as Membership[],
  membershipsLoading: false,
  membershipsError: null as string | null,
  backpack: [] as BackpackItem[],
  backpackLoading: false,
  backpackError: null as string | null,
};

export const usePlatformStore = create<PlatformState>()(
  persist(
    (set, get) => ({
      roomTypeFilter: 'idle', // P4a 仅放置房；其余房型为未来期权（§2.1 不展示空能力）
      roomView: 'stream',
      ...initialListState,

      setRoomTypeFilter: async (filter) => {
        set({ roomTypeFilter: filter });
        await get().loadWorlds(true);
      },

      loadWorlds: async (reset = true) => {
        const { roomTypeFilter, worldsCursor } = get();
        set({ worldsLoading: true, worldsError: null });
        try {
          const params = new URLSearchParams();
          if (roomTypeFilter !== 'all') params.set('type', roomTypeFilter);
          if (!reset && worldsCursor) params.set('cursor', worldsCursor);
          const qs = params.toString();
          const data = await cloudFetch<{ worlds: WorldSummary[]; nextCursor: string | null }>(
            `/api/worlds${qs ? `?${qs}` : ''}`,
          );
          const incoming = data.worlds ?? [];
          set((s) => ({
            worlds: reset ? incoming : [...s.worlds, ...incoming],
            worldsCursor: data.nextCursor ?? null,
            worldsHasMore: !!data.nextCursor,
            worldsLoading: false,
          }));
        } catch (e) {
          set({ worldsLoading: false, worldsError: describeCloudError(e) });
        }
      },

      loadReports: async () => {
        set({ reportsLoading: true, reportsError: null });
        try {
          const data = await cloudFetch<{ reports: ReportListItem[]; nextCursor: number | null }>(
            '/api/me/reports',
          );
          const reports = data.reports ?? [];
          // 按世界聚合「我的世界」；reports 已按 createdAt DESC，首个即最新。
          const byWorld = new Map<string, MyWorldEntry>();
          for (const r of reports) {
            const entry =
              byWorld.get(r.worldId) ??
              { worldId: r.worldId, characterIds: [], unreadCount: 0, totalReports: 0 };
            if (!entry.characterIds.includes(r.characterId)) entry.characterIds.push(r.characterId);
            entry.totalReports += 1;
            if (!r.opened) entry.unreadCount += 1;
            if (!entry.latestReportId) {
              entry.latestReportId = r.id;
              entry.latestReportDay = r.reportDay;
            }
            byWorld.set(r.worldId, entry);
          }
          const myWorlds = [...byWorld.values()];
          set({ reports, myWorlds, reportsLoading: false });
          // best-effort 补世界标题（失败静默，不影响列表可用）。
          void get().enrichWorldTitles(myWorlds.map((w) => w.worldId));
        } catch (e) {
          set({ reportsLoading: false, reportsError: describeCloudError(e) });
        }
      },

      // 权威「我的角色 × 世界」：直接读 world_members（补日报反推盲区）。返回列表供页面链式聚合（羁绊/档案）。
      loadMemberships: async () => {
        set({ membershipsLoading: true, membershipsError: null });
        try {
          const data = await cloudFetch<{ memberships: Membership[] }>('/api/me/memberships');
          const memberships = data.memberships ?? [];
          // 顺带补世界标题缓存（memberships 自带 worldTitle，避免各页再请求 /worlds/{id}）。
          const titles: Record<string, string> = {};
          for (const m of memberships) if (m.worldTitle) titles[m.worldId] = m.worldTitle;
          set((s) => ({
            memberships,
            worldTitles: { ...titles, ...s.worldTitles },
            membershipsLoading: false,
          }));
          return memberships;
        } catch (e) {
          set({ membershipsLoading: false, membershipsError: describeCloudError(e) });
          return [];
        }
      },

      // 跨世界背包（纯读 /me/backpack；服务端已按 user_id 归属且排除 consumed）。
      loadBackpack: async () => {
        set({ backpackLoading: true, backpackError: null });
        try {
          const data = await cloudFetch<{ items: BackpackItem[] }>('/api/me/backpack');
          set({ backpack: data.items ?? [], backpackLoading: false });
        } catch (e) {
          set({ backpackLoading: false, backpackError: describeCloudError(e) });
        }
      },

      enrichWorldTitles: async (ids) => {
        for (const id of ids) {
          if (get().worldTitles[id]) continue;
          try {
            const d = await cloudFetch<WorldDetail>(`/api/worlds/${id}`);
            set((s) => ({ worldTitles: { ...s.worldTitles, [id]: d.title } }));
          } catch {
            // best-effort：标题缺失时页面回退展示 worldId
          }
        }
      },

      setRoomView: (view) => set({ roomView: view }),

      unreadTotal: () => get().reports.reduce((n, r) => (r.opened ? n : n + 1), 0),

      reset: () => set({ ...initialListState }),
    }),
    {
      name: 'museai-platform-ui',
      version: 1,
      storage: createJSONStorage(() => localStorage),
      // 仅持久化 UI 偏好，不缓存云端列表（云端为权威，每次进入重新拉取）。
      partialize: (state) => ({ roomTypeFilter: state.roomTypeFilter, roomView: state.roomView }) as PlatformState,
      migrate: (persisted) => persisted as PlatformState,
    },
  ),
);
