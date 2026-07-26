//! 数据看板（总览 + 按天趋势）+ 经济运营。均为只读 SQL 聚合/取数，不产生副作用、不建结算。
//!
//! `/admin/metrics/overview` 含三段：核心运营计数 · 成本仪表（§17【拍板 16】）·
//! **叙事质量 SLO**（`narrativeSlo`，VALIDATION §4.2，口径与聚合在 `crate::slo`）。

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::db::now_ms;
use crate::error::ApiError;

use super::require_role;

/// 单标量计数辅助（列名一律别名 n）。
async fn count(db: &AnyPool, sql: &str) -> Result<i64, ApiError> {
    let row = sqlx::query(sql).fetch_one(db).await?;
    Ok(row.try_get::<i64, _>("n")?)
}

fn rate(numer: i64, denom: i64) -> f64 {
    if denom <= 0 {
        0.0
    } else {
        (numer as f64) / (denom as f64)
    }
}

/// `rate` 的诚实版：分母为 0 → `None`（渲染成 `—`），而不是编一个 0.0。
///
/// 用在错峰仪表上：「窗口内一拍都没有」与「窗口内跑了 100 拍、一拍都没走错峰」是两件事，
/// 前者是**没有数据**、后者是**真实的 0%**。前端据此区分空态与真值（同 `/admin/worlds`
/// 的 `successRate`：无已终结 tick → null，不得当 0% 读）。
fn ratio_or_null(numer: i64, denom: i64) -> Option<f64> {
    (denom > 0).then(|| (numer as f64) / (denom as f64))
}

// ---------------- 成本仪表口径（总规格 §17【拍板 16】） ----------------

/// token→人民币单价默认值（分 / 千 token）。
///
/// **与 runtime 侧同源**：`runtime::token_cny_cents_per_1k` 用同一个 env 名、同一个默认值、
/// 同一个换算公式。runtime 侧那个函数是模块私有的（服务于逐拍预算熔断），本模块不便跨模块调用，
/// 故在此复刻**同一口径**而不是另立一套单价——看板金额与熔断金额必须是同一个数，
/// 否则会出现「后台看着没超预算、世界却被熔断」。
/// 调价只需改 env `MUSE_TOKEN_CNY_CENTS_PER_1K`，两侧同时生效（VALIDATION §0.2 参数化，禁写死）。
const DEFAULT_TOKEN_CNY_CENTS_PER_1K: i64 = 2;

/// 读取 token 单价（分/千 token）。env 覆盖 + 默认常量，范式同 `runtime::token_cny_cents_per_1k`。
pub(super) fn token_cny_cents_per_1k() -> i64 {
    std::env::var("MUSE_TOKEN_CNY_CENTS_PER_1K")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &i64| *v > 0)
        .unwrap_or(DEFAULT_TOKEN_CNY_CENTS_PER_1K)
}

/// tokens → 人民币分。公式与 runtime 预算熔断里的 `est_cny` 逐字一致（整数除法、向下取整）。
pub(super) fn tokens_to_cents(tokens: i64) -> i64 {
    tokens.saturating_mul(token_cny_cents_per_1k()) / 1000
}

/// 分 → 元（纯展示派生值）。账面口径一律整数分；元只为前端直接渲染省一次换算。
pub(super) fn cents_to_cny(cents: i64) -> f64 {
    cents as f64 / 100.0
}

/// 每玩家成本分摊：**按该世界 active 成员人均等分**（世界累计成本 ÷ active 成员数）。
///
/// 为什么选等分而不是「按参与拍数」：
/// ① `world_ticks.cost_tokens` 是**整拍**口径——一次 tick 的导演/仲裁/写作调用产出的是
///    全世界共享的一段叙事，账上根本没有 per-member 的 token 分解，「按参与拍数分摊」
///    在当前数据下无法计算（world_ticks 不记录本拍参与了哪些成员）；
/// ② 退而用 `world_events.actors_json` 近似「谁参与了」是**产出侧戏份**而非**成本侧投入**，
///    会把「戏多」错当成「费钱」，比等分更失真且更难解释；
/// ③ 等分正面回答定价要问的那个问题——「这个世界再多进一个玩家，摊薄后每人多少钱」。
/// 局限（须随数一起看）：等分假设玩家对成本贡献均等；戏份极不均的世界会低估重度玩家的真实成本。
///
/// 成员数为 0（无人在场/全员退出）→ None：无成员的世界没有「每玩家成本」这个量，不除零、不编 0。
fn per_player(value: i64, members: i64) -> Option<f64> {
    if members > 0 {
        Some(value as f64 / members as f64)
    } else {
        None
    }
}

// ---------------- 错峰调度可观测面（迁移 0038；总规格 §17【拍板 16】成本杠杆①） ----------------

