//! **按校准维度分组的只读读数**（总规格 §79/§83 内容生产流水线的第三环补位）。
//!
//! 流水线是「人工校准 → 仿真试跑 → 世界质量回归」。前两环已建成（`admin_api::calibration` 的三维
//! 视图 + `runtime::simulation` / `slo::quality` 的完读率·阻断率·结局分布），但**中间少一根线**：
//! 现有指标全部按 `character_id` / 按世界聚合，与「运营调的那个旋钮」（身份 id、境界档）无关，
//! 所以「这样配是不是更好」在指标结构上根本问不出来。本模块补的就是这一根线：
//! 把已有的质量口径**按校准维度重新分组**，让「这一维的配置」与「叙事结果」并排出现。
//!
//! # 🔴 三条不许越过的线（每条都有用例锁着，改动前先读用例名）
//!
//! ## ① 只读聚合，绝不回灌引擎
//!
//! 本模块**没有一条 INSERT / UPDATE / DELETE**，产物只作为 JSON 返回给运营后台。
//! 理由与 `world_contributions` 独立于 `narrative_state_json` 单独建表**完全同源**（迁移 0025）：
//! 一旦这些数被写回世界状态、或以任何形式进入引擎的判定输入，「按身份分组的戏份差」就变成了
//! 「身份影响判定」，直接违反 §0.1 平权红线。**算出来给人看，不能变成引擎判定输入。**
//! 锁：`calibration_readings_never_write_anything`、`calibration_readings_never_touch_narrative_state`。
//!
//! ## ② 四态必须分得开（前三态口径抄 `slo::ooc_appeal_block`，第四态见 §0.5）
//!
//! | 情形 | status | value 侧 | 后台显示 |
//! |---|---|---|---|
//! | 这一维从未被任何模板配置过 | `entry_not_open`（块级） | 全部计数缺席 | `—` |
//! | 配置过，但窗口内零样本 | `no_data_in_window` | 计数为 0 但无读数 | `—` |
//! | 有样本，但 n < `minN` | `insufficient_sample`（读数级） | `value=null`，**点估计与区间照给** | `样本不足（n=3）` |
//! | 有样本且 n ≥ `minN` | `ok` | 真数（**可以是 0**） | 真数 |
//!
//! 这条纪律的由来：直接报 0 会得到「看起来棒极了、实际上什么都没测」的数，而门槛判定恰恰要拿它
//! 决定继续 / 调整 / 停止。第四态是同一个病的另一半：`meanShareGini` 在 **3 个观察**和
//! **300 个观察**上曾长得一模一样，运营会追着噪声调参。
//! 锁：`identity_dimension_separates_three_empty_states`、`realm_dimension_separates_three_empty_states`、
//! `readings_separate_insufficient_sample_from_the_two_empty_states`。
//!
//! ## ③ 不许算成「越高越好」的单一分数
//!
//! 校准是**多目标**的（公平 vs 戏剧性：把戏份摊平到基尼 0 也就没有主角了）。给一个综合评分会诱导
//! 运营去优化那个数字本身。本模块**只给分维度的事实，不给判语**——没有 score、没有 grade、
//! 没有 pass/fail、没有 recommendation，与 `admin_api::calibration` 现有页面「只呈现事实」一致。
//! 锁：`calibration_readings_expose_no_composite_score`。
//!
//! 🔴 补了置信区间之后**更不能**顺手给一个「显著/不显著」的布尔：那等于把统计判断包办了，
//! 而「差 0.1 算不算差」取决于运营在权衡什么，不取决于 p 值。**给区间，让人自己看。**
//! `insufficient_sample` 不是判语的例外——它说的是「这个数还不能用」，不是「配得对不对」。
//!
//! # 两维形状**刻意不同构**（想清楚再改）
//!
//! - **身份维**（§5）：身份是**各不相同**的开局站位，一个世界里同时存在多个身份 → 有**组内分布**。
//!   故读数是「同一个世界内部，各身份拿到的戏份份额」，直接回答「某个身份是不是系统性拿得更少」。
//! - **戏服维**（§6【拍板 3】）：境界档**全员统一**——一个世界只有一件戏服、无池、无配额、
//!   装配层零抽样。它**没有组内分布可看**。故读数只能是**跨世界对比**：把窗口内的世界按钉住的戏服
//!   分桶，各桶各自报世界质量三指标（完读 / 阻断 / 结局）。
//!   🔴 把身份维的「分布」形状套到戏服维上，得到的是一个恒等于 0 的假指标。
//!
//! # 窗口：**同一批世界**（cohort 口径），两维共用
//!
//! 窗口谓词是 `worlds.created_at ∈ [start, end)`——「窗口内开出的这一批世界」，两维共用同一批，
//! 于是两维的读数彼此可比，且**只需一次 `worlds` 分页扫描**就同时喂饱两维。
//!
//! ⚠️ 这与 §4 `attentionGini` 的窗口口径**不同**（那里是「窗口内有贡献分更新的世界」），
//! 故两处的世界集合不是同一批，数字**不可互相校验**。选 cohort 是因为戏服维要用
//! `quality` 的全生命周期三指标，而那三个数只有对「一批世界」才有意义。
//!
//! ⚠️ cohort 口径的代价：近期窗口里必然有大量世界还没跑完，`completionRate` 天然偏低。
//! 这不影响本模块的用途——**本模块要的是横向对比**，同一窗口内各桶承受同样的截断偏差；
//! 但各桶的世界年龄分布若差得远，横向比也会失真，故每桶一并给出 `firstCreatedAt` /
//! `lastCreatedAt` / `unfinished`，让读的人自己看得见这个偏差（给事实，不给判语）。
//!
//! # 口径复用，不另立第二套
//!
//! 集中度一律 `super::gini_coefficient`（与叙事注意力基尼同一个实现）、中位数 `super::percentile`、
//! 比率 `super::rate`、收尾分类与三指标 `super::quality`（`completion_stats` / `block_stats` /
//! `ending_distribution`）。本模块**只新增分组的圈法**，不新增任何算法——
//! 回归与看板必须永远算同一个数。
//!
//! # 性能（本模块的产物挂在被轮询的 `/admin/metrics/overview` 上）
//!
//! `worlds.assembled_json` 可达数十 KB/份，故按主键**分页**扫描（`id > ? ORDER BY id ASC`），
//! 每页解析完即丢，只留几个整数与短串；世界数超过 `calibration_world_cap` 即
//! **明说跳过**（`skipped_too_large`）而不是硬算。其余取数一律按 `IN (…)` 分批的聚合查询，
//! **没有一条按世界逐个发的 SQL**（`slo` 模块头注释明令禁止的 N+1）。
//! 查询预算 ≈ 分页 4 条 + 成员分 2 条 + `quality` 批量事实 6 条 + 两个存在性探针 2 条 ≈ **14 条**，
//! 与世界数无关（只与页数/分批数有关，二者都被 cap 封顶）。
//! `?slo=0` 一并跳过（本模块由 `narrative_slo` 调用，那个开关在更外层）。
//!
//! # 双库可移植 SQL（`db.rs` 约定）
//!
//! 只用 `COUNT` / `SUM(CASE …)` / `COALESCE` / `LEFT JOIN` / `IN` / `LIKE` / BIGINT 比较；
//! 聚合列一律 `CAST(… AS BIGINT)`（PG 下 `SUM(bigint)` 返回 numeric，不 CAST 解码会炸）。
//! 无 JSONB、无 serial、无 `NOW()`、无 `strftime` / `date_trunc`。占位符 `$N` 严格顺序不复用。
//! **JSON 一律 Rust 侧 serde 解析**（`json_extract` 是 SQLite 方言）。
//!
//! # 确定性契约（直接决定 §0.5 能用什么统计方法）
//!
//! 本模块**不用系统随机、不用浮点 RNG、不依赖 map 迭代顺序**（分组一律 `BTreeMap` / `BTreeSet`，
//! 桶序由 id 升序定死）。同一份数据必须永远算出同一个数——否则「调参前后差了多少」这句话失去意义。
//! 这条约束把 **bootstrap 置信区间排除在外**（它要随机重采样），故 §0.5 的区间一律是**闭式解**；
//! 不适合闭式解的统计量（基尼）宁可不给区间，也不引入随机性，理由与替代方案写在 [`ci_notes`] 的 `giniNoInterval` 一条里（随响应下发）。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::db::Placeholders;
use crate::error::ApiError;

