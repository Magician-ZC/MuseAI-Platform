//! 副本卡资产与合成（R2 枢纽）。总规格 `docs/build/spec-world-ecosystem.md` §10【拍板 1、6、7、11、17】。
//!
//! 副本卡 = **官方世界结算产出的「剧情结晶」**——把你亲历的剧情副本铸成一张永久蓝图，
//! 装进自定义房当内容燃料（房散了卡还在），或作为高星世界/秘境的敲门砖、叙事信物、收藏陈列。
//!
//! 端点（玩家端，全部 `AuthUser`）：
//! GET  /me/subplot-cards?status=owned|consumed|all   我的副本卡（含星级、来源、合成血缘）
//! POST /me/subplot-cards/synthesize                  同星合成 N×n★ → 1×(n+1)★（Idempotency-Key 可选）
//!
//! ────────────────────────────────────────────────────────────────────────────
//! 🔴 五条硬约束，改本模块前先读完
//! ────────────────────────────────────────────────────────────────────────────
//!
//! ① **零 RNG，确定性产出**（§10【拍板 17】+ §16「去抽卡化是定性防线的关键」）。
//!    产出链路 = 三层结算算贡献分 → 查**公示产出表**（随实例钉在 `worlds.assembled_json`）→ 确定发放。
//!    **同一贡献分 + 同一张表 ⇒ 恒得同一张卡**，全程不掷一次骰子：没有爆率、没有开箱、没有保底。
//!    张力来自「能否完成钩子 / 推动主线」的过程不确定性，不来自随机数。产出挂结算不挂时长（防泡时长）。
//!    本模块源码级不含任何随机数 API（`red_line_module_is_rng_free` 断言）；行主键 `new_id` 是
//!    **行标识**不是产出内容——产出内容（星级/档位/来源）全部由查表得出，可 replay 复算。
//!    运营话术同步避开「开箱/抽卡/爆率」，统一用「结算产出 / 产出表」。
//!
//! ② **永不加战力**（§0.1 平权宪法）。副本卡不进任何引擎决策或数值判定路径：
//!    引擎侧（`crates/muse-engine`）与 `RoundInput` 组装处（`server/src/runtime/mod.rs`）
//!    源码级零 `subplot` 引用；本模块也绝不写 `worlds` 的世界叙事状态列（那一列每拍被原样回灌进
//!    引擎 `RoundInput.state`，写进去就等于把资产喂给了决策——口径同 0025 贡献账本的红线）。
//!    由 `red_line_subplot_cards_never_enter_engine_decision` grep 级断言守死，口径同历练 mileage。
//!
//! ③ **玩家间交易暂不开**（§10 + §0.5 无提现红线）。无提现红线下道具交易 = RMT 侧门。
//!    本模块**没有任何转让/赠送/挂单端点**，`owner_id` 只在 INSERT 时写入、全模块无任何改写它的 SQL。
//!    由 `red_line_no_transfer_or_trade_path`（路由白名单 + 「无改写 owner 的 UPDATE」源码断言 +
//!    运行时 404 探测）三重锁死。远期开放需重风控 + 显式评审。
//!
//! ④ **未验证功能默认关闭**（VALIDATION.md §0.1）。整块能力由运营开关 `MUSE_SUBPLOT_CARDS` 控制，
//!    **默认关闭**。范式抄 `worlds::deathmatch_enabled`——**前门拒绝 + 读取侧降级双保险**：
//!    关闭时两个端点 404（功能不存在，不泄露「平台有这个未开放功能」），**结算也不铸卡**，
//!    已有的卡读不出、合不了；再打开则原样恢复（可逆急停阀，不是一次性阉割）。
//!    数据侧另有一层默认关闭：模板产出表未声明 `subplotCard` → 该档不发卡（开闸靠运营录数据）。
//!
//! ⑤ **资产单一写入路径 + 自带幂等键**（§0.2）。副本卡的唯一写入口是 `grant_card_tx`，
//!    幂等落在 DB 唯一约束 `subplot_cards(owner_id, grant_key)` 上——**不是**寄生在调用方判定上。
//!    历练 `grant_mileage_tx` 的裸 UPDATE 靠调用方「未通关 → 通关」的天然转变沿去重；
//!    副本卡是 INSERT 类资产、没有那种转变沿，所以幂等必须自带（范式同 `backpacks.reward_hook_key`）。
//!
//! 合成（回收口，防通胀）：N×n★ → 1×(n+1)★，N 与星级上限均为运营参数（§0.2 禁写死）。
//! 源卡销毁与新卡铸成在**同一事务**内完成：源卡走 `status='owned'` 的条件 UPDATE（CAS），
//! 任一张没抢到即整笔回滚——不会出现「卡熔了但新卡没出来」，也不会「重复提交多出一张卡」。

