# 副本卡混装（Subplot Cards）— 实现规格

> 目标：把「副本内容」从世界模板中解耦为独立资产**副本卡**（SubplotCard）——一段可插拔的剧情内容块
> （storylines + 隐藏任务池 + 专属 NPC + 道具 + 地点片段），可从任意小说提取或手工创作；世界模板降级为
> **容器**：创作者建世界时可混装自己原著的卡 + 他人授权/公开的卡，装配时从容器内所有卡的并集超集里采样。
>
> 本规格全部基于真实代码核对，行号为核对当时工作区版本的位置（引用格式 `文件:行`）。
> 编写日期 2026-07-24。不改任何代码，仅给落点。

---

## 0. 现状地形（改动的支点）

| 支点 | 位置 | 与本规格的关系 |
|---|---|---|
| Skeleton 结构（超集内容池） | `server/src/assembly/mod.rs:126-162` | 卡的内容块与它同构；容器合并后仍产出一个 Skeleton |
| 确定性采样 `plan_sampling` | `server/src/assembly/mod.rs:629-804`，种子 `instance_seed`=`H(world_id‖阵容指纹‖template_version)`（:521-524），禁三样（:352-357），域常量 0x51–0x56（:390-395） | 采样单位从 storyline 升为（卡, storyline）二级；种子纳入卡集合指纹 |
| 装配入口 + C-7 CAS 钉住 | `server/src/assembly/mod.rs:849-1026`（CAS :1001-1023）；审计段仅服务端可见（:51-55, :61-75） | 副本内 replay 一致性的既有保障，本规格不破坏 |
| 引用完整性校验 | `server/src/assembly/mod.rs:1110-1171` `validate_skeleton_refs` | 复用为「卡内闭包校验」 |
| 创作者世界发布 + 超集校验 | `server/src/assets/worlds.rs`：`MIN_REDUNDANCY_RATIO=3.0`（:40-41）、`validate_superset`（:145-211）、机审拼接 `world_scan_text`（:215-256）、manifest（:277-306）、publish（:311-413） | 卡发布端点对标此模块；冗余门上移到容器级 |
| 提取管线 | `crates/muse-engine/src/world/mod.rs`（`synthesize_superset` :382-505）、`types.rs`（`WorldSkeletonDraft` :305-333）、`superset.rs`（`assemble` :28-113、`compute_sampling` :159-186） | 单书超集 = 单卡母体；Phase 3 增加「按 storyline 切片导出卡」 |
| 防刷规则 | `docs/build/rules-anti-farming.md`（分层种子、超集采样、种子不可被用户控制） | 本规格是防刷第三环：卡集合维度 |
| 分成账本 | `server/src/ledger/mod.rs`：`resolve_share`（:199-243）、`DEFAULT_REVENUE_SHARE_BPS=7000`（:21-24）、自打赏归零（:223-226）、未成年挂平台（:230-241）、`post_journal` SUM=0（:119-174） | 卡作者分成沿用此模式做多方拆分 |
| 准入体系 | `server/src/admission/mod.rs`：`check_admission`（:100-149）、`translate_item` 只降不升（:151-164）、`check_location_admission`（:166-209）、`KNOWN_COSMOLOGIES`（:82） | 跨 cosmology 混装的硬约束全部复用，零新机制 |
| 道具单一写入 + 章节兑现 | `server/src/backpack/mod.rs:78-133` `grant_item_tx`（幂等键 :112-127）；`server/src/chapters/mod.rs:218-233,283-289` | hook_key 前缀化后天然不冲突，零改动 |
| 模板钉住 + 建房校验 | `server/migrations/0001_init.sql:89-120`；`server/src/worlds/mod.rs:717-761`（moderation/withdrawn 检查 + template_version 钉住） | 容器→卡引用同样按版本钉住，级联校验 |

---

## 1. 资产模型：SubplotCard schema

### 1.1 结构（camelCase，与 Skeleton 子集同构）

