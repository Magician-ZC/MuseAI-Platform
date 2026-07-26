-- MuseAI 平台库 0039（R3：**if 线付费副本** —— 人设保险三级出口的第 3 级，最后一级）。
-- 总规格 `docs/build/spec-world-ecosystem.md` §7「人设保险（三级出口，公共事实不可改）」第 3 条原文：
--
--   **事后·if 线**：世界结束后花资源以某拍为分叉点开单人平行线副本（**不影响原世界线**）——
--   把遗憾变成付费内容。
--
-- 三级出口至此完整：事前底线硬约束（engine，critic）· 事中注解权（0037）· 事后 if 线（本迁移）。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第一红线：不影响原世界线（§0.3「公共事实不可回滚」）
-- ═══════════════════════════════════════════════════════════════════════════
-- 本迁移**只新建一张表，不 ALTER 任何既有表、不建任何指向世界线表的外键、不插一行种子数据**。
-- if 线是**平行线，不是改写**：
--   `worlds`（含 `narrative_state_json` / `state_revision`）· `world_events` · `world_ticks` ·
--   `world_members` · `world_contributions` · `consent_requests` · `interventions` ·
--   `world_biographies` · 结算账本（`backpacks` / `cloud_characters.mileage` / `ledger_*` /
--   `arena_rewards`）——**一个字节不动**。
--
-- 🔴 **if 线世界为什么不是一行 `worlds`**（本批次最重要的一个结构决定）：
--
--   `worlds` 是**结算管线的入口**。一行 `worlds` + 若干行 `world_members`，在
--   `runtime::commit_tick → end_world_tx → finalize_ending_tx` 里会自动触发
--   `progression::settle_idle_world_ending_tx`（发历练）、`subplot::settle_subplot_card_tx`（铸卡）、
--   `arena_rewards`（荣誉）。也就是说：**只要 if 线是一行 `worlds`，它的产出就会自动反哺账户资产**——
--   而历练是准入门槛与卡位解锁的钥匙，于是「花钱开 if 线」立刻等价于「花钱买数值」，
--   直接踩穿 §0.1「付费只买体验容量，永不买结果」。
--
--   把 if 线放进本表（独立实例、独立 id 空间 `ifw_`）之后，这条反哺路径在**物理上不存在**：
--   结算管线只认 `worlds` 行与 `world_members` 行，本表两者都不是，没有任何一条 SQL 会扫到它。
--   这比「在结算里加一个 `if is_ifline { skip }`」强得多——后者是一行随时可能被误删的判断。
--
-- 🔴 **if 线在形状上为什么不可能冒充原世界线**（四道结构性保证，口径抄 0037 的批注）：
--
--   ① **独立表的独立行**：if 线只存在于 `ifline_worlds`，世界事实只存在于 `worlds`/`world_events`。
--      两者无外键、无联合视图、无 UNION 读路径。想把 if 线读成世界线，得先写一条把两边并起来的
--      新查询——那是显式的、要过评审的动作，不会「不小心」发生。
--   ② **每一条 if 线都有主人**（`owner_id TEXT NOT NULL`，单人平行线）：`worlds` 没有 owner 列
--      （`host_user_id` 可空，语义是「谁开的房」而非「这个世界属于谁」）。一行有主人的世界在结构上
--      就不是「大家共处的那条世界线」。
--   ③ **引擎没有读取路径**：`runtime` 与 `crates/muse-engine` 对本表零引用（源码级 grep 断言）。
--      快照是**冻结的死数据**：它从原世界的终局态**复制**而来，复制之后与原世界再无任何同步通道，
--      既改不了过去，也影响不了原世界的未来。
--   ④ **读取面自带层次标签**：if 线只经 `/api/me/iflines/**` 与 `/api/worlds/{id}/ifline-fork-points`
--      出，每条恒带 `layer="ifline"` / `isWorldFact=false` / `affectsOriginWorld=false`；
--      世界事实经 `/api/worlds/{id}/events` 出（`events` 模块，本批次零改动）。两条管道各出各的。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第二红线：分叉点**不假装**（本批次最容易做假的地方）
-- ═══════════════════════════════════════════════════════════════════════════
-- 规格写的是「以某拍为分叉点」，但**仓库里没有任何一拍的状态快照**，核实如下：
--   - `world_ticks`（0001 + 0002 + 0030 + 0038）的列是
--     `base_revision / status / error / cost_tokens / attempts / started_at / finished_at /
--      off_peak / price_ratio_pct / defer_ms`——**没有一列存状态**，只存「那一拍基于哪个 revision」。
--   - `worlds.narrative_state_json` 是**单行、每拍被 CAS 覆盖**的最终态
--     （`runtime::commit_tick` 的 `UPDATE worlds SET narrative_state_json=? ... WHERE state_revision=?`），
--     历史版本不留存。
--   - `world_events` 存的是**投影后的展示文本**（`public_projection_json` / `private_projections_json`），
--     引擎的 `StatePatch`（状态变化的唯一事实源）在 `commit_tick` 里被**丢弃**，从不落库
--     → 事件流**无法重放**出中间态。
--   - 引擎 FS（`crates/muse-engine/src/store.rs`）是 DB 那一列的每拍物化，同样只有当前态。
--
-- 因此「精确还原第 N 拍」在当前数据模型下**做不到**。本批次的选择是**诚实降级**：
-- **只支持终局分叉**（`fork_point='terminal'`，即原世界最后一拍已落定的那份状态，逐字节复制），
-- 请求中间拍一律 **400 明确拒绝**，绝不用终局态冒充第 N 拍——
-- 「看起来是那一拍、其实不是」是最坏的一种实现，它会让玩家为一个假的分叉付费。
-- 想真正支持任意拍分叉，前置条件是先加一张逐拍状态快照表（成本：每拍多存一份完整
-- `NarrativeState`），那是另一个批次的事，不在本迁移范围内。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第三红线：社交防火墙（§14 恨隔面具原则）
-- ═══════════════════════════════════════════════════════════════════════════
-- 规格写的是「**单人**平行线副本」。原世界里其他玩家的角色**一律不进快照**：
-- 冻结前按 `world_members` 逐个比对，凡是「他人玩家的卡」的条目与其关系边全部剥离，
-- 剥离台账落 `redaction_json`（可审计、玩家可见）。
-- 理由是 §14 的直接推论：**未经同意把别人的角色拖进你的 if 线，并让它做原主人从没做过的事**，
-- 等于以他人角色之名生成他人未授权的言行——比暴露真人身份更难挽回。
-- NPC 不剥离：NPC 是世界的，不是谁的（与 `world_events` 无 owner 列同一条道理）。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第四红线：产出不得反哺原世界（§0.1 不卖胜负）
-- ═══════════════════════════════════════════════════════════════════════════
-- 本表**没有任何数值列**：没有历练、没有贡献分、没有奖励、没有系数、没有余额。
-- if 线的产物只可能是「内容」（玩家自己那条平行线的叙事），永远不会是「资产」。
-- 与之配套的源码级断言：`ifline` 模块零引用 `grant_mileage_tx` / `grant_item_tx` /
-- `grant_card_tx` / `settle_*` / `ledger` / `world_contributions`。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 「花资源」= 烧一张副本卡（§10 副本卡经济）
-- ═══════════════════════════════════════════════════════════════════════════
-- **不新造货币**（§0.5 无提现红线下多一种货币就多一条 RMT 侧门）。开 if 线消耗
-- `MUSE_IFLINE_CARD_COST`（默认 1）张在手的副本卡——副本卡正是「把你亲历的剧情副本铸成的卡」，
-- 用它去开那段剧情的平行线，语义上严丝合缝。
--
-- 消耗走**副本卡既有的状态机**：`status='owned' → 'consumed'` 的条件 UPDATE（CAS），
-- `consumed_into` 指向本表的 if 线 id、`consumed_at` 记时刻——与合成回收口逐字同款
-- （`subplot::synthesize` 的那段 SQL）。**本模块不 INSERT `subplot_cards`**：
-- 铸卡的唯一写入路径仍是 `subplot::grant_card_tx`（§0.2 资产单一写入路径），
-- 本模块只做「已发出的卡改状态」，不产生任何新资产。
-- 副作用是 if 线成为副本卡的**第二个回收口**（第一个是合成升级），对经济体是净收缩，无通胀风险。
--
-- ⚠️ 与 §10「永久蓝图（装入自定义房，房散卡在）」的关系要说清楚：那句话描述的是**自定义房装配**
-- 这条用途——房散了卡还在。if 线是**另一条用途**：把这张剧情结晶烧成一条只属于你的平行人生，
-- 一次性内容燃料。之所以选「烧」而不是「占用」，是因为「占用」需要一张绑定表，而合成端
-- （`subplot::synthesize`）的 CAS 只看 `status='owned'`，会把被占用的卡照熔不误——
-- 于是出现「卡熔了、if 线还开着」的白嫖漏洞，而堵它必须改 `subplot/`（本批次不越界）。
-- 「烧」则天然复用同一个状态机：卡一旦 `consumed`，合成端的 CAS 自动排除它，零跨模块接线。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 未验证功能默认关闭（§0.1）
-- ═══════════════════════════════════════════════════════════════════════════
-- 整块能力由运行时开关 **`MUSE_IFLINE_PARALLEL`** 控制，**默认关闭**（登记在 `flags::KNOWN_FLAGS`，
-- 解析链 user > world > global > env > 默认）。本迁移**不插任何种子数据**——建表 ≠ 开闸。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔与计数；
-- 无 JSONB、无 serial、无 NOW()、无 CHECK、无 ON CONFLICT、无 partial index、
-- 无 strftime/date_trunc。范式见 0037 / 0036 / 0034。

