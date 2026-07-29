//! if 线付费副本测试（sqlite::memory + oneshot 真实路由）。总规格 §7 人设保险**第 3 级**。覆盖：
//!
//! - **默认关闭**：五个端点全 404，且**一行都不落库、一张卡都不烧**（前门 + 状态侧双保险）；
//! - 🔴 **开 if 线后原世界逐字节不变**（红线，十一张世界线/资产表**逐字节快照比对**，非源码级近似）；
//! - 🔴 **if 线不是一行 `worlds`**：`worlds` / `world_members` 行数与内容零变化，
//!   if 线 id 在 `worlds` 里查无此行——它进不了世界列表、发现页、结算管线、SLO 与贡献账本；
//! - 🔴 **产出不反哺**：开 if 线后历练 / 背包 / 贡献账本 / 副本卡**净发放量**全部零变化
//!   （唯一的资产变化是「烧掉了一张」，方向是净收缩）；
//! - 🔴 **分叉点不假装**：请求中间拍 400、请求 `forkPoint=tick` 400，且两种拒绝都**不动任何资源**；
//! - 🔴 **§14 单人平行线**：他人玩家角色 + 其关系边 + `knownTo` 引用全部剥离，台账可查；NPC 保留；
//! - 🔴 **传世卡不得复活**：已封卷的卡开 if 线 400（if 线不是付费复活）；
//! - **读取面可区分**：每条响应恒带 `layer=ifline` / `isWorldFact=false` / `affectsOriginWorld=false`
//!   / `forkPoint.stateFidelity`，且**不含 `ownerId`**；
//! - **幂等**：换幂等键再点也只读回既有那条，副本卡只烧一次；
//! - **信息边界**：别人的 if 线一律 404（不是 403）；
//! - 🔴 **源码级红线**：不写世界线表 · 不铸任何资产 · 引擎零引用 · 卡状态字面量与 `subplot` 一致。

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
/// 全局锁才不串味。运行时开关体系恰好提供了「不碰 env 也能开」的路径：写一条 DB 记录只影响
/// 本用例自己的内存库，天然无需加锁（口径逐字抄 `annotations::tests::open_flag`）。
async fn open_flag(state: &AppState, scope: &str, target: &str) {
    sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) \
         VALUES ($1, $2, $3, $4, 1, 0, 0, 'test', $5, '用例开闸', $6)",
    )
    .bind(new_id("rf"))
    .bind(ENV_IFLINE_PARALLEL)
    .bind(scope)
    .bind(target)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    crate::flags::invalidate(&state.db);
}

/// 播一张云端角色卡。
///
/// 🔴 `card_json` 用**引擎真的解析得动**的固定卡（复用黄金世界 fixture），不是 `{}`：
/// 0039 只做立项与开局，卡的内容从头到尾没被读过，`{}` 够用；0041 的推进要把卡喂进
/// `RoundInput.active_cards`，`{}` 会在 `CharacterCardV2` 反序列化处静默失败 →
/// 「主角卡不可读」。用真卡播种，测的才是真实路径。
async fn seed_char(state: &AppState, id: &str, owner: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES ($1, $2, 'loc', 1, $3, 'original', 'approved', 0, $4)",
    )
    .bind(id)
    .bind(owner)
    .bind(card_for(id, "shenyan").to_string())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_tick(state: &AppState, world: &str, tick_no: i64, status: &str) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
         VALUES ($1, $2, $3, $4, $5, 120, $6)",
    )
    .bind(format!("tk_{world}_{tick_no}"))
    .bind(world)
    .bind(tick_no)
    .bind(tick_no - 1)
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

/// 一张在手的副本卡（**用 `subplot` 表的真列**播种，于是「烧卡」走的是真实资产）。
async fn seed_card(state: &AppState, id: &str, owner: &str, world: &str) {
    sqlx::query(
        "INSERT INTO subplot_cards (id, owner_id, star_rating, label, origin_kind, grant_key, \
         source_world_id, source_template_id, source_template_version, synthesized_from_json, \
         status, consumed_into, acquired_at, consumed_at) \
         VALUES ($1, $2, 2, '城门一步', 'settlement', $3, $4, 'tpl', 1, '[]', 'owned', NULL, $5, NULL)",
    )
    .bind(id)
    .bind(owner)
    .bind(format!("settlement:{world}:{id}:worldline"))
    .bind(world)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 原世界的终局叙事态：**两名玩家角色（c1 属 u1、c2 属 u2）+ 一个 NPC + 两条关系边**。
///
/// 这份状态是 §14 剥离用例的靶子：c2 与涉及它的边必须消失，npc_shen 与 c1 必须留下。
fn terminal_state() -> Value {
    json!({
        "schemaVersion": 1,
        "runId": "run_w1",
        "revision": 7,
        "world": { "era": "城破之年" },
        "characters": {
            "c1": { "goals": ["活下去"], "arcStage": "低谷", "location": "城门" },
            "c2": { "goals": ["复仇"], "arcStage": "转折", "location": "城门" },
            "npc_shen": { "goals": ["守城"], "arcStage": "终局", "location": "城楼" }
        },
        "relations": [
            { "from": "c1", "to": "c2", "trust": 0.2, "affinity": 0.1, "fear": 0.4, "debt": 0.0,
              "knownTo": ["c1", "c2", "npc_shen"], "notes": ["城门口那一步"] },
            { "from": "c1", "to": "npc_shen", "trust": 0.6, "affinity": 0.5, "fear": 0.0, "debt": 1.0,
              "knownTo": ["c1", "c2", "npc_shen"], "notes": ["守城之恩"] }
        ],
        "narrative": { "pendingConsents": [] }
    })
}

async fn set_state(state: &AppState, world: &str, value: &Value, revision: i64) {
    sqlx::query("UPDATE worlds SET narrative_state_json = $1, state_revision = $2 WHERE id = $3")
        .bind(value.to_string())
        .bind(revision)
        .bind(world)
        .execute(&state.db)
        .await
        .unwrap();
}

/// 一个「世界已经演完、可以开 if 线」的完整布景：
/// 已结束的世界 w1 + 两名玩家（u1/c1、u2/c2）+ 三拍（第 3 拍 failed，故终局拍是第 2 拍）+
/// 两条事件 + u1 手上一张副本卡 + 终局叙事态。
async fn seed_ended_world(state: &AppState) {
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_world(&state.db, "w1", 7, "ended").await;
    seed_char(state, "c1", "u1").await;
    seed_char(state, "c2", "u2").await;
    seed_member(&state.db, "wm1", "w1", "u1", "c1", "active").await;
    seed_member(&state.db, "wm2", "w1", "u2", "c2", "active").await;
    seed_tick(state, "w1", 1, "done").await;
    seed_tick(state, "w1", 2, "done").await;
    // 🔴 第 3 拍 failed：终局拍必须是**最后一拍已落定的**第 2 拍，不是 MAX(tick_no)=3。
    // 拿一拍没落定的状态当分叉点就是分在半空中。
    seed_tick(state, "w1", 3, "failed").await;
    seed_event(state, "w1", 1, 1, "c1").await;
    seed_event(state, "w1", 2, 2, "c2").await;
    seed_card(state, "sc1", "u1", "w1").await;
    set_state(state, "w1", &terminal_state(), 7).await;
}

/// 合法的开 if 线请求体。
fn open_body() -> Value {
    json!({
        "characterId": "c1",
        "premise": "如果他在城门口没有退那一步。",
        "cardIds": ["sc1"],
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 逐字节快照工具
// ═══════════════════════════════════════════════════════════════════════════

/// 把一张表的全部行拉出来、逐列格式化、排序后拼成一个字符串。
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

/// 全部**世界线 / 资产**表（开 if 线绝不能碰的那一批）。
///
/// 🔴 `subplot_cards` **刻意不在这张表里**：它是本功能唯一被允许改动的表（烧卡），
/// 它的变化由专门的用例逐字段验证（只允许 `owned → consumed` 一种转移，且必须留下反向血缘）。
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
    "arena_rewards",
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

async fn card_status(db: &AnyPool, card_id: &str) -> (String, Option<String>) {
    let r = sqlx::query("SELECT status, consumed_into FROM subplot_cards WHERE id = $1")
        .bind(card_id)
        .fetch_one(db)
        .await
        .unwrap();
    (r.try_get("status").unwrap(), r.try_get("consumed_into").unwrap())
}

// ═══════════════════════════════════════════════════════════════════════════
// ① 未验证功能默认关闭（§0.1）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **开关默认关闭 → 五个端点全 404，且一行都不落库、一张卡都不烧**。
///
/// 404 而不是 403：不向外泄露「平台有这个未开放功能」。**读端点同样 404**（读取侧降级一致）。
#[tokio::test]
async fn disabled_by_default_all_endpoints_404_and_nothing_happens() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    let tk = token(&state, "u1");

    let cases: Vec<(&str, &str, Option<Value>)> = vec![
        ("GET", "/api/worlds/w1/ifline-fork-points", None),
        ("POST", "/api/worlds/w1/iflines", Some(open_body())),
        ("GET", "/api/me/iflines", None),
        ("GET", "/api/me/iflines/ifw_x", None),
    ];
    for (m, uri, body) in cases {
        let (st, _) = send(&state, m, uri, &tk, body, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{m} {uri} 在开关关闭时必须 404（不是 403）");
    }
    // 运营面同样不可见（急停语义：关阀 → 一并不可见）。
    let (st, _) = send(&state, "GET", "/api/admin/iflines", &admin_token(&state, "admin"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "开关从未开过时运营列表也必须 404");

    // 状态侧保险：没有任何副作用。
    assert_eq!(count_rows(&state.db, "ifline_worlds").await, 0, "关闭时不得落 if 线行");
    assert_eq!(card_status(&state.db, "sc1").await.0, "owned", "关闭时不得烧卡");
}

/// 迁移本身不插种子数据（建表 ≠ 开闸）。
#[tokio::test]
async fn migration_seeds_no_rows() {
    let state = test_state().await;
    assert_eq!(count_rows(&state.db, "ifline_worlds").await, 0, "🔴 迁移 0039 不得插入任何种子记录");
}

// ═══════════════════════════════════════════════════════════════════════════
// ② 正常路径
// ═══════════════════════════════════════════════════════════════════════════

/// 开一条 if 线：独立实例落库 + 副本卡被烧 + 分叉点是**终局拍（第 2 拍）**而不是 MAX(tick_no)=3。
#[tokio::test]
async fn open_ifline_registers_instance_and_burns_card() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (st, body) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    assert_eq!(st, StatusCode::OK, "开 if 线应成功：{body}");
    assert_eq!(body["created"], json!(true));
    assert!(body["id"].as_str().unwrap().starts_with("ifw_"), "if 线 id 必须自带 `ifw_` 前缀");
    assert_eq!(body["status"], json!("sealed"));
    // 🔴 分叉点 = 最后一拍**已落定**的第 2 拍（第 3 拍 failed，不算）。
    assert_eq!(body["forkPoint"]["kind"], json!("terminal"));
    assert_eq!(body["forkPoint"]["tickNo"], json!(2));
    assert_eq!(body["forkPoint"]["stateRevision"], json!(7));
    assert_eq!(body["forkPoint"]["stateFidelity"], json!("origin_terminal_state"));
    assert_eq!(body["forkPoint"]["isApproximate"], json!(false));
    assert_eq!(body["protagonistInSnapshot"], json!(true));
    assert_eq!(body["premise"], json!("如果他在城门口没有退那一步。"));

    // 资源确实花了，且走的是副本卡既有状态机（软删可溯 + 反向血缘指向本条 if 线）。
    let (status, into) = card_status(&state.db, "sc1").await;
    assert_eq!(status, "consumed", "开 if 线必须真的烧掉副本卡");
    assert_eq!(into.as_deref(), body["id"].as_str(), "consumed_into 必须指向本条 if 线（反向血缘）");
    assert_eq!(body["cost"]["subplotCardIds"], json!(["sc1"]));

    // 审计留痕（§0.2 全链审计：烧资产必须留痕）。
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'ifline.opened' AND actor_id = 'u1'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(n, 1, "开 if 线必须落一条 audit_logs");
}

// ═══════════════════════════════════════════════════════════════════════════
// ③ 🔴 红线：不影响原世界线（§0.3 公共事实不可回滚）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **开 if 线后原世界数据逐字节不变**（本项最重要的红线）。
///
/// 玩家买 if 线的动机恰恰是「想改写那一拍」，所以这里用**逐字节快照比对**而不是抽查几列：
/// 十一张世界线/资产表在开 if 线前后必须完全相同。
#[tokio::test]
async fn red_line_opening_ifline_leaves_worldline_byte_identical() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let before = dump_worldline(&state.db).await;

    let (st, body) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // 顺带把读取面也跑一遍（读也不许有副作用）。
    let id = body["id"].as_str().unwrap().to_string();
    send(&state, "GET", "/api/me/iflines", &tk, None, None).await;
    send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk, None, None).await;
    send(&state, "GET", "/api/worlds/w1/ifline-fork-points", &tk, None, None).await;

    let after = dump_worldline(&state.db).await;
    for ((t, b), (_, a)) in before.iter().zip(after.iter()) {
        assert_eq!(b, a, "🔴 开 if 线后表 `{t}` 必须逐字节不变（§0.3 公共事实不可回滚）");
    }

    // 尤其点名那一列：世界的叙事状态与 revision 一个字节不动（if 线是复制，不是搬走）。
    let row = sqlx::query("SELECT narrative_state_json, state_revision, status FROM worlds WHERE id = 'w1'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(
        row.try_get::<String, _>("narrative_state_json").unwrap(),
        terminal_state().to_string(),
        "🔴 原世界 narrative_state_json 必须逐字节不变"
    );
    assert_eq!(row.try_get::<i64, _>("state_revision").unwrap(), 7);
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "ended");
}

/// 🔴 **if 线不是一行 `worlds`**——这是「产出不可能反哺」的结构性前提。
///
/// 一行 `worlds` + `world_members` 会被 `runtime → end_world_tx → finalize_ending_tx` 自动带进
/// 结算管线（发历练 / 铸卡 / 荣誉）。if 线落在自己的表里，那条路径在物理上不存在。
#[tokio::test]
async fn red_line_ifline_is_not_a_world_row() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let worlds_before = count_rows(&state.db, "worlds").await;
    let members_before = count_rows(&state.db, "world_members").await;

    let (st, body) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let id = body["id"].as_str().unwrap();

    assert_eq!(count_rows(&state.db, "worlds").await, worlds_before, "🔴 if 线绝不新建 worlds 行");
    assert_eq!(
        count_rows(&state.db, "world_members").await,
        members_before,
        "🔴 if 线绝不新建 world_members 行（那是结算管线的参与者名单）"
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worlds WHERE id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "🔴 if 线 id 在 worlds 里必须查无此行——它进不了世界列表、发现页与结算管线");
}

