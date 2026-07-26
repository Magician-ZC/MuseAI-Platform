-- MuseAI 平台库 0042（R3 收官：**直播场** —— 定档 + 延迟缓冲 + 弹幕）。
-- 总规格 `docs/build/spec-world-ecosystem.md` §2 场次节奏三档：
--   | **直播场** | 密集拍，一晚跑完一阶段 | 官方阶段定档场次、赛事 | 弹幕流 + 实时观战 + 打赏 |
-- 同 §15 运行时内容安全五层漏斗第 **4** 层：
--   | 4 | **直播场延迟 1-2 拍缓冲**（给 2/3 层拦截窗口） | 0 |
-- 以及 `docs/VALIDATION.md` §2 T5：开放范围「50-100 人世界；**直播场 + 弹幕**」，
-- 门槛「**直播场观众→玩家转化 ≥2%**」，预案「审核成本失控 → **直播延迟拍数上调**」。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第一红线：延迟缓冲**不是**把事实押后，而是把**公开投影**押后
-- ═══════════════════════════════════════════════════════════════════════════
-- 本迁移**不新建任何"待播内容"的副本表**，这是刻意的。
--
-- 一拍跑完之后，`world_events` 已经在 `runtime::commit_tick` 的事务里落定了——那就是世界事实，
-- §0.3「公共事实不可回滚」说的正是它。延迟缓冲**一个字节都不碰它**：
--
--   世界内（参赛者 / 成员）  ── `/api/worlds/{id}/events`  ── 按真实节奏，**不延迟**
--   世界外（直播观众）      ── `/api/live/sessions/{id}/feed` ── 落后 N 拍播出
--
-- 「待播内容存在哪」这个问题的答案因此是：**存在它本来就在的地方**（`world_events`），
-- 只是直播播出面多了一条水位线 `tick_no <= 最新已完成拍 - delay_ticks`。
-- 一份内容一处存储，不存在"缓冲区与正本不一致"这种错位的可能。
--
-- 反过来说，若真去建一张 `pending_broadcast_events` 副本表，就会立刻出现两个事实源：
-- 副本写失败时世界演过了而直播永远缺一拍；副本被改写时观众看到的与世界记载的不是同一件事。
-- 那才是真正的"事实错乱"。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第二红线：审核不通过 = **不外发**，不是**回滚**
-- ═══════════════════════════════════════════════════════════════════════════
-- 缓冲窗口内被审核判否的内容，处置路径有两条，**两条都不改写世界事实**：
--
-- ① 自动（既有链路，本迁移零改动）：§15 第 2 层 `safety::moderate_runtime_projection` 在
--    **落库前**打码 + 置 `world_events.moderation='pending'`；第 3 层异步复核（`safety` 模块
--    TODO(§15-L3)）在缓冲窗口内把 `moderation` 收紧。直播播出面与其它读取面同口径，
--    只出 `moderation='approved'`，于是未过审内容根本进不了播出面。
-- ② 人工（本迁移新增 `live_withholds`）：运营在缓冲窗口内把某条**从这一场直播的播出面**撤下。
--    🔴 它写的是**独立的一张撤下表**，不是 `UPDATE world_events`：
--    - 世界事实一个字节不动（`world_events` 的 SELECT-only，红线用例逐字节快照守死）；
--    - 参赛者的既有读取面完全不受影响——他们的角色刚刚经历了这件事，把它从他们眼前抹掉
--      才是真正的事实错乱；
--    - 撤下只作用于**这一场直播**（`session_id` 是唯一键的一半），不外溢到战报/回放/日报。
--    记 `preemptive` 一列如实标注它是**播出前拦下**（缓冲生效，观众从未看见）还是
--    **播出后撤下**（只减少后续可见性，收不回已经看见的——诚实标注，不假装能撤回）。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第三红线：弹幕是 UGC，不是世界事实
-- ═══════════════════════════════════════════════════════════════════════════
-- `live_danmaku` 与 `world_events` 之间**没有外键、没有视图、没有 UNION 读路径**，
-- 弹幕永不进 `world_events`，因而永不进战报 / 回放 / 日报 / `RoundInput`。
-- 观众的一句话不会变成世界里发生过的事，也不会影响任何角色的决策（§0.1 平权红线）。
--
-- 弹幕另有两道结构性约束：
-- - `moderation` 列：过 `safety::moderate_and_queue`（静态 UGC 的唯一入队/记险入口），
--   读取面只出 `approved`（与 `character_annotations` / `worlds.cover_url` 同范式）；
-- - `display_name` 列：服务端派生的**面具**（§14 恨隔面具原则）。表里存 `user_id` 只为限频与
--   风控溯源，**任何响应体都不下发**；观众之间互相看到的只有 `观众xxxx` 这样的场次内代号。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 第四件：`live_viewers` 是 T5 门槛「观众→玩家转化 ≥2%」的**数据源**
-- ═══════════════════════════════════════════════════════════════════════════
-- 门槛写了却没有数据源，就等于测不了——这个仓库有过先例（OOC 申诉率的门槛悬空到 R3 才补上
-- `ooc_appeals`）。直播观看在此前全仓**没有任何埋点**：`world_members` 只记入场的玩家，
-- `world_events` 只记世界内发生的事，观众来过一次不留任何痕迹。
--
-- 于是本表就是那个「真新建件」：观众每次拉播出面记一行足迹（每场每人一行，幂等 upsert）。
-- 🔴 `was_player` 列**在首次观看那一刻冻结**，是整个口径的关键：转化率问的是
-- 「**当时还不是玩家**的观众里，后来有多少入了场」。若在统计时现算，那些看完就入场的人
-- 会因为"现在是玩家了"而被移出分母，分子分母一起缩水，转化率被系统性低估。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 未验证功能默认关闭（§0.1）
-- ═══════════════════════════════════════════════════════════════════════════
-- 整块能力由运行时开关 `MUSE_LIVE_STAGE` 控制，**默认关闭**（登记在 `flags::KNOWN_FLAGS`，
-- 解析链 user > world > global > env > 默认）。本迁移**不插任何种子数据**——建表 ≠ 开闸。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔 / 纯新建不 ALTER；
-- 无 JSONB、无 serial、无 NOW()、无 CHECK、无 partial index、无 strftime/date_trunc。
-- 范式见 0040 / 0037 / 0036。

