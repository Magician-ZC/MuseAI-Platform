I have a complete, grounded picture. Here is the implementation spec.

---

# 【异步时间线全房型 + 地点碰撞落地】实现规格（P2 Stage3）

> 代码根：`/Users/zc/Desktop/py_project/MuseAI-Platform/`
> 引擎：`crates/muse-engine/src/narrative/`　服务端：`server/src/runtime|chapters|arena|worlds|admin_api/`

## 概述

现状（已落地部分，实测代码）：

- **引擎侧的 DES 已经房型无关**。`run_event_step`（`narrative/mod.rs:567`）→ `select_time`（`:666`）→ `select_cohort`（`:692`，**同 location + 时间窗 `[T,T+dur)` 重叠**的碰撞组，退化为「空闲于 T」）→ 调**未改动的** `run_round`（`:196`）→ `persist_timeline`（`:747`）→ `is_terminal`（`:725`）。这套逻辑只读 `NarrativeState`，不认房型。
- **run_round 本身对所有房型已按 location 分组**（`:218-227`），但那是**同步**分组（一 tick 内所有地点组同一 revision 提交）。真正的「时间窗碰撞 + 逐地点串行、各自独立 revision/timestamp」只在 `run_event_step` 内。
- **server 侧 dispatch 已经房型无关**：`process_tick_inner` 的分派点（`runtime/mod.rs:1443`）纯看 `world.timeline_mode == "event"` → `run_event_step`，否则 `run_round`。**任何** event 世界都会走 DES，与 room_type 无关。

那么为什么 Stage3 碰撞「只在 idle 生效」？三道**房型闸**把 chapter/arena 挡在 event 模式之外：

1. **建房层硬拒**：`admin_api/worlds_ops.rs:333` —— `timeline_mode=="event" && room_type != "idle"` 直接 `BadRequest`。chapter/arena **根本无法被建成 event 房**，因此永远走 `run_round`（interval），碰不到 `select_cohort`。
2. **调度层把 event 等同于「背靠背自治」**：`schedule_due_ticks` 的 event 分支（`runtime/mod.rs:228-259`）对**任何** event 世界都做「无 outstanding 就立即排新 tick」。若 chapter/arena 进 event，会被调度器无限自动跑 LLM 回合，**摧毁 arena「节目节奏优先于定时器」与 chapter「会话驱动」语义**。
3. **终局层 idle-gated**：`load_endgame_policy`（`runtime/mod.rs:682`）`enabled = room_type=="idle"`，`run_event_step` 的终局短路（`:1456`）与 `commit_tick` 终局评估（`:1575`）都被 `policy.enabled` 门住。这一层**恰好是我们想保留的**——chapter/arena 各有自己的结算路径（finish / settle）。

**本 Stage3 的核心洞察**：`timeline_mode=="event"` 现在**过载**了两个正交语义——(a)**引擎 dispatch**（run_event_step 的地点碰撞 + 游戏时钟）与 (b)**调度节奏**（背靠背自治）。让 chapter/arena 也吃到地点碰撞，只需**解耦这两者**：

- **(a) 引擎 dispatch 由 `timeline_mode` 驱动**——已房型无关，几乎零改。
- **(b) 调度节奏由 `room_type` 驱动**——`idle` = 背靠背自治；`chapter/arena` = **手动端点触发**（host/tick、chapter start），调度器只做补偿 re-enqueue，**不自动排新 tick**。

于是 chapter/arena 的「通关/finish、主播 tick/淘汰」保持**手动端点**语义不变，而端点排下的每个 tick 走 `run_event_step` → 吃到**同地点 + 时间窗碰撞**（逐地点串行、各自 revision/timestamp）。终局仍 idle-gated（chapter/arena 的 `is_terminal` 天然不触发，见「与现有交互」），结算走各自既有路径。确定性与 interval 退化完全保持——**只有显式设为 event 的世界行为改变**。

## 复用与新增

