-- MuseAI 平台库 0015：P3 云成长服务位 + 平台道具单向售卖（平台规格「付费点整合 §4 / 道具经济与流转红线」）。
--
-- P3 新增两条**平台单向售卖**付费点，均**不分成**（平台增值服务/平台自营售卖 → charge 传 world_id=None → 全额入平台）：
--   ① 云成长（cloud_growth）：买「容量/服务位」——云角色位 / 同时在场世界数 / 背包容量等**平台配额（非战力）**，
--      落 user_entitlements 记生效额度。红线：只买过程/服务位，**不买战力、不买胜负**（荣誉/胜负仍由引擎评估）。
--   ② 平台道具售卖（item_purchase）：**平台→玩家**单向售卖，走 ledger::charge + 同事务 crate::backpack::grant_item_tx
--      （道具单一写入路径不破；reward_hook_key=订单号做幂等键防重复发货）。**绝无**玩家→玩家交易/转移/回购换 cent。
--
-- 资金红线仍集中在 ledger::charge：余额不足拒付零副作用、SUM(postings)=0、取整余数归平台
--   （此二付费点 world_id=None → 全额平台，无创作者分成对手方）。
-- 可移植：TEXT id / BIGINT 毫秒 / INTEGER 布尔，CREATE TABLE/INDEX + INSERT 双库通用（对齐 0008/0013/0014）。
-- 迁移无 feature 门控（表无条件存在，default 构建亦安全）；购买端点 + charge 才随 billing/arena feature 装配。

-- 云成长配额：每用户每 kind 一行，quantity 累加（(user_id, kind) 唯一 → upsert 累加）。
-- 语义：平台增值服务位（cloud_character_slot 云角色位 / world_presence_slot 同时在场世界数 /
--   backpack_capacity 背包容量 ...）。**非战力**——经济只碰配额/服务，不碰强度仲裁与胜负。
CREATE TABLE user_entitlements (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  kind TEXT NOT NULL,                         -- cloud_character_slot / world_presence_slot / backpack_capacity ...
  quantity BIGINT NOT NULL DEFAULT 0,         -- 累计已购额度（份）
  ref_id TEXT,                                -- 最近一次购买的 journal_id（审计溯源；免费 no-op 时可空）
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_user_entitlements_user_kind ON user_entitlements(user_id, kind);

-- 云成长 SKU 定价（分）：每份增加 quantity 份 entitlement_kind 配额；price_cents 全额入平台（不分成）。
CREATE TABLE growth_sku_map (
  sku TEXT PRIMARY KEY,
  entitlement_kind TEXT NOT NULL,             -- 购买后累加到 user_entitlements 的 kind
  quantity BIGINT NOT NULL DEFAULT 1,         -- 每份增加的额度
  price_cents BIGINT NOT NULL DEFAULT 0,      -- 单价（分）；0 → charge no-op（免费领取，保留免费能力）
  enabled INTEGER NOT NULL DEFAULT 1,
  label TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL
);

-- dev 播种：平台增值服务位（非战力）。price_cents>0 → 购买需先充值（未成年余额恒 0 → 必然余额不足 409）。
INSERT INTO growth_sku_map (sku, entitlement_kind, quantity, price_cents, enabled, label, created_at) VALUES
  ('cloud_slot_1',     'cloud_character_slot', 1, 1000, 1, '云角色位 +1',     0),
  ('world_presence_1', 'world_presence_slot',  1, 1000, 1, '同时在场世界 +1', 0),
  ('backpack_cap_10',  'backpack_capacity',   10,  500, 1, '背包容量 +10',    0);

-- 平台道具 SKU 目录：**平台单向售卖（平台→玩家）**。购买走 ledger::charge(world_id=None 全额平台) + grant_item_tx。
-- 红线：道具只可被消耗（consumed）/封存（sealed），**无回购换 cent 路径**（否则=变相提现出口）；无玩家间交易/转移端点。
-- 道具定义字段镜像 items 表（narrative/effect_tags/origin/cosmology/power_tier）；入包时 items.id = 'item_sku_'||sku 共享去重。
-- 运营须保证售卖道具为非战力/装饰或低 power_tier；per-world 准入仍由 admission 兜底（转译/降档/封存）。
CREATE TABLE item_sku_map (
  sku TEXT PRIMARY KEY,
  price_cents BIGINT NOT NULL DEFAULT 0,
  narrative TEXT NOT NULL DEFAULT '',
  effect_tags TEXT NOT NULL DEFAULT '[]',                 -- 镜像 items.effect_tags（JSON 数组字符串）
  origin_world_template_id TEXT NOT NULL DEFAULT 'platform_shop',
  cosmology_json TEXT NOT NULL DEFAULT '[]',
  power_tier INTEGER NOT NULL DEFAULT 1,
  enabled INTEGER NOT NULL DEFAULT 1,
  label TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL
);

-- dev 播种：纯装饰道具（effect_tags 空 + power_tier 1 → 无机械效果/无战力，per-world 准入恒过；诚实体现「买装饰不买战力」）。
INSERT INTO item_sku_map (sku, price_cents, narrative, effect_tags, origin_world_template_id, cosmology_json, power_tier, enabled, label, created_at) VALUES
  ('cosmetic_lantern', 500, '一盏温润的登场纸灯，仅作装饰点缀。', '[]', 'platform_shop', '["mundane"]', 1, 1, '登场纸灯', 0),
  ('cosmetic_badge',   300, '一枚记名徽章，仅作装饰点缀。',       '[]', 'platform_shop', '["mundane"]', 1, 1, '记名徽章', 0);
