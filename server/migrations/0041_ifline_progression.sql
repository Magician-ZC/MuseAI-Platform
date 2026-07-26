-- MuseAI 平台库 0041（R3：**if 线推进（跑拍）接线**）—— 0039 立的项，在这里真正跑起来。
--
-- 0039 交付的是 if 线的「立项与开局」：校验 → 烧副本卡 → 逐字节冻结分叉态 → 注册独立实例 → 可读可审，
-- `status` 恒为 `sealed`。玩家花掉一张副本卡，拿到的是一个**冻结的开局**——剧情推不动。
-- 本迁移补上推进：`sealed → running → ended`，一拍一行落 `ifline_beats`。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 最高红线：if 线的终局绝不进入任何一条结算管线
-- ═══════════════════════════════════════════════════════════════════════════
-- `progression::settle_idle_world_ending_tx`（发历练）/ `subplot::settle_subplot_card_tx`（铸卡）/
-- `arena_rewards`（荣誉）—— **一条都不许进**。
--
-- 理由不是洁癖，是定价结构：历练是**准入门槛与卡位解锁的钥匙**。一旦 if 线的终局能发历练，
-- 「花钱开 if 线」立刻等于「花钱买数值」，踩穿总规格 §0.1「付费只买体验容量，永不买结果」
-- 与平台红线「不卖胜负与数值平权」。
--
-- 0039 的结构性防线在本迁移里**一寸不退**：
--
--   ① **推进不写 `worlds`，不写 `world_ticks`**。这是本迁移最重要的一个决定，也是最容易图省事
--      走反的一步。`runtime` 的 tick 管线与结算管线是**连体的**——
--      `commit_tick` 里状态 CAS 成功即评估终局、终局即 `end_world_tx → finalize_ending_tx →
--      settle_*`。只要 if 线的一拍落进 `world_ticks`、或者 if 线是一行 `worlds`，
--      它就会被那条自动链路捡走。所以推进走的是**另一套表、另一套代码路径**
--      （`ifline_beats` + `ifline::runner`），与 `runtime::commit_tick` 零交叉。
--      物理隔离胜过「在结算里加一行 `if is_ifline { skip }`」——后者是一行随时会被误删的判断。
--   ② **本表没有任何数值列**：没有历练、没有贡献分、没有奖励系数、没有余额、没有掉落。
--      唯一的数字是 `cost_tokens`（**花出去的**成本，不是**发下来的**收益）——方向相反，不构成产出。
--   ③ **终局产物只可能是内容**：`ending_reason` / `ending_label` + `ifline_beats.prose`
--      拼成一份可读的私人传记。没有一行 SQL 会把它变成资产。
--
-- 用例（`ifline::tests`）：`red_line_ifline_ending_grants_nothing`（跑到终局后
-- `cloud_characters.mileage` 全表求和 / `subplot_cards` 总行数 / `backpacks` / `arena_rewards` /
-- `world_contributions` 全部零变化）+ 源码级 `red_line_never_mints_assets` / `red_line_never_writes_worldline`
-- （两者的扫描面在本批次**扩到 `runner.rs`**，否则新增文件是红线的盲区）。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 成本必须可追踪（付费功能的成本失真是最不该有的失真）
-- ═══════════════════════════════════════════════════════════════════════════
-- if 线跑拍同样烧 token，但**不能写 `world_ticks`**（那是世界线的表，见上）。于是成本记两处：
--   - `ifline_beats.cost_tokens` —— 逐拍实测（口径与 `world_ticks.cost_tokens` **完全一致**：
--     同一个 `TokenMeter` 汇总引擎每次 ModelCall 的 input+output，模型未回报时回退引擎预估）；
--   - `ifline_worlds.cost_tokens_total` —— 该条 if 线的累计（同事务累加，便于按实例查，无需扫明细）。
-- 运营读取面：`GET /api/admin/iflines/cost`（本批次新增）。
-- ⚠️ **现状必须说清**：`admin_api::dashboards` 的成本看板只 SUM `world_ticks.cost_tokens`，
-- **尚未并入 if 线开销**——即主看板当前会系统性漏掉这部分。不静默漏掉的做法是：本迁移显式记账、
-- 单开运营端点让它可见、并在 `docs/API.md` 与 `docs/VALIDATION.md` 写明「主看板未接」。
-- 接的时候只需在看板里并上 `SELECT SUM(cost_tokens) FROM ifline_beats WHERE created_at BETWEEN ? AND ?`
-- （本迁移已为此建好 `idx_ifline_beats_created`）。之所以本批次不动 `dashboards.rs`：
-- 那是并行批次正在改的文件，跨批次抢改会把两边的账都搅乱。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 叙事质量 SLO 的归属：**不并入世界线 SLO**
-- ═══════════════════════════════════════════════════════════════════════════
-- 现有五项 SLO（基尼 / 无戏份 / 收尾 / 二次入世 / 状态-文本矛盾）度量的是**多人世界线**的质量。
-- if 线是**单人**平行线，前四项对它要么无意义、要么会污染世界线指标：
--   - 基尼（戏份分布不平等度）：单人样本的基尼恒为 0（完美），每条 if 线都会往池子里灌一个满分，
--     **把真实的多人不公平稀释掉**——这不是"多了点噪声"，是让指标失去报警能力；
--   - 无戏份率：单人线里结构上不可能有人没戏份，同样只贡献免费的合格样本；
--   - 二次入世率：if 线**没有入世**这件事（不进 `world_members`、不 join），指标无所指；
--   - 收尾率：if 线的收尾常由**拍数上限**强制触发（见下），与世界线「叙事弧完成」不是同一件事，
--     混进去会悄悄改变这项指标的定义。
-- 唯一真正同质的是**状态-文本矛盾**（逐回合质检，与人数无关）。为它留了 `ifline_beats.critic_json`
-- （逐拍存引擎 critic 报告），将来要做「if 线质量读数」时数据现成——但那必须是**独立读数**，
-- 不是并进世界线 SLO 的同一个池子。本批次**不动 `slo/`**。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 §14 单人平行线：推进时同样不得引入其他真人玩家
-- ═══════════════════════════════════════════════════════════════════════════
-- 开局已按 `world_members` 剥离他人玩家角色（0039），推进时**每一拍再剥一次**（纵深防御）：
-- 从 `ifline_worlds.cast_json` 组阵容时，凡命中原世界 `world_members` 里他人的 `cloud_character_id`
-- 一律剔除；NPC 保留（NPC 是世界的，不是谁的）。演员表逐拍落 `ifline_beats.cast_json`，可审。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 确定性契约
-- ═══════════════════════════════════════════════════════════════════════════
-- 不用系统随机、不用浮点 RNG、不依赖 map 迭代顺序驱动 RNG。
-- 每条 if 线在首次推进时钉一个 `run_seed`（`fnv1a_64` 派生，十六进制文本落库，永不再变），
-- 逐拍子流 `Rng(fnv1a_64(run_seed ‖ beat_no) ^ 0x5B)`（SplitMix64；域常量 `DOMAIN_IFLINE_CAST=0x5B`，
-- 已登记进 `assembly` 的域常量表——0x51-0x5A 已被装配层与仿真工装占用）。
-- 抽样对象**先排序成 Vec 再抽**（绝不在 BTreeMap 迭代上驱动 RNG）。
-- 于是：**同样的分叉态 + 同样的 run_seed + 同样的 beat_no → 同样的演员表**，可复算、可复现。
-- （模型本身的采样不确定性不在此列——那是外部系统，我们能保证的是喂进去的东西逐字节可复现。）
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔与计数；
-- 无 JSONB、无 serial、无 NOW()、无 strftime/date_trunc。
-- `ALTER TABLE ... ADD COLUMN` 一句一列（SQLite 与 Postgres 语法一致，范式同 0034 / 0038）。