```jsonc
{
  "schemaVersion": 1,
  "id": "scard_xxx",                      // 服务端 new_id("scard")，客户端声明忽略
  "sourceWork": { "sourceId": "…", "title": "…" },   // 卡自己的原著来源（≠ 容器的 sourceWork）
  "rightsDeclaration": "original | public_domain_adaptation",  // 卡级独立版权声明，必填（红线①）
  // ---- 内容块（字段名/结构逐字对齐 assembly::Skeleton 对应字段，:126-162）----
  "storylines":        [ /* StorylineSpec，≥1 条 */ ],
  "mainlineNodes":     [ /* MainlineNode（fated/variantGroup/arcTags）*/ ],
  "hiddenContentPool": [ /* PoolItem */ ],
  "sideHookPool":      [ /* PoolItem */ ],
  "endingPool":        [ /* EndingCandidate */ ],
  "worldCharacters":   [ /* WorldCharacter（专属 NPC，完整 CharacterCardV2）*/ ],
  "worldItems":        [ /* ItemDefinition（专属道具目录，单一事实源）*/ ],
  "locations":         [ /* LocationSpec + 本卡新增 anchors 见 §3.3 */ ],
  "anchors":           [ "loc-entrance" ],  // 对外缝合口白名单：容器缝合边只能落在 anchor 上
  // ---- 卡级元数据 ----
  "sampling": { /* SamplingHints：卡自身的建议抽样量；冗余率仅标注不设门槛，见 §2.3 */ }
}
```

服务端**派生**（不信客户端声明，对齐 §9.6「服务端权威」）：

- `cardCosmologies` = ∪(`worldItems[].origin.cosmology`) ∪ ∪(`locations[].gate.requiredCosmologies`)，
  须 ⊆ `KNOWN_COSMOLOGIES`（`admission/mod.rs:82`，引擎侧同款 `world/types.rs:13`）；
- `cardMaxPowerTier` = max(`worldItems[].origin.power_tier`)（1–5，`admission/mod.rs:15`）；
- `version`：按 owner+title 服务端递增（照抄 `assets/worlds.rs:358-365`）。

### 1.2 内容块引用完整性 = 「卡内闭包」

卡内一切引用必须能在**本卡内**解引用：`rewardItemRef`/`residentItemIds`/`carriedItemIds`/
`gate.requiredItemIds` → 本卡 `worldItems`；`connections`/`homeLocation` → 本卡 `locations`；
`storylines.{mainlineNodeIds,hiddenPoolIds,endingIds}` 与 `arcTags` → 本卡对应池。
实现 = 把 `validate_skeleton_refs`（`assembly/mod.rs:1110-1171`）+ `validate_superset` 的
storyline 引用检查（`assets/worlds.rs:153-181`）在**单卡 JSON** 上原样跑一遍（两函数入参本就是
`&Value`/骨架结构，无需改造，新增薄封装 `validate_card_refs`）。唯一放行的对外出口是 `anchors`
（只声明「本卡哪些地点可被容器缝合」，不引用外部 id）。**卡内 id 禁含 `:`**（§3.2 命名空间分隔符），
发布时正则校验。

### 1.3 与现有 Skeleton 的关系：现有模板 = 单卡世界的退化形态

- **等价视角**：一个现行超集模板 ≡ 隐式单卡容器——卡即模板本体全部内容，cardId 取模板 id。
- **迁移策略：零破坏、零回填**。`world_templates.skeleton_json` 新增可选字段 `subplotCardRefs`
  （`serde(default)`，装配层 `load_skeleton` 的防御式 `unwrap_or_default` 语义 `assembly/mod.rs:1030-1040`
  天然忽略未知字段）：
  - 无 `subplotCardRefs` → **旧路径 byte 级不变**：不合并、不改种子公式（§4.1 兼容规则）、
    `plan_sampling` 不走卡采样步。已上线的全部模板与实例行为与现在完全一致。
  - 有 `subplotCardRefs` → 容器形态，装配前经合并器（§3.2）合成一个内存 Skeleton 再走现有管线。
- 新表 `subplot_cards`，不动 `world_templates` 现有列（新增列全部 `ALTER TABLE ADD COLUMN` 可空/带默认，
  照 0011/0013/0014 的迁移惯例）：

```sql
-- migration 00XX_subplot_cards.sql
CREATE TABLE subplot_cards (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  source_work_json TEXT NOT NULL,
  rights_declaration TEXT NOT NULL,          -- original | public_domain_adaptation，必填
  card_json TEXT NOT NULL,                   -- §1.1 全文（≤ 1 MiB，容器上限 2 MiB 的一半，见 worlds.rs:38）
  cosmologies_json TEXT NOT NULL,            -- 服务端派生
  max_power_tier INTEGER NOT NULL,           -- 服务端派生
  version INTEGER NOT NULL DEFAULT 1,
  moderation TEXT NOT NULL DEFAULT 'pending',
  withdrawn INTEGER NOT NULL DEFAULT 0,
  visibility TEXT NOT NULL DEFAULT 'private',-- private | licensed | public（§7.1）
  star_rating INTEGER NOT NULL DEFAULT 1,    -- §6，运营评定
  manifest_json TEXT,
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_subplot_cards_owner ON subplot_cards(owner_id);
```