use super::quality::{self, WorldQualityFacts};
use super::{gini_coefficient, over_cap, percentile, rate, skipped_too_large, SloConfig};

// ============================================================================
// §0 常量与桶名
// ============================================================================

/// 身份维的**对照桶**：在场、但那一轮装配没给他站位的成员。
/// 🔴 不丢弃、不并进任何身份——「有身份的人和没身份的人拿到的戏一样多吗」正是要看的对比之一。
const IDENTITY_UNASSIGNED: &str = "(unassigned)";
/// 戏服维的**对照桶**：实例没钉住任何境界档（模板没配 / 配之前就开出来了）。
/// 同样不丢弃：它是「配了戏服的世界」唯一的参照系。
const REALM_NONE: &str = "(none)";
/// 相对份额 → 整数千分位。`gini_coefficient` 的入参是 `&[i64]`（刻意的：整数无表示歧义），
/// 故均值要先定点化再送进去；1.000 = 恰好均分。
const SHARE_SCALE: f64 = 1000.0;
/// 分页扫描 `worlds`（含 `assembled_json`）每页行数。峰值内存 ≈ 本页装配 JSON 大小之和。
const SCAN_PAGE: i64 = 100;
/// 单条 `IN (…)` 最多绑几个参数（SQLite 老版本 `SQLITE_MAX_VARIABLE_NUMBER` 默认 999）。
const BIND_CHUNK: usize = 200;
/// 每维展开的桶数上限（脏数据防线：身份 id 是自由文本，历史实例可能钉着任意串）。
/// 超出即**按观察数取头部并置 `bucketsTruncated`**，不静默丢。
const BUCKET_MAX: usize = 50;

// ============================================================================
// §0.5 样本量与不确定性（每个读数的统一信封）
// ============================================================================
//
// 由来：上一批建成的读数里，`meanShareGini` 在 **3 个观察**和 **300 个观察**上长得一模一样。
// 拿它做门槛判定，运营会追着噪声调参——而这些读数存在的**全部理由**就是给运营调参用。
// 本节补两件事：① 每个读数随身带 `n`（跑不掉：`value` 与 `n` 在同一个对象里，
// 取值必须穿过这层信封）；② 低于最小样本量的读数明确标成「样本不足」而不是给一个看起来正常的数。

/// 读数的**第四态**：测了、有样本，但样本量不足以据此调参。
///
/// 🔴 **绝不许渲染成 0 或空。** 它与两个空态的区别是**多**给数据而不是少给：
/// `n` / 分子分母 / 点估计 / 置信区间全在，只有 `value`（「可以据此调参的读数」）是 null。
/// 后台该显示的是「样本不足（n=3）」而不是 `—`，更不是 `0`。
const STATUS_INSUFFICIENT: &str = "insufficient_sample";
/// 读数级的零样本（分母为 0）。与块级 `no_data_in_window` 同名同义，粒度不同：
/// 一个 `ok` 的块里完全可能有某个读数的分母是 0（如某桶一个事件都没落）——
/// 此时 `rate()` 会给 0.0，而「0 个事件里 0 个被扣留 = 扣留率 0%」正是「看起来棒极了」的那种数。
const STATUS_NO_DATA: &str = "no_data_in_window";
/// 真数（**可以是 0**）。
const STATUS_OK: &str = "ok";

/// 标准正态 0.975 分位点（双侧 95%）。**这是常数不是随机数**——本模块的确定性契约禁 RNG，
/// 所有区间都是闭式解，同一份数据永远算出同一个区间。
const Z95: f64 = 1.959_964;
/// 置信水平（随区间一起下发，免得有人把 95% 的区间当 99% 读）。
const CI_LEVEL: f64 = 0.95;

/// 比例类读数的 95% **Wilson 得分区间**。
///
/// 为什么是 Wilson 而不是正态近似 `p̂ ± z√(p̂(1−p̂)/n)`：后者在 p̂ 贴边时宽度**塌成 0**——
/// 「3 个观察全是零分 → 零分率 100%，区间 [1,1]」，偏偏那正是最该提示「这是噪声」的地方。
/// Wilson 把估计往 0.5 收缩、区间恒非退化，小样本与边界上都不骗人（n=3、x=3 时给出的是
/// 约 [0.44, 1.00]，一眼可见「什么都没测出来」）。闭式解、无迭代、无随机。
///
/// 分母 ≤0 → `None`（不除零、不编区间）。
fn wilson_interval(numer: i64, denom: i64) -> Option<(f64, f64)> {
    if denom <= 0 {
        return None;
    }
    let n = denom as f64;
    let p = (numer as f64 / n).clamp(0.0, 1.0);
    let z2 = Z95 * Z95;
    let scale = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / scale;
    let half = (Z95 / scale) * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    Some(((center - half).max(0.0), (center + half).min(1.0)))
}

/// 样本量 → 读数状态。三档：零样本 / 样本不足 / 够。
fn reading_status(n: i64, min_n: i64) -> &'static str {
    if n <= 0 {
        STATUS_NO_DATA
    } else if n < min_n {
        STATUS_INSUFFICIENT
    } else {
        STATUS_OK
    }
}

/// `value` 只在 `ok` 态给数——**这就是「样本不足不许渲染成正常数」的物理实现**。
/// 点估计另存 `pointEstimate`（事实不藏），但那个键名摆明了是「原始算术」，不是可据此调参的读数。
fn gated_value(status: &str, point: Option<f64>) -> Value {
    if status == STATUS_OK {
        json!(point)
    } else {
        Value::Null
    }
}

