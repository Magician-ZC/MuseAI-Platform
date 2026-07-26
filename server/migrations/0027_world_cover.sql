-- MuseAI 平台库 0027：世界封面行级字段（客户端设计文档 docs/design/client-ui-design.md §6
-- 「所有可见世界封面使用真实位图资产，不使用占位框、Emoji 或 CSS 绘图替代」）。
--
-- 范式完全照抄 0016_character_avatar.sql（角色立绘）：对象键 + 回读 URL + 机审裁决三列。
-- 封面不进任何不可变结构（不改 world_templates 的模板超集，也不进 assembled_json），
-- 作为 worlds 行级可变字段存储——同一模板开出的不同世界实例可以各有各的封面。
--
-- 双库可跑（db.rs 约定）：SQLite 不支持单条 ALTER 多列，故拆为三条独立 ADD COLUMN；
-- 全部 TEXT 可空，无 CHECK 约束（枚举合法性在应用层归一：非 'approved' 一律不下发）。
--
-- 默认值语义（VALIDATION.md §0.1「未验证功能默认关闭」）：
--   三列均可空且无默认值 → **全部历史世界行留 NULL（无封面）**。读取面对 NULL 的处理是
--   「不下发 coverUrl 字段」（不是空串、不是 null 值），前端沿用它已实现的
--   「按 world.id 哈希确定性挑选一张内置真实位图」兜底，行为与本迁移前**完全一致**。
--
-- 🔴 cover_moderation 是三态机审裁决（'approved' | 'pending' | 'rejected'，来自
-- ModerationProvider::check_image），读取面**双过滤**：仅 'approved' 才下发 cover_url。
-- 口径与 cloud_characters.avatar_moderation 逐字一致（红线见 providers/mod.rs check_image
-- 注释「未过审头像绝不外泄——裁决落 avatar_moderation，读取面双过滤」），封面同理：
-- 未过审的封面绝不出现在大厅列表 / 世界详情 / 上传回执里。
--
-- 说明（已知缺口，与头像同源）：人审改判的回写路径（admin_api::audit 的 subject_kind 分支）
-- 目前只覆盖 character / template，尚无 world_cover 分支。故本波次封面机审裁决**不入 audit_queue**
-- （避免制造无法被改判的死队列项），与 avatar 现状一致；接入人审时补 admin 侧回写分支即可，
-- 本迁移的三列形态无需变动。

ALTER TABLE worlds ADD COLUMN cover_object_key TEXT;
ALTER TABLE worlds ADD COLUMN cover_url TEXT;
ALTER TABLE worlds ADD COLUMN cover_moderation TEXT;
