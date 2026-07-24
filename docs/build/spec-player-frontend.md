Research complete. Here is the implementation spec.

---

# 实现规格：玩家端前端体验（留存核心）

## 概述

目标闭环：**投卡进世界 → 看我的角色世界线 → 干预(托梦/道具) → 读《昨日人生》日报 → 看跨世界背包/羁绊/角色一生档案**。

调研结论（关键）：**后端玩家端 API 已基本齐全，前端已建成约 60% 骨架**。已有 `PlatformShell` + 大厅/发布/我的世界/`WorldRoom`（事件流+干预+同意+投放+关系图谱/势力地图/状态面板）/日报。**真正缺失的留存件是三块**：

1. **「我的角色在各世界」**——当前 `MyWorlds` 用日报反推投放世界（`src/pages/platform/MyWorlds.tsx:2`），刚投放但还没日报的角色/世界不显示，且以「世界」为轴而非以「角色」为轴。缺一个以角色为中心的视图 + 一个权威 memberships 端点。
2. **「我的角色世界线」时间线**——`WorldRoom` 只有全世界事件流，没有「只看我这个角色的故事线」（按角色过滤 + 叙事化阅读态）。这是情感留存的核心（"看得到自己角色的故事"）。
3. **跨世界背包 / 羁绊 / 角色一生档案**——`GET /me/backpack` 后端已实现（`server/src/backpack/mod.rs:150`）但**前端零页面**；羁绊、角色一生档案**前后端都没有**，需前端聚合现有端点装配。

本规格：1 个小后端新增（`GET /me/memberships`）+ 5 个前端页面/视图 + store 扩展 + 1 处 WS resume cursor 修复。**其余全部复用现有 API 与 `WorldRoom` 导出的组件**。

---

## 复用与新增

### 直接复用（不改）
- `cloudFetch` / `cloudStream`（`src/utils/cloudApi.ts:43,75`）——所有云端读写与 WS。
- `WorldRoom` 导出件：`useWorldEvents`（`src/pages/platform/WorldRoom.tsx:65`）、`useWorldStateSummary`（:126）、`WorldViewPanel`（:698）、`EventStream/EventCards/RelationGraph/FactionMap/StatusPanel`、`WorldHeader`（:740）、`InterventionPanel`（内部，:866）。
- `usePlatformStore` 的展示助手 `eventTypeMeta`/`provenanceMeta`/`roomTypeLabel`/`moderationMeta`/`describeCloudError`（`src/stores/usePlatformStore.ts:224–344`）。
- `useAuthStore`（`src/stores/useAuthStore.ts`）、`usePartnerStore.characterCardsV2`（本地卡名解析，`WorldRoom.tsx:1143`）。
- 已有端点：`/assets/characters/mine`、`/worlds/{id}`、`/worlds/{id}/events`、`/worlds/{id}/state-summary`、`/worlds/{id}/interventions(/mine)`、`/me/reports(/{id})`、`/me/backpack`、`/me/consents`。

### 新增
- **后端 1 个**：`GET /me/memberships`（`server/src/backpack/mod.rs` 或新 `server/src/memberships/mod.rs`）——权威列出「我的角色 × 世界」，补日报反推的盲区。
- **前端页面 4 个**：`MyCharacters`（我的角色 · 各世界）、`Backpack`（跨世界背包）、`Bonds`（羁绊）、`CharacterArchive`（角色一生档案）。
- **前端视图 1 个**：`WorldRoom` 内新增 `worldline`（世界线）L1 视图 + 角色选择器（按角色过滤事件）。
- **store 扩展**：`usePlatformStore` 增 memberships/backpack 状态与 action。
- **修复 1 处**：`cloudStream` resume cursor 用 `sequence`（见「与现有交互」）。

---

## 数据结构（字段 + 位置）

### 后端新增端点响应（`GET /me/memberships`）
```jsonc
// 位置：server/src/backpack(或 memberships)/mod.rs::my_memberships
{ "memberships": [
  { "worldId":"wld_..", "worldTitle":"..", "roomType":"idle",
    "worldStatus":"running", "stateRevision": 7,
    "cloudCharacterId":"cc_..", "characterName":"..",  // 从 card_json.identity.name
    "membershipStatus":"active",                        // active|left
    "joinedAt": 1730000000000 } ] }
```
> 复用 `worlds/mod.rs::world_detail` 里 `card_json → identity.name` 的解析法（`server/src/worlds/mod.rs:207`）。

