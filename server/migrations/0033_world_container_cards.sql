-- MuseAI 平台库 0033（R2：自定义房装配 —— 副本卡的消费端）。
-- 总规格 `docs/build/spec-world-ecosystem.md` §10「自定义房闭环」；
-- 技术方案 `docs/build/spec-subplot-cards.md` §3 容器世界 / §4 装配与防刷 / §5 冲突与一致性
-- （⚠️ 该文件顶部「有效性地图」：§1/§2/§6/§7/§8 业务假设已作废，只有 §3/§4/§5 技术方案有效）。
--
-- 本表记录**一个自定义房实例装了哪几张副本卡**（装配期一次性钉住，与 `worlds.assembled_json`
-- 的 C-7 CAS 同一时刻）。它是容器装配的账，不是资产的账。
--
-- ---------------------------------------------------------------------------
-- 🔴 为什么必须是一张「引用表」，而不是给 subplot_cards 加一列「装在哪个房」
-- ---------------------------------------------------------------------------
-- §10【拍板 11】把副本卡的效力定死为「**永久蓝图**：装入自定义房，**房散卡在**」——
-- 与「消耗类：用掉即没」是两类道具。于是：
--   ① **装配不消耗卡**。装配只在本表 INSERT 一行引用，**绝不 UPDATE/DELETE `subplot_cards`**
--      （唯一的销毁语义是合成：`status='consumed'` + `consumed_into`，由 `subplot/` 独占）。
--      源码级断言：`assembly::container_tests::red_line_assembly_never_writes_subplot_cards`
--      扫 `assembly/mod.rs` 全文，出现任何针对 subplot_cards 的 UPDATE/DELETE 即红。
--   ② **一卡多房**。同一张卡可以同时装在任意多个房里（蓝图可复制，资产不转移），
--      故唯一键是 (world_id, card_id) 而非 card_id ——给卡加一列「装在哪个房」表达不了这件事，
--      还会诱导出「装房即占用」的错误语义。
--   ③ **房散卡在**。房结束/删除时本表行可留作审计，卡本身与本表无因果——
--      资产的生死只由 `subplot_cards` 一张表说了算（§0.2 资产单一写入路径）。
--
-- ---------------------------------------------------------------------------
-- 🔴 幂等：(world_id, card_id) 唯一
-- ---------------------------------------------------------------------------
-- 装配可能被并发触发（chapter start 与首 tick 同时进），C-7 CAS 只保证 assembled_json 单写，
-- 本表的重复写由唯一索引兜住：装配前先读已有 card_id 集合、只补缺失行，撞唯一键也不会多出行。
-- 因此「同一房重复装配」在本表上恒等幂等，与「同一房重复装配得同一份 assembled_json」同构。
--
-- ---------------------------------------------------------------------------
-- 审计口径
-- ---------------------------------------------------------------------------
-- `card_version` = 装配时钉住的卡蓝图版本（`subplot_cards.source_template_version`）——
-- 它同时是四段式实例种子第四段「卡集合指纹」的组成部分，故本表 + `assembled_json.sampling.seed`
-- 两侧可交叉复算：给定 (world_id, 阵容, template_version, 本表行) 必能重放出同一份装配。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 计数 / 无方言特性
-- （无 JSONB、无 serial、无 NOW()、无 CHECK、无 partial index）。范式见 0032 / 0030。

CREATE TABLE world_container_cards (
  id TEXT PRIMARY KEY,
  -- 自定义房实例。
  world_id TEXT NOT NULL,
  -- 被装入的副本卡（`subplot_cards.id`）。**本表不改写它，只引用它**。
  card_id TEXT NOT NULL,
  -- 装配时钉住的卡蓝图版本（`subplot_cards.source_template_version`）。
  -- 卡发新版不自动生效（与 `worlds.template_version` 钉住哲学一致）。
  card_version BIGINT NOT NULL,
  -- 装入时的卡主（= 房主）。无交易红线下卡只能由本人装入自己的房，此列为事后对账留痕。
  owner_id TEXT NOT NULL,
  -- 容器模板 id（`worlds.template_id` 快照）：卡装进了哪个容器。
  template_id TEXT NOT NULL,
  -- 容器 `skeleton_json.subplotCardRefs` 中的模板序下标（确定性序，replay 对账用）。
  slot_no INTEGER NOT NULL,
  assembled_at BIGINT NOT NULL
);

-- 幂等闸：同一房同一张卡至多一行（重复/并发装配撞这里）。
CREATE UNIQUE INDEX idx_world_container_cards_unique ON world_container_cards(world_id, card_id);
-- 反查：这张卡被装进过哪些房（一卡多房是正常形态，见上）。
CREATE INDEX idx_world_container_cards_card ON world_container_cards(card_id);
