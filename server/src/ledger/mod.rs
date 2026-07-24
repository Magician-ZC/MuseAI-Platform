//! 复式账本（P0 账本地基，平台规格「账本模型 / 数据结构」）。
//!
//! feature 与 billing/arena 一致（`any(billing, arena)`）：billing 充值/退款双写这里，付费点（gift/revive/
//! room_open/cloud_growth，feature=arena/后续）统一走 `charge()`。所有资金流的**唯一记账口在此模块**。
//!
//! 资金红线（集中守护，测试锁死）：
//! - **有借必有贷**：`post_journal` 断言同 journal `SUM(postings.delta_cents)==0`，不平即内部错误回滚，
//!   绝不外泄不平账本、绝不凭空产分。
//! - **无提现出口**：`ledger_accounts.withdrawable` 恒 0（`ensure_account` 硬编码），首版无任何 payout。
//! - **不透支**：`charge` 前置校验 user_wallet（= billing_balances 物化视图）余额充足，不足 → 409 零副作用。
//! - **取整余数归平台**：分成 `floor(price*bps/10000)`，余数（price − 分成）恒入 `platform_revenue`。
//! - **自打赏防刷**：付费方 == 模板 owner → 分成归零（全额平台）。
//! - **未成年不得当创作者收款方**：owner `age_declared != 1` → 分成挂平台（不注入未经充值的余额）。
//! - **买过程不买结果**：本模块只动账，不碰任何仲裁/胜负/免死标志（由各付费点调用方保证）。

use sqlx::{Any, Transaction};

use crate::db::{new_id, now_ms};
use crate::error::ApiError;

/// 全局默认分成率（basis points）：创作者 70%。`world_templates.revenue_share_bps` 为 NULL 时用此。
/// P0 仅测试与后续付费点（P1+）消费 `charge`；prod 尚无调用者，对齐 backpack::grant_item 的 allow(dead_code) 约定。
#[allow(dead_code)]
pub const DEFAULT_REVENUE_SHARE_BPS: i64 = 7000;

/// AML 可疑大额单笔消费阈值（分）：单笔 `charge` 超此额留痕（异常大额监测，非拦截）。
/// 参考 billing `MAX_RECHARGE_CENTS=10万元`，取其 5%（5000 元）为「异常大额消费」信号阈值——
/// 只留痕供 finance 核查，**不改扣费结果**（红线：留痕≠拦截，避免误伤正常大额消费）。
const SUSPICIOUS_CHARGE_THRESHOLD_CENTS: i64 = 500_000;

/// 复式账本内部错误：不平账本绝不外泄给客户端（映射 `ApiError::Internal` → 500，事务回滚）。
#[derive(Debug, thiserror::Error)]
enum LedgerError {
    #[error("复式分录不平衡：SUM(postings)={0}，必须为 0（账本红线：有借必有贷）")]
    Unbalanced(i64),
    #[error("复式凭证至少需 2 条分录（有借必有贷）")]
    TooFewPostings,
}

/// 账户引用：确定性定位/创建一个账户。id 由 (kind, owner) 唯一决定，天然幂等去重。
#[derive(Debug, Clone)]
pub enum AccountRef {
    /// 用户可消费钱包（充值入口，等价 `billing_balances` 物化视图）。红线：永不可提现。
    UserWallet(String),
    /// 创作者分成账（站内可消费，默认不可提现）。
    CreatorEarnings(String),
    /// 平台抽成收入（内部账，单向汇集）。
    PlatformRevenue,
    /// 充值对手方（外部支付履约的记账对手，代表外部资金入账）。
    PlatformRechargeSource,
}