-- ---------------------------------------------------------------------------
-- ifline_worlds：推进态
-- ---------------------------------------------------------------------------

-- 确定性种子（十六进制 u64 文本；'' = 尚未推进过）。首次推进时派生并**一次性钉死**，此后永不改写。
-- 用 TEXT 而不是 BIGINT：u64 塞进有符号 BIGINT 会在半数取值上变成负数，两库的显示与比较行为还不一致，
-- 而这个值唯一的用途是被读回来重新派生子流——文本十六进制既无歧义也可直接肉眼对账。
ALTER TABLE ifline_worlds ADD COLUMN run_seed TEXT NOT NULL DEFAULT '';

-- 当前**活的**叙事态（JSON）。'' = 尚未推进过，此时以 `snapshot_json` 为准。
-- 🔴 与 `snapshot_json` 分列而不是就地覆盖：`snapshot_json` 是**分叉点证据**
--（「这条 if 线确实是从原世界那一拍那份状态岔出去的」），一旦被推进覆盖，玩家与运营就再也无法
-- 核验保真度，`state_fidelity` 那一列跟着变成一句无法证伪的话。冻结的必须一直冻结。
ALTER TABLE ifline_worlds ADD COLUMN live_state_json TEXT NOT NULL DEFAULT '';

-- 活态修订号（推进的 CAS 令牌，语义同 `worlds.state_revision`）。提交时
-- `WHERE id = ? AND live_revision = ?`，命中 0 行即并发冲突整笔放弃——**不是先查后写**。
ALTER TABLE ifline_worlds ADD COLUMN live_revision BIGINT NOT NULL DEFAULT 0;

