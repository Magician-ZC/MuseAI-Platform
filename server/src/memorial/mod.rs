//! 传世卡 · 封卷与遗作馆（R2 最小版）。总规格 `docs/build/spec-world-ecosystem.md` §12【拍板 23】。
//!
//! **死亡 = 传记封卷，不是资产清零。**（「输」的张力与资产安全感的三角闭合：
//! 会死【张力】、死得其所【仲裁合理】、死有所归【传世卡】。）
//!
//! 卡死后转为「传世卡」：**只读、入遗作馆陈列、不可再入世界**；道具归账户背包；
//! 与其有羁绊的**在世**角色获得「故人」印记（你的死成为别人故事的一部分）。
//! **内核可复制，履历不可复制**——同内核开新卡是转世（双胞胎），不是复活：它没死过那一次。
//!
//! 端点：
//! GET  /memorial/characters            遗作馆陈列（列表，**只读**）
//! GET  /memorial/characters/{id}       传世卡详情：累计人生（历练 / 传记 / 足迹 / 谁还记得他）
//! GET  /me/memorial/marks              我的角色获得的「故人」印记
//! POST /me/characters/{id}/memorial    封卷（**本模块唯一的写端点**，不在 `/memorial` 命名空间下）
//!
//! ────────────────────────────────────────────────────────────────────────────
//! 🔴 六条硬约束，改本模块前先读完
//! ────────────────────────────────────────────────────────────────────────────
//!
//! ① **传世卡不可再入世界 —— 拦截点复用 `withdrawn`**（§12 原文）。
//!    唯一有效拦截点是 `worlds::join_world`，而它的资格查询是一条**列名写死**的 SELECT
//!    （`owner_id, moderation, withdrawn, mileage, source_fingerprint, pristine`）：
//!    新增 `memorial_status` 列它根本读不到。因此封卷是**一次原子双写**——
//!    `memorial_status='sealed'`（语义状态 + 幂等 CAS）**与** `withdrawn=1`（复用 join 已有那道门，
//!    命中即 `character_withdrawn`）。语义也吻合：`withdrawn` 的既有含义就是「停止后续投放」。
//!    前提是 `withdrawn` 为**单向门**——全仓不存在把它置回 0 的 SQL（`red_line_*` 源码级断言守死）。
//!
//! ② **公共事实不可回滚**（§0.3）。死亡是**已落定**的公共事实，封卷只改卡自己的状态：
//!    不写 `world_events`、不写 `worlds.narrative_state_json`、不改 `consent_requests`、
//!    不删 `world_members`（足迹是履历的一部分）。本模块对世界线**只读不写**
//!    （`red_line_never_rewrites_worldline` 源码级断言）。
//!
//! ③ **「故人」印记绝不进 `worlds.narrative_state_json`**（§0.1 平权红线）。
//!    那一列每 tick 经 `runtime::build_seed_state` 原样回灌进引擎 `RoundInput.state`，
//!    写进去就等于把结算侧记账喂给了决策。印记落**独立表** `memorial_marks`，引擎侧零读取路径
//!    （口径同 0025 贡献账本 / 0030 critic 报告 / 0032 副本卡）。
//!
//! ④ **无隐藏数值**（§12 原文「全是显性资产，无隐藏数值」）。传世卡与「故人」印记
//!    **不带任何加成/系数/强度**：卡的价值 = 历练 + 传记 + 足迹 + 羁绊，全部是已存在的显性资产。
//!    印记表没有强度列，也永远不会有——它是叙事事实，不是 buff。
//!
//! ⑤ **未验证功能默认关闭**（VALIDATION.md §0.1）。整块能力由 `MUSE_MEMORIAL` 控制，**默认关闭**：
//!    关闭时四个端点全 404（不泄露「平台有这个未开放功能」），**封卷也不发生**——
//!    已封卷的卡不因关阀而"复活"（那是状态，不是功能），只是读不出。范式抄 `worlds::deathmatch_enabled`。
//!
//! ⑥ **封卷幂等 = DB 条件 UPDATE（CAS）**。`WHERE memorial_status = 'living'` 抢到才继续：
//!    重复封卷命中 0 行即整段短路，**不重复归还道具、不重复打印记**。印记另有
//!    `memorial_marks(character_id, deceased_character_id)` 唯一键作第二道闸。
//!
//! ────────────────────────────────────────────────────────────────────────────
//! 关于「道具归账户背包」的实现口径（与直觉相反，务必读）
//! ────────────────────────────────────────────────────────────────────────────
//! §12 原文：「道具归账户背包（**道具本为账户资产**）」。括号里那句就是实现口径——
//! `backpacks` 行的归属键是 `user_id` 而不是角色，道具**从来没有离开过账户**：
//! `POST /worlds/{id}/carry` 做的只是把行标成 `carried`/`sealed` 并钉上 `carried_world_id`。
//! 所以「归还」= **解除携带**（`carried|sealed → owned`，清 `carried_world_id` 与 S-5 降档覆盖），
//! 而**不是**调 `backpack::grant_item_tx`：那是 INSERT 类的**发货**路径，对一件本就在账户里的道具
//! 再发一次会**凭空多出一行**——一次死亡把道具变成两件，直接违反 §0.2 资产守恒与全链审计。
//! 因此本模块**绝不 INSERT `backpacks`**（`red_line_never_mints_items` 源码级断言守死）：
//! 归还只有 UPDATE，道具总数在封卷前后**恒等**。
//! （若将来出现「只存在于世界、尚未入账」的道具形态，那时的归还才该走 `grant_item_tx`。）

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Any, AnyPool, Row, Transaction};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;

