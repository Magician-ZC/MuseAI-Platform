-- MuseAI 平台库 0001：核心表（可移植 SQL 子集：TEXT id / BIGINT 毫秒 / TEXT JSON / INTEGER 布尔）
-- 表清单对应平台规格 §9.3

CREATE TABLE users (
  id TEXT PRIMARY KEY,
  phone TEXT UNIQUE,
  email TEXT UNIQUE,
  nickname TEXT NOT NULL DEFAULT '',
  age_declared INTEGER NOT NULL DEFAULT 0,      -- 0 未声明 / 1 成年声明 / 2 未成年
  role TEXT NOT NULL DEFAULT 'user',            -- user / admin / operator / reviewer / support / finance
  status TEXT NOT NULL DEFAULT 'active',        -- active / banned
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);

CREATE TABLE sms_challenges (
  id TEXT PRIMARY KEY,
  phone TEXT NOT NULL,
  code_hash TEXT NOT NULL,
  expires_at BIGINT NOT NULL,
  consumed INTEGER NOT NULL DEFAULT 0,
  created_at BIGINT NOT NULL
);

CREATE TABLE refresh_tokens (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  token_hash TEXT NOT NULL,
  expires_at BIGINT NOT NULL,
  revoked INTEGER NOT NULL DEFAULT 0,
  created_at BIGINT NOT NULL
);

CREATE TABLE identity_verification_refs (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  reference_id TEXT NOT NULL,
  status TEXT NOT NULL,                          -- pending / verified / failed
  created_at BIGINT NOT NULL
);

CREATE TABLE notification_preferences (
  user_id TEXT PRIMARY KEY,
  channels_json TEXT NOT NULL DEFAULT '{}',
  quiet_hours TEXT NOT NULL DEFAULT '',
  unsubscribed INTEGER NOT NULL DEFAULT 0,
  updated_at BIGINT NOT NULL
);

CREATE TABLE data_requests (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  kind TEXT NOT NULL,                            -- export / delete
  status TEXT NOT NULL DEFAULT 'pending',        -- pending / running / done / failed
  result_key TEXT,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);

