以下为实现规格（直接作为返回内容）。

---

# 【第二块】异步时间线（离散事件仿真 DES）实现规格

> 代码根：`/Users/zc/Desktop/py_project/MuseAI-Platform/`
> 引擎：`crates/muse-engine/src/narrative/`　服务端：`server/src/runtime/`

## 概述

现状是**全局同步回合制**：`run_round`（`narrative/mod.rs:138-371`）一次调用取全体 `active_cards`、导演给单一 situation、全员并发 `role_decide` 后按 `character_id` 定序、单个 `StatePatch` 原子提交、`revision += 1`。引擎内部**无任何时间概念**——`tick = current.revision`（`mod.rs:152`），`DomainEvent` 只有回合内 `sequence`（`types.rs:201`）、`RoleDecision` 无 `duration`（`types.rs:230-245`）。服务端 `schedule_due_ticks`（`runtime/mod.rs:177-247`）按墙钟固定间隔 `86_400_000/tick_per_day` 排 tick，一 tick = 一次 `run_round` = 一个 revision。

目标是转成**事件驱动的离散事件仿真**：每个行动带 `duration`，每角色有 `next_time` 指针，世界推进到「最小 `next_time` 的角色（或同刻/同地碰撞组）」→ 仅这批角色行动 → 产事件打游戏时间戳 → `next_time += duration`；世界终止看主线完成或时间上限，**不等全员 `next_time` 收敛**。

**核心结论（重写 vs 包装）**：
- **`run_round` 不重写，做「包装 + 小改」。** 它已经是一个「给定一个角色 cohort → 产一个原子提交」的原语：cohort 就是传入的 `active_cards`。DES 的新增部分是它**上方**的一层调度器 `run_event_step`，负责「选 cohort → 调 `run_round` → 按 duration 推进 `next_time`」。`run_round` 的 reducer / commit / 不变量 / 门控契约**原样复用**，只做三处小改：定序键 `character_id → (next_time, character_id)`、事件带 `timestamp`、budget 公式随 cohort 浮动。
- **确定性可保持**：全序键从 `character_id` 升级为「先按 `next_time`（游戏时刻）后按 `character_id`」；cohort 选择本身确定（BTreeMap 有序 + 取最小）；`duration` 是决策的一部分（给定模型输出即确定）；跨步事件全序 = `(timestamp, sequence)`。**关键取舍：单一 revision 轴串行提交**（不做 per-timeline 分支），从而绕开 `snapshot.rs` 只有 `branch_from`、**无确定性 merge** 的缺口。
- **可渐进**：Phase 0 只加数据字段（serde default 全后向兼容，零行为变化）；Phase 1 在服务端/引擎加「cohort 过滤 + next_time 推进」两步，不动 reducer/commit 原子性；Phase 2 加游戏时钟与世界级开关（老世界走 interval 不变）；Phase 3 才引入「同地点碰撞」（依赖第一块 location）；Phase 4 接终局（第三块）。

---

## 数据结构

### 引擎侧新增/改动

| 名称 | 所在文件 | 字段与类型 | 用途 |
|---|---|---|---|
| `TimelineLayer`（**新增**） | `narrative/types.rs`（`NarrativeState` 旁，`types.rs:130-147` 追加一层） | `now: i64`（游戏时钟，ms 或抽象单位）；`next_time: BTreeMap<String, i64>`（角色 id → 下次可行动的游戏时刻）；`time_cap: Option<i64>`（时间上限，None=无限）；`schema_version: u32` | 每角色行动指针 + 世界游戏时钟。**引擎调度元数据，不经 reducer 白名单**（类比 `pending_consents`，`types.rs:104-108`） |
| `NarrativeState.timeline`（**新增字段**） | `narrative/types.rs:130-147` | `#[serde(default)] pub timeline: TimelineLayer` | 挂进五层状态；`default` 保证旧存档反序列化为空 timeline（后向兼容） |
| `RoleDecision.duration`（**新增字段**） | `narrative/types.rs:228-245` | `#[serde(default)] pub duration: i64` | 本行动耗时（游戏时间单位）。模型输出，`role_decide` 缺省填 `DEFAULT_DURATION`、clamp 到 `[MIN_DURATION, MAX_DURATION]` |
| `DomainEvent.timestamp`（**新增字段**） | `narrative/types.rs:195-214` | `#[serde(default)] pub timestamp: i64` | 事件对应行动**在游戏时间轴上的落点**（= 本步 cohort 的激活时刻 `T`）。与 `sequence` 组成跨步全序 `(timestamp, sequence)` |
| `EventStep`（**新增**，调度器返回） | `narrative/mod.rs`（`RoundOutcome` 旁，`mod.rs:107-115`） | `outcome: Option<RoundOutcome>`；`activated: Vec<String>`（本步激活角色）；`at_time: i64`；`terminal: Option<Terminal>` | `run_event_step` 的返回，让 server 知道「推进到哪个游戏时刻、哪些角色动了、是否终局」 |
| `Terminal`（**新增 enum**，接第三块） | `narrative/mod.rs` | `MainlineDone { ending: Option<String> }` / `TimeCapReached` / `Starved`（无可调度角色） | run 级终态信号。目前 `RoundOutcome` 只有 `blocked`（`mod.rs:113`），无终局出口 |
| 常量 | `narrative/mod.rs:29` 旁 | `DEFAULT_DURATION: i64`、`MIN_DURATION`、`MAX_DURATION`、`RETRY_STEP: i64`（blocked/gated 后 next_time 兜底推进量，防饿死） | 时间单位与兜底步长 |

