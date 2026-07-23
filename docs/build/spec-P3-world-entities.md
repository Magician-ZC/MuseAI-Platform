I have thorough grounding in the real code. Here is the implementation spec.

---

# 实现规格：世界固有角色(NPC/反派) + 地点/秘境维度 + 原著道具

> 目标工程：`/Users/zc/Desktop/py_project/MuseAI-Platform/`（引擎 crate `muse-engine` + server）。所有 file:line 均已核对当前代码。

## 概述

三个维度是一套耦合的改动，共享同一条落地缝：**skeleton_json 预审核内容池 → 装配钉住 → runtime 每 tick 回灌进引擎 RoundInput → run_round 决策/仲裁/写作**。

- **worldCharacters（世界固有角色）**：原著提取的 NPC/反派卡，复用 `CharacterCardV2`（`crates/muse-engine/src/character/types.rs:264`），存 `skeleton_json.worldCharacters[]`。runtime 在成员卡装配后（`server/src/runtime/mod.rs:750` 之后）追加进 `active_cards` / `other_brief`，**但不进 `members_projection`**（无 owner、不投影日报）。引擎几乎零改动即可让 NPC 全程 role_decide/碰撞；唯一必须的引擎改动是**同意门控豁免**（NPC 无主人可授权，`crates/muse-engine/src/narrative/mod.rs:614-677` 的 gate 会误伤 NPC 死亡）。反派"主动议程"通过其卡的 `dramaticCore`/`agency.plotSeeds` 天然驱动决策，无需引擎特判。

- **locations（地点/秘境维度）**：地点图 `{id,name,connections[],admission?,residentItems[]}` 存 `skeleton_json.locations[]`。角色动态位置作为 `CharacterState.location`（新字段）走 reducer 落定；"移动到地点"是一种普通行动，合法性（连通 + 准入）在仲裁新增规则里判。碰撞按同 location 判定：导演分组设局、决策 others 按同场过滤、仲裁 R2 / 不变量 I3 的"在场"从"active 全集"收窄为"同 location 子集"。**秘境 = 带 admission 门槛 + 隐藏 residentItems 的特殊地点**，准入判定复用 `admission::check_admission` 的 cosmology/power_tier 语义（`server/src/admission/mod.rs:103-147`）扩展为 LocationGate。

- **worldItems（原著固有道具）**：道具目录 `skeleton_json.worldItems[]`，元素即 `admission::ItemDefinition`（`server/src/admission/mod.rs:18-23`）。与 `hiddenContentPool[].rewardItem`（`server/src/assembly/mod.rs:123`，已是同类型）统一——把内联 `reward_item` 改为对目录的 `rewardItemRef: String` 引用，单一事实源。道具分布到地点（`Location.residentItems`）/NPC（NPC 卡的 `worldAdaptation` 或装配层 item_distribution）。

- **提取管线扩展**：复刻 muse-engine `ExtractionPipeline`（`crates/muse-engine/src/character/mod.rs:61`）的任务化多阶段模式，新增"世界骨架提取"产出扩展版 Skeleton（含 worldCharacters/locations/worldItems + 已有的 mainlineNodes/endingPool），人工确认后经预审核门发布进 `world_templates.skeleton_json`。

**设计原则**：地点图与固有角色卡是**静态模板数据**，每 tick 由 caller（runtime）随 RoundInput 传入引擎（与 `active_cards`/`fragments` 同款"调用方组装、后端无状态"，`RoundInput` 定义 `crates/muse-engine/src/narrative/mod.rs:68-87`）；只有角色的**动态位置** `CharacterState.location` 进 NarrativeState 走 reducer/CAS。此选择把 reducer 白名单与五层状态契约的改动压到最小（只加一个字段一条路径）。

---

## 数据结构

### 引擎侧（muse-engine）

**`CharacterState.location`** · `crates/muse-engine/src/narrative/types.rs:21-36`
- 新增字段 `#[serde(default)] pub location: String`（空串 = 无地点/全局场景，向后兼容）。用途：角色动态位置，碰撞分组的唯一动态依据，movement 行动的落定目标。