/// 这个读数的区间怎么处理。
enum Ci {
    /// 给 Wilson 区间；`Some(note)` 附上这个区间的适用边界（不是免责声明，是读法说明）。
    Wilson(Option<&'static str>),
    /// 不给区间，附**为什么不给**——空着会被当成「忘了算」，而这里是想清楚了才不给。
    Omitted(&'static str),
}

// ---- 区间说明：读数里只带**短码**，全文在 `calibration.ciNotes` 里给一次 ----
//
// 🔴 这不是为了省事：每条说明 150-500 字，而读数最多可达 50 桶 × 每桶 4 条 ——
// 全文逐条重复会让被轮询的 `/admin/metrics/overview` 凭空胖上百 KB，
// 与模块头「保护被轮询端点」是同一条纪律。短码在同一个文档里解析得到，
// 由 `every_reading_carries_its_own_sample_size` 锁住不会出现悬空引用。

/// 观察按世界聚类时，Wilson 区间的读法（Wilson 假设 n 次**独立**伯努利试验）。
const NOTE_CLUSTERED: &str = "clustered";
/// 完读率专属：区间只管抽样噪声，管不了 cohort 截断偏差。
const NOTE_TRUNCATION: &str = "cohortTruncation";
/// 「先选最大再算比例」的读数为什么不给区间。
const NOTE_POST_SELECTION: &str = "postSelection";
/// 均值类读数为什么不给区间。
const NOTE_MEAN: &str = "meanNotIid";
/// 🔴 基尼为什么不给区间（本批被点名的那一条，判断与替代方案都在这条文案里）。
const NOTE_GINI: &str = "giniNoInterval";

/// 短码 → 全文。随 `calibration.ciNotes` 下发**一次**（数会被复制走，注释不会）。
fn ci_notes() -> Value {
    let mut m = serde_json::Map::new();
    m.insert(
        NOTE_CLUSTERED.into(),
        json!(
            "n 按世界聚类（同一个世界的多个成员 / 多拍 / 多事件不独立），而 Wilson 假设 n 次独立试验：\
             真实区间比这个**宽**（设计效应 ≥1）。当宽度下界读——它足以否定一个数，不足以坐实一个数。"
        ),
    );
    m.insert(
        NOTE_TRUNCATION.into(),
        json!(
            "区间只描述抽样噪声，**不覆盖 cohort 截断偏差**（分母含未收尾世界，近期窗口天然偏低）。\
             那是系统偏差，样本量再大也不会消失——跨桶横比时请一并看 firstCreatedAt / lastCreatedAt / unfinished。"
        ),
    );
    m.insert(
        NOTE_POST_SELECTION.into(),
        json!(
            "topEndingShare 是**先选出最大的那个结局、再算它的占比**（post-selection）：\
             朴素区间会系统性偏窄偏乐观，结局种类越多越乐观。故不给区间，改为把 n（落到真实结局的世界数）\
             与 distinctEndings 一并给出，让读的人自己判断「最大占比 0.6」是几个结局里挑出来的。"
        ),
    );
    m.insert(
        NOTE_MEAN.into(),
        json!(
            "均值类读数不给置信区间，因为观察**不独立**：同一个世界内各成员的相对份额之和恒等于成员数\
             （结构约束），且同一个世界同时向多个身份桶供数。t / 正态区间会系统性低估宽度，\
             而把它标成 95% 会比不给更糟。做对了要按世界聚类的稳健方差，可窗口内世界数（worlds）\
             常常只有个位数，那样的区间同样不可信。这里给 n / worlds（聚类数）/ sd / 中位数 / 极值，\
             宽度由读的人自己判断。"
        ),
    );
    m.insert(
        NOTE_GINI.into(),
        json!(
            "基尼是非线性统计量，没有闭式置信区间。三条路各自的结论：\
             ① **bootstrap 不做** —— 它要随机重采样，而本模块的确定性契约禁系统随机与浮点 RNG\
             （同一份数据必须永远算出同一个数，否则「调参前后差了多少」无从谈起）；\
             ② **jackknife（留一，无随机）技术上可做，但答错了问题** —— 它重采样的是**分组**，\
             而身份池 / 结局池是配置出来的总体、不是抽样得到的样本；真正的抽样波动在**组内**\
             （每个分组的均值只由 sampleN 个观察支撑）。留一组区间会给出一个看起来很权威、\
             实则没人问过的数，比不给更危险；\
             ③ **delta 法**能把组内标准误传播上去，但基尼在并列值处不可导、值域有界，k 又常常只有 2-8，\
             正是正态近似最不成立的区间——在那里发一条 ± 带正好是本批要堵的病。\
             故**改为报样本量本身**：n（分组数）、sampleN（门槛盯的那个观察数）、\
             minGroups / minN，外加各组均值的极值。n=3 与 n=300 因此长得不一样，这就是本读数要的效果。"
        ),
    );
    Value::Object(m)
}

/// 比例类读数（分子分母都是计数）的统一信封。
///
/// 🔴 `n` 与 `value` 在同一个对象里是**刻意的**：取值必须穿过这层信封，
/// 于是「拿到一个比例却不知道它压在几个观察上」在结构上不可能发生。
/// `ciNoteRef` 是短码，全文在 `calibration.ciNotes` 里给一次（见上方那段关于载荷体积的说明）。
fn proportion_reading(numer: i64, denom: i64, unit: &str, min_n: i64, ci: Ci) -> Value {
    let status = reading_status(denom, min_n);
    let point = (denom > 0).then(|| rate(numer, denom));
    let (ci95, note) = match ci {
        Ci::Wilson(note) => (
            wilson_interval(numer, denom)
                .map(|(lo, hi)| json!({ "low": lo, "high": hi, "method": "wilson", "level": CI_LEVEL }))
                .unwrap_or(Value::Null),
            note,
        ),
        Ci::Omitted(why) => (Value::Null, Some(why)),
    };
    json!({
        "status": status,
        "value": gated_value(status, point),
        "pointEstimate": point,
        "numerator": numer,
        "n": denom,
        "unit": unit,
        "minN": min_n,
        "ci95": ci95,
        "ciNoteRef": note,
    })
}

/// 均值类读数的统一信封（入参必须**已升序**：中位数与极值直接取自它）。
///
/// `worlds` = 这些观察落在几个世界里，即**聚类数**。它与 `n` 一起给不是冗余：
/// 「30 个观察」落在 15 个世界与落在 1 个世界，可信度差着量级。
fn mean_reading(sorted: &[f64], worlds: i64, min_n: i64) -> Value {
    let n = sorted.len() as i64;
    let status = reading_status(n, min_n);
    let mean = (n > 0).then(|| sorted.iter().sum::<f64>() / n as f64);
    // 样本标准差（n−1 分母）。n<2 无从谈起 → None，不编 0（0 会被读成「完全一致」）。
    let sd = match (n >= 2, mean) {
        (true, Some(m)) => {
            let ss: f64 = sorted.iter().map(|v| (v - m) * (v - m)).sum();
            Some((ss / (n as f64 - 1.0)).sqrt())
        }
        _ => None,
    };
    json!({
        "status": status,
        "value": gated_value(status, mean),
        "pointEstimate": mean,
        "n": n,
        "worlds": worlds,
        "unit": "observation",
        "minN": min_n,
        "sd": sd,
        "median": percentile(sorted, 0.5),
        "min": sorted.first().copied(),
        "max": sorted.last().copied(),
        "ci95": Value::Null,
        "ciNoteRef": NOTE_MEAN,
    })
}

/// 集中度（基尼）类读数的入参。字段多是因为**它的样本量有两层**：
/// 统计量算在几个分组上（`groups`），以及那些分组各自压在多少观察上（`sample_n`）。
/// 只报前者会重演本批要修的病：2 个身份 × 各 2 个观察，与 2 个身份 × 各 300 个观察，`groups` 都是 2。
struct GiniInput<'a> {
    /// 点估计。分组数 <2 时传 `None`（那时基尼恒为 0，是个符号反了的假指标）。
    point: Option<f64>,
    /// 统计量算在几个分组上。
    groups: i64,
    /// 分组的单位名（`identity` / `ending`）。
    group_unit: &'a str,
    /// **门槛盯的那个观察数**：取「最弱那条腿」或「总观察数」，由调用方按语义选，并在 `sample_basis` 里说明。
    sample_n: i64,
    /// `sample_n` 数的是什么（下发给读的人，不然 n 与 sampleN 谁是谁分不清）。
    sample_basis: &'a str,
}

/// **最小样本量约定**：随读数一起下发（同 `shapeRationale` 的理由——数会被复制走，注释不会）。
///
/// 依据写进响应而不是只写进注释，是因为「30 从哪来的」会在第一次有人质疑读数时被问到，
/// 而那时手边只有 JSON。
fn sample_floor(cfg: &SloConfig) -> Value {
    json!({
        "minN": cfg.calibration_min_n,
        "minGroups": cfg.calibration_min_groups,
        "envKeys": ["MUSE_SLO_CALIBRATION_MIN_N", "MUSE_SLO_CALIBRATION_MIN_GROUPS"],
        "rationale": [
            "minN 默认 30 的依据①：比例类读数在最坏情形 p̂=0.5 下的 95% Wilson 区间半宽 —— n=3 → ±0.37（几乎覆盖整个值域）、n=10 → ±0.26、n=30 → ±0.17、n=100 → ±0.10。30 大致是「区间首次窄过运营真正会据此行动的效应量（两档之间差 20 个百分点）」的位置。",
            "依据②：n=30 时单个观察最多把比例挪动 3.3 个百分点，n=3 时是 33 个百分点 —— 「一条数据翻转结论」在 30 上不再发生，这是能不能拿它调参的分界。",
            "依据③：30 同时是 CLT 的教科书惯例门槛，均值类读数在此之上谈「均值的抽样分布近似正态」才不荒唐。",
            "minGroups 默认 2：gini_coefficient 在只有 1 个分组时恒返回 0，而 0 读起来是「很分散」—— 真相恰恰相反（全压在这一个分组上）。那是个符号反了的假指标，必须被门槛拦住。",
            "🔴 这是**默认值不是物理常量**（同 attentionGiniMax）：预注册纪律要求门槛「开测前可改、开测后冻结」，故经 env 可覆盖。",
            "🔴 过了 minN 不等于结论成立 —— 那是区间要回答的问题，不是样本量门槛能回答的。本段只拦住「数太少不该看」，不判断「配得对不对」。",
        ],
        "readingStatuses": {
            "entry_not_open": "这一维从未被任何模板配置过（块级）。value=null，显示 —。",
            "no_data_in_window": "配置过 / 有世界，但这个读数的分母是 0。value=null，显示 —。",
            "insufficient_sample": "有样本但 n < minN。value=null，pointEstimate 与 ci95 照给。🔴 显示「样本不足（n=…）」，不许显示成 0，也不宜与 — 混同。",
            "ok": "n ≥ minN。value 给真数（可以是 0）。",
        },
    })
}

/// 集中度类读数的统一信封。**不给区间**，理由与替代方案见 [`ci_notes`] 的 `giniNoInterval`。
fn gini_reading(input: GiniInput<'_>, min_groups: i64, min_n: i64) -> Value {
    let status = if input.groups <= 0 {
        STATUS_NO_DATA
    } else if input.point.is_none() || input.groups < min_groups || input.sample_n < min_n {
        STATUS_INSUFFICIENT
    } else {
        STATUS_OK
    };
    json!({
        "status": status,
        "value": gated_value(status, input.point),
        "pointEstimate": input.point,
        "n": input.groups,
        "unit": input.group_unit,
        "sampleN": input.sample_n,
        "sampleBasis": input.sample_basis,
        "minGroups": min_groups,
        "minN": min_n,
        "ci95": Value::Null,
        "ciNoteRef": NOTE_GINI,
    })
}

// ============================================================================
// §1 装配产物的防御式解析（Rust 侧，绝不用 json_extract）
// ============================================================================

/// 一个世界在本次扫描里留下的**全部**痕迹（几个整数 + 几个短串；装配 JSON 解析完即丢）。
struct ScannedWorld {
    id: String,
    status: String,
    created_at: i64,
    /// `/assembly/identityAssignments` → `[(角色 id, 身份 id)]`。未装配 / 无该键 → 空。
    assignments: Vec<(String, String)>,
    /// `/assembly/realmTier.id`。未钉住 / 空 id → `None`。
    realm_tier: Option<String>,
    assembled: bool,
}

/// 从实例 `assembled_json` 读回 `[(cid, identityId)]`。
///
/// ⚠️ 结构口径与 `runtime::parse_identity_assignments`、`admin_api::calibration::parse_assignments`
/// **逐字一致**（同一个 JSON 指针、同样的逐条防御式跳过），但**刻意第三次各写各的**：
/// 那两处一个是 runtime 私有、一个是 admin 观测面私有，跨模块借用会让「观测面挂了不该有能力影响
/// tick」这条隔离失效。三份实现的契约由各自的解析退化用例分别锁住
/// （本文件是 `assembly_parsers_degrade_defensively`）。
fn parse_assignments(assembled_json: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = assembled_json else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(arr) = v.pointer("/assembly/identityAssignments").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pair in arr {
        let Some(p) = pair.as_array() else { continue };
        let (Some(cid), Some(iid)) = (
            p.first().and_then(Value::as_str).map(str::trim),
            p.get(1).and_then(Value::as_str).map(str::trim),
        ) else {
            continue;
        };
        if cid.is_empty() || iid.is_empty() {
            continue;
        }
        out.push((cid.to_string(), iid.to_string()));
    }
    out
}

/// 从实例 `assembled_json` 读回钉住的境界档 id。空 id 钉不住任何东西 → `None`。
fn parse_pinned_realm_id(assembled_json: Option<&str>) -> Option<String> {
    let v: Value = serde_json::from_str(assembled_json?).ok()?;
    let id = v.pointer("/assembly/realmTier/id").and_then(Value::as_str)?.trim();
    (!id.is_empty()).then(|| id.to_string())
}

// ============================================================================
// §2 取数
// ============================================================================

/// 「这一维在平台上**开过工没有**」的全生命周期存在性探针（不切窗口）。
///
/// 🔴 **这不是 JSON 解析**，是骨架文本的存在性探针，只用于三态里 `entry_not_open` 的判定；
/// 任何进入读数的数值一律来自 Rust 侧 serde 解析（§0 约定）。
/// 选 `LIKE` 而不是把全表骨架捞回来 Rust 侧解析，是因为这里只需要一个布尔，
/// 而骨架可达数十 KB/份——为一个布尔扫几十 MB 是不可接受的。
///
/// 误判方向是**安全**的：模板真声明了该键，`skeleton_json` 里就一定有这个字面串，故**不会漏判**
/// （不会把「开过工」误报成「从未开工」）；反向的假阳性（骨架别处恰好出现同名字符串）只会把
/// `entry_not_open` 说成 `no_data_in_window`，而这两者在后台**同样显示 `—`**。
/// 子查询带 `LIMIT 1`，命中一条即短路，不做全表计数。
async fn template_declares(db: &AnyPool, json_key: &str) -> Result<bool, ApiError> {
    let pattern = format!("%\"{json_key}\"%");
    let n: i64 = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM ( \
             SELECT id FROM world_templates WHERE skeleton_json LIKE $1 LIMIT 1 \
         ) t",
    )
    .bind(pattern)
    .fetch_one(db)
    .await?
    .try_get("n")?;
    Ok(n > 0)
}