### 服务端新增/改动

| 名称 | 所在文件 | 字段与类型 | 用途 |
|---|---|---|---|
| `worlds.timeline_mode`（**新增列**） | `server/migrations/000X_timeline.sql`（新迁移） | `TEXT DEFAULT 'interval'`（`interval` \| `event`） | 世界级开关：老世界默认 `interval` 走原路，新世界 `event` 走 DES 调度。**渐进核心闸** |
| `worlds.game_time`（**新增列**） | 同上；`worlds` 表 `0001_init.sql:101-120` | `BIGINT DEFAULT 0` | 世界游戏时钟快照（`= NarrativeState.timeline.now`），供调度器读「下一步游戏时刻」而不必反序列化整份 narrative_state_json |
| `TickJob.cohort_at`（**新增字段**，可选） | `server/src/runtime/mod.rs`（`TickJob` 定义处，`schedule_tick` 用于 `queue::push_json` `:222-227`） | `at_time: Option<i64>` | event 模式下携带「本 tick 要推进到的游戏时刻」，让 `process_tick` 传给 `run_event_step`。interval 模式为 None |

> **不新增 scheduled_events 表**（MVP）：DES 权威定时锚在 `NarrativeState.timeline.next_time`（随 narrative_state_json 落 DB + FS），`world_ticks` 继续做耐久底座（研究已确认 `MemQueue` 非持久，未来定时项不可依赖 `queue::push` 的 `due_ms`）。放置房用「背靠背立即推进」模式（见核心算法），不需要墙钟到点表。

---

## 改动文件清单

### `crates/muse-engine/src/narrative/types.rs`
- `NarrativeState`（`:130-147`）：加 `timeline: TimelineLayer` 字段（`#[serde(default)]`）。
- 新增 `TimelineLayer` struct（`now/next_time/time_cap/schema_version`，全 `#[serde(default)]`）。
- `RoleDecision`（`:228-245`）：加 `duration: i64`（`#[serde(default)]`）。
- `DomainEvent`（`:195-214`）：加 `timestamp: i64`（`#[serde(default)]`）。**不动 `sequence`**（仍为步内序号）。

### `crates/muse-engine/src/narrative/mod.rs`（核心，但**不重写 run_round 主体**）
- **新增 `run_event_step`**（放在 `run_round` 之上）：调度器主循环单步 —— 载状态 → 算最小 `next_time` = `T` → 选 cohort → 构造「过滤后的 `RoundInput`」（`active_cards` 只留 cohort，`other_cards_brief` 保留全体名以维持在场感知，见研究 A①）→ 调 `run_round` → 按 `duration` 推进 cohort 的 `next_time` → `timeline.now = T` → 持久化 timeline → 检查终局。返回 `EventStep`。
- **新增 `select_cohort`**：Phase 1 = `{ c : next_time[c] == T }`；Phase 3 = `{ c : same_location(c, T) ∧ window_overlap }`（依赖第一块）。缺席角色（`next_time` 未初始化）视为 `now`，首步全体入场。
- **新增 `advance_next_time` / `persist_timeline`**：镜像 `persist_pending_consents`（`mod.rs:358-359`）—— **绕过 reducer 白名单直接重写状态**，因为 timeline 是引擎调度元数据（与 `pending_consents` 同性质，`types.rs:104-108`）。在 `commit_scene` 之后调用。
- **新增 `is_terminal`**：全硬 `OutlineNode` Done/Bypassed → `MainlineDone`；`timeline.now >= time_cap` → `TimeCapReached`；`next_time` 为空 → `Starved`。**不以「全员 next_time 耗尽」为条件**。
- **改 `run_round` 内**：
  - 定序键升级：`decisions.sort_by(character_id)`（`:234`）与 `outcomes.sort_by((character_id, decision_id))`（`:254-256`）保持不变（cohort 内同刻，仍以 character_id 定序即可全序）；**跨步全序由 `run_event_step` 的 T 保证**。
  - `build_events` 调用后给每个事件写 `timestamp = tick_time`（cohort 激活时刻 `T`），`T` 由 `run_event_step` 经 `RoundInput` 新增字段 `now_hint: i64` 传入（或复用 `input` 扩展）。
  - budget 公式 `calls = active_ids.len() + 4`（`:159`）语义不变，但 `active_ids` 现在是 cohort 子集 → 单步成本随 cohort 浮动（无需改公式，`estimate` `:127-134` 说明补注即可）。
