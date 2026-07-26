-- MuseAI 平台库 0045：**处置申诉**——被下架的作者对处置本身提异议的入口。
--
-- ## 背景缺口
--
-- 0044 给了平台对已发布内容的处置能力，作者侧却只多了一条**告知**
-- （`GET /assets/characters/{id}/status` 的 `takedown` 字段：状态 + 时间 + 固定文案）。
-- 作者要提异议，只能走客服工单——一条没有状态机、没有时限、不进任何审计口径的通道。
-- 对一项可以让内容立即消失的权力来说，「有处置无救济」是不完整的。
--
-- ## 🔴 为什么新建表，而不是扩 `moderation_appeals`（0018）
--
-- 三条理由，任何一条单独成立都够：
--
-- ① **键的形状不同，且改 0018 会静默毁掉它的核心保证。**
--    0018 是「每**主体**终身一次」：`UNIQUE(subject_kind, subject_id)`。
--    处置不是主体的一种状态，而是**一次事件**——同一张卡可以被下架、恢复、再被下架，
--    每一次都是独立的处置，作者对每一次都该有独立的申诉权。要把处置申诉塞进 0018，
--    唯一索引必须扩成 `(subject_kind, subject_id, takedown_ref)`，而 0018 的存量行没有
--    `takedown_ref` 只能留 NULL —— **SQLite 与 Postgres 的唯一索引都不认为两个 NULL 相等**，
--    于是「发布期申诉终身一次」这条保证会在扩索引的那一刻**无声失效**：同一张卡可以被反复申诉，
--    没有任何报错。为了给新功能腾地方而拆掉旧功能的不变式，代价出在看不见的地方。
--
-- ② **改判动作不同，混用会重新捅开 0044 刚堵上的洞。**
--    0018 的 `overturn`（`admin_api::audit::resolve_appeal`）直接
--    `UPDATE ... SET moderation = 'approved'`。若处置申诉走同一条路，那正是 0044 在
--    `audit::review` 上加守卫要防的事：**绕过 `restore` 的可逆性台阶与权限台阶**把下架撤销，
--    还会留下一条自称仍在下架的 `content_takedowns` 记录。处置申诉的改判必须写回
--    `prev_moderation`（回到下架前，不是顺手放行）并把台账翻成 `restored` ——
--    与 `restore` **共用同一段事务实现**，不另开第二条恢复路径。
--
-- ③ **受理条件相反。** 0018 只受理 `rejected`（发布时被驳回）；本表只受理
--    `restricted` / `removed`（过审后被处置）。两张表各自的前置校验都因此是单条件的、
--    读得懂的；合表则每处都要先分流再校验。
--
-- ## 每处置事件一次
--
-- `content_takedowns` 每主体只留**当前**一行，且重复下架走 ON CONFLICT **覆盖**（`id` 不变、
-- `created_at` 刷新）。所以「哪一次处置」的标识是 `(takedown_id, created_at)` 这一对，
-- 唯一索引因此建在 `(takedown_id, disposal_at)` 上：同一次处置只能申诉一次；
-- 恢复之后又被重新下架 → `disposal_at` 变了 → 作者重新获得申诉权。
-- 两列都 NOT NULL，不落进上面 ① 说的 NULL 陷阱。
--
-- ## 🔴 运营内部理由不进作者侧
--
-- `content_takedowns.reason` 是运营内部处置备注（口径同 `audit_logs.reason`），**本表不复制它、
-- 作者侧端点也不 JOIN 它**。本表的 `resolution_reason` 是复审人**写给作者**的答复
-- （口径同 0018 的同名列，本就是为回显而设），两者不是一回事，不得互相顶替。
-- 红线用例：`admin_api::takedown::tests::red_line_author_facing_appeal_never_echoes_internal_reason`。
--
-- 双库可移植（db.rs 约定）：id/外键 TEXT；时间 BIGINT 毫秒；无方言特性；
-- 无 CHECK 约束（枚举合法性在应用层白名单归一）。

CREATE TABLE disposal_appeals (
  id TEXT PRIMARY KEY,
  -- 被申诉的那次处置：`content_takedowns.id` + 该次处置的 `created_at`（见上「每处置事件一次」）。
  takedown_id TEXT NOT NULL,
  disposal_at BIGINT NOT NULL,
  -- 处置主体（character / character_avatar；owner 可判定的两类，见 assets 侧端点注释）。
  subject_kind TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  -- 申诉人 = 主体 owner（提交时校验，非 owner 一律 404 不泄露存在性）。
  owner_id TEXT NOT NULL,
  -- 申诉正文（trim 后 1..=500 字符，口径同 0018）。
  appeal_text TEXT NOT NULL,
  -- 提交时的处置态快照（restricted / removed）。留档用：裁决时处置态可能已变。
  disposal_state TEXT NOT NULL,
  -- pending / upheld（维持处置）/ overturned（改判，内容恢复）
  status TEXT NOT NULL DEFAULT 'pending',
  -- 🔴 复审人写给**作者**的答复（会回显）。**不是** content_takedowns.reason（运营内部备注，不回显）。
  resolution_reason TEXT,
  reviewer_id TEXT,
  created_at BIGINT NOT NULL,
  resolved_at BIGINT
);

-- 每次处置只受理一次申诉（重复提交由唯一索引冲突 → 409）。
CREATE UNIQUE INDEX idx_disposal_appeals_event ON disposal_appeals(takedown_id, disposal_at);
-- 后台队列按 status 筛 + (created_at DESC, id DESC) 全序翻页。
CREATE INDEX idx_disposal_appeals_status ON disposal_appeals(status, created_at);
-- 作者侧回显：按主体取最新一条。
CREATE INDEX idx_disposal_appeals_subject ON disposal_appeals(subject_kind, subject_id, created_at);
