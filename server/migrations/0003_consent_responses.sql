-- MuseAI 平台库 0003（HC 加固）：同意响应独立表 —— respond 并发安全（C-5）。
-- 背景：原 respond 对 consent_requests.responses_json 做读改写且无事务，并发响应会丢更新
-- （失败保守但审计失真）。改为每个当事角色的响应独立成行，UNIQUE(consent_id, subject) 幂等去重；
-- respond 在事务内（Postgres 行锁 / SQLite 单连接事务天然串行）重算裁决状态，消除读改写竞态。
-- 可移植 SQL 子集：TEXT id / BIGINT 毫秒 / TEXT 枚举。
CREATE TABLE consent_responses (
  id TEXT PRIMARY KEY,
  consent_id TEXT NOT NULL,
  subject_character_id TEXT NOT NULL,           -- 当事角色（cloud_character_id）
  user_id TEXT NOT NULL,                         -- 响应人（当事角色主人）
  verdict TEXT NOT NULL,                         -- approved / declined
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_consent_response_unique ON consent_responses(consent_id, subject_character_id);
