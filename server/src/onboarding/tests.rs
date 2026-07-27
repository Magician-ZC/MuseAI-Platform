//! 新手动线集成测试（sqlite::memory + oneshot 全栈）。
//!
//! 覆盖清单（每条对应模块头的一条硬约束）：
//! - 开关默认关闭 → 四个端点全 404，且**已领过的礼包也读不出**（读取侧降级）；
//! - 领取幂等：DB 主键（不是应用层读-判-写）保证每人只发一张卡、只建一个世界；
//! - 预制卡确实 `approved` 且能走**既有** `POST /worlds/{id}/join` 入场；
//! - 🔴 **微本能真正跑起来**：不卡在 `insufficient_members`，至少推进一拍，并在上限内收束；
//! - 卡位约束：预制卡占卡位，卡位满 → 409；
//! - 同源唯一不会拦住两个新用户（两张预制卡进同一个世界都放行）；
//! - 预制卡 / NPC 卡能被 `CharacterCardV2` 解析（守住「静默跳过 → 成员凭空消失」这个假绿）；
//! - 本模块永不写 `world_members`（源码断言）。

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::AnyPool;
use tower::ServiceExt;

use crate::app::{build_router, AppState};
use crate::config::ServerConfig;
use crate::db::{new_id, now_ms};
use crate::runtime::{insert_tick, process_tick_with_model, TickStatus};
use crate::worlds::{create_world, CreateWorldParams};

use muse_engine::character::types::CharacterCardV2;
use muse_engine::host::CancelFlag;
use muse_engine::model::{ModelCallSpec, ModelClient, ModelOutput};
use muse_engine::EngineError;

use super::{microworld, presets, OnboardingSwitch};

// ---------- 脚手架 ----------

fn test_config() -> ServerConfig {
    ServerConfig {
        database_url: crate::testkit::test_database_url(),
        bind_addr: "127.0.0.1:0".into(),
        jwt_secret: "test-secret".into(),
        access_ttl_secs: 3600,
        refresh_ttl_secs: 100_000,
        dev_mode: true,
        object_store_dir: std::env::temp_dir().join(new_id("muse-onb")).to_string_lossy().into_owned(),
    }
}

async fn test_state() -> AppState {
    AppState::new(crate::testkit::test_pool().await, test_config())
}

fn token(state: &AppState, user_id: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, user_id, "user", 3600).unwrap()
}

async fn seed_user(db: &AnyPool, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
         VALUES ($1, '', 1, 'active', $2, $3)",
    )
    .bind(id)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 钉住 active 的模型路由版本：`create_world_tx` 建房时从这里解析并钉进世界行，
/// runtime 据此判定「有模型配置」（真实调用走注入的 mock）。**必须在领取礼包之前 seed**。
async fn seed_model_routes(db: &AnyPool, version: &str) {
    let routes = json!({
        "default": { "interface": "OpenAI-compatible", "baseUrl": "http://mock", "apiKey": "k", "model": "mock-model" }
    });
    sqlx::query("INSERT INTO model_routes (id, version, routes_json, active, created_at) VALUES ($1, $2, $3, 1, $4)")
        .bind(new_id("mr"))
        .bind(version)
        .bind(routes.to_string())
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
}

async fn post_json(
    app: &axum::Router,
    uri: &str,
    tok: &str,
    idem_key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = idem_key {
        b = b.header("Idempotency-Key", k);
    }
    let req = b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn get_json(app: &axum::Router, uri: &str, tok: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn count(db: &AnyPool, sql: &str, bind: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).bind(bind).fetch_one(db).await.unwrap()
}

/// 环节感知的 mock ModelClient（与 `runtime::tests::MockModel` 同款：按 `spec.agent` 回合法 JSON，
/// 与调用顺序/次数解耦，可跨多拍复用）。
struct MockModel;

#[async_trait]
impl ModelClient for MockModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"渡口茶棚，灯芯将尽，河上传来船桨声。"}"#,
            "roleDecide" => r#"{"intent":"观望","action":"把碗放下，抬眼看向对面","speak":{"willSpeak":true,"purpose":"探问"},"targets":[],"acceptableCosts":[],"predictions":[]}"#,
            "arbiter" => r#"{"outcomes":[]}"#,
            "writer" => r#"{"prose":"茶棚里三个人各自沉默，谁也没先开口，只有河水在棚外一声一声地拍着木桩。"}"#,
            "critic" => r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#,
            _ => "{}",
        };
        Ok(ModelOutput { content: content.into(), input_tokens: Some(10), output_tokens: Some(20) })
    }
}

