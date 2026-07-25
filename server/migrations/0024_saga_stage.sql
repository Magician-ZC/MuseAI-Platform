-- MuseAI 平台库 0024（R1：Saga 归组字段）。
-- 总规格 §3「Saga 阶段制——官方按剧情提取」：长篇小说提取为**世界系列（Saga）**，
-- 即 saga_id + stage_no 归组的 N 个阶段模板。**阶段 = 现有「世界模板」粒度**（零新概念）——
-- 本迁移只加归组坐标，不改模板语义、不改装配、不改 join。
--
-- 字数多 → 阶段多（横向），不是单局长（纵向）：单局长度由 endgame 策略锁死，
-- 阶段数量由剧情结构（分卷检测 + 运营校准）决定。
--
-- 默认值语义（未验证功能默认关闭，VALIDATION.md §0.1）：
--   saga_id = ''  → 不属于任何世界系列的独立模板（**全部历史行落到这里**，行为零变化）
--   stage_no = 0  → 未编阶段。仅当 saga_id 非空时 stage_no 才有意义（应用层校验 >= 1）
-- 因此本迁移对既有模板、既有世界、既有装配路径完全无影响；Saga 能力经运营录入数据启用。
--
-- 与既有维度的关系（三者正交，见总规格 §11）：
--   saga_id/stage_no = 剧情位置（本迁移）· star_rating = 内容规格档位（0020）· 生死契约 = 风险档（待做）
--
-- 双库可跑（db.rs 约定）：单列 ADD COLUMN + NOT NULL DEFAULT，SQLite/Postgres 通用，
-- SQLite 不支持单条 ALTER 多列，故拆两条。

ALTER TABLE world_templates ADD COLUMN saga_id TEXT NOT NULL DEFAULT '';
ALTER TABLE world_templates ADD COLUMN stage_no INTEGER NOT NULL DEFAULT 0;

-- 阶段列表页按 saga 归组取数（saga_id 筛选 + stage_no 排序）。
CREATE INDEX IF NOT EXISTS idx_world_templates_saga ON world_templates(saga_id, stage_no);