-- 角色云端副本：不可变版本（发布即快照）
CREATE TABLE cloud_characters (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  local_card_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  card_json TEXT NOT NULL,
  rights_declaration TEXT NOT NULL,              -- original / public_domain_adaptation
  moderation TEXT NOT NULL DEFAULT 'pending',    -- pending / approved / quarantined / rejected
  withdrawn INTEGER NOT NULL DEFAULT 0,
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_cloud_characters_owner ON cloud_characters(owner_id);

CREATE TABLE audit_queue (
  id TEXT PRIMARY KEY,
  subject_kind TEXT NOT NULL,                    -- character / template / intervention / event
  subject_id TEXT NOT NULL,
  machine_verdict TEXT NOT NULL,
  machine_hits TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL DEFAULT 'open',           -- open / approved / rejected
  reviewer_id TEXT,
  reviewed_at BIGINT,
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_audit_queue_status ON audit_queue(status);

-- 世界模板与世界实例分离（平台规格 §9.5.C）
CREATE TABLE world_templates (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  room_type TEXT NOT NULL,                       -- idle / chapter / arena
  skeleton_json TEXT NOT NULL,                   -- 主线硬节点/结局池/隐藏内容池/装配规则（预审核内容池）
  admission_json TEXT NOT NULL DEFAULT '{"mode":"open"}',
  official INTEGER NOT NULL DEFAULT 1,
  version INTEGER NOT NULL DEFAULT 1,
  moderation TEXT NOT NULL DEFAULT 'approved',
  created_at BIGINT NOT NULL
);

CREATE TABLE worlds (
  id TEXT PRIMARY KEY,
  template_id TEXT NOT NULL,
  template_version INTEGER NOT NULL,             -- 版本钉住
  engine_version TEXT NOT NULL,
  prompt_set_version TEXT NOT NULL,
  model_route_version TEXT NOT NULL,
  room_type TEXT NOT NULL,
  title TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open',           -- open / running / paused / ended
  visibility TEXT NOT NULL DEFAULT 'private',    -- official / public / private
  host_user_id TEXT,
  member_limit INTEGER NOT NULL DEFAULT 10,
  tick_per_day INTEGER NOT NULL DEFAULT 3,
  assembled_json TEXT,                           -- 开局装配结果（实例派生内容，钉住）
  state_revision BIGINT NOT NULL DEFAULT 0,
  narrative_state_json TEXT NOT NULL DEFAULT '{}',
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
CREATE INDEX idx_worlds_status ON worlds(status);

CREATE TABLE world_members (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  cloud_character_id TEXT NOT NULL,
  boundary_json TEXT NOT NULL DEFAULT '{}',      -- 入场协议（同意边界）
  status TEXT NOT NULL DEFAULT 'active',         -- active / left / retired
  joined_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_world_member_unique ON world_members(world_id, cloud_character_id);
CREATE INDEX idx_world_members_user ON world_members(user_id);

CREATE TABLE world_ticks (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  tick_no BIGINT NOT NULL,
  base_revision BIGINT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',        -- pending / running / done / failed
  error TEXT,
  cost_tokens BIGINT NOT NULL DEFAULT 0,
  started_at BIGINT,
  finished_at BIGINT,
  created_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_world_tick_unique ON world_ticks(world_id, tick_no);

-- WorldEvent：投影后展示层（公共投影与私有投影分开存）
CREATE TABLE world_events (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  tick_no BIGINT NOT NULL,
  sequence BIGINT NOT NULL,
  domain_event_id TEXT NOT NULL,
  event_type TEXT NOT NULL,
  actors_json TEXT NOT NULL DEFAULT '[]',
  visibility TEXT NOT NULL,                      -- public / restricted / private
  audience_json TEXT,                            -- principal id 列表（非 public 必填）
  public_projection_json TEXT,
  private_projections_json TEXT,
  arbiter_note TEXT,
  moderation TEXT NOT NULL DEFAULT 'approved',
  ai_label INTEGER NOT NULL DEFAULT 1,
  occurred_at BIGINT NOT NULL
);
CREATE INDEX idx_world_events_world ON world_events(world_id, sequence);

CREATE TABLE interventions (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  character_id TEXT NOT NULL,                    -- cloud_character_id
  kind TEXT NOT NULL,                            -- whisper / item
  payload_json TEXT NOT NULL,
  expected_revision BIGINT NOT NULL,
  status TEXT NOT NULL DEFAULT 'accepted',       -- accepted / applied / rejected
  reject_reason TEXT,
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_interventions_world ON interventions(world_id, status);

CREATE TABLE consent_requests (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  event_kind TEXT NOT NULL,                      -- death / permanent_exit / severe_harm / relationship / adult
  subject_character_ids TEXT NOT NULL,
  detail TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',        -- pending / approved / declined / expired_conservative
  responses_json TEXT NOT NULL DEFAULT '{}',
  expires_at BIGINT NOT NULL,
  created_at BIGINT NOT NULL,
  resolved_at BIGINT
);

CREATE TABLE world_budgets (
  world_id TEXT PRIMARY KEY,
  daily_token_budget BIGINT NOT NULL,
  daily_cny_budget_cents BIGINT NOT NULL DEFAULT 0,
  spent_tokens_today BIGINT NOT NULL DEFAULT 0,
  budget_day TEXT NOT NULL DEFAULT '',
  fused INTEGER NOT NULL DEFAULT 0,
  updated_at BIGINT NOT NULL
);

CREATE TABLE prompt_versions (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,                           -- director / decide / arbiter / writer / critic / report
  version TEXT NOT NULL,
  content TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 0,
  canary_world_ids TEXT NOT NULL DEFAULT '[]',
  created_at BIGINT NOT NULL
);

CREATE TABLE model_routes (
  id TEXT PRIMARY KEY,
  version TEXT NOT NULL,
  routes_json TEXT NOT NULL,                     -- stage -> ModelProfile
  active INTEGER NOT NULL DEFAULT 0,
  created_at BIGINT NOT NULL
);

CREATE TABLE daily_reports (
  id TEXT PRIMARY KEY,
  world_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  character_id TEXT NOT NULL,
  report_day TEXT NOT NULL,                      -- YYYY-MM-DD
  content_json TEXT NOT NULL,
  opened_at BIGINT,
  created_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_daily_report_unique ON daily_reports(world_id, character_id, report_day);

CREATE TABLE notification_outbox (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',        -- pending / sent / failed / suppressed
  attempts INTEGER NOT NULL DEFAULT 0,
  dedupe_key TEXT,
  due_at BIGINT NOT NULL,
  sent_at BIGINT,
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_outbox_status ON notification_outbox(status, due_at);

CREATE TABLE risk_events (
  id TEXT PRIMARY KEY,
  user_id TEXT,
  world_id TEXT,
  kind TEXT NOT NULL,                            -- injection / forged_state / admission_bypass / abuse ...
  detail_json TEXT NOT NULL,
  created_at BIGINT NOT NULL
);

CREATE TABLE audit_logs (
  id TEXT PRIMARY KEY,
  actor_id TEXT NOT NULL,
  actor_role TEXT NOT NULL,
  action TEXT NOT NULL,
  subject TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL
);

-- P5：物品/背包/准入（平台规格 §9.5）
CREATE TABLE items (
  id TEXT PRIMARY KEY,
  narrative TEXT NOT NULL,
  effect_tags TEXT NOT NULL DEFAULT '[]',
  origin_world_template_id TEXT NOT NULL,
  cosmology_json TEXT NOT NULL DEFAULT '[]',
  power_tier INTEGER NOT NULL DEFAULT 1,
  created_at BIGINT NOT NULL
);

CREATE TABLE backpacks (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  acquired_world_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'owned',          -- owned / carried / sealed / consumed
  carried_world_id TEXT,
  acquired_at BIGINT NOT NULL
);
CREATE INDEX idx_backpacks_user ON backpacks(user_id);

-- 幂等键（副作用接口统一）
CREATE TABLE idempotency_keys (
  key TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  endpoint TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  response_json TEXT,
  created_at BIGINT NOT NULL
);

-- P4b 条件性：计费（代码在 feature=billing 后面，表先建好）
CREATE TABLE billing_balances (
  user_id TEXT PRIMARY KEY,
  balance_cents BIGINT NOT NULL DEFAULT 0,
  updated_at BIGINT NOT NULL
);

CREATE TABLE orders (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  kind TEXT NOT NULL,                            -- recharge / ticket / subscription
  amount_cents BIGINT NOT NULL,
  status TEXT NOT NULL DEFAULT 'created',        -- created / paid / fulfilled / refunded / failed
  external_ref TEXT,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);

CREATE TABLE ledger_entries (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  order_id TEXT,
  delta_cents BIGINT NOT NULL,
  reason TEXT NOT NULL,
  created_at BIGINT NOT NULL
);
