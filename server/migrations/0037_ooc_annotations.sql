-- MuseAI 平台库 0037（R3：**OOC 注解权** —— 人设保险三级出口的第 2 级）。
-- 总规格 `docs/build/spec-world-ecosystem.md` §7「人设保险（三级出口，公共事实不可改）」第 2 条原文：
--
--   **事中·注解权**：单拍 OOC 申诉——世界事实不改，**私人传记可加内心批注**；
--   复核确认模型错误则补偿托梦配额。**事实归世界，解释权归玩家。**
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第一红线：世界事实不改（§0.3「公共事实不可回滚」）
-- ═══════════════════════════════════════════════════════════════════════════
-- 本迁移**只新建三张表，不 ALTER 任何既有表、不建任何指向世界线表的外键、不插一行种子数据**。
-- 于是「申诉」这件事在物理上没有任何写入世界线的路径：
--   `world_events` / `worlds.narrative_state_json` / `world_ticks` / `world_contributions` /
--   `consent_requests` / `interventions` / 结算账本（`ledger_*` / `backpacks` / `cloud_characters.mileage`）
-- 全部一个字节不动。申诉的产物是**另一层数据**，与公共世界线并存而非覆盖。
--
-- 🔴 **批注为什么物理上无法冒充事实**（这是本批次最容易做错的地方，四道结构性保证）：
--
--   ① **独立表的独立行**：批注只存在于 `character_annotations`，世界事实只存在于 `world_events`。
--      两张表之间没有外键、没有联合视图、没有任何 UNION 的读路径。想把批注读成事实，
--      得先写一条把两张表并起来的新查询——那是显式的、要过评审的动作，不会「不小心」发生。
--   ② **每一行批注都有主人**（`owner_id NOT NULL`）：世界事实**没有 owner 列**（`world_events`
--      是世界的，不是谁的）。一行有主人的数据在结构上就不可能是「世界说的」。
--   ③ **引擎没有读取路径**：`runtime` / `crates/muse-engine` 对本表零引用（源码级 grep 断言）。
--      批注永远进不了 `RoundInput.state`，于是它既改不了过去（事实已落定），也影响不了未来
--      （不进决策）。口径与 0025 贡献账本 / 0030 critic 报告 / 0034 故人印记逐字一致。
--   ④ **读取面自带层次标签**：批注只经 `/api/me/**` 出，且每条恒带 `layer="annotation"` 与
--      `isWorldFact=false`；世界事实经 `/api/worlds/{id}/events` 出，两条管道各出各的。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第二红线：托梦补偿**不改 interventions 的计数口径**
-- ═══════════════════════════════════════════════════════════════════════════
-- 托梦配额当前由 `interventions::create_intervention` 判定：
--     `COUNT(*) FROM interventions WHERE world_id=? AND character_id=? AND kind='whisper'
--                                    AND status IN ('accepted','applied')  >= dream_quota_per_stage()`
-- 补偿**绝不**通过往 `interventions` 里插行或改行来兑现——那等于伪造一条玩家从未发过的托梦，
-- 或者篡改一条已经被引擎消费过的记录（后者还会让「已被消费」这个事实消失）。
--
-- 本迁移的做法是**只加加数、不动被加数**：补偿落 `dream_quota_compensations` 独立账，
-- 有效配额 = `dream_quota_per_stage() + SUM(grants)`。左边那个 `COUNT(*)` 的 SQL 一个字符不变，
-- 变的只是它被拿去比较的**阈值**。见 `annotations::dream_quota_bonus` 与其上方的接线说明。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 未验证功能默认关闭（§0.1）
-- ═══════════════════════════════════════════════════════════════════════════
-- 整块能力由运行时开关 `MUSE_OOC_ANNOTATIONS` 控制，**默认关闭**（登记在 `flags::KNOWN_FLAGS`，
-- 解析链 user > world > global > env > 默认）。本迁移**不插任何种子数据**——建表 ≠ 开闸。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔 / 单列 ADD COLUMN；
-- 无 JSONB、无 serial、无 NOW()、无 CHECK、无 ON CONFLICT、无 partial index、
-- 无 strftime/date_trunc。范式见 0036 / 0035 / 0031。

