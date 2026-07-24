//! MuseAI 平台后端入口。dev 态零配置：`cargo run` 即以 SQLite 内存库 + Dev providers 启动。

mod admin_api;
mod admission;
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
mod idempotency;
mod interventions;
mod notifications;
// 波次 2：历练值 + 卡位制（成长值只作准入与解锁，绝不进引擎决策）。
mod progression;
mod providers;
mod queue;
mod reports;
mod runtime;
mod safety;
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