-- 已推进的拍数（= 下一拍的 `beat_no`，0 基）。同时是拍数上限的计数器。
ALTER TABLE ifline_worlds ADD COLUMN beat_count BIGINT NOT NULL DEFAULT 0;

-- 阵容与场景（JSON）：`{ "npcs": [...], "locations": [...], "endings": [...] }`。
-- 首次推进时从原世界 `assembled_json` **复制一次**后钉死。
-- 🔴 复制而不是每拍现读原世界：0039 的立论是「快照是死数据，与原世界再无同步通道」。
-- 每拍回原世界取 NPC 会把这条通道重新打开（原世界一旦被改，if 线跟着变），
-- 那 if 线就不再是「一条独立的平行线」，而是「原世界的一个视图」。
ALTER TABLE ifline_worlds ADD COLUMN cast_json TEXT NOT NULL DEFAULT '';

-- 🔴 累计 token 成本。**这是花出去的钱，不是发下来的收益**——它是本表唯一的数字列，
-- 方向与「产出」相反，不构成任何可反哺原世界的资产。
ALTER TABLE ifline_worlds ADD COLUMN cost_tokens_total BIGINT NOT NULL DEFAULT 0;

-- 终局原因：'' | 'mainline_done' | 'time_cap' | 'starved' | 'beat_cap'。
-- 前三者来自引擎终局信号（与世界线**同一个判定函数** `muse_engine::narrative::is_terminal`——
-- if 线是一条真的叙事线，不是降级模拟，故用同一把尺）；`beat_cap` 是本模块的成本闸（见下）。
ALTER TABLE ifline_worlds ADD COLUMN ending_reason TEXT NOT NULL DEFAULT '';

-- 结局名（内容标签，取自 `cast_json.endings` 首项，确定性；无则空）。
-- 🔴 **它是一个字符串，不是一个奖励**。世界线的结局会经 `finalize_ending_tx` 触发荣誉与结算，
-- if 线的结局只是这条私人传记的最后一行字。
ALTER TABLE ifline_worlds ADD COLUMN ending_label TEXT NOT NULL DEFAULT '';

ALTER TABLE ifline_worlds ADD COLUMN ended_at BIGINT;

