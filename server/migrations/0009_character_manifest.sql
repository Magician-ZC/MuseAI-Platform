-- MuseAI 平台库 0009：角色资产可审计 manifest（平台规格 §2.3）。
-- 发布时为每个云端角色快照落一份 manifest，列明「字段清单 / 用途 / 可见范围 / 删除策略」，
-- 供后台审核与合规审计核对最小发布清单。
-- 可移植：单列 ADD COLUMN 在 SQLite / Postgres 通用；列可空（历史行留 NULL），新发布恒填充。
-- 迁移不随 feature 门控，default 构建亦安全运行。

ALTER TABLE cloud_characters ADD COLUMN manifest_json TEXT;
