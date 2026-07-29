//! 运行时开关体系用例（sqlite::memory + axum oneshot）。
//!
//! 覆盖清单（与 `docs/VALIDATION.md` §3.1 补记的验收项一一对应）：
//!   · 默认关闭（DB 空 + env 未设 → 关）——**红线**
//!   · env 兜底（DB 空 + env 设了 → 按 env，逐值与旧实现比对）——**回归保护**
//!   · DB 覆盖 env（两个方向都测）
//!   · 三作用域优先级（用户 > 世界 > 全局）
//!   · 时间窗生效 / 未到 / 已过期 / 窗口外回落更宽作用域
//!   · fail-closed：损坏配置 / 查库失败 / 未登记开关名——**红线**
//!   · 缓存失效及时性（写后立即生效，不等 TTL）
//!   · 审计留痕（flag.set / flag.delete 落 audit_logs，含变更前后状态）
//!   · RBAC（operator 只读、非 admin 不能写、无 token 401）
//!   · 参考接线（onboarding）在 DB 空时行为逐字不变 + 按用户灰度真的生效

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::{build_router, AppState};
use crate::config::ServerConfig;
use crate::db::{new_id, now_ms};

use super::*;

/// 🔴 **env 夹具直接复用 `onboarding::OnboardingSwitch`，不自建第二把锁。**
///
/// `MUSE_ONBOARDING` 是进程级 env，而 `onboarding::tests` 与本文件同属一个测试二进制、
/// 默认并发跑。两个模块各拿各的锁 = 等于没有锁：症状是随机失败的「开关值对不上」。
/// **碰同一个 env 的用例必须共用同一把锁**，故本文件一律经 `OnboardingSwitch::raw`
/// 摆布 `MUSE_ONBOARDING`（`None` = 移除），附带的 `MUSE_FLAGS_CACHE_TTL_MS` 也搭它的车。
use crate::onboarding::OnboardingSwitch;