**`LocationDef`（新结构）** · `crates/muse-engine/src/narrative/types.rs`（新增，紧邻 `OutlineNode`）
```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocationDef {
    pub id: String,
    pub name: String,
    #[serde(default)] pub connections: Vec<String>,   // 可直达的地点 id
    #[serde(default)] pub is_secret_realm: bool,       // 秘境标记（影响 others 可见性）
    #[serde(default)] pub gate: Option<LocationGate>,  // 准入门槛（秘境用）
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocationGate {
    #[serde(default)] pub required_item_ids: Vec<String>,   // 需持有的 worldItem id
    #[serde(default)] pub required_effect_tags: Vec<String>,// 需具备的 effect_tag
    #[serde(default)] pub required_cosmologies: Vec<String>,// 复用 KNOWN_COSMOLOGIES
    #[serde(default)] pub max_power_tier: Option<u8>,
}
```
用途：地点图节点 + 秘境准入。**静态**，不进 NarrativeState，随 RoundInput 传入。

**`RoundInput.locations` / `RoundInput.world_controlled`** · `crates/muse-engine/src/narrative/mod.rs:68-87`
- 新增 `#[serde(default)] pub locations: BTreeMap<String, LocationDef>`：本回合地点图（空 = 退化为当前单一全局场景，向后兼容）。
- 新增 `#[serde(default)] pub world_controlled: Vec<String>`：世界固有角色 id 集合。用途：同意门控豁免（这些 subject 的不可逆结果不需要 owner 授权，等价于自动 approved），并可用于成本/日志区分。`Default` impl（`mod.rs:89-105`）补两个空值。

**`CharacterCardV2` controller 标记** · `crates/muse-engine/src/character/types.rs:74-89`（`Identity`）
- `narrative_role: Option<String>` 已存在（`types.rs:81`），可承载 `"villain"`/`"npc"`。但 **controller='world' 语义不塞进卡**——它是"这张卡由谁控制"的运行时归属，应由 caller 通过 `RoundInput.world_controlled` 表达，避免污染可云化/可复用的卡资产。卡里仅 `importance`（`types.rs:82`，Core/Major/Functional）用于分层，反派 = Core + narrative_role="villain"。

### Server 侧（skeleton_json 内容池，`server/src/assembly/mod.rs`）

**`Skeleton` 新增三字段** · `server/src/assembly/mod.rs:64-79`
```rust
#[serde(default)] world_characters: Vec<WorldCharacter>,  // NPC/反派卡目录
#[serde(default)] locations: Vec<LocationSpec>,            // 地点图 + 秘境
#[serde(default)] world_items: Vec<ItemDefinition>,        // 道具目录（单一事实源）
```
均 `#[serde(default)]`，向后兼容（`load_skeleton` 用 `unwrap_or_default`，`assembly/mod.rs:323-333`）。

**`WorldCharacter`（新）** · `server/src/assembly/mod.rs`
```rust
struct WorldCharacter {
    card: CharacterCardV2,       // 复用引擎卡（assembly 已 import，:24）
    #[serde(default)] home_location: String,   // 初始 location
    #[serde(default)] carried_item_ids: Vec<String>,  // 引用 world_items[].id
    #[serde(default)] agenda_nodes: Vec<String>, // 反派主动议程绑定的 mainline 节点
}
```

**`LocationSpec`（新）** · `server/src/assembly/mod.rs`
```rust
struct LocationSpec {
    id: String, name: String,
    #[serde(default)] connections: Vec<String>,
    #[serde(default)] is_secret_realm: bool,
    #[serde(default)] gate: Option<LocationGateSpec>, // 与引擎 LocationGate 同形
    #[serde(default)] resident_item_ids: Vec<String>, // 引用 world_items[].id
}
```

**`PoolItem.reward_item` → `reward_item_ref`** · `server/src/assembly/mod.rs:109-124`
- 现状 `reward_item: Option<ItemDefinition>`（`:123`，内联定义）。改为 `#[serde(default)] reward_item_ref: Option<String>`（指向 `world_items[].id`）。装配时解引用后仍填 `CharacterHook.reward_item: Option<ItemDefinition>`（`assembly/mod.rs:49`），**下游 `chapter_finish`/`grant_item_tx` 完全不改**（只认 `ItemDefinition`）。兼容期保留内联 `reward_item` 为 fallback。

