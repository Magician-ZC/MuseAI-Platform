I have everything needed. Writing the complete implementation spec now.

---

# 放置房终局（Idle-Room Endgame）实现规格

> 根路径：`/Users/zc/Desktop/py_project/MuseAI-Platform/`
> 目标：放置房拥有 5–8 个软主线里程碑；推进 = 关系/事件强度累积到阈值；终局 = 主线走完 ∨ 世界时间到上限 ∨ 关键角色退场；并修掉"空 skeleton → 秒结束"隐患。

## 概述

现状三处硬事实（全部核对过代码）：

1. **推进逻辑**在 `build_patch`（`crates/muse-engine/src/narrative/mod.rs:508-550`）：本回合任一 outcome 为 `Success|PartialSuccess` → `progressed=true`（`mod.rs:521-523`）→ 把 `next_pending`（首个 Pending 节点，`constraints.rs:225-227`）标 `done`。即"有成功就推一个节点"，与节点内容、关系强度、累积量**完全无关**，一回合最多推一个。

2. **终局判定不存在**。引擎 `RoundOutcome` 只有 `blocked: Option<String>`（`mod.rs:117-124`），无 completed/ending 出口；全节点 Done 后 `next_pending` 返 None，`build_patch` 的 `if progressed { if let Some(node) … }` 直接跳过，回合照跑照提交。server 侧 `worlds.status` 枚举有 `ended`（`migrations/0001_init.sql:110`）但**全库无任何代码写入 `ended`**；`schedule_due_ticks` 只要 `status='running'` 就无限排 tick（`runtime/mod.rs:202`）。

3. **"空 skeleton → 秒结束"是前向陷阱**：一旦天真实现"所有节点 Done → ended"，空 skeleton（`mainlineNodes=[]`，seed 后 `outline_nodes` 为空）会让"全部完成"在**空集上真空成立**，世界在第 1 个 tick 即 `ended`；已入队的后续 tick 命中 `already_done`（`runtime/mod.rs:635-637`）/ `world_not_running`（`runtime/mod.rs:658-662`），观察到的就是"建房即秒结束"。本规格从设计上杜绝该真空。

**设计要点（三条同时成立才动手）**：

- 里程碑推进改为**阈值累积判定**：每个软节点带 `threshold`（事件强度累积阈值）+ 可选 `advanceWhen`（关系强度谓词门），`progress[node] += 本回合强度` 达标且谓词命中才 `Pending→Done`。
- 累积载体复用**现成 reducer 路径**：进度写 `world.increment`（`reducer.rs:440-447` 已支持），状态翻转写 `outlineNodes[id].status`（`reducer.rs:89-101` 已支持）——**reducer 白名单零改动**。约束：`world.<key>` 必须单段、无 `.`/`[`（`reducer.rs:48-55`），故进度键用扁平命名 `milestoneProgress_<nodeId>`。
- 终局判定**分层**：引擎判"主线走完"（它持有 outline_nodes，带非空守卫），server 判"世界时间到上限 / 关键角色退场"（它持有世界时钟与成员表），两路汇聚到新 helper `end_world()`。终局逻辑仅对 `room_type='idle'` 生效，不碰 chapter/arena 的既有收敛旁路。

## 数据结构

### 引擎侧（Rust，`crates/muse-engine/`）

**`OutlineNode` 扩展** — `src/narrative/types.rs:73-80`

| 字段 | 类型 | 用途 |
|---|---|---|
| `id` | `String` | 既有，节点 id（约定单 token，无 `.`/`[`；seed 时校验） |
| `summary` | `String` | 既有 |
| `constraint` | `ConstraintLevel` | 既有；放置房里程碑用 `Soft` |
| `status` | `NodeStatus` | 既有（`types.rs:64-71`：Pending/Done/Bypassed/Blocked） |
| `threshold` | `Option<f32>` | **新增**。`#[serde(default)]`。里程碑累积阈值；`Some` 标识"阈值节点"（走新逻辑），`None` 走旧 `progressed=>done`（向后兼容） |
| `advance_when` | `Option<String>` | **新增**。`#[serde(default)]`。关系强度谓词（受限 DSL，如 `relations[a->b].affinity > 0.6`），复用 `constraints::parse_predicate`/`eval_predicate` |
| `weights` | `Option<IntensityWeights>` | **新增，可选**。`#[serde(default)]`。本节点的强度权重覆盖；`None` 用全局默认 |

