//! 叙事质量 SLO（`docs/VALIDATION.md` §4.2 验证基建三件套第二件）：**只读观测**，进运营看板。
//!
//! **模块边界（红线）**：只读、只算、只报。本模块不含任何 INSERT/UPDATE/DELETE，也绝不把算出来的数
//! 回灌进 `worlds.narrative_state_json`——那会让引擎读到结算侧数值，直接违反「不卖胜负与数值平权」
//! （同 `world_contributions` 单独建表的理由，见迁移 0025 注释）。
//!
//! **八项 SLO 现在有六项算得出来**（数据可得性核实于 2026-07-26，见 VALIDATION §4.2 那张表）：
//!
//! | 指标 | 本模块 | 数据源 |
//! |---|---|---|
//! | 叙事注意力基尼系数 | ✅ | `world_contributions` ∩ `world_members`（**须取交集**，NPC 也入账本） |
//! | 角色最长连续无有效戏份拍数 | ✅ | `world_events.actors_json` × `world_ticks` 拍域 ∩ `world_members` |
//! | 强制收尾率 | ✅ | `audit_logs(action='world.ended')` 的 reason 前缀，分母 `worlds.status='ended'` |
//! | 同角色二次入世率 | ✅ | `world_members` 的 `COUNT(DISTINCT world_id) >= 2`，分母 `cloud_characters.withdrawn=0` |
//! | OOC 申诉率 | ✅ 可算（2026-07-26） | `ooc_appeals`（迁移 0037，R3 OOC 注解权）。🔴 **不是** `moderation_appeals`——那是内容风控申诉，与「演得不像」零关系 |
//! | 剧情重复率 | ❌ 无数据源 | fact 文本已进投影，但**相似度口径未拍板**（用什么算法、多相似算重复） |
//! | 状态-文本矛盾率 | ✅ 可算（2026-07-26） | `world_tick_critic`（迁移 0030）；每个已提交 tick 恒落一行故分母可信 |
//! | 用户跳过/退出率 | （不在本模块）| 退出侧已可算：`world_members.left_at`（迁移 0030）在 leave 时写入；
//! |  |  | 但历史行 NULL = 退出时刻未知，统计留存须排除 `status <> 'active' AND left_at IS NULL` 的盲区行。「跳过」仍无埋点 |
//!
//! 🔴 **不可算的指标必须显式标注为「无数据源」而不是 0 或空**（`SloStatus::NoDataSource`，
//! `value` 恒为 `null`）——后台显示 `—` 与显示 `0%` 是两个完全不同的经营判断，混同即事故。
//!
//! **参数化（VALIDATION §0.2 禁写死）**：门槛与扫描上限全部落在 `SloConfig`，env 可覆盖，
//! 测试直接构造。T2 门槛「基尼 ≤0.35」是 VALIDATION §2 T2 的原文数值，作为**默认值**而非常量语义。
//!
//! **双库可移植（`db.rs` 约定）**：无 `strftime`/`date_trunc` 等方言函数；日界一律由调用方
//! （`admin_api::dashboards` 的 `utc_day_start_ms` + `DAY_MS`）在 Rust 侧算成 BIGINT 毫秒区间传入；
//! 所有聚合列 `CAST(... AS BIGINT)`（PG 下 `SUM/COUNT` 可能返回 numeric，不 CAST 会解码失败）。
//!
//! **性能（本模块的产物挂在被轮询的后台端点上）**：每个指标**一次 GROUP BY / 一次范围扫描**，
//! 绝无按世界逐个发 SQL 的 N+1；两个逐行扫描的指标（基尼、无戏份）走 `LIMIT cap+1` 的溢出探测，
//! 超过 `scan_row_cap` 就**明说跳过**（`SloStatus::SkippedTooLarge`）而不是硬算——
//! 宁可这一栏显示"数据量超限"，也不许把运营后台拖垮。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::error::ApiError;

/// **世界质量三指标**（完读率 / 阻断率 / 结局分布，总规格 §4「世界质量回归」）。
///
/// 与本文件 §4 的八项 SLO **分母不同、用途不同**，故物理分开：
/// 本文件 = 平台运营滚动窗口；`quality` = 一批世界的全生命周期（仿真试跑 / 灰度批次）。
/// 但**分类口径同源**——`quality` 复用本文件的 `classify_conclusion` / `parse_ended_reason` /
/// `gini_coefficient` / `rate`，不另立第二套。
pub(crate) mod quality;

/// **拍域谓词**（本模块所有「以拍为分母」的指标共用的一把尺，写死成一处避免各写各的漂移）。
///
/// 语义 = **真正演出来、并把事件落进 `world_events` 的拍**。三个条件缺一不可：
/// - `status='done'` —— 排掉 failed / 还没跑完的 pending·running；
/// - `cost_tokens > 0` —— 排掉一个模型都没调过的空转拍（`world_not_running` / `superseded` /
///   `no_model_config` / `insufficient_members` / 终局短路），它们**根本没有回合**，
///   把它们算作「没戏份」是噪声；
/// - `error IS NULL` —— 排掉**跑了但没提交**的拍，当前只有一种：`blocked`
///   （硬节点/不变量/底线拦下，`runtime::finish_tick_blocked`）。
///
/// 🔴 第三个条件是 #42 的连带项，**不是可选的**。在 #42 之前阻断拍被错记成 `cost_tokens=0`，
/// 于是恰好被第二个条件挡在拍域之外；#42 把它改成记真实成本之后，若不补这一条，
/// 一批「引擎跑了但一个事件都没落」的拍会突然涌进无戏份/申诉率的分母，
/// 让「全员被晾着」的假信号随阻断率一起上涨——而阻断本身已有 `quality::blocked_ticks` 在专门盯。
/// 换句话说：**成本口径与叙事口径必须在这里分家**，一个问"花了多少钱"，一个问"演出来没有"。
///
/// 用法：把 `world_ticks` **一律别名为 `wt`**，再把本谓词拼进 `WHERE`，如
/// `format!("... FROM world_ticks wt WHERE {TICK_DOMAIN} AND wt.created_at >= ?")`。
/// 列名带 `wt.` 前缀是必需的：`ooc_appeal_block` 里它跑在以 `world_members` 为外层的相关子查询中，
/// 而 `world_members` 也有 `status` 列——不加前缀就靠 SQL 的作用域优先级"碰巧对"，两库一致性不值得赌。
/// 只用可移植 SQL 子集（`db.rs`）：无方言函数、无 JSON 运算。
const TICK_DOMAIN: &str = "wt.status = 'done' AND wt.cost_tokens > 0 AND wt.error IS NULL";

// ============================================================================
// §1 指标口径纯函数（从 `runtime::golden` 提升为生产代码；口径注释原样保留）
// ============================================================================

/// 基尼系数：叙事注意力公平度，**T2 门槛「≤0.35」的算法实现**。
///
/// `G = ΣᵢΣⱼ|xᵢ-xⱼ| / (2·n·Σx)`，值域 `[0, 1)`：0 = 完全均分，越大越集中。
///
/// 🔴 输入必须是**已与 `world_members` 取过交集**的成员贡献分。`world_contributions` 把 NPC
/// （世界固有角色）一并入表（引擎按 character_id 折算不区分主体，见迁移 0025 注释），
/// 不取交集就会把"NPC 拿了多少戏"算进玩家公平度里 —— 见 `world_attention_gini` 与
/// `gini_excludes_world_controlled_npc` 用例。
///
/// 纯函数、无 IO、无浮点 RNG；空集与全零集返回 0.0（"无人有戏"不是不公平，是没开演）。
pub(crate) fn gini_coefficient(values: &[i64]) -> f64 {
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    let total: i128 = values.iter().map(|v| *v as i128).sum();
    if total <= 0 {
        return 0.0;
    }
    let mut abs_diff_sum: i128 = 0;
    for a in values {
        for b in values {
            abs_diff_sum += (*a as i128 - *b as i128).abs();
        }
    }
    abs_diff_sum as f64 / (2.0 * n as f64 * total as f64)
}