/// 领取礼包 → 用既有 join 端点投放 → 开演。返回 `(worldId, cloudCharacterId)`。
/// 调用方须自行持有 `OnboardingSwitch`（开关是进程级 env，必须由用例作用域控制）。
async fn claim_join_start(app: &axum::Router, tok: &str) -> (String, String) {
    let (st, body) = post_json(app, "/api/me/onboarding/gift", tok, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "领取新人礼包应成功: {body}");
    let wid = body["worldId"].as_str().unwrap().to_string();
    let cid = body["cloudCharacterId"].as_str().unwrap().to_string();

    // 🔴 入场走**既有** join 端点：礼包不写 world_members，join 的全部服务端校验一条不少。
    let (st, jb) = post_json(
        app,
        &format!("/api/worlds/{wid}/join"),
        tok,
        None,
        json!({ "cloudCharacterId": cid, "boundary": {} }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "预制卡应能通过 join 的全部资格门: {jb}");

    let (st, sb) = post_json(app, "/api/me/onboarding/microworld/start", tok, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "开演应成功: {sb}");
    assert_eq!(sb["status"], "running");
    (wid, cid)
}

// ==================== ① 开关默认关闭 ====================

/// 开关默认关闭 → 四个端点全 404（不是 403：不泄露「平台有这个未开放功能」）。
#[tokio::test]
async fn endpoints_are_absent_when_switch_off_by_default() {
    let _sw = OnboardingSwitch::set(false);
    let state = test_state().await;
    seed_user(&state.db, "u_off").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_off");

    for (method, uri) in [
        ("GET", "/api/onboarding/presets"),
        ("GET", "/api/me/onboarding"),
        ("POST", "/api/me/onboarding/gift"),
        ("POST", "/api/me/onboarding/microworld/start"),
    ] {
        let (st, _) = if method == "GET" {
            get_json(&app, uri, &tok).await
        } else {
            post_json(&app, uri, &tok, None, json!({})).await
        };
        assert_eq!(st, StatusCode::NOT_FOUND, "{method} {uri} 在开关关闭时应 404");
    }
    // 关闭态下不得有任何发放副作用。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM onboarding_grants WHERE user_id = $1", "u_off").await, 0);
}

/// 读取侧降级：**已经领过**的用户，在开关被关掉后同样读不出、开不了演（可逆急停阀）。
#[tokio::test]
async fn existing_grant_is_unreadable_after_switch_off() {
    let state = test_state().await;
    seed_user(&state.db, "u_rev").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_rev");

    {
        let _on = OnboardingSwitch::set(true);
        let (st, _) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
        assert_eq!(st, StatusCode::OK);
    }
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM onboarding_grants WHERE user_id = $1", "u_rev").await, 1);

    let _off = OnboardingSwitch::set(false);
    let (st, _) = get_json(&app, "/api/me/onboarding", &tok).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "开关关掉后，已有登记行也必须读不出");
    let (st, _) = post_json(&app, "/api/me/onboarding/microworld/start", &tok, None, json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "开关关掉后，已领的礼包也开不了演");
}

// ==================== ② 领取幂等（DB 唯一键） ====================

/// 重复领取（**不带 Idempotency-Key**，即幂等键完全不参与）只发一次：
/// 唯一性来自 `onboarding_grants.user_id` 主键，而不是应用层的读-判-写。
#[tokio::test]
async fn claim_is_idempotent_without_idempotency_key() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_idem").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_idem");

    let (s1, b1) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    let (s2, b2) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK, "重复领取是幂等成功，不是错误");
    assert_eq!(b1, b2, "两次领取必须返回逐字节相同的回执");

    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM onboarding_grants WHERE user_id = $1", "u_idem").await, 1);
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM cloud_characters WHERE owner_id = $1", "u_idem").await,
        1,
        "只发一张预制卡"
    );
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM worlds WHERE host_user_id = $1", "u_idem").await,
        1,
        "只建一个微本世界（第二次领取的建房必须随事务回滚，不留残行）"
    );
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM world_budgets WHERE world_id IN (SELECT id FROM worlds WHERE host_user_id = $1)", "u_idem").await,
        1,
        "预算行同样不得残留"
    );
}

