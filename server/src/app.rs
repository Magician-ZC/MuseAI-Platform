//! 应用装配：AppState + 总路由。领域模块各自提供 `router()`，此处统一挂载（主循环所有，agent 勿改）。

use std::sync::Arc;

use axum::Router;
use sqlx::AnyPool;

use crate::config::ServerConfig;
use crate::providers::{DevModeration, DevSms, LocalObjectStore, ModerationProvider, SmsProvider};
use crate::queue::{MemQueue, Queue};

#[derive(Clone)]
pub struct AppState {
    pub db: AnyPool,
    pub config: Arc<ServerConfig>,
    pub queue: Arc<dyn Queue>,
    pub sms: Arc<dyn SmsProvider>,
    pub moderation: Arc<dyn ModerationProvider>,
    pub objects: Arc<LocalObjectStore>,
    /// WS 事件广播中心（events 模块定义）
    pub ws_hub: Arc<crate::events::WsHub>,
}

impl AppState {
    pub fn new(db: AnyPool, config: ServerConfig) -> Self {
        let objects = Arc::new(LocalObjectStore::new(config.object_store_dir.clone()));
        Self {
            db,
            config: Arc::new(config),
            queue: MemQueue::new(),
            sms: Arc::new(DevSms),
            moderation: Arc::new(DevModeration::default()),
            objects,
            ws_hub: Arc::new(crate::events::WsHub::default()),
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        .merge(crate::auth::router())
        .merge(crate::assets::router())
        .merge(crate::worlds::router())
        .merge(crate::events::router())
        .merge(crate::interventions::router())
        .merge(crate::consents::router())
        .merge(crate::notifications::router())
        .merge(crate::reports::router())
        .merge(crate::backpack::router())
        .merge(crate::chapters::router())
        .merge(crate::admin_api::router());

    #[cfg(feature = "arena")]
    let api = api.merge(crate::arena::router()).merge(crate::livegate::router());

    #[cfg(feature = "billing")]
    let api = api.merge(crate::billing::router());

    // P3 平台售卖（云成长 / 平台道具售卖 / 创作者收益查询）：依赖复式账本，与 ledger 同 feature 门控。
    #[cfg(any(feature = "billing", feature = "arena"))]
    let api = api.merge(crate::shop::router());

    Router::new()
        .nest("/api", api)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