/// 置 `MUSE_ONBOARDING` 为原始值（None = 移除），可附带其它 env。
///
/// 🔴 **默认把 `MUSE_FLAGS_CACHE_TTL_MS` 钉成 `0`（即关掉缓存）**，除非调用方自己指定。
///
/// # 为什么必须默认关掉
///
/// 开关缓存是**整表快照 + 进程级**的。`env_guard` 只还原 env，**还原不了缓存**——
/// 而本文件里测缓存行为的用例会把 TTL 设成 60 秒并往缓存里灌一份快照。
/// 于是任何一条「`raw_insert` 写库 → `is_enabled` 读」的用例，
/// 只要排在它后面且时间窗口没过，读到的就是**上一条用例留下的旧快照**，
/// 断言随机失败，信息是「开关值对不上」——**指向被测逻辑，而根因是缓存**。
///
/// ⚠️ 这个坑在 SQLite 上几乎不发：那一遍跑得快，交错窗口窄。
/// 它是在**真 Postgres** 上跑全量时炸出来的（同一批用例，两次跑挂的还不是同一条）。
/// 本文件头那段「两个模块各拿各的锁 = 等于没有锁」说的是同一类问题的另一半：
/// **进程级共享的东西，谁都得管到底**——env 要还原，缓存也要。
///
/// 测缓存本身的用例照常在 `extra` 里显式给 TTL，会覆盖这里的默认值。
fn env_guard(onboarding: Option<&str>, extra: &[(&'static str, &str)]) -> OnboardingSwitch {
    if extra.iter().any(|(k, _)| *k == ENV_CACHE_TTL_MS) {
        return OnboardingSwitch::raw(onboarding, extra);
    }
    let mut all: Vec<(&'static str, &str)> = vec![(ENV_CACHE_TTL_MS, "0")];
    all.extend_from_slice(extra);
    OnboardingSwitch::raw(onboarding, &all)
}

fn test_config() -> ServerConfig {
    ServerConfig {
        database_url: crate::testkit::test_database_url(),
        bind_addr: "127.0.0.1:0".into(),
        jwt_secret: "test-secret".into(),
        access_ttl_secs: 3600,
        refresh_ttl_secs: 100_000,
        dev_mode: true,
        object_store_dir: std::env::temp_dir()
            .join(new_id("muse-flags-test"))
            .to_string_lossy()
            .into_owned(),
    }
}

async fn test_state() -> AppState {
    AppState::new(crate::testkit::test_pool().await, test_config())
}

fn token(state: &AppState, id: &str, role: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, id, role, 3600).unwrap()
}

async fn get(app: &axum::Router, uri: &str, tok: Option<&str>) -> (StatusCode, Value) {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(t) = tok {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn post(app: &axum::Router, uri: &str, tok: Option<&str>, body: Value) -> (StatusCode, Value) {
    send("POST", app, uri, tok, Some(body)).await
}

async fn delete(app: &axum::Router, uri: &str, tok: Option<&str>) -> (StatusCode, Value) {
    send("DELETE", app, uri, tok, None).await
}

async fn send(
    method: &str,
    app: &axum::Router,
    uri: &str,
    tok: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = tok {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&v).unwrap()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

use tower::ServiceExt;

async fn seed_user(db: &AnyPool, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, phone, nickname, age_declared, role, status, created_at, updated_at) \
         VALUES ($1, $2, '昵称', 1, 'user', 'active', $3, $4)",
    )
    .bind(id)
    .bind(format!("1{:010}", id.len() * 7919 % 1_000_000_000))
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

async fn seed_world(db: &AnyPool, id: &str) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, created_at, updated_at) \
         VALUES ($1, 'tpl_x', 1, 'e1', 'p1', 'm1', 'idle', '世界', 'open', $2, $3)",
    )
    .bind(id)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 直插一条记录（绕过端点校验，用于构造**损坏配置**等端点不允许写出的状态）。
#[allow(clippy::too_many_arguments)]
async fn raw_insert(
    db: &AnyPool,
    id: &str,
    flag: &str,
    scope: &str,
    target: &str,
    enabled: i64,
    starts: i64,
    ends: i64,
) {
    sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 'tester', $8, '用例', $9)",
    )
    .bind(id)
    .bind(flag)
    .bind(scope)
    .bind(target)
    .bind(enabled)
    .bind(starts)
    .bind(ends)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

const F: &str = "MUSE_ONBOARDING";

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 红线：默认关闭仍然是默认
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **本任务最大的风险点**：引入开关体系不得让任何未验证功能变成默认开启。
///
/// 断言写成**按来源分支**而不是「直接断言全 false」，是因为其余 8 个开关的 env 被
/// 别的模块用例（`worlds::DeathmatchSwitch`、`memorial`、`subplot`…）并发摆布着，
/// 在这里强行清空它们只会造出一个随机失败的用例。真正要钉死的命题是：
/// **DB 空且没有 env 时（source=default），取值必须等于登记表里声明的默认值**；
/// 而「声明的默认值除审核链外全为 false」由 `red_line_only_safety_chain_defaults_on` 锁死。
/// 两条合起来即「引入开关体系没有把任何未验证功能变成默认开启」。
#[tokio::test]
async fn red_line_empty_db_and_no_env_means_disabled() {
    let state = test_state().await;

    for def in KNOWN_FLAGS {
        let r = resolve(&state.db, def.name, FlagCtx::global()).await;
        match &r.source {
            // env 未设（或设了垃圾值）→ 必须精确等于声明的默认值。
            FlagSource::Default => assert_eq!(
                r.enabled, def.default_enabled,
                "🔴 {} 在 DB 空 + 无 env 时必须回落到声明默认值 {}",
                def.name, def.default_enabled
            ),
            // 并发用例设了 env——env 语义由 empty_db_reproduces_env_semantics_exactly 专门覆盖。
            FlagSource::Env { .. } => {}
            other => panic!("🔴 {} 空表时不该出现来源 {other:?}", def.name),
        }
    }

    // 参考接线的那个开关可以拿到锁，于是能做**确定性**的强断言。
    let _g = env_guard(None, &[]);
    let r = resolve(&state.db, F, FlagCtx::global()).await;
    assert_eq!(r.source.kind(), "default", "env 已移除，必须走默认值分支");
    assert!(!r.enabled, "🔴 {F} 是未验证功能，DB 空 + env 未设时必须为关");
    assert!(!crate::onboarding::onboarding_enabled(&state.db, Some("usr_any")).await);
}

/// 🔴 **每个开关都必须声明它实际解析哪几档，且必须含 global。**
///
/// `scopes` 描述的是**代码现状**（消费点真的解析了哪几个维度），不是「允许配哪几档」。
/// 少了这一列，给一个只读 global 的开关写一条 world 记录会**写得进去、且毫无效果**——
/// 而 `admin_api::flags::set_flag` 自己的注释就把这种情形称作
/// 「这套体系最难自查的失败模式」。必须含 global 是因为全局档是每个开关的兜底面：
/// 不给它，这个开关就没法平台级开合，急停阀就没有阀门。
#[test]
fn every_flag_declares_scopes_including_global() {
    for def in KNOWN_FLAGS {
        assert!(!def.scopes.is_empty(), "🔴 {} 未声明 scopes", def.name);
        assert!(
            def.scopes.contains(&SCOPE_GLOBAL),
            "🔴 {} 的 scopes 必须含 global —— 否则它没有平台级开合的阀门",
            def.name
        );
        for sc in def.scopes {
            assert!(
                SCOPES_BY_PRIORITY.contains(sc),
                "🔴 {} 声明了非法作用域「{sc}」",
                def.name
            );
        }
    }
    // 🔴 审核链只允许平台级急停：按世界/按人关掉敏感词过滤不是运营动作，是内容安全事故。
    let lex = find_flag("MUSE_SAFETY_LEXICON").expect("审核链必须在登记表里");
    assert_eq!(
        lex.scopes,
        &[SCOPE_GLOBAL],
        "🔴 MUSE_SAFETY_LEXICON 只允许 global 作用域（MIGRATION_NOTES 原注的迁移前提）"
    );
}

/// 🔴 写一条**该开关根本不解析**的作用域记录 → 400，而不是写进去毫无效果。
#[tokio::test]
async fn writing_a_scope_the_flag_never_reads_is_rejected() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");
    seed_world(&state.db, "wld_x").await;

    // MUSE_ONBOARDING 只解析 user/global（微本世界是领礼包时才建的）。
    let (st, body) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "world", "targetId": "wld_x", "enabled": true, "reason": "试" }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "🔴 不解析的档必须被拒绝: {body}");
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("不解析"), "错误信息要说清是「读不到」而不是「没权限」: {msg}");
    assert!(msg.contains("user"), "并要告诉运营它实际解析哪几档: {msg}");
    assert_eq!(
        count_flag_rows(&state.db).await,
        0,
        "🔴 被拒绝的写入一行都不许落库（否则就成了「有记录但不生效」）"
    );

    // 审核链：连 world/user 都不许写，只能平台级急停。
    for scope in ["world", "user"] {
        let target = if scope == "world" { "wld_x" } else { "usr_x" };
        let (st, _) = post(
            &app,
            "/api/admin/flags",
            Some(&admin),
            json!({ "flag": "MUSE_SAFETY_LEXICON", "scope": scope, "targetId": target,
                    "enabled": false, "reason": "试" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "🔴 审核链不得按 {scope} 关闭");
    }
}

async fn count_flag_rows(db: &AnyPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags").fetch_one(db).await.unwrap()
}

/// 🔴 登记表本身的红线：除审核链外，`default_enabled` 必须全为 false。
/// 谁把某个未验证功能的默认值改成 true，这条立刻红。
#[test]
fn red_line_only_safety_chain_defaults_on() {
    for def in KNOWN_FLAGS {
        if def.name == "MUSE_SAFETY_LEXICON" {
            assert!(def.default_enabled, "审核链的 fail-safe 方向是「继续过滤」，默认必须为 true");
        } else {
            assert!(
                !def.default_enabled,
                "🔴 VALIDATION §0.1：未验证功能 {} 的默认值必须为 false",
                def.name
            );
        }
    }
    // 迁移 0036 不插种子数据 —— 登记表非空但数据表为空，是「有开关体系 ≠ 功能被打开」的形状。
    //
    // 计数 9 → 10 → 11 → 12：R3 的三条新建件 `MUSE_OOC_ANNOTATIONS`（OOC 注解权，§7 人设保险第 2 级）、
    // `MUSE_IFLINE_PARALLEL`（if 线付费副本，§7 人设保险第 3 级）
    // 与 `MUSE_SOCIAL_IDENTITY_UNLOCK`（真人社交解锁，§14【拍板 22】恨隔面具原则）
    // **从建成之日起就经本体系解析**（`wired: true`，无历史 env 语义要保留）。
    //
    // 12 → 13：`MUSE_OFFPEAK_SCHEDULING`（错峰调度，§17【拍板 16】）。⚠️ 它与上面三条**性质不同**——
    // 前三条是新建件，它是**第一个从纯 env 迁进体系的存量开关**，所以待迁移数同步 8 → 7。
    // 语义连续（env 仍是解析链第 ④ 层兜底），变化只是前面多了 user/world/global 三层，
    // 错峰从「全局一刀切」变成可按世界灰度。
    //
    // 13 → 14：`MUSE_LIVE_STAGE`（直播场 = 定档 + 延迟缓冲 + 弹幕，R3 收官件）。
    // 与 OOC 注解权 / if 线 / 真人社交解锁同性质——新建件，无历史 env 语义，建成即接线。
    //
    // 14 → 15：`MUSE_DISPOSAL_NAME_GATE`（被处置内容的卡名读取面闸门，migration 0044 的第四条腿）。
    // ⚠️ 它与「已过审内容处置」本身**不是一回事**：处置能力是合规设施、恒开、刻意不登记开关
    // （见 VALIDATION §3.1 的边界段）；本开关只决定「已经露在存量世界里的名字要不要换成中性占位」，
    // 那会改变运行中世界对玩家的显示，是产品决策，所以按 §0.1 默认关闭。
    //
    // 15 → 16：`MUSE_SAFETY_SEMANTIC_RECHECK`（§15 第 3 层语义分类异步复核，migration 0046）。
    // ⚠️ 它是**第二个属于 `safety` 的开关**，而与 `MUSE_SAFETY_LEXICON` 的默认值**相反**——
    // 上面那个循环因此仍然只给词库闸开了口子。两者不矛盾：默认值一律指向「不改变现状」的那一侧，
    // 词库闸已经在线上跑（关掉 = 现状被改），第 3 层从未生效过（开着 = 现状被改，且开始烧 token）。
    // 🔴 它默认关闭还有第二重意义：provider 目前是 Dev 桩，开着也拦不住任何东西，
    // 默认关闭避免了「链路开着」被误读成「防线生效」。
    //
    // 16 → 17：`MUSE_SAFETY_RECHECK_SWEEP`（第 3 层复核的补偿轮询，migration 0048）。
    // ⚠️ 这是**第三个属于 `safety` 的开关**，默认同样为关，但理由与上面两个都不同：
    // 它是全仓唯一一处**凭数据自发烧 token** 的路径——不需要有人推进世界，只要数据里
    // 有「没被复核过的拍」它就会发起送审。默认开着等于让一次代码合并抬高成本曲线而
    // 没有人按过开关，正是 §0.1 要挡的那件事。
    //
    // 17 → 18：`MUSE_IFLINE_ADVANCE_SWEEP`（if 线推进的对账式补偿，migration 0052）。
    // ⚠️ 与上一条（`MUSE_SAFETY_RECHECK_SWEEP`）同形态、同理由，但**赌注更大**：
    // 它是第二处「凭数据自发调模型」的路径，而 if 线是**付费**内容——一次补投就是一次真的
    // 模型调用。故它除了默认关闭，还自带补投次数封顶（`MUSE_IFLINE_SWEEP_MAX_REDELIVERIES`）：
    // 一个每次都在清标记前就死掉的 worker，配上无封顶的补投，就是个无限烧钱的循环。
    //
    // 18 → 21：`MUSE_ATTENTION_TICK_FAILURE` / `MUSE_ATTENTION_BLOCKED_STREAK` /
    // `MUSE_ATTENTION_STALLED`（健康档三个新维度，open-decisions §1 于 2026-07-28 产品选定）。
    // ⚠️ 这三个与前面所有开关都**不同类**：它们既不改变用户可见范围，也不烧任何 token——
    // 它们只改运营看板上「需关注」这个数**的含义**。默认关闭的理由因此也不同：
    // 开着合并意味着**没有人按过开关，而某天早上那个数字自己变大了**，
    // 运营会去追一个并不存在的事故。§0.1 在这里挡的是「口径静默漂移」而不是「功能静默上线」。
    // 🔵 三条各自还带自己的阈值 env（§0.2 参数化），开关只管开不开。
    //
    // 21 → 22：`MUSE_AMBIENT_GIFT_EVENTS`（观众礼物 → 引擎回合的**展示层**注入，
    // open-decisions §5 于 2026-07-28 拍板选项 A）。
    // 🔴 这是全表**赌注最大**的一个默认关闭：其它开关关错了是功能没上线或多烧点 token，
    // 这一个一旦上线并被玩家感知为「打赏有用」，**撤回就等于承认平台卖过优势**——
    // 而平台红线第一条正是「不卖胜负与数值平权」。它必须是有人按过开关才生效。
    // 边界另有引擎侧源码级红线扫死（`ambient_events_never_leave_the_presentation_layer`）。
    //
    // 这些都是登记表**新增了默认关闭的开关**，不是断言被放宽——上面那个循环仍逐条钉死
    // 「除审核链外默认值必须为 false」，新开关同样在其中。
    //
    // 🔵 这条计数是**有意的棘轮**，不是会过期的基线：它就是要在每次新增开关时变红，
    // 逼一次人工评审（同 `red_line_world_events_has_one_ratchet_and_one_guarded_relax`
    // 钉死「world_events 上只许有 3 条写路径」）。与 CLAUDE.md 里去掉的那几处硬编码计数
    // 性质相反——那些是**描述现状**的文档数字（过期即误导），这个是**约束变更**的闸门
    // （过期即报警，正是它要的）。
    assert_eq!(
        KNOWN_FLAGS.len(),
        22,
        "登记表应覆盖 9 个存量 env 开关 + R3 新建的 MUSE_OOC_ANNOTATIONS / MUSE_IFLINE_PARALLEL / \
         MUSE_SOCIAL_IDENTITY_UNLOCK / MUSE_LIVE_STAGE + MUSE_DISPOSAL_NAME_GATE + \
         MUSE_SAFETY_SEMANTIC_RECHECK + MUSE_SAFETY_RECHECK_SWEEP + MUSE_IFLINE_ADVANCE_SWEEP \
         + 健康档三维度（MUSE_ATTENTION_TICK_FAILURE / _BLOCKED_STREAK / _STALLED），\
         其中 MUSE_OFFPEAK_SCHEDULING 已由纯 env 迁入体系"
    );
}

/// 迁移本身不得插入任何记录（种子数据 = 静默开闸）。
#[tokio::test]
async fn red_line_migration_seeds_no_rows() {
    let state = test_state().await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "🔴 迁移 0036 不得插入任何种子记录");
}