impl AccountRef {
    fn kind(&self) -> &'static str {
        match self {
            AccountRef::UserWallet(_) => "user_wallet",
            AccountRef::CreatorEarnings(_) => "creator_earnings",
            AccountRef::PlatformRevenue => "platform_revenue",
            AccountRef::PlatformRechargeSource => "platform_recharge_source",
        }
    }
    fn owner_id(&self) -> Option<&str> {
        match self {
            AccountRef::UserWallet(u) | AccountRef::CreatorEarnings(u) => Some(u.as_str()),
            AccountRef::PlatformRevenue | AccountRef::PlatformRechargeSource => None,
        }
    }
    /// 确定性账户 id：同 (kind, owner) 永远映射同一账户，upsert 天然幂等。
    fn account_id(&self) -> String {
        match self {
            AccountRef::UserWallet(u) => format!("acct_wallet_{u}"),
            AccountRef::CreatorEarnings(u) => format!("acct_creator_{u}"),
            AccountRef::PlatformRevenue => "acct_platform_revenue".to_string(),
            AccountRef::PlatformRechargeSource => "acct_platform_recharge_source".to_string(),
        }
    }
}

/// 一条复式分录：账户 + 金额（正=入账 / 负=出账）。
pub struct Posting {
    pub account: AccountRef,
    pub delta_cents: i64,
}

/// `charge` 回执。
#[allow(dead_code)]
#[derive(Debug)]
pub struct ChargeReceipt {
    /// 实际入账的凭证 id；免费（price==0）no-op 时为 None（不产 journal）。
    pub journal_id: Option<String>,
    /// 分账给创作者的金额（自打赏/未成年 owner/官方模板/无世界/世界不存在 → 0）。
    pub creator_earnings_cents: i64,
    /// 归平台的金额（含取整余数）。
    pub platform_revenue_cents: i64,
    /// 实际扣款（== price_cents；免费时 0）。
    pub charged_cents: i64,
}