/// 「有效戏份」的事件类型口径：只认叙事事件（行动 / 对白）。
/// `consent_request` 这类**流程事件**不构成戏份——否则"被同意门拦下、这一拍什么也没演成"
/// 会被算作有戏，正好把最该被指标抓住的情况漏掉。
pub(crate) const NARRATIVE_EVENT_TYPES: &[&str] = &["action", "dialogue"];

/// 每个成员的**最长连续无有效戏份拍数**（VALIDATION §4.2「角色连续 N 拍无有效戏份比例」的底座）。
///
/// `appearances` 是 `(tick_no, character_id)` 的**规范化**出场集合 —— 刻意不用现有查询的
/// `LIKE '%cid%'`：那种写法在 id 互为前缀时会误判（`li` 命中 `lixia`），做统计必须精确解析
/// `actors_json`。纯函数，`ticks` 与 `members` 由调用方给定全域。
pub(crate) fn max_silent_streaks(
    members: &[String],
    ticks: &[i64],
    appearances: &BTreeSet<(i64, String)>,
) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for m in members {
        let (mut cur, mut best) = (0i64, 0i64);
        for t in ticks {
            if appearances.contains(&(*t, m.clone())) {
                cur = 0;
            } else {
                cur += 1;
                best = best.max(cur);
            }
        }
        out.insert(m.clone(), best);
    }
    out
}

/// 收尾类型（VALIDATION §4.2「强制收尾率」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConclusionKind {
    /// 主线走完 —— 唯一的自然收尾。
    Natural,
    /// 世界时间上限 / 无可调度角色 —— 被系统掐掉。
    Forced,
    /// 世界线崩塌（关键角色永久退场）—— 也是强制收尾，但结算口径不同（③ 归零 · ① 减半）。
    Collapsed,
    /// 尚未收尾 / reason 无法识别。
    Unknown,
}

impl ConclusionKind {
    /// 响应里的稳定机读串（前端按它分桶，不要改）。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Forced => "forced",
            Self::Collapsed => "collapsed",
            Self::Unknown => "unknown",
        }
    }
}

/// `terminal_reason()` 与 runtime 终局路径产出的 reason 串 → 收尾类型。
///
/// 口径来源：`runtime::terminal_reason`（`mainline_complete` / `time_cap` / `starved`）+
/// `commit_tick` 的时间上限 `time_limit` + `key_character_exited` 的 `key_character_exit`。
/// **未知 reason 一律不算自然收尾**（保守：宁可把强制收尾率算高，也不许悄悄漏掉）。
pub(crate) fn classify_conclusion(reason: &str) -> ConclusionKind {
    match reason {
        "mainline_complete" => ConclusionKind::Natural,
        "time_cap" | "time_limit" | "starved" => ConclusionKind::Forced,
        "key_character_exit" => ConclusionKind::Collapsed,
        "" => ConclusionKind::Unknown,
        _ => ConclusionKind::Unknown,
    }
}

/// 是否**强制收尾**（`Natural` 之外全是；`Unknown` 保守计入强制）。
pub(crate) fn is_forced_conclusion(reason: &str) -> bool {
    classify_conclusion(reason) != ConclusionKind::Natural
}

/// 拆 `audit_logs('world.ended').reason` 的写入格式 `{reason}|ending={ending}`
/// （见 `runtime::finalize_ending_tx`），返回 `(reason, ending)`。无分隔符时整串即 reason。
pub(crate) fn parse_ended_reason(raw: &str) -> (String, String) {
    let (reason, rest) = raw.split_once('|').unwrap_or((raw, ""));
    (reason.to_string(), rest.trim_start_matches("ending=").to_string())
}

// ============================================================================
// §2 单世界口径（黄金世界回归的消费面；预留给世界诊断下钻）
// ============================================================================
//
// 这三个函数是**单世界全生命周期**口径，与 §4 的平台级窗口口径不同（后者按运营窗口滚动）。
// 当前唯一消费方是 `runtime::golden`（黄金世界回归）；`/admin/worlds/{id}/diagnostics` 的
// 逐世界下钻是自然的下一个消费方，但 `worlds_ops.rs` 不在本次改动范围内，故暂标 allow(dead_code)。