// ═══════════════════════════════════════════════════════════════════════════
// env 兜底与回归保护
// ═══════════════════════════════════════════════════════════════════════════

/// DB 空 → 完全按 env 解析。
#[tokio::test]
async fn empty_db_falls_back_to_env() {
    let state = test_state().await;
    {
        let _g = env_guard(Some("1"), &[]);
        assert!(is_enabled(&state.db, F, FlagCtx::global()).await);
    }
    {
        let _g = env_guard(Some("0"), &[]);
        assert!(!is_enabled(&state.db, F, FlagCtx::global()).await);
    }
}

/// 🔴 **回归保护**：DB 空时，`flags` 的 env 解析与接线前的旧实现**逐值一致**。
/// 旧实现（`onboarding::onboarding_enabled` 的原体）在此原样复刻为 `legacy`，逐个取值比对。
#[tokio::test]
async fn empty_db_reproduces_env_semantics_exactly() {
    // 接线前的原实现，逐字复刻。
    fn legacy(raw: Option<&str>) -> bool {
        const DEFAULT: bool = false;
        match raw {
            Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "on" | "yes" => true,
                "0" | "false" | "off" | "no" => false,
                _ => DEFAULT,
            },
            None => DEFAULT,
        }
    }

    let state = test_state().await;
    // 含大小写、空白、垃圾值、空串——垃圾值必须回落默认（关），不得静默开启。
    let cases = [
        None,
        Some("1"),
        Some("0"),
        Some("true"),
        Some("TRUE"),
        Some(" On "),
        Some("yes"),
        Some("false"),
        Some("off"),
        Some("no"),
        Some(""),
        Some("   "),
        Some("maybe"),
        Some("2"),
        Some("enabled"),
    ];
    for raw in cases {
        let _g = env_guard(raw, &[]);
        let got = is_enabled(&state.db, F, FlagCtx::global()).await;
        assert_eq!(got, legacy(raw), "env={raw:?} 时新旧实现必须一致");
    }
}