### 前端 store 契约镜像（追加到 `src/stores/usePlatformStore.ts`，紧邻 :164 `MyWorldEntry`）
```ts
export interface Membership {           // GET /me/memberships
  worldId: string; worldTitle: string; roomType: string; worldStatus: string;
  stateRevision: number; cloudCharacterId: string; characterName: string;
  membershipStatus: string; joinedAt: number;
}
export interface BackpackItem {          // GET /me/backpack（镜像 backpack/mod.rs:165 my_backpack）
  backpackId: string; status: string;    // owned|carried|sealed|consumed
  acquiredWorldId: string; carriedWorldId: string | null;
  item: { id: string; narrative: string; effectTags: string[];
    origin: { worldTemplateId: string; cosmology: string[]; powerTier: number } };
}
export interface BondEdge {              // 前端派生（非端点）：跨世界聚合 state-summary.relations
  worldId: string; worldTitle: string;
  myCharacterId: string; otherCharacterId: string; otherName: string;
  trust: number; affinity: number; fear: number; debt: number;
  direction: 'out' | 'in';               // 我的角色是 from(out) 还是被 known_to(in)
}
// CharacterArchive 用现有 Membership/ReportListItem/BackpackItem/BondEdge 组合，不新增类型
```
> 复用已存在的 `WorldRelation`（:75）、`WorldCharacterState`（:85）、`ReportListItem`（:107）、`WorldEventItem`（:57）。

---

## 改动文件清单（逐 file：改什么）

**后端**
- `server/src/backpack/mod.rs`（或新建 `server/src/memberships/mod.rs` + `app.rs:47` 挂载）：新增 `my_memberships` handler + `.route("/me/memberships", get(...))`（backpack `router()` 在 :328）。查询 `world_members JOIN worlds JOIN cloud_characters WHERE world_members.user_id=? AND status='active'`。
- `server/src/app.rs`：若走新 crate 模块，`build_router` 里 `.merge(crate::memberships::router())`（:47 附近）；复用 backpack 模块则无需改。

**前端 — 路由与导航**
- `src/App.tsx:106–120`：新增 5 条路由：`characters`（MyCharacters）、`characters/:cid`（CharacterArchive）、`backpack`（Backpack）、`bonds`（Bonds）；`worlds/:id` 支持 query `?character=cc_..` 打开世界线视图（WorldRoom 内部读 `useSearchParams`，不新增路由）。全部包 `<RequireAuth>`。
- `src/pages/platform/PlatformShell.tsx:24–31 NAV_ITEMS`：`我的世界` 后插入 `我的角色`(`/platform/characters`)、`背包`(`/platform/backpack`)、`羁绊`(`/platform/bonds`)；`activeNavKey`（:34）自动适配最长前缀。

**前端 — store**
- `src/stores/usePlatformStore.ts`：追加 `Membership/BackpackItem/BondEdge` 类型（:164 后）；`PlatformState` 增 `memberships/backpack/backpackLoading/loadMemberships()/loadBackpack()`（:350 接口 + :391 实现，仿 `loadReports` :426）；`partialize`（:480）不缓存这些云端列表。

**前端 — 新页面**
- `src/pages/platform/MyCharacters.tsx`（新）：以角色为轴，`loadMemberships()` 按 `cloudCharacterId` 分组 → 每角色列出所在世界 + 未读日报角标（合并 `myWorlds` 的 unread）。
- `src/pages/platform/Backpack.tsx`（新）：`loadBackpack()` → 按 `status` 分组（在库/随身/封印/已消耗）的物品卡（narrative + origin.powerTier + effectTags + 来源世界）。
- `src/pages/platform/Bonds.tsx`（新）：`loadMemberships()` → 对每个世界并发 `GET /worlds/{id}/state-summary` → 过滤含我角色的关系边 → 跨世界聚合成 `BondEdge[]`，按 |affinity| 排序展示羁绊强度条。
- `src/pages/platform/CharacterArchive.tsx`（新，route `characters/:cid`）：单角色一生档案——头部身份卡（本地卡 `identity` + `moderation`）+ 该角色所在世界时间线（memberships）+ 日报流（`/me/reports` 过滤 characterId）+ 该角色带来的物品 + 该角色的羁绊。

