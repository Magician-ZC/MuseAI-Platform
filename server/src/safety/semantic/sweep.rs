//! §15 第 3 层的**补偿轮询**：把「从未被复核过的拍」从数据里重新算出来，补投回队列。
//!
//! 闭合的是 `safety::semantic` 模块头挂了一整批的那条如实登记：
//!
//! > ⚠️ `MemQueue` 是进程内内存队列，**不持久**。进程重启时在飞的复核任务会丢，
//! > 那一拍的事件就停留在 `approved` 且再无人复核。……接 Redis 实现或加一条
//! > 「扫尾未复核拍」的补偿轮询即可闭合。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 为什么是对账，不是持久队列
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 两条路都写在那条登记里，选轮询是因为**它们闭合的不是同一个洞**：
//!
//! | 丢失路径 | 持久队列（Redis） | 补偿轮询 |
//! |---|---|---|
//! | 入队后进程重启 | ✅ | ✅ |
//! | worker 取出后崩溃（未 ack） | 要额外做 ack/可见性超时 | ✅（没有终局行 = 没做完） |
//! | **压根没入队**（开关当时关着、后来开了） | ❌ | ✅ |
//! | **压根没入队**（tick 走 blocked / cas_conflict 分支，代码根本没到那一行） | ❌ | ✅ |
//! | topic 名写错 / 序列化失败被 `push_json` 静默吞掉 | ❌ | ✅ |
//!
//! 持久队列保证的是「**已经入队**的任务不丢」。而这条链上更常见的失败是「**根本没入队**」——
//! 队列对此一无所知，因为待办从来没进过它的视野。轮询不问「任务在哪」，它问
//! 「**这一拍到底被复核过没有**」，答案从 `world_ticks ⋈ safety_recheck_runs` 现算。
//!
//! 🔵 顺带闭合的那个洞值得单独说：`runtime` 里 blocked（预算/审核阻断收尾）与 cas_conflict
//! 两条路径也把 `world_ticks.status` 写成 `'done'`，但它们**不经过** `commit_tick` 末尾的
//! `enqueue_after_commit`。那些拍若带着 approved 事件，在本模块之前**从未被第 3 层看过一眼**，
//! 而这件事此前没有任何地方登记过——它是写这次对账查询时才被查出来的。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 无状态：轮询不记自己的账
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 本模块**不建表、不留游标、不维护计数器**。缺口数每次从两张既有表现算。
//!
//! 理由不是省事，是**可观测性的方向**：如果轮询维护自己的计数器，那它一旦死掉
//! （panic 后 task 消失、开关被误关、部署漏了这个进程），那些数字会**冻结在健康的样子**，
//! 而真实缺口在背后继续变大。现算的缺口数则是「轮询死了也照样变大」的量——
//! 运营面上读到的是**病情**，不是病历。
//!
//! 同理，[`gap_report`] 不依赖轮询是否跑过：开关关着时它照样报出真实缺口，
//! 于是「要不要开这个开关」有据可依，而不是先开了才知道有没有用。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 判据：什么叫「这一拍还没被复核」
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 需要一个能同时表达「从未入队」「入队但没跑完」「跑完了」的判据。`safety_recheck_runs`
//! 的 `outcome` 正好就是它——一次尝试一行，而**尝试链的终局**只有三种：
//!
//! | outcome | 终局？ | 含义 |
//! |---|---|---|
//! | `done` | ✅ | 本次尝试全部拿到裁决 |
//! | `failed_closed` | ✅ | 重试预算耗尽，已按 fail-closed 收紧 |
//! | `skipped` | ✅ | 看过了，这一拍没有可送审的内容 |
//! | `retry_scheduled` | ❌ | 有报错、已排下一次——**链条还没走完** |
//! | （一行都没有） | ❌ | 从未跑过 |
//!
//! ⚠️ `skipped` 此前**不落库**（`run_recheck` 的两处早返回都在 `persist_run` 之前）。
//! 那对轮询是致命的：一拍若有 approved 事件但正文全空，复核会一直 skip、一直不留行，
//! 于是**每一轮扫描都把它当成缺口重投一次，永不收敛**。本批次因此把「看过了但无候选」
//! 这一种 skip 补成落库（**开关关闭那一种仍然不落库**——那条路径必须与接线前逐字节相同，
//! 由 `disabled_is_byte_identical_to_before_wiring` 守着）。
//!
//! 「看了，没东西可看」本来就是一个值得入账的事实。它此前不入账，正是因为没人从数据侧
//! 反过来问过「这一拍查过没有」。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 续号，不是重投：`attempt = MAX(attempt) + 1`
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 补投的任务**必须接着已有的尝试号往下走**，不能一律从 1 开始。`safety_recheck_runs`
//! 的唯一键是 `(world_id, tick_no, attempt)`，写入侧是 `ON CONFLICT DO NOTHING`——
//! 若补投的任务也用 `attempt = 1`，而这一拍此前已经有过一行 `(T, 1, retry_scheduled)`，
//! 那么这次复核**跑完了也写不进台账**（被冲突吞掉），于是：
//!
//! 1. 台账里永远没有终局行 → 下一轮扫描又把它当缺口 → 又补投 → **每 5 分钟重烧一遍整拍的 token**；
//! 2. 而运营面上什么都看不出来：`runs` 不涨，缺口数不降。
//!
//! 这是本模块最容易写错、且写错了**只在生产上以账单的形式显形**的一处。续号同时也是
//! 语义上正确的：补投确实是这条尝试链的下一次尝试，重试预算（`MUSE_SAFETY_L3_MAX_ATTEMPTS`）
//! 应当继续消耗，而不是被重置成无限。
//!
//! ⚠️ **补偿路径比正常路径粗**：正常重试只重查上次报错的那几条（`retry_ids`），
//! 而丢失的任务把那份名单一起带走了，补投只能重查**整拍**（仍受
//! `MUSE_SAFETY_L3_MAX_EVENTS_PER_TICK` 封顶）。这是「不建持久状态」的直接代价，如实记在这里。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 不扫全表：三道边界
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `world_ticks` 只增不减。一个「每 5 分钟全表扫一遍只增不减的表」的后台任务，
//! 上线当天没人看得出来，半年后是数据库上最贵的那条查询。三道边界：
//!
//! - **上界（grace）**：只看 `finished_at < now - grace`。刚落定的拍留给正常路径自己跑完，
//!   否则轮询会与在飞的任务重复送审。默认 5 分钟 ≫ 最坏一条尝试链的耗时
//!   （4 次尝试 × 5s 超时 + 2+4+8s 退避 ≈ 34s）。
//! - **下界（lookback）**：只看 `finished_at >= now - lookback`（默认 24h）。
//!   🔴 **这是一条真实的覆盖上限**：进程连续挂掉超过 lookback，那段时间的拍就永远补不回来了。
//!   本模块不假装没有这条线——[`gap_report`] 会单独报出**刚刚掉出窗口**的那一段还剩多少拍
//!   （`justOutsideWindow`），它 >0 就是「lookback 配短了」的直接证据。
//! - **批量（batch）**：一轮最多补投 N 拍（默认 200）。够不够看 `truncated`。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! ⚠️ 已知边界（不修，如实登记）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 1. **单实例**。多实例同时开着这个开关会重复扫、重复补投（重复的那次多半被
//!    `attempt` 唯一键挡在台账外，但 provider 调用是真烧的）。与 STARTUP §8.1 里
//!    `world_events.sequence` 那条同类：属**发布纪律**，代码解决不了。
//! 2. **与人审放宽的交叉**：一条被人审 reinstate 回 `approved` 的事件，若它所在的拍
//!    恰好有一条**卡住的重试链**（`retry_scheduled` 且已超 grace），补投会把它重新送审、
//!    可能再次收紧。方向是 fail-closed（本层的安全侧），后果是人审要再点一次，
//!    不是内容外泄。要触发它需要同时满足「provider 报过错 + 进程在重试间隙死掉 +
//!    人审恰好动了这一拍 + 全部发生在 lookback 窗口内」，故记录而不加防。
//! 3. 补投**不改变** `world_events` 的任何写入路径——本模块对那张表**只有 SELECT**，
//!    由红线用例 `red_line_world_events_has_one_ratchet_and_one_guarded_relax`（全仓盘点）
//!    与本模块自己的 `red_line_sweep_never_writes_world_events` 一并守着。