> `threshold`/`advance_when`/`weights` 均为**只读配置**：仅 seed 写入、仅 `build_patch` 读取，**永不出现在任何 StatePatch 路径**，因此 reducer 白名单不受影响。

**`IntensityWeights`（新增）** — `src/narrative/types.rs`（新结构，紧邻 OutlineNode）

| 字段 | 类型 | 用途 |
|---|---|---|
| `success` | `f32` (default 1.0) | 每个 `Success` outcome 的强度贡献 |
| `partial` | `f32` (default 0.5) | 每个 `PartialSuccess` |
| `failure` | `f32` (default 0.2) | 每个 `Failure` |
| `speak` | `f32` (default 0.25) | 每个 `willSpeak=true` 决策的互动强度 |

**`EndingSignal`（新增）** — `src/narrative/types.rs`

| 字段 | 类型 | 用途 |
|---|---|---|
| `reason` | `String` | `"mainline_complete"`（引擎唯一来源） |
| `completed_node_ids` | `Vec<String>` | 完成的里程碑 id 列表（供 server 审计/选结局） |

**`RoundOutcome` 扩展** — `src/narrative/mod.rs:117-124`

| 字段 | 类型 | 用途 |
|---|---|---|
| `ending` | `Option<EndingSignal>` | **新增**。回合提交后若检测到主线走完则为 `Some`；否则 `None`。默认 `None`，不影响 blocked/预算路径 |

**进度存储（无新字段，用现成 world KV）** — `NarrativeState.world: BTreeMap<String,Value>`
键：`milestoneProgress_<nodeId>` → `Value::Number`（f64）。经 `PatchOp::Increment` 累加（`reducer.rs:440-447`）。

### server 侧（Rust，`server/`）

**`RoomEndgamePolicy`（新增）** — `server/src/runtime/mod.rs`（新结构）

| 字段 | 类型 | 用途 |
|---|---|---|
| `enabled` | `bool` | 仅 `room_type=='idle'` 为 true；否则终局逻辑全跳过 |
| `max_world_ticks` | `i64` | 世界时间上限（回退口径：`world_ticks.tick_no` 计数）。达到即终局 |
| `min_world_ticks` | `i64` | **终局地板**（默认 3）。任何终局在 `tick_no < min` 前一律不触发——第二道防秒结束 |
| `key_character_ids` | `Vec<String>` | 关键角色 id；其永久退场触发终局 |
| `world_time_limit` | `Option<i64>` | **与第二块世界时钟集成点**：若 block-2 世界时钟就绪，用游戏时间上限；`None` 时回退到 `max_world_ticks` |

来源：seed 自 `world_templates.skeleton_json` 的新 `endgame` 对象（弱类型 raw 读，见下），叠加 `worlds` 行的 `room_type`。

**skeleton `endgame` 对象（模板 JSON，无迁移）** — `world_templates.skeleton_json`

```jsonc
"endgame": {
  "maxWorldTicks": 120,
  "minWorldTicks": 3,
  "keyCharacterIds": ["heroine"],
  "worldTimeLimit": null          // 预留 block-2 世界时钟
}
```

`mainlineNodes[]` 每项扩展（seed 读）：`{ id, summary, constraint, threshold, advanceWhen, milestone:true }`。

**worlds 表** — 无 schema 改动；复用既有 `status` 枚举写入 `'ended'`（`migrations/0001_init.sql:110`）。

## 改动文件清单

### 1. `crates/muse-engine/src/narrative/types.rs`
- OutlineNode（`:73-80`）加 `threshold/advance_when/weights` 三个 `#[serde(default)]` 字段。
- 新增 `IntensityWeights`（`Default` impl 给默认权重）、`EndingSignal` 两个结构。
- **不改** DomainEventType（`:186-193`）——终局事件由 server 侧 `world_events` 承载（对齐 arena 做法），避免动引擎事件枚举的爆炸面。（可选增强：加 `WorldEnded` 变体，列为 Phase 3。）