/// 唯一性是**数据库主键**：绕过端点直插第二行必须失败。
/// （这条用例守的是「不要哪天把主键降级成普通索引 + 应用层判断」。）
#[tokio::test]
async fn grant_uniqueness_is_enforced_by_primary_key() {
    let state = test_state().await;
    sqlx::query("INSERT INTO onboarding_grants (user_id, preset_id, cloud_character_id, world_id, created_at) VALUES ('u_pk','p','c','w',1)")
        .execute(&state.db)
        .await
        .unwrap();
    let err = sqlx::query("INSERT INTO onboarding_grants (user_id, preset_id, cloud_character_id, world_id, created_at) VALUES ('u_pk','p2','c2','w2',2)")
        .execute(&state.db)
        .await;
    let err = err.expect_err("同一 user_id 第二行必须被主键拒绝");
    match err {
        sqlx::Error::Database(e) => assert!(e.is_unique_violation(), "应为唯一键冲突: {e}"),
        other => panic!("应为数据库唯一键错误，实得 {other:?}"),
    }
}

/// `Idempotency-Key` 是**另一层**：同 key 同载荷返回缓存响应（覆盖同一次点击的 HTTP 重试）。
#[tokio::test]
async fn claim_idempotency_key_returns_cached_response() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_key").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_key");

    let (s1, b1) = post_json(&app, "/api/me/onboarding/gift", &tok, Some("k-1"), json!({})).await;
    let (s2, b2) = post_json(&app, "/api/me/onboarding/gift", &tok, Some("k-1"), json!({})).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b1, b2);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM onboarding_grants WHERE user_id = $1", "u_key").await, 1);
}

/// 未知 presetId → 400（绝不静默发一张别的卡）。
#[tokio::test]
async fn unknown_preset_id_is_rejected() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_bad").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_bad");

    let (st, _) =
        post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({ "presetId": "preset_不存在" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM onboarding_grants WHERE user_id = $1", "u_bad").await, 0);
}

// ==================== ③ 预制卡 approved 且能 join ====================

/// 预制卡落库即 `approved`、`pristine=0`、`source_fingerprint` 为 NULL，
/// 且能通过既有 `POST /worlds/{id}/join` 的全部资格门入场。
#[tokio::test]
async fn preset_card_is_approved_and_passes_join() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_join").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_join");

    let (st, body) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let wid = body["worldId"].as_str().unwrap().to_string();
    let cid = body["cloudCharacterId"].as_str().unwrap().to_string();

    let row = sqlx::query(
        "SELECT moderation, withdrawn, pristine, source_fingerprint, mileage FROM cloud_characters WHERE id = $1",
    )
    .bind(&cid)
    .fetch_one(&state.db)
    .await
    .unwrap();
    use sqlx::Row;
    assert_eq!(row.try_get::<String, _>("moderation").unwrap(), "approved", "否则 join 会撞 character_not_approved");
    assert_eq!(row.try_get::<i64, _>("withdrawn").unwrap(), 0);
    assert_eq!(row.try_get::<i64, _>("pristine").unwrap(), 0, "预制卡视为「已由用户领取」，不参与同源唯一");
    assert!(row.try_get::<Option<String>, _>("source_fingerprint").unwrap().is_none(), "原创虚构卡无提取源指纹");
    assert_eq!(row.try_get::<i64, _>("mileage").unwrap(), 0, "历练唯一写入路径是 grant_mileage_tx，礼包不发历练");

    let (st, jb) = post_json(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &tok,
        None,
        json!({ "cloudCharacterId": cid, "boundary": {} }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "预制卡应能入场: {jb}");
    // 庇护档：新手场死亡不可能，join 不需要任何契约签署。
    assert_eq!(jb["lethality"], "sanctuary");
}

