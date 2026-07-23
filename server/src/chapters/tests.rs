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

/// Phase 0：reward_item_ref 从 world_items 目录解引用填 CharacterHook.reward_item（单一事实源）；
/// ref 优先于内联 reward_item，悬空 ref 退回内联 fallback。
#[tokio::test]
async fn reward_item_ref_dereferences_world_items_catalog() {
    // 骨架：world_items 目录 + 两个池物品（ref 命中目录 / 仅内联 fallback）。命中执念的排前。
    const SKELETON_REF: &str = r#"{
      "worldItems": [
        { "id": "wi_relic", "narrative": "目录里的记忆残片", "effectTags": ["info:reveal"],
          "origin": { "worldTemplateId": "tpl_ref", "cosmology": ["myth"], "powerTier": 3 } }
      ],
      "hiddenContentPool": [
        { "id": "hc_ref", "themes": ["遗忘"], "template": "{name} 直面 {fear}。", "difficultyBase": 0.5,
          "rewardItemRef": "wi_relic",
          "rewardItem": { "id": "wi_inline_should_lose", "narrative": "内联应被覆盖", "effectTags": [],
            "origin": { "worldTemplateId": "tpl_ref", "cosmology": ["myth"], "powerTier": 1 } } }
      ],
      "assemblyRules": { "hiddenPerCharacter": 1, "endingWeightThreshold": 0.5 }
    }"#;

    let state = test_state().await;
    seed_user(&state, "usrRef").await;
    let card = make_card("chRef", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], None, true);
    seed_char(&state, "chRef", "usrRef", &card).await;
    seed_template(&state, "tpl_ref", "chapter", SKELETON_REF, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_ref").await;
    seed_member(&state, &wid, "usrRef", "chRef").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();
    assert_eq!(assembled.per_character_hooks.len(), 1);
    let hook = &assembled.per_character_hooks[0];
    assert_eq!(hook.pool_item_id, "hc_ref");
    let reward = hook.reward_item.as_ref().expect("reward_item_ref 应解引用出目录道具");
    // ref 命中目录：填目录里的道具，而非内联 fallback。
    assert_eq!(reward.id, "wi_relic", "应取 world_items 目录条目而非内联");
    assert_eq!(reward.narrative, "目录里的记忆残片");
    assert_eq!(reward.origin.power_tier, 3);
}