**纯复用（零改动）**：
- `run_event_step` / `select_time` / `select_cohort` / `persist_timeline` / `is_terminal` / `clamp_duration`（`narrative/mod.rs:567-758`）——碰撞、时钟、终局判定，房型无关，一行不动。
- `run_round` 的 location 分组 / 逐组导演/仲裁/写作 / reducer / commit / 不变量 / 门控（`narrative/mod.rs:196-554`）——一行不动。
- `arbiter` R2/R6、`continuity` I2/I3（已按分组收窄）——一行不动。
- `process_tick_inner` 的 dispatch（`runtime/mod.rs:1443`）、NPC/locationGraph 注入（`:1247-1305`）、backpack 道具物化（`:1382`）——房型无关，一行不动。
- chapter finish 结算 / arena eliminate·settle·winner（`chapters/mod.rs:165`、`arena/mod.rs:366/446/531`）——手动结算语义**保持**，一行不动。

**新增/微改（都在 server 层，引擎零改）**：
1. 放宽建房闸（允许 event × chapter/arena）。
2. 调度节奏按 room_type 解耦（背靠背仅 idle）。
3. chapter/arena event 房的装配保证（locationGraph 注入前提）。
4. （可选）chapter/arena 的 tick 端点在 event 下逐地点推进的接线。

## 数据结构（字段 + 位置）

**无新增表 / 无新增列**。Stage2 的 `worlds.timeline_mode` / `worlds.game_time`（`migrations/0010_timeline.sql:10-11`）已足够；引擎 `TimelineLayer` / `RoleDecision.duration` / `DomainEvent.timestamp`（`narrative/types.rs`）已存在。Stage3 是**约束放宽 + 调度分派**，不引入新持久化字段。

唯一语义扩展：`worlds.timeline_mode`（`worlds/mod.rs:412`）的合法组合从「event ⟺ idle」放宽为「event × {idle, chapter, arena} 全允许」；`interval` 保持默认。归一化枚举（`worlds/mod.rs:481` 的 `matches!("interval"|"event")`）不变。

## 改动文件清单（逐 file: 改什么）

### `server/src/admin_api/worlds_ops.rs`（建房闸放宽）
- `:333` 的跨字段约束 `tm == "event" && p.room_type != "idle"` → 删除该 idle 硬拒，改为**允许 event × {idle,chapter,arena}**。保留枚举校验（`:327` 的 `interval|event`）。
- 补注释：event 对 chapter/arena 表示「引擎走 DES 地点碰撞」，调度节奏仍由端点驱动（见 runtime）；终局仍 idle-gated（chapter/arena 的引擎终局天然不触发，结算走 finish/settle）。
- 如需覆盖测试，`admin_api/tests.rs:686` 的 `world_create_timeline_mode` 旁新增 event×arena / event×chapter 成功建房断言。

### `server/src/runtime/mod.rs`（调度节奏解耦 —— 本 Stage 核心）
- **`schedule_due_ticks` 查询（`:213`）**：`SELECT id, tick_per_day, timeline_mode` → 追加 `room_type`。
- **event 分支（`:228-259`）**：`if timeline_mode == "event"` 内部，**背靠背自动排新 tick（`:247-257` 的 `outstanding==0 → schedule_tick`）用 `room_type == "idle"` 门住**。chapter/arena 的 event 房：**只保留 straggler 补偿 re-enqueue（`:229-246`）**，不自动排新 tick（新 tick 全部来自 host/tick、chapter start 端点）。伪码见下。
- **interval 分支（`:261-298`）不变**：老世界（含 interval 的 chapter/arena）逐字节走原路。
- **dispatch（`:1443`）不改**：`world.timeline_mode == "event"` → `run_event_step`，已房型无关。
- **idle-自动装配块（`:1184`）扩容**：现 `world.room_type == "idle" && world.assembled_json.is_none()` → 放宽为 `world.assembled_json.is_none() && (world.room_type == "idle" || world.timeline_mode == "event")`。使 **event×arena** 房在首个 host/tick 排下的 tick 里一次性装配（产 locationGraph/worldCharacterEntries，碰撞前提）。chapter 已在 start 装配、命中 `assembled_json.is_some()` 短路，逐字节不变；interval 世界 `timeline_mode != "event"` 不触发，零影响。
- **终局短路（`:1452-1473`）/ commit 终局评估（`:1575`）不改**：`endgame_policy.enabled`（idle）门天然把 chapter/arena 排除，落到 `finish_tick_noop("terminal")` 或 `final_status=Done`，世界**不停机**（结算交端点）。

