//! OOC 注解权测试（sqlite::memory + oneshot 真实路由）。总规格 §7 人设保险第 2 级。覆盖：
//!
//! - **默认关闭**：六个端点全 404，且**一行都不落库**（前门 + 状态侧双保险）；
//! - **申诉幂等**：同一拍同一角色只受理一次（换幂等键再点也只读回既有那条）；
//! - 🔴 **申诉与复核不改写任何世界线数据**（红线，十张表**逐字节快照比对**，非源码级近似）；
//! - 🔴 **批注只对本人可见**（信息边界：他人读 404、他人列表不含、响应不带 ownerId）；
//! - **复核补偿生效**：补偿入账后，用 `interventions` **原样抄来**的配额计数 SQL 验证
//!   「本会被拒的第 4 条托梦，在补偿后落进了额度内」——不改它的口径也能生效；
//! - **复核落审计**：`audit_logs` 有行、含决定与补偿数；且与状态更新同事务（CAS 幂等）；
//! - **SLO 端到端**：走真实端点提申诉后 `narrative_slo` 的 oocAppealRate 算得出真数；
//! - 🔴 **源码级红线**：不写世界线表 · 批注不进引擎 · 不碰 interventions 表。

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
    idem_key: Option<&str>,
) -> (StatusCode, Value) {
    let app = crate::app::build_router(state.clone());
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tk}"));
    if let Some(k) = idem_key {
        builder = builder.header("Idempotency-Key", k);
    }
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

fn admin_token(state: &AppState, role: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, &format!("adm_{role}"), role, 3600).unwrap()
}

/// 🔴 打开开关走 **`runtime_flags` DB 记录**而不是 env。
///
/// env 是**进程级**的，本模块用例与其它模块同属一个测试二进制、默认并发跑，改 env 必须共用
/// 全局锁才不串味（见 `memorial::MemorialSwitch` 的注释）。而运行时开关体系恰好提供了
/// 「不碰 env 也能开」的路径：写一条 DB 记录只影响本用例自己的内存库，天然无需加锁。
/// 这也顺带演示了这套开关体系存在的意义（按世界/按用户灰度）。
async fn open_flag(state: &AppState, scope: &str, target: &str) {
    sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) \
         VALUES ($1, $2, $3, $4, 1, 0, 0, 'test', $5, '用例开闸', $6)",
    )
    .bind(new_id("rf"))
    .bind(ENV_OOC_ANNOTATIONS)
    .bind(scope)
    .bind(target)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    crate::flags::invalidate(&state.db);
}