-- ---------------------------------------------------------------------------
-- ① 场次定档（观众提前知道什么时候看）
-- ---------------------------------------------------------------------------
-- 状态机（单向，无回环）：
--   scheduled ──开播──> live ──收播──> ended
--       └────────取消────────> canceled（终局）
CREATE TABLE live_sessions (
  id TEXT PRIMARY KEY,

  -- 这一场直播播的是哪个世界（`worlds.id`）。一个世界可以有多场（分阶段定档）。
  world_id TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '',

  -- scheduled（已定档未开播）/ live（直播中）/ ended（已收播）/ canceled（已取消）
  status TEXT NOT NULL DEFAULT 'scheduled',

  -- 🔴 **预告公开时刻**：节目单只列出 `announce_at <= now` 的场次。
  -- 定档提前量（`starts_at - announce_at` 的下限）参数化于 `MUSE_LIVE_ANNOUNCE_LEAD_MS`——
  -- "提前多久放出预告"是运营节奏，不是代码常量（§0.2）。
  announce_at BIGINT NOT NULL,
  -- **开播时刻**（定档的核心：观众据此安排时间）。
  starts_at BIGINT NOT NULL,
  -- 预计收播时刻（0 = 未定；只作展示，不参与任何判定）。
  ends_at BIGINT NOT NULL DEFAULT 0,

  -- 🔴 **本场生效的延迟拍数**（§15 第 4 层）。默认取 `MUSE_LIVE_DELAY_TICKS`（默认 2），
  -- 建场时快照进本列，此后**按场可调**——这正是 VALIDATION §2 T5 预案
  -- 「审核成本失控 → **直播延迟拍数上调**」那个运营旋钮的落点。
  -- 上调只影响尚未播出的拍：已播出的水位线由下面的 `published_high_tick` 单调守住。
  delay_ticks BIGINT NOT NULL DEFAULT 0,

  -- 🔴 **单调播出水位线**：本场已经公开播出到第几拍（-1 = 一拍都还没播）。
  --
  -- 存在的唯一理由是**已播出的不许缩回**。若播出边界每次都由
  -- 「最新已完成拍 - delay_ticks」现算，那么运营把 delay_ticks 从 1 调到 5 的那一刻，
  -- 已经在观众屏幕上滚过去的 4 拍会**从播出面消失**——那是对已公开内容的回滚（§0.3）。
  -- 有了这一列，实际播出边界恒为 `max(现算值, published_high_tick)`：
  -- 上调延迟只勒住未来，不追溯过去。要撤回已播出的内容只有一条显式的、带审计的路径
  -- （`live_withholds`，且如实标注 `preemptive=0`）。
  published_high_tick BIGINT NOT NULL DEFAULT -1,

  -- 场次容量（同时可进场观看的观众上限；0 = 不限）。参数化默认值 `MUSE_LIVE_SESSION_CAPACITY`。
  capacity BIGINT NOT NULL DEFAULT 0,

  created_by TEXT NOT NULL DEFAULT '',
  -- 实际开播 / 收播时刻（`started_at` 只作展示；`ended_at` 参与**尾拍放行**判定：
  -- 收播后再等 `MUSE_LIVE_DRAIN_GRACE_MS` 才把缓冲里剩的尾拍放出去，
  -- 否则最后 N 拍会因为世界不再产新拍而被永久卡在缓冲里）。
  started_at BIGINT NOT NULL DEFAULT 0,
  ended_at BIGINT NOT NULL DEFAULT 0,

  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
-- 节目单读路径（按开播时刻升序；`id` 作次级键保证全序——PG 对并列行不保证顺序）。
CREATE INDEX idx_live_sessions_schedule ON live_sessions(announce_at, starts_at, id);
-- 某世界的场次（运营面 / 世界详情页）。
CREATE INDEX idx_live_sessions_world ON live_sessions(world_id, starts_at, id);
-- 按状态筛（直播中 / 已定档）。
CREATE INDEX idx_live_sessions_status ON live_sessions(status, starts_at, id);

-- ---------------------------------------------------------------------------
-- ② 弹幕（观众实时评论；UGC，过审核链 + 限频）
-- ---------------------------------------------------------------------------
CREATE TABLE live_danmaku (
  id TEXT PRIMARY KEY,

  session_id TEXT NOT NULL,
  -- 冗余一列世界 id：风控与运营按世界聚合时免去 JOIN（弹幕是高频表，少一次 JOIN 有意义）。
  world_id TEXT NOT NULL,

  -- 🔴 发言人真人 id。**只用于限频与风控溯源，任何响应体都不下发**（§14 恨隔面具原则）。
  user_id TEXT NOT NULL,
  -- 服务端派生的场次内面具（`观众xxxx`）。同一个人在同一场里稳定、跨场不可关联。
  -- 不记昵称、不记手机号——观众之间不需要认识彼此，给了就再也收不回来。
  display_name TEXT NOT NULL DEFAULT '',

  -- 弹幕正文（长度上限参数化于 `MUSE_LIVE_DANMAKU_MAX_LEN`；命中敏感词库者存打码后的文本）。
  body TEXT NOT NULL DEFAULT '',

  -- 🔴 **锚定的播出拍**（-1 = 发言时尚无内容播出）。
  --
  -- 这是「观众看到的」与「世界内实际发生的」之间那段时间差**不造成事实错乱**的关键一步：
  -- 观众评论的永远是他**当下看见的那一拍**，而不是世界当下跑到的那一拍。由服务端按播出水位线
  -- 计算写入，**不接受客户端传值**——否则观众可以把弹幕锚到尚未播出的拍上，等于替世界剧透。
  -- 回放时按本列对齐，弹幕与画面严丝合缝。
  anchor_tick BIGINT NOT NULL DEFAULT -1,

  -- 机审裁决：approved / pending / rejected。
  -- 🔴 **无论裁决都落库，读取面只出 approved**（人审改判后无需玩家重发；范式同
  -- `character_annotations.moderation` / `cloud_characters.avatar_moderation`）。
  moderation TEXT NOT NULL DEFAULT 'pending',

  created_at BIGINT NOT NULL
);
-- 🔴 弹幕列表读路径 = 复合键 `(session_id, created_at DESC, id DESC)`。
-- 次级键 `id` 不是装饰：弹幕是**同毫秒批量写入的重灾区**（一场直播里几十个人同时发），
-- 单列 `created_at` 游标在并列行横跨页边界时会**永久丢行**（见 `server/src/pagination.rs`）。
CREATE INDEX idx_live_danmaku_feed ON live_danmaku(session_id, created_at, id);
-- 按播出拍取弹幕（回放对齐）。
CREATE INDEX idx_live_danmaku_anchor ON live_danmaku(session_id, anchor_tick, created_at, id);
-- 限频窗口读路径（某人在最近 W 毫秒内发了几条）。
CREATE INDEX idx_live_danmaku_rate ON live_danmaku(user_id, created_at);

-- ---------------------------------------------------------------------------
-- ③ 观众足迹（T5「观众→玩家转化 ≥2%」的唯一数据源）
-- ---------------------------------------------------------------------------
-- 每场每人一行（幂等 upsert）。**只在观众真正拉取播出面时写**——"打开了节目单"不算观看。
CREATE TABLE live_viewers (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  world_id TEXT NOT NULL,
  user_id TEXT NOT NULL,

  -- 🔴 **首次观看那一刻此人是否已经是玩家**（`world_members` 里有没有他的行），0/1。
  -- 冻结在写入时刻，绝不在统计时现算 —— 理由见本文件顶部第四件。
  was_player INTEGER NOT NULL DEFAULT 0,

  first_seen_at BIGINT NOT NULL,
  last_seen_at BIGINT NOT NULL
);
-- 每场每人一行（幂等键）。
CREATE UNIQUE INDEX idx_live_viewers_unique ON live_viewers(session_id, user_id);
-- 转化率窗口扫描（按首次观看时刻切窗，再按人去重）。
CREATE INDEX idx_live_viewers_window ON live_viewers(first_seen_at, user_id);
-- 「这个人是什么时候第一次看直播的」（分子那一步的相关子查询）。
CREATE INDEX idx_live_viewers_user ON live_viewers(user_id, first_seen_at);

-- ---------------------------------------------------------------------------
-- ④ 播出面撤下（缓冲窗口内的人工拦截）
-- ---------------------------------------------------------------------------
-- 🔴 **这张表存在的全部意义，就是让"撤下"不必去 UPDATE `world_events`。**
-- 它只回答一个问题：「这一场直播的播出面要不要跳过这条事件」。
-- 世界事实、参赛者读取面、战报、回放、日报——全部一个字节不动。
CREATE TABLE live_withholds (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  world_id TEXT NOT NULL,
  -- 被撤下的世界事件（`world_events.id`）。**不建外键**：世界事实的生命周期不该被
  -- 一张运营表牵着走，反之亦然（口径同 0036 / 0040 对灰度与社交记录不建外键的理由）。
  event_id TEXT NOT NULL,
  tick_no BIGINT NOT NULL,

  -- 🔴 1 = 播出前拦下（缓冲生效，观众从未看见）；0 = 播出后撤下（只减少后续可见性）。
  -- 如实记录而不是一律当成"拦下了"：延迟缓冲的**有效性**就是靠这一列度量的
  -- （preemptive 占比越低，说明延迟拍数配得越不够）。
  preemptive INTEGER NOT NULL DEFAULT 1,

  reason TEXT NOT NULL DEFAULT '',
  actor_id TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL
);
-- 同一场同一条事件只撤一次（重复调用幂等复用既有那行）。
CREATE UNIQUE INDEX idx_live_withholds_event ON live_withholds(session_id, event_id);
-- 播出面的 NOT EXISTS 子查询 + 运营面按场次列出。
CREATE INDEX idx_live_withholds_session ON live_withholds(session_id, created_at, id);