/// 分页扫描窗口内开出的世界，解析装配产物后**只留下几个整数与短串**。
///
/// 返回 `None` = 世界数超过 `cfg.calibration_world_cap`（调用方转 `skipped_too_large`）：
/// 宁可这一栏显示「数据量超限」，也不许把被轮询的后台端点拖垮。
async fn scan_cohort(db: &AnyPool, cfg: &SloConfig) -> Result<Option<Vec<ScannedWorld>>, ApiError> {
    let cap = cfg.calibration_world_cap;
    let mut cursor = String::new();
    let mut out: Vec<ScannedWorld> = Vec::new();
    loop {
        // `id > $3` 的主键游标分页：`ORDER BY id ASC` 是**全序**（id 是主键），
        // PG 无稳定序保证，分页游标必须建立在全序上，否则会漏行 / 重复行。
        let page = sqlx::query(
            "SELECT id, status, created_at, assembled_json FROM worlds \
             WHERE created_at >= $1 AND created_at < $2 AND id > $3 \
             ORDER BY id ASC LIMIT $4",
        )
        .bind(cfg.window_start)
        .bind(cfg.window_end)
        .bind(&cursor)
        .bind(SCAN_PAGE)
        .fetch_all(db)
        .await?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len() as i64;
        for r in &page {
            let id: String = r.try_get("id")?;
            cursor = id.clone();
            let assembled: Option<String> = r.try_get("assembled_json")?;
            let has = assembled.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
            out.push(ScannedWorld {
                id,
                status: r.try_get("status")?,
                created_at: r.try_get("created_at")?,
                assignments: parse_assignments(assembled.as_deref()),
                realm_tier: parse_pinned_realm_id(assembled.as_deref()),
                assembled: has,
            });
        }
        if over_cap(out.len(), cap) {
            return Ok(None);
        }
        if page_len < SCAN_PAGE {
            break;
        }
    }
    Ok(Some(out))
}

/// 窗口内这批世界的「成员 → 戏份分」：`world_members` **LEFT JOIN** `world_contributions`。
///
/// 🔴 **分母是 `world_members` 全集，无贡献行的成员记 0 分**——这与 §4 `attentionGini` 的
/// **交集**口径是有意的不同，两处不可互相校验：
/// 贡献分行是**挣到分才落**的（`progression::accumulate_contributions_tx` 里 `delta <= 0` 直接
/// `continue`，不建行），于是交集口径**看不见「一分没挣到」的人**。而本读数要回答的恰恰是
/// 「某个身份是不是系统性拿不到戏」——把零分的人漏掉，这个问题就答不了，还会把最坏的情况
/// 洗成「查无此人」。
///
/// NPC 不入 `world_members`（与 `world_contributions` 不同），故这里**不需要**像基尼那样再取交集。
async fn member_scores(
    db: &AnyPool,
    world_ids: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, i64>>, ApiError> {
    let mut out: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for chunk in world_ids.chunks(BIND_CHUNK) {
        // 整条语句只有这一串参数，故从 $1 起顺序发号，与下面 bind 的循环顺序一一对应。
        let ph = Placeholders::new().list(chunk.len());
        // 不加 ORDER BY：结果全部落进 BTreeMap 累加，消费与行序无关（输出定序由 BTreeMap 保证）。
        let sql = format!(
            "SELECT wm.world_id AS world_id, wm.cloud_character_id AS cid, \
             CAST(COALESCE(wc.score_milli, 0) AS BIGINT) AS score \
             FROM world_members wm \
             LEFT JOIN world_contributions wc \
               ON wc.world_id = wm.world_id AND wc.character_id = wm.cloud_character_id \
             WHERE wm.world_id IN ({ph})"
        );
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id.as_str());
        }
        for r in q.fetch_all(db).await? {
            out.entry(r.try_get::<String, _>("world_id")?)
                .or_default()
                .insert(r.try_get::<String, _>("cid")?, r.try_get::<i64, _>("score")?);
        }
    }
    Ok(out)
}

// ============================================================================
// §3 维度一：身份维（身份分配 × 戏份分布）
// ============================================================================