-- ---------------------------------------------------------------------------
-- ifline_beats：一拍一行（**不是 `world_ticks`**，见文件头红线①）
-- ---------------------------------------------------------------------------
CREATE TABLE ifline_beats (
  -- 前缀 `ifb_`。与 `world_ticks.id` 不同名空间——光看 id 就分得清这一拍属于哪一层。
  id TEXT PRIMARY KEY,

  -- 所属 if 线（`ifline_worlds.id`，`ifw_` 前缀）。
  -- 刻意**不建外键**：与 0039 同口径（本功能全程不与世界线表结构耦合），删档走应用层。
  ifline_id TEXT NOT NULL,
  -- 第几拍（0 基，连续）。与 `ifline_id` 组唯一 —— 这是**推进的并发闸**：
  -- 两个并发推进请求抢同一个 `beat_no`，先到的插入成功即取得该拍的所有权，
  -- 后到的撞唯一键（`ON CONFLICT DO NOTHING` → 影响 0 行）直接 409。
  -- 🔴 用唯一键抢占而不是「先查 beat_count 再写」：后者在并发下两个请求会读到同一个计数，
  -- 各跑各的模型、各花各的 token，最后各写各的状态——重复扣成本且状态互相覆盖。
  beat_no BIGINT NOT NULL,

  -- 'running'（已抢占，模型跑批中）| 'done'（已提交状态）| 'blocked'（硬节点不可满足，未提交状态）
  -- | 'failed'（引擎重试后仍失败）。
  status TEXT NOT NULL DEFAULT 'running',
  -- 本拍基于哪个 `live_revision`（CAS 前置条件，语义同 `world_ticks.base_revision`）。
  base_revision BIGINT NOT NULL DEFAULT 0,

  -- 本拍子流种子（十六进制，= `fnv1a_64(run_seed ‖ beat_no) ^ DOMAIN_IFLINE_CAST`）。
  -- 落库是为了**可复算**：拿这一行就能重放出当时的演员表，不必反推。
  seed_hex TEXT NOT NULL DEFAULT '',
  -- 🔴 §14 台账：本拍实际上场的角色 id（JSON 数组，升序）。主角 + 若干 NPC，**不含任何他人玩家角色**。
  -- 逐拍存而不是只存一次：剥离是每拍都做的动作，每拍都要留下可审的证据。
  cast_json TEXT NOT NULL DEFAULT '[]',

  -- 本拍正文（引擎 writer 产出）。**这就是 if 线的产物**——内容，不是资产。
  prose TEXT NOT NULL DEFAULT '',
  -- 机审三态：approved / pending / rejected。私密不豁免机审（口径同 0037 批注与 0039 分叉前提）：
  -- 私密只决定「谁能看」，不决定「平台是否为它负责」。无论裁决都落库，读取面仅 approved 给正文。
  moderation TEXT NOT NULL DEFAULT 'pending',
  -- 引擎 critic 报告（JSON）。**为将来的「if 线独立质量读数」留的数据**，
  -- 不进世界线 SLO 池子（理由见文件头）。
  critic_json TEXT NOT NULL DEFAULT '{}',

  -- 🔴 本拍实测 token 成本（口径与 `world_ticks.cost_tokens` 逐字一致）。
  -- 成本可追踪是付费功能的底线：玩家花了一张副本卡，平台花了多少算力必须查得出来。
  cost_tokens BIGINT NOT NULL DEFAULT 0,

  -- 本拍产生的终局信号 / 阻断原因（'' = 无）。终局值域同 `ifline_worlds.ending_reason`。
  terminal_reason TEXT NOT NULL DEFAULT '',
  -- 失败或阻断的可读原因（供玩家看「为什么这一拍没往前走」，也供运营排查）。
  note TEXT,

  created_at BIGINT NOT NULL,
  finished_at BIGINT
);

-- 🔴 推进并发闸（见 `beat_no` 注释）。这条唯一索引是「一拍只跑一次」的物理保证。
CREATE UNIQUE INDEX idx_ifline_beats_unique ON ifline_beats(ifline_id, beat_no);
-- 玩家读取面（一条 if 线的正文按拍序读 = 那份私人传记）。
CREATE INDEX idx_ifline_beats_read ON ifline_beats(ifline_id, beat_no, status);
-- 🔴 成本看板接入点：按时间窗求和 if 线开销。主看板尚未并入（见文件头），建好索引使接入是一句 SQL。
CREATE INDEX idx_ifline_beats_created ON ifline_beats(created_at);
