-- MuseAI 平台库 0031（R2：新手动线 · 新人大礼包发放登记，总规格 §13【拍板 21】）。
--
-- 本表只解决一件事：**「每人只能领一次新人大礼包」由数据库保证，而不是应用层的读-判-写**。
-- 读-判-写有 TOCTOU：并发两次领取都读到「未领过」→ 各发一张预制卡 + 各建一个微本世界，
-- 卡位与世界数被双倍消耗，事后无法自动收敛。故 user_id 直接做 **PRIMARY KEY**：
-- 第二次领取在 INSERT 处撞唯一键 → 整个事务（预制卡 + 微本世界 + 登记行）一起回滚，
-- 调用方读回既有登记行、返回**与首次逐字节相同**的响应（幂等）。
--
-- 与 `idempotency_keys` 的分工（两者都要，不可互相替代）：
--   - `Idempotency-Key` 覆盖的是「同一次点击的 HTTP 重试」——同 key 同载荷返回缓存响应；
--     客户端不传 key、或换一个 key 再点，它**拦不住**。
--   - 本表的主键覆盖的是「这个人这辈子只领一次」——与请求头无关，是业务事实的唯一约束。
--
-- 🔴 未验证功能默认关闭（VALIDATION.md §0.1）：整条新手动线由运营开关 `MUSE_ONBOARDING` 控制，
--    **默认关闭**（`onboarding::onboarding_enabled`）。本表在关闭态下不会有任何写入；
--    已有登记行在关闭态下也读不出（读取侧降级，范式同 `MUSE_ROOM_INVITATIONS`）。
--
-- 🔴 资产单一写入路径（真红线 §0.2）：本表**不是资产表**——它只登记「谁领过、领到了哪张卡、
--    哪个世界」。礼包里的道具/历练若将来落地，一律走 `backpack::grant_item_tx` /
--    `progression::grant_mileage_tx`，绝不在本表或本模块直插 backpacks / cloud_characters.mileage。
--
-- 🔴 本表与 `world_members` **没有任何写入关系**（体例同 0029 房间邀请）：领取礼包只发卡 + 建房，
--    真正入场仍须走 `POST /worlds/{id}/join`，于是 join 的全部服务端权威校验（角色属本人 /
--    approved / 未撤回 · 人数上限 · 一人一卡防自刷 · 同源唯一 · 星级历练准入 · 生死契约签署 ·
--    未成年门）一条不少地生效。礼包是**入口**，不是特权通道。
--
-- 双库可移植子集（db.rs 约定）：TEXT id / BIGINT 毫秒 / 无方言函数（无 strftime、无 date_trunc、
-- 无 JSONB、无 serial、无 ON CONFLICT 方言写法）；本迁移只新建一张表，不改任何既有表结构
-- （零回填、零锁表，SQLite 不支持单条 ALTER 多列的限制在此不适用）。
CREATE TABLE onboarding_grants (
  -- 🔴 每人一行 = 每人只领一次。主键即业务约束，不依赖任何应用层判断。
  user_id TEXT PRIMARY KEY,
  -- 领到的预制卡模板 id（`onboarding::presets` 里的常量 id，非数据库外键——预制卡库是代码内 fixture，
  -- 随版本走；这里存 id 只为审计「当时发的是哪一张」，卡库改版不回改历史登记）。
  preset_id TEXT NOT NULL,
  -- 实际落库的云端角色卡 id（cloud_characters.id）。**占卡位**：与用户自己发布的卡同一口径，
  -- 礼包不绕过 users.card_slots 这个产品约束（详见 onboarding::claim_gift 注释）。
  cloud_character_id TEXT NOT NULL,
  -- 为该用户新建的单人微本世界 id（worlds.id）。一人一世界实例：既天然回避同源唯一撞车，
  -- 又保证「5 分钟速通」的节奏隔离（详见 onboarding 模块头注释）。
  world_id TEXT NOT NULL,
  created_at BIGINT NOT NULL
);
-- 反查「这个微本世界属于哪次礼包发放」（开演端点按 world_id 校验归属，避免拿别人的世界开演）。
CREATE INDEX idx_onboarding_grants_world ON onboarding_grants(world_id);
