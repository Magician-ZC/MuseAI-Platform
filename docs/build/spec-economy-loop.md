Investigation complete. Returning the full spec.

# 经济系统闭环 — 详细实现规格（MuseAI-Platform / server 侧）

> 全部基于真实代码核对；行号为核对当时 `main@a788259` 的位置。核心结论前置：**当前"双记账"其实是"单账户 + 每次余额变动配一条 ledger 行"的单边流水（不变量 `SUM(ledger)==balance`）**，不是含借贷科目的复式记账。要支撑"创作者分成 + 平台抽成"的可审计闭环，必须把 ledger 升级为**多账户复式账本**。这是本规格的中心改动。

---

## 概述

现状（已落地，真实代码）：

- **充值/退款/余额**：`server/src/billing/mod.rs` 已实现 `POST /billing/orders`(recharge)、`GET /billing/balance`、`POST /billing/refunds`，单事务写 `orders`+`ledger_entries`+`billing_balances`，幂等（`idempotency::guard`），未成年拒充（`age_declared==1` 才放行，mod.rs:121-127），**路由层不存在 withdraw/transfer/cashout**（mod.rs:38-44，测试 tests.rs:330-342 断言 404）。
- **付费点（未接扣费）**：
  - 观众打赏 `POST /arena/{worldId}/gift`（`livegate/mod.rs:186` `spectator_gift`）+ 外部直播 webhook（同文件:53），共用 `apply_gift`（:106），只写 `arena_env_events`+`gift_events`，**扣费是 TODO seam**（:185 "实际扣费 `billing::charge` 跨 feature，本期先记账不扣费"）。
  - 复活赛资格 `POST /arena/{worldId}/revive-match`（`arena/mod.rs:433`），只写 `arena_revive_grants(status='eligible')`，**扣费 TODO seam**（:461），boundary 显式 `buys=revive_eligibility, notImmunity, notFinalVerdict`（:482）。
- **道具单一写入**：`backpack/mod.rs:81` `grant_item_tx` 是唯一写入路径（§9.6：tick 结算 or 支付履约），`reward_hook_key` 唯一键幂等（:127）。玩家间无转移接口；`carry`（:252）只做归属+准入校验，携带非本人物品记 `forged_state` 风控（:289）并整单拒绝。
- **创作者资产**：`assets/worlds.rs` 创作者发布 `world_templates(official=0, owner_id=发布者)`（:381-384），走机审+版本+撤回。**玩家在其世界消费无任何回流** —— 分成完全缺失。
- **成本熔断**：`world_budgets`（0001:196-204，`daily_token_budget`/`daily_cny_budget_cents`/`spent_tokens_today`/`fused`），runtime 熔断在 `runtime/mod.rs:940`。这是**平台成本侧**，与用户资金账本是两套账，不要混。

目标闭环：**充值 → 钱包（复式账本）→ 消费(开房/打赏/复活/角色云成长) → charge 单事务扣钱包 + 记账 → 按世界模板 owner 分账入创作者账 + 平台抽成 → 全程无提现出口（创作者提现单独隔离为受牌照约束的结算管线，默认关闭）**。

---

## 账本模型（双记账 schema + 分录规则）

### 现状与缺口

现 `ledger_entries`（0001:319-326）：`id, user_id, order_id, delta_cents, reason, created_at`。它绑定单一 `user_id`，`billing_balances` 是唯一余额表，不变量 `SUM(ledger.delta)==balance`（tests.rs:122）。**没有"科目/账户"维度**，无法表达"钱从玩家钱包 → 创作者钱包 + 平台收入"这种一笔多向的资金流。

### 目标：多账户复式账本（新增，不破坏现表）

引入**账户（accounts）**与**复式分录（journal + entries）**，每笔业务生成一个 journal，其下 ≥2 条 entries，**同一 journal 内 `SUM(amount_cents)==0`**（有借必有贷）。账户余额 = 该账户所有 entries 之和。

账户类型（`account_kind`）：

