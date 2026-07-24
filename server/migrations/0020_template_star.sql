-- MuseAI 平台库 0020（波次 3：模板星级 + 产出封顶 + 历练准入）。
-- 星级（star_rating 1-5）：世界模板的内容规格档位，服务端权威——发布时自动定档（保守起步，
-- 自动档封顶 2★，见 assets/worlds.rs），3-5★ 只能运营 curation 晋升（admin_api，数据晋升）。
-- 消费方：装配产出封顶（assembly，奖励道具档位 ≤ 星级 + 稀有预算）、join 历练准入（worlds，
-- star≥3 要求投放卡 mileage 达阈值）、大厅列表/详情投影 starRating。
-- star_source：'auto'（发布自动定档）| 'curated'（运营定档）——审计与后台展示用，引擎不读。
-- 双库可跑（db.rs 约定）：单列 ADD COLUMN + NOT NULL DEFAULT，SQLite/Postgres 通用，
-- 历史行安全回填为 1★/'auto'（老模板保守起步，不影响既有装配退化路径与 join 行为）。

ALTER TABLE world_templates ADD COLUMN star_rating INTEGER NOT NULL DEFAULT 1;
ALTER TABLE world_templates ADD COLUMN star_source TEXT NOT NULL DEFAULT 'auto';