- **改 `build_events`（`:554-608`）**：签名加 `at_time: i64` 参数，写入每个 `DomainEvent.timestamp`。`sequence` 仍步内自增。
- **改 `decision_id` 派生**（在 `decide.rs`，见下）：现 `dec:{run_id}:{character_id}`（`decide.rs:162`）不含时间，同角色异步多次行动会撞 id → 改为 `dec:{run_id}:{tick_or_time}:{character_id}`。

### `crates/muse-engine/src/narrative/decide.rs`
- `role_decide` 输出 schema（prompt `:126` 处）：JSON 加 `"duration"` 字段说明。
- 补齐逻辑（`:331` 附近）：`duration` 缺省填 `DEFAULT_DURATION`，clamp 到 `[MIN, MAX]`（防模型给出 0 或负导致同角色永远抢占 `T`、其它角色饿死）。
- `decision_id` 派生（`:162`）：加入 tick/time 判别段。

### `crates/muse-engine/src/narrative/continuity.rs`
- `deterministic_invariants`（`:23`）：I3「actor/target ⊆ 在场」（`:60-73`）—— Phase 1 在场集 = cohort（`run_event_step` 传入的 `active_ids` 已是子集，天然收窄，**无需改逻辑**）。Phase 3 才改为「同地点在场」。
- I2「patch.source_decision_ids ⊆ 本回合决策」（`:46-51`）—— **单步内成立**（cohort 决策就是本步全部决策），跨步不受影响。**这是「包装而非重写」能守住 I2 的关键**：每个 event_step 就是一个自洽的「回合」，I2 边界不变。

### `crates/muse-engine/src/narrative/arbiter.rs`
- `rule_arbitrate`（`:79`）R2 目标在场（`:128-130`）、R4 同目标冲突（`:94`）—— Phase 1 输入 `active_ids` 已是 cohort，无需改。Phase 3 改为「同地点」。

### `crates/muse-engine/src/narrative/state.rs`
- `commit_scene`（`:51-73`）：**不改**。timeline 推进走独立的 `persist_timeline`（绕 reducer），提交原子性契约维持。

### `server/src/runtime/mod.rs`
- **`schedule_due_ticks`（`:177-247`）**：加 `timeline_mode` 分支。`event` 模式下：
  - 放置房 MVP：只要世界 `running` 且上一 tick 已 done 且非终局，就**立即** `schedule_tick`（背靠背推进，不看墙钟 interval）。研究已证 arena/chapter 的带外 `schedule_tick`（`arena/mod.rs:166`、`chapters/mod.rs:136`）与定时器共存，是现成先例。
  - `straggler` 补偿窗口 `interval`（`:212`）与 `due = now - last >= interval`（`:238-241`）在 event 模式改用固定 `RECLAIM_PENDING_MIN_MS` 阈值（去掉 interval 依赖）。
- **`schedule_tick`（`:155-175`）**：event 模式携带 `TickJob.at_time`（= 从 `worlds.game_time` 读的当前游戏时刻，或让引擎在 `run_event_step` 内自算 `T`，server 只传 None）。`tick_no = MAX+1` 稠密单调、`base_revision` 快照、CAS **全部不变**。
- **`process_tick_inner`（`:614-879`）**：组装 `active_cards`（`:725-750`）后，event 模式改调 `run_event_step` 而非 `run_round`（interval 模式仍调 `run_round`）。`run_event_step` 内部做 cohort 过滤，因此 server 仍可把全体成员卡塞进 `active_cards`（cohort 选择在引擎内做，避免 server 重复实现选择逻辑）。
- **`commit_tick`（`:884-978`）**：CAS（`:903-917`）不变；新增：把 `outcome.new_state.timeline.now` 写回 `worlds.game_time`；消费 `EventStep.terminal` 信号 → 若终局，`UPDATE worlds SET status='ended'`（接第三块 `end_world()`）。事件投影落库（`:931-933`）保持，事件现多带 `timestamp` 字段（DB 事件表若有列约束需加列，否则 JSON 透传）。

