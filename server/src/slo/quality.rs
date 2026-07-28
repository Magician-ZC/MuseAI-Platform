//! 世界质量三指标：**完读率 / 阻断率 / 结局分布**
//! （总规格 §4「内容中台工业线」增量投入项「仿真质检自动化（世界质量回归）」的度量面）。
//!
//! **为什么单独一个文件而不是塞进 `slo/mod.rs`**：`slo/mod.rs` §4 的八项是**平台运营窗口**口径
//! （滚动 N 天、跨全平台世界、进 `/admin/metrics/overview`）；本文件是**一批世界的全生命周期**口径
//! （一次仿真试跑 / 一次灰度批次 = 一组 world_id，从开局算到收尾）。两者分母根本不同，
//! 混在一个函数里迟早会有人把两个数当同一个数用。
//!
//! **口径复用，绝不另立**：收尾分类走 `super::classify_conclusion`（与 §4.2「强制收尾率」同一张表），
//! 结局串解析走 `super::parse_ended_reason`，集中度走 `super::gini_coefficient`（与「叙事注意力基尼」
//! 同一个实现），比率走 `super::rate`。本文件只新增**分子/分母的圈法**，不新增任何算法。
//!
//! 🔴 **完读率 ≠ 1 − 强制收尾率**。两者分母不同，不可互相换算，也不可互相校验：
//! - §4.2 `forcedConclusionRate` 分母 = `worlds.status='ended'`（**只看已收尾的世界**）；
//! - 本文件 `completionRate` 分母 = **纳入本批的全部世界**，含**跑不到收尾**的那些。
//!   一个批次里 90 个世界卡在半路没结束、10 个自然收尾，强制收尾率是 0%（看起来完美），
//!   完读率是 10%（真相）。**T1/T2 门槛要看的是后者**——"跑不完"正是最需要被抓住的失败形态，
//!   而它在 `status='ended'` 的分母里根本不出现。两个数都输出，便于交叉核对，但不得混用。
//!
//! **本文件不接任何写入面、不含任何运营开关**（它只有读）。消费方有两个：
//! ① `runtime::simulation`（离线仿真试跑工装，单世界取数走 [`collect_world_facts`]）；
//! ② `slo::calibration` 的戏服维读数（挂在被轮询的 `/admin/metrics/overview` 上，
//!    批量取数走 [`collect_world_facts_bulk`]）。
//! 两条取数路径**共用同一套分类规则**（[`add_tick_bucket`] / `classify_conclusion`），
//! 这正是把口径写成生产代码而非埋在 `#[cfg(test)]` 里的目的（黄金世界回归把指标提升进
//! `crate::slo` 时立的规矩：回归与看板必须永远算同一个数）。

use std::collections::BTreeMap;

use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::db::Placeholders;
use crate::error::ApiError;

use super::{classify_conclusion, gini_coefficient, parse_ended_reason, rate, ConclusionKind};

/// 单条 `IN (…)` 最多绑几个参数（SQLite 老版本 `SQLITE_MAX_VARIABLE_NUMBER` 默认 999，
/// 不分批会在大批次上直接报错）。
const BIND_CHUNK: usize = 200;

// ============================================================================
// §1 单世界原始计数（三个指标唯一的输入形态）
// ============================================================================

/// 一个世界跑完（或跑到观测截止）之后的**原始计数**。
///
/// 刻意只装整数与短串，不装任何比率：**比率是派生量，只能在聚合层算一次**。
/// 每个世界各自算一遍比率再平均，会得到「小世界与大世界等权」的假数——
/// 三个指标全部按**总量比**（分子总和 / 分母总和）而非「各世界比率的平均」计算。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorldQualityFacts {
    pub world_id: String,
    /// `worlds.status`：running / ended / paused …
    pub status: String,
    /// 收尾原因（`audit_logs('world.ended')` 前缀）。未收尾 = 空串。
    pub conclusion_reason: String,
    /// 结局 id（`{reason}|ending={ending}` 的后缀）。无结局 = 空串。
    pub ending_id: String,
    /// 引擎跑完一整回合**并提交**的拍。
    pub committed_ticks: i64,
    /// 引擎跑完一整回合、结果**被拒绝写入公共事实**的拍（`world_ticks.error='blocked'`）。
    pub blocked_ticks: i64,
    /// 终局短路拍：`conclude_world_no_round` 写入终局 reason，**没有跑回合**。
    pub terminal_ticks: i64,
    /// 前置门跳过拍（world_not_running / superseded / budget_fused / no_model_config /
    /// insufficient_members / cas_conflict）：**没有跑回合**，不进阻断率分母。
    pub gated_ticks: i64,
    /// `status='failed'` 的拍（重试耗尽）。
    pub failed_ticks: i64,
    /// 仍 pending/running 的拍（观测截止时未终结）。
    pub open_ticks: i64,
    /// 落库事件总数。
    pub events_total: i64,
    /// 被内容安全第 2 层扣下待人审的事件（`moderation <> 'approved'`）：
    /// **打码 + 转人审，不是删除**，故叫「扣留」不叫「阻断」。
    pub events_withheld: i64,
}