**`AssembledInstance` 新增输出** · `server/src/assembly/mod.rs:28-38`
```rust
#[serde(default)] world_character_entries: Vec<WorldCharacterEntry>, // {characterId, card, location, carriedItems}
#[serde(default)] location_graph: Vec<LocationDef>,                  // 装配后钉住的地点图
```
用途：随 `worlds.assembled_json` 钉住（`assembly/mod.rs:287-292` 的 wrapper），runtime 每 tick 读回。

---

## 改动文件清单

### 引擎 crate（muse-engine）

**`crates/muse-engine/src/narrative/types.rs`**
- `CharacterState`（`:21-36`）加 `location` 字段。
- 新增 `LocationDef` / `LocationGate` 结构（见上）。

**`crates/muse-engine/src/narrative/reducer.rs`**
- `parse_path`（`:60-76`）：`characters.<id>.<field>` 的 `match field` 白名单（`:66-68`）加 `"location"` 分支 → `ParsedPath::Character{id,field}`。
- `apply_character`（被 `:193` 调用）：加 `"location"` → `apply_str`（Set 单值，非 list）。当前 character 字段处理需确认 location 走标量 Set 而非 list append；新增一个 `apply_scalar_str` 分支或复用 `arcStage` 的标量处理路径。
- **不动** `validate_and_apply` 的 CAS/幂等/禁止谓词后校验（`:137-169`），契约不变。

**`crates/muse-engine/src/narrative/mod.rs`** —— 核心
- `RoundInput`（`:68-87`）加 `locations` / `world_controlled`；`Default`（`:89-105`）补空值。
- `run_round`（`:138`）：
  - `active_ids`（`:156`）保留为全体定序。**新增按 location 分组**：`groups: BTreeMap<String, Vec<String>>`，key 为地点 id，用回合起始 `current.characters[id].location` 归组（`locations` 为空时退化为单组 `""`）。
  - 成本公式（`:159`）：`calls = active_ids.len() + 组数*2 + 2`（每组 1 导演 + 1 写作，仲裁/审校仍全局各 1）——见核心算法。同步改 `estimate`（`:127-134`）。
  - 导演段（`:184-196`）：`call_director` 从"全局单一 situation"改为"逐组设局"，返回 `situations: BTreeMap<location, String>`。
  - 决策段（`:210-234`）：`assemble_visible_context` 调用（`:219-221`）传该角色所在组的 `situation` + **同组 other_brief 子集**（others 按同 location 过滤）。
  - 仲裁 `rule_arbitrate`（`:237`）：传入的 `active` 集参数改为"同组在场"（R2 判定，见 arbiter.rs）。
  - 写作段（`:272-286`）：逐组写作（每组一个 SceneRecord 片段或合并，见风险）。
  - 门控 `gate_consents`（`:614-677`）：对 `subject ∈ world_controlled` 视为自动 approved，不产 ConsentRequested。
- `build_patch`（`:508-550`）：新增 movement op 生成——若某角色决策 action 被仲裁判为合法移动，追加 `characters.<id>.location` Set。progressed→推节点逻辑（`:533-542`）不变。
- `build_events`（`:554-612`）：movement 产 1 个 `ActionResolved`（fact 含 from/to location），无需新事件类型。
- `call_director`（`:401-444`）：签名加 `location_id` + 该地点的同场角色子集，user prompt（`:424-431`）注入"当前地点"。

**`crates/muse-engine/src/narrative/arbiter.rs`**
- `rule_arbitrate`（`:120` 起循环）：R2 在场判定（`:129`）的 `active` 改为"actor 同 location 子集"——跨地点 target 判 Invalid("目标不在场")。
- **新增 R6 移动合法性**（R5 之后）：若 decision.action 是移动意图（约定 targets 含 `loc:<id>` 或独立 move 字段），校验 `目标 ∈ 当前地点.connections` 且 `check_location_admission` 通过；否则 Invalid("无法抵达/未满足准入")。准入判定为纯函数，读 RoundInput.locations + 角色 resources/持有道具。

**`crates/muse-engine/src/narrative/decide.rs`**
- `assemble_visible_context`（`:44-116`）：`other_cards_brief`（`:87-89`）已按 caller 传入过滤，改为**由 run_round 传同场子集**即可；秘境（`is_secret_realm`）内角色对外部不可见、外部对秘境内不可见——分组时天然隔离。`targets` 白名单（`:353` 附近）随同场子集收窄。