/// 参考接线的模块函数本身也走同一条链（`onboarding::onboarding_enabled`）。
#[tokio::test]
async fn wired_onboarding_matches_legacy_env_semantics() {
    let state = test_state().await;
    for (raw, want) in [(None, false), (Some("1"), true), (Some("maybe"), false), (Some("no"), false)]
    {
        let _g = env_guard(raw, &[]);
        assert_eq!(
            crate::onboarding::onboarding_enabled(&state.db, None).await,
            want,
            "env={raw:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DB 覆盖 env + 作用域优先级
// ═══════════════════════════════════════════════════════════════════════════

/// DB 记录压过 env（两个方向都测：env 关→DB 开、env 开→DB 关）。
#[tokio::test]
async fn db_record_overrides_env_both_directions() {
    let state = test_state().await;

    {
        let _g = env_guard(Some("0"), &[]);
        raw_insert(&state.db, "flg_on", F, SCOPE_GLOBAL, "", 1, 0, 0).await;
        assert!(is_enabled(&state.db, F, FlagCtx::global()).await, "DB 开应压过 env 关");
    }
    sqlx::query("DELETE FROM runtime_flags").execute(&state.db).await.unwrap();
    {
        let _g = env_guard(Some("1"), &[]);
        raw_insert(&state.db, "flg_off", F, SCOPE_GLOBAL, "", 0, 0, 0).await;
        assert!(!is_enabled(&state.db, F, FlagCtx::global()).await, "DB 关应压过 env 开");
    }
}

/// 🔴 三作用域优先级：**用户 > 世界 > 全局**（窄的赢）。
/// 一次性摆出三条互相矛盾的记录，逐层剥掉，验证每一层都真的接管了。
#[tokio::test]
async fn scope_priority_user_beats_world_beats_global() {
    let _g = env_guard(Some("0"), &[]); // env 关，确保结论来自 DB
    let state = test_state().await;
    let (u, w) = ("usr_p", "wld_p");

    // 全局关 / 世界开 / 用户关：三层互相矛盾。
    raw_insert(&state.db, "f_g", F, SCOPE_GLOBAL, "", 0, 0, 0).await;
    raw_insert(&state.db, "f_w", F, SCOPE_WORLD, w, 1, 0, 0).await;
    raw_insert(&state.db, "f_u", F, SCOPE_USER, u, 0, 0, 0).await;

    let ctx = FlagCtx::user(u).with_world(w);
    assert!(!is_enabled(&state.db, F, ctx).await, "用户层(关)必须赢过世界层(开)");

    // 剥掉用户层 → 世界层接管（开）。
    sqlx::query("DELETE FROM runtime_flags WHERE id = 'f_u'").execute(&state.db).await.unwrap();
    assert!(is_enabled(&state.db, F, ctx).await, "剥掉用户层后世界层(开)接管");

    // 剥掉世界层 → 全局层接管（关）。
    sqlx::query("DELETE FROM runtime_flags WHERE id = 'f_w'").execute(&state.db).await.unwrap();
    assert!(!is_enabled(&state.db, F, ctx).await, "剥掉世界层后全局层(关)接管");

    // 剥掉全局层 → 回落 env（关）。
    sqlx::query("DELETE FROM runtime_flags WHERE id = 'f_g'").execute(&state.db).await.unwrap();
    let r = resolve(&state.db, F, ctx).await;
    assert!(!r.enabled);
    assert_eq!(r.source.kind(), "env", "全部记录剥掉后必须回落 env");
}

/// 上下文里没给的作用域不参与匹配：只给 user 时，world 记录不生效。
#[tokio::test]
async fn scope_not_in_ctx_is_skipped() {
    let _g = env_guard(Some("0"), &[]);
    let state = test_state().await;
    raw_insert(&state.db, "f_w", F, SCOPE_WORLD, "wld_x", 1, 0, 0).await;

    assert!(!is_enabled(&state.db, F, FlagCtx::user("usr_x")).await, "ctx 无 world_id 时不该命中");
    assert!(is_enabled(&state.db, F, FlagCtx::world("wld_x")).await, "ctx 带对的 world_id 才命中");
    assert!(!is_enabled(&state.db, F, FlagCtx::world("wld_other")).await, "别的世界不受影响");
}

// ═══════════════════════════════════════════════════════════════════════════
// 时间窗
// ═══════════════════════════════════════════════════════════════════════════

/// 窗口内生效 / 未到 / 已过期。窗口外 = 回落更宽作用域（不是强制关闭）。
#[tokio::test]
async fn time_window_activates_and_expires() {
    let _g = env_guard(Some("0"), &[]);
    let state = test_state().await;
    let now = now_ms();
    // 全局开，窗口 [now-1000, now+1000)
    raw_insert(&state.db, "f_win", F, SCOPE_GLOBAL, "", 1, now - 1_000, now + 1_000).await;

    assert!(resolve_at(&state.db, F, FlagCtx::global(), now).await.enabled, "窗口内应生效");

    let early = resolve_at(&state.db, F, FlagCtx::global(), now - 5_000).await;
    assert!(!early.enabled, "窗口未到不生效");
    assert_eq!(early.source.kind(), "env", "窗口外应跳过该记录、回落 env");
    assert_eq!(early.skipped, vec!["global:".to_string()], "被跳过的记录要报出来供诊断");

    let late = resolve_at(&state.db, F, FlagCtx::global(), now + 5_000).await;
    assert!(!late.enabled, "窗口已过期不生效");
    assert_eq!(late.source.kind(), "env");

    // 右开区间：ends_at 那一毫秒本身已在窗外。
    assert!(!resolve_at(&state.db, F, FlagCtx::global(), now + 1_000).await.enabled);
    // 左闭区间：starts_at 那一毫秒已在窗内。
    assert!(resolve_at(&state.db, F, FlagCtx::global(), now - 1_000).await.enabled);
}

/// 单边窗口：只给 starts_at（此后永久生效）/ 只给 ends_at（此前一直生效）。
#[tokio::test]
async fn one_sided_windows() {
    let _g = env_guard(Some("0"), &[]);
    let state = test_state().await;
    let now = now_ms();

    raw_insert(&state.db, "f_from", F, SCOPE_GLOBAL, "", 1, now, 0).await;
    assert!(!resolve_at(&state.db, F, FlagCtx::global(), now - 1).await.enabled);
    assert!(resolve_at(&state.db, F, FlagCtx::global(), now + 10_000_000).await.enabled);

    sqlx::query("DELETE FROM runtime_flags").execute(&state.db).await.unwrap();
    raw_insert(&state.db, "f_until", F, SCOPE_GLOBAL, "", 1, 0, now).await;
    assert!(resolve_at(&state.db, F, FlagCtx::global(), now - 1).await.enabled);
    assert!(!resolve_at(&state.db, F, FlagCtx::global(), now).await.enabled);
}

/// 窗口外的**窄层**记录回落到**宽层**记录（而非直接关闭）——组合性的关键。
#[tokio::test]
async fn expired_narrow_record_falls_back_to_wider_scope() {
    let _g = env_guard(Some("0"), &[]);
    let state = test_state().await;
    let now = now_ms();
    raw_insert(&state.db, "f_g", F, SCOPE_GLOBAL, "", 1, 0, 0).await; // 全局常开
    raw_insert(&state.db, "f_u", F, SCOPE_USER, "usr_e", 0, 0, now).await; // 用户层「曾经关过」，已过期

    let r = resolve_at(&state.db, F, FlagCtx::user("usr_e"), now).await;
    assert!(r.enabled, "用户层窗口过期后应回落到全局层(开)");
    assert_eq!(r.source.kind(), "db");
    assert_eq!(r.skipped, vec!["user:usr_e".to_string()]);
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 红线：fail-closed
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 损坏配置一律 fail-closed 到声明默认值，**且不回落 env**
/// （否则「配坏了」会被静默降级成「按 env 开着」）。
#[tokio::test]
async fn red_line_corrupt_records_fail_closed() {
    // env 故意设成 **开**：若 fail-closed 写错成「跳过损坏行继续回落」，这里会变成 true 而漏检。
    let _g = env_guard(Some("1"), &[]);

    // 逐种损坏形态各起一个干净库（损坏是整开关级的，混在一起测不出是哪条触发的）。
    let cases: Vec<(&str, &str, &str, i64, i64, i64)> = vec![
        // (说明, scope, target, enabled, starts, ends)
        ("作用域拼错", "wrold", "wld_1", 1, 0, 0),
        ("global 带了 target", SCOPE_GLOBAL, "wld_1", 1, 0, 0),
        ("user 没带 target", SCOPE_USER, "", 1, 0, 0),
        ("enabled 非 0/1", SCOPE_GLOBAL, "", 7, 0, 0),
        ("时间窗为负", SCOPE_GLOBAL, "", 1, -5, 0),
        ("时间窗反转", SCOPE_GLOBAL, "", 1, 9_000, 1_000),
    ];

    for (desc, scope, target, enabled, starts, ends) in cases {
        let state = test_state().await;
        raw_insert(&state.db, "f_bad", F, scope, target, enabled, starts, ends).await;

        let r = resolve(&state.db, F, FlagCtx::user("usr_1").with_world("wld_1")).await;
        assert!(!r.enabled, "🔴 {desc}：损坏配置必须 fail-closed 到关（env 开也不许放行）");
        assert_eq!(r.source.kind(), "fail_closed", "🔴 {desc}：来源必须标为 fail_closed");
    }
}

/// 🔴 查库失败（表被删）→ fail-closed 到关，不 panic、不回落 env。
#[tokio::test]
async fn red_line_query_failure_fails_closed() {
    let _g = env_guard(Some("1"), &[(ENV_CACHE_TTL_MS, "0")]);
    let state = test_state().await;
    sqlx::query("DROP TABLE runtime_flags").execute(&state.db).await.unwrap();

    let r = resolve(&state.db, F, FlagCtx::global()).await;
    assert!(!r.enabled, "🔴 查库失败必须按关处理（env 设为开也不许放行）");
    assert_eq!(r.source.kind(), "fail_closed");
}

/// 🔴 审核链的 fail-safe 方向相反：出错时必须保持**开着**（继续过滤），不能退成放行。
#[tokio::test]
async fn red_line_safety_chain_fails_safe_to_on() {
    const SAFETY: &str = "MUSE_SAFETY_LEXICON";
    let _g = env_guard(None, &[(SAFETY, "0"), (ENV_CACHE_TTL_MS, "0")]);
    let state = test_state().await;
    sqlx::query("DROP TABLE runtime_flags").execute(&state.db).await.unwrap();

    let r = resolve(&state.db, SAFETY, FlagCtx::global()).await;
    assert!(r.enabled, "🔴 审核链出错时必须保持过滤（fail-safe 方向是开，不是字面的 false）");
    assert_eq!(r.source.kind(), "fail_closed");
}

/// 未登记的开关名 → 关（打错一个字母不该悄悄得到某个值）。
#[tokio::test]
async fn unknown_flag_name_is_disabled() {
    let state = test_state().await;
    let r = resolve(&state.db, "MUSE_NOT_A_REAL_FLAG", FlagCtx::global()).await;
    assert!(!r.enabled);
    assert_eq!(r.source.kind(), "fail_closed");
}

// ═══════════════════════════════════════════════════════════════════════════
// 缓存
// ═══════════════════════════════════════════════════════════════════════════

/// 缓存开启时：① 命中缓存能挡住直插的库变更（证明缓存真的在工作）；
/// ② `invalidate` 之后立即看到新值（证明「运营点了开关不用等 TTL」）。
#[tokio::test]
async fn cache_hides_raw_writes_but_invalidate_is_immediate() {
    // TTL 拉到 60s：若失效逻辑坏了，用例会实实在在地读到旧值而不是靠 TTL 蒙混过关。
    let _g = env_guard(Some("0"), &[(ENV_CACHE_TTL_MS, "60000")]);
    let state = test_state().await;

    // 先读一次填充快照（此时表空 → 关）。
    assert!(!is_enabled(&state.db, F, FlagCtx::global()).await);

    // 绕过端点直插「开」：缓存未失效，应仍读到关。
    raw_insert(&state.db, "f_c", F, SCOPE_GLOBAL, "", 1, 0, 0).await;
    assert!(!is_enabled(&state.db, F, FlagCtx::global()).await, "TTL 内应命中快照，读到旧值");

    // 失效后立即生效。
    invalidate(&state.db);
    assert!(is_enabled(&state.db, F, FlagCtx::global()).await, "invalidate 后必须立即读到新值");
}

/// 🔴 缓存失效及时性的**端到端**版本：走 admin 端点改开关，下一次读取立即生效。
/// 这是运营真实路径——端点内部已封了 `invalidate`，运营不需要知道缓存的存在。
#[tokio::test]
async fn cache_invalidation_through_admin_endpoint_is_immediate() {
    let _g = env_guard(Some("0"), &[(ENV_CACHE_TTL_MS, "60000")]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");

    assert!(!is_enabled(&state.db, F, FlagCtx::global()).await);

    let (st, _) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "global", "enabled": true, "reason": "T0 开测" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    assert!(
        is_enabled(&state.db, F, FlagCtx::global()).await,
        "🔴 运营点完开关必须立即生效，不能等 TTL"
    );

    // 关回去同样立即生效。
    let (st, _) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "global", "enabled": false, "reason": "急停" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(!is_enabled(&state.db, F, FlagCtx::global()).await, "急停必须立即生效");
}

/// TTL=0 时完全不走缓存（每次直查库）——给需要强一致的部署留的逃生口。
#[tokio::test]
async fn ttl_zero_disables_cache() {
    let _g = env_guard(Some("0"), &[(ENV_CACHE_TTL_MS, "0")]);
    let state = test_state().await;
    assert!(!is_enabled(&state.db, F, FlagCtx::global()).await);
    raw_insert(&state.db, "f_c", F, SCOPE_GLOBAL, "", 1, 0, 0).await;
    assert!(is_enabled(&state.db, F, FlagCtx::global()).await, "TTL=0 应每次直查库");
}

// ═══════════════════════════════════════════════════════════════════════════
// RBAC
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 写操作 **admin 专属**：operator/reviewer/support/finance 一律 403；普通用户 403；无 token 401。
#[tokio::test]
async fn rbac_only_admin_can_write_flags() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let body = json!({ "flag": F, "scope": "global", "enabled": true, "reason": "试图开闸" });

    for role in ["operator", "reviewer", "support", "finance"] {
        let t = token(&state, &format!("actor_{role}"), role);
        let (st, _) = post(&app, "/api/admin/flags", Some(&t), body.clone()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "🔴 {role} 不得改开关");
    }
    let (st, _) = post(&app, "/api/admin/flags", Some(&token(&state, "u1", "user")), body.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "🔴 普通用户不得改开关");

    let (st, _) = post(&app, "/api/admin/flags", None, body.clone()).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "无 token 应 401");

    // 🔴 最要紧的一条：被拒之后数据库里不能留下任何记录。
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "🔴 无权限的写请求必须零副作用");
    assert!(!is_enabled(&state.db, F, FlagCtx::global()).await, "开关仍必须是关");
}

/// 删除同样 admin 专属。
#[tokio::test]
async fn rbac_only_admin_can_delete_flags() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    raw_insert(&state.db, "f_d", F, SCOPE_GLOBAL, "", 1, 0, 0).await;

    let op = token(&state, "op1", "operator");
    let (st, _) = delete(&app, "/api/admin/flags/f_d?reason=x", Some(&op)).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 1, "被拒的删除必须零副作用");
}

