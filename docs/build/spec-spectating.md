I now have a complete, code-grounded picture. Note the task premise ("无实时观战流/回放/观战前端") is partly outdated — a WS stream, a `cloudStream` client, and `WorldSpectate`/`ArenaSpectate` pages already exist. The real gaps are: arena elimination/winner never hit the live stream, `ArenaSpectate` is a poll-once snapshot with no live updates and no in-app 打赏入口, the reconnect-compensation param is broken, and there's no seekable replay. Here is the spec.

---

# 赛事房观战直播 + 回放 — 实现规格

## 概述

商业核心是「观众实时看比赛 → 打赏」。调研后发现**基础设施已大部分就位，但赛事房这条主线没接通**：

- **实时观战流已存在**：`server/src/events/mod.rs:391` 的 `WS /worlds/{id}/stream` 已做双硬隔离（推送层 `ws_visible` events/mod.rs:63、查询层 `row_to_event` Rust 精确复核 events/mod.rs:300），观战资格 `can_view_world`(events/mod.rs:272) 对 `official/public` 世界放行。前端 `cloudStream`(cloudApi.ts:76) + `useWorldEvents`(WorldRoom.tsx:65) + `WorldSpectate.tsx` 已能实时看引擎回合的 public 投影。
- **真正的缺口有四个**：
  1. **赛制事件不进流**：`eliminate`(arena/mod.rs:366)、`settle`/`recompute_winner`(arena/mod.rs:446/531) 只改 `arena_matches`，**从不 `ws_hub.publish` 也不写 world_events**。观众在流里看不到淘汰/胜者落定,只能靠轮询 report → 直播最关键的「淘汰高潮」是哑的。
  2. **ArenaSpectate 无实时**：`ArenaSpectate.tsx:68` 只 `loadReport()` 一次 + 手动「刷新」按钮，无 stream 订阅、无 gift 入口。「边看边打赏」的商业闭环缺前端承载。
  3. **无 in-app 打赏入口**：`livegate` webhook(livegate/mod.rs:52) 是**外部直播平台**回调；站内观众没有走系统频道的打赏按钮。
  4. **无回放**：`world_events` 全量落库（0001_init.sql:150，`sequence` 单调 + `occurred_at`），**数据足以回放但没有回放端点/播放器**；`GET /events`(events/mod.rs:346) 是 principal 过滤的分页拉取，不是 public 时间线重建。

本规格**务实复用** `events/stream` + `livegate` + `arena/report`，最小新增：一个「赛事系统频道」（public-only world_event，双隔离天然满足）+ 一个回放端点（从 world_events 重建 public 时间线）+ 站内打赏端点（复用 `upsert_gift_boon`）+ ArenaSpectate 改造为实时 + 新增回放页。**红线**：打赏只写 `arena_env_events`/`gift_events` 系统频道，永不触碰 `eliminations_json`/`winner_char_id`/`interventions`。

## 复用与新增

**复用（不改结构）**
- `events::WsHub` + `WsMessage`(events/mod.rs:37-60) — 每世界广播通道；赛事系统事件走同一通道。
- `events::ws_visible`/`row_to_event`(events/mod.rs:63/300) — 双硬隔离，赛事系统事件设 `audience=None`(public) 即被所有观众可见、不泄任何私密。
- `events::can_view_world`(events/mod.rs:272) — 观战资格判定，回放/打赏端点直接调用。
- `events::insert_events_tx` 的落库模式(events/mod.rs:197) + `build_payload`(events/mod.rs:172) — 抽出复用给系统事件。
- `runtime` 的「提交后广播」模式(runtime/mod.rs:1631-1640) — 复制到 arena。
- `livegate::upsert_gift_boon`/`record_gift`(livegate/mod.rs:132/185) — 站内打赏复用同一聚合+记账路径。
- 前端 `cloudStream`(cloudApi.ts:76) + `useWorldEvents`(WorldRoom.tsx:65) + `eventTypeMeta`(usePlatformStore.ts:291)。

**新增**
- Server: `arena::emit_arena_event`（赛事系统频道 helper）、`GET /arena/{id}/replay`、`POST /arena/{id}/gift`（站内打赏）。
- `events` 抽公共 helper `persist_and_broadcast_public_event`（供 arena 复用）。
- Migration `0012_arena_spectate.sql`（gift_events 增列 `via`，可选回放游标索引）。
- 前端: `useArenaLive` hook、`ArenaReplay.tsx` 新页、`ArenaSpectate.tsx` 改造为实时 + 打赏入口、`cloudApi.ts` 修复 reconnect 参数 bug、store 增类型。