---

## 2. 发布与审核

### 2.1 独立发布端点（对标 `/assets/worlds`，`assets/worlds.rs:43-50`）

```
POST /assets/subplot-cards                  发布：cardJson + rightsDeclaration
GET  /assets/subplot-cards/mine             owner 隔离列表
GET  /assets/subplot-cards/{id}/status      审核态 + manifest（非本人 404）
GET  /assets/subplot-cards/{id}/manifest
POST /assets/subplot-cards/{id}/withdraw    停止后续被引用（已钉住实例不回收）
POST /assets/subplot-cards/{id}/licenses    授权管理（§7.1，licensed 可见性用）
GET  /assets/subplot-cards/market           可引用卡目录（public + 对我 licensed，仅 approved 且未 withdrawn）
```

发布流程逐段照抄 `publish`（`assets/worlds.rs:311-413`）：结构校验 → 卡内闭包校验（§1.2）→
幂等 guard → 服务端权威版本号 → 机审 → 落库 → manifest。Idempotency-Key 语义、
「客户端伪造 version/moderation 被忽略」测试（`assets/worlds.rs:582-612`）同款复刻。

### 2.2 机审

- 扫描文本 = 卡版 `world_scan_text`（`assets/worlds.rs:215-256` 原函数即可直接用：它按顶层键取
  worldCharacters/locations/worldItems/hiddenContentPool/sideHookPool/storylines，卡 JSON 键名相同）。
- `safety::moderate_and_queue(state, "subplot_card", card_id, text)` —— 铁律不变：**它是唯一入队/记险方**
  （`assets/worlds.rs:13-14, 370-374`），本模块绝不二次写 audit_queue/risk_events。
- `admin` 审核工作台 `audit.rs` 认新 `subject_kind="subplot_card"`，approve/reject 回写
  `subplot_cards.moderation`（对齐 world_template 的既有处理）；申诉复用 `moderation_appeals`
  （(subject_kind, subject_id) 唯一约束天然支持新 kind，migration 0018）。
- NPC 卡在**装配期**还有第二道 S-3 机审门（`assembly/mod.rs:1173-1212`，仅 Approved 钉入实例），保持不变
  ——发布期过审 + 装配期复核的双门是现有安全模型，卡不豁免。

### 2.3 超集校验按卡粒度怎么做：**冗余门在容器级（硬门），卡级只做结构自洽（软标注）**

结论与理由：

- **卡级**（发布卡时）：只做①卡内闭包（§1.2）②`storylines ≥ 1`③每个具名 `variantGroup ≥2 成员`
  （复用 `enforce_variant_groups` 语义，`superset.rs:115-157`——服务端校验版直接用
  `validate_superset` 的第 3 步 `assets/worlds.rs:191-208`）④`sampling` 数字合法（各 count ≥1 或 0）。
  **不设 `redundancyRatio ≥ 3.0` 硬门**：防刷的度量对象是「副本实例可采出多少种不同组合」，而实例从
  **容器并集**采样；把 3.0 压到卡级会误伤「小而精」的单线卡（比如一条支线 + 一个 NPC 的番外卡），
  抬高创作门槛，却对防刷无增益。
- **容器级**（发布容器时）：对**合并后的并集**执行现有 `MIN_REDUNDANCY_RATIO=3.0` 门
  （`assets/worlds.rs:40-41, 184-189`），且冗余率由**服务端按并集重算**（镜像
  `superset.rs:159-186` `compute_sampling` 的「各维度 总量/抽样量 取最小」口径），不再信客户端声明的
  `sampling.redundancyRatio`——顺带堵掉现行「客户端虚报冗余率」的口子（现校验只读声明值）。
  冗余不足 → 400，提示「再挂一张卡或扩充本体内容」。

---

## 3. 容器世界

### 3.1 模板引用卡列表（版本钉住）

`skeleton_json` 顶层新增：

```jsonc
"subplotCardRefs": [
  { "cardId": "scard_a", "cardVersion": 3, "weight": 1.0 },
  { "cardId": "scard_b", "cardVersion": 1, "weight": 0.5 }
],
"seams": [ { "from": "scard_a:loc-gate", "to": "scard_b:loc-dock" } ],   // §3.3
"nexus": { "name": "十字驿站" }                                            // §3.3，可选
```