/// 读账本算基尼：`world_contributions.score_milli` ∩ `world_members`（active 与已退场的都算——
/// 退场者在场时挣的戏份不该凭空消失），返回 `(基尼, 参与统计的成员数)`。
#[allow(dead_code)]
pub(crate) async fn world_attention_gini(
    db: &AnyPool,
    world_id: &str,
) -> Result<(f64, usize), ApiError> {
    let rows = sqlx::query(
        "SELECT wc.score_milli AS s FROM world_contributions wc \
         JOIN world_members wm ON wm.world_id = wc.world_id AND wm.cloud_character_id = wc.character_id \
         WHERE wc.world_id = $1 ORDER BY wc.character_id ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    let mut values: Vec<i64> = Vec::with_capacity(rows.len());
    for r in &rows {
        values.push(r.try_get::<i64, _>("s")?);
    }
    Ok((gini_coefficient(&values), values.len()))
}

/// 读库算无戏份拍数（单世界全生命周期）。
///
/// **拍域口径** = 真正演出来并落了事件的拍
/// （`world_ticks.status='done' AND cost_tokens > 0 AND error IS NULL`，见 [`TICK_DOMAIN`]）。
/// 终局短路拍 / `insufficient_members` 跳过拍根本没有回合，把它们算作「没戏份」是噪声。
#[allow(dead_code)]
pub(crate) async fn world_silent_streaks(
    db: &AnyPool,
    world_id: &str,
) -> Result<BTreeMap<String, i64>, ApiError> {
    let rows = sqlx::query(
        "SELECT cloud_character_id AS c FROM world_members WHERE world_id = $1 \
         ORDER BY cloud_character_id ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    let mut members: Vec<String> = Vec::with_capacity(rows.len());
    for r in &rows {
        members.push(r.try_get::<String, _>("c")?);
    }

    let sql = format!(
        "SELECT wt.tick_no AS tick_no FROM world_ticks wt WHERE wt.world_id = $1 AND {TICK_DOMAIN} \
         ORDER BY wt.tick_no ASC"
    );
    let rows = sqlx::query(&sql).bind(world_id).fetch_all(db).await?;
    let mut ticks: Vec<i64> = Vec::with_capacity(rows.len());
    for r in &rows {
        ticks.push(r.try_get::<i64, _>("tick_no")?);
    }

    let rows = sqlx::query(
        "SELECT tick_no, event_type, actors_json FROM world_events WHERE world_id = $1 \
         ORDER BY sequence ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await?;
    let mut appearances: BTreeSet<(i64, String)> = BTreeSet::new();
    for r in &rows {
        let et: String = r.try_get("event_type")?;
        if !NARRATIVE_EVENT_TYPES.contains(&et.as_str()) {
            continue;
        }
        let tick_no: i64 = r.try_get("tick_no")?;
        let raw: String = r.try_get("actors_json")?;
        // 规范化解析，不用 LIKE 子串匹配。
        for actor in parse_actors(&raw) {
            appearances.insert((tick_no, actor));
        }
    }
    Ok(max_silent_streaks(&members, &ticks, &appearances))
}

/// 从审计取本世界的收尾 `(reason, ending)`。
///
/// 事实源选 `audit_logs('world.ended')` 而非 `world_ticks.error`：走 `commit_tick` 的收尾会把
/// `world_ticks.error` 置 NULL（同一条 UPDATE 里），只有 `conclude_world_no_round` 那条路径才写 error。
/// 审计行两条路径都写，是唯一齐全的口径。
#[allow(dead_code)]
pub(crate) async fn world_conclusion(
    db: &AnyPool,
    world_id: &str,
) -> Result<(String, String), ApiError> {
    let Some(row) = sqlx::query(
        "SELECT reason FROM audit_logs WHERE action = 'world.ended' AND subject = $1 LIMIT 1",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?
    else {
        return Ok((String::new(), String::new()));
    };
    Ok(parse_ended_reason(&row.try_get::<String, _>("reason")?))
}

/// `actors_json`（JSON 文本数组）→ 角色 id 列表。解析失败返回空（宁可少算戏份，不许瞎猜）。
fn parse_actors(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

// ============================================================================
// §3 运营参数（VALIDATION §0.2：门槛与上限一律可配置，禁止写死）
// ============================================================================

/// T2 门槛：叙事注意力基尼 ≤0.35（VALIDATION §2 · T2「叙事注意力公平」原文数值）。
/// 这是**当前默认门槛**而非物理常量——预注册纪律要求"开测前可改、开测后冻结"，故经 env 可覆盖。
const DEFAULT_ATTENTION_GINI_MAX: f64 = 0.35;
/// 「连续 N 拍无有效戏份」的告警档位（拍）。VALIDATION §4.2 只写「连续 N 拍」，N 本就是运营参数。
const DEFAULT_SILENT_STREAK_MAX: i64 = 3;
/// T1 门槛：OOC/裁决不公申诉 **<10%/阶段**（VALIDATION §2 · T1 原文数值）。
/// 同基尼门槛，这是**当前默认门槛**而非物理常量——预注册纪律要求"开测前可改、开测后冻结"。
const DEFAULT_OOC_APPEAL_RATE_MAX: f64 = 0.10;
/// SLO 观测窗口默认天数（滚动窗口；成本趋势用 7 天是"看走势"，叙事质量看的是"这一批世界演得怎么样"）。
pub(crate) const DEFAULT_SLO_WINDOW_DAYS: i64 = 30;
/// 单个逐行扫描指标的行数上限。超过即**跳过该指标并明说原因**，不硬算——保护被轮询的后台端点。
const DEFAULT_SCAN_ROW_CAP: i64 = 50_000;
/// 榜单长度（最不公平的世界 / 最久没戏的角色只展示头部；分布统计仍覆盖窗口全量）。
const TOP_N: usize = 10;

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.parse::<i64>().ok()).filter(|v| *v > 0).unwrap_or(default)
}

/// SLO 计算参数。窗口由调用方按 `dashboards::utc_day_start_ms` + `DAY_MS` 算好毫秒区间传入
/// （日界口径全仓一处，SQL 侧永远只做 BIGINT 范围过滤）。
#[derive(Debug, Clone)]
pub(crate) struct SloConfig {
    /// 窗口天数（仅用于回显，真正生效的是下面两个毫秒边界）。
    pub days: i64,
    /// 窗口左闭边界（ms）。
    pub window_start: i64,
    /// 窗口右开边界（ms）。
    pub window_end: i64,
    /// 基尼门槛（T2）。
    pub gini_max: f64,
    /// OOC 申诉率门槛（T1「<10%/阶段」）。
    pub ooc_appeal_rate_max: f64,
    /// 连续无戏份告警档位（拍）。
    pub silent_streak_max: i64,
    /// 单指标扫描行数上限。
    pub scan_row_cap: i64,
}

impl SloConfig {
    /// env 覆盖 + 默认值。`MUSE_SLO_GINI_MAX` / `MUSE_SLO_SILENT_STREAK_TICKS` /
    /// `MUSE_SLO_SCAN_ROW_CAP` / `MUSE_SLO_OOC_APPEAL_RATE_MAX`。
    pub(crate) fn from_env(days: i64, window_start: i64, window_end: i64) -> Self {
        Self {
            days,
            window_start,
            window_end,
            gini_max: env_f64("MUSE_SLO_GINI_MAX", DEFAULT_ATTENTION_GINI_MAX),
            ooc_appeal_rate_max: env_f64(
                "MUSE_SLO_OOC_APPEAL_RATE_MAX",
                DEFAULT_OOC_APPEAL_RATE_MAX,
            ),
            silent_streak_max: env_i64("MUSE_SLO_SILENT_STREAK_TICKS", DEFAULT_SILENT_STREAK_MAX),
            scan_row_cap: env_i64("MUSE_SLO_SCAN_ROW_CAP", DEFAULT_SCAN_ROW_CAP),
        }
    }
}

/// 除法：分母 ≤0 一律 0.0（无数据不等于 0%，故调用处必须同时给出分子分母原始计数供前端判空）。
fn rate(numer: i64, denom: i64) -> f64 {
    if denom <= 0 {
        0.0
    } else {
        (numer as f64) / (denom as f64)
    }
}

/// 最近秩百分位（不插值）。空集 → None（不除零、不编 0）。
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((p * sorted.len() as f64).ceil() as usize).saturating_sub(1);
    sorted.get(idx.min(sorted.len() - 1)).copied()
}

/// 溢出探测：查询恒带 `LIMIT cap+1`，行数超过 cap 即判定"数据量超限"。
fn over_cap(rows: usize, cap: i64) -> bool {
    rows as i64 > cap
}

/// 数据量超限时的统一响应体：**给出上限与实测下界，不给数**。
fn skipped_too_large(metric: &str, title: &str, cap: i64) -> Value {
    json!({
        "metric": metric,
        "title": title,
        "status": "skipped_too_large",
        "value": Value::Null,
        "rowCap": cap,
        "reason": format!(
            "窗口内待扫描行数超过上限 {cap}，为保护被轮询的后台端点本次不计算；\
             请缩小 ?sloDays= 窗口，或调高 MUSE_SLO_SCAN_ROW_CAP 后单独查询。"
        ),
    })
}

// ============================================================================
// §4 平台级聚合（四个可算指标；每项一次 GROUP BY / 一次范围扫描，无 N+1）
// ============================================================================

/// 叙事注意力基尼：按世界算，再出平台级分布。
///
/// **一次查询**取回窗口内世界的「成员 × 贡献分」全量行（`world_contributions` JOIN `world_members`，
/// 🔴 交集即 NPC 过滤器），Rust 侧按 world_id 分组算逐世界基尼。
///
/// 窗口口径：`wc.world_id IN (窗口内有贡献分更新的世界)` —— 注意**不是**过滤单条贡献行。
/// `world_contributions` 是累计账本，逐行过滤会把"本窗口内没再拿到戏"的成员整条剔掉，
/// 基尼立刻失真（分母少了最该被看见的那些人）；按世界筛选则保证每个入选世界都是**全员全量**的。
///
/// 单成员世界（`n < 2`）单列：基尼在 n=1 时恒为 0，混进均值会把平台公平度洗白。
async fn attention_gini_block(db: &AnyPool, cfg: &SloConfig) -> Result<Value, ApiError> {
    const METRIC: &str = "attentionGini";
    const TITLE: &str = "叙事注意力基尼系数（每角色有效戏份分布）";

    let sql = format!(
        "SELECT wc.world_id AS world_id, wc.score_milli AS score \
         FROM world_contributions wc \
         JOIN world_members wm ON wm.world_id = wc.world_id AND wm.cloud_character_id = wc.character_id \
         WHERE wc.world_id IN ( \
             SELECT world_id FROM world_contributions WHERE updated_at >= $1 AND updated_at < $2 \
         ) \
         ORDER BY wc.world_id ASC, wc.character_id ASC LIMIT {}",
        cfg.scan_row_cap + 1
    );
    let rows = sqlx::query(&sql)
        .bind(cfg.window_start)
        .bind(cfg.window_end)
        .fetch_all(db)
        .await?;
    if over_cap(rows.len(), cfg.scan_row_cap) {
        return Ok(skipped_too_large(METRIC, TITLE, cfg.scan_row_cap));
    }

    let mut by_world: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for r in &rows {
        by_world
            .entry(r.try_get::<String, _>("world_id")?)
            .or_default()
            .push(r.try_get::<i64, _>("score")?);
    }

    let mut single_member = 0i64;
    let mut ginis: Vec<(String, f64, usize)> = Vec::new();
    for (world_id, scores) in &by_world {
        if scores.len() < 2 {
            single_member += 1;
            continue;
        }
        ginis.push((world_id.clone(), gini_coefficient(scores), scores.len()));
    }

    let mut sorted: Vec<f64> = ginis.iter().map(|(_, g, _)| *g).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let counted = ginis.len() as i64;
    let over = ginis.iter().filter(|(_, g, _)| *g > cfg.gini_max).count() as i64;
    let mean = if counted > 0 { Some(sorted.iter().sum::<f64>() / counted as f64) } else { None };

    // 平台级分布直方图：**门槛本身就是一条桶边界**，"越线了多少个世界"一眼可读。
    // 门槛可被 env 调到任意值，故边界排序后再用（乱序边界会让分桶张冠李戴）。
    let mut edges = vec![0.10_f64, 0.20, cfg.gini_max, 0.50];
    edges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    edges.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
    let mut buckets = vec![0i64; edges.len() + 1];
    for (_, g, _) in &ginis {
        let mut idx = edges.len();
        for (i, e) in edges.iter().enumerate() {
            if *g <= *e {
                idx = i;
                break;
            }
        }
        buckets[idx] += 1;
    }
    let mut distribution = Vec::with_capacity(buckets.len());
    let mut low = 0.0_f64;
    for (i, n) in buckets.iter().enumerate() {
        let high = edges.get(i).copied();
        distribution.push(json!({
            "from": low,
            "to": high,          // null = 最后一桶（开区间到 1）
            "worlds": n,
        }));
        low = high.unwrap_or(1.0);
    }

    let mut worst = ginis.clone();
    worst.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    worst.truncate(TOP_N);

    Ok(json!({
        "metric": METRIC,
        "title": TITLE,
        "status": "ok",
        "source": "world_contributions ∩ world_members（交集即 NPC 过滤器）",
        "threshold": cfg.gini_max,
        "thresholdSource": "VALIDATION §2 · T2「叙事注意力公平：基尼 ≤0.35」",
        "worldsCounted": counted,
        "worldsSingleMember": single_member,
        "worldsOverThreshold": over,
        "overThresholdRate": rate(over, counted),
        "meanGini": mean,
        "medianGini": percentile(&sorted, 0.5),
        "p90Gini": percentile(&sorted, 0.9),
        "maxGini": sorted.last().copied(),
        "distribution": distribution,
        "worstWorlds": worst
            .iter()
            .map(|(w, g, n)| json!({ "worldId": w, "gini": g, "members": *n as i64 }))
            .collect::<Vec<_>>(),
        "rowsScanned": rows.len() as i64,
    }))
}

/// 角色最长连续无有效戏份拍数。
///
/// **三条窗口内范围扫描**（拍域 / 成员 / 出场），Rust 侧按世界分组后套 `max_silent_streaks`：
/// ① 拍域 = [`TICK_DOMAIN`]（真演出来的拍；空转跳过拍与阻断拍都不是"没戏份"）；
/// ② 成员 = 拍域涉及世界的 `world_members` 全集（差集的被减数）；
/// ③ 出场 = `event_type ∈ NARRATIVE_EVENT_TYPES` 的事件，`actors_json` **规范化解析**
///    （不用 `LIKE '%cid%'`：`li` 会被 `lixia` 蹭到戏份）。
///
/// 与 `world_silent_streaks` 的差别：这里是**滚动窗口**口径（最近 N 天的拍域），
/// 单世界函数是全生命周期口径。运营看板要回答的是"最近这批世界有没有人被晾着"。
async fn silent_streak_block(db: &AnyPool, cfg: &SloConfig) -> Result<Value, ApiError> {
    const METRIC: &str = "silentStreak";
    const TITLE: &str = "角色最长连续无有效戏份拍数";
    let cap = cfg.scan_row_cap;

    // ① 拍域。
    let sql = format!(
        "SELECT wt.world_id AS world_id, wt.tick_no AS tick_no FROM world_ticks wt \
         WHERE {TICK_DOMAIN} AND wt.created_at >= $1 AND wt.created_at < $2 \
         ORDER BY wt.world_id ASC, wt.tick_no ASC LIMIT {}",
        cap + 1
    );
    let tick_rows =
        sqlx::query(&sql).bind(cfg.window_start).bind(cfg.window_end).fetch_all(db).await?;
    if over_cap(tick_rows.len(), cap) {
        return Ok(skipped_too_large(METRIC, TITLE, cap));
    }
    let mut ticks_by_world: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for r in &tick_rows {
        ticks_by_world
            .entry(r.try_get::<String, _>("world_id")?)
            .or_default()
            .push(r.try_get::<i64, _>("tick_no")?);
    }

    // ② 成员全集（只取拍域涉及的世界，子查询与 ① 同条件，避免把全平台成员表拖进来）。
    let sql = format!(
        "SELECT world_id, cloud_character_id FROM world_members WHERE world_id IN ( \
             SELECT wt.world_id FROM world_ticks wt \
             WHERE {TICK_DOMAIN} AND wt.created_at >= $1 AND wt.created_at < $2 \
         ) ORDER BY world_id ASC, cloud_character_id ASC LIMIT {}",
        cap + 1
    );
    let member_rows =
        sqlx::query(&sql).bind(cfg.window_start).bind(cfg.window_end).fetch_all(db).await?;
    if over_cap(member_rows.len(), cap) {
        return Ok(skipped_too_large(METRIC, TITLE, cap));
    }
    let mut members_by_world: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in &member_rows {
        members_by_world
            .entry(r.try_get::<String, _>("world_id")?)
            .or_default()
            .push(r.try_get::<String, _>("cloud_character_id")?);
    }

    // ③ 出场（有效戏份口径写死在 NARRATIVE_EVENT_TYPES，SQL 谓词由它拼出来，单一事实源）。
    let types = NARRATIVE_EVENT_TYPES.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT world_id, tick_no, actors_json FROM world_events \
         WHERE event_type IN ({types}) AND occurred_at >= $1 AND occurred_at < $2 LIMIT {}",
        cap + 1
    );
    let event_rows =
        sqlx::query(&sql).bind(cfg.window_start).bind(cfg.window_end).fetch_all(db).await?;
    if over_cap(event_rows.len(), cap) {
        return Ok(skipped_too_large(METRIC, TITLE, cap));
    }
    let mut appearances: BTreeMap<String, BTreeSet<(i64, String)>> = BTreeMap::new();
    for r in &event_rows {
        let world_id: String = r.try_get("world_id")?;
        let tick_no: i64 = r.try_get("tick_no")?;
        let raw: String = r.try_get("actors_json")?;
        let set = appearances.entry(world_id).or_default();
        for actor in parse_actors(&raw) {
            set.insert((tick_no, actor));
        }
    }

    // 逐世界套纯函数。
    let empty: BTreeSet<(i64, String)> = BTreeSet::new();
    let mut all: Vec<(String, String, i64)> = Vec::new();
    let mut worlds_counted = 0i64;
    let mut ticks_counted = 0i64;
    let mut max_streak = 0i64;
    let mut sum_streak = 0i64;
    let mut over = 0i64;
    for (world_id, ticks) in &ticks_by_world {
        let Some(members) = members_by_world.get(world_id) else {
            continue; // 只有 NPC / 无成员的世界（成员全集为空）：没有"谁被晾着"这个量。
        };
        if members.is_empty() {
            continue;
        }
        worlds_counted += 1;
        ticks_counted += ticks.len() as i64;
        let app = appearances.get(world_id).unwrap_or(&empty);
        for (cid, streak) in max_silent_streaks(members, ticks, app) {
            max_streak = max_streak.max(streak);
            sum_streak += streak;
            if streak >= cfg.silent_streak_max {
                over += 1;
            }
            all.push((world_id.clone(), cid, streak));
        }
    }
    let members_counted = all.len() as i64;
    let mean = if members_counted > 0 { Some(sum_streak as f64 / members_counted as f64) } else { None };

    let mut worst = all;
    worst.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    worst.truncate(TOP_N);

    Ok(json!({
        "metric": METRIC,
        "title": TITLE,
        "status": "ok",
        "source": "world_events.actors_json（规范化解析）× world_ticks 拍域 ∩ world_members",
        "narrativeEventTypes": NARRATIVE_EVENT_TYPES,
        "threshold": cfg.silent_streak_max,
        "worldsCounted": worlds_counted,
        "ticksCounted": ticks_counted,
        "membersCounted": members_counted,
        "maxStreak": max_streak,
        "meanStreak": mean,
        "membersOverThreshold": over,
        "overThresholdRate": rate(over, members_counted),
        "worstMembers": worst
            .iter()
            .map(|(w, c, s)| json!({ "worldId": w, "characterId": c, "streak": s }))
            .collect::<Vec<_>>(),
        "rowsScanned": event_rows.len() as i64,
    }))
}

/// 强制收尾率。
///
/// 事实源 `audit_logs(action='world.ended')` 的 reason 前缀 —— **不用 `world_ticks.error`**：
/// 走 `commit_tick` 的收尾会在同一条 UPDATE 里把 `error` 置 NULL，只有 `conclude_world_no_round`
/// 那条路径才写 error，`world_ticks.error` 天然缺一半样本；审计行两条路径都写，是唯一齐全的口径。
///
/// 查询形态是 **`GROUP BY reason`**：结果集大小 = 不同 reason 串的种类数（个位数量级），
/// 与已结束世界数无关，不随时间膨胀，无需窗口。分母按规格取 `worlds WHERE status='ended'`。
///
/// 无审计行的已结束世界记入 `unaccounted`，**保守计入强制**（同 `classify_conclusion` 的 Unknown 口径）。
async fn forced_conclusion_block(db: &AnyPool) -> Result<Value, ApiError> {
    let ended: i64 = sqlx::query("SELECT CAST(COUNT(*) AS BIGINT) AS n FROM worlds WHERE status = 'ended'")
        .fetch_one(db)
        .await?
        .try_get("n")?;

    let rows = sqlx::query(
        "SELECT reason, CAST(COUNT(DISTINCT subject) AS BIGINT) AS n FROM audit_logs \
         WHERE action = 'world.ended' GROUP BY reason ORDER BY reason ASC",
    )
    .fetch_all(db)
    .await?;
    let classified: i64 = sqlx::query(
        "SELECT CAST(COUNT(DISTINCT subject) AS BIGINT) AS n FROM audit_logs WHERE action = 'world.ended'",
    )
    .fetch_one(db)
    .await?
    .try_get("n")?;

    let mut by_kind: BTreeMap<&'static str, i64> = BTreeMap::new();
    let mut by_reason: Vec<Value> = Vec::with_capacity(rows.len());
    let mut forced = 0i64;
    for r in &rows {
        let raw: String = r.try_get("reason")?;
        let n: i64 = r.try_get("n")?;
        let (reason, ending) = parse_ended_reason(&raw);
        let kind = classify_conclusion(&reason);
        *by_kind.entry(kind.as_str()).or_insert(0) += n;
        if is_forced_conclusion(&reason) {
            forced += n;
        }
        by_reason.push(json!({
            "reason": reason,
            "ending": ending,
            "kind": kind.as_str(),
            "forced": is_forced_conclusion(&reason),
            "worlds": n,
        }));
    }
    // 已结束但没有审计行的世界：口径未知 → 与 `classify_conclusion("")` 一致，计入强制。
    let unaccounted = (ended - classified).max(0);
    *by_kind.entry(ConclusionKind::Unknown.as_str()).or_insert(0) += unaccounted;
    let forced_total = forced + unaccounted;

    Ok(json!({
        "metric": "forcedConclusionRate",
        "title": "强制收尾率",
        "status": "ok",
        "source": "audit_logs(action='world.ended').reason 前缀；分母 worlds.status='ended'",
        "endedWorlds": ended,
        "classifiedWorlds": classified,
        "unaccountedWorlds": unaccounted,
        "forcedWorlds": forced_total,
        "forcedRate": rate(forced_total, ended),
        "byKind": {
            "natural": by_kind.get("natural").copied().unwrap_or(0),
            "forced": by_kind.get("forced").copied().unwrap_or(0),
            "collapsed": by_kind.get("collapsed").copied().unwrap_or(0),
            "unknown": by_kind.get("unknown").copied().unwrap_or(0),
        },
        "byReason": by_reason,
        "notes": [
            "自然收尾只有 mainline_complete 一种；time_cap/time_limit/starved=forced，key_character_exit=collapsed，未知 reason 保守计入强制。",
            "无 world.ended 审计行的已结束世界记入 unaccounted 并计入强制（宁可把强制率算高，不许悄悄漏掉）。",
        ],
    }))
}

/// 同角色二次入世率。
///
/// `world_members` 的 `COUNT(DISTINCT world_id) >= 2`（唯一索引 `(world_id, cloud_character_id)`
/// 保证同一世界不会重复计数），分母 `cloud_characters WHERE withdrawn = 0`。
/// 三条标量查询，全部走 GROUP BY / COUNT DISTINCT，无逐行回传。
///
/// **全生命周期口径、不加窗口**：二次入世本就是留存量，切窗口会把"上个月进的第一个世界"截断，
/// 把留存算成 0。
async fn repeat_entry_block(db: &AnyPool) -> Result<Value, ApiError> {
    let characters_total: i64 =
        sqlx::query("SELECT CAST(COUNT(*) AS BIGINT) AS n FROM cloud_characters WHERE withdrawn = 0")
            .fetch_one(db)
            .await?
            .try_get("n")?;
    let ever_joined: i64 = sqlx::query(
        "SELECT CAST(COUNT(DISTINCT cloud_character_id) AS BIGINT) AS n FROM world_members",
    )
    .fetch_one(db)
    .await?
    .try_get("n")?;
    let repeat: i64 = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM ( \
             SELECT cloud_character_id FROM world_members \
             GROUP BY cloud_character_id HAVING COUNT(DISTINCT world_id) >= 2 \
         ) t",
    )
    .fetch_one(db)
    .await?
    .try_get("n")?;

    Ok(json!({
        "metric": "repeatEntryRate",
        "title": "同角色二次入世率",
        "status": "ok",
        "source": "world_members COUNT(DISTINCT world_id)>=2；分母 cloud_characters.withdrawn=0",
        "charactersTotal": characters_total,
        "charactersEverJoined": ever_joined,
        "charactersTwoPlusWorlds": repeat,
        "repeatEntryRate": rate(repeat, characters_total),
        "repeatAmongJoinedRate": rate(repeat, ever_joined),
        "notes": [
            "全生命周期口径、不切窗口：二次入世是留存量，切窗口会把首次入世截断而把留存算成 0。",
            "charactersEverJoined 不过滤 withdrawn（已下架卡的历史入世事实仍然成立），故它可能大于 charactersTotal。",
        ],
    }))
}

