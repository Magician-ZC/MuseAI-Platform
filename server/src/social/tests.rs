//! 真人社交解锁测试（sqlite::memory + oneshot 真实路由）。总规格 §14【拍板 22】恨隔面具原则。覆盖：
//!
//! - **默认关闭**：`all_endpoints()` 列的全部端点一律 404，且**一行都不落库**（前门 + 状态侧双保险）；
//! - 🔴 **青少年模式限真人社交是服务端拒绝**（红线）：未声明/未成年/无用户行三种账号调用
//!   五个身份端点全 403，且**全库逐字节快照相等**（零副作用：不落幂等键、不发通知、不改任何表）；
//! - 🔴 **对端未成年同样拒绝**，且拒绝文案与"被拉黑""不够格"**逐字相同**（不得成为年龄探测器）；
//! - 🔴 **敌对线永久匿名**：达敌对判据即一票否决，「一起死过」也不豁免；
//! - 🔴 **社交资产零数值影响**（红线）：走完全套流程后资产/进度/世界线九张表逐字节快照相等；
//! - 🔴 **源码级红线**：本模块的写入目标只能是三张 social 表 + 风控/审计留痕表，
//!   且源码里不出现任何资产/进度标识符；
//! - **拉黑有服务端实效**：撤销已授予身份 → 身份读不出 → 对方发不出解锁请求 → 连房间邀请也发不出；
//! - **双向自愿**：pending 期间双方都看不到对方真身；拒绝即终局，不能再问一次；
//! - **举报进队列 + 阈值升级 + 运营处置留痕**；
//! - **参数化**：阈值/配额/开关全部经 env 回落规则单测（纯函数，不摆布进程 env）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::{Column, Row};
use tower::ServiceExt;

use super::*;
use crate::safety::testkit::{seed_member, seed_user, seed_world, test_state, token};

// ═══════════════════════════════════════════════════════════════════════════
// 脚手架
// ═══════════════════════════════════════════════════════════════════════════

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    tk: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let app = crate::app::build_router(state.clone());
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tk}"));
    let request = match body {
        Some(b) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

/// `ApiError::Conflict(m)` 的线上渲染形状（`error.rs`：`"状态冲突: {m}"`）。
/// 断言渲染后的整句而不是内层常量——**用户看到的是这一句**，它才是"无法区分原因"的载体。
fn refuse_line() -> String {
    format!("状态冲突: {REFUSE_GENERIC}")
}

fn admin_token(state: &AppState, role: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, &format!("adm_{role}"), role, 3600).unwrap()
}

/// 🔴 开闸走 **`runtime_flags` DB 记录**而不是 env：env 是进程级的，本模块用例与其它模块同属
/// 一个测试二进制、默认并发跑，改 env 必须共用全局锁才不串味。写一条 DB 记录只影响本用例
/// 自己的内存库，天然无需加锁（范式抄 `annotations::tests::open_flag`）。
async fn open_flag(state: &AppState, scope: &str, target: &str) {
    sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) \
         VALUES ($1, $2, $3, $4, 1, 0, 0, 'test', $5, '用例开闸', $6)",
    )
    .bind(new_id("rf"))
    .bind(ENV_SOCIAL_IDENTITY_UNLOCK)
    .bind(scope)
    .bind(target)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    crate::flags::invalidate(&state.db);
}

async fn seed_char(state: &AppState, id: &str, owner: &str, name: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES ($1, $2, 'loc', 1, $3, 'original', 'approved', 0, $4)",
    )
    .bind(id)
    .bind(owner)
    .bind(json!({ "identity": { "name": name } }).to_string())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 写世界的关系图（引擎 `relation_dynamics` 的产出形状）。
async fn set_relations(state: &AppState, world_id: &str, relations: Value) {
    sqlx::query("UPDATE worlds SET narrative_state_json = $1 WHERE id = $2")
        .bind(json!({ "relations": relations }).to_string())
        .bind(world_id)
        .execute(&state.db)
        .await
        .unwrap();
}

/// 造一条**双向正向羁绊**（替代原先用「一起死过」当资格的写法）。
///
/// ⚠️ 2026-07-29 起「一起死过」这条资格路径**已删除**（随 memorial 整块移除，
/// 见 `social/mod.rs` 里那段说明）。现在唯一的资格路径是正向羁绊达阈值，
/// 所以测试构造资格也只能走这条——**这不是等价替换，是口径变了**。
async fn set_positive_bond(state: &AppState, world_id: &str, a: &str, b: &str) {
    set_relations(
        state,
        world_id,
        json!([
            { "from": a, "to": b, "trust": 0.9, "affinity": 0.9 },
            { "from": b, "to": a, "trust": 0.9, "affinity": 0.9 },
        ]),
    )
    .await;
}

/// 改年龄声明（0 未声明 / 1 成年 / 2 未成年）。
async fn set_age(state: &AppState, user_id: &str, age: i64) {
    sqlx::query("UPDATE users SET age_declared = $1 WHERE id = $2")
        .bind(age)
        .bind(user_id)
        .execute(&state.db)
        .await
        .unwrap();
}

/// 两个成年用户 u1/u2，各一张卡 c1/c2，同在世界 w1。
async fn base_world(state: &AppState) {
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_world(&state.db, "w1", 1, "running").await;
    seed_char(state, "c1", "u1", "青梧").await;
    seed_char(state, "c2", "u2", "白露").await;
    seed_member(&state.db, "m1", "w1", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w1", "u2", "c2", "active").await;
}

async fn dump_table(db: &AnyPool, table: &str) -> String {
    let rows = sqlx::query(&format!("SELECT * FROM {table}")).fetch_all(db).await.unwrap();
    let mut out: Vec<String> = Vec::with_capacity(rows.len());
    for r in &rows {
        let mut cells: Vec<String> = Vec::new();
        for (i, col) in r.columns().iter().enumerate() {
            let name = col.name();
            let cell = if let Ok(v) = r.try_get::<Option<String>, _>(i) {
                format!("{name}=s{v:?}")
            } else if let Ok(v) = r.try_get::<Option<i64>, _>(i) {
                format!("{name}=i{v:?}")
            } else if let Ok(v) = r.try_get::<Option<f64>, _>(i) {
                format!("{name}=f{v:?}")
            } else {
                format!("{name}=<undecodable>")
            };
            cells.push(cell);
        }
        out.push(cells.join("|"));
    }
    out.sort();
    out.join("\n")
}

async fn dump(db: &AnyPool, tables: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for t in tables {
        out.push(((*t).to_string(), dump_table(db, t).await));
    }
    out
}

/// 本模块**可能**写到的全部表 —— 「零副作用」断言扫的就是这一批（含幂等键与通知 outbox：
/// 拒绝若发生在它们之后，快照就会不等，从而暴露"拒绝但已留下痕迹"）。
const SIDE_EFFECT_TABLES: &[&str] = &[
    "social_unlock_requests",
    "social_blocks",
    "social_reports",
    "risk_events",
    "audit_logs",
    "idempotency_keys",
    "notification_outbox",
];

/// 🔴 资产 / 进度 / 世界线表 —— 社交资产「我们的角色一起死过」绝不能碰的那一批。
const ASSET_TABLES: &[&str] = &[
    "cloud_characters",   // mileage / withdrawn
    "backpacks",          // 道具
    "users",              // card_slots
    "world_contributions",// 三层结算 ③ 贡献账本
    "subplot_cards",      // 副本卡
    "worlds",             // narrative_state_json（引擎回灌面）
    "world_events",       // 公共事实
    "world_members",      // 足迹
];

/// 玩家面 + 运营面全部端点（method, uri, body, 是否 admin token）。
fn all_endpoints() -> Vec<(&'static str, String, Option<Value>, bool)> {
    vec![
        ("GET", "/api/worlds/w1/social/bonds".into(), None, false),
        (
            "POST",
            "/api/worlds/w1/social/unlock-requests".into(),
            Some(json!({ "targetCharacterId": "c2" })),
            false,
        ),
        ("GET", "/api/me/social/unlock-requests".into(), None, false),
        (
            "POST",
            "/api/me/social/unlock-requests/sul_x/respond".into(),
            Some(json!({ "accept": true })),
            false,
        ),
        ("GET", "/api/me/social/identities".into(), None, false),
        ("GET", "/api/me/social/blocks".into(), None, false),
        ("POST", "/api/me/social/blocks".into(), Some(json!({ "characterId": "c2" })), false),
        ("DELETE", "/api/me/social/blocks/sbk_x".into(), None, false),
        (
            "POST",
            "/api/me/social/reports".into(),
            Some(json!({ "subjectKind": "character", "subjectId": "c2", "category": "harassment" })),
            false,
        ),
        ("GET", "/api/admin/social/reports".into(), None, true),
        ("GET", "/api/admin/social/reports/summary".into(), None, true),
        (
            "POST",
            "/api/admin/social/reports/srp_x/resolve".into(),
            Some(json!({ "action": "dismissed", "reason": "测试" })),
            true,
        ),
    ]
}

/// 身份相关端点（受青少年门约束的那一批；拉黑/举报**不在其中**，见模块头 ③）。
fn identity_endpoints() -> Vec<(&'static str, String, Option<Value>)> {
    vec![
        ("GET", "/api/worlds/w1/social/bonds".into(), None),
        (
            "POST",
            "/api/worlds/w1/social/unlock-requests".into(),
            Some(json!({ "targetCharacterId": "c2" })),
        ),
        ("GET", "/api/me/social/unlock-requests".into(), None),
        (
            "POST",
            "/api/me/social/unlock-requests/sul_x/respond".into(),
            Some(json!({ "accept": true })),
        ),
        ("GET", "/api/me/social/identities".into(), None),
    ]
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 红线①：未验证功能默认关闭
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn red_line_disabled_by_default_all_endpoints_404_and_no_side_effect() {
    let state = test_state().await;
    base_world(&state).await;
    let tk = token(&state, "u1");
    let atk = admin_token(&state, "admin");

    let before = dump(&state.db, SIDE_EFFECT_TABLES).await;
    for (method, uri, body, is_admin) in all_endpoints() {
        let t = if is_admin { &atk } else { &tk };
        let (status, _) = send(&state, method, &uri, t, body).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "🔴 VALIDATION §0.1：开关默认关闭时 {method} {uri} 必须 404（功能不存在）"
        );
    }
    let after = dump(&state.db, SIDE_EFFECT_TABLES).await;
    assert_eq!(before, after, "🔴 关闭态必须零副作用：一行都不落库");

    // 登记表侧：默认值为关，且迁移不插种子数据。
    assert!(!crate::flags::declared_default(ENV_SOCIAL_IDENTITY_UNLOCK));
    assert!(!social_enabled(&state.db, Some("u1"), Some("w1")).await);
}

#[tokio::test]
async fn flag_scope_world_opens_world_endpoints_only() {
    // 文档化的运营须知（模块头）：只按 world 灰度时，玩家能在该世界发起，却读不到 /me 收件箱。
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "world", "w1").await;
    let tk = token(&state, "u1");

    let (s1, _) = send(&state, "GET", "/api/worlds/w1/social/bonds", &tk, None).await;
    assert_eq!(s1, StatusCode::OK, "world 作用域开闸后，世界维度端点可用");
    let (s2, _) = send(&state, "GET", "/api/me/social/identities", &tk, None).await;
    assert_eq!(s2, StatusCode::NOT_FOUND, "账户维度端点无 world 坐标 → 仍按全局解析（关）");
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 红线②：青少年模式限真人社交 —— **服务端拒绝 + 零副作用**
// ═══════════════════════════════════════════════════════════════════════════

/// 三种非成年账号（未声明 0 / 未成年 2 / 用户行缺失）调用五个身份端点全部 403，
/// 且**全库逐字节快照相等**——没有落幂等键、没有发通知、没有改任何一张表。
#[tokio::test]
async fn red_line_minor_rejected_by_server_with_zero_side_effect() {
    for (label, age) in [("未声明", Some(0_i64)), ("未成年", Some(2)), ("无用户行", None)] {
        let state = test_state().await;
        base_world(&state).await;
        open_flag(&state, "global", "").await;

        let actor = match age {
            Some(a) => {
                set_age(&state, "u1", a).await;
                "u1".to_string()
            }
            // 用户行缺失：签一个库里根本没有的 user 的 token（fail-closed 必须挡住它）。
            None => "ghost".to_string(),
        };
        let tk = token(&state, &actor);

        let before = dump(&state.db, SIDE_EFFECT_TABLES).await;
        for (method, uri, body) in identity_endpoints() {
            let (status, _) = send(&state, method, &uri, &tk, body).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "🔴 真红线 §0.4：{label}账号调用 {method} {uri} 必须被**服务端**拒绝（403）"
            );
        }
        let after = dump(&state.db, SIDE_EFFECT_TABLES).await;
        assert_eq!(before, after, "🔴 {label}账号被拒后必须零副作用（含幂等键与通知 outbox）");
    }
}