- **精确版本钉住**：引用 (cardId, cardVersion)。卡发新版不自动生效——与 `worlds.template_version`
  钉住哲学一致（`0001_init.sql:104`、`worlds/mod.rs:735,761`）。容器要用新版卡须发容器新版本。
- 容器本体（skeleton_json 里直接写的 mainlineNodes/... 自有内容）= **第 0 张隐式卡**，cardId 取模板 id，
  恒为 `coreCard`（§4.2 必选）。允许纯策展容器（本体只有 sourceWork + refs，内容全部来自卡）。
- **发布容器时的级联校验**（在 `assets/worlds.rs::publish` 的 :342-347 校验段后追加）：每个 cardId
  须存在、`moderation='approved'`、`withdrawn=0`（红线②），且授权成立（owner 本人 / public /
  licensed 且有有效授权行，§7.1）；cardVersion 须是该卡已存在版本。
- **建房时的级联校验**（`worlds/mod.rs::create_room` 现有 template_not_approved/template_withdrawn
  检查 :727-732 之后）：重查各引用卡 moderation/withdrawn——卡被人审下架后，引用它的容器**停止后续建房**
  （已钉住实例照旧运行，withdraw 语义与 `assets/worlds.rs:300-304` deletionPolicy 一致）。

### 3.2 跨卡引用完整性：命名空间前缀 `cardId:原id`

合并器 `compose_skeleton(container_skeleton, cards) -> Skeleton`（新增于 `server/src/assembly/`，
纯函数、可单测）在装配读取骨架后、`plan_sampling` 之前执行：

1. **id 重写**：每张卡的全部 id 加前缀 `{cardId}:`，分隔符 `:`（卡内 id 禁含 `:`，§1.2 已在发布期校验）。
   重写覆盖**定义位与引用位全集**：`mainlineNodes.id/variantGroup/arcTags`、`storylines.id` 及其三个
   id 列表、`hiddenContentPool`/`sideHookPool` 的 `id/rewardItemRef/variantGroup/arcTags`、
   `endingPool.id/variantGroup/arcTags`、`worldItems.id`、`locations.id/connections/residentItemIds/
   gate.requiredItemIds`、`worldCharacters.card.id/carriedItemIds/homeLocation/agendaNodes`。
   隐式卡前缀 = 模板 id（保证与真卡不撞）。
2. **variantGroup 带前缀 → 互斥组天然不跨卡**（有意语义：不同小说的变体不该互斥）。
3. **道具目录合并**：各卡 `worldItems` 前缀化后直接并集——id 冲突在构造上不可能（前缀唯一）。
   `ItemOrigin.world_template_id` 钉入容器模板 id（引擎草稿本就留空待发布期钉入，
   `world/types.rs:164-166` 注释）。下游零改动：`resolve_reward_item`（`assembly/mod.rs:1067-1074`）、
   `distribute_resident_items`（:1078-1101）、NPC 携带解引用（:1198-1203）都在合并后目录上查，行为不变。
   章节兑现幂等键 `hook_key = {world_id}:{cid}:{pool_item_id}`（`chapters/mod.rs:219-233`）——
   pool_item_id 已带卡前缀，`grant_item_tx` 的 (user_id, reward_hook_key) 唯一键（`backpack/mod.rs:112-127`）
   天然不冲突，**backpack/chapters 零改动**。
4. 合并后整体跑一遍 `validate_skeleton_refs`（发布容器时；装配期靠悬空静默丢弃的既有防御式兜底）。

### 3.3 地点连接跨卡缝合（明确方案）

- 卡内 `connections` 只许指向卡内地点（闭包）；跨卡连接**只能**经容器级 `seams` 显式声明，
  两端须存在且均为各自卡的 `anchors` 成员（发布容器时校验）；**秘境不可作缝合口**
  （`is_secret_realm=true` 的地点禁止进 anchors，gate 语义完整保留在卡内）。
- **无缝合声明时的默认策略**：合并器自动生成容器枢纽地点 `{tplId}:loc-nexus`（名称取容器 `nexus.name`，
  缺省「交汇之地」），把每张被引用卡的首个 anchor（无 anchors 则该卡首个非秘境地点）与 nexus 双向连接。
- **连通性保障**：`sample_location_ids` 是「种子 + BFS 沿 connections 扩张、只加相邻地点」
  （`assembly/mod.rs:553-627`）——若两卡地点图不连通，实例图会裂成孤岛。因此合并器把 **nexus 与全部
  seam 端点并入 loc_seeds 必选种子**（与「含驻留道具地点必选」:776-779 同列），保证被选各卡的地点分量
  在实例内互达；「秘境保连通」测试语义（:1607-1621）在跨卡场景下同样成立。

