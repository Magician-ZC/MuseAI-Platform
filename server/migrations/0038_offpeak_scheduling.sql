-- MuseAI 平台库 0038（R3 成本工程）：**错峰调度的可观测面**。
--
-- 总规格 `docs/build/spec-world-ecosystem.md` §17【拍板 16】列了四条成本杠杆：
--   ①错峰调度（连载/慢炖场 tick 排夜间折扣时段，5-7.5 折）
--   ②上下文缓存（已有）  ③Batch API（本批次未做，可行性分析见 runtime/mod.rs 的 `offpeak` 模块头）
--   ④分环节路由（已有 model_routes）
-- 本迁移是①的**记账侧**：把「这一拍是不是排在折扣时段、被错峰压了多久」写进逐拍成本台账，
-- 使 §17「成本仪表」（`world_ticks.cost_tokens` + `admin_api/dashboards.rs` 的 `cost.*`）
-- 能直接把 token 花销拆成「折扣时段 / 原价时段」两桶——**没有这三列，错峰省了多少是不可度量的**，
-- 而 VALIDATION.md T3 门槛「ARPPU ≥ 3× 单用户月度模型成本」正需要这个数。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 本迁移不改变任何行为
-- ═══════════════════════════════════════════════════════════════════════════
-- 三列全部带 DEFAULT，语义上的「中性值」= 现状：
--   off_peak=0（未命中折扣时段）· price_ratio_pct=100（原价）· defer_ms=0（未被延后）。
-- 错峰调度器本身**默认关闭**（开关 `MUSE_OFFPEAK_SCHEDULING`，见 runtime/mod.rs），
-- 关闭时每一拍都按中性值落库，于是这三列对存量数据与关闭态部署**恒为中性**，
-- 既有 `cost.*` 聚合（只读 cost_tokens）逐字不受影响。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 口径声明（读这三列的人必须知道）
-- ═══════════════════════════════════════════════════════════════════════════
-- - `off_peak`：**排期时刻**调度器判定本拍落在折扣时段（1）还是没有（0）。
--   判定的时区口径是 **UTC**（与 `admin_api::dashboards::utc_day_start_ms` /
--   `runtime::day_string` / `reports::day_bounds` 同一套日界，全仓唯一一套）；
--   运营若要按供应商所在时区书写窗口，用 `MUSE_OFFPEAK_TZ_OFFSET_MIN` 声明偏移，
--   **解析期就折算成 UTC**，落库与判定链路上不存在第二套时区口径。
-- - `price_ratio_pct`：排期时命中的**名义**价格档位（百分比，100=原价，50=5 折）。
--   🔴 它是**运营配置的折扣档位**，不是供应商账单的结算价——用于估算「错峰省了多少」，
--   不得当作对账依据。非错峰路径（开关关闭 / 直播场豁免 / 手动端点排的拍）恒 100，
--   即便那一拍碰巧落在夜间：只归因给调度器真正做出的决策，宁可低估不可高估。
-- - `defer_ms`：本拍因错峰被**实际延后**的毫秒数（0=未延后）。它是「错峰生效了多少」的
--   直接度量；进程内存态计时，server 重启会让在途的延后账清零（保守低估，可接受降级）。
--
-- 可移植子集（server/src/db.rs）：三列均为 INTEGER/BIGINT + 常量 DEFAULT，
-- SQLite 与 Postgres 的 `ALTER TABLE ... ADD COLUMN` 语法一致；不含任何方言函数。

ALTER TABLE world_ticks ADD COLUMN off_peak INTEGER NOT NULL DEFAULT 0;
ALTER TABLE world_ticks ADD COLUMN price_ratio_pct INTEGER NOT NULL DEFAULT 100;
ALTER TABLE world_ticks ADD COLUMN defer_ms BIGINT NOT NULL DEFAULT 0;

-- 成本仪表按「世界 × 折扣档位」分桶聚合的支撑索引（cost.* 的查询形态恒为
-- `WHERE world_id = ? AND created_at >= ? AND created_at < ?` 再按 off_peak 分组）。
CREATE INDEX idx_world_ticks_offpeak ON world_ticks(world_id, off_peak);