impl WorldQualityFacts {
    /// 阻断率的分母：**真正跑完一整回合**的拍 = 提交 + 阻断。
    ///
    /// 终局短路 / 前置门 / failed 一律不进——它们连回合都没跑，把它们塞进分母只会稀释阻断率，
    /// 让「引擎越来越常拒绝提交」这件事在数字上消失。
    pub fn engine_ticks(&self) -> i64 {
        self.committed_ticks + self.blocked_ticks
    }

    /// 收尾分类（复用 §4.2 同一张表）。
    pub fn conclusion_kind(&self) -> ConclusionKind {
        classify_conclusion(&self.conclusion_reason)
    }

    /// 是否**跑到了收尾**（无论自然还是强制）。`worlds.status='ended'` 是唯一权威。
    pub fn is_ended(&self) -> bool {
        self.status == "ended"
    }

    /// 是否**完读**：跑到收尾 **且** 收尾方式是自然收尾（主线走完）。
    ///
    /// 两个条件缺一不可：只看 reason 会把「审计行写了但世界没停机」的异常算成完读。
    pub fn is_complete_read(&self) -> bool {
        self.is_ended() && self.conclusion_kind() == ConclusionKind::Natural
    }
}

/// 结局分布里「收尾了但没有结局 id」的桶名（如 `starved` / `time_cap` 常常没有结局）。
/// **不丢弃、不并入其它桶**——"这批世界有一半根本没落到任何结局上"本身就是要看的结论。
pub(crate) const ENDING_NONE: &str = "(none)";
/// 结局分布里「压根没跑到收尾」的桶名。
pub(crate) const ENDING_UNFINISHED: &str = "(unfinished)";

// ============================================================================
// §2 取数：单世界事实（可移植 SQL 子集，SQLite / Postgres 同源）
// ============================================================================

/// tick 的 `error` 串 → 是否属于**终局短路**（`conclude_world_no_round` 写的那些）。
///
/// 复用 `classify_conclusion` 的词表，额外认一个字面量 `terminal`
/// （`run_event_step` 判出终局但未到终局地板时写的 no-op 标记）。
fn is_terminal_note(err: &str) -> bool {
    err == "terminal" || classify_conclusion(err) != ConclusionKind::Unknown
}

/// `world_ticks` 的一组 `(status, error, 行数)` → 落进事实的哪个桶。
///
/// 🔴 **单世界版与批量版共用这一处**，是刻意的：分类规则是**代码事实**
/// （`finish_tick_noop` 写了哪些 note、`finish_tick_blocked` 写了什么），
/// 各写一份必然漂移，而漂移的表现是「同一批世界在看板与回归里阻断率不一样」——
/// 那种 bug 没人会在数字上看出来。
fn add_tick_bucket(f: &mut WorldQualityFacts, status: &str, err: &str, n: i64) {
    match status {
        "failed" => f.failed_ticks += n,
        "done" if err.is_empty() => f.committed_ticks += n,
        "done" if err == "blocked" => f.blocked_ticks += n,
        "done" if is_terminal_note(err) => f.terminal_ticks += n,
        "done" => f.gated_ticks += n,
        // pending / running：观测截止时还没终结。
        _ => f.open_ticks += n,
    }
}

/// 读一个世界的质量事实。
///
/// SQL 全部落在双库可移植子集内：只用 `COUNT` / `GROUP BY` / `COALESCE` / `CAST(... AS BIGINT)`，
/// 无 `strftime` / `date_trunc` / `ON CONFLICT` / JSON 函数。tick 分类在 Rust 侧做——
/// 分类规则是**代码事实**（`finish_tick_noop` 写了哪些 note），写进 SQL 的 CASE 里会立刻和它漂移。
pub(crate) async fn collect_world_facts(
    db: &AnyPool,
    world_id: &str,
) -> Result<WorldQualityFacts, ApiError> {
    let mut f = WorldQualityFacts { world_id: world_id.to_string(), ..Default::default() };

    f.status = sqlx::query("SELECT status FROM worlds WHERE id = $1")
        .bind(world_id)
        .fetch_optional(db)
        .await?
        .map(|r| r.try_get::<String, _>("status"))
        .transpose()?
        .unwrap_or_default();

    let (reason, ending) = super::world_conclusion(db, world_id).await?;
    f.conclusion_reason = reason;
    f.ending_id = ending;

    let rows = sqlx::query(
        "SELECT status, COALESCE(error, '') AS err, CAST(COUNT(*) AS BIGINT) AS n \
         FROM world_ticks WHERE world_id = $1 GROUP BY status, COALESCE(error, '')",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    for r in &rows {
        let status: String = r.try_get("status")?;
        let err: String = r.try_get("err")?;
        let n: i64 = r.try_get("n")?;
        add_tick_bucket(&mut f, &status, &err, n);
    }

    f.events_total = sqlx::query("SELECT CAST(COUNT(*) AS BIGINT) AS n FROM world_events WHERE world_id = $1")
        .bind(world_id)
        .fetch_one(db)
        .await?
        .try_get("n")?;
    f.events_withheld = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM world_events WHERE world_id = $1 AND moderation <> 'approved'",
    )
    .bind(world_id)
    .fetch_one(db)
    .await?
    .try_get("n")?;

    Ok(f)
}