/// 一个身份桶累计到的观察。**相对份额**是这一维唯一的可比单位（见下方 `relative_shares` 注释）。
#[derive(Default)]
struct IdentityAcc {
    shares: Vec<f64>,
    worlds: BTreeSet<String>,
    zero_score: i64,
    raw_score_milli: i128,
}

impl IdentityAcc {
    fn push(&mut self, world_id: &str, share: f64, raw: i64) {
        self.shares.push(share);
        self.worlds.insert(world_id.to_string());
        if raw <= 0 {
            self.zero_score += 1;
        }
        self.raw_score_milli += raw as i128;
    }

    fn mean(&self) -> Option<f64> {
        (!self.shares.is_empty())
            .then(|| self.shares.iter().sum::<f64>() / self.shares.len() as f64)
    }

    fn observations(&self) -> i64 {
        self.shares.len() as i64
    }

    fn worlds(&self) -> i64 {
        self.worlds.len() as i64
    }

    /// 升序副本（`mean_reading` 的入参契约：中位数与极值直接取自它）。
    fn sorted_shares(&self) -> Vec<f64> {
        let mut v = self.shares.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v
    }
}

/// 一个世界内部，把每个成员的戏份折成**相对均分的倍率**：`(score / total) × n`。
///
/// 🔴 为什么必须归一化而不能直接比原始分：不同世界跑的拍数差几十倍，原始分跨世界求和只会让
/// 「跑得久的世界里那个身份」看起来戏份多，测的其实是世界寿命不是身份失衡。
/// 归一化之后单位是「相对本世界均分的倍率」：**1.0 = 恰好拿到均分**，0.5 = 只拿到均分的一半。
/// 于是每个观察（世界 × 角色）等权，大世界不会主导，小世界也不会。
///
/// 返回 `None` 表示这个世界不具备可比性：成员数 < 2（一个人的世界里「份额」恒为 1，无信息），
/// 或全员零分（「无人有戏」不是不公平，是没开演——同 `gini_coefficient` 对全零集的处理）。
fn relative_shares(scores: &BTreeMap<String, i64>) -> Option<BTreeMap<String, f64>> {
    let n = scores.len();
    if n < 2 {
        return None;
    }
    let total: i128 = scores.values().map(|v| *v as i128).sum();
    if total <= 0 {
        return None;
    }
    Some(
        scores
            .iter()
            .map(|(cid, s)| (cid.clone(), (*s as f64 / total as f64) * n as f64))
            .collect(),
    )
}