| kind | 说明 | 提现 |
|---|---|---|
| `user_wallet:{uid}` | 用户可消费余额（充值入口，等价现 `billing_balances`） | 否（红线） |
| `creator_earnings:{uid}` | 创作者分成账（站内可消费，默认不可提现） | 默认否，见合规章 |
| `platform_revenue` | 平台抽成收入（内部账，单向汇集） | N/A（内部） |
| `platform_recharge_source` | 充值对手方（外部支付履约的记账对手，代表"外部资金入账"） | N/A |
| `escrow:{worldId}` | 可选：结果未定的消费先入托管，settle 时再拆分成/退回（买过程不买结果的资金侧保障） | N/A |

分录规则（示例，金额单位 cent）：

```
充值 amount:
  DR platform_recharge_source  -amount    (外部资金源，负=流出到用户)
  CR user_wallet:{uid}         +amount
  ⇒ 用户钱包 +amount；journal 和为 0

打赏/开房消费 price（世界模板 owner=creator, 分成率 r, 抽成 1-r）:
  DR user_wallet:{uid}         -price
  CR creator_earnings:{owner}  +round(price * r)
  CR platform_revenue          +(price - round(price*r))
  ⇒ 三条 entries 和为 0（取整余数归平台，避免丢分）

复活/角色云成长（平台服务，无创作者对手方）:
  DR user_wallet:{uid}         -price
  CR platform_revenue          +price

退款（现 recharge 退款，逆向）:
  DR user_wallet:{uid}         -amount
  CR platform_recharge_source  +amount
```

**取整规则（资金红线）**：分成 `round(price*r)`，余数（`price - 分成`）永远归 `platform_revenue`，**禁止任何路径产生凭空分**——`SUM(entries)==0` 是硬约束，用 CHECK/测试双保。`r` 存 `world_templates` 或全局配置（basis points，如 `revenue_share_bps=7000` 表示创作者 70%），避免浮点。

### 与现表的兼容策略（渐进）

- **保留 `billing_balances` 作为 `user_wallet` 的物化视图**：现 recharge/refund/balance 代码继续读写它；`user_wallet` 账户余额必须与 `billing_balances.balance_cents` 恒等（迁移期双写 + 对账测试）。
- 现 `ledger_entries` 保留（充值/退款审计链不断），新增复式账本表并列，**新付费点只走新账本**，充值这条也补写新账本的 `user_wallet` + `platform_recharge_source` 两条 entries。终态：`ledger_entries` 视为 `user_wallet` 账户的投影。

---

## 数据结构

新增迁移 `0013_creator_economy.sql`（可移植子集：TEXT id / BIGINT ms / INTEGER 布尔，与 0001 一致）：

```sql
-- 账户表：每个用户一个 user_wallet；创作者按需一个 creator_earnings；平台账户固定几条。
CREATE TABLE ledger_accounts (
  id TEXT PRIMARY KEY,                 -- 如 "acct_wallet_{uid}" / "acct_creator_{uid}" / "acct_platform_revenue"
  kind TEXT NOT NULL,                  -- user_wallet / creator_earnings / platform_revenue / platform_recharge_source / escrow
  owner_id TEXT,                       -- user_wallet/creator_earnings 的归属用户；平台账户 NULL
  scope_id TEXT,                       -- escrow 的 world_id 等
  balance_cents BIGINT NOT NULL DEFAULT 0,  -- 物化余额（= SUM(entries)，事务内维护）
  withdrawable INTEGER NOT NULL DEFAULT 0,  -- 是否允许进入结算管线（红线开关；user_wallet 恒 0）
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX idx_ledger_accounts_kind_owner ON ledger_accounts(kind, owner_id, scope_id);

-- 复式凭证头：一笔业务一个 journal，其下 entries 和恒为 0。
CREATE TABLE ledger_journals (
  id TEXT PRIMARY KEY,
  reason TEXT NOT NULL,                -- recharge / gift / room_open / revive / cloud_growth / refund / share_payout ...
  ref_kind TEXT,                       -- order / gift_event / revive_grant / world ...
  ref_id TEXT,                         -- 关联业务主键（对账/审计）
  world_id TEXT,                       -- 分成溯源（可空）
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_ledger_journals_ref ON ledger_journals(ref_kind, ref_id);

-- 复式分录：每条挂账户 + 金额（正/负），同 journal 内 SUM=0。
CREATE TABLE ledger_postings (
  id TEXT PRIMARY KEY,
  journal_id TEXT NOT NULL,
  account_id TEXT NOT NULL,
  delta_cents BIGINT NOT NULL,         -- 正=入账，负=出账
  created_at BIGINT NOT NULL
);
CREATE INDEX idx_ledger_postings_account ON ledger_postings(account_id);
CREATE INDEX idx_ledger_postings_journal ON ledger_postings(journal_id);

-- 分成率配置（basis points，避免浮点）；缺省走全局 default。
ALTER TABLE world_templates ADD COLUMN revenue_share_bps INTEGER;   -- NULL → 用全局默认（如 7000）
```