/// 一次取回**一组**世界的质量事实：与 [`collect_world_facts`] **逐字同口径**，
/// 只是把「每个世界 4 条 SQL」折成「按 `IN (…)` 分批的 3 条聚合」。
///
/// 🔴 存在的唯一理由是 **N+1**：单世界版给 100 个世界取数就是 400 条 SQL，
/// 而 `slo` 的读数挂在被轮询的 `/admin/metrics/overview` 上，模块头注释明令禁止按世界逐个发 SQL。
/// 分类规则不另写一份（tick 走 [`add_tick_bucket`]、收尾串走 `parse_ended_reason`），
/// 故两条取数路径永远算同一个数。
///
/// `worlds`：`world_id → worlds.status`（status 由调用方在扫世界时顺手带出来，本函数不再回查）。
/// 返回值按 world_id 升序（`BTreeMap` 保证跨运行定序）。
///
/// 可移植 SQL：只用 `COUNT` / `SUM(CASE …)` / `COALESCE` / `IN` / `CAST(… AS BIGINT)`，
/// 占位符 `$N` 由 [`Placeholders`] 顺序发号，无方言函数、无 JSON 运算。
pub(crate) async fn collect_world_facts_bulk(
    db: &AnyPool,
    worlds: &BTreeMap<String, String>,
) -> Result<Vec<WorldQualityFacts>, ApiError> {
    if worlds.is_empty() {
        return Ok(Vec::new());
    }
    let mut out: BTreeMap<String, WorldQualityFacts> = worlds
        .iter()
        .map(|(id, status)| {
            (
                id.clone(),
                WorldQualityFacts {
                    world_id: id.clone(),
                    status: status.clone(),
                    ..Default::default()
                },
            )
        })
        .collect();
    let ids: Vec<&String> = worlds.keys().collect();

    // ① 收尾 (reason, ending)。事实源同单世界版：`audit_logs('world.ended')`
    //    （走 commit_tick 的收尾会把 world_ticks.error 置 NULL，那一列天然缺一半样本）。
    //    单世界版取 `LIMIT 1`；这里同一 subject 若有多行，按 **全序** `(subject, id)` 取第一行——
    //    PG 无稳定序保证，"任取一行"必须落在全序上才跨库可复现。
    for chunk in ids.chunks(BIND_CHUNK) {
        let ph = Placeholders::new().list(chunk.len());
        let sql = format!(
            "SELECT subject, reason FROM audit_logs \
             WHERE action = 'world.ended' AND subject IN ({ph}) \
             ORDER BY subject ASC, id ASC"
        );
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id.as_str());
        }
        for r in q.fetch_all(db).await? {
            let subject: String = r.try_get("subject")?;
            let Some(f) = out.get_mut(&subject) else { continue };
            if !f.conclusion_reason.is_empty() || !f.ending_id.is_empty() {
                continue; // 首行胜出（全序下确定）。
            }
            let (reason, ending) = parse_ended_reason(&r.try_get::<String, _>("reason")?);
            f.conclusion_reason = reason;
            f.ending_id = ending;
        }
    }

    // ② tick 分桶。分类在 Rust 侧做（同单世界版）：写进 SQL 的 CASE 里会立刻和代码事实漂移。
    for chunk in ids.chunks(BIND_CHUNK) {
        let ph = Placeholders::new().list(chunk.len());
        let sql = format!(
            "SELECT world_id, status, COALESCE(error, '') AS err, CAST(COUNT(*) AS BIGINT) AS n \
             FROM world_ticks WHERE world_id IN ({ph}) \
             GROUP BY world_id, status, COALESCE(error, '')"
        );
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id.as_str());
        }
        // 不加 ORDER BY：每行都是 `+=` 累加，消费与行序无关。
        for r in q.fetch_all(db).await? {
            let world_id: String = r.try_get("world_id")?;
            let Some(f) = out.get_mut(&world_id) else { continue };
            let status: String = r.try_get("status")?;
            let err: String = r.try_get("err")?;
            add_tick_bucket(f, &status, &err, r.try_get::<i64, _>("n")?);
        }
    }

    // ③ 事件总数 / 安全扣留数（单世界版的两条标量查询在这里合成一条 GROUP BY）。
    for chunk in ids.chunks(BIND_CHUNK) {
        let ph = Placeholders::new().list(chunk.len());
        let sql = format!(
            "SELECT world_id, CAST(COUNT(*) AS BIGINT) AS total_n, \
             CAST(COALESCE(SUM(CASE WHEN moderation <> 'approved' THEN 1 ELSE 0 END), 0) AS BIGINT) AS withheld_n \
             FROM world_events WHERE world_id IN ({ph}) GROUP BY world_id"
        );
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id.as_str());
        }
        for r in q.fetch_all(db).await? {
            let world_id: String = r.try_get("world_id")?;
            let Some(f) = out.get_mut(&world_id) else { continue };
            f.events_total = r.try_get("total_n")?;
            f.events_withheld = r.try_get("withheld_n")?;
        }
    }

    Ok(out.into_values().collect())
}