use std::collections::HashMap;

use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::db::now_ms;
use crate::error::ApiError;

use super::RecheckJob;

// ═══════════════════════════════════════════════════════════════════════════
// 开关与参数（§0.1 默认关闭 · §0.2 全部参数化）
// ═══════════════════════════════════════════════════════════════════════════

/// 补偿轮询的运行时开关（**全局档**——它是一个跨世界的进程级循环，不按世界灰度；
/// 每一拍要不要真的补投，仍然逐个世界过第 3 层自己的开关）。
///
/// ℹ️ 名字用 `MUSE_SAFETY_RECHECK_SWEEP` 而不是 `MUSE_SAFETY_L3_SWEEP`，是照本模块既有的
/// 命名分工：**开关**会出现在运营后台的登记表里，名字要自解释；`MUSE_SAFETY_L3_*` 是
/// 只在部署侧改的**调参**前缀。
pub(crate) const ENV_SWEEP: &str = "MUSE_SAFETY_RECHECK_SWEEP";

/// 默认 = **关闭**（§0.1）。理由与第 3 层本身同源，但**多一条**：
/// 轮询是唯一一处会「凭数据自发烧 token」的路径——它不需要有人推进世界就能发起送审。
/// 这类东西默认开着，等于让一次合并把成本曲线抬起来而没人按过开关。
const DEFAULT_SWEEP_ENABLED: bool = false;