### `server/migrations/000X_timeline.sql`（**新增迁移**）
- `ALTER TABLE worlds ADD COLUMN timeline_mode TEXT NOT NULL DEFAULT 'interval';`
- `ALTER TABLE worlds ADD COLUMN game_time BIGINT NOT NULL DEFAULT 0;`

---

## 核心算法

### 调度器主步（新增 `run_event_step`，`narrative/mod.rs`，紧邻 `run_round` `mod.rs:138`）

```
async fn run_event_step(routes, prompts, input, cancel) -> EventStep:
    state = store.load(input.run_id)                         # 复用 state.rs 载入

    # 1) 终局先判（世界结束不等全员）—— 接第三块
    if let Some(t) = is_terminal(&state):                    # 全硬节点 Done / now>=time_cap
        return EventStep { outcome: None, terminal: Some(t), ... }

    # 2) 求最小 next_time（缺席角色视为 now，首步全入场）
    #    确定性：BTreeMap 有序遍历 + 取 min，平手落到后续 cohort 的 character_id 定序
    T = state.timeline.next_time.values().min()              # None → 全体首次入场，T = now
    #    Phase 1: 同刻 cohort
    cohort = { c in state.characters.keys()
               : next_time.get(c).unwrap_or(now) == T }
    #    Phase 3 (依赖第一块 location):
    #    cohort = { c : same_location(c) ∧ [T, T+dur) 与组内窗口重叠 }

    if cohort.is_empty():
        return EventStep { terminal: Some(Terminal::Starved), ... }

    # 3) 过滤 RoundInput：active_cards 只留 cohort，other_cards_brief 保留全体名
    filtered = RoundInput {
        active_cards: input.active_cards ∩ cohort,           # 子集
        other_cards_brief: input.other_cards_brief,          # 全体（维持在场感知，研究 A①）
        now_hint: T,                                         # 传给 build_events 打 timestamp
        ..input
    }

    # 4) 调用【未改动核心】run_round —— 仲裁/写作/门控/不变量/原子提交全复用
    outcome = self.run_round(routes, prompts, filtered, cancel).await?

    if outcome.blocked.is_some():
        # blocked 不提交状态；cohort 兜底推进防饿死（否则同一 T 反复重试锁死）
        for c in cohort: next_time[c] = T + RETRY_STEP
        persist_timeline(state, next_time, now=T)
        return EventStep { outcome: Some(outcome), activated: cohort, at_time: T, terminal: None }

    # 5) 按 duration 推进各角色 next_time（duration 来自决策，确定性）
    for c in cohort:
        dur = clamp(outcome.scene.decisions[c].duration, MIN_DURATION, MAX_DURATION)
        next_time[c] = T + dur
    # gated/未落定角色（被同意门控剔除）也推进，避免下一步立刻重抢 T

    # 6) 持久化 timeline（绕 reducer，镜像 persist_pending_consents mod.rs:358）
    new_state = persist_timeline(outcome.new_state, next_time, now=T)

    return EventStep { outcome: Some(outcome.with_state(new_state)),
                       activated: cohort, at_time: T, terminal: None }
```

### `run_round` 内的最小改动点（`mod.rs`）
```
# :234, :254  定序键：cohort 内仍按 character_id / (character_id, decision_id) —— 不变
#             跨步全序由 run_event_step 的 T 单调保证
# build_events(..., at_time=T)  每事件写 timestamp=T   （mod.rs:298 调用处 + :554 签名）
# budget: calls = cohort.len() + 4                       （:159，语义不变，cohort 更小）
```