#[cfg(test)]
mod tests;

// ---------------- 运营开关（VALIDATION.md §0.1 未验证功能默认关闭） ----------------

/// 传世卡运营开关环境变量。
const ENV_MEMORIAL_ENABLED: &str = "MUSE_MEMORIAL";

/// 传世卡默认值 = **关闭**。
///
/// 🔴 死亡机制整体属 VALIDATION.md §2 中 T5 才验证的范围（生死状排在 T5「规模化」，
/// T0-T2 明写「暂不验证：死亡」）。代码合并不等于对用户开放——必须运营显式打开本开关。
const DEFAULT_MEMORIAL_ENABLED: bool = false;

/// 传世卡能力是否已由运营开启（env 覆盖 + 默认常量，范式同 `worlds::deathmatch_enabled`
/// 与 `subplot::subplot_cards_enabled`——本仓库尚无配置表，env 是当前唯一的运营开关形态；
/// 将来配置表落地后只改本函数内部，调用点与降级语义不变）。
///
/// 开关是**全局急停阀**，作用在读取侧与封卷侧：关掉之后不再发生任何封卷、遗作馆读不出；
/// 再打开则原样恢复。**已封卷的卡不因关阀而回到在世**——封卷是状态转换（且是单向的），
/// 不是可开可关的功能；关阀只让它暂时不可见。
pub fn memorial_enabled() -> bool {
    match std::env::var(ENV_MEMORIAL_ENABLED) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            // 配错不静默开启未验证的死亡机制：回落默认（关闭）。
            _ => DEFAULT_MEMORIAL_ENABLED,
        },
        Err(_) => DEFAULT_MEMORIAL_ENABLED,
    }
}