/// 🔴 编译期钉死默认值的两个事实源（范式同第 3 层自己的 `DEFAULT_SEMANTIC_RECHECK_ENABLED`）。
const _: () = assert!(
    crate::flags::declared_default(ENV_SWEEP) == DEFAULT_SWEEP_ENABLED,
    "flags::KNOWN_FLAGS 中 MUSE_SAFETY_RECHECK_SWEEP 的默认值必须与 DEFAULT_SWEEP_ENABLED 一致"
);

/// 两轮扫描的间隔。
const ENV_INTERVAL_MS: &str = "MUSE_SAFETY_L3_SWEEP_INTERVAL_MS";
const DEFAULT_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// 宽限期：拍落定不足这么久就不扫（留给正常路径跑完，避免与在飞任务重复送审）。
/// 同一个值也用来判「这一拍最近有没有人在动它」（台账里有 grace 内的行 = 有）。
const ENV_GRACE_MS: &str = "MUSE_SAFETY_L3_SWEEP_GRACE_MS";
const DEFAULT_GRACE_MS: i64 = 5 * 60 * 1000;

/// 回看窗口。🔴 这是覆盖上限，不是性能旋钮——见模块头「三道边界」。
const ENV_LOOKBACK_MS: &str = "MUSE_SAFETY_L3_SWEEP_LOOKBACK_MS";
const DEFAULT_LOOKBACK_MS: i64 = 24 * 60 * 60 * 1000;

/// 一轮最多补投多少拍。
const ENV_BATCH: &str = "MUSE_SAFETY_L3_SWEEP_BATCH";
const DEFAULT_BATCH: i64 = 200;

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(default)
}

pub(crate) fn interval_ms() -> i64 {
    env_i64(ENV_INTERVAL_MS, DEFAULT_INTERVAL_MS)
}
pub(crate) fn grace_ms() -> i64 {
    env_i64(ENV_GRACE_MS, DEFAULT_GRACE_MS)
}
pub(crate) fn lookback_ms() -> i64 {
    env_i64(ENV_LOOKBACK_MS, DEFAULT_LOOKBACK_MS)
}
pub(crate) fn batch() -> i64 {
    env_i64(ENV_BATCH, DEFAULT_BATCH)
}

/// 轮询本身是否开着（全局档）。fail-closed 由 `flags::is_enabled` 自带。
pub(crate) async fn sweep_enabled(db: &AnyPool) -> bool {
    crate::flags::is_enabled(db, ENV_SWEEP, crate::flags::FlagCtx::global()).await
}

// ═══════════════════════════════════════════════════════════════════════════
// 对账查询（本模块对 `world_events` / `world_ticks` **只有 SELECT**）
// ═══════════════════════════════════════════════════════════════════════════

/// 终局 outcome 白名单。写死在 SQL 文本里而不是拼进来：这三个值决定「什么叫做完了」，
/// 拼错一个的后果是**永不收敛的重复送审**，必须一眼看得见。
const TERMINAL_OUTCOMES: &str = "('done', 'failed_closed', 'skipped')";

/// 一个待补投的坐标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unresolved {
    pub(crate) world_id: String,
    pub(crate) tick_no: i64,
    /// 台账里这一拍已有的最大尝试号（没有行则为 0）。补投号 = 它 + 1，见模块头「续号」。
    pub(crate) last_attempt: i64,
}