---

## 4. 装配与防刷

### 4.1 种子纳入卡集合指纹（防「换一张卡组合刷同一世界」）

现公式（`assembly/mod.rs:521-524`）：
`seed = fnv1a_64("{world_id}\u{1}{roster_fingerprint}\u{1}{template_version}")`。

升级（仅容器形态生效）：

```
card_set_fingerprint = 排序去重的 "{cardId}@{cardVersion}" 以 "\n" 连接   // 对齐 roster_fingerprint 的构造法 :504-510
seed = fnv1a_64("{world_id}\u{1}{roster_fingerprint}\u{1}{template_version}\u{1}{card_set_fingerprint}")
```

- **兼容规则（零破坏）**：无 `subplotCardRefs` → 沿用三段式原公式，**byte 不变**——旧模板新装配的种子
  与现在完全一致（已装配实例本就被 C-7 CAS 钉住不重掷）。测试向量照 `prng_test_vectors`（:1503-1512）
  加四段式向量锁死。
- 价值：①防旁路——即使有人绕过版本递增（运营后台直改 skeleton_json 换卡而 version 未动），换卡即换种子，
  无法「换卡试探同一实例的高收益路径」；②审计可复算——审计段能证明实例由哪个卡集合装配。
- 审计段 `InstanceSampling`（:61-75）新增两字段（`serde(default)`，旧数据可读）：
  `card_set_fingerprint: String`（哈希，如 rosterFingerprint 之例 :795 只存 fnv 哈希不存明文）、
  `selected_cards: Vec<String>`。**仅服务端/审计可见，绝不进 members_projection 或日报**（:51-55 契约延续）。

### 4.2 采样单位升为（卡, storyline）二级

`plan_sampling`（:629-804）头部新增**第 0 步：卡采样**，其余 6 步在被选卡的内容子集上原样跑：

- 新域常量 `DOMAIN_CARD: u64 = 0x57`（续 :390-395 序列），子流 `Rng(seed ^ DOMAIN_CARD)`。
- 候选 = `subplotCardRefs` **模板序**（Vec 序，禁 map 序——禁三样 :352-357 继续适用）；
  权重 = `ref.weight × (1 + 卡 affinity boost)`，卡 affinity boost 取卡内各 storyline
  `affinity_boost`（:527-535）的最大值（贴合阵容的卡整卡更可能入选）。
- 抽 `sampling.instanceCardCount` 张（容器级 sampling 新字段；缺省 `ceil(卡数/2).max(1)`，
  对齐 storyline 缺省 :658-661），用现有 `choose_k`（:423-461）。
- **隐式卡（容器本体）必选**：`coreCard` 恒 true，不占 count 名额——容器作者的自有内容是世界基底，
  类比 fated 必留（:705-712）。跨全容器的 `fated` 主线节点仅允许出现在 coreCard（发布容器时校验），
  非 core 卡的 fated 只在**该卡入选时**必留——「fated 必留」不变量按卡收敛，不跨未选卡。
- 第 1–6 步改动仅一处：各维度候选先按前缀过滤到被选卡
  （`id.split(':').next() ∈ selected_cards ∪ {隐式卡}`——前缀即归属映射，无需附表），
  之后 storyline 采样、fated/变体组/执念加权/NPC 议程加权/地点保连通逐行不动。
- 退化：`instanceCardCount ≥ 卡数` 或未设 → 全卡入选，二级采样退化为现行一级采样（行为守恒）。

### 4.3 副本内 replay 一致性不变（契约自证）

种子输入（world_id、阵容指纹、template_version、cardRefs）全部在首次 start 前钉住；卡采样在同一纯函数
`plan_sampling` 内、同一 SplitMix64 域机制下；结果仍经 C-7 CAS 一次性写入 `worlds.assembled_json`
（:1001-1023），读回复用不重掷；runtime 只读钉住结果组装 RoundInput（`runtime/mod.rs:453,484-495,864-867,
1235-1243`），**不感知卡的存在**。`sampling_tests` 全套（#1 同种子同采样 :1516-1527、#2 副本间不同、
#4 阵容敏感 :1543-1550、#5 fated、#6 变体互斥、#7 脊柱自洽、#8 计数上限、#9 退化）在容器形态下平移复刻，
另加三条：换卡集合种子必变、未选卡内容零泄漏（断言被选 id 前缀 ⊆ selected_cards）、无 cardRefs 时
与三段式结果逐字段一致。

