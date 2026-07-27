//! HTTP 内容审核 provider 用例。
//!
//! 覆盖清单，按重要性排：
//!   · 🔴 **零配置 = 今天的行为，逐字节**——`zero_config_falls_back_to_dev_moderation`
//!     / `zero_config_assembly_keeps_the_dev_stub`
//!   · 🔴 **每一种失败模式都报错，绝不返回 Approved**——`red_line_every_failure_mode_errors_never_approves`
//!   · 🔴 **凭据不泄漏**（错误串 / Debug / 落库字段 / 启动日志四处一起搜）
//!     ——`red_line_credentials_never_leak_anywhere`
//!   · 🔴 **`is_dev_stub()` 翻面且随数据走**——`configured_provider_flips_the_stub_fact_end_to_end`
//!   · 🔴 **配错一律启动失败**（逐条形态）——`misconfiguration_matrix_fails_at_startup`
//!   · 模板三种塞法 · 响应路径与通配取最严 · 超时 · 成本比值
//!
//! ⚠️ **不依赖任何外部网络**：全部 HTTP 用例都打一个本地 `127.0.0.1:0` 上的 axum mock
//! （[`Mock::spawn`]），CI 里没有外网审核服务。用真 mock 而不是注入假 transport，是为了让
//! reqwest 的请求构造、请求头、超时、状态码这些真实路径**也在用例覆盖里**——
//! 假 transport 只能测到映射逻辑，测不到「头发出去长什么样」。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::StatusCode;
use base64::Engine as _;
use serde_json::Value;

use crate::providers::{ModerationProvider, ModerationVerdict};

use super::*;

// ═══════════════════════════════════════════════════════════════════════════
// 夹具：本地 mock 审核服务
// ═══════════════════════════════════════════════════════════════════════════

/// mock 收到的一次请求（原样存下来，供凭据泄漏与模板用例断言）。
#[derive(Debug, Clone)]
struct Received {
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Received {
    /// 整条请求拍平成一段文本——「凭据有没有发出去」按它搜。
    fn flat(&self) -> String {
        let h = self.headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
        format!("{} {}\n{}\n{}", self.method, self.uri, h, self.body)
    }
}

/// mock 的应答脚本。
#[derive(Clone)]
enum Reply {
    /// 定死的状态码 + 响应体。
    Fixed(u16, String),
    /// 把收到的整条请求回显进响应体（模拟「网关把 Authorization 抄进错误体」）。
    EchoRequest(u16),
    /// 收到请求后先睡 N 毫秒再回（测超时）。
    Slow(u64, String),
}

struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<Received>>>,
}