use std::collections::BTreeSet;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Any, Row, Transaction};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;

#[cfg(test)]
mod tests;

// ---------------- 运营开关（VALIDATION.md §0.1 未验证功能默认关闭） ----------------

/// 副本卡运营开关环境变量。
const ENV_SUBPLOT_CARDS_ENABLED: &str = "MUSE_SUBPLOT_CARDS";

/// 副本卡默认值 = **关闭**。
///
/// 🔴 副本卡是整条经济闭环的入口（结算产出 → 合成 → 自定义房消费），属 VALIDATION.md §2
/// T4「平台生态」才验证的范围；代码合并不等于对用户开放，必须运营显式打开本开关。
const DEFAULT_SUBPLOT_CARDS_ENABLED: bool = false;

/// 副本卡能力是否已由运营开启（env 覆盖 + 默认常量，范式同 `worlds::deathmatch_enabled`
/// 与 `invitations::invitations_enabled`——本仓库尚无配置表，env 是当前唯一的运营开关形态；
/// 将来配置表落地后只改本函数内部，调用点与降级语义不变）。
///
/// 开关是**全局急停阀**，作用在读取侧与发放侧：关掉之后结算立刻不再铸卡、已有卡读不出也合不了，
/// 再打开则原样恢复（已铸出的卡不因关阀而丢失——那是资产，不是功能）。
pub fn subplot_cards_enabled() -> bool {
    match std::env::var(ENV_SUBPLOT_CARDS_ENABLED) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            // 配错不静默开启未验证的经济闭环：回落默认（关闭）。
            _ => DEFAULT_SUBPLOT_CARDS_ENABLED,
        },
        Err(_) => DEFAULT_SUBPLOT_CARDS_ENABLED,
    }
}