-- ---------------------------------------------------------------------------
-- if 线世界实例（**独立世界实例**，不是 `worlds` 行）
-- ---------------------------------------------------------------------------
CREATE TABLE ifline_worlds (
  -- if 线世界 id。前缀 `ifw_`，与 `worlds.id` 的前缀不同名空间——**光看 id 就分得清是哪一层**。
  id TEXT PRIMARY KEY,

  -- 🔴 唯一的主人（`users.id`）。规格「**单人**平行线副本」的落库形态，也是「不可能冒充世界线」的
  -- 结构性保证之一：`worlds` 没有 owner 列。本列 NOT NULL，永不为空。
  owner_id TEXT NOT NULL,
  -- 主角卡（`cloud_characters.id`）。受理时校验：属本人 + 在过原世界（`world_members` 有行）+
  -- **在世**（`memorial_status='living'` 且 `withdrawn=0`）。
  -- 🔴 传世卡不得进 if 线：§12「传世卡只读、**不可再入世界**」。允许了就是「付费复活」，
  -- 那正是本项最需要避免的「付费改命」形态。
  character_id TEXT NOT NULL,

  -- ── 分叉来源（**只读指针，永不写回**） ──────────────────────────────────
  -- 原世界（`worlds.id`）。本模块对该行只有 SELECT，全仓不存在从本模块出发的任何 worlds 写入。
  origin_world_id TEXT NOT NULL,
  -- 原世界的模板指针（内容蓝图解引用入口，与副本卡的 source_template_* 同口径）。
  origin_template_id TEXT NOT NULL DEFAULT '',
  origin_template_version BIGINT NOT NULL DEFAULT 0,

  -- ── 分叉点（🔴 诚实标注，见文件头第二红线） ────────────────────────────
  -- 分叉点档位。当前**只有 `terminal`**（原世界终局态）。留列而不是写死，是为了将来真加了
  -- 逐拍状态快照表之后可以扩出 `tick`（任意拍）而不必改表；在那之前任何其它取值都进不来。
  fork_point TEXT NOT NULL DEFAULT 'terminal',
  -- 分叉发生在哪一拍（= 原世界最后一拍已落定的 `world_ticks.tick_no`）。记录它是为了让玩家与运营
  -- 都能对上「这条 if 线是从哪儿岔出去的」，**不代表可以从别的拍岔**。
  fork_tick_no BIGINT NOT NULL,
  -- 分叉时原世界的 `state_revision`（快照身份证）。原世界已 ended 故此值恒定，用于事后核验
  -- 「这份快照确实是那个时点的那份状态」。
  fork_state_revision BIGINT NOT NULL,
  -- 🔴 状态保真度，**必须随每一次读取一起下发**：
  --   `origin_terminal_state` = 原世界终局态的逐字节复制（当前唯一取值）。
  -- 将来若出现降级档（例如「只带走角色卡与关系」），必须在此显式标注，
  -- 绝不允许出现一份没写清保真度的快照——那等于让玩家自己去猜他买到的是什么。
  state_fidelity TEXT NOT NULL DEFAULT 'origin_terminal_state',

  -- 冻结的分叉态（JSON 文本）：从 `worlds.narrative_state_json` 复制、按 §14 剥离他人玩家角色后落定。
  -- 🔴 **复制之后与原世界再无同步通道**：原世界不会因为这份快照变化，这份快照也不会因为原世界变化
  -- （原世界已 ended，本就不再变）。它是死数据，不是引用。
  snapshot_json TEXT NOT NULL,
  -- §14 剥离台账（JSON 文本）：被移除的他人角色 id、被移除的关系边数、剥离理由。
  -- 玩家可见 + 运营可审：不能既剥离了又不告诉人剥离了什么，那会让 if 线的内容缺口无从解释。
  redaction_json TEXT NOT NULL DEFAULT '{}',
  -- 快照里是否含主角本人（保真度的一部分；世界从未把该卡写进状态时为 0）。
  protagonist_in_snapshot INTEGER NOT NULL DEFAULT 0,

  -- ── 玩家写的「如果……」 ────────────────────────────────────────────────
  -- 分叉前提（玩家手写，可空）。这是 if 线的内容种子：「如果他那时没有退那一步」。
  premise TEXT NOT NULL DEFAULT '',
  -- 机审裁决：approved / pending / rejected。**无论裁决都落库，读取面仅 approved 才给正文**
  -- （范式同 0037 的 `character_annotations.moderation`）。私密不豁免机审。
  premise_moderation TEXT NOT NULL DEFAULT 'pending',

  -- ── 花掉的资源（副本卡，见文件头） ────────────────────────────────────
  -- 烧掉的副本卡 id 列表（JSON 数组，升序）。反向血缘在 `subplot_cards.consumed_into`，两边互为对账。
  -- 🔴 **不是数值**：这里存的是「哪几张卡没了」，不是余额、不是价格、不是积分。
  cost_card_ids_json TEXT NOT NULL DEFAULT '[]',

  -- 生命周期。当前恒为 `sealed`（已立项、分叉态已冻结、资源已扣；推进由运行器接线，见模块头待办）。
  -- 留列是为了运行器接线后可扩 `running` / `ended`，本批次不产生其它取值。
  status TEXT NOT NULL DEFAULT 'sealed',

  -- 🔴 幂等键（与 `owner_id` 组唯一）：`{origin_world_id}:{character_id}:{fork_point}:{fork_tick_no}`。
  -- **同一个人拿同一张卡从同一个分叉点只开得出一条 if 线**。这一条约束同时挡住两件事：
  --   ① 重复点击 / 并发请求把副本卡扣两次（钱货两清的底线）；
  --   ② 同一个遗憾被反复变现（那会把 if 线变成刷内容的通道）。
  -- 单靠 `Idempotency-Key` 头挡不住「换个 key 再点」，故幂等必须落在 DB 约束上（范式同 0037）。
  fork_key TEXT NOT NULL,

  created_at BIGINT NOT NULL
);

-- 🔴 幂等的物理保证。
CREATE UNIQUE INDEX idx_ifline_worlds_fork_unique ON ifline_worlds(owner_id, fork_key);
-- 玩家读取面（我的 if 线，按时间倒序）。最左前缀即 owner_id：任何漏掉 owner 过滤的查询都用不上
-- 这个索引，慢查询会先于信息泄露暴露出来（结构性提醒，保证仍在应用层的 `WHERE owner_id = ?`）。
CREATE INDEX idx_ifline_worlds_owner ON ifline_worlds(owner_id, created_at);
-- 运营读取面 / 急停排查（按状态 + 时间）。
CREATE INDEX idx_ifline_worlds_status ON ifline_worlds(status, created_at);
-- 「这个原世界岔出过几条 if 线」——运营侧看某个世界的遗憾变现规模，也是红线巡检的入口。
CREATE INDEX idx_ifline_worlds_origin ON ifline_worlds(origin_world_id, created_at);
