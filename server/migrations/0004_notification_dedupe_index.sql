-- 0004（HD）：通知 outbox dedupe_key 唯一索引（C-6）。
-- 背景：enqueue 原本靠「先 SELECT 再 INSERT」去重，存在 TOCTOU——并发同键可双插。
-- 收口：加部分唯一索引，enqueue 改为靠约束去重（冲突即跳过）。
-- 部分索引 `WHERE dedupe_key IS NOT NULL`：允许无去重键（NULL）的通知并存多条，
-- 仅对非空 dedupe_key 强制唯一。SQLite(≥3.8.0) 与 PostgreSQL 均支持此部分索引语法。
CREATE UNIQUE INDEX idx_outbox_dedupe_unique
  ON notification_outbox(dedupe_key)
  WHERE dedupe_key IS NOT NULL;