/// 开关门：关闭时整块能力**不存在**（404，而非 403）——不向外泄露「平台有这个未开放功能」。
/// 每个端点第一行都调它，读端点同样调（读取侧降级）。
fn ensure_enabled() -> Result<(), ApiError> {
    if memorial_enabled() {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 测试专用：传世卡相关 env 的 RAII 夹具。
///
/// 这些 env 是**进程级**的，而本模块用例与其它模块同属一个测试二进制、默认并发跑，
/// 故所有对 env 敏感的用例共用**同一把锁**串行化，并在 Drop 时把 env 恢复原状。
/// 范式同 `worlds::DeathmatchSwitch` / `subplot::SubplotSwitch`。
#[cfg(test)]
pub(crate) struct MemorialSwitch {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Vec<(&'static str, Option<String>)>,
}

#[cfg(test)]
impl MemorialSwitch {
    /// 只置总开关。
    pub(crate) fn set(on: bool) -> Self {
        Self::with(on, &[])
    }

    /// 置总开关 + 若干额外 env（如 `MUSE_MEMORIAL_BOND_MIN`）；返回值存活期间取值稳定。
    /// ⚠️ `extra` 里不要再放总开关键，否则 Drop 时同键恢复两次会把状态留给下一个用例。
    pub(crate) fn with(on: bool, extra: &[(&'static str, &str)]) -> Self {
        let mut sw = Self::take_lock();
        std::env::set_var(ENV_MEMORIAL_ENABLED, if on { "1" } else { "0" });
        for (k, v) in extra {
            sw.prev.push((k, std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        sw
    }

    /// 把总开关置成一个**原始字符串**（测试非法值回落用）。
    pub(crate) fn raw(value: &str) -> Self {
        let sw = Self::take_lock();
        std::env::set_var(ENV_MEMORIAL_ENABLED, value);
        sw
    }

    /// 清空总开关 env（测试「默认值」用：默认必须是关闭）。
    pub(crate) fn cleared() -> Self {
        let sw = Self::take_lock();
        std::env::remove_var(ENV_MEMORIAL_ENABLED);
        sw
    }

    fn take_lock() -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = vec![(ENV_MEMORIAL_ENABLED, std::env::var(ENV_MEMORIAL_ENABLED).ok())];
        Self { _guard: guard, prev }
    }
}

#[cfg(test)]
impl Drop for MemorialSwitch {
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

/// 「够得上『故人』」的羁绊强度阈值 env（默认 0.0 = **有关系记录即算羁绊**）。
///
/// 强度 = `max(|trust|, |affinity|, |fear|, |debt|)`，取绝对值是刻意的：§12 说的是
/// **羁绊**对方，不是「朋友」——「你的死成为别人故事的一部分」对宿敌同样成立（恨也是羁绊）。
/// 这个折算**只用于挑选打印记的对象**，算完即抛，不落库、不进任何判定（§12 无隐藏数值）。
const ENV_BOND_MIN_INTENSITY: &str = "MUSE_MEMORIAL_BOND_MIN";
const DEFAULT_BOND_MIN_INTENSITY: f64 = 0.0;

/// 遗作馆单页条数 env（默认 20，clamp [1,100]——上限防一次扫全表）。
const ENV_HALL_PAGE_SIZE: &str = "MUSE_MEMORIAL_PAGE_SIZE";
const DEFAULT_HALL_PAGE_SIZE: i64 = 20;
const MIN_HALL_PAGE_SIZE: i64 = 1;
const MAX_HALL_PAGE_SIZE: i64 = 100;

/// 羁绊强度阈值（运营可调；缺失/非法/负数 → 默认 0.0）。
/// 「多深的关系才算故人」是最需要随真实数据调的产品参数之一，绝不写死。
fn bond_min_intensity() -> f64 {
    std::env::var(ENV_BOND_MIN_INTENSITY)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(DEFAULT_BOND_MIN_INTENSITY)
}

/// 遗作馆默认页大小（运营可调）。
fn hall_page_size() -> i64 {
    std::env::var(ENV_HALL_PAGE_SIZE)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_HALL_PAGE_SIZE)
        .clamp(MIN_HALL_PAGE_SIZE, MAX_HALL_PAGE_SIZE)
}

// ---------------- 枚举字面量 ----------------

/// 在世（**全部历史行的默认值**，行为零变化）。
const STATUS_LIVING: &str = "living";
/// 传世卡：只读、入遗作馆、不可再入世界。`living → sealed` **单向**，无反向转换。
const STATUS_SEALED: &str = "sealed";
/// 印记种类：故人。
const MARK_DEPARTED: &str = "departed";

/// 不可逆事件类别：死亡（与引擎 `IrreversibleRules::classify` 产出的 `event_kind` 逐字对齐）。
const EVENT_KIND_DEATH: &str = "death";

// ---------------- 唯一写入路径：封卷（§0.2 资产单一写入路径与全链审计） ----------------

/// 一次封卷的回执（读取面与日志共用；**无任何数值字段**）。
#[derive(Debug, Default)]
pub(crate) struct SealOutcome {
    /// 本次调用是否真的完成了状态转换。`false` = 已是传世卡（幂等短路），
    /// 此时下面两个计数恒为 0——**这就是「不重复发道具、不重复打印记」的直接证据**。
    pub sealed: bool,
    /// 归还账户背包（解除携带）的道具行数。
    pub items_returned: u64,
    /// 新打出的「故人」印记条数。
    pub marks_granted: u64,
}

/// 封卷（**传世卡状态的唯一写入路径**，仿 `backpack::grant_item_tx` / `subplot::grant_card_tx` 惯例）。
///
/// 在调用方事务内执行——任一步失败则整笔回滚，绝无「卡封了但道具没还」或「印记打了但卡没封」。
///
/// 🔴 幂等的全部重量压在第一步的 CAS 上：`WHERE memorial_status = 'living'` 抢到才继续。
/// 重复封卷（重试 / 并发 / 将来 runtime 自动接线与玩家手动认领撞车）命中 0 行即整段短路，
/// 返回 `sealed: false`。这不是"寄生在调用方判定上"的幂等——它是 DB 级的状态转换沿。
///
/// 参数 `world_id` = 死于哪个世界（传记落款 + 归还道具的范围 + 找羁绊的范围）。
///
/// **不做什么**（§0.3 公共事实不可回滚）：不写 `world_events`、不写 `narrative_state_json`、
/// 不改 `consent_requests`、不删 `world_members`。死亡已经发生过了，封卷不改写它。
pub(crate) async fn seal_character_tx(
    tx: &mut Transaction<'_, Any>,
    character_id: &str,
    owner_id: &str,
    world_id: &str,
) -> Result<SealOutcome, ApiError> {
    let sealed_at = now_ms();

    // ① 状态转换 CAS（幂等闸）。**双写**：memorial_status 是语义状态；withdrawn 是 join 的拦截点
    //    （`worlds::join_world` 读 withdrawn，读不到 memorial_status——见模块头 ①）。
    let res = sqlx::query(
        "UPDATE cloud_characters \
         SET memorial_status = $1, memorial_sealed_at = $2, memorial_world_id = $3, withdrawn = 1 \
         WHERE id = $4 AND owner_id = $5 AND memorial_status = $6",
    )
    .bind(STATUS_SEALED)
    .bind(sealed_at)
    .bind(world_id)
    .bind(character_id)
    .bind(owner_id)
    .bind(STATUS_LIVING)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        // 已是传世卡（或卡不存在 / 不属本人）→ 幂等短路：一件道具不发、一枚印记不打。
        return Ok(SealOutcome::default());
    }

    // ② 道具归账户背包 = **解除携带**（见模块头「与直觉相反」一节：道具本就在账户里，
    //    再走一次发货会凭空多出一行）。同时清掉 S-5 转译入场的降档覆盖——那是"在那个世界里"的
    //    临时形态，人都不在了，覆盖不该跟着回账户。
    let returned = sqlx::query(
        "UPDATE backpacks \
         SET status = 'owned', carried_world_id = NULL, power_tier_override = NULL, \
             effect_tags_override = NULL \
         WHERE user_id = $1 AND carried_world_id = $2 AND status IN ('carried', 'sealed')",
    )
    .bind(owner_id)
    .bind(world_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    // ③ 「故人」印记：与死者有羁绊的**在世**角色。
    let marks = grant_departed_marks_tx(tx, character_id, world_id, sealed_at).await?;

    tracing::info!(
        character_id,
        owner_id,
        world_id,
        items_returned = returned,
        marks_granted = marks,
        "角色卡封卷为传世卡"
    );
    Ok(SealOutcome { sealed: true, items_returned: returned, marks_granted: marks })
}

/// 给与死者有羁绊的**在世**角色打「故人」印记（§12「你的死成为别人故事的一部分」）。
///
/// 羁绊来源：`worlds.narrative_state_json` 的 `relations`（引擎 `relation_dynamics` 的产出，
/// `from`/`to` 即 `cloud_character_id`）。本函数**只读那一列，绝不写它**（§0.1 平权红线）。
///
/// 三条筛选（全部是"少打"的方向，宁可漏不可滥）：
/// - 关系任一端是死者，取**另一端**（双向都算：他记得你、你记得他，是同一段羁绊）；
/// - 强度 ≥ `MUSE_MEMORIAL_BOND_MIN`（默认 0 = 有记录即算）；
/// - 对方必须是**在世**的云端角色卡（`memorial_status='living'`）。
///   给已封卷的卡打印记等于改写只读的传世卡，与「传世卡只读」直接冲突；
///   引擎的世界固有角色（NPC）没有 `cloud_characters` 行，天然被这一条过滤掉。
///
/// 幂等：`memorial_marks(character_id, deceased_character_id)` 唯一键——撞键即静默跳过。
async fn grant_departed_marks_tx(
    tx: &mut Transaction<'_, Any>,
    deceased_id: &str,
    world_id: &str,
    granted_at: i64,
) -> Result<u64, ApiError> {
    let state_json: Option<String> =
        sqlx::query_scalar("SELECT narrative_state_json FROM worlds WHERE id = $1")
            .bind(world_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(state_json) = state_json else { return Ok(0) };
    // 结构损坏 / 空状态 → 一枚印记不打（保守降级，绝不因解析失败中断封卷）。
    let Ok(state) = serde_json::from_str::<Value>(&state_json) else { return Ok(0) };

    let min_intensity = bond_min_intensity();
    // BTreeSet：对端去重 + **确定性遍历序**（同一份状态恒产出同一批印记，可 replay 复算）。
    let mut counterparts: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for rel in state.get("relations").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        let from = rel.get("from").and_then(Value::as_str).unwrap_or("");
        let to = rel.get("to").and_then(Value::as_str).unwrap_or("");
        let other = if from == deceased_id {
            to
        } else if to == deceased_id {
            from
        } else {
            continue;
        };
        if other.is_empty() || other == deceased_id {
            continue;
        }
        // 强度只用于"够不够格打印记"的挑选，算完即抛——不落库、不进任何判定（§12 无隐藏数值）。
        let intensity = ["trust", "affinity", "fear", "debt"]
            .iter()
            .filter_map(|k| rel.get(*k).and_then(Value::as_f64))
            .map(f64::abs)
            .fold(0.0_f64, f64::max);
        if intensity >= min_intensity {
            counterparts.insert(other.to_string());
        }
    }

    let mut granted = 0_u64;
    for other in counterparts {
        // 对端必须是**在世**的云端角色卡（NPC 无此行 → 天然跳过；传世卡只读 → 不再接收印记）。
        let row = sqlx::query(
            "SELECT owner_id FROM cloud_characters WHERE id = $1 AND memorial_status = $2",
        )
        .bind(&other)
        .bind(STATUS_LIVING)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else { continue };
        let other_owner: String = row.try_get("owner_id")?;

        let res = sqlx::query(
            "INSERT INTO memorial_marks \
             (id, character_id, owner_id, deceased_character_id, world_id, kind, granted_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(new_id("mm"))
        .bind(&other)
        .bind(&other_owner)
        .bind(deceased_id)
        .bind(world_id)
        .bind(MARK_DEPARTED)
        .bind(granted_at)
        .execute(&mut **tx)
        .await;
        match res {
            Ok(_) => granted += 1,
            // (character_id, deceased_character_id) 唯一键命中：已记得这位故人 → 幂等，不重复打。
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(granted)
}

// ---------------- 死亡公共事实的服务端权威核验 ----------------

/// 「这张卡确实死了」的证据（服务端权威，§9.6：绝不接受客户端声明"我死了"）。
struct DeathEvidence {
    world_id: String,
}

/// 核验死亡是否**已落定为公共事实**，返回死于哪个世界；无证据 → `None`（fail-closed）。
///
/// 两条证据必须同时成立，缺一不可：
/// - **(a) 当事人已授权**：`consent_requests` 有一条 `event_kind='death'` / `status='approved'`
///   且 subject 含本卡的记录（同意制世界的死亡授权闸，`runtime::create_consents_for_round` 落库）；
/// - **(b) 引擎确已落定**：该世界 `narrative_state_json` 的 `narrative.pendingConsents`
///   **不再**含 `{subject: 本卡, eventKind: 'death'}`。
///
/// 为什么必须两条：授权 ≠ 死亡。引擎在**下一拍**才凭 `approved_consents` 回灌落定该结果
/// （`gate_consents`），落定的同时把对应 pending 条目清除（golden 用例
/// `golden_world_death_requires_consent_then_lands` 钉死了这个先后关系）。
/// 只看 (a) 会把「刚点了同意、世界还没跑下一拍」的活角色误封卷——那是**捏造死亡**，
/// 比漏封严重得多。世界被暂停/熔断而永远跑不到落定那一拍时，(b) 恒不成立 → 永不封卷（保守安全默认）。
///
/// ⚠️ **已知缺口（生死状档）**：`Lethality::Deathmatch` 世界入场即签、事后不再征询，
/// 引擎**不产 ConsentRequested**，故 (a) 恒不成立 → 该档下本函数永远查不到证据。
/// 这不是 bug 而是本批次的边界：生死状档本身也默认关闭（`MUSE_LETHALITY_DEATHMATCH`），
/// 且自动封卷的正解是由死亡落定处直接调 `seal_character_tx`（见模块尾「接线待办」）。
async fn find_death_evidence(
    db: &AnyPool,
    character_id: &str,
) -> Result<Option<DeathEvidence>, ApiError> {
    let rows = sqlx::query(
        "SELECT world_id, subject_character_ids FROM consent_requests \
         WHERE event_kind = $1 AND status = 'approved' \
         ORDER BY resolved_at ASC, id ASC",
    )
    .bind(EVENT_KIND_DEATH)
    .fetch_all(db)
    .await?;

    for r in &rows {
        let subjects_json: String = r.try_get("subject_character_ids")?;
        // 精确匹配 JSON 数组元素，绝不用子串包含（`chA` 会命中 `chAB`）。
        let hit = serde_json::from_str::<Vec<String>>(&subjects_json)
            .map(|list| list.iter().any(|s| s == character_id))
            .unwrap_or(false);
        if !hit {
            continue;
        }
        let world_id: String = r.try_get("world_id")?;
        if death_has_landed(db, &world_id, character_id).await? {
            return Ok(Some(DeathEvidence { world_id }));
        }
    }
    Ok(None)
}

/// 证据 (b)：该世界的 `narrative.pendingConsents` 已不含本卡的 death 条目（= 引擎已落定并清账）。
///
/// 世界行缺失 / 状态 JSON 解析失败 → 返回 `false`（**保守：查不到就当没落定**，
/// 宁可拒绝封卷也不凭空判人死亡）。
async fn death_has_landed(db: &AnyPool, world_id: &str, character_id: &str) -> Result<bool, ApiError> {
    let state_json: Option<String> =
        sqlx::query_scalar("SELECT narrative_state_json FROM worlds WHERE id = $1")
            .bind(world_id)
            .fetch_optional(db)
            .await?;
    let Some(state_json) = state_json else { return Ok(false) };
    let Ok(state) = serde_json::from_str::<Value>(&state_json) else { return Ok(false) };
    // 空状态（`{}`，世界还没跑过任何一拍）也视为"没落定"：没跑过就不可能死过。
    let Some(narrative) = state.get("narrative") else { return Ok(false) };
    let still_pending = narrative
        .get("pendingConsents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .any(|p| {
            p.get("subject").and_then(Value::as_str) == Some(character_id)
                && p.get("eventKind").and_then(Value::as_str) == Some(EVENT_KIND_DEATH)
        });
    Ok(!still_pending)
}

// ---------------- 写侧：封卷（本模块唯一的写端点） ----------------

/// POST /me/characters/{id}/memorial —— 把一张**已死**的卡封卷为传世卡。
///
/// 服务端权威：只认 `find_death_evidence` 查出的公共事实，**不接受任何客户端声明**。
/// 无证据 → 409（可解冲突：世界跑到落定那一拍后重试即可通过），绝不 fail-open。
///
/// 幂等两层：① `Idempotency-Key`（覆盖同一次点击的 HTTP 重试，与 `worlds.join` 同款）；
/// ② DB 状态 CAS（覆盖跨请求 / 并发 / 将来自动接线撞车）——第二层才是真正的那一层。
async fn seal_memorial(
    State(state): State<AppState>,
    user: AuthUser,
    Path(character_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled()?;
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash =
        idempotency::hash_payload(&serde_json::to_vec(&json!({ "characterId": &character_id })).unwrap_or_default());
    let guard =
        idempotency::guard(&state.db, &user.user_id, "memorial.seal", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    // 归属校验（§9.6 服务端权威）：只有卡的主人能为它封卷。
    let row = sqlx::query("SELECT owner_id, memorial_status FROM cloud_characters WHERE id = $1")
        .bind(&character_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner_id: String = row.try_get("owner_id")?;
    let current_status: String = row.try_get("memorial_status")?;
    if owner_id != user.user_id {
        return Err(ApiError::Forbidden);
    }

    // 已封卷 → 直接返回既有回执（幂等；不再核验死亡证据，也不再碰任何资产）。
    if current_status == STATUS_SEALED {
        let response = json!({
            "characterId": character_id,
            "sealed": false,
            "memorialStatus": STATUS_SEALED,
            "itemsReturned": 0,
            "marksGranted": 0,
            "note": "该角色卡已封卷为传世卡（只读、不可再入世界）",
        });
        guard.store_response(&state.db, &response.to_string()).await?;
        return Ok(Json(response));
    }

    let Some(evidence) = find_death_evidence(&state.db, &character_id).await? else {
        return Err(ApiError::Conflict(
            "该角色没有已落定的死亡记录，不可封卷。封卷只承接世界里真实发生过的死亡——\
             公共事实不可回滚，也不可凭空捏造"
                .into(),
        ));
    };

    let mut tx = state.db.begin().await?;
    let outcome = seal_character_tx(&mut tx, &character_id, &user.user_id, &evidence.world_id).await?;
    tx.commit().await?;

    let response = json!({
        "characterId": character_id,
        "sealed": outcome.sealed,
        "memorialStatus": STATUS_SEALED,
        "worldId": evidence.world_id,
        "itemsReturned": outcome.items_returned,
        "marksGranted": outcome.marks_granted,
        "note": "已封卷为传世卡：只读、入遗作馆陈列、不可再入世界。道具已归账户背包；\
                 同内核开新卡是转世，不是复活",
    });
    guard.store_response(&state.db, &response.to_string()).await?;
    Ok(Json(response))
}

// ---------------- 读侧：遗作馆（**全部只读**） ----------------

/// 从 `card_json` 取展示名（口径同 `worlds::character_display_name`）。
fn display_name(card_json: &str) -> String {
    serde_json::from_str::<Value>(card_json)
        .ok()
        .and_then(|v| v.pointer("/identity/name").and_then(Value::as_str).map(str::to_string))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "无名者".to_string())
}

#[derive(Debug, Deserialize)]
struct HallQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
}

/// GET /memorial/characters —— 遗作馆陈列（分页，按封卷时刻倒序）。
///
/// 🔴 **角色面具**（§14 恨隔面具原则）：响应只出**角色**维度的事实，
/// **不出 `owner_id`/昵称/任何真人身份**。遗作馆是角色的墓园，不是玩家名录。
async fn memorial_hall(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<HallQuery>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled()?;
    let limit = q.limit.unwrap_or_else(hall_page_size).clamp(MIN_HALL_PAGE_SIZE, MAX_HALL_PAGE_SIZE);
    let offset = q.offset.unwrap_or(0).max(0);

    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cloud_characters WHERE memorial_status = $1")
            .bind(STATUS_SEALED)
            .fetch_one(&state.db)
            .await?;

    // 次级排序键 id 不可省：同毫秒封卷时行序由 DB 决定，分页会漏行/重行。
    let rows = sqlx::query(
        "SELECT cc.id AS id, cc.card_json AS card_json, cc.mileage AS mileage, \
                cc.avatar_url AS avatar_url, cc.memorial_sealed_at AS sealed_at, \
                cc.memorial_world_id AS world_id, w.title AS world_title \
         FROM cloud_characters cc LEFT JOIN worlds w ON w.id = cc.memorial_world_id \
         WHERE cc.memorial_status = $1 \
         ORDER BY cc.memorial_sealed_at DESC, cc.id ASC LIMIT $2 OFFSET $3",
    )
    .bind(STATUS_SEALED)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let mut characters = Vec::with_capacity(rows.len());
    for r in &rows {
        let card_json: String = r.try_get("card_json")?;
        characters.push(json!({
            "id": r.try_get::<String, _>("id")?,
            "name": display_name(&card_json),
            "avatarUrl": r.try_get::<Option<String>, _>("avatar_url")?,
            // 历练：显性资产之一（§12「卡的价值 = 累计人生」）。只作陈列，绝不进引擎决策。
            "mileage": r.try_get::<i64, _>("mileage")?,
            "sealedAt": r.try_get::<Option<i64>, _>("sealed_at")?,
            "sealedIn": {
                "worldId": r.try_get::<Option<String>, _>("world_id")?,
                "title": r.try_get::<Option<String>, _>("world_title")?,
            },
        }));
    }

    Ok(Json(json!({
        "characters": characters,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// GET /memorial/characters/{id} —— 传世卡详情：这张卡的**累计人生**。
///
/// 四段显性资产（§12「历练/羁绊/传记/足迹，全是显性资产，无隐藏数值」）：
/// - `mileage` 历练 · `biography` 传记（卡面 + 封卷落款）
/// - `footprints` 足迹（去过哪些世界；`world_members` 一行不删，死亡不抹掉走过的路）
/// - `remembrance` 羁绊（谁带着他的「故人」印记）
///
/// 在世的卡在这里查不到（404）——遗作馆只陈列传世卡。
async fn memorial_detail(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(character_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    ensure_enabled()?;
    let row = sqlx::query(
        "SELECT cc.card_json AS card_json, cc.mileage AS mileage, cc.avatar_url AS avatar_url, \
                cc.memorial_sealed_at AS sealed_at, cc.memorial_world_id AS world_id, \
                w.title AS world_title \
         FROM cloud_characters cc LEFT JOIN worlds w ON w.id = cc.memorial_world_id \
         WHERE cc.id = $1 AND cc.memorial_status = $2",
    )
    .bind(&character_id)
    .bind(STATUS_SEALED)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let card_json: String = row.try_get("card_json")?;

    // 足迹：这张卡走过的世界（含已退场的——**足迹是履历，不因死亡消失**）。
    let foot_rows = sqlx::query(
        "SELECT wm.world_id AS world_id, wm.status AS status, wm.joined_at AS joined_at, \
                wm.left_at AS left_at, w.title AS title \
         FROM world_members wm LEFT JOIN worlds w ON w.id = wm.world_id \
         WHERE wm.cloud_character_id = $1 ORDER BY wm.joined_at ASC, wm.world_id ASC",
    )
    .bind(&character_id)
    .fetch_all(&state.db)
    .await?;
    let mut footprints = Vec::with_capacity(foot_rows.len());
    for r in &foot_rows {
        footprints.push(json!({
            "worldId": r.try_get::<String, _>("world_id")?,
            "title": r.try_get::<Option<String>, _>("title")?,
            "status": r.try_get::<String, _>("status")?,
            "joinedAt": r.try_get::<i64, _>("joined_at")?,
            "leftAt": r.try_get::<Option<i64>, _>("left_at")?,
        }));
    }

    // 羁绊：谁还记得他（带着他的「故人」印记的在世角色）。同样只出角色面具，不出主人。
    let mark_rows = sqlx::query(
        "SELECT mm.character_id AS character_id, mm.world_id AS world_id, \
                mm.granted_at AS granted_at, cc.card_json AS card_json \
         FROM memorial_marks mm LEFT JOIN cloud_characters cc ON cc.id = mm.character_id \
         WHERE mm.deceased_character_id = $1 ORDER BY mm.granted_at ASC, mm.character_id ASC",
    )
    .bind(&character_id)
    .fetch_all(&state.db)
    .await?;
    let mut remembered_by = Vec::with_capacity(mark_rows.len());
    for r in &mark_rows {
        let card: Option<String> = r.try_get("card_json")?;
        remembered_by.push(json!({
            "characterId": r.try_get::<String, _>("character_id")?,
            "name": card.as_deref().map(display_name),
            "worldId": r.try_get::<String, _>("world_id")?,
            "grantedAt": r.try_get::<i64, _>("granted_at")?,
        }));
    }

    let card: Value = serde_json::from_str(&card_json).unwrap_or_else(|_| json!({}));
    Ok(Json(json!({
        "id": character_id,
        "name": display_name(&card_json),
        "avatarUrl": row.try_get::<Option<String>, _>("avatar_url")?,
        "memorialStatus": STATUS_SEALED,
        "readOnly": true,
        "mileage": row.try_get::<i64, _>("mileage")?,
        "biography": {
            // 卡面身份原样陈列（不可变快照的一部分，一个字节不改）。
            "identity": card.get("identity").cloned().unwrap_or(json!(null)),
            "sealedAt": row.try_get::<Option<i64>, _>("sealed_at")?,
            "sealedIn": {
                "worldId": row.try_get::<Option<String>, _>("world_id")?,
                "title": row.try_get::<Option<String>, _>("world_title")?,
            },
        },
        "footprints": footprints,
        "remembrance": { "rememberedBy": remembered_by },
    })))
}

/// GET /me/memorial/marks —— 我的角色获得的「故人」印记
/// （§12「你的死成为别人故事的一部分」的读取面）。
async fn my_marks(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    ensure_enabled()?;
    let rows = sqlx::query(
        "SELECT mm.character_id AS character_id, mm.deceased_character_id AS deceased_id, \
                mm.world_id AS world_id, mm.kind AS kind, mm.granted_at AS granted_at, \
                mine.card_json AS mine_card, gone.card_json AS gone_card, w.title AS world_title \
         FROM memorial_marks mm \
         LEFT JOIN cloud_characters mine ON mine.id = mm.character_id \
         LEFT JOIN cloud_characters gone ON gone.id = mm.deceased_character_id \
         LEFT JOIN worlds w ON w.id = mm.world_id \
         WHERE mm.owner_id = $1 ORDER BY mm.granted_at DESC, mm.character_id ASC",
    )
    .bind(&user.user_id)
    .fetch_all(&state.db)
    .await?;

    let mut marks = Vec::with_capacity(rows.len());
    for r in &rows {
        let mine_card: Option<String> = r.try_get("mine_card")?;
        let gone_card: Option<String> = r.try_get("gone_card")?;
        marks.push(json!({
            "kind": r.try_get::<String, _>("kind")?,
            "character": {
                "id": r.try_get::<String, _>("character_id")?,
                "name": mine_card.as_deref().map(display_name),
            },
            "departed": {
                "id": r.try_get::<String, _>("deceased_id")?,
                "name": gone_card.as_deref().map(display_name),
            },
            "worldId": r.try_get::<String, _>("world_id")?,
            "worldTitle": r.try_get::<Option<String>, _>("world_title")?,
            "grantedAt": r.try_get::<i64, _>("granted_at")?,
        }));
    }
    Ok(Json(json!({ "marks": marks })))
}

// ---------------- 路由 ----------------
//
// 🔴 **遗作馆只读**：`/memorial/*` 下**只有 GET**，没有任何修改/删除/编辑传世卡的端点
// （传世卡是只读的陈列品）。唯一的写端点 `POST /me/characters/{id}/memorial` 刻意放在
// `/me/characters` 命名空间下——它是**卡的状态转换**，不是对陈列品的编辑。
// 由 `red_line_memorial_hall_is_read_only` 的路由白名单 + 运行时探测双重锁死。
//
// ⚠️ **接线待办（需 runtime 侧接手，见报告）**：本批次的封卷入口是玩家主动认领
// （服务端核验公共事实）。**自动封卷**的正解是在「死亡落定」那一处直接调 `seal_character_tx`，
// 但那一处在 `runtime::commit_tick` 内，属他人范围；且平权红线要求 `runtime/mod.rs`
// 对资产模块零引用（同 subplot 的处理：结算铸卡挂在 `progression::settle_worldline_tx`，
// 不挂在 runtime）。故正确落点是**结算侧的薄接线层**，不是 runtime 本身。

/// 结算侧自动封卷（**接线层**，由 `progression::settle_idle_world_ending_tx` 在终局事务内调用）。
///
/// 为什么落在结算侧而不是死亡落定处：死亡落定在 `runtime::commit_tick` 内，而平权红线要求
/// `runtime/mod.rs` 对资产模块零引用（同 subplot：结算铸卡挂 `progression`，不挂 runtime）。
///
/// ⚠️ **已知的延迟窗口（如实记录，不要误以为已闭合）**：角色可能在世界跑到一半时死亡，
/// 而本函数要等**世界终局**才封卷。这段窗口里那张卡的 `withdrawn` 仍是 0，
/// 理论上能去 join 别的世界——与 §12「传世卡不可再入世界」有出入。
/// 收窄它需要在死亡落定那一拍就封卷，那要么破平权红线、要么给 runtime 加一条不含资产语义的
/// 出口回调，属独立的一件事。当前折中的依据：死者在原世界已不再行动，且玩家主动认领的
/// 入口（`POST /me/characters/{id}/memorial`）随时可用、不必等终局。
///
/// 幂等由 `seal_character_tx` 的 `WHERE memorial_status='living'` CAS 承担：
/// 世界重复结算、或玩家已主动认领过，都只会短路返回 `sealed:false`。
pub(crate) async fn auto_seal_dead_participants_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    participants: &[(String, String)],
) -> Result<u64, ApiError> {
    if !memorial_enabled() {
        return Ok(0);
    }
    let mut sealed = 0u64;
    for (character_id, owner_id) in participants {
        // 两条证据同时成立才封卷，口径与玩家主动认领完全一致（find_death_evidence 的 doc 有详述）：
        // (a) consent_requests 有 approved 的 death 且 subject 含本卡；
        // (b) 该世界 narrative.pendingConsents 已不含本卡的 death 条目（引擎确已落定）。
        // 任一不成立 → 跳过。**授权 ≠ 死亡**，只看 (a) 会把活角色误封卷（捏造死亡）。
        if !death_evidence_holds_tx(tx, world_id, character_id).await? {
            continue;
        }
        if seal_character_tx(tx, character_id, owner_id, world_id).await?.sealed {
            sealed += 1;
        }
    }
    Ok(sealed)
}

/// `find_death_evidence` + `death_has_landed` 的**事务内**版本，限定在单个世界。
///
/// 与池版本的差异只有取连接的方式与「已知世界」这一约束；判定口径逐条对齐，
/// 任一侧改口径都必须同步改另一侧（两处都在本文件内，便于一起看）。
async fn death_evidence_holds_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    character_id: &str,
) -> Result<bool, ApiError> {
    // 证据 (a)：本世界有 approved 的 death 且 subject 精确含本卡。
    let rows = sqlx::query(
        "SELECT subject_character_ids FROM consent_requests \
         WHERE world_id = $1 AND event_kind = $2 AND status = 'approved'",
    )
    .bind(world_id)
    .bind(EVENT_KIND_DEATH)
    .fetch_all(&mut **tx)
    .await?;
    let authorized = rows.iter().any(|r| {
        r.try_get::<String, _>("subject_character_ids")
            .ok()
            .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
            // 精确匹配数组元素，绝不用子串包含（`chA` 会命中 `chAB`）。
            .map(|list| list.iter().any(|s| s == character_id))
            .unwrap_or(false)
    });
    if !authorized {
        return Ok(false);
    }

    // 证据 (b)：pendingConsents 已不含本卡的 death 条目。查不到/解析失败一律当"没落定"。
    let state_json: Option<String> =
        sqlx::query_scalar("SELECT narrative_state_json FROM worlds WHERE id = $1")
            .bind(world_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(state_json) = state_json else { return Ok(false) };
    let Ok(state) = serde_json::from_str::<Value>(&state_json) else { return Ok(false) };
    let Some(narrative) = state.get("narrative") else { return Ok(false) };
    let still_pending = narrative
        .get("pendingConsents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .any(|p| {
            p.get("subject").and_then(Value::as_str) == Some(character_id)
                && p.get("eventKind").and_then(Value::as_str) == Some(EVENT_KIND_DEATH)
        });
    Ok(!still_pending)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/memorial/characters", get(memorial_hall))
        .route("/memorial/characters/{id}", get(memorial_detail))
        .route("/me/memorial/marks", get(my_marks))
        .route("/me/characters/{id}/memorial", post(seal_memorial))
}