async fn seed_char(state: &AppState, id: &str, owner: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES ($1, $2, 'loc', 1, '{}', 'original', 'approved', 0, $3)",
    )
    .bind(id)
    .bind(owner)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_tick(state: &AppState, world: &str, tick_no: i64, status: &str) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
         VALUES ($1, $2, $3, 0, $4, 120, $5)",
    )
    .bind(format!("tk_{world}_{tick_no}"))
    .bind(world)
    .bind(tick_no)
    .bind(status)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_event(state: &AppState, world: &str, tick_no: i64, seq: i64, actor: &str) {
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, public_projection_json, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, 'action', $6, 'public', '{\"text\":\"他在城门口退了一步\"}', $7)",
    )
    .bind(format!("ev_{world}_{seq}"))
    .bind(world)
    .bind(tick_no)
    .bind(seq)
    .bind(format!("dev_{world}_{seq}"))
    .bind(json!([actor]).to_string())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_whisper(state: &AppState, id: &str, world: &str, user: &str, character: &str, status: &str) {
    sqlx::query(
        "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, \
         expected_revision, status, created_at) \
         VALUES ($1, $2, $3, $4, 'whisper', '{\"text\":\"小心\"}', 0, $5, $6)",
    )
    .bind(id)
    .bind(world)
    .bind(user)
    .bind(character)
    .bind(status)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 一个「世界已经演过几拍」的完整布景：世界 + 卡 + 成员 + 两拍 + 两条事件 + 一条托梦。
async fn seed_world_with_history(state: &AppState) {
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_world(&state.db, "w1", 7, "running").await;
    seed_char(state, "c1", "u1").await;
    seed_char(state, "c2", "u2").await;
    seed_member(&state.db, "wm1", "w1", "u1", "c1", "active").await;
    seed_member(&state.db, "wm2", "w1", "u2", "c2", "active").await;
    seed_tick(state, "w1", 1, "done").await;
    seed_tick(state, "w1", 2, "done").await;
    seed_event(state, "w1", 1, 1, "c1").await;
    seed_event(state, "w1", 2, 2, "c2").await;
    seed_whisper(state, "iv1", "w1", "u1", "c1", "applied").await;
}

/// 🔴 **世界线快照**：把一张表的全部行拉出来、逐列格式化、排序后拼成一个字符串。
///
/// 用「SELECT * + 逐列取值」而不是挑几列比对：挑列意味着**漏掉的那一列就是没被守住的那一列**，
/// 而红线要守的是「一个字节都没动」。排序使比对与行序无关（行序本身不是事实的一部分）。
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

/// 全部**世界线 / 资产**表（申诉绝不能碰的那一批）。
const WORLDLINE_TABLES: &[&str] = &[
    "worlds",
    "world_events",
    "world_ticks",
    "world_members",
    "world_contributions",
    "consent_requests",
    "interventions",
    "backpacks",
    "cloud_characters",
    "world_biographies",
];

async fn dump_worldline(db: &AnyPool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for t in WORLDLINE_TABLES {
        out.push(((*t).to_string(), dump_table(db, t).await));
    }
    out
}

async fn count_rows(db: &AnyPool, table: &str) -> i64 {
    sqlx::query(&format!("SELECT CAST(COUNT(*) AS BIGINT) AS n FROM {table}"))
        .fetch_one(db)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
}

/// 🔴 **逐字节抄自 `interventions::create_intervention` 的配额计数 SQL**。
///
/// 抄而不是复用（那边是私有实现），也刻意不简化：本用例要证明的正是
/// 「**这条 SQL 一个字符不改**，补偿也能生效」。它一旦与生产不一致，本用例就失去意义——
/// 若将来 interventions 改了口径，这里应当同步抄成新的那份，而不是让它悄悄漂移。
async fn whisper_used(db: &AnyPool, world_id: &str, character_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM interventions \
         WHERE world_id = $1 AND character_id = $2 AND kind = 'whisper' \
           AND status IN ('accepted', 'applied')",
    )
    .bind(world_id)
    .bind(character_id)
    .fetch_one(db)
    .await
    .unwrap()
}

