//! MuseAI 平台后端入口。dev 态零配置：`cargo run` 即以 SQLite 内存库 + Dev providers 启动。

mod admin_api;
mod admission;
// R3 OOC 注解权（总规格 §7「人设保险（三级出口）」第 2 级）：单拍 OOC 申诉 →
// **世界事实不改**，私人传记可加内心批注；复核确认模型错误则补偿托梦配额。
// 默认关闭的运行时开关 `MUSE_OOC_ANNOTATIONS`；它同时是 VALIDATION §4.2「OOC 申诉率」
// 这一项 SLO 的唯一数据源（此前八项里唯一算不出来的那项），也是 T1 门槛的测量手段。
mod annotations;
mod app;
mod assembly;
mod assets;
mod auth;
mod backpack;
mod chapters;
mod config;
mod consents;
mod db;
mod error;
mod events;
// 运行时开关体系（VALIDATION.md §0.1「未验证功能默认关闭」的基础设施层，R1 补齐项）：
// 统一读取入口 `flags::is_enabled`，解析链 **按用户 > 按世界 > 全局 > env > 代码内默认值**。
// 🔴 env 是兜底而非被替代：`runtime_flags` 表为空时全部现存模块行为逐字不变。
// 本批次只接线 `MUSE_ONBOARDING` 一个，其余 8 个的迁移清单见 `flags::MIGRATION_NOTES`。
mod flags;
mod idempotency;
// R3 if 线付费副本（总规格 §7「人设保险（三级出口）」第 3 级，三级出口的最后一级）：
// 世界结束后花副本卡以**终局**为分叉点开单人平行线副本。
// 🔴 **平行线不是改写**：原世界的 world_events / narrative_state / 结算账本一个字节不动（§0.3）；
// if 线是**独立实例表**而不是一行 `worlds`——那样它就会自动走进结算管线发历练/铸卡，
// 「花钱开 if 线」立刻等于「花钱买数值」（§0.1）。
// 🔴 分叉点**不假装**：仓库不存逐拍状态快照，故只支持终局分叉，请求中间拍明确 400。
// 默认关闭的运行时开关 `MUSE_IFLINE_PARALLEL`。
mod ifline;
mod interventions;
// 房间邀请（客户端设计文档 §6 辅助栏）：默认关闭的运营开关 `MUSE_ROOM_INVITATIONS`，
// 接受邀请只点亮引导入口，入场仍走 worlds::join_world 的全部校验。
mod invitations;
// R2 传世卡（总规格 §12【拍板 23】）：死亡 = 传记封卷，不是资产清零。
// 封卷 = 卡转「传世卡」（只读、入遗作馆陈列、不可再入世界）+ 道具归账户背包 + 羁绊方得「故人」印记。
// 默认关闭的运营开关 `MUSE_MEMORIAL`；不改写世界线（公共事实不可回滚），无任何隐藏数值。
mod memorial;
mod notifications;
// 新手动线（总规格 §13【拍板 21】）：预制卡库 + 单人微本模板 + 新人礼包发放。
// 默认关闭的运营开关 `MUSE_ONBOARDING`；礼包只发卡 + 建房，入场仍走 worlds::join_world 的全部校验。
mod onboarding;
// 波次 2：历练值 + 卡位制（成长值只作准入与解锁，绝不进引擎决策）。
mod progression;
mod providers;
mod queue;
mod reports;
mod runtime;
mod safety;
// 叙事质量 SLO（VALIDATION §4.2）：只读观测口径 + 平台级聚合，供运营看板消费。
mod slo;
// R3 真人社交解锁（总规格 §14【拍板 22】恨隔面具原则）：默认角色面具，
// 仅**正向羁绊线**达阈值后**双向自愿**解锁真人身份；**敌对线永久匿名**（一票否决）。
// 配套拉黑（按 user 判定、按面具录入，撤销已授予的身份可见性）+ 举报队列（可运营 + 累计升级风控）。
// 🔴 青少年模式限真人社交是**服务端拒绝**（`ensure_adult_social`，fail-closed 到未成年）。
// 🔴 独有社交资产「我们的角色一起死过」是**只读派生的关系凭证**，无存储、零数值影响。
// 默认关闭的运行时开关 `MUSE_SOCIAL_IDENTITY_UNLOCK`。
mod social;
// R2 副本卡（总规格 §10【拍板 1、6、7、11、17】）：结算产出的剧情结晶 + 同星合成回收口。
// 默认关闭的运营开关 `MUSE_SUBPLOT_CARDS`；零 RNG（查公示产出表）、无任何交易/转让路径、永不加战力。
mod subplot;
mod worlds;

#[cfg(feature = "arena")]
mod arena;
#[cfg(feature = "arena")]
mod clips;
#[cfg(feature = "arena")]
mod livegate;
#[cfg(feature = "billing")]
mod billing;
// 复式账本（P0）：billing 充值/退款双写 + 各付费点统一扣费口。feature 与经济模块（billing/arena）一致。
#[cfg(any(feature = "billing", feature = "arena"))]
mod ledger;
// P3 平台售卖：云成长服务位 + 平台道具单向售卖 + 创作者收益查询。依赖 ledger，feature 一致。
#[cfg(any(feature = "billing", feature = "arena"))]
mod shop;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let config = config::ServerConfig::from_env();
    let pool = db::connect(&config.database_url).await?;
    let state = app::AppState::new(pool, config.clone());

    // 世界运行时：tick 调度器 + worker（后台任务）
    runtime::spawn_workers(state.clone());
    // 通知 outbox 消费
    notifications::spawn_outbox_worker(state.clone());

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, dev = config.dev_mode, "muse-server 启动");
    axum::serve(listener, app::build_router(state)).await?;
    Ok(())
}