## 数据结构（字段+位置）

**1. 复用 `world_events` 承载赛事系统事件**（0001_init.sql:150）— 无新表。系统事件 = 一行 public world_event：
- `event_type`：新增枚举值 `arena_elim` / `arena_winner` / `arena_gift`（与引擎 `action/dialogue/status/...` 并列，纯展示层字符串，无 schema 迁移）。
- `visibility='public'`、`audience_json=NULL`、`private_projections_json=NULL` → 天然满足双隔离。
- `public_projection_json`：`{"summary": "...", "arenaKind":"elim|winner|gift", "characterId":"..", "sku"?:"..", "aggregatedCount"?:N}`。
- `domain_event_id`：合成 `sys_<uuid>`（非引擎事件，标识系统来源）。
- `tick_no`：取当前 `arena_matches` 关联的最新 tick（无则 0）。`sequence` 由落库分配（复用 events/mod.rs:203 的 `MAX(sequence)+1`）。

**2. `gift_events` 增列 `via`**（0008_gift_clips.sql）— 区分打赏来源，供分成/审计：
```sql
ALTER TABLE gift_events ADD COLUMN via TEXT NOT NULL DEFAULT 'livegate'; -- 'livegate' | 'in_app'
```

**3. 前端 store 新类型**（usePlatformStore.ts，接在 ArenaReport 后 :213）
```ts
export interface ArenaReplayEvent {          // 回放/直播统一事件
  id: string; sequence: number; tick: number; occurredAt: number;
  type: string;                              // action|dialogue|status|arena_elim|arena_winner|arena_gift|...
  actors: string[]; summary: string; ruleRefs: string[];
  arenaKind?: 'elim' | 'winner' | 'gift'; characterId?: string;
  sku?: string; aggregatedCount?: number;
}
export interface ArenaReplay {
  worldId: string; match: ArenaMatchState;
  events: ArenaReplayEvent[]; nextCursor: number | null;
  durationMs: number; startedAt: number; endedAt: number;
}
export interface ArenaGiftResult {           // POST /arena/{id}/gift 返回
  worldId: string; sku: string; count: number; mapped: boolean;
  boon: unknown; envEventId?: string; aggregatedCount?: number;
  boundary: { buys: 'process_boon'; notImmunity: true; notFinalVerdict: true };
}
```

## 改动文件清单（逐 file: 改什么）

**Server (Rust)**
- `server/migrations/0012_arena_spectate.sql` **[新增]**：`gift_events` 加 `via` 列；`CREATE INDEX idx_world_events_replay ON world_events(world_id, sequence)`（已有 idx_world_events_world 覆盖，若重复则省略；仅当需按 occurred_at 二级排序才加）。
- `server/src/events/mod.rs` **[改]**：抽出 `pub async fn persist_and_broadcast_public_event(state, world_id, event_type, summary, actors, extra: Value) -> Result<StoredEvent>`（单事务分配 sequence + 落 public 行 + `ws_hub.publish`）。复用现有 `build_payload` 逻辑，`extra` 合并进 projection。
- `server/src/arena/mod.rs` **[改]**：
  - `settle_consented_eliminations`(arena/mod.rs:466)：每落定一个 `approved` 淘汰后调 `emit_arena_event(elim)`。
  - `recompute_winner`(arena/mod.rs:531)：收敛出 winner 后调 `emit_arena_event(winner)`。
  - 新增 `GET /arena/{world_id}/replay`(handler `get_replay`) + 路由(arena/mod.rs:43)。
  - 新增 `emit_arena_event` 私有 helper（包 `events::persist_and_broadcast_public_event`）。
- `server/src/livegate/mod.rs` **[改]**：
  - 抽 `pub async fn apply_gift(state, world_id, sku, count, from_user, via) -> Result<Value>`（现 `webhook` 体内 SKU→boon→upsert→record 逻辑 livegate/mod.rs:83-124 抽出），webhook 调它 `via="livegate"`。
  - 命中映射后**追加** `arena::emit_arena_event(gift)` → 打赏进流。
  - 新增 `POST /arena/{world_id}/gift`(handler `spectator_gift`，AuthUser + can_view_world 守卫) → 调 `apply_gift(via="in_app")` + billing seam。路由(livegate/mod.rs:29)。
- `server/src/arena/tests.rs` / `server/src/livegate/tests.rs` **[改]**：新增测试（见测试节）。