---

## 5. 冲突与一致性

### 5.1 不同小说 cosmology 混装的硬约束（全部复用现有闸门，零新机制）

- **发布容器时静态检查**：容器 `admission_json`（`WorldAdmissionPolicy`，`admission/mod.rs:45-56`）
  与每张卡的服务端派生元数据（§1.1）做相容性判定——allowlist 模式：`cardCosmologies ⊆ policy.cosmologies`；
  denylist：交集为空；`maxPowerTier` 设定时 `cardMaxPowerTier ≤ 上限`，超限给两个选项：拒绝发布，或容器
  声明 `rejectedHandling=translate` 走**结构化降档**（`translate_item` :151-164——powerTier 夹到上限、
  effectTags 恒不变，防转译成为强度后门）。判定直接对卡内每件 `ItemDefinition` 逐件跑 `check_admission`
  （:100-149），纯函数免费。
- **运行时不变**：玩家跨世界背包携带仍走 `backpack::carry → check_admission` 双重校验；秘境准入仍是
  `LocationGate` 硬闸（`check_location_admission` :166-209，无降档中间态）；`gate.requiredCosmologies ⊆
  KNOWN_COSMOLOGIES` 已由卡内闭包校验前置拦截（对齐 :180-183 注释的既有分工）。

### 5.2 世界观违和：**取舍结论 = 创作者自担 + 市场自然惩罚，平台不做语义校验**

理由：①数值/规则层面的破坏已被 cosmology 枚举白名单 + powerTier 上限 + translate 降档封死，违和只剩
审美问题，**无经济外部性**；②「协调性」语义审查需进发布路径的模型调用，成本高、误杀率高，且越出现有
机审边界（`moderate_and_queue` 只管安全与注入，`assets/worlds.rs:13-14`）；③自然反馈渠道已上线：
热度分 = 近 48h 事件×1 + 7 天打赏×5 + 成员×2（`worlds/mod.rs:4-9,131,167`），违和世界事件少、打赏少
→ hot 榜自然沉底；星级（§6）是第二惩罚通道。平台义务止于**知情透明**：容器详情页/manifest 列出全部卡的
`sourceWork + cosmologies + 星级`，玩家投卡前可见「这是修仙×赛博的缝合世界」。

---

## 6. 星级与产出

现状：全仓无星级字段（代码与 docs 均无命中）——本节为新增概念给落点，列已建于 §1.3 表
（`subplot_cards.star_rating`）+ `world_templates` 加 `curation_stars INTEGER NOT NULL DEFAULT 1`。
评定由 admin 端点（reviewer/admin 角色，写 audit_logs 留痕，对齐 appeals resolve 的 RBAC 惯例）。

- **容器星级公式（建议）**：
  `container_stars = min(curation_stars, max(各引用卡 star_rating, 隐式卡按容器本体评级))`
  ——封顶于「容器自身策展质量」与「其最强卡」的较小者。性质：挂再多低星卡不稀释最强卡（鼓励好卡被复用）；
  但策展烂（curation 低）时白嫖一张五星卡也带不动容器（防「挂名蹭星」）。第一版不加加权平均下限，保持
  单调、可解释、不可被凑卡操纵。
- **产出封顶：`powerTier ≤ container_stars`**，两处落点：
  1. 装配期统一夹持：`assemble_instance` 对全部解引用道具（钩子 `reward_item` :921、
     `resident_items` :979、NPC `carried_items` :1198-1203）套 `translate_item` 语义夹
     `power_tier = min(power_tier, container_stars)`（只降不升，effectTags 不变）；
  2. 发货期兜底：`grant_item_tx` 的两个合法调用点（章节结算 `chapters/mod.rs:287`、商店
     `shop/mod.rs:180-238`）前再夹一次，防旁路。
  效果：低星容器混装五星卡 → 道具自动降档产出，堵死「借高星卡在低质容器刷高价值道具」；配合 §4 采样
  防刷与「买过程不买结果」红线闭环。
- 星级只影响曝光（hot 榜权重可后续叠加）与产出封顶，**不影响任何战力判定**（荣誉非战力红线不动）。

---

## 7. 权属与分成

### 7.1 授权模型（`subplot_cards.visibility`）