### `server/src/arena/mod.rs`（主播 tick 在 event 下 = 一步碰撞）
- **`host_tick`（`:141`）无需改分派**：`:166` 的 `schedule_tick` 排下的 tick 会被 worker 走 `run_event_step`（因 world 是 event）→ 一次 host/tick = **一个 anchor 地点的碰撞组推进一步**（各自 revision/timestamp）。主播连续按 = 逐地点串行推进，天然契合「节目节奏」。
- **可选增强**：若一次 host/tick 想推进「当前所有空闲地点各一步」，在 `:166` 后循环 `schedule_tick` 直到无空闲角色（读 `game_time`/timeline）。**MVP 不做**——保持「一击一地点」最简语义，主播多按几次即可。
- `ensure_match` 首击装配：event×arena 首个 host/tick 已由 runtime `:1184` 扩容块兜底装配，arena 侧无需显式 assemble。

### `server/src/chapters/mod.rs`（会话在 event 下 = 逐步碰撞）
- **`chapter_start`（`:94`）不改装配/CAS**：`:137` 的 `schedule_tick` 排下的 tick 在 event 下走 `run_event_step`（start 已装配 → locationGraph 就绪 → 碰撞生效）。
- **`chapter_finish`（`:165`）保持手动结算**：主线推进/通关/grant/离线（`:237-302`）与引擎 DES 正交，一行不动。通关判定仍按 `currentNode`（`:243`）而非引擎 `Terminal`（chapter 的 `is_terminal` 不触发 MainlineDone，见交互）。
- **可选增强**：会话内多步碰撞需多个 event step。MVP 方案：`chapter_start` 一次排一个 tick（一步）；前端/客户端按需重复调用 start（幂等键区分）或复用 arena 式「advance」端点排下一步。**建议**新增极薄的 `POST /worlds/{id}/chapters/advance`（镜像 `arena host_tick`，仅 `schedule_tick`，require_chapter_room + 在场校验），把「推进一个碰撞步」与「finish 结算」显式分开。列为 Stage3 可选项。

### 测试（见「测试与验证」）
- `server/src/runtime/tests.rs`：新增 event×arena / event×chapter「手动排 tick 才推进、调度器不背靠背」用例；复用 `:960` 的 room_type 建房 helper。
- `server/src/worlds/tests.rs` / `admin_api/tests.rs`：event×非 idle 建房不再被拒。

## 核心逻辑（伪码，带 file:line）

### 调度节奏解耦（`runtime/mod.rs:228`，schedule_due_ticks 内）
```
# 查询追加 room_type（:213）
for w in worlds(status='running'):          # :216
    (world_id, tick_per_day, timeline_mode, room_type) = w

    if timeline_mode == "event":            # :228
        # straggler 补偿 re-enqueue —— 房型无关，保留（:229-246）
        re_enqueue_pending_older_than(RECLAIM_PENDING_MIN_MS)

        # ★Stage3 关键改动：背靠背自治仅 idle（:247-257）
        if room_type == "idle":
            if outstanding_ticks(world_id) == 0:
                schedule_tick(world_id)      # 自治放置房：无 outstanding 立即推进
        # else chapter/arena：不自动排新 tick —— 新 tick 全来自
        #   arena host_tick(:166) / chapter start(:137) / (可选)chapters advance
        continue                             # :258

    # interval 分支完全不变（:261-298）：老世界墙钟排 tick → run_round
```

### 建房闸放宽（`admin_api/worlds_ops.rs:326`）
```
if let Some(tm) = req.timeline_mode:        # :326
    if tm not in {"interval","event"}: BadRequest  # :327 保留
    # ★删除 :333 的 `tm=="event" && room_type!="idle"` 硬拒
    # event 对 chapter/arena 表示引擎走 DES 碰撞；调度节奏由端点驱动；终局 idle-gated
    p.timeline_mode = tm                     # :336
```

### 装配兜底扩容（`runtime/mod.rs:1184`）
```
# 原：world.room_type == "idle" && world.assembled_json.is_none()
# ★新：
let world = if world.assembled_json.is_none()
        && (world.room_type == "idle" || world.timeline_mode == "event") {   # :1184
    assemble_instance(state, world_id)       # 产 locationGraph/worldCharacterEntries/enabledEndings
    load_world(db, world_id)                 # reload 使 :1257 注入命中
} else { world };
# chapter：start 已装配 → assembled_json.is_some() 短路，逐字节不变
# arena(event)：首个 host_tick 的 tick 在此一次性装配 → 后续碰撞按地点分组
# interval 世界：timeline_mode!="event" → 不触发（零影响）
```