/// 🔴 **产出不反哺原世界**（§0.1 不卖胜负：付费只买体验容量，永不买结果）。
///
/// 开 if 线之后：历练、背包、贡献账本、荣誉、账本流水全部零变化；
/// 副本卡**只减不增**（唯一的资产变化是烧掉那一张，方向是净收缩）。
#[tokio::test]
async fn red_line_ifline_grants_nothing_back_to_origin() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let mileage_before: i64 =
        sqlx::query_scalar("SELECT CAST(COALESCE(SUM(mileage), 0) AS BIGINT) FROM cloud_characters")
            .fetch_one(&state.db)
            .await
            .unwrap();
    let owned_before = count_rows(&state.db, "subplot_cards").await;

    let (st, body) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let mileage_after: i64 =
        sqlx::query_scalar("SELECT CAST(COALESCE(SUM(mileage), 0) AS BIGINT) FROM cloud_characters")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(mileage_after, mileage_before, "🔴 if 线不得产出历练——历练是准入与卡位的钥匙，那就是买数值");
    assert_eq!(count_rows(&state.db, "backpacks").await, 0, "🔴 if 线不得发道具");
    assert_eq!(count_rows(&state.db, "world_contributions").await, 0, "🔴 if 线不得记世界线贡献");
    assert_eq!(count_rows(&state.db, "arena_rewards").await, 0, "🔴 if 线不得发荣誉");
    // 副本卡**总行数不增**：本模块只改状态，绝不 INSERT（铸卡的唯一入口仍是 subplot::grant_card_tx）。
    assert_eq!(
        count_rows(&state.db, "subplot_cards").await,
        owned_before,
        "🔴 if 线不得铸出任何新副本卡（资产单一写入路径：只有 subplot::grant_card_tx 可以铸卡）"
    );
    let still_owned: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subplot_cards WHERE owner_id='u1' AND status='owned'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(still_owned, 0, "唯一的资产变化方向是净收缩：那张卡被烧掉了");
}

// ═══════════════════════════════════════════════════════════════════════════
// ④ 🔴 红线：分叉点不假装（只支持终局分叉）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **请求中间拍必须被明确拒绝**，且**不动任何资源**。
///
/// 仓库不存逐拍状态快照（world_ticks 无状态列 / narrative_state_json 每拍被覆盖 /
/// world_events 只是投影文本且 StatePatch 从不落库），所以第 1 拍的世界态已不可复原。
/// 平台**不会**拿终局态冒充第 1 拍——那是在为一个假分叉收费。
#[tokio::test]
async fn red_line_mid_tick_fork_is_rejected_without_touching_resources() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let mut body = open_body();
    body["tickNo"] = json!(1); // 终局拍是 2，这里点名要第 1 拍。
    let (st, resp) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(body), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "🔴 中间拍分叉必须明确拒绝，不得静默降级：{resp}");
    let msg = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("第 1 拍"), "拒绝理由要点名是哪一拍：{msg}");
    assert!(msg.contains("终局拍（第 2 拍）"), "拒绝理由要告诉玩家唯一可用的分叉点：{msg}");

    assert_eq!(count_rows(&state.db, "ifline_worlds").await, 0, "被拒的请求不得落 if 线行");
    assert_eq!(card_status(&state.db, "sc1").await.0, "owned", "🔴 被拒的请求一张卡都不许烧");
}

/// 显式请求终局拍号是允许的（玩家知道自己在分哪儿）。
#[tokio::test]
async fn explicit_terminal_tick_no_is_accepted() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let mut body = open_body();
    body["tickNo"] = json!(2);
    let (st, resp) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(body), None).await;
    assert_eq!(st, StatusCode::OK, "显式点名终局拍应当被接受：{resp}");
    assert_eq!(resp["forkPoint"]["tickNo"], json!(2));
}

/// 🔴 未支持的分叉点档位（`tick`）必须 400，**不得静默降级成 terminal**。
#[tokio::test]
async fn red_line_unsupported_fork_point_kind_is_rejected() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let mut body = open_body();
    body["forkPoint"] = json!(FORK_POINT_TICK);
    let (st, resp) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(body), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{resp}");
    assert_eq!(count_rows(&state.db, "ifline_worlds").await, 0);
    assert_eq!(card_status(&state.db, "sc1").await.0, "owned");
}

/// 分叉点端点把**限制本身**结构化下发：客户端不必猜，也不会先让玩家填完表单才告诉他不行。
#[tokio::test]
async fn fork_points_endpoint_declares_the_limitation() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (st, body) = send(&state, "GET", "/api/worlds/w1/ifline-fork-points", &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["eligible"], json!(true));
    assert_eq!(body["layer"], json!("ifline"));
    assert_eq!(body["affectsOriginWorld"], json!(false));

    let supported = body["supportedForkPoints"].as_array().unwrap();
    assert_eq!(supported.len(), 1, "🔴 当前**只有一个**可用分叉点：终局");
    assert_eq!(supported[0]["kind"], json!("terminal"));
    assert_eq!(supported[0]["tickNo"], json!(2));
    assert_eq!(supported[0]["isApproximate"], json!(false));

    let unsupported = body["unsupportedForkPoints"].as_array().unwrap();
    assert_eq!(unsupported[0]["kind"], json!("tick"));
    let reason = unsupported[0]["reason"].as_str().unwrap();
    assert!(reason.contains("world_ticks"), "限制说明必须给出证据而不是一句「暂不支持」：{reason}");
    assert!(unsupported[0]["unlockPath"].is_string(), "要写清楚补齐路径");
    assert_eq!(body["cost"]["subplotCards"], json!(1));
}

/// 世界还在演的时候不开分叉（if 线是**事后**出口，§7 原文「世界结束后」）。
#[tokio::test]
async fn running_world_cannot_be_forked() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    sqlx::query("UPDATE worlds SET status='running' WHERE id='w1'")
        .execute(&state.db)
        .await
        .unwrap();
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (st, resp) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{resp}");
    assert_eq!(card_status(&state.db, "sc1").await.0, "owned", "被拒时不许烧卡");

    let (st, body) = send(&state, "GET", "/api/worlds/w1/ifline-fork-points", &tk, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["eligible"], json!(false));
    assert!(body["ineligibleReason"].is_string(), "不合格必须给出可读理由");
    assert_eq!(body["supportedForkPoints"].as_array().unwrap().len(), 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑤ 🔴 红线：§14 社交防火墙 —— 单人平行线
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **他人玩家角色不得未经同意进入你的 if 线**（§14）。
///
/// c2（属 u2）与涉及它的关系边、以及其它关系边里对它的 `knownTo` 引用全部剥离；
/// NPC（npc_shen）与本人角色（c1）保留——NPC 是世界的，不是谁的。
/// 剥离台账必须可查：**不能既剥离了又不告诉人剥离了什么**。
#[tokio::test]
async fn red_line_foreign_player_characters_are_redacted_from_snapshot() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (st, body) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let id = body["id"].as_str().unwrap().to_string();

    // 台账（列表面就能看到）。
    assert_eq!(body["redaction"]["removedCharacterIds"], json!(["c2"]));
    assert_eq!(body["redaction"]["removedRelationEdges"], json!(1));
    assert_eq!(body["redaction"]["removedKnownToRefs"], json!(1));
    assert!(body["redaction"]["rule"].as_str().unwrap().contains("§14"));

    // 快照本体（详情面）。
    let (st, detail) = send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "{detail}");
    let chars = detail["snapshot"]["characters"].as_object().unwrap();
    assert!(chars.contains_key("c1"), "本人角色必须留下");
    assert!(chars.contains_key("npc_shen"), "NPC 不剥离——NPC 是世界的，不是谁的");
    assert!(!chars.contains_key("c2"), "🔴 他人玩家角色必须被剥离（§14）");

    let rels = detail["snapshot"]["relations"].as_array().unwrap();
    assert_eq!(rels.len(), 1, "🔴 涉及他人角色的关系边必须整条移除");
    assert_eq!(rels[0]["to"], json!("npc_shen"));
    let known: Vec<&str> = rels[0]["knownTo"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(!known.contains(&"c2"), "🔴 剩余关系边里对他人角色的 knownTo 引用也必须清掉（否则引用悬空）");
    assert!(known.contains(&"c1") && known.contains(&"npc_shen"));

    // 除被剥离的以外，其余一个字节不改（弱类型复制，不丢未知字段）。
    assert_eq!(detail["snapshot"]["world"], json!({ "era": "城破之年" }));
    assert_eq!(detail["snapshot"]["schemaVersion"], json!(1));
    assert_eq!(detail["snapshot"]["narrative"], json!({ "pendingConsents": [] }));
}