/// 开关门：关闭时整块能力**不存在**（404，而非 403）——不向外泄露「平台有这个未开放功能」。
/// 每个端点第一行都调它，读端点同样调（读取侧降级）。
fn ensure_enabled() -> Result<(), ApiError> {
    if subplot_cards_enabled() {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 测试专用：副本卡相关 env 的 RAII 夹具（开关 + 可选的参数化 env）。
///
/// 这些 env 是**进程级**的，而本模块用例与其它模块同属一个测试二进制、默认并发跑，
/// 故所有对 env 敏感的用例共用**同一把锁**串行化，并在 Drop 时把 env 恢复原状。
/// 范式同 `worlds::DeathmatchSwitch` / `invitations::InvitationSwitch`。
#[cfg(test)]
pub(crate) struct SubplotSwitch {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Vec<(&'static str, Option<String>)>,
}

#[cfg(test)]
impl SubplotSwitch {
    /// 只置总开关。
    pub(crate) fn set(on: bool) -> Self {
        Self::with(on, &[])
    }

    /// 置总开关 + 若干额外 env（如 `MUSE_SUBPLOT_SYNTHESIS_N`）；返回值存活期间取值稳定。
    /// ⚠️ `extra` 里不要再放总开关键，否则 Drop 时同键恢复两次会把状态留给下一个用例。
    pub(crate) fn with(on: bool, extra: &[(&'static str, &str)]) -> Self {
        let mut sw = Self::take_lock();
        std::env::set_var(ENV_SUBPLOT_CARDS_ENABLED, if on { "1" } else { "0" });
        for (k, v) in extra {
            sw.prev.push((k, std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        sw
    }

    /// 把总开关置成一个**原始字符串**（测试非法值回落用）。
    pub(crate) fn raw(value: &str) -> Self {
        let sw = Self::take_lock();
        std::env::set_var(ENV_SUBPLOT_CARDS_ENABLED, value);
        sw
    }

    /// 清空总开关 env（测试「默认值」用：默认必须是关闭）。
    pub(crate) fn cleared() -> Self {
        let sw = Self::take_lock();
        std::env::remove_var(ENV_SUBPLOT_CARDS_ENABLED);
        sw
    }

    /// 取锁并记下总开关原值（各构造器共用）。
    fn take_lock() -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = vec![(ENV_SUBPLOT_CARDS_ENABLED, std::env::var(ENV_SUBPLOT_CARDS_ENABLED).ok())];
        Self { _guard: guard, prev }
    }
}

#[cfg(test)]
impl Drop for SubplotSwitch {
    fn drop(&mut self) {
        for (k, v) in &self.prev {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

// ---------------- 参数化（VALIDATION.md §0.2 产品规则参数化，禁止写死） ----------------

/// 合成配方的源卡张数 env（默认 3：3×2★ → 1×3★ 是规格给的**初值**，不是承诺）。
const ENV_SYNTHESIS_SOURCE_COUNT: &str = "MUSE_SUBPLOT_SYNTHESIS_N";
const DEFAULT_SYNTHESIS_SOURCE_COUNT: i64 = 3;
/// 配方张数的安全区间：至少 2 张才叫「合成」，至多 10 张避免一次请求扫太多行。
const MIN_SYNTHESIS_SOURCE_COUNT: i64 = 2;
const MAX_SYNTHESIS_SOURCE_COUNT: i64 = 10;

/// 星级上限 env（默认 5：与模板星级 curation 0020 的 1-5★ 同一把尺子）。
const ENV_MAX_STAR_RATING: &str = "MUSE_SUBPLOT_MAX_STAR";
const DEFAULT_MAX_STAR_RATING: i64 = 5;
const MIN_MAX_STAR_RATING: i64 = 1;
const HARD_MAX_STAR_RATING: i64 = 10;

/// 合成一张卡需要几张同星源卡（运营可调；非法值回落默认，再 clamp 进安全区间）。
/// 「回收口的力度」是经济体最需要随真实通胀数据调的参数之一，绝不写死在代码里。
pub(crate) fn synthesis_source_count() -> i64 {
    read_i64_env(ENV_SYNTHESIS_SOURCE_COUNT, DEFAULT_SYNTHESIS_SOURCE_COUNT)
        .clamp(MIN_SYNTHESIS_SOURCE_COUNT, MAX_SYNTHESIS_SOURCE_COUNT)
}

/// 副本卡星级上限（运营可调）。到顶的卡不可再合成——回收口在顶端自然封口。
pub(crate) fn max_star_rating() -> i64 {
    read_i64_env(ENV_MAX_STAR_RATING, DEFAULT_MAX_STAR_RATING)
        .clamp(MIN_MAX_STAR_RATING, HARD_MAX_STAR_RATING)
}

/// 读整数 env（缺失/非法 → 默认值；范式同 `interventions::dream_quota_per_stage`）。
fn read_i64_env(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(default)
}

// ---------------- 枚举字面量 ----------------

/// 在手（可装进自定义房、可作合成材料）。
const STATUS_OWNED: &str = "owned";
/// 已作为合成材料销毁（软删：回收口必须可溯，§0.2 全链审计）。
const STATUS_CONSUMED: &str = "consumed";

/// 铸成来源：三层结算 ③ 世界线层的确定性产出（唯一的「打出来的卡」）。
pub(crate) const ORIGIN_SETTLEMENT: &str = "settlement";
/// 铸成来源：同星合成升级。
pub(crate) const ORIGIN_SYNTHESIS: &str = "synthesis";
/// 铸成来源：新手礼包 / 运营发放（§13 新人大礼包的「1 张低星副本卡」走这一档）。
/// 本模块自身不发礼包（那是 onboarding 的事），此处只定义口径与入口，故允许暂时无调用点。
#[allow(dead_code)]
pub(crate) const ORIGIN_GRANT: &str = "grant";

// ---------- 公示产出表的「副本卡侧视图」（§10【拍板 17】确定性产出，无 RNG） ----------
//
// 为什么本模块自己解析 `assembled_json`、而不给 `assembly::PayoutTier` 加字段：
// 副本卡的产出规格是**副本卡模块自己的事**，装配层不必知道有这种资产（少一处耦合、少一处漂移）。
// 两侧解析的是**同一个 `worldlineTiers` 数组、同一份档位序**，档位选取用**同一条规则**
// （门槛 ≤ 贡献分中门槛最高的一档，定点整数域比较），因此「历练/道具命中哪一档」与
// 「副本卡命中哪一档」恒为同一档——由 `card_tier_matches_progression_tier` 交叉断言锁死。
// 门槛唯一性（同分不歧义）由建模板期 `assembly::validate_skeleton_refs` 前置保证。

/// 产出表中与副本卡有关的部分（`/assembly/payoutTable`）。
/// `Default` = 空表 = **不发卡**（模板未声明即默认关闭，VALIDATION §0.1 的数据侧一层）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubplotCardPayouts {
    #[serde(default)]
    worldline_tiers: Vec<SubplotCardTier>,
}

/// 一档（只取副本卡关心的三个字段；`item`/`mileage` 由 progression 侧消费，此处忽略）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubplotCardTier {
    #[serde(default)]
    label: String,
    #[serde(default)]
    min_score: f64,
    /// 本档铸出的副本卡（`None` = 本档不发卡）。**无概率字段**——命中即发，不存在爆率。
    #[serde(default)]
    subplot_card: Option<SubplotCardSpec>,
}

/// 一档声明的副本卡规格（公示口径）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubplotCardSpec {
    /// 卡的星级。**产出封顶不可绕过**：> 实例星级即整张剔除（不降级、不替换），与装配层同口径。
    #[serde(default = "default_card_star_rating")]
    star_rating: i64,
    /// 卡面公示名（留空 → 回落档位 label）。
    #[serde(default)]
    label: String,
}

fn default_card_star_rating() -> i64 {
    1
}

impl SubplotCardPayouts {
    /// 从 `worlds.assembled_json` 解析出的 wrapper Value 中取副本卡产出表。
    /// 缺失 / 结构不符 → 空表（**不发卡**），与 `progression::payout_context_from_wrapper`
    /// 的保守退化同哲学：读不懂就什么都不发，绝不猜。
    pub(crate) fn from_wrapper_value(v: Option<&Value>) -> Self {
        v.and_then(|v| v.pointer("/assembly/payoutTable"))
            .and_then(|t| serde_json::from_value::<Self>(t.clone()).ok())
            .unwrap_or_default()
    }

    /// 是否声明了任何副本卡产出（用于跳过无谓的查询）。
    fn is_empty(&self) -> bool {
        self.worldline_tiers.is_empty()
    }

    /// 查表：取「门槛 ≤ 贡献分」中**门槛最高**的一档，再看该档有没有声明副本卡。
    ///
    /// 🔴 注意语义：命中的是**该分数所属的那一档**，而不是「有卡的最高档」——否则高分玩家会
    /// 掉回低档去捡卡，与历练/道具的档位判定漂移，「同分同产出」就不成立了。
    /// 纯查表、零随机：同一分数重复查恒得同一结果，可 replay。
    pub(crate) fn resolve(&self, score: f64) -> Option<(&SubplotCardTier, &SubplotCardSpec)> {
        let key = |min_score: f64| (min_score * crate::progression::CONTRIBUTION_SCALE).round() as i64;
        let s = key(score);
        let tier = self
            .worldline_tiers
            .iter()
            .filter(|t| key(t.min_score) <= s)
            .max_by_key(|t| key(t.min_score))?;
        tier.subplot_card.as_ref().map(|spec| (tier, spec))
    }
}

// ---------------- 唯一写入路径（§0.2 资产单一写入路径与全链审计） ----------------

/// 一张待铸的副本卡（`grant_card_tx` 的入参；字段多故用结构体，避免长参数列表）。
pub(crate) struct NewSubplotCard<'a> {
    pub owner_id: &'a str,
    pub star_rating: i64,
    pub label: &'a str,
    /// `ORIGIN_SETTLEMENT` / `ORIGIN_SYNTHESIS` / `ORIGIN_GRANT`。
    pub origin_kind: &'a str,
    /// 幂等键：与 `owner_id` 组成 DB 唯一约束。**必填，且必须由调用方保证同一次发放恒等**。
    pub grant_key: String,
    pub source_world_id: Option<&'a str>,
    pub source_template_id: Option<&'a str>,
    pub source_template_version: Option<i64>,
    /// 合成血缘（源卡 id，升序）。非合成卡传空。
    pub synthesized_from: Vec<String>,
}

/// 铸一张副本卡（**副本卡的唯一写入路径**，仿 `backpack::grant_item_tx` 惯例）。
///
/// 在调用方事务内执行——结算/合成回滚则卡同滚，绝无「结算失败仍发卡」。
/// 幂等落在 DB 唯一约束 `(owner_id, grant_key)`：命中即「已发放」，返回 `Ok(None)`（不二次发卡）；
/// 成功铸出返回 `Ok(Some(card_id))`。
///
/// 星级越界（≤ 0 或 > 运营上限）→ 直接拒绝（`BadRequest`），不静默钳位：
/// 钳位会把一次配置错误变成一张真实资产，事后无从分辨是运营意图还是 bug。
///
/// 新手礼包（§13「1 张低星副本卡」）与运营补发都走这里，只需给 `ORIGIN_GRANT` 与自己的
/// `grant_key`（如 `starter:{user_id}`），幂等自动成立。
pub(crate) async fn grant_card_tx(
    tx: &mut Transaction<'_, Any>,
    card: &NewSubplotCard<'_>,
) -> Result<Option<String>, ApiError> {
    let max_star = max_star_rating();
    if card.star_rating < 1 || card.star_rating > max_star {
        return Err(ApiError::BadRequest(format!(
            "副本卡星级非法：{}（合法区间 1-{max_star}）",
            card.star_rating
        )));
    }
    if card.grant_key.trim().is_empty() {
        return Err(ApiError::BadRequest("副本卡发放必须带幂等键".into()));
    }

    let card_id = new_id("sc");
    let res = sqlx::query(
        "INSERT INTO subplot_cards (id, owner_id, star_rating, label, origin_kind, grant_key, \
         source_world_id, source_template_id, source_template_version, synthesized_from_json, \
         status, consumed_into, acquired_at, consumed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'owned', NULL, $11, NULL)",
    )
    .bind(&card_id)
    .bind(card.owner_id)
    .bind(card.star_rating)
    .bind(card.label)
    .bind(card.origin_kind)
    .bind(&card.grant_key)
    .bind(card.source_world_id)
    .bind(card.source_template_id)
    .bind(card.source_template_version)
    .bind(serde_json::to_string(&card.synthesized_from).unwrap_or_else(|_| "[]".into()))
    .bind(now_ms())
    .execute(&mut **tx)
    .await;

    match res {
        Ok(_) => {
            tracing::info!(
                card_id = %card_id,
                owner_id = card.owner_id,
                star_rating = card.star_rating,
                origin_kind = card.origin_kind,
                "铸出副本卡"
            );
            Ok(Some(card_id))
        }
        // (owner_id, grant_key) 唯一键命中：该次发放已完成 → 幂等，不二次铸卡。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            tracing::info!(owner_id = card.owner_id, grant_key = %card.grant_key, "副本卡已发放，跳过");
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

// ---------------- 结算产出接点（三层结算 ③ 世界线层） ----------------

/// 世界线结算铸卡（由 `progression::settle_worldline_tx` 在**同一结算事务内**调用）。
///
/// 入参 `score` 是调用方已折算好的世界线贡献分（含崩塌系数），`star_cap` 是实例星级快照。
/// 与道具/历练发放**同事务、同幂等口径**：
/// ① 事务原子性——结算回滚则卡同滚；
/// ② 调用方 `world_contributions.settled_at` 的 CAS 占位——重复/并发结算根本走不到这里；
/// ③ 本函数的 `(owner_id, grant_key)` DB 唯一键——即使前两层被绕过也不会二次铸卡。
///
/// 返回铸出的卡摘要（`None` = 开关关闭 / 表未声明 / 该档无卡 / 超星级封顶 / 已发放过）。
pub(crate) async fn settle_subplot_card_tx(
    tx: &mut Transaction<'_, Any>,
    payouts: &SubplotCardPayouts,
    world_id: &str,
    character_id: &str,
    owner_id: &str,
    score: f64,
    star_cap: i64,
) -> Result<Option<Value>, ApiError> {
    // 开关未开 → 结算侧一张不铸（前门 404 之外的第二道保险：产出路径不经过端点）。
    if !subplot_cards_enabled() || payouts.is_empty() {
        return Ok(None);
    }
    let Some((tier, spec)) = payouts.resolve(score) else {
        return Ok(None);
    };

    // 产出封顶不可绕过：星级 > 实例星级即整张剔除（不降级、不替换），与装配层 culled_over_tier 同口径。
    if spec.star_rating > star_cap {
        tracing::warn!(
            world_id,
            character_id,
            star_rating = spec.star_rating,
            star_cap,
            "副本卡产出超出星级封顶，剔除"
        );
        return Ok(None);
    }
    // 星级非法（配置错误）→ 剔除并告警，不让一次配置错误变成一张真实资产、也不拖垮整笔结算。
    if spec.star_rating < 1 || spec.star_rating > max_star_rating() {
        tracing::warn!(world_id, character_id, star_rating = spec.star_rating, "副本卡星级非法，剔除");
        return Ok(None);
    }

    let label = if spec.label.trim().is_empty() { tier.label.clone() } else { spec.label.clone() };
    let (template_id, template_version) = world_template_ref_tx(tx, world_id).await?;

    // 幂等键 = 世界 × 角色 × 世界线层：同一世界同一张卡的世界线结算恒只铸一张
    // （与 ③ 层道具的 reward_hook_key `{world_id}:{cid}:worldline` 同一口径）。
    let grant_key = format!("{ORIGIN_SETTLEMENT}:{world_id}:{character_id}:worldline");
    let card = NewSubplotCard {
        owner_id,
        star_rating: spec.star_rating,
        label: &label,
        origin_kind: ORIGIN_SETTLEMENT,
        grant_key,
        source_world_id: Some(world_id),
        source_template_id: template_id.as_deref(),
        source_template_version: template_version,
        synthesized_from: Vec::new(),
    };
    let Some(card_id) = grant_card_tx(tx, &card).await? else {
        return Ok(None);
    };
    Ok(Some(json!({
        "id": card_id,
        "starRating": spec.star_rating,
        "label": label,
        "tier": tier.label,
    })))
}

/// 读世界的模板指针（内容蓝图解引用入口）。行缺失 → `(None, None)`，不报错：
/// 卡的来源标注缺一半也不该拖垮整笔结算（防御式，同 `load_payout_context_tx` 哲学）。
async fn world_template_ref_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
) -> Result<(Option<String>, Option<i64>), ApiError> {
    let row = sqlx::query("SELECT template_id, template_version FROM worlds WHERE id = $1")
        .bind(world_id)
        .fetch_optional(&mut **tx)
        .await?;
    match row {
        Some(r) => Ok((r.try_get("template_id").ok(), r.try_get("template_version").ok())),
        None => Ok((None, None)),
    }
}

// ---------------- 读侧：我的副本卡 ----------------

/// 一行 `subplot_cards` 的 JSON 投影（玩家可见面：星级 + 来源 + 血缘，**无任何数值/战力字段**）。
fn project_card(r: &sqlx::any::AnyRow) -> Result<Value, ApiError> {
    let synthesized_from: String = r.try_get("synthesized_from_json")?;
    Ok(json!({
        "id": r.try_get::<String, _>("id")?,
        "starRating": r.try_get::<i64, _>("star_rating")?,
        "label": r.try_get::<String, _>("label")?,
        "originKind": r.try_get::<String, _>("origin_kind")?,
        "status": r.try_get::<String, _>("status")?,
        "source": {
            "worldId": r.try_get::<Option<String>, _>("source_world_id")?,
            "templateId": r.try_get::<Option<String>, _>("source_template_id")?,
            "templateVersion": r.try_get::<Option<i64>, _>("source_template_version")?,
        },
        "synthesizedFrom": serde_json::from_str::<Value>(&synthesized_from).unwrap_or_else(|_| json!([])),
        "consumedInto": r.try_get::<Option<String>, _>("consumed_into")?,
        "acquiredAt": r.try_get::<i64, _>("acquired_at")?,
        "consumedAt": r.try_get::<Option<i64>, _>("consumed_at")?,
    }))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    /// `owned`（默认）/ `consumed`（已熔的材料，可溯）/ `all`。
    #[serde(default)]
    status: Option<String>,
}

/// GET /me/subplot-cards：我的副本卡（含星级与来源）。
///
/// 合成规则随响应一并公示（`synthesisRule`）——参数在服务端，前端不得写死一份副本。
async fn my_subplot_cards(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled()?;
    let filter = q.status.unwrap_or_else(|| STATUS_OWNED.to_string());
    if !matches!(filter.as_str(), STATUS_OWNED | STATUS_CONSUMED | "all") {
        return Err(ApiError::BadRequest("status 只接受 owned / consumed / all".into()));
    }

    let rows = sqlx::query(
        "SELECT id, star_rating, label, origin_kind, status, source_world_id, source_template_id, \
         source_template_version, synthesized_from_json, consumed_into, acquired_at, consumed_at \
         FROM subplot_cards WHERE owner_id = $1 AND ($2 = 'all' OR status = $3) \
         ORDER BY star_rating DESC, acquired_at DESC, id ASC",
    )
    .bind(&user.user_id)
    .bind(&filter)
    .bind(&filter)
    .fetch_all(&state.db)
    .await?;

    let cards: Vec<Value> = rows.iter().map(project_card).collect::<Result<_, _>>()?;
    Ok(Json(json!({
        "cards": cards,
        "synthesisRule": {
            "sourceCount": synthesis_source_count(),
            "maxStarRating": max_star_rating(),
        },
    })))
}

// ---------------- 写侧：同星合成（回收口） ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SynthesizeRequest {
    #[serde(default)]
    card_ids: Vec<String>,
}

/// 合成用的源卡快照（事务内逐张读出）。
struct SourceCard {
    id: String,
    star_rating: i64,
    label: String,
    source_world_id: Option<String>,
    source_template_id: Option<String>,
    source_template_version: Option<i64>,
}

/// POST /me/subplot-cards/synthesize：同星合成 N×n★ → 1×(n+1)★。
///
/// **消耗性操作**：源卡销毁（软删 `status='consumed'`，回收口必须可溯）+ 目标卡铸成，
/// 二者在**同一事务**内完成。
///
/// 幂等三重防线：
/// ① 事务原子性——任一步失败整笔回滚，不会「卡熔了但新卡没出来」；
/// ② 源卡 `status='owned'` 条件 UPDATE 的 CAS——重复/并发提交第二次抢不到源卡（0 行）即整笔回滚；
/// ③ `(owner_id, grant_key)` DB 唯一键（grant_key 由**升序源卡 id** 拼出，同一组源卡恒等）——
///    最后一道防线，重复提交绝不凭空多出一张卡。
/// 另可选带 `Idempotency-Key` 头：同 key 同载荷直接回缓存响应（不重放事务）。
async fn synthesize(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<SynthesizeRequest>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled()?;

    // ---- 前门校验（纯参数，不碰库） ----
    let required = synthesis_source_count();
    let ids: BTreeSet<String> =
        body.card_ids.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if ids.len() != body.card_ids.len() {
        return Err(ApiError::BadRequest("源卡 id 不得重复或为空".into()));
    }
    if ids.len() as i64 != required {
        return Err(ApiError::BadRequest(format!(
            "合成需要 {required} 张同星副本卡，实际提交 {}",
            ids.len()
        )));
    }
    let ordered: Vec<String> = ids.iter().cloned().collect();

    // 幂等键：载荷哈希按**升序 id** 算，提交顺序不同不算不同载荷。
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash =
        idempotency::hash_payload(&serde_json::to_vec(&json!({ "cardIds": ordered })).unwrap_or_default());
    let guard =
        idempotency::guard(&state.db, &user.user_id, "subplot.synthesize", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    let now = now_ms();
    let mut tx = state.db.begin().await?;

    // ---- 读源卡（owner 隔离：不属于本人的卡一律「不存在」，不泄露他人资产） ----
    let mut sources: Vec<SourceCard> = Vec::with_capacity(ordered.len());
    for id in &ordered {
        let row = sqlx::query(
            "SELECT id, star_rating, label, status, source_world_id, source_template_id, \
             source_template_version FROM subplot_cards WHERE id = $1 AND owner_id = $2",
        )
        .bind(id)
        .bind(&user.user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let row = row.ok_or_else(|| ApiError::BadRequest(format!("副本卡不存在或不属于你：{id}")))?;
        let status: String = row.try_get("status")?;
        if status != STATUS_OWNED {
            return Err(ApiError::Conflict(format!("副本卡已不在手中（{status}）：{id}")));
        }
        sources.push(SourceCard {
            id: row.try_get("id")?,
            star_rating: row.try_get("star_rating")?,
            label: row.try_get("label")?,
            source_world_id: row.try_get("source_world_id")?,
            source_template_id: row.try_get("source_template_id")?,
            source_template_version: row.try_get("source_template_version")?,
        });
    }

    // ---- 同星校验 + 星级上限 ----
    let star = sources[0].star_rating;
    if sources.iter().any(|c| c.star_rating != star) {
        return Err(ApiError::BadRequest("合成要求全部源卡同星级".into()));
    }
    let max_star = max_star_rating();
    if star >= max_star {
        return Err(ApiError::BadRequest(format!("{star}★ 已达星级上限（{max_star}★），无法继续合成")));
    }
    let target_star = star + 1;

    // ---- 铸目标卡（蓝图来源按**源卡 id 升序取首张**确定性继承；血缘全量留存） ----
    // 先铸后熔：幂等防线③（唯一键）在动任何源卡之前就把重复提交挡下，事务里不会留下
    // 「唯一键已冲突却继续用同一个事务」的状态（Postgres 下语句失败即整事务作废，必须立刻退出）。
    let blueprint = &sources[0];
    let grant_key = format!("{ORIGIN_SYNTHESIS}:{}", ordered.join("+"));
    let new_card = NewSubplotCard {
        owner_id: &user.user_id,
        star_rating: target_star,
        label: &blueprint.label,
        origin_kind: ORIGIN_SYNTHESIS,
        grant_key,
        source_world_id: blueprint.source_world_id.as_deref(),
        source_template_id: blueprint.source_template_id.as_deref(),
        source_template_version: blueprint.source_template_version,
        synthesized_from: ordered.clone(),
    };
    let card_id = grant_card_tx(&mut tx, &new_card)
        .await?
        .ok_or_else(|| ApiError::Conflict("这组源卡已经合成过了".into()))?;

    // ---- 销毁源卡（CAS：只熔仍在手的卡；抢不到即整笔回滚，新卡随之消失） ----
    for c in &sources {
        let res = sqlx::query(
            "UPDATE subplot_cards SET status = $1, consumed_at = $2, consumed_into = $3 \
             WHERE id = $4 AND owner_id = $5 AND status = $6",
        )
        .bind(STATUS_CONSUMED)
        .bind(now)
        .bind(&card_id)
        .bind(&c.id)
        .bind(&user.user_id)
        .bind(STATUS_OWNED)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            // 并发/重复提交：另一笔已经把它熔了 → 整笔回滚（tx 未 commit，drop 即回滚）。
            return Err(ApiError::Conflict(format!("副本卡状态已变化，请刷新后重试：{}", c.id)));
        }
    }

    // 全链审计（§0.2）：回收口的每一次销毁都留痕。
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES ($1, $2, 'user', 'subplot.card_synthesized', $3, $4, $5)",
    )
    .bind(new_id("aud"))
    .bind(&user.user_id)
    .bind(&card_id)
    .bind(format!("{}x{star}star->{target_star}star|sources={}", ordered.len(), ordered.join(",")))
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let response = json!({
        "card": {
            "id": card_id,
            "starRating": target_star,
            "label": blueprint.label,
            "originKind": ORIGIN_SYNTHESIS,
            "status": STATUS_OWNED,
            "source": {
                "worldId": blueprint.source_world_id,
                "templateId": blueprint.source_template_id,
                "templateVersion": blueprint.source_template_version,
            },
            "synthesizedFrom": ordered,
        },
        "consumedCardIds": ordered,
        "sourceCount": required,
    });
    guard.store_response(&state.db, &response.to_string()).await?;
    Ok(Json(response))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/subplot-cards", get(my_subplot_cards))
        .route("/me/subplot-cards/synthesize", post(synthesize))
}