// ============================================================================
// §3 指标 ①：完读率
// ============================================================================

/// 完读率统计。四个桶**互斥且穷尽**，加起来恒等于 `worlds`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CompletionStats {
    /// 分母：纳入本批的全部世界（**含未收尾**）。
    pub worlds: i64,
    /// 分子：跑到收尾且自然收尾（`mainline_complete`）。
    pub natural: i64,
    /// 收尾了，但是被时间上限 / 无可调度角色掐掉的。
    pub forced: i64,
    /// 收尾了，且属于世界线崩塌（关键角色永久退场）。
    pub collapsed: i64,
    /// 收尾了，但 reason 无法识别（保守：**不算完读**）。
    pub unknown_ended: i64,
    /// **没跑到收尾**（still running / paused / 观测窗口内没结束）。
    pub unfinished: i64,
}

impl CompletionStats {
    /// 完读率 = 自然收尾 / 全部世界（**分母含未收尾**，见文件头红字）。
    pub fn completion_rate(&self) -> f64 {
        rate(self.natural, self.worlds)
    }

    /// 已收尾世界数（= §4.2 `forcedConclusionRate` 的分母，供交叉核对）。
    pub fn ended(&self) -> i64 {
        self.natural + self.forced + self.collapsed + self.unknown_ended
    }

    /// 强制收尾率（**§4.2 同口径同分母**：分母只含已收尾世界）。与完读率并列输出，
    /// 一眼就能看出「这批世界是被掐掉的多，还是压根没跑完的多」。
    pub fn forced_rate(&self) -> f64 {
        rate(self.forced + self.collapsed + self.unknown_ended, self.ended())
    }
}

pub(crate) fn completion_stats(facts: &[WorldQualityFacts]) -> CompletionStats {
    let mut s = CompletionStats { worlds: facts.len() as i64, ..Default::default() };
    for f in facts {
        if !f.is_ended() {
            s.unfinished += 1;
            continue;
        }
        match f.conclusion_kind() {
            ConclusionKind::Natural => s.natural += 1,
            ConclusionKind::Forced => s.forced += 1,
            ConclusionKind::Collapsed => s.collapsed += 1,
            ConclusionKind::Unknown => s.unknown_ended += 1,
        }
    }
    s
}

// ============================================================================
// §4 指标 ②：阻断率
// ============================================================================

/// 阻断率统计。**两条互相独立的阻断通道**，分开报，绝不合成一个数：
/// 前者是引擎规则层拒绝提交（叙事走不下去），后者是内容安全层扣留事件（写出来了但不能公开），
/// 合成一个数就再也分不清「世界卡住了」和「世界在说不该说的话」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BlockStats {
    /// 通道 A 分子：引擎跑完回合但拒绝提交的拍。
    pub blocked_ticks: i64,
    /// 通道 A 分母：真正跑完回合的拍（提交 + 阻断）。
    pub engine_ticks: i64,
    /// 通道 B 分子：被内容安全第 2 层扣留待人审的事件。
    pub events_withheld: i64,
    /// 通道 B 分母：落库事件总数。
    pub events_total: i64,
    /// 旁证（不进任何分母）：终局短路拍。
    pub terminal_ticks: i64,
    /// 旁证（不进任何分母）：前置门跳过拍。
    pub gated_ticks: i64,
    /// 旁证（不进任何分母）：重试耗尽的失败拍。
    pub failed_ticks: i64,
    /// 旁证（不进任何分母）：观测截止时仍未终结的拍。
    pub open_ticks: i64,
    /// 至少被阻断过一次的世界数（"阻断集中在少数世界" vs "普遍性阻断"）。
    pub worlds_with_block: i64,
}

impl BlockStats {
    /// 阻断率（通道 A）= 阻断拍 / 真正跑完回合的拍。
    pub fn blocked_rate(&self) -> f64 {
        rate(self.blocked_ticks, self.engine_ticks)
    }

    /// 安全扣留率（通道 B）= 扣留事件 / 事件总数。
    pub fn withheld_rate(&self) -> f64 {
        rate(self.events_withheld, self.events_total)
    }
}

pub(crate) fn block_stats(facts: &[WorldQualityFacts]) -> BlockStats {
    let mut s = BlockStats::default();
    for f in facts {
        s.blocked_ticks += f.blocked_ticks;
        s.engine_ticks += f.engine_ticks();
        s.events_withheld += f.events_withheld;
        s.events_total += f.events_total;
        s.terminal_ticks += f.terminal_ticks;
        s.gated_ticks += f.gated_ticks;
        s.failed_ticks += f.failed_ticks;
        s.open_ticks += f.open_ticks;
        if f.blocked_ticks > 0 {
            s.worlds_with_block += 1;
        }
    }
    s
}

// ============================================================================
// §5 指标 ③：结局分布
// ============================================================================