/// 🔴 **单位陷阱**：`world_ticks.price_ratio_pct` 是**百分数整数**（100=原价、50=5 折、40=4 折），
/// 而本接口对外的一切**比率**字段一律是 **0..1 小数**（与 `successRate` / `usageRatio` / `openRate`
/// 同一套约定，见 docs/API.md）。两者必须显式换算，绝不能把 `50` 直接当成比率下发——
/// 那会让前端把「5 折」渲染成「5000%」。
///
/// 故对外同时给两个字段、各自守住各自的口径：
/// - `priceRatioPct`：原列值，百分数整数，**运营配置的名义档位**；
/// - `priceRatio`：`pct/100`，0..1 小数，供前端按统一比率通道渲染。
fn price_ratio(pct: i64) -> f64 {
    pct as f64 / 100.0
}

/// 按名义档位估算「相对原价省下多少」（分）：`原价分 × (100 - pct) / 100`。
///
/// 口径与 docs/API.md「错峰调度」小节的公式逐字一致：
/// `Σ cost_tokens × (100-price_ratio_pct)/100 × MUSE_TOKEN_CNY_CENTS_PER_1K`。
/// 🔴 它是**估算折让**，不是供应商账单——`price_ratio_pct` 只是运营配置的名义档位（迁移 0038 口径声明），
/// 不得当对账依据。
///
/// 先按档位把 token 汇总再换算（而不是逐拍取整再相加），避免逐拍地板误差累积成一个肉眼可见的低估。
/// `pct` 越界（脏数据）时钳到 [0,100]：折让不会为负，也不会超过原价。
fn saved_cents_at(tokens: i64, pct: i64) -> i64 {
    let discount = 100 - pct.clamp(0, 100);
    tokens_to_cents(tokens).saturating_mul(discount) / 100
}

/// 错峰仪表的窗口内累加器（与成本趋势共用**同一条**窗口查询，不额外发 SQL）。
#[derive(Default)]
struct OffPeakAgg {
    /// 窗口内全部拍数（错峰与否都算，是各类占比的分母）。
    ticks: i64,
    /// 窗口内全部 token（应恒等于 `Σ trend[].tokens`）。
    tokens: i64,
    /// `off_peak = 1` 的拍数 / token（「错峰生效了多少」的直接度量）。
    off_peak_ticks: i64,
    off_peak_tokens: i64,
    /// `defer_ms > 0` 的拍数与被压总时长/峰值（延后账是进程内存态，重启会低估，见迁移 0038）。
    deferred_ticks: i64,
    defer_ms_total: i64,
    defer_ms_max: i64,
    /// 名义档位 → (拍数, token)。BTreeMap 保证按档位升序（折扣最深的在前、原价 100 在末），
    /// 输出顺序确定，前端不必再排。
    by_pct: BTreeMap<i64, (i64, i64)>,
}

impl OffPeakAgg {
    fn push(&mut self, tokens: i64, off_peak: bool, pct: i64, defer_ms: i64) {
        self.ticks += 1;
        self.tokens += tokens;
        if off_peak {
            self.off_peak_ticks += 1;
            self.off_peak_tokens += tokens;
        }
        if defer_ms > 0 {
            self.deferred_ticks += 1;
            self.defer_ms_total += defer_ms;
            self.defer_ms_max = self.defer_ms_max.max(defer_ms);
        }
        let slot = self.by_pct.entry(pct).or_insert((0, 0));
        slot.0 += 1;
        slot.1 += tokens;
    }