约束/不变量（代码 + 测试守）：

- 每 journal `SUM(postings.delta_cents)==0`（`post_journal()` 内断言 + 集成测试 + 定期对账 job）。
- 每 account `balance_cents == SUM(postings.delta WHERE account_id=..)`。
- `user_wallet.balance_cents == billing_balances.balance_cents`（迁移期恒等）。
- `user_wallet.balance_cents >= 0`（charge 前置校验，余额不足拒付，不透支）。

核心 Rust API（新增 `server/src/ledger/mod.rs`，跨 feature 复用）：

```rust
// 统一记账入口：在调用方已开启的事务内执行，保证与业务副作用原子。
pub struct Posting { pub account: AccountRef, pub delta_cents: i64 }
pub async fn post_journal(
    tx: &mut Transaction<'_, Any>,
    reason: &str, ref_kind: &str, ref_id: &str, world_id: Option<&str>,
    postings: &[Posting],          // 断言 SUM==0，否则 Err（内部错误，绝不外泄不平账本）
) -> Result<String /*journal_id*/, ApiError>;

// 统一扣费：校验 user_wallet 余额充足 → 扣钱包 → 分账（创作者+平台）→ post_journal，全在传入 tx 内。
pub async fn charge(
    tx: &mut Transaction<'_, Any>,
    user_id: &str, price_cents: i64,
    reason: &str, ref_kind: &str, ref_id: &str,
    world_id: Option<&str>,        // Some → 查 template.owner_id 分成；None → 全额入 platform_revenue
) -> Result<ChargeReceipt, ApiError>;  // 余额不足 → ApiError::Conflict("insufficient_balance")
```

`charge()` 是所有付费点的唯一扣费口，**红线守护集中在此一处**（余额校验、分成拆分、取整归平台、SUM=0）。

---

## 创作者分成

**溯源链**：消费发生在某 `world`（`worlds.template_id` → 0001:103）→ `world_templates.owner_id`（0011:8）= 创作者。**注意 `worlds.host_user_id`（房主，0001:112）≠ 创作者**，分成必须认 `template.owner_id`，不是房主。官方模板 `owner_id IS NULL`（0011:2）→ 无分成对手方，全额入 `platform_revenue`。

分成触发点（谁付谁得）：

| 付费点 | 付费方 | 分成对手方 | reason |
|---|---|---|---|
| 观众打赏 gift | 观众(登录用户) | 该世界模板 owner | `gift` |
| 开房费（新增，见下） | 建房者/房主 | 模板 owner | `room_open` |
| 复活资格 revive | 观众/角色主人 | **建议不分成**（平台服务，避免"付费改结果"观感） | `revive` |
| 角色云成长 | 角色主人 | **不分成**（平台云存储/成长位服务） | `cloud_growth` |

规则：

- `charge(world_id=Some)` 时，若 `template.owner_id` 存在且 `owner_id != 付费用户`（禁自打赏套利）→ 拆 `creator_earnings:{owner}` + `platform_revenue`；否则全额 `platform_revenue`。
- **自打赏防刷**：付费用户 == 模板 owner 时分成率归零（全额入平台），否则创作者可给自己刷分成再想办法套现。集中在 `charge()` 判定。
- 分成落 `creator_earnings` 账户，**站内可消费**（可作为 charge 的资金来源之一，等价"用赚的钱开房/打赏"），**默认 `withdrawable=0`**（见合规章）。
- 创作者查询：新增 `GET /me/earnings`（AuthUser）→ 读 `creator_earnings:{uid}` 余额 + 明细（join `ledger_journals` 溯源 world/reason），owner 隔离（只出本人）。