/// upsert 账户行（确定性 id + `ON CONFLICT DO NOTHING` 幂等），返回账户 id。
/// **红线**：`withdrawable` 恒 0（首版无任何提现出口）。
async fn ensure_account(tx: &mut Transaction<'_, Any>, account: &AccountRef) -> Result<String, ApiError> {
    let id = account.account_id();
    let now = now_ms();
    // 双库通用 upsert：命中主键即跳过（账户一经创建余额由 postings 维护，DO NOTHING 不覆盖）。
    sqlx::query(
        "INSERT INTO ledger_accounts (id, kind, owner_id, scope_id, balance_cents, withdrawable, created_at, updated_at) \
         VALUES (?, ?, ?, NULL, 0, 0, ?, ?) ON CONFLICT(id) DO NOTHING",
    )
    .bind(&id)
    .bind(account.kind())
    .bind(account.owner_id())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// 统一记账入口：在调用方已开启的事务内执行，与业务副作用原子。
/// **红线**：断言同 journal `SUM(postings)==0` 且 ≥2 条（有借必有贷），否则内部错误回滚（绝不外泄不平账本）。
/// 副作用：写 `ledger_journals` 头 + 每条 `ledger_postings`，并同步各账户物化余额 `balance_cents`。
pub async fn post_journal(
    tx: &mut Transaction<'_, Any>,
    reason: &str,
    ref_kind: &str,
    ref_id: &str,
    world_id: Option<&str>,
    postings: &[Posting],
) -> Result<String, ApiError> {
    if postings.len() < 2 {
        return Err(ApiError::internal(LedgerError::TooFewPostings));
    }
    // i128 累加：即便单条 i64 接近上限也不会溢出后误判平衡。
    let sum: i128 = postings.iter().map(|p| p.delta_cents as i128).sum();
    if sum != 0 {
        return Err(ApiError::internal(LedgerError::Unbalanced(sum as i64)));
    }

    let now = now_ms();
    let journal_id = new_id("jrnl");
    sqlx::query(
        "INSERT INTO ledger_journals (id, reason, ref_kind, ref_id, world_id, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&journal_id)
    .bind(reason)
    .bind(ref_kind)
    .bind(ref_id)
    .bind(world_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    for p in postings {
        let account_id = ensure_account(tx, &p.account).await?;
        sqlx::query(
            "INSERT INTO ledger_postings (id, journal_id, account_id, delta_cents, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(new_id("post"))
        .bind(&journal_id)
        .bind(&account_id)
        .bind(p.delta_cents)
        .bind(now)
        .execute(&mut **tx)
        .await?;
        // 物化余额同步（account.balance_cents == SUM(该账户 postings)）。
        sqlx::query("UPDATE ledger_accounts SET balance_cents = balance_cents + ?, updated_at = ? WHERE id = ?")
            .bind(p.delta_cents)
            .bind(now)
            .bind(&account_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(journal_id)
}

/// 全额入平台（无创作者分成对手方）的原因——供扣费口 `charge` 判定是否可疑交易留痕。
/// 前三种是正常业务（不留痕）；后两种是红线触发（留痕供 finance 核查）。
enum NoShareReason {
    /// 无世界（平台服务：复活/云成长）→ 全额平台。正常。
    NoWorld,
    /// 世界不存在 → 保守全额平台（绝不凭空产创作者分成）。异常但非资金可疑，不留痕。
    WorldNotFound,
    /// 官方模板（owner NULL）→ 无分成对手方，全额平台。正常。
    OfficialTemplate,
    /// 自打赏防刷：付费方 == 模板 owner → 分成归零（红线⑤）。**留痕**（套利刷分成监测）。
    SelfTip { owner_id: String },
    /// 未成年不得当创作者收款方：owner age_declared != 1 → 分成挂平台冻结待成年（红线④）。**留痕**（合规待成年补实名）。
    MinorOwner { owner_id: String, held_cents: i64 },
}

/// 分成判定结果 —— 红线集中地。
enum ShareOutcome {
    /// 正常分成给创作者：(分成额, 创作者账户 owner)。
    Creator { cut: i64, owner_id: String },
    /// 全额入平台，附原因（供可疑交易留痕判定）。
    AllPlatform(NoShareReason),
}

/// 分成拆分判定。溯源：world 实例 → 模板 owner + 分成率。
/// **认 template.owner_id（创作者），不是 worlds.host_user_id（房主）**。
async fn resolve_share(
    tx: &mut Transaction<'_, Any>,
    world_id: Option<&str>,
    payer_id: &str,
    price_cents: i64,
) -> Result<ShareOutcome, ApiError> {
    let Some(world_id) = world_id else {
        return Ok(ShareOutcome::AllPlatform(NoShareReason::NoWorld));
    };
    let row: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT t.owner_id, t.revenue_share_bps FROM worlds w JOIN world_templates t ON w.template_id = t.id \
         WHERE w.id = ?",
    )
    .bind(world_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((owner_id, bps)) = row else {
        return Ok(ShareOutcome::AllPlatform(NoShareReason::WorldNotFound));
    };
    let Some(owner_id) = owner_id else {
        return Ok(ShareOutcome::AllPlatform(NoShareReason::OfficialTemplate));
    };
    // 红线：自打赏防刷 —— 付费方 == 模板 owner → 分成归零（否则创作者可给自己刷分成套现）。
    if owner_id == payer_id {
        return Ok(ShareOutcome::AllPlatform(NoShareReason::SelfTip { owner_id }));
    }
    let bps = bps.unwrap_or(DEFAULT_REVENUE_SHARE_BPS).clamp(0, 10_000);
    // 取整：创作者分成 = floor(price * bps / 10000)，余数归平台。i128 防溢出。
    let creator_cut = (price_cents as i128 * bps as i128 / 10_000) as i64;
    // 红线：未成年不得作创作者收款方 —— owner age_declared != 1（未声明/未成年/无行）→ 分成挂平台。
    // 记录本应分给创作者的额度（held_cents）供留痕，便于成年补实名后核查/追溯。
    let owner_age: Option<(i64,)> = sqlx::query_as("SELECT age_declared FROM users WHERE id = ?")
        .bind(&owner_id)
        .fetch_optional(&mut **tx)
        .await?;
    if !matches!(owner_age, Some((1,))) {
        return Ok(ShareOutcome::AllPlatform(NoShareReason::MinorOwner {
            owner_id,
            held_cents: creator_cut,
        }));
    }
    Ok(ShareOutcome::Creator { cut: creator_cut, owner_id })
}

/// 统一扣费口 —— **所有付费点的唯一入口**，资金红线集中守护于此。
/// 流程（全程在传入 tx 内，与业务副作用原子）：
///   余额充足校验（不透支）→ 扣 user_wallet → 按世界模板 owner 分账（创作者 + 平台，取整余数归平台）→ post_journal（SUM=0）
///   → 同步 billing_balances 物化视图，维持 `user_wallet == billing_balances` 恒等。
/// 参数：`world_id=Some` → 溯源 template.owner 分成；`None` → 全额入平台（复活/云成长等平台服务）。
/// 返回：`price==0` → no-op（不产 journal，保留免费能力）；余额不足 → `ApiError::Conflict("insufficient_balance")` 零副作用。
/// P0 prod 尚无付费点接入（P1+ 落地 gift/revive/room_open），故 allow(dead_code)——现由 ledger::tests 全覆盖。
#[allow(dead_code)]
pub async fn charge(
    tx: &mut Transaction<'_, Any>,
    user_id: &str,
    price_cents: i64,
    reason: &str,
    ref_kind: &str,
    ref_id: &str,
    world_id: Option<&str>,
) -> Result<ChargeReceipt, ApiError> {
    if price_cents < 0 {
        return Err(ApiError::BadRequest("price 不能为负".into()));
    }
    if price_cents == 0 {
        // 免费（如 room_open_price==0）：no-op，不产 journal。
        return Ok(ChargeReceipt {
            journal_id: None,
            creator_earnings_cents: 0,
            platform_revenue_cents: 0,
            charged_cents: 0,
        });
    }

    // 行锁序列化并发扣费：Postgres 下自赋值 UPDATE ≈ SELECT ... FOR UPDATE（对齐 billing refund 技巧）；
    // SQLite 单连接事务本互斥，此语句为无害占位。防两个并发 charge 双扣透支。
    sqlx::query("UPDATE billing_balances SET balance_cents = balance_cents WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    let balance: i64 = sqlx::query_as::<_, (i64,)>("SELECT balance_cents FROM billing_balances WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|(b,)| b)
        .unwrap_or(0);
    // 红线：不透支 —— 余额不足拒付。此前未写任何 journal/业务 → 零副作用。
    if balance < price_cents {
        return Err(ApiError::Conflict("insufficient_balance".into()));
    }

    let outcome = resolve_share(tx, world_id, user_id, price_cents).await?;
    // 拆出：实际分成额 + 创作者 owner + 全额入平台原因（后者供可疑交易留痕）。
    let (creator_cut, owner_id, no_share_reason) = match outcome {
        ShareOutcome::Creator { cut, owner_id } => (cut, Some(owner_id), None),
        ShareOutcome::AllPlatform(reason) => (0, None, Some(reason)),
    };
    let platform_cut = price_cents - creator_cut; // 取整余数恒归平台
    debug_assert!(
        creator_cut >= 0 && platform_cut >= 0 && creator_cut + platform_cut == price_cents,
        "分账不守恒"
    );

    // 组装 postings：user_wallet 出账 + 创作者/平台入账。零额分录不落，保证无冗余 posting 且 ≥2 条。
    let mut postings = Vec::with_capacity(3);
    postings.push(Posting { account: AccountRef::UserWallet(user_id.to_string()), delta_cents: -price_cents });
    if creator_cut > 0 {
        if let Some(owner) = &owner_id {
            postings.push(Posting { account: AccountRef::CreatorEarnings(owner.clone()), delta_cents: creator_cut });
        }
    }
    if platform_cut > 0 {
        postings.push(Posting { account: AccountRef::PlatformRevenue, delta_cents: platform_cut });
    }

    let journal_id = post_journal(tx, reason, ref_kind, ref_id, world_id, &postings).await?;

    // billing_balances 是 user_wallet 的物化视图：post_journal 已扣 user_wallet 账户，此处同步扣视图，维持恒等。
    sqlx::query("UPDATE billing_balances SET balance_cents = balance_cents - ?, updated_at = ? WHERE user_id = ?")
        .bind(price_cents)
        .bind(now_ms())
        .bind(user_id)
        .execute(&mut **tx)
        .await?;

    // 可疑交易留痕（P4 合规增强）——与扣费同事务原子，随扣费提交/回滚，绝不错位。
    // **红线：留痕 ≠ 拦截**，此处只记 risk_events 供 finance 核查，扣费结果（已定）不受影响。
    record_suspicious(tx, user_id, world_id, reason, price_cents, &journal_id, no_share_reason.as_ref()).await?;

    Ok(ChargeReceipt {
        journal_id: Some(journal_id),
        creator_earnings_cents: creator_cut,
        platform_revenue_cents: platform_cut,
        charged_cents: price_cents,
    })
}

/// 可疑交易留痕判定 —— 在 `charge` 扣费成功后、同事务内写 `risk_events`（P4 合规增强）。
/// 触发（互不影响资金，纯留痕供 finance 核查/AML）：
/// - `minor_creator_hold`：未成年 owner 分成挂账冻结（红线④，held_cents=本应分给创作者、现挂平台的额度）。
/// - `self_tip`：自打赏防刷分成归零（红线⑤，套利刷分成监测）。
/// - `large_charge`：单笔消费超 `SUSPICIOUS_CHARGE_THRESHOLD_CENTS`（异常大额监测）。
/// 正常场景（NoWorld/WorldNotFound/OfficialTemplate/正常分成）不留痕，避免噪声淹没真信号。
async fn record_suspicious(
    tx: &mut Transaction<'_, Any>,
    user_id: &str,
    world_id: Option<&str>,
    reason: &str,
    price_cents: i64,
    journal_id: &str,
    no_share_reason: Option<&NoShareReason>,
) -> Result<(), ApiError> {
    match no_share_reason {
        Some(NoShareReason::MinorOwner { owner_id, held_cents }) => {
            crate::safety::record_risk_tx(
                tx,
                Some(user_id),
                world_id,
                "minor_creator_hold",
                serde_json::json!({
                    "reason": reason,
                    "payerId": user_id,
                    "ownerId": owner_id,
                    "heldCents": held_cents,        // 本应分给创作者、现挂平台冻结待成年补实名
                    "priceCents": price_cents,
                    "journalId": journal_id,
                    "note": "未成年/未实名 owner 分成挂平台冻结（红线：未成年不得当创作者收款方）",
                }),
            )
            .await?;
        }
        Some(NoShareReason::SelfTip { owner_id }) => {
            crate::safety::record_risk_tx(
                tx,
                Some(user_id),
                world_id,
                "self_tip",
                serde_json::json!({
                    "reason": reason,
                    "payerId": user_id,
                    "ownerId": owner_id,           // == payerId（自打赏）
                    "priceCents": price_cents,
                    "journalId": journal_id,
                    "note": "付费方==模板 owner，分成归零（红线：自打赏防刷/套利监测）",
                }),
            )
            .await?;
        }
        _ => {}
    }

    // 异常大额单笔消费（与分成场景正交，独立判定）：留痕不拦截。
    if price_cents >= SUSPICIOUS_CHARGE_THRESHOLD_CENTS {
        crate::safety::record_risk_tx(
            tx,
            Some(user_id),
            world_id,
            "large_charge",
            serde_json::json!({
                "reason": reason,
                "payerId": user_id,
                "priceCents": price_cents,
                "thresholdCents": SUSPICIOUS_CHARGE_THRESHOLD_CENTS,
                "journalId": journal_id,
                "note": "单笔消费超异常大额阈值（AML 监测，仅留痕不拦截）",
            }),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