// ==================== ④ 🔴 微本能真正跑起来（本任务最容易做死的地方） ====================

/// 🔴 核心用例：单人微本**不卡在 `insufficient_members`**，能真正推进至少一拍。
///
/// 死锁原理（`runtime` 的门是 `member_ids.is_empty() || active_cards.len() < 2`，
/// 而 `active_cards` 把 NPC 也算在内）：骨架若不带 `worldCharacters`，单人房永远跳过。
/// 本用例断言的正是「玩家 1 张卡 + 骨架自带 2 个 NPC = 3 张活跃卡」这条链路真的接通了。
#[tokio::test]
async fn microworld_advances_at_least_one_tick_with_single_player() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    // 模型路由必须先于建房存在：create_world_tx 建房时钉住 active 版本。
    seed_model_routes(&state.db, "onb-routes").await;
    seed_user(&state.db, "u_tick").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_tick");

    let (wid, cid) = claim_join_start(&app, &tok).await;

    // 只有一名玩家成员。
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_members WHERE world_id = $1 AND status='active'")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(members, 1, "微本是单角色本：玩家角色数恒为 1");

    let model: Arc<dyn ModelClient> = Arc::new(MockModel);
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    let status = process_tick_with_model(&state, &wid, 0, model).await.unwrap();

    assert_ne!(
        status,
        TickStatus::Skipped("insufficient_members"),
        "单人微本卡在 insufficient_members —— 骨架的 worldCharacters(NPC) 没有被装配进 active_cards"
    );
    assert_eq!(status, TickStatus::Done, "单人微本首拍应走完整 run_round→commit");

    // 装配确实钉进了 2 个 NPC（这是过门的实际原因，直接断言而不是只看结论）。
    let assembled: Option<String> =
        sqlx::query_scalar("SELECT assembled_json FROM worlds WHERE id = $1").bind(&wid).fetch_one(&state.db).await.unwrap();
    let assembled: Value = serde_json::from_str(&assembled.expect("首 tick 应触发装配")).unwrap();
    let npcs = assembled.pointer("/assembly/worldCharacterEntries").and_then(Value::as_array).unwrap();
    assert_eq!(npcs.len(), 2, "骨架的 2 个 NPC 必须全部过机审并钉进实例");
    // 玩家的卡拿到了绑定自己执念的隐藏钩子（「这个世界认得我这张卡」）。
    let hooks = assembled.pointer("/assembly/perCharacterHooks").and_then(Value::as_array).unwrap();
    assert!(hooks.iter().any(|h| h["characterId"] == cid.as_str()), "玩家角色应至少绑定一条 per-character 钩子");

    // 状态真的推进了（不是 noop）。
    let rev: i64 =
        sqlx::query_scalar("SELECT state_revision FROM worlds WHERE id = $1").bind(&wid).fetch_one(&state.db).await.unwrap();
    assert_eq!(rev, 1, "state_revision 应从 0 推进到 1");
    let done: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_ticks WHERE world_id = $1 AND status='done'")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(done, 1);
}

/// 微本必然收束：`maxWorldTicks` 默认 3 → tick 0/1/2 推进、tick 3 终局停机。
/// 「新手绝不会掉进一个跑不完的世界」这条承诺必须有测试兜着。
#[tokio::test]
async fn microworld_concludes_within_max_ticks() {
    // 把上限压到 2 拍，用例跑得快且口径不变（参数化本身也顺带被验证）。
    let _sw = OnboardingSwitch::with(true, &[("MUSE_ONBOARDING_MAX_TICKS", "2"), ("MUSE_ONBOARDING_MIN_TICKS", "1")]);
    let state = test_state().await;
    seed_model_routes(&state.db, "onb-routes-2").await;
    seed_user(&state.db, "u_end").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_end");

    let (wid, _cid) = claim_join_start(&app, &tok).await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel);

    let mut statuses = Vec::new();
    for n in 0..=2i64 {
        insert_tick(&state.db, &wid, n, n).await.unwrap();
        statuses.push(process_tick_with_model(&state, &wid, n, model.clone()).await.unwrap());
        if matches!(statuses.last(), Some(TickStatus::Concluded)) {
            break;
        }
    }
    assert!(
        !statuses.iter().any(|s| matches!(s, TickStatus::Skipped("insufficient_members"))),
        "任何一拍都不得卡在 insufficient_members: {statuses:?}"
    );
    assert!(matches!(statuses.last(), Some(TickStatus::Concluded)), "微本必须在上限内收束: {statuses:?}");

    let status: String =
        sqlx::query_scalar("SELECT status FROM worlds WHERE id = $1").bind(&wid).fetch_one(&state.db).await.unwrap();
    assert_eq!(status, "ended");

    // 状态端点据此判「完成微本」（T0 门槛「10 分钟内完成首个微本」的读口径）。
    let (st, body) = get_json(&app, "/api/me/onboarding", &tok).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["world"]["status"], "ended");
    assert_eq!(body["next"][0]["step"], "graduate", "演完应指向 1-2★ 官方世界");
}

