-- MuseAI 平台库 0030（叙事质量 SLO 数据补齐，`docs/VALIDATION.md` §4.2「补齐性价比排序」①③）。
--
-- 本迁移只做两件事，都是「数据已经存在、只是没被留下来」的纯观测补缺，**无任何产品行为变化**：
--   ① `world_tick_critic`   —— 引擎每回合已经跑完的叙事 critic 报告落库（§4.2「状态-文本矛盾率」的直接数据源）。
--   ③ `world_members.left_at` —— 退出时刻（§4.2「用户跳过/退出率」此前只能算截面、算不了留存曲线的原因）。
--
-- （② `fact.consequence` 进事件投影是纯代码改动，见 `server/src/events/mod.rs::event_summary`，不需要迁移。）
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔与计数 / 无方言特性
-- （无 JSONB、无 serial、无 NOW()、无 CHECK）；SQLite 不支持单条 ALTER 多列 → ③ 只加一列。

-- ---------------------------------------------------------------------------
-- ① 叙事 critic 报告（每个**已提交**的 tick 一行）
-- ---------------------------------------------------------------------------
-- 背景：引擎每回合都会跑一次叙事 critic（`crates/muse-engine/src/narrative/continuity.rs`
-- `narrative_critic`），产出 `characterConsistencyIssues` / `causalIssues` / `revisionSuggestions`
-- 三条结构化列表，随 `RoundOutcome.critic` 一路传回 server —— 然后在 `runtime::commit_tick`
-- 的解构处**被直接丢弃**。模型已经算了、钱已经花了，结果没人留：这是 §4.2 表里最可惜的一格。
--
-- 🔴 **为何单独建表、而不写进 `worlds.narrative_state_json`**（与 0025 世界线贡献账本同一条红线）：
--    `narrative_state_json` 每 tick 被 `build_seed_state` 原样回灌进引擎 `RoundInput.state`，
--    任何写进去的东西都可能被 `role_decide` / 仲裁读到。critic 是**对模型自身产出的评价**，
--    一旦回灌就变成「引擎读自己的评分再决策」，属 §0.1 平权红线禁区（同 `cloud_characters.mileage`
--    与 `world_contributions` 的口径，均由 grep 级测试守护）。本表**引擎永不读取**。
--
-- 🔴 **本表不是读取面**：三列 issue 文本是模型原始输出，**未过 §15 第 2 层敏感词库闸**
--    （闸作用在 `ProjectedEvent` 上，见 `safety::moderate_runtime_projection`）。
--    它只服务运营侧 SLO 统计与人工复盘，**不得**在未过闸的情况下下发到任何玩家可见接口。
--    将来若要接读取面（如后台看板逐条展示 issue 原文），必须先补一层过闸/脱敏。
--
-- 为何「一行一 tick + 计数列 + 全量 JSON」而不是「一行一 issue」：
--   - **denominator 必须可辨**：矛盾率 = 有问题的 tick / **critic 真的跑过的 tick**。只落 issue 行的话，
--     「critic 跑了但一条问题都没有」与「critic 从未落库（本迁移之前的历史 tick）」在库里长得一模一样，
--     分母就永远算不准。本表**每个已提交 tick 恒落一行**（哪怕三列全空），行存在本身即「critic 跑过」。
--   - **统计不解析 JSON**：三个计数列让 `矛盾率 = COUNT(*) WHERE consistency_issue_count > 0 OR
--     causal_issue_count > 0` 成为纯 SQL 聚合，不需要 JSON 函数（双库可移植子集禁 JSONB / json_extract）。
--   - **结构化列表不丢**：`report_json` 保留完整 `CriticReport`（camelCase，与引擎 serde 逐字对齐），
--     需要逐条文本时 serde 直接读回，不做有损压缩。
--
-- 落库时机：与状态 CAS **同一个事务**（`runtime::commit_tick`）。critic 是观测数据、丢了不影响正确性，
-- 但放事务内更简单且与 tick 同成同败 —— CAS 冲突回滚时 critic 行一并消失，不会留下「tick 没提交却有
-- critic 记录」的孤儿行，(world_id, tick_no) 与 `world_ticks` 的 done 行严格一一对应。纯本地 INSERT、
-- 无 IO，成本相对同一 tick 里的 5+ 次模型调用可忽略（同 §15 第 2 层闸放在事务内的理由）。
--
-- 主键 (world_id, tick_no) 与 `world_ticks` 的唯一索引同口径 → 天然幂等（重复提交不可能，CAS 只放行一次），
-- 且最左前缀即 world_id，按世界聚合走主键范围扫描，无需额外索引。
CREATE TABLE world_tick_critic (
  world_id TEXT NOT NULL,
  tick_no BIGINT NOT NULL,
  -- 三个计数列：SLO 聚合的唯一口径（不解析 JSON）。0 = critic 跑过且未发现该类问题。
  consistency_issue_count INTEGER NOT NULL DEFAULT 0,
  causal_issue_count INTEGER NOT NULL DEFAULT 0,
  revision_suggestion_count INTEGER NOT NULL DEFAULT 0,
  -- 完整 CriticReport（camelCase：characterConsistencyIssues / causalIssues / revisionSuggestions）。
  report_json TEXT NOT NULL DEFAULT '{}',
  created_at BIGINT NOT NULL,
  PRIMARY KEY (world_id, tick_no)
);

-- ---------------------------------------------------------------------------
-- ③ 成员退出时刻
-- ---------------------------------------------------------------------------
-- 现状（`0001_init.sql` world_members）只有 `joined_at`，`status` 置 'left'/'retired' 时不记时刻，
-- 于是「退出率」只能拿当前 status 算一个截面，画不出留存曲线、做不了任何时序分析（§4.2「⚠️ 半」那一格）。
--
-- **可空、无默认值、不回填**：历史行一律留 NULL，语义是「退出时刻未知」——**不是**「没退出」。
-- 刻意不回填成 `joined_at` 或 `now()`：那是凭空编造一个从未被观测到的时刻，会让留存曲线看起来
-- 有数据而实际是假的，比缺数据更坏。留存统计须显式排除 `left_at IS NULL AND status <> 'active'` 的行
-- （= 本迁移之前退出的老成员），并在口径里写明这段历史盲区。
--
-- 写入点（应用层，见 `worlds::leave_world`）：`POST /worlds/{id}/leave` 的 UPDATE 带
-- `status='active'` 守卫 → **重复 leave 的第二次 rows_affected=0（404），首次时刻永不被覆盖**（幂等）。
-- 当前全仓没有任何把 status 置 'retired' 的生产路径（仅测试里直接改库）；将来新增时须一并写 left_at。
--
-- 复活（left/retired → active，`worlds::join_world` 的复活分支）会把 `left_at` 置回 NULL：
-- 该分支本来就已经覆盖 `joined_at`（成员纪元重置），若只重置 joined_at 而留着旧 left_at，
-- 会得到 `left_at < joined_at` 的自相矛盾行，任何时序统计都会被它污染。**这不违反「公共事实不可
-- 回滚」**——那条红线约束的是世界的公共叙事事实（world_events / 账本 / 结算），而 world_members
-- 是可变的成员状态行（status / user_id / joined_at 本来就随复活重写）。
--
-- 双库可移植：单列 ADD COLUMN，可空故不带 NOT NULL（NOT NULL 就必须给 DEFAULT，而任何默认值
-- 都是编造数据）。范式见 0027_world_cover.sql 的可空列写法。
ALTER TABLE world_members ADD COLUMN left_at BIGINT;
