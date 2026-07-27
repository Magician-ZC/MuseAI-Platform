//! if 线推进的**对账式补偿**（migration `0052`）——`docs/VALIDATION.md` §3.2 遗留栏第 1b 条。
//!
//! # 它补的到底是什么
//!
//! 0050 把推进改成异步之后，`queue::MemQueue` 是**进程内内存队列、不持久**。进程重启会把
//! 在飞的任务带走，而 `advance_requested_at` 已经写下。0050 为此在请求层 CAS 上带了「陈旧线」
//! 一支，于是玩家等 10 分钟后能再点一次——**那是安全底线，不是补偿**：
//!
//! > 陈旧线只是让玩家能再点一次，不是把丢掉的那次补上。
//!
//! 本模块补的就是后半句：玩家**没再点**，也把丢掉的那一拍推完。
//!
//! # 判据为什么是干净的
//!
//! 0050 已经把出口收严了——`run_advance_job` 的**任何出口都清掉 `advance_requested_at`**
//! （成功清、失败清并写 `last_error`）。于是：
//!
//! ```text
//! advance_requested_at != 0  且  已过陈旧线   ⇒   这次推进确实丢了，且没有任何人会重试它
//! ```
//!
//! 三种成因（进程重启 / 读行失败早退 / 收尾写库失败）都归到这同一条判据，动作也相同，
//! 故**不需要**区分。这比 §15 第 3 层那个 sweep 简单得多——那边要 `world_ticks ⋈
//! safety_recheck_runs` 对账，这边一列就够，因为「在飞」这个状态本身就是持久的。
//!
//! # 🔴 补投必须有封顶，否则它比原问题更糟
//!
//! 补投会**真的调模型**（付费内容、真 token）。若 worker 每次都在清标记之前就死掉
//! （panic / OOM / 被 kill），「查到就补投」会变成一个**无限烧钱的循环**，而且每一轮
//! 看起来都很正常。故 `0052` 加了 `advance_sweep_count`，[`ENV_MAX_REDELIVERIES`] 封顶。
//!
//! §15 第 3 层不需要这一列，是因为它那边补投用 `attempt = MAX+1`，既有的重试预算
//! 天然把补投也封了顶；if 线没有尝试台账（失败的那次压根不落 `ifline_beats` 行），
//! 所以这个封顶得自己带。
//!
//! 到顶时**不是静默放弃**：清 `advance_requested_at` + 把原因写进 `last_error`，
//! 于是玩家立刻能再点（不必再等陈旧线），读取面也能说出发生过什么。
//! 静默放弃会把「补偿机制」变成「一个更难查的静默失败」——那正是 0050 那一批在防的东西。
//!
//! # 🔴 补投窗口不得早于请求层的陈旧线
//!
//! 陈旧线是本仓对「不可能还在跑」的既有估计。若 sweep 比它更早开火，就可能对一个**仍在跑**
//! 的任务补投：`(ifline_id, beat_no)` 唯一键会挡住第二次落库，但那次模型调用是真烧掉的。
//! 故 [`sweep_after_ms`] 恒取 `max(配置值, advance_stale_ms() + 1 分钟)`——
//! 两个参数各自可调，但这条不变式由代码保证，不靠运营记得。
//!
//! # 边界
//!
//! - 默认关闭（[`ENV_SWEEP`]，§0.1）。关着时这条循环只做一次开关解析。
//! - 「玩家拉动」这条设计**没有被本模块破坏**：sweep 只补投**玩家已经点过**的那一次
//!   （`advance_requested_at != 0` 恒由玩家点击写下），它不会凭空推进任何一条 if 线。
//!   源码级红线 `red_line_no_scheduler_ever_touches_iflines` 仍成立，本模块也在其豁免名单外
//!   ——它不是调度器，是同一次玩家请求的续投。

use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::flags::{self, FlagCtx};

use super::{advance_stale_ms, AdvanceJob, ADVANCE_TOPIC};

