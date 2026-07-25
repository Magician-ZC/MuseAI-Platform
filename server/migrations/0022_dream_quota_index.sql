-- MuseAI 平台库 0022（R1：托梦配额——每卡每阶段计数的支撑索引）。
-- 配额维度由「(world_id, user_id, 时间窗) 且 status='accepted'」改为
-- 「(world_id, character_id) 全量累计，kind='whisper' 且 status IN ('accepted','applied')」
-- （规格 §8【拍板 12】"每卡每阶段 N 条"；条数运营可调，见 interventions::dream_quota_per_stage）。
-- 现有 idx_interventions_world(world_id, status) 不含 character_id，撑不住新计数 SQL 的选择性，
-- 故补一条 (world_id, character_id, status) 复合索引；原索引保留（runtime 按 world+status 取 accepted 仍走它）。
-- 双库可跑（db.rs 约定）：仅 CREATE INDEX，无 partial index / INCLUDE / 表达式索引等方言特性。
CREATE INDEX idx_interventions_char ON interventions(world_id, character_id, status);
