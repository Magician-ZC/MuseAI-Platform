-- MuseAI 平台库 0005（HE 加固）：章节房幂等下沉 DB + per-carry 降档持久化。
-- 分配：HE 域独占 0005（见 docs/build/P4a-HARDENING.md）。可移植 SQL 子集（TEXT/INTEGER，NULL 语义双库一致）。

-- C-3：把「隐藏道具不二次发货」从单行 JSON 下沉为 DB 约束。
--  reward_hook_key = {world_id}:{character_id}:{pool_item_id}（章节兑现的幂等键；非兑现路径写 NULL）。
--  UNIQUE(user_id, reward_hook_key)：SQLite/Postgres 均视多个 NULL 为互异，故仅对有键的兑现去重，
--  不影响支付履约/测试直发（reward_hook_key = NULL）多行并存。
ALTER TABLE backpacks ADD COLUMN reward_hook_key TEXT;
CREATE UNIQUE INDEX idx_backpacks_reward_hook ON backpacks(user_id, reward_hook_key);

-- S-5：per-carry 转译降档覆盖列。carry Translated 时落库（power_tier 降档 + effect_tags 快照），
--  未来强度仲裁读覆盖值而非物品定义原值，堵住「转译只进响应」的 maxPowerTier 后门。
--  非转译入场（admitted/sealed/rejected）写 NULL，避免历史覆盖残留。
ALTER TABLE backpacks ADD COLUMN power_tier_override INTEGER;
ALTER TABLE backpacks ADD COLUMN effect_tags_override TEXT;