/// 运行时开关（§0.1 未验证功能默认关闭）。**全局档**：它是一条后台循环，
/// 没有「按用户 / 按世界」的语义（同 §15 第 3 层的 sweep）。
pub(crate) const ENV_SWEEP: &str = "MUSE_IFLINE_ADVANCE_SWEEP";
const DEFAULT_SWEEP_ENABLED: bool = false;

const _: () = assert!(
    !DEFAULT_SWEEP_ENABLED,
    "§0.1：未验证功能默认关闭。补投会真的调模型、真的烧 token，默认开启等于让「合并代码」\
     直接改变线上成本。"
);

/// 两次对账之间的间隔（§0.2 参数化）。默认 5 分钟——同 §15 第 3 层的 sweep。
const ENV_INTERVAL_MS: &str = "MUSE_IFLINE_SWEEP_INTERVAL_MS";
const DEFAULT_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// 补投窗口：在飞标记停留多久之后才认为它丢了。默认 30 分钟（= 陈旧线默认值的 3 倍）。
/// 🔴 实际取值恒 ≥ `advance_stale_ms() + 1 分钟`，见 [`sweep_after_ms`]。
const ENV_AFTER_MS: &str = "MUSE_IFLINE_SWEEP_AFTER_MS";
const DEFAULT_AFTER_MS: i64 = 30 * 60 * 1000;

/// 同一次「在飞」最多被自动补投几次。默认 3。
const ENV_MAX_REDELIVERIES: &str = "MUSE_IFLINE_SWEEP_MAX_REDELIVERIES";
const DEFAULT_MAX_REDELIVERIES: i64 = 3;

/// 单次对账最多处理几行（每行一次模型调用，成本闸）。默认 50。
const ENV_BATCH: &str = "MUSE_IFLINE_SWEEP_BATCH";
const DEFAULT_BATCH: i64 = 50;

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.parse::<i64>().ok()).filter(|v| *v > 0).unwrap_or(default)
}

pub(crate) fn interval_ms() -> i64 {
    env_i64(ENV_INTERVAL_MS, DEFAULT_INTERVAL_MS)
}

/// 🔴 恒 ≥ 请求层陈旧线 + 1 分钟。见模块头「补投窗口不得早于请求层的陈旧线」。
pub(crate) fn sweep_after_ms() -> i64 {
    env_i64(ENV_AFTER_MS, DEFAULT_AFTER_MS).max(advance_stale_ms() + 60_000)
}

pub(crate) fn max_redeliveries() -> i64 {
    env_i64(ENV_MAX_REDELIVERIES, DEFAULT_MAX_REDELIVERIES)
}

pub(crate) fn batch() -> i64 {
    env_i64(ENV_BATCH, DEFAULT_BATCH)
}

pub(crate) async fn sweep_enabled(db: &AnyPool) -> bool {
    // 默认值由 `flags::KNOWN_FLAGS` 那条登记承载（`default_enabled: false`），
    // 与 §15 第 3 层 sweep 同口径——开关体系只有一个入口。
    flags::is_enabled(db, ENV_SWEEP, FlagCtx::global()).await
}

/// 一条待补投的 if 线。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stranded {
    pub(crate) ifline_id: String,
    /// 读到的在飞时刻。CAS 用它做「没人动过」的凭据。
    pub(crate) requested_at: i64,
    pub(crate) sweep_count: i64,
}

/// 找出「在飞标记还在、且已过补投窗口」的行。
///
/// `status` 不进判据：已收尾的线不可能还有在飞标记（收尾走的也是 `run_advance_job` 的出口，
/// 一样清标记）；若真出现了，补投一次会被 `advance_one_beat` 的既有校验挡住并写 `last_error`
/// ——那正是我们想要的可见结果，而不是被这里悄悄跳过。
pub(crate) async fn find_stranded(db: &AnyPool, now: i64, limit: i64) -> Vec<Stranded> {
    let rows = sqlx::query(
        "SELECT id, advance_requested_at, advance_sweep_count FROM ifline_worlds \
         WHERE advance_requested_at > 0 AND advance_requested_at < $1 \
         ORDER BY advance_requested_at ASC, id ASC LIMIT $2",
    )
    .bind(now - sweep_after_ms())
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    rows.iter()
        .filter_map(|r| {
            Some(Stranded {
                ifline_id: r.try_get::<String, _>("id").ok()?,
                requested_at: r.try_get::<i64, _>("advance_requested_at").ok()?,
                sweep_count: r.try_get::<i64, _>("advance_sweep_count").ok()?,
            })
        })
        .collect()
}