**前端 — WorldRoom 世界线视图**
- `src/pages/platform/WorldRoom.tsx`：
  - `L1_OPTIONS`（:690）增 `{ label:'世界线', value:'worldline' }`；`RoomView` 类型（`usePlatformStore.ts:348`）加 `'worldline'`。
  - 新组件 `CharacterWorldline`（filter `events` 到 `ev.actors.includes(selectedCharId)`，按 `sequence` 升序叙事化渲染，复用 `EventStream` 的卡片样式 + 标注「我的角色」「仅你可见」）。
  - `WorldViewPanel`（:698）增 `myChars`/`selectedCharId`/`onSelectChar` props；view==='worldline' 时渲染角色选择器 + `CharacterWorldline`。
  - 页面主体（:1233）把 `myChars`（:1185）与选中角色透传给 `WorldViewPanel`；支持 URL `?character=` 预选。

---

## 核心逻辑（伪码，带 file:line）

### 1. 后端 memberships（`server/src/backpack/mod.rs` 新 handler）
```rust
// 仿 consents/mod.rs:120 的 world_members WHERE user_id 查询 + worlds/mod.rs:207 名字解析
async fn my_memberships(State(state), user: AuthUser) -> Result<Json<Value>> {
  let rows = sqlx::query(
    "SELECT wm.cloud_character_id, wm.status, wm.joined_at,
            w.id, w.title, w.room_type, w.status AS wstatus, w.state_revision,
            cc.card_json
     FROM world_members wm
     JOIN worlds w ON w.id = wm.world_id
     JOIN cloud_characters cc ON cc.id = wm.cloud_character_id
     WHERE wm.user_id = ? AND wm.status = 'active'
     ORDER BY wm.joined_at DESC")
    .bind(&user.user_id).fetch_all(&state.db).await?;
  // name = card_json.identity.name（unwrap_or worldId 兜底，同 worlds/mod.rs:207）
  Ok(Json(json!({ "memberships": rows.map(project) })))
}
```
> 权威性：直接读 `world_members`，无 owner 泄漏（WHERE user_id=本人）。补齐「刚投放没日报」的盲区。

### 2. 世界线过滤（`WorldRoom.tsx` 新 `CharacterWorldline`）
```tsx
// 复用 useWorldEvents(:65) 已取的全量投影事件；纯前端过滤，无新请求
const mine = events.filter(ev => ev.actors.includes(selectedCharId))
                   .sort((a,b) => a.sequence - b.sequence);
// 渲染：Timeline，每条标 tick、visibility!=public→「仅你可见」、projection.summary
// 空态："TA 还没在这个世界留下故事，等待下一个节拍"
```

### 3. 羁绊跨世界聚合（`Bonds.tsx`）
```tsx
const ms = await loadMemberships();                 // 我的角色×世界
const myByWorld = groupBy(ms, 'worldId');           // worldId → 我的 charIds
const bonds: BondEdge[] = [];
await Promise.all(uniqueWorldIds.map(async wid => {
  const s = await cloudFetch<WorldStateSummary>(`/api/worlds/${wid}/state-summary`); // :142 已有 hook 同源
  for (const rel of s.relations) {                  // 服务端已按 principal 过滤(events/mod.rs:502)
    const mineSet = myByWorld[wid];
    if (mineSet.has(rel.from)) bonds.push({ ...map(rel), direction:'out', myCharacterId: rel.from, otherCharacterId: rel.to });
    else if (mineSet.has(rel.to)) bonds.push({ ...map(rel), direction:'in',  myCharacterId: rel.to,  otherCharacterId: rel.from });
  }
}));
bonds.sort((a,b) => Math.abs(b.affinity) - Math.abs(a.affinity));
```
> 复用后端已做的 principal 隔离（`server/src/events/mod.rs:502` 只返回 `from==我 或 known_to 含我` 的边），前端只做展示层聚合。

