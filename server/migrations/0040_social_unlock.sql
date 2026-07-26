-- MuseAI 平台库 0040（R3：真人社交解锁 —— 恨隔面具原则的**唯一**解锁面 + 拉黑/举报治理）。
-- 总规格 `docs/build/spec-world-ecosystem.md` §14【拍板 22】「社交：恨隔面具原则」：
--   - 默认全员**角色面具**（匿名，以角色身份互动）；
--   - **仅正向羁绊线**（共历生死/结盟/救命）达阈值后**双向自愿**解锁真人身份；
--   - **敌对线永久匿名**——背叛与仇杀不暴露玩家真身，冲突外溢的网暴通道结构性焊死；
--   - 配套：拉黑 / 举报 / 青少年模式限真人社交。独有社交资产：「我们的角色一起死过」。
--
-- ---------------------------------------------------------------------------
-- 🔴 三张表分别承担什么，以及**它们合起来不承担什么**
-- ---------------------------------------------------------------------------
-- ① `social_unlock_requests` —— 双向自愿的**状态机**。它是「真人身份可见」这件事的唯一事实源：
--    没有一行 accepted 记录，服务端就不会向任何人下发任何人的真人身份字段。
-- ② `social_blocks`          —— 拉黑。**按 user 维度判定**（按角色判定会被"换张卡继续骚扰"绕过），
--    但**按面具录入**（拉黑时你看到的只有角色 id，真人 id 由服务端内部解析，永不下发）。
-- ③ `social_reports`         —— 举报队列。运营可处理（pending → actioned/dismissed），
--    累计到阈值另写一条 `risk_events` 升级（复用既有风控面，不另造看板）。
--
-- 🔴 **三张表都没有任何数值列**，将来也不会有。
--    「我们的角色一起死过」是**关系凭证**，不是资产：它由既有的 `cloud_characters.memorial_*`
--    与 `world_members` **只读派生**，不落库、不计分、不发道具、不进历练/卡位/背包/结算。
--    「不落库」是刻意的——只要它没有自己的存储，它就不可能被误当成可累积的进度。
--    （由 `social::tests::red_line_social_asset_has_zero_numeric_effect` 与
--      `red_line_module_writes_only_social_tables` 两条用例守死。）
--
-- 🔴 **不建外键**（与 0036 `runtime_flags` 同口径）：用户/角色/世界的删除不得级联删掉
--    拉黑与举报——那会静默放开一条已被用户关上的门，且事后无从复盘。悬挂 id 由应用层
--    的存在性校验挡掉，读取面对不存在的对端一律降级为中性展示。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔 / 无方言特性
-- （无 JSONB、无 serial、无 NOW()、无 CHECK、无 ON CONFLICT、无 partial index）。
-- SQLite 不支持单条 ALTER 多列——本迁移不改任何既有表，纯新建。

-- ---------------------------------------------------------------------------
-- ① 真人身份解锁请求（双向自愿的状态机）
-- ---------------------------------------------------------------------------
-- 状态机（单向，无回环）：
--   pending ──accept──> accepted ──对方拉黑/我拉黑──> revoked
--      │                    ▲
--      ├──decline──> declined（**终局**，见下）
--      └──TTL 到期──> expired（**终局**）
--
-- 🔴 `declined` / `expired` / `revoked` 全是**终局**：唯一索引
--    `(world_id, requester_character_id, target_character_id)` 使同一条线永远只有一行。
--    这不是省事，是防骚扰：真人身份是全平台最敏感的一次授予，「被拒后可以再问一次」
--    会把它变成可以反复施压的通道。想换个方式接触，只剩下"在世界里把关系演到对方愿意主动开口"
--    这一条路——这正是 §14 想要的社交形状。
--
-- 🔴 `eligibility_json` 是**发起那一刻的资格快照**，只作审计（回答「当时凭什么够格」）。
--    它**不参与任何判定**：接受时服务端会用当下的数据**重新算一遍**资格（世界线会继续跑，
--    昨天的正向羁绊今天可能已经翻脸成敌对线）。存快照而按现值判定，是「公共事实不可回滚 +
--    敌对线永久匿名」两条同时成立的唯一方式。
CREATE TABLE social_unlock_requests (
  id TEXT PRIMARY KEY,
  -- 这段羁绊长在哪个世界（`worlds.id`）。解锁按世界发起：跨世界的同一对玩家是两段独立关系。
  world_id TEXT NOT NULL,
  requester_user_id TEXT NOT NULL,
  -- 发起人的面具（`cloud_characters.id`）。对外展示只用它，绝不用 user_id。
  requester_character_id TEXT NOT NULL,
  -- 收件人真人 id：服务端内部定位收件箱用，**任何响应体都不下发**（§14）。
  target_user_id TEXT NOT NULL,
  target_character_id TEXT NOT NULL,
  -- pending / accepted / declined / expired / revoked
  status TEXT NOT NULL DEFAULT 'pending',
  -- 发起那一刻的资格快照（只读审计，不参与判定）。
  eligibility_json TEXT NOT NULL DEFAULT '{}',
  expires_at BIGINT NOT NULL,
  responded_at BIGINT NOT NULL DEFAULT 0,
  revoked_at BIGINT NOT NULL DEFAULT 0,
  created_at BIGINT NOT NULL
);
-- 🔴 一条线一行（含终局态）：拒绝/过期/撤销之后不得再发起。
CREATE UNIQUE INDEX idx_social_unlock_pair
  ON social_unlock_requests(world_id, requester_character_id, target_character_id);