/// 开演要求已投放：未 join 就开演 → 409 并指回 join。
/// （否则调度器会在空阵容上钉死装配，新人拿不到任何 per-character 钩子。）
#[tokio::test]
async fn start_requires_join_and_is_idempotent() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_start").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_start");

    let (_, body) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    let wid = body["worldId"].as_str().unwrap().to_string();
    let cid = body["cloudCharacterId"].as_str().unwrap().to_string();

    let (st, _) = post_json(&app, "/api/me/onboarding/microworld/start", &tok, None, json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "未投放不得开演");
    let s: String =
        sqlx::query_scalar("SELECT status FROM worlds WHERE id = $1").bind(&wid).fetch_one(&state.db).await.unwrap();
    assert_eq!(s, "open");

    post_json(&app, &format!("/api/worlds/{wid}/join"), &tok, None, json!({ "cloudCharacterId": cid })).await;
    let (s1, _) = post_json(&app, "/api/me/onboarding/microworld/start", &tok, None, json!({})).await;
    let (s2, b2) = post_json(&app, "/api/me/onboarding/microworld/start", &tok, None, json!({})).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK, "重复开演幂等成功");
    assert_eq!(b2["status"], "running");
}

// ==================== ⑤ 卡位约束 ====================

/// 预制卡**占卡位**：卡位已满（默认 3）→ 409，礼包不给「白得一个养成容器」的侧路。
#[tokio::test]
async fn claim_rejected_when_card_slots_full() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_slot").await;
    // 塞满默认 3 个卡位（未撤回的云端角色数）。
    for i in 0..3 {
        sqlx::query(
            "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
             rights_declaration, moderation, withdrawn, created_at) \
             VALUES ($1, 'u_slot', $2, 1, '{}', 'original', 'approved', 0, $3)",
        )
        .bind(format!("cc_slot_{i}"))
        .bind(format!("local_{i}"))
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
    }
    let app = build_router(state.clone());
    let tok = token(&state, "u_slot");

    let (st, body) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "卡位满应拒绝领取: {body}");
    assert!(body["error"]["message"].as_str().unwrap().contains("卡位已满"));
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM onboarding_grants WHERE user_id = $1", "u_slot").await, 0);

    // 撤回一张释放卡位后即可领取（卡位是可解的约束，不是死门）。
    sqlx::query("UPDATE cloud_characters SET withdrawn = 1 WHERE id = 'cc_slot_0'").execute(&state.db).await.unwrap();
    let (st, _) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    assert_eq!(st, StatusCode::OK);
}

// ==================== ⑥ 同源唯一不拦新用户 ====================

