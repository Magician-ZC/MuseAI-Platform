-- MuseAI 平台库 0002（HA 加固）：tick 原子认领与重试可观测性。
-- 可移植 SQL 子集（sqlite dev/test + postgres prod 均可）。
--
-- 变更：
-- - world_ticks.attempts：认领计数。跨重启限制单个 tick 的总处理次数（C-9 重试上限 + 终态化）。
-- - idx_world_ticks_world_status：(world_id, status) 索引，支撑原子认领 CAS 与调度器 pending/running 扫描（C-1）。

ALTER TABLE world_ticks ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_world_ticks_world_status ON world_ticks(world_id, status);