/// 身份维读数：**身份分配 → 戏份分布**。
///
/// 回答的问题只有一个：**某个身份的角色，是不是系统性地拿到更少戏份。**
/// 每个身份给出「相对均分倍率」的均值 / 中位数 / 极值 / 零分观察数，外加各身份**均值之间**的
/// 集中度基尼（`super::gini_coefficient`，与叙事注意力基尼同一个实现）。
///
/// ⚠️ `meanShareGini` 与 §4 的 `attentionGini` **不是同一个数、不可互相校验**：
/// 前者的观察单位是「身份」（各身份平均分到的份额彼此差多少），后者是「角色」
/// （单个世界里各角色的原始分彼此差多少）。一个身份池完全均衡（本数 = 0）与
/// 某个世界里有人独占戏份（`attentionGini` 很高）**可以同时成立**。
///
/// 🔴 本读数**不判断哪种分布更好**：戏份完全摊平（各身份均值都是 1.0）意味着没有主角，
/// 那未必是想要的结果。这里只给事实。
async fn identity_block(
    db: &AnyPool,
    worlds: &[ScannedWorld],
    cfg: &SloConfig,
) -> Result<Value, ApiError> {
    const METRIC: &str = "identityShareBalance";
    const TITLE: &str = "身份维：身份分配 × 戏份分布";

    // 三态第一档：这一维从未被任何模板配置过 → 窗口里不可能有分配，报 `—` 而不是 0。
    if !template_declares(db, "identityPool").await? {
        return Ok(json!({
            "metric": METRIC,
            "title": TITLE,
            "dimension": "identity",
            "status": "entry_not_open",
            "value": Value::Null,
            "notes": [
                "全平台没有任何模板声明过 skeletonJson.identityPool —— 身份维在这个平台上从未开过工。",
                "🔴 这是「没测过」不是「分配很均衡」：后台必须显示 —，显示 0 即为误报。",
                "开工方式：建模板时录入 skeletonJson.identityPool（POST /admin/world-templates）。",
            ],
        }));
    }

    let mut by_identity: BTreeMap<String, IdentityAcc> = BTreeMap::new();
    let mut worlds_with_assignments = 0i64;
    let mut worlds_counted = 0i64;
    let mut worlds_single_member = 0i64;
    let mut worlds_without_score = 0i64;
    let mut unknown_assignments = 0i64;

    let assigned_world_ids: Vec<String> =
        worlds.iter().filter(|w| !w.assignments.is_empty()).map(|w| w.id.clone()).collect();
    let scores_by_world = member_scores(db, &assigned_world_ids).await?;

    for w in worlds.iter().filter(|w| !w.assignments.is_empty()) {
        worlds_with_assignments += 1;
        let empty = BTreeMap::new();
        let scores = scores_by_world.get(&w.id).unwrap_or(&empty);
        // 分配里出现、却不在 world_members 里的角色：模板改版 / NPC 被写进分配 / 成员已被清理。
        // 它们没有可比的戏份，单列计数，不进任何桶（也不许悄悄当成 0 分成员）。
        let assign: BTreeMap<&str, &str> =
            w.assignments.iter().map(|(c, i)| (c.as_str(), i.as_str())).collect();
        unknown_assignments +=
            assign.keys().filter(|c| !scores.contains_key(**c)).count() as i64;

        let Some(shares) = relative_shares(scores) else {
            if scores.len() < 2 {
                worlds_single_member += 1;
            } else {
                worlds_without_score += 1;
            }
            continue;
        };
        worlds_counted += 1;
        for (cid, share) in &shares {
            let bucket = assign.get(cid.as_str()).copied().unwrap_or(IDENTITY_UNASSIGNED);
            let raw = scores.get(cid).copied().unwrap_or(0);
            by_identity.entry(bucket.to_string()).or_default().push(&w.id, *share, raw);
        }
    }

    let observations: i64 = by_identity.values().map(|a| a.shares.len() as i64).sum();

    // 三态第二档：配置过，但窗口内没有可比观察 → 零样本，报 `—`。
    // 计数照样下发：让运营一眼看出是「没世界」还是「有世界但都没分配 / 都还没挣到分」。
    if observations == 0 {
        return Ok(json!({
            "metric": METRIC,
            "title": TITLE,
            "dimension": "identity",
            "status": "no_data_in_window",
            "value": Value::Null,
            "worldsScanned": worlds.len() as i64,
            "worldsWithAssignments": worlds_with_assignments,
            "worldsSingleMember": worlds_single_member,
            "worldsWithoutScore": worlds_without_score,
            "observations": 0,
            "notes": [
                "窗口内没有一个「有身份分配 ∧ 成员数≥2 ∧ 有正贡献分」的世界 —— 零样本。",
                "🔴「没测过」不是「分配很均衡」：后台必须显示 —，显示 0 即为误报。",
                "worldsSingleMember / worldsWithoutScore 分开计数：前者是单人世界（份额恒为 1，无信息），后者是全员还没挣到分（没开演，不是不公平）。",
                "🔴 与 insufficient_sample 也不是一回事：那是「有样本但不够（n < minN）」，这里是「一个可比观察都没有（n = 0）」。",
            ],
        }));
    }

    let min_n = cfg.calibration_min_n;

    // 逐身份读数。桶按 id 升序（BTreeMap），跨运行定序。
    let mut rows: Vec<Value> = Vec::with_capacity(by_identity.len());
    let mut declared_means: Vec<i64> = Vec::new();
    // 🔴 基尼的「最弱那条腿」：各身份桶里观察数最少的那个。只报身份桶数（n=2）而不报它，
    // 就会让「2 个身份 × 各 2 个观察」与「2 个身份 × 各 300 个观察」长得一模一样。
    let mut weakest_identity_obs = i64::MAX;
    // (身份 id, 均值, 该桶的均值读数信封) —— 极值直接复用信封，同一个键名不出现两种形状。
    let mut ranked: Vec<(String, f64, Value)> = Vec::new();
    for (identity_id, acc) in &by_identity {
        let sorted = acc.sorted_shares();
        let obs = acc.observations();
        let mean_block = mean_reading(&sorted, acc.worlds(), min_n);
        if identity_id != IDENTITY_UNASSIGNED {
            if let Some(m) = acc.mean() {
                // 定点化后送进整数基尼（`gini_coefficient` 入参恒为 &[i64]）。
                declared_means.push((m * SHARE_SCALE).round() as i64);
                weakest_identity_obs = weakest_identity_obs.min(obs);
                ranked.push((identity_id.clone(), m, mean_block.clone()));
            }
        }
        rows.push(json!({
            "identityId": identity_id,
            // 本桶整体的样本量状态（各读数另有自己的状态，分母不同时会不一样）。
            "status": reading_status(obs, min_n),
            "observations": obs,
            "worlds": acc.worlds(),
            "meanRelativeShare": mean_block,
            "zeroScoreObservations": acc.zero_score,
            "zeroScoreRate": proportion_reading(
                acc.zero_score, obs, "observation", min_n, Ci::Wilson(Some(NOTE_CLUSTERED)),
            ),
            "totalScoreMilli": acc.raw_score_milli as i64,
        }));
    }
    let buckets_truncated = rows.len() > BUCKET_MAX;
    // 「样本量达标的身份数」：`(unassigned)` 不是配置出来的身份，与 `identitiesObserved` 一致地排除。
    let buckets_with_sample = rows
        .iter()
        .filter(|r| r["identityId"] != IDENTITY_UNASSIGNED && r["status"] == STATUS_OK)
        .count() as i64;
    rows.truncate(BUCKET_MAX);

    // 各身份**均值之间**的集中度：0 = 各身份平均拿到一样多，越大 = 越不均。
    // 至少两个身份桶才谈得上「之间」；`(unassigned)` 不是配置出来的身份，不进这个数。
    let identities_observed = declared_means.len() as i64;
    let mean_share_gini = gini_reading(
        GiniInput {
            point: (declared_means.len() >= 2).then(|| gini_coefficient(&declared_means)),
            groups: identities_observed,
            group_unit: "identity",
            sample_n: if identities_observed > 0 { weakest_identity_obs } else { 0 },
            sample_basis: "观察数最少的那个身份桶的观察数（最弱那条腿：基尼只与它同样可信）",
        },
        cfg.calibration_min_groups,
        min_n,
    );
    ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    let extreme = |x: Option<&(String, f64, Value)>| -> Value {
        x.map(|(id, _, block)| json!({ "identityId": id, "meanRelativeShare": block }))
            .unwrap_or(Value::Null)
    };

    let unassigned = by_identity.get(IDENTITY_UNASSIGNED);
    let mut assigned_sorted: Vec<f64> = by_identity
        .iter()
        .filter(|(k, _)| k.as_str() != IDENTITY_UNASSIGNED)
        .flat_map(|(_, a)| a.shares.iter().copied())
        .collect();
    assigned_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // 聚类数取并集：同一个世界可能同时向多个身份桶供数，直接加桶内世界数会重复计。
    let assigned_worlds: BTreeSet<&str> = by_identity
        .iter()
        .filter(|(k, _)| k.as_str() != IDENTITY_UNASSIGNED)
        .flat_map(|(_, a)| a.worlds.iter().map(String::as_str))
        .collect();

    Ok(json!({
        "metric": METRIC,
        "title": TITLE,
        "dimension": "identity",
        "status": "ok",
        "source": "worlds.assembled_json./assembly/identityAssignments × (world_members LEFT JOIN world_contributions)",
        "unit": "relativeShare：本世界内相对均分的倍率，1.0 = 恰好拿到均分",
        "worldsScanned": worlds.len() as i64,
        "worldsWithAssignments": worlds_with_assignments,
        "worldsCounted": worlds_counted,
        "worldsSingleMember": worlds_single_member,
        "worldsWithoutScore": worlds_without_score,
        "observations": observations,
        "unknownAssignmentCount": unknown_assignments,
        "identitiesObserved": identities_observed,
        "identitiesWithSufficientSample": buckets_with_sample,
        "meanShareGini": mean_share_gini,
        "lowestMeanIdentity": extreme(ranked.first()),
        "highestMeanIdentity": extreme(ranked.last()),
        // 「有身份的人」与「没拿到身份的人」的直接对照（前者若系统性更高，说明站位真的在起作用）。
        "assignedMeanRelativeShare": mean_reading(
            &assigned_sorted, assigned_worlds.len() as i64, min_n,
        ),
        // 一个没拿到站位的成员都没有 → 给 n=0 的零样本读数，**不给 null**：
        // 「没有对照组」是事实，不是「字段缺席」。
        "unassignedMeanRelativeShare": unassigned
            .map(|a| mean_reading(&a.sorted_shares(), a.worlds(), min_n))
            .unwrap_or_else(|| mean_reading(&[], 0, min_n)),
        "byIdentity": rows,
        "bucketsTruncated": buckets_truncated,
        "notes": [
            "relativeShare = (该成员贡献分 ÷ 本世界成员贡献分总和) × 本世界成员数；1.0 = 恰好均分，0.5 = 只拿到均分的一半。",
            "先按世界归一化再跨世界求均值：原始分跨世界求和测的是世界寿命不是身份失衡（跑得久的世界分自然多）。归一化后每个「世界 × 角色」观察等权。",
            "🔴 分母是 world_members 全集，无贡献分行的成员按 0 分计入 —— 与 §4 attentionGini 的交集口径不同，两个数不可互相校验。贡献分行是挣到分才落的，交集口径看不见「一分没挣到」的人，而那正是本读数要抓的。",
            "unknownAssignmentCount = 分配里有、world_members 里没有的角色（模板改版 / 成员已清理），不进任何桶，也不当成 0 分成员。",
            "(unassigned) 是对照桶不是身份：它不进 meanShareGini，但与 assignedMeanRelativeShare 并排给出。",
            "meanShareGini 的观察单位是「身份」，attentionGini 的是「角色」，两者可以一个为 0 一个很高，不矛盾也不可换算。",
            "🔴 每个读数随身带 n 与 minN：n < minN 时 status=insufficient_sample 且 value=null（点估计与区间仍给出）—— 那是「样本不足」，既不是 0 也不是空，后台应显示「样本不足（n=…）」。",
            "meanShareGini 有两层样本量：n = 身份桶数，sampleN = 观察数最少的那个桶的观察数。2 个身份 × 各 2 个观察与 2 个身份 × 各 300 个观察，n 都是 2 —— 门槛盯的是 sampleN。",
            "zeroScoreRate 带 95% Wilson 区间（小样本与 0/1 边界上比正态近似稳；正态近似在 p̂=1 时区间会塌成一个点）。n 按世界聚类，故真实区间更宽，当宽度下界读。",
            "meanRelativeShare 不给区间：观察不独立（同一世界内份额之和恒等于成员数，且同一世界同时供多个桶），iid 区间会低估宽度。改给 n / worlds（聚类数）/ sd / 中位数 / 极值，全文见 ciNotes.meanNotIid。",
            "lowestMeanIdentity / highestMeanIdentity 是**选出来的极值**，本身带选择偏差（桶越多、每桶 n 越小，极值越极端）：两者各自携带完整读数信封（含 n 与 status），先看 n 再看数。",
            "🔴 本读数不判断哪种分布更好：把戏份摊平到各身份均值全是 1.0 就没有主角了，公平与戏剧性是两个目标，这里只给事实。",
            "配额视角（quota / fillRatio / 从没被分到过的站位）在 GET /admin/world-templates/{id}/identity-pool，本读数不重复。",
            "byIdentity 按身份 id 升序，超过上限时截断并置 bucketsTruncated —— 那是针对脏数据的防线（身份 id 是自由文本，老实例可能钉着任意串），正常身份池远小于上限。identitiesObserved 与极值取自截断前的全集。",
        ],
    }))
}

// ============================================================================
// §4 维度二：戏服维（境界档 × 世界质量，跨世界对比）
// ============================================================================