/// 读端点放给 operator（急停时更需要看得见），但普通用户仍进不来。
#[tokio::test]
async fn rbac_operator_can_read_flags() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());

    let (st, body) = get(&app, "/api/admin/flags", Some(&token(&state, "op1", "operator"))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["flags"].as_array().unwrap().len(), KNOWN_FLAGS.len());

    let (st, _) = get(&app, "/api/admin/flags", Some(&token(&state, "u1", "user"))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, _) = get(&app, "/api/admin/flags/resolve?flag=MUSE_ONBOARDING", Some(&token(&state, "op1", "operator"))).await;
    assert_eq!(st, StatusCode::OK);
}

// ═══════════════════════════════════════════════════════════════════════════
// 运营端点：校验、审计、dry-run
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 每次开关变更都落 `audit_logs`，且 reason 里带**变更前后**状态（只写新值无法复盘改了什么）。
#[tokio::test]
async fn audit_trail_records_who_when_what_why() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin_zhang", "admin");
    seed_user(&state.db, "usr_a").await;

    // 建（无 → 开）
    let (st, _) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "user", "targetId": "usr_a", "enabled": true,
                "reason": "T0 邀请制首批内测" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 改（开 → 关）
    let (st, _) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "user", "targetId": "usr_a", "enabled": false,
                "reason": "该用户反馈异常，先关" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let rows = sqlx::query(
        "SELECT actor_id, actor_role, action, subject, reason, created_at FROM audit_logs \
         WHERE action = 'flag.set' ORDER BY created_at ASC",
    )
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "两次变更两条留痕");

    for r in &rows {
        assert_eq!(r.get::<String, _>("actor_id"), "admin_zhang", "谁");
        assert_eq!(r.get::<String, _>("actor_role"), "admin");
        assert_eq!(r.get::<String, _>("subject"), format!("{F}:user:usr_a"), "改了哪个开关的哪个目标");
        assert!(r.get::<i64, _>("created_at") > 0, "何时");
    }
    let first: String = rows[0].get("reason");
    assert!(first.contains("<无记录> -> on"), "首次应记录「无 → 开」，实得：{first}");
    assert!(first.contains("T0 邀请制首批内测"), "运营填的理由要留下");
    let second: String = rows[1].get("reason");
    assert!(second.contains("on[0~0] -> off"), "第二次应记录「开 → 关」，实得：{second}");
    assert!(second.contains("该用户反馈异常"));

    // 记录行自身也留痕（现状面，与 audit_logs 流水面互补）。
    let rec = get_record(&state.db, &sqlx::query_scalar::<_, String>(
        "SELECT id FROM runtime_flags WHERE flag = $1 AND scope = 'user' AND target_id = 'usr_a'",
    )
    .bind(F)
    .fetch_one(&state.db)
    .await
    .unwrap())
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rec.updated_by, "admin_zhang");
    assert!(rec.updated_at > 0);
    assert!(rec.reason.contains("该用户反馈异常"));
}

