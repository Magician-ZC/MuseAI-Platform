-- 0017 世界发现（搜索 + 热门排序）：world_events.occurred_at 补索引。
-- 热度分需按时间窗（近 48h）聚合 world_events；0001 只有 (world_id, sequence) 索引，
-- occurred_at 无索引会全表扫。gift_events 已有 (world_id, created_at) 索引（0008），无需补。
-- 可移植 SQL 子集（禁方言/日期函数），IF NOT EXISTS 双库（SQLite/PG）皆支持。
CREATE INDEX IF NOT EXISTS idx_world_events_occurred ON world_events(occurred_at);