### 确定性论证（务实）
- **步内**：`run_round` 原有确定性（并发决策后 `sort_by(character_id)` `:234`、outcomes `(character_id, decision_id)` `:254`、reducer 幂等 + base_revision CAS）**原样保留**。cohort 是子集，不引入新的不确定。
- **跨步**：`T = min(next_time)` 确定；平手时同刻多角色进同一 cohort，仍步内定序；`duration` 是决策字段，给定模型输出即定 → `next_time` 推进确定。
- **事件全序**：`(timestamp=T, sequence)`。T 单调不减（每步取 min，且推进量 `duration>0`），`sequence` 步内单调 → 全局 replay 可复现。
- **单写者**：所有 cohort 提交串行到**同一 revision 轴**（`reducer.rs:169` 递增 + `commit_tick` CAS `runtime:903`）。**刻意不做 per-timeline 分支**，因此**不触发** `snapshot.rs` 的「有 branch 无 merge」缺口。代价是无真并行——对单世界放置房 sim（一 world 一 worker）完全够用。

---

## 测试影响

### 会破坏（需改断言）
- `mod.rs` 内联 `run_round_happy_path`（`mod.rs:949`）：断言精确 5 次模型调用、`narrative_events==4`、`revision 0→1`。改走 `run_event_step` 后，若首步 cohort=全体则调用数/事件数不变；但 `decision_id` 加时间段后 `dec:run-1:li` 断言（`mod.rs`/`decide.rs:353`）要改。
- `estimate_uses_n_plus_4`（`mod.rs:1202`）、`run_round_budget_exhausted`（`mod.rs:996` 硬编码 cost 600）：cohort 浮动后成本随激活子集变，需参数化。
- `DomainEvent` / `RoleDecision` / `NarrativeState` 的序列化快照测试：新增字段（`timestamp`/`duration`/`timeline`）改变 JSON 形态（`#[serde(default)]` 兜底反序列化，但序列化输出多字段）。
- 服务端 `tick_runs_full_round`（`runtime/tests.rs:232/:265`）：断言 revision 无限累积、chA+chB 同在场 4 事件 —— event 模式下若 cohort 拆分，在场集与事件数变；需为 event 模式单立测试，interval 模式测试保留。

### 需新增
- **`event_step_advances_next_time`**：单步后 cohort 的 `next_time = T + duration`，非 cohort 角色 `next_time` 不变。
- **`event_step_picks_min_next_time`**：多角色不同 `next_time`，只激活最小者组。
- **`confluence`（确定性核心）**：同一组 per-character 决策（含 duration），任意合法调度顺序产出**相同终态 state + 相同事件全序 `(timestamp, sequence)`**。
- **`decision_id_includes_time`**：同角色跨两步不撞 id。
- **`event_timestamp_monotonic`**：跨步 `timestamp` 不减。
- **`blocked_step_does_not_starve`**：blocked 后 cohort `next_time += RETRY_STEP`，下步能推进。
- **`terminal_not_wait_all`**：一角色 `next_time` 远在未来，主线全 Done 时世界仍判 `MainlineDone`（不等该角色）。
- **`duration_clamped`**：模型给 0/负 duration 被 clamp，不锁死 T。
- 服务端 **`timeline_mode_event_back_to_back`**：event 世界连续 tick 推进 game_time；interval 世界不受影响。
- 服务端 **`game_time_written_back`**：commit 后 `worlds.game_time == timeline.now`。

---

## 依赖（其他部分）

- **第一块（location）—— 硬依赖，仅 Phase 3。** 「同地点 + 时间窗重叠」的碰撞组需要 `CharacterState.location`（第一块引入的 reducer 路径 `locations.*` + 在场判定收窄）。Phase 1/2 用「同刻」近似碰撞，可**独立于第一块先落地**。建议 Phase 3 与第一块合并做（研究 A② 与 B⑥ 均建议合并）。
- **第三块（终局）—— 软依赖，Phase 4。** `is_terminal` 产的 `Terminal::MainlineDone` 需 server `commit_tick` 消费置 `worlds.status='ended'` + `end_world()`。第二块提供信号，第三块提供消费者与停机（scheduler 跳过 ended，`runtime:202`）。可解耦：先只做 `TimeCapReached`（纯引擎自足），主线终局等第三块。
- **无依赖**：世界固有角色（第一块 A）与本块正交——NPC 只要有 `next_time` 就能进调度。

---

## 风险