### 2. `crates/muse-engine/src/narrative/mod.rs`
- **`build_patch`（`:508-550`）重写节点推进段**：
  - 保留 pacingNotes 追加（`:525-532`）不变。
  - `progressed` 计算保留，仅用于"旧式节点"（`threshold==None`）路径的向后兼容。
  - 新增 `round_intensity(decisions, outcomes, weights) -> f64`（本文件新私有 fn）。
  - `next_pending` 命中的节点若 `threshold.is_some()`：生成一条 `Increment world.milestoneProgress_<id> += Δ`；本地计算 `new = cur + Δ`；`advance_when` 谓词命中（复用 `eval_predicate`，构造临时 `ForbiddenPredicate`）且 `new >= threshold` 时追加 `Set outlineNodes[id].status = done`。否则不翻转。
  - `threshold.is_none()` → 走原逻辑（`progressed => set done`）。
- **`RoundOutcome`（`:117-124`）加 `ending` 字段**；所有构造点（blocked 短路 `:264`、不变量违规 `:322`、正常提交 `:367`）显式填 `ending`。仅正常提交路径在 `commit_scene`（`:354`）之后调用新 fn `detect_mainline_complete(&new_state)` 得到 `Option<EndingSignal>`；blocked/违规路径恒 `None`。
- 新增私有 fn `detect_mainline_complete(state) -> Option<EndingSignal>`：见核心算法，**带里程碑非空守卫**。

### 3. `crates/muse-engine/src/narrative/constraints.rs`
- 无逻辑改动。`eval_predicate`（`:138`）、`parse_predicate`（`:133`）、`next_pending`（`:225-227`）被 build_patch 复用。
- （可选）暴露一个 `eval_expression(state, expr) -> Result<bool>` 薄封装，免得 build_patch 手搓临时 ForbiddenPredicate。

### 4. `crates/muse-engine/src/narrative/reducer.rs`
- **零改动**。进度用既有 `world.<key>` Increment（`:440-447`，键校验 `:48-55`），状态用既有 `outlineNodes[id].status`（`:89-101`）。新节点配置字段不入 patch → parse_path 白名单不需扩展。

### 5. `server/src/runtime/mod.rs`
- **`seed_narrative_layer`（`:348-427`，mainlineNodes 段 `:386-410`）**：读 `node.threshold`（f64）、`node.advanceWhen`（str，`parse_predicate` 校验，非法则丢弃谓词但保留节点为纯阈值门）、并把节点 `constraint` 缺省从 `Soft` 保持；校验 `node.id` 无 `.`/`[`（否则进度键非法，跳过该节点并 warn）。填入扩展后的 `OutlineNode`。
- **新增 `load_endgame_policy(db, world) -> RoomEndgamePolicy`**：读 `room_type` + skeleton `endgame` raw；非 idle 房 `enabled=false`。
- **新增 `end_world(tx, world_id, reason, ending_id)`**（镜像 `pause_world` `:468-475`）：`UPDATE worlds SET status='ended', updated_at=? WHERE id=? AND status='running'`（幂等：非 running 则 rows=0，不重复结算）；写一条 `world_events` 终局行（public 可见）+ audit。
- **`commit_tick`（`:884-978`）在 CAS 成功后、`tx.commit`（`:947`）之前**插入终局评估段（见核心算法）：综合 `outcome.ending`（引擎主线信号）、世界时间上限、关键角色退场，且过 `min_world_ticks` 地板 + `policy.enabled` 门，命中则事务内 `end_world`。终局与状态提交**同事务**保证原子性。
- `schedule_due_ticks`（`:202`）已 `WHERE status='running'`，ended 世界自动停排；遗留 tick 命中 `world_not_running`（`:659`）noop——**无需改**，仅在注释标注 ended 归入此路径。
- `TickStatus`（`:73-84`）**加 `Concluded` 变体**（可观测/测试用），`commit_tick` 终局时返回 `TickStatus::Concluded` 而非 `Done`。

