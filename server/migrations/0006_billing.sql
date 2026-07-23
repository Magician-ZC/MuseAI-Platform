-- MuseAI 平台库 0006：计费（P4b，feature=billing）索引。
-- billing_balances / orders / ledger_entries 三表已在 0001 建好；此处仅补查询路径索引，
-- 不改表结构、不改语义。可移植：CREATE INDEX 在 SQLite / Postgres 通用。
-- 迁移不随 feature 门控（表在 0001 无条件存在），default 构建运行本迁移亦安全。

CREATE INDEX idx_orders_user ON orders(user_id);
CREATE INDEX idx_ledger_user ON ledger_entries(user_id);
CREATE INDEX idx_ledger_order ON ledger_entries(order_id);