-- ---------------------------------------------------------------------------
-- ① OOC 申诉：针对**某一拍**的一次异议
-- ---------------------------------------------------------------------------
-- 「单拍」是规格原文的粒度（§7「**单拍** OOC 申诉」）：申诉对象恒为 (world_id, tick_no, character_id)
-- 三元组，不存在「对整个世界申诉」这种形态——那会变成「我不喜欢这个结局」，无从复核。
CREATE TABLE ooc_appeals (
  id TEXT PRIMARY KEY,

  -- 被申诉的那一拍所在世界（`worlds.id`）。
  world_id TEXT NOT NULL,
  -- 被申诉的拍号（`world_ticks.tick_no`）。受理时校验该拍确实已提交（不许对不存在的拍申诉）。
  tick_no BIGINT NOT NULL,
  -- 谁的角色演得不像（`cloud_characters.id`）。受理时校验它是本人在该世界的在场卡。
  character_id TEXT NOT NULL,
  -- 申诉人（`users.id`）。角色的主人，冗余一列免去每次读取面都 JOIN `world_members`。
  user_id TEXT NOT NULL,

  -- 异议类别。当前白名单 'ooc'（角色演得不像自己）/ 'unfair_ruling'（裁决不公）——
  -- 正对 VALIDATION §2 T1 门槛原文「**OOC/裁决不公**申诉 <10%/阶段」的两个词。
  -- 白名单在代码内（`annotations::REASON_CODES`），不做 DB CHECK（方言不可移植，且分类会随数据演进）。
  reason_code TEXT NOT NULL DEFAULT 'ooc',
  -- 玩家自述的理由（必填，长度上限在应用层）。这是 T1 门槛之外最有价值的定性素材。
  reason_text TEXT NOT NULL DEFAULT '',

  -- 复核状态：pending（待复核）/ confirmed（**确认模型错误**，触发托梦补偿）/ dismissed（不予支持）。
  -- 🔴 刻意不用 `upheld`：`moderation_appeals`（内容风控申诉）里的 `upheld` 意思是「**维持原判**」
  -- = 申诉被驳回，与这里的「申诉成立」正好相反。同一个词在两张表里指相反的事，是最容易在
  -- 看板上算反的那类命名。
  -- 🔴 注意三种状态**都不改世界线**——confirmed 的含义是「我们承认这一拍演砸了」，
  -- 不是「这一拍没发生过」。承认错误与回滚事实是两件事，规格选的是前者。
  status TEXT NOT NULL DEFAULT 'pending',

  -- 复核留痕（现状面；流水面另落 `audit_logs`，两边都写，缺一不可——口径同 0036 的 updated_by）。
  reviewer_id TEXT NOT NULL DEFAULT '',
  review_reason TEXT NOT NULL DEFAULT '',
  reviewed_at BIGINT NOT NULL DEFAULT 0,

  created_at BIGINT NOT NULL
);

-- 🔴 幂等键 = (世界, 拍, 角色)。**同一拍同一角色只受理一次**。
-- 允许多条会让「申诉率」这个 SLO 的分子直接失真（一个人反复点 = 一群人在申诉），
-- 也会让复核队列被同一件事刷屏。唯一约束把它挡在写入处：重复提交读回既有行返回（`created:false`），
-- 不是 409——重复点击不该被当成错误，它只是「已经受理过了」。
CREATE UNIQUE INDEX idx_ooc_appeals_slot ON ooc_appeals(world_id, tick_no, character_id);

-- 复核队列读路径（admin 按状态 + 时间翻页）。
CREATE INDEX idx_ooc_appeals_status ON ooc_appeals(status, created_at);
-- 玩家读取面（我的申诉，按时间倒序）。
CREATE INDEX idx_ooc_appeals_user ON ooc_appeals(user_id, created_at);
-- SLO 窗口扫描（`slo::ooc_appeal_block` 按 created_at 区间聚合）。
CREATE INDEX idx_ooc_appeals_window ON ooc_appeals(created_at);
-- 补偿求和路径（`annotations::dream_quota_bonus` 按世界+角色）。
CREATE INDEX idx_ooc_appeals_target ON ooc_appeals(world_id, character_id);

-- ---------------------------------------------------------------------------
-- ② 私人传记批注：玩家的**解释层**
-- ---------------------------------------------------------------------------
-- 规格原文「私人传记可加**内心批注**」。这是整条规则的产品内核：
-- **事实归世界，解释权归玩家**——世界说「他在城门口退了一步」，玩家可以在自己的传记里补一句
-- 「他不是怕，他在等那个人先走」。两句话并存，前者是公共事实，后者是私人解释。
--
-- 🔴 **只对本人可见**。读取面恒为 `/api/me/**` 且 SQL 恒带 `WHERE owner_id = ?`。
--    与 §14 社交防火墙一致：批注**不出真人身份**——不记昵称、不记手机号，`owner_id` 只用于
--    权限过滤，从不出现在任何响应体里。别人（包括同世界的其他玩家、包括观战者）看不到它存在。
CREATE TABLE character_annotations (
  id TEXT PRIMARY KEY,

  -- 🔴 唯一可见者（`users.id`）。本列 NOT NULL 是「批注不可能冒充世界事实」的结构性保证之一：
  -- 世界事实表（`world_events`）没有 owner 列，一行有主人的数据在形状上就不是世界说的话。
  owner_id TEXT NOT NULL,

  -- 批注挂在谁的传记上（`cloud_characters.id`）。
  character_id TEXT NOT NULL,
  -- 批注针对哪个世界的哪一拍（与 `ooc_appeals` 同坐标；读取面按此排序还原时间线）。
  world_id TEXT NOT NULL,
  tick_no BIGINT NOT NULL,

  -- 来源申诉（`ooc_appeals.id`）。当前批注**必随申诉产生**，故恒非空；
  -- 留成普通列而非外键：外键级联删除会让「删一条申诉顺手删掉玩家写的话」成为可能，
  -- 而玩家写的东西不该被别的表的生命周期决定（口径同 0036 对灰度记录不建外键的理由）。
  appeal_id TEXT NOT NULL DEFAULT '',

  -- 批注正文（玩家手写）。
  body TEXT NOT NULL DEFAULT '',

  -- 机审裁决：approved / pending / rejected。
  -- 🔴 **无论裁决都落库，但读取面仅 approved 才给正文**（范式同 `worlds.cover_url` /
  -- `cloud_characters.avatar_moderation`）：人审改判后无需玩家重写。
  -- 私密不豁免机审——私密只决定「谁能看」，不决定「平台是否为它负责」。
  moderation TEXT NOT NULL DEFAULT 'pending',

  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);