**Frontend (TS/React)**
- `src/utils/cloudApi.ts` **[改]**：修 `cloudStream` reconnect bug —— `lastEventId` 参数改名 `last_event_id`、值用 `payload.sequence`(number) 而非 `payload.id`(string)（cloudApi.ts:88/91）。
- `src/stores/usePlatformStore.ts` **[改]**：加上述类型 + `arenaEventKindMeta()` 映射（elim/winner/gift → 中文标签+色）。
- `src/pages/platform/ArenaSpectate.tsx` **[改]**：接 `cloudStream` 实时合并淘汰/胜者/打赏事件；淘汰事件弹 toast；新增「打赏入口」按钮组（SKU 快捷键）调 `POST /arena/{id}/gift`；顶部加「回放」入口（concluded 时）。
- `src/pages/platform/ArenaReplay.tsx` **[新增]**：回放页（虚拟时钟 + 播放/暂停/倍速/进度条，从 `/replay` 分页拉取增量重建）。
- `src/App.tsx` **[改]**：加路由 `arena/:worldId/replay`(App.tsx:119 后)。
- `src/pages/platform/WorldRoom.tsx` **[可选改]**：`eventTypeMeta` 补 arena_* 三类（否则回退默认色，非阻塞）。

## 核心逻辑（伪码，带 file:line）

**① events: 公共系统事件 helper**（新增于 events/mod.rs，紧邻 `persist_events` :257）
```rust
// 落一行 public world_event 并广播；audience=None → 双隔离天然满足（仅 public 投影，无私密）。
pub async fn persist_and_broadcast_public_event(
    state: &AppState, world_id: &str, tick_no: i64,
    event_type: &str, summary: &str, actors: &[String], extra: Value,
) -> Result<(), ApiError> {
    let mut tx = state.db.begin().await?;
    let base: i64 = /* SELECT COALESCE(MAX(sequence),-1) ... 复用 events/mod.rs:203 */;
    let seq = base + 1; let id = new_id("we"); let now = now_ms();
    let mut proj = json!({ "summary": summary });
    merge(&mut proj, extra);                       // arenaKind/characterId/sku...
    sqlx::query("INSERT INTO world_events (... visibility, audience_json, \
        public_projection_json, private_projections_json ...) \
        VALUES (?,?, 'public', NULL, ?, NULL, ...)")
        .bind(&id)...bind(proj.to_string())...execute(&mut *tx).await?;
    tx.commit().await?;
    let payload = json!({ "id": id, "worldId": world_id, "tick": tick_no, "sequence": seq,
        "type": event_type, "actors": actors, "visibility": "public",
        "projection": proj, "aiLabel": {"visible": true}, "occurredAt": now });
    state.ws_hub.publish(WsMessage {           // 复用 runtime/mod.rs:1635 模式
        world_id: world_id.into(), audience_user_ids: None, payload_json: payload.to_string() });
    Ok(())
}
```

**② arena: 淘汰/胜者进流**（arena/mod.rs:486 与 :548）
```rust
// settle_consented_eliminations 内，approved 分支 arena/mod.rs:486 之后：
add_elimination(...).await?; mark_elim(..., "eliminated").await?;
emit_arena_event(state, world_id, "arena_elim",
    &format!("角色 {cid} 已淘汰（当事人同意，不可逆）"), &[cid.clone()],
    json!({ "arenaKind":"elim", "characterId": cid })).await;   // 失败不回滚落定

// recompute_winner 内，grant_champion_reward 之后 arena/mod.rs:552：
emit_arena_event(state, world_id, "arena_winner",
    &format!("唯一胜者：{winner}（荣誉性奖励，非强度）"), &[winner.clone()],
    json!({ "arenaKind":"winner", "characterId": winner })).await;
```
`emit_arena_event` = 薄封装 `events::persist_and_broadcast_public_event`，tick_no 取 `SELECT MAX(tick_no) FROM world_ticks WHERE world_id=?`（无则 0）。**红线保持**：淘汰落定/胜者仍只由 consent-gated `settle` 决定，事件仅为**事后广播**，不参与仲裁。

