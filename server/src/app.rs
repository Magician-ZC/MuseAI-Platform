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

    /// **生产装配**：在 [`Self::new`] 之上按环境变量替换真实外部 provider。
    ///
    /// 目前只有一路：内容审核（[`crate::providers::HttpModerationProvider`]）。
    ///
    /// 🔴 三种结果，方向都是刻意的：
    /// - **未配置** → 保留 `DevModeration`，行为与接线前**逐字节一致**。dev 与 CI 都是零配置
    ///   环境，所以这是默认路径（§0.1 未验证功能默认关闭）。此时打一条 warn，因为
    ///   「审核是个桩」这件事在生产里必须一眼可见。
    /// - **配置完整** → 换上真实 provider，`is_dev_stub()` 随之翻成 `false`
    ///   （见 `safety::semantic` 模块头「当前是桩」那张表：三处载体自动翻面）。
    /// - **配错** → 返回 `Err`，`main` 据此**让进程起不来**。方向抄 [`cors_layer`] 对
    ///   `MUSE_CORS_ORIGINS` 的处理：宁可立刻、显式地起不来，也不静默降级——一个配错的审核
    ///   provider 与没有审核，在看板上是看不出区别的（`providerStub` 仍会显示 `false`）。
    ///
    /// ⚠️ 刻意**不写进 [`Self::new`]**：`new` 是全部用例的入口（`safety::testkit::test_state`
    /// 等），把读进程 env 塞进去会让「设了这组变量的开发机」跑出与 CI 不同的测试结果。
    pub fn from_env(db: AnyPool, config: ServerConfig) -> Result<Self, String> {
        let mut state = Self::new(db, config);
        match crate::providers::HttpModerationProvider::from_env()? {
            Some(p) => {
                tracing::info!("{}", p.config().describe());
                state.moderation = Arc::new(p);
            }
            None => tracing::warn!(
                env = crate::providers::http_moderation::ENV_ENDPOINT,
                "内容审核使用 Dev 桩（DevModeration）：只匹配一张小关键词表，不做任何语义分类。\
                 §15 五层漏斗的第 3 层因此拦不住任何东西——面向公众上线前必须配置真实服务商。"
            ),
        }
        Ok(state)
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
        .merge(crate::invitations::router())
        .merge(crate::notifications::router())
        .merge(crate::reports::router())
        .merge(crate::backpack::router())
        .merge(crate::chapters::router())
        .merge(crate::progression::router())
        .merge(crate::subplot::router())
        .merge(crate::imprint::router())
        .merge(crate::onboarding::router())
        .merge(crate::annotations::router())
        .merge(crate::ifline::router())
        .merge(crate::social::router())
        // 直播场（定档 + 延迟缓冲 + 弹幕）。**不挂 `arena` feature 门控**：延迟缓冲是内容
        // 安全机制（§15 第 4 层），把它编进可选 feature 等于让默认构建缺一层安全闸；
        // 且它只依赖 events/safety/flags，与 ledger 无关。能力本身由运行时开关
        // `MUSE_LIVE_STAGE` 控制，默认关闭。
        .merge(crate::livestage::router())
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
        .layer(cors_layer())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// 本地开发默认放行的来源。**三个前端都与 server 不同源**：
/// 玩家端 Vite `:1420`、运营后台 Vite `:1430`、Tauri webview（各平台 origin 不同）。
const DEFAULT_DEV_ORIGINS: &str = "http://localhost:1420,http://127.0.0.1:1420,\
                                   http://localhost:1430,http://127.0.0.1:1430,\
                                   tauri://localhost,https://tauri.localhost";

/// 跨源白名单。
///
/// 🔴 **为什么必须有这一层**：在此之前 `build_router` 只挂了 `TraceLayer`，而
/// `admin/vite.config` 与根 `vite.config.ts` **都没有配 proxy**，两个前端都直连
/// `http://127.0.0.1:8787`。结果是浏览器同源策略拦掉每一个请求——**运营后台与玩家端
/// 在真实浏览器里一个接口都调不通**。这件事长期没被发现，是因为前端一律只验证到
/// `npm run build` 通过：构建通过和浏览器里能用，中间隔着一条同源策略。
/// （`Cargo.toml` 的 `tower-http` 早就开着 `cors` feature，只是从未使用。）
///
/// 🔴 **不用 `AllowOrigin::any()`**：这些接口虽有 JWT 鉴权，放开任意源仍是无谓攻击面——
/// 任何网站都能在受害者浏览器里向本服务发请求。故一律走白名单。
///
/// 生产部署用 `MUSE_CORS_ORIGINS` 显式指定（逗号分隔）；不设时只放行本地开发来源。
/// 解析失败的条目**跳过并告警**，全部失败则退化为「不放行任何跨源」——
/// fail-closed 方向：配错了宁可前端连不上（立刻可见），也不要静默放行成通配。
///
/// ⚠️ 不开 `allow_credentials`：两个前端都用 `Authorization: Bearer`
/// （admin 存 sessionStorage、玩家端存 localStorage），没有 cookie 要带。
/// 开了它反而会把「白名单必须精确」的约束变成安全关键项。
fn cors_layer() -> tower_http::cors::CorsLayer {
    use axum::http::{header, Method};

    let raw = std::env::var("MUSE_CORS_ORIGINS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_DEV_ORIGINS.to_string());
    let origins = parse_origins(&raw);
    if origins.is_empty() {
        tracing::warn!("CORS 白名单为空——所有跨源请求都将被拒绝（前端将无法连接）");
    }

    tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// 逗号分隔的来源串 → 白名单。非法条目跳过并告警，**不** panic：
/// 一个打错的字符不该让整个服务起不来，但也不该被静默扩大成通配。
fn parse_origins(raw: &str) -> Vec<axum::http::HeaderValue> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|item| match item.parse::<axum::http::HeaderValue>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(origin = item, "MUSE_CORS_ORIGINS 中有无法解析的来源，已跳过");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod cors_tests {
    use super::*;
    use axum::http::{header, HeaderValue, Method, StatusCode};

    /// 默认白名单必须覆盖**全部三个前端**。少一个就是那个前端在浏览器里连不上。
    #[test]
    fn default_whitelist_covers_every_frontend() {
        let got = parse_origins(DEFAULT_DEV_ORIGINS);
        for must in [
            "http://localhost:1420",   // 玩家端 Vite
            "http://127.0.0.1:1420",
            "http://localhost:1430",   // 运营后台 Vite
            "http://127.0.0.1:1430",
            "tauri://localhost",       // Tauri webview（macOS / Linux）
            "https://tauri.localhost", // Tauri webview（Windows）
        ] {
            assert!(
                got.iter().any(|v| v == must),
                "默认白名单缺少 {must}——该前端在浏览器里会被同源策略全拦"
            );
        }
    }

    /// 🔴 非法条目**跳过**而不是让整串作废，也不是 panic。
    #[test]
    fn malformed_entries_are_skipped_not_fatal() {
        // `\n` 无法进 HeaderValue；空条目与多余空格应被吃掉。
        let got = parse_origins(" http://a.example , \n , , http://b.example ");
        assert_eq!(got, vec!["http://a.example", "http://b.example"]);
        assert!(parse_origins("").is_empty());
        assert!(parse_origins(" , , ").is_empty());
    }

    fn state() -> AppState {
        AppState::new(
            sqlx::any::AnyPoolOptions::new()
                .max_connections(1)
                .connect_lazy("sqlite::memory:")
                .unwrap(),
            ServerConfig {
                database_url: "sqlite::memory:".into(),
                bind_addr: "127.0.0.1:0".into(),
                jwt_secret: "test-secret".into(),
                access_ttl_secs: 3600,
                refresh_ttl_secs: 100_000,
                dev_mode: true,
                object_store_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            },
        )
    }

    async fn preflight(origin: &str) -> (StatusCode, Option<HeaderValue>) {
        use tower::ServiceExt;
        let resp = build_router(state())
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/auth/login")
                    .header(header::ORIGIN, origin)
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization,content-type")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let allow = resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).cloned();
        (resp.status(), allow)
    }

    /// 白名单内的来源：预检必须回 `Access-Control-Allow-Origin`。
    ///
    /// ⚠️ 本用例不设 `MUSE_CORS_ORIGINS`（env 是进程级的，设了会与并发用例互踩），
    /// 因此测的是**默认白名单**这条真实的开发路径——恰好也是此前完全走不通的那条。
    #[tokio::test]
    async fn preflight_from_admin_origin_is_allowed() {
        let (status, allow) = preflight("http://127.0.0.1:1430").await;
        assert!(status.is_success(), "预检应被 CorsLayer 直接放行，实际 {status}");
        assert_eq!(
            allow.as_ref().map(|v| v.to_str().unwrap()),
            Some("http://127.0.0.1:1430"),
            "🔴 缺 Access-Control-Allow-Origin —— 浏览器会拦掉后续所有请求"
        );
    }

    /// 🔴 白名单外的来源**不得**拿到放行头。这条是 `allow_origin(Any)` 与白名单的分水岭：
    /// 若哪天有人图省事换成 `Any`，本用例立刻红。
    #[tokio::test]
    async fn preflight_from_unknown_origin_is_not_allowed() {
        let (_, allow) = preflight("https://evil.example").await;
        assert!(
            allow.is_none(),
            "🔴 未登记来源拿到了放行头：任何网站都能拿受害者浏览器里的 token 发请求"
        );
    }
}

// ============================================================================
// 🔴 经济 feature 关闭时，一条付费路径都不可达
// ============================================================================

/// **只在默认构建（无 `billing` / `arena`）里编译**——它测的正是那个构建的形状。
///
/// 编译器已经保证了「不能调用 `ledger`」（无 feature 时那个模块根本不存在，
/// 调用点不 gate 就编译不过）。但编译器保证不了**路由是否真的不存在**：
/// 有人完全可以注册一个不碰账本、却暴露付费内容的端点，那样编译照过。
///
/// 本用例是那一半的行为面证据：默认构建下这些前缀必须 404。
///
/// ⚠️ `POST /worlds` 是**同一个路径上的方法差异**（`GET` 恒在、`POST` 只在有经济时注册），
/// 故它期望的是 405 而不是 404——把两者混为一谈会让这条断言在错误的地方绿。
///
/// 🔵 **三处故障注入，结果不一样，如实记**：
/// - 给默认构建注册一个不碰账本的 `/me/earnings` → **红**（这正是编译器管不到、本用例管得到的那一类）；
/// - 把大厅列表也一并门掉（过度门控）→ **红**（下面第二条的 `assert_ne!` 就是防这个）；
/// - 把 `POST /worlds` 错误地注册进默认构建 → **编译不过**。
///   `create_room` 自己就要调 `ledger`，无 feature 时那个模块不存在。
///   也就是说这一种错**编译器已经挡住了**，本用例在这一点上是纵深而不是唯一防线——
///   写清楚比笼统说「三处注入全红」诚实。
#[cfg(all(test, not(any(feature = "billing", feature = "arena"))))]
mod economy_gate_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn status_of(method: &str, path: &str) -> StatusCode {
        let state = crate::safety::testkit::test_state().await;
        let app = build_router(state);
        let req = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        app.oneshot(req).await.unwrap().status()
    }

    /// 付费面在默认构建里整片不存在。
    #[tokio::test]
    async fn paid_routes_are_absent_without_economy_features() {
        for (method, path) in [
            ("POST", "/api/shop/items/sku_x/purchase"),   // 平台道具售卖
            ("GET", "/api/me/earnings"),                  // 创作者收益
            ("POST", "/api/billing/orders"),              // 充值下单
            ("GET", "/api/arena/w1/clips"),               // 竞技场切片
            ("GET", "/api/arena/gift-skus"),              // 礼物目录
        ] {
            assert_eq!(
                status_of(method, path).await,
                StatusCode::NOT_FOUND,
                "🔴 默认构建（无 billing/arena）里 `{method} {path}` 竟然可达——\
                 经济 feature 关闭时必须一条付费路径都没有"
            );
        }
    }

    /// 建房收费：同一路径上 `GET` 恒在、`POST` 只在有经济时注册 → 405 而非 404。
    #[tokio::test]
    async fn creating_a_room_is_unavailable_but_the_lobby_still_lists() {
        assert_eq!(
            status_of("POST", "/api/worlds").await,
            StatusCode::METHOD_NOT_ALLOWED,
            "🔴 建房携开房费，无经济 feature 时不得注册 POST"
        );
        // 大厅列表恒在（它不花钱）——顺带证明上面那条 405 不是「整条路由都没了」。
        assert_ne!(
            status_of("GET", "/api/worlds").await,
            StatusCode::NOT_FOUND,
            "🔴 大厅列表与经济无关，不得被一并门掉"
        );
    }
}