/// 结局分布。**分布形状本身就是指标**——1000 个世界全落同一个结局，与全部跑不完，
/// 是两种不同的失败，但都会让"这个模板值得上线"这句话不成立。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EndingDistribution {
    /// 结局 id → 世界数，按 id 升序（`BTreeMap` 保证跨运行定序）。
    /// 含两个特殊桶：`(none)` 收尾但无结局、`(unfinished)` 未收尾。
    pub by_ending: BTreeMap<String, i64>,
    /// 收尾类型 → 世界数（natural / forced / collapsed / unknown），未收尾不计。
    pub by_kind: BTreeMap<String, i64>,
    /// 收尾原因 → 世界数（比结局粒度更细，用于定位"为什么强制收尾"）。
    pub by_reason: BTreeMap<String, i64>,
    /// 落到**真实结局 id**（排除两个特殊桶）的世界数。
    pub with_ending: i64,
    /// 出现过的真实结局种数。
    pub distinct_endings: i64,
}

impl EndingDistribution {
    /// 最大单一结局的世界数（`top_share` 的分子；渲染层判空要单独拿它）。
    pub fn top_count(&self) -> i64 {
        self
            .by_ending
            .iter()
            .filter(|(k, _)| k.as_str() != ENDING_NONE && k.as_str() != ENDING_UNFINISHED)
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(0)
    }

    /// 最大单一结局占比（分母 = 落到真实结局的世界数）。结局池形同虚设时这个数会贴近 1.0。
    pub fn top_share(&self) -> f64 {
        rate(self.top_count(), self.with_ending)
    }

    /// 结局集中度（基尼，**复用 `slo::gini_coefficient`**，与叙事注意力基尼同一个实现）。
    ///
    /// 0 = 各结局等量，1 = 全压在一个结局上。⚠️ 输入是**各结局的世界数**，
    /// 只统计真正出现过的结局：结局池里定义了但一次都没抽到的结局不在样本里，
    /// 故这个数**低估**了集中度，用它比较不同批次时两边必须是同一份结局池。
    pub fn concentration_gini(&self) -> f64 {
        let counts: Vec<i64> = self
            .by_ending
            .iter()
            .filter(|(k, _)| k.as_str() != ENDING_NONE && k.as_str() != ENDING_UNFINISHED)
            .map(|(_, v)| *v)
            .collect();
        gini_coefficient(&counts)
    }
}

pub(crate) fn ending_distribution(facts: &[WorldQualityFacts]) -> EndingDistribution {
    let mut d = EndingDistribution::default();
    for f in facts {
        if !f.is_ended() {
            *d.by_ending.entry(ENDING_UNFINISHED.to_string()).or_insert(0) += 1;
            continue;
        }
        let kind = f.conclusion_kind();
        *d.by_kind.entry(kind.as_str().to_string()).or_insert(0) += 1;
        let reason = if f.conclusion_reason.is_empty() {
            "(no-audit-row)".to_string()
        } else {
            f.conclusion_reason.clone()
        };
        *d.by_reason.entry(reason).or_insert(0) += 1;
        if f.ending_id.is_empty() {
            *d.by_ending.entry(ENDING_NONE.to_string()).or_insert(0) += 1;
        } else {
            *d.by_ending.entry(f.ending_id.clone()).or_insert(0) += 1;
            d.with_ending += 1;
        }
    }
    d.distinct_endings = d
        .by_ending
        .keys()
        .filter(|k| k.as_str() != ENDING_NONE && k.as_str() != ENDING_UNFINISHED)
        .count() as i64;
    d
}

// ============================================================================
// §6 汇总报告：两个序列化面（人读 / 机比）
// ============================================================================

/// 三指标汇总。
#[derive(Debug, Clone, Default)]
pub(crate) struct WorldQualityReport {
    pub completion: CompletionStats,
    pub blocking: BlockStats,
    pub endings: EndingDistribution,
}

/// 报告随身携带的**诚实边界**声明。
///
/// 🔴 它是 JSON 里的一等字段而不是文档里的一句话，因为**数会被复制走，文档不会**。
/// 任何把这三个数贴进评审材料的人，都必须同时看见这段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QualitySource {
    /// 规则化假模型（种子驱动，不接任何真实模型）跑出来的数。
    SimulatedStub,
    /// 真实模型产出的生产世界。**当前无任何代码走这条**，留着是为了让枚举本身把边界写死：
    /// 换成真实模型时必须显式改这个值，改不动就说明数据源没换。
    /// （`allow(dead_code)`：它的价值恰恰在于"还没有人能构造它"——一旦有人构造得出来，
    /// 就说明真的接上了真实模型数据源，那时这条 allow 自然该删。）
    #[allow(dead_code)]
    Production,
}

