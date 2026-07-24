-- MuseAI 平台库 0019（波次 2：历练值 + 卡位制）。
-- 历练（mileage）：参与产出的成长值，挂在云端角色卡上（卡是养成容器），只用于准入与解锁。
-- 红线（产品根基·叙事平权）：历练**绝不进入引擎决策**——RoundInput / role_decide / 仲裁一概不读本列，
-- 唯一写入路径为 progression::grant_mileage_tx（各结算点同事务调用）。
-- 卡位（card_slots）：用户可同时持有的未撤回云端角色数上限，默认 3；由总历练解锁至多 6（阈值见 progression 常量区）。
-- 双库可跑（db.rs 约定）：计数 BIGINT/INTEGER + NOT NULL DEFAULT，无方言特性；
-- SQLite 不支持单条 ALTER 多列，拆为两条独立 ADD COLUMN（SQLite/Postgres 通用）。

ALTER TABLE cloud_characters ADD COLUMN mileage BIGINT NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN card_slots INTEGER NOT NULL DEFAULT 3;