// ⚠️ 此处原有 `red_line_memorial_sealed_character_cannot_open_ifline`：传世卡不可进 if 线
// （否则就是「付费复活」）。**2026-07-29 随 memorial 整块删除**——角色卡永不损失，
// 不存在封卷这回事，「付费复活」这个担心也就不成立了。`withdrawn` 那一半的闸仍在。


/// 拿别人的卡开 if 线 → 风控拦截（403 risk_blocked）+ 记险，且不烧卡。
#[tokio::test]
async fn foreign_character_is_risk_blocked() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let mut body = open_body();
    body["characterId"] = json!("c2"); // 是 u2 的卡
    let (st, resp) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(body), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{resp}");
    assert_eq!(resp["error"]["code"], json!("risk_blocked"));
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM risk_events WHERE kind='ifline_denied'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 1, "伪造他人角色必须记险");
    assert_eq!(card_status(&state.db, "sc1").await.0, "owned");
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑥ 幂等与资源扣减
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **幂等：换幂等键再点也只开一条、只烧一张**。
///
/// 单靠 `Idempotency-Key` 会被「换 key 再点」击穿，故第二道闸落在 DB 唯一键
/// `(owner_id, fork_key)` 上。两次都验：同 key 重放 + 换 key 重放。
#[tokio::test]
async fn opening_is_idempotent_and_never_double_burns() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    // 再给一张卡：如果幂等失效，第二次点击会把它也烧掉——用它来抓漏。
    seed_card(&state, "sc2", "u1", "w1").await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (st, first) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), Some("k1")).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    let id = first["id"].as_str().unwrap().to_string();

    // ① 同幂等键重放 → 命中幂等缓存。
    let (st, again) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), Some("k1")).await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["id"], json!(id));

    // ② **换幂等键**再点（幂等键那层被绕过）→ DB 唯一键接住，读回既有那条。
    let mut other = open_body();
    other["cardIds"] = json!(["sc2"]); // 甚至换一张卡：也不许再烧
    let (st, third) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(other), Some("k2")).await;
    assert_eq!(st, StatusCode::OK, "{third}");
    assert_eq!(third["id"], json!(id), "同人同卡同分叉点只开得出一条 if 线");
    assert_eq!(third["created"], json!(false), "重复请求必须回 created:false");

    assert_eq!(count_rows(&state.db, "ifline_worlds").await, 1, "🔴 绝不重复建实例");
    assert_eq!(card_status(&state.db, "sc1").await.0, "consumed");
    assert_eq!(card_status(&state.db, "sc2").await.0, "owned", "🔴 重复请求绝不二次扣资源");
}

/// 不属于自己 / 已消耗的副本卡 → 409，且**整笔回滚**（if 线不留、其它卡不烧）。
#[tokio::test]
async fn card_not_in_hand_rolls_everything_back() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    seed_card(&state, "sc_other", "u2", "w1").await; // 别人的卡
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let mut body = open_body();
    body["cardIds"] = json!(["sc_other"]);
    let (st, resp) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(body), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{resp}");

    assert_eq!(count_rows(&state.db, "ifline_worlds").await, 0, "🔴 抢不到卡 → if 线整笔回滚，不得留下");
    assert_eq!(card_status(&state.db, "sc_other").await.0, "owned", "别人的卡一个字节不动");
    assert_eq!(card_status(&state.db, "sc1").await.0, "owned");
}

/// 卡数不对（多给/少给/重复）→ 400，前门就挡下，不进事务。
#[tokio::test]
async fn wrong_card_count_is_rejected_at_the_front_door() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    seed_card(&state, "sc2", "u1", "w1").await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    for cards in [json!([]), json!(["sc1", "sc2"]), json!(["sc1", "sc1"])] {
        let mut body = open_body();
        body["cardIds"] = cards.clone();
        let (st, resp) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(body), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "cardIds={cards} 应被拒：{resp}");
    }
    assert_eq!(card_status(&state.db, "sc1").await.0, "owned");
    assert_eq!(card_status(&state.db, "sc2").await.0, "owned");
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑦ 读取面：可区分 + 信息边界
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **if 线在读取面一眼可辨，且不冒充原世界线**。
///
/// 每条响应恒带四个标签（`layer` / `isWorldFact` / `affectsOriginWorld` / `stateFidelity`），
/// 且**不含 `ownerId`**（§14：owner 只用于 SQL 过滤，从不出现在响应里）。
#[tokio::test]
async fn red_line_read_surface_is_distinguishable_from_worldline() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (_, created) = send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (st, list) = send(&state, "GET", "/api/me/iflines", &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let (st, detail) = send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "{detail}");

    for item in [&created, &list["items"][0], &detail] {
        assert_eq!(item["layer"], json!("ifline"), "每条 if 线都必须自报层次");
        assert_eq!(item["isWorldFact"], json!(false), "🔴 if 线不是世界事实");
        assert_eq!(item["affectsOriginWorld"], json!(false), "🔴 恒 false 且写进响应：明说原世界没动");
        assert_eq!(item["forkPoint"]["stateFidelity"], json!("origin_terminal_state"), "保真度必须随每次读取下发");
        assert_eq!(item["origin"]["worldId"], json!("w1"));
        assert_eq!(item["origin"]["readOnly"], json!(true));
        assert!(item.get("ownerId").is_none(), "🔴 响应体不得出现 ownerId（§14 社交防火墙）");
    }
    // 列表层也带标签（前端拿到整页也能一眼分清）。
    assert_eq!(list["layer"], json!("ifline"));
    assert_eq!(list["affectsOriginWorld"], json!(false));

    // 🔴 本模块从不返回任何世界事实：世界事实的唯一读路径是 /api/worlds/{id}/events。
    let dump = serde_json::to_string(&detail).unwrap();
    assert!(!dump.contains("他在城门口退了一步"), "🔴 if 线读取面不得夹带 world_events 的投影正文");
}

/// 信息边界：别人的 if 线一律 **404**（不是 403——403 等于承认它存在）。
#[tokio::test]
async fn other_users_ifline_is_404_not_403() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk1 = token(&state, "u1");
    let tk2 = token(&state, "u2");

    let (_, created) = send(&state, "POST", "/api/worlds/w1/iflines", &tk1, Some(open_body()), None).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (st, _) = send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk2, None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "🔴 越权读一律 404");

    let (st, list) = send(&state, "GET", "/api/me/iflines", &tk2, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 0, "别人的列表里不得出现我的 if 线");
}

/// 运营面：operator 档可读、非运营档 403；列表**不下发 ownerId**（§14 口径同复核队列）。
#[tokio::test]
async fn admin_list_requires_operator_and_hides_owner() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");
    send(&state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;

    let (st, _) = send(&state, "GET", "/api/admin/iflines", &admin_token(&state, "support"), None, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "非 operator/admin 档不得查 if 线");

    let (st, body) = send(&state, "GET", "/api/admin/iflines", &admin_token(&state, "operator"), None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["originWorldId"], json!("w1"));
    assert_eq!(items[0]["costCardIds"], json!(["sc1"]));
    assert!(items[0].get("ownerId").is_none(), "🔴 运营列表不下发真人身份（§14）");
    assert_eq!(body["affectsOriginWorld"], json!(false));
}

