//! 历练值 + 卡位制（波次 2）。
//!
//! 产品定位（红线）：历练 = 参与产出的成长值，挂在云端角色卡上（卡是养成容器），
//! 只用于**准入与解锁**（卡位阈值等），**绝不进入引擎决策**——RoundInput / role_decide /
//! 仲裁一概不读历练（叙事平权是产品根基，见本模块 tests 的 grep 级红线断言）。
//!
//! - `grant_mileage_tx` 是历练的**唯一写入路径**（仿 backpack::grant_item_tx 惯例）：
//!   各结算点（章节房通关/隐藏任务、idle 房终局、arena 结算）在**既有结算事务内**调用，
//!   结算失败即回滚，不发历练。
//! - 卡位：users.card_slots（默认 3，上限 6）；发布云端角色时校验「未撤回角色数 < 卡位数」；
//!   总历练（owner 全部未撤回卡的 mileage 之和）达阈值可解锁下一卡位。
//!
//! R1 追加：**三层结算 ③ 世界线层 + 确定性产出表**（总规格 §9【拍板 9(修订)】、§10【拍板 17】）。
//! - 贡献归因：`contribution_breakdown` 按引擎同一套 `IntensityWeights` 逐角色折算，
//!   `accumulate_contributions_tx` 在 commit_tick 事务内累积进**独立账本** `world_contributions`
//!   （🔴 绝不写 narrative_state_json——那会被回灌进引擎，违反平权红线）。
//! - 确定性产出：`resolve_payout_tier` 查实例钉住的**公示产出表**（`assembled_json`），
//!   查表即发、**零随机数**（§16 去抽卡化是合规定性防线）。
//! - 崩塌：`is_collapse_reason` + 具名系数常量 → ③ 归零 + ① 减半 + ② 已锁定保留。
//!
//! R2 追加：③ 层命中的那一档若声明了 `subplotCard`，同一事务内再铸一张**副本卡**
//! （总规格 §10【拍板 1、6、7、11、17】，实现在 `crate::subplot`）——同一张表、同一档、
//! 同一次结算，产出仍是查表所得、零随机。副本卡运营开关默认关闭时该接点恒不发放。
//!
//! 端点：
//! GET  /me/progression        → { totalMileage, cardSlots, maxSlots, nextSlotAt }
//! POST /me/card-slots/unlock  → 达下一阈值则 card_slots+1；未达/已到上限 → 400

use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sqlx::{Any, AnyPool, Row, Transaction};

use crate::app::AppState;
use crate::assembly::{PayoutTable, PayoutTier};
use crate::auth::AuthUser;
use crate::error::ApiError;

use muse_engine::narrative::types::{
    ArbiterOutcome, ArbiterResult, IntensityWeights, PatchOp, RoleDecision, StatePatch,
};

// ---------------- 平衡参数集中区（可调，数值即产品策划口径） ----------------

/// 章节房通关：每张参与卡（结算时该世界 active 成员的云端角色）+100。
pub(crate) const MILEAGE_CHAPTER_CLEAR: i64 = 100;
/// 章节房隐藏任务：每完成一个隐藏钩子（与隐藏道具带出同幂等口径）+50。
pub(crate) const MILEAGE_HIDDEN_TASK: i64 = 50;
/// idle 放置房终局：每张在场卡（终局那一刻的 active 成员角色）+60。
pub(crate) const MILEAGE_IDLE_ENDING: i64 = 60;
/// arena 赛事结算：每张参赛卡（收敛落定时的 active 成员角色，含冠军）+40。
#[cfg(feature = "arena")]
pub(crate) const MILEAGE_ARENA_PARTICIPANT: i64 = 40;
/// arena 赛事冠军：唯一胜者在参与分之外另 +120。
#[cfg(feature = "arena")]
pub(crate) const MILEAGE_ARENA_CHAMPION: i64 = 120;

/// 卡位硬上限：历练解锁到第 6 位为止。
pub(crate) const MAX_CARD_SLOTS: i64 = 6;

// ---- 三层结算 ③ 世界线层与崩塌（总规格 §9【拍板 9(修订)】；系数可被模板 payoutTable 覆盖） ----

/// 世界线崩塌时 ① 保底层的折算系数：**减半**（出席分打折，不清零——有输、有痛、不冤案）。
pub(crate) const COLLAPSE_BASELINE_FACTOR: f64 = 0.5;
/// 世界线崩塌时 ③ 世界线层的折算系数：**归零**（≤ 0 即整层不发放）。
/// ② 成就层不在此列——已锁定的钩子产出**原样保留**（"完成即锁定"的锁定语义正为此设计）。
pub(crate) const COLLAPSE_WORLDLINE_FACTOR: f64 = 0.0;
/// 判定「世界线崩塌」的终局 reason 白名单（runtime 终局 reason 串，见 terminal_reason / 关键角色退场）。
/// 其余 reason（主线走完 / 时间上限 / 无可调度角色）均按正常收束结算。
pub(crate) const COLLAPSE_TERMINAL_REASONS: &[&str] = &["key_character_exit"];

/// 贡献分定点缩放：分值 ×1000 存 BIGINT（`world_contributions.*_milli`）。
/// 用定点整数而非 REAL：双库（SQLite/Postgres）浮点列的精度与比较语义不完全一致，
/// 而"同一贡献分必得同一产出"要求可比较、可复算——定点化把确定性锁在整数域。
pub(crate) const CONTRIBUTION_SCALE: f64 = 1000.0;

/// 卡位解锁阈值：当前卡位数 → 解锁下一位所需**总历练**（owner 全部未撤回卡 mileage 之和）。
/// 第 4 位 = 500、第 5 位 = 1500、第 6 位 = 4000；已到上限 → None。
pub(crate) fn next_slot_threshold(current_slots: i64) -> Option<i64> {
    match current_slots {
        ..=3 => Some(500),
        4 => Some(1500),
        5 => Some(4000),
        _ => None,
    }
}

// ---------------- 唯一写入路径 ----------------

/// 发放历练（历练的**唯一写入路径**，仿 grant_item_tx 惯例）：
/// 在调用方结算事务内 `UPDATE cloud_characters SET mileage = mileage + ?`，与既有结算原子——
/// 结算回滚则历练同滚，绝无「结算失败仍发历练」。
/// `amount` 必须为正（历练只增不减，无任何扣减/回收路径）；`reason` 仅作审计日志标注。
/// 返回受影响行数（0 = 目标角色不存在，容忍跳过不报错，避免 NPC/已删卡拖垮整笔结算）。
pub(crate) async fn grant_mileage_tx(
    tx: &mut Transaction<'_, Any>,
    cloud_character_id: &str,
    amount: i64,
    reason: &str,
) -> Result<u64, ApiError> {
    if amount <= 0 {
        return Err(ApiError::BadRequest("历练发放数额必须为正数".into()));
    }
    let res = sqlx::query("UPDATE cloud_characters SET mileage = mileage + $1 WHERE id = $2")
        .bind(amount)
        .bind(cloud_character_id)
        .execute(&mut **tx)
        .await?;
    let rows = res.rows_affected();
    if rows > 0 {
        tracing::info!(cloud_character_id, amount, reason, "发放历练");
    } else {
        tracing::warn!(cloud_character_id, amount, reason, "历练发放目标角色不存在，跳过");
    }
    Ok(rows)
}