/// 处理一行：到顶则终结（清标记 + 写原因），否则 CAS 续投。
///
/// 返回 `true` 表示真的投了一份任务出去（用例与日志据此计数）。
pub(crate) async fn redeliver_one(state: &AppState, s: &Stranded, now: i64) -> bool {
    if s.sweep_count >= max_redeliveries() {
        // 到顶：**不是静默放弃**。清掉在飞标记（玩家立刻能再点，不必等陈旧线）
        // 并把原因留在读取面上。
        let msg = format!(
            "自动补投已达上限（{} 次）仍未落定，已停止自动重试。可以再点一次推进；\
             若反复如此，请联系运营查后台日志。",
            max_redeliveries()
        );
        let done = sqlx::query(
            "UPDATE ifline_worlds SET advance_requested_at = 0, advance_sweep_count = 0, \
             last_error = $1, last_error_at = $2 WHERE id = $3 AND advance_requested_at = $4",
        )
        .bind(&msg)
        .bind(now)
        .bind(&s.ifline_id)
        .bind(s.requested_at)
        .execute(&state.db)
        .await;
        if let Err(e) = done {
            tracing::error!(ifline = %s.ifline_id, error = %e, "if 线补投到顶后写库失败");
        } else {
            tracing::warn!(ifline = %s.ifline_id, max = max_redeliveries(), "if 线自动补投到顶，停止重试");
        }
        return false;
    }

    // CAS：拿读到的 `advance_requested_at` 当凭据。若这中间玩家自己又点了、或另一轮对账
    // 抢先了，`rows_affected == 0` → 本轮跳过（宁可少投一次，也不能双投烧两遍）。
    let claimed = sqlx::query(
        "UPDATE ifline_worlds SET advance_requested_at = $1, advance_sweep_count = advance_sweep_count + 1 \
         WHERE id = $2 AND advance_requested_at = $3",
    )
    .bind(now)
    .bind(&s.ifline_id)
    .bind(s.requested_at)
    .execute(&state.db)
    .await;
    match claimed {
        Ok(r) if r.rows_affected() > 0 => {
            crate::queue::push_json(
                &*state.queue,
                ADVANCE_TOPIC,
                &AdvanceJob { ifline_id: s.ifline_id.clone() },
                now,
            )
            .await;
            tracing::info!(
                ifline = %s.ifline_id, attempt = s.sweep_count + 1,
                "if 线在飞任务已丢失，自动补投"
            );
            true
        }
        Ok(_) => false, // 期间被别人改过，正常跳过。
        Err(e) => {
            tracing::error!(ifline = %s.ifline_id, error = %e, "if 线补投 CAS 失败");
            false
        }
    }
}

/// 跑一轮对账。返回真正投出去的份数。
pub(crate) async fn sweep_once(state: &AppState, now: i64) -> usize {
    let mut sent = 0;
    for s in find_stranded(&state.db, now, batch()).await {
        if redeliver_one(state, &s, now).await {
            sent += 1;
        }
    }
    sent
}

/// 后台循环。关着时每轮只做一次开关解析（不碰 `ifline_worlds`）。
pub fn spawn_sweeper(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(interval_ms().max(1) as u64)).await;
            if !sweep_enabled(&state.db).await {
                continue;
            }
            let now = super::now_ms();
            let sent = sweep_once(&state, now).await;
            if sent > 0 {
                tracing::info!(sent, "if 线对账补投完成");
            }
        }
    });
}