- `private`：仅 owner 自己的容器可引用（默认）。
- `licensed`：逐授权引用。新表
  `subplot_card_licenses(id, card_id, licensee_user_id, status('active'|'revoked'), created_at, revoked_at)`；
  卡主经 `POST /assets/subplot-cards/{id}/licenses` 授予/撤销。**撤销只影响后续新容器发布与建房**
  （已发布容器按钉住版本继续、已运行实例不回收——与 withdraw 的 deletionPolicy 同款语义，
  `assets/worlds.rs:300-304`）。
- `public`：任何人可引用（仍强制 approved + 分成，不是放弃权益）。

### 7.2 分成拆分（沿用 `ledger::resolve_share` 模式，给 bps 建议）

现状：`charge`（`ledger/mod.rs:245-330`）→ `resolve_share`（:199-243）认 `template.owner_id`，
`creator_cut = floor(price × bps/10000)`，bps 默认 7000（:21-24），余数归平台，postings 经
`post_journal` SUM=0 断言（:119-174）。

扩展：在 **creator_cut 蛋糕内**二次拆分（平台 30% 不动）：

- **容器作者 6000 bps**（60% of creator_cut）——策展 + 自有内容 + 承担容器级合规；
- **卡作者池 4000 bps**（40% of creator_cut）——按该实例 `assembled_json → sampling.selected_cards`
  **实际被选卡等分**（真实入戏的内容才分钱，防挂名蹭分；等分而非按内容量加权——确定性、可审计、
  不激励灌水）。隐式卡占一份（回流容器作者）。
- **退化恒等**：全部卡都是容器作者自己的（含纯隐式单卡）→ 两份合流，分成结果 == 现行为，零破坏。
- 实现：`resolve_share` 返回 `Vec<(owner_id, cut_cents)>`；每份独立执行既有红线判定——
  自打赏（payer == 该份 owner → 该份归平台，**其余份不受影响**，:223-226 逐份化）、
  未成年 owner（该份挂平台，:230-241 逐份化）；每份 floor，**全部取整余数归平台**（:11 红线延续）。
  postings 由 ≤3 条扩为 N 条，`post_journal` 本就支持任意条数 + SUM=0 断言，`charge` 签名与各付费点
  （gift/revive/room_open/云成长/shop）**零改动**。
- **分成快照物化**：实例创建时把 `revenueSplit: [{ownerId, bps}]` 写进 `assembled_json` 包装对象
  （与 assembly/chapterState 并列，`load_wrapper`/`save_wrapper` :819-843），`resolve_share` 优先读快照
  ——避免每笔 charge 反查卡表，且卡授权/归属后续变更不引起历史世界分成漂移（钉住哲学一致）。
  无快照 → 走现行单创作者路径。
- finance 对账 `/admin/ledger/reconcile` 的全账 SUM=0 恒等式无需改动，天然覆盖多方拆分。

---

## 8. 红线（全部写进实现与测试）

1. **每卡独立版权声明必填**：`rightsDeclaration ∈ {original, public_domain_adaptation}`（复用
   `valid_rights`，`assets/worlds.rs:80-82`），缺失/非法 → 400；容器 manifest 聚合列出全部卡的
   `sourceWork + rightsDeclaration + ownerId`（`build_manifest` :277-306 扩展）——版权责任**按卡到人**，
   容器作者对引用行为负连带注意义务（引用前可见卡的声明与审核态）。
2. **未过审卡不可被引用**：容器发布期 + 建房期双重校验 `moderation='approved' ∧ withdrawn=0`（§3.1）；
   人审 reject 已上架卡 → 引用容器停止后续建房，已钉住实例不受影响。测试：pending/rejected/withdrawn
   卡引用 → 400/409。
3. **防刷种子不外泄**：`seed / rosterFingerprint / cardSetFingerprint / selectedCards` 仅存
   `assembled_json` 审计段，绝不进 members_projection、日报、任何客户端投影（`assembly/mod.rs:51-55,
   58-60` 契约延续）；种子输入不可被用户控制（换卡、退出重进、观察均不可预测/复现，
   `rules-anti-farming.md` 平衡条款延续到卡维度）。
4. **分成自打赏归零沿用且逐份执行**：任何收益方（容器作者或任一卡作者）== 付费方 → 该份归平台并留痕
   （`NoShareReason::SelfTip` :185-189 扩展记录份别）——卡作者不能给引用自己卡的世界打赏刷分成。
5. **无提现出口不破**：新增账户仍经 `ensure_account`，`withdrawable` 恒 0（:99-117）；授权/分成
   不得携带任何链下结算字段。