// ---------- 三层结算 ③ 世界线层：贡献归因 + 确定性产出（总规格 §9、§10【拍板 17】） ----------
//
// 三层分工（本模块承载 ① 与 ③，② 在 chapters::finish 与钩子道具同幂等口径发放）：
//   ① 保底层   在场至终局的卡（出席制，不看行动率）→ 基础历练
//   ② 成就层   完成自己隐藏钩子的卡，完成即锁定    → 钩子道具 + 历练（崩塌亦保留）
//   ③ 世界线层 引擎记录的里程碑推动者（确定性数据）→ 查公示产出表确定发放
//
// 🔴 平权红线（§0.1）：贡献分**只决定产出**，绝不进引擎决策——它落在独立的 world_contributions
//    账本里，既不进 narrative_state_json（会被回灌进 RoundInput），也不进任何角色卡字段。
// 🔴 确定性红线（§10 拍板 17 / §16 去抽卡化）：查表发放，全链路零随机数。

/// 结算上下文：实例钉住的公示产出表 + 装配时快照的模板星级（产出封顶输入）。
/// 单一来源 `worlds.assembled_json`——与"确定性 / 可 replay"同源，且结算点本就把它读在手里。
pub(crate) struct PayoutContext {
    pub table: Option<PayoutTable>,
    /// 装配时快照的模板星级；缺失（老实例 / 无装配）→ 1★，即最保守封顶。
    pub star_rating: i64,
    /// 同一张产出表的**副本卡侧视图**（R2，总规格 §10【拍板 1、6、7、11、17】）：
    /// 与 `table` 解析自同一个 `worldlineTiers` 数组、用同一条查表规则命中同一档，
    /// 只是各取所需的字段（历练/道具在 `table`，副本卡星级与卡面在这里）。
    /// 装配层因此不必知道有"副本卡"这种资产——少一处耦合、少一处口径漂移。
    pub subplot_cards: crate::subplot::SubplotCardPayouts,
}

impl PayoutContext {
    /// 贡献分折算权重：产出表声明优先，未声明 → 引擎 `IntensityWeights` 默认值（口径与引擎一致）。
    fn weights(&self) -> IntensityWeights {
        self.table.as_ref().map(|t| t.contribution_weights.clone()).unwrap_or_default()
    }

    /// ① 保底层折算系数：崩塌 → 产出表系数（缺省 COLLAPSE_BASELINE_FACTOR）；正常收束 → 1.0。
    fn baseline_factor(&self, collapsed: bool) -> f64 {
        if !collapsed {
            return 1.0;
        }
        self.table.as_ref().map(|t| t.collapse.baseline_factor).unwrap_or(COLLAPSE_BASELINE_FACTOR)
    }

    /// ③ 世界线层折算系数：崩塌 → 产出表系数（缺省 COLLAPSE_WORLDLINE_FACTOR = 归零）；正常 → 1.0。
    fn worldline_factor(&self, collapsed: bool) -> f64 {
        if !collapsed {
            return 1.0;
        }
        self.table.as_ref().map(|t| t.collapse.worldline_factor).unwrap_or(COLLAPSE_WORLDLINE_FACTOR)
    }
}

/// 读结算上下文（事务内单行主键查询）：`worlds.assembled_json` 的 `/assembly/payoutTable` 与 `/starRating`。
/// 行缺失 / 未装配 / 解析失败 → `(None, 1★)`：③ 层不发放、封顶按最保守档（防御式，与 load_skeleton 同哲学）。
pub(crate) async fn load_payout_context_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
) -> Result<PayoutContext, ApiError> {
    let row = sqlx::query("SELECT assembled_json FROM worlds WHERE id = $1")
        .bind(world_id)
        .fetch_optional(&mut **tx)
        .await?;
    let raw: Option<String> = match row {
        Some(r) => r.try_get("assembled_json")?,
        None => None,
    };
    Ok(payout_context_from_wrapper(raw.as_deref()))
}

/// 从 `assembled_json` 文本解析结算上下文（纯函数，可单测；解析失败一律退化为"不发放 + 1★ 封顶"）。
pub(crate) fn payout_context_from_wrapper(raw: Option<&str>) -> PayoutContext {
    let Some(v) = raw.and_then(|s| serde_json::from_str::<Value>(s).ok()) else {
        return PayoutContext {
            table: None,
            star_rating: 1,
            subplot_cards: crate::subplot::SubplotCardPayouts::default(),
        };
    };
    let table = v
        .pointer("/assembly/payoutTable")
        .and_then(|t| serde_json::from_value::<PayoutTable>(t.clone()).ok());
    let star_rating = v.get("starRating").and_then(Value::as_i64).unwrap_or(1);
    let subplot_cards = crate::subplot::SubplotCardPayouts::from_wrapper_value(Some(&v));
    PayoutContext { table, star_rating, subplot_cards }
}

/// 终局 reason 是否构成「世界线崩塌」（关键 NPC 退场等）。runtime 只传 reason 串，判定收在本模块。
pub(crate) fn is_collapse_reason(reason: &str) -> bool {
    COLLAPSE_TERMINAL_REASONS.contains(&reason)
}

/// 系数折算（历练类整数量）：四舍五入，负数夹到 0（历练只增不减，0 表示本次不发）。
fn scaled_amount(base: i64, factor: f64) -> i64 {
    if !factor.is_finite() || factor <= 0.0 {
        return 0;
    }
    ((base as f64) * factor).round().max(0.0) as i64
}

// ---------- 贡献归因（纯函数：与引擎 round_intensity 同口径，只多了 character 维度） ----------

/// 逐角色贡献分折算。
///
/// **与引擎口径的对应关系**：引擎 `narrative::round_intensity` 把整回合折算成一个**标量**
/// （Σ outcomes 结果权重 + Σ willSpeak 决策互动权重），用于累积 `world.milestoneProgress_<id>`；
/// 它丢弃了 character 维度。本函数用**同一个 `IntensityWeights` 结构、同一组 decisions/outcomes、
/// 同一套权重映射**逐角色分摊，因此恒有：
///
/// ```text
/// Σ contribution_breakdown(..).values() == round_intensity(..)
/// ```
///
/// 于是"世界线推进了多少"与"谁推进的"是同一笔账的总额与分项——无需改引擎，也不会与引擎口径漂移。
/// `Invalid / Blocked` 结果计 0（与引擎一致：无效与阻断不构成推进）。纯函数、无 IO、无随机，可 replay。
pub(crate) fn contribution_breakdown(
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    w: &IntensityWeights,
) -> BTreeMap<String, f64> {
    let mut acc: BTreeMap<String, f64> = BTreeMap::new();
    for o in outcomes {
        let v = match o.result {
            ArbiterResult::Success => w.success,
            ArbiterResult::PartialSuccess => w.partial,
            ArbiterResult::Failure => w.failure,
            ArbiterResult::Invalid | ArbiterResult::Blocked => 0.0,
        };
        if v != 0.0 {
            *acc.entry(o.character_id.clone()).or_insert(0.0) += v;
        }
    }
    for d in decisions {
        if d.speak.will_speak && w.speak != 0.0 {
            *acc.entry(d.character_id.clone()).or_insert(0.0) += w.speak;
        }
    }
    acc
}