### 4. 角色一生档案（`CharacterArchive.tsx`）
```tsx
// 并发 fan-out 已有端点，纯组合，无新后端
const [ms, reports, backpack, chars] = await Promise.all([
  loadMemberships(), cloudFetch('/api/me/reports'),
  cloudFetch('/api/me/backpack'), cloudFetch('/api/assets/characters/mine')]);
const worldsOfChar = ms.filter(m => m.cloudCharacterId === cid);       // 一生走过的世界
const reportsOfChar = reports.filter(r => r.characterId === cid);      // 逐日人生
const itemsOfChar   = /* backpack 无 characterId 字段 → 见「已知取舍」 */;
// 头部：本地卡 identity（usePartnerStore）+ cloud moderation（chars.find(cid)）
```

---

## 与现有交互

- **WS resume cursor 修复（必要）**：`src/utils/cloudApi.ts:95` 现为 `lastEventId = payload.id`（字符串 `we_..`），但服务端 `StreamQuery.last_event_id` 是 `i64`，比对 `sequence`（`server/src/events/mod.rs:388,419`）。字符串无法解析 → serde None → 补偿从 -1 全量重放（靠 `upsertEvent` 去重救了正确性，但浪费）。**改为 `lastEventId = String(payload.sequence)`**，query 键改 `sequence`（或保持 `lastEventId` 但传 sequence 值）。世界线视图依赖增量流，值得一并修。
- **`stateRevision` 供干预**：`world_detail`（`server/src/worlds/mod.rs:226`）已返回 `stateRevision`，`WorldDetail.stateRevision`（`usePlatformStore.ts:53`）已镜像，干预面板 CAS 已用（`WorldRoom.tsx:1162,922`）。新 memberships 端点亦返回 `stateRevision`，`MyCharacters` 直达世界时可预填，减一次请求。
- **导航一致性**：`MyWorlds` 保留（以世界为轴）；`MyCharacters` 新增（以角色为轴）二者互补，均从 `/platform` 大厅（`PlatformHall`）与投放（`WorldRoom` JoinPanel :785）汇入。
- **离场按钮补全**：`POST /worlds/{id}/leave`（`server/src/worlds/mod.rs:372`）后端已实现但前端未接线——`MyCharacters` 每个「角色×世界」行加「离场」按钮（idempotent 非必需，leave 天然幂等）。
- **道具干预现状**：`POST interventions kind=item` 后端 P5 未接线，合法持有仍返回 `BadRequest("道具干预暂未开放")`（`server/src/interventions/mod.rs:154`）。`Backpack` 页与干预面板道具 tab 须显式提示「跨世界携带经入场 carry 生效，主动投放道具后续开放」，避免误导。

## 已知取舍
- **backpack 无 characterId**：`backpacks` 表按 `user_id` 归属，非按角色（`server/src/backpack/mod.rs:152`）。故「角色一生档案」的「TA带来的物品」只能按 `acquiredWorldId ∈ 该角色所在世界` 近似归因，或档案页物品区退化为「账号背包」并注明。规格采用「按获得世界近似 + 注明」，不新增后端字段。
- **羁绊是每世界快照聚合**，非历史时间序列（state-summary 只给当前值），展示为「当前羁绊强度」，不做趋势线。

---

## 测试与验证

沿用现有测试基建（`src/__tests__/platform-*.test.tsx`，已 mock `cloudFetch`/`invoke`，见 `platform-world-room.test.tsx`）：
- `platform-memberships.test.tsx`（新）：mock `/me/memberships` → `MyCharacters` 按角色分组、离场按钮调 `/leave`、空态引导投放。
- `platform-backpack.test.tsx`（新）：mock `/me/backpack` → 分组渲染、origin.powerTier/effectTags 展示、carried/sealed 状态标签。
- `platform-bonds.test.tsx`（新）：mock memberships + 多世界 state-summary → 只显含我角色的边、direction 判定、按 affinity 排序。
- `platform-character-archive.test.tsx`（新）：fan-out 组合、按 characterId 过滤日报、moderation 标签。
- `platform-world-room.test.tsx`（扩展）：worldline 视图按角色过滤事件、`?character=` 预选、空态。
- `platform-store.test.ts`（扩展）：`loadMemberships`/`loadBackpack` 成功/失败（`describeCloudError`）。
- 后端 `server/src/backpack/mod.rs` 内联 `#[cfg(test)]`：memberships owner 隔离（他人角色不出现）、只返 active、name 解析兜底。仿 `interventions/tests`（:268）。
- CI 门：`npm run test` + `npm run build`(tsc) + `cargo test`（CLAUDE.md 约定）。手动验：起 server(`8787`) + `npm run tauri dev` → 登录 → 投卡 → 进世界看世界线 → 托梦 → 次日日报 → 背包/羁绊/档案。

