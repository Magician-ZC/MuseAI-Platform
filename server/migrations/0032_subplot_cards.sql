-- MuseAI 平台库 0032（R2：副本卡资产与合成）。
-- 总规格 `docs/build/spec-world-ecosystem.md` §10【拍板 1、6、7、11、17】+ §18 衔接表
-- 「副本卡 → subplot_cards 资产表 + 合成 + 自定义房装配」。
--
-- 副本卡 = **官方世界结算产出的「剧情结晶」**（把你亲历的剧情副本铸成卡）：
--   ① 官方专营——唯一产出口是三层结算 ③ 世界线层查**公示产出表**的确定发放（无 RNG，见下）；
--   ② 永久蓝图——装进自定义房当内容燃料，**房散卡还在**（不同于消耗类道具用掉即没）；
--   ③ 合成升级消耗——N×n★ → 1×(n+1)★，这是经济体的**回收口**（防通胀），也是本表唯一的销毁语义。
--
-- ---------------------------------------------------------------------------
-- 🔴 为何独立建表，而不是塞进 items / backpacks 当一个特殊 tag
-- ---------------------------------------------------------------------------
-- 1) **两种资产的生命周期语义相反**。`backpacks` 是「道具持有关系」：owned/carried/sealed/consumed，
--    可被 `POST /worlds/{id}/carry` 带进世界、可被引擎消费。副本卡**永不入场、永不被引擎读取**，
--    它是开房时的**内容蓝图**，装进自定义房之后卡仍在账户里。把它塞进 backpacks 就必须给 carry /
--    admission / 准入策略全链路开一堆「这个 tag 不参与」的例外——例外即漏点。
-- 2) **`items` 是引擎可见的世界物**（narrative / effect_tags / cosmology / power_tier 全部会随
--    `carry` 与装配进入 `RoundInput`）。副本卡带 §0.1 平权红线：**永不加战力、永不进引擎决策**。
--    物理隔离在表一级，比在应用层写「记得过滤」可靠得多——引擎侧根本没有本表的读取路径
--    （由 `subplot::tests::red_line_subplot_cards_never_enter_engine_decision` 源码级断言守死）。
-- 3) **合成需要销毁语义与血缘**（consumed_into / synthesized_from_json），backpacks 没有这两列，
--    加进去会污染道具语义（道具的 consumed 是「用掉」，不是「熔了变成另一件」）。
-- 4) §18 衔接表明写新增面是 `subplot_cards` 资产表——与规格逐字对齐。
--
-- ---------------------------------------------------------------------------
-- 🔴 幂等：`(owner_id, grant_key)` 唯一索引 = 副本卡的**唯一发放闸**
-- ---------------------------------------------------------------------------
-- 历练 `grant_mileage_tx` 是裸 UPDATE，其幂等寄生在调用方的「未通关 → 通关」转变沿判定上；
-- 副本卡是 INSERT 类资产，**没有那种天然转变沿**，因此幂等必须自带、且必须落在 DB 约束上
-- （与 `backpacks(user_id, reward_hook_key)`（0005）同款范式）：
--   - 结算产出 grant_key = `settlement:{world_id}:{character_id}:worldline`
--     → 同一世界同一张卡的世界线结算恒只铸一张，重复/并发结算撞唯一键即「已发放」，静默跳过。
--   - 合成产出 grant_key = `synthesis:{按 id 升序拼接的全部源卡}`
--     → 同一组源卡只可能合成一次，重复提交不可能凭空多出卡（源卡 CAS 之外的第二道防线）。
--   - 新手礼包/运营发放 grant_key 由调用方给（如 `starter:{user_id}`）。
--
-- ---------------------------------------------------------------------------
-- 🔴 无交易：本表没有任何「转让」形态
-- ---------------------------------------------------------------------------
-- 无提现红线（§0.5）下玩家间道具交易 = RMT 侧门，§10 明写「玩家间交易暂不开」。
-- 因此 `owner_id` **只在 INSERT 时写入，全仓不存在任何改写它的 SQL**
-- （源码级断言：`subplot::tests::red_line_no_transfer_or_trade_path`）。远期若开放需重风控 + 显式评审。
--
-- 双库可移植子集（`db.rs` 约定）：TEXT id / BIGINT 毫秒 / INTEGER 计数与星级 / 无方言特性
-- （无 JSONB、无 serial、无 NOW()、无 CHECK、无 partial index）。范式见 0025 / 0030。

CREATE TABLE subplot_cards (
  id TEXT PRIMARY KEY,
  -- 拥有者。**只在 INSERT 时写入**（无转让路径，见上）。
  owner_id TEXT NOT NULL,
  -- 星级 1..N（N = 运营参数 MUSE_SUBPLOT_MAX_STAR，默认 5）。与模板星级 curation（0020）同一把尺子：
  -- 结算发卡时 `star_rating > 实例 starRating` 一律**剔除**（不降级、不替换），与装配层产出封顶同口径。
  star_rating INTEGER NOT NULL DEFAULT 1,
  -- 卡面公示名（产出表档位声明；缺省回落档位 label）。展示与审计用，无任何数值含义。
  label TEXT NOT NULL DEFAULT '',
  -- 铸成来源：settlement（结算产出）/ synthesis（合成）/ grant（新手礼包与运营发放）。
  origin_kind TEXT NOT NULL,
  -- 幂等键（与 owner_id 组唯一，见上）。
  grant_key TEXT NOT NULL,
  -- 来源世界与模板（内容蓝图指针，自定义房装配的解引用入口）。
  -- 合成卡按「源卡 id 升序取首张」确定性继承（无随机、可 replay），血缘另存 synthesized_from_json。
  source_world_id TEXT,
  source_template_id TEXT,
  source_template_version BIGINT,
  -- 合成血缘：源卡 id 的 JSON 数组（升序）。非合成卡为 '[]'。全链审计（§0.2）用，不解析入统计。
  synthesized_from_json TEXT NOT NULL DEFAULT '[]',
  -- owned（在手）/ consumed（已作为合成材料销毁）。**销毁是软删**：合成的回收口必须可溯，
  -- 物理删行会让「3 张 2★ 去哪了」在账上凭空消失，违反 §0.2 全链审计。
  status TEXT NOT NULL DEFAULT 'owned',
  -- 被合成进哪张卡（status='consumed' 时非空）。反向血缘，与 synthesized_from_json 互为对账。
  consumed_into TEXT,
  acquired_at BIGINT NOT NULL,
  consumed_at BIGINT
);

-- 发放闸：同一 owner 同一 grant_key 至多一张卡（结算重放 / 合成重提交 / 礼包重发全部撞这里）。
CREATE UNIQUE INDEX idx_subplot_cards_grant_unique ON subplot_cards(owner_id, grant_key);
-- 列表与合成选材的唯一读路径：owner → status → star。最左前缀即 owner_id，无需再建单列索引。
CREATE INDEX idx_subplot_cards_owner_status_star ON subplot_cards(owner_id, status, star_rating);