/// 按世界灰度：只给 w1 开 → w1 能开分叉，另一个已结束的世界仍 404。
#[tokio::test]
async fn world_scoped_gray_release_works() {
    let state = test_state().await;
    seed_ended_world(&state).await;
    seed_world(&state.db, "w2", 3, "ended").await;
    open_flag(&state, crate::flags::SCOPE_WORLD, "w1").await;
    let tk = token(&state, "u1");

    let (st, _) = send(&state, "GET", "/api/worlds/w1/ifline-fork-points", &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "灰度命中的世界应可见");
    let (st, _) = send(&state, "GET", "/api/worlds/w2/ifline-fork-points", &tk, None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "未灰度的世界仍不可见");

    // 运营面判定用 `entry_ever_open`：按世界灰度时仍可见，否则已开的 if 线运营查不到。
    let (st, _) = send(&state, "GET", "/api/admin/iflines", &admin_token(&state, "operator"), None, None).await;
    assert_eq!(st, StatusCode::OK, "入口曾对任何人开放过 → 运营列表必须可见");
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑧ 🔴 源码级红线
// ═══════════════════════════════════════════════════════════════════════════

/// 只保留**代码行**（丢掉整行注释）的源码视图。
///
/// 🔴 存在的理由：本模块的注释里大量出现 `grant_card_tx` / `mileage` / `INSERT INTO worlds`
/// 这类词——**正是在逐条解释「为什么不碰它们」**。对全文 grep 会把「写清楚了不碰」误判成「碰了」，
/// 于是逼着后来的人删注释来过测试，恰好把最该留下的说明删掉。红线要锁的是**代码**。
fn code_only(src: &str) -> String {
    src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// 🔴 **红线的扫描面 = 本功能的全部源码**。
///
/// 0039 只有 `mod.rs`，源码级用例直接 `include_str!("mod.rs")`；0041 加了 `runner.rs`（推进）；
/// 0052 加了 `sweep.rs`（对账补投）。若扫描面不跟着扩，新增文件就是红线的盲区——
/// 「不写世界线 / 不铸资产 / 不用系统随机」这三条会在最需要它们的那个文件上失效，
/// 而所有用例仍然全绿。
///
/// ⚠️ **这里原本写着「将来再拆文件时必须同步加进这里」——而它当场就没被做到**：
/// 0052 新增 `sweep.rs` 时我漏了这一步，三条红线对新文件静默失效了一整批提交
/// （所幸那个文件确实没有违反，但那是运气不是保证）。
/// 所以现在不靠人记得：`ifline_source_files_are_all_scanned` 从**目录**核对扫描面，
/// 少一个文件就红。`include_str!` 必须是字面量（编译期读取），故清单仍写在这里，
/// 但「清单是否完整」由那条用例负责。
fn ifline_sources() -> String {
    [
        code_only(include_str!("mod.rs")),
        code_only(include_str!("runner.rs")),
        code_only(include_str!("sweep.rs")),
    ]
    .join("\n")
}

/// 🔴 扫描面完整性：`ifline/` 下每一个生产源码文件都必须在 `ifline_sources()` 里。
///
/// 这条防的是上面那段注释**已经预言过、而且真的发生了**的事：加了新文件、忘了加扫描面，
/// 于是三条红线在新文件上静默失效，全绿。
#[test]
fn ifline_source_files_are_all_scanned() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ifline");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .expect("读 ifline 目录")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".rs") && n != "tests.rs")
        .collect();
    files.sort();
    assert!(files.len() >= 3, "ifline 目录只解出 {} 个文件，读目录疑似失效：{files:?}", files.len());

    let listed = include_str!("tests.rs");
    for f in &files {
        assert!(
            listed.contains(&format!("include_str!(\"{f}\")")),
            "🔴 `ifline/{f}` 没有进 `ifline_sources()` 的扫描面——\n\
             「不写世界线 / 不铸资产 / 不用系统随机」三条红线对这个文件是失效的，而用例会全绿。\n\
             把 `code_only(include_str!(\"{f}\"))` 加进 `ifline_sources()`。"
        );
    }
}

/// 🔴 **本模块对世界线表只有 SELECT**（§0.3 公共事实不可回滚的源码级证据）。
///
/// 逐字节快照用例证明「这一次没写」，源码级断言证明「压根没有写的语句」——两者都要。
#[test]
fn red_line_never_writes_worldline() {
    // 扫描面含 `runner.rs`（推进）——见 `ifline_sources` 的注释。
    let src = ifline_sources();
    for banned in [
        "INSERT INTO worlds",
        "UPDATE worlds",
        "DELETE FROM worlds",
        "INSERT INTO world_events",
        "UPDATE world_events",
        "INSERT INTO world_ticks",
        "UPDATE world_ticks",
        "INSERT INTO world_members",
        "UPDATE world_members",
        "UPDATE cloud_characters",
        "INSERT INTO world_contributions",
        "UPDATE consent_requests",
        "INSERT INTO interventions",
        "narrative_state_json =",
        "SET narrative_state_json",
    ] {
        assert!(
            !src.contains(banned),
            "🔴 if 线模块出现世界线写入语句 `{banned}`：if 线是平行线，不是改写（§0.3）"
        );
    }
}

/// 🔴 **if 线不铸任何资产、不进任何结算路径**（§0.1 付费只买体验容量，永不买结果）。
#[test]
fn red_line_never_mints_assets() {
    // 扫描面含 `runner.rs`（推进）：if 线的**终局**在那个文件里，红线最需要守的正是那里。
    let src = ifline_sources();
    for banned in [
        "grant_card_tx",
        "grant_item_tx",
        "grant_mileage_tx",
        "settle_subplot_card_tx",
        "settle_worldline_tx",
        "settle_idle_world_ending_tx",
        "accumulate_contributions_tx",
        "INSERT INTO subplot_cards",
        "INSERT INTO backpacks",
        "INSERT INTO arena_rewards",
        "INSERT INTO ledger_",
        "crate::ledger",
        "mileage",
    ] {
        assert!(
            !src.contains(banned),
            "🔴 if 线模块出现资产产出路径 `{banned}`：if 线的产出只能是内容，不能是资产"
        );
    }
    // 唯一被允许的资产写入：把已发出的副本卡从 owned 改成 consumed（净收缩）。
    assert!(
        src.contains("UPDATE subplot_cards SET status = $1"),
        "烧卡必须走副本卡既有的状态机（owned → consumed 的 CAS）"
    );
}

/// 🔴 **引擎零读取路径**：if 线永远进不了 `RoundInput.state`，
/// 于是它既改不了过去（事实已落定），也影响不了原世界的未来。
/// 口径与 0025 贡献账本 / 0030 critic / 0034 故人印记 / 0037 批注逐字一致。
#[test]
fn red_line_ifline_never_enters_engine() {
    let runtime_src = include_str!("../runtime/mod.rs");
    assert!(
        !runtime_src.contains("ifline"),
        "runtime/mod.rs（RoundInput 组装处）不得引用 if 线：平行线不进原世界的引擎决策"
    );
    let engine_narrative_src = include_str!("../../../crates/muse-engine/src/narrative/mod.rs");
    assert!(
        !engine_narrative_src.contains("ifline"),
        "muse-engine narrative（RoundInput/role_decide/仲裁）不得引用 if 线"
    );
    for table in ["ifline_worlds"] {
        assert!(!runtime_src.contains(table), "runtime 不得引用 `{table}`");
    }
}

/// 🔴 **零系统随机**：与副本卡同一条合规定性防线（有随机就有爆率，有爆率就是抽卡）。
/// if 线的产出是确定的：给什么卡、分哪一拍、剥离谁、哪几个 NPC 上场，全部可复算。
///
/// 禁的是**不可复现**的系统随机（`thread_rng` / `random` / `shuffle` / `gen_range`）。
/// 0041 的推进确实用了 PRNG，但用的是仓库登记在册的确定性子流
/// （`fnv1a_64` + SplitMix64 + 域常量 `0x5B`，见 `runner` 模块头「为什么这里可以有 PRNG」），
/// 且它只决定叙事编排（这一拍哪几个 NPC 上场），不决定任何资产——
/// 可复现性由 `cast_selection_is_deterministic_and_seed_sensitive` 正面守。
#[test]
fn red_line_module_is_rng_free() {
    let src = ifline_sources().to_ascii_lowercase();
    for banned in ["rand::", "random", "shuffle", "thread_rng", "gen_range"] {
        assert!(
            !src.contains(banned),
            "if 线模块出现**不可复现**的随机数 API `{banned}`：确定性产出是合规红线。\
             需要随机请走 assembly 的 fnv1a_64 + Rng（SplitMix64）并登记域常量"
        );
    }
}

/// 副本卡状态字面量必须与 `subplot` 一致——两处漂移会让 CAS 静默命中 0 行，
/// 表现为「玩家有卡却说没卡」，是最难查的那类 bug。
#[test]
fn card_status_literals_match_subplot() {
    let subplot_src = include_str!("../subplot/mod.rs");
    assert!(
        subplot_src.contains(&format!("STATUS_OWNED: &str = \"{CARD_STATUS_OWNED}\"")),
        "`owned` 字面量必须与 subplot 一致"
    );
    assert!(
        subplot_src.contains(&format!("STATUS_CONSUMED: &str = \"{CARD_STATUS_CONSUMED}\"")),
        "`consumed` 字面量必须与 subplot 一致"
    );
}

/// 参数化（§0.2 禁写死）：卡开销可调、有上限、非法值回落默认。
#[test]
fn card_cost_is_parameterised_with_bounds() {
    assert_eq!(DEFAULT_IFLINE_CARD_COST, 1);
    assert!(MAX_IFLINE_CARD_COST >= DEFAULT_IFLINE_CARD_COST);
    // 未设 env 时取默认（本进程其它用例不设置它）。
    assert!(ifline_card_cost() >= 1 && ifline_card_cost() <= MAX_IFLINE_CARD_COST);
}

/// 🔴 冻结函数的纯函数级验证：**除被剥离的以外一个字节不改**，且未知字段不丢。
#[test]
fn freeze_keeps_unknown_fields_and_only_removes_foreign() {
    let raw = json!({
        "schemaVersion": 1,
        "someFutureField": { "deep": [1, 2, 3] },
        "characters": { "me": {}, "you": {}, "npc": {} },
        "relations": [
            { "from": "me", "to": "you", "knownTo": ["me", "you"] },
            { "from": "me", "to": "npc", "knownTo": ["me", "you", "npc"] }
        ]
    })
    .to_string();
    let f = freeze_snapshot(&raw, &["you".to_string()], "me").expect("可冻结");
    assert_eq!(
        f.snapshot["someFutureField"],
        json!({ "deep": [1, 2, 3] }),
        "🔴 未知字段必须原样保留（用弱类型 Value 复制的意义就在这里）"
    );
    assert_eq!(f.character_count, 2);
    assert_eq!(f.relation_count, 1);
    assert!(f.protagonist_present);
    assert_eq!(f.redaction["removedCharacterIds"], json!(["you"]));
    assert_eq!(f.snapshot["relations"][0]["knownTo"], json!(["me", "npc"]));

    // 空状态（世界从未演过任何一拍）→ 不可冻结。
    assert!(freeze_snapshot("{}", &[], "me").is_none(), "空状态没有可分叉的东西");
    assert!(freeze_snapshot("不是 JSON", &[], "me").is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑧ 推进（跑拍，迁移 0041）
// ═══════════════════════════════════════════════════════════════════════════
//
// 这一节的靶子是接线时最高优先级的那条红线：
//
//   🔴 **if 线的终局绝不允许进入 `progression::settle_*` / `subplot::settle_subplot_card_tx` /
//      `arena_rewards`** —— 它的产出只能是内容，不能是资产。
//
// 光有源码级 grep 不够（那只证明「没写那几个符号」），必须有**跑到终局后逐表求和**的运行时用例
// （`red_line_ifline_ending_grants_nothing`）。两者一起才既证明「压根没有那条语句」，
// 又证明「真跑一遍确实什么都没发」。

use std::collections::{BTreeMap, BTreeSet};

use muse_engine::host::CancelFlag as TestCancel;
use muse_engine::model::{ModelCallSpec, ModelClient as TestModelClient, ModelOutput};
use muse_engine::EngineError;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// 推进脚手架
// ---------------------------------------------------------------------------

/// 剧本化模型（与 `runtime::golden::ScriptedModel` 同源思路，但只需五个环节的兜底响应——
/// 本节测的是**接线与红线**，不是内容质量，故不编排具体剧情）。
///
/// 🔴 它回放的是人写的固定响应，不是真实模型响应：本节的断言全部落在
/// 「跑完之后库里变了什么 / 没变什么」上，与模型说了什么无关。
struct ScriptedModel {
    calls: AtomicUsize,
}

impl ScriptedModel {
    fn new() -> Arc<Self> {
        Arc::new(Self { calls: AtomicUsize::new(0) })
    }
    fn call_count(&self) -> usize {
        self.calls.load(AtomicOrdering::SeqCst)
    }
}

#[async_trait::async_trait]
impl TestModelClient for ScriptedModel {
    async fn complete(
        &self,
        spec: &ModelCallSpec,
        cancel: &TestCancel,
    ) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"城门未闭，风把灯笼吹得直晃。"}"#.to_string(),
            "arbiter" => r#"{"outcomes":[]}"#.to_string(),
            "writer" => r#"{"prose":"他没有退那一步。风从城门灌进来，把两个人的影子推到一处。"}"#.to_string(),
            "critic" => {
                r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#
                    .to_string()
            }
            _ => json!({
                "intent": "站住不动",
                "action": "把脚跟钉在原地，看着来人",
                "speak": { "willSpeak": false, "purpose": "" },
                "targets": [],
                "acceptableCosts": [],
                "predictions": [],
                "duration": 60,
            })
            .to_string(),
        };
        Ok(ModelOutput { content, input_tokens: Some(40), output_tokens: Some(60) })
    }
}

/// 从黄金世界的固定角色卡库里取一张，并把卡内 `id` 改写成本用例用的角色 id。
///
/// **复用 fixture 而不另造一套**：另造一套等于多一处会与引擎 `CharacterCardV2` schema 漂移的地方，
/// 而漂移的表现是「卡静默解析失败 → 角色不上场」，属最难查的那类。
fn card_for(character_id: &str, fixture: &str) -> Value {
    let all: BTreeMap<String, Value> =
        serde_json::from_str(include_str!("../runtime/golden/cards.json")).expect("cards.json 合法");
    let mut card = all.get(fixture).unwrap_or_else(|| panic!("cards.json 缺少 {fixture}")).clone();
    card["id"] = json!(character_id);
    card
}

async fn seed_routes(state: &AppState) {
    let routes = json!({
        "default": { "interface": "OpenAI-compatible", "baseUrl": "http://mock", "apiKey": "k", "model": "mock" }
    });
    sqlx::query("INSERT INTO model_routes (id, version, routes_json, active, created_at) VALUES ($1, 'm1', $2, 1, $3)")
        .bind(new_id("mr"))
        .bind(routes.to_string())
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
}

/// 给原世界钉一份装配结果：一个 NPC（沈砚的卡挂在 `npc_shen` 上）+ 一个地点 + 一个结局名。
///
/// 🔴 用例刻意让 `worldCharacterEntries` 里**也混进一个他人玩家角色 `c2`**：
/// §14 的纵深防御必须在这里就把它挡掉，而不是指望装配层永远不会写玩家进去。
async fn seed_assembled(state: &AppState, world: &str) {
    let assembled = json!({
        "assembly": {
            "worldCharacterEntries": [
                { "characterId": "npc_shen", "card": card_for("npc_shen", "shenyan"), "location": "城门" },
                // 🔴 §14 靶子：他人玩家角色混进装配表，必须被剔除。
                { "characterId": "c2", "card": card_for("c2", "peizhao"), "location": "城门" }
            ],
            "locationGraph": [
                { "id": "城门", "name": "城门", "isSecretRealm": false, "connectsTo": [] }
            ],
            "enabledEndings": ["城门未闭"]
        }
    });
    sqlx::query("UPDATE worlds SET assembled_json = $1 WHERE id = $2")
        .bind(assembled.to_string())
        .bind(world)
        .execute(&state.db)
        .await
        .unwrap();
}

/// 一份「跑一拍就到终局」的分叉态：`timeline.now >= timeCap` → 引擎 `is_terminal` 判 `TimeCapReached`。
///
/// 用时间上限而不是里程碑：里程碑要靠模型把 outline 推 Done，那是内容编排；
/// 本节要的是一个**确定、与模型输出无关**的终局触发器。
fn terminal_capped_state() -> Value {
    let mut v = terminal_state();
    v["timeline"] = json!({ "now": 0, "timeCap": 0 });
    v
}