**③ livegate: 打赏进流 + 站内入口**（livegate/mod.rs:96 后 & 新 handler）
```rust
pub async fn apply_gift(state, world_id, sku, count, from_user, via) -> Result<Value> {
    // = 现 webhook 体 livegate/mod.rs:83-124 抽出：查 gift_sku_map → upsert_gift_boon → record_gift(via)
    if let Some(env_id) = mapped {
        emit_arena_event(state, world_id, "arena_gift",
            &format!("观众打赏「{label}」×{count} 已注入场内环境（系统代投）"), &[],
            json!({ "arenaKind":"gift", "sku": sku, "aggregatedCount": agg })).await;
    }
    Ok(resp)  // 含 boundary{buys:"process_boon", notImmunity, notFinalVerdict}
}

// POST /arena/{world_id}/gift（AuthUser） — 站内观众打赏
async fn spectator_gift(state, user, Path(world_id), headers, Json(body)) -> ... {
    if !events::can_view_world(&state.db, &world_id, &user.user_id).await? { return Forbidden; }
    let guard = idempotency::guard(... "arena.gift" ...);      // 复用 livegate/mod.rs:68 幂等模式
    // seam(诚实标注): billing::charge(user, gift_sku) 跨 feature，本期 TODO
    let resp = apply_gift(state, &world_id, &body.sku, body.count,
                          Some(&user.user_id), "in_app").await?;
    guard.store_response(...); Ok(Json(resp))
}
```
**红线**：`apply_gift` 只写 `arena_env_events`(kind=gift_boon) + `gift_events`，`emit_arena_event` 只写 public world_event；**绝不** touch `arena_matches.eliminations_json/winner_char_id` 或 `interventions`（HC 已禁玩家 item 干预，livegate/mod.rs:10 铁律）。SKU 映射表(0008)已约束 boon 仅 `advantage/reroll/info`，无免死/最终判定。

**④ arena: 回放端点**（新 handler，路由 arena/mod.rs:47）
```rust
// GET /arena/{world_id}/replay?cursor=&limit=  — 从 world_events 重建 public 时间线（含 arena_* 系统事件）
async fn get_replay(state, user, Path(world_id), Query(q)) -> Result<Json<Value>> {
    if !events::can_view_world(&state.db, &world_id, &user.user_id).await? { return Forbidden; }
    let m = load_match(&state.db, &world_id).await?;                 // 复用 arena/mod.rs:99
    let cursor = q.cursor.unwrap_or(-1); let limit = q.limit.unwrap_or(200).clamp(1,500);
    // 只取 public（回放=可公开验证日志，与 report 同口径 arena/mod.rs:193）；按 sequence 升序，可分页 seek。
    let rows = sqlx::query("SELECT id,tick_no,sequence,event_type,actors_json,\
        public_projection_json,arbiter_note,occurred_at FROM world_events \
        WHERE world_id=? AND visibility='public' AND sequence>? ORDER BY sequence ASC LIMIT ?")
        .bind(&world_id).bind(cursor).bind(limit).fetch_all(&state.db).await?;
    let events = rows.map(|r| json!({ "id":.., "sequence":.., "tick":.., "occurredAt":..,
        "type": event_type, "actors":.., "summary": proj.summary,
        "ruleRefs": extract_rule_refs(&proj, arbiter_note),          // 复用 arena/mod.rs:274
        "arenaKind": proj.get("arenaKind"), "characterId": proj.get("characterId"),
        "sku": proj.get("sku"), "aggregatedCount": proj.get("aggregatedCount") }));
    let next = events.last().map(|e| e.sequence);
    let (started, ended) = (events.first().occurredAt, events.last().occurredAt);
    Ok(Json(json!({ "worldId": world_id, "match": {phase,eliminations,winnerCharId,alliances},
        "events": events, "nextCursor": next,
        "startedAt": started, "endedAt": ended, "durationMs": ended-started,
        "compliance": {"arbitrationPublic": true, "aiGenerated": true} })))
}
```

**⑤ 前端: 修复 reconnect + 实时观战**（cloudApi.ts:88）
```ts
// cloudStream onmessage：BUG — 现用 payload.id(string) 存 lastEventId 且参数名 lastEventId，
// 服务端 StreamQuery.last_event_id: Option<i64>(events/mod.rs:388) 收不到 → 重连补偿从不生效。
ws.onmessage = (e) => { const p = JSON.parse(e.data);
  if (typeof p?.sequence === 'number') lastEventSeq = p.sequence;   // 用 sequence
  onEvent(p); };
// connect(): params.set('last_event_id', String(lastEventSeq));   // 参数名对齐 + 值为 i64
```
```ts
// ArenaSpectate: 实时合并 —— 复用 useWorldEvents(worldId)(WorldRoom.tsx:65) 的 stream，
// 过滤 arena_* 事件 merge 进 report.rounds；淘汰事件弹 message.warning(toast)。
useEffect(() => {
  const unsub = cloudStream(worldId, (raw) => { const ev = raw as ArenaReplayEvent;
    if (ev.type === 'arena_elim') message.warning(`${nameOf(ev.characterId)} 被淘汰`);
    if (ev.type === 'arena_winner') message.success(`胜者：${nameOf(ev.characterId)}`);
    setLiveEvents(prev => upsert(prev, ev));                        // 复用 upsertEvent 语义
    if (ev.type === 'arena_gift' || ev.type === 'arena_winner') void loadReport(); // 补权威快照
  });
  return unsub; }, [worldId]);
// 打赏入口：<Button onClick={() => cloudFetch(`/api/arena/${worldId}/gift`,
//   { method:'POST', idempotent:true, body:{ sku:'rose', count:1 } })} />
```