impl QualitySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::SimulatedStub => "simulated-stub",
            Self::Production => "production",
        }
    }

    /// 这批数**测得了什么、测不了什么**。
    fn caveats(self) -> Vec<&'static str> {
        match self {
            Self::SimulatedStub => vec![
                "数据源 = 规则化假模型（种子驱动的桩），全程不调用任何真实模型。",
                "完读率在此只说明「主线推进 + 终局判定管线在各种决策组合下能否走到自然收尾」，\
                 不说明「故事好不好看」。100% 完读率 = 管线不卡死，不等于内容质量已验证。",
                "阻断率在此只说明「规则层（底线/硬节点/不变量）在压力下被触发的频率」，\
                 不说明「模型输出是否合规」——桩不会说违规的话。",
                "结局分布在此只说明「结局加权采样 + 终局判定的分布形状」，\
                 不说明「玩家会喜欢哪个结局」。",
                "内容安全扣留率（events.moderation）在桩下恒为 0：桩文本永不命中敏感词库，\
                 该通道在仿真里**没有被测试**，只是被计算了。",
                "要度量内容质量，前置件是 record-and-replay 的 ModelClient（VALIDATION §4.1）：\
                 工具已建（muse_engine::replay，2026-07-26）、接线已通（runtime::record，2026-07-27，\
                 默认关闭），但**尚无任何真实模型录制**（需运营方自带 API Key），\
                 「差异多大算 OOC」的评分口径亦未定义。仿真因此仍全程跑桩——\
                 在真实录制与质量口径都到位之前，本报告的任何数字都不得表述为「叙事质量已验证」。",
            ],
            Self::Production => vec![
                "数据源 = 真实模型产出的生产世界；三个指标此时才同时包含内容质量成分。",
            ],
        }
    }
}

impl WorldQualityReport {
    pub fn of(facts: &[WorldQualityFacts]) -> Self {
        Self {
            completion: completion_stats(facts),
            blocking: block_stats(facts),
            endings: ending_distribution(facts),
        }
    }

    /// **人读面**：含比率（f64）、旁证与诚实边界。用于打印、贴报告、进看板。
    pub fn to_json(&self, source: QualitySource) -> Value {
        let c = &self.completion;
        let b = &self.blocking;
        let e = &self.endings;
        json!({
            "source": source.as_str(),
            "honesty": source.caveats(),
            "completion": {
                "metric": "completionRate",
                "title": "完读率",
                "definition": "自然收尾（mainline_complete）的世界数 / 纳入本批的全部世界数（分母含未收尾世界）",
                "worlds": c.worlds,
                "natural": c.natural,
                "forced": c.forced,
                "collapsed": c.collapsed,
                "unknownEnded": c.unknown_ended,
                "unfinished": c.unfinished,
                // 🔴 分母为 0 → null 而不是 0.0。空批次上的 `completionRate: 0` 会被读成
                // 「一个都没读完」，而真相是「这一批一个世界都没有」。同 `slo::rate_or_null`
                // 的理由（见那里的注释）；这里不带 status 字段，判空全靠紧邻的计数 + null。
                "completionRate": crate::slo::rate_or_null(c.natural, c.worlds),
                "endedWorlds": c.ended(),
                "forcedRateAmongEnded": crate::slo::rate_or_null(
                    c.forced + c.collapsed + c.unknown_ended,
                    c.ended(),
                ),
                "note": "forcedRateAmongEnded 与 VALIDATION §4.2 强制收尾率同口径同分母（只含已收尾世界）；\
                         completionRate 分母另含未收尾世界，两者不可互相换算。",
            },
            "blocking": {
                "metric": "blockedRate",
                "title": "阻断率",
                "definition": "引擎跑完整回合但拒绝提交的拍 / 真正跑完整回合的拍（提交 + 阻断）",
                "blockedTicks": b.blocked_ticks,
                "engineTicks": b.engine_ticks,
                "blockedRate": crate::slo::rate_or_null(b.blocked_ticks, b.engine_ticks),
                "worldsWithBlock": b.worlds_with_block,
                "safetyWithheld": {
                    "metric": "safetyWithheldEventRate",
                    "title": "内容安全扣留率（独立通道，不与阻断率合并）",
                    "definition": "moderation <> 'approved' 的事件 / 落库事件总数（扣留 = 打码转人审，不是删除）",
                    "eventsWithheld": b.events_withheld,
                    "eventsTotal": b.events_total,
                    "withheldRate": crate::slo::rate_or_null(b.events_withheld, b.events_total),
                },
                "excludedFromDenominator": {
                    "note": "以下拍**没有跑回合**，一律不进阻断率分母——放进去只会稀释阻断率。",
                    "terminalTicks": b.terminal_ticks,
                    "gatedTicks": b.gated_ticks,
                    "failedTicks": b.failed_ticks,
                    "openTicks": b.open_ticks,
                },
            },
            "endings": {
                "metric": "endingDistribution",
                "title": "结局分布",
                "definition": "audit_logs('world.ended').reason 的 `|ending=` 后缀分桶；\
                               (none) = 收尾但无结局，(unfinished) = 未收尾，两者均不丢弃",
                "byEnding": e.by_ending,
                "byKind": e.by_kind,
                "byReason": e.by_reason,
                "worldsWithEnding": e.with_ending,
                "distinctEndings": e.distinct_endings,
                "topEndingShare": crate::slo::rate_or_null(e.top_count(), e.with_ending),
                // ⚠️ `concentrationGini` 沿用 `gini_coefficient` 的既定契约（空集/全零集 → 0.0，
                // 「没开演不是不公平」），不在这里改判空——那条契约有多个调用点共享，
                // 单独在渲染层改一处会让同一个函数在不同出口给出不同语义。
                // 判空线索由紧邻的 `worldsWithEnding` / `distinctEndings` 给出。
                "concentrationGini": e.concentration_gini(),
            },
        })
    }

