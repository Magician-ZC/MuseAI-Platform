# 放置房软主线示例 skeleton（Idle-Room Soft Mainline Example）

> P1 放置房终局配套样例。演示一条 6 里程碑的软主线：**推进 = 事件强度累积到 `threshold` ∧ 关系强度谓词 `advanceWhen` 命中**；**终局 = 主线全里程碑 Done ∨ 世界时间到上限 ∨ 关键角色退场**。
>
> 本文档 JSON 与 `server/src/runtime/tests.rs::example_idle_skeleton()` 镜像同一份，并由测试
> `example_idle_skeleton_seeds_valid_milestones` 守护「可加载 + 结构合法」，防样例腐化。

## 用法

把下面的 JSON 作为 `world_templates.skeleton_json` 建一个 `room_type='idle'` 的模板。`mainlineNodes`
的每个节点是一个**里程碑**（带 `threshold`），`endgame` 段配置终局策略。

```jsonc
{
  "mainlineNodes": [
    { "id": "firstMeeting",  "summary": "初次照面：两人第一次在同一空间独处", "constraint": "soft", "threshold": 2.0 },
    { "id": "smallTalk",     "summary": "日常寒暄累积成习惯",                 "constraint": "soft", "threshold": 3.0, "advanceWhen": "relations[heroine->player].affinity > 0.2" },
    { "id": "sharedSecret",  "summary": "有人先卸下防备，交换一个秘密",       "constraint": "soft", "threshold": 4.0, "advanceWhen": "relations[heroine->player].trust > 0.4" },
    { "id": "conflict",      "summary": "一次误会让关系出现裂痕",             "constraint": "soft", "threshold": 4.0, "advanceWhen": "relations[player->heroine].affinity > 0.5" },
    { "id": "reconcile",     "summary": "裂痕后的和解，关系更进一步",         "constraint": "soft", "threshold": 5.0, "advanceWhen": "relations[heroine->player].trust > 0.6" },
    { "id": "turningPoint",  "summary": "面对去留的抉择，主线收束",           "constraint": "soft", "threshold": 6.0, "advanceWhen": "relations[heroine->player].affinity > 0.7" }
  ],
  "forbiddenPredicates": [],
  "endgame": {
    "minWorldTicks": 5,
    "maxWorldTicks": 240,
    "keyCharacterIds": ["heroine"],
    "worldTimeLimit": null
  }
}
```

## 字段说明

### `mainlineNodes[]`（里程碑节点）

| 字段 | 类型 | 含义 |
|---|---|---|
| `id` | string | 节点 id。**必须无 `.` / `[`**（进度键 `world.milestoneProgress_<id>` 需单段合法），否则该里程碑被 seed 跳过并 warn。 |
| `summary` | string | 一句话描述，供入场导演设局与日报。 |
| `constraint` | `"soft"` | 放置房里程碑恒为软节点（不触发硬约束 Blocked）。 |
| `threshold` | number(>0) | **事件强度累积阈值**。`Some` 标识「阈值里程碑」，走累积推进；缺省则退化为老式「有成功就推进」的兼容节点。 |
| `advanceWhen` | string(可选) | **关系强度谓词门**（受限 DSL）。达到 `threshold` 且该谓词命中才 `Pending→Done`。谓词语法非法 → 丢弃谓词、退化为纯阈值门（不 fail-closed）。 |

**推进语义**：每回合把本回合强度 `Δ`（Σ outcome 折算 + Σ willSpeak 互动）累加到当前首个 Pending 里程碑的
`world.milestoneProgress_<id>`；`累积 >= threshold` **且** `advanceWhen` 命中时翻 `Done`。每回合**至多推进一个**
里程碑（保持顺序节拍）。事件维度走 `threshold` 累积，关系维度走 `advanceWhen` 谓词，二者「与」。

**`advanceWhen` DSL**（关系数值比较，见 `crates/muse-engine/src/narrative/constraints.rs`）：

```
relations[<from>-><to>].<field> <op> <num>
  <field> ∈ { trust, affinity, fear, debt }
  <op>    ∈ { <, >, == }
```

- `from` / `to` 为世界内固定角色 id（本例的 `heroine` / `player`）。
- 引用的关系实体缺失 → 谓词「未命中」（`false`），不误推进。
- 关系值主要由 seed / 外部注入（托梦、干预）驱动；引擎回合默认不自动漂移关系强度。

### `endgame`（终局策略，`room_type='idle'` 生效）

| 字段 | 类型 | 含义 |
|---|---|---|
| `minWorldTicks` | int(默认 3) | **防秒结束地板**：任何终局在 `tick_no < min` 前一律不触发。 |
| `maxWorldTicks` | int(默认 120) | 世界时间上限（回退口径：`world_ticks.tick_no` 计数）。到点即终局，**兜底保证任意 idle 房必终止**。 |
| `keyCharacterIds` | string[] | **关键角色 id**（`cloud_character_id`）。其永久退场——成员表 `left`/`retired`，或已 landed 的 `permanent_exit` consent——触发终局。 |
| `worldTimeLimit` | int/null | 与 P2 世界时钟集成点：`Some` 时另按 `game_time >= limit` 判终局；`null` 回退 `maxWorldTicks`。 |

## 三条终局路径

1. **主线走完**：全部里程碑 `Done`（引擎 `is_terminal → MainlineDone`；空里程碑集恒不触发——防秒结束守卫①）。
2. **世界时间上限**：`tick_no >= maxWorldTicks`（或 `game_time >= worldTimeLimit`）。
3. **关键角色退场**：`keyCharacterIds` 任一角色永久退场（先于 `insufficient_members` 门评估，避免离场使在场跌破 2 而卡死）。

命中任一（且过 `minWorldTicks` 地板、`room_type=='idle'`）→ 世界 `status='ended'` 停机；同事务写终局审计，
从装配层 `enabledEndings` 选定结局（`select_ending`，`weight_endings` 保底 ≥1），落成**荣誉奖励**
（走 arena 红线：只记荣誉、非战力、无买判定），并复用 `reports::generate_report` 产出终局日报。
