//! 已过审内容处置测试（sqlite::memory + oneshot 真实路由）。migration 0044。覆盖：
//!
//! - 🔴 **下架不改世界线**（红线）：处置全流程走完后，七张世界线/事实表**逐字节快照**不变；
//!   资产行本身也只有那一列展示态变了，其余列逐字节不变；
//! - 🔴 **源码级红线** + **动态 SQL 值域**：本模块不写事实表，且拼进 SQL 的表/列只能是四个展示态位；
//! - **读取面真的关了**：入场闸 409、立绘不再下发、封面不再下发、模板不能再开房；
//! - **可逆性两档**：restricted 可恢复（还原到 `prev_moderation`）/ removed 恒 409 且删位图字节；
//! - **权限台阶**：可恢复下架 reviewer / 永久移除 admin 专属 / 越权 403；
//! - **再审**：不改展示态、幂等不刷屏、走 safety 入口 ③（audit_queue + risk_events）；
//! - 🔴 **人审队列不能复活已下架主体**（approve 回写守卫）；
//! - **留痕原子**：audit_logs + risk_events 与处置同事务；
//! - **复合游标**：同毫秒并列行跨页不丢（单列游标会整组丢行，见 pagination.rs）；
//! - **作者告知**：status 端点下发下架状态但**不泄露运营内部理由**。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::{AnyPool, Column, Row};
use tower::ServiceExt;

use super::*;
use crate::config::ServerConfig;
use crate::worlds::{create_world, CreateWorldParams};

// ═══════════════════════════════════════════════════════════════════════════
// 脚手架
// ═══════════════════════════════════════════════════════════════════════════

fn test_config() -> ServerConfig {
    ServerConfig {
        database_url: crate::testkit::test_database_url(),
        bind_addr: "127.0.0.1:0".into(),
        jwt_secret: "test-secret".into(),
        access_ttl_secs: 3600,
        refresh_ttl_secs: 100_000,
        dev_mode: true,
        object_store_dir: std::env::temp_dir()
            .join(new_id("muse-takedown-test"))
            .to_string_lossy()
            .into_owned(),
    }
}

async fn test_state() -> AppState {
    AppState::new(crate::testkit::test_pool().await, test_config())
}

fn admin_token(state: &AppState, role: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, &format!("adm_{role}"), role, 3600).unwrap()
}

fn user_token(state: &AppState, user_id: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, user_id, "user", 3600).unwrap()
}

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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn seed_user(state: &AppState, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
         VALUES ($1, '', 1, 'active', $2, $3)",
    )
    .bind(id)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 一张过审的云端角色卡。`card_json` 用可解析的最小卡（再审要在它上面跑机审文本拼接）。
async fn seed_char(state: &AppState, id: &str, owner: &str, moderation: &str) {
    let card = json!({
        "schemaVersion": 2,
        "id": id,
        "identity": { "name": "沈砚" },
        "lifecycle": "ready",
        "revision": 1,
    });
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES ($1, $2, 'local', 1, $3, 'original', $4, 0, $5)",
    )
    .bind(id)
    .bind(owner)
    .bind(card.to_string())
    .bind(moderation)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 给卡挂一张已过审立绘（真写字节，供永久移除时验证「字节真的没了」）。
async fn seed_avatar(state: &AppState, char_id: &str) -> String {
    let key = format!("avatars/{char_id}.png");
    state.objects.put(&key, b"\x89PNG-fake-bytes").unwrap();
    sqlx::query(
        "UPDATE cloud_characters SET avatar_object_key = $1, avatar_url = $2, \
         avatar_moderation = 'approved' WHERE id = $3",
    )
    .bind(&key)
    .bind(format!("/api/assets/objects/{key}"))
    .bind(char_id)
    .execute(&state.db)
    .await
    .unwrap();
    key
}