## 与现有交互

- **仲裁/结算不变**：`settle`(arena/mod.rs:446) 仍是唯一落定路径，consent-gated（approved 才淘汰）。系统事件是 commit **之后**的广播，`emit_arena_event` 失败仅 log 不回滚（与 runtime 集成接线同策略 runtime/mod.rs:1642）。
- **双硬隔离不破**：赛事系统事件全部 `visibility=public/audience=None`，`ws_visible`(events/mod.rs:63) 与 `row_to_event`(events/mod.rs:300) 对 public 一律放行；私有投影路径完全不经过本改动 → 观众永远只见 public，参赛者私密不泄。
- **report 与 replay 同源**：都读 `world_events` public 行 + `extract_rule_refs`(arena/mod.rs:274)，口径一致；report 是「按 tick 聚合的当前快照」，replay 是「按 sequence 展开的可 seek 时间线」，系统事件同时出现在两者（report 的 rounds 会多出 arena_* 事件，`ArenaSpectate` 现有渲染 eventTypeMeta 回退默认色即可，补映射更佳）。
- **打赏闭环**：站内 `POST /arena/{id}/gift` 与外部 `livegate/webhook` 汇合于 `apply_gift`，同一 `upsert_gift_boon` 聚合(livegate/mod.rs:132)、同一 `arena_env_events` 系统频道、同一 `emit_arena_event(gift)` 进流；`via` 列区分来源供 billing 分成。
- **billing seam 诚实标注**：站内打赏扣费 `billing::charge` 跨 feature，本期 TODO（与 revive_match 的 billing seam 一致 arena/mod.rs:329）。

## 测试与验证

**Rust（`cargo test -p museai-server --features arena`）**
- `arena/tests.rs`：
  - `settle_emits_elim_and_winner_public_events`：2 角色成局，eliminate→approve consent→settle → 断言 `world_events` 出现 `arena_elim` + `arena_winner` 且 `visibility='public'`、`audience_json IS NULL`。
  - `replay_returns_public_timeline_seekable`：写多条 public+private world_events → `GET /replay` 只回 public、按 sequence 升序、`nextCursor` 正确、private 摘要不泄。
  - `replay_forbidden_for_private_world_non_member`：private 世界非成员 → 403（复用 events/mod.rs:272 语义，仿 state_summary_private_world_requires_membership events/mod.rs:683）。
  - **红线**：`gift_does_not_touch_eliminations_or_winner`：打赏后断言 `arena_matches.eliminations_json/winner_char_id` 不变。
- `livegate/tests.rs`：
  - `spectator_gift_maps_to_env_and_stream`：`POST /arena/{id}/gift{sku:'rose'}` → `arena_env_events` 有 gift_boon 行、`gift_events.via='in_app'`、且产生 `arena_gift` public 事件。
  - `spectator_gift_unmapped_sku_no_boon`：未映射 SKU → 不写 env、不进流、仍记 gift_events（对齐 livegate/mod.rs:109）。
  - `spectator_gift_requires_view_permission`：private 世界非成员 → 403。
  - `spectator_gift_idempotent`：同 Idempotency-Key 重投 → 计数不翻倍。

**前端（`npm run test`）**
- `cloudStream` 单测：mock WS，收到 `{sequence:5}` 后重连，断言 URL 带 `last_event_id=5`（回归 reconnect bug）。
- `ArenaSpectate` 渲染：注入 `arena_elim` 流事件 → 断言 toast 与时间线合并。