### 6. `server/src/runtime/tests.rs`
- 新增终局用例（见测试影响）。

## 核心算法

### A. 回合强度累积 + 阈值推进（引擎）

`crates/muse-engine/src/narrative/mod.rs:508-550` `build_patch` 内，替换 `:533-542` 的推进块：

```rust
// mod.rs (build_patch, 替换 :533-542)
fn round_intensity(decisions: &[RoleDecision],
                   outcomes: &[ArbiterOutcome],
                   w: &IntensityWeights) -> f64 {
    let mut e = 0.0;
    for o in outcomes {
        e += match o.result {
            ArbiterResult::Success        => w.success as f64,
            ArbiterResult::PartialSuccess => w.partial as f64,
            ArbiterResult::Failure        => w.failure as f64,
            _ /* Invalid|Blocked */       => 0.0,
        };
    }
    for d in decisions {
        if d.speak.will_speak { e += w.speak as f64; }   // 互动强度
    }
    e
}

// —— 推进段 ——
if let Some(node) = constraints::next_pending(&state.narrative.outline_nodes) {
    match node.threshold {
        Some(threshold) => {                                  // 阈值里程碑
            let w = node.weights.clone().unwrap_or_default();
            let delta = round_intensity(decisions, outcomes, &w);
            let key = format!("milestoneProgress_{}", node.id); // 单段键，合规 (reducer.rs:48-55)
            let cur = state.world.get(&key).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let next = cur + delta;

            if delta > 0.0 {                                  // 只在有强度时累加
                operations.push(PatchOperation {
                    op: PatchOp::Increment,
                    path: format!("world.{key}"),
                    value: Some(json!(delta)),
                    precondition: None,
                });
            }
            let gate_ok = match &node.advance_when {
                None => true,
                Some(expr) => constraints::eval_predicate(
                    state,
                    &ForbiddenPredicate { id: String::new(),
                                          expression: expr.clone(),
                                          reason: String::new() },
                ).unwrap_or(false),                           // 谓词非法/实体缺失 => 未命中
            };
            if next >= threshold as f64 && gate_ok {
                operations.push(PatchOperation {
                    op: PatchOp::Set,
                    path: format!("narrative.outlineNodes[{}].status", node.id),
                    value: Some(json!("done")),
                    precondition: None,
                });
            }
        }
        None if progressed => {                               // 旧式节点：向后兼容
            operations.push(PatchOperation {
                op: PatchOp::Set,
                path: format!("narrative.outlineNodes[{}].status", node.id),
                value: Some(json!("done")),
                precondition: None,
            });
        }
        None => {}
    }
}
```

推进语义：
- **每回合最多推进首个 Pending 节点**（保留 5–8 里程碑的顺序节拍）。
- 关系维度经 `advance_when` 谓词表达（`relations[a->b].affinity > x`，`eval_predicate` 的 `RelNumCmp` 分支 `constraints.rs:~163` 已支持 `trust/affinity/fear/debt` + `> < ==`）；事件维度经 `threshold` 累积。二者**与**关系（`next>=threshold && gate_ok`）= "关系/事件强度累积到阈值"。
- 确定性：`delta` 只依赖已定序的 decisions/outcomes（`mod.rs:234,254` 已排序），`cur` 读自权威 state，纯函数，replay 可复现。

### B. 主线走完检测（引擎，带防秒结束守卫）

`mod.rs`（`commit_scene` 之后 `:354`）：

```rust
fn detect_mainline_complete(state: &NarrativeState) -> Option<EndingSignal> {
    // 只统计「里程碑节点」= 带 threshold 的软节点；chapter/arena 的硬节点 threshold=None 不计入
    let milestones: Vec<&OutlineNode> = state.narrative.outline_nodes.iter()
        .filter(|n| n.threshold.is_some())
        .collect();
    if milestones.is_empty() {                 // ★ 防秒结束守卫①：空里程碑集 => 永不"完成"
        return None;
    }
    let all_done = milestones.iter()
        .all(|n| matches!(n.status, NodeStatus::Done | NodeStatus::Bypassed));
    if !all_done { return None; }
    Some(EndingSignal {
        reason: "mainline_complete".into(),
        completed_node_ids: milestones.iter().map(|n| n.id.clone()).collect(),
    })
}
```