---

## 付费点整合

统一形态：**副作用端点 = `idempotency::guard` → 开事务 → `ledger::charge(tx, ...)` → 业务写入 → 提交**。charge 与业务副作用同一事务，要么全成要么全无（对齐现 billing 的单事务模式 mod.rs:149-201）。

### 1) 打赏 gift（`livegate/mod.rs:186` spectator_gift）

- 现状：`apply_gift`(:106) 只写 `arena_env_events`+`gift_events`，seam :185 明说未扣费。
- 改：在 `spectator_gift` 内，`apply_gift` 前先在事务里 `charge(user, sku_price, "gift", "gift_event", <gift_id>, world_id=Some)`。SKU 价格从 `gift_sku_map` 扩列 `price_cents` 读取。余额不足 → 409，不写任何 gift/env 事件。
- 红线不变：boon 仍只映射 `advantage/reroll/info` 过程增益（:150 boundary），**charge 不改变** `apply_gift` 只写系统频道、不碰 `eliminations/winner/interventions` 的铁律（:103-105）。
- 外部 webhook（:53）路径：观众在直播平台已付费，站内不二次扣钱包；仍记 `gift_events(via='livegate')`，分成结算走**对账批处理**（按 via 区分，:272 record_gift 已留 via 供分成）——外部资金入账 → `platform_recharge_source` → 拆创作者/平台，异步 job 落 journal。

### 2) 复活资格 revive（`arena/mod.rs:433`）

- 现状：只写 `arena_revive_grants(eligible)`，seam :461 未扣费。boundary :482 已诚实标注 `buys=revive_eligibility`。
- 改：写 grant 前 `charge(user, revive_price, "revive", "revive_grant", <grant_id>, world_id=None)`（不分成，平台服务）。
- **红线守护（最关键）**：charge 成功 ≠ 免死。仍只写 `status='eligible'`，**绝不**设免死/复活落定标志、**绝不**碰 `eliminations/winner`（:462 注释已在，保持）。买的是"进复活赛的资格（过程）"，结果由赛制/同意门控决定。

### 3) 开房费（新增付费点）

- 现 `create_world`（`worlds/mod.rs:463`）是内部函数（admin S6 + 房主建房复用，:461）。为房主建房加一个 HTTP 端点 `POST /worlds`（AuthUser），在其中：`age_declared` 非硬门（建房不等于充值，但消费余额需先有钱）→ `charge(host, room_open_price, "room_open", "world", <world_id>, world_id=Some(该世界))` → `create_world`，同事务。分成给模板 owner（房主用别人模板建房，创作者得分成）。
- 免费房：`room_open_price==0` 时 charge 直接 no-op（不产 journal），保留免费开房能力。

### 4) 角色云成长（新增：云存储/跨世界成长位）

- 语义：角色 `cloud_characters`（0001:62）本身是不可变快照；"成长位/跨世界携带槽"是平台增值服务（更多云角色位、更多同时在场世界数、背包容量等**平台配额**，非战力）。
- `charge(user, growth_price, "cloud_growth", "cloud_character", <cid>, world_id=None)`，全额入平台。落一张 `user_entitlements`（配额表，新增）记生效额度。
- **红线**：只买"容量/位"（过程/服务），**不买战力、不买胜负**。荣誉/战力仍由引擎评估（对齐"荣誉非战力"红线）。

---

## 道具经济与流转红线

- **单一写入路径不变**：`grant_item_tx`（`backpack/mod.rs:81`）仍是唯一入包口，§9.6 两条合法来源——tick 结算 / 支付履约（:79-80 注释）。若做"付费购买道具"，走 `charge()` + 在**同事务**内调 `grant_item_tx`（pool 版 :136 已注明"供支付履约调用"），`reward_hook_key` 用订单号做幂等键（:127 唯一键防重复发货）。
- **禁止玩家间道具交易/转移（红线）**：不新增任何 `transfer_item`/`sell_item`/挂售端点。理由：道具可交易 = 变相二级市场 = 变相提现/洗钱通道。`carry`（:252）只在自己账户内做归属+准入，携带非本人物品直接 `forged_state` 风控（:289）。保持。
- **道具不可回购成余额**：无"卖道具换 cent"路径（否则 = 提现出口）。道具只能被消耗（`status='consumed'`，:155 查询已排除）或封存（sealed）。
- 若未来要"道具市场"，必须是**平台单向售卖**（平台→玩家，走 charge+grant），**绝不**玩家→玩家结算。这条写死在设计里。