/// 本回合的强度是否**真的喂进了里程碑**：引擎 `build_patch` 只在推进阈值里程碑时产出
/// `world.milestoneProgress_<id>` 的 `Increment` op。命中即"本回合参与者是里程碑推动者"——
/// ③ 层「里程碑推动者」的确定性判据，纯读引擎已定序的 patch，不做任何二次推断。
pub(crate) fn fed_milestone(patch: &StatePatch) -> bool {
    patch
        .operations
        .iter()
        .any(|o| o.op == PatchOp::Increment && o.path.starts_with("world.milestoneProgress"))
}

/// 贡献分累计（**在 commit_tick 的同一事务内**调用）：逐角色折算 → 累加进 `world_contributions`。
/// tick 回滚则贡献同滚，绝无"状态没提交但贡献已记"的错位。
///
/// NPC（世界固有角色）同样入账（引擎按 character_id 折算不区分主体），结算时与 world_members 取交集，
/// 无主之卡自然不发放——留档只为审计与运营看板可溯。
///
/// 事务内不使用 `ON CONFLICT ... DO UPDATE`（双库方言差异）：先条件 UPDATE、0 行再 INSERT，
/// 与 backpack::grant_item_tx 的可移植写法同一范式。同一世界的 tick 由 state_revision CAS 串行化，无竞态。
pub(crate) async fn accumulate_contributions_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    patch: &StatePatch,
) -> Result<(), ApiError> {
    let ctx = load_payout_context_tx(tx, world_id).await?;
    let per_character = contribution_breakdown(decisions, outcomes, &ctx.weights());
    if per_character.is_empty() {
        return Ok(());
    }
    let milestone = fed_milestone(patch);
    let now = crate::db::now_ms();
    for (cid, score) in &per_character {
        let delta = (score * CONTRIBUTION_SCALE).round() as i64;
        if delta <= 0 {
            continue;
        }
        let milestone_delta = if milestone { delta } else { 0 };
        let updated = sqlx::query(
            "UPDATE world_contributions SET score_milli = score_milli + $1, \
             milestone_score_milli = milestone_score_milli + $2, updated_at = $3 \
             WHERE world_id = $4 AND character_id = $5",
        )
        .bind(delta)
        .bind(milestone_delta)
        .bind(now)
        .bind(world_id)
        .bind(cid)
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO world_contributions \
                 (world_id, character_id, score_milli, milestone_score_milli, settled_at, updated_at) \
                 VALUES ($1, $2, $3, $4, 0, $5)",
            )
            .bind(world_id)
            .bind(cid)
            .bind(delta)
            .bind(milestone_delta)
            .bind(now)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

// ---------- 确定性产出：查表（无 RNG） ----------

/// 查公示产出表：取「门槛 ≤ 贡献分」中**门槛最高**的一档；无档命中 → `None`（不发放）。
/// 纯查表、零随机——同一贡献分 + 同一张表 ⇒ 恒同一档。门槛唯一性由建模板期
/// `validate_skeleton_refs` 第 5 段前置保证（重复门槛即同分歧义，会被拒绝入库）。
/// 比较在定点整数域完成（×1000 取整），杜绝浮点表示差异导致的档位漂移。
pub(crate) fn resolve_payout_tier(table: &PayoutTable, score: f64) -> Option<&PayoutTier> {
    let s = (score * CONTRIBUTION_SCALE).round() as i64;
    let key = |t: &PayoutTier| (t.min_score * CONTRIBUTION_SCALE).round() as i64;
    table.worldline_tiers.iter().filter(|t| key(t) <= s).max_by_key(|t| key(t))
}

/// ③ 世界线层结算（调用方结算事务内）：里程碑推动者按贡献分查产出表，确定发放。
///
/// `participants`：本次结算的在场成员 `(cloud_character_id, user_id)`——道具须发给**卡的主人**，
/// 故 user_id 不可省（结算点的成员查询必须一并取出）。
///
/// **幂等三重防线**（与 chapters::finish 的发货口径对齐）：
/// ① 事务原子性——结算标记与发放同生共死，回滚则一起消失；
/// ② `world_contributions.settled_at = 0` 条件 UPDATE 的 CAS——重复 / 并发结算 0 行命中即跳过，
///    这层是必须的：③ 层没有 chapters 那种"未通关 → 通关"的天然转变沿，不能寄生在调用方判定上；
/// ③ `backpacks (user_id, reward_hook_key)` DB 唯一键（hook_key = `{world_id}:{cid}:worldline`）——
///    最后一道防线，即使前两层被绕过也不会二次发货。
///
/// **产出封顶不可绕过**：道具 `powerTier > 实例星级`、副本卡 `starRating > 实例星级` 即剔除
/// （不降级、不替换），与装配层封顶同口径。
/// **单一写入路径**：道具一律 `backpack::grant_item_tx`，历练一律 `grant_mileage_tx`，
/// 副本卡一律 `subplot::grant_card_tx`（自带 DB 幂等键），一律不直插任何表。
///
/// 返回已发放明细（供 chapters::finish 回给前端；无产出表 / 无贡献者 → 空 Vec，不报错）。
/// 🔴 **结算期需要的全部运营开关，在进事务之前解析好的一份快照。**
///
/// 为什么要这么一个结构体，而不是给结算函数各加一个 `bool` 参数：
///
/// 1. **事务边界**：结算路径（`chapters::finish`、`runtime::finalize_ending_tx`）全程持有一个
///    事务，而 `flags::is_enabled` 要查库——单连接池上在事务里再借连接就是 `PoolTimedOut`
///    自锁，且那种死锁**在只跑内存 SQLite 的用例里不一定复现**。所以只能在事务外解析。
/// 2. **参数不爆炸**：结算事务里挂着的开关不止一个（副本卡铸卡 / 传世卡自动封卷 /
///    BE 结局传记，见 `flags::MIGRATION_NOTES`）。一个一个加 bool，
///    `settle_idle_world_ending_tx` 迟早变成七个 bool 的函数，且调用点没人看得出哪个是哪个。
/// 3. **一处可查**：「结算这一刻，哪些开关是开的」变成一个能打印、能断言、能进日志的值。
///
/// 🔴 **ctx 一律取 world**：结算是**一个世界事件、多个卡主**。要按人解析就得在事务里
/// 逐 owner 查库——正是第 1 条要避开的东西。端点侧仍按人解析（见各模块自己的说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SettlementFlags {
    /// 副本卡铸卡（`MUSE_SUBPLOT_CARDS`）。关 = 结算一张不铸，**不报错**。
    pub(crate) subplot_cards: bool,
}