### 碰撞对所有房型生效（**无需新代码**，`narrative/mod.rs:595`）
```
# process_tick_inner dispatch（:1443）—— event 世界（任意房型）走：
run_event_step:                              # :567
    t = select_time(state)                   # :591  最小 next_time
    cohort = select_cohort(state, t)         # :595  ★同 location 锚 + 空闲于 T（:692-709）
    run_round(active_cards ∩ cohort)         # :620  cohort 恒同 location → 单组处理
    advance next_time[c] = t + duration      # :639
    persist_timeline; is_terminal            # :648/:651
# chapter/arena 一旦是 event 房，逐地点串行碰撞、各自 revision/timestamp —— 与 idle 同源
```

## 与现有交互

- **终局天然对 chapter/arena 无害**（这是「碰撞开、终局关」得以成立的关键）：`is_terminal`（`narrative/mod.rs:725`）的 `MainlineDone` 只在**里程碑集（`threshold.is_some()` 的 OutlineNode）非空且全 Done** 时触发（守卫①，`:727-733`）。chapter/arena 的 skeleton 用硬节点 `threshold=None` → 里程碑集**空** → 恒不发 MainlineDone。`time_cap` 缺省 None、characters 非空 → 也不发 TimeCapReached/Starved。故 chapter/arena 的 `run_event_step` **恒返回 `outcome=Some, terminal=None`**，commit 正常走完，`endgame_policy.enabled=false` 使终局评估短路（`runtime/mod.rs:1575`），**世界永不被引擎自动停机**——结算权 100% 留给 chapter finish / arena settle。
- **chapter finish 的 state_revision CAS 与 event tick 的 commit CAS 串行**：两者都 CAS `worlds.state_revision`（chapter `:263`；event commit `:1554`）。若 finish 在 tick schedule→process 之间 bump revision，event tick 命中 `superseded`（`runtime/mod.rs:1107`）→ `finish_tick_noop("superseded")` 良性跳过；finish 有 `MAX_CAS_RETRIES=8` 兜底。高频 finish 可能让叙事步偶尔被 supersede，属可接受的稀有竞争。
- **arena eliminate/settle 与 DES 正交**：eliminate 走 consent 门控（`arena/mod.rs:414`），settle 读 consent 落定 + `recompute_winner`（`:531`），全部读 world_members/eliminations_json，不碰 narrative_state。event tick 推进叙事、settle 收敛赛制，互不阻塞。
- **成本熔断照旧兜底**：event×arena/chapter 因手动驱动，天然低频（无背靠背），但仍受 per-tick 预算熔断（`runtime/mod.rs:1143-1156` fuse_and_pause）保护。
- **确定性 / interval 退化**：`select_cohort` 全序（BTreeMap 有序 + 锚地点字典序，`:704-709`）、`run_round` 步内定序不变。**未显式设 event 的世界**（含所有现存 chapter/arena）走 interval `run_round`，逐字节保持——本 Stage 零回归面。

## 测试与验证

**新增（server, `runtime/tests.rs`，复用 `:960` room_type helper）**：
- `event_arena_manual_only_no_backtoback`：建 event×arena（running），跑一轮 `schedule_due_ticks`，断言**无自动 tick**（world_ticks 计数不增）；调 `host_tick` 后有且仅有一个新 tick。
- `event_chapter_start_schedules_one_step`：event×chapter，start 后恰一个 tick；再跑 scheduler 不追加。
- `event_idle_still_back_to_back`：event×idle 回归——scheduler 仍背靠背排 tick（保护 Stage2 行为，对齐现有 `timeline_mode_event_back_to_back` `:850`）。
- `event_arena_collision_by_location`（可注入 mock model）：给 arena 装配含 2 地点的 locationGraph、4 角色分居两地，`process_tick_with_model` 走 event 步 → 断言**单步 cohort 恒同一 location**（`activated` 全同地）、跨两步各自独立 timestamp/revision。
- `event_non_idle_never_concludes`：event×arena 跑到里程碑（若有）后 `is_terminal` 不发 MainlineDone、world 保持 running（终局 idle-gated 回归）。

**新增（build/admin）**：
- `admin_api/tests.rs`：`world_create_event_arena_ok` / `world_create_event_chapter_ok`（`:686` 旁）——放宽后不再 BadRequest。