/// 状态-文本矛盾率：引擎叙事 critic 报出问题的拍占比。
///
/// 数据源 `world_tick_critic`（迁移 0030，2026-07-26 补齐——此前 `CriticReport` 被 server
/// 解构后直接丢弃）。**每个已提交 tick 恒落一行（哪怕三列全空）**，所以行数本身就是分母：
/// 若改成"只在有问题时才写"，「跑了但很干净」与「历史 tick 从未落库」在库里长得一样，
/// 分母永远算不准。
///
/// 口径：分子 = `consistency_issue_count > 0 OR causal_issue_count > 0` 的拍
/// （`revision_suggestion_count` **不计入** —— 修订建议是"可以更好"，不是"自相矛盾"，
/// 混进来会把本指标从矛盾率稀释成"critic 话多率"）。
/// 与 `world_ticks.error='blocked'` 是**两个不同的指标**，不可互相冒充：后者统计"被不变量拦下的拍"
/// （压根没提交），本指标统计"提交了、但 critic 认为文本与状态对不上的拍"。
async fn contradiction_block(db: &AnyPool, cfg: &SloConfig) -> Result<Value, ApiError> {
    let row = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS total, \
         CAST(SUM(CASE WHEN consistency_issue_count > 0 OR causal_issue_count > 0 THEN 1 ELSE 0 END) AS BIGINT) AS flagged, \
         CAST(COALESCE(SUM(consistency_issue_count), 0) AS BIGINT) AS consistency, \
         CAST(COALESCE(SUM(causal_issue_count), 0) AS BIGINT) AS causal \
         FROM world_tick_critic WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(cfg.window_start)
    .bind(cfg.window_end)
    .fetch_one(db)
    .await?;

    let total: i64 = row.try_get("total")?;
    let flagged: i64 = row.try_get("flagged").unwrap_or(0);
    let consistency: i64 = row.try_get("consistency").unwrap_or(0);
    let causal: i64 = row.try_get("causal")?;

    // 窗口内一拍都没跑过 → 没测过，不是「矛盾率 0」。同 §5 的口径：显示 — 而不是 0%。
    if total == 0 {
        return Ok(json!({
            "metric": "stateTextContradictionRate",
            "title": "状态-文本矛盾率",
            "status": "no_data_in_window",
            "value": Value::Null,
            "ticksTotal": 0,
            "notes": ["窗口内没有已提交的 tick——「没测过」不是「矛盾率 0」，后台应显示 — 而非 0%。"],
        }));
    }

    Ok(json!({
        "metric": "stateTextContradictionRate",
        "title": "状态-文本矛盾率",
        "status": "ok",
        "value": flagged as f64 / total as f64,
        "ticksTotal": total,
        "ticksFlagged": flagged,
        "consistencyIssues": consistency,
        "causalIssues": causal,
        "notes": [
            "分母 = 窗口内已提交且落了 critic 行的拍；每个已提交 tick 恒落一行，故分母可信。",
            "分子 = 一致性问题或因果问题计数 > 0 的拍；revisionSuggestions 不计入（那是「可以更好」不是「自相矛盾」）。",
            "与 world_ticks.error='blocked' 不是同一指标：那统计被不变量拦下、压根没提交的拍。",
        ],
    }))
}