---

## 未成年与提现合规（关键）

### 未成年（已落地，扩展到新付费点）

- 现 `age_declared`（users, 0001:9；0=未声明/1=成年/2=未成年）。充值 gate 在 `billing/mod.rs:121-127`：**仅 `Some((1,))` 放行**，未声明(0)/未成年(2)/无行一律 403（保守拒充，堵"仅拦 2"空防）。声明入口 `POST /auth/age-declaration`（auth/mod.rs:460）。
- **消费侧无需重复年龄门**：因为余额只能来自充值，充值已 gate → 未成年账户余额恒为 0 → charge 必然余额不足 409。但**分成入账（creator_earnings）**是个绕过口：未成年若能当创作者收分成，就有了未经充值的余额。**处置**：未成年（age_declared==2 或未声明）不得成为创作者收款方——`charge` 分成拆分时，若 `owner` 的 `age_declared != 1`，分成额改入 `platform_revenue`（挂账/冻结待成年补实名），不进未成年 creator 账。集中在 `charge()`。

### 提现（红线核心，务必讲透）

**用户钱包（user_wallet）绝对不可提现/转账** —— 现 billing 路由层零 withdraw/transfer（mod.rs:43 注释 + tests.rs:330 断言）。这条不动。

**创作者分成能否提现 = 决定是否触碰支付/资金结算牌照的分水岭：**

1. **法律定性**：一旦"用户充值的钱 → 经平台 → 打给另一个自然人并可提现到银行卡"，平台就是在做**资金归集与代付/结算**，在中国大陆需要《支付业务许可证》（第三方支付牌照）或与持牌机构合作的**分账/结算**方案（如银行/持牌支付的"平台商户分账"）。无牌自建提现 = 无证经营支付业务 + 潜在非法集资/洗钱风险。
2. **默认设计（本规格采纳，安全优先）**：
   - `creator_earnings` 账户 `withdrawable=0`，**只能站内消费**（作为 charge 的资金来源，用于开房/打赏/云成长）。这样分成是"平台内权益/积分"，**不是可兑付资金**，不触发结算牌照。**不提供任何 payout 端点**（对齐 `providers::PaymentProvider` 现只有 `create_order`，无 payout 方法，providers/mod.rs:67-70）。
   - 明确产品文案："创作者收益为站内可消费权益，不可提现"——诚实标注，避免被认定为承诺兑付。
3. **若确要开创作者提现（后续，需前置条件，不在首版）**：
   - 必须走**持牌结算通道**：接入持牌支付/银行的平台分账 API，平台**不自持资金池**（资金在持牌机构备付金账户），提现由持牌方代付。新增 `PayoutProvider` trait（与 `PaymentProvider` 并列），Dev 实现仅日志，生产=持牌接入。
   - 创作者**强实名**（`identity_verification_refs` 已有表，0001:34）+ 签署结算/代扣代缴协议；提现按**稿酬/劳务报酬**代扣个税，开票/代征。定性为"平台向创作者采购内容服务的报酬"，**不是用户对用户转账**。
   - 提现独立为受控管线：`creator_earnings` → 冻结/审核（`finance` 角色已在 users.role 枚举，0001:10）→ 持牌代付 → journal（`share_payout`：DR creator_earnings / CR platform_settlement_clearing）。反洗钱：单笔/日/月限额、可疑交易上报、留痕。
   - **强隔离**：提现能力挂 feature flag（如 `feature=payout`），默认编译不进，与 `billing` 一样条件装配（app.rs:56 模式）。无牌照前该 feature 永不开启。

**红线总结**：user_wallet 永不提现；creator_earnings 默认站内消费不可提现；提现若做必须持牌+实名+代扣税+独立 feature，且平台不碰资金池。

---

## 改动文件清单

