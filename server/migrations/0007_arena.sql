-- MuseAI 平台库 0007（P6 赛事房 arena）：赛制状态机 + 荣誉奖励 + 环境事件通道 + 复活资格 + 淘汰同意台账。
-- 分配：P6a 域独占 0007（见 docs/build/P4b-P6-BUILD.md）。可移植 SQL 子集（TEXT id / BIGINT 毫秒 / INTEGER 布尔+计数 / TEXT JSON）。
-- 红线（规格 §2.5）：买过程不买结果——arena_rewards 仅荣誉（称号/立绘框/榜单）无强度字段；复活仅记资格不免死；
-- 淘汰不可逆须经 consents 同意门控——arena_eliminations 记「提案→consent→落定」台账，仅 approved 才进
-- arena_matches.eliminations_json；pending/declined/expired_conservative 保守不落定。

-- 赛制状态机（每世界一场；唯一胜者：现役参赛角色扣除已落定淘汰后收敛到 1 人即 winner）。
CREATE TABLE arena_matches (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  phase TEXT NOT NULL DEFAULT 'lobby',           -- lobby / running / concluded
  alliances_json TEXT NOT NULL DEFAULT '[]',     -- 结盟/阵营快照（结盟可变，最终仍唯一胜者）
  eliminations_json TEXT NOT NULL DEFAULT '[]',  -- 已「落定」的淘汰角色（经同意门控 approved 后写入）
  winner_char_id TEXT,                           -- 收敛到 1 人即胜者；未决为 NULL
  updated_at BIGINT NOT NULL,
  created_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_arena_match_world ON arena_matches(world_id);

-- 胜者荣誉奖励（非强度）：称号 / 立绘框 / 赛季榜。绝不含 power/strength/tier 等强度字段（红线）。
CREATE TABLE arena_rewards (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  character_id TEXT NOT NULL,
  kind TEXT NOT NULL,                            -- title / portrait_frame / leaderboard
  label TEXT NOT NULL DEFAULT '',                -- 展示文案（荣誉性）
  season TEXT,                                   -- 赛季榜标识（可空）
  created_at BIGINT NOT NULL
);
-- 同一世界同一角色同类奖励幂等（重复结算不重复发荣誉）。
CREATE UNIQUE INDEX idx_arena_reward_unique ON arena_rewards(world_id, character_id, kind);

-- 跨 agent 契约（P6b livegate 写 kind='gift_boon'；P6a 读作透明战报/环境）。schema 固定，见 P4b-P6-BUILD.md。
CREATE TABLE arena_env_events (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  applied_tick INTEGER,                          -- 应用到的回合（NULL=尚未注入引擎回合，seam）
  kind TEXT NOT NULL,                            -- gift_boon / env ...
  payload_json TEXT NOT NULL DEFAULT '{}',
  aggregated_count INTEGER NOT NULL DEFAULT 1,   -- 同回合同 SKU 聚合计数（防事件风暴）
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_arena_env_world ON arena_env_events(world_id, created_at);

-- 复活赛「资格」台账（可买资格不可买免死）：仅记 eligibility；实际扣费经 billing 集成留 TODO seam。
CREATE TABLE arena_revive_grants (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  character_id TEXT NOT NULL,                    -- 复活资格指向的参赛角色
  user_id TEXT NOT NULL,                         -- 购买/申请资格者（观众或角色主人）
  status TEXT NOT NULL DEFAULT 'eligible',       -- eligible（仅资格；不等于免死/不改最终判定）
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_arena_revive_world ON arena_revive_grants(world_id);

-- 淘汰同意门控台账（补 P4a 重审「同意机制触发源未接通」缺口，在 arena 层落地，不改引擎/consents）：
-- player-owned 角色淘汰不可逆 → 先 consents::create_consent(permanent_exit)，此处记 pending_consent；
-- settle 时仅 approved 落定进 eliminations，declined/expired_conservative 保守 spared，pending 暂不动。
CREATE TABLE arena_eliminations (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  character_id TEXT NOT NULL,
  consent_id TEXT,                               -- 关联同意请求（player-owned 必有）
  status TEXT NOT NULL DEFAULT 'pending_consent', -- pending_consent / eliminated / spared
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_arena_elim_unique ON arena_eliminations(world_id, character_id);