/// Phase 0：无 world_items 目录 / 无 reward_item_ref 时退化为旧行为——内联 reward_item 直填。
#[tokio::test]
async fn inline_reward_item_survives_without_catalog() {
    let state = test_state().await;
    seed_user(&state, "usrInline").await;
    let card = make_card("chInline", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chInline", "usrInline", &card).await;
    // 复用原骨架（无 worldItems、hc_abandon 仅内联 rewardItem）。
    seed_template(&state, "tpl_inline", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_inline").await;
    seed_member(&state, &wid, "usrInline", "chInline").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();
    let hook = &assembled.per_character_hooks[0];
    let reward = hook.reward_item.as_ref().expect("内联 reward_item 应保留");
    assert_eq!(reward.id, "item_relic", "无目录时退回内联道具");
}

/// Phase 1：world_characters 解引用 + 机审 → worldCharacterEntries 钉入 assembled_json；
/// 携带道具从 world_items 目录解引用（不进 per_character_hooks，NPC 无 owner）。
#[tokio::test]
async fn world_characters_assemble_into_entries_and_pin() {
    let state = test_state().await;
    seed_user(&state, "usrW").await;
    let card = make_card("chW", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], None, true);
    seed_char(&state, "chW", "usrW", &card).await;

    // NPC（反派）卡 + world_items 目录（NPC 携带道具引用）。
    let npc_card: Value =
        serde_json::from_str(&make_card("npc_villain", "厉无咎", "夺权", &["布局朝堂"], None, true)).unwrap();
    let skeleton = json!({
        "worldItems": [
            { "id": "wi_seal", "narrative": "调兵虎符", "effectTags": ["advantage:combat"],
              "origin": { "worldTemplateId": "tpl_npc", "cosmology": ["mundane"], "powerTier": 3 } }
        ],
        "worldCharacters": [
            { "card": npc_card, "homeLocation": "朝堂", "carriedItemIds": ["wi_seal", "wi_missing"],
              "agendaNodes": ["n1"] }
        ],
        "assemblyRules": { "hiddenPerCharacter": 1, "endingWeightThreshold": 0.5 }
    });
    seed_template(&state, "tpl_npc", "chapter", &skeleton.to_string(), r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_npc").await;
    seed_member(&state, &wid, "usrW", "chW").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();

    // 反派 NPC 装配为一条 worldCharacterEntry（机审通过）。
    assert_eq!(assembled.world_character_entries.len(), 1, "反派 NPC 应装配为一条 worldCharacterEntry");
    let e = &assembled.world_character_entries[0];
    assert_eq!(e.character_id, "npc_villain");
    assert_eq!(e.card.identity.name, "厉无咎");
    assert_eq!(e.location, "朝堂");
    // 携带道具从 world_items 目录解引用：命中 wi_seal，悬空 wi_missing 静默丢弃。
    assert_eq!(e.carried_items.len(), 1, "仅命中目录的携带道具被解引用");
    assert_eq!(e.carried_items[0].id, "wi_seal");
    // NPC 不进 per_character_hooks（那是玩家个性化钩子）。
    assert!(!assembled.per_character_hooks.iter().any(|h| h.character_id == "npc_villain"));

    // 钉入 assembled_json（随实例持久化，runtime 每 tick 读回注入）。
    let raw: String = sqlx::query("SELECT assembled_json FROM worlds WHERE id = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("assembled_json")
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    let entries = v["assembly"]["worldCharacterEntries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["characterId"], "npc_villain");
    assert_eq!(entries[0]["location"], "朝堂");
}

/// Phase 2：skeleton.locations → AssembledInstance.location_graph 钉入 assembled_json（含秘境 gate），
/// runtime 每 tick 读回组装引擎 RoundInput.locations。
#[tokio::test]
async fn locations_assemble_into_location_graph_and_pin() {
    let state = test_state().await;
    seed_user(&state, "usrL").await;
    let card = make_card("chL", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], None, true);
    seed_char(&state, "chL", "usrL", &card).await;

    let skeleton = json!({
        "locations": [
            { "id": "hall", "name": "前厅", "connections": ["secret"] },
            { "id": "secret", "name": "密室", "connections": ["hall"], "isSecretRealm": true,
              "gate": { "requiredItemIds": ["jade_key"], "maxPowerTier": 3 } }
        ],
        "assemblyRules": { "hiddenPerCharacter": 1, "endingWeightThreshold": 0.5 }
    });
    seed_template(&state, "tpl_loc", "chapter", &skeleton.to_string(), r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_loc").await;
    seed_member(&state, &wid, "usrL", "chL").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();
    assert_eq!(assembled.location_graph.len(), 2, "两地点应钉入 location_graph");
    let secret = assembled.location_graph.iter().find(|l| l.id == "secret").unwrap();
    assert!(secret.is_secret_realm, "秘境标记应保留");
    let gate = secret.gate.as_ref().expect("秘境应带 gate");
    assert_eq!(gate.required_item_ids, vec!["jade_key".to_string()]);
    assert_eq!(gate.max_power_tier, Some(3));

    // 钉入 assembled_json（随实例持久化，runtime 每 tick 读回）。
    let raw: String = sqlx::query("SELECT assembled_json FROM worlds WHERE id = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("assembled_json")
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    let graph = v["assembly"]["locationGraph"].as_array().unwrap();
    assert_eq!(graph.len(), 2);
    assert_eq!(graph[0]["id"], "hall");
    assert_eq!(graph[0]["connections"][0], "secret");
}

/// Phase 3：地点 residentItemIds → 从 world_items 目录解引用为 resident_items 分布（秘境隐藏道具单一事实源）；
/// 悬空 id 静默丢弃；无驻留道具的地点不产组。location_graph（引擎 LocationDef）丢弃 residentItemIds。
#[tokio::test]
async fn resident_items_distribute_from_world_items_catalog() {
    let state = test_state().await;
    seed_user(&state, "usrR").await;
    let card = make_card("chR", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], None, true);
    seed_char(&state, "chR", "usrR", &card).await;

    let skeleton = json!({
        "worldItems": [
            { "id": "wi_orb", "narrative": "混沌珠", "effectTags": ["advantage:magic"],
              "origin": { "worldTemplateId": "tpl_res", "cosmology": ["cultivation"], "powerTier": 4 } }
        ],
        "locations": [
            { "id": "hall", "name": "前厅", "connections": ["secret"] },
            { "id": "secret", "name": "秘境", "connections": ["hall"], "isSecretRealm": true,
              "gate": { "requiredItemIds": ["wi_orb"] },
              "residentItemIds": ["wi_orb", "wi_missing"] }
        ],
        "assemblyRules": { "hiddenPerCharacter": 1, "endingWeightThreshold": 0.5 }
    });
    seed_template(&state, "tpl_res", "chapter", &skeleton.to_string(), r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_res").await;
    seed_member(&state, &wid, "usrR", "chR").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();
    // 只有秘境有驻留道具组（前厅无 → 不产组）；悬空 wi_missing 丢弃，仅解出 wi_orb。
    assert_eq!(assembled.resident_items.len(), 1, "仅秘境产驻留道具组");
    let grp = &assembled.resident_items[0];
    assert_eq!(grp.location_id, "secret");
    assert!(grp.is_secret_realm, "秘境驻留道具标记为隐藏道具");
    assert_eq!(grp.items.len(), 1, "悬空 residentItemId 应静默丢弃");
    assert_eq!(grp.items[0].id, "wi_orb");
    assert_eq!(grp.items[0].origin.power_tier, 4);
    // location_graph（引擎 LocationDef）保留结构字段，丢弃 residentItemIds。
    let secret = assembled.location_graph.iter().find(|l| l.id == "secret").unwrap();
    assert!(secret.is_secret_realm);
    assert_eq!(secret.gate.as_ref().unwrap().required_item_ids, vec!["wi_orb".to_string()]);

    // 钉入 assembled_json（随实例持久化）。
    let raw: String = sqlx::query("SELECT assembled_json FROM worlds WHERE id = ?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .try_get("assembled_json")
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    let groups = v["assembly"]["residentItems"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["locationId"], "secret");
    assert_eq!(groups[0]["items"][0]["id"], "wi_orb");
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
    // 携带随入场：本人角色须在场（Low 加固：carry 成员/世界态校验）。
    seed_char(&state, "chA", "usrA", &make_card("chA", "甲", "恐惧", &[], None, false)).await;
    seed_member(&state, &wid, "usrA", "chA").await;

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
    seed_char(&state, "chA", "usrA", &make_card("chA", "甲", "恐惧", &[], None, false)).await;
    seed_member(&state, &wid, "usrA", "chA").await;
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

// ---------- C-3：并发 finish 只发一次货（事务 + state_revision CAS + DB 唯一约束） ----------

#[tokio::test]
async fn concurrent_finish_grants_reward_exactly_once() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    let card = make_card("chA", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chA", "usrA", &card).await;
    seed_template(&state, "tpl_chapter", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_chapter").await;
    seed_member(&state, &wid, "usrA", "chA").await;
    let ta = token(&state, "usrA");

    // start：一次性装配。
    let (st, _) = post(&app, &format!("/api/worlds/{wid}/chapters/start"), &ta, None, json!({})).await;
    assert_eq!(st, StatusCode::OK);

    // 两个并发 finish（无幂等键，均越过 guard）：竞争兑现同一隐藏道具。
    let uri = format!("/api/worlds/{wid}/chapters/finish");
    let f1 = post(&app, &uri, &ta, None, json!({}));
    let f2 = post(&app, &uri, &ta, None, json!({}));
    let ((s1, b1), (s2, b2)) = tokio::join!(f1, f2);
    assert_eq!(s1, StatusCode::OK, "{b1}");
    assert_eq!(s2, StatusCode::OK, "{b2}");

    // 跨两个并发响应，item_relic 至多被发一次（另一方为空发货）。
    let granted_count = |b: &Value| -> usize {
        b["grantedItems"].as_array().map(|a| a.iter().filter(|g| g["itemId"] == "item_relic").count()).unwrap_or(0)
    };
    assert_eq!(
        granted_count(&b1) + granted_count(&b2),
        1,
        "并发 finish 隐藏道具只应发一次货: b1={b1} b2={b2}"
    );

    // DB 权威：item_relic 背包行恰好 1 条（资产复制被下沉到 (user_id, reward_hook_key) 唯一约束堵死）。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM backpacks WHERE user_id='usrA' AND item_id='item_relic'").await,
        1,
        "并发/重放 finish 只入包一次"
    );

    // 幂等重放（第三次 finish）仍不二次发货。
    let (st3, b3) = post(&app, &format!("/api/worlds/{wid}/chapters/finish"), &ta, None, json!({})).await;
    assert_eq!(st3, StatusCode::OK, "{b3}");
    assert_eq!(b3["grantedItems"].as_array().unwrap().len(), 0, "已兑现不二次发货");
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM backpacks WHERE user_id='usrA' AND item_id='item_relic'").await,
        1
    );
}

// ---------- S-5：carry 转译降档持久化到 backpacks 覆盖列（未来仲裁读覆盖值） ----------

#[tokio::test]
async fn carry_translate_persists_downgrade_override_and_clears_on_readmit() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    // world1：denylist tech + maxPowerTier=3 + translate → tech tier5 转译降档到 3。
    seed_template(&state, "tpl_t", "chapter", CHAPTER_SKELETON, r#"{"mode":"denylist","cosmologies":["tech"],"maxPowerTier":3,"rejectedHandling":"translate"}"#).await;
    let w1 = make_chapter_world(&state, "tpl_t").await;
    seed_char(&state, "chA", "usrA", &make_card("chA", "甲", "恐惧", &[], None, false)).await;
    seed_member(&state, &w1, "usrA", "chA").await;
    grant_item(&state.db, "usrA", &item_def("laser_gun", &["tech"], 5), "w0").await.unwrap();

    let (st, body) = post(&app, &format!("/api/worlds/{w1}/carry"), &token(&state, "usrA"), None, json!({ "itemIds": ["laser_gun"] })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["results"][0]["decision"], "translated");

    // 覆盖列已落库：powerTier 降档到 3，effectTags 快照持久化（不再只进响应）。
    let row = sqlx::query("SELECT power_tier_override, effect_tags_override FROM backpacks WHERE user_id='usrA' AND item_id='laser_gun'")
        .fetch_one(&state.db).await.unwrap();
    assert_eq!(row.try_get::<Option<i64>, _>("power_tier_override").unwrap(), Some(3), "转译降档 powerTier 应持久化为 3");
    let et: Option<String> = row.try_get("effect_tags_override").unwrap();
    assert!(et.as_deref().unwrap_or("").contains("advantage:combat"), "effectTags 覆盖应落库: {et:?}");

    // 再携带进一个放行世界（open 全收，无强度上限）→ admitted，覆盖列清 NULL（不残留历史降档）。
    seed_template(&state, "tpl_open", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let w2 = make_chapter_world(&state, "tpl_open").await;
    seed_member(&state, &w2, "usrA", "chA").await;
    let (st2, body2) = post(&app, &format!("/api/worlds/{w2}/carry"), &token(&state, "usrA"), None, json!({ "itemIds": ["laser_gun"] })).await;
    assert_eq!(st2, StatusCode::OK, "{body2}");
    assert_eq!(body2["results"][0]["decision"], "admitted");
    let pt: Option<i64> = sqlx::query("SELECT power_tier_override FROM backpacks WHERE user_id='usrA' AND item_id='laser_gun'")
        .fetch_one(&state.db).await.unwrap().try_get("power_tier_override").unwrap();
    assert_eq!(pt, None, "非转译入场应清除历史降档覆盖");
}

// ---------- S-5 / Low：carry 需世界可加入态 + 本人在场成员 ----------

#[tokio::test]
async fn carry_requires_active_membership() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    seed_template(&state, "tpl", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl").await;
    grant_item(&state.db, "usrA", &item_def("magic_wand", &["magic"], 2), "w0").await.unwrap();

    // 非成员携带自有物品 → 403（携带随入场，须先在场）。
    let (st, _) = post(&app, &format!("/api/worlds/{wid}/carry"), &token(&state, "usrA"), None, json!({ "itemIds": ["magic_wand"] })).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "非成员不得携带");

    // 成为在场成员后 → 放行。
    seed_char(&state, "chA", "usrA", &make_card("chA", "甲", "恐惧", &[], None, false)).await;
    seed_member(&state, &wid, "usrA", "chA").await;
    let (st2, body2) = post(&app, &format!("/api/worlds/{wid}/carry"), &token(&state, "usrA"), None, json!({ "itemIds": ["magic_wand"] })).await;
    assert_eq!(st2, StatusCode::OK, "{body2}");
    assert_eq!(body2["results"][0]["decision"], "admitted");
}

// ---------- C-7：首次装配并发保护——第二次装配不覆盖已推进的 chapterState ----------

#[tokio::test]
async fn duplicate_assembly_preserves_chapter_state() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrA").await;
    let card = make_card("chA", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chA", "usrA", &card).await;
    seed_template(&state, "tpl_chapter", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_chapter").await;
    seed_member(&state, &wid, "usrA", "chA").await;
    let ta = token(&state, "usrA");

    // start 装配 + finish 推进（currentNode→1，兑现 item_relic）。
    post(&app, &format!("/api/worlds/{wid}/chapters/start"), &ta, None, json!({})).await;
    let (stf, _) = post(&app, &format!("/api/worlds/{wid}/chapters/finish"), &ta, None, json!({})).await;
    assert_eq!(stf, StatusCode::OK);

    // 模拟并发/重复装配：再次 assemble_instance——C-7 修复后应「仅当 assembly 为 null 才写」，
    // 已装配则复用，不覆盖 → chapterState（currentNode/grantedHookIds）完好。
    let _ = assemble_instance(&state, &wid).await.unwrap();

    let raw: String = sqlx::query("SELECT assembled_json FROM worlds WHERE id=?")
        .bind(&wid).fetch_one(&state.db).await.unwrap().try_get("assembled_json").unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["chapterState"]["currentNode"], json!(1), "重复装配不得重置章节推进");
    assert_eq!(
        v["chapterState"]["grantedHookIds"].as_array().map(|a| a.len()).unwrap_or(0),
        1,
        "重复装配不得清空已兑现钩子集"
    );
    // 背包仍只有 1 件 item_relic（重复装配未导致后续 finish 二次发货）。
    let (stf2, bf2) = post(&app, &format!("/api/worlds/{wid}/chapters/finish"), &ta, None, json!({})).await;
    assert_eq!(stf2, StatusCode::OK, "{bf2}");
    assert_eq!(bf2["grantedItems"].as_array().unwrap().len(), 0);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM backpacks WHERE item_id='item_relic'").await, 1);
}

#[tokio::test]
async fn concurrent_assemble_instance_writes_single_consistent_assembly() {
    let state = test_state().await;
    seed_user(&state, "usrA").await;
    let card = make_card("chA", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chA", "usrA", &card).await;
    seed_template(&state, "tpl_chapter", "chapter", CHAPTER_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_chapter").await;
    seed_member(&state, &wid, "usrA", "chA").await;

    // 两个并发装配：CAS 占位保证只有一个落库，另一个复用同一结果。
    let (a1, a2) = tokio::join!(assemble_instance(&state, &wid), assemble_instance(&state, &wid));
    let a1 = a1.unwrap();
    let a2 = a2.unwrap();
    assert_eq!(a1.per_character_hooks.len(), 1);
    assert_eq!(a2.per_character_hooks.len(), 1);
    assert_eq!(a1.per_character_hooks[0].pool_item_id, a2.per_character_hooks[0].pool_item_id, "并发装配应返回一致实例");

    // DB 内恰好一份 assembly，且 chapterState 为初值（未被并发覆盖损坏）。
    let raw: String = sqlx::query("SELECT assembled_json FROM worlds WHERE id=?")
        .bind(&wid).fetch_one(&state.db).await.unwrap().try_get("assembled_json").unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["assembly"]["perCharacterHooks"].as_array().unwrap().len(), 1);
    assert_eq!(v["chapterState"]["currentNode"], json!(0));
}

// ---------- S-3：Pending（机审未过/注入命中）钩子不嵌入，换下一候选 ----------

const FLAGGED_SKELETON: &str = r#"{
  "sourceWork": { "sourceId": "src_novel", "title": "测试小说" },
  "mainlineNodes": [ { "id": "n1", "fated": true } ],
  "endingPool": [ { "id": "e1", "affinity": null, "baseWeight": 0.6 } ],
  "hiddenContentPool": [
    { "id": "hc_flagged", "themes": ["遗忘"], "template": "测试敏感词：{name} 面对 {fear}。", "difficultyBase": 0.5 },
    { "id": "hc_clean", "themes": ["遗忘"], "template": "{name} 静静面对 {fear}。", "difficultyBase": 0.3 }
  ],
  "assemblyRules": { "hiddenPerCharacter": 1 }
}"#;

#[tokio::test]
async fn pending_moderation_hook_is_skipped_for_next_candidate() {
    let state = test_state().await;
    seed_user(&state, "usrA").await;
    // 恐惧含「遗忘」→ 两个候选都命中主题；平手按池序 hc_flagged 排前。
    let card = make_card("chA", "苏未央", "害怕被遗忘", &[], Some(("src_novel", "测试小说")), false);
    seed_char(&state, "chA", "usrA", &card).await;
    seed_template(&state, "tpl_flagged", "chapter", FLAGGED_SKELETON, r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_flagged").await;
    seed_member(&state, &wid, "usrA", "chA").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();

    // hc_flagged 文本含 DevModeration 敏感词 → Pending → 不嵌入；换下一候选 hc_clean 过审嵌入。
    assert_eq!(assembled.per_character_hooks.len(), 1, "应只嵌入过审的那个候选");
    assert_eq!(assembled.per_character_hooks[0].pool_item_id, "hc_clean", "Pending 候选应被跳过，换下一候选");
    assert!(
        !assembled.per_character_hooks.iter().any(|h| h.pool_item_id == "hc_flagged"),
        "未复核（Pending）内容不得钉进实例"
    );

    // Pending 候选已进人审队列（moderate_and_queue 的分层管道），供后台复核。
    assert!(
        count(&state.db, "SELECT COUNT(*) FROM audit_queue WHERE subject_kind='assembly_hook' AND status='open'").await >= 1,
        "Pending 装配钩子应入人审队列"
    );
}

// ---------- 装配采样（防刷第二环）：DB 集成 ----------

/// 超集章节骨架：2 storylines / mainline 含 fated + 变体组 + 非 fated（模板全量 5，采样后 2）/ 3 NPC（采样 1）。
/// isSuperset + storylines + sampling 三判据满足 → 走种子采样。
fn superset_chapter_skeleton() -> String {
    let npc1: Value = serde_json::from_str(&make_card("npc-1", "厉无咎", "夺权", &["布局朝堂"], None, true)).unwrap();
    let npc2: Value = serde_json::from_str(&make_card("npc-2", "沈孤鸿", "背叛", &["归隐"], None, false)).unwrap();
    let npc3: Value = serde_json::from_str(&make_card("npc-3", "白清欢", "孤独", &["寻医"], None, false)).unwrap();
    json!({
        "sourceWork": { "sourceId": "src_novel", "title": "测试小说" },
        "isSuperset": true,
        "storylines": [
            { "id": "arc-1", "affinity": "strategist", "mainlineNodeIds": ["mn-fate","mn-x1","mn-x2","mn-y"], "hiddenPoolIds": ["hc-1","hc-2"], "endingIds": ["end-1"] },
            { "id": "arc-2", "affinity": "social",     "mainlineNodeIds": ["mn-z"],                            "hiddenPoolIds": ["hc-3"],       "endingIds": ["end-2"] }
        ],
        "mainlineNodes": [
            { "id": "mn-fate", "fated": true, "arcTags": ["arc-1","arc-2"] },
            { "id": "mn-x1", "variantGroup": "vgx", "arcTags": ["arc-1"] },
            { "id": "mn-x2", "variantGroup": "vgx", "arcTags": ["arc-1"] },
            { "id": "mn-y", "arcTags": ["arc-1"] },
            { "id": "mn-z", "arcTags": ["arc-2"] }
        ],
        "hiddenContentPool": [
            { "id": "hc-1", "themes": ["遗忘"], "template": "{name} 直面 {fear}", "arcTags": ["arc-1"] },
            { "id": "hc-2", "themes": ["权谋"], "template": "静室布局", "arcTags": ["arc-1"] },
            { "id": "hc-3", "themes": ["情谊"], "template": "月下结盟", "arcTags": ["arc-2"] }
        ],
        "endingPool": [
            { "id": "end-1", "affinity": "strategist", "baseWeight": 1.0, "arcTags": ["arc-1"] },
            { "id": "end-2", "affinity": "social",     "baseWeight": 1.0, "arcTags": ["arc-2"] }
        ],
        "worldCharacters": [
            { "card": npc1, "agendaNodes": ["mn-fate"] },
            { "card": npc2, "agendaNodes": [] },
            { "card": npc3, "agendaNodes": [] }
        ],
        "sampling": { "instanceStorylineCount": 1, "instanceMainlineCount": 1, "instanceHiddenCount": 1, "instanceNpcCount": 1 }
    })
    .to_string()
}

/// 采样钉住：超集实例产出采样审计段（seed + 被选子集），主线收窄到 2（fated + 1 非 fated），NPC 收窄到 1。
#[tokio::test]
async fn superset_instance_pins_sampling_and_narrows_pools() {
    let state = test_state().await;
    seed_user(&state, "usrS").await;
    let card = make_card("chS", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chS", "usrS", &card).await;
    seed_template(&state, "tpl_superset", "chapter", &superset_chapter_skeleton(), r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_superset").await;
    seed_member(&state, &wid, "usrS", "chS").await;

    let assembled = assemble_instance(&state, &wid).await.unwrap();
    let s = assembled.sampling.as_ref().expect("超集实例必须产出采样审计段");

    // 种子：16 位十六进制，非零。
    assert_eq!(s.seed.len(), 16, "seed 应为 u64 十六进制");
    assert_ne!(s.seed, "0000000000000000");
    assert!(!s.roster_fingerprint.is_empty());

    // 主线：fated 必留 + 计数收窄。模板全量 5 → 采样后 2（mn-fate + 1 非 fated）。
    assert!(s.selected_mainline.contains(&"mn-fate".to_string()), "fated 硬节点必留");
    assert_eq!(s.selected_mainline.len(), 2, "采样后主线 = fated + 1 非 fated（模板全量 5）: {:?}", s.selected_mainline);

    // NPC：3 → 采样 1；仅被选 NPC 装配为 worldCharacterEntries。
    assert_eq!(s.selected_npcs.len(), 1, "NPC 采样收窄到 1: {:?}", s.selected_npcs);
    assert_eq!(assembled.world_character_entries.len(), 1, "仅被选 NPC 钉入 entries");
    assert_eq!(assembled.world_character_entries[0].character_id, s.selected_npcs[0]);

    // 隐藏内容：收窄到 1，且 ⊆ 所选 storyline。
    assert!(s.selected_hidden.len() <= 1, "隐藏内容采样 ≤ 1: {:?}", s.selected_hidden);

    // 钉入 assembled_json /assembly/sampling（服务端审计，随实例钉住）。
    let raw: String = sqlx::query("SELECT assembled_json FROM worlds WHERE id = ?")
        .bind(&wid).fetch_one(&state.db).await.unwrap().try_get("assembled_json").unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["assembly"]["sampling"]["seed"], json!(s.seed));
    assert!(v["assembly"]["sampling"]["selectedMainline"].as_array().unwrap().len() == 2);
}

/// 退出重进不重掷：CAS 输家读回既有实例，seed 与被选子集不变（replay 一致）。
#[tokio::test]
async fn superset_reassembly_replays_same_seed() {
    let state = test_state().await;
    seed_user(&state, "usrRe").await;
    let card = make_card("chRe", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chRe", "usrRe", &card).await;
    seed_template(&state, "tpl_replay", "chapter", &superset_chapter_skeleton(), r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_replay").await;
    seed_member(&state, &wid, "usrRe", "chRe").await;

    let a1 = assemble_instance(&state, &wid).await.unwrap();
    let a2 = assemble_instance(&state, &wid).await.unwrap(); // CAS 输家：读回既有实例，不重掷。
    let (s1, s2) = (a1.sampling.unwrap(), a2.sampling.unwrap());
    assert_eq!(s1.seed, s2.seed, "退出重进 seed 不变");
    assert_eq!(s1.selected_mainline, s2.selected_mainline, "replay 主线一致");
    assert_eq!(s1.selected_hidden, s2.selected_hidden, "replay 隐藏一致");
    assert_eq!(s1.selected_npcs, s2.selected_npcs, "replay NPC 一致");
    assert_eq!(s1.selected_locations, s2.selected_locations, "replay 地点一致");
}

/// 副本间不同：两个不同 world_id、同阵容同模板 → 服务端算出的种子不同（world_id 唯一 → 不可换卡/重进复现）。
#[tokio::test]
async fn superset_distinct_worlds_get_distinct_seeds() {
    let state = test_state().await;
    seed_user(&state, "usrD").await;
    let card = make_card("chD", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chD", "usrD", &card).await;
    seed_template(&state, "tpl_distinct", "chapter", &superset_chapter_skeleton(), r#"{"mode":"open"}"#).await;

    let w1 = make_chapter_world(&state, "tpl_distinct").await;
    seed_member(&state, &w1, "usrD", "chD").await;
    let w2 = make_chapter_world(&state, "tpl_distinct").await;
    seed_member(&state, &w2, "usrD", "chD").await;

    let s1 = assemble_instance(&state, &w1).await.unwrap().sampling.unwrap();
    let s2 = assemble_instance(&state, &w2).await.unwrap().sampling.unwrap();
    assert_ne!(s1.seed, s2.seed, "不同实例（world_id 唯一）→ 种子不同");
    // 阵容指纹相同（同一 roster），证明种子差异来自 world_id 而非阵容 → 不可换卡复现。
    assert_eq!(s1.roster_fingerprint, s2.roster_fingerprint, "同阵容指纹一致");
}

/// 下游生效：通关判定按被选主线数（2），而非模板全量（5）——否则采样后永不通关。
#[tokio::test]
async fn superset_clearance_uses_selected_mainline_count() {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user(&state, "usrC").await;
    let card = make_card("chC", "苏未央", "害怕被遗忘", &["寻找失散的姐姐"], Some(("src_novel", "测试小说")), true);
    seed_char(&state, "chC", "usrC", &card).await;
    seed_template(&state, "tpl_clear", "chapter", &superset_chapter_skeleton(), r#"{"mode":"open"}"#).await;
    let wid = make_chapter_world(&state, "tpl_clear").await;
    seed_member(&state, &wid, "usrC", "chC").await;
    let tc = token(&state, "usrC");

    // start（触发装配）。
    let (st, sbody) = post(&app, &format!("/api/worlds/{wid}/chapters/start"), &tc, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{sbody}");
    // 种子不外泄：start 响应不含 seed / sampling。
    let sbody_str = sbody.to_string();
    assert!(!sbody_str.contains("\"seed\""), "start 响应不得暴露 seed");
    assert!(!sbody_str.contains("sampling"), "start 响应不得暴露 sampling 段");

    // 第 1 次 finish：totalNodes = 被选主线数 2（非模板全量 5）；currentNode 0→1，未通关。
    let (st, f1) = post(&app, &format!("/api/worlds/{wid}/chapters/finish"), &tc, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{f1}");
    assert_eq!(f1["totalNodes"], json!(2), "通关判定应按被选主线数（2），而非模板全量（5）");
    assert_eq!(f1["advancedTo"], json!(1));
    assert_eq!(f1["cleared"], json!(false));

    // 第 2 次 finish：advancedTo 2 ≥ 2 → 通关。
    let (st, f2) = post(&app, &format!("/api/worlds/{wid}/chapters/finish"), &tc, None, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{f2}");
    assert_eq!(f2["advancedTo"], json!(2));
    assert_eq!(f2["cleared"], json!(true), "推进到被选主线数即通关");
}