/// 完整布景 + 已开好的一条 if 线，返回 if 线 id。
async fn seed_open_ifline(state: &AppState) -> String {
    seed_ended_world(state).await;
    seed_routes(state).await;
    seed_assembled(state, "w1").await;
    open_flag(state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(state, "u1");
    let (st, body) =
        send(state, "POST", "/api/worlds/w1/iflines", &tk, Some(open_body()), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// 推进一拍（注入剧本模型，走生产同一条 `runner::advance_one_beat`）。
async fn advance(
    state: &AppState,
    ifline_id: &str,
    model: &Arc<ScriptedModel>,
) -> Result<runner::BeatOutcome, ApiError> {
    let row = fetch_ifline(&state.db, ifline_id).await.unwrap().expect("if 线存在");
    let mc: Arc<dyn TestModelClient> = model.clone();
    runner::advance_one_beat(state, &row, Some(mc)).await
}

// ---------------------------------------------------------------------------
// 🔴 最高优先级红线
// ---------------------------------------------------------------------------

/// 🔴 **if 线跑到终局后，什么资产都没发**（本批次的头号红线）。
///
/// 历练是准入门槛与卡位解锁的钥匙。一旦 if 线的终局能发历练，「花钱开 if 线」立刻等于
/// 「花钱买数值」，踩穿 §0.1「付费只买体验容量，永不买结果」与平台红线「不卖胜负与数值平权」。
///
/// 断言口径刻意用**全表求和 / 全表行数**而不是「查某一行没变」：
/// 挑行意味着漏掉的那一行就是没被守住的那一行，而红线要守的是「一分钱都没发」。
#[tokio::test]
async fn red_line_ifline_ending_grants_nothing() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    set_state(&state, "w1", &terminal_capped_state(), 7).await;
    // 分叉态在开 if 线时已冻结，故直接把冻结那份换成「跑一拍即终局」的版本。
    sqlx::query("UPDATE ifline_worlds SET snapshot_json = $1 WHERE id = $2")
        .bind(terminal_capped_state().to_string())
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();

    let mileage_before: i64 =
        sqlx::query_scalar("SELECT CAST(COALESCE(SUM(mileage), 0) AS BIGINT) FROM cloud_characters")
            .fetch_one(&state.db)
            .await
            .unwrap();
    let cards_before = count_rows(&state.db, "subplot_cards").await;
    let backpacks_before = count_rows(&state.db, "backpacks").await;
    let contrib_before = count_rows(&state.db, "world_contributions").await;
    let rewards_before = count_rows(&state.db, "arena_rewards").await;
    let ticks_before = count_rows(&state.db, "world_ticks").await;

    let model = ScriptedModel::new();
    let out = advance(&state, &id, &model).await.expect("推进成功");

    // 前置确认：这一拍**真的跑到了终局**（否则下面的"什么都没发"是废断言）。
    assert_eq!(out.ifline_status, "ended", "本用例要求跑到终局，否则红线断言没有意义");
    assert_eq!(out.ending_reason, "time_cap");
    assert!(model.call_count() > 0, "确认引擎真的被调用过（这一拍不是空跑）");

    // ── 🔴 红线本体 ─────────────────────────────────────────────────────────
    let mileage_after: i64 =
        sqlx::query_scalar("SELECT CAST(COALESCE(SUM(mileage), 0) AS BIGINT) FROM cloud_characters")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(
        mileage_after, mileage_before,
        "🔴 if 线终局不得产出历练——历练是准入与卡位解锁的钥匙，发了就是「花钱买数值」（§0.1）"
    );
    assert_eq!(
        count_rows(&state.db, "subplot_cards").await,
        cards_before,
        "🔴 if 线终局不得铸卡（铸卡的唯一写入路径仍是 subplot::grant_card_tx，§0.2 资产单一写入）"
    );
    assert_eq!(
        count_rows(&state.db, "backpacks").await,
        backpacks_before,
        "🔴 if 线终局不得发道具"
    );
    assert_eq!(
        count_rows(&state.db, "world_contributions").await,
        contrib_before,
        "🔴 if 线终局不得记世界线贡献"
    );
    assert_eq!(
        count_rows(&state.db, "arena_rewards").await,
        rewards_before,
        "🔴 if 线终局不得发荣誉"
    );
    assert_eq!(
        count_rows(&state.db, "world_ticks").await,
        ticks_before,
        "🔴 if 线的拍绝不落 world_ticks——落进去就会被 commit_tick → end_world_tx → settle_* 那条自动链路捡走"
    );

    // 终局产物必须是**内容**：状态为 ended + 两个字符串 + 一份可读正文。
    let tk = token(&state, "u1");
    let (st, body) = send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ended");
    assert_eq!(body["ending"]["isContentOnly"], true);
    assert_eq!(body["ending"]["grantedAssets"], json!([]), "🔴 终局投影必须明写「什么都没发」");
    assert_eq!(body["ending"]["reason"], "time_cap");

    let (st, beats) =
        send(&state, "GET", &format!("/api/me/iflines/{id}/beats"), &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "{beats}");
    assert_eq!(beats["items"].as_array().unwrap().len(), 1, "跑了一拍就该有一拍正文");
    assert_eq!(beats["grantedAssets"], json!([]));
    assert!(
        beats["items"][0]["prose"].as_str().unwrap_or("").contains("没有退那一步"),
        "终局产物 = 可读的正文（内容），这就是玩家买到的全部东西"
    );

    // 🔴 AI 生成标识（平台红线 §0.6）：if 线正文是**整段模型生成**的付费内容。
    // 它此前一直没有标识，而同为叙事读取面的 `events` / `clips` / `worlds` 都有——
    // 「显式标注已实现」这句话曾经只对其中一部分成立。
    assert_eq!(
        beats["items"][0]["aiLabel"]["visible"],
        json!(true),
        "🔴 if 线正文必须带 AI 生成标识"
    );

    // 审计必须把「什么都没发」这件事记下来（将来任何人翻记录都能一眼确认）。
    let reason: String = sqlx::query_scalar(
        "SELECT reason FROM audit_logs WHERE action = 'ifline.ended' AND subject = $1",
    )
    .bind(format!("ifline:{id}"))
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert!(reason.contains("grantedAssets=none"), "审计必须明写没有发放资产：{reason}");
    assert!(reason.contains("settlementEntered=none"), "审计必须明写没有进结算：{reason}");
    assert!(reason.contains("worldlineChanged=false"), "审计必须明写世界线没变：{reason}");
}

/// 🔴 **推进过程中原世界逐字节不变**（红线：平行线不是改写，§0.3）。
///
/// 开 if 线那一刻的逐字节比对已有用例（0039）；本用例守的是**跑起来之后**——
/// 引擎会产状态、产事件、产 critic，这些东西一个字节都不许流回世界线。
#[tokio::test]
async fn red_line_advancing_leaves_worldline_byte_identical() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let before = dump_worldline(&state.db).await;

    let model = ScriptedModel::new();
    advance(&state, &id, &model).await.expect("推进成功");
    advance(&state, &id, &model).await.expect("再推一拍");

    let after = dump_worldline(&state.db).await;
    for ((t, b), (_, a)) in before.iter().zip(after.iter()) {
        assert_eq!(b, a, "🔴 推进 if 线后 `{t}` 必须逐字节不变——if 线是平行线，不是改写（§0.3）");
    }
}

/// 🔴 **源码级：推进路径不碰任何结算入口**。
///
/// 运行时用例证明「这一次什么都没发」，源码级断言证明「压根没有那条语句」——两者都要。
/// 扫描面**必须含 `runner.rs`**：新增文件若不进扫描面就是红线的盲区。
#[test]
fn red_line_runner_never_enters_settlement() {
    let src = code_only(include_str!("runner.rs"));
    for banned in [
        "settle_idle_world_ending_tx",
        "settle_subplot_card_tx",
        "settle_worldline_tx",
        "accumulate_contributions_tx",
        "finalize_ending_tx",
        "end_world_tx",
        "commit_tick",
        "grant_card_tx",
        "grant_item_tx",
        "grant_mileage_tx",
        "arena_rewards",
        "crate::ledger",
        "crate::progression",
        "crate::subplot",
        "crate::arena",
    ] {
        assert!(
            !src.contains(banned),
            "🔴 if 线推进出现结算/发资产路径 `{banned}`：终局产物只能是内容，不能是资产（§0.1）"
        );
    }
    // 世界线写入同样一条不许有。
    for banned in [
        "INSERT INTO worlds",
        "UPDATE worlds",
        "INSERT INTO world_ticks",
        "UPDATE world_ticks",
        "INSERT INTO world_events",
        "INSERT INTO world_members",
        "UPDATE world_members",
        "UPDATE cloud_characters",
        "INSERT INTO world_contributions",
        "INSERT INTO consent_requests",
        "INSERT INTO subplot_cards",
        "INSERT INTO backpacks",
    ] {
        assert!(
            !src.contains(banned),
            "🔴 if 线推进出现世界线写入 `{banned}`：本模块对世界线表只有 SELECT（§0.3）"
        );
    }
    // 反向确认：推进确实写的是自己那两张表（否则上面全是"没写任何东西"的空断言）。
    assert!(src.contains("INSERT INTO ifline_beats"), "推进必须落 ifline_beats");
    assert!(src.contains("UPDATE ifline_worlds SET live_state_json"), "推进必须 CAS 活态");
}

/// 🔴 **确定性**：同种子同拍号 → 同演员表；换种子或换拍号 → 结果随之改变（种子真的在起作用）。
///
/// 抽样入参是**排好序的切片**而不是 map：确定性契约禁止用 map 迭代序驱动 RNG。
#[test]
fn cast_selection_is_deterministic_and_seed_sensitive() {
    let pool: Vec<String> =
        ["npc_a", "npc_b", "npc_c", "npc_d", "npc_e", "npc_f"].iter().map(|s| s.to_string()).collect();

    let draw = |seed: u64, beat: i64| {
        let (mut rng, _) = runner::beat_stream(seed, beat);
        runner::select_cast(&pool, 3, &mut rng)
    };

    // ① 同种子同拍号 → 逐字节相同（可复算）。
    assert_eq!(draw(0xDEAD_BEEF, 0), draw(0xDEAD_BEEF, 0), "🔴 同分叉态 + 同种子 + 同拍号必须复现");
    assert_eq!(draw(0xDEAD_BEEF, 7), draw(0xDEAD_BEEF, 7));

    // ② 结果恒为升序且无重复（返回值只取决于抽中了谁，与抽中次序无关）。
    let d = draw(0xDEAD_BEEF, 0);
    assert_eq!(d.len(), 3);
    let mut sorted = d.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(d, sorted, "演员表必须升序且不重复");

    // ③ 种子/拍号确实在起作用（否则「确定性」退化成「恒定」，那是把 RNG 写死了）。
    let across_beats: BTreeSet<Vec<String>> = (0..8).map(|b| draw(0xDEAD_BEEF, b)).collect();
    assert!(across_beats.len() > 1, "不同拍号必须能抽出不同演员表，否则后面的 NPC 永无戏份");
    let across_seeds: BTreeSet<Vec<String>> =
        [1u64, 2, 3, 4, 5, 6, 7, 8].iter().map(|s| draw(*s, 0)).collect();
    assert!(across_seeds.len() > 1, "不同 run_seed 必须能抽出不同演员表");

    // ④ 候选不足时全取（不 panic、不补空位）。
    let two = vec!["x".to_string(), "y".to_string()];
    let (mut rng, _) = runner::beat_stream(1, 0);
    assert_eq!(runner::select_cast(&two, 5, &mut rng), two);
    let (mut rng, _) = runner::beat_stream(1, 0);
    assert!(runner::select_cast(&[], 3, &mut rng).is_empty());
}

/// 🔴 **种子从不可变身份要素派生，且一旦钉住就不再改**。
#[tokio::test]
async fn run_seed_is_pinned_on_first_advance_and_never_changes() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let row = fetch_ifline(&state.db, &id).await.unwrap().unwrap();
    assert_eq!(row.run_seed, "", "开局时尚未推进，种子应为空");
    let expected = format!("{:016x}", runner::derive_run_seed(&row));

    let model = ScriptedModel::new();
    advance(&state, &id, &model).await.expect("推进成功");
    let seed1: String = sqlx::query_scalar("SELECT run_seed FROM ifline_worlds WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(seed1, expected, "种子必须由不可变身份要素确定性派生");

    advance(&state, &id, &model).await.expect("再推一拍");
    let seed2: String = sqlx::query_scalar("SELECT run_seed FROM ifline_worlds WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(seed2, seed1, "🔴 种子一旦钉住就是这条线身份的一部分，不许在后续推进中漂移");
}

// ---------------------------------------------------------------------------
// §14 单人平行线
// ---------------------------------------------------------------------------

/// 🔴 **他人玩家角色永远上不了 if 线的场**（§14 社交防火墙，纵深防御）。
///
/// 本用例的装配表里**故意混进了他人玩家角色 `c2`**（见 `seed_assembled`）：
/// 即便装配层把玩家写了进去，推进也必须把它剔除。
/// 未经同意把别人的角色拖进你的 if 线、让它做原主人从没做过的事，
/// 等于以他人角色之名生成他人未授权的言行——比暴露真人身份更难挽回，且不可撤销。
#[tokio::test]
async fn red_line_foreign_players_never_enter_beat_cast() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let model = ScriptedModel::new();
    let out = advance(&state, &id, &model).await.expect("推进成功");

    assert!(out.cast.contains(&"c1".to_string()), "主角必须在场");
    assert!(out.cast.contains(&"npc_shen".to_string()), "NPC 保留——NPC 是世界的，不是谁的");
    assert!(
        !out.cast.contains(&"c2".to_string()),
        "🔴 他人玩家角色 c2 绝不能上场（§14），哪怕装配表里写了它"
    );

    // 钉住的阵容里也不能留着他（否则下一拍又会被抽中）。
    let cast_json: String = sqlx::query_scalar("SELECT cast_json FROM ifline_worlds WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(!cast_json.contains("\"c2\""), "🔴 钉住的阵容里不许留他人玩家角色：{cast_json}");

    // 逐拍台账可审。
    let stored: String = sqlx::query_scalar("SELECT cast_json FROM ifline_beats WHERE ifline_id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(!stored.contains("\"c2\""), "逐拍台账同样不许出现他人玩家角色");

    // 活态里也不能因为跑了一拍就把 c2 带回来。
    let live: String = sqlx::query_scalar("SELECT live_state_json FROM ifline_worlds WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    let live: Value = serde_json::from_str(&live).unwrap();
    assert!(
        live["characters"].get("c2").is_none(),
        "🔴 §14 纵深防御第 2 层：他人角色即便被塞进状态也活不过一拍"
    );
}