**`crates/muse-engine/src/narrative/continuity.rs`**
- I3（`:53-73`）：`active` 参数（`:62,:68` 的 `active.contains`）改为"事件所属地点的同场集"。actor/target 在场判定按同场重定义。

### Server

**`server/src/assembly/mod.rs`**
- `Skeleton`（`:64-79`）加三字段 + 新结构 `WorldCharacter`/`LocationSpec`/`LocationGateSpec`。
- `assemble_instance`（`:194-319`）：主循环后新增装配段——解引用 `world_characters` → 过 `moderate_and_queue`（复用 `:221-232` 模式，kind=`"assembly_npc"`/`"assembly_location"`，仅 Approved 钉入）→ 填 `AssembledInstance.world_character_entries` / `location_graph`。`reward_item_ref` 解引用 `world_items` 目录填 `CharacterHook.reward_item`。
- `AssembledInstance`（`:28-38`）加两输出字段。

**`server/src/admission/mod.rs`**
- 新增纯函数 `check_location_admission(gate: &LocationGate, held_item_ids: &[String], held_tags: &[String], cosmologies: &[String], power_tier: u8) -> bool`，复用 `validate_cosmologies`（`:87-96`）+ cosmology/power 闸门语义（`:112-135`）。保持"不触库、可单测全分支"承诺（`:2-3`）。
- `LocationGate` 结构（server 侧镜像，或直接从 `assembly` 引用）。

**`server/src/runtime/mod.rs`** —— tick 组装
- active_cards 循环后（`:750` 之后、`:751` 门槛之前）：从 `world.assembled_json` 的 `assembly.worldCharacterEntries` 读 NPC 卡，`active_cards.insert(npc_id, card)` + `other_brief.insert(npc_id, name)`，**不 push 进 `members_projection`**（`:742`）。`active_cards.len() < 2` 门槛（`:751`）改为按玩家成员数或总活跃数（设计选择：NPC 计入活跃即可满足，但需防"只有 NPC 无玩家"空跑——加 `member_ids.is_empty()` 短路）。
- 组装 `world_controlled: Vec<String>` = NPC id 集合，传入 `RoundInput`。
- 组装 `locations: BTreeMap<..>` = 从 `assembly.locationGraph` 读，传入 `RoundInput`。
- `build_seed_state`（`:318-346`）：首 tick 冷启动种子（`:341-343`）里，把 NPC 也 `s.characters.insert(npc_id, CharacterState{location: home_location, ..default})`，玩家成员的 `location` 从其入场地点或默认起点初始化。跨 tick 回灌（`:328-330`）的 `or_default` 对新 NPC 同样补齐。
- `seed_narrative_layer`（`:350-429`）：可选——在此额外把 location 图/NPC 初始位置写进种子（若不走 RoundInput 静态传入而选择落 state，则在此；本规格选 RoundInput 静态传入，故此处仅确保 NPC 角色格存在）。

**`server/src/admin_api/worlds_ops.rs`**
- `create_template`（`:434-484`）：可选加结构校验——`serde_json::from_value::<Skeleton>()` 试解析 + 校验 `reward_item_ref` 可解引用、`location.gate.cosmology ∈ KNOWN_COSMOLOGIES`、`connections`/`residentItems`/`carried_item_ids` 引用完整性。需提升 `Skeleton` 可见性或另立校验 DTO（当前 `Skeleton` 为 `assembly` 私有）。

**`server/src/chapters/mod.rs`**
- `assembly_of`（`:75-80`）读回的 `AssembledInstance` 现在含新字段，自动透传，`chapter_finish` 兑现链路（`:219-234,:280-286`）不变（仍只认 `CharacterHook.reward_item`）。秘境隐藏道具经同一 `grant_item_tx` 兑现。

### 提取管线（新增，跨引擎+desktop+server）