/// 对账主查询：**落定了、有 approved 事件、却没有终局复核行**的拍。
///
/// 时间窗是半开区间 `[from_ms, to_ms)`，两端都由调用方给（轮询给
/// `[now-lookback, now-grace)`，缺口报告另给一段用于探测「刚掉出窗口」）。
///
/// 三个条件各挡一类：
/// - `status = 'done' AND finished_at ∈ [from, to)` —— 只看落定且够久的拍；
/// - `EXISTS(approved 事件)` —— 没有 approved 事件的拍无事可做（全被第 2 层拦了 / 空拍），
///   不进缺口数，否则缺口会被一堆无害的空拍灌满而失去指示意义；
/// - `NOT EXISTS(终局行 OR grace 内的行)` —— 后半段挡的是**正在被重试的拍**：
///   台账里有 grace 以内的行 = 有人正在动它，轮询不插手。
async fn find_unresolved(
    db: &AnyPool,
    from_ms: i64,
    to_ms: i64,
    recent_floor_ms: i64,
    limit: i64,
) -> Result<Vec<Unresolved>, ApiError> {
    // 占位符按 **SQL 文本顺序**发号，bind 顺序与之一一对应（$3 与 $2 恰好同值，
    // 但语义不同——一个是「拍落定得够久」，一个是「台账里最近没人动过」——故不复用编号）。
    let sql = format!(
        "SELECT t.world_id AS world_id, t.tick_no AS tick_no, \
         (SELECT CAST(COALESCE(MAX(r2.attempt), 0) AS BIGINT) FROM safety_recheck_runs r2 \
          WHERE r2.world_id = t.world_id AND r2.tick_no = t.tick_no) AS last_attempt \
         FROM world_ticks t \
         WHERE t.status = 'done' AND t.finished_at >= $1 AND t.finished_at < $2 \
         AND EXISTS (SELECT 1 FROM world_events e \
                     WHERE e.world_id = t.world_id AND e.tick_no = t.tick_no \
                     AND e.moderation = 'approved') \
         AND NOT EXISTS (SELECT 1 FROM safety_recheck_runs r \
                         WHERE r.world_id = t.world_id AND r.tick_no = t.tick_no \
                         AND (r.outcome IN {TERMINAL_OUTCOMES} OR r.created_at >= $3)) \
         ORDER BY t.finished_at ASC, t.id ASC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(from_ms)
        .bind(to_ms)
        .bind(recent_floor_ms)
        .bind(limit)
        .fetch_all(db)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(Unresolved {
            world_id: row.try_get("world_id")?,
            tick_no: row.try_get("tick_no")?,
            last_attempt: row.try_get("last_attempt").unwrap_or(0),
        });
    }
    Ok(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// 补投
// ═══════════════════════════════════════════════════════════════════════════

/// 一轮扫描的结果（只回给日志与用例；**不落库**，见模块头「无状态」）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SweepReport {
    /// 对账查出的待补投拍数。
    pub(crate) found: i64,
    /// 真正补投进队列的拍数。
    pub(crate) requeued: i64,
    /// 因该世界的第 3 层开关是关的而跳过。
    pub(crate) skipped_flag_off: i64,
    /// 因本进程刚补投过（仍在 grace 内）而跳过。
    pub(crate) skipped_in_flight: i64,
    /// 命中批量上限 —— 还有更多没扫到，下一轮继续。
    pub(crate) truncated: bool,
}

/// 本进程刚补投过什么：`(world_id, tick_no) → 补投时刻`。
///
/// 为什么需要它：补投后到台账落行之间有一段空窗（任务还在队列里排队），
/// 这段时间内对账查询仍会把这一拍算成缺口。若下一轮扫描恰好落在这段空窗里，
/// 就会重复补投同一拍——**重复的是真实的 provider 调用**。
///
/// 🔴 刻意**不做成进程级 static**：那样测试进程里所有并发用例会共用一张表、互相干扰，
/// 而这类干扰的表现是「单跑绿、全跑偶发红」，最难查。它是轮询循环的局部状态，
/// 由调用方持有并传入。
#[derive(Debug, Default)]
pub(crate) struct InFlight(HashMap<(String, i64), i64>);

impl InFlight {
    /// 丢掉过期条目（超过 grace 的：那时台账要么已落行，要么这一拍确实又该补投了）。
    fn prune(&mut self, now_ms: i64, grace_ms: i64) {
        self.0.retain(|_, at| now_ms - *at < grace_ms);
    }

    /// 登记一次补投；已在飞则返回 `false`。
    fn admit(&mut self, world_id: &str, tick_no: i64, now_ms: i64) -> bool {
        let key = (world_id.to_string(), tick_no);
        if self.0.contains_key(&key) {
            return false;
        }
        self.0.insert(key, now_ms);
        true
    }
}

/// 跑一轮对账 + 补投。
///
/// 🔴 **不开事务**（同第 3 层主流程：本模块的调用里夹着队列写入，且单连接池下
/// 事务持有唯一连接期间任何再借连接的操作直接 `PoolTimedOut`）。
pub(crate) async fn sweep_once(
    state: &AppState,
    in_flight: &mut InFlight,
) -> Result<SweepReport, ApiError> {
    let now = now_ms();
    let (grace, lookback, limit) = (grace_ms(), lookback_ms(), batch());
    in_flight.prune(now, grace);

    let found = find_unresolved(&state.db, now - lookback, now - grace, now - grace, limit).await?;
    let mut r = SweepReport {
        found: found.len() as i64,
        truncated: found.len() as i64 >= limit,
        ..Default::default()
    };

    for u in &found {
        // 逐个世界过第 3 层自己的开关：轮询开着 ≠ 每个世界都要补。
        // （放在循环里而不是先去重世界——一轮最多 `batch` 条，
        //  而 `flags::is_enabled` 本就带缓存，收益不值得多一层结构。）
        if !super::enabled(&state.db, &u.world_id).await {
            r.skipped_flag_off += 1;
            continue;
        }
        if !in_flight.admit(&u.world_id, u.tick_no, now) {
            r.skipped_in_flight += 1;
            continue;
        }
        // 🔴 续号，不是从 1 重投——理由见模块头（写错只会在账单上显形）。
        let job = RecheckJob {
            world_id: u.world_id.clone(),
            tick_no: u.tick_no,
            attempt: u.last_attempt + 1,
            // 补偿路径比正常重试粗：丢失的任务把 `retry_ids` 一起带走了，只能重查整拍。
            retry_ids: Vec::new(),
        };
        crate::queue::push_json(&*state.queue, super::TOPIC, &job, now).await;
        r.requeued += 1;
    }

    if r.requeued > 0 || r.truncated {
        tracing::warn!(
            found = r.found,
            requeued = r.requeued,
            skipped_flag_off = r.skipped_flag_off,
            skipped_in_flight = r.skipped_in_flight,
            truncated = r.truncated,
            "§15 第 3 层补偿轮询补投了未复核拍（正常路径丢过任务，或那些拍从未入过队）"
        );
    }
    Ok(r)
}

/// 启动补偿轮询循环（`main` 调用，单任务）。
///
/// 🔴 **先扫一轮再睡**：这条循环存在的首要理由就是进程重启后把丢掉的任务捡回来，
/// 「启动后先睡 5 分钟」会把它最该起作用的那一刻睡过去。
pub fn spawn_sweeper(state: AppState) {
    tokio::spawn(async move {
        let mut in_flight = InFlight::default();
        loop {
            if sweep_enabled(&state.db).await {
                if let Err(e) = sweep_once(&state, &mut in_flight).await {
                    // 轮询是旁路：扫不动就下一轮再来，绝不把进程带走。
                    tracing::warn!(error = %e, "§15 第 3 层补偿轮询本轮失败（下一轮重试）");
                }
            }
            // 间隔在循环里现读：改 env 重启即可生效，不需要改代码。
            tokio::time::sleep(std::time::Duration::from_millis(interval_ms().max(1) as u64)).await;
        }
    });
    tracing::info!(
        flag = ENV_SWEEP,
        "§15 第 3 层补偿轮询已启动（默认关闭；开着时按 world_ticks ⋈ safety_recheck_runs 对账补投）"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 运营面：缺口报告（进 `GET /api/admin/safety/recheck` 的 `durability` 块）
// ═══════════════════════════════════════════════════════════════════════════

/// 缺口报告。**不读轮询的任何自有状态**——全部从 `world_ticks ⋈ safety_recheck_runs` 现算，
/// 于是轮询关着、死了、从没部署过，这些数字照样是真的（见模块头「无状态」）。
///
/// 三个数各回答一个问题：
///
/// | 字段 | 问题 |
/// |---|---|
/// | `unresolvedInWindow` | 补偿窗口内还有多少拍没被复核过（轮询开着时它应当趋近 0） |
/// | `enabledWorldTicks` | 其中有多少落在**第 3 层开着**的世界上 —— 只有这部分是真缺口 |
/// | `justOutsideWindow` | 刚掉出窗口那一段还剩多少 —— 🔴 >0 = `lookback` 配短了，那些拍**永远补不回来** |
///
/// `unresolvedInWindow - enabledWorldTicks` 不是缺口，是「那些世界压根没开第 3 层」。
/// 两个数分开给，是因为把它们并成一个会让运营在开关全局关闭时看到一个巨大的、
/// 无从下手的数字，然后学会忽略它。
pub(crate) async fn gap_report(state: &AppState) -> Value {
    let now = now_ms();
    let (grace, lookback, limit) = (grace_ms(), lookback_ms(), batch());

    // 窗口内：与轮询用的是**同一个查询、同一组边界**——运营面上看到的缺口，
    // 就是轮询下一轮会去处理的那批，不存在两套口径。
    let in_window =
        find_unresolved(&state.db, now - lookback, now - grace, now - grace, limit).await;
    // 刚掉出窗口的那一段（宽度同 lookback）。用来证明 lookback 够不够，见字段表。
    let outside =
        find_unresolved(&state.db, now - 2 * lookback, now - lookback, now - grace, limit).await;

    let (rows, err) = match in_window {
        Ok(v) => (v, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };

    // 逐个世界解析第 3 层开关，得出「真缺口」。世界先去重：一个世界丢几十拍是常态，
    // 不去重会把同一次开关解析做几十遍。
    let mut per_world: HashMap<&str, bool> = HashMap::new();
    for u in &rows {
        if !per_world.contains_key(u.world_id.as_str()) {
            let on = super::enabled(&state.db, &u.world_id).await;
            per_world.insert(u.world_id.as_str(), on);
        }
    }
    let enabled_ticks =
        rows.iter().filter(|u| *per_world.get(u.world_id.as_str()).unwrap_or(&false)).count() as i64;

    let truncated = rows.len() as i64 >= limit;
    json!({
        "flag": ENV_SWEEP,
        "sweepEnabled": sweep_enabled(&state.db).await,
        "unresolvedInWindow": rows.len() as i64,
        "enabledWorldTicks": enabled_ticks,
        "justOutsideWindow": outside.as_ref().map(|v| v.len() as i64).unwrap_or(-1),
        "truncated": truncated,
        "window": {
            "graceMs": grace,
            "lookbackMs": lookback,
            "batch": limit,
            "envs": [ENV_INTERVAL_MS, ENV_GRACE_MS, ENV_LOOKBACK_MS, ENV_BATCH],
            "intervalMs": interval_ms(),
        },
        "error": err,
        "meaning": "「落定 ≥graceMs、带 approved 事件、却没有终局复核行（done/failed_closed/skipped）的拍」。\
                    数字从 world_ticks ⋈ safety_recheck_runs 现算，**不依赖轮询自己的记账**——\
                    轮询关着、挂了、没部署，这里照样报出真实缺口。",
        "honesty": [
            "🔴 unresolvedInWindow 里只有 enabledWorldTicks 这部分是真缺口；其余是「那个世界压根没开第 3 层」，\
             不开就不查，本来就不该有复核行。",
            "🔴 justOutsideWindow > 0 = MUSE_SAFETY_L3_SWEEP_LOOKBACK_MS 配短了：\
             那些拍已经掉出补偿窗口，**永远补不回来**。补偿轮询有覆盖上限，本页不掩盖它。",
            "⚠️ truncated = true 表示本次只数到批量上限（batch）就停了，真实数字更大。",
            "⚠️ 补偿路径比正常重试粗：它重查**整拍**，而正常重试只重查上次报错的那几条\
             （那份名单随丢失的任务一起没了）。故补投的送审量可能高于正常路径。",
            "⚠️ 单实例假设：多实例同时开着本开关会重复扫、重复补投（重复的调用是真烧的）。\
             属发布纪律，代码解决不了 —— 同 world_events.sequence 那条。",
        ],
    })
}