impl Mock {
    /// 起一个只监听 `127.0.0.1:0` 的 axum 服务，返回其 base URL。
    ///
    /// 端口取 0（内核分配）而不是写死：用例并发跑时写死端口必冲突。
    async fn spawn(reply: Reply) -> Self {
        let seen: Arc<Mutex<Vec<Received>>> = Arc::new(Mutex::new(Vec::new()));
        let state = (reply, seen.clone());
        let app = axum::Router::new()
            .fallback(axum::routing::any(handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { base: format!("http://{addr}"), seen }
    }

    fn seen(&self) -> Vec<Received> {
        self.seen.lock().expect("mock 锁").clone()
    }

    fn last(&self) -> Received {
        self.seen().pop().expect("mock 收到过请求")
    }
}

async fn handler(
    axum::extract::State((reply, seen)): axum::extract::State<(Reply, Arc<Mutex<Vec<Received>>>)>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    let headers = req
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect::<Vec<_>>();
    let bytes = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap_or_default();
    let body = String::from_utf8_lossy(&bytes).to_string();
    let rec = Received { method, uri, headers, body };
    seen.lock().expect("mock 锁").push(rec.clone());

    let (status, out) = match reply {
        Reply::Fixed(s, b) => (s, b),
        Reply::EchoRequest(s) => (s, serde_json::json!({ "echo": rec.flat() }).to_string()),
        Reply::Slow(ms, b) => {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            (200, b)
        }
    };
    axum::response::Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
        .header("content-type", "application/json")
        .body(Body::from(out))
        .expect("mock 响应")
}

// ═══════════════════════════════════════════════════════════════════════════
// 夹具：配置查表
// ═══════════════════════════════════════════════════════════════════════════

/// 用 `HashMap` 驱动配置解析。🔴 **不碰进程级 env**：env 是进程级的，设了会与并发用例互踩
/// （同 `safety::semantic::tests` 用 `runtime_flags` 而不是 env 开开关的理由）。
fn cfg_from(pairs: &[(&str, &str)]) -> Result<Option<HttpModerationConfig>, String> {
    let map: HashMap<String, String> =
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    HttpModerationConfig::from_lookup(&|k| map.get(k).cloned())
}

/// 一份最小可用配置（指向 mock），用例按需覆盖其中几项。
fn base_pairs(endpoint: &str) -> Vec<(&'static str, String)> {
    vec![
        (ENV_ENDPOINT, endpoint.to_string()),
        (ENV_VERDICT_PATH, "suggestion".to_string()),
        (ENV_APPROVED_VALUES, "pass".to_string()),
        (ENV_PENDING_VALUES, "review".to_string()),
        (ENV_REJECTED_VALUES, "block".to_string()),
    ]
}

fn build(pairs: Vec<(&'static str, String)>) -> HttpModerationProvider {
    let borrowed: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let cfg = cfg_from(&borrowed).expect("配置应当合法").expect("配置应当启用");
    HttpModerationProvider::new(cfg).expect("构建 provider")
}

fn with(mut pairs: Vec<(&'static str, String)>, k: &'static str, v: &str) -> Vec<(&'static str, String)> {
    pairs.retain(|(name, _)| *name != k);
    pairs.push((k, v.to_string()));
    pairs
}

// ═══════════════════════════════════════════════════════════════════════════
// ① 🔴 零配置 = 今天的行为
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **整组变量一个都没配 → `Ok(None)`**，装配侧因此保留 `DevModeration`。
/// dev 与 CI 都是零配置环境，所以这就是默认路径（§0.1）。
#[test]
fn zero_config_falls_back_to_dev_moderation() {
    assert!(cfg_from(&[]).expect("零配置不是错误").is_none(), "🔴 零配置必须不启用");
    // 空串、纯空白也算没配（运维用 `VAR=` 清空是常见做法）。
    assert!(cfg_from(&[(ENV_ENDPOINT, "")]).unwrap().is_none());
    assert!(cfg_from(&[(ENV_ENDPOINT, "   ")]).unwrap().is_none());
}

/// 🔴 零配置下的**装配结果**与今天逐字节一致：`AppState::from_env` 得到的仍是 Dev 桩，
/// `is_dev_stub()` 仍是 `true`，`call_price_cents_per_1k()` 仍是 `None`。
///
/// 本用例**不设 env**，跑的就是 dev 与 CI 那条路径。
/// （若你的 shell 里恰好设了 `MUSE_MODERATION_HTTP_*`，本用例会红——那不是误报，
/// 是在告诉你「这台机器已经不是零配置环境了」。）
#[tokio::test]
async fn zero_config_assembly_keeps_the_dev_stub() {
    let pool = crate::testkit::test_pool().await;
    let state = crate::app::AppState::from_env(pool, crate::config::ServerConfig::from_env())
        .expect("零配置必须能起来");
    assert!(
        state.moderation.is_dev_stub(),
        "🔴 零配置下装配出的必须仍是 Dev 桩（若本机设了 {ENV_ENDPOINT}，请清掉后重跑）"
    );
    assert_eq!(state.moderation.call_price_cents_per_1k(), None);
    // 与 `DevModeration` 的行为逐条对齐（关键词命中 → Pending，其余 → Approved）。
    assert_eq!(state.moderation.check_text("普通文本").await.unwrap(), ModerationVerdict::Approved);
    assert_eq!(
        state.moderation.check_text("这里有测试敏感词哦").await.unwrap(),
        ModerationVerdict::Pending
    );
    assert_eq!(state.moderation.check_image(b"anything").await.unwrap(), ModerationVerdict::Approved);
}

// ═══════════════════════════════════════════════════════════════════════════
// ② 🔴 失败方向：报错就是报错，绝不吞成放行
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **本用例是这个模块存在的前提**。
///
/// 第 3 层定的是「窗口期重试 fail-open → 预算耗尽 fail-closed」，理由是：若 fail-open，
/// 打掉审核 provider 就成了绕过第 3 层的手段，审核链可用性会变成内容安全的上限。
/// provider 只负责**返回裁决或错误**；一旦它把错误吞成 `Approved`，那条 fail-closed
/// 就被悄悄改成了 fail-open，而调用方**察觉不到**（它收到的是一个正常裁决）。
///
/// 逐个失败模式扫：连不上 / 非 2xx / 响应不是 JSON / 路径取不到 / 标签没映射 / 超时。
#[tokio::test]
async fn red_line_every_failure_mode_errors_never_approves() {
    // (场景名, mock 应答, 额外配置)
    let cases: Vec<(&str, Option<Reply>, Vec<(&'static str, &'static str)>)> = vec![
        ("服务返回 5xx", Some(Reply::Fixed(500, r#"{"suggestion":"pass"}"#.into())), vec![]),
        ("服务返回 4xx", Some(Reply::Fixed(401, r#"{"message":"unauthorized"}"#.into())), vec![]),
        ("响应不是 JSON", Some(Reply::Fixed(200, "<html>502 Bad Gateway</html>".into())), vec![]),
        ("路径取不到标量", Some(Reply::Fixed(200, r#"{"other":"pass"}"#.into())), vec![]),
        ("路径指到对象而非标量", Some(Reply::Fixed(200, r#"{"suggestion":{"x":1}}"#.into())), vec![]),
        ("标签未映射", Some(Reply::Fixed(200, r#"{"suggestion":"UNKNOWN_LABEL"}"#.into())), vec![]),
        ("响应体为空", Some(Reply::Fixed(200, String::new())), vec![]),
        ("超时", Some(Reply::Slow(400, r#"{"suggestion":"pass"}"#.into())), vec![(ENV_TIMEOUT_MS, "60")]),
    ];

    for (name, reply, extra) in cases {
        let (endpoint, _keep) = match reply {
            Some(r) => {
                let m = Mock::spawn(r).await;
                (format!("{}/check", m.base), Some(m))
            }
            None => (String::new(), None),
        };
        let mut pairs = base_pairs(&endpoint);
        for (k, v) in extra {
            pairs = with(pairs, k, v);
        }
        let p = build(pairs);
        let got = p.check_text("待审文本").await;
        assert!(got.is_err(), "🔴「{name}」没有报错，实际 {got:?}——错误被吞掉就是 fail-open");
        assert_ne!(
            got.ok(),
            Some(ModerationVerdict::Approved),
            "🔴「{name}」返回了 Approved：provider 自己把故障转化成了放行"
        );
    }

    // 连不上（端口上没有服务）单独一条：不能起 mock，故不在上面的循环里。
    let p = build(base_pairs("http://127.0.0.1:1/check"));
    let got = p.check_text("待审文本").await;
    assert!(got.is_err(), "🔴 连不上审核服务时返回了 {got:?}");
}

/// provider **不自己重试、不自己退避**——那是调用方（`safety::semantic`）的职责，
/// 它按 `MUSE_SAFETY_L3_MAX_ATTEMPTS` / `_BACKOFF_MS` 记账并最终 fail-closed。
/// provider 偷偷重试会让那本账少算，也会让「超时」这个参数失去意义。
#[tokio::test]
async fn provider_does_not_retry_on_its_own() {
    let m = Mock::spawn(Reply::Fixed(500, "boom".into())).await;
    let p = build(base_pairs(&format!("{}/check", m.base)));
    assert!(p.check_text("x").await.is_err());
    assert_eq!(m.seen().len(), 1, "🔴 provider 自己重试了：重试预算的账会算不准");
}

// ═══════════════════════════════════════════════════════════════════════════
// ③ 🔴 凭据脱敏
// ═══════════════════════════════════════════════════════════════════════════

const API_KEY: &str = "sk-live-0123456789abcdef";

/// 🔴 凭据不许进日志、不许进落库字段。强度对齐
/// `muse_engine::replay::tests::recording_never_leaks_credentials`：把凭据同时藏进
/// URL query、请求头、请求体三处，再让服务端把**整条请求回显进错误体**（真实网关会这么干），
/// 然后在四个出口里搜原文：错误串、`Debug`、启动自述、以及经由 `ApiError` 落地的文本。
#[tokio::test]
async fn red_line_credentials_never_leak_anywhere() {
    let m = Mock::spawn(Reply::EchoRequest(401)).await;
    let pairs = vec![
        (ENV_ENDPOINT, format!("{}/check?token={{{{API_KEY}}}}", m.base)),
        (ENV_API_KEY, API_KEY.to_string()),
        (ENV_HEADERS, format!("Authorization: Bearer {PH_API_KEY}|X-Key: {PH_API_KEY}")),
        (ENV_BODY, format!(r#"{{"key":"{PH_API_KEY}","text":"{PH_TEXT}"}}"#)),
        (ENV_VERDICT_PATH, "suggestion".to_string()),
        (ENV_APPROVED_VALUES, "pass".to_string()),
        (ENV_REJECTED_VALUES, "block".to_string()),
    ];
    let p = build(pairs);

    // 凭据确实**发出去了**（否则这条用例是空转）。
    let err = p.check_text("待审文本").await.unwrap_err();
    let sent = m.last().flat();
    assert!(sent.contains(API_KEY), "前置条件：凭据本就该发给服务端\n{sent}");
    // URL query 1 处 + 请求头 2 处 + 请求体 1 处：四个藏法都要真的塞进去，
    // 否则下面「搜不到原文」可能只是因为它压根没发出去。
    assert_eq!(sent.matches(API_KEY).count(), 4, "四处藏法都要真的塞进去\n{sent}");

    // 出口 1：错误串（它会进 tracing::warn 与 ApiError::Internal 的日志）。
    assert!(!err.contains(API_KEY), "🔴 错误串里出现凭据原文：\n{err}");
    assert!(err.contains(REDACTED), "脱敏后应留下占位，便于运维知道「这里本来有个密钥」");

    // 出口 2：Debug（配置整个打印出来）。
    let dbg = format!("{:?}", p.config());
    assert!(!dbg.contains(API_KEY), "🔴 Debug 里出现凭据原文：\n{dbg}");
    assert!(!dbg.contains("token="), "🔴 Debug 里的 endpoint 没有剥掉 query");

    // 出口 3：启动自述（会原样进 tracing::info）。
    let desc = p.config().describe();
    assert!(!desc.contains(API_KEY), "🔴 启动日志里出现凭据原文：\n{desc}");
    assert!(!desc.contains("token="), "🔴 启动日志里的 endpoint 没有剥掉 query");

    // 出口 4：整个 provider 的 Debug（含内层 reqwest 客户端）。
    assert!(!format!("{p:?}").contains(API_KEY), "🔴 provider 的 Debug 里出现凭据原文");
}

/// URL 脱敏对三种藏法都成立（口径与 `replay::sanitize_base_url` 一致）。
#[test]
fn url_sanitizer_strips_query_and_userinfo() {
    assert_eq!(sanitize_url("https://u:p@h.example.com/v1?key=abc"), "https://<redacted>@h.example.com/v1");
    assert_eq!(sanitize_url("https://h.example.com/v1?key=abc#frag"), "https://h.example.com/v1");
    assert_eq!(sanitize_url("https://h.example.com/v1"), "https://h.example.com/v1");
    // 路径里的 `@` 不是 userinfo，别误伤。
    assert_eq!(sanitize_url("https://h.example.com/u/@bob"), "https://h.example.com/u/@bob");
}

/// 凭据被百分号编码进 URL 后再被回显时，也要能抹掉。
#[test]
fn redaction_covers_percent_encoded_form() {
    let cfg = cfg_from(&[
        (ENV_ENDPOINT, "https://h.example.com/c?k={{API_KEY}}"),
        (ENV_API_KEY, "abc/def+ghi=jkl"),
        (ENV_BODY, r#"{"t":"{{TEXT}}"}"#),
        (ENV_VERDICT_PATH, "s"),
        (ENV_APPROVED_VALUES, "pass"),
        (ENV_REJECTED_VALUES, "block"),
    ])
    .unwrap()
    .unwrap();
    let leaked = format!("gateway echoed: k={}", percent_encode("abc/def+ghi=jkl"));
    let clean = cfg.redact(&leaked);
    assert!(!clean.contains("abc%2Fdef"), "🔴 百分号编码形式没被抹掉：{clean}");
    assert!(clean.contains(REDACTED));
    // 原文形式同样抹掉。
    assert!(!cfg.redact("raw abc/def+ghi=jkl").contains("abc/def"));
}

/// 🔴 「凭据太短所以没被脱敏」这个缺口在本模块**不存在**：它被提前成了启动校验。
#[test]
fn short_credentials_are_rejected_at_startup_so_redaction_always_applies() {
    let e = cfg_from(&[
        (ENV_ENDPOINT, "https://h.example.com/c"),
        (ENV_API_KEY, "short"),
        (ENV_HEADERS, "Authorization: Bearer {{API_KEY}}"),
        (ENV_VERDICT_PATH, "s"),
        (ENV_APPROVED_VALUES, "pass"),
        (ENV_REJECTED_VALUES, "block"),
    ])
    .unwrap_err();
    assert!(e.contains(ENV_API_KEY) && e.contains("脱敏"), "{e}");
    assert!(MIN_API_KEY_LEN >= 8, "下限不得低于 replay::redact 的那条线");
}

// ═══════════════════════════════════════════════════════════════════════════
// ④ 🔴 配错一律启动失败（fail-closed，不运行时降级）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 逐条形态。取向抄 `app::cors_layer` 对 `MUSE_CORS_ORIGINS` 的处理——
/// 宁可立刻、显式地起不来，也不静默降级：一个配错的审核 provider 与没有审核，
/// 在运营看板上是**看不出区别**的（`providerStub` 照样显示 `false`）。
#[test]
fn misconfiguration_matrix_fails_at_startup() {
    /// 基线配置 + 覆盖若干项。
    fn ok<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut v = vec![
            (ENV_ENDPOINT, "https://h.example.com/c"),
            (ENV_VERDICT_PATH, "suggestion"),
            (ENV_APPROVED_VALUES, "pass"),
            (ENV_PENDING_VALUES, "review"),
            (ENV_REJECTED_VALUES, "block"),
        ];
        for (k, val) in extra {
            v.retain(|(name, _)| name != k);
            v.push((*k, *val));
        }
        v
    }
    // 基线本身必须合法，否则下面每条都会「碰巧」失败。
    assert!(cfg_from(&ok(&[])).expect("基线配置应当合法").is_some());
    let hardcoded_header = format!("Authorization: Bearer {API_KEY}");

    let cases: Vec<(&str, Vec<(&str, &str)>, &str)> = vec![
        // 🔴 最危险的一种：配了别的却没配 endpoint ⇒ 静默留在 Dev 桩上。
        (
            "只配了 key 没配 endpoint",
            vec![(ENV_API_KEY, API_KEY), (ENV_VERDICT_PATH, "s")],
            ENV_ENDPOINT,
        ),
        ("只配了 body 没配 endpoint", vec![(ENV_BODY, r#"{"t":"{{TEXT}}"}"#)], ENV_ENDPOINT),
        ("endpoint 不是 URL", ok(&[(ENV_ENDPOINT, "不是URL")]), "合法 URL"),
        ("endpoint scheme 不对", ok(&[(ENV_ENDPOINT, "ftp://h.example.com/c")]), "scheme"),
        ("方法不支持", ok(&[(ENV_METHOD, "DELETE")]), ENV_METHOD),
        ("GET 却配了 body", ok(&[(ENV_METHOD, "GET"), (ENV_BODY, r#"{"t":"{{TEXT}}"}"#)]), ENV_BODY),
        // 有 endpoint 没有 key：模板引用了占位符却没有值。
        (
            "模板引用 {{API_KEY}} 但没配凭据",
            ok(&[(ENV_HEADERS, "Authorization: Bearer {{API_KEY}}")]),
            "为空",
        ),
        // 反向：配了凭据却没人用它 ⇒ 请求裸奔。
        (
            "配了凭据但没有任何引用",
            ok(&[(ENV_API_KEY, API_KEY), (ENV_HEADERS, "X-Foo: bar")]),
            "裸奔",
        ),
        (
            "凭据被硬编码进头里",
            ok(&[(ENV_API_KEY, API_KEY), (ENV_HEADERS, hardcoded_header.as_str())]),
            "硬编码",
        ),
        ("凭据含控制字符", ok(&[(ENV_API_KEY, "abcdefgh\r\nX-Evil: 1"), (ENV_HEADERS, "A: {{API_KEY}}")]), "注入"),
        // 🔴 送审文本一个占位符都没有 ⇒ 每次送空文本，厂商全回 pass。
        ("请求体不含文本占位符", ok(&[(ENV_BODY, r#"{"text":"hello"}"#)]), "空文本"),
        ("body 不是 JSON", ok(&[(ENV_BODY, "text={{TEXT}}")]), "合法 JSON"),
        ("请求头写错形式", ok(&[(ENV_HEADERS, "Authorization Bearer x")]), ENV_HEADERS),
        ("裁决路径缺失", ok(&[(ENV_VERDICT_PATH, "")]), ENV_VERDICT_PATH),
        ("裁决路径有空段", ok(&[(ENV_VERDICT_PATH, "data..suggestion")]), "空路径段"),
        ("过审值表为空", ok(&[(ENV_APPROVED_VALUES, "")]), "没有任何内容能过审"),
        // 🔴 一个永远拦不下任何东西的「真实 provider」。
        (
            "拦截值表全空",
            ok(&[(ENV_PENDING_VALUES, ""), (ENV_REJECTED_VALUES, "")]),
            "永远拦不下",
        ),
        ("同一标签跨表", ok(&[(ENV_PENDING_VALUES, "pass")]), "歧义"),
        ("超时不是数字", ok(&[(ENV_TIMEOUT_MS, "五秒")]), ENV_TIMEOUT_MS),
        ("超时越界", ok(&[(ENV_TIMEOUT_MS, "0")]), ENV_TIMEOUT_MS),
        ("截断长度不是数字", ok(&[(ENV_MAX_CHARS, "-1")]), ENV_MAX_CHARS),
        ("单价非正", ok(&[(ENV_PRICE_CENTS_PER_1K_CALLS, "0")]), ENV_PRICE_CENTS_PER_1K_CALLS),
        ("图片处置值不认识", ok(&[(ENV_IMAGE_FALLBACK, "ignore")]), ENV_IMAGE_FALLBACK),
    ];

    for (name, pairs, must_mention) in cases {
        let got = cfg_from(&pairs);
        let e = match got {
            Err(e) => e,
            Ok(other) => panic!("🔴「{name}」没有在启动时被拦下，得到 {:?}", other.is_some()),
        };
        assert!(
            e.contains(must_mention),
            "「{name}」的错误信息没提到「{must_mention}」，运维会不知道改哪个变量：{e}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑤ 请求模板：三种塞法
// ═══════════════════════════════════════════════════════════════════════════

/// 默认体 + 特殊字符：`{{TEXT}}` 走 JSON 序列化，引号 / 换行 / 中文都必须**自动转义**，
/// 服务端解出来要与原文逐字节相同（模板拼串最容易在这里炸）。
#[tokio::test]
async fn text_placeholder_is_json_safe() {
    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"pass"}"#.into())).await;
    let p = build(base_pairs(&format!("{}/check", m.base)));
    let nasty = "他说：\"退\"\n然后\\走了 —— 中文 & <tag>";
    assert_eq!(p.check_text(nasty).await.unwrap(), ModerationVerdict::Approved);

    let body: Value = serde_json::from_str(&m.last().body).expect("请求体必须是合法 JSON");
    assert_eq!(body["text"].as_str().unwrap(), nasty, "🔴 送审文本被模板拼串改坏了");
    assert_eq!(m.last().method, "POST");
    assert!(
        m.last().headers.iter().any(|(k, v)| k == "content-type" && v.contains("application/json")),
        "未指定时应当自动带上 JSON content-type"
    );
}

/// 阿里云那种「参数再 JSON 编码一层」的塞法：`{{TEXT_JSON}}`。
#[tokio::test]
async fn nested_json_placeholder_survives_double_encoding() {
    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"pass"}"#.into())).await;
    let pairs = with(
        base_pairs(&format!("{}/check", m.base)),
        ENV_BODY,
        r#"{"Service":"comment_detection","ServiceParameters":"{\"content\":\"{{TEXT_JSON}}\"}"}"#,
    );
    let p = build(pairs);
    let nasty = "带\"引号\"和\\反斜杠的正文";
    assert_eq!(p.check_text(nasty).await.unwrap(), ModerationVerdict::Approved);

    let outer: Value = serde_json::from_str(&m.last().body).expect("外层是 JSON");
    let inner: Value = serde_json::from_str(outer["ServiceParameters"].as_str().expect("内层是字符串"))
        .expect("🔴 内层没能解成 JSON —— 双层编码被拼串破坏了");
    assert_eq!(inner["content"].as_str().unwrap(), nasty);
}

/// 腾讯云那种 base64 塞法。
#[tokio::test]
async fn base64_placeholder_encodes_utf8_bytes() {
    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"pass"}"#.into())).await;
    let pairs =
        with(base_pairs(&format!("{}/check", m.base)), ENV_BODY, r#"{"Content":"{{TEXT_BASE64}}"}"#);
    let p = build(pairs);
    assert_eq!(p.check_text("中文正文").await.unwrap(), ModerationVerdict::Approved);

    let body: Value = serde_json::from_str(&m.last().body).unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body["Content"].as_str().unwrap())
        .expect("必须是合法 base64");
    assert_eq!(String::from_utf8(decoded).unwrap(), "中文正文");
}

/// GET + 把文本放进 query：URL 里的占位符必须**百分号编码**，否则空格 / `&` / 中文会把请求打散。
#[tokio::test]
async fn get_with_text_in_query_is_percent_encoded() {
    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"pass"}"#.into())).await;
    let pairs = base_pairs(&format!("{}/check?q={{{{TEXT}}}}", m.base));
    let pairs = with(pairs, ENV_METHOD, "GET");
    let p = build(pairs);
    assert_eq!(p.check_text("a b&c=中文").await.unwrap(), ModerationVerdict::Approved);

    let got = m.last();
    assert_eq!(got.method, "GET");
    assert!(got.body.is_empty(), "GET 不该带体");
    assert!(got.uri.contains("q=a%20b%26c%3D%E4%B8%AD%E6%96%87"), "URI 未百分号编码：{}", got.uri);
}

/// 自定义请求头照发，且不会覆盖运维自己给的 content-type。
#[tokio::test]
async fn custom_headers_are_sent_verbatim() {
    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"pass"}"#.into())).await;
    let pairs = with(
        base_pairs(&format!("{}/check", m.base)),
        ENV_HEADERS,
        "X-Tenant: acme\nContent-Type: application/json; charset=utf-8",
    );
    let p = build(pairs);
    p.check_text("x").await.unwrap();
    let h = m.last().headers;
    assert!(h.iter().any(|(k, v)| k == "x-tenant" && v == "acme"));
    assert!(h.iter().any(|(k, v)| k == "content-type" && v.contains("charset=utf-8")));
}

/// 截断：默认 0 = 不截断（让厂商自己的长度上限去拒绝，那条拒绝是一次 Err ⇒ fail-closed）。
#[tokio::test]
async fn truncation_is_off_by_default_and_cuts_on_char_boundary() {
    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"pass"}"#.into())).await;
    let long = "中".repeat(50);

    let p = build(base_pairs(&format!("{}/check", m.base)));
    p.check_text(&long).await.unwrap();
    let sent: Value = serde_json::from_str(&m.last().body).unwrap();
    assert_eq!(sent["text"].as_str().unwrap().chars().count(), 50, "默认不得截断");

    let p = build(with(base_pairs(&format!("{}/check", m.base)), ENV_MAX_CHARS, "10"));
    p.check_text(&long).await.unwrap();
    let sent: Value = serde_json::from_str(&m.last().body).unwrap();
    // 按**字符**而不是字节截断：按字节会把 UTF-8 切碎。
    assert_eq!(sent["text"].as_str().unwrap(), "中".repeat(10));
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑥ 响应映射
// ═══════════════════════════════════════════════════════════════════════════

/// 三档标签各自映射到位，且大小写 / 前后空白不敏感（厂商文档里的大小写并不统一）。
#[tokio::test]
async fn labels_map_to_all_three_verdicts() {
    for (label, want) in [
        ("pass", ModerationVerdict::Approved),
        ("PASS", ModerationVerdict::Approved),
        (" review ", ModerationVerdict::Pending),
        ("Block", ModerationVerdict::Rejected),
    ] {
        let m = Mock::spawn(Reply::Fixed(200, serde_json::json!({ "suggestion": label }).to_string()))
            .await;
        let p = build(base_pairs(&format!("{}/check", m.base)));
        assert_eq!(p.check_text("x").await.unwrap(), want, "标签「{label}」");
    }
}

/// 数值型标签（0/1/2 这类）同样可映射——响应里的 `Number` 会被渲染成文本再比对。
#[tokio::test]
async fn numeric_labels_are_mappable() {
    let m = Mock::spawn(Reply::Fixed(200, r#"{"data":{"conclusionType":2}}"#.into())).await;
    let pairs = base_pairs(&format!("{}/check", m.base));
    let pairs = with(pairs, ENV_VERDICT_PATH, "data.conclusionType");
    let pairs = with(pairs, ENV_APPROVED_VALUES, "1");
    let pairs = with(pairs, ENV_REJECTED_VALUES, "2");
    let pairs = with(pairs, ENV_PENDING_VALUES, "3");
    assert_eq!(build(pairs).check_text("x").await.unwrap(), ModerationVerdict::Rejected);
}

/// `*` 通配数组：多个标签命中时**取最严**（保守方向），且结果与元素顺序无关
/// ⇒ 不依赖任何数组/对象迭代序（确定性契约）。
#[tokio::test]
async fn wildcard_takes_the_most_severe_and_is_order_independent() {
    for arr in [r#"["pass","review","block"]"#, r#"["block","pass","review"]"#, r#"["review","block"]"#]
    {
        let body = format!(r#"{{"data":{{"results":{}}}}}"#, arr);
        let m = Mock::spawn(Reply::Fixed(200, body)).await;
        let pairs = with(base_pairs(&format!("{}/check", m.base)), ENV_VERDICT_PATH, "data.results.*");
        assert_eq!(
            build(pairs).check_text("x").await.unwrap(),
            ModerationVerdict::Rejected,
            "🔴 通配命中多个标签时必须取最严：{arr}"
        );
    }
    // 全过则过。
    let m = Mock::spawn(Reply::Fixed(200, r#"{"data":{"results":["pass","pass"]}}"#.into())).await;
    let pairs = with(base_pairs(&format!("{}/check", m.base)), ENV_VERDICT_PATH, "data.results.*");
    assert_eq!(build(pairs).check_text("x").await.unwrap(), ModerationVerdict::Approved);
}

/// 路径解析与下降的纯逻辑（不起服务，直接喂 JSON）。
#[test]
fn path_resolution_handles_indices_objects_and_wildcards() {
    let v: Value = serde_json::from_str(
        r#"{"data":{"list":[{"s":"pass"},{"s":"block"}],"map":{"b":"review","a":"pass"},"0":"numeric-key"}}"#,
    )
    .unwrap();
    let get = |p: &str| {
        resolve(&v, &parse_path(p).unwrap()).iter().filter_map(|x| scalar_text(x)).collect::<Vec<_>>()
    };
    assert_eq!(get("data.list.0.s"), vec!["pass"]);
    assert_eq!(get("data.list.*.s"), vec!["pass", "block"]);
    // 对象通配按键排序取值（把「不依赖 map 迭代序」写成代码）。
    assert_eq!(get("data.map.*"), vec!["pass", "review"]);
    // 数字段取不到数组时回落为同名对象键。
    assert_eq!(get("data.0"), vec!["numeric-key"]);
    assert!(get("data.nope.s").is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑦ 图片：不许继承直过默认实现
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 本模块只接**文本**审核 API。若它继承 `ModerationProvider::check_image` 的直过默认实现，
/// 就会造出一个自称 `is_dev_stub() == false`、却对图片一律放行的 provider——
/// 正是 `is_dev_stub` 注释里说的「一条纯占位的链路被读成已生效的防线」。
#[tokio::test]
async fn image_never_silently_passes() {
    let p = build(base_pairs("https://h.example.com/c"));
    assert_eq!(
        p.check_image(b"\x89PNG...").await.unwrap(),
        ModerationVerdict::Pending,
        "🔴 默认必须进人审，绝不能沿用 trait 的直过默认实现"
    );
    // 图审由平台外流程覆盖时才可显式放开（等于承认图片没有机审）。
    let p = build(with(base_pairs("https://h.example.com/c"), ENV_IMAGE_FALLBACK, "approved"));
    assert_eq!(p.check_image(b"x").await.unwrap(), ModerationVerdict::Approved);
    // 最严档。
    let p = build(with(base_pairs("https://h.example.com/c"), ENV_IMAGE_FALLBACK, "error"));
    assert!(p.check_image(b"x").await.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑧ 🔴 is_dev_stub 翻面 + 成本比值（端到端，走真实端点）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **配置生效 ⇒ `safety_recheck_runs.provider_stub` 与运营面 `honesty[]` 自动翻面**，
/// 领域代码一行不动。
///
/// 这条走的是完整链路：本地 mock 审核服务 → `HttpModerationProvider` → 第 3 层复核 →
/// 落 `safety_recheck_runs` → `GET /api/admin/safety/recheck`。
#[tokio::test]
async fn configured_provider_flips_the_stub_fact_end_to_end() {
    use crate::safety::semantic::testkit as l3;

    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"block"}"#.into())).await;
    let provider = build(base_pairs(&format!("{}/check", m.base)));
    assert!(!provider.is_dev_stub(), "🔴 接了真实服务商的实现必须显式覆写为 false");

    let mut state = crate::safety::testkit::test_state().await;
    state.moderation = Arc::new(provider);
    l3::seed_running_world(&state).await;
    l3::enable(&state.db).await;
    l3::seed_tick(&state, "w1", 0, "会被拦下的正文").await;

    let report = crate::safety::semantic::run_recheck(&state, &l3::job("w1", 0, 1)).await.unwrap();
    assert_eq!(report.tightened, 1, "mock 回 block ⇒ 该事件应被收紧");

    // 载体 1：台账表一等列。
    let stub: i64 = sqlx::query_scalar("SELECT CAST(provider_stub AS BIGINT) FROM safety_recheck_runs")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(stub, 0, "🔴 provider_stub 那一列没有翻面");

    // 载体 2：风控留痕。
    let detail: String =
        sqlx::query_scalar("SELECT detail_json FROM risk_events WHERE kind = 'semantic'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["providerStub"], serde_json::json!(false));

    // 载体 3：运营面响应 + 诚实边界数组。
    let body = l3::admin_recheck(&state).await;
    assert_eq!(body["providerStub"], serde_json::json!(false));
    assert_eq!(body["source"], serde_json::json!("production"));
    let joined = body["honesty"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("");
    assert!(joined.contains("真实 ModerationProvider"), "{joined}");
    assert!(joined.contains("不等于「已验证」"), "🔴 接上了仍不等于验证过：{joined}");
}

/// 🔴 **接上真实 provider 之后，第 3 层的 fail 方向一个字符都没变**：窗口期重试 fail-open →
/// 预算耗尽 fail-closed。
///
/// 上面 `red_line_every_failure_mode_errors_never_approves` 证的是 provider 自己不吞错误；
/// 这条证的是**那条错误确实一路走到了收紧**——两者之间还隔着一个 `check_with_timeout` 与
/// 一个重试计数器，只测其中一头会漏掉这段接缝。
#[tokio::test]
async fn provider_outage_still_ends_in_fail_closed_through_the_real_seam() {
    use crate::safety::semantic::testkit as l3;

    let m = Mock::spawn(Reply::Fixed(503, "service unavailable".into())).await;
    let mut state = crate::safety::testkit::test_state().await;
    state.moderation = Arc::new(build(base_pairs(&format!("{}/check", m.base))));
    l3::seed_running_world(&state).await;
    l3::enable(&state.db).await;
    l3::seed_tick(&state, "w1", 0, "正文").await;

    // ① 窗口期（第 1 次尝试）：重排重试，内容**仍然外发**。
    let r = crate::safety::semantic::run_recheck(&state, &l3::job("w1", 0, 1)).await.unwrap();
    assert_eq!(r.provider_errors, 1);
    assert_eq!(r.failed_closed, 0, "窗口期内不收紧");
    let m1: String = sqlx::query_scalar("SELECT moderation FROM world_events")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(m1, "approved", "重试窗口内内容仍正常外发（fail-open）");

    // ② 预算耗尽（尝试数远超上限）：收紧为 pending + 无条件入人审。
    let r = crate::safety::semantic::run_recheck(&state, &l3::job("w1", 0, 99)).await.unwrap();
    assert_eq!(r.failed_closed, 1, "🔴 预算耗尽后必须 fail-closed");
    let m2: String = sqlx::query_scalar("SELECT moderation FROM world_events")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(m2, "pending", "🔴 给不出裁决 ≠ 放行");
    assert_ne!(m2, "rejected", "「机器没能判定」不该被记成终判");
}

/// 成本比值：**两侧单价都显式配置**才翻成 true；缺任一半仍是 false 并说明缺哪一半。
///
/// 🔴 不摆一个假的 5%：厂商响应里没有计价字段（按调用次数离线结算），所以补的是
/// 「运营填的合同单价」，且分母侧同样只认显式配置（不回落代码内的默认估算）。
#[tokio::test]
async fn cost_ratio_needs_both_unit_prices_declared() {
    use crate::safety::semantic::testkit as l3;

    // ① 只有审核侧单价 ⇒ 仍算不出（生成侧单价在本用例进程里未显式配置）。
    let m = Mock::spawn(Reply::Fixed(200, r#"{"suggestion":"pass"}"#.into())).await;
    let priced = build(with(
        base_pairs(&format!("{}/check", m.base)),
        ENV_PRICE_CENTS_PER_1K_CALLS,
        "30",
    ));
    assert_eq!(priced.call_price_cents_per_1k(), Some(30));

    let mut state = crate::safety::testkit::test_state().await;
    state.moderation = Arc::new(priced);
    l3::seed_running_world(&state).await;
    l3::enable(&state.db).await;
    l3::seed_tick(&state, "w1", 0, "正文").await;
    crate::safety::semantic::run_recheck(&state, &l3::job("w1", 0, 1)).await.unwrap();

    let body = l3::admin_recheck(&state).await;
    let cost = &body["cost"];
    assert_eq!(cost["moderationUnitPriceCentsPer1kCalls"], serde_json::json!(30));
    assert_eq!(
        cost["ratioAvailable"],
        serde_json::json!(false),
        "🔴 只有一半单价时不得给出比值"
    );
    assert!(cost["why"].as_str().unwrap().contains("生成侧"), "必须说清缺的是哪一半：{cost}");
    assert!(cost["why"].as_str().unwrap().contains("5%"));
    assert!(cost.as_object().unwrap().get("ratioBp").is_none());

    // ② Dev 桩连审核侧单价都没有 ⇒ 缺两半（桩的调用成本恒为 0，比值毫无意义）。
    let mut state = crate::safety::testkit::test_state().await;
    state.moderation = Arc::new(crate::providers::DevModeration::default());
    assert_eq!(state.moderation.call_price_cents_per_1k(), None);
    l3::seed_running_world(&state).await;
    let body = l3::admin_recheck(&state).await;
    assert_eq!(body["cost"]["ratioAvailable"], serde_json::json!(false));
    assert_eq!(body["cost"]["moderationUnitPriceCentsPer1kCalls"], Value::Null);
}

/// 比值本身的算法：整数分 + 万分比整数，**禁浮点**（金额与门槛判定必须逐位可复现）。
#[test]
fn cost_math_is_integer_only() {
    use crate::safety::semantic::testkit as l3;
    // 1000 次调用 × 30 分/千次 = 30 分；100_000 token × 2 分/千 token = 200 分；30/200 = 1500bp = 15%。
    assert_eq!(l3::cost_cents(1_000, 30), 30);
    assert_eq!(l3::cost_cents(100_000, 2), 200);
    assert_eq!(l3::threshold_bp(), 500, "T5 门槛 5% 写成万分比整数");
}