---

## 依赖（其他两块）

- **引擎 / runtime 块**：世界线与日报的「内容」全靠 runtime 产出——`world_events` 投影落库（`server/src/events/mod.rs:197`）、`narrative_state_json` 填充驱动 `state-summary` 的 relations/characters（否则 :500 优雅退化空快照，羁绊/图谱为空）、`generate_report` 每日边界生成日报（`server/src/reports/mod.rs:22`）、`grant_item` 通关入包（`backpack/mod.rs:136`）、`ConsentRequested` 触发同意（`consents/mod.rs:26`）。**前端在这些为空时全部优雅降级（已具备），但留存体感依赖 runtime 真跑起来**。
- **供给 / admin 块**：大厅要有 `open/running` 且 `official/public` 的世界（`worlds/mod.rs:121`）；角色须 `moderation='approved'` 才能投放（`worlds/mod.rs:283`）——审核由 admin/safety 块驱动。无世界=无入口，无审核通过=无法投卡。
- 对本块**无阻塞**：所有新页面在依赖未就绪时显示空态/降级，可独立开发联调（mock 数据即可）。

---

## 渐进式分步落地

1. **P0 世界线视图**（最高留存杠杆，纯前端）：`WorldRoom` 加 `worldline` L1 + 角色选择器 + `?character=` 深链。改 1 文件，复用已取事件。**先上这个**。
2. **P0 WS cursor 修复**：`cloudApi.ts:95` 用 sequence。1 行。
3. **P1 memberships 端点 + MyCharacters 页**：后端 1 handler + 前端 1 页 + store 1 action + 导航 + 离场按钮。补「刚投放不显示」盲区。
4. **P1 跨世界背包页**：`Backpack.tsx` + store，纯读 `/me/backpack`。
5. **P2 羁绊页**：`Bonds.tsx`，聚合 state-summary。
6. **P2 角色一生档案页**：`CharacterArchive.tsx`，fan-out 组合（依赖 3/4/5 的 store）。
7. **测试补齐 + CI 绿**。

每步可独立合并、独立上线（route + nav 增量），互不阻塞。

---

## 工作量与影响面估计

| 项 | 规模 | 影响面 |
|---|---|---|
| 后端 `/me/memberships` | ~60 行 + 内联测试 | 新增端点，零回归（不改现有表/路由语义） |
| WS cursor 修复 | ~2 行 | `cloudApi.ts`，涉及所有世界流；行为更优，`upsertEvent` 已兜正确性 |
| store 扩展 | ~80 行 | `usePlatformStore.ts` 追加，`partialize` 保持不缓存 |
| WorldRoom 世界线视图 | ~120 行 | 单文件内新增，L1 现有视图不受影响 |
| MyCharacters | ~150 行 | 新页面 + nav + 路由 |
| Backpack | ~130 行 | 新页面 |
| Bonds | ~140 行 | 新页面（N 个 state-summary 并发，需限并发/loading 态） |
| CharacterArchive | ~200 行 | 新页面（组合 4 端点，注意 fan-out 错误隔离） |
| 前端测试 5 文件 | ~400 行 | 独立 |

**总量**：约 1 个后端小改 + 6 个前端文件新增/扩展，~1400 行含测试。**风险点**：Bonds 的多世界并发（限 6 并发 + 单世界失败静默，仿 `enrichWorldTitles` :457）、backpack 无 characterId 的归因取舍（已定方案）、道具干预未接线的文案防误导。**回归风险低**：现有页面与端点语义不改，新功能全部增量挂载 + 空态降级。