/// ⚠️ 与上一条互补：拉黑 / 举报是**保护工具**，未成年同样可用。
/// 把它们一并关掉是把"保护未成年"做成了"让未成年无法自保"。
#[tokio::test]
async fn minor_can_still_block_and_report() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    set_age(&state, "u1", 2).await; // 未成年
    let tk = token(&state, "u1");

    let (s1, _) = send(
        &state,
        "POST",
        "/api/me/social/blocks",
        &tk,
        Some(json!({ "characterId": "c2", "reason": "对方一直私聊我" })),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "未成年必须能拉黑");

    let (s2, _) = send(
        &state,
        "POST",
        "/api/me/social/reports",
        &tk,
        Some(json!({ "subjectKind": "character", "subjectId": "c2", "category": "minor_risk" })),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "未成年必须能举报");

    let (s3, _) = send(&state, "GET", "/api/me/social/blocks", &tk, None).await;
    assert_eq!(s3, StatusCode::OK, "未成年必须能查看自己的黑名单");
}

/// 🔴 对端未成年同样拒绝，且拒绝文案与「被拉黑」「不够格」**逐字相同**——
/// 否则解锁端点就成了「探测任意用户是否未成年」的接口。
#[tokio::test]
async fn red_line_minor_target_refused_with_indistinguishable_message() {
    let state = test_state().await;
    base_world(&state).await;
    seed_user(&state.db, "u3").await;
    seed_char(&state, "c3", "u3", "阿箬").await;
    seed_member(&state.db, "m3", "w1", "u3", "c3", "active").await;
    open_flag(&state, "global", "").await;
    // c2/c3 都与 c1 有达标的正向羁绊（够格），差别只在年龄与拉黑。
    // （原先用「一起死过」当资格，该路径已随 memorial 删除。）
    set_relations(
        &state,
        "w1",
        json!([
            { "from": "c1", "to": "c2", "trust": 0.9, "affinity": 0.9 },
            { "from": "c2", "to": "c1", "trust": 0.9, "affinity": 0.9 },
            { "from": "c1", "to": "c3", "trust": 0.9, "affinity": 0.9 },
            { "from": "c3", "to": "c1", "trust": 0.9, "affinity": 0.9 },
        ]),
    )
    .await;
    set_age(&state, "u2", 2).await; // 对端未成年
    let tk1 = token(&state, "u1");

    // ① 对端未成年 → 拒绝。
    let (s_minor, b_minor) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    assert_eq!(s_minor, StatusCode::CONFLICT);
    assert_eq!(b_minor["error"]["message"], refuse_line());
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM social_unlock_requests")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "🔴 对端未成年被拒时不得落任何解锁请求行");

    // ② 对端成年但已互相拉黑 → 拒绝，文案必须**逐字相同**。
    send(
        &state,
        "POST",
        "/api/me/social/blocks",
        &tk1,
        Some(json!({ "characterId": "c3" })),
    )
    .await;
    let (s_blocked, b_blocked) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c3" })),
    )
    .await;
    assert_eq!(s_blocked, StatusCode::CONFLICT);
    assert_eq!(
        b_blocked["error"]["message"], b_minor["error"]["message"],
        "🔴 未成年拒绝与拉黑拒绝的文案必须无法区分（否则端点即年龄探测器）"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 红线③：社交资产「我们的角色一起死过」零数值影响
// ═══════════════════════════════════════════════════════════════════════════

/// 走完**全套**社交流程（自查 → 发起 → 接受 → 读身份 → 拉黑 → 举报）之后，
/// 资产 / 进度 / 世界线九张表**逐字节快照相等**。
///
/// 这条断言的强度在于它是运行时级而非源码级：即便将来有人在某条路径上偷偷加了一次
/// `mileage + 1`，快照也会当场不等。
#[tokio::test]
async fn red_line_social_asset_has_zero_numeric_effect() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    // 造出达标的正向羁绊（唯一还剩的资格路径）。
    set_positive_bond(&state, "w1", "c1", "c2").await;
    // 顺带造一些资产，确保快照不是在空表上比较。
    sqlx::query("UPDATE cloud_characters SET mileage = 777 WHERE id = 'c1'")
        .execute(&state.db)
        .await
        .unwrap();
    crate::safety::testkit::seed_backpack(&state.db, "bp1", "u1", "item1", "owned", None).await;

    let tk1 = token(&state, "u1");
    let tk2 = token(&state, "u2");

    let before = dump(&state.db, ASSET_TABLES).await;

    // ① 自查：凭证存在且够格。
    let (s, body) = send(&state, "GET", "/api/worlds/w1/social/bonds", &tk1, None).await;
    assert_eq!(s, StatusCode::OK);
    let bond = &body["bonds"][0];
    assert_eq!(bond["paths"]["positiveBond"], json!(true), "双向正向羁绊达阈值 → 资格成立");
    assert_eq!(bond["eligible"], json!(true), "正向羁绊是当前唯一的资格路径");

    // ② 发起 → ③ 接受。
    let (s, req) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{req:?}");
    let rid = req["id"].as_str().unwrap().to_string();
    let (s, _) = send(
        &state,
        "POST",
        &format!("/api/me/social/unlock-requests/{rid}/respond"),
        &tk2,
        Some(json!({ "accept": true })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // ④ 读真人身份 → ⑤ 拉黑 → ⑥ 举报。
    send(&state, "GET", "/api/me/social/identities", &tk1, None).await;
    send(&state, "POST", "/api/me/social/blocks", &tk2, Some(json!({ "characterId": "c1" }))).await;
    send(
        &state,
        "POST",
        "/api/me/social/reports",
        &tk2,
        Some(json!({ "subjectKind": "character", "subjectId": "c1", "category": "harassment" })),
    )
    .await;

    let after = dump(&state.db, ASSET_TABLES).await;
    assert_eq!(
        before, after,
        "🔴 平台红线①：「我们的角色一起死过」是关系凭证，对历练 / 卡位 / 背包 / 副本卡 / \
         贡献账本 / 世界线**一律零影响**"
    );
}

/// 🔴 源码级红线：本模块的写入目标只能是三张 social 表 + 风控/审计留痕表，
/// 且源码里不出现任何资产 / 进度标识符。
///
/// 与上一条运行时快照互补：快照证明"这次流程没改"，源码扫描证明"没有任何一条路径能改"。
#[test]
fn red_line_module_writes_only_social_tables() {
    let src = include_str!("mod.rs");
    // 注释整行剔除（红线说明本身要能自由地写出这些词）。
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    /// 允许写入的表。
    const WRITE_ALLOWLIST: &[&str] = &[
        "social_unlock_requests",
        "social_blocks",
        "social_reports",
        // 举报累计升级 → 复用既有风控面；运营处置 → 审计留痕。两者都不是资产。
        "risk_events",
        "audit_logs",
    ];

    for verb in ["INSERT INTO ", "UPDATE ", "DELETE FROM "] {
        let mut rest = code.as_str();
        while let Some(pos) = rest.find(verb) {
            rest = &rest[pos + verb.len()..];
            let table: String =
                rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            assert!(
                WRITE_ALLOWLIST.contains(&table.as_str()),
                "🔴 social 模块出现了对 `{table}` 的 {verb}写入 —— 只允许写 {WRITE_ALLOWLIST:?}"
            );
        }
    }

    // 资产 / 进度 / 引擎回灌面的标识符一个都不许出现在代码里（读也不行的那几个，
    // 因为一旦读得到就迟早有人顺手写）。
    for forbidden in [
        "mileage",
        "backpacks",
        "card_slots",
        "subplot_cards",
        "world_contributions",
        "grant_item_tx",
        "grant_mileage_tx",
        "billing_balances",
        "ledger_postings",
        "user_entitlements",
    ] {
        assert!(
            !code.contains(forbidden),
            "🔴 social 模块不得出现资产/进度标识符 `{forbidden}`（社交资产不是数值）"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 红线④：敌对线永久匿名
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn red_line_hostile_line_never_unlocks_no_matter_what() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    // 关系是宿敌 —— 敌对判据一票否决，任何正向路径都不豁免。
    set_relations(&state, "w1", json!([{ "from": "c1", "to": "c2", "trust": -0.9, "affinity": -0.8 }]))
        .await;
    let tk1 = token(&state, "u1");

    let (_, body) = send(&state, "GET", "/api/worlds/w1/social/bonds", &tk1, None).await;
    let bond = &body["bonds"][0];
    assert_eq!(bond["hostile"], json!(true));
    assert_eq!(bond["eligible"], json!(false), "🔴 §14：敌对线永久匿名，一票否决");
    assert!(
        bond["blockers"].as_array().unwrap().iter().any(|b| b == BLOCK_HOSTILE),
        "拒绝原因应明示为敌对线"
    );

    let (s, b) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b["error"]["message"], refuse_line());
}

/// 接受时**用当下数据重算**：发起后翻脸成敌对线 → 接受被拒，且状态仍留在 pending。
#[tokio::test]
async fn hostile_after_request_blocks_acceptance() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    set_relations(&state, "w1", json!([{ "from": "c1", "to": "c2", "trust": 0.9, "affinity": 0.9 }]))
        .await;
    let (tk1, tk2) = (token(&state, "u1"), token(&state, "u2"));

    let (s, req) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{req:?}");
    let rid = req["id"].as_str().unwrap().to_string();

    // 世界继续跑，关系翻脸。
    set_relations(&state, "w1", json!([{ "from": "c1", "to": "c2", "trust": -0.9 }])).await;

    let (s, _) = send(
        &state,
        "POST",
        &format!("/api/me/social/unlock-requests/{rid}/respond"),
        &tk2,
        Some(json!({ "accept": true })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "🔴 接受时必须按当下关系重算，快照不作数");
    let status: String =
        sqlx::query_scalar("SELECT status FROM social_unlock_requests WHERE id = $1")
            .bind(&rid)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(status, UNLOCK_PENDING, "重算不通过不改状态（关系还可能回暖）");
}

// ═══════════════════════════════════════════════════════════════════════════
// 双向自愿 / 面具不泄露
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn identity_hidden_until_both_consent_then_declined_is_terminal() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    set_relations(&state, "w1", json!([{ "from": "c1", "to": "c2", "trust": 0.8 }])).await;
    let (tk1, tk2) = (token(&state, "u1"), token(&state, "u2"));

    let (_, req) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    let rid = req["id"].as_str().unwrap().to_string();

    // pending 期间：双方的身份面都是空的。
    for tk in [&tk1, &tk2] {
        let (_, ids) = send(&state, "GET", "/api/me/social/identities", tk, None).await;
        assert_eq!(ids["identities"].as_array().unwrap().len(), 0, "🔴 未双向同意前不下发任何真人身份");
    }
    // 收件箱只有面具名，不含任何真人标识。
    let (_, inbox) = send(&state, "GET", "/api/me/social/unlock-requests", &tk2, None).await;
    let raw = inbox.to_string();
    assert!(raw.contains("青梧"), "应展示发起人的角色面具名");
    assert!(!raw.contains("u1"), "🔴 §14：收件箱不得出现发起人的真人 id");
    assert!(inbox["requests"][0].get("fromUserId").is_none());

    // 拒绝 → 终局，且不能再问一次。
    let (s, b) = send(
        &state,
        "POST",
        &format!("/api/me/social/unlock-requests/{rid}/respond"),
        &tk2,
        Some(json!({ "accept": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["status"], UNLOCK_DECLINED);

    let (s, _) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "🔴 拒绝即终局：真人身份只问一次，不给反复施压的空间");
}

#[tokio::test]
async fn accepted_unlock_exposes_only_user_id_and_nickname() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    sqlx::query("UPDATE users SET nickname = '露露', phone = '13800000002' WHERE id = 'u2'")
        .execute(&state.db)
        .await
        .unwrap();
    set_relations(&state, "w1", json!([{ "from": "c1", "to": "c2", "trust": 0.8 }])).await;
    let (tk1, tk2) = (token(&state, "u1"), token(&state, "u2"));

    let (_, req) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    let rid = req["id"].as_str().unwrap().to_string();
    send(
        &state,
        "POST",
        &format!("/api/me/social/unlock-requests/{rid}/respond"),
        &tk2,
        Some(json!({ "accept": true })),
    )
    .await;

    let (s, ids) = send(&state, "GET", "/api/me/social/identities", &tk1, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ids["identities"][0]["identity"]["userId"], "u2");
    assert_eq!(ids["identities"][0]["identity"]["nickname"], "露露");
    assert!(!ids.to_string().contains("13800000002"), "🔴 手机号等强 PII 绝不下发");
}

// ═══════════════════════════════════════════════════════════════════════════
// 拉黑的服务端实效
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn block_revokes_identity_and_stops_all_social_actions() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    set_relations(&state, "w1", json!([{ "from": "c1", "to": "c2", "trust": 0.8 }])).await;
    let (tk1, tk2) = (token(&state, "u1"), token(&state, "u2"));

    // 先达成解锁。
    let (_, req) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    let rid = req["id"].as_str().unwrap().to_string();
    send(
        &state,
        "POST",
        &format!("/api/me/social/unlock-requests/{rid}/respond"),
        &tk2,
        Some(json!({ "accept": true })),
    )
    .await;
    let (_, ids) = send(&state, "GET", "/api/me/social/identities", &tk1, None).await;
    assert_eq!(ids["identities"].as_array().unwrap().len(), 1, "前置：身份已互相可见");

    // u2 拉黑 c1（面具寻址）。
    let (s, blk) = send(
        &state,
        "POST",
        "/api/me/social/blocks",
        &tk2,
        Some(json!({ "characterId": "c1", "reason": "不想再联系" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(blk["revokedUnlocks"], json!(1), "拉黑必须撤销已达成的解锁");

    // ① 身份可见性立即收回（双方都读不到）。
    for tk in [&tk1, &tk2] {
        let (_, ids) = send(&state, "GET", "/api/me/social/identities", tk, None).await;
        assert_eq!(ids["identities"].as_array().unwrap().len(), 0, "🔴 拉黑后身份必须立刻读不出");
    }
    // ② 被拉黑者从社交面消失。
    let (_, bonds) = send(&state, "GET", "/api/worlds/w1/social/bonds", &tk1, None).await;
    assert_eq!(bonds["bonds"].as_array().unwrap().len(), 0, "拉黑双方互相不可见");
    // ③ 被拉黑者发不出新的解锁请求。
    let (s, b) = send(
        &state,
        "POST",
        "/api/worlds/w1/social/unlock-requests",
        &tk1,
        Some(json!({ "targetCharacterId": "c2" })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b["error"]["message"], refuse_line());

    // ④ 解除拉黑不恢复已撤销的解锁（revoked 是终局态）。
    let bid = blk["id"].as_str().unwrap().to_string();
    let (s, _) = send(&state, "DELETE", &format!("/api/me/social/blocks/{bid}"), &tk2, None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, ids) = send(&state, "GET", "/api/me/social/identities", &tk1, None).await;
    assert_eq!(ids["identities"].as_array().unwrap().len(), 0, "解除拉黑不等于恢复身份授予");
}

/// 拉黑的实效必须**跨通道**：房间邀请同样发不出。
/// 且拉黑是保护态——即便社交开关被关掉，既有拉黑在邀请通道上仍然生效。
#[tokio::test]
async fn block_also_stops_room_invitations_even_when_social_flag_off() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    // u2 有一张不在 w2 里的卡 c9，用于接收邀请。
    seed_char(&state, "c9", "u2", "白露·分身").await;
    seed_world(&state.db, "w2", 1, "open").await;
    seed_member(&state.db, "m9", "w2", "u1", "c1", "active").await;
    let (tk1, tk2) = (token(&state, "u1"), token(&state, "u2"));

    // u2 拉黑 c1。
    let (s, _) = send(&state, "POST", "/api/me/social/blocks", &tk2, Some(json!({ "characterId": "c1" })))
        .await;
    assert_eq!(s, StatusCode::OK);

    // 关掉社交开关（模拟运营急停），只留邀请开关。
    sqlx::query("UPDATE runtime_flags SET enabled = 0 WHERE flag = $1")
        .bind(ENV_SOCIAL_IDENTITY_UNLOCK)
        .execute(&state.db)
        .await
        .unwrap();
    crate::flags::invalidate(&state.db);
    let _inv = crate::invitations::InvitationSwitch::set(true);

    let (s, b) = send(
        &state,
        "POST",
        "/api/worlds/w2/invitations",
        &tk1,
        Some(json!({ "targetCharacterId": "c9" })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "🔴 拉黑必须跨通道生效：被拉黑者连房间邀请也发不出");
    assert!(b["error"]["message"].as_str().unwrap().contains("不能被邀请"), "沿用邀请侧的统一拒绝文案");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_invitations")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "被拉黑的邀请一行都不落库");
}

// ═══════════════════════════════════════════════════════════════════════════
// 举报队列
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn report_enters_queue_dedupes_escalates_and_is_resolvable() {
    let state = test_state().await;
    base_world(&state).await;
    for (u, c) in [("u3", "c3"), ("u4", "c4"), ("u5", "c5")] {
        seed_user(&state.db, u).await;
        seed_char(&state, c, u, c).await;
        seed_member(&state.db, &format!("m_{u}"), "w1", u, c, "active").await;
    }
    open_flag(&state, "global", "").await;

    // ① 举报进队列，且响应不泄露被举报人的真人 id。
    let (s, r1) = send(
        &state,
        "POST",
        "/api/me/social/reports",
        &token(&state, "u1"),
        Some(json!({
            "subjectKind": "character", "subjectId": "c2",
            "category": "harassment", "detail": "反复发送骚扰内容"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r1["status"], REPORT_PENDING);
    assert!(r1.get("subjectUserId").is_none(), "🔴 举报回执不得下发被举报人的真人 id");

    // ② 冷却窗口内重复提交 → 幂等复用，不刷队列。
    let (_, r2) = send(
        &state,
        "POST",
        "/api/me/social/reports",
        &token(&state, "u1"),
        Some(json!({ "subjectKind": "character", "subjectId": "c2", "category": "harassment" })),
    )
    .await;
    assert_eq!(r2["deduped"], json!(true));
    assert_eq!(r2["id"], r1["id"]);

    // ③ 累计到阈值（默认 3）→ 写一条 risk_events 升级。
    for u in ["u3", "u4"] {
        let (s, _) = send(
            &state,
            "POST",
            "/api/me/social/reports",
            &token(&state, u),
            Some(json!({ "subjectKind": "character", "subjectId": "c2", "category": "harassment" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
    let escalations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM risk_events WHERE kind = 'social_report_threshold' AND user_id = 'u2'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(escalations, 1, "恰好达阈值时升级一次（一次跨越只升级一次）");

    // ④ 运营队列可见 + 处置 + 审计留痕。
    let rtk = admin_token(&state, "reviewer");
    let (s, list) = send(&state, "GET", "/api/admin/social/reports?status=pending", &rtk, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["reports"].as_array().unwrap().len(), 3);
    let rid = r1["id"].as_str().unwrap().to_string();
    let (s, res) = send(
        &state,
        "POST",
        &format!("/api/admin/social/reports/{rid}/resolve"),
        &rtk,
        Some(json!({ "action": "actioned", "reason": "已警告并限制其私信" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(res["status"], REPORT_ACTIONED);
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'social.report_resolved' AND subject = $1",
    )
    .bind(&rid)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(audits, 1, "运营处置必须留痕（没有留痕就没有复盘）");

    // ⑤ 重复处置 → 409（CAS，不覆盖别人的结论）。
    let (s, _) = send(
        &state,
        "POST",
        &format!("/api/admin/social/reports/{rid}/resolve"),
        &rtk,
        Some(json!({ "action": "dismissed", "reason": "改主意" })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn report_rejects_unknown_category_and_self_report() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    let tk = token(&state, "u1");

    let (s, _) = send(
        &state,
        "POST",
        "/api/me/social/reports",
        &tk,
        Some(json!({ "subjectKind": "character", "subjectId": "c2", "category": "我不喜欢他" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "未知类别必须 400，绝不静默归到 other");

    let (s, _) = send(
        &state,
        "POST",
        "/api/me/social/reports",
        &tk,
        Some(json!({ "subjectKind": "character", "subjectId": "c1", "category": "other" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "不能举报自己");
}

#[tokio::test]
async fn admin_report_queue_enforces_role_matrix() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    for (role, expect) in [
        ("admin", StatusCode::OK),
        ("reviewer", StatusCode::OK),
        ("support", StatusCode::OK),
        ("operator", StatusCode::FORBIDDEN),
        ("finance", StatusCode::FORBIDDEN),
    ] {
        let (s, _) =
            send(&state, "GET", "/api/admin/social/reports", &admin_token(&state, role), None).await;
        assert_eq!(s, expect, "{role} 的举报队列权限不符预期");
    }
}

/// 🔴 举报队列的复合游标不丢行（docs/VALIDATION.md §3.3 P1）。
///
/// 举报是安全通道：一条举报若被分页永久跳过，运营**永远看不到**它 = 永远不会被处置。
/// 单列 `created_at` 游标 + `created_at < cursor` 的严格小于，在同毫秒并列组横跨页边界时
/// 必然发生这件事（两个库皆然，SQLite 只是每次丢的是同一条所以更难被发现）。
/// 播种 21 条同毫秒举报（默认页 20），断言两页并起来恰好是全集。
#[tokio::test]
async fn admin_report_queue_keyset_cursor_never_drops_a_report() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    let at = now_ms();
    let mut all: Vec<String> = Vec::new();
    for i in 0..21 {
        let id = new_id("srp");
        sqlx::query(
            "INSERT INTO social_reports (id, reporter_user_id, subject_kind, subject_id, subject_user_id, \
             world_id, category, detail, status, created_at) \
             VALUES ($1, 'u1', 'character', $2, 'u2', 'w1', 'harassment', '', 'pending', $3)",
        )
        .bind(&id)
        .bind(format!("c{i}"))
        .bind(at) // 整批同毫秒
        .execute(&state.db)
        .await
        .unwrap();
        all.push(id);
    }

    let tk = admin_token(&state, "reviewer");
    let ids_of = |v: &Value| -> Vec<String> {
        v["reports"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
    };

    let (s, page1) = send(&state, "GET", "/api/admin/social/reports", &tk, None).await;
    assert_eq!(s, StatusCode::OK);
    let ids1 = ids_of(&page1);
    assert_eq!(ids1.len(), 20, "首页满页（默认 page_size=20）");
    let cursor = page1["nextCursor"].as_i64().unwrap();
    let cursor_id = page1["nextCursorId"].as_str().unwrap().to_string();

    // 旧客户端（只带 cursor）：第二页空 —— 第 21 条举报就此从队列里消失。
    let (_, legacy) =
        send(&state, "GET", &format!("/api/admin/social/reports?cursor={cursor}"), &tk, None).await;
    assert!(ids_of(&legacy).is_empty(), "单列游标在并列组上必然空翻页（同时证明旧客户端零行为变化）");

    // 复合游标：那一条必须回来，且与首页不重叠。
    let (_, page2) = send(
        &state,
        "GET",
        &format!("/api/admin/social/reports?cursor={cursor}&cursorId={cursor_id}"),
        &tk,
        None,
    )
    .await;
    let ids2 = ids_of(&page2);
    assert_eq!(ids2.len(), 1, "并列组被 id 精确切开，被跳过的那条举报必须出现");
    let mut seen = ids1;
    seen.extend(ids2);
    seen.sort();
    let mut expect = all;
    expect.sort();
    assert_eq!(seen, expect, "举报队列两页并起来必须是全集：安全通道不许丢行");
}

/// 队列筛选：未知筛选值必须 **400**，绝不静默返回空列表。
///
/// 🔴 这条守的是安全队列上最危险的一种误读：`?status=Pending`（大小写写错）若走
/// 「匹配不到 → 空列表」，运营看到的是一个**空队列**，会读成「没有积压」，
/// 于是一整批待处理举报没人处理，而界面上没有任何异常。
#[tokio::test]
async fn admin_report_queue_rejects_unknown_filter_values_instead_of_returning_an_empty_queue() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    let tk = admin_token(&state, "reviewer");

    for uri in [
        "/api/admin/social/reports?status=Pending",       // 大小写
        "/api/admin/social/reports?status=待处理",         // 中文
        "/api/admin/social/reports?category=我不喜欢他",   // 未知类别
        "/api/admin/social/reports?subjectKind=world",    // 未知主体种类
    ] {
        let (s, _) = send(&state, "GET", uri, &tk, None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{uri} 必须 400，不能静默空队列");
    }

    // 白名单值与 all 正常放行；缺省 = 待处理。
    for uri in [
        "/api/admin/social/reports",
        "/api/admin/social/reports?status=all&category=all&subjectKind=all",
        "/api/admin/social/reports?status=dismissed&category=minor_risk&subjectKind=unlock_request",
    ] {
        let (s, v) = send(&state, "GET", uri, &tk, None).await;
        assert_eq!(s, StatusCode::OK, "{uri} 应放行");
        assert!(v["reports"].is_array());
    }
}

/// 筛选真的下推到 SQL：类别/主体种类各筛出自己那一份，且与状态筛可叠加。
#[tokio::test]
async fn admin_report_queue_filters_by_category_and_subject_kind() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;

    let seed = |kind: &'static str, category: &'static str, status: &'static str| {
        let db = state.db.clone();
        async move {
            sqlx::query(
                "INSERT INTO social_reports (id, reporter_user_id, subject_kind, subject_id, \
                 subject_user_id, world_id, category, detail, status, created_at) \
                 VALUES ($1, 'u1', $2, 'c2', 'u2', 'w1', $3, '', $4, $5)",
            )
            .bind(new_id("srp"))
            .bind(kind)
            .bind(category)
            .bind(status)
            .bind(now_ms())
            .execute(&db)
            .await
            .unwrap();
        }
    };
    seed("character", "harassment", "pending").await;
    seed("character", "minor_risk", "pending").await;
    seed("unlock_request", "harassment", "pending").await;
    seed("character", "harassment", "actioned").await;

    let tk = admin_token(&state, "reviewer");
    let count = |uri: &'static str| {
        let state = state.clone();
        let tk = tk.clone();
        async move {
            let (s, v) = send(&state, "GET", uri, &tk, None).await;
            assert_eq!(s, StatusCode::OK);
            v["reports"].as_array().unwrap().len()
        }
    };

    assert_eq!(count("/api/admin/social/reports").await, 3, "默认只出待处理");
    assert_eq!(count("/api/admin/social/reports?status=all").await, 4);
    assert_eq!(count("/api/admin/social/reports?category=harassment").await, 2, "待处理 + 骚扰");
    assert_eq!(
        count("/api/admin/social/reports?status=all&category=harassment").await,
        3,
        "全部状态 + 骚扰"
    );
    assert_eq!(count("/api/admin/social/reports?subjectKind=unlock_request").await, 1);
    assert_eq!(
        count("/api/admin/social/reports?category=minor_risk&subjectKind=unlock_request").await,
        0,
        "两个筛选是且的关系"
    );
}

/// 🔴 末页必须回 `nextCursor: null`。
///
/// 只按「末行有没有」发游标的话，最后一页也带游标返回，界面上的「加载更多」永远在。
/// 在举报队列上这不是难看的问题——它让运营**分不清「翻完了」和「还没翻完」**，
/// 而"还有没有没看过的举报"正是这个队列唯一要回答的问题。
#[tokio::test]
async fn admin_report_queue_last_page_returns_null_cursor() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    let tk = admin_token(&state, "reviewer");

    // 空队列：一上来就没有下一页。
    let (s, empty) = send(&state, "GET", "/api/admin/social/reports", &tk, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(empty["nextCursor"].is_null() && empty["nextCursorId"].is_null(), "空队列不得回游标");

    // 播种「满一页 + 1」条同毫秒举报：第一页满 → 有游标；第二页只有 1 条 → 无游标。
    let page = page_size();
    let at = now_ms();
    for i in 0..(page + 1) {
        sqlx::query(
            "INSERT INTO social_reports (id, reporter_user_id, subject_kind, subject_id, \
             subject_user_id, world_id, category, detail, status, created_at) \
             VALUES ($1, 'u1', 'character', $2, 'u2', 'w1', 'harassment', '', 'pending', $3)",
        )
        .bind(new_id("srp"))
        .bind(format!("c{i}"))
        .bind(at)
        .execute(&state.db)
        .await
        .unwrap();
    }

    let (_, p1) = send(&state, "GET", "/api/admin/social/reports", &tk, None).await;
    assert_eq!(p1["reports"].as_array().unwrap().len() as i64, page, "首页满页");
    assert_eq!(p1["pageSize"].as_i64().unwrap(), page);
    let cursor = p1["nextCursor"].as_i64().expect("满页必须回游标");
    let cursor_id = p1["nextCursorId"].as_str().expect("满页必须回复合游标第二段").to_string();

    let (_, p2) = send(
        &state,
        "GET",
        &format!("/api/admin/social/reports?cursor={cursor}&cursorId={cursor_id}"),
        &tk,
        None,
    )
    .await;
    assert_eq!(p2["reports"].as_array().unwrap().len(), 1, "第二页是最后一条");
    assert!(p2["nextCursor"].is_null(), "末页不得回游标，否则「加载更多」永远在");
    assert!(p2["nextCursorId"].is_null());
}

/// 队列形状端点：全量聚合，不受分页与筛选影响；白名单键恒出现（哪怕是 0）。
#[tokio::test]
async fn admin_report_summary_counts_the_whole_queue_not_one_page() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    let tk = admin_token(&state, "reviewer");

    // 空库：三档状态与全部类别都要在，值为 0——缺档会被界面读成「这一档没有数据源」。
    let (s, empty) = send(&state, "GET", "/api/admin/social/reports/summary", &tk, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(empty["total"], json!(0));
    for st in REPORT_STATUSES {
        assert_eq!(empty["byStatus"][*st], json!(0), "{st} 档必须在场");
    }
    let cats = empty["byCategory"].as_array().unwrap();
    assert_eq!(cats.len(), REPORT_CATEGORIES.len(), "类别按白名单给全，不是「库里有什么给什么」");
    assert!(empty["oldestPendingCreatedAt"].is_null());
    assert_eq!(empty["escalateAt"], json!(report_escalate_at()));
    assert_eq!(empty["escalatedSubjectCount"], json!(0));

    // 播种「超过一页」条：summary 必须数全量，而不是一页。
    let page = page_size();
    let base = now_ms();
    for i in 0..(page + 5) {
        sqlx::query(
            "INSERT INTO social_reports (id, reporter_user_id, subject_kind, subject_id, \
             subject_user_id, world_id, category, detail, status, created_at) \
             VALUES ($1, 'u1', 'character', $2, 'u2', 'w1', 'harassment', '', 'pending', $3)",
        )
        .bind(new_id("srp"))
        .bind(format!("c{i}"))
        .bind(base + i) // 递增时间戳：最久未处理的那条可判定
        .execute(&state.db)
        .await
        .unwrap();
    }
    // 另一个被举报人 + 另一类别 + 另一主体种类 + 已处置态。
    sqlx::query(
        "INSERT INTO social_reports (id, reporter_user_id, subject_kind, subject_id, \
         subject_user_id, world_id, category, detail, status, created_at) \
         VALUES ($1, 'u1', 'unlock_request', 'sul_x', 'u3', 'w1', 'minor_risk', '', 'dismissed', $2)",
    )
    .bind(new_id("srp"))
    .bind(base + 999)
    .execute(&state.db)
    .await
    .unwrap();

    let (_, sum) = send(&state, "GET", "/api/admin/social/reports/summary", &tk, None).await;
    assert_eq!(sum["total"].as_i64().unwrap(), page + 6);
    assert_eq!(sum["byStatus"]["pending"].as_i64().unwrap(), page + 5, "积压是全量，不是一页");
    assert_eq!(sum["byStatus"]["dismissed"], json!(1));
    assert_eq!(sum["byStatus"]["actioned"], json!(0));
    assert_eq!(sum["oldestPendingCreatedAt"].as_i64().unwrap(), base, "最久未处理 = 最早那条");
    // u2 的 pending 数远超阈值，u3 那条是 dismissed 不计。
    assert_eq!(sum["escalatedSubjectCount"], json!(1));

    let by_cat = sum["byCategory"].as_array().unwrap();
    let harassment = by_cat.iter().find(|c| c["key"] == "harassment").unwrap();
    assert_eq!(harassment["pending"].as_i64().unwrap(), page + 5);
    assert_eq!(harassment["total"].as_i64().unwrap(), page + 5);
    let minor = by_cat.iter().find(|c| c["key"] == "minor_risk").unwrap();
    assert_eq!(minor["dismissed"], json!(1));
    assert_eq!(minor["pending"], json!(0));
    let by_kind = sum["bySubjectKind"].as_array().unwrap();
    assert_eq!(by_kind.len(), SUBJECT_KINDS.len());
    assert_eq!(by_kind.iter().find(|k| k["key"] == "unlock_request").unwrap()["total"], json!(1));
}

/// 队列形状端点与列表端点走**同一条**角色矩阵（operator/finance 进不来）。
#[tokio::test]
async fn admin_report_summary_enforces_the_same_role_matrix_as_the_queue() {
    let state = test_state().await;
    base_world(&state).await;
    open_flag(&state, "global", "").await;
    for (role, expect) in [
        ("admin", StatusCode::OK),
        ("reviewer", StatusCode::OK),
        ("support", StatusCode::OK),
        ("operator", StatusCode::FORBIDDEN),
        ("finance", StatusCode::FORBIDDEN),
    ] {
        let (s, _) = send(
            &state,
            "GET",
            "/api/admin/social/reports/summary",
            &admin_token(&state, role),
            None,
        )
        .await;
        assert_eq!(s, expect, "{role} 的队列形状权限必须与队列本身一致");
    }
}

/// 筛选值校验是纯函数，单独钉一遍回落规则（不摆布进程 env、不碰库）。
#[test]
fn filter_value_defaults_and_whitelist() {
    // 缺省 / 空串 / 纯空白 → 默认值。
    for raw in [None, Some(String::new()), Some("   ".into())] {
        assert_eq!(filter_value(raw, REPORT_STATUSES, "status", REPORT_PENDING).unwrap(), "pending");
    }
    // all 与白名单值放行（两端空白容忍）。
    assert_eq!(filter_value(Some(" all ".into()), REPORT_STATUSES, "s", "pending").unwrap(), "all");
    assert_eq!(
        filter_value(Some("actioned".into()), REPORT_STATUSES, "s", "pending").unwrap(),
        "actioned"
    );
    // 白名单外一律 400（大小写不宽容：状态是落库字面量，不是自然语言）。
    assert!(filter_value(Some("Pending".into()), REPORT_STATUSES, "s", "pending").is_err());
    assert!(filter_value(Some("other2".into()), REPORT_CATEGORIES, "c", FILTER_ALL).is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// ⚠️ 此处原有两组「我们的角色一起死过」用例 —— 2026-07-29 随该资格路径一并删除
// ═══════════════════════════════════════════════════════════════════════════
//
// 删掉的是：① `died_together` 三档派生口径（they_fell / both_fell / 无共同世界）；
// ② 「传世卡 withdrawn=1 但仍是合格社交对手」那条（它守的是「别拿 withdrawn 当门」，
// 而在卡永不损失的模型下根本不会有 withdrawn=1 的在世卡去参与社交）。
//
// 🔴 留一句给下一个人：现在**只剩正向羁绊一条资格路径**，社交门槛比之前**更严**。
// 若将来要补一条等价凭证（如「共历终局」），必须先答一个问题——
// 退场在新模型下是常见事件，而死亡曾是罕见事件，照搬会让凭证极易获得，
// 等于**放宽**这道连着未成年保护与网暴防线的门。见 `social/mod.rs` 里那段说明。


// ═══════════════════════════════════════════════════════════════════════════
// 纯函数：羁绊折算与参数回落
// ═══════════════════════════════════════════════════════════════════════════

/// 现行默认：`debt` 不计入正向分（`ENV_BOND_COUNTS_DEBT` 默认 false）。
const DEBT_OFF: bool = super::DEFAULT_BOND_COUNTS_DEBT;
const _: () = assert!(!DEBT_OFF, "本文件多处按「默认不计 debt」写断言；默认值若翻转须逐条重看");

#[test]
fn bond_requires_both_directions_and_detects_hostility() {
    let two_way = json!({ "relations": [
        { "from": "a", "to": "b", "trust": 0.9 },
        { "from": "b", "to": "a", "trust": 0.1 },
    ]})
    .to_string();
    let v = bond_between(&two_way, "a", "b", 0.3, 0.5, DEBT_OFF);
    assert_eq!(v.edges, 2);
    assert!(!v.hostile);
    assert!((v.positive - 0.1).abs() < 1e-9, "双向取较小者：一头热不构成羁绊线");

    let one_way = json!({ "relations": [{ "from": "a", "to": "b", "affinity": 0.7 }]}).to_string();
    assert!((bond_between(&one_way, "a", "b", 0.3, 0.5, DEBT_OFF).positive - 0.7).abs() < 1e-9);

    // debt 默认**不**计入正向线（2026-07-28 产品拍板：解锁门是「双向自愿」，欠人情是义务）。
    let debt = json!({ "relations": [{ "from": "a", "to": "b", "debt": 0.8 }]}).to_string();
    assert_eq!(bond_between(&debt, "a", "b", 0.3, 0.5, DEBT_OFF).positive, 0.0);
    // 旧口径由 env 一键取回，语义不变。
    assert!((bond_between(&debt, "a", "b", 0.3, 0.5, true).positive - 0.8).abs() < 1e-9);

    // 敌对三判据，任一命中即敌对。
    for rel in [
        json!({ "from": "a", "to": "b", "trust": -0.3 }),
        json!({ "from": "b", "to": "a", "affinity": -0.5 }),
        json!({ "from": "a", "to": "b", "fear": 0.5 }),
    ] {
        let s = json!({ "relations": [rel] }).to_string();
        assert!(bond_between(&s, "a", "b", 0.3, 0.5, DEBT_OFF).hostile, "应判为敌对");
    }

    // 无关的边、损坏的状态、空状态 → 中性视图（解锁不了，但也不冤枉成敌对）。
    for s in [
        json!({ "relations": [{ "from": "a", "to": "z", "trust": -1.0 }]}).to_string(),
        "{ not json".to_string(),
        "{}".to_string(),
    ] {
        let v = bond_between(&s, "a", "b", 0.3, 0.5, DEBT_OFF);
        assert_eq!(v, BondView::default());
    }
}

#[test]
fn params_fall_back_and_reject_garbage() {
    assert_eq!(parse_positive(None, 7), 7);
    assert_eq!(parse_positive(Some("0"), 7), 7, "非正数回落默认");
    assert_eq!(parse_positive(Some("-3"), 7), 7);
    assert_eq!(parse_positive(Some("abc"), 7), 7);
    assert_eq!(parse_positive(Some(" 12 "), 7), 12);

    assert!((parse_non_negative_f64(None, 0.6) - 0.6).abs() < 1e-9);
    assert!((parse_non_negative_f64(Some("-1"), 0.6) - 0.6).abs() < 1e-9, "负数回落默认");
    assert!((parse_non_negative_f64(Some("nan"), 0.6) - 0.6).abs() < 1e-9);
    assert!((parse_non_negative_f64(Some("0.25"), 0.6) - 0.25).abs() < 1e-9);

    assert!(parse_bool(None, true));
    assert!(!parse_bool(Some("off"), true));
    assert!(parse_bool(Some("yes"), false));
    assert!(parse_bool(Some("随便"), true), "配错不静默改变状态：回落默认");

    // §0.2：每一条产品规则都必须是可配置的（这里断言默认值确实是"保守方向"）。
    assert!(DEFAULT_UNLOCK_MIN_BOND > 0.0, "解锁阈值不得默认为 0（那等于人人可解锁）");
    assert!(DEFAULT_UNLOCK_DAILY_LIMIT <= DEFAULT_REPORT_DAILY_LIMIT, "举报配额必须比解锁宽松");
    assert!(!DEFAULT_SOCIAL_ENABLED);
}

#[test]
fn refusal_message_is_single_and_generic() {
    // 🔴 全模块只有一句"不能解锁"的话：多一句就多一条可被用来区分原因的信息。
    let src = include_str!("mod.rs");
    let occurrences = src.matches("REFUSE_GENERIC").count();
    assert!(occurrences >= 5, "所有解锁类拒绝都应走同一个常量，实测引用 {occurrences} 次");
    assert!(!REFUSE_GENERIC.contains("未成年"));
    assert!(!REFUSE_GENERIC.contains("拉黑"));
    assert!(!REFUSE_GENERIC.contains("敌对"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 解锁资格快照：记下被比较的那个值 + 给它一个读取面
//
// 补的是两个**工程缺陷**（不是产品决定）：
//   ① 快照只记 thresholds.minBond，不记被比较的 bond 本身——记了标尺没记读数；
//   ② eligibility_json 一直在写，全仓**没有任何地方读它**——没人看得到的审计等于没写。
// 顺带产出 open-decisions §2 自己写的那个 settling 证据（真实分布）。
// ═══════════════════════════════════════════════════════════════════════════

/// 直插一条带指定 bond 的快照（只为验读取面的聚合，不经资格判定）。
/// ⚠️ 唯一键是 `(world_id, requester_character_id, target_character_id)`，
/// 故每条用**不同的角色对**（拿 id 派生），否则第二条就撞键。
async fn seed_snapshot(state: &AppState, id: &str, bond_json: Option<f64>) {
    let elig = match bond_json {
        Some(b) => json!({ "eligible": true, "bond": b, "thresholds": { "minBond": 0.6 } }),
        // 旧快照形状：有门槛、没读数。
        None => json!({ "eligible": true, "thresholds": { "minBond": 0.6 } }),
    };
    sqlx::query(
        "INSERT INTO social_unlock_requests (id, world_id, requester_user_id, \
         requester_character_id, target_user_id, target_character_id, status, eligibility_json, \
         expires_at, responded_at, revoked_at, created_at) \
         VALUES ($1, 'w1', 'u1', $2, 'u2', $3, 'pending', $4, 0, 0, 0, $5)",
    )
    .bind(id)
    .bind(format!("c_req_{id}"))
    .bind(format!("c_tgt_{id}"))
    .bind(elig.to_string())
    .bind(crate::db::now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 🔴 审计快照必须记下**被比较的那个值**，不只是门槛。
///
/// 一份记了标尺、没记读数的快照，答不上「我为什么解锁不了」——那条关系的正向分
/// 没有任何地方留下过（叙事态是活的，早就变了）。
#[tokio::test]
async fn the_audit_snapshot_records_the_value_that_was_compared_not_only_the_threshold() {
    let src = include_str!("mod.rs");
    // 源码级：两者必须**同时**出现在审计快照里。
    let i = src.find("fn to_audit_json").expect("审计快照函数应存在");
    let j = src[i..].find("fn ").map(|k| i + k + 3).unwrap_or(src.len());
    // 🔴 按**字符边界**收敛，不能直接切字节：这份源码是中文注释为主，
    // `i + 800` 落在多字节字符中间会直接 panic（2026-07-29 改动上游内容时踩到过一次）。
    let end = {
        let mut e = j.max(i + 800).min(src.len());
        while e < src.len() && !src.is_char_boundary(e) {
            e += 1;
        }
        e
    };
    let body = &src[i..end];
    assert!(
        body.contains("\"bond\": self.bond.positive"),
        "🔴 审计快照必须记下被比较的正向分本身；只记 thresholds.minBond 等于记了标尺没记读数"
    );
    assert!(body.contains("minBond"), "门槛也要一起记，否则事后不知道当时的标尺是多少");

    // 🔴 而**自查视图仍然不给分**——那是刻意的：正向分由双方的边共同决定（跨边取 min），
    // 露给本人等于泄露对方对他的感受（§14 信息边界）。
    let k = src.find("fn to_self_json").expect("自查视图应存在");
    let self_end = {
        let mut e = (k + 700).min(src.len());
        while e < src.len() && !src.is_char_boundary(e) {
            e += 1;
        }
        e
    };
    let self_body = &src[k..self_end];
    assert!(
        !self_body.contains("self.bond.positive"),
        "🔴 自查视图不得下发原始羁绊分 —— 它由双方的边共同决定，露给本人就是泄露对方的感受"
    );
}

/// 🔴 分布读数：旧快照（没有 `bond` 字段）**单列如实报，绝不按 0 计入直方图**。
/// 按 0 计会在最左边堆出一座根本不存在的山，而那正是调阈值时最容易被误读的位置。
#[tokio::test]
async fn legacy_snapshots_without_a_bond_are_reported_separately_not_counted_as_zero() {
    let state = test_state().await;
    let tk = admin_token(&state, "operator");
    seed_snapshot(&state, "ur_new1", Some(0.75)).await;
    seed_snapshot(&state, "ur_new2", Some(0.25)).await;
    seed_snapshot(&state, "ur_old1", None).await;
    seed_snapshot(&state, "ur_old2", None).await;

    let (st, b) = send(&state, "GET", "/api/admin/social/bond-distribution", &tk, None).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["sampled"], json!(4));
    assert_eq!(b["withBond"], json!(2));
    assert_eq!(b["legacyWithoutBond"], json!(2), "🔴 旧快照要单列: {b}");
    let counts = b["histogram"]["counts"].as_array().unwrap();
    assert_eq!(counts[0], json!(0), "🔴 最左桶必须是 0 —— 旧快照按 0 计会堆出一座不存在的山: {b}");
    assert_eq!(counts[2], json!(1), "0.25 落第 3 桶");
    assert_eq!(counts[7], json!(1), "0.75 落第 8 桶");
}

/// `wouldPassAt` 回答的是调阈值时唯一想知道的事：挪到 X 会有多少条通过。
#[tokio::test]
async fn would_pass_at_answers_the_only_question_you_have_when_tuning_the_threshold() {
    let state = test_state().await;
    let tk = admin_token(&state, "operator");
    for (i, v) in [0.1, 0.5, 0.7, 0.9].iter().enumerate() {
        seed_snapshot(&state, &format!("ur_{i}"), Some(*v)).await;
    }
    let (_st, b) = send(&state, "GET", "/api/admin/social/bond-distribution", &tk, None).await;

    let at = |t: f64| -> i64 {
        b["wouldPassAt"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| (x["threshold"].as_f64().unwrap() - t).abs() < 1e-9)
            .unwrap()["wouldPass"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(at(0.0), 4, "阈值 0 → 全过");
    assert_eq!(at(0.5), 3);
    assert_eq!(at(0.7), 2);
    assert_eq!(at(1.0), 0);
    // 当前阈值 0.6（默认）下应当是 2 条。
    assert_eq!(b["passingAtCurrentThreshold"], json!(2), "{b}");

    // 🔴 诚实边界必须随数据走：这份分布**天然偏高**（够不上阈值而没去点的人不在样本里）。
    let honesty = b["honesty"].as_array().unwrap();
    assert!(
        honesty.iter().any(|h| h.as_str().unwrap_or("").contains("天然偏高")),
        "🔴 样本偏差必须写在响应里，否则会被当成全平台关系分布读: {b}"
    );
}

/// 🔴 只回聚合，**绝不下发真人身份或逐条明细**（§14）——排查阈值不需要知道是谁。
#[tokio::test]
async fn the_distribution_never_leaks_who() {
    let state = test_state().await;
    let tk = admin_token(&state, "operator");
    seed_snapshot(&state, "ur_x", Some(0.8)).await;
    let (_st, b) = send(&state, "GET", "/api/admin/social/bond-distribution", &tk, None).await;
    let raw = b.to_string();
    for leak in ["u1", "u2", "c1", "c2", "ur_x", "requester"] {
        assert!(!raw.contains(leak), "🔴 分布读数泄露了「{leak}」——它只该回计数与分布: {raw}");
    }
    assert!(b.get("items").is_none() && b.get("requests").is_none(), "不得有逐条明细");
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 跨 crate 线上契约：引擎写出来的 `relations`，敌对判据读得懂吗
//
// `bond_between` 按**字符串键**手读引擎的 `RelationState`（from / to / trust /
// affinity / fear / debt）。而 `muse-engine` 侧与本模块**各自按自己的假设写**，
// 中间没有任何东西验证两边说的是同一件事——本模块既有用例喂的都是**手写 JSON**。
//
// 🔴 **失败方向是 fail-open，且撞的是隐私红线。** 字段名一旦漂移，`f()` 对每个维度都
// 取不到值 → 全部读成 0.0 → 同时发生两件事：
//   ① `positive = 0` → 正向路径走不通（这一半是 fail-closed，无害）；
//   ② `hostile = false` → **一票否决静默失效**。
// 而 `died_together` 是一条**独立**的解锁路径（不看 positive）——于是
// **敌对线的两个玩家仍能互相解锁真人身份**，直接违反 §14「敌对线永久匿名（一票否决）」。
//
// 下面用**真实引擎类型 + 真实 serde** 走一遍，把这条契约钉住：谁改了序列化，这里立刻红。
// ═══════════════════════════════════════════════════════════════════════════

/// 用**引擎自己的类型**造一份含关系的叙事态并序列化——不是手写 JSON。
/// 手写 JSON 只能验「读侧认得我手写的形状」，验不到「引擎真的写出这个形状」。
fn engine_state_with_relation(from: &str, to: &str, trust: f32, affinity: f32, fear: f32, debt: f32) -> String {
    use muse_engine::narrative::types::{NarrativeState, RelationState};
    let mut st = NarrativeState::default();
    st.relations.push(RelationState {
        from: from.into(),
        to: to.into(),
        trust,
        affinity,
        fear,
        debt,
        known_to: Vec::new(),
        notes: Vec::new(),
    });
    serde_json::to_string(&st).expect("引擎叙事态可序列化")
}

/// 🔴 **敌对必须被引擎写出来的形状识别到**——这条一旦失效，一票否决就静默失效，
/// 而 `died_together` 那条独立路径仍会放行，敌对线的两人就能互解真人身份（§14 红线）。
#[test]
fn hostility_is_detected_from_what_the_engine_actually_writes() {
    // 强敌意：trust 深负（超过 hostile_max 默认阈值）。
    let json = engine_state_with_relation("cA", "cB", -0.9, 0.0, 0.0, 0.0);
    let view = super::bond_between(&json, "cA", "cB", 0.5, 0.6, DEBT_OFF);
    assert!(
        view.hostile,
        "🔴 敌对没被识别到。这条红了多半是 muse-engine 的 RelationState 改了序列化\
         （改名 / 去掉 rename_all）——而读侧对此**完全无法与「关系平淡」区分**：\
         两者都读成 0.0。失败方向是 fail-open，且 died_together 那条独立路径仍会放行。\
         实得 {view:?}"
    );
    assert!(view.edges > 0, "边必须被数到，否则说明连 from/to 都没匹配上: {view:?}");

    // 高恐惧同样构成敌对（另一条判据，独立于 trust/affinity）。
    let fear_json = engine_state_with_relation("cA", "cB", 0.1, 0.1, 0.9, 0.0);
    assert!(
        super::bond_between(&fear_json, "cA", "cB", 0.5, 0.6, DEBT_OFF).hostile,
        "🔴 fear 维度的敌对判据也必须认得引擎写出来的形状"
    );
}

/// 正向分也必须从引擎写出来的形状里算得出来——否则解锁门恒不通过，
/// 而那是 fail-closed 的一半：不报错、不告警，只是**所有人永远解锁不了**。
#[test]
fn the_positive_bond_is_computed_from_what_the_engine_actually_writes() {
    let json = engine_state_with_relation("cA", "cB", 0.8, 0.4, 0.0, 0.0);
    let view = super::bond_between(&json, "cA", "cB", 0.5, 0.6, DEBT_OFF);
    assert!(!view.hostile, "无敌意: {view:?}");
    assert_eq!(view.edges, 1, "{view:?}");
    // 逐边 max(trust, affinity, debt, 0) = 0.8；单边时跨边 min 就是它本身。
    assert!(
        (view.positive - 0.8).abs() < 1e-6,
        "🔴 正向分算不出来 = 解锁门恒不通过，而它**不报错也不告警**——\
         所有人永远解锁不了，且没人知道为什么。实得 {view:?}"
    );

    // 🔴 debt 与 fear 都不计入正向：**现行公式**（debt 那一条 2026-07-28 由产品拍定），
    // 钉住它是因为悄悄改掉会改变谁能看到谁的真身。
    let debt_json = engine_state_with_relation("cA", "cB", 0.0, 0.0, 0.0, 0.7);
    assert_eq!(
        super::bond_between(&debt_json, "cA", "cB", 0.5, 0.6, DEBT_OFF).positive, 0.0,
        "🔴 debt 是义务不是意愿，默认不计入正向"
    );
    let fear_only = engine_state_with_relation("cA", "cB", 0.0, 0.0, 0.4, 0.0);
    assert_eq!(
        super::bond_between(&fear_only, "cA", "cB", 0.5, 0.6, DEBT_OFF).positive, 0.0,
        "🔴 fear 绝不计入正向 —— 怕一个人不是跟他关系好"
    );
}

/// 🔴 **双向取 min**：单方面的好感不构成羁绊线。
/// 用引擎写出来的两条有向边验——A→B 很好、B→A 平淡时，正向分取**弱的那一向**。
#[test]
fn a_one_sided_bond_does_not_pass_because_the_weaker_direction_gates() {
    use muse_engine::narrative::types::{NarrativeState, RelationState};
    let mk = |from: &str, to: &str, trust: f32| RelationState {
        from: from.into(), to: to.into(), trust, affinity: 0.0, fear: 0.0, debt: 0.0,
        known_to: Vec::new(), notes: Vec::new(),
    };
    let mut st = NarrativeState::default();
    st.relations.push(mk("cA", "cB", 0.9)); // 一头热
    st.relations.push(mk("cB", "cA", 0.1)); // 对方平淡
    let json = serde_json::to_string(&st).unwrap();

    let view = super::bond_between(&json, "cA", "cB", 0.5, 0.6, DEBT_OFF);
    assert_eq!(view.edges, 2, "两条有向边都要数到: {view:?}");
    assert!(
        (view.positive - 0.1).abs() < 1e-6,
        "🔴 必须取**弱的那一向**（0.1），不是强的、也不是平均。\
         「双向自愿」在数据层的前置就是这一步——一头热的人不该因为自己感觉好\
         就够格去要对方的真身。实得 {view:?}"
    );
}

/// 🔴 **「救命属正向线」这条规格没有被 debt 的改动动到——由引擎实际产出的形状证明。**
///
/// 2026-07-28 产品拍定 `debt` 不再计入正向羁绊分。表面看它和规格 §14「救命属正向线」冲突，
/// 因为救命之恩在引擎里正是 `debt`。**不冲突**，理由是结构性的，这条用例把它钉住：
///
/// 引擎 `relation_dynamics` 对「救」类命中时写的是
/// `affinity` / `trust` **双向**增，另加 `debt` **只加在被救者→救人者那一条边**。
/// 而羁绊分**跨边取 min**，最小值只可能来自**救人者那条边**——那条边上压根没有 debt。
///
/// 所以纯救命场景下，计不计 debt 算出来的分**一模一样**：
/// 救命之恩仍是正向线，只不过它由双向的 trust/affinity 承载，不由单边的 debt 承载。
///
/// ⚠️ 这条用例**直接驱动引擎**（`derive_relation_ops`）而不是照抄常量：
/// 哪天引擎改成「救类只加 debt、不加 trust/affinity」，上面那个论证就不成立了，
/// 而这里会立刻红——那正是需要重新回到产品面的时刻。
#[test]
fn rescue_bond_is_identical_whether_debt_counts_or_not() {
    use muse_engine::narrative::relation_dynamics::derive_relation_ops;
    use muse_engine::narrative::types::{
        ArbiterOutcome, ArbiterResult, CharacterState, NarrativeState, RoleDecision, SpeakIntent,
    };

    let mut state = NarrativeState { schema_version: 1, run_id: "r".into(), ..Default::default() };
    for c in ["rescuer", "rescued"] {
        state.characters.insert(c.into(), CharacterState::default());
    }
    let decisions = vec![RoleDecision {
        decision_id: "d1".into(),
        character_id: "rescuer".into(),
        intent: "护住他".into(),
        action: "出手救下坠崖的同伴".into(),
        speak: SpeakIntent { will_speak: false, purpose: String::new() },
        targets: vec!["rescued".into()],
        acceptable_costs: Vec::new(),
        predictions: Vec::new(),
        duration: 0,
    }];
    let outcomes = vec![ArbiterOutcome {
        decision_id: "d1".into(),
        character_id: "rescuer".into(),
        result: ArbiterResult::Success,
        rule_refs: vec![],
        consequence: "救下了".into(),
    }];
    let ops = derive_relation_ops(&decisions, &outcomes, &state);

    // ---- 引擎侧的结构事实：debt 单边、trust/affinity 双向 ----
    let val = |path: &str| -> Option<f64> {
        ops.iter().find(|o| o.path == path).and_then(|o| o.value.as_ref()).and_then(|v| v.as_f64())
    };
    let debt_edges: Vec<&str> =
        ops.iter().filter(|o| o.path.ends_with(".debt")).map(|o| o.path.as_str()).collect();
    assert_eq!(
        debt_edges.len(),
        1,
        "🔴 救类的 debt 必须只加在一条边上；变成双向就推翻了「debt 不影响救命线」的论证：{debt_edges:?}"
    );
    assert!(
        debt_edges[0].contains("rescued->rescuer"),
        "debt 记在被救者→救人者那一边（救命之恩记欠）：{debt_edges:?}"
    );
    for path in ["relations[rescuer->rescued]", "relations[rescued->rescuer]"] {
        for field in ["trust", "affinity"] {
            assert!(
                val(&format!("{path}.{field}")).is_some_and(|v| v > 0.0),
                "🔴 救类必须**双向**抬 trust/affinity，否则救命线就只剩单边 debt 撑着：缺 {path}.{field}"
            );
        }
    }

    // ---- 服务端侧：把引擎算出来的值搭成两条边，计不计 debt 结论一致 ----
    let mut after = NarrativeState::default();
    for (from, to) in [("rescuer", "rescued"), ("rescued", "rescuer")] {
        let g = |f: &str| val(&format!("relations[{from}->{to}].{f}")).unwrap_or(0.0) as f32;
        after.relations.push(muse_engine::narrative::types::RelationState {
            from: from.into(),
            to: to.into(),
            trust: g("trust"),
            affinity: g("affinity"),
            fear: g("fear"),
            debt: g("debt"),
            known_to: Vec::new(),
            notes: Vec::new(),
        });
    }
    let json = serde_json::to_string(&after).expect("叙事态可序列化");

    let off = super::bond_between(&json, "rescuer", "rescued", 0.5, 0.6, false);
    let on = super::bond_between(&json, "rescuer", "rescued", 0.5, 0.6, true);
    assert_eq!(off.edges, 2, "两条边都该被数到：{off:?}");
    assert!(!off.hostile && !on.hostile, "救命不该判敌对");
    assert!(
        (off.positive - on.positive).abs() < 1e-9,
        "🔴 救命场景下计不计 debt 必须同分——这正是「不冲突」的全部依据。\
         实得 off={:?} on={:?}",
        off.positive,
        on.positive
    );
    assert!(off.positive > 0.0, "救命必须留下正向分，否则规格 §14 那条线就断了：{off:?}");
}