    /// 组装 `cost.offPeak`。窗口与成本趋势同一个（`?costDays=`），故不另立一套时间口径。
    fn to_json(&self, window_days: i64) -> Value {
        let nominal_cents = tokens_to_cents(self.tokens);
        let mut saved_cents = 0i64;
        let mut by_ratio = Vec::with_capacity(self.by_pct.len());
        for (&pct, &(ticks, tokens)) in &self.by_pct {
            let saved = saved_cents_at(tokens, pct);
            saved_cents += saved;
            by_ratio.push(json!({
                "priceRatioPct": pct,              // 百分数整数（原列值，100=原价）
                "priceRatio": price_ratio(pct),    // 0..1 小数（同 successRate 约定）
                "ticks": ticks,
                "tokens": tokens,
                "savedCents": saved,
                "savedCny": cents_to_cny(saved),
            }));
        }
        // 逐档地板求和恒 ≤ 整体地板，故折让不会超过原价，effective 不会为负。
        let effective_cents = (nominal_cents - saved_cents).max(0);
        json!({
            "windowDays": window_days,
            "ticks": self.ticks,
            "tokens": self.tokens,
            "offPeakTicks": self.off_peak_ticks,
            "offPeakTokens": self.off_peak_tokens,
            // 三个比率一律 **0..1 小数**；窗口内一拍都没有 → null（没有数据 ≠ 真实的 0%）。
            "tickRatio": ratio_or_null(self.off_peak_ticks, self.ticks),
            "tokenRatio": ratio_or_null(self.off_peak_tokens, self.tokens),
            "savedRatio": ratio_or_null(saved_cents, nominal_cents),
            "nominalCents": nominal_cents,
            "nominalCny": cents_to_cny(nominal_cents),
            "savedCents": saved_cents,
            "savedCny": cents_to_cny(saved_cents),
            "effectiveCents": effective_cents,
            "effectiveCny": cents_to_cny(effective_cents),
            "deferredTicks": self.deferred_ticks,
            "deferMsTotal": self.defer_ms_total,
            "deferMsMax": self.defer_ms_max,
            // 平均延后时长的分母是**被延后过的拍**（不是全部拍）；一拍都没被延后 → null。
            "avgDeferMs": (self.deferred_ticks > 0)
                .then(|| self.defer_ms_total as f64 / self.deferred_ticks as f64),
            "byRatio": by_ratio,
            "notes": [
                "窗口与 cost.trend 同一个（?costDays=，默认 7、clamp [1,60]、UTC 日界、末桶即今天）。",
                "🔴 priceRatioPct 是百分数整数（100=原价、50=5 折），priceRatio 是同一个数的 0..1 形态；tickRatio/tokenRatio/savedRatio 一律 0..1 小数。",
                "savedCents = Σ 按档位汇总的 tokens × (100-priceRatioPct)/100 × MUSE_TOKEN_CNY_CENTS_PER_1K，是相对原价的**估算折让**，不是供应商账单，不得当对账依据（迁移 0038 口径声明）。",
                "cost.today / cost.trend / cost.total 一律按**原价**计（口径不变），错峰折让只在本节体现，两者不重复相减。",
                "错峰调度默认关闭（MUSE_OFFPEAK_SCHEDULING，VALIDATION §0.1），关闭时每拍落中性值 off_peak=0 / price_ratio_pct=100 / defer_ms=0，本节各项恒为 0/空——是「未开启」，不是「没省下钱」。",
                "本节不下发开关状态：错峰是**按世界**生效的（登记进 flags::KNOWN_FLAGS 后走 world 作用域灰度，未登记时退回 env 兜底），没有一个诚实的平台级布尔可报；登记后可在 GET /admin/flags 查每档解析结果。",
                "deferMs 由进程内存态计时，server 重启会让在途延后账清零（方向是低估错峰效果，不会虚报）。",
                "avgDeferMs 的分母只含被延后过的拍（deferredTicks），无被延后拍时为 null。",
            ],
        })
    }
}

/// 成本榜单长度（每局成本卡片只展示头部世界；平台合计仍按全量世界算）。
const COST_TOP_N: usize = 10;

/// 成本趋势默认窗口（天）。看板健康条要的是「最近一周走势」；可经 `?costDays=` 覆盖。
const DEFAULT_COST_TREND_DAYS: i64 = 7;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OverviewQuery {
    /// 成本趋势窗口天数，clamp 到 [1,60]（与 trends 同），默认 7。
    cost_days: Option<i64>,
    /// 叙事质量 SLO 观测窗口天数，clamp 到 [1,365]，默认 `slo::DEFAULT_SLO_WINDOW_DAYS`（30）。
    slo_days: Option<i64>,
    /// `?slo=0` 跳过叙事质量 SLO 段（高频轮询减负开关）。缺省即计算。
    /// 用整数而不是 bool：query string 里 `slo=0` 比 `slo=false` 更顺手，serde 的 bool 只认 true/false。
    slo: Option<i64>,
}

