//! S4 集成测试（sqlite::memory + oneshot）：开局装配 / 跨世界背包 / 服务端权威 carry / 章节结算。
//! 覆盖点：装配 per-character 钩子绑定执念 + 结局阵容加权 + 主场标注；carry 越权→risk_event；
//! admission 落地背包状态；grant_item 入包；章节 start 装配 → finish 兑现隐藏道具 + 离线夹层（幂等不二次发货）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::any::AnyPoolOptions;
use sqlx::Row;
use tower::ServiceExt;

use crate::admission::{ItemDefinition, ItemOrigin};
use crate::app::{build_router, AppState};
use crate::assembly::assemble_instance;
use crate::backpack::grant_item;
use crate::config::ServerConfig;
use crate::db::{new_id, now_ms};
use crate::worlds::{create_world, CreateWorldParams};

use muse_engine::character::types::*;

// ---------- 脚手架 ----------

static INIT: std::sync::Once = std::sync::Once::new();

fn test_config() -> ServerConfig {
    ServerConfig {
        database_url: "sqlite::memory:".into(),
        bind_addr: "127.0.0.1:0".into(),
        jwt_secret: "test-secret".into(),
        access_ttl_secs: 3600,
        refresh_ttl_secs: 100_000,
        dev_mode: true,
        object_store_dir: std::env::temp_dir().join(new_id("muse-s4")).to_string_lossy().into_owned(),
    }
}

async fn test_state() -> AppState {
    INIT.call_once(sqlx::any::install_default_drivers);
    let pool = AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, test_config())
}

fn token(state: &AppState, user_id: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, user_id, "user", 3600).unwrap()
}

async fn seed_user(state: &AppState, id: &str) {
    sqlx::query("INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) VALUES (?, '', 1, 'active', ?, ?)")
        .bind(id)
        .bind(now_ms())
        .bind(now_ms())
        .execute(&state.db)
        .await
        .unwrap();
}

/// 造一张卡：可设核心恐惧 / 剧情种子 / 来源作品 / 是否谋略型（影响结局加权）。
fn make_card(id: &str, name: &str, fear: &str, seeds: &[&str], source: Option<(&str, &str)>, strategist: bool) -> String {
    let card = CharacterCardV2 {
        schema_version: 2,
        id: id.into(),
        lifecycle: CardLifecycle::Ready,
        identity: Identity {
            name: name.into(),
            source_work: source.map(|(sid, title)| SourceWork {
                source_id: sid.into(),
                title: title.into(),
                version: None,
            }),
            ..Default::default()
        },
        dramatic_core: DramaticCore { core_fear: fear.into(), ..Default::default() },
        decision_model: if strategist {
            DecisionModel {
                value_priorities: vec!["以谋略取胜".into()],
                risk_appetite: "善于算计布局".into(),
                ..Default::default()
            }
        } else {
            Default::default()
        },
        perception: Default::default(),
        emotion_dynamics: Default::default(),
        relation_grammar: Default::default(),
        expression_fingerprint: Default::default(),
        agency: Agency { plot_seeds: seeds.iter().map(|s| s.to_string()).collect(), ..Default::default() },
        growth_arc: Default::default(),
        world_adaptation: Default::default(),
        evidence_index: Default::default(),
        revision: 1,
        created_at: 0,
        updated_at: 0,
    };
    serde_json::to_string(&card).unwrap()
}

async fn seed_char(state: &AppState, id: &str, owner: &str, card_json: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, rights_declaration, moderation, withdrawn, created_at) \
         VALUES (?, ?, 'local', 1, ?, 'original', 'approved', 0, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(card_json)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_template(state: &AppState, id: &str, room_type: &str, skeleton_json: &str, admission_json: &str) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES (?, '测试模板', ?, ?, ?, 1, 1, 'approved', ?)",
    )
    .bind(id)
    .bind(room_type)
    .bind(skeleton_json)
    .bind(admission_json)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn seed_member(state: &AppState, world_id: &str, user_id: &str, char_id: &str) {
    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
         VALUES (?, ?, ?, ?, '{}', 'active', ?)",
    )
    .bind(new_id("wm"))
    .bind(world_id)
    .bind(user_id)
    .bind(char_id)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

async fn make_chapter_world(state: &AppState, template_id: &str) -> String {
    let mut p = CreateWorldParams::official(template_id, 1, "章节世界");
    p.room_type = "chapter".into();
    create_world(&state.db, p).await.unwrap()
}

