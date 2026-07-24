-- MuseAI 平台库 0012（P6 观战直播 + 回放，feature=arena）：礼物来源列。
-- 可移植 SQL 子集（sqlite dev/test + postgres prod 均可）；NOT NULL DEFAULT，历史行零回填即安全（同 0010 风格）。
--
-- 变更：
-- - gift_events.via：区分打赏来源。'livegate'（外部直播平台 webhook 回调）或 'in_app'（站内观众打赏按钮）。
--   供主播分成/审计区分渠道。老账默认 'livegate'（原 webhook 唯一来源）。
--
-- 回放端点（GET /arena/{id}/replay）按 world_events(world_id, sequence) 升序 seek 分页拉取——
-- 既有 idx_world_events_world(world_id, sequence)（0001_init.sql:167）已完全覆盖，无需新增索引。

ALTER TABLE gift_events ADD COLUMN via TEXT NOT NULL DEFAULT 'livegate';