impl SettlementFlags {
    /// 在**进事务之前**解析（按世界）。调用点：每个 `db.begin()` 的上方。
    pub(crate) async fn resolve(db: &AnyPool, world_id: &str) -> Self {
        Self {
            subplot_cards: crate::subplot::subplot_cards_enabled(
                db,
                crate::flags::FlagCtx::world(world_id),
            )
            .await,
        }
    }
}

pub(crate) async fn settle_worldline_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    participants: &[(String, String)],
    collapsed: bool,
    flags: SettlementFlags,
) -> Result<Vec<Value>, ApiError> {
    let ctx = load_payout_context_tx(tx, world_id).await?;
    settle_worldline_with_ctx_tx(tx, world_id, participants, collapsed, &ctx, flags).await
}

async fn settle_worldline_with_ctx_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    participants: &[(String, String)],
    collapsed: bool,
    ctx: &PayoutContext,
    flags: SettlementFlags,
) -> Result<Vec<Value>, ApiError> {
    // 未声明产出表 / 表内无档位 → ③ 层默认关闭（VALIDATION §0.1），不发放也不报错。
    let Some(table) = ctx.table.as_ref().filter(|t| !t.worldline_tiers.is_empty()) else {
        return Ok(Vec::new());
    };
    let factor = ctx.worldline_factor(collapsed);
    let now = crate::db::now_ms();
    let mut granted: Vec<Value> = Vec::new();

    for (cid, user_id) in participants {
        // 只认「里程碑推动者」：milestone_score_milli > 0 且尚未结算。无贡献者根本不参与本层。
        let row = sqlx::query(
            "SELECT milestone_score_milli FROM world_contributions \
             WHERE world_id = $1 AND character_id = $2 AND settled_at = 0 AND milestone_score_milli > 0",
        )
        .bind(world_id)
        .bind(cid)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else {
            continue;
        };
        let milestone_milli: i64 = row.try_get("milestone_score_milli")?;

        // 防线②：settled_at CAS 占位。0 行命中 = 已被并发/重复结算 → 跳过，绝不二次发放。
        let claimed = sqlx::query(
            "UPDATE world_contributions SET settled_at = $1, updated_at = $2 \
             WHERE world_id = $3 AND character_id = $4 AND settled_at = 0",
        )
        .bind(now)
        .bind(now)
        .bind(world_id)
        .bind(cid)
        .execute(&mut **tx)
        .await?;
        if claimed.rows_affected() == 0 {
            continue;
        }

        // 崩塌 ③ 归零：占位已落（不留待重复结算的空子），但本层不发放任何产出。
        if factor <= 0.0 {
            continue;
        }
        let score = (milestone_milli as f64 / CONTRIBUTION_SCALE) * factor;
        let Some(tier) = resolve_payout_tier(table, score) else {
            continue;
        };

        if tier.mileage > 0 {
            grant_mileage_tx(tx, cid, tier.mileage, "worldline_settlement").await?;
        }
        let mut item_id: Option<String> = None;
        if let Some(item) = &tier.item {
            if item.origin.power_tier as i64 > ctx.star_rating {
                // 星级封顶：超顶产出直接剔除（不降级、不替换），与装配层 culled_over_tier 同口径。
                tracing::warn!(
                    world_id,
                    cid,
                    item_id = %item.id,
                    power_tier = item.origin.power_tier,
                    star_rating = ctx.star_rating,
                    "世界线产出超出星级封顶，剔除"
                );
            } else {
                let hook_key = format!("{world_id}:{cid}:worldline");
                if crate::backpack::grant_item_tx(tx, user_id, item, world_id, Some(&hook_key))
                    .await?
                    .is_some()
                {
                    item_id = Some(item.id.clone());
                }
            }
        }
        // 副本卡（R2，§10【拍板 1、6、7、11、17】）：与道具/历练**同事务、同一档、同幂等口径**。
        // 幂等不共用 ③ 层的 reward_hook_key（那把键锁的是 backpacks 行），副本卡自带
        // `subplot_cards(owner_id, grant_key)` 唯一键——它是 INSERT 类资产，没有"未通关→通关"
        // 那种天然转变沿可寄生（见 subplot 模块 ⑤）。开关默认关闭时本调用恒返回 None，不发卡。
        let subplot_card = crate::subplot::settle_subplot_card_tx(
            tx,
            &ctx.subplot_cards,
            world_id,
            cid,
            user_id,
            score,
            ctx.star_rating,
            flags.subplot_cards,
        )
        .await?;

        granted.push(json!({
            "characterId": cid,
            "tier": tier.label,
            "contributionScore": score,
            "mileage": tier.mileage,
            "itemId": item_id,
            "subplotCard": subplot_card,
        }));
    }

    // 全链审计（§0.2 资产单一写入路径与全链审计）：本次 ③ 层结算留一条可溯痕迹。
    if !granted.is_empty() {
        sqlx::query(
            "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
             VALUES ($1, 'system', 'system', 'world.worldline_settled', $2, $3, $4)",
        )
        .bind(crate::db::new_id("aud"))
        .bind(world_id)
        .bind(format!(
            "collapsed={collapsed}|factor={factor}|star={}|payouts={}",
            ctx.star_rating,
            serde_json::to_string(&granted).unwrap_or_else(|_| "[]".into())
        ))
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(granted)
}

/// idle 放置房终局结算：① 保底层（每张在场卡 +60）+ ③ 世界线层（查产出表确定发放）。
/// runtime 终局事务内调用；`collapsed` 由调用方传 `is_collapse_reason(reason)`。
///
/// 崩塌语义（§9）：① 减半 · ③ 归零 · ②（已锁定的钩子产出）**原样保留**——② 在 chapters::finish
/// 发货时即由 DB 唯一键锁定，本函数根本不碰它，"保留"是结构性的，不靠任何补偿逻辑。
///
/// 收在本模块是刻意设计：runtime/mod.rs（RoundInput 组装处）不出现任何历练字段引用，
/// 红线「历练不进引擎决策」在 grep 级即可验证（见 tests）。
pub(crate) async fn settle_idle_world_ending_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    participants: &[(String, String)],
    collapsed: bool,
    flags: SettlementFlags,
) -> Result<(), ApiError> {
    let ctx = load_payout_context_tx(tx, world_id).await?;

    // ① 保底层（出席制，不看行动率）：崩塌按 baseline_factor 折算（缺省减半）。
    let amount = scaled_amount(MILEAGE_IDLE_ENDING, ctx.baseline_factor(collapsed));
    if amount > 0 {
        for (cid, _) in participants {
            grant_mileage_tx(tx, cid, amount, "idle_world_ending").await?;
        }
    }

    // ③ 世界线层：里程碑推动者按贡献分查公示产出表确定发放（崩塌归零）。
    settle_worldline_with_ctx_tx(tx, world_id, participants, collapsed, &ctx, flags).await?;

    // 崩塌 → 封卷出一份「BE 结局传记」（§9「坏结局也是内容，封卷收藏」）。与结算同事务：
    // 结算回滚则传记同滚，绝不出现"奖罚没落地但墓志铭已刻好"。正常终局不产出（有输才有痛）。
    seal_be_biography_tx(tx, world_id, collapsed, &ctx).await?;

    // 自动封卷（§12【拍板 23】「死亡 = 传记封卷，不是资产清零」）：本世界里已死的卡转传世卡。
    //
    // 落在结算侧而非死亡落定处，是因为后者在 runtime::commit_tick 内，而平权红线要求
    // runtime/mod.rs 对资产模块零引用（与 subplot 铸卡同一条理由：挂 progression 不挂 runtime）。
    //
    // 顺序在 ①③ 与 BE 传记**之后**：封卷会把 withdrawn 置 1，而 total_mileage 虽已按
    // 「withdrawn=0 OR memorial_status='sealed'」统计（死者历练仍算数），但结算发放本身
    // 只认 participants 名单、不重查库，放在后面可确保「先把这局该给的给完，再封卷」这个直觉次序。
    //
    // 幂等由 seal_character_tx 的 CAS 承担；开关关闭时整段短路。失败不阻断结算——
    // 封卷是纪念不是账目，玩家还可随时用主动认领入口补上。
    if let Err(e) = crate::memorial::auto_seal_dead_participants_tx(tx, world_id, participants).await {
        tracing::warn!(world_id, error = %e, "自动封卷失败（结算照常落定，玩家可主动认领）");
    }
    Ok(())
}