fn item_def(id: &str, cosmology: &[&str], tier: u8) -> ItemDefinition {
    ItemDefinition {
        id: id.into(),
        narrative: format!("道具-{id}"),
        effect_tags: vec!["advantage:combat".into()],
        origin: ItemOrigin {
            world_template_id: "tpl".into(),
            cosmology: cosmology.iter().map(|s| s.to_string()).collect(),
            power_tier: tier,
        },
    }
}

/// 预审核骨架：主线 2 硬节点 / 结局池（谋略 vs 社交）/ 隐藏内容池（绑定「遗忘」+ 无关支线）/ 主场来源。
const CHAPTER_SKELETON: &str = r#"{
  "sourceWork": { "sourceId": "src_novel", "title": "测试小说" },
  "mainlineNodes": [ { "id": "n1", "fated": true }, { "id": "n2", "fated": false } ],
  "endingPool": [
    { "id": "ending_smart", "affinity": "strategist", "baseWeight": 0.6 },
    { "id": "ending_bond", "affinity": "social", "baseWeight": 0.3 }
  ],
  "hiddenContentPool": [
    { "id": "hc_abandon", "themes": ["遗忘", "孤独"], "template": "{name} 必须直面 {fear}。", "difficultyBase": 0.5,
      "rewardItem": { "id": "item_relic", "narrative": "记忆残片", "effectTags": ["info:reveal"],
        "origin": { "worldTemplateId": "tpl_chapter", "cosmology": ["myth"], "powerTier": 2 } } },
    { "id": "hc_generic", "themes": ["权力"], "template": "一段与阵容无关的支线。", "difficultyBase": 0.3 }
  ],
  "sideHookPool": [ { "id": "sh1", "themes": ["复仇"] } ],
  "assemblyRules": { "hiddenPerCharacter": 1, "endingWeightThreshold": 0.5 }
}"#;

