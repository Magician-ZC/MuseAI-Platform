-- MuseAI 平台库 0035（R2 收尾两项）：
--   ① **世界系列自动扩容**（总规格 `docs/build/spec-world-ecosystem.md` §5「世界系列自动扩容【新增】」）
--      —— series 排队分房层：1 号实例满员自动开 2 号。运营基建，**建房参数复制 + 排队队列**。
--   ② **BE 结局传记**（同规格 §9「世界线崩塌」）
--      —— 崩塌 → ③归零 + ①减半 + ②已锁定保留 + 产出「BE 结局传记」（坏结局也是内容，封卷收藏）。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔 / 无方言特性
-- （无 JSONB、无 serial、无 NOW()、无 CHECK、无 ON CONFLICT、无 partial index、无 strftime/date_trunc）。
-- SQLite 不支持单条 ALTER 多列，本迁移不加列、只建表。范式见 0032 / 0034。

-- ===========================================================================
-- ① 世界系列（自动扩容的登记面）
-- ===========================================================================
--
-- 🔴 **为何要一张显式登记表，而不是「同模板的世界自动成系列」**
-- 「同 template_id 即同系列」看着省事，实则会把**玩家自建房**（`POST /worlds` 走同一批官方模板）
-- 和运营开的官方场混进同一个队列——一个玩家的私密房满员就去开一个官方房，荒唐且不可控。
-- 更要命的是它让扩容变成**隐式**的：打开开关的那一刻，全站每一个满员世界都成了扩容源。
-- 因此本表是「未验证功能默认关闭」（VALIDATION.md §0.1）的**数据侧那一层**：
--   env 开关 `MUSE_WORLD_SERIES_AUTOSCALE` 是全局急停阀（默认关闭）；
--   **本表的登记是逐系列的显式开闸**——没登记的世界（含全部历史世界、全部玩家自建房）永不扩容。
-- 两道闸都开，某个世界才可能长出下一号实例。范式同副本卡（总开关 + 模板产出表未声明即不发卡）。
--
-- 🔴 **建房参数的唯一复制源是 `origin_world_id`（1 号实例）**，不是"上一号实例"。
-- 若以上一号为源，运营中途改过 2 号的某个参数，3 号就继承了这次偏移，4 号再继承——
-- 误差会沿着队列累积（漂移的漂移）。锚死 1 号则任何一号都与 1 号逐字段一致，
-- 队列多长都不漂。由 `worlds::tests` 的「新实例参数与 1 号一致」用例守死。
CREATE TABLE world_series (
  id TEXT PRIMARY KEY,
  -- 系列源头 = 1 号实例（`worlds.id`）。**全部建房参数从这一行复制**，见上。
  origin_world_id TEXT NOT NULL,
  -- 冗余的模板指针（= 1 号实例的 template_id），供运营按模板/Saga 阶段检索系列，不参与扩容判定。
  template_id TEXT NOT NULL,
  -- 🔴 该系列的实例数上限（**含 1 号**）。规格 §0.2「产品规则参数化，禁止写死」：
  -- 运营建系列时逐系列指定；另有全局硬顶 env `MUSE_WORLD_SERIES_MAX_INSTANCES`（默认 10），
  -- 生效上限取二者较小值——两道都可调，任一收紧立即生效。
  -- 上限存在的理由是硬约束而非洁癖：每个 running 实例都进调度器（`schedule_due_ticks`
  -- 的 `WHERE status='running'`）并各自持有日预算，无限扩容会同时压垮调度器与成本盘。
  max_instances BIGINT NOT NULL DEFAULT 1,
  -- active（可继续扩容）/ closed（运营停扩：既有实例照常跑，但不再开新号）。
  -- 这是**逐系列**的急停阀，粒度比 env 总开关细一档（env 是全站，这里是一条队列）。
  status TEXT NOT NULL DEFAULT 'active',
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
-- 一个世界至多作为一个系列的源头（重复登记 = 幂等命中既有系列，不新建）。
CREATE UNIQUE INDEX idx_world_series_origin ON world_series(origin_world_id);
-- 运营按模板检索系列。
CREATE INDEX idx_world_series_template ON world_series(template_id);

-- 系列成员登记：(系列, 号数) → 世界。
--
-- 🔴 **复合主键 (series_id, instance_no) 就是扩容的幂等键**。
-- 「1 号满员」这件事会被并发的多个 join 同时观察到，于是它们会同时想去开 2 号。
-- 幂等不能寄生在应用层的「先查再建」上（那正是 TOCTOU），必须落在数据库约束上：
-- 建 2 号 = 同一事务内 `INSERT worlds` + `INSERT world_series_instances(series, 2, …)`，
-- 撞主键的那些并发者整笔事务回滚（世界一并回滚，不留孤儿房），回头重查队列即可看到赢家开的 2 号。
--
-- 🔴 **本表不含任何成员/资格信息**。扩容只回答"去哪个实例"，绝不回答"能不能进"——
-- 玩家仍须对新实例调 `POST /worlds/{id}/join`，同源唯一 / 防自刷 / 星级准入 / 生死契约签署 /
-- 未成年门一条不少地重跑一遍（源码断言：`worlds::tests` 的「扩容路径零 world_members 写入」）。
CREATE TABLE world_series_instances (
  series_id TEXT NOT NULL,
  -- 号数，1 起（1 = origin_world_id 那一号）。
  instance_no BIGINT NOT NULL,
  world_id TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  PRIMARY KEY (series_id, instance_no)
);
-- 一个世界只能属于一个系列的一个号（join 侧按 world_id 反查所属队列的读路径）。
CREATE UNIQUE INDEX idx_world_series_instances_world ON world_series_instances(world_id);

-- ===========================================================================
-- ② BE 结局传记（世界崩塌后的封卷）
-- ===========================================================================
--
-- 规格 §9：世界线崩塌 → ③归零 + ①减半 + ②已锁定保留 + **产出「BE 结局传记」**
-- （坏结局也是内容，封卷收藏）+ 崩塌责任仲裁公开可溯。**有输、有痛、有纪念、无冤案、无武器化。**
--
-- 🔴 **公共事实不可回滚（§0.3）：传记是只读汇总，不改写任何世界线数据。**
-- 本表**只被 INSERT**，且产出路径（`progression::seal_be_biography_tx`）对
-- `world_events` / `worlds` / `world_ticks` / `world_contributions` / `world_members`
-- 只有 SELECT。由 `progression::tests` 的两道断言守死：
--   (a) 源码级——只读区内不出现任何针对世界线表的 UPDATE/DELETE；
--   (b) 运行时级——封卷前后对上述五张表做全量快照，逐字节相等。
--
-- 🔴 **无冤案：崩塌原因不许模型现编。**
-- `terminal_reason` 取自 `runtime::terminal_reason()` 与 `audit_logs(action='world.ended')`
-- 的既有确定性串；责任文案取自代码内的**固定字典**（`collapse_reason_label`）。
-- 本模块源码级不含任何模型/provider 调用（`red_line_be_biography_is_model_free` 断言）。
-- 「蓄意毁世界者进风控」是既有 `risk_events` 的事，本表不做判定、只做呈现。
--
-- 🔴 **不复制叙事正文。** `summary_json` 里只有**计量与结构**（拍数、事件按类型计数、
-- 里程碑推进量、成员足迹的时刻与贡献分），没有一个字的 public/private projection。
-- 理由是安全的：正文的读取面有受众投影隔离与机审门（`world_events.moderation` / `visibility`），
-- 一旦复制进传记就等于给正文开了第二条不过闸的读路径。正文的唯一事实源仍是 `world_events`。
--
-- 🔴 **不下发真人身份（§14 恨隔面具原则）**：足迹只记 `cloud_character_id` 与角色面具名，
-- 不记 `user_id`——传记是角色的墓志铭，不是真人的花名册。
CREATE TABLE world_biographies (
  -- 🔴 幂等键：一个世界至多一份传记（world_id 即主键）。终局停机本身有
  -- `end_world_tx` 的 `WHERE status='running'` CAS 只结算一次，本主键是第二道防线。
  world_id TEXT PRIMARY KEY,
  -- 传记种类。当前恒为 'be'（崩塌封卷）。留列是为了将来正常终局也要封卷时不必改表；
  -- **正常终局不产出 BE 传记**（有输才有痛，`is_collapse_reason` 说了算）。
  kind TEXT NOT NULL DEFAULT 'be',
  -- 终局原因串（`runtime::terminal_reason()` 词表：key_character_exit / mainline_complete /
  -- time_cap / starved / time_limit）。BE 传记恒为崩塌白名单内的值。
  terminal_reason TEXT NOT NULL,
  -- 选定结局 id（来自 `audit_logs('world.ended').reason` 的 `ending=` 段）；无结局为空串。
  ending_id TEXT NOT NULL DEFAULT '',
  -- 摘要正文（JSON 文本）：世界线摘要 + 崩塌原因 + 参与者足迹。结构见 progression 模块注释。
  summary_json TEXT NOT NULL,
  sealed_at BIGINT NOT NULL
);
-- 「BE 陈列馆」读路径：按种类 + 封卷时刻倒序。最左前缀即 kind，无需再建单列索引。
CREATE INDEX idx_world_biographies_sealed ON world_biographies(kind, sealed_at);