// ===== BE-BIOGRAPHY-READONLY-REGION-BEGIN =====
//
// ============ BE 结局传记（总规格 §9「世界线崩塌」）：**只读汇总区** ============
//
// 规格原文：世界线崩塌（关键 NPC 退场等终局条件）→ ③归零 + ①减半 + ②已锁定保留 +
// 产出「BE 结局传记」（坏结局也是内容，封卷收藏）+ 崩塌责任仲裁公开可溯。
// **有输、有痛、有纪念、无冤案、无武器化。**
//
// 本区只做**内容产出侧**：把"世界崩了"这件事变成一件可收藏的东西。奖罚折算（①减半 ③归零
// ②保留）早在上面的 `settle_*` 里完成了，本区一个数值都不改。
//
// ── 🔴 三条硬约束（改本区前先读完） ────────────────────────────────────────
//
// ① **公共事实不可回滚（§0.3）：传记是只读汇总。**
//    本区对 `worlds` / `world_events` / `world_ticks` / `world_contributions` / `world_members`
//    **只有 SELECT**，唯一的写入是往 `world_biographies` INSERT 一行 + 一条 audit_logs 留痕。
//    由两道断言守死：源码级（`be_biography_region_is_read_only`，扫本区文本里的 UPDATE/DELETE）
//    + 运行时级（`be_biography_never_mutates_worldline`，封卷前后五张表全量快照逐字节相等）。
//
// ② **无冤案：崩塌原因不许模型现编。**
//    崩塌原因的唯一来源是两处**既有确定性数据**：`runtime::terminal_reason()` 产的 reason 串，
//    与它写进 `audit_logs(action='world.ended')` 的那一行（格式 `{reason}|ending={ending}`）。
//    责任文案取自代码里的**固定字典** `collapse_reason_label`——不是模型写的句子，
//    因此不会出现"AI 觉得是你害死了这个世界"这种无法对质的指控。
//    本区源码级不含任何模型/provider 调用（`be_biography_is_model_free` 断言）。
//    双重确认：不但结算侧说这是崩塌（`collapsed` 参数），审计痕里的 reason 也必须在
//    `COLLAPSE_TERMINAL_REASONS` 白名单里，两处不一致就**不产出**——宁可没有墓志铭，
//    也不刻一句来源不明的死因。
//    「蓄意毁世界者进风控」是既有 `risk_events` 的事，本区不做任何判定、不点任何人的名。
//
// ③ **不复制叙事正文，也不下发真人身份。**
//    摘要里只有**计量与结构**（拍数、事件按类型计数、里程碑推进量、成员足迹的时刻与贡献分），
//    没有一个字的 public/private projection——正文的读取面有受众投影隔离与机审门
//    （`world_events.visibility` / `.moderation`），复制进传记等于给正文开第二条不过闸的读路径。
//    足迹只记角色 id 与角色面具名，**不记 `user_id`**（§14 恨隔面具原则：传记是角色的墓志铭，
//    不是真人的花名册）。
//
// ── 未验证功能默认关闭（VALIDATION.md §0.1） ────────────────────────────────
// env 开关 `MUSE_WORLD_BE_BIOGRAPHY`，**默认关闭**。范式抄 `worlds::deathmatch_enabled`：
// 产出侧不产出（本区第一行）+ 读取侧 `GET /worlds/{id}/biography` 恒 404（`worlds` 模块）。
// 可逆急停阀——关阀期间崩塌的世界不产传记，再打开也不会追溯补写（传记是**封卷那一刻**的快照，
// 补写会把"当时的事实"换成"今天重算的事实"，那才是真的改写历史）。

/// BE 结局传记运营开关环境变量。
const ENV_BE_BIOGRAPHY: &str = "MUSE_WORLD_BE_BIOGRAPHY";
/// BE 结局传记默认值 = **关闭**。
const DEFAULT_BE_BIOGRAPHY_ENABLED: bool = false;

/// 足迹条数上限的 env 名（§0.2 参数化：摘要长度一律可配）。
const ENV_BE_BIO_MAX_FOOTPRINTS: &str = "MUSE_BE_BIO_MAX_FOOTPRINTS";
/// 足迹条数默认上限：200 位角色。超出部分不写进传记，但 `total` 如实给出（截断可见，不装作没有）。
const DEFAULT_BE_BIO_MAX_FOOTPRINTS: i64 = 200;
const MIN_BE_BIO_MAX_FOOTPRINTS: i64 = 1;
const HARD_BE_BIO_MAX_FOOTPRINTS: i64 = 2_000;

/// 事件类型分档条数上限的 env 名。
const ENV_BE_BIO_MAX_EVENT_KINDS: &str = "MUSE_BE_BIO_MAX_EVENT_KINDS";
/// 事件类型分档默认上限：20 类（按出现次数降序取前 N 类）。
const DEFAULT_BE_BIO_MAX_EVENT_KINDS: i64 = 20;
const MIN_BE_BIO_MAX_EVENT_KINDS: i64 = 1;
const HARD_BE_BIO_MAX_EVENT_KINDS: i64 = 200;

/// 传记种类：崩塌封卷。正常终局不产出本类传记。
const BIOGRAPHY_KIND_BE: &str = "be";
/// 摘要结构版本（结构演进时 +1，读取面据此兼容旧封卷）。
const BIOGRAPHY_SCHEMA_VERSION: i64 = 1;

/// BE 结局传记是否已由运营开启（env 覆盖 + 默认常量，范式同 `worlds::deathmatch_enabled`）。
pub fn be_biography_enabled() -> bool {
    match std::env::var(ENV_BE_BIOGRAPHY) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            // 配错不静默开启：回落默认（关闭）。
            _ => DEFAULT_BE_BIOGRAPHY_ENABLED,
        },
        Err(_) => DEFAULT_BE_BIOGRAPHY_ENABLED,
    }
}