空 skeleton（`outline_nodes=[]`）或无阈值节点 → `milestones` 空 → 恒 `None` → 引擎永不误报完成。这是**第一道**防秒结束。

### C. 终局评估（server，CAS 后 / commit 前）

`server/src/runtime/mod.rs` `commit_tick`，插在 CAS 成功（`:917`）之后、`tx.commit`（`:947`）之前：

```rust
// commit_tick 内，事务 tx 仍开着
let policy = load_endgame_policy(&state.db, world_id).await?;  // 或调用前预取
let mut ending: Option<(&str, Option<String>)> = None;        // (reason, ending_id)

if policy.enabled && tick_no >= policy.min_world_ticks {       // ★ 防秒结束守卫②：地板
    // (1) 引擎主线信号
    if let Some(sig) = &outcome.ending {
        ending = Some(("mainline_complete", select_ending(&state, world_id, sig).await?));
    }
    // (2) 世界时间上限（block-2 世界时钟优先，回退 tick_no 计数）
    else if reached_time_limit(&policy, tick_no /*, world_clock */) {
        ending = Some(("time_limit", select_ending_default(world_id).await?));
    }
    // (3) 关键角色退场（成员表 / 已落定 permanent_exit）
    else if key_character_exited(&mut tx, world_id, &policy).await? {
        ending = Some(("key_character_exit", select_ending_default(world_id).await?));
    }
}

let final_status = if let Some((reason, ending_id)) = ending {
    end_world_tx(&mut tx, world_id, reason, ending_id).await?;  // UPDATE ... status='ended'
    TickStatus::Concluded
} else {
    TickStatus::Done
};
// … 既有 world_ticks done / events / budget 落库不变 …
tx.commit().await?;
// 若 Concluded：提交后可复用 reports::generate_report 生成终局日报（:970-975 现成）
Ok(final_status)
```

- `reached_time_limit`：`policy.world_time_limit` 为 `Some` 时比游戏时钟（block-2），否则 `tick_no >= policy.max_world_ticks`。这就是**与第二块世界时间的结合点**——接口预留，block-2 落地后仅换比较量，不动结构。
- `key_character_exited`：查 `world_members.status='left'/'retired'` 或已 landed 的 `permanent_exit` consent，命中任一 `key_character_ids`。
- **幂等**：`end_world_tx` 的 `WHERE status='running'` 保证重复 tick / 并发只结算一次（对齐 arena settle、chapters grantedHookIds 幂等模式）。
- 终局奖励须过 arena 红线（非强度、无买判定，镜像 `arena/mod.rs:529-559` 与 arena 红线测试）——放置房终局若发奖，走 `arena_rewards` 同类只记荣誉的旁路，Phase 3 落地。

## 测试影响

### 破坏（需同步修）

- **引擎 `RoundOutcome` 构造点**：加 `ending` 字段后，所有内联构造（`mod.rs:264` blocked、`:322` 违规、`:367` 提交）编译报错，须补 `ending: None`（前两者）/ 计算值（后者）。
- **引擎 happy_path（`mod.rs:949`）**：其硬节点 `threshold=None` → 走旧 `progressed=>done`，断言 `outline_nodes[0].status==Done`（`:976`）**仍成立**，但需确认 `ending==None`（该测试节点无 threshold → detect 返回 None）。加一行断言即可，不改行为。
- **引擎其余 run_round 测试**（预算 `:996`、blocked `:1014`、不变量 `:1040`、同意 `:1075/:1142`、取消 `:1185`）：`ending` 默认 `None`，均补字段后通过；`round_intensity` 只在阈值节点触发，这些用例节点无 threshold → 零行为变化。
- **server `commit_tick` 返回值**：新增 `TickStatus::Concluded`，`process_tick_inner`/`worker_loop` 的匹配需加分支（当作成功终态，不重试）。
- **server `tick_runs_full_round`（`runtime/tests.rs:232`）**：其 skeleton 节点为 hard fated（`threshold=None`）→ 非 idle 或无阈值 → `policy.enabled=false`/引擎不发信号 → 世界继续 running，revision 累积断言（`:296`）**不破**。若该测试世界 `room_type` 恰为 idle 需显式设 `max_world_ticks` 足够大或 `enabled=false`。