    /// **机比面（跨版本回归比对口径）**：只含**整数计数**，一个浮点都没有。
    ///
    /// 🔴 为什么不比比率：比率是 f64，跨平台/跨优化等级的最后一位可能不同，
    /// 而回归比对必须逐字节。计数是整数、无歧义、且**信息量更大**——
    /// 比率相同但分子分母同时翻倍，是必须被看见的变化。
    pub fn baseline_json(&self) -> Value {
        let c = &self.completion;
        let b = &self.blocking;
        let e = &self.endings;
        json!({
            "completion": {
                "worlds": c.worlds,
                "natural": c.natural,
                "forced": c.forced,
                "collapsed": c.collapsed,
                "unknownEnded": c.unknown_ended,
                "unfinished": c.unfinished,
            },
            "blocking": {
                "blockedTicks": b.blocked_ticks,
                "engineTicks": b.engine_ticks,
                "worldsWithBlock": b.worlds_with_block,
                "eventsWithheld": b.events_withheld,
                "eventsTotal": b.events_total,
                "terminalTicks": b.terminal_ticks,
                "gatedTicks": b.gated_ticks,
                "failedTicks": b.failed_ticks,
                "openTicks": b.open_ticks,
            },
            "endings": {
                "byEnding": e.by_ending,
                "byKind": e.by_kind,
                "byReason": e.by_reason,
                "worldsWithEnding": e.with_ending,
                "distinctEndings": e.distinct_endings,
            },
        })
    }
}