/// 删除也留痕，且记下「删掉的是什么」。
#[tokio::test]
async fn delete_is_audited_with_prior_state() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");
    raw_insert(&state.db, "f_del", F, SCOPE_GLOBAL, "", 1, 0, 0).await;

    let (st, body) = delete(&app, "/api/admin/flags/f_del?reason=灰度结束收回", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["deleted"], json!(true));
    // 删除后回落到 env/默认（关）——回执直接告诉运营「现在它变成了什么」。
    assert_eq!(body["fallsBackTo"]["enabled"], json!(false));

    let reason: String = sqlx::query_scalar(
        "SELECT reason FROM audit_logs WHERE action = 'flag.delete' LIMIT 1",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert!(reason.contains("删除 on[0~0]"), "要记下删掉的是什么，实得：{reason}");
    assert!(reason.contains("灰度结束收回"));
    assert!(!is_enabled(&state.db, F, FlagCtx::global()).await);
}

/// 🔴 理由必填：不填 reason 一律 400，且零副作用。
#[tokio::test]
async fn reason_is_mandatory_for_writes() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");

    for bad in [json!({ "flag": F, "scope": "global", "enabled": true }),
                json!({ "flag": F, "scope": "global", "enabled": true, "reason": "   " })] {
        let (st, _) = post(&app, "/api/admin/flags", Some(&admin), bad).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "🔴 无理由的开关变更必须被拒");
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags").fetch_one(&state.db).await.unwrap();
    assert_eq!(n, 0);

    raw_insert(&state.db, "f_x", F, SCOPE_GLOBAL, "", 1, 0, 0).await;
    let (st, _) = delete(&app, "/api/admin/flags/f_x", Some(&admin)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "🔴 删除也必须给理由");
}