// ═══════════════════════════════════════════════════════════════════════════
// ① 未验证功能默认关闭（VALIDATION §0.1）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 开关默认关闭时**六个端点全部 404**，且**一行都不落库**。
///
/// 404 而不是 403：不向外泄露「平台有这个未开放功能」（口径同 onboarding / memorial）。
/// 「前门拒绝 + 状态侧不落库」是双保险：只挡前门的话，任何一条漏掉 `ensure_enabled` 的
/// 新端点都会成为绕过开关的侧路。
#[tokio::test]
async fn red_line_disabled_by_default_all_endpoints_404_and_nothing_persists() {
    let state = test_state().await;
    seed_world_with_history(&state).await;
    let tk = token(&state, "u1");
    let adm = admin_token(&state, "reviewer");

    // 前提：没写任何 runtime_flags 记录（env 由 flags 兜底，默认 false）。
    assert_eq!(count_rows(&state.db, "runtime_flags").await, 0, "前提：开关表为空 = 默认关闭");

    let body = json!({"tickNo": 1, "characterId": "c1", "reasonText": "他不会这么做", "annotation": "他在等人"});
    let cases: Vec<(&str, &str, Option<Value>, &str)> = vec![
        ("POST", "/api/worlds/w1/ooc-appeals", Some(body.clone()), &tk),
        ("GET", "/api/me/ooc-appeals", None, &tk),
        ("PUT", "/api/me/ooc-appeals/ooc_x/annotation", Some(json!({"body": "x"})), &tk),
        ("GET", "/api/me/characters/c1/annotations", None, &tk),
        ("GET", "/api/admin/ooc-appeals", None, &adm),
        (
            "POST",
            "/api/admin/ooc-appeals/ooc_x/review",
            Some(json!({"decision": "confirm_model_error", "reason": "确认"})),
            &adm,
        ),
    ];
    for (method, uri, b, t) in cases {
        let (st, _) = send(&state, method, uri, t, b, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{method} {uri} 在开关关闭时必须 404");
    }

    assert_eq!(count_rows(&state.db, "ooc_appeals").await, 0, "🔴 关闭时不得受理任何申诉");
    assert_eq!(count_rows(&state.db, "character_annotations").await, 0, "🔴 关闭时不得写任何批注");
    assert_eq!(count_rows(&state.db, "dream_quota_compensations").await, 0, "🔴 关闭时不得发任何补偿");
}

/// 登记表里本开关的默认值必须是 false（与模块常量编译期已钉死，这里再从 API 侧确认一次）。
#[test]
fn red_line_flag_defaults_off() {
    let def = crate::flags::find_flag(ENV_OOC_ANNOTATIONS).expect("开关必须登记在 KNOWN_FLAGS");
    assert!(!def.default_enabled, "🔴 VALIDATION §0.1：未验证功能默认必须关闭");
    assert!(def.wired, "本模块从建成之日起就经 flags 体系解析");
    assert!(!DEFAULT_OOC_ANNOTATIONS_ENABLED);
}

// ═══════════════════════════════════════════════════════════════════════════
// ② 申诉：受理与幂等
// ═══════════════════════════════════════════════════════════════════════════

async fn open_and_seed() -> AppState {
    let state = test_state().await;
    seed_world_with_history(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    state
}

#[tokio::test]
async fn create_appeal_accepts_and_attaches_private_annotation() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let (st, v) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({
            "tickNo": 1,
            "characterId": "c1",
            "reasonCode": "ooc",
            "reasonText": "他不会在城门口退这一步",
            "annotation": "他不是怕，他在等那个人先走。"
        })),
        Some("k1"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["created"], true);
    assert_eq!(v["status"], "pending");
    assert_eq!(v["worldFactChanged"], false, "🔴 响应必须明说世界事实没动");
    assert_eq!(v["annotation"]["layer"], "annotation");
    assert_eq!(v["annotation"]["isWorldFact"], false);
    assert_eq!(v["annotation"]["body"], "他不是怕，他在等那个人先走。");
    assert!(v["annotation"].get("ownerId").is_none(), "🔴 §14：响应不得带真人身份");
}

/// 🔴 **同一拍同一角色只受理一次**——换幂等键再点也只读回既有那条。
///
/// 两层幂等各挡一种情况：`Idempotency-Key` 挡同一次点击的 HTTP 重试，DB 唯一键挡
/// 「换个 key 再点」。只有前者会被换 key 击穿，只有后者挡不住同一次点击的并发重试。
#[tokio::test]
async fn appeal_is_idempotent_per_tick_and_character() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let body = json!({"tickNo": 1, "characterId": "c1", "reasonText": "演得不像"});

    let (_, first) = send(&state, "POST", "/api/worlds/w1/ooc-appeals", &tk, Some(body.clone()), Some("k1")).await;
    assert_eq!(first["created"], true);

    // 同一次点击的重试（同 key）。
    let (st, again) = send(&state, "POST", "/api/worlds/w1/ooc-appeals", &tk, Some(body.clone()), Some("k1")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(again["id"], first["id"], "同幂等键必须返回同一条");

    // 换 key 再点（击穿幂等键的那种），并且换个理由文本 —— 仍然只有一条。
    let (st, third) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "换个说法再提一次"})),
        Some("k2"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(third["id"], first["id"], "🔴 同一拍同一角色只受理一次");
    assert_eq!(third["created"], false, "重复提交必须明说没有新建");
    assert_eq!(third["reasonText"], "演得不像", "既有那条不被后来的文本覆盖");
    assert_eq!(count_rows(&state.db, "ooc_appeals").await, 1);

    // 换一拍 = 另一件事，可以受理。
    let (_, other) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 2, "characterId": "c1", "reasonText": "这一拍也不对"})),
        Some("k3"),
    )
    .await;
    assert_eq!(other["created"], true);
    assert_eq!(count_rows(&state.db, "ooc_appeals").await, 2);
}

