-- MuseAI 平台库 0044：已过审内容的事后处置（再审 / 下架 / 恢复）台账。
--
-- 背景缺口：`audit_queue` 的 approve/reject 只作用于**仍在人审队列里**的条目。
-- 角色卡、立绘、世界封面、世界模板一旦过审就再也拿不下来 —— 而举报队列（0040）处理的
-- 恰恰是「已经在线上的内容出了问题」。合规主体责任要求平台对**已发布**内容具备处置能力。
--
-- ## 🔴 下架作用在「展示面」，不是「已发生的世界事实」（§0.3 公共事实不可回滚）
--
-- 本迁移**不新增任何世界线列，也不改动任何世界线表**。处置的落点是四张既有的
-- **展示态列**（各自都已有「非 'approved' 一律不下发」的读取面闸门）：
--
--   | 处置主体 kind      | 表               | 展示态列            | 既有闸门 |
--   |--------------------|------------------|---------------------|----------|
--   | character          | cloud_characters | moderation          | worlds::join_world「非 approved → 409 character_not_approved」；invitations 接受邀请同判 |
--   | character_avatar   | cloud_characters | avatar_moderation   | CharacterView / world roster / backpack 三处读取面「仅 approved 才下发 avatarUrl」 |
--   | world_cover        | worlds           | cover_moderation    | worlds::visible_cover_url（大厅 / 世界详情 / 后台世界列表共用的**唯一**闸门） |
--   | world_template     | world_templates  | moderation          | worlds::create_room「非 approved → 409 template_not_approved」；assembly 蓝图解引用同判 |
--
-- 下架 = 把该列置为 'takedown'（一个**非 approved 值**）。选择「写既有列」而不是「加一列新过滤条件」
-- 是刻意的**失效安全**设计：上面四个闸门判的都是「等于 approved」，因此任何非 approved 值都会
-- 自动关闭它们 —— 不需要逐个读取面补 WHERE 条件，也就不存在「漏改一处 = 下架无效」的风险面。
--
-- 相应地，`world_events` / `world_ticks` / `world_members` / `world_contributions` /
-- `world_biographies` 这些**已落定的事实表一个字节都不动**。一张卡被下架不意味着它参演过的
-- 世界事实要被抹掉。红线由用例逐字节快照守死（admin_api::takedown::tests）。
--
-- ## 可逆性：两档，权限台阶不同
--
--   state='restricted' —— **可恢复**下架。展示态置 'takedown'，`prev_moderation` 记住下架前的值；
--                         恢复即把它写回去。reviewer 可下、可恢复（误操作可自愈）。
--   state='removed'    —— **不可恢复**的永久移除，供「被要求永久移除」的合规场景。恢复端点恒 409。
--                         **admin 专属**（不可逆动作要更高的门槛）；位图主体（立绘 / 封面）
--                         另删除对象存储字节（`bytes_purged=1`）——被要求删除时只改标志位不算删除。
--                         文本主体（card_json / skeleton_json）**不删字节**：运行中的世界仍引用
--                         那份不可变快照，删了会让运行中的世界崩掉。
--   state='restored'   —— 已恢复，历史留档（不删行：这张卡「曾经被下架过」本身是处置记录的一部分）。
--
-- 每主体只保留**当前**一行（唯一索引 + ON CONFLICT 覆盖）；完整的处置历史在 `audit_logs`
-- （action='content.takedown' / 'content.takedown_permanent' / 'content.restore' / 'content.recheck'）
-- 与 `risk_events`（kind='content_disposal'）里，两处都由既有入口写入，本表不是唯一事实源。
--
-- 双库可移植（db.rs 约定）：id/外键 TEXT；时间 BIGINT 毫秒；布尔 INTEGER 0/1；
-- 无方言特性（无 JSONB / serial / NOW()）；无 CHECK 约束（枚举合法性在应用层白名单归一）。

CREATE TABLE content_takedowns (
  id TEXT PRIMARY KEY,
  -- character / character_avatar / world_cover / world_template（应用层白名单，见 takedown.rs SUBJECTS）
  subject_kind TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  -- restricted（可恢复）/ removed（永久移除，不可恢复）/ restored（已恢复，历史留档）
  state TEXT NOT NULL,
  -- 下架前的展示态。恢复时写回它 —— 恢复的唯一依据，不靠「恢复即 approved」这种猜测。
  prev_moderation TEXT NOT NULL,
  -- 运营内部处置理由（必填 1..=500 字）。🔴 **不回显给作者**：口径同 audit_logs.reason。
  -- 面向作者的告知走 GET /assets/characters/{id}/status 的 takedown 字段（只给状态与时间，不给内部备注）。
  reason TEXT NOT NULL,
  actor_id TEXT NOT NULL,
  actor_role TEXT NOT NULL,
  -- 位图主体永久移除时是否已删除对象存储字节（0/1）。文本主体恒 0（见上文「不删字节」）。
  bytes_purged INTEGER NOT NULL DEFAULT 0,
  created_at BIGINT NOT NULL,
  restored_at BIGINT,
  restored_by TEXT,
  restore_reason TEXT
);

-- 每主体当前只有一条处置行（重复下架走 ON CONFLICT 覆盖，不堆积）。
CREATE UNIQUE INDEX idx_content_takedowns_subject ON content_takedowns(subject_kind, subject_id);
-- 台账列表按 (state) 筛 + (created_at DESC, id DESC) 全序翻页。
CREATE INDEX idx_content_takedowns_state ON content_takedowns(state, created_at);