1. **饿死 / 时间锁死（中）**：模型给 `duration<=0` 或某角色反复抢占最小 `T` → clamp + `RETRY_STEP` 兜底 + blocked 也推进 next_time。
2. **确定性回归（中高）**：现有全部确定性测试基于「单同步回合」假设（`mod.rs:234`/`arbiter.rs:117`/`mod.rs:254`）。DES 引入 `T` 维度后必须新增 confluence 测试作为新的确定性核心，否则 replay/透明战报（按 `(tick_no, sequence)` 取序）会漂。**单写者 revision 轴**是最大的降险决策——不做分支就不需要 merge。
3. **server 定时语义重定义（中）**：`straggler` 窗口与 `due` 判定强耦合 `interval`（`runtime:212/:238`）。event 模式去掉固定 interval 后这两处阈值必须换成绝对阈值，否则补偿 re-enqueue 失效（长回合被多 worker 重复投递，破 C-1 幂等）。
4. **成本失控（中）**：背靠背立即推进使放置房可能高频 tick。必须靠现有 per-tick 预算熔断（`process_tick_inner:669-715` fuse_and_pause）兜底；`time_cap` 提供硬上界。
5. **DomainEvent schema 版本（低）**：加 `timestamp` 需 bump 事件 schema 或靠 `#[serde(default)]` 兼容；平台 P3 WorldEvent 包装层（`events::project_domain_events`）要透传新字段。
6. **`MemQueue` 非持久（低，已规避）**：不依赖 `queue::push` 的未来 `due_ms`；权威定时锚在 `timeline.next_time`（随 narrative_state_json 落 DB），`world_ticks` 做耐久底座。

---

## 渐进式分步落地（避免大爆炸）

- **Phase 0 — 纯数据（零行为变化，1 PR）**：`TimelineLayer` + `NarrativeState.timeline` + `RoleDecision.duration` + `DomainEvent.timestamp`，全 `#[serde(default)]`。`run_round` 不动，`duration`/`timestamp` 被忽略。加序列化 round-trip 测试。**可独立合并、可回滚。**
- **Phase 1 — cohort 过滤 + next_time 推进（引擎，单写者不变，1-2 PR）**：新增 `run_event_step`/`select_cohort`(同刻)/`persist_timeline`/`is_terminal`(仅 TimeCapReached/Starved)。定序键与 `decision_id` 加时间段。`run_round` 核心不动（只加 `build_events` 的 `at_time`）。**reducer/commit 原子性契约零改动。** 引擎侧可完整单测 confluence。
- **Phase 2 — server event 模式（1-2 PR）**：迁移加 `timeline_mode`/`game_time`；`schedule_due_ticks` 加 event 分支（放置房背靠背）；`process_tick_inner` 按 mode 分派 `run_event_step`/`run_round`；`commit_tick` 回写 game_time。**老世界 `timeline_mode='interval'` 完全走原路**。用 arena/chapter 带外触发作渐进验证样例。
- **Phase 3 — 同地点碰撞（依赖第一块，合并做）**：`select_cohort` 升级为「同 location + 时间窗重叠」；`arbiter` R2/R4、`continuity` I3、导演/写作分组按同地点收窄（`decide.rs:86`/`arbiter.rs:129`/`continuity.rs:60`）。
- **Phase 4 — 主线终局（接第三块）**：`is_terminal` 加 `MainlineDone`；`commit_tick` 消费 → `end_world()`；scheduler 跳过 ended。

每个 Phase 独立可测、可上线、可回滚；Phase 0-2 不依赖任何其它块。

---

## 工作量与影响面估计

| 维度 | 估计 |
|---|---|
| **重写规模（诚实）** | **`run_round` 主体（~230 行）约 90% 复用**，改动是加法：定序键注释级微调、`build_events` 加 1 参数、decision_id 加段。真正的新代码是 `run_event_step` 及辅助（`select_cohort`/`persist_timeline`/`advance_next_time`/`is_terminal`），约 **150-250 行新增**，不是推翻重写。reducer/state/constraints/snapshot **零改动**（研究结论「E3 确定性状态机可原样复用」成立）。 |
| **引擎改动文件** | 5 个：`types.rs`(字段)、`mod.rs`(新方法 + 微调)、`decide.rs`(duration + id)、`continuity.rs`/`arbiter.rs`(Phase 3 才动) |
| **server 改动文件** | 3 个 + 1 迁移：`runtime/mod.rs`(schedule/process/commit 三处分支)、新迁移、`TickJob` |
| **测试** | 破坏 ~6 个内联测试（断言参数化），新增 ~10 个（含 confluence 核心） |
| **净估** | Phase 0-2（引擎 DES 骨架 + server 接线，不含 location）：**中等工作量**，约 3-5 个聚焦 PR。Phase 3（碰撞，含第一块）另计为大改。Phase 4（终局，含第三块）小改。 |
| **风险等级** | **中**（单写者 + serde default + 世界级开关三重降险后）。若做 per-timeline 分支并行则升为高（需补 merge，不建议）。 |