// ---------------------------------------------------------------------------
// 推进语义
// ---------------------------------------------------------------------------

/// 推进一拍：生命周期 sealed → running，落一行正文，进度可读。
#[tokio::test]
async fn advancing_moves_lifecycle_and_produces_readable_prose() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let model = ScriptedModel::new();

    let out = advance(&state, &id, &model).await.expect("推进成功");
    assert_eq!(out.beat_no, 0);
    assert_eq!(out.status, "done");
    assert_eq!(out.ifline_status, "running", "跑过一拍即 running（不再是 sealed）");
    assert!(out.cost_tokens > 0, "跑了模型就必须记成本");

    let tk = token(&state, "u1");
    let (st, body) = send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk, None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "running");
    assert_eq!(body["progress"]["beatCount"], 1);
    assert_eq!(body["progress"]["advanceable"], true);
    assert_eq!(body["ending"], Value::Null, "还没收尾就不该有结局");
    // 层次标签在推进后依然恒带（读取面不可冒充世界线）。
    assert_eq!(body["layer"], "ifline");
    assert_eq!(body["isWorldFact"], false);
    assert_eq!(body["affectsOriginWorld"], false);
}

/// 🔴 **冻结的分叉态永不被推进覆盖**：它是分叉点证据，覆盖了 `stateFidelity` 就无从核验。
#[tokio::test]
async fn snapshot_stays_frozen_while_live_state_advances() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let snap_before: String =
        sqlx::query_scalar("SELECT snapshot_json FROM ifline_worlds WHERE id = $1")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();

    let model = ScriptedModel::new();
    advance(&state, &id, &model).await.expect("推进成功");
    advance(&state, &id, &model).await.expect("再推一拍");

    let (snap_after, live, rev): (String, String, i64) = sqlx::query_as(
        "SELECT snapshot_json, live_state_json, live_revision FROM ifline_worlds WHERE id = $1",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(
        snap_after, snap_before,
        "🔴 冻结的分叉态必须逐字节不变——它是「这条线确实从那一拍岔出去」的唯一证据"
    );
    assert_ne!(live, "", "活态必须另存并随推进变化");
    assert!(rev >= 2, "活态修订号必须随每一拍推进（CAS 令牌），实际 {rev}");
}

/// 🔴 **一拍只跑一次**：`(ifline_id, beat_no)` 唯一键是推进的并发闸。
///
/// 这条守的是钱：若用「先查 beat_count 再写」，两个并发请求会读到同一个计数、
/// 各跑各的模型、各花各的 token，最后各写各的状态互相覆盖。
#[tokio::test]
async fn second_advance_on_same_beat_is_rejected_without_burning_tokens() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let model = ScriptedModel::new();

    // 手工占掉第 0 拍（模拟"另一个请求已经抢到了这一拍"）。
    sqlx::query(
        "INSERT INTO ifline_beats (id, ifline_id, beat_no, status, base_revision, created_at) \
         VALUES ($1, $2, 0, 'running', 0, $3)",
    )
    .bind(new_id("ifb"))
    .bind(&id)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();

    let err = advance(&state, &id, &model).await.expect_err("同一拍不许跑第二次");
    assert!(matches!(err, ApiError::Conflict(_)), "抢占失败必须是 409，实际 {err:?}");
    assert_eq!(model.call_count(), 0, "🔴 抢占失败必须在调模型之前——一个 token 都不许花");
}

/// 拍数上限（**成本闸**）：到顶强制收尾，且**不跑模型、不花 token**。
///
/// 一张副本卡换一条 if 线，若推进无上限则单条 if 线的算力开销无界——那是个业务窟窿。
/// 到顶不是报错而是收尾：玩家拿到的是一个完整的（哪怕仓促的）结局，不是一条断掉的线。
#[tokio::test]
async fn beat_cap_forces_ending_without_calling_the_model() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    // 直接把计数顶到上限（不改 env：env 是进程级的，会串味到并发跑的其它用例）。
    sqlx::query("UPDATE ifline_worlds SET beat_count = $1, status = 'running' WHERE id = $2")
        .bind(runner::ifline_max_beats())
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();

    let model = ScriptedModel::new();
    let out = advance(&state, &id, &model).await.expect("到顶应收尾而不是报错");
    assert_eq!(out.ifline_status, "ended");
    assert_eq!(out.ending_reason, "beat_cap");
    assert_eq!(out.cost_tokens, 0, "强制收尾不该花钱");
    assert_eq!(model.call_count(), 0, "🔴 到顶后不许再调模型");
    assert_eq!(
        count_rows(&state.db, "ifline_beats").await,
        0,
        "强制收尾不产生新的一拍（没跑就没有那一拍）"
    );

    // 收尾之后不可再推进。
    let err = advance(&state, &id, &model).await.expect_err("已收尾不可再推进");
    assert!(matches!(err, ApiError::Conflict(_)), "实际 {err:?}");
}

/// 🔴 成本落两处且对得上：逐拍明细 + 实例累计。
#[tokio::test]
async fn cost_is_recorded_per_beat_and_rolled_up() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let model = ScriptedModel::new();
    advance(&state, &id, &model).await.expect("推进成功");
    advance(&state, &id, &model).await.expect("再推一拍");

    let (sum, n): (i64, i64) = sqlx::query_as(
        "SELECT CAST(COALESCE(SUM(cost_tokens), 0) AS BIGINT), CAST(COUNT(*) AS BIGINT) \
         FROM ifline_beats WHERE ifline_id = $1",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(n, 2);
    assert!(sum > 0, "跑了模型就必须记 token 成本，否则付费功能的成本会系统性失真");

    let total: i64 =
        sqlx::query_scalar("SELECT cost_tokens_total FROM ifline_worlds WHERE id = $1")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(total, sum, "实例累计必须与逐拍明细对得上（两处互为对账）");

    // 运营成本读数看得到它，并**明说主看板还没接**（不静默漏掉）。
    let admin = admin_token(&state, "operator");
    let (st, body) = send(&state, "GET", "/api/admin/iflines/cost", &admin, None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["costTokens"], sum);
    assert_eq!(body["beats"], 2);
    // 主看板已并入（`cost.ifline` + `cost.combined`）。这个自述字段的价值不在它此刻是 true 还是
    // false，而在于**响应自己说得出实情**——接线状态若哪天回退，这里必须跟着变，
    // 不能让「主看板包含 if 线开销」变成一句只活在某个人记忆里的话。
    assert_eq!(
        body["dashboardIntegration"]["mainDashboardIncludesIfline"], true,
        "🔴 主看板已并入 if 线开销，响应必须如实反映"
    );
    assert!(
        body["dashboardIntegration"]["where"].as_str().unwrap().contains("cost.ifline"),
        "自述里要指出并到了哪个字段，否则运营仍然不知道去哪看"
    );
}

/// if 线的拍**绝不进世界线 SLO**：它不落 `world_ticks`，而 SLO 判定「一拍真的跑了」的口径
/// 恰是 `world_ticks.status='done' AND cost_tokens > 0`。
///
/// 这条断言把「默认排除」变成「有意的决定」——单人线的基尼恒为满分，混进去会稀释多人不公平，
/// 让指标失去报警能力（详见 `runner.rs` 模块头的归属表）。
#[tokio::test]
async fn ifline_beats_never_enter_worldline_slo_input() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let ticks_before = count_rows(&state.db, "world_ticks").await;
    let model = ScriptedModel::new();
    advance(&state, &id, &model).await.expect("推进成功");

    assert_eq!(
        count_rows(&state.db, "world_ticks").await,
        ticks_before,
        "🔴 if 线的拍不许落 world_ticks —— 那既是结算管线的入口，也是世界线 SLO 的取数口径"
    );
    assert!(count_rows(&state.db, "ifline_beats").await > 0, "它落的是自己那张表");
    // critic 数据留着（供将来的**独立** if 线质量读数），但不并入世界线指标。
    let critic: String = sqlx::query_scalar("SELECT critic_json FROM ifline_beats WHERE ifline_id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(critic != "{}" && !critic.is_empty(), "逐拍 critic 必须留档：{critic}");
}

// ---------------------------------------------------------------------------
// 开关与信息边界（推进端点与既有端点同口径）
// ---------------------------------------------------------------------------

/// 🔴 开关关闭时推进端点 **404 且零副作用**（不是 403：不向外泄露「平台有这个未开放功能」）。
#[tokio::test]
async fn advance_endpoints_are_404_when_flag_off() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    // 关阀（删掉开闸记录）。
    sqlx::query("DELETE FROM runtime_flags WHERE flag = $1")
        .bind(ENV_IFLINE_PARALLEL)
        .execute(&state.db)
        .await
        .unwrap();
    crate::flags::invalidate(&state.db);

    let tk = token(&state, "u1");
    for (method, uri) in [
        ("POST", format!("/api/me/iflines/{id}/beats")),
        ("GET", format!("/api/me/iflines/{id}/beats")),
    ] {
        let (st, _) = send(&state, method, &uri, &tk, None, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{method} {uri} 关闭时必须 404");
    }
    assert_eq!(count_rows(&state.db, "ifline_beats").await, 0, "关闭时一拍都不许落库");
    let (beats, cost): (i64, i64) =
        sqlx::query_as("SELECT beat_count, cost_tokens_total FROM ifline_worlds WHERE id = $1")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!((beats, cost), (0, 0), "关闭时零副作用");
}