### 新增（引擎）

1. 阈值累积：连续 N 回合 `Success` 使 `milestoneProgress_<id>` 单调增，`next<threshold` 时节点保持 Pending、`>=threshold` 时翻 Done。
2. `advance_when` 门：progress 达标但关系谓词未命中 → 不翻转；关系达标后同回合翻转。
3. 谓词非法/关系实体缺失 → `gate_ok=false`（`eval_predicate` 返 false），不误推进。
4. 每回合仅推进首个 Pending（多里程碑顺序性）。
5. `detect_mainline_complete` 空里程碑集 → `None`（**防秒结束单测**）；全 Done → `Some`；混入 Pending → `None`。
6. 旧式节点（`threshold=None`）回归：`progressed=>done` 不变。

### 新增（server）

7. idle 房全里程碑 Done → `end_world` 置 `ended` + `TickStatus::Concluded`；ended 后 `schedule_due_ticks` 不再排 tick；遗留 tick → `world_not_running` noop。
8. 世界时间上限：`tick_no >= max_world_ticks` → ended（主线未走完也终止）。
9. 关键角色退场 → ended。
10. **防秒结束地板**：空 skeleton 的 idle 房，`tick_no < min_world_ticks` 恒不 ended；空 skeleton 永不因"主线完成"结束（守卫①），只可能在 `max_world_ticks` 到点结束（守卫②兜底）。
11. 幂等：并发/重复 tick 只结算一次（`end_world` rows=0 校验）。
12. 非 idle 房（chapter/arena）：`policy.enabled=false`，终局逻辑全跳过，既有 cleared/concluded 旁路不受影响。

## 依赖

- **第二块（异步时间线 / 世界时钟）**：终局条件 (2) "世界时间到上限"通过 `RoomEndgamePolicy.world_time_limit` + `reached_time_limit` 预留接口对接。block-2 未落地时用 `world_ticks.tick_no` 计数回退，本块**可独立发布**、不硬依赖 block-2。
- **第一块（世界固有角色 / NPC）**：条件 (3) "关键角色退场"若关键角色是 NPC（无 owner）而非玩家成员，退场判定需读 NPC 状态源；本块先支持玩家成员 `key_character_ids`，NPC 变体待第一块的 owner-less subject 门控模型确定后接入。
- **装配层 `enabled_endings`（`assembly/mod.rs`）**：`select_ending` 从实例已启用结局池选一个，复用现成加权结果（`weight_endings` 保底至少一个可用结局）。
- **consents / arena 红线**：终局发奖须复用 arena 荣誉奖励旁路与红线约束。
- **reports**：终局日报复用 `commit_tick` 现成 `reports::generate_report`（`:970-975`）。

## 风险