/// OOC 申诉率：**VALIDATION §2 T1 门槛「OOC/裁决不公申诉 <10%/阶段」的测量实现**。
///
/// 数据源 `ooc_appeals`（迁移 0037，R3「OOC 注解权」）。此前这一项是八项 SLO 里**唯一未解**的：
/// 🔴 全仓唯一的申诉表 `moderation_appeals` 是**内容风控申诉**（只受理 rejected 的卡/头像、
/// 每主体终身一次），与「角色演得不像 / 裁决不公」零关系，**不得拿来充数**——
/// 拿它算出来的数与本指标语义完全无关，比没有数更坏。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 口径（分子 / 分母 / 窗口 / 无数据）
/// ════════════════════════════════════════════════════════════════════════════
///
/// 门槛原文的单位是「**/阶段**」，而托梦配额的既有口径是「**一个 world 实例 = 一个阶段**」
/// （`interventions::dream_quota_per_stage` 上方注释）。本指标沿用同一口径，于是：
///
/// - **分母** = 窗口内**真正演过戏的**世界里的「角色 × 世界(阶段)」对数。
///   「演过戏」= 该世界在窗口内有落在 [`TICK_DOMAIN`] 里的拍
///   （与 `world_silent_streaks` 的拍域口径一致：终局短路拍 / 跳过拍没有回合，阻断拍跑了但什么也没演成，
///   都不构成可申诉的对象）。
///   没演过戏的世界里没有可申诉的东西，把它们算进分母只会**稀释**申诉率，
///   让一个本该报警的数看起来很安全。
///   `world_members` 的行即「玩家角色 × 世界」（NPC 不入该表，与 `world_contributions` 不同，
///   所以这里**不需要**像基尼那样再取交集）。
/// - **分子** = 窗口内新建的申诉，按 `(world_id, character_id)` **去重**后的对数。
///   去重是必须的：一个人对同一个世界的 5 拍各提一次，是**一个角色不满意**，不是 5 个。
///   不去重会让分子超过分母，得到 >100% 的申诉率。分子另加与分母**同样的两个 EXISTS 过滤**，
///   保证 `分子 ≤ 分母` 恒成立（窗口边界上申诉在窗内、拍在窗外的情形被一致地排除）。
/// - **窗口** = `SloConfig` 的 `[window_start, window_end)`，与基尼/无戏份/矛盾率同一把尺。
///   申诉按 `created_at` 落窗，世界按「窗口内是否演过戏」落窗。
/// - **辅助数**（不进 `value`，但进响应）：原始申诉条数、按类别（ooc / unfair_ruling）分布、
///   按复核状态分布、**坐实率**（confirmed / 已复核）与补偿发放量。
///   🔴 申诉率与坐实率必须分开看：前者是「多少人不满」（T1 门槛盯的就是它），
///   后者是「其中多少确实是模型的错」。把两者混成一个数会同时丢掉两个信号。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 三种「没有数」的状态必须分得开
/// ════════════════════════════════════════════════════════════════════════════
///
/// | 情形 | status | value | 后台显示 |
/// |---|---|---|---|
/// | 申诉入口从未对任何人开放（开关默认关闭） | `entry_not_open` | `null` | `—` |
/// | 入口开着，但窗口内没有任何世界演过戏 | `no_data_in_window` | `null` | `—` |
/// | 入口开着、演过戏、**没人申诉** | `ok` | `0.0` | `0%` |
///
/// 第一行是本指标特有的坑，也是它最容易骗人的地方：本功能**默认关闭**，此时窗口内一条申诉
/// 都不会有。若直接报 `0%`，运营看板上会出现「OOC 申诉率 0%」——一个看起来棒极了、
/// 实际上什么都没测的数，而 T1 恰恰要拿这个数决定「继续 / 调整 / 停止」。
/// 「入口没开」与「没人申诉」是完全不同的两件事，绝不能长成同一个样子。
/// 判定见 `annotations::entry_ever_open`（fail-safe 方向是「没开过」→ 报 `—` 而不是 `0%`）。
async fn ooc_appeal_block(db: &AnyPool, cfg: &SloConfig) -> Result<Value, ApiError> {
    // 🔴 先判入口是否开过：入口没开时后面的 0 全都没有意义。
    if !crate::annotations::entry_ever_open(db).await {
        return Ok(json!({
            "metric": "oocAppealRate",
            "title": "OOC 申诉率",
            "status": "entry_not_open",
            "value": Value::Null,
            "notes": [
                "OOC 申诉入口（运行时开关 MUSE_OOC_ANNOTATIONS）从未对任何人开放，窗口内不可能有申诉。",
                "🔴 这是「没测过」不是「没人申诉」：后台必须显示 —，显示 0% 即为误报（T1 门槛会据此误判为通过）。",
                "开放方式：运营后台 POST /admin/flags 写一条 enabled=1 的记录（global / world / user 三档任选）。",
            ],
        }));
    }

    // 分母：窗口内真正演过戏的世界里的「角色 × 世界(阶段)」对。
    let sql = format!(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM world_members wm \
         WHERE EXISTS ( \
             SELECT 1 FROM world_ticks wt WHERE wt.world_id = wm.world_id \
               AND {TICK_DOMAIN} \
               AND wt.created_at >= $1 AND wt.created_at < $2 \
         )"
    );
    let member_stages: i64 = sqlx::query(&sql)
    .bind(cfg.window_start)
    .bind(cfg.window_end)
    .fetch_one(db)
    .await?
    .try_get("n")?;

    // 分子：窗口内申诉去重到 (world_id, character_id)，并施加与分母同样的两个 EXISTS。
    let sql = format!(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM ( \
             SELECT a.world_id, a.character_id FROM ooc_appeals a \
             WHERE a.created_at >= $1 AND a.created_at < $2 \
               AND EXISTS ( \
                   SELECT 1 FROM world_members wm WHERE wm.world_id = a.world_id \
                     AND wm.cloud_character_id = a.character_id \
               ) \
               AND EXISTS ( \
                   SELECT 1 FROM world_ticks wt WHERE wt.world_id = a.world_id \
                     AND {TICK_DOMAIN} \
                     AND wt.created_at >= $3 AND wt.created_at < $4 \
               ) \
             GROUP BY a.world_id, a.character_id \
         ) t"
    );
    let pairs_appealed: i64 = sqlx::query(&sql)
    .bind(cfg.window_start)
    .bind(cfg.window_end)
    .bind(cfg.window_start)
    .bind(cfg.window_end)
    .fetch_one(db)
    .await?
    .try_get("n")?;

    // 辅助分布：按复核状态 / 按异议类别（一次 GROUP BY 各一条，无 N+1）。
    let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
    let mut appeals_total: i64 = 0;
    for r in sqlx::query(
        "SELECT status, CAST(COUNT(*) AS BIGINT) AS n FROM ooc_appeals \
         WHERE created_at >= $1 AND created_at < $2 GROUP BY status",
    )
    .bind(cfg.window_start)
    .bind(cfg.window_end)
    .fetch_all(db)
    .await?
    {
        let k: String = r.try_get("status")?;
        let n: i64 = r.try_get("n")?;
        appeals_total += n;
        by_status.insert(k, n);
    }

    let mut by_reason: BTreeMap<String, i64> = BTreeMap::new();
    for r in sqlx::query(
        "SELECT reason_code, CAST(COUNT(*) AS BIGINT) AS n FROM ooc_appeals \
         WHERE created_at >= $1 AND created_at < $2 GROUP BY reason_code",
    )
    .bind(cfg.window_start)
    .bind(cfg.window_end)
    .fetch_all(db)
    .await?
    {
        by_reason.insert(r.try_get("reason_code")?, r.try_get("n")?);
    }

    // 补偿发放量（复核确认模型错误的产物；不是资产，只是「说话的机会」）。
    let comp = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS rows_n, CAST(COALESCE(SUM(grants), 0) AS BIGINT) AS grants_n \
         FROM dream_quota_compensations WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(cfg.window_start)
    .bind(cfg.window_end)
    .fetch_one(db)
    .await?;
    let comp_rows: i64 = comp.try_get("rows_n")?;
    let comp_grants: i64 = comp.try_get("grants_n")?;

    // 🔴 状态字面量取 `confirmed` 而非 `upheld`：`moderation_appeals` 里的 `upheld` 意思是
    // 「维持原判」= 申诉被驳回，与这里的「申诉成立」正好相反。同词反义是看板上最容易算反的坑。
    let confirmed = by_status.get("confirmed").copied().unwrap_or(0);
    let dismissed = by_status.get("dismissed").copied().unwrap_or(0);
    let resolved = confirmed + dismissed;
    // 坐实率：已复核里确认模型错误的占比。一条都没复核 → null（"还没人看过"不是"全都不成立"）。
    let confirmed_rate =
        if resolved > 0 { json!(confirmed as f64 / resolved as f64) } else { Value::Null };

    // 窗口内没有任何世界演过戏 → 零样本，不是「申诉率 0」。
    if member_stages == 0 {
        return Ok(json!({
            "metric": "oocAppealRate",
            "title": "OOC 申诉率",
            "status": "no_data_in_window",
            "value": Value::Null,
            "memberStagesCounted": 0,
            "appealsTotal": appeals_total,
            "notes": [
                "窗口内没有任何世界跑过计费拍 —— 分母为零样本，「没测过」不是「申诉率 0」。",
                "分母口径 = 窗口内有 done 且 cost_tokens>0 且 error IS NULL 的拍的世界 × 其 world_members 行。",
            ],
        }));
    }

    Ok(json!({
        "metric": "oocAppealRate",
        "title": "OOC 申诉率",
        "status": "ok",
        "value": pairs_appealed as f64 / member_stages as f64,
        "thresholdMax": cfg.ooc_appeal_rate_max,
        "overThreshold": (pairs_appealed as f64 / member_stages as f64) >= cfg.ooc_appeal_rate_max,
        "memberStagesCounted": member_stages,
        "charactersAppealed": pairs_appealed,
        "appealsTotal": appeals_total,
        "byStatus": Value::Object(by_status.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
        "byReasonCode": Value::Object(by_reason.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
        "confirmedRate": confirmed_rate,
        "compensationsGranted": comp_rows,
        "compensationWhispersGranted": comp_grants,
        "notes": [
            "T1 门槛「OOC/裁决不公申诉 <10%/阶段」的直接实现；阶段口径 = 一个 world 实例（同托梦配额）。",
            "分母 = 窗口内演过戏（done 且 cost_tokens>0 且 error IS NULL）的世界 × 其 world_members 行（NPC 不入该表，无需取交集）。",
            "分子 = 窗口内申诉按 (worldId, characterId) 去重后的对数 —— 同一角色对多拍申诉算一次「这个角色不满意」。",
            "分子施加与分母相同的两个 EXISTS 过滤，故 分子 ≤ 分母 恒成立，不会出现 >100% 的申诉率。",
            "🔴 申诉率 ≠ 坐实率：value 是「多少人不满」（T1 门槛盯的），confirmedRate 是「其中多少确实是模型的错」。",
            "🔴 申诉不改写任何世界线数据；confirmed 的含义是「承认这一拍演砸了」，不是「这一拍没发生过」（§0.3）。",
        ],
    }))
}

// ============================================================================
// §5 🔴 不可算的项：显式标注「无数据源」，绝不显示 0 或空
// ============================================================================

/// 目前**没有数据源**的 SLO：`(metric, title, 为什么算不了, 补齐它需要什么)`。
///
/// 🔴 它们与"值为 0"是两回事：0 意味着"测了、没发生"，而这里是"根本没测"。后台必须显示 `—`。
/// 每次有人补上其中一项的数据源，就把它从本表挪进 §4，并同步改 VALIDATION §4.2 那张可得性表。
///
/// 历次转正：`stateTextContradictionRate`（2026-07-26，迁移 0030 CriticReport 落库）·
/// `oocAppealRate`（2026-07-26，迁移 0037 OOC 注解权）。**清单缩短是数据源到位的预期结果**，
/// 不是断言被放宽——转正后各自有专门的红线用例守「不许悄悄退回标注」。
const UNAVAILABLE_METRICS: &[(&str, &str, &str, &str)] = &[
    (
        "plotRepetitionRate",
        "剧情重复率",
        "prose 从未落库：正文只存在于引擎 FS 的 scene 文件；事件投影的 event_summary 只找 fact 的 \
         summary/narrative/text 三键，而 ActionResolved 的 fact 是 {result,action,consequence}、\
         DialogueSpoken 是 {purpose}，全部落到兜底分支，拿不到可比对的文本。",
        "fact.consequence/purpose/action 已于 2026-07-26 进入事件投影（迁移无关，纯代码），\
         剩余阻塞项是**相似度口径拍板**（用什么算法、多相似算重复）。",
    ),
];

/// 渲染三项无数据源指标。`value` 恒为 `null`、`status` 恒为 `no_data_source`。
fn unavailable_blocks() -> BTreeMap<String, Value> {
    UNAVAILABLE_METRICS
        .iter()
        .map(|(metric, title, why, needs)| {
            (
                (*metric).to_string(),
                json!({
                    "metric": metric,
                    "title": title,
                    "status": "no_data_source",
                    "value": Value::Null,
                    "reason": why,
                    "blockedBy": needs,
                }),
            )
        })
        .collect()
}

// ============================================================================
// §6 对外入口
// ============================================================================

/// 叙事质量 SLO 总装。**纯只读**：七个指标（六可算 + 一项显式标注无数据源）一次返回。
///
/// 查询预算：基尼 1 条 + 无戏份 3 条 + 强制收尾 3 条（含 2 条标量）+ 二次入世 3 条 +
/// 矛盾率 1 条 + OOC 申诉率 5 条（+ 入口开关判定 ≤2 条）= **约 18 条**，
/// 与世界数/成员数无关（不存在按世界逐个发 SQL 的 N+1）。逐行扫描的两项带 `LIMIT cap+1` 溢出探测。
pub(crate) async fn narrative_slo(db: &AnyPool, cfg: &SloConfig) -> Result<Value, ApiError> {
    let mut metrics: BTreeMap<String, Value> = BTreeMap::new();
    metrics.insert("attentionGini".into(), attention_gini_block(db, cfg).await?);
    metrics.insert("silentStreak".into(), silent_streak_block(db, cfg).await?);
    metrics.insert("forcedConclusionRate".into(), forced_conclusion_block(db).await?);
    metrics.insert("repeatEntryRate".into(), repeat_entry_block(db).await?);
    // 2026-07-26：CriticReport 落库（迁移 0030）后本项从「无数据源」转为可算。
    metrics.insert("stateTextContradictionRate".into(), contradiction_block(db, cfg).await?);
    // 2026-07-26：OOC 注解权落地（迁移 0037）后本项从「无数据源」转为可算——
    // 它是 §4.2 那张表里最后一个「唯一未解」，也是 T1 门槛唯一的测量手段。
    metrics.insert("oocAppealRate".into(), ooc_appeal_block(db, cfg).await?);
    metrics.extend(unavailable_blocks());

    let unavailable: Vec<&str> = UNAVAILABLE_METRICS.iter().map(|(m, ..)| *m).collect();

    Ok(json!({
        "status": "ok",
        "windowDays": cfg.days,
        "windowStartMs": cfg.window_start,
        "windowEndMs": cfg.window_end,
        "thresholds": {
            "attentionGiniMax": cfg.gini_max,
            "silentStreakTicks": cfg.silent_streak_max,
            "scanRowCap": cfg.scan_row_cap,
            "oocAppealRateMax": cfg.ooc_appeal_rate_max,
        },
        "metrics": Value::Object(metrics.into_iter().collect()),
        "unavailable": unavailable,
        "notes": [
            "本段全部为只读聚合，不写库、不回灌引擎（回灌会违反「数值不进引擎决策」红线）。",
            "🔴 unavailable 里的项是「没有数据源」不是「值为 0」：后台必须显示 —，显示 0% 即为误报。",
            "窗口只作用于基尼与无戏份两项（滚动观测）；强制收尾率与二次入世率是全生命周期口径，切窗口会失真。",
            "基尼须与 world_members 取交集：NPC（世界固有角色）也入 world_contributions，不取交集会污染玩家公平度。",
            "有效戏份口径 = event_type ∈ {action, dialogue}；consent_request 等流程事件不算戏份（被同意门拦下的那一拍恰恰什么也没演成）。",
            "actors_json 走规范化 JSON 解析而非 LIKE 子串匹配（LIKE 会让 li 被 lixia 蹭到戏份）。",
            "日界为 UTC，由 dashboards::utc_day_start_ms + DAY_MS 在 Rust 侧算成毫秒区间传入；SQL 不含任何方言日期函数。",
            "门槛与扫描上限可配（MUSE_SLO_GINI_MAX / MUSE_SLO_SILENT_STREAK_TICKS / MUSE_SLO_SCAN_ROW_CAP / MUSE_SLO_OOC_APPEAL_RATE_MAX），VALIDATION §0.2 参数化。",
            "🔴 oocAppealRate 有三态：entry_not_open（入口没开过，—）/ no_data_in_window（零样本，—）/ ok（真数，可以是 0%）。三者不可混同。",
            "扫描行数超过 scanRowCap 的指标返回 skipped_too_large 而不是残缺数——保护被轮询的后台端点。",
        ],
    }))
}

/// `?slo=0` 时的占位：**明说是被调用方跳过的**，与"无数据源""数据量超限"三态互不混淆。
pub(crate) fn skipped_by_request(days: i64) -> Value {
    json!({
        "status": "skipped_by_request",
        "windowDays": days,
        "metrics": {},
        "unavailable": UNAVAILABLE_METRICS.iter().map(|(m, ..)| *m).collect::<Vec<_>>(),
        "notes": ["调用方传了 ?slo=0，本次未计算叙事质量 SLO（高频轮询减负开关）。去掉该参数即恢复。"],
    })
}

#[cfg(test)]
mod tests;