/// 一个戏服桶的世界质量读数：**完全由 `super::quality` 的三个既有函数算出**，本函数只负责分桶、
/// 套上 §0.5 的样本量信封、序列化。
///
/// 🔴 三个比率各有各的分母（世界 / 拍 / 事件），所以**各带各的 n**：
/// 「这一桶有 40 个世界」不等于「阻断率压在 40 个观察上」——阻断率的分母是拍，
/// 一个世界可能贡献几十拍，也可能一拍都没跑。混着读会把最不可信的那个数当成最可信的。
fn realm_bucket_json(
    tier_id: &str,
    facts: &[WorldQualityFacts],
    first: i64,
    last: i64,
    cfg: &SloConfig,
) -> Value {
    let c = quality::completion_stats(facts);
    let b = quality::block_stats(facts);
    let e = quality::ending_distribution(facts);
    let min_n = cfg.calibration_min_n;

    // 真实结局（排掉 `(none)` / `(unfinished)` 两个特殊桶）的逐结局世界数——
    // 集中度基尼与 topEndingShare 的共同底座，与 `EndingDistribution` 内部口径逐字一致。
    let ending_counts: Vec<i64> = e
        .by_ending
        .iter()
        .filter(|(k, _)| {
            k.as_str() != quality::ENDING_NONE && k.as_str() != quality::ENDING_UNFINISHED
        })
        .map(|(_, v)| *v)
        .collect();
    let top_ending_worlds = ending_counts.iter().copied().max().unwrap_or(0);

    json!({
        "tierId": tier_id,
        // 本桶「世界数」这一层的样本量状态（拍 / 事件那两层各自另有状态）。
        "status": reading_status(c.worlds, min_n),
        "worlds": c.worlds,
        "minN": min_n,
        // 桶内世界的年龄跨度：横向比之前先看这个，年龄差得远的两桶不具可比性。
        "firstCreatedAt": first,
        "lastCreatedAt": last,
        "completion": {
            "worlds": c.worlds,
            "natural": c.natural,
            "forced": c.forced,
            "collapsed": c.collapsed,
            "unknownEnded": c.unknown_ended,
            "unfinished": c.unfinished,
            "endedWorlds": c.ended(),
            "completionRate": proportion_reading(
                c.natural, c.worlds, "world", min_n, Ci::Wilson(Some(NOTE_TRUNCATION)),
            ),
            "forcedRateAmongEnded": proportion_reading(
                c.forced + c.collapsed + c.unknown_ended, c.ended(), "world", min_n,
                Ci::Wilson(None),
            ),
        },
        "blocking": {
            "blockedTicks": b.blocked_ticks,
            "engineTicks": b.engine_ticks,
            "blockedRate": proportion_reading(
                b.blocked_ticks, b.engine_ticks, "tick", min_n, Ci::Wilson(Some(NOTE_CLUSTERED)),
            ),
            "worldsWithBlock": b.worlds_with_block,
            "eventsWithheld": b.events_withheld,
            "eventsTotal": b.events_total,
            "withheldRate": proportion_reading(
                b.events_withheld, b.events_total, "event", min_n, Ci::Wilson(Some(NOTE_CLUSTERED)),
            ),
        },
        "endings": {
            "worldsWithEnding": e.with_ending,
            "distinctEndings": e.distinct_endings,
            "topEndingShare": proportion_reading(
                top_ending_worlds, e.with_ending, "world", min_n, Ci::Omitted(NOTE_POST_SELECTION),
            ),
            "concentrationGini": gini_reading(
                GiniInput {
                    // 只有一种结局时基尼恒为 0，读起来是「很分散」，真相是「全压在这一个上」——
                    // 符号是反的，故不给点估计，交由样本量门槛拦住。
                    point: (ending_counts.len() >= 2).then(|| e.concentration_gini()),
                    groups: e.distinct_endings,
                    group_unit: "ending",
                    sample_n: e.with_ending,
                    sample_basis: "落到真实结局的世界数（结局分布的抽样波动来自世界，不来自结局池）",
                },
                cfg.calibration_min_groups,
                min_n,
            ),
            "byKind": e.by_kind,
            "byEnding": e.by_ending,
        },
    })
}

/// 戏服维读数：**按钉住的境界档分桶的世界质量三指标**。
///
/// 🔴 **为什么这一维没有「分布」**：境界档是世界发给全员的**同一件戏服**
/// （总规格 §6【拍板 3】「境界跟着副本走，不跟着角色走」）——全员统一、无池、无配额、
/// 装配层零抽样。一个世界里只有一个境界档取值，**组内分布恒为退化**（基尼恒 0）。
/// 把身份维的分布形状套上来只会得到一个恒等于 0 的假指标。
/// 它唯一能被度量的方式是**跨世界对比**：不同戏服的世界，其质量指标是否有系统性差异。
///
/// 三指标一律走 `super::quality`（`completion_stats` / `block_stats` / `ending_distribution`），
/// 与仿真试跑、世界质量回归**算的是同一个数**，本函数只负责分桶。
async fn realm_block(
    db: &AnyPool,
    worlds: &[ScannedWorld],
    cfg: &SloConfig,
) -> Result<Value, ApiError> {
    const METRIC: &str = "realmTierWorldQuality";
    const TITLE: &str = "戏服维：境界档 × 世界质量（跨世界对比）";
    const SHAPE: &str = "境界档全员统一（总规格 §6【拍板 3】）：一个世界只有一件戏服，无池、无配额、\
         装配层零抽样，因此它没有组内分布可看（组内基尼恒为 0，是个假指标）。本维的读数只能是\
         跨世界对比 —— 把窗口内的世界按钉住的戏服分桶，各桶各自报世界质量三指标。";

    // 三态第一档：这一维从未被任何模板配置过。
    if !template_declares(db, "realmTier").await? {
        return Ok(json!({
            "metric": METRIC,
            "title": TITLE,
            "dimension": "realmTier",
            "status": "entry_not_open",
            "value": Value::Null,
            "shapeRationale": SHAPE,
            "notes": [
                "全平台没有任何模板声明过 skeletonJson.realmTier —— 戏服维在这个平台上从未开过工。",
                "🔴 这是「没测过」不是「各戏服没差别」：后台必须显示 —，显示 0 即为误报。",
                "开工方式：建模板时录入 skeletonJson.realmTier（POST /admin/world-templates）。",
            ],
        }));
    }

    // 三态第二档：配置过，但窗口内一个世界都没有 → 零样本。
    if worlds.is_empty() {
        return Ok(json!({
            "metric": METRIC,
            "title": TITLE,
            "dimension": "realmTier",
            "status": "no_data_in_window",
            "value": Value::Null,
            "shapeRationale": SHAPE,
            "worldsScanned": 0,
            "notes": [
                "窗口内没有开出任何世界 —— 零样本，不是「各戏服质量一样」。",
                "🔴 后台必须显示 —，显示 0 即为误报。",
            ],
        }));
    }

    // 世界事实批量取回（**无 N+1**：`IN (…)` 分批的三条聚合查询，口径与单世界版逐字一致）。
    let statuses: BTreeMap<String, String> =
        worlds.iter().map(|w| (w.id.clone(), w.status.clone())).collect();
    let facts = quality::collect_world_facts_bulk(db, &statuses).await?;
    let facts_by_id: BTreeMap<&str, &WorldQualityFacts> =
        facts.iter().map(|f| (f.world_id.as_str(), f)).collect();

    // 分桶：钉住的戏服 id，未钉住的进 `(none)` 对照桶（不丢弃 —— 那是唯一的参照系）。
    let mut buckets: BTreeMap<String, (Vec<WorldQualityFacts>, i64, i64)> = BTreeMap::new();
    let mut worlds_assembled = 0i64;
    for w in worlds {
        if w.assembled {
            worlds_assembled += 1;
        }
        let key = w.realm_tier.clone().unwrap_or_else(|| REALM_NONE.to_string());
        let Some(f) = facts_by_id.get(w.id.as_str()) else { continue };
        let slot = buckets.entry(key).or_insert_with(|| (Vec::new(), i64::MAX, i64::MIN));
        slot.0.push((*f).clone());
        slot.1 = slot.1.min(w.created_at);
        slot.2 = slot.2.max(w.created_at);
    }

    let without_tier =
        buckets.get(REALM_NONE).map(|(v, _, _)| v.len() as i64).unwrap_or(0);
    let tiers_observed =
        buckets.keys().filter(|k| k.as_str() != REALM_NONE).count() as i64;

    let mut rows: Vec<Value> =
        buckets.iter().map(|(k, (f, lo, hi))| realm_bucket_json(k, f, *lo, *hi, cfg)).collect();
    let buckets_truncated = rows.len() > BUCKET_MAX;
    // 「够样本的桶」= 世界数达到 minN 的桶。跨戏服对比至少要两个这样的桶才谈得上对比 ——
    // 一个 40 世界的桶与一个 2 世界的桶并排，看起来像对比，实际上只有一边有数。
    let buckets_with_sample = rows
        .iter()
        .filter(|r| r["tierId"] != REALM_NONE && r["status"] == STATUS_OK)
        .count() as i64;
    rows.truncate(BUCKET_MAX);

    Ok(json!({
        "metric": METRIC,
        "title": TITLE,
        "dimension": "realmTier",
        "status": "ok",
        "source": "worlds.assembled_json./assembly/realmTier.id 分桶 × slo::quality 的完读率 / 阻断率 / 结局分布",
        "shapeRationale": SHAPE,
        "worldsScanned": worlds.len() as i64,
        "worldsAssembled": worlds_assembled,
        "worldsWithoutTier": without_tier,
        "tiersObserved": tiers_observed,
        "tiersWithSufficientSample": buckets_with_sample,
        "byRealmTier": rows,
        "bucketsTruncated": buckets_truncated,
        "notes": [
            "三指标全部复用 slo::quality（completion_stats / block_stats / ending_distribution），与仿真试跑、世界质量回归算的是同一个数；本读数只负责分桶。",
            "🔴 每个比率随身带自己的 n 与 minN：完读率的 n 是**世界数**、阻断率是**拍数**、扣留率是**事件数**，三者不是一个数，不可互相代读。n < minN → status=insufficient_sample 且 value=null（点估计与区间照给）。",
            "比率一律带 95% Wilson 区间（小样本与 0/1 边界上比正态近似稳）。拍 / 事件按世界聚类，真实区间更宽，当宽度下界读；完读率的区间另不覆盖 cohort 截断偏差（那是系统偏差，样本量再大也不消失）。",
            "topEndingShare 不给区间：它是「先选最大再算占比」，post-selection 的朴素区间会偏乐观。concentrationGini 不给区间的理由见 ciNotes.giniNoInterval（bootstrap 违反确定性契约；jackknife 重采样的是结局池不是抽样波动的来源）。",
            "tiersWithSufficientSample = 世界数达到 minN 的非对照桶数；它 < 2 时「跨戏服对比」这件事本身还不成立 —— 一个 40 世界的桶与一个 2 世界的桶并排，看起来像对比，实际上只有一边有数。",
            "(none) 是对照桶不是缺数据：没钉住戏服的世界是「配了戏服的世界」唯一的参照系，不得丢弃。模板声明 realmTier 之前装配的实例本就不会回溯补写。",
            "tiersObserved < 2 时本读数只是那一桶的事实，不构成跨戏服对比 —— 「对比」至少要有两个非空桶。",
            "窗口口径 = worlds.created_at 落窗（cohort：窗口内开出的这一批世界），与 §4 attentionGini 的「有贡献分更新的世界」不是同一批，两处数字不可互相校验。",
            "completionRate 的分母含未收尾世界（quality.rs 的口径），故近期窗口天然偏低；本读数的用途是横向对比 —— 同一窗口内各桶承受同样的截断偏差。",
            "各桶的 firstCreatedAt / lastCreatedAt / unfinished 一并给出：两桶世界年龄差得远时横向比也会失真，这个偏差要读的人自己看得见。",
            "🔴 不给综合评分、不给哪件戏服更好的判语：完读率高而结局全压在一个桶，与完读率低但结局发散，是两种不同的结果，不是一个分数的高低。",
            "byRealmTier 按戏服 id 升序，超过上限时截断并置 bucketsTruncated —— 脏数据防线（老实例可能钉着任意档 id），正常一个窗口内的档数远小于上限。",
        ],
    }))
}