/// 两个新用户领**同一张**预制卡，进**同一个**世界，都必须放行。
///
/// 两道保险任意一道成立即可：预制卡无 `sourceWork` → `source_fingerprint` 为 NULL；
/// 落库 `pristine=0`。本用例把它们一起钉死——将来谁给预制卡补了 `sourceWork`，
/// 这条用例仍靠 `pristine=0` 保持绿；两道都被拆掉才会红，那正是该红的时候。
#[tokio::test]
async fn two_users_preset_cards_can_join_same_world() {
    let _sw = OnboardingSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_a").await;
    seed_user(&state.db, "u_b").await;
    let app = build_router(state.clone());

    let mut ids = Vec::new();
    for u in ["u_a", "u_b"] {
        let tok = token(&state, u);
        let (st, body) =
            post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({ "presetId": "preset_a_luo" })).await;
        assert_eq!(st, StatusCode::OK);
        ids.push((u.to_string(), tok, body["cloudCharacterId"].as_str().unwrap().to_string()));
    }

    // 一个可容纳两人的共享世界（用同一个微本模板，1★ → 无历练准入门）。
    let version = microworld::ensure_template(&state.db).await.unwrap();
    let mut p = CreateWorldParams::official(microworld::MICROWORLD_TEMPLATE_ID, version, "共享测试世界");
    p.member_limit = 5;
    let shared = create_world(&state.db, p).await.unwrap();

    for (u, tok, cid) in &ids {
        let (st, body) = post_json(
            &app,
            &format!("/api/worlds/{shared}/join"),
            tok,
            None,
            json!({ "cloudCharacterId": cid, "boundary": {} }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{u} 的预制卡不应被同源唯一拦下: {body}");
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_members WHERE world_id = $1 AND status='active'")
        .bind(&shared)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 2, "两张同款预制卡应同时在场");
}

// ==================== ⑦ 卡与骨架自身的正确性（守「静默跳过」这个假绿） ====================

/// 预制卡与 NPC 卡**必须**能被 `CharacterCardV2` 解析。
///
/// 这是本模块最隐蔽的失败模式：`runtime` 组装成员卡是 `if let Ok(card)`，装配层注入 NPC 也是
/// `serde_json::from_value(...).ok()` —— 解析不出来的卡被**静默跳过**，表现为「成员凭空消失」，
/// 世界卡死在 `insufficient_members`，而所有单元测试照样绿。
#[test]
fn preset_and_npc_cards_round_trip_as_character_card_v2() {
    for p in presets::PRESETS {
        let card = p.card_for("cchar_probe");
        let text = serde_json::to_string(&card).unwrap();
        let back: CharacterCardV2 = serde_json::from_str(&text).expect("预制卡必须可被 CharacterCardV2 解析");
        assert_eq!(back.identity.name, p.name, "库内展示名必须与卡内 identity.name 一致");
        assert_eq!(back.id, "cchar_probe");
        assert!(back.identity.source_work.is_none(), "预制卡必须无提取源（否则同源唯一会撞车）");
        assert!(!back.dramatic_core.bottom_lines.is_empty(), "底线是仲裁硬约束的输入（§7 人设保险），不得为空");
    }
    // NPC 走的是「骨架 JSON → assembly::WorldCharacter → CharacterCardV2」这条实际路径。
    let sk = microworld::skeleton_value();
    let wcs = sk["worldCharacters"].as_array().expect("骨架必须有 worldCharacters");
    assert_eq!(wcs.len(), 2, "微本必须自带 2 个 NPC（见 microworld 模块头）");
    for wc in wcs {
        let card: CharacterCardV2 =
            serde_json::from_value(wc["card"].clone()).expect("NPC 卡必须可被 CharacterCardV2 解析");
        assert!(!card.id.trim().is_empty(), "无 id 的 NPC 会被装配层跳过");
        assert_eq!(wc["homeLocation"], "onb_loc_teahouse", "NPC 必须与玩家同处一地，否则不产生碰撞");
    }
}

/// 官方内容不因「不走用户审核」而绕过安全检查：卡库全文过注入检测。
#[test]
fn preset_and_npc_cards_are_injection_clean() {
    for p in presets::PRESETS {
        let card = p.card_for("cchar_probe");
        let v = serde_json::to_value(&card).unwrap();
        let hits = crate::safety::detect_injection(&crate::safety::card_scan_text(&v));
        assert!(hits.is_empty(), "预制卡 {} 命中注入规则: {hits:?}", p.id);
    }
    for card in microworld::npc_cards() {
        let v = serde_json::to_value(&card).unwrap();
        let hits = crate::safety::detect_injection(&crate::safety::card_scan_text(&v));
        assert!(hits.is_empty(), "NPC 卡 {} 命中注入规则: {hits:?}", card.id);
    }
}

/// 骨架的结构契约：终局参数可读、地点恰好一个、星级 1★、不声明产出表（未验证功能默认关闭）。
#[test]
fn skeleton_declares_endgame_single_location_and_no_payout_table() {
    let sk = microworld::skeleton_value();
    assert_eq!(sk["endgame"]["minWorldTicks"], json!(microworld::min_world_ticks()));
    assert_eq!(sk["endgame"]["maxWorldTicks"], json!(microworld::max_world_ticks()));
    assert!(microworld::max_world_ticks() >= 1, "上限必须 ≥1，否则世界无法推进");
    assert_eq!(
        sk["locations"].as_array().unwrap().len(),
        1,
        "多于一个地点会把 1 玩家 + 2 NPC 拆成互不见面的小组，微本立刻没戏可演"
    );
    assert_eq!(sk["locations"][0]["isSecretRealm"], json!(false));
    assert!(sk["mainlineNodes"].as_array().unwrap().iter().all(|n| n["fated"] == json!(true)));
    assert!(sk.get("payoutTable").is_none(), "产出表属 §10，未验证功能默认关闭：不声明 → ③ 层只累计不发放");
    assert!(sk["worldItems"].as_array().unwrap().is_empty(), "微本不发道具（道具单一写入路径为 grant_item_tx）");
}

/// 🔴 微本模板是**第二条写入路径**：`ensure_template` 直接 INSERT，不过
/// `POST /admin/world-templates` 上那道 `validate_skeleton_refs`。于是那道闸拦不到它——
/// 微本骨架里写错一个顶层键（`forbiddenPredicates` → `forbidden_predicates` 之类），
/// 结果是每一个新手玩家进的都是个静默退化的世界，且没有任何报错。这里补上等价的校验。
#[test]
fn microworld_skeleton_would_pass_the_create_template_gate() {
    let sk = microworld::skeleton_value();
    assert!(
        crate::assembly::validate_skeleton_refs(&sk, false).is_ok(),
        "{:?}",
        crate::assembly::validate_skeleton_refs(&sk, false)
    );
}

/// 模板 ensure 幂等：重复调用不改版本号；骨架变更（env 改参数）才升版。
#[tokio::test]
async fn ensure_template_is_idempotent_and_versions_on_change() {
    let state = test_state().await;
    let v1 = {
        let _sw = OnboardingSwitch::with(true, &[("MUSE_ONBOARDING_MAX_TICKS", "3")]);
        let a = microworld::ensure_template(&state.db).await.unwrap();
        let b = microworld::ensure_template(&state.db).await.unwrap();
        assert_eq!(a, b, "骨架未变 → 版本号不动");
        a
    };
    let _sw = OnboardingSwitch::with(true, &[("MUSE_ONBOARDING_MAX_TICKS", "7")]);
    let v2 = microworld::ensure_template(&state.db).await.unwrap();
    assert_eq!(v2, v1 + 1, "骨架变了 → 升版（老世界仍按钉住的老版本跑）");

    use sqlx::Row;
    let row = sqlx::query("SELECT room_type, moderation, star_rating, official FROM world_templates WHERE id = $1")
        .bind(microworld::MICROWORLD_TEMPLATE_ID)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(row.try_get::<String, _>("room_type").unwrap(), "idle", "非 idle 房不会装配 NPC、也永不收束");
    assert_eq!(row.try_get::<String, _>("moderation").unwrap(), "approved");
    assert_eq!(row.try_get::<i64, _>("star_rating").unwrap(), 1, "1★：新手无历练准入门，产出封顶最保守");
    assert_eq!(row.try_get::<i64, _>("official").unwrap(), 1);
}

// ==================== ⑧ 源码断言：礼包不是特权通道 ====================

/// 🔴 本模块**永不写 `world_members`**（体例同 `invitations::tests::module_never_writes_world_members`）。
///
/// 领取礼包只发卡 + 建房；入场必须走 `POST /worlds/{id}/join`，于是同源唯一 / 防自刷 / 星级准入 /
/// 生死契约 / 未成年门一条不少。这条性质是**结构性**的，不是约定——源码里出现任何成员表写语句即红。
#[test]
fn module_never_writes_world_members() {
    for (name, src) in [
        ("onboarding/mod.rs", include_str!("mod.rs")),
        ("onboarding/microworld.rs", include_str!("microworld.rs")),
        ("onboarding/presets.rs", include_str!("presets.rs")),
    ] {
        let lower = src.to_ascii_lowercase();
        for forbidden in ["insert into world_members", "update world_members", "delete from world_members"] {
            assert!(!lower.contains(forbidden), "{name} 出现了成员表写语句「{forbidden}」——礼包不得成为入场侧路");
        }
        // 资产单一写入路径（真红线 §0.2）：道具走 grant_item_tx、历练走 grant_mileage_tx。
        for forbidden in ["insert into backpacks", "update backpacks", "insert into items", "set mileage"] {
            assert!(!lower.contains(forbidden), "{name} 直写资产表「{forbidden}」——违反资产单一写入路径");
        }
    }
}

// ==================== 新人礼包的副本卡（§13「1 张低星副本卡」） ====================

/// 两个开关是**正交**的：新手动线开着、副本卡关着 → 礼包少一张卡，其余照常。
///
/// 这条守的是"不该因为另一块未开放的能力而整体失败"——新人仍须拿到预制卡与微本，
/// 否则一个尚未开放的经济模块就能把整条新手动线打死。
#[tokio::test]
async fn starter_subplot_card_is_skipped_when_that_switch_is_off() {
    let state = test_state().await;
    seed_user(&state.db, "u_nocard").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_nocard");

    let _on = OnboardingSwitch::set(true);
    let _off = crate::subplot::SubplotSwitch::set(false);

    let (st, body) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "副本卡开关关闭不得让整个礼包失败: {body}");
    // 预制卡与微本世界照常拿到。
    assert!(body["cloudCharacterId"].as_str().is_some(), "预制卡仍须发放");
    assert!(body["worldId"].as_str().is_some(), "微本世界仍须创建");

    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subplot_cards WHERE owner_id = 'u_nocard'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "副本卡开关关闭时一张都不该铸出");
}