/// GET /admin/metrics/overview：核心运营指标聚合。
/// 注册数、日报打开率、tick 成功率、按世界 token 成本、审核积压、活跃/熔断世界、风控计数，
/// 以及**成本仪表**（§17【拍板 16】）：今日成本 + 近 N 日趋势 + 每局成本 + 每玩家分摊
/// + **错峰调度**（`cost.offPeak`，迁移 0038 三列：占比 / 估算折让 / 延后时长）。
pub(super) async fn metrics_overview(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<OverviewQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator", "finance"])?;
    let db = &state.db;

    // 用户：注册总数 / 封禁数。
    let users_total = count(db, "SELECT COUNT(*) AS n FROM users").await?;
    let users_banned =
        count(db, "SELECT COUNT(*) AS n FROM users WHERE status = 'banned'").await?;

    // 日报送达/打开率。
    let dr = sqlx::query(
        "SELECT COUNT(*) AS total, \
         COALESCE(SUM(CASE WHEN opened_at IS NOT NULL THEN 1 ELSE 0 END), 0) AS opened \
         FROM daily_reports",
    )
    .fetch_one(db)
    .await?;
    let reports_total: i64 = dr.try_get("total")?;
    let reports_opened: i64 = dr.try_get("opened")?;

    // tick 成功率。
    let tk = sqlx::query(
        "SELECT COUNT(*) AS total, \
         COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) AS done, \
         COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS failed \
         FROM world_ticks",
    )
    .fetch_one(db)
    .await?;
    let ticks_total: i64 = tk.try_get("total")?;
    let ticks_done: i64 = tk.try_get("done")?;
    let ticks_failed: i64 = tk.try_get("failed")?;

    // ==================== 成本仪表（§17【拍板 16】「成本仪表」） ====================
    // 三个维度共三条只读聚合查询，**无 N+1**（绝不按世界逐个发 SQL——本端点是后台轮询调用的）：
    //   ① 每局成本：world_ticks 按 world_id 一次 GROUP BY 累计 token；
    //   ② 每玩家成本：world_members 按 world_id 一次 GROUP BY 数 active 成员，Rust 侧等分分摊；
    //   ③ 今日成本 + 近 N 日趋势：一条窗口范围查询取回 (cost_tokens, created_at)，Rust 侧分桶到天。
    // 日界：沿用本文件 `utc_day_start_ms`/`DAY_MS`（UTC 日界，与 metrics_trends 同一套口径），
    // SQL 只做 BIGINT 毫秒范围过滤，**不用 strftime/date_trunc**（方言特有、SQLite/PG 不可移植）。
    let cents_per_1k = token_cny_cents_per_1k();

    // ① 每局（每个世界实例）累计 token。去掉了历史上的 `LIMIT 10`：平台合计与每玩家均值必须覆盖
    //    全部世界才有定价意义；GROUP BY 本就要扫全表，多回传几行世界级聚合的代价可忽略，
    //    榜单仍只取头部 COST_TOP_N 条。
    // 🔴 次级键 `world_id` 不可省：这是按 `SUM()` 聚合值排序，**零成本世界结构性全部并列**
    //    （tokens 都是 0），而 Rust 侧随后切 `COST_TOP_N` 条 ⇒ 单键下榜单的**成员**都是任意的。
    //    `world_id` 是 GROUP BY 键 ⇒ 每行唯一，补上即得全序。
    let cost_rows = sqlx::query(
        "SELECT world_id, CAST(COALESCE(SUM(cost_tokens), 0) AS BIGINT) AS tokens FROM world_ticks \
         GROUP BY world_id ORDER BY tokens DESC, world_id ASC",
    )
    .fetch_all(db)
    .await?;

    // ② 各世界 active 成员数（分摊分母）。
    let member_rows = sqlx::query(
        "SELECT world_id, COUNT(*) AS n FROM world_members WHERE status = 'active' GROUP BY world_id",
    )
    .fetch_all(db)
    .await?;
    let mut active_members: HashMap<String, i64> = HashMap::with_capacity(member_rows.len());
    for r in &member_rows {
        active_members.insert(r.try_get::<String, _>("world_id")?, r.try_get::<i64, _>("n")?);
    }

    // ①+② 组装：榜单取头部；合计覆盖全量世界。
    // 合计的成员分母只累加**有 tick 记账的世界**——从未跑过的世界成本恒为 0，把它的成员计进分母
    // 会把平台均值稀释成一个没人能解释的数。
    let mut token_cost_by_world = Vec::new();
    let mut cost_by_world = Vec::new();
    let mut total_tokens = 0i64;
    let mut total_members = 0i64;
    for r in &cost_rows {
        let world_id: String = r.try_get("world_id")?;
        let tokens: i64 = r.try_get("tokens")?;
        let members = active_members.get(&world_id).copied().unwrap_or(0);
        total_tokens += tokens;
        total_members += members;
        if cost_by_world.len() < COST_TOP_N {
            let cents = tokens_to_cents(tokens);
            // 兼容既有看板字段（旧 Metrics 页读的就是它），值与 cost.byWorld 同源同序。
            token_cost_by_world.push(json!({ "worldId": world_id.clone(), "tokens": tokens }));
            cost_by_world.push(json!({
                "worldId": world_id,
                "tokens": tokens,
                "cents": cents,
                "cny": cents_to_cny(cents),
                "activeMembers": members,
                "tokensPerPlayer": per_player(tokens, members),
                "centsPerPlayer": per_player(cents, members),
            }));
        }
    }
    let total_cents = tokens_to_cents(total_tokens);

    // ③ 今日成本 + 近 N 日趋势（UTC 日界，空天补零，末桶即"今天"）。
    let cost_days = q.cost_days.unwrap_or(DEFAULT_COST_TREND_DAYS).clamp(1, 60);
    let today_start = utc_day_start_ms(now_ms());
    let cost_start = today_start - (cost_days - 1) * DAY_MS;
    let cost_end = today_start + DAY_MS; // 右开区间，含今天整天
    let mut day_tokens = vec![0i64; cost_days as usize];
    // 同一条窗口查询顺带喂错峰仪表（迁移 0038 三列）：**不新开查询、不新开端点**——
    // 本端点是后台轮询调用的，"数据在写没人看"要补的是读出口，不是再加一次全表扫描。
    // 三列均为 INTEGER/BIGINT，`CAST(... AS BIGINT)` 保证 SQLite/Postgres 解码一致
    //（同 metrics_trends 对 gift_count 的处理）；别名与原列名错开，避免 PG 下的名字解析歧义。
    let mut day_off_peak_tokens = vec![0i64; cost_days as usize];
    let mut off_peak = OffPeakAgg::default();
    let rows = sqlx::query(
        "SELECT cost_tokens, created_at, \
         CAST(off_peak AS BIGINT) AS off_peak_flag, \
         CAST(price_ratio_pct AS BIGINT) AS price_pct, \
         CAST(defer_ms AS BIGINT) AS defer_millis \
         FROM world_ticks WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(cost_start)
    .bind(cost_end)
    .fetch_all(db)
    .await?;
    for r in &rows {
        let ts: i64 = r.try_get("created_at")?;
        // 窗口起点即 UTC 0 点，(ts-start)/天宽 整除即天序号；SQL 已过滤，越界防御性跳过。
        if ts >= cost_start && ts < cost_end {
            let tokens: i64 = r.try_get("cost_tokens")?;
            // 布尔按仓库约定存 INTEGER 0/1（db.rs 头注释），非 0 即真。
            let is_off_peak = r.try_get::<i64, _>("off_peak_flag")? != 0;
            let idx = ((ts - cost_start) / DAY_MS) as usize;
            day_tokens[idx] += tokens;
            if is_off_peak {
                day_off_peak_tokens[idx] += tokens;
            }
            off_peak.push(
                tokens,
                is_off_peak,
                r.try_get::<i64, _>("price_pct")?,
                r.try_get::<i64, _>("defer_millis")?,
            );
        }
    }
    let mut cost_trend = Vec::with_capacity(day_tokens.len());
    for (i, tokens) in day_tokens.iter().enumerate() {
        let cents = tokens_to_cents(*tokens);
        cost_trend.push(json!({
            "day": crate::runtime::day_string(cost_start + i as i64 * DAY_MS),
            "tokens": tokens,
            "cents": cents,
            "cny": cents_to_cny(cents),
            // 当日 token 里走了折扣时段的那部分（`tokens` 仍是当日全量，两者是**包含**关系，不可相加）。
            // 错峰关闭 / 无错峰拍时恒为 0，既有折线逐字不受影响。
            "offPeakTokens": day_off_peak_tokens[i],
        }));
    }
    let today_tokens = *day_tokens.last().unwrap_or(&0);
    let today_cents = tokens_to_cents(today_tokens);

    // ============ 叙事质量 SLO（VALIDATION §4.2 验证基建三件套第二件） ============
    // **为什么挂在 overview 的新顶层键、而不是新开 /admin/metrics/slo**：
    // ① 运营看板本来就一次拉 overview——"这个平台现在健康吗"里，叙事质量与成本仪表是同一屏的
    //    两半（成本回答"贵不贵"，SLO 回答"演得好不好"），拆两个端点只是让前端多轮询一次；
    // ② RBAC / 日界 / CAST 口径与本文件已有的成本仪表逐条一致，复用而非另立一套；
    // ③ 新增端点要改 `admin_api/mod.rs` 的路由表，那是并发改动面，本次刻意不碰。
    // 代价是 overview 变重 → 用三道闸控住（见 slo 模块头注释）：滚动窗口（默认 30 天）、
    // 每指标一次 GROUP BY / 范围扫描（无 N+1）、逐行扫描项带 `LIMIT cap+1` 溢出探测超限即跳过。
    // 另留 `?slo=0` 给高频轮询彻底减负。真要拆出独立端点时，`slo::narrative_slo` 已是自洽入口，
    // 加一条 route + 一个薄 handler 即可，本段可原样搬走。
    let slo_days = q.slo_days.unwrap_or(crate::slo::DEFAULT_SLO_WINDOW_DAYS).clamp(1, 365);
    let narrative_slo = if q.slo == Some(0) {
        crate::slo::skipped_by_request(slo_days)
    } else {
        // 日界口径复用本文件的 `utc_day_start_ms` + `DAY_MS`（UTC 日界，与成本仪表/trends 同源）；
        // SQL 侧只收 BIGINT 毫秒区间，不出现任何 strftime/date_trunc 方言函数。
        let slo_cfg = crate::slo::SloConfig::from_env(
            slo_days,
            today_start - (slo_days - 1) * DAY_MS,
            today_start + DAY_MS, // 右开区间，含今天整天
        );
        crate::slo::narrative_slo(db, &slo_cfg).await?
    };

    // 审核积压 / 活跃世界 / 熔断世界 / 风控事件。
    let audit_backlog =
        count(db, "SELECT COUNT(*) AS n FROM audit_queue WHERE status = 'open'").await?;
    let worlds_active =
        count(db, "SELECT COUNT(*) AS n FROM worlds WHERE status IN ('open','running')").await?;
    let worlds_fused =
        count(db, "SELECT COUNT(*) AS n FROM world_budgets WHERE fused = 1").await?;
    let risk_total = count(db, "SELECT COUNT(*) AS n FROM risk_events").await?;
    let data_requests_pending = count(
        db,
        "SELECT COUNT(*) AS n FROM data_requests WHERE status IN ('pending','running')",
    )
    .await?;

    Ok(Json(json!({
        "users": { "total": users_total, "banned": users_banned },
        "dailyReports": {
            "total": reports_total,
            "opened": reports_opened,
            "openRate": rate(reports_opened, reports_total),
        },
        "ticks": {
            "total": ticks_total,
            "done": ticks_done,
            "failed": ticks_failed,
            "successRate": rate(ticks_done, ticks_total),
        },
        "tokenCostByWorld": token_cost_by_world,
        // 成本仪表（§17【拍板 16】）。金额一律**整数分**（cents）为账面口径，cny 仅为展示派生；
        // perPlayer 系列是派生均值（浮点），不是账面金额。
        "cost": {
            "centsPer1kTokens": cents_per_1k,
            "today": {
                "day": crate::runtime::day_string(today_start),
                "tokens": today_tokens,
                "cents": today_cents,
                "cny": cents_to_cny(today_cents),
            },
            "trendDays": cost_days,
            "trend": cost_trend,
            "byWorld": cost_by_world,
            // 错峰调度可观测面（迁移 0038 三列的唯一读出口）。窗口同 trend；口径与局限见其 notes[]。
            "offPeak": off_peak.to_json(cost_days),
            "total": {
                "worlds": cost_rows.len() as i64,
                "tokens": total_tokens,
                "cents": total_cents,
                "cny": cents_to_cny(total_cents),
                "activeMembers": total_members,
                "tokensPerPlayer": per_player(total_tokens, total_members),
                "centsPerPlayer": per_player(total_cents, total_members),
            },
            "allocation": "per_member_equal_split",
            "notes": [
                "单价口径：MUSE_TOKEN_CNY_CENTS_PER_1K（分/千 token），与 runtime 预算熔断同一个参数与公式。",
                "每玩家成本 = 世界累计成本 ÷ 该世界 world_members 中 status='active' 的成员数（人均等分）。",
                "选等分的理由：cost_tokens 是整拍口径，账上没有 per-member 分解，「按参与拍数分摊」当前无数据可算；等分正面回答定价问题（多进一人摊薄多少）。",
                "局限：等分假设玩家对成本贡献均等，戏份极不均的世界会低估重度玩家的真实成本。",
                "平台合计的成员分母只含有 tick 记账的世界；无 active 成员的世界 perPlayer 为 null（不除零）。",
                "日界为 UTC（与 metrics/trends、world_budgets.budget_day 同口径）；趋势末桶即今天。",
                "本对象的 today/trend/byWorld/total 一律按**原价**估算（不扣错峰折让）；错峰省了多少见 offPeak.savedCents，两处不重复相减。",
            ],
        },
        // 叙事质量 SLO（VALIDATION §4.2）。只读观测，七项：四项算得出来、
        // 🔴 三项显式标注 status="no_data_source" + value=null（后台必须显示 —，显示 0% 即误报）。
        "narrativeSlo": narrative_slo,
        "auditBacklog": audit_backlog,
        "worlds": { "active": worlds_active, "fused": worlds_fused },
        "riskEvents": risk_total,
        "dataRequestsPending": data_requests_pending,
    })))
}

// ---------------- 按天趋势 ----------------

pub(super) const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Deserialize)]
pub(super) struct TrendsQuery {
    days: Option<i64>,
}