**手动/E2E**：`docker-compose up` 起 server(:8787)；开赛事房 → 一窗口 host 触发 tick+eliminate+settle，另一窗口 `ArenaSpectate` 观战 → 断言淘汰**实时**弹出、打赏按钮→环境日志即时增长、concluded 后进「回放」页可拖动进度条重放。`npm run build` + `cargo test` + `cargo clippy` 三绿。

## 依赖（其他两块）

- **引擎/runtime 块**：礼物 boon 真正注入 LLM `RoundInput` 仍是既有 seam（arena/mod.rs:17、livegate/mod.rs:11，`applied_tick=NULL` 待消费）。本规格**不依赖**其完成——打赏进流/进战报即可闭合观战+打赏商业环；boon 生效是后续增强。若 runtime 后续在 `commit_tick`(runtime/mod.rs:1607) 消费 gift_boon 并置 `applied_tick`，回放/report 的 `env.appliedTick` 自动变真，无需改本块。
- **billing 块**：站内打赏实际扣费 + 主播分成依赖 `billing::charge`（billing/mod.rs 现有 orders/balance :40-42）。本期以 seam TODO 接线，端点先记账（gift_events）不扣费；billing 就绪后在 `spectator_gift` 内加一次 `charge` 事务即可。
- **clips 块**：回放页可选嵌入高光切片 `GET /arena/{id}/clips`(livegate/mod.rs:248) 已就绪，直接复用，无新依赖。

## 渐进式分步落地

1. **Step 1（server 地基，独立可测）**：`events::persist_and_broadcast_public_event` 抽取 + 单测。不改任何现有行为。
2. **Step 2（赛制进流，红线核心）**：`settle`/`recompute_winner` 加 `emit_arena_event`；红线测试（gift/淘汰不越权）。此步后 `WorldSpectate`(已有实时流) 立即能看到淘汰/胜者。
3. **Step 3（打赏系统频道）**：`livegate::apply_gift` 抽取 + `POST /arena/{id}/gift` + `via` 迁移 0012 + 测试。商业入口后端就绪。
4. **Step 4（回放端点）**：`GET /arena/{id}/replay` + 测试。
5. **Step 5（前端修 bug + 实时化）**：`cloudApi.ts` reconnect 修复（先行，独立价值）→ `ArenaSpectate` 接流 + 打赏入口 + store 类型。
6. **Step 6（回放页）**：`ArenaReplay.tsx` + 路由。
7. **Step 7（打磨）**：淘汰 toast/动效、eventTypeMeta 补 arena_*、回放嵌 clips、billing seam 标注文案。

每步独立编译/测试通过再进下一步；Step 1-2 即可让「实时观战淘汰」上线，Step 3-5 闭合打赏，Step 6 补回放。

## 工作量与影响面估计

| 模块 | 改动 | 量级 | 风险 |
|---|---|---|---|
| `events/mod.rs` | 抽 public 事件 helper | ~50 行 | 低（纯新增，复用现有落库/广播） |
| `arena/mod.rs` | emit 接线 + replay handler | ~120 行 | 中（碰红线路径，需红线测试兜底） |
| `livegate/mod.rs` | apply_gift 抽取 + gift 端点 | ~80 行 | 低（复用现有聚合/记账） |
| migration 0012 | 加列 | ~5 行 | 低（NOT NULL DEFAULT，零回填安全，同 0010 风格） |
| `cloudApi.ts` | reconnect bug 修复 | ~5 行 | 低（且修既有隐患） |
| `ArenaSpectate.tsx` | 实时化 + 打赏入口 | ~150 行 | 中（UI 状态合并去重） |
| `ArenaReplay.tsx` | 新页 + 播放器 | ~250 行 | 中（虚拟时钟/倍速/分页拉取） |
| store/App/tests | 类型+路由+测试 | ~200 行 | 低 |

**总量**：约 850-900 行，server 侧 ~250 行 / 前端 ~600 行。**影响面**：server 集中在 `arena`+`livegate`+`events` 三模块，`events`/`runtime` 核心结构零改动（只加 helper）；前端集中在 `platform/` 目录，桌面/手机功能域(`src/pages/*` chat/story 等)完全不受影响。**最高风险点**是 Step 2 触碰结算路径——以「commit 后广播、失败不回滚、红线测试锁定 eliminations/winner 不被打赏路径改写」三重约束控制。**关键前置修复**：`cloudStream` reconnect 参数 bug(cloudApi.ts:88-91)——命名(`lastEventId` vs 服务端 `last_event_id`)与类型(string id vs i64 sequence)双重错配，导致 WS 断线补偿当前**完全失效**，应在 Step 5 优先单独修。
