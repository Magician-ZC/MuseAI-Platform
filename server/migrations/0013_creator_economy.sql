-- MuseAI 平台库 0013：创作者经济 — 多账户复式账本（P0 账本地基，平台规格「账本模型」）。
--
-- 现状：0001/0006 的 billing 是「单账户 + 每次余额变动配一条 ledger 流水」的单边账
--   （不变量 SUM(ledger.delta)==billing_balances.balance）；绑定单一 user_id，无「科目/账户」维度，
--   无法表达「玩家钱包 → 创作者分成 + 平台抽成」这种一笔多向的资金流。
-- 本迁移引入复式账本（不破坏现表）：
--   账户 ledger_accounts + 复式凭证头 ledger_journals + 复式分录 ledger_postings。
--   同一 journal 内 SUM(postings.delta_cents)==0（有借必有贷）；账户余额 = 其所有 postings 之和（事务内物化）。
--
-- 资金红线（迁移落地为结构、代码 + 测试双守）：
--   ① 无提现出口：withdrawable 恒 0（user_wallet 永 0；creator_earnings 默认 0；平台账户内部）。
--   ② 有借必有贷：post_journal 断言 SUM==0，不平即回滚。
--   ③ 取整余数恒归平台：分成 floor(price*bps/10000)，余数入 platform_revenue，禁止凭空产分。
--   ④ user_wallet 不透支：charge 前置余额校验。
-- 兼容策略：保留 billing_balances 作为 user_wallet 的物化视图；充值/退款双写并与之恒等（对账测试守）。
--
-- 可移植：TEXT id / BIGINT 毫秒 / INTEGER 布尔，与 0001 一致；CREATE TABLE/INDEX、ADD COLUMN 双库通用。
-- 迁移不随 feature 门控（表无条件存在），default 构建亦安全运行（对齐 0006 billing 迁移）。

-- 账户表：每个用户一个 user_wallet；创作者按需一个 creator_earnings；平台账户固定几条。
-- id 采用确定性命名（acct_wallet_{uid} / acct_creator_{uid} / acct_platform_revenue /
-- acct_platform_recharge_source / acct_escrow_{worldId}），同 (kind, owner, scope) 恒映射同一账户，天然幂等。
CREATE TABLE ledger_accounts (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,                        -- user_wallet / creator_earnings / platform_revenue / platform_recharge_source / escrow
  owner_id TEXT,                             -- user_wallet/creator_earnings 的归属用户；平台账户 NULL
  scope_id TEXT,                             -- escrow 的 world_id 等；否则 NULL
  balance_cents BIGINT NOT NULL DEFAULT 0,   -- 物化余额（= SUM(postings)，事务内维护）
  withdrawable INTEGER NOT NULL DEFAULT 0,   -- 红线开关：是否允许进结算/提现管线。首版恒 0（无任何提现出口）
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_ledger_accounts_kind_owner ON ledger_accounts(kind, owner_id, scope_id);

-- 复式凭证头：一笔业务一个 journal，其下 postings 和恒为 0。
CREATE TABLE ledger_journals (
  id TEXT PRIMARY KEY,
  reason TEXT NOT NULL,                       -- recharge / refund / gift / room_open / revive / cloud_growth / share_payout ...
  ref_kind TEXT,                             -- order / gift_event / revive_grant / world ...
  ref_id TEXT,                               -- 关联业务主键（对账/审计）
  world_id TEXT,                             -- 分成溯源（可空）
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_ledger_journals_ref ON ledger_journals(ref_kind, ref_id);

-- 复式分录：每条挂账户 + 金额（正=入账/负=出账），同 journal 内 SUM=0。
CREATE TABLE ledger_postings (
  id TEXT PRIMARY KEY,
  journal_id TEXT NOT NULL,
  account_id TEXT NOT NULL,
  delta_cents BIGINT NOT NULL,
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_ledger_postings_account ON ledger_postings(account_id);
CREATE INDEX idx_ledger_postings_journal ON ledger_postings(journal_id);

-- 分成率（basis points，避免浮点）；NULL → 用全局默认（ledger::DEFAULT_REVENUE_SHARE_BPS=7000，创作者 70%）。
ALTER TABLE world_templates ADD COLUMN revenue_share_bps INTEGER;

-- 礼物 SKU 定价（分）：站内打赏（POST /arena/{worldId}/gift）扣费金额，gift 总价 = price_cents × count。
-- 默认 0（P1）：未定价 / 未知 SKU 走 charge 免费 no-op（price==0 不产 journal，保留免费打赏），
--   dev 播种 SKU（0008 的 rose/rocket/crown/shield）保持免费，避免破坏既有 livegate 测试。
-- 外部直播 webhook（via='livegate'）**不读此列**：观众已在直播平台付费，站内仅记账不二次扣钱包（红线）。
ALTER TABLE gift_sku_map ADD COLUMN price_cents BIGINT NOT NULL DEFAULT 0;