/// 别人的 if 线一律 404（不是 403）：不确认「这条 if 线存在」这件事本身（信息边界，§14）。
#[tokio::test]
async fn others_iflines_cannot_be_advanced_or_read() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let intruder = token(&state, "u2");
    for (method, uri) in [
        ("POST", format!("/api/me/iflines/{id}/beats")),
        ("GET", format!("/api/me/iflines/{id}/beats")),
    ] {
        let (st, _) = send(&state, method, &uri, &intruder, None, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{method} {uri} 越权必须 404 而不是 403");
    }
    assert_eq!(count_rows(&state.db, "ifline_beats").await, 0, "越权尝试不许留下任何一拍");
}

/// 参数化（§0.2 禁写死）：拍数上限可调、有界、非法值回落默认。
#[test]
fn progression_params_are_parameterised_with_bounds() {
    let n = runner::ifline_max_beats();
    assert!(n >= 1 && n <= 60, "拍数上限必须落在有界区间内，实际 {n}");
}

/// 🔴 **抢占之后失败必须推进拍数，否则这条付费的线永久卡死**。
///
/// `beat_no` 由唯一键占住，若失败时不推进 `beat_count`，玩家每次重推都算出同一个号、
/// 每次都撞 409——而他已经烧掉了副本卡。这是「花了钱、东西不能用」的那类故障，
/// 比推进失败本身严重得多，故单开一条用例守它。
#[tokio::test]
async fn failed_beat_still_advances_the_counter_so_the_line_never_deadlocks() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    // 把活态弄成引擎无法解析的东西 → 抢占之后失败（这一步刻意选在抢占之后的路径上）。
    sqlx::query("UPDATE ifline_worlds SET live_state_json = $1, status = 'running' WHERE id = $2")
        .bind(r#"{"characters":"这不是一个对象"}"#)
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();

    let model = ScriptedModel::new();
    let err = advance(&state, &id, &model).await.expect_err("坏状态必须推不动");
    assert!(matches!(err, ApiError::Internal(_)), "实际 {err:?}");

    // 这一拍标 failed，且**拍数已推进**。
    let (status, note): (String, Option<String>) =
        sqlx::query_as("SELECT status, note FROM ifline_beats WHERE ifline_id = $1 AND beat_no = 0")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(status, "failed", "note={note:?}");
    let beats: i64 = sqlx::query_scalar("SELECT beat_count FROM ifline_worlds WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(beats, 1, "🔴 失败拍也必须推进拍数，否则玩家永远撞在同一个被占掉的 beat_no 上");

    // 🔴 关键断言：修好活态后**下一次推进能真的走通**（不是又一个 409）。
    sqlx::query("UPDATE ifline_worlds SET live_state_json = '' WHERE id = $1")
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();
    let out = advance(&state, &id, &model).await.expect("下一拍必须推得动，否则这条线就是死的");
    assert_eq!(out.beat_no, 1, "重推应拿到下一拍，不是撞在第 0 拍上");
    assert_eq!(out.status, "done");

    // 失败拍不计成本（没有成功的尝试可计），也不推进活态修订号。
    let cost: i64 = sqlx::query_scalar(
        "SELECT cost_tokens FROM ifline_beats WHERE ifline_id = $1 AND beat_no = 0",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(cost, 0, "失败拍不该计成本");
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 异步推进（migration 0050）：契约从「同步回这一拍」翻成「202 + 轮询」
//
// 这一节存在的直接原因：改契约时发现**端点这一层几乎没有覆盖**——既有用例全是直接调
// `runner::advance_one_beat`，于是把同步契约整个翻掉，一条用例都没红。
// 下面这几条钉的正是异步化**新引入**的那些失败形态，而不是重复验证 runner 已经验过的东西。
// ═══════════════════════════════════════════════════════════════════════════

/// 直接读库里那三列（0050），不经读取面——要验的是落库本身。
async fn advance_latch(state: &AppState, id: &str) -> (i64, String, i64) {
    let r = sqlx::query(
        "SELECT advance_requested_at, last_error, last_error_at FROM ifline_worlds WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    (
        r.try_get("advance_requested_at").unwrap(),
        r.try_get("last_error").unwrap(),
        r.try_get("last_error_at").unwrap(),
    )
}

/// 🔴 POST 回 **202「已受理」**而不是「已推进」，且请求内**一次模型都不调**。
#[tokio::test]
async fn advance_returns_202_and_never_calls_the_model_in_request() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let tk = token(&state, "u1");

    let (st, body) =
        send(&state, "POST", &format!("/api/me/iflines/{id}/beats"), &tk, Some(json!({})), None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "🔴 契约是「已受理」，不是「已推进」: {body}");
    assert_eq!(body["accepted"], true);
    assert_eq!(body["advancePending"], true);
    assert_eq!(body["expectedBeatNo"], 0);
    assert!(body.get("beat").is_none(), "202 不该回一拍的内容 —— 它还没跑");

    // 在飞标记已落库；玩家端从读取面也看得见。
    let (at, err, _) = advance_latch(&state, &id).await;
    assert!(at > 0, "🔴 请求层闸必须落库，否则重复点击挡不住");
    assert!(err.is_empty());
    let (_st, v) = send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk, None, None).await;
    assert_eq!(v["advance"]["pending"], true, "{v}");
    assert!(v["advance"]["lastError"].is_null());

    // 队列里确实有那份任务（否则就是「受理了但没人干」）。
    let job: super::AdvanceJob = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        crate::queue::pop_json(&*state.queue, super::ADVANCE_TOPIC),
    )
    .await
    .expect("应当已入队")
    .expect("载荷可解析");
    assert_eq!(job.ifline_id, id);
}

/// 🔴 **重复点击被请求层挡下**。异步之前这件事由 `(ifline_id, beat_no)` 唯一键天然承担
/// （请求还没回来，第二次点击必撞）；异步之后第二次点击会在第一份任务还没被取走时就入队，
/// 排两份、**烧两遍**。故必须在请求这一层再加一道闸。
#[tokio::test]
async fn a_second_click_is_rejected_while_one_advance_is_in_flight() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let tk = token(&state, "u1");
    let path = format!("/api/me/iflines/{id}/beats");

    let (st, _) = send(&state, "POST", &path, &tk, Some(json!({})), None).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let (st, body) = send(&state, "POST", &path, &tk, Some(json!({})), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "🔴 在飞期间不得再受理: {body}");
    assert!(
        body["error"]["message"].as_str().unwrap_or("").contains("不会让你少推一拍"),
        "拒绝文案要说清这只挡重复点击，别让玩家以为丢了一拍: {body}"
    );

    // 队列里**只有一份**。
    assert!(pop_advance(&state).await.is_some());
    assert!(pop_advance(&state).await.is_none(), "🔴 第二次点击不得入队 —— 那是白烧一次模型调用");
}

async fn pop_advance(state: &AppState) -> Option<super::AdvanceJob> {
    tokio::time::timeout(
        std::time::Duration::from_millis(150),
        crate::queue::pop_json(&*state.queue, super::ADVANCE_TOPIC),
    )
    .await
    .ok()
    .flatten()
}

/// 🔴 **失败原因必须落库**——异步化最容易制造的静默失败就在这里。
///
/// 同步版本里「原世界模型路由未配置」是当场返回给玩家的那句话；改成异步后它发生在 worker 里，
/// 不落库就彻底消失：玩家点一下、什么都没发生、也没有任何解释。
/// 本用例走 dev 态（没有模型路由），让 worker 真的失败一次。
#[tokio::test]
async fn worker_failure_is_recorded_not_swallowed() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let tk = token(&state, "u1");
    // 制造一个**与同步版本逐字相同**的拒绝理由：原世界钉住的模型路由不可解析。
    sqlx::query("DELETE FROM model_routes").execute(&state.db).await.unwrap();
    send(&state, "POST", &format!("/api/me/iflines/{id}/beats"), &tk, Some(json!({})), None).await;

    super::run_advance_job(&state, &id, None).await;

    let (at, err, err_at) = advance_latch(&state, &id).await;
    assert_eq!(at, 0, "🔴 任何出口都必须清掉在飞标记，否则玩家要等到陈旧线才能再点");
    assert!(!err.is_empty(), "🔴 失败原因必须留下来，否则就是静默失败");
    assert!(err.contains("模型路由"), "留下的应当是**给玩家看的那句话**，不是内部码: {err}");
    assert!(err_at > 0);

    // 读取面下发它。
    let (_st, v) = send(&state, "GET", &format!("/api/me/iflines/{id}"), &tk, None, None).await;
    assert_eq!(v["advance"]["pending"], false);
    assert!(v["advance"]["lastError"].as_str().unwrap_or("").contains("模型路由"), "{v}");

    // 清掉标记之后可以再点（失败不该把这条付费的线锁死）。
    let (st, _) = send(&state, "POST", &format!("/api/me/iflines/{id}/beats"), &tk, Some(json!({})), None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "🔴 上一次失败不得挡住下一次尝试");
}

/// 🔴 **成功之后必须清掉在飞标记**。忘了清，玩家要等到陈旧线（默认 10 分钟）才能推下一拍——
/// 对付费内容来说那已经是事故。
///
/// ⚠️ 这条是**故障注入补出来的**：只覆盖失败路径时，「成功后不清标记」这个注入从所有用例底下
/// 溜了过去。走成功路径需要注入模型，故 `run_advance_job` 沿用 `advance_one_beat` 既有的
/// `model_override` 位（生产恒传 `None`），而不是新造一条 test-only 通路。
#[tokio::test]
async fn a_successful_advance_clears_the_in_flight_latch() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let tk = token(&state, "u1");
    send(&state, "POST", &format!("/api/me/iflines/{id}/beats"), &tk, Some(json!({})), None).await;
    let (at, _, _) = advance_latch(&state, &id).await;
    assert!(at > 0, "前提：受理后标记是立着的");

    // 🔴 先造一条**上一次的失败原因**：不这么做，下面「成功要清空它」那条断言恒真——
    // 故障注入实测过，「清了标记但没清错误」的写法能从它底下溜过去。
    sqlx::query("UPDATE ifline_worlds SET last_error = $1, last_error_at = $2 WHERE id = $3")
        .bind("上一次的旧错误")
        .bind(crate::db::now_ms() - 60_000)
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();

    let model = ScriptedModel::new();
    let mc: Arc<dyn TestModelClient> = model.clone();
    super::run_advance_job(&state, &id, Some(mc)).await;

    let (at, err, err_at) = advance_latch(&state, &id).await;
    assert_eq!(at, 0, "🔴 成功之后必须清掉在飞标记，否则玩家 10 分钟内点不动下一拍");
    assert!(
        err.is_empty(),
        "🔴 成功还必须清掉上一次的失败原因，否则一条早已解决的旧错误会一直挂在读取面上，\
         玩家/运营会照着它去查一个不存在的问题。实得：{err}"
    );
    assert_eq!(err_at, 0, "🔴 时刻也要一起清");

    // 真的推进了一拍，且可以立刻点下一拍。
    let (_st, beats) = send(&state, "GET", &format!("/api/me/iflines/{id}/beats"), &tk, None, None).await;
    assert_eq!(beats["items"].as_array().map(|a| a.len()).unwrap_or(0), 1, "{beats}");
    let (st, _) = send(&state, "POST", &format!("/api/me/iflines/{id}/beats"), &tk, Some(json!({})), None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "🔴 上一拍落定后应当能立刻推下一拍");
}