-- 收件箱读路径。
CREATE INDEX idx_social_unlock_target ON social_unlock_requests(target_user_id, status);
-- 发件箱 + 日配额窗口读路径。
CREATE INDEX idx_social_unlock_requester ON social_unlock_requests(requester_user_id, created_at);

-- ---------------------------------------------------------------------------
-- ② 拉黑（保护态）
-- ---------------------------------------------------------------------------
-- 🔴 **判定按 user，录入按面具**：
--    - 判定按 `blocker_user_id`/`blocked_user_id` 两个真人 id —— 按角色判定会被
--      「换一张卡继续找同一个人」整个绕过，那样的拉黑等于没有；
--    - 录入时用户只提供 `blocked_character_id`（他唯一看得见的东西），真人 id 由服务端解析。
--      `blocked_character_id` / `world_id` 只留作展示与溯源（「我当时拉黑的是哪张面具」）。
--
-- 🔴 **拉黑是保护态，不随功能开关消失**：即便 `MUSE_SOCIAL_IDENTITY_UNLOCK` 被关掉，
--    既有拉黑记录在其它社交通道（如房间邀请）上**仍然生效**。方向与 `MUSE_SAFETY_LEXICON`
--    的 fail-safe 一致——「安全」永远指向不扩大可达范围的那一侧，不是字面的关。
--
-- 拉黑**不是**举报：它只改变"谁能找到我"，不产生任何对被拉黑者的处置，故不入运营队列、
-- 不写 risk_events、不通知对方（通知对方等于把拉黑变成一次挑衅）。
CREATE TABLE social_blocks (
  id TEXT PRIMARY KEY,
  blocker_user_id TEXT NOT NULL,
  blocked_user_id TEXT NOT NULL,
  -- 拉黑时看到的那张面具（`cloud_characters.id`）。展示与溯源用，不参与判定。
  blocked_character_id TEXT NOT NULL DEFAULT '',
  world_id TEXT NOT NULL DEFAULT '',
  reason TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL
);
-- 同一对（拉黑人 → 被拉黑人）至多一条：重复拉黑幂等复用同一行。
CREATE UNIQUE INDEX idx_social_blocks_pair ON social_blocks(blocker_user_id, blocked_user_id);
-- 反向读路径（「我是不是被某人拉黑了」——发起社交动作前的前门校验走它）。
CREATE INDEX idx_social_blocks_blocked ON social_blocks(blocked_user_id);

-- ---------------------------------------------------------------------------
-- ③ 举报队列（可运营）
-- ---------------------------------------------------------------------------
-- 与既有 `moderation_appeals`（内容风控申诉）分表，因为主体与流向都不同：
--   - `moderation_appeals` 是**被处置者**对机审/人审结果提异议（申诉，往回走）；
--   - `social_reports`     是**第三方**对某个面具/某次解锁请求发起投诉（举报，往前走）。
-- 合表会让「谁在告谁」这件事在同一列里有两种相反的含义，看板必然算反。
--
-- `subject_user_id` 是服务端内部解析出的被举报真人 id：运营需要它做累计与处置，
-- **玩家侧任何响应体都不下发**（举报不是探测对方真身的接口，§14）。
CREATE TABLE social_reports (
  id TEXT PRIMARY KEY,
  reporter_user_id TEXT NOT NULL,
  -- character（举报一张面具）/ unlock_request（举报一次解锁请求本身，含其附言）
  subject_kind TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  -- 服务端内部解析的被举报人。玩家侧永不下发。
  subject_user_id TEXT NOT NULL,
  world_id TEXT NOT NULL DEFAULT '',
  -- 举报类别（白名单在 `social::REPORT_CATEGORIES`，不落 DB CHECK：双库可移植子集禁 CHECK，
  -- 且类别会随真实举报数据演进，属 §0.2 参数化范畴）。
  category TEXT NOT NULL,
  detail TEXT NOT NULL DEFAULT '',
  -- pending（待处理）/ actioned（已处置）/ dismissed（不予支持）
  status TEXT NOT NULL DEFAULT 'pending',
  handled_by TEXT NOT NULL DEFAULT '',
  resolution TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL,
  resolved_at BIGINT NOT NULL DEFAULT 0
);
-- 运营队列读路径（按状态 + 时间）。
CREATE INDEX idx_social_reports_status ON social_reports(status, created_at);
-- 累计升级读路径（同一被举报人的 pending 数达阈值 → 写 risk_events）。
CREATE INDEX idx_social_reports_subject_user ON social_reports(subject_user_id, status);
-- 冷却窗口去重读路径（同一举报人对同一对象在窗口内只受理一次，幂等复用既有那条）。
-- 刻意**不建唯一索引**：唯一即"终身只能举报一次"，会让再犯无法被举报。
CREATE INDEX idx_social_reports_reporter
  ON social_reports(reporter_user_id, subject_kind, subject_id, created_at);