/// 测试专用：BE 传记相关 env 的 RAII 夹具（范式同 `worlds::DeathmatchSwitch` / `subplot::SubplotSwitch`）。
///
/// 这些 env 是**进程级**的，本模块用例与其它模块同属一个测试二进制、默认并发跑，
/// 故对开关敏感的用例共用**同一把锁**串行化，并在 Drop 时把 env 恢复原状。
/// 跨模块可见（`worlds` 的传记读取面用例也要它），故 `pub(crate)`。
#[cfg(test)]
pub(crate) struct BiographySwitch {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Vec<(&'static str, Option<String>)>,
}

#[cfg(test)]
impl BiographySwitch {
    /// 只置总开关。
    pub(crate) fn set(on: bool) -> Self {
        Self::with(on, &[])
    }

    /// 置总开关 + 若干额外 env（如 `MUSE_BE_BIO_MAX_FOOTPRINTS`）。
    /// ⚠️ `extra` 里不要再放总开关键，否则 Drop 时同键恢复两次会把状态留给下一个用例。
    pub(crate) fn with(on: bool, extra: &[(&'static str, &str)]) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut prev = vec![(ENV_BE_BIOGRAPHY, std::env::var(ENV_BE_BIOGRAPHY).ok())];
        std::env::set_var(ENV_BE_BIOGRAPHY, if on { "1" } else { "0" });
        for (k, v) in extra {
            prev.push((k, std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        Self { _guard: guard, prev }
    }
}

#[cfg(test)]
impl Drop for BiographySwitch {
    fn drop(&mut self) {
        for (k, v) in &self.prev {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// 读整数 env（缺失/非法 → 默认值，再 clamp 进安全区间；范式同 `subplot::read_i64_env`）。
fn read_clamped_env(key: &str, default: i64, min: i64, max: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

/// 传记里保留的参与者足迹条数上限（运营可调）。
fn be_bio_max_footprints() -> i64 {
    read_clamped_env(
        ENV_BE_BIO_MAX_FOOTPRINTS,
        DEFAULT_BE_BIO_MAX_FOOTPRINTS,
        MIN_BE_BIO_MAX_FOOTPRINTS,
        HARD_BE_BIO_MAX_FOOTPRINTS,
    )
}

/// 传记里保留的事件类型分档数上限（运营可调）。
fn be_bio_max_event_kinds() -> i64 {
    read_clamped_env(
        ENV_BE_BIO_MAX_EVENT_KINDS,
        DEFAULT_BE_BIO_MAX_EVENT_KINDS,
        MIN_BE_BIO_MAX_EVENT_KINDS,
        HARD_BE_BIO_MAX_EVENT_KINDS,
    )
}

/// 🔴 崩塌原因的**固定文案字典**（不是模型生成的句子）。
///
/// 入参恒为 `runtime::terminal_reason()` 词表内的串。未知串走兜底文案，且**只复述 reason 本身**，
/// 绝不编造归因——"无冤案"的具体落地就是这一行：字典没有的死因，宁可说不清楚，不许现编。
fn collapse_reason_label(reason: &str) -> &'static str {
    match reason {
        "key_character_exit" => "关键角色永久退场，世界线失去支点而崩塌",
        _ => "世界线崩塌（原因串见 terminalReason，未在文案字典内）",
    }
}

/// 拆 `audit_logs('world.ended').reason` 的写入格式 `{reason}|ending={ending}`。
///
/// 口径与 `runtime::finalize_ending_tx` 的写入格式、`slo` 的读取解析**必须一致**：
/// 无 `|` 分隔（防御，历史/异常数据）→ 整串当 reason、结局空；`ending=none` 视同无结局。
fn split_world_ended_reason(raw: &str) -> (String, String) {
    let (reason, rest) = match raw.split_once('|') {
        Some((r, rest)) => (r, rest),
        None => (raw, ""),
    };
    let ending = rest.strip_prefix("ending=").unwrap_or("");
    let ending = if ending == "none" { "" } else { ending };
    (reason.trim().to_string(), ending.trim().to_string())
}

/// 角色卡的公开面具名（`card_json.identity.name`）。取不到 → 空串，绝不因取名失败中断封卷。
fn card_display_name(card_json: Option<&str>) -> String {
    card_json
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|v| v.pointer("/identity/name").and_then(|n| n.as_str()).map(str::to_string))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 崩塌终局 → 封卷一份 BE 结局传记（**在调用方结算事务内**）。
///
/// 不产出的全部情形（每一种都是正当理由，一律静默返回 Ok，绝不拖垮整笔结算）：
/// - `collapsed == false`：**正常终局不产出**（主线走完 / 时间上限 / 无可调度角色都不是 BE）；
/// - 运营开关未开（§0.1 未验证功能默认关闭）；
/// - 本世界已有传记（幂等，见下）；
/// - 找不到 `audit_logs('world.ended')` 那条痕，或痕里的 reason 不在崩塌白名单里
///   （🔴 无冤案：崩塌原因必须有确定性出处，两处口径不一致就不刻墓志铭）。
///
/// **幂等两道**：① 先查 `world_biographies` 有无本世界的行（同一事务内可见）；
/// ② `world_biographies.world_id` 主键兜底。不用 `ON CONFLICT`（双库方言禁用，见 db.rs）——
/// 同一世界的终局由 `end_world_tx` 的 `WHERE status='running'` CAS 串行化，本就只可能走一次。
async fn seal_be_biography_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    collapsed: bool,
    ctx: &PayoutContext,
) -> Result<(), ApiError> {
    if !collapsed || !be_biography_enabled() {
        return Ok(());
    }
    // 幂等①：已封卷 → 不重复产出（重复触发不重复产传记）。
    let existed: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM world_biographies WHERE world_id = $1")
            .bind(world_id)
            .fetch_optional(&mut **tx)
            .await?;
    if existed.is_some() {
        return Ok(());
    }

    // ---- 崩塌原因：唯一来源 = audit_logs('world.ended')（runtime 在本事务内、本函数之前写入） ----
    // 🔴 次级键 `id` 不可省：这条 SELECT 的产物是写进 BE 传记的**死因**，注释自称"唯一确定性事实源"——
    // 排序键并列即证伪该自称。`end_world_tx` 的 `WHERE status='running'` CAS 保证同一世界只会写一条
    // `world.ended`，故实践中不会并列；但"实践中不会"不是确定性，补 `id ASC` 让它在 schema 层面成立。
    let audit_row = sqlx::query(
        "SELECT reason, created_at FROM audit_logs \
         WHERE action = 'world.ended' AND subject = $1 ORDER BY created_at ASC, id ASC LIMIT 1",
    )
    .bind(world_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(audit_row) = audit_row else {
        // 🔴 没有审计痕 = 崩塌原因没有确定性出处 → 不产出（不许靠推断补一个死因）。
        tracing::warn!(world_id, "崩塌世界缺 world.ended 审计痕，跳过 BE 传记产出");
        return Ok(());
    };
    let audit_reason: String = audit_row.try_get("reason")?;
    let ended_at: i64 = audit_row.try_get("created_at")?;
    let (terminal_reason, ending_id) = split_world_ended_reason(&audit_reason);
    if !is_collapse_reason(&terminal_reason) {
        // 结算侧说崩塌、审计痕说不是 —— 两处口径不一致，不刻墓志铭（无冤案）。
        tracing::warn!(world_id, %terminal_reason, "审计痕 reason 不在崩塌白名单，跳过 BE 传记产出");
        return Ok(());
    }

    // ---- 世界元信息（只读） ----
    let w = sqlx::query(
        "SELECT title, room_type, template_id, template_version, lethality, tick_per_day, \
         member_limit, created_at FROM worlds WHERE id = $1",
    )
    .bind(world_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    let template_id: String = w.try_get("template_id")?;
    let star_rating: i64 =
        sqlx::query_scalar("SELECT star_rating FROM world_templates WHERE id = $1")
            .bind(&template_id)
            .fetch_optional(&mut **tx)
            .await?
            .unwrap_or(ctx.star_rating);

    // ---- 世界线摘要：拍数（按 tick 状态分档）+ 末拍号 ----
    let tick_rows = sqlx::query("SELECT status, COUNT(*) AS n FROM world_ticks WHERE world_id = $1 GROUP BY status")
        .bind(world_id)
        .fetch_all(&mut **tx)
        .await?;
    let mut ticks_by_status: BTreeMap<String, i64> = BTreeMap::new();
    for r in &tick_rows {
        ticks_by_status.insert(r.try_get("status")?, r.try_get("n")?);
    }
    let total_ticks: i64 = ticks_by_status.values().sum();
    let last_tick_no: Option<i64> = sqlx::query_scalar(
        "SELECT tick_no FROM world_ticks WHERE world_id = $1 ORDER BY tick_no DESC LIMIT 1",
    )
    .bind(world_id)
    .fetch_optional(&mut **tx)
    .await?;

    // ---- 世界线摘要：事件计量（只计数，不复制正文） ----
    let total_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE world_id = $1")
        .bind(world_id)
        .fetch_one(&mut **tx)
        .await?;
    let event_kind_total: i64 =
        sqlx::query_scalar("SELECT COUNT(DISTINCT event_type) FROM world_events WHERE world_id = $1")
            .bind(world_id)
            .fetch_one(&mut **tx)
            .await?;
    let kind_limit = be_bio_max_event_kinds();
    // 次序确定性：先按次数降序，同次数再按类型名升序 —— 两次封卷必得同一份 JSON。
    let event_rows = sqlx::query(
        "SELECT event_type, COUNT(*) AS n FROM world_events WHERE world_id = $1 \
         GROUP BY event_type ORDER BY COUNT(*) DESC, event_type ASC LIMIT $2",
    )
    .bind(world_id)
    .bind(kind_limit)
    .fetch_all(&mut **tx)
    .await?;
    let mut events_by_type = Vec::new();
    for r in &event_rows {
        events_by_type.push(json!({
            "eventType": r.try_get::<String, _>("event_type")?,
            "count": r.try_get::<i64, _>("n")?,
        }));
    }

    // ---- 参与者足迹：成员行（时刻与状态）× 贡献账本（确定性分值） ----
    let contrib_rows = sqlx::query(
        "SELECT character_id, score_milli, milestone_score_milli FROM world_contributions WHERE world_id = $1",
    )
    .bind(world_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut contributions: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut milestone_total: i64 = 0;
    let mut contribution_total: i64 = 0;
    for r in &contrib_rows {
        let cid: String = r.try_get("character_id")?;
        let score: i64 = r.try_get("score_milli")?;
        let milestone: i64 = r.try_get("milestone_score_milli")?;
        contribution_total += score;
        milestone_total += milestone;
        contributions.insert(cid, (score, milestone));
    }

    let member_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_members WHERE world_id = $1")
        .bind(world_id)
        .fetch_one(&mut **tx)
        .await?;
    let footprint_limit = be_bio_max_footprints();
    // 次序确定性：入场时刻升序，同刻按成员行 id 升序。
    let member_rows = sqlx::query(
        "SELECT m.cloud_character_id AS cid, m.status, m.joined_at, m.left_at, cc.card_json AS card \
         FROM world_members m LEFT JOIN cloud_characters cc ON cc.id = m.cloud_character_id \
         WHERE m.world_id = $1 ORDER BY m.joined_at ASC, m.id ASC LIMIT $2",
    )
    .bind(world_id)
    .bind(footprint_limit)
    .fetch_all(&mut **tx)
    .await?;
    let mut footprints = Vec::new();
    for r in &member_rows {
        let cid: String = r.try_get("cid")?;
        let card: Option<String> = r.try_get("card")?;
        let (score, milestone) = contributions.get(&cid).copied().unwrap_or((0, 0));
        footprints.push(json!({
            "characterId": cid,
            // 🔴 只有角色面具名，没有 user_id（§14）。
            "name": card_display_name(card.as_deref()),
            "status": r.try_get::<String, _>("status")?,
            "joinedAt": r.try_get::<i64, _>("joined_at")?,
            "leftAt": r.try_get::<Option<i64>, _>("left_at")?,
            "contributionMilli": score,
            "milestoneMilli": milestone,
        }));
    }

    // ---- 组装摘要（纯确定性数据；无随机、无模型） ----
    let summary = json!({
        "schemaVersion": BIOGRAPHY_SCHEMA_VERSION,
        "world": {
            "id": world_id,
            "title": w.try_get::<String, _>("title")?,
            "roomType": w.try_get::<String, _>("room_type")?,
            "templateId": template_id,
            "templateVersion": w.try_get::<i64, _>("template_version")?,
            "starRating": star_rating,
            // 落库原值（建房方意图）：传记是"当时那场戏是什么契约"的事实快照。
            "lethality": w.try_get::<String, _>("lethality")?,
            "tickPerDay": w.try_get::<i64, _>("tick_per_day")?,
            "memberLimit": w.try_get::<i64, _>("member_limit")?,
            "createdAt": w.try_get::<i64, _>("created_at")?,
            "endedAt": ended_at,
        },
        "collapse": {
            "terminalReason": &terminal_reason,
            // 固定字典文案（见 collapse_reason_label）——不是模型写的。
            "reasonLabel": collapse_reason_label(&terminal_reason),
            "endingId": &ending_id,
            // 原始审计痕原样附上：任何人都能拿它回 audit_logs 对质（崩塌责任仲裁公开可溯）。
            "auditReason": &audit_reason,
            "auditedAt": ended_at,
            "source": "runtime::terminal_reason + audit_logs(action='world.ended')",
            // 🔴 显式声明：本段不含任何模型生成内容，也不含任何责任归属判定。
            "modelGenerated": false,
            "blameAssigned": false,
        },
        "worldline": {
            "totalTicks": total_ticks,
            "ticksByStatus": ticks_by_status.iter().map(|(k, v)| json!({ "status": k, "count": v })).collect::<Vec<_>>(),
            "lastTickNo": last_tick_no,
            "totalEvents": total_events,
            "eventTypeTotal": event_kind_total,
            "eventsByType": events_by_type,
            "eventsTruncated": event_kind_total > kind_limit,
            // 贡献账本合计（定点 ×1000，口径同 world_contributions）。
            "contributionMilliTotal": contribution_total,
            "milestoneMilliTotal": milestone_total,
        },
        "footprints": {
            "total": member_total,
            "truncated": member_total > footprint_limit,
            "items": footprints,
        },
        // 三层结算在崩塌下的实际折算系数（§9：①减半 ③归零 ②已锁定保留），与本次结算所用的完全同源。
        "settlement": {
            "baselineFactor": ctx.baseline_factor(true),
            "worldlineFactor": ctx.worldline_factor(true),
            "achievementNote": "② 成就层已锁定产出原样保留（结算时即由 DB 唯一键锁定，崩塌不回收）",
        },
        "notes": [
            "本传记是对既有确定性数据的只读汇总，不改写任何世界线事实（§0.3 公共事实不可回滚）。",
            "崩塌原因取自 runtime 终局判定与 audit_logs('world.ended')，非模型生成；本传记不做任何责任归属判定。",
            "不复制叙事正文：正文的唯一事实源仍是 world_events，其受众投影与审核门不变。",
            "足迹只含角色面具名，不含真人身份（§14）。",
        ],
    });

    // ---- 唯一写入：封卷一行 ----
    let now = crate::db::now_ms();
    sqlx::query(
        "INSERT INTO world_biographies (world_id, kind, terminal_reason, ending_id, summary_json, sealed_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(world_id)
    .bind(BIOGRAPHY_KIND_BE)
    .bind(&terminal_reason)
    .bind(&ending_id)
    .bind(summary.to_string())
    .bind(now)
    .execute(&mut **tx)
    .await?;

    // 全链审计（§0.2）：封卷这件事本身也留痕。
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES ($1, 'system', 'system', 'world.be_biography_sealed', $2, $3, $4)",
    )
    .bind(crate::db::new_id("aud"))
    .bind(world_id)
    .bind(format!("terminalReason={terminal_reason}|ending={ending_id}|footprints={}", footprints.len()))
    .bind(now)
    .execute(&mut **tx)
    .await?;

    Ok(())
}
// ===== BE-BIOGRAPHY-READONLY-REGION-END =====

// ---------------- 读侧辅助（卡位校验 / 进度查询共用） ----------------

/// owner 的总历练 = 全部**未撤回**云端角色 **＋ 已封卷的传世卡** 的 mileage 之和。
///
/// 🔴 为什么传世卡要算进来（总规格 §12【拍板 23】）：该节标题就是
/// **「死亡 = 传记封卷，不是资产清零」**，并明写「卡的价值 = 累计人生（历练/羁绊/传记/足迹），
/// 全是显性资产」。封卷时为了复用 join 那道门会把 `withdrawn` 置 1（见 `memorial` 模块），
/// 若这里只按 `withdrawn = 0` 统计，角色一死玩家的**卡位解锁进度就会倒退**——
/// 那正是「资产清零」，与该节的产品承诺直接冲突。
///
/// **主动撤回仍然不算**：那是玩家自己把卡收回、不再参与，与「死得其所」是两回事，
/// 保持原口径（撤回可逆，封卷不可逆）。
///
/// 与 `count_active_cards` 的区别是刻意的：那个统计**卡位占用**，传世卡已不可再入世界、
/// 不该继续占着容器位，所以那边必须保持 `withdrawn = 0`。
/// 一句话：**传世卡不再占位，但它挣来的历练永远算数。**
///
/// 汇总在 Rust 侧完成（避免 SQL SUM 的双库类型差异，遵守 db.rs 可移植子集约定）。
pub(crate) async fn total_mileage(db: &AnyPool, owner_id: &str) -> Result<i64, ApiError> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT mileage FROM cloud_characters \
         WHERE owner_id = $1 AND (withdrawn = 0 OR memorial_status = 'sealed')",
    )
    .bind(owner_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(m,)| m).sum())
}

/// owner 现有未撤回云端角色数（发布卡位校验的分子）。
pub(crate) async fn count_active_cards(db: &AnyPool, owner_id: &str) -> Result<i64, ApiError> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cloud_characters WHERE owner_id = $1 AND withdrawn = 0")
            .bind(owner_id)
            .fetch_one(db)
            .await?;
    Ok(n)
}

/// 用户当前卡位数（users.card_slots；行缺失按默认 3 兜底，不因历史数据报错）。
pub(crate) async fn card_slots_of(db: &AnyPool, user_id: &str) -> Result<i64, ApiError> {
    let slots: Option<i64> = sqlx::query_scalar("SELECT card_slots FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    Ok(slots.unwrap_or(3))
}

// ---------------- 端点 ----------------

/// GET /me/progression：我的历练进度与卡位。
/// `nextSlotAt` = 解锁下一卡位所需总历练阈值；已到 6 上限 → null。
async fn my_progression(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let total = total_mileage(&state.db, &user.user_id).await?;
    let slots = card_slots_of(&state.db, &user.user_id).await?;
    Ok(Json(json!({
        "totalMileage": total,
        "cardSlots": slots,
        "maxSlots": MAX_CARD_SLOTS,
        "nextSlotAt": next_slot_threshold(slots),
    })))
}

/// POST /me/card-slots/unlock：总历练达下一阈值则解锁一个卡位（card_slots+1）。
/// - 已到 6 上限 → 400；
/// - 总历练未达阈值 → 400（文案含还差多少）；
/// - 并发解锁由 `card_slots = 旧值` 的 CAS 挡住（0 行命中 → 409 请重试），绝不越阈值双跳。
async fn unlock_card_slot(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let slots = card_slots_of(&state.db, &user.user_id).await?;
    let Some(threshold) = next_slot_threshold(slots) else {
        return Err(ApiError::BadRequest(format!("卡位已达上限（{MAX_CARD_SLOTS}），无法继续解锁")));
    };
    let total = total_mileage(&state.db, &user.user_id).await?;
    if total < threshold {
        let lack = threshold - total;
        return Err(ApiError::BadRequest(format!(
            "历练不足：解锁第 {} 个卡位需总历练 {threshold}，当前 {total}，还差 {lack}",
            slots + 1
        )));
    }
    // CAS：仅当卡位仍是读到的旧值才 +1（历练只增不减，阈值判定单调安全；并发解锁只成一次）。
    let res = sqlx::query("UPDATE users SET card_slots = card_slots + 1, updated_at = $1 WHERE id = $2 AND card_slots = $3")
        .bind(crate::db::now_ms())
        .bind(&user.user_id)
        .bind(slots)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::Conflict("卡位状态已变化，请刷新后重试".into()));
    }
    let new_slots = slots + 1;
    Ok(Json(json!({
        "cardSlots": new_slots,
        "totalMileage": total,
        "maxSlots": MAX_CARD_SLOTS,
        "nextSlotAt": next_slot_threshold(new_slots),
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/progression", get(my_progression))
        .route("/me/card-slots/unlock", post(unlock_card_slot))
}

#[cfg(test)]
mod tests;