6. **道具单一写入路径不破**：跨卡道具仍仅经 `grant_item_tx`（`backpack/mod.rs:78-133`）两条合法路径
   （tick/章节结算、支付履约）入包。

---

## 9. 渐进落地：Phase 划分与影响面

每阶段可独立上线（前一阶段不依赖后一阶段，功能开关 = 数据形态本身：无 cardRefs 即旧世界）。

| Phase | 内容 | 测试锚点 | 上线判据 |
|---|---|---|---|
| **P0 卡资产层** | `subplot_cards` 表 + 6 端点 + 卡内闭包校验 + 机审(subject_kind=subplot_card) + manifest + admin 审核/申诉认新 kind | 对标 `assets/worlds.rs` 测试全套（发布/幂等/注入折 Pending 单条入队记险/owner 404 隔离/withdraw 幂等，:582-849）平移 | 无人引用即零影响，可静默上线 |
| **P1 容器与装配** | `subplotCardRefs/seams/nexus` + `compose_skeleton` 命名空间合并 + 容器发布级联校验与并集冗余门（服务端重算）+ `plan_sampling` 第 0 步卡采样 + 种子四段式 + 审计段扩展 + 建房级联检查 | `sampling_tests` 平移 + 新增 4 条（§4.3）+ 合并器纯函数单测（前缀重写全位点/缝合校验/nexus 连通/hook_key 唯一）+ 跨 crate round-trip（对标 :854-871） | 旧模板路径 byte 不变的回归全绿（#9 退化测试族） |
| **P2 分成** | `subplot_card_licenses` + `resolve_share` 多方拆分（6000/4000 bps）+ 分成快照物化 + 留痕扩展 | 对标 `ledger/tests.rs`：取整余数归平台、自打赏逐份归零、未成年逐份挂平台、**单卡退化 == 现行为**、reconcile SUM=0 | 快照缺失走现行路径的回归 |
| **P3 星级与生态** | `star_rating/curation_stars` + admin 评定端点 + 装配/发货双点 powerTier 夹持 + hot 榜权重 + 提取管线「按 storyline 切片导出卡草稿」（复用 `synthesize_superset` 产物按 `Storyline.{mainlineNodeIds,hiddenPoolIds,endingIds}` 归组切片）+ 桌面端卡发布 UI | translate 只降不升测试平移（`admission/mod.rs:315-322`）+ 夹持后 `grant_item_tx` 入包 tier 断言 | 星级缺省 1 时夹持不改变任何现有产出（默认星=默认 tier 下限自证） |

### 对现有 8 个已上线系统的影响面

| # | 系统 | 影响 | Phase |
|---|---|---|---|
| 1 | 提取管线（`crates/muse-engine/src/world/`） | **P0–P2 零改动**（`WorldSkeletonDraft` 即单卡母体）；P3 加切片导出（纯新增函数，不动 8 阶段任务机） | P3 |
| 2 | 创作者资产发布（`server/src/assets/`） | 新增平行模块 `subplot_cards.rs`（P0）；`worlds.rs::publish` 加 cardRefs 分支：级联校验 + 冗余门改为对合并并集重算（P1） | P0/P1 |
| 3 | 开局装配与防刷采样（`server/src/assembly/mod.rs`） | **核心改动面**：compose 前置、种子四段式、`plan_sampling` +第 0 步、审计段 +2 字段；退化路径逐行不动 | P1 |
| 4 | 准入体系（`server/src/admission/`） | **零代码改动**——纯函数被容器发布校验与星级夹持新增调用 | P1/P3 |
| 5 | 章节结算与背包（`chapters`/`backpack`） | **零改动**（hook_key 前缀天然唯一）；P3 在 grant 调用点前加一行夹持 | P3 |
| 6 | 运行时（`server/src/runtime/`） | **零改动**——只读钉住的 `assembled_json`，不感知卡（:453,:1235-1243 通用装配含 idle 首 tick 路径照旧） | — |
| 7 | 经济账本与付费点（`ledger`/`livegate`/`shop`/`worlds::create_room`） | 仅 `resolve_share` 内部扩展 + 快照读取；`charge` 签名与全部付费点零改动 | P2 |
| 8 | 运营与发现 + admin（hot 榜/审核工作台/申诉/看板） | audit 认新 subject_kind（P0）；hot 榜与星级权重、评定端点（P3）；appeals 表结构天然兼容 | P0/P3 |

（观战直播与图谱可视化只消费 events 受众投影，卡维度对其透明，无影响面。）