async fn post(app: &axum::Router, uri: &str, token: &str, idem: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = idem {
        b = b.header("Idempotency-Key", k);
    }
    let req = b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn get(app: &axum::Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn count(db: &sqlx::AnyPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(db).await.unwrap()
}

async fn backpack_row(db: &sqlx::AnyPool, user: &str, item: &str) -> (String, Option<String>) {
    let row = sqlx::query("SELECT status, carried_world_id FROM backpacks WHERE user_id=? AND item_id=?")
        .bind(user)
        .bind(item)
        .fetch_one(db)
        .await
        .unwrap();
    (row.try_get::<String, _>("status").unwrap(), row.try_get::<Option<String>, _>("carried_world_id").unwrap())
}

// ---------- 装配：per-character 钩子绑定执念 + 结局阵容加权 + 主场标注 ----------

#[tokio::test]
async fn assemble_binds_hidden_content_to_obsession_and_weights_endings() {
    let state = test_state().await;
    seed_user(&state, "usrA").await;
    // 谋略型 + 恐惧「被遗忘」+ 来源=本书 → 应绑定 hc_abandon、启用 ending_smart、获主场标注。
    let card = make_card("chA", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chA", "usrA", &card).await;
    seed_template(&state, "tpl_chapter", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_chapter").await;
    seed_member(&state, &wid, "usrA", "chA").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();

    // per-character 钩子绑定执念：选中 hc_abandon（而非无关的 hc_generic），文本内含执念词。
    assert_eq!(assembled.per_character_hooks.len(), 1, "每角色应恰得 1 个钩子");
    let hook = &assembled.per_character_hooks[0];
    assert_eq!(hook.character_id, "chA");
    assert_eq!(hook.pool_item_id, "hc_abandon", "应选中绑定执念的隐藏内容而非无关支线");
    assert!(hook.parameterized_text.contains("遗忘"), "参数化文本应嵌入绑定的执念: {}", hook.parameterized_text);
    assert!(hook.parameterized_text.contains("苏未央"), "应填充角色名");
    assert!(hook.difficulty_score > 0.6, "绑定执念应抬高难度分: {}", hook.difficulty_score);
    assert!(hook.reward_item.is_some(), "隐藏内容应携带预审核池中的奖励道具");

    // 结局阵容加权：全员谋略 → ending_smart 启用，社交向 ending_bond 低于阈值不启用。
    assert!(assembled.enabled_endings.contains(&"ending_smart".to_string()), "谋略阵容应启用智斗结局");
    assert!(!assembled.enabled_endings.contains(&"ending_bond".to_string()), "社交结局权重不足不应启用");

    // 主场优劣势：本书角色获预知知识包 + 原作宿命硬节点标注。
    assert_eq!(assembled.home_advantages.len(), 1);
    let ha = &assembled.home_advantages[0];
    assert_eq!(ha.character_id, "chA");
    assert!(ha.prescience_pack);
    assert_eq!(ha.fated_nodes, vec!["n1".to_string()], "宿命硬节点应为标 fated 的主线节点");

    // 装配结果写入 worlds.assembled_json 并钉住（含派生 templateVersion）。
    let raw: String = sqlx::query("SELECT assembled_json FROM worlds WHERE id = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("assembled_json")
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["assembly"]["perCharacterHooks"].as_array().unwrap().len(), 1);
    assert_eq!(v["templateVersion"], json!(1));
}

// ---------- 服务端权威：carry 越权 → risk_event，整单拒绝 ----------

#[tokio::test]
async fn carry_forged_unowned_item_records_risk_and_is_blocked() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_user(&state, "usrB").await;
    seed_template(&state, "tpl", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl").await;

    // 道具属于 B；A 抓包伪造背包清单试图携带 B 的道具。
    grant_item(&state.db, "usrB", &item_def("item_b", &["magic"], 2), "w0").await.unwrap();

    let (st, _) = post(
        &app,
        &format!("/api/worlds/{wid}/carry"),
        &token(&state, "usrA"),
        None,
        json!({ "itemIds": ["item_b"] }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "携带非本人物品应被风控拦截");

    // 记一条 forged_state 风控事件。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM risk_events WHERE kind='forged_state'").await, 1);
    // B 的道具状态未被篡改（仍 owned，未被 A 携带）。
    let (b_status, _) = backpack_row(&state.db, "usrB", "item_b").await;
    assert_eq!(b_status, "owned");
}

// ---------- carry：admission 按策略落地背包状态 ----------

#[tokio::test]
async fn carry_applies_admission_decisions_per_policy() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    // 魔法世界拒收 tech 体系（denylist），拒收留背包。
    seed_template(&state, "tpl", "chapter", CHAPTER_SKELETON, r#"{"mode":"denylist","cosmologies":["tech"],"rejectedHandling":"stay_in_backpack"}"#).await;
    let wid = make_chapter_world(&state, "tpl").await;

    grant_item(&state.db, "usrA", &item_def("magic_wand", &["magic"], 2), "w0").await.unwrap();
    grant_item(&state.db, "usrA", &item_def("laser_gun", &["tech"], 2), "w0").await.unwrap();

    let (st, body) = post(
        &app,
        &format!("/api/worlds/{wid}/carry"),
        &token(&state, "usrA"),
        Some("carry-1"),
        json!({ "itemIds": ["magic_wand", "laser_gun"] }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let results = body["results"].as_array().unwrap();
    let decision = |id: &str| {
        results.iter().find(|r| r["itemId"] == id).map(|r| r["decision"].as_str().unwrap().to_string()).unwrap()
    };
    assert_eq!(decision("magic_wand"), "admitted");
    assert_eq!(decision("laser_gun"), "rejected");

    // 落地背包状态：magic 入场（carried + carriedWorldId），tech 留背包（owned）。
    let (m_status, m_world) = backpack_row(&state.db, "usrA", "magic_wand").await;
    assert_eq!(m_status, "carried");
    assert_eq!(m_world.as_deref(), Some(wid.as_str()));
    let (t_status, t_world) = backpack_row(&state.db, "usrA", "laser_gun").await;
    assert_eq!(t_status, "owned");
    assert!(t_world.is_none(), "被拒物品不入场");

    // 幂等重放：同 key 同载荷 → 相同响应。
    let (st2, body2) = post(
        &app,
        &format!("/api/worlds/{wid}/carry"),
        &token(&state, "usrA"),
        Some("carry-1"),
        json!({ "itemIds": ["magic_wand", "laser_gun"] }),
    )
    .await;
    assert_eq!(st2, StatusCode::OK);
    assert_eq!(body2, body);
}

#[tokio::test]
async fn carry_translate_mode_marks_translated_and_carries() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    // translate 世界：tech 被拒 → 转译入场。
    seed_template(&state, "tpl", "chapter", CHAPTER_SKELETON, r#"{"mode":"denylist","cosmologies":["tech"],"maxPowerTier":3,"rejectedHandling":"translate"}"#).await;
    let wid = make_chapter_world(&state, "tpl").await;
    grant_item(&state.db, "usrA", &item_def("laser_gun", &["tech"], 5), "w0").await.unwrap();

    let (st, body) = post(
        &app,
        &format!("/api/worlds/{wid}/carry"),
        &token(&state, "usrA"),
        None,
        json!({ "itemIds": ["laser_gun"] }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["results"][0]["decision"], "translated");
    assert_eq!(body["results"][0]["carried"], json!(true));
    // 降档不升：powerTier 5 → 夹到上限 3；effectTags 不变（转译不成为强度后门）。
    assert_eq!(body["results"][0]["translatedTo"]["powerTier"], json!(3));
    assert_eq!(body["results"][0]["translatedTo"]["effectTags"][0], "advantage:combat");
}

// ---------- grant_item 唯一写入路径 → 背包可见 ----------

#[tokio::test]
async fn grant_item_writes_backpack_and_lists_in_my_backpack() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;

    let bp_id = grant_item(&state.db, "usrA", &item_def("relic", &["myth"], 3), "w_src").await.unwrap();
    assert!(bp_id.starts_with("bp_"));

    let (st, body) = get(&app, "/api/me/backpack", &token(&state, "usrA")).await;
    assert_eq!(st, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["item"]["id"], "relic");
    assert_eq!(items[0]["item"]["origin"]["cosmology"][0], "myth");
    assert_eq!(items[0]["item"]["origin"]["powerTier"], json!(3));
    assert_eq!(items[0]["acquiredWorldId"], "w_src");
    assert_eq!(items[0]["status"], "owned");
}

// ---------- 章节：start 装配 → finish 兑现隐藏道具 + 离线夹层（幂等不二次发货） ----------

#[tokio::test]
async fn chapter_start_assembles_and_finish_grants_reward_and_offline() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    let card = make_card("chA", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chA", "usrA", &card).await;
    seed_template(&state, "tpl_chapter", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_chapter").await;
    seed_member(&state, &wid, "usrA", "chA").await;
    let ta = token(&state, "usrA");

    // start：触发一次性装配 + 会话驱动 tick。
    let (st, body) = post(&app, &format!("/api/worlds/{wid}/chapters/start"), &ta, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["hookCount"], json!(1));
    assert!(body["enabledEndings"].as_array().unwrap().iter().any(|e| e == "ending_smart"));
    // 世界已转 running（会话驱动）。
    let wstatus: String = sqlx::query("SELECT status FROM worlds WHERE id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("status")
        .unwrap();
    assert_eq!(wstatus, "running");

    // finish：主线推进 + 兑现隐藏道具 item_relic + 离线夹层启动。
    let (stf, bf) = post(&app, &format!("/api/worlds/{wid}/chapters/finish"), &ta, None, json!({})).await;
    assert_eq!(stf, StatusCode::OK, "{bf}");
    assert_eq!(bf["advancedTo"], json!(1));
    assert_eq!(bf["totalNodes"], json!(2));
    assert_eq!(bf["cleared"], json!(false));
    let granted = bf["grantedItems"].as_array().unwrap();
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0]["itemId"], "item_relic");

    // 隐藏道具经 grant_item 进入账号背包。
    let (_, bp) = get(&app, "/api/me/backpack", &ta).await;
    assert_eq!(bp["items"].as_array().unwrap().len(), 1);
    assert_eq!(bp["items"][0]["item"]["id"], "item_relic");

    // 离线夹层收益可领取。
    let (sto, go) = get(&app, &format!("/api/worlds/{wid}/offline-gains"), &ta).await;
    assert_eq!(sto, StatusCode::OK);
    assert_eq!(go["gains"].as_array().unwrap().len(), 1);
    assert_eq!(go["characterId"], "chA");

    // 再次 finish：主线继续推进至通关，但隐藏道具不二次发货（grantedHookIds 幂等）。
    let (stf2, bf2) = post(&app, &format!("/api/worlds/{wid}/chapters/finish"), &ta, None, json!({})).await;
    assert_eq!(stf2, StatusCode::OK, "{bf2}");
    assert_eq!(bf2["advancedTo"], json!(2));
    assert_eq!(bf2["cleared"], json!(true), "推进至最后硬节点应判通关");
    assert_eq!(bf2["grantedItems"].as_array().unwrap().len(), 0, "已兑现钩子不应二次发货");

    // 背包仍只有 1 件 item_relic（无重复入包）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM backpacks WHERE user_id='usrA' AND item_id='item_relic'").await, 1);
}

#[tokio::test]
async fn chapter_endpoints_reject_non_chapter_room() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_char(&state, "chA", "usrA", &make_card("chA", "甲", "恐惧", &[], None, false)).await;
    // idle 房：章节端点不生效（功能开关）。
    seed_template(&state, "tpl_idle", "idle", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let mut p = CreateWorldParams::official("tpl_idle", 1, "放置世界");
    p.room_type = "idle".into();
    let wid = create_world(&state.db, p).await.unwrap();
    seed_member(&state, &wid, "usrA", "chA").await;

    let (st, _) = post(&app, &format!("/api/worlds/{wid}/chapters/start"), &token(&state, "usrA"), None, json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "非章节房应拒绝（room_type 开关）");
}