#[tokio::test]
async fn appeal_rejects_unknown_tick_and_foreign_character() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");

    // 不存在的拍。
    let (st, _) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 99, "characterId": "c1", "reasonText": "无中生有"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "不许对不存在/未落定的拍申诉（否则申诉率分子会被噪声撑大）");

    // 别人的角色 → 风控拦截（口径同 interventions）。
    let (st, _) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c2", "reasonText": "替别人喊冤"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "冒用他人角色 = RiskBlocked");
    assert_eq!(count_rows(&state.db, "risk_events").await, 1, "必须留风控痕迹");
    assert_eq!(count_rows(&state.db, "ooc_appeals").await, 0);

    // 类别白名单外 → 400（绝不静默归类，否则两类的分布统计会被污染）。
    let (st, _) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonCode": "whatever", "reasonText": "x"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

// ═══════════════════════════════════════════════════════════════════════════
// ③ 🔴 红线：世界事实不改
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **申诉 + 加批注 + 复核确认，走完全流程后，十张世界线/资产表逐字节不变。**
///
/// 这是本批次最重要的一条用例。§0.3「公共事实不可回滚」在这一项上最容易被违反：
/// 「申诉成立」听起来天然像「把那一拍撤销」，而规格要的恰恰相反——
/// **承认错误，但事实照旧**。快照比对是唯一能把这件事钉死的方法（源码级 grep 只能挡住
/// 明写的表名，挡不住经由别的函数间接改写）。
#[tokio::test]
async fn red_line_appeal_and_review_leave_worldline_byte_identical() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let adm = admin_token(&state, "reviewer");

    let before = dump_worldline(&state.db).await;
    // 前提：快照确实不是空的（否则「比对通过」毫无意义）。
    for t in ["worlds", "world_events", "world_ticks", "world_members", "interventions"] {
        let d = before.iter().find(|(n, _)| n == t).unwrap();
        assert!(!d.1.is_empty(), "前提：{t} 必须有数据，否则本用例形同虚设");
    }

    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({
            "tickNo": 1, "characterId": "c1", "reasonCode": "unfair_ruling",
            "reasonText": "这个裁决与前一拍的事实矛盾", "annotation": "他其实早就看穿了"
        })),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap().to_string();

    // 改批注（第二条写路径）。
    let (st, _) = send(
        &state,
        "PUT",
        &format!("/api/me/ooc-appeals/{id}/annotation"),
        &tk,
        Some(json!({"body": "改一版：他不是看穿，是认了。"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 复核确认模型错误（第三条写路径，且是三条里唯一会发东西的那条）。
    let (st, review) = send(
        &state,
        "POST",
        &format!("/api/admin/ooc-appeals/{id}/review"),
        &adm,
        Some(json!({"decision": "confirm_model_error", "reason": "仲裁确实与前一拍矛盾"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{review}");
    assert_eq!(review["status"], "confirmed");
    assert_eq!(review["worldlineChanged"], false, "🔴 复核回执必须明说世界线没动");

    let after = dump_worldline(&state.db).await;
    for ((t1, d1), (t2, d2)) in before.iter().zip(after.iter()) {
        assert_eq!(t1, t2);
        assert_eq!(
            d1, d2,
            "🔴 §0.3 公共事实不可回滚：OOC 申诉/批注/复核后 `{t1}` 发生了改变。\n\
             「承认这一拍演砸了」与「这一拍没发生过」是两件事，规格选的是前者。"
        );
    }

    // 三张新表确实写进去了（证明上面的「没变」不是因为整条流程什么都没做）。
    assert_eq!(count_rows(&state.db, "ooc_appeals").await, 1);
    assert_eq!(count_rows(&state.db, "character_annotations").await, 1);
    assert_eq!(count_rows(&state.db, "dream_quota_compensations").await, 1);
}

/// 源码级红线：本模块不出现任何写世界线表的 SQL。
///
/// 与上面的运行时快照互补：快照证明「这次没改」，源码断言证明「以后想改也得先删掉这条用例」。
#[test]
fn red_line_never_rewrites_worldline() {
    let src = include_str!("mod.rs");
    for verb in ["INSERT INTO", "UPDATE", "DELETE FROM"] {
        for table in [
            "worlds",
            "world_events",
            "world_ticks",
            "world_members",
            "world_contributions",
            "consent_requests",
            "interventions",
            "backpacks",
            "cloud_characters",
            "world_biographies",
            "ledger_entries",
        ] {
            let banned = format!("{verb} {table}");
            assert!(
                !src.contains(&banned),
                "🔴 OOC 注解权模块出现 `{banned}`：申诉只加解释层，绝不改写世界线与资产（§0.3 / §0.2）"
            );
        }
    }
    // 🔴 更进一步：本模块**连读都不读** `interventions`。
    // 补偿是「只加加数、不动被加数」——阈值合成发生在 interventions 侧（见 `dream_quota_bonus`
    // 的接线待办），本模块若自己去查那张表，就等于把配额口径复制出了第二份，
    // 两份迟早漂移，而漂移的那天没人会发现。
    //
    // 断言前先剥掉注释：本模块的文档里**必须**出现 `dream_quota_per_stage()` 与那条 COUNT SQL
    // （接线说明要写清楚给 interventions 的负责人看），剥注释才能只针对真代码判定。
    let code: String = src
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("//") || t.starts_with("///") || t.starts_with("//!"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    for banned in ["FROM interventions", "dream_quota_per_stage", "interventions::"] {
        assert!(
            !code.contains(banned),
            "🔴 本模块的可执行代码出现 `{banned}`：托梦补偿只提供加数，配额口径的唯一事实源在 interventions 侧"
        );
    }
}

/// 🔴 批注**不进引擎决策**：`runtime` 与 `crates/muse-engine` 对三张新表零引用。
///
/// 口径与 0025 贡献账本 / 0030 critic / 0034 故人印记逐字一致：
/// `worlds.narrative_state_json` 每 tick 原样回灌进 `RoundInput.state`，任何写进那里的东西
/// 都成了决策输入。独立表 + 引擎侧零读取路径，是比「记得过滤」可靠得多的物理隔离。
#[test]
fn red_line_annotations_never_enter_engine() {
    let runtime_src = include_str!("../runtime/mod.rs");
    for table in ["ooc_appeals", "character_annotations", "dream_quota_compensations"] {
        assert!(
            !runtime_src.contains(table),
            "🔴 runtime 出现 `{table}`：批注是玩家的私人解释层，永远不得进入引擎决策（§0.1 平权红线）"
        );
    }
    assert!(
        !runtime_src.contains("annotations::"),
        "🔴 runtime 不得引用 annotations 模块"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ④ 🔴 信息边界：批注只对本人可见（§14 社交防火墙）
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn red_line_annotation_is_visible_only_to_its_owner() {
    let state = open_and_seed().await;
    let mine = token(&state, "u1");
    let other = token(&state, "u2");

    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &mine,
        Some(json!({
            "tickNo": 1, "characterId": "c1", "reasonText": "不像他",
            "annotation": "他在等那个人先走。"
        })),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap().to_string();

    // 本人读得到。
    let (st, mine_list) = send(&state, "GET", "/api/me/characters/c1/annotations", &mine, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(mine_list["items"].as_array().unwrap().len(), 1);
    assert_eq!(mine_list["items"][0]["body"], "他在等那个人先走。");
    assert_eq!(mine_list["isWorldFact"], false);
    assert!(mine_list["items"][0].get("ownerId").is_none(), "🔴 响应不得带 ownerId");

    // 🔴 他人读**别人的卡**：404（连「这张卡存在」都不确认）。
    let (st, _) = send(&state, "GET", "/api/me/characters/c1/annotations", &other, None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "🔴 别人的角色传记批注不可见");

    // 🔴 他人的申诉列表里没有我的申诉。
    let (st, other_list) = send(&state, "GET", "/api/me/ooc-appeals", &other, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(other_list["items"].as_array().unwrap().is_empty(), "🔴 申诉列表按 user_id 硬隔离");

    // 🔴 他人改我的批注：404（不是 403 —— 403 等于承认这条申诉存在）。
    let (st, _) = send(
        &state,
        "PUT",
        &format!("/api/me/ooc-appeals/{id}/annotation"),
        &other,
        Some(json!({"body": "我来替你写"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let body: String = sqlx::query_scalar("SELECT body FROM character_annotations WHERE appeal_id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(body, "他在等那个人先走。", "🔴 越权请求不得改动一个字");

    // 批注是私人解释层：整张表里每一行都有主人（结构性保证之一）。
    let ownerless: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM character_annotations WHERE owner_id IS NULL OR owner_id = ''",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(ownerless, 0, "🔴 无主的批注行 = 一条冒充世界事实的数据");
}

/// 批注可在申诉之后补写、可改、可清空（「想说什么」往往比「气不过」晚到）。
#[tokio::test]
async fn annotation_can_be_added_updated_and_cleared_later() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "不像他"})),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap().to_string();
    assert!(appeal["annotation"].is_null(), "没写批注时就是 null");

    let uri = format!("/api/me/ooc-appeals/{id}/annotation");
    let (_, v) = send(&state, "PUT", &uri, &tk, Some(json!({"body": "第一版"})), None).await;
    assert_eq!(v["annotation"]["body"], "第一版");
    let (_, v) = send(&state, "PUT", &uri, &tk, Some(json!({"body": "第二版"})), None).await;
    assert_eq!(v["annotation"]["body"], "第二版");
    assert_eq!(count_rows(&state.db, "character_annotations").await, 1, "改写走 UPDATE，不产生第二行");

    let (_, v) = send(&state, "PUT", &uri, &tk, Some(json!({"body": "  "})), None).await;
    assert!(v["annotation"].is_null(), "清空 = 删掉自己写的话，这是玩家的权利");
    assert_eq!(count_rows(&state.db, "character_annotations").await, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑤ 复核：补偿生效 · 审计 · 幂等 · 鉴权
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **补偿真的生效**——用 `interventions` 原样抄来的计数 SQL 验证。
///
/// 布景：默认配额 3 条，玩家已用满 3 条（`accepted`/`applied` 各计数）。
/// 此时第 4 条托梦会被 `used >= dream_quota_per_stage()` 判为 `rejected("quota")`。
/// 复核确认模型错误后补 1 条：**计数 SQL 一个字符没改**（`used` 仍是 3），
/// 但阈值变成 `3 + bonus = 4`，于是第 4 条落进额度内。
#[tokio::test]
async fn review_confirmation_grants_effective_dream_quota_compensation() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let adm = admin_token(&state, "reviewer");

    // 用满默认配额（3 条：2 条 applied + 1 条 accepted；rejected 的不计数）。
    seed_whisper(&state, "iv2", "w1", "u1", "c1", "applied").await;
    seed_whisper(&state, "iv3", "w1", "u1", "c1", "accepted").await;
    seed_whisper(&state, "iv4", "w1", "u1", "c1", "rejected").await;
    let base_quota = 3i64; // interventions::DEFAULT_DREAM_QUOTA_PER_STAGE
    let used = whisper_used(&state.db, "w1", "c1").await;
    assert_eq!(used, 3, "前提：三条计入配额（rejected 不算）");
    assert!(used >= base_quota, "前提：此刻第 4 条托梦会被判 rejected(quota)");
    assert_eq!(dream_quota_bonus(&state.db, "w1", "c1").await, 0, "复核之前没有任何补偿");

    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "托梦被无视且行为反人设"})),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap().to_string();

    let (st, v) = send(
        &state,
        "POST",
        &format!("/api/admin/ooc-appeals/{id}/review"),
        &adm,
        Some(json!({"decision": "confirm_model_error", "reason": "模型忽略了卡的底线字段"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "confirmed");
    assert_eq!(v["compensation"]["dreamWhispers"], 1);

    // 🔴 关键断言：计数口径没变，阈值变了。
    let used_after = whisper_used(&state.db, "w1", "c1").await;
    assert_eq!(used_after, used, "🔴 补偿绝不改动 interventions 的任何一行（计数必须原样）");
    let bonus = dream_quota_bonus(&state.db, "w1", "c1").await;
    assert_eq!(bonus, 1, "补偿入账");
    assert!(
        used_after < base_quota + bonus,
        "🔴 补偿生效：接线后 `used >= dream_quota_per_stage() + bonus` 不再成立，第 4 条托梦得以受理"
    );

    // 补偿是「说话的机会」，不是资产：不进背包、不进历练、不进账本。
    assert_eq!(count_rows(&state.db, "backpacks").await, 0);
}

/// 不予支持 → 不发补偿。
#[tokio::test]
async fn dismissed_review_grants_nothing() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let adm = admin_token(&state, "reviewer");
    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "我就是不服"})),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap();

    let (st, v) = send(
        &state,
        "POST",
        &format!("/api/admin/ooc-appeals/{id}/review"),
        &adm,
        Some(json!({"decision": "dismiss", "reason": "该行为符合卡的既有设定与前情"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["status"], "dismissed");
    assert!(v["compensation"].is_null());
    assert_eq!(count_rows(&state.db, "dream_quota_compensations").await, 0);
    assert_eq!(dream_quota_bonus(&state.db, "w1", "c1").await, 0);
}

/// 🔴 **复核必须落审计**（运营改判 = §0.2 全链审计），且与状态更新在同一事务里。
#[tokio::test]
async fn red_line_review_writes_audit_log() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let adm = admin_token(&state, "reviewer");
    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 2, "characterId": "c1", "reasonText": "不像他"})),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap().to_string();

    let before = count_rows(&state.db, "audit_logs").await;
    send(
        &state,
        "POST",
        &format!("/api/admin/ooc-appeals/{id}/review"),
        &adm,
        Some(json!({"decision": "confirm_model_error", "reason": "确认为模型问题"})),
        None,
    )
    .await;
    assert_eq!(count_rows(&state.db, "audit_logs").await, before + 1);

    let row = sqlx::query(
        "SELECT actor_id, actor_role, action, subject, reason FROM audit_logs \
         WHERE action = 'ooc_appeal.confirmed'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(row.try_get::<String, _>("actor_role").unwrap(), "reviewer");
    assert_eq!(row.try_get::<String, _>("actor_id").unwrap(), "adm_reviewer");
    assert_eq!(row.try_get::<String, _>("subject").unwrap(), format!("ooc_appeal:{id}"));
    let reason: String = row.try_get("reason").unwrap();
    for expect in ["confirmed", "world=w1", "tick=2", "character=c1", "compensation=1", "确认为模型问题"] {
        assert!(reason.contains(expect), "审计理由必须写清改判全貌，缺 `{expect}`：{reason}");
    }
}

/// 复核幂等：状态 CAS + 补偿唯一索引双闸，重复复核 409 且绝不补第二次。
#[tokio::test]
async fn review_is_idempotent_and_never_double_compensates() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let adm = admin_token(&state, "reviewer");
    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "不像他"})),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap().to_string();
    let uri = format!("/api/admin/ooc-appeals/{id}/review");
    let body = json!({"decision": "confirm_model_error", "reason": "确认为模型问题"});

    let (st, _) = send(&state, "POST", &uri, &adm, Some(body.clone()), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&state, "POST", &uri, &adm, Some(body.clone()), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "已复核不可重复裁决");
    // 换个决定再来一次也不行（改判只有一次机会）。
    let (st, _) = send(
        &state,
        "POST",
        &uri,
        &adm,
        Some(json!({"decision": "dismiss", "reason": "反悔了"})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    assert_eq!(count_rows(&state.db, "dream_quota_compensations").await, 1, "🔴 绝不补第二次");
    assert_eq!(dream_quota_bonus(&state.db, "w1", "c1").await, 1);
    assert_eq!(
        count_rows(&state.db, "audit_logs").await,
        1,
        "被 CAS 挡下的复核不落审计（没发生的事不该有流水）"
    );
}

/// 复核走 reviewer 档（与内容风控申诉同档：都是改判类动作）；其它后台角色一律 403。
#[tokio::test]
async fn review_requires_reviewer_grade() {
    let state = open_and_seed().await;
    let tk = token(&state, "u1");
    let (_, appeal) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &tk,
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "不像他"})),
        Some("k1"),
    )
    .await;
    let id = appeal["id"].as_str().unwrap().to_string();
    let uri = format!("/api/admin/ooc-appeals/{id}/review");
    let body = json!({"decision": "confirm_model_error", "reason": "确认"});

    for role in ["operator", "support", "finance"] {
        let (st, _) = send(&state, "POST", &uri, &admin_token(&state, role), Some(body.clone()), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{role} 不承担改判职责");
        let (st, _) = send(&state, "GET", "/api/admin/ooc-appeals", &admin_token(&state, role), None, None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{role} 不该看到申诉内容");
    }
    // 普通玩家连后台守卫都过不去。
    let (st, _) = send(&state, "POST", &uri, &tk, Some(body.clone()), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // admin 是超级用户，reviewer 是本职。
    let (st, _) = send(&state, "GET", "/api/admin/ooc-appeals", &admin_token(&state, "admin"), None, None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&state, "POST", &uri, &admin_token(&state, "reviewer"), Some(body), None).await;
    assert_eq!(st, StatusCode::OK);
}

/// 按世界灰度时复核队列仍可用（全局关 + 某世界开）——否则「申诉进得来、复核进不去」。
#[tokio::test]
async fn ops_queue_available_under_world_scoped_rollout() {
    let state = test_state().await;
    seed_world_with_history(&state).await;
    open_flag(&state, crate::flags::SCOPE_WORLD, "w1").await;

    // 全局解析仍是关闭。
    assert!(!ooc_annotations_enabled(&state.db, None, None).await);
    // 但该世界的申诉入口是开的。
    assert!(ooc_annotations_enabled(&state.db, Some("u1"), Some("w1")).await);

    let (st, _) = send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &token(&state, "u1"),
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "不像他"})),
        Some("k1"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, q) = send(&state, "GET", "/api/admin/ooc-appeals", &admin_token(&state, "reviewer"), None, None).await;
    assert_eq!(st, StatusCode::OK, "🔴 队列必须跟随「有人能提申诉」而不是全局开关");
    assert_eq!(q["items"].as_array().unwrap().len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑥ SLO 端到端：走真实端点之后，oocAppealRate 算得出真数
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 本模块存在的**首要理由**：让 VALIDATION §4.2 里那项「唯一未解」的 SLO 变成可算，
/// 从而让 T1 门槛「OOC/裁决不公申诉 <10%/阶段」第一次具备判定手段。
#[tokio::test]
async fn slo_ooc_appeal_rate_becomes_computable_after_real_appeal() {
    let state = open_and_seed().await;
    let now = now_ms();
    let cfg = crate::slo::SloConfig::from_env(30, now - 86_400_000, now + 86_400_000);

    // 开关已开、世界演过戏、但还没人申诉 → 真的 0%（不是「没测过」）。
    let slo = crate::slo::narrative_slo(&state.db, &cfg).await.unwrap();
    let x = &slo["metrics"]["oocAppealRate"];
    assert_eq!(x["status"], "ok");
    assert_eq!(x["value"], 0.0);
    assert_eq!(x["memberStagesCounted"], 2, "w1 有两名成员");
    assert!(
        !slo["unavailable"].as_array().unwrap().iter().any(|v| v == "oocAppealRate"),
        "🔴 转正后不得再出现在无数据源清单里：{slo}"
    );

    // 提一条真申诉 → 分子变 1，申诉率 50%，超过 T1 门槛 10%。
    send(
        &state,
        "POST",
        "/api/worlds/w1/ooc-appeals",
        &token(&state, "u1"),
        Some(json!({"tickNo": 1, "characterId": "c1", "reasonText": "不像他"})),
        Some("k1"),
    )
    .await;
    let slo = crate::slo::narrative_slo(&state.db, &cfg).await.unwrap();
    let x = &slo["metrics"]["oocAppealRate"];
    assert_eq!(x["charactersAppealed"], 1);
    assert_eq!(x["appealsTotal"], 1);
    assert_eq!(x["value"], 0.5);
    assert!(x["overThreshold"].as_bool().unwrap());
    assert_eq!(x["byReasonCode"]["ooc"], 1);
    assert_eq!(x["byStatus"]["pending"], 1);
    assert!(x["confirmedRate"].is_null(), "一条都没复核 → 坐实率 null，不是 0");
}