**`crates/muse-engine/src/world/`（新模块，仿 `character/`）** 或复用 `character` 管线扩展
- `WorldExtractionPipeline`：`create_task`（切章 `chapters::split_chapters`，复用）→ 逐章 `scan_world_entities`（发现 NPC/地点/道具/势力/剧情节拍/结局线索，产 mentions+证据，形态照搬 `CharacterMention`/`MentionEvidence`，`character/types.rs:394-421`）→ `merge`（复用 `merge::rule_merge`+`model_merge`）→ `tiering`（NPC core/major/functional，反派=高中心性）→ `synthesize`（产 worldCharacters=`CharacterCardV2` / locations=`LocationDef` / worldItems=`ItemDefinition`）→ Review（复用 roster confirm 模式）。Prompt 全由 caller 传入（后端无状态，仿 `character/mod.rs:33`）。

**desktop `src-tauri/src/commands/`**：新增命令壳 `start_world_extraction` 等，仿 `character_v2.rs`。

**server `server/src/assets/mod.rs`**：新增 `/assets/worlds` 端点（对标 `/assets/characters`，`assets/mod.rs:32-40,156`），产物入 `world_templates.skeleton_json`，走 `safety::moderate_and_queue` 预审核门。

---

## 核心算法

### 1) 按地点分组的回合推进（`crates/muse-engine/src/narrative/mod.rs:156-286` 重构）

```
// 起点：run_round，current = store.load，input.locations / input.world_controlled 已传入
active_ids = current-order(input.active_cards.keys())          // mod.rs:156，全体定序不变

// 分组：按回合起始 location 归组（locations 空 → 单组 ""）
groups: BTreeMap<loc_id, Vec<char_id>> = {}
for cid in active_ids:
    loc = current.characters.get(cid).map(|c| c.location).unwrap_or("")
    groups[loc].push(cid)                                       // 秘境组天然与外部隔离

// 成本（替换 mod.rs:159）
group_count = groups.len()
calls = active_ids.len() + group_count*2 + 2   // N决策 + 每组(导演+写作) + 仲裁1 + 审校1
if budget 不足: return BudgetExhausted           // mod.rs:161-168 不变

// 逐组导演（替换 mod.rs:184-196）
situations: BTreeMap<loc_id, String> = {}
for (loc, members) in groups (按 loc 字典序，确定性):
    situations[loc] = call_director(host, ..., loc, members, current)   // mod.rs:401 加 loc 参数

// 决策（mod.rs:210-234）：分批并发不变，per-角色注入其组 situation + 同组 others
for cid in active_ids:
    loc = group_of(cid)
    same_scene_brief = other_brief 过滤 { k | group_of(k)==loc && k!=cid }
    ctx = assemble_visible_context(current, cid, card, same_scene_brief, situations[loc], ..)
    decisions.push(role_decide(...))
decisions.sort_by(character_id)                                 // mod.rs:234 确定性不变

// 仲裁（mod.rs:237）：R2/R6 用同组在场集
for group: rule_arbitrate(current, group_decisions, group_members)  // active=同组
// R6 移动合法性见下；pending → model_arbitrate（全局一次，mod.rs:240）
outcomes.sort_by((character_id, decision_id))                   // mod.rs:254 不变

// 写作（mod.rs:272-286）：逐组写作，合并为单 SceneRecord（tick=revision 不变）
// build_patch（mod.rs:508）：追加 movement op；continuity I3 用同组在场集
// 原子提交 revision+1（mod.rs:354）：契约完全不变——单 patch / 单 revision
```

**关键不变量保持**：整回合仍是**单 patch、单 revision、原子提交**（`mod.rs:297,354`）；分组只影响"谁和谁在同一 situation/在场集里被判定"，不拆分 reducer/CAS 原子性。确定性排序键仍是 `character_id`（组内）+ 组按 `loc_id` 字典序，全序可复现。

### 2) 移动行动仲裁（`arbiter.rs:120` 循环内，R5 后新增 R6）

```
// d.action 解析出移动目标 dest（约定：targets 含 "loc:<id>" 或引擎侧 move 字段）
if is_move(d):
    cur_loc = state.characters[d.character_id].location
    dest = parse_dest(d)
    // R6a 连通性
    if dest not in input.locations[cur_loc].connections:
        Invalid("无法从当前位置抵达该地点"); continue
    // R6b 准入（秘境）
    if let Some(gate) = input.locations[dest].gate:
        held = state.characters[d.character_id].resources    // 道具以 resource 形式持有
        if !check_location_admission(gate, held, ...):
            Invalid("未满足秘境准入条件"); continue
    Success("移动"): consequence 触发 build_patch 生成 location Set op
```

