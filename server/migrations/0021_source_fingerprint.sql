-- MuseAI 平台库 0021（R1：同源卡同世界唯一，规格 §7「这个世界只有一个唐三」）。
-- source_fingerprint：角色卡的提取源指纹 = card_json.identity.sourceWork.sourceId
--   （muse-engine character 合成时写入的源文件全字节哈希）。发布时（assets::publish）由服务端从卡里
--   提取物化成列——join 是热路径，不能每次反序列化整张 CharacterCardV2 再取字段。
--   服务端权威：只从 card_json 推导，绝不接受客户端另传的指纹（§9.6 铁律）。
-- pristine：是否「未编辑原味卡」（lifecycle='draft' 且 revision=0，即 synthesis 合成后未经用户编辑）。
--   只有 pristine=1 参与同源拒绝；用户编辑过的卡一律放行——把撞卡压力转化为编辑创作激励，
--   是本规则三重收益之一，拦编辑过的卡会直接毁掉该设计。
-- 历史行：source_fingerprint 保持 NULL、pristine 回填 0 —— 老卡不参与同源判定（保守降级，
--   绝不因迁移前缺字段拒绝用户入世）。
-- 索引：join 校验按 source_fingerprint 反查同世界在场卡，普通索引（非条件唯一索引——
--   「pristine 卡按 fingerprint 唯一」需要带 WHERE 的部分唯一索引，不在双库可移植子集内）。
-- 双库可跑（db.rs 约定）：TEXT id / INTEGER 布尔 + NOT NULL DEFAULT；
-- SQLite 不支持单条 ALTER 多列，拆为两条独立 ADD COLUMN（SQLite/Postgres 通用）。

ALTER TABLE cloud_characters ADD COLUMN source_fingerprint TEXT;
ALTER TABLE cloud_characters ADD COLUMN pristine INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_cloud_characters_source_fingerprint ON cloud_characters(source_fingerprint);