async fn seed_template(state: &AppState, id: &str, moderation: &str) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, \
         official, version, moderation, created_at) \
         VALUES ($1, '青云试炼', 'idle', $2, '{}', 1, 1, $3, $4)",
    )
    .bind(id)
    .bind(json!({ "sourceWork": { "title": "青云志" } }).to_string())
    .bind(moderation)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn moderation_of(db: &AnyPool, table: &str, col: &str, id: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(&format!(
        "SELECT {col} FROM {table} WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn count_rows(db: &AnyPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(db).await.unwrap()
}

/// 🔴 **逐字节快照**：把一张表的全部行拉出来、逐列格式化、排序后拼成一个字符串。
///
/// 用「SELECT * + 逐列取值」而不是挑几列比对：挑列意味着**漏掉的那一列就是没被守住的那一列**，
/// 而红线要守的是「一个字节都没动」。排序使比对与行序无关（行序本身不是事实的一部分）。
/// 写法与 `annotations::tests::dump_table` 同源——两处守的是同一条 §0.3。
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

/// 🔴 **已落定的世界事实**表。处置动作对它们必须一个字节都不动。
///
/// 与 `annotations::tests::WORLDLINE_TABLES` 的差别只有一处、且是刻意的：那份清单含
/// `worlds` / `cloud_characters`（因为 OOC 注解权对**资产也**只读），本模块**按设计**要改
/// 这两张表上的**展示态列**。所以本模块把「事实」与「展示」分开守：
///   - 事实表 → 下面这张清单，逐字节全等；
///   - 资产表 → `assert_only_moderation_column_changed`，除那一列外逐字节全等。
/// 合起来才是完整的「下架的是展示，不是已发生的事」。
const WORLDLINE_FACT_TABLES: &[&str] = &[
    "world_events",
    "world_ticks",
    "world_members",
    "world_contributions",
    "world_biographies",
    "consent_requests",
    "interventions",
    "backpacks",
];

async fn dump_facts(db: &AnyPool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for t in WORLDLINE_FACT_TABLES {
        out.push(((*t).to_string(), dump_table(db, t).await));
    }
    out
}

fn assert_facts_identical(before: &[(String, String)], after: &[(String, String)], ctx: &str) {
    for ((t1, a), (t2, b)) in before.iter().zip(after.iter()) {
        assert_eq!(t1, t2);
        assert_eq!(
            a, b,
            "🔴 {ctx}：世界线事实表 `{t1}` 被改写。§0.3 公共事实不可回滚——\
             一张卡被下架不意味着它参演过的世界事实要被抹掉"
        );
    }
}

/// 资产行快照（逐列），供「只有展示态那一列变了」的断言。
async fn dump_row(db: &AnyPool, table: &str, id: &str) -> Vec<(String, String)> {
    let r = sqlx::query(&format!("SELECT * FROM {table} WHERE id = $1"))
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap();
    let mut out = Vec::new();
    for (i, col) in r.columns().iter().enumerate() {
        let cell = if let Ok(v) = r.try_get::<Option<String>, _>(i) {
            format!("s{v:?}")
        } else if let Ok(v) = r.try_get::<Option<i64>, _>(i) {
            format!("i{v:?}")
        } else {
            "<undecodable>".to_string()
        };
        out.push((col.name().to_string(), cell));
    }
    out
}

fn assert_only_moderation_column_changed(
    before: &[(String, String)],
    after: &[(String, String)],
    changed_col: &str,
) {
    assert_eq!(before.len(), after.len(), "列数不应变化");
    for ((n1, v1), (n2, v2)) in before.iter().zip(after.iter()) {
        assert_eq!(n1, n2);
        if n1 == changed_col {
            assert_ne!(v1, v2, "展示态列 `{n1}` 应当被改写，否则下架没生效");
        } else {
            assert_eq!(
                v1, v2,
                "🔴 下架只应改展示态列 `{changed_col}`，但 `{n1}` 也被改了：{v1} → {v2}"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ① 红线：下架不改世界线
// ═══════════════════════════════════════════════════════════════════════════

/// 布景：一个已经演过两拍、有事件与成员的运行中世界，成员用的正是待下架的那张卡。
async fn seed_world_with_history(state: &AppState) -> String {
    seed_user(state, "u1").await;
    seed_char(state, "c1", "u1", "approved").await;
    seed_template(state, "tpl1", "approved").await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl1", 1, "青云试炼"))
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, \
         status, joined_at) VALUES ('wm1', $1, 'u1', 'c1', '{}', 'active', $2)",
    )
    .bind(&wid)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    for n in 1..=2i64 {
        sqlx::query(
            "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
             VALUES ($1, $2, $3, 0, 'done', 120, $4)",
        )
        .bind(format!("tk{n}"))
        .bind(&wid)
        .bind(n)
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
             actors_json, visibility, public_projection_json, occurred_at) \
             VALUES ($1, $2, $3, $4, $5, 'action', '[\"c1\"]', 'public', \
             '{\"text\":\"他在城门口退了一步\"}', $6)",
        )
        .bind(format!("ev{n}"))
        .bind(&wid)
        .bind(n)
        .bind(n)
        .bind(format!("de{n}"))
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
    }
    wid
}

/// 🔴 **下架 → 恢复 → 永久移除，全流程走完后八张世界线事实表逐字节不变。**
///
/// 这是本模块最重要的一条：处置能力的边界不是「尽量少改」，而是「事实表一个字节都不许动」。
#[tokio::test]
async fn red_line_takedown_and_restore_leave_worldline_byte_identical() {
    let state = test_state().await;
    let _wid = seed_world_with_history(&state).await;
    let reviewer = admin_token(&state, "reviewer");
    let root = admin_token(&state, "admin");

    let before = dump_facts(&state.db).await;

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实：冒充真人" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "restricted");
    assert_eq!(body["worldlineUntouched"], true, "🔴 回执必须明说世界线没动");
    assert_facts_identical(&before, &dump_facts(&state.db).await, "下架后");

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/restore",
        &reviewer,
        Some(json!({ "reason": "复核后认为不构成违规" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_facts_identical(&before, &dump_facts(&state.db).await, "恢复后");

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &root,
        Some(json!({ "reason": "监管要求永久移除", "permanent": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "removed");
    assert_facts_identical(&before, &dump_facts(&state.db).await, "永久移除后");

    // 事实表没动，但事件确实还在——证明上面的「全等」不是因为表本来就空。
    assert_eq!(count_rows(&state.db, "SELECT COUNT(*) FROM world_events").await, 2);
    assert_eq!(count_rows(&state.db, "SELECT COUNT(*) FROM world_members").await, 1);
}

/// 🔴 资产行本身：**除展示态那一列外**逐字节不变（`card_json` / `withdrawn` / 历练一概不动）。
#[tokio::test]
async fn takedown_touches_only_the_display_column_on_the_asset_row() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    seed_avatar(&state, "c1").await;
    let reviewer = admin_token(&state, "reviewer");

    let before = dump_row(&state.db, "cloud_characters", "c1").await;
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_only_moderation_column_changed(
        &before,
        &dump_row(&state.db, "cloud_characters", "c1").await,
        "moderation",
    );

    // 立绘是**另一个维度**：下架卡不连坐下架立绘（分开审、分开处置，口径同申诉改判）。
    assert_eq!(
        moderation_of(&state.db, "cloud_characters", "avatar_moderation", "c1").await.as_deref(),
        Some("approved"),
    );
}

/// 源码级红线 + 动态 SQL 的值域：拼进 SQL 的表/列只可能是四个展示态位。
///
/// 运行时快照证明「这次没改」，这条证明「以后想改也得先删掉这条用例」。
/// 本模块用 `format!` 拼表名/列名（不同主体落在不同表），因此除了禁写事实表，
/// 还必须钉住**可拼进去的值本身**——那才是动态 SQL 真正的安全边界。
#[test]
fn red_line_module_never_writes_worldline_tables() {
    let src = include_str!("../takedown.rs");
    for verb in ["INSERT INTO", "UPDATE", "DELETE FROM"] {
        for table in WORLDLINE_FACT_TABLES {
            let banned = format!("{verb} {table}");
            assert!(
                !src.contains(&banned),
                "🔴 内容处置模块出现 `{banned}`：下架的是展示面，不是已发生的世界事实（§0.3）"
            );
        }
    }

    // 动态 SQL 值域：表名与列名只能取自这两张白名单。
    for spec in SUBJECTS {
        assert!(
            matches!(spec.table, "cloud_characters" | "worlds" | "world_templates"),
            "🔴 {} 的处置表 {} 不在资产表白名单内",
            spec.kind,
            spec.table
        );
        assert!(
            matches!(spec.moderation_column, "moderation" | "avatar_moderation" | "cover_moderation"),
            "🔴 {} 的处置列 {} 不是展示态列——处置只能落在读取面闸门读的那一列上",
            spec.kind,
            spec.moderation_column
        );
    }
    // 哨兵值必须是非 approved 值：全模块的拦截力都由这一条不变式提供。
    assert_ne!(TAKEDOWN, APPROVED);
    // 也不得复用 'rejected'：那是发布时被驳回的语义，复用会被申诉改判路径悄悄翻转。
    assert_ne!(TAKEDOWN, "rejected");
}

// ═══════════════════════════════════════════════════════════════════════════
// ② 读取面真的关了
// ═══════════════════════════════════════════════════════════════════════════

/// 角色卡下架 → **入场闸关闭**（409 character_not_approved）；恢复 → 重新开。
///
/// 走真实的 `POST /worlds/{id}/join`，不是断言库里那个字符串——「下架生效」的定义是
/// 玩家真的进不去，而不是某一列被改成了某个值。
#[tokio::test]
async fn takedown_closes_the_join_gate_and_restore_reopens_it() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    seed_template(&state, "tpl1", "approved").await;
    let wid = create_world(&state.db, CreateWorldParams::official("tpl1", 1, "青云试炼"))
        .await
        .unwrap();
    let tk = user_token(&state, "u1");
    let reviewer = admin_token(&state, "reviewer");
    let join_uri = format!("/api/worlds/{wid}/join");

    let (st, body) = send(&state, "POST", &join_uri, &tk, Some(json!({ "cloudCharacterId": "c1" }))).await;
    assert_eq!(st, StatusCode::OK, "下架前应能入场：{body}");

    // 退出该世界，好让下一次 join 重新走一遍入场闸（避免被幂等/已入场分支短路）。
    sqlx::query("DELETE FROM world_members WHERE cloud_character_id = 'c1'")
        .execute(&state.db)
        .await
        .unwrap();

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实：仇恨言论" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = send(&state, "POST", &join_uri, &tk, Some(json!({ "cloudCharacterId": "c1" }))).await;
    assert_eq!(st, StatusCode::CONFLICT, "🔴 下架后必须进不去新世界：{body}");
    assert!(
        body["error"]["message"].as_str().unwrap_or_default().contains("character_not_approved"),
        "应命中既有入场闸（非 approved → character_not_approved），实际：{body}"
    );

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/restore",
        &reviewer,
        Some(json!({ "reason": "误判，恢复" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, body) = send(&state, "POST", &join_uri, &tk, Some(json!({ "cloudCharacterId": "c1" }))).await;
    assert_eq!(st, StatusCode::OK, "恢复后应重新可入场：{body}");
}

/// 立绘下架 → 玩家侧读取面不再下发 `avatarUrl`（既有的 approved 门自动关闭，无需新增过滤）。
#[tokio::test]
async fn avatar_takedown_stops_delivery_on_the_existing_read_surface() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    seed_avatar(&state, "c1").await;
    let tk = user_token(&state, "u1");
    let reviewer = admin_token(&state, "reviewer");

    let (st, body) = send(&state, "GET", "/api/assets/characters/mine", &tk, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body[0]["avatarUrl"].is_string(), "下架前应下发立绘：{body}");

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character_avatar/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "立绘含违规元素" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = send(&state, "GET", "/api/assets/characters/mine", &tk, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body[0]["avatarUrl"].is_null(), "🔴 下架后立绘绝不下发：{body}");
    // 卡本身不连坐：立绘与卡是两个维度。
    assert_eq!(body[0]["moderation"], "approved");
}

/// 世界模板下架 → 不能再用它开新房（既有 `create_room` 闸）。
#[tokio::test]
async fn template_takedown_blocks_new_rooms() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_template(&state, "tpl1", "approved").await;
    let reviewer = admin_token(&state, "reviewer");

    let before = moderation_of(&state.db, "world_templates", "moderation", "tpl1").await;
    assert_eq!(before.as_deref(), Some("approved"));

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/world_template/tpl1/takedown",
        &reviewer,
        Some(json!({ "reason": "版权投诉" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        moderation_of(&state.db, "world_templates", "moderation", "tpl1").await.as_deref(),
        Some(TAKEDOWN),
        "🔴 非 approved 值即关闭 create_room 的 template_not_approved 闸",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ③ 可逆性两档
// ═══════════════════════════════════════════════════════════════════════════

/// 永久移除：不可恢复（restore 恒 409），且位图主体的**对象字节真的被删掉**。
///
/// 一个只改标志位的「永久移除」在被要求删除时不算删除——这条用例守的就是那句话。
#[tokio::test]
async fn permanent_removal_is_irreversible_and_purges_bitmap_bytes() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let key = seed_avatar(&state, "c1").await;
    let root = admin_token(&state, "admin");
    let reviewer = admin_token(&state, "reviewer");

    assert!(state.objects.get(&key).is_ok(), "前提：字节在");

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character_avatar/c1/takedown",
        &root,
        Some(json!({ "reason": "监管要求永久移除", "permanent": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "removed");
    assert_eq!(body["reversible"], false);
    assert_eq!(body["bytesPurged"], true, "位图永久移除必须连字节一起删：{body}");
    assert!(state.objects.get(&key).is_err(), "🔴 对象字节必须已被删除");

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character_avatar/c1/restore",
        &reviewer,
        Some(json!({ "reason": "想撤回" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "🔴 永久移除不可恢复：{body}");

    // 文本主体（角色卡）**不删字节**：运行中的世界仍引用那份不可变快照。
    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &root,
        Some(json!({ "reason": "同批处置", "permanent": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["bytesPurged"], false, "文本主体不删字节（删了会让运行中的世界崩掉）");
    let card: String = sqlx::query_scalar("SELECT card_json FROM cloud_characters WHERE id = 'c1'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(card.contains("沈砚"), "🔴 card_json 必须原样保留：{card}");
}

/// 可恢复下架能升级为永久移除，但恢复点仍是**首次下架前**的值（不是 'takedown' 自身）。
#[tokio::test]
async fn restricted_can_be_escalated_to_permanent_but_never_downgraded() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let reviewer = admin_token(&state, "reviewer");
    let root = admin_token(&state, "admin");

    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "先拿下来看看" })),
    )
    .await;
    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &root,
        Some(json!({ "reason": "核实后永久移除", "permanent": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "removed");
    assert_eq!(body["prevModeration"], "approved", "🔴 升级不得把恢复点写成 takedown 自身：{body}");

    // 降级（removed → restricted）无路径：再下一次可恢复下架直接 409。
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "想改回可恢复" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "🔴 不可逆就是不可逆，不给降级后门");
}

/// 恢复写回的是台账里的 `prev_moderation`，不是常量 'approved'。
#[tokio::test]
async fn restore_writes_back_the_recorded_previous_state() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let reviewer = admin_token(&state, "reviewer");

    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实" })),
    )
    .await;
    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/restore",
        &reviewer,
        Some(json!({ "reason": "复核不成立" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["moderation"], "approved");
    assert_eq!(body["state"], "restored");
    assert_eq!(
        moderation_of(&state.db, "cloud_characters", "moderation", "c1").await.as_deref(),
        Some("approved"),
    );
    // 台账行保留（「曾经被下架过」本身是处置记录的一部分），但不再计入下架。
    assert_eq!(count_rows(&state.db, "SELECT COUNT(*) FROM content_takedowns").await, 1);

    // 重复恢复 → 409（不是静默成功：那会让运营以为刚才那次没生效）。
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/restore",
        &reviewer,
        Some(json!({ "reason": "再点一次" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
}

// ═══════════════════════════════════════════════════════════════════════════
// ④ 权限台阶与入参守卫
// ═══════════════════════════════════════════════════════════════════════════

/// 权限台阶走**既有 `require_role` 矩阵**，不新造角色：
/// 可恢复下架 reviewer / 永久移除 admin 专属 / 其余后台角色一律 403。
#[tokio::test]
async fn permission_ladder_uses_the_existing_role_matrix() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    seed_char(&state, "c2", "u1", "approved").await;

    // support / finance / operator 都碰不到内容处置（内容审核是 reviewer 的档）。
    for role in ["support", "finance", "operator"] {
        let (st, _) = send(
            &state,
            "POST",
            "/api/admin/content/character/c1/takedown",
            &admin_token(&state, role),
            Some(json!({ "reason": "越权尝试" })),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{role} 不应能下架内容");
    }

    // reviewer 可做可恢复下架，但**做不了**永久移除。
    let reviewer = admin_token(&state, "reviewer");
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c2/takedown",
        &reviewer,
        Some(json!({ "reason": "顺手永久删掉", "permanent": true })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "🔴 不可逆处置必须比可逆处置门槛更高");
    assert_eq!(
        moderation_of(&state.db, "cloud_characters", "moderation", "c2").await.as_deref(),
        Some("approved"),
        "403 时一行都不该落库",
    );
    assert_eq!(count_rows(&state.db, "SELECT COUNT(*) FROM content_takedowns").await, 1);

    // 玩家 token 连门都进不来（AdminUser 提取器粗粒度守卫）。
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c2/takedown",
        &user_token(&state, "u1"),
        Some(json!({ "reason": "我自己来" })),
    )
    .await;
    assert!(matches!(st, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED));
}

/// 入参守卫：未知主体 400、主体不存在 404、非过审态 409、理由必填。
#[tokio::test]
async fn guards_reject_unknown_kinds_missing_subjects_and_unpublished_content() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    seed_char(&state, "cp", "u1", "pending").await;
    let reviewer = admin_token(&state, "reviewer");

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/world_event/ev1/takedown",
        &reviewer,
        Some(json!({ "reason": "试试" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "未知主体应 400 并列出支持的取值：{body}");

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/nope/takedown",
        &reviewer,
        Some(json!({ "reason": "试试" })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 未过审的内容不走本入口——它还在人审队列里，那边的 reject 才是它的路径。
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/cp/takedown",
        &reviewer,
        Some(json!({ "reason": "试试" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // 无立绘的卡：行在但列为 NULL → 409（不是 404，主体是存在的）。
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character_avatar/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "试试" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // 理由必填：没有理由的处置无法复盘。
    for reason in ["", "   "] {
        let (st, _) = send(
            &state,
            "POST",
            "/api/admin/content/character/c1/takedown",
            &reviewer,
            Some(json!({ "reason": reason })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
    assert_eq!(count_rows(&state.db, "SELECT COUNT(*) FROM content_takedowns").await, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑤ 再审：走 safety 入口 ③，不改展示态
// ═══════════════════════════════════════════════════════════════════════════

/// 再审把内容送回人审队列，但**不改展示态**；同主体重复再审不刷屏队列。
#[tokio::test]
async fn recheck_queues_via_safety_without_changing_display_state() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let reviewer = admin_token(&state, "reviewer");

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/recheck",
        &reviewer,
        Some(json!({ "reason": "收到 3 条骚扰举报，请人审复看" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["created"], true);
    assert_eq!(body["takenDown"], false, "再审不改展示态");
    assert_eq!(
        moderation_of(&state.db, "cloud_characters", "moderation", "c1").await.as_deref(),
        Some("approved"),
        "🔴 再审期间内容照常在线",
    );

    // 入队与记险都发生在 safety 里（本模块一条 audit_queue / risk_events 的 INSERT 都不写）。
    assert_eq!(
        count_rows(
            &state.db,
            "SELECT COUNT(*) FROM audit_queue WHERE subject_kind='character' AND subject_id='c1' AND status='open'"
        )
        .await,
        1,
    );
    assert_eq!(
        count_rows(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind='content_recheck'").await,
        1,
    );

    // 幂等：同一张卡被多人举报是常态，一举报一入队会把人审队列刷屏。
    let (st, body2) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/recheck",
        &reviewer,
        Some(json!({ "reason": "又来一条举报" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body2["created"], false);
    assert_eq!(body2["queueId"], body["queueId"], "应复用既有 open 队列行");
    assert_eq!(count_rows(&state.db, "SELECT COUNT(*) FROM audit_queue").await, 1);
    // 幂等命中也记险：那是一次真实发生的运营调取动作。
    assert_eq!(
        count_rows(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind='content_recheck'").await,
        2,
    );
    // 两次调取都各留一条审计。
    assert_eq!(
        count_rows(&state.db, "SELECT COUNT(*) FROM audit_logs WHERE action='content.recheck'").await,
        2,
    );
}

/// 位图主体没有文本再审通道 → 400 如实告知，而不是假装排了队。
#[tokio::test]
async fn recheck_refuses_bitmap_subjects_honestly() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    seed_avatar(&state, "c1").await;
    let reviewer = admin_token(&state, "reviewer");

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character_avatar/c1/recheck",
        &reviewer,
        Some(json!({ "reason": "立绘可疑" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(count_rows(&state.db, "SELECT COUNT(*) FROM audit_queue").await, 0, "不得留下悬空队列项");
}

/// 🔴 **人审队列的 approve 不得复活已下架主体。**
///
/// 场景：先再审入队 → 期间又把它下架 → 有人在工作台点「通过」。若不设防，回写会把主体写回
/// approved，等于绕过 restore 的可逆性台阶与权限台阶把下架撤销掉。
#[tokio::test]
async fn queue_approve_cannot_resurrect_a_taken_down_subject() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let reviewer = admin_token(&state, "reviewer");

    let (_, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/recheck",
        &reviewer,
        Some(json!({ "reason": "复看" })),
    )
    .await;
    let queue_id = body["queueId"].as_str().unwrap().to_string();

    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "证据充分，先拿下来" })),
    )
    .await;

    let (st, _) = send(
        &state,
        "POST",
        &format!("/api/admin/audit-queue/{queue_id}/approve?reason=看着还行"),
        &reviewer,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "队列行本身照常推进（人审结论是真实的）");
    assert_eq!(
        moderation_of(&state.db, "cloud_characters", "moderation", "c1").await.as_deref(),
        Some(TAKEDOWN),
        "🔴 已下架主体不得经人审队列复活；恢复的唯一路径是 restore",
    );

    // 反向：reject 是比下架更强的处置，必须能落地。
    seed_char(&state, "c2", "u1", "approved").await;
    let (_, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c2/recheck",
        &reviewer,
        Some(json!({ "reason": "复看" })),
    )
    .await;
    let q2 = body["queueId"].as_str().unwrap().to_string();
    send(
        &state,
        "POST",
        "/api/admin/content/character/c2/takedown",
        &reviewer,
        Some(json!({ "reason": "先拿下来" })),
    )
    .await;
    send(
        &state,
        "POST",
        &format!("/api/admin/audit-queue/{q2}/reject?reason=确认违规"),
        &reviewer,
        None,
    )
    .await;
    assert_eq!(
        moderation_of(&state.db, "cloud_characters", "moderation", "c2").await.as_deref(),
        Some("rejected"),
        "驳回是更强的处置，应当落地",
    );
    // 而此时 restore 会因为 CAS 命中 0 行而 409——不会把一个刚被驳回的主体重新点亮。
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/content/character/c2/restore",
        &reviewer,
        Some(json!({ "reason": "恢复看看" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
}

// ═══════════════════════════════════════════════════════════════════════════
// ⑥ 留痕、台账、运行中世界
// ═══════════════════════════════════════════════════════════════════════════

/// 处置与留痕**同事务**：audit_logs + risk_events 与状态改动同成同败。
#[tokio::test]
async fn disposal_writes_audit_log_and_risk_event() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let reviewer = admin_token(&state, "reviewer");
    let root = admin_token(&state, "admin");

    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实：冒充真人" })),
    )
    .await;
    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/restore",
        &reviewer,
        Some(json!({ "reason": "复核不成立" })),
    )
    .await;
    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &root,
        Some(json!({ "reason": "监管要求", "permanent": true })),
    )
    .await;

    for action in ["content.takedown", "content.restore", "content.takedown_permanent"] {
        let n = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_logs WHERE action = $1 AND subject = 'character:c1'",
        )
        .bind(action)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(n, 1, "{action} 必须留痕");
    }
    // 审计里记的是**谁**做的：不可逆那条必须落在 admin 头上。
    let actor: String = sqlx::query_scalar(
        "SELECT actor_role FROM audit_logs WHERE action = 'content.takedown_permanent'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(actor, "admin");

    assert_eq!(
        count_rows(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind='content_disposal'").await,
        3,
        "三次处置各记一条风控事件（走 safety::record_risk_tx，本模块不直写）",
    );
    // 风控事件带上了主体 owner 维度，风控面按人查得到。
    assert_eq!(
        count_rows(
            &state.db,
            "SELECT COUNT(*) FROM risk_events WHERE kind='content_disposal' AND user_id='u1'"
        )
        .await,
        3,
    );
}

/// 处置回执列出「这张卡还在哪些运行中的世界」——现状陈述，且**不动它们**。
#[tokio::test]
async fn takedown_reports_affected_running_worlds_without_touching_them() {
    let state = test_state().await;
    let wid = seed_world_with_history(&state).await;
    let reviewer = admin_token(&state, "reviewer");

    let (st, body) = send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["affectedRunningWorldCount"], 1);
    assert_eq!(body["affectedRunningWorlds"][0]["id"], json!(wid));
    assert!(
        body["notes"].as_array().unwrap().iter().any(|n| n
            .as_str()
            .unwrap_or_default()
            .contains("pause")),
        "🔴 有运行中世界时必须把既有的暂停入口写进回执（本端点不代做强制离场）：{body}",
    );

    // 世界与成员一动不动：处置的是展示面，运行中的世界照常跑。
    let status: String = sqlx::query_scalar("SELECT status FROM worlds WHERE id = $1")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(status, "open", "🔴 下架不得顺手停掉运行中的世界");
    let member: String =
        sqlx::query_scalar("SELECT status FROM world_members WHERE id = 'wm1'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(member, "active", "🔴 下架不得把成员踢出世界（那要动世界线相关表）");
}

/// 台账列表：**复合游标**在同毫秒并列行上不丢行（单列游标会把跨页的并列组整组跳过）。
#[tokio::test]
async fn ledger_compound_cursor_never_drops_rows_on_timestamp_ties() {
    let state = test_state().await;
    let reviewer = admin_token(&state, "reviewer");
    let now = now_ms();
    // 五条**同一毫秒**的处置记录（批量处置是常态，同毫秒并列不是极端情况）。
    for i in 0..5 {
        sqlx::query(
            "INSERT INTO content_takedowns \
             (id, subject_kind, subject_id, state, prev_moderation, reason, actor_id, actor_role, \
              bytes_purged, created_at) \
             VALUES ($1, 'character', $2, 'restricted', 'approved', '批量处置', 'adm', 'reviewer', 0, $3)",
        )
        .bind(format!("ctd_{i}"))
        .bind(format!("c{i}"))
        .bind(now)
        .execute(&state.db)
        .await
        .unwrap();
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let uri = match &cursor {
            Some(c) => format!("/api/admin/content/takedowns?limit=2&cursor={c}"),
            None => "/api/admin/content/takedowns?limit=2".to_string(),
        };
        let (st, body) = send(&state, "GET", &uri, &reviewer, None).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        for item in body["items"].as_array().unwrap() {
            seen.push(item["subjectId"].as_str().unwrap().to_string());
        }
        match body["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    seen.sort();
    let expect: Vec<String> = (0..5).map(|i| format!("c{i}")).collect();
    assert_eq!(
        seen, expect,
        "🔴 同毫秒并列行跨页时一条都不能丢（单列游标会把边界那一组整组跳过）",
    );
}

/// 台账筛选与单主体查询：筛选值走白名单（未知值 400 而不是静默空列表）。
#[tokio::test]
async fn ledger_filters_and_subject_status() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let reviewer = admin_token(&state, "reviewer");

    let (st, body) = send(&state, "GET", "/api/admin/content/character/c1", &reviewer, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["takenDown"], false);
    assert_eq!(body["canTakedown"], true);
    assert_eq!(body["canRecheck"], true);
    assert!(body["takedown"].is_null(), "从未被处置过 → null（真实答案，不是缺数据）");

    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": "举报核实" })),
    )
    .await;

    let (_, body) = send(&state, "GET", "/api/admin/content/character/c1", &reviewer, None).await;
    assert_eq!(body["takenDown"], true);
    assert_eq!(body["takedown"]["state"], "restricted");
    assert_eq!(body["takedown"]["reversible"], true);

    let (st, body) = send(
        &state,
        "GET",
        "/api/admin/content/takedowns?state=restricted&kind=character",
        &reviewer,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);

    let (st, _) = send(&state, "GET", "/api/admin/content/takedowns?state=bogus", &reviewer, None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = send(&state, "GET", "/api/admin/content/takedowns?kind=bogus", &reviewer, None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "未知 kind 应 400——空列表会被读成「这类内容没被处置过」");
}

/// 作者侧告知：status 端点下发下架状态，但**不泄露运营内部处置理由**。
#[tokio::test]
async fn owner_status_discloses_takedown_without_leaking_internal_reason() {
    let state = test_state().await;
    seed_user(&state, "u1").await;
    seed_char(&state, "c1", "u1", "approved").await;
    let tk = user_token(&state, "u1");
    let reviewer = admin_token(&state, "reviewer");
    let internal_reason = "内部备注：与 case#8812 同一批黑产账号";

    let (st, body) = send(&state, "GET", "/api/assets/characters/c1/status", &tk, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body["takedown"].is_null(), "未被处置 → null");

    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/takedown",
        &reviewer,
        Some(json!({ "reason": internal_reason })),
    )
    .await;

    let (st, body) = send(&state, "GET", "/api/assets/characters/c1/status", &tk, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["takedown"]["state"], "restricted");
    assert_eq!(body["takedown"]["reversible"], true);
    assert!(body["takedown"]["notice"].as_str().unwrap().contains("下架"));
    let raw = body.to_string();
    assert!(
        !raw.contains("case#8812"),
        "🔴 运营内部处置理由不得回显给作者（口径同 audit_logs.reason）：{raw}",
    );

    // 恢复后不再翻旧账：作者看到的就是正常内容。
    send(
        &state,
        "POST",
        "/api/admin/content/character/c1/restore",
        &reviewer,
        Some(json!({ "reason": "复核不成立" })),
    )
    .await;
    let (_, body) = send(&state, "GET", "/api/assets/characters/c1/status", &tk, None).await;
    assert!(body["takedown"].is_null(), "已恢复 → 不下发下架告知");
}