`check_location_admission`（`server/src/admission/mod.rs` 纯函数，镜像 `check_admission` 的 `:112-135`）：required_item_ids ⊆ held ∧ required_effect_tags ⊆ held_tags ∧ cosmology 白名单 ∧ power_tier ≤ max。

### 3) NPC 同意门控豁免（`mod.rs:614-677` gate_consents）

```
for consent_req in 本回合不可逆结果:
    subject = consent_req.actor/subject
    if subject in input.world_controlled:      // NPC：无 owner，自动放行
        落定该不可逆结果（不产 ConsentRequested，不记 pending_consents）
    else if subject in input.approved_consents: // 玩家：既有逻辑，mod.rs:631-677
        落定
    else:
        产 ConsentRequested + 记 pending_consents（门控不落定）
```

### 4) NPC 注入 tick（`runtime/mod.rs:750` 后）

```
// 玩家成员循环已填 active_cards/other_brief/members_projection（:738-750）
world_controlled: Vec<String> = []
if let Some(entries) = assembled.pointer("/assembly/worldCharacterEntries"):
    for e in entries:
        card = parse CharacterCardV2(e.card)
        active_cards.insert(e.characterId, card)          // 参与决策
        other_brief.insert(e.characterId, card.identity.name)  // 被玩家感知
        world_controlled.push(e.characterId)
        // 不 push members_projection —— 无 owner、不投影日报
locations = parse assembled.pointer("/assembly/locationGraph")
// 门槛（:751）：改为 if member_ids.is_empty() → skip（防纯 NPC 空跑）
RoundInput { ..., locations, world_controlled }
```

---

## 测试影响

### 破坏的现有测试（需改断言）

- **`crates/muse-engine/src/narrative/mod.rs:949`（happy_path）**：脚本化 5 条 ScriptedModel 响应（`:952`）与 `narrative_events==4`（`:991`）依赖"全局单导演单写作"。多组后每组 1 导演+1 写作，调用序列与计数变。单地点/locations 为空时应保持 5 调用（退化路径），**新增多地点变体测试**而非改原测试——保证向后兼容路径不破。
- **`mod.rs:996`（budget_exhausted）** + **`mod.rs:1202`（estimate N+4）**：成本公式改为 `N + 组数*2 + 2`。locations 空时 `组数=1` → `N+4` 恰好不变，原断言可存活；仍需补多组成本测试。
- **`reducer.rs:606`（parse_path_rejects_offpath）**：现在 `characters.<id>.location` 应被接受——需确认该测试的拒绝样例不含 location，并**新增** location 路径接受测试。
- **`continuity.rs:254`（i3_flags_offscene）** / **`arbiter.rs:325`（R2 在场）**：语义从"active 全集"→"同组在场"，精确违规计数需按地点重定义。
- **`server/src/runtime/tests.rs:265`（tick_runs_full_round）**：断言 chA+chB 同在场 + 4 事件。NPC 注入后活跃集/事件数变；地点分区后在场集变。需按新组装重算。
- **`server/src/runtime/tests.rs:751` 门槛**：`active_cards.len()<2` 语义变（NPC 计入），门槛测试改。

### 新增测试

引擎：
1. 多地点导演/写作调用计数 + 成本公式（替代 N+4 硬编码）。
2. 同地点才共享 situation；不同地点角色互不进 `assemble_visible_context`（秘境隔离铁律）。
3. `characters.<id>.location` reducer 接受 + 越界路径仍拒；movement patch op 落定。
4. R6 移动合法性：连通拒绝 / 秘境准入拒绝 / 准入通过。
5. I3/R2 "同组在场"重定义（跨地点 target 判 Invalid）。
6. **NPC 同意门控豁免**：`world_controlled` 中 subject 死亡不产 ConsentRequested、直接落定；不在其中的玩家 subject 仍门控（对照 `mod.rs:1075` vs `:1142`）。
7. NPC 进 active_cards 的信息边界：NPC 私密不泄漏给玩家、NPC 能否被 whisper（约定不可，NPC 无 owner）。