// ============================================================================
// §7 口径单测（纯函数，不落库）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn f(world_id: &str, status: &str, reason: &str, ending: &str) -> WorldQualityFacts {
        WorldQualityFacts {
            world_id: world_id.into(),
            status: status.into(),
            conclusion_reason: reason.into(),
            ending_id: ending.into(),
            ..Default::default()
        }
    }

    /// 完读率分母必须含未收尾世界——这正是它与 §4.2 强制收尾率的唯一实质差别。
    #[test]
    fn completion_rate_counts_unfinished_worlds_in_denominator() {
        let facts = vec![
            f("w1", "ended", "mainline_complete", "e_alliance"),
            f("w2", "running", "", ""),
            f("w3", "running", "", ""),
            f("w4", "running", "", ""),
        ];
        let s = completion_stats(&facts);
        assert_eq!((s.worlds, s.natural, s.unfinished), (4, 1, 3));
        assert!((s.completion_rate() - 0.25).abs() < 1e-9, "完读率 = 1/4");
        assert_eq!(s.ended(), 1);
        assert!(
            (s.forced_rate() - 0.0).abs() < 1e-9,
            "强制收尾率在同一批数据上是 0%（分母只含已收尾），与 25% 的完读率并存 —— \
             这就是两个数不可互换的证据"
        );
    }

    /// 收尾但 reason 不可识别：保守计为「非完读」，与 `classify_conclusion` 的保守取向一致。
    #[test]
    fn unknown_ended_world_is_not_a_complete_read() {
        let facts =
            vec![f("w1", "ended", "some_new_reason_nobody_mapped", ""), f("w2", "ended", "", "")];
        let s = completion_stats(&facts);
        assert_eq!(s.unknown_ended, 2);
        assert_eq!(s.natural, 0);
        assert_eq!(s.completion_rate(), 0.0);
    }

    /// 有审计行但世界没停机 = 异常，不算完读（两个条件缺一不可）。
    #[test]
    fn audit_row_without_ended_status_is_not_complete() {
        let facts = vec![f("w1", "running", "mainline_complete", "e_alliance")];
        let s = completion_stats(&facts);
        assert_eq!(s.natural, 0, "status 未 ended，reason 再漂亮也不算完读");
        assert_eq!(s.unfinished, 1);
    }

    /// 阻断率分母只含「真跑了回合」的拍：终局/前置门/失败/未终结全部排除。
    #[test]
    fn blocked_rate_denominator_excludes_ticks_that_never_ran_a_round() {
        let facts = vec![WorldQualityFacts {
            world_id: "w1".into(),
            status: "ended".into(),
            committed_ticks: 7,
            blocked_ticks: 3,
            terminal_ticks: 1,
            gated_ticks: 40,
            failed_ticks: 5,
            open_ticks: 2,
            ..Default::default()
        }];
        let b = block_stats(&facts);
        assert_eq!(b.engine_ticks, 10, "分母 = 提交 7 + 阻断 3，其余 48 拍一概不进");
        assert!((b.blocked_rate() - 0.3).abs() < 1e-9);
        assert_eq!(b.worlds_with_block, 1);
        assert_eq!(
            (b.terminal_ticks, b.gated_ticks, b.failed_ticks, b.open_ticks),
            (1, 40, 5, 2),
            "排除项必须照样报出来 —— 排除不等于隐藏"
        );
    }

    /// 两条阻断通道分开算，绝不合成一个数。
    #[test]
    fn safety_withholding_is_a_separate_channel_from_engine_blocking() {
        let facts = vec![WorldQualityFacts {
            world_id: "w1".into(),
            status: "ended".into(),
            committed_ticks: 10,
            blocked_ticks: 0,
            events_total: 40,
            events_withheld: 8,
            ..Default::default()
        }];
        let b = block_stats(&facts);
        assert_eq!(b.blocked_rate(), 0.0, "引擎一次没拦");
        assert!((b.withheld_rate() - 0.2).abs() < 1e-9, "安全层扣了 20% 的事件");
    }

    /// 终局 note 的识别复用 `classify_conclusion` 词表，未知 error 落 gated 而不是 terminal。
    #[test]
    fn terminal_notes_reuse_conclusion_vocabulary() {
        for t in ["mainline_complete", "time_cap", "time_limit", "starved", "key_character_exit", "terminal"] {
            assert!(is_terminal_note(t), "{t} 应识别为终局短路");
        }
        for g in ["cas_conflict", "superseded", "budget_fused", "no_model_config", "insufficient_members", ""] {
            assert!(!is_terminal_note(g), "{g} 不是终局短路");
        }
    }

    /// 结局分布：两个特殊桶不丢弃，且不进「真实结局」的统计口径。
    #[test]
    fn ending_distribution_keeps_none_and_unfinished_buckets_separate() {
        let facts = vec![
            f("w1", "ended", "mainline_complete", "e_alliance"),
            f("w2", "ended", "mainline_complete", "e_alliance"),
            f("w3", "ended", "mainline_complete", "e_silence"),
            f("w4", "ended", "starved", ""),
            f("w5", "running", "", ""),
        ];
        let d = ending_distribution(&facts);
        assert_eq!(d.by_ending.get("e_alliance").copied(), Some(2));
        assert_eq!(d.by_ending.get("e_silence").copied(), Some(1));
        assert_eq!(d.by_ending.get(ENDING_NONE).copied(), Some(1), "收尾无结局单独一桶");
        assert_eq!(d.by_ending.get(ENDING_UNFINISHED).copied(), Some(1), "未收尾单独一桶");
        assert_eq!(d.with_ending, 3, "只有 3 个世界落到真实结局");
        assert_eq!(d.distinct_endings, 2);
        assert!((d.top_share() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(d.by_kind.get("natural").copied(), Some(3));
        assert_eq!(d.by_kind.get("forced").copied(), Some(1));
        assert_eq!(d.by_reason.get("starved").copied(), Some(1));
    }

    /// 结局全压在一个桶 = 结局池形同虚设，集中度基尼必须能把它抓出来。
    #[test]
    fn ending_concentration_detects_a_degenerate_ending_pool() {
        let healthy: Vec<WorldQualityFacts> = (0..9)
            .map(|i| f(&format!("w{i}"), "ended", "mainline_complete", &format!("e{}", i % 3)))
            .collect();
        let degenerate: Vec<WorldQualityFacts> = (0..9)
            .map(|i| f(&format!("w{i}"), "ended", "mainline_complete", "e0"))
            .collect();
        let h = ending_distribution(&healthy);
        let g = ending_distribution(&degenerate);
        assert_eq!(h.distinct_endings, 3);
        assert!((h.concentration_gini() - 0.0).abs() < 1e-9, "三个结局等量 → 基尼 0");
        assert!((h.top_share() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(g.distinct_endings, 1);
        assert!((g.top_share() - 1.0).abs() < 1e-9, "退化池的最大占比 = 100%");
    }

    /// 基线面必须一个浮点都没有——跨版本逐字节比对的前提。
    #[test]
    fn baseline_json_contains_no_floats() {
        let facts = vec![WorldQualityFacts {
            world_id: "w1".into(),
            status: "ended".into(),
            conclusion_reason: "mainline_complete".into(),
            ending_id: "e_alliance".into(),
            committed_ticks: 3,
            events_total: 9,
            ..Default::default()
        }];
        let baseline = WorldQualityReport::of(&facts).baseline_json();
        fn assert_no_float(v: &Value, path: &str) {
            match v {
                Value::Number(n) => assert!(n.is_i64() || n.is_u64(), "{path} 出现浮点：{n}"),
                Value::Object(m) => {
                    for (k, sub) in m {
                        assert_no_float(sub, &format!("{path}.{k}"));
                    }
                }
                Value::Array(a) => {
                    for (i, sub) in a.iter().enumerate() {
                        assert_no_float(sub, &format!("{path}[{i}]"));
                    }
                }
                _ => {}
            }
        }
        assert_no_float(&baseline, "$");
        // 人读面反过来必须带上诚实边界（数被复制走时这段跟着走）。
        let human = WorldQualityReport::of(&facts).to_json(QualitySource::SimulatedStub);
        assert_eq!(human["source"], "simulated-stub");
        let honesty = human["honesty"].as_array().unwrap();
        assert!(!honesty.is_empty());
        assert!(
            honesty.iter().any(|s| s.as_str().unwrap_or_default().contains("不等于内容质量已验证")),
            "桩数据的报告必须自带「不等于内容质量已验证」这句话"
        );
    }
}