/// 两个开关都开 → 礼包附赠 1★ 副本卡；且**重复领取不多出卡**（双层幂等）。
#[tokio::test]
async fn starter_subplot_card_is_granted_once_and_is_idempotent() {
    let state = test_state().await;
    seed_user(&state.db, "u_card").await;
    let app = build_router(state.clone());
    let tok = token(&state, "u_card");

    let _on = OnboardingSwitch::set(true);
    let _cards = crate::subplot::SubplotSwitch::set(true);

    let (st, body) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let star: i64 = sqlx::query_scalar("SELECT star_rating FROM subplot_cards WHERE owner_id = 'u_card'")
        .fetch_one(&state.db).await.expect("礼包应铸出一张副本卡");
    assert_eq!(star, 1, "礼包卡固定 1★——礼包是教学物不是产出物");
    let origin: String = sqlx::query_scalar("SELECT origin_kind FROM subplot_cards WHERE owner_id = 'u_card'")
        .fetch_one(&state.db).await.unwrap();
    assert_eq!(origin, "grant");
    let key: String = sqlx::query_scalar("SELECT grant_key FROM subplot_cards WHERE owner_id = 'u_card'")
        .fetch_one(&state.db).await.unwrap();
    assert_eq!(key, "starter:u_card", "幂等键须可预测，运营补发才能对齐");
    let status: String = sqlx::query_scalar("SELECT status FROM subplot_cards WHERE owner_id = 'u_card'")
        .fetch_one(&state.db).await.unwrap();
    assert_eq!(status, "owned");

    // 重复领取：外层 onboarding_grants 主键先挡下，内层 (owner_id, grant_key) 是第二道。
    let (st2, _) = post_json(&app, "/api/me/onboarding/gift", &tok, None, json!({})).await;
    assert_eq!(st2, StatusCode::OK, "重复领取应幂等返回既有礼包");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subplot_cards WHERE owner_id = 'u_card'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(n, 1, "重复领取绝不能多出第二张卡");
}
