-- MuseAI 平台库 0011：创作者自制世界模板上云（/assets/worlds，平台规格 §2.3 / §9.5.C）。
-- world_templates 复用为双形态：官方模板（official=1, owner_id NULL，admin 后台建）
-- + 创作者资产（official=0, owner_id 非空，走机审 + 版本 + 撤回生命周期，对齐 cloud_characters）。
-- 追加 owner 隔离 / 权利声明 / 可审计 manifest / 撤回位。
-- 可移植：单列 ADD COLUMN 在 SQLite / Postgres 通用；owner_id/rights/manifest 可空（官方模板留 NULL）；
-- withdrawn NOT NULL DEFAULT 0 对历史行安全回填。迁移不随 feature 门控，default 构建亦安全运行。

ALTER TABLE world_templates ADD COLUMN owner_id TEXT;
ALTER TABLE world_templates ADD COLUMN rights_declaration TEXT;
ALTER TABLE world_templates ADD COLUMN manifest_json TEXT;
ALTER TABLE world_templates ADD COLUMN withdrawn INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_world_templates_owner ON world_templates(owner_id);
