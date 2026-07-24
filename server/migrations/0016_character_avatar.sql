-- MuseAI 平台库 0016：角色头像行级字段（Phase A）。
-- 头像不进不可变 card_json，作为 cloud_characters 行级可变字段存储（对象键 + 回读 URL + 机审裁决）。
-- 双库可跑：SQLite 不支持单条 ALTER 多列，故拆为三条独立 ADD COLUMN（SQLite/Postgres 通用）。
-- 列均可空：历史行留 NULL（无头像），上传后填充；不随 feature 门控，default 构建亦安全。

ALTER TABLE cloud_characters ADD COLUMN avatar_object_key TEXT;
ALTER TABLE cloud_characters ADD COLUMN avatar_url TEXT;
ALTER TABLE cloud_characters ADD COLUMN avatar_moderation TEXT;