Server：
8. NPC 从 assembled_json 注入 active_cards、不进 members_projection（无日报投影）。
9. `check_location_admission` 纯函数全分支（仿 `admission/mod.rs:190-285` 测试风格）。
10. 秘境隐藏道具经 `grant_item_tx` 兑现幂等。
11. `reward_item_ref` 解引用装配 → `CharacterHook.reward_item` 填充正确、下游兑现不变。
12. 纯 NPC 无玩家世界 skip（门槛）。

装配：`server/src/chapters/tests.rs`（S4，装配测试实际在此，非 assembly/tests.rs）—— 新增 worldCharacterEntries/locationGraph 钉入 assembled_json 的覆盖。

---

## 依赖

- **同意门控 owner-less subject 模型**（引擎侧本规格已解：`world_controlled` 豁免）是与 **arena `permanent_exit`**（`arena/mod.rs` 淘汰门控）共享的系统性缺口。若 arena 也引入 NPC，需同款豁免；本规格只覆盖 narrative run_round，arena 侧留待"竞技/淘汰"块。
- **道具持有表示**：R6b 准入读"角色持有道具"。当前 `CharacterState.resources: Vec<String>`（`types.rs:27`）是自由字符串；需约定道具以 `item:<id>` 形式进 resources，或 runtime 从 `backpacks` 表（`backpack/mod.rs`）注入持有清单进 RoundInput。**依赖 backpack 与 narrative state 的道具事实源对齐**——建议 runtime 在组装 RoundInput 时把玩家 backpack 物化进 `CharacterState.resources`。
- **提取管线**依赖 desktop 三处接线模式（CLAUDE.md：Tauri command + mobile_server 路由 + appInvoke 分支）用于桌面提取 UI；server `/assets/worlds` 依赖既有 `moderate_and_queue` + audit 流。
- **秘境可见性**与 §异步时间线（第二块）的"同刻同地才互动"分组共用地点维度——本规格的分组基础设施应设计成可被时间线复用（组 = 地点 × 时刻）。
- **放置房终局**（第三块）依赖本块的 mainlineNodes/NPC 议程判定终局条件，但不在本规格范围。

---

## 风险

1. **成本随活跃数 + 地点数双重线性放大**。`calls = N + 组数*2 + 2`：NPC 每个 +1 决策，每地点 +2（导演+写作）。多 NPC 多地点世界的 tick 成本可能翻数倍，撞预算硬停（`mod.rs:161`）。缓解：地点组内无角色则跳过导演/写作；反派议程可降频（非每 tick 决策）。
2. **确定性排序在分组下的可复现性**。组按 `loc_id` 字典序、组内 `character_id` 字典序——必须保证 `groups` 迭代确定（用 BTreeMap）。若引入并发逐组导演，需收集后按 loc 排序，仿决策段 `:234` 的 sort。
3. **写作合并为单 SceneRecord**。多组各写一段，合并进一个 SceneRecord（tick=revision 单值，`:152`）。风险：叙事文本跨地点拼接的连贯性，及审校 I1 私密不入正文（`continuity.rs`）需对每组正文分别校验。
4. **道具事实源分裂**：backpack 表 vs CharacterState.resources vs worldItems 目录。R6b/准入若读错源 → 秘境门形同虚设。必须单一物化点。
5. **NPC 无 members_projection 的副作用**：NPC 触发的事件对玩家的可见投影（`events::project_domain_events`，commit `:932-933`）若按 principal 过滤，NPC 事件可能对谁都不可见。需确认 Public 事件不依赖 projection member 即可广播（`build_events` 均 `EventVisibility::Public`，`mod.rs:588,605`，应安全）。
6. **秘境隔离过强**：若玩家全部进秘境、外部无人，非秘境组为空，导演/在场退化。需处理空组与"世界只剩秘境活跃"的边界。
7. **模板校验缺失**：`create_template` 仅 `is_object()`（`worlds_ops.rs:447`）。若不加引用完整性校验，坏 `reward_item_ref`/`connections` 悬空引用会在装配/运行时静默退化（防御式解析吞掉，`seed_narrative_layer:349` 风格）——数据错误难发现。建议加建模板期校验。

---

## 渐进式分步落地

**Phase 0 — worldItems 目录统一（server only，零引擎改动，最小）**
- `Skeleton` 加 `world_items`（`assembly/mod.rs:64-79`）；`PoolItem.reward_item` 加 `reward_item_ref` 解引用（保留内联 fallback）。装配 `assemble_instance` 解引用填 `CharacterHook.reward_item`。下游 `chapter_finish`/`grant_item_tx` 不动。**可独立发布、无破坏**。