新增：

- `server/migrations/0013_creator_economy.sql` — `ledger_accounts` / `ledger_journals` / `ledger_postings` / `world_templates.revenue_share_bps` / `gift_sku_map.price_cents` / `user_entitlements`。
- `server/src/ledger/mod.rs` — `post_journal()` / `charge()` / 账户 upsert / 分成拆分 / SUM=0 守卫。核心红线集中地。
- `server/src/ledger/tests.rs` — 复式恒等/分成/取整/余额不足/自打赏/未成年分成挂账。

改动：

- `server/src/app.rs`（:49-57）— 挂 `ledger` 模块；`GET /me/earnings`；（未来 payout feature 条件 merge）。
- `server/src/billing/mod.rs`（:149-201, :256-285）— 充值/退款补写新复式账本（`user_wallet` + `platform_recharge_source`），与 `billing_balances`/`ledger_entries` 双写对账；不动路由（仍无提现）。
- `server/src/livegate/mod.rs`（:186 spectator_gift）— charge seam 落地（:185 TODO）；`apply_gift`(:106) 前扣费+分成；webhook(:53) 外部资金对账 job。
- `server/src/arena/mod.rs`（:433 revive_match，:461 TODO）— charge seam 落地，不分成，红线注释保持。
- `server/src/worlds/mod.rs`（:463 create_world）— 新增 `POST /worlds` 房主建房端点，开房费 charge + 分成给模板 owner。
- `server/src/backpack/mod.rs`（:136 grant_item）— 付费购道具时 charge + 同事务 grant_item_tx；无玩家间转移（保持）。
- `server/src/providers/mod.rs`（:67）— （未来）新增 `PayoutProvider` trait + `DevPayout`，默认不装配。
- `server/migrations` — `gift_sku_map` 建表若未含 price 则补列；`user_entitlements` 云成长配额。

---

## 红线守护点（逐条对应）

1. **无提现/转账**：billing 路由不注册 withdraw/transfer/cashout（mod.rs:43，tests.rs:330 已断言）；`user_wallet.withdrawable` 恒 0；`creator_earnings` 默认 0；无 `PayoutProvider` 装配。
2. **买过程不买结果**：gift boon 仅 advantage/reroll/info（livegate:150 boundary）；revive 仅 `eligible`（arena:462 不碰 eliminations/winner）；charge 成功不设任何结果标志。boundary 字段（`buys/notImmunity/notFinalVerdict`）诚实标注保留。
3. **未成年拒充**：`age_declared==1` 才放行（billing:125）；扩展——未成年不得作分成收款方（charge 内改挂 platform）。
4. **道具单一写入**：`grant_item_tx`（backpack:81）唯一路径不变；付费购道具复用它 + `reward_hook_key` 幂等（:127）；禁玩家间交易。
5. **荣誉非战力 / AI 标注**：经济系统只碰"配额/过程增益/服务位"，不碰战力评估与胜负；`world_events.ai_label`（0001:164 默认 1）不受经济改动影响，AI 生成内容标注链不变。
6. **账本不平即拒**：`post_journal` SUM≠0 → 内部错误回滚，绝不外泄不平账本；取整余数恒归平台。
7. **自打赏防套利**：付费方==模板 owner 时分成归零。

---

## 测试点

- **复式恒等**：任意付费序列后，每 journal `SUM(postings)==0`；每 account `balance==SUM(postings)`；`user_wallet==billing_balances`。
- **分成拆分**：gift/room_open 后 `creator_earnings+platform_revenue==price`；`round(price*r)` 取整余数入平台；`r` 从 `revenue_share_bps` 读，官方模板(owner NULL)全额平台。
- **自打赏**：owner 给自己世界打赏 → 分成 0，全额平台。
- **余额不足**：钱包 < price → charge 返回 409，**零副作用**（无 grant/gift/revive/world 写入，对齐 billing 拒充零副作用 tests.rs:239-243 模式）。
- **未成年**：未成年充值 403（已有 tests.rs:231）；未成年作创作者收款 → 分成挂 platform 不进其账。
- **revive 红线**：charge 成功后 grant 仍 `eligible`，`arena_matches` 的 eliminations/winner 不变。
- **gift 红线**：charge 后 `apply_gift` 只写 `arena_env_events`+`gift_events`，interventions 表零写入。
- **幂等**：各付费点同 `Idempotency-Key` 重投不双扣（复用 `idempotency::guard`，对齐 tests.rs:155）。
- **无提现端点**：扩展 tests.rs:330 覆盖 `/api/creator/withdraw`、`/api/me/earnings/withdraw` 等 → 404。
- **道具无转移**：无 transfer/sell 端点 → 404；carry 非本人物品 → RiskBlocked（backpack:297）。
- **退款一致**：退款后新复式账本与 `billing_balances`/`ledger_entries` 三者恒等。