// ============================================================================
// §5 对外入口
// ============================================================================

/// 校准维度读数总装（`narrative_slo` 的 `calibration` 段）。**纯只读**。
///
/// 一次 `worlds` 分页扫描喂两维：身份维（组内分布）+ 戏服维（跨世界对比）。
/// 扫描世界数超过 `cfg.calibration_world_cap` → 两维一并 `skipped_too_large`
/// （宁可显示「数据量超限」，也不给残缺数）。
pub(crate) async fn calibration_readings(
    db: &AnyPool,
    cfg: &SloConfig,
) -> Result<Value, ApiError> {
    let cap = cfg.calibration_world_cap;
    let Some(worlds) = scan_cohort(db, cfg).await? else {
        return Ok(json!({
            "status": "skipped_too_large",
            "windowDays": cfg.days,
            "windowStartMs": cfg.window_start,
            "windowEndMs": cfg.window_end,
            "worldScanCap": cap,
            "dimensions": {
                "identityShareBalance": skipped_too_large(
                    "identityShareBalance", "身份维：身份分配 × 戏份分布", cap,
                ),
                "realmTierWorldQuality": skipped_too_large(
                    "realmTierWorldQuality", "戏服维：境界档 × 世界质量（跨世界对比）", cap,
                ),
            },
            "notes": ["窗口内开出的世界数超过 worldScanCap；请缩小 ?sloDays= 窗口，或调高 MUSE_SLO_CALIBRATION_WORLD_CAP。"],
        }));
    };

    let identity = identity_block(db, &worlds, cfg).await?;
    let realm = realm_block(db, &worlds, cfg).await?;

    Ok(json!({
        "status": "ok",
        "windowDays": cfg.days,
        "windowStartMs": cfg.window_start,
        "windowEndMs": cfg.window_end,
        "windowBasis": "worlds.created_at（cohort：窗口内开出的这一批世界，两维共用）",
        "worldsScanned": worlds.len() as i64,
        "worldScanCap": cap,
        // 🔴 样本量约定随数一起下发（同 shapeRationale 的理由：**数会被复制走，注释不会**）。
        "sampleFloor": sample_floor(cfg),
        // 区间说明全文，按短码索引。读数里只带 `ciNoteRef`，全文在这里给一次 ——
        // 逐条重复会让被轮询的端点凭空胖上百 KB（50 桶 × 每桶 4 条 × 每条数百字）。
        "ciNotes": ci_notes(),
        "dimensions": {
            "identityShareBalance": identity,
            "realmTierWorldQuality": realm,
        },
        "notes": [
            "🔴 只读聚合：本段不写任何库、不回灌引擎。这些数永远不进 narrative_state_json，也永远不作为引擎判定输入 —— 同 world_contributions 独立建表的理由（回灌即违反 §0.1 平权红线）。",
            "🔴 四态：entry_not_open（这一维从未被任何模板配置过）/ no_data_in_window（配过但零样本）/ insufficient_sample（有样本但 n < minN）/ ok（真数，可以是 0）。前两者显示 —；第三者显示「样本不足（n=…）」，🔴 不许显示成 0 或 —。",
            "🔴 每个读数是一个信封：value（可据此调参的读数，样本不足时为 null）/ pointEstimate（原始算术，永远给）/ n / minN / ci95 / ciNote。n 与 value 在同一个对象里是刻意的 —— 取值必须穿过信封，「拿到比例却不知道压在几个观察上」在结构上不可能发生。",
            "🔴 有区间不等于有判语：本段不给显著性布尔、不给「差异是否成立」的结论。给区间，让人自己看 —— 「差 0.1 算不算差」取决于运营在权衡什么，不取决于 p 值。",
            "🔴 不给综合评分：校准是多目标的（公平 vs 戏剧性），一个「越高越好」的分会诱导去优化那个数字本身。这里只给分维度的事实，不给判语。",
            "两维形状刻意不同构：身份各不相同 → 看组内分布；境界档全员统一 → 只能看跨世界对比。把后者套成前者会得到恒为 0 的假指标。",
            "🔴 读数建成 ≠ 校准闭环已验证（§0.3 七档）：闭环成立要等运营真的用它调过参、并在下一批世界上看到因果。当前状态最高只到 Implemented。",
        ],
    }))
}

/// `?slo=0` 时的占位（形状与 `super::skipped_by_request` 同源，三态互不混淆）。
pub(crate) fn skipped_by_request() -> Value {
    json!({
        "status": "skipped_by_request",
        "dimensions": {},
        "notes": ["调用方传了 ?slo=0，本次未计算校准维度读数（高频轮询减负开关）。去掉该参数即恢复。"],
    })
}

#[cfg(test)]
mod tests;
