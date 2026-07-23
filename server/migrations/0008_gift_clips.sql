-- 0008 P6b（feature=arena）：礼物网关账本 + SKU→boon 映射 + 高光切片任务。
-- 跨 agent 契约：arena_env_events 由 P6a 的 0007 建（本迁移不建该表，只写入其行）。
-- 可移植 SQL 子集（TEXT id / BIGINT 毫秒 / TEXT JSON / INTEGER 布尔），禁方言特性。

-- 礼物 SKU → 场内增益映射。
-- 红线（§2.5）：只映射"过程"增益（环境/道具，advantage/reroll/info 有限枚举，§9.5 叙事数值化表），
-- 绝不含免死或最终判定修改（买过程不买结果）。
CREATE TABLE gift_sku_map (
  sku TEXT PRIMARY KEY,
  boon_json TEXT NOT NULL,               -- {kind, effectTag, magnitude, ...} 过程增益模板
  label TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at BIGINT NOT NULL
);

-- dev 播种：均为"买过程不买结果"的环境/道具增益。
INSERT INTO gift_sku_map (sku, boon_json, label, enabled, created_at) VALUES
  ('rose',   '{"kind":"env_prop","effectTag":"advantage:combat","magnitude":1}',  '玫瑰·助战微光', 1, 0),
  ('rocket', '{"kind":"env_event","effectTag":"reroll:once","magnitude":1}',       '火箭·重掷机会', 1, 0),
  ('crown',  '{"kind":"env_prop","effectTag":"info:reveal","magnitude":1}',        '皇冠·情报揭示', 1, 0),
  ('shield', '{"kind":"env_prop","effectTag":"advantage:defense","magnitude":1}',  '护盾·环境掩体', 1, 0);

-- 礼物账：用于透明战报与结算 seam（worldId/sku/count/at）。
-- seam：真实扣费/主播分成经 billing（跨 feature）后接；未成年人礼物限额在 livegate 层标注为 seam。
CREATE TABLE gift_events (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  sku TEXT NOT NULL,
  gift_count INTEGER NOT NULL DEFAULT 1,
  from_user TEXT,                        -- 观众平台标识（可空）
  mapped INTEGER NOT NULL DEFAULT 0,     -- 是否命中 SKU 映射
  env_event_id TEXT,                     -- 命中时关联的 arena_env_events 行 id
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_gift_events_world ON gift_events(world_id, created_at);

-- 高光切片任务：按需生成，产物入本地对象存储；不进 tick 关键路径。
CREATE TABLE clip_jobs (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  event_id TEXT NOT NULL,                -- world_events.id
  object_key TEXT NOT NULL,              -- 产物在对象存储的 key
  status TEXT NOT NULL DEFAULT 'done',   -- pending / done / failed
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_clip_jobs_world ON clip_jobs(world_id, created_at);
