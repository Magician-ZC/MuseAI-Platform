-- MuseAI 平台库 0029（客户端辅助栏两块能力）：
--   A. 发布物浏览/收藏计数（`docs/design/client-ui-design.md` §6「发布状态」区块的浏览数/收藏数）
--   B. 房间邀请（同 §6「房间邀请」区块，此前纯空态、无任何后端）
--
-- 双库可移植子集（db.rs 约定）：TEXT id / BIGINT 毫秒 / INTEGER 布尔 / 无方言函数（无 strftime、
-- 无 date_trunc、无 JSONB、无 serial）；分桶用**应用层整除**算好后落 BIGINT，不在 SQL 里算日期。
-- SQLite 不支持单条 ALTER 多列 —— 本迁移全是新建表，不改任何既有表结构（零回填、零锁表）。

-- ---------------------------------------------------------------------------
-- A1. 发布物浏览登记表（去重登记 = 计数源）
-- ---------------------------------------------------------------------------
-- 🔴 防刷（`docs/build/rules-anti-farming.md` 的同一精神：不让重复动作换来重复收益）：
--    计数口径是「窗口内去重的浏览人次」，不是「HTTP 请求次数」。同一 viewer 在同一
--    `window_bucket` 内无论刷多少次，**主键冲突**都会把第二次起丢弃 —— 防刷由数据库唯一性
--    保证，不依赖应用层的读-判-写（那有 TOCTOU，并发刷即可击穿）。
--
-- 🔴 为何是「append-only 登记表」而不是「计数列 UPDATE」：
--    浏览是高频写。若每次浏览 `UPDATE ... SET view_count = view_count + 1 WHERE template_id = ?`，
--    同一热门发布物的所有并发浏览都会争抢**同一行**的行锁（Postgres 下串行化排队、SQLite 下写锁
--    全表串行），热点行是这类计数最典型的故障模式。本表写路径只有 INSERT，且每行的键都带 viewer_id
--    —— **不同用户天然写不同行，同一用户同窗口只写得进一行**，行级争用被结构性消除（无 UPDATE 即
--    无写-写冲突）。代价是读侧要 COUNT 聚合：这是刻意的取舍——写高频（每次浏览）、读低频
--    （创作者自己打开发布物列表时才读），把成本放在低频侧。
--    复合主键 (template_id, viewer_id, window_bucket) 的**最左前缀即 template_id**，
--    `COUNT(*) WHERE template_id = ?` 走主键索引范围扫描，无需额外索引。
--
-- window_bucket：`floor(浏览时刻毫秒 / 去重窗口毫秒)` 的**对齐分桶**（窗口默认 24h，
--    env `MUSE_VIEW_DEDUP_WINDOW_MS` 可调 —— VALIDATION.md §0.2 产品规则参数化）。
--    刻意用对齐分桶而不是滑动窗口：滑动窗口要「查最近一次浏览时间再判断」，即读-判-写，既回到
--    TOCTOU，又要为每个 (template, viewer) 维护一行可变的 last_seen（又是热点行）。对齐分桶的
--    已知代价是桶边界处同一用户可能相邻两次都计数（上限每窗口 1 次），这在计数语义上完全可接受。
CREATE TABLE world_template_views (
  template_id TEXT NOT NULL,
  viewer_id TEXT NOT NULL,
  window_bucket BIGINT NOT NULL,
  first_viewed_at BIGINT NOT NULL,
  PRIMARY KEY (template_id, viewer_id, window_bucket)
);

