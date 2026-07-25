-- MuseAI 平台库 0025（R1：三层结算 ③ 世界线层贡献归因账本）。
-- 总规格 §9【拍板 9(修订)、10、19】三层结算 + §10【拍板 17】确定性产出表：
--   ① 保底层（出席）  = 既有 grant_mileage_tx 通关/终局历练，不落本表；
--   ② 成就层（钩子）  = 既有 grant_item_tx + reward_hook_key 幂等，不落本表；
--   ③ 世界线层（里程碑推动者）= 本表。引擎每回合按 IntensityWeights 口径折算出的**逐角色**贡献分，
--      在 commit_tick 事务内累积；终局/通关结算时查公示产出表（assembled_json 内钉住）确定发放。
--
-- 🔴 为何单独建表、而不是塞进 worlds.narrative_state_json：
--    narrative_state_json 会被回灌进引擎（RoundInput.state），任何写进去的数值都可能被 role_decide /
--    仲裁读到，直接违反 §0.1「不卖胜负与数值平权：数值优势不得进入引擎决策」。贡献分是**结算侧账本**，
--    与引擎决策物理隔离——引擎永不读本表（同 cloud_characters.mileage 的红线口径，grep 级测试守护）。
--
-- 字段：
--   score_milli            本世界该角色累计贡献分 ×1000（定点整数，避免双库 REAL 精度/排序差异）。
--                          全量口径：每回合 Σ(outcome 结果权重) + Σ(willSpeak 互动权重)，逐角色分摊，
--                          分项之和 == 引擎 round_intensity 标量（同一套 IntensityWeights）。
--   milestone_score_milli  其中**真正喂进里程碑**（该回合 patch 含 world.milestoneProgress_* Increment）
--                          的部分 ×1000。③ 层结算只认这一列——「里程碑推动者」的确定性定义。
--   settled_at             ③ 层结算时刻（0 = 未结算）。结算走条件 UPDATE ... WHERE settled_at=0 的 CAS，
--                          rows>0 才发放 → 重复/并发结算天然不双发（同 end_world_tx 的幂等范式）。
--
-- NPC（世界固有角色）也会入表（引擎按 character_id 折算不区分主体），但结算时与 world_members 取交集，
-- 无主之卡自然不发放——留档只为审计与运营看板可溯。
--
-- 双库可移植子集（db.rs 约定）：TEXT id / BIGINT 毫秒与定点分 / 无方言特性 / 复合主键；
-- 复合主键 (world_id, character_id) 即世界维度前缀索引，无需额外 CREATE INDEX。

CREATE TABLE world_contributions (
  world_id TEXT NOT NULL,
  character_id TEXT NOT NULL,
  score_milli BIGINT NOT NULL DEFAULT 0,
  milestone_score_milli BIGINT NOT NULL DEFAULT 0,
  settled_at BIGINT NOT NULL DEFAULT 0,
  updated_at BIGINT NOT NULL,
  PRIMARY KEY (world_id, character_id)
);