---

## 渐进落地

- **P0（账本地基）**：加 `0013` 迁移 + `ledger/mod.rs`（`post_journal`/`charge`/账户 upsert）；充值/退款双写新账本并对账。不改任何付费点行为。CI 三样（npm test / build / cargo test）过。
- **P1（打赏扣费）**：`spectator_gift` 接 `charge`（:185 seam），`gift_sku_map` 加价，站内打赏走钱包+分成；外部 webhook 仅记账，分成对账 job 挂后。
- **P2（复活+开房）**：revive `charge` 不分成（:461 seam）；新增 `POST /worlds` 房主建房 + 开房费分成。
- **P3（云成长+道具售卖）**：`user_entitlements` + cloud_growth charge；付费购道具复用 `grant_item_tx`。`GET /me/earnings`。
- **P4（合规增强，非提现）**：未成年分成挂账、`finance` 角色对账后台、可疑交易留痕。
- **P5（提现，仅在获牌后）**：`PayoutProvider` + 强实名 + 代扣税 + `feature=payout`，独立评审。默认永不开启。

---

## 风险（尤其资金/牌照/合规）

1. **【最高】创作者提现 = 支付/结算牌照**：任何"分成可兑付到银行卡"的实现都可能构成无证支付业务。首版**必须**把 `creator_earnings` 锁为站内不可提现权益；提现留到持牌通道 + feature 隔离。误开即重大合规事故。
2. **资金池风险**：平台若归集用户充值资金并代付创作者，即自持资金池——非法集资/挪用备付金风险。设计上平台不持池，充值资金定性为"预付卡/虚拟货币充值"（受《单用途/多用途商业预付卡》规范约束，需备案/限额/不可退现规则），提现走持牌机构备付金。
3. **虚拟货币充值合规**：user_wallet 是预付充值余额，涉及预付卡管理办法（实名、限额、不可折现、有效期/退款规则）。现 `MAX_RECHARGE_CENTS=10万元`（billing:36）偏高，建议按合规下调 + 分级限额。退款政策要与"不可提现"自洽（现 refund 只退回钱包 mod.rs:271，不出金，合规）。
4. **道具/分成变相提现**：道具可交易或分成可兑付都会变成洗钱/套现通道。红线：道具平台单向售卖、不可回购、禁玩家间交易；分成站内消费。
5. **未成年绕过**：分成/道具赠予/外部打赏都可能给未成年账户注入未经充值的价值。已处置分成挂账；需同步审计 grant_item 是否会给未成年发可变现物（道具不可变现即安全）。
6. **税务**：若开创作者提现，涉及个税代扣代缴、发票/代征、平台经济个体户/劳务报酬定性。无税务方案不得开提现。
7. **取整/并发一致性**：分成取整余数处理不当会累积丢分或造钱；`SUM=0` 硬约束 + 余数归平台 + 并发下账户余额更新需行锁（Postgres `FOR UPDATE`，参考 billing refund 的 `UPDATE ... SET status=status` 串行化技巧 mod.rs:239）。
8. **外部直播打赏对账**：webhook 资金在直播平台侧，站内只映射增益；分成结算依赖对账批处理，存在对账缺口/延迟风险，需 `gift_events.via`（0012:11）分流 + 定期核对，避免重复/漏分成。
9. **反洗钱/风控**：打赏刷分成、自建世界自打赏套利、异常大额充值。已加自打赏归零；需补单用户/单世界打赏限额、异常监测（`safety::record_risk` 已有 backpack:289 可复用）。