-- ---------------------------------------------------------------------------
-- A2. 发布物收藏表（收藏 = 一行，取消 = 删一行）
-- ---------------------------------------------------------------------------
-- 幂等天然成立：收藏是 INSERT（唯一冲突即视为已收藏，吞掉冲突而不是报错），取消是 DELETE
-- （删 0 行也算成功）。计数同样由 COUNT(*) 派生，不维护可变计数列 —— 与 A1 同一个理由：
-- 避免热点行，且「谁收藏了」本身就是产品要查的事实（GET /assets/worlds/{id}/favorite）。
-- 创作者不得收藏自己的发布物（应用层拒绝），防止自刷收藏数。
CREATE TABLE world_template_favorites (
  template_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  PRIMARY KEY (template_id, user_id)
);
-- 反向索引：按用户查「我收藏了哪些世界」（前端收藏夹的读侧，主键最左前缀覆盖不到）。
CREATE INDEX idx_wt_fav_user ON world_template_favorites(user_id);

-- ---------------------------------------------------------------------------
-- B. 房间邀请
-- ---------------------------------------------------------------------------
-- 🔴 社交防火墙（总规格 §14【拍板 22】恨隔面具原则）：邀请只在**世界维度 + 角色维度**寻址与展示。
--    `invitee_user_id` 仅供服务端定位收件人，**任何响应体都不下发**；被邀请者由**角色**寻址
--    （invitee_character_id），邀请人也只以其在该世界的**角色面具**示人（inviter_character_id）。
--    手机号/真人身份一律不进本表、不进响应。
--
-- 🔴 防骚扰：本表**刻意没有自由文本字段**（无留言/附言列）。邀请是结构化的「世界 + 角色」信号，
--    不是私信通道 —— 自由文本会立刻变成绕过内容审核的骚扰面，且需要另一套机审成本。
--    其余防骚扰措施在应用层：pending 唯一、declined 即终局（同一邀请人不得再邀同一角色进同一世界）、
--    每人每日发出配额、TTL 过期、被邀请者可随时拒绝。
--
-- 🔴 邀请不是特权通道（VALIDATION.md §0.1 与真红线 §0.4）：本表**不参与入场**。
--    accepted 只是把「引导入口」点亮，真正入场仍必须走既有 `POST /worlds/{id}/join`，
--    同源唯一 / 防自刷 / 星级准入 / 生死契约签署 / 未成年门一条不少。
--    故本表与 world_members 之间**没有任何写入关系**（invitations 模块永不写 world_members）。
--
-- status：pending（待响应）| accepted（已接受，仍需自行 join）| declined（已拒绝，终局）
--         | expired（TTL 过期，惰性判定）。
CREATE TABLE room_invitations (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  inviter_user_id TEXT NOT NULL,
  -- 邀请人在该世界的角色（面具身份）。房主若未投放角色则为空串，展示侧回落「房主」。
  inviter_character_id TEXT NOT NULL DEFAULT '',
  -- 收件人定位用，**永不下发**（§14 匿名）。
  invitee_user_id TEXT NOT NULL,
  invitee_character_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  expires_at BIGINT NOT NULL,
  responded_at BIGINT NOT NULL DEFAULT 0,
  created_at BIGINT NOT NULL
);
-- 收件箱（GET /me/invitations）：按收件人 + 状态。
CREATE INDEX idx_room_inv_invitee ON room_invitations(invitee_user_id, status);
-- 发件箱（GET /worlds/{id}/invitations）+ 每日配额计数：按世界 + 邀请人。
CREATE INDEX idx_room_inv_world_inviter ON room_invitations(world_id, inviter_user_id);
-- pending 唯一 / declined 终局判定：按世界 + 被邀请角色。
--
-- 取舍说明（体例同 worlds::join_world 的防自刷注释）：「同一 (world, inviter, invitee) 至多一条
-- pending」需要带 WHERE 的**部分唯一索引**，不在 SQLite/Postgres 双库可移植子集内（0021 迁移同样
-- 只建普通索引）。故用普通索引 + 应用层判定 + Idempotency-Key 覆盖正常路径；并发窗口下同一对
-- 理论上可多落一条 pending，但邀请无资损、不改任何资产与成员状态，重复邀请至多是一条重复通知，
-- 可事后治理，收益不抵不可移植索引的代价。
CREATE INDEX idx_room_inv_pair ON room_invitations(world_id, invitee_character_id);
