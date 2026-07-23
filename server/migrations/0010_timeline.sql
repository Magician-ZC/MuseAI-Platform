-- MuseAI 平台库 0010：异步时间线（离散事件仿真 DES）世界级开关 + 游戏时钟快照（第二块 Phase 2）。
-- 可移植 SQL 子集（sqlite dev/test + postgres prod 均可）；两列均带 NOT NULL DEFAULT，历史世界零回填即安全。
--
-- 变更：
-- - worlds.timeline_mode：世界级渐进闸。'interval'（默认，老世界完全走原路：墙钟固定间隔排 tick → run_round）
--   或 'event'（放置房 DES：背靠背推进 → run_event_step 调度）。**渐进核心闸**——老世界不受任何影响。
-- - worlds.game_time：世界游戏时钟快照（= NarrativeState.timeline.now），commit_tick 每步回写。
--   供调度器/展示读「当前游戏时刻」而不必反序列化整份 narrative_state_json。interval 世界恒为 0（不推进时钟）。

ALTER TABLE worlds ADD COLUMN timeline_mode TEXT NOT NULL DEFAULT 'interval';
ALTER TABLE worlds ADD COLUMN game_time BIGINT NOT NULL DEFAULT 0;
