-- MuseAI 平台库 0034（R2：传世卡最小版 —— 封卷 + 遗作馆）。
-- 总规格 `docs/build/spec-world-ecosystem.md` §12【拍板 23】「死亡与传承——传记封卷制」。
--
-- **死亡 = 传记封卷，不是资产清零。**（「输」的张力与资产安全感的三角闭合：
-- 会死【张力】、死得其所【仲裁合理】、死有所归【传世卡】。）
--   - 卡死后转为「传世卡」：**只读、入遗作馆陈列、不可再入世界**；
--   - 道具归账户背包（道具本为账户资产）；羁绊对方角色获得「故人」印记；
--   - **内核可复制，履历不可复制**：同内核开新卡 = 转世（双胞胎），不是复活。
--
-- ---------------------------------------------------------------------------
-- 🔴 为何封卷同时写 `withdrawn = 1`（而不是只加一个新状态列）
-- ---------------------------------------------------------------------------
-- 「不可再入世界」的**唯一有效拦截点是 `worlds::join_world`**。它的资格查询是一条列名写死的
-- SELECT（`owner_id, moderation, withdrawn, mileage, source_fingerprint, pristine`）——
-- 新加的 `memorial_status` 列**不会**被它读到，只加新列等于没拦住，传世卡照样能投放。
-- 因此封卷是**一次原子的双写**：
--   ① `memorial_status = 'sealed'` —— 语义状态（遗作馆读它、CAS 幂等靠它）；
--   ② `withdrawn = 1`             —— 复用 join 已有的那道门（`withdrawn != 0` → `character_withdrawn`）。
-- 语义也恰好吻合：`withdrawn` 的既有含义就是「停止后续投放」，与「不可再入世界」逐字同义。
-- 安全性前提（已 grep 全仓核实、并由 `memorial::tests` 源码级断言守死）：
-- 全仓**不存在任何把 `cloud_characters.withdrawn` 置回 0 的 SQL**——withdrawn 是单向门，
-- 没有「取消下架」端点，故不存在「撤回封卷 = 复活」的侧门。
-- 反面选项 `moderation` 被否决：那是机审/人审裁决列，`admin_api::audit` 有把它改回 'approved'
-- 的合法路径，拿它当死亡门等于给复活开了一扇后台侧门。
--
-- ---------------------------------------------------------------------------
-- 🔴 公共事实不可回滚（§0.3）：封卷**不改写世界线**
-- ---------------------------------------------------------------------------
-- 本迁移不新增、不修改、不删除任何 `world_events` / `worlds.narrative_state_json` /
-- `consent_requests` 的语义。死亡是已落定的公共事实，封卷只改**卡自己的状态**：
-- 死者仍留在 `world_members`（足迹是履历的一部分，绝不删行），事件流一个字节不动。
--
-- ---------------------------------------------------------------------------
-- 🔴 无隐藏数值（§12 原文）：传世卡的价值全是显性资产
-- ---------------------------------------------------------------------------
-- 本迁移**不引入任何数值列**：没有加成、没有系数、没有分数。传世卡的价值 = 累计人生
-- （历练 `mileage` / 传记 / 足迹 `world_members` / 羁绊 `memorial_marks`），全部是已存在的显性资产。
-- `memorial_marks` 也只有「谁记得谁、在哪个世界、什么时候」，没有任何强度字段
-- ——「故人」是**叙事印记**，不是 buff。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / 无方言特性（无 JSONB、无 serial、
-- 无 NOW()、无 CHECK、无 ON CONFLICT、无 partial index）。SQLite 不支持单条 ALTER 多列，
-- 故逐列拆开。范式见 0021 / 0026 / 0032。

-- 封卷状态：living（在世，**全部历史行的默认值** → 行为零变化）/ sealed（传世卡）。
-- 状态机是**单向**的：living → sealed，没有反向转换（复活违反 §12「不是复活」与 §0.3 公共事实不可回滚）。
-- 幂等闸即建在本列上：封卷是 `WHERE memorial_status = 'living'` 的条件 UPDATE（CAS），
-- 抢到才发道具、才打印记，重复封卷命中 0 行直接短路——不重复发道具、不重复打印记。
ALTER TABLE cloud_characters ADD COLUMN memorial_status TEXT NOT NULL DEFAULT 'living';