/// 写入期校验：未登记开关名 / 非法作用域 / 目标 id 缺失或多余 / 反转窗口 / 目标不存在。
#[tokio::test]
async fn write_validation_rejects_bad_input() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");
    let r = "理由";

    let cases = vec![
        ("未登记开关名", json!({ "flag": "MUSE_NOPE", "scope": "global", "enabled": true, "reason": r })),
        ("非法作用域", json!({ "flag": F, "scope": "planet", "enabled": true, "reason": r })),
        ("global 多给了 target", json!({ "flag": F, "scope": "global", "targetId": "x", "enabled": true, "reason": r })),
        ("user 少了 target", json!({ "flag": F, "scope": "user", "enabled": true, "reason": r })),
        ("时间窗反转", json!({ "flag": F, "scope": "global", "enabled": true, "startsAt": 9000, "endsAt": 1000, "reason": r })),
        ("时间窗为负", json!({ "flag": F, "scope": "global", "enabled": true, "startsAt": -1, "reason": r })),
        // 打错 id 是灰度名单最难自查的失败模式：写入期就挡掉。
        ("用户不存在", json!({ "flag": F, "scope": "user", "targetId": "usr_ghost", "enabled": true, "reason": r })),
        ("世界不存在", json!({ "flag": F, "scope": "world", "targetId": "wld_ghost", "enabled": true, "reason": r })),
    ];
    for (desc, body) in cases {
        let (st, _) = post(&app, "/api/admin/flags", Some(&admin), body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{desc} 应被拒");
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags").fetch_one(&state.db).await.unwrap();
    assert_eq!(n, 0, "全部被拒，零副作用");
}

/// upsert 语义：同一 (flag, scope, target) 反复写只有一行，后写的赢。
#[tokio::test]
async fn set_flag_is_upsert_not_append() {
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");
    // ⚠️ 用 **user** 档而不是 world：`MUSE_ONBOARDING` 的消费点只解析 user/global
    //（微本世界是领礼包时**才建**的，判定发生在世界存在之前）。本用例原先写的是 world 档，
    // 即一条**写得进去却永远不生效**的记录——`KNOWN_FLAGS.scopes` 校验上线后第一个抓到的就是它。
    seed_user(&state.db, "usr_u").await;

    for (on, why) in [(true, "开灰度"), (false, "关灰度"), (true, "再开")] {
        let (st, _) = post(
            &app,
            "/api/admin/flags",
            Some(&admin),
            json!({ "flag": F, "scope": "user", "targetId": "usr_u", "enabled": on, "reason": why }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags").fetch_one(&state.db).await.unwrap();
    assert_eq!(n, 1, "同一目标只应有一行");
    assert!(is_enabled(&state.db, F, FlagCtx::user("usr_u")).await, "最后一次写的赢");
    // 三次变更三条留痕。
    let a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE action = 'flag.set'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(a, 3);
}

/// dry-run 端点如实报出「值 + 来自哪一层」，是复盘的主力工具。
#[tokio::test]
async fn resolve_endpoint_explains_the_source() {
    let _g = env_guard(Some("1"), &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");
    seed_user(&state.db, "usr_r").await;

    // 无记录 → 来自 env
    let (_, b) = get(&app, &format!("/api/admin/flags/resolve?flag={F}&userId=usr_r"), Some(&admin)).await;
    assert_eq!(b["enabled"], json!(true));
    assert_eq!(b["source"]["kind"], json!("env"));

    // 写一条用户层「关」→ 来自 db/user
    post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "user", "targetId": "usr_r", "enabled": false, "reason": "个案关闭" }),
    )
    .await;
    let (_, b) = get(&app, &format!("/api/admin/flags/resolve?flag={F}&userId=usr_r"), Some(&admin)).await;
    assert_eq!(b["enabled"], json!(false));
    assert_eq!(b["source"]["kind"], json!("db"));
    assert_eq!(b["source"]["scope"], json!("user"));
    // 别的用户不受影响，仍走 env。
    let (_, b2) = get(&app, &format!("/api/admin/flags/resolve?flag={F}&userId=usr_other"), Some(&admin)).await;
    assert_eq!(b2["enabled"], json!(true));
    assert_eq!(b2["source"]["kind"], json!("env"));
}

/// 列表端点报出每个开关的**全局生效值**与登记元信息（运营一眼看清大盘）。
#[tokio::test]
async fn list_endpoint_reports_effective_global_state() {
    // 只对能拿到锁的那个开关（MUSE_ONBOARDING）做确定性断言；其余 8 个的 env 被别的模块
    // 用例并发摆布，只校验与 env 无关的结构性字段。理由同 red_line_empty_db_and_no_env_means_disabled。
    let _g = env_guard(None, &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");

    let (st, b) = get(&app, "/api/admin/flags", Some(&admin)).await;
    assert_eq!(st, StatusCode::OK);
    let arr = b["flags"].as_array().unwrap();
    assert_eq!(arr.len(), KNOWN_FLAGS.len());
    for f in arr {
        let name = f["name"].as_str().unwrap();
        let def = find_flag(name).unwrap();
        assert_eq!(f["defaultEnabled"], json!(def.default_enabled), "{name} 的声明默认值");
        assert_eq!(f["owner"], json!(def.owner));
        if name == F {
            assert_eq!(f["globalEffective"], json!(false), "env 已移除 → 大盘为关");
            assert_eq!(f["globalSource"]["kind"], json!("default"));
        }
    }
    // 已接线本体系的开关。清单只增不减，且**每加一个都要在这里显式登记**——
    // 它是「哪些开关的 DB 记录真的会生效」的唯一清单，漏登记 = 运营点了没反应。
    //   - `MUSE_ONBOARDING`：0036 批次的参考接线（存量 env 开关的迁移样板）；
    //   - `MUSE_OOC_ANNOTATIONS`：R3 新建件，无历史 env 语义要保留，建成即接线；
    //   - `MUSE_IFLINE_PARALLEL`：同上（R3 if 线付费副本，§7 人设保险第 3 级）；
    //   - `MUSE_SOCIAL_IDENTITY_UNLOCK`：同上（R3 真人社交解锁，§14【拍板 22】恨隔面具原则）。
    //   - `MUSE_LIVE_STAGE`：同上（R3 直播场 = 定档 + 延迟缓冲 + 弹幕，§2 场次节奏三档 + §15 第 4 层）。
    //   - `MUSE_OFFPEAK_SCHEDULING`：⚠️ **不同性质**——第一个从纯 env 迁进体系的**存量**开关。
    //     `runtime::offpeak::enabled_for_world` 早已写好「已登记走体系、未登记退 env」的分支，
    //     所以登记这一步不需要改 runtime 一行代码，世界级灰度即刻可用。
    //   - `MUSE_SAFETY_SEMANTIC_RECHECK`：新建件，建成即接线（`safety::semantic::enabled`）。
    //     ⚠️ 它是**审核链**上的开关，却默认关闭（与同属 safety 的 `MUSE_SAFETY_LEXICON` 相反）——
    //     理由见 `red_line_only_safety_chain_defaults_on` 里那段：默认值指向「不改变现状」的一侧。
    //   - `MUSE_DISPOSAL_NAME_GATE`：新建件，建成即接线（`safety::disposal::NameGate::resolve`）。
    //     ⚠️ 它守的是**读取面显示**而不是一项功能的开合：关闭 = 各读取面逐字节维持今天的输出。
    // 其余存量开关仍是纯 env，迁移清单见 `flags::MIGRATION_NOTES`。
    // 🔴 **迁移清单走完之后，这条断言换了形态**：原先比对一份手维护的「已接线名单」，
    // 而那份名单每迁一个开关就要改一次——正是本仓库反复栽跟头的那类会过期的清单
    // （CLAUDE.md 里去掉的那几处硬编码计数是同一个病）。现在登记表里**每一个**开关都已接线，
    // 于是不变式可以写成自维护的形式：
    //
    //   **登记表里不允许存在 `wired=false` 的开关。**
    //
    // 新增一个未接线的开关会让本条立刻红——那正是要的：登记一个「写记录对它无效」的开关
    // 必须是一次显式决定，而不是顺手留下的半成品。若确有理由暂不接线，改本断言并在此写明理由。
    let unwired: Vec<&str> = arr
        .iter()
        .filter(|f| f["wired"] != json!(true))
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(
        unwired.is_empty(),
        "🔴 登记表里出现了未接线的开关 {unwired:?} —— 它们的运行时记录写了也不生效。\
         接线，或在本断言处写明为什么暂不接"
    );
    assert_eq!(
        arr.len(),
        KNOWN_FLAGS.len(),
        "列表端点应当把登记表逐条报出来（含未接线的，运营要看得见全貌）"
    );
    assert!(b["records"].as_array().unwrap().is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 参考接线端到端：onboarding 按用户灰度
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **T0「邀请制 ≤100 人」的可执行性证明**：全局关闭的前提下，只给指定用户开，
/// 该用户能看到新手动线，其他人仍是 404。这正是 env 两态开关做不到的事。
#[tokio::test]
async fn wired_onboarding_supports_per_user_canary() {
    // env 关：大盘不开放。
    let _g = env_guard(Some("0"), &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");
    seed_user(&state.db, "usr_invited").await;
    seed_user(&state.db, "usr_outsider").await;

    let invited = token(&state, "usr_invited", "user");
    let outsider = token(&state, "usr_outsider", "user");

    // 开测前：两人都看不到。
    for t in [&invited, &outsider] {
        let (st, _) = get(&app, "/api/onboarding/presets", Some(t)).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "大盘关闭时应 404");
    }

    // 只给受邀者开。
    let (st, _) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "user", "targetId": "usr_invited", "enabled": true,
                "reason": "T0 邀请制首批" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = get(&app, "/api/onboarding/presets", Some(&invited)).await;
    assert_eq!(st, StatusCode::OK, "🔴 受邀用户应立即看到新手动线");
    assert!(!body["presets"].as_array().unwrap().is_empty());

    let (st, _) = get(&app, "/api/onboarding/presets", Some(&outsider)).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "🔴 未受邀用户必须仍然 404");

    // 读端点同样按用户灰度（读取侧降级口径不变）。
    let (st, _) = get(&app, "/api/me/onboarding", Some(&invited)).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = get(&app, "/api/me/onboarding", Some(&outsider)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// 时间窗 + 按用户：窗口过期后受邀用户自动回落到大盘状态（关），无需运营再写一条「关闭」。
#[tokio::test]
async fn wired_onboarding_canary_expires_by_window() {
    let _g = env_guard(Some("0"), &[]);
    let state = test_state().await;
    let app = build_router(state.clone());
    let admin = token(&state, "admin1", "admin");
    seed_user(&state.db, "usr_w").await;
    let u = token(&state, "usr_w", "user");
    let now = now_ms();

    // 已经过期的窗口。
    let (st, body) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "user", "targetId": "usr_w", "enabled": true,
                "startsAt": now - 20_000, "endsAt": now - 10_000, "reason": "上一轮内测" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // 回执直接说明「现在并不生效」——写完就能发现窗口填错了。
    assert_eq!(body["effectiveNow"]["enabled"], json!(false));

    let (st, _) = get(&app, "/api/onboarding/presets", Some(&u)).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "窗口已过期，应回落大盘（关）");

    // 换成覆盖当下的窗口 → 立即生效。
    let (st, _) = post(
        &app,
        "/api/admin/flags",
        Some(&admin),
        json!({ "flag": F, "scope": "user", "targetId": "usr_w", "enabled": true,
                "startsAt": now - 10_000, "endsAt": now + 600_000, "reason": "本轮内测" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = get(&app, "/api/onboarding/presets", Some(&u)).await;
    assert_eq!(st, StatusCode::OK, "窗口内应生效");
}

/// 🔴 回归保护：`runtime_flags` 为空时，onboarding 的四个端点行为与接线前**逐字不变**
/// （关 → 全 404；开 → 全通）。
#[tokio::test]
async fn wired_onboarding_unchanged_when_table_empty() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state.db, "usr_e").await;
    let u = token(&state, "usr_e", "user");
    let paths = ["/api/onboarding/presets", "/api/me/onboarding"];

    {
        let _g = env_guard(Some("0"), &[]);
        for p in paths {
            let (st, _) = get(&app, p, Some(&u)).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "{p} 在 env 关时应 404");
        }
        let (st, _) = post(&app, "/api/me/onboarding/gift", Some(&u), json!({})).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = post(&app, "/api/me/onboarding/microworld/start", Some(&u), json!({})).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }
    {
        let _g = env_guard(Some("1"), &[]);
        for p in paths {
            let (st, _) = get(&app, p, Some(&u)).await;
            assert_eq!(st, StatusCode::OK, "{p} 在 env 开时应通");
        }
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_flags").fetch_one(&state.db).await.unwrap();
    assert_eq!(n, 0, "整个过程不该产生任何开关记录");
}

/// 🔴 **全仓只许 `flags` 模块直接查 `runtime_flags` 表**。
///
/// 这个模块存在的全部意义是「开关的唯一读取入口」。而 `entry_ever_open`
/// （「这块功能曾经对任何人开过吗」）此前在 **`annotations` / `ifline` / `livestage` /
/// `social` 四个模块里各写了一遍**，四份都绕过本模块直接查表——等于在唯一入口旁边
/// 开了四条旁路。其中一处的文档还明写着「口径逐字抄 `annotations::entry_ever_open`」，
/// **手抄被当成了实现方式**。
///
/// 🔴 它决定的是两件不会立刻显形的事：
/// - **指标诚不诚实**：三态判定（`entry_not_open` / `no_data_in_window` / `ok`）。
///   漂移了指标照样出数，只是那个数是假的——而 T1/T5 门槛要拿它决定继续/调整/停止。
/// - **审核闭环开不开得了**：按世界灰度开着时弹幕/举报会真实落库，而按全局解析会把
///   运营面判成 404 —— 弹幕进得来撤不下去、举报进得来处置不进去。漂移了运营面照样 404，
///   只是本该可见。
///
/// 两种失败都**不报错、不告警**。故用源码级扫描保证唯一性，而不是靠注释里的「口径一致」。
#[test]
fn red_line_only_flags_queries_the_runtime_flags_table() {
    // 🔴 用共用扫描器：它按**花括号配平**剥离 `#[cfg(test)] mod X { .. }`，
    // 而不是按模块名截断。本仓库的内联测试模块不止叫 `tests`（`cors_tests` /
    // `container_tests` / `sampling_tests` / `member_order_tests`），
    // 按名字截断会把它们的测试代码当成生产代码扫——本条第一版就被 `assembly` 误报过一次。
    let offenders: Vec<String> = crate::testkit::production_sources()
        .into_iter()
        // 豁免：本模块自己，以及 admin 的开关 CRUD 面（它的语义就是直接操作这张表，
        // 且写入侧的校验都落在 `flags::KNOWN_FLAGS` 上）。
        .filter(|(rel, _)| rel != "flags/mod.rs" && rel != "admin_api/flags.rs")
        .filter(|(_, src)| src.contains("FROM runtime_flags") || src.contains("INTO runtime_flags"))
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        offenders.is_empty(),
        "🔴 `runtime_flags` 只能由 `flags` 模块读写（admin 的开关 CRUD 面除外）。\
         这些文件绕过了唯一入口：{offenders:?}。\
         绕过的后果不会立刻显形——指标照常出数（只是假的）、运营面照常 404（只是本该可见）。"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 文档同步闸：`KNOWN_FLAGS` ↔ `docs/VALIDATION.md` 的双坐标功能台账
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 **这两道闸补的是 2026-07-29 实测到的一类漂移：代码前进了，台账没跟。**
//
// 当时 §3 台账里有三行还写着 `Specified`——单人微本 + 新手礼包、生死契约三档、
// 副本卡 + 自定义房装配——而三者的模块、迁移、路由、开关**全都已经在跑**
// （`onboarding/` + 0031、0026 三档 + join 契约门、`subplot/` + 0032/0033）。
// 「内容中台工业线」那行写着 `Concept`，而它七道工序里有六道已是 `Implemented`。
//
// 这个方向的错**比写少了更贵**：一份说「还没做」的台账会让下一个人去**重做一遍**
// （`slo` 那边差点因此造出同一读数的第二份实现，见
// `slo::tests::validation_doc_mentions_every_calibration_dimension` 的注释），
// 或者让评审在「这功能到底能不能开」上按着一份假状态做决定。
//
// 已有的 `KNOWN_FLAGS.len()` 棘轮只管**代码这一侧**（新增开关必须被人评审一次），
// 它对「评审完了但台账没改」完全无感。下面两道闸各封一个方向。
//
// ⚠️ **它们挡不住什么，如实写在这里**（本仓 §3.8.1「红线本身会骗人」）：
// 若某个已落地功能**在台账里整行缺失**、且它的开关名散落在别的小节里，
// 两道闸都会绿。真正能封死那一半的做法是把「功能 → 台账行」做成机器可判的映射，
// 而那份映射今天不存在（`FlagDef.owner` 是模块名，不是产品功能名）。
// 因此这两道闸的定位是**收窄**漂移面，不是消灭它。

/// 每个登记开关的名字，都必须在 `docs/VALIDATION.md` 里至少出现一次。
///
/// 判据刻意只要求「名字出现过」、不校验措辞——措辞会变，而「加了一个开关却
/// 从没在验证计划里露过名字」是确定性的疏漏：那意味着这块功能**没有进过 T0-T5 的任何一格**，
/// 而 §0.1 的全部约束（未验证功能默认关闭、按阶段开闸）都挂在那张表上。
///
/// 🔵 这道闸落地当天就抓到了健康档三维度：§3.40 通篇只说「三条 `FlagDef`」，
/// 一个名字都没写（`MUSE_ATTENTION_TICK_FAILURE` / `_BLOCKED_STREAK` / `_STALLED`）。
/// 形状与 `slo::tests::validation_doc_mentions_every_calibration_dimension` 同源。
#[test]
fn validation_doc_mentions_every_known_flag() {
    let doc = include_str!("../../../docs/VALIDATION.md");
    let missing: Vec<&str> = KNOWN_FLAGS
        .iter()
        .map(|f| f.name)
        .filter(|name| !doc.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "这些开关已在 `KNOWN_FLAGS` 登记，但 `docs/VALIDATION.md` 全文一次都没提过：{missing:?}。\n\
         一个在验证计划里从没露过名字的开关 = 这块功能没进过 T0-T5 的任何一格。\n\
         请在相应小节写清它开的是什么、按哪一阶段开闸，而不是只把这里的断言改绿。"
    );
}

/// §3 双坐标台账里**点名了某个开关**的行，开发状态不得仍是 `Specified` / `Concept`。
///
/// 逻辑很短：开关存在 ⇒ 那块代码已经接线（`KNOWN_FLAGS` 全表 `wired: true`）⇒
/// 开发状态至少是 `Implemented`。写着 `Specified` 只可能是台账没跟上。
///
/// 🔴 解析口径本身也要能失败：列数变了 / 表体空了 / 点名开关的行数塌了，
/// 三种都直接红，而不是静默扫了个空表passing——「可被悄悄缩小的扫描面」是本仓
/// §3.8.1 点名的红线骗人写法之一。
#[test]
fn ledger_rows_that_name_a_flag_are_not_still_specified() {
    let doc = include_str!("../../../docs/VALIDATION.md");
    let rows = ledger_rows(doc);

    // 表体塌了要红：解析口径失效时，下面的循环会一条不扫地「全绿」。
    assert!(
        rows.len() >= 15,
        "§3 台账只解析出 {} 行，解析口径疑似失效（标题被改？表格被换成别的写法？）",
        rows.len()
    );

    let mut named = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    for (dev_status, raw) in &rows {
        let hits: Vec<&str> = KNOWN_FLAGS
            .iter()
            .map(|f| f.name)
            .filter(|n| raw.contains(n))
            .collect();
        if hits.is_empty() {
            continue;
        }
        named += 1;
        if !(dev_status.contains("Implemented") || dev_status.contains("Integrated")) {
            offenders.push(format!("{hits:?} 所在行的开发状态是「{}」", dev_status.trim()));
        }
    }

    assert!(
        offenders.is_empty(),
        "🔴 台账与代码漂了：这些行点名了已登记的运行时开关，开发状态却还不是 \
         Implemented/Integrated：\n  {}\n\
         开关存在 ⇒ 代码已接线（`KNOWN_FLAGS` 全表 wired）⇒ 状态至少是 Implemented。\n\
         2026-07-29 就是这么抓到三行的（新手动线 / 生死契约 / 副本卡+装配）。",
        offenders.join("\n  ")
    );

    // 扫描面下限：把开关名从台账里删掉就能让上面的循环一条都不扫——这条堵死那个出口。
    // 🔵 它是**有意的棘轮**（同 `KNOWN_FLAGS.len()` 那条）：给台账新点名一个开关时它不会红，
    // 只有**变少**才红；数字要往上抬时顺手确认一下少掉的那行是被合并了还是被漏了。
    assert!(
        named >= 6,
        "§3 台账里点名运行时开关的行只剩 {named} 行（应 ≥ 6）。\
         开关名是这道闸唯一的锚点，锚点被摘掉等于闸门失效。"
    );
}

/// 解析 §3 双坐标功能台账的**表体**，返回 `(开发状态列, 整行原文)`。
///
/// 从 `## 3. 双坐标功能台账` 起、到下一个 `### ` 小节止；跳过表头与分隔行。
/// 表格下方那段引用块（`> | 组合 | SQLite | …`）不会被误收——它的行首是 `>`。
fn ledger_rows(doc: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in doc.lines() {
        if line.starts_with("## 3. 双坐标功能台账") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("### ") {
            break;
        }
        let t = line.trim();
        if !t.starts_with('|') || t.contains("---") || t.starts_with("| 功能 |") {
            continue;
        }
        let cells: Vec<&str> = t.split('|').collect();
        // 4 列 ⇒ split 出 6 段（首尾各一个空串）。列数变了必须红：静默跳过 = 扫描面被悄悄缩小。
        assert_eq!(
            cells.len(),
            6,
            "台账行的列数不是 4（解析口径失效，或某个单元格里混进了裸 `|`）：{t}"
        );
        out.push((cells[2].to_string(), t.to_string()));
    }
    out
}