**回归**：`cargo test --manifest-path server/Cargo.toml`（重点 runtime/worlds/admin_api）、`--features arena`；引擎 `cargo test -p muse-engine`（confluence/terminal_not_wait_all 等应**全绿零改**，证明引擎零改）。

**手动验证**：admin 建 event×arena（带 2 地点模板）→ host/tick 数次 → `GET /arena/{id}/report` 观察逐地点、逐 timestamp 的回合；确认 world 不自动停机、settle 正常收敛胜者。

## 依赖（其他两块）

- **第一块（location / worldCharacterEntries / locationGraph）—— 硬依赖，已就绪**：`select_cohort` 的碰撞靠 `CharacterState.location`；locationGraph/NPC 注入在 `runtime/mod.rs:1257-1305` 已实现且房型无关。chapter/arena 要吃到**地点**碰撞，其模板/装配须产 `locationGraph`（否则退化为单一全局 cohort，仍正确但无地点维度）。装配管线 `assembly::assemble_instance` 已存在。
- **第三块（终局）—— 无新增依赖**：本 Stage **刻意不让** chapter/arena 走引擎终局（idle-gated 保留），二者结算走各自既有端点。第三块的 idle 终局管线不受影响。
- **无依赖**：Stage2（`run_event_step`/`select_cohort`/migration 0010）已合并，本 Stage 纯建立在其上。

## 渐进式分步落地

- **Step A — 建房闸放宽（1 PR，最小）**：改 `worlds_ops.rs:333`，允许 event×{chapter,arena}。**此时若不改调度器，event×arena/chapter 会被背靠背自动跑**——故 A 必须与 B 同 PR 或 B 先行。建议 **A+B 合一**。
- **Step B — 调度节奏解耦（同 PR，核心）**：`schedule_due_ticks` 查 room_type + 背靠背门 idle（`runtime/mod.rs:247`）。落地后 chapter/arena event 房「手动排 tick 才推进」。加 `event_arena_manual_only_no_backtoback` 等测试。**可独立回滚**（回滚即回到「event 仅 idle」硬拒 + 背靠背全 event）。
- **Step C — 装配兜底扩容（同/次 PR）**：`runtime/mod.rs:1184` 放宽到 event 房。使 event×arena 首击自动装配 locationGraph。chapter 已在 start 装配，无变化。加 `event_arena_collision_by_location`。
- **Step D（可选）— chapter advance 端点**：`chapters/mod.rs` 加薄 `advance`（仅 schedule_tick），把「推进碰撞步」与「finish 结算」显式分离。非阻塞项，可后置。

每步独立可测/可上线/可回滚；A+B 是最小可用集（碰撞对 chapter/arena 生效），C 补齐 arena 地点维度，D 是体验增强。

## 工作量与影响面估计

| 维度 | 估计 |
|---|---|
| **引擎改动** | **0 行**。`run_event_step`/`select_cohort`/`is_terminal` 房型无关，Stage2 已就绪。这是本 Stage「务实」的核心——碰撞代码已写好，只是被 server 房型闸挡住。 |
| **server 改动** | 3 处小改：`worlds_ops.rs:333`（删 1 条 if）、`runtime/mod.rs:213/247`（查 room_type + 背靠背门 idle，~5 行）、`runtime/mod.rs:1184`（装配条件放宽，~1 行）。可选 D：chapters advance 端点 ~30 行。 |
| **迁移** | **0**。复用 0010 的 timeline_mode/game_time。 |
| **新增测试** | ~6 个（runtime 4 + admin 2）；引擎测试全绿零改（反证引擎未动）。 |
| **影响面** | **仅显式设 event 的 chapter/arena 世界行为改变**。所有现存世界（interval，含全部现役 chapter/arena）逐字节不变——零回归面。event×idle（Stage2）回归受 `event_idle_still_back_to_back` 保护。 |
| **风险** | **低**。最大风险是「A 放宽但 B 未随」导致 event×arena 被背靠背自治跑爆 LLM 成本——用 A+B 同 PR + per-tick 熔断双保险规避。终局对 chapter/arena 天然不触发（里程碑集空守卫），无「误停机」风险。 |
| **净估** | A+B+C（碰撞全房型可用 + arena 地点维度）：**小工作量**，1-2 个聚焦 PR。D（chapter advance）：**极小**，独立可选。 |