1. **真空完成（秒结束）** — 已双守卫：引擎侧空里程碑集恒不发信号（守卫①）；server 侧 `min_world_ticks` 地板（守卫②）。风险降为低。**残留**：模板作者把所有 `threshold` 设为 0 → 第 `min_world_ticks` tick 一次性全达标结束；缓解——seed 时校验 `threshold > 0`，否则退化为大默认值 + warn。
2. **进度键冲突/非法** — `world.<key>` 单段约束（`reducer.rs:48-55`）要求 nodeId 无 `.`/`[`。缓解：seed 校验 id 合法，非法节点跳过并 warn。与保留键 `appliedPatchIds`（`reducer.rs:28`）用固定前缀 `milestoneProgress_` 隔离。
3. **确定性回归** — `round_intensity` 依赖 outcomes/decisions 顺序；二者在 run_round 已定序（`mod.rs:234,254`），保持确定性。新增须纳入现有确定性测试范式。
4. **CAS/事务边界** — 终局 `UPDATE worlds` 与状态 CAS 同事务（同一 `tx`），避免"状态提交成功但终局丢失"或反之的裂缝；`end_world` 幂等 `WHERE status='running'` 兜住并发。
5. **误伤非 idle 房** — `policy.enabled` 严格门在 `room_type=='idle'`；阈值逻辑严格门在 `threshold.is_some()`。chapter 硬节点 / arena 吃鸡零影响。
6. **关系强度不随回合变动** — 当前引擎 outcomes 不写 relations，`advance_when` 读到的关系多为 seed/外部（whisper/干预）值；"关系累积"主要靠 seed + 外部注入，而非回合内自动漂移。已在设计中明确：事件强度走 threshold 累积，关系走谓词门。若后续要回合内关系漂移，另开工。

## 渐进式分步落地

**Phase 0（server-only，最小、立即防秒结束 + 保证可终止）**
- 加 `RoomEndgamePolicy` + `load_endgame_policy` + `end_world_tx` + `TickStatus::Concluded`。
- `commit_tick` 只接条件 (2) 世界时间上限（`tick_no >= max_world_ticks`）+ `min_world_ticks` 地板 + idle 门。
- 引擎零改动。上线后：任意 idle 房必在 `max_world_ticks` 终止，空 skeleton 不再无限跑也不秒结束。测试 7(时间上限分支)/10/11。

**Phase 1（引擎阈值推进，behind `threshold.is_some()`）**
- OutlineNode 加 `threshold/advance_when/weights` + `IntensityWeights`；`build_patch` 阈值累积（算法 A）；`seed_narrative_layer` 读新字段。
- 旧式节点走兼容路径，happy_path 等回归零变化。测试 1–6。

**Phase 2（引擎主线信号 + server 消费）**
- `RoundOutcome.ending` + `detect_mainline_complete`（算法 B，守卫①）；`commit_tick` 接条件 (1) + `select_ending`。测试 5、7(主线分支)、12。

**Phase 3（关键角色退场 + 终局产出）**
- 条件 (3) `key_character_exited`；终局日报 / 荣誉奖励（过 arena 红线）；可选 `DomainEventType::WorldEnded`。测试 9、终局奖励红线。

每 Phase 独立可测、可回滚；无大爆炸重写；reducer/commit 原子契约全程不动。

## 工作量与影响面估计

| 模块 | 改动 | 规模 | 影响面 |
|---|---|---|---|
| `narrative/types.rs` | OutlineNode +3 字段、+2 结构 | ~40 行 | 低（纯加字段，serde default 向后兼容） |
| `narrative/mod.rs` | build_patch 推进段重写、round_intensity、detect_mainline_complete、RoundOutcome +字段 | ~90 行 | 中（build_patch 是热点，但改动局部；构造点须补字段） |
| `narrative/reducer.rs` | 0 | 0 | 无（复用现成路径） |
| `narrative/constraints.rs` | 可选 eval 薄封装 | ~10 行 | 低 |
| `runtime/mod.rs` | policy/seed/end_world/commit_tick 终局段/TickStatus | ~150 行 | 中（commit_tick 事务内插入，须保 CAS 原子性） |
| `runtime/tests.rs` | 新增 6+ 用例 | ~250 行 | 测试 |
| 引擎测试补字段 | RoundOutcome 构造点 | ~10 处 1 行 | 机械 |

**总量**：引擎 ~140 行产品代码 + server ~150 行 + 测试 ~350 行。**净新增为主，重写仅 build_patch 一段（~10 行→~40 行）**。无迁移（skeleton_json/worlds 均复用）。核心不变量（reducer 白名单、commit CAS、确定性排序）零改动，回归风险集中在 build_patch 局部与 commit_tick 事务插入两点，均有对应新测试兜底。整体工作量：**2–3 人日**（按 Phase 拆分可并行/分批交付）。
