-- MuseAI 平台库 0014：P2 复活 + 开房定价（平台规格「付费点整合 §2/§3」）。
--
-- P2 新增两个付费点，各需一个定价来源（对齐 P1 gift 从 gift_sku_map.price_cents 读单价的模式）：
--   ① 开房费 room_open_price_cents —— 房主用某模板建房时扣费，**分成给模板 owner**（创作者经济回流）。
--   ② 复活费 revive_price_cents   —— 观众/角色主人买复活赛「资格」时扣费，**平台服务不分成**
--       （charge 传 world_id=None → 全额入平台；买过程不买结果，避免「付费改判」观感）。
--
-- 两列均落在 world_templates（题材维度定价，运营/创作者可为该题材设价）；worlds 实例经 template_id 溯源。
-- 默认 0 = 免费：charge(price==0) 走 no-op（不产 journal），**保留既有免费开房/免费复活能力**，
--   不破坏任何现有测试（现有世界/模板无此列值即视为 0）。
--
-- 资金红线仍集中在 ledger::charge：余额不足拒付零副作用、自打赏归零、未成年 owner 挂平台、取整余数归平台、SUM=0。
-- 可移植：单列 ADD COLUMN + 常量 DEFAULT，SQLite / Postgres 通用（对齐 0011/0013 的 ADD COLUMN 模式）。
-- 迁移无 feature 门控（表列无条件存在）；charge/POST /worlds 才随 billing/arena feature 装配。

ALTER TABLE world_templates ADD COLUMN room_open_price_cents BIGINT NOT NULL DEFAULT 0;
ALTER TABLE world_templates ADD COLUMN revive_price_cents BIGINT NOT NULL DEFAULT 0;