/// ms 时间戳 → 所在天的 UTC 0 点 ms。
/// 时区口径与 `reports::day_bounds` / `runtime::day_string` 一致：**UTC 日界**，
/// 桶恒为 `[UTC 0 点, +86_400_000)`（全仓禁 SQL 日期函数，SQL 只做 BIGINT ms 范围过滤）。
pub(super) fn utc_day_start_ms(ms: i64) -> i64 {
    use chrono::NaiveTime;
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.date_naive().and_time(NaiveTime::MIN).and_utc().timestamp_millis())
        .unwrap_or(0)
}

/// GET /admin/metrics/trends?days=N：运营看板按天趋势（operator/finance，admin 直通）。
///
/// days clamp 到 [1,60]，默认 14；按天升序返回、含今天（UTC 口径），无数据的天补零。
/// 查询策略：**每张来源表一条全区间查询**取出 ms 时间戳列，Rust 侧 chrono 分桶到天——
/// 避免 N 天 × N 指标的查询风暴；窗口起点为 UTC 0 点、桶宽固定一天，整除即得天序号。
pub(super) async fn metrics_trends(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<TrendsQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator", "finance"])?;
    let db = &state.db;

    let days = q.days.unwrap_or(14).clamp(1, 60);
    let today_start = utc_day_start_ms(now_ms());
    let start = today_start - (days - 1) * DAY_MS;
    let end = today_start + DAY_MS; // 右开区间，含今天整天。

    let n = days as usize;
    let mut new_users = vec![0i64; n];
    let mut events = vec![0i64; n];
    let mut tick_tokens = vec![0i64; n];
    let mut gift_count = vec![0i64; n];
    let mut revenue_cents = vec![0i64; n];
    let mut active_worlds = vec![HashSet::<String>::new(); n];

    // 分桶：窗口起点即 UTC 0 点，(ts-start)/天宽 整除即天序号；SQL 已按 [start,end) 过滤，越界防御性跳过。
    let bucket = |ts: i64| -> Option<usize> {
        if ts < start || ts >= end {
            return None;
        }
        Some(((ts - start) / DAY_MS) as usize)
    };

    // ① 新增用户（users.created_at）。
    let rows = sqlx::query("SELECT created_at FROM users WHERE created_at >= $1 AND created_at < $2")
        .bind(start)
        .bind(end)
        .fetch_all(db)
        .await?;
    for r in &rows {
        if let Some(i) = bucket(r.try_get::<i64, _>("created_at")?) {
            new_users[i] += 1;
        }
    }

    // ② tick：一条查询同时喂两个指标——当日有 tick 的 distinct 世界数 + 当日 token 消耗。
    let rows = sqlx::query(
        "SELECT world_id, cost_tokens, created_at FROM world_ticks \
         WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;
    for r in &rows {
        if let Some(i) = bucket(r.try_get::<i64, _>("created_at")?) {
            active_worlds[i].insert(r.try_get::<String, _>("world_id")?);
            tick_tokens[i] += r.try_get::<i64, _>("cost_tokens")?;
        }
    }

    // ③ 世界事件（口径 world_events.occurred_at）。
    let rows =
        sqlx::query("SELECT occurred_at FROM world_events WHERE occurred_at >= $1 AND occurred_at < $2")
            .bind(start)
            .bind(end)
            .fetch_all(db)
            .await?;
    for r in &rows {
        if let Some(i) = bucket(r.try_get::<i64, _>("occurred_at")?) {
            events[i] += 1;
        }
    }

    // ④ 礼物量（gift_events.gift_count；INTEGER 列 CAST 成 BIGINT 保证双库解码一致）。
    let rows = sqlx::query(
        "SELECT CAST(gift_count AS BIGINT) AS gift_count, created_at FROM gift_events \
         WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;
    for r in &rows {
        if let Some(i) = bucket(r.try_get::<i64, _>("created_at")?) {
            gift_count[i] += r.try_get::<i64, _>("gift_count")?;
        }
    }

    // ⑤ 平台收入贷方净增：platform_revenue 科目当日 postings 净额（正=入账、负=冲销，净和即贷方净增）。
    //    postings.created_at 由 ledger::post_journal 以业务时间写入（与 journal 同刻），无需 JOIN journals。
    let rows = sqlx::query(
        "SELECT p.delta_cents, p.created_at FROM ledger_postings p \
         JOIN ledger_accounts a ON a.id = p.account_id \
         WHERE a.kind = 'platform_revenue' AND p.created_at >= $1 AND p.created_at < $2",
    )
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;
    for r in &rows {
        if let Some(i) = bucket(r.try_get::<i64, _>("created_at")?) {
            revenue_cents[i] += r.try_get::<i64, _>("delta_cents")?;
        }
    }

    // 组装：按天升序，空天各指标自然为 0（vec 初始化即补零）。
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(json!({
            "day": crate::runtime::day_string(start + i as i64 * DAY_MS),
            "newUsers": new_users[i],
            "activeWorlds": active_worlds[i].len() as i64,
            "events": events[i],
            "tickTokens": tick_tokens[i],
            "giftCount": gift_count[i],
            "revenueCents": revenue_cents[i],
        }));
    }
    Ok(Json(json!({ "days": out })))
}

/// GET /admin/economy/overview：真实只读经济聚合（finance/admin）。
///
/// 数据源恒存在（0001 建 orders/ledger_entries/billing_balances，0008 建 gift_events），
/// 与 billing/arena feature 无关，default 构建即可读。仅 COUNT/SUM 只读聚合——
/// **不建结算、不碰创作者分成**（§2.6：创作者结算是签约主体的另一套账，不在此）。
/// 资金以账本双录（ledger_entries）为权威：recharge 正流水、refund 负流水，
/// 恒有 `充值总额 - 退款总额 == 当前余额合计`（billing 模块单事务保证）。
/// SUM 一律 CAST(... AS BIGINT)：跨 SQLite/Postgres 可移植（PG 下 SUM(bigint) 返回 numeric）。
pub(super) async fn economy_overview(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["finance"])?;
    let db = &state.db;

    // 账本双录（资金权威）：充值正流水 / 退款负流水。
    let recharge_total =
        count(db, "SELECT CAST(COALESCE(SUM(delta_cents),0) AS BIGINT) AS n FROM ledger_entries WHERE reason = 'recharge'").await?;
    let recharge_count =
        count(db, "SELECT COUNT(*) AS n FROM ledger_entries WHERE reason = 'recharge'").await?;
    // 退款账本为负值，取相反数作为出账正数展示。
    let refund_neg =
        count(db, "SELECT CAST(COALESCE(SUM(delta_cents),0) AS BIGINT) AS n FROM ledger_entries WHERE reason = 'refund'").await?;
    let refund_count =
        count(db, "SELECT COUNT(*) AS n FROM ledger_entries WHERE reason = 'refund'").await?;
    let refund_total = -refund_neg;

    // 当前用户钱包余额合计（非创作者结算）。
    let balance_total =
        count(db, "SELECT CAST(COALESCE(SUM(balance_cents),0) AS BIGINT) AS n FROM billing_balances").await?;
    let wallet_count = count(db, "SELECT COUNT(*) AS n FROM billing_balances").await?;

    // 订单按状态计数（created/paid/fulfilled/refunded/failed）。
    let order_rows =
        sqlx::query("SELECT status, COUNT(*) AS n FROM orders GROUP BY status").fetch_all(db).await?;
    let mut by_status = serde_json::Map::new();
    for s in ["created", "paid", "fulfilled", "refunded", "failed"] {
        by_status.insert(s.to_string(), json!(0));
    }
    let mut orders_total: i64 = 0;
    for r in &order_rows {
        let st = r.try_get::<String, _>("status")?;
        let n = r.try_get::<i64, _>("n")?;
        orders_total += n;
        by_status.insert(st, json!(n));
    }

    // 礼物流水（gift_events，0008 恒存在）：条数 / 礼物总量 / 覆盖世界数。
    let gift_events = count(db, "SELECT COUNT(*) AS n FROM gift_events").await?;
    let gift_count =
        count(db, "SELECT CAST(COALESCE(SUM(gift_count),0) AS BIGINT) AS n FROM gift_events").await?;
    let gift_worlds = count(db, "SELECT COUNT(DISTINCT world_id) AS n FROM gift_events").await?;

    // billingEnabled 反映实际：有充值即 true（§2.6 阶段门由真实数据体现，不写死）。
    let billing_enabled = recharge_total > 0;

    Ok(Json(json!({
        "billingEnabled": billing_enabled,
        "recharge": { "totalCents": recharge_total, "count": recharge_count },
        "refund": { "totalCents": refund_total, "count": refund_count },
        "balance": { "totalCents": balance_total, "wallets": wallet_count },
        // 账本净额 = 充值 - 退款，恒等于当前余额合计（双录不变量），供前端自检。
        "ledgerNetCents": recharge_total - refund_total,
        "orders": { "total": orders_total, "byStatus": Value::Object(by_status) },
        "gifts": { "events": gift_events, "giftCount": gift_count, "worlds": gift_worlds },
        // 创作者结算是另一套账（签约主体/合同/税务/KYC/退款准备金），不在本只读聚合内（§2.6）。
        "creatorSettlement": { "enabled": false },
        "notes": [
            "资金来自账本双录（ledger_entries）与钱包余额（billing_balances），只读聚合，不建结算。",
            "充值总额=账本 recharge 正流水；退款总额=账本 refund 出账；余额合计=当前钱包净额（充值−退款）。",
            "用户余额不可提现/转账；创作者结算为另一套账，不在此聚合（§2.6）。"
        ],
    })))
}