-- 一条申诉至多一条批注（改写走 UPDATE，不产生第二行）。
CREATE UNIQUE INDEX idx_character_annotations_appeal ON character_annotations(appeal_id);
-- 🔴 读取面唯一路径：**owner 打头**。最左前缀即 owner_id，任何漏掉 owner 过滤的查询都用不上这个索引，
-- 慢查询会先于信息泄露暴露出来（结构性提醒，不是保证——保证在应用层的 WHERE owner_id = ?）。
CREATE INDEX idx_character_annotations_owner
  ON character_annotations(owner_id, character_id, world_id, tick_no);

-- ---------------------------------------------------------------------------
-- ③ 托梦配额补偿账：复核确认模型错误后的补偿
-- ---------------------------------------------------------------------------
-- 规格原文「复核确认模型错误则**补偿托梦配额**」。
--
-- 🔴 **为什么是独立的一张加数表，而不是往 `interventions` 里写**：
--   - 往 `interventions` 插一行「假托梦」= 伪造玩家从未发过的言论，且会被 runtime 当真喂给引擎；
--   - 把某条已 `applied` 的托梦改成别的状态 = 抹掉「它已经被消费过」这个已落定的事实
--     （§0.3 公共事实不可回滚，且那条托梦确实影响过世界）；
--   - 两者都要改 `interventions/` 的表数据，而配额计数口径正建立在那张表的行上——
--     动被加数就是动口径。
-- 本表只提供**加数**：有效配额 = `dream_quota_per_stage()`（不变） + `SUM(grants)`（本表）。
-- `interventions` 的那条 `COUNT(*) ... status IN ('accepted','applied')` 一个字符都不用改。
CREATE TABLE dream_quota_compensations (
  id TEXT PRIMARY KEY,

  -- 因哪条申诉而补偿（`ooc_appeals.id`）。**唯一索引即幂等闸**：一条申诉至多补一次，
  -- 重复复核 / 并发复核都不会补出第二份（复核端另有 `status='pending'` 的 CAS 作第一道闸）。
  appeal_id TEXT NOT NULL,

  -- 补给谁的哪张卡在哪个世界（与托梦配额的计数坐标 (world_id, character_id) 严格对齐——
  -- 对不齐就等于补了个用不上的额度）。
  world_id TEXT NOT NULL,
  character_id TEXT NOT NULL,
  -- 受益人（`users.id`）。读取面「我被补了多少」按人过滤，省一次 JOIN。
  user_id TEXT NOT NULL,

  -- 补偿条数（正整数）。参数化（§0.2 禁写死）：`MUSE_OOC_COMPENSATION_WHISPERS`，默认 1。
  -- 🔴 **这不是资产**：托梦是「说话的机会」，不是道具、不是历练、不是余额，
  -- 因此不走 `grant_item_tx` / `grant_mileage_tx` / 复式账本——那三条是资产单一写入路径（§0.2），
  -- 把非资产塞进去会污染账本的守恒断言。它也永不进引擎决策（§0.1 平权红线）：
  -- 多一次说话机会不等于多一分胜算，托梦本就可被角色依本性忽略。
  grants BIGINT NOT NULL DEFAULT 0,

  -- 谁批的（复核人 `users.id`）+ 为什么（复核理由）。流水面另落 `audit_logs`。
  granted_by TEXT NOT NULL DEFAULT '',
  reason TEXT NOT NULL DEFAULT '',

  created_at BIGINT NOT NULL
);

-- 🔴 一条申诉一次补偿（幂等的物理保证）。
CREATE UNIQUE INDEX idx_dream_quota_comp_appeal ON dream_quota_compensations(appeal_id);
-- 求和路径：`SUM(grants) WHERE world_id = ? AND character_id = ?`（将来由 interventions 侧读）。
CREATE INDEX idx_dream_quota_comp_target ON dream_quota_compensations(world_id, character_id);
-- 玩家读取面（我的补偿记录）。
CREATE INDEX idx_dream_quota_comp_user ON dream_quota_compensations(user_id, created_at);