/// 🔴 **陈旧线兜住「队列把任务弄丢了」**——这是异步化引入的、比原问题更糟的死锁风险。
///
/// `MemQueue` 不持久：进程重启会带走在飞的任务，而 `advance_requested_at` 已经写下。
/// 若 CAS 只认 `= 0`，这条**付费**的 if 线就永久推不动，而玩家已经烧掉了副本卡。
#[tokio::test]
async fn a_stale_in_flight_latch_does_not_lock_the_paid_line_forever() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let tk = token(&state, "u1");
    let path = format!("/api/me/iflines/{id}/beats");

    // 模拟「入队后进程重启，任务丢了」：标记留在库里，队列里什么都没有。
    let long_ago = crate::db::now_ms() - super::advance_stale_ms() - 1_000;
    sqlx::query("UPDATE ifline_worlds SET advance_requested_at = $1 WHERE id = $2")
        .bind(long_ago)
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();

    let (st, body) = send(&state, "POST", &path, &tk, Some(json!({})), None).await;
    assert_eq!(
        st,
        StatusCode::ACCEPTED,
        "🔴 陈旧的在飞标记必须能被重新抢占 —— 否则丢一次任务 = 这条付费的线永久报废: {body}"
    );

    // 而**没到**陈旧线的标记仍然挡得住（这一支不能因为上面那支而失效）。
    let recent = crate::db::now_ms() - 1_000;
    sqlx::query("UPDATE ifline_worlds SET advance_requested_at = $1 WHERE id = $2")
        .bind(recent)
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();
    let (st, _) = send(&state, "POST", &path, &tk, Some(json!({})), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "🔴 新鲜的在飞标记必须照常挡住重复点击");
}

/// 🔴 「玩家拉动」这条设计**没有因为异步化而改变**：没有任何调度器会去碰 `ifline_worlds`。
/// 源码级断言——它一旦被改成「调度器扫 ifline」，付费内容就会在玩家没看的时候自己烧完。
#[test]
fn red_line_no_scheduler_ever_touches_iflines() {
    let runtime_src = include_str!("../runtime/mod.rs");
    for forbidden in ["ifline_worlds", "ifline_beats", "ifline::spawn_workers"] {
        assert!(
            !runtime_src.contains(forbidden),
            "🔴 runtime（世界调度器）出现了「{forbidden}」——if 线必须由玩家一拍一拍拉动，\
             不是被调度器推动。付费内容的消耗节奏必须由付费的人掌握。"
        );
    }
    // 入队点是**可数的、且每一处都要说得出理由**——多一处就可能是「有人替玩家决定推进」。
    //
    // 端点（玩家点击触发）：1 处。
    let ifline_src = include_str!("mod.rs");
    assert_eq!(
        ifline_src.matches("push_json(").count(),
        1,
        "🔴 `ifline/mod.rs` 的入队点必须只有端点那一处"
    );
    // 对账补投（0052）：1 处，在 `sweep.rs`。
    //
    // 🔴 它**不违反**「玩家拉动」，理由必须站得住，否则这道红线就白开了口子：
    // 它的判据恒为 `advance_requested_at > 0`——那一列**只由玩家点击写下**（端点的 CAS）。
    // 也就是说 sweep 补投的永远是「玩家已经点过、但那次任务丢了」的那一拍，
    // 不会凭空发起任何一拍。故下面两条一起钉：入队只有一处，且该处的取数判据带着这个条件。
    let sweep_src = include_str!("sweep.rs");
    assert_eq!(
        sweep_src.matches("push_json(").count(),
        1,
        "🔴 对账补投的入队点必须只有一处"
    );
    assert!(
        sweep_src.contains("advance_requested_at > 0"),
        "🔴 补投的取数判据必须钉着「玩家已经点过」（`advance_requested_at > 0`）——\
         去掉这个条件，这条循环立刻变成一个替玩家烧副本卡的调度器"
    );
}

// ============================================================================
// 对账式补偿（0052 · `ifline::sweep`）
// ============================================================================
//
// 补的是 0050 落地时**自己登记**的那条遗留：陈旧线只让玩家「能再点一次」，
// 不把丢掉的那次补上。下面这组钉的是补投的判据、封顶、以及封顶时不静默。

/// 进程级 env 锁 + 原值恢复（范式同 `SubplotSwitch` / `ContainerSwitch`）。
struct SweepEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Vec<(&'static str, Option<String>)>,
}

impl SweepEnv {
    fn set(pairs: &[(&'static str, &str)]) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let keys = [
            "MUSE_IFLINE_SWEEP_AFTER_MS",
            "MUSE_IFLINE_SWEEP_MAX_REDELIVERIES",
            "MUSE_IFLINE_ADVANCE_STALE_MS",
        ];
        let prev = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        for (k, v) in pairs {
            std::env::set_var(k, v);
        }
        Self { _guard: guard, prev }
    }
}

impl Drop for SweepEnv {
    fn drop(&mut self) {
        for (k, v) in &self.prev {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// 把在飞标记设成「久到已过补投窗口」。
async fn strand(state: &AppState, id: &str, sweep_count: i64) -> i64 {
    let long_ago = now_ms() - super::sweep::sweep_after_ms() - 1_000;
    sqlx::query(
        "UPDATE ifline_worlds SET advance_requested_at = $1, advance_sweep_count = $2 WHERE id = $3",
    )
    .bind(long_ago)
    .bind(sweep_count)
    .bind(id)
    .execute(&state.db)
    .await
    .unwrap();
    long_ago
}

async fn ifline_row(state: &AppState, id: &str) -> (i64, i64, String) {
    let r = sqlx::query(
        "SELECT advance_requested_at, advance_sweep_count, last_error FROM ifline_worlds WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    (
        r.try_get("advance_requested_at").unwrap(),
        r.try_get("advance_sweep_count").unwrap(),
        r.try_get("last_error").unwrap(),
    )
}

/// 队列里现在有没有这条线的推进任务（取走一份）。
///
/// ⚠️ **必须带超时**：`MemQueue::pop` 在空队列上是**无限等待**的（它是给 worker 循环用的）。
/// 直接 `pop_json().await` 去断言「没有入队」会把用例挂死——这里就先挂过一次。
async fn popped_advance(state: &AppState) -> Option<String> {
    tokio::time::timeout(
        std::time::Duration::from_millis(300),
        crate::queue::pop_json::<super::AdvanceJob>(&*state.queue, super::ADVANCE_TOPIC),
    )
    .await
    .ok()
    .flatten()
    .map(|j| j.ifline_id)
}

/// 丢掉的任务被补投回队列，且计数 +1、在飞窗口重新计时。
#[tokio::test]
async fn sweep_redelivers_a_lost_in_flight_task() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    strand(&state, &id, 0).await;

    let now = now_ms();
    assert_eq!(super::sweep::sweep_once(&state, now).await, 1, "陈旧的在飞标记应被补投");
    assert_eq!(popped_advance(&state).await.as_deref(), Some(id.as_str()), "任务必须真的进队列");

    let (req_at, count, _) = ifline_row(&state, &id).await;
    assert_eq!(req_at, now, "补投后在飞窗口重新计时，否则下一轮对账会立刻再投一次");
    assert_eq!(count, 1, "补投次数必须记账——不记账就没法封顶");
}

/// 🔴 **不是调度器**：玩家没点过的线，无论多久都不补投。
///
/// `advance_requested_at` 只由端点的 CAS 写下。这一条与源码级红线
/// `red_line_no_scheduler_ever_touches_iflines` 是同一件事的两个面：那边钉判据文本，
/// 这边钉真实行为。少了它，补投循环就会变成一个替玩家烧副本卡的调度器。
#[tokio::test]
async fn sweep_never_touches_a_line_the_player_never_clicked() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    // 从未点过（标记为 0），且这条线本身建得很久了。
    sqlx::query("UPDATE ifline_worlds SET advance_requested_at = 0, created_at = 1 WHERE id = $1")
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();

    assert_eq!(super::sweep::sweep_once(&state, now_ms()).await, 0, "🔴 玩家没点过就绝不推进");
    assert!(popped_advance(&state).await.is_none(), "🔴 队列里不该出现任何任务");
}

/// 还在补投窗口内的在飞标记不动它——那可能是一个仍在跑的任务，补投会白烧一次模型调用。
#[tokio::test]
async fn sweep_leaves_a_fresh_in_flight_task_alone() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    sqlx::query("UPDATE ifline_worlds SET advance_requested_at = $1 WHERE id = $2")
        .bind(now_ms() - 1_000)
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();

    assert_eq!(super::sweep::sweep_once(&state, now_ms()).await, 0);
    assert!(popped_advance(&state).await.is_none());
}

/// 🔴 补投到顶后**停手，但不静默**：清标记（玩家立刻能再点）+ 把原因写进 `last_error`。
///
/// 没有封顶的话，一个每次都在清标记前就死掉的 worker 会把这条循环变成无限烧钱；
/// 而封顶后若静默放弃，「补偿机制」就变成了一个更难查的静默失败。
#[tokio::test]
async fn sweep_stops_at_the_cap_and_says_why() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    let _env = SweepEnv::set(&[("MUSE_IFLINE_SWEEP_MAX_REDELIVERIES", "2")]);
    strand(&state, &id, 2).await; // 已达上限

    assert_eq!(super::sweep::sweep_once(&state, now_ms()).await, 0, "到顶后不得再投");
    assert!(popped_advance(&state).await.is_none(), "到顶后队列里不该有任务");

    let (req_at, count, err) = ifline_row(&state, &id).await;
    assert_eq!(req_at, 0, "🔴 必须清掉在飞标记——否则玩家还得再等一个陈旧线才能自己重点");
    assert_eq!(count, 0, "链已终结，计数归零");
    assert!(err.contains("上限"), "🔴 停手的理由必须留在读取面上，不能静默: {err}");
}

/// 一次推进落定（成功或失败）即清零补投计数——否则一条 if 线**一生**只能被补投 N 次。
#[tokio::test]
async fn finishing_an_advance_resets_the_redelivery_counter() {
    let state = test_state().await;
    let id = seed_open_ifline(&state).await;
    strand(&state, &id, 2).await;

    super::finish_advance(&state.db, &id, Some("这次失败了")).await;
    let (req_at, count, err) = ifline_row(&state, &id).await;
    assert_eq!(req_at, 0);
    assert_eq!(count, 0, "🔴 落定即终结这条尝试链，下一次点击是全新的一条");
    assert_eq!(err, "这次失败了", "失败原因照常落库（0050 的约定不变）");
}

/// 🔴 补投窗口恒 ≥ 请求层陈旧线 + 1 分钟，**由代码保证**而不是靠运营记得。
/// 配小了会对仍在跑的任务补投——唯一键挡得住第二次落库，但那次模型调用是真烧掉的。
#[test]
fn sweep_window_can_never_fire_before_the_request_stale_line() {
    let _env = SweepEnv::set(&[
        ("MUSE_IFLINE_SWEEP_AFTER_MS", "1"), // 运营把它配成 1 毫秒
        ("MUSE_IFLINE_ADVANCE_STALE_MS", "600000"),
    ]);
    assert!(
        super::sweep::sweep_after_ms() >= super::advance_stale_ms() + 60_000,
        "🔴 补投窗口被配小到早于陈旧线了：{} < {}",
        super::sweep::sweep_after_ms(),
        super::advance_stale_ms() + 60_000
    );
}

/// §0.1：默认关闭。补投是这条链上唯一「凭数据自发调模型」的路径，且 if 线是付费内容。
#[tokio::test]
async fn sweep_defaults_to_off() {
    let state = test_state().await;
    assert!(!super::sweep::sweep_enabled(&state.db).await, "🔴 补投必须默认关闭（§0.1）");
}