-- 封卷时刻（毫秒）。NULL = 在世。遗作馆按它倒序陈列。
ALTER TABLE cloud_characters ADD COLUMN memorial_sealed_at BIGINT;

-- 死于哪个世界（`worlds.id`）。传记的落款，遗作馆详情展示用；NULL = 在世或世界不可考。
-- **只是指针**：不复制世界线内容，公共事实仍以 world_events 为唯一事实源。
ALTER TABLE cloud_characters ADD COLUMN memorial_world_id TEXT;

-- 遗作馆陈列的唯一读路径：状态 → 封卷时刻倒序。最左前缀即 memorial_status，无需再建单列索引。
CREATE INDEX idx_cloud_characters_memorial ON cloud_characters(memorial_status, memorial_sealed_at);

-- ---------------------------------------------------------------------------
-- 「故人」印记（§12「羁绊对方角色获得『故人』印记——你的死成为别人故事的一部分」）
-- ---------------------------------------------------------------------------
-- 🔴 **为何必须独立建表，绝不能写进 `worlds.narrative_state_json`**：
-- 那一列每 tick 经 `runtime::build_seed_state` **原样回灌进引擎 `RoundInput.state`**。
-- 任何写进去的东西都成了引擎决策的输入——即便「故人」本身不带数值，把结算侧/资产侧的记账
-- 喂回决策也是 §0.1 平权红线的禁区（口径与 0025 贡献账本、0030 critic 报告逐字一致）。
-- 独立表 ⇒ 引擎侧根本没有读取路径，物理隔离比应用层「记得过滤」可靠得多
-- （由 `memorial::tests::red_line_marks_never_enter_engine_decision` 源码级断言守死）。
--
-- 🔴 **印记不是 buff**：本表没有任何强度/等级/加成列，也永远不会有。它是一条叙事事实
-- （「你的角色记得那个人」），只进读取面与陈列，不进任何判定。
--
-- 🔴 **只读的传世卡不再接收印记**：`character_id` 只会是在世卡（`memorial_status='living'`）。
-- 给已封卷的卡加印记等于改写只读卡，与「传世卡只读」直接冲突（应用层保证，见 memorial/mod.rs）。
CREATE TABLE memorial_marks (
  id TEXT PRIMARY KEY,
  -- 获得印记的**在世**角色（`cloud_characters.id`）。
  character_id TEXT NOT NULL,
  -- 该角色的主人（读取面 `/me/memorial/marks` 按人过滤，省一次 JOIN）。
  owner_id TEXT NOT NULL,
  -- 逝者（`cloud_characters.id`，已封卷的传世卡）。
  deceased_character_id TEXT NOT NULL,
  -- 羁绊发生在哪个世界（`worlds.id`）。印记的叙事落款。
  world_id TEXT NOT NULL,
  -- 印记种类。当前恒为 'departed'（故人）；留列是为了将来「同袍/宿敌」等叙事印记扩展时不必再改表。
  kind TEXT NOT NULL DEFAULT 'departed',
  granted_at BIGINT NOT NULL
);

-- 🔴 印记幂等闸：同一对「生者 ← 逝者」至多一条。封卷的 CAS 是第一道幂等，本唯一键是第二道
-- （即便 CAS 被绕过——例如同一逝者在两条并发路径上封卷——也不可能重复打印记）。
-- 不含 world_id：跨世界重逢同一逝者仍只算一次「故人」，否则同一段羁绊会被计重。
CREATE UNIQUE INDEX idx_memorial_marks_unique ON memorial_marks(character_id, deceased_character_id);
-- 「我的角色都记得谁」读路径。
CREATE INDEX idx_memorial_marks_owner ON memorial_marks(owner_id, granted_at);
-- 「谁还记得这位逝者」读路径（遗作馆详情的 remembrance 段）。
CREATE INDEX idx_memorial_marks_deceased ON memorial_marks(deceased_character_id);
