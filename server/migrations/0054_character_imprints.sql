-- 0054：世界线烙印（Worldline Imprint）—— 让「复刻内核 ≠ 复刻这张卡」。
--
-- 提案与完整设计：`docs/build/spec-worldline-imprint.md`。本迁移是其中的第 1 步。
--
-- ════════════════════════════════════════════════════════════════════════════
-- 🔴 为什么烙印必须在服务端，而不是写进角色卡
-- ════════════════════════════════════════════════════════════════════════════
-- 角色卡是一份**可导出、可导入、可逐字复制**的 JSON。任何写进卡里的东西都可被复刻。
-- 而这套系统要保证的恰恰是「即使别人一字不差复刻了内核，也不会有同样的表现」。
--
-- ⇒ 烙印必须同时满足三条：
--   ① **落服务端、绑 `cloud_characters.id`** —— 不随卡导出，复刻内核复刻不到它；
--   ② **由确定性事实派生，不接受客户端写入** —— 不可自造（同「资产单一写入路径」红线）；
--   ③ **append-only** —— 经历是既成事实（同「公共事实不可回滚」）。
--
-- 🔵 它给了卡一个真正不可伪造的身份：`source_fingerprint`（0021）回答的是
-- 「你是不是从同一本书提取的」，烙印回答的是「**你是不是同一张卡**」——后者此前完全没有。
--
-- ════════════════════════════════════════════════════════════════════════════
-- 幂等：靠唯一索引，不靠「先查后写」
-- ════════════════════════════════════════════════════════════════════════════
-- `(character_id, world_id, kind, code)` 唯一 —— 同一张卡在同一个世界的同一类同一码只留一条。
-- 结算可能因重试而重入，唯一索引让重复派生变成一次 no-op，不需要在应用层做 TOCTOU 检查
-- （口径与 `notification_outbox.dedupe_key` / `backpacks.reward_hook_key` 同源）。
--
-- ⚠️ **`seq` 不是全局自增，是「这张卡的第几条烙印」**：褪色阶梯与容量都按它算。
-- 它由写入方在同一事务内取 `MAX(seq)+1`，与 `world_events.sequence` 那次踩过的坑同形——
-- 但这里**不需要发号器表**：并发面完全不同（那是全世界共写一条序列，这里是「一张卡在一次结算里」，
-- 而一张卡同一时刻只能在一个世界，那条闸已经把并发写入收敛掉了）。
--
-- 双库可移植（db.rs 约定）：id/外键 TEXT；时间 BIGINT 毫秒；无方言特性；无 CHECK 约束。

CREATE TABLE character_imprints (
  id TEXT PRIMARY KEY,
  -- 这条烙印属于哪张卡（`cloud_characters.id`）。
  character_id TEXT NOT NULL,
  -- 在哪个世界留下的。留着是为了可对账：烙印必须能被同一份 world_events 复算出来。
  world_id TEXT NOT NULL,
  -- 这张卡的第几条烙印（从 1 起）。褪色阶梯与容量按它算，见 `imprint::weather`。
  seq BIGINT NOT NULL,
  -- 五类之一：choice / bond / circumstance / unfinished / witness。
  kind TEXT NOT NULL,
  -- 类目内的确定性代码（如 `walked_to_the_end`）。🔴 **中性事实，不含评价词**——
  -- 「守界」而不是「意志坚定」。一旦含评价，它就变成了对角色的外部判定，
  -- 而角色应该只被自己的内核判定。
  code TEXT NOT NULL,
  -- 派生时钉住的确定性细节（如退场发生在第几拍 / 共几拍）。**只放数字与枚举，不放模型文本**。
  detail_json TEXT NOT NULL DEFAULT '{}',
  created_at BIGINT NOT NULL
);

-- 幂等锚点：同卡同世界同类同码只留一条。
CREATE UNIQUE INDEX idx_character_imprints_unique
  ON character_imprints(character_id, world_id, kind, code);
-- 读取面：按卡取全部烙印，按 seq 定序（褪色与指纹都要求确定序）。
CREATE INDEX idx_character_imprints_card ON character_imprints(character_id, seq);