**Phase 1 — worldCharacters 无地点参与（引擎轻改）**
- `Skeleton.world_characters` + `assemble_instance` 装配 `worldCharacterEntries` 钉入 assembled_json。
- runtime `:750` 后注入 NPC 进 active_cards（不进 members_projection）；`RoundInput.world_controlled`。
- 引擎唯一改动：`RoundInput.world_controlled` 字段 + gate_consents 豁免（`mod.rs:614-677`）。**此时所有角色仍全局同场**（无 location），NPC 与玩家平权 role_decide/碰撞。反派议程靠卡内容驱动。
- 验证：NPC 参与决策、反派死亡不误门控、日报无 NPC。

**Phase 2 — location 维度（引擎大改，本块核心）**
- `CharacterState.location` + reducer 路径 + `LocationDef`/`RoundInput.locations`。
- `run_round` 分组：导演/decide-others/arbiter R2/continuity I3 按同 location。movement 行动 + R6 仲裁。
- **保证 locations 空 = 完全退化为 Phase 1 行为**（单组 `""`），老世界不受影响；成本公式在单组时恒等 N+4。
- 验证：碰撞按地点、移动行动、跨地点隔离。

**Phase 3 — 秘境 + 道具分布**
- `LocationGate` + `check_location_admission`；`is_secret_realm` 可见性隔离；`residentItems`/NPC carried items 分布，秘境隐藏道具经 `grant_item_tx` 兑现。
- 玩家持有道具物化进 RoundInput（backpack → CharacterState.resources）。
- 建模板期引用完整性校验（`worlds_ops.rs`）。

**Phase 4 — 提取管线**
- `WorldExtractionPipeline`（引擎）+ desktop 命令壳 + `/assets/worlds` 端点。产出扩展 Skeleton 灌进可编辑模板编辑器 → 预审核发布。**独立于运行时，可最后做**。

每个 Phase 可独立测试/发布；Phase 2 的"locations 空退化"是关键安全阀，确保引入地点不破坏既有世界。

---

## 工作量与影响面估计

| Phase | 引擎(muse-engine) | Server | Desktop | 测试 | 相对工作量 |
|---|---|---|---|---|---|
| 0 worldItems 统一 | 0 | assembly + 校验，~80 行 | 0 | 装配测试改 + 引用测试 | S |
| 1 worldCharacters | RoundInput 2 字段 + gate 豁免，~40 行 | runtime 注入 + assemble，~120 行 | 0 | 引擎门控豁免 + server 注入，~6 测试 | M |
| 2 location 维度 | types/reducer/mod/arbiter/decide/continuity 六文件，~250 行核心重构 | runtime seed + RoundInput 组装，~80 行 | 0 | 引擎分组/移动/隔离，~10 测试 | **L（最重）** |
| 3 秘境+道具分布 | R6 准入 ~30 行 | admission 纯函数 + 分布 + 校验，~150 行 | 0 | 准入全分支 + 兑现幂等，~8 测试 | M |
| 4 提取管线 | 新 world 模块，~400 行（仿 character/） | /assets/worlds 端点，~150 行 | 命令壳 + UI，~300 行 | 管线阶段测试，~10 测试 | **L** |

**影响面集中区**：
- **`crates/muse-engine/src/narrative/mod.rs`（1209 行）** 的 `run_round`（`:138-371`）是最高风险重构点——分组逻辑、成本公式、导演/写作循环、门控豁免全在此。建议此文件优先 code review。
- **`server/src/runtime/mod.rs`（1048 行）** 的 tick 组装段（`:718-805`）是 server 唯一改动集中区。
- reducer/arbiter/continuity/decide 均为"加分支/收窄集合"式小改，风险低于 mod.rs。

**关键契约保持不变**（降低整体风险）：reducer 的 clone-on-apply + CAS + 幂等 + 禁止谓词后校验（`reducer.rs:137-169`）、单 patch 单 revision 原子提交（`mod.rs:297,354`）、E3 确定性状态机全部原样复用——所有改动都是在这套骨架上"加输入维度 + 按维度分组过滤"，不推倒重建。
