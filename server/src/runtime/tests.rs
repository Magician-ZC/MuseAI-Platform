//! S2 运行时联编集成测试（E-1 回归基线）：注入 mock ModelClient，走完整
//! run_round→commit_tick，断言状态跨 tick 累积、事件落库、实测 token 预算累计（B-1）、
//! 只消费本 tick 实际喂入的干预（Q-3）、CAS 冲突终态化（C-2）、原子认领幂等（C-1）。
//!
//! 这补上了此前 86 测试「全部在无模型跳过处提前返回、从不执行 run_round」的最大盲区。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use sqlx::any::AnyPoolOptions;
use sqlx::AnyPool;

use crate::app::AppState;
use crate::config::ServerConfig;
use crate::db::{new_id, now_ms};
use crate::runtime::{insert_tick, process_tick, process_tick_with_model, TickStatus};
use crate::worlds::{create_world, load_world, CreateWorldParams};

use muse_engine::character::types::{CardLifecycle, CharacterCardV2, Identity};
use muse_engine::host::CancelFlag;
use muse_engine::model::{ModelCallSpec, ModelClient, ModelOutput};
use muse_engine::narrative::types::{ConstraintLevel, NarrativeState, NodeStatus};
use muse_engine::EngineError;

// ---------- 可控 mock 模型路由 ----------

/// 环节感知的 mock ModelClient：按 spec.agent 返回合法 JSON，携带固定 input/output token。
/// 与调用顺序/次数解耦（比脚本化 Vec 更稳），可跨多 tick 复用。
struct MockModel {
    input_tokens: u32,
    output_tokens: u32,
}

#[async_trait]
impl ModelClient for MockModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"密室之中，烛火摇曳，两人对坐。"}"#,
            "roleDecide" => r#"{"intent":"观望","action":"上前拱手行礼","speak":{"willSpeak":true,"purpose":"寒暄"},"targets":[],"acceptableCosts":[],"predictions":[]}"#,
            "arbiter" => r#"{"outcomes":[]}"#,
            "writer" => r#"{"prose":"两位大臣于烛下各怀心事，礼数周全，言语间暗藏机锋。"}"#,
            "critic" => r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#,
            _ => "{}",
        };
        Ok(ModelOutput {
            content: content.to_string(),
            input_tokens: Some(self.input_tokens),
            output_tokens: Some(self.output_tokens),
        })
    }
}

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
        object_store_dir: std::env::temp_dir()
            .join(new_id("muse-rt-test"))
            .to_string_lossy()
            .into_owned(),
    }
}

async fn test_state() -> AppState {
    INIT.call_once(|| sqlx::any::install_default_drivers());
    let pool = AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, test_config())
}

fn sample_card_json(id: &str, name: &str) -> String {
    let card = CharacterCardV2 {
        schema_version: 2,
        id: id.into(),
        lifecycle: CardLifecycle::Ready,
        identity: Identity { name: name.into(), ..Default::default() },
        dramatic_core: Default::default(),
        decision_model: Default::default(),
        perception: Default::default(),
        emotion_dynamics: Default::default(),
        relation_grammar: Default::default(),
        expression_fingerprint: Default::default(),
        agency: Default::default(),
        growth_arc: Default::default(),
        world_adaptation: Default::default(),
        evidence_index: Default::default(),
        revision: 1,
        created_at: 0,
        updated_at: 0,
    };
    serde_json::to_string(&card).unwrap()
}

async fn seed_user(db: &AnyPool, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
         VALUES (?, '', 0, 'active', ?, ?)",
    )
    .bind(id)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

async fn seed_char(db: &AnyPool, id: &str, owner: &str, name: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES (?, ?, 'local', 1, ?, 'original', 'approved', 0, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(sample_card_json(id, name))
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

async fn seed_member(db: &AnyPool, world_id: &str, user_id: &str, cid: &str) {
    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
         VALUES (?, ?, ?, ?, '{}', 'active', ?)",
    )
    .bind(new_id("wm"))
    .bind(world_id)
    .bind(user_id)
    .bind(cid)
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 世界模板：带一个宿命(硬)主线节点 + 一个禁止谓词（供 E-1 种子提取）。
async fn seed_template(db: &AnyPool, id: &str) {
    let skeleton = json!({
        "mainlineNodes": [{ "id": "n1", "summary": "两位大臣在密室摊牌", "fated": true }],
        "forbiddenPredicates": [
            { "id": "f1", "expression": "characters.chA.secrets contains \"叛国\"", "reason": "禁止叛国情节" }
        ]
    });
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES (?, '联编模板', 'idle', ?, '{\"mode\":\"open\"}', 1, 1, 'approved', ?)",
    )
    .bind(id)
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 钉住的模型路由版本：default profile 指向 mock（runtime 只用它判定「有模型配置」，真实调用走注入的 mock）。
async fn seed_model_routes(db: &AnyPool, version: &str) {
    let routes = json!({
        "default": { "interface": "OpenAI-compatible", "baseUrl": "http://mock", "apiKey": "k", "model": "mock-model" }
    });
    sqlx::query("INSERT INTO model_routes (id, version, routes_json, active, created_at) VALUES (?, ?, ?, 1, ?)")
        .bind(new_id("mr"))
        .bind(version)
        .bind(routes.to_string())
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
}

async fn seed_whisper(db: &AnyPool, id: &str, world_id: &str, user_id: &str, cid: &str, text: &str) {
    sqlx::query(
        "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, expected_revision, status, created_at) \
         VALUES (?, ?, ?, ?, 'whisper', ?, 0, 'accepted', ?)",
    )
    .bind(id)
    .bind(world_id)
    .bind(user_id)
    .bind(cid)
    .bind(json!({ "text": text }).to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

async fn i64_one(db: &AnyPool, sql: &str, bind: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).bind(bind).fetch_one(db).await.unwrap()
}

async fn text_one(db: &AnyPool, sql: &str, bind: &str) -> String {
    sqlx::query_scalar::<_, String>(sql).bind(bind).fetch_one(db).await.unwrap()
}

/// 建一个 running、钉住 mock 路由、带 2 名成员的世界。
async fn running_world_with_two_members(state: &AppState) -> String {
    seed_template(&state.db, "tpl-x").await;
    seed_model_routes(&state.db, "test-routes").await;
    seed_user(&state.db, "uA").await;
    seed_user(&state.db, "uB").await;
    seed_char(&state.db, "chA", "uA", "李").await;
    seed_char(&state.db, "chB", "uB", "王").await;

    let mut p = CreateWorldParams::official("tpl-x", 1, "联编测试世界");
    p.status = Some("running".into());
    p.model_route_version = Some("test-routes".into());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0; // 本用例不测 cny 熔断
    let wid = create_world(&state.db, p).await.unwrap();

    seed_member(&state.db, &wid, "uA", "chA").await;
    seed_member(&state.db, &wid, "uB", "chB").await;
    wid
}

// ---------- 主用例：完整 tick 联编，跨 tick 累积 ----------

#[tokio::test]
async fn tick_runs_full_round_accumulates_state_events_and_metered_budget() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;

    // 投给在场角色 chA 的 whisper（应喂入并消费）；投给非在场 ghost 的 whisper（不应消费）。
    seed_whisper(&state.db, "iv-fed", &wid, "uA", "chA", "小心那个人").await;
    seed_whisper(&state.db, "iv-unfed", &wid, "uA", "ghost", "无处投递").await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // ===== tick 0：首 tick 冷启动种子 + 完整回合 =====
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    let s0 = process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap();
    assert_eq!(s0, TickStatus::Done, "首 tick 应走完整 run_round→commit，而非 fail-closed 暂停");

    // 状态累积：revision 0→1，narrative_state_json 是完整 NarrativeState。
    let w1 = load_world(&state.db, &wid).await.unwrap();
    assert_eq!(w1.state_revision, 1, "state_revision 应从 0 推进到 1");
    let st1: NarrativeState = serde_json::from_str(&w1.narrative_state_json).unwrap();
    assert_eq!(st1.revision, 1);
    assert_eq!(st1.run_id, wid, "run_id 稳定到 world 粒度");

    // E-1 种子：硬节点从 skeleton 注入并在本回合推进为 done（硬节点完成率落点）。
    assert_eq!(st1.narrative.outline_nodes.len(), 1, "应从 skeleton 种入 1 个大纲节点");
    assert_eq!(st1.narrative.outline_nodes[0].id, "n1");
    assert_eq!(st1.narrative.outline_nodes[0].constraint, ConstraintLevel::Hard, "fated 节点应为硬约束");
    assert_eq!(st1.narrative.outline_nodes[0].status, NodeStatus::Done);
    // 禁止谓词也随种子进入状态并被携带（约束未放宽）。
    assert_eq!(st1.narrative.forbidden_predicates.len(), 1);
    assert_eq!(st1.narrative.forbidden_predicates[0].id, "f1");
    // reducer 确实累积了节拍。
    assert!(!st1.narrative.pacing_notes.is_empty(), "应经 reducer 累积 pacingNotes");
    // 在场角色进入状态。
    assert!(st1.characters.contains_key("chA") && st1.characters.contains_key("chB"));

    // 事件落库：tick 0 有 world_events 行。
    let ev0 = i64_one(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id=? AND tick_no=0", &wid).await;
    assert!(ev0 > 0, "tick 0 应落库 world_events（2 ActionResolved + 2 DialogueSpoken）");

    // 预算实测计费（B-1）：director+decide×2+writer+critic = 5 次调用 ×(10+20) = 150。
    let spent0 = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=?", &wid).await;
    assert_eq!(spent0, 150, "预算应按 ModelClient 实测 token 累计（5 调用 ×30）");
    let cost0 =
        sqlx::query_scalar::<_, i64>("SELECT cost_tokens FROM world_ticks WHERE world_id=? AND tick_no=0")
            .bind(&wid)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(cost0, 150, "tick 成本记录应为实测 token");

    // Q-3：只消费本 tick 实际喂入的 whisper。
    assert_eq!(text_one(&state.db, "SELECT status FROM interventions WHERE id=?", "iv-fed").await, "applied");
    assert_eq!(
        text_one(&state.db, "SELECT status FROM interventions WHERE id=?", "iv-unfed").await,
        "accepted",
        "非在场角色的 whisper 不应被 blanket 标 applied"
    );

    // ===== tick 1：状态在同一 world 的引擎 FS 上继续累积 =====
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    let s1 = process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap();
    assert_eq!(s1, TickStatus::Done);

    let w2 = load_world(&state.db, &wid).await.unwrap();
    assert_eq!(w2.state_revision, 2, "第二 tick 应把 revision 累积到 2（而非 run_id 逐 tick 变导致丢状态）");
    let st2: NarrativeState = serde_json::from_str(&w2.narrative_state_json).unwrap();
    assert_eq!(st2.revision, 2);
    // 幂等账 appliedPatchIds 跨 tick 单调增长（patch-0、patch-1 各不相同，不受节拍集合去重影响）：
    // 这直接证明第二 tick 的回合是在第一 tick 的引擎 FS 状态之上推进（run_id 稳定 + 每 tick 回灌）。
    let applied = |st: &NarrativeState| -> usize {
        st.world.get("appliedPatchIds").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
    };
    assert_eq!(applied(&st1), 1, "tick 0 应用 patch-0");
    assert_eq!(applied(&st2), 2, "tick 1 在 tick 0 状态之上应用 patch-1（引擎 FS 状态跨 tick 累积）");

    // 事件序号继续增长。
    let ev_total = i64_one(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id=?", &wid).await;
    assert!(ev_total > ev0, "第二 tick 应追加事件");

    // 预算继续累积。
    let spent1 = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=?", &wid).await;
    assert_eq!(spent1, 300, "两 tick 累计 = 300");
}

// ---------- C-1 / C-2：原子认领 + CAS 冲突终态化 ----------

#[tokio::test]
async fn concurrent_claim_and_stale_tick_are_terminalized() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 5, output_tokens: 5 });

    // 认领幂等（C-1）：tick 0 处理完成后再次处理 → already_done，不重复跑、不重复计费。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    let spent_after_first = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=?", &wid).await;
    assert_eq!(
        process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(),
        TickStatus::Skipped("already_done")
    );
    let spent_after_second = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=?", &wid).await;
    assert_eq!(spent_after_first, spent_after_second, "重复处理不得二次计费");

    // 陈旧 tick（C-2）：world 已推进到 revision 1，但补投一个 base_revision=0 的 tick_no=1 → 终态跳过，
    // 不留 pending 无限 re-enqueue。
    insert_tick(&state.db, &wid, 1, 0).await.unwrap();
    let stale = process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap();
    assert_eq!(stale, TickStatus::Skipped("superseded"));
    let t1_status = text_one(&state.db, "SELECT status FROM world_ticks WHERE world_id=? AND tick_no=1", &wid).await;
    assert_eq!(t1_status, "done", "陈旧 tick 应终态化（done），不再 pending");
}

// ---------- 无模型仍安全跳过（dev 态回归，走生产入口 process_tick） ----------

#[tokio::test]
async fn tick_without_model_config_skips_via_public_entry() {
    let state = test_state().await;
    seed_user(&state.db, "uA").await;
    let mut p = CreateWorldParams::official("tpl", 1, "无模型世界");
    p.status = Some("running".into());
    // 不 seed model_routes：model_route_version 无匹配 → 跳过。
    let wid = create_world(&state.db, p).await.unwrap();
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick(&state, &wid, 0).await.unwrap(), TickStatus::Skipped("no_model_config"));
}

// ---------- #3b：不可逆行动同意链（消费 ConsentRequested + 审批回灌落定，规格 §2.4 / REMEDIATION #3） ----------

/// 驱动一个「死亡」不可逆结果的 mock：roleDecide 让活跃角色对 `victim` 施加致命行动，
/// 触发引擎不可逆分类（death）。模板有待推进硬节点 ⇒ 规则层升级到模型仲裁，mock 空 outcomes
/// 回退 Success ⇒ 结果「实际发生」并进入门控分类。其余环节返回合法占位 JSON。
struct IrreversibleMockModel {
    victim: String,
}

#[async_trait]
impl ModelClient for IrreversibleMockModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"刀光血影，杀机毕露。"}"#.to_string(),
            "roleDecide" => format!(
                r#"{{"intent":"取其性命","action":"拔剑当场杀死对手","speak":{{"willSpeak":false,"purpose":""}},"targets":["{}"],"acceptableCosts":[],"predictions":[]}}"#,
                self.victim
            ),
            "arbiter" => r#"{"outcomes":[]}"#.to_string(),
            "writer" => r#"{"prose":"剑光如雪，一击定生死。"}"#.to_string(),
            "critic" => r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string(),
            _ => "{}".to_string(),
        };
        Ok(ModelOutput { content, input_tokens: Some(5), output_tokens: Some(5) })
    }
}

#[tokio::test]
async fn irreversible_action_gates_consent_then_approve_lands() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    // chB 为受害者：本回合活跃角色对 chB 施加致命行动（共享 mock），不可逆主体统一为 chB。
    let model: Arc<dyn ModelClient> = Arc::new(IrreversibleMockModel { victim: "chB".into() });

    // ===== tick 0：不可逆行动被引擎门控 → 产 ConsentRequested → runtime 建同意；死亡不落定 =====
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(),
        TickStatus::Done,
        "不可逆结果被门控但场景仍提交（其余行动落定），非 blocked/fail"
    );

    // runtime 消费本回合 ConsentRequested → 恰好建 1 条 pending 同意（多个同 subject 事件被幂等去重）。
    let n_pending = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM consent_requests WHERE world_id=? AND status='pending'",
        &wid,
    )
    .await;
    assert_eq!(n_pending, 1, "不可逆行动应触发恰好一条 pending 同意请求");
    let ck = text_one(
        &state.db,
        "SELECT event_kind FROM consent_requests WHERE world_id=? AND status='pending'",
        &wid,
    )
    .await;
    assert_eq!(ck, "death", "同意事件类别应为 death");
    let subjects = text_one(
        &state.db,
        "SELECT subject_character_ids FROM consent_requests WHERE world_id=? AND status='pending'",
        &wid,
    )
    .await;
    assert!(subjects.contains("chB"), "当事角色应为受害者 chB，got={subjects}");
    // 通知已投递给当事角色主人 uB（同意触发源接通）。
    let n_notif = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM notification_outbox WHERE kind='consent_request' AND user_id=?",
        "uB",
    )
    .await;
    assert!(n_notif >= 1, "应通知当事角色主人来响应");

    // 死亡未落定：narrative_state.pending_consents 记录 chB/death（引擎门控证据）。
    let st1: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    assert!(
        st1.narrative.pending_consents.iter().any(|p| p.subject == "chB" && p.event_kind == "death"),
        "未获批的死亡应记入 pending_consents（门控不落定）"
    );

    // 幂等：再跑一遍同一 tick（already_done），不得重复建同意。
    let _ = process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap();
    let n_pending_again = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM consent_requests WHERE world_id=? AND status='pending'",
        &wid,
    )
    .await;
    assert_eq!(n_pending_again, 1, "重复 tick 不得重复建同意");

    // ===== 当事人 approve（等价 respond 落定；respond 端点在 consents/tests.rs 另有覆盖） =====
    let cid = text_one(
        &state.db,
        "SELECT id FROM consent_requests WHERE world_id=? AND status='pending'",
        &wid,
    )
    .await;
    sqlx::query("UPDATE consent_requests SET status='approved', resolved_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&cid)
        .execute(&state.db)
        .await
        .unwrap();

    // ===== tick 1：approved_consents 回灌 → 引擎落定死亡 + 清 pending =====
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(),
        TickStatus::Done
    );
    let st2: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    assert!(
        !st2.narrative.pending_consents.iter().any(|p| p.subject == "chB"),
        "获批后不可逆结果应落定并清除对应 pending_consents"
    );
    // 落定回合不产 ConsentRequested → 不新建、也无残留 pending 同意。
    let n_pending_after = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM consent_requests WHERE world_id=? AND status='pending'",
        &wid,
    )
    .await;
    assert_eq!(n_pending_after, 0, "落定后不应残留/重复新建 pending 同意");
}

// ---------- Phase 1：世界固有角色（NPC/反派）注入 ----------

/// 把一组 NPC 条目钉进 worlds.assembled_json 的 worldCharacterEntries（模拟装配产物，runtime 每 tick 读回）。
async fn pin_world_characters(db: &AnyPool, world_id: &str, npcs: &[(&str, &str)]) {
    let entries: Vec<serde_json::Value> = npcs
        .iter()
        .map(|(id, name)| {
            let card: serde_json::Value = serde_json::from_str(&sample_card_json(id, name)).unwrap();
            json!({ "characterId": id, "card": card, "location": "", "carriedItems": [] })
        })
        .collect();
    let assembled = json!({ "assembly": { "worldCharacterEntries": entries } });
    sqlx::query("UPDATE worlds SET assembled_json=? WHERE id=?")
        .bind(assembled.to_string())
        .bind(world_id)
        .execute(db)
        .await
        .unwrap();
}

/// NPC 从 assembled_json 注入 active_cards：参与本回合决策 → 产出 world_events（actor=npc，Public 广播），
/// 但【不是 world_member】（不进 members_projection、无日报投影）。预算多一次 decide 调用为证。
#[tokio::test]
async fn world_character_injected_participates_and_is_not_a_member() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    pin_world_characters(&state.db, &wid, &[("npc1", "黑衣人")]).await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    // NPC 参与决策 → 其行动落库为 world_event（actor 含 npc1，Public 广播）。
    let npc_events = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_events WHERE world_id=? AND actors_json LIKE '%npc1%'",
        &wid,
    )
    .await;
    assert!(npc_events > 0, "NPC 应参与本回合决策并产出可广播的 world_events");

    // NPC 无 owner，不是 world_member（故不进 members_projection、无日报投影）。
    let npc_member_rows = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_members WHERE world_id=? AND cloud_character_id='npc1'",
        &wid,
    )
    .await;
    assert_eq!(npc_member_rows, 0, "NPC 无 owner，不应是 world_member");

    // 预算实测：3 活跃角色（chA/chB/npc1）→ director + decide×3 + writer + critic = 6 调用 ×30 = 180
    //（对照纯 2 成员基线 150：NPC 计入活跃多一次 decide）。
    let spent = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=?", &wid).await;
    assert_eq!(spent, 180, "NPC 计入活跃 → 多一次 decide 调用（6 调用 ×30）");
}

// ---------- Phase 2：地点维度（初始位置种入 + 按地点分组） ----------

/// 钉地点图 + NPC 落在与玩家默认起点不同的地点：玩家默认起点 = 首个非秘境地点（id 序 hall<north → hall），
/// NPC home=north → 玩家与 NPC 分属两组。
async fn pin_locations_and_remote_npc(db: &AnyPool, world_id: &str) {
    let npc_card: serde_json::Value = serde_json::from_str(&sample_card_json("npc1", "北境守将")).unwrap();
    let assembled = json!({
        "assembly": {
            "worldCharacterEntries": [
                { "characterId": "npc1", "card": npc_card, "location": "north", "carriedItems": [] }
            ],
            "locationGraph": [
                { "id": "hall", "name": "前厅", "connections": ["north"] },
                { "id": "north", "name": "北境", "connections": ["hall"] }
            ]
        }
    });
    sqlx::query("UPDATE worlds SET assembled_json=? WHERE id=?")
        .bind(assembled.to_string())
        .bind(world_id)
        .execute(db)
        .await
        .unwrap();
}

/// build_seed_state 给角色初始 location：玩家 → 默认起点 hall，NPC → 其 home north；
/// 两地点分两组 → 导演/写作各按组放大（成本随地点组数上升）。
#[tokio::test]
async fn locations_seed_initial_positions_and_split_groups() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    pin_locations_and_remote_npc(&state.db, &wid).await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    // 初始位置种入：玩家 chA/chB → 默认起点 hall；NPC npc1 → 其 home north。
    let st: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    assert_eq!(st.characters["chA"].location, "hall", "玩家默认起点 = 首个非秘境地点");
    assert_eq!(st.characters["chB"].location, "hall");
    assert_eq!(st.characters["npc1"].location, "north", "NPC 落在其 home_location");

    // 2 组（hall:{chA,chB}、north:{npc1}）→ 导演2 + 决策3 + 写作2 + 审校1 = 8 调用 ×30 = 240。
    // （对照单组 3 活跃基线 180：多一个地点组 → 多一次导演 + 一次写作。）
    let spent = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=?", &wid).await;
    assert_eq!(spent, 240, "地点分组 → 导演/写作按组放大（8 调用 ×30）");
}

// ---------- Phase 3：道具事实源单一化（backpack / NPC 携带 → resources）+ 秘境准入端到端 ----------

/// 直接写 items 定义（绕过 grant_item，测试脚手架）。
async fn seed_item(db: &AnyPool, id: &str, effect_tags: &[&str], cosmology: &[&str], tier: i64) {
    sqlx::query(
        "INSERT INTO items (id, narrative, effect_tags, origin_world_template_id, cosmology_json, power_tier, created_at) \
         VALUES (?, '测试道具', ?, 'tpl-x', ?, ?, ?)",
    )
    .bind(id)
    .bind(serde_json::to_string(effect_tags).unwrap())
    .bind(serde_json::to_string(cosmology).unwrap())
    .bind(tier)
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 直接写一条 carried 背包行（模拟 carry 入场：物品随角色携带进本世界）。
async fn seed_carried(db: &AnyPool, user: &str, item_id: &str, world_id: &str) {
    sqlx::query(
        "INSERT INTO backpacks (id, user_id, item_id, acquired_world_id, status, carried_world_id, acquired_at) \
         VALUES (?, ?, ?, ?, 'carried', ?, ?)",
    )
    .bind(new_id("bp"))
    .bind(user)
    .bind(item_id)
    .bind(world_id)
    .bind(world_id)
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 钉一个携带道具的 NPC（carriedItems 已是装配解引用后的 ItemDefinition 形态）。
async fn pin_npc_with_carried(db: &AnyPool, world_id: &str, npc_id: &str, name: &str, item_id: &str, effect_tags: &[&str]) {
    let card: serde_json::Value = serde_json::from_str(&sample_card_json(npc_id, name)).unwrap();
    let carried = json!([{
        "id": item_id, "narrative": "", "effectTags": effect_tags,
        "origin": { "worldTemplateId": "tpl-x", "cosmology": ["mundane"], "powerTier": 2 }
    }]);
    let assembled = json!({
        "assembly": { "worldCharacterEntries": [
            { "characterId": npc_id, "card": card, "location": "", "carriedItems": carried }
        ] }
    });
    sqlx::query("UPDATE worlds SET assembled_json=? WHERE id=?")
        .bind(assembled.to_string())
        .bind(world_id)
        .execute(db)
        .await
        .unwrap();
}

/// 玩家 backpack + NPC 携带道具都物化进 CharacterState.resources（`item:<id>`/`tag:<t>`），单一事实源。
/// 未携带的玩家无道具事实；跨 tick 幂等（不累积重复项）。
#[tokio::test]
async fn player_backpack_and_npc_items_materialize_into_resources() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;

    // 玩家 uA(chA) 携带 jade_key 入场；chB 未携带任何道具。
    seed_item(&state.db, "jade_key", &["advantage:stealth"], &["myth"], 2).await;
    seed_carried(&state.db, "uA", "jade_key", &wid).await;
    // NPC npc1 携带 dark_blade（装配钉住）。
    pin_npc_with_carried(&state.db, &wid, "npc1", "黑衣人", "dark_blade", &["advantage:combat"]).await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);

    let st: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    // 玩家 backpack 物化：chA 持有 item:jade_key + tag:advantage:stealth。
    let cha = &st.characters["chA"].resources;
    assert!(cha.contains(&"item:jade_key".to_string()), "玩家携带道具应物化为持有事实: {cha:?}");
    assert!(cha.contains(&"tag:advantage:stealth".to_string()), "effectTag 应物化: {cha:?}");
    // 未携带的 chB 无道具事实。
    assert!(
        !st.characters["chB"].resources.iter().any(|r| r.starts_with("item:")),
        "未携带的玩家不应有道具事实"
    );
    // NPC 携带道具物化：npc1 持有 item:dark_blade + tag:advantage:combat。
    let npc = &st.characters["npc1"].resources;
    assert!(npc.contains(&"item:dark_blade".to_string()), "NPC 携带道具应物化: {npc:?}");
    assert!(npc.contains(&"tag:advantage:combat".to_string()));

    // 跨 tick 幂等：再跑一 tick，道具事实不重复累积（backpack 单一事实源，物化前先清派生项）。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model).await.unwrap(), TickStatus::Done);
    let st2: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    let jade_count = st2.characters["chA"].resources.iter().filter(|r| *r == "item:jade_key").count();
    assert_eq!(jade_count, 1, "跨 tick 道具事实应幂等，不累积重复项");
}

/// 秘境准入端到端：MoveMockModel 让全体尝试进秘境；持钥匙者（backpack 物化 → 引擎 R6b 读持有）被准入并
/// 移动落定，无钥匙者被拒留原地。证明 backpack → resources → 引擎 R6b 的完整链路。
struct MoveMockModel {
    dest: String,
}

#[async_trait]
impl ModelClient for MoveMockModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"前厅通往秘境的石门前。"}"#.to_string(),
            "roleDecide" => format!(
                r#"{{"intent":"探秘","action":"前往秘境","speak":{{"willSpeak":false,"purpose":""}},"targets":["loc:{}"],"acceptableCosts":[],"predictions":[]}}"#,
                self.dest
            ),
            "arbiter" => r#"{"outcomes":[]}"#.to_string(),
            "writer" => r#"{"prose":"石门轰然而开。"}"#.to_string(),
            "critic" => r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#.to_string(),
            _ => "{}".to_string(),
        };
        Ok(ModelOutput { content, input_tokens: Some(5), output_tokens: Some(5) })
    }
}

/// 钉一个「前厅 ⇄ 秘境（gate 需 jade_key）」的地点图。
async fn pin_secret_realm(db: &AnyPool, world_id: &str) {
    let assembled = json!({
        "assembly": { "locationGraph": [
            { "id": "hall", "name": "前厅", "connections": ["secret"] },
            { "id": "secret", "name": "秘境", "connections": ["hall"], "isSecretRealm": true,
              "gate": { "requiredItemIds": ["jade_key"] } }
        ] }
    });
    sqlx::query("UPDATE worlds SET assembled_json=? WHERE id=?")
        .bind(assembled.to_string())
        .bind(world_id)
        .execute(db)
        .await
        .unwrap();
}

#[tokio::test]
async fn secret_realm_admission_gates_move_by_materialized_backpack() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    pin_secret_realm(&state.db, &wid).await;
    // chA(uA) 持秘境钥匙 jade_key 并携带入场；chB 无。
    seed_item(&state.db, "jade_key", &[], &["myth"], 1).await;
    seed_carried(&state.db, "uA", "jade_key", &wid).await;

    let model: Arc<dyn ModelClient> = Arc::new(MoveMockModel { dest: "secret".into() });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    let st: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    // 持钥匙者通过 R6b 准入 → 移动落定到秘境。
    assert_eq!(st.characters["chA"].location, "secret", "持秘境钥匙者应被准入并移动到秘境");
    // 无钥匙者被 R6b 拒绝 → 留在前厅（初始默认起点）。
    assert_eq!(st.characters["chB"].location, "hall", "无钥匙者应被秘境准入拒绝，留在原地");
}

// ---------- 第二块 Phase 2：server event 模式接线（DES 时间线） ----------

/// soft 模板（普通软节点，无 threshold 里程碑 → is_terminal 永不判 MainlineDone，世界持续可推进）。
/// 用于 event 模式跨 tick 推进 game_time 的用例（里程碑模板首 tick 即完成主线 → 次 tick 终局短路，时钟不再前进）。
async fn seed_template_soft(db: &AnyPool, id: &str) {
    let skeleton = json!({
        "mainlineNodes": [{ "id": "n1", "summary": "两位大臣寒暄", "fated": false, "constraint": "soft" }],
        "forbiddenPredicates": []
    });
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES (?, '软节点模板', 'idle', ?, '{\"mode\":\"open\"}', 1, 1, 'approved', ?)",
    )
    .bind(id)
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 建一个 running、soft 模板、带 2 名成员的世界，按 `mode` 设 timeline_mode（'interval'/'event'）。
/// 各资源 id 以 `tag` 隔离，便于单个测试内并存多个世界（内存库无跨测试共享）。
async fn running_soft_world(state: &AppState, tag: &str, mode: &str) -> String {
    let tpl = format!("tpl-{tag}");
    let routes_v = format!("routes-{tag}");
    let (ua, ub) = (format!("u{tag}A"), format!("u{tag}B"));
    let (ca, cb) = (format!("c{tag}A"), format!("c{tag}B"));
    seed_template_soft(&state.db, &tpl).await;
    seed_model_routes(&state.db, &routes_v).await;
    seed_user(&state.db, &ua).await;
    seed_user(&state.db, &ub).await;
    seed_char(&state.db, &ca, &ua, "李").await;
    seed_char(&state.db, &cb, &ub, "王").await;

    let mut p = CreateWorldParams::official(tpl.clone(), 1, "DES 测试世界");
    p.status = Some("running".into());
    p.model_route_version = Some(routes_v.clone());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    let wid = create_world(&state.db, p).await.unwrap();

    seed_member(&state.db, &wid, &ua, &ca).await;
    seed_member(&state.db, &wid, &ub, &cb).await;

    if mode != "interval" {
        sqlx::query("UPDATE worlds SET timeline_mode=? WHERE id=?")
            .bind(mode)
            .bind(&wid)
            .execute(&state.db)
            .await
            .unwrap();
    }
    wid
}

/// commit_tick 把 NarrativeState.timeline.now 回写到 worlds.game_time；event 模式跨 tick 单调推进，
/// interval 模式恒为 0（run_round 不触碰 timeline）。
#[tokio::test]
async fn game_time_written_back() {
    let state = test_state().await;
    let ev = running_soft_world(&state, "ev", "event").await;
    let iv = running_soft_world(&state, "iv", "interval").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // event 世界 tick 0：首步激活时刻 T=0（全体缺席 next_time 视为 now=0）→ game_time 回写 0，与状态一致。
    insert_tick(&state.db, &ev, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &ev, 0, model.clone()).await.unwrap(), TickStatus::Done);
    let gt0 = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=?", &ev).await;
    let st0: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &ev).await.unwrap().narrative_state_json).unwrap();
    assert_eq!(gt0, st0.timeline.now, "game_time 应等于 timeline.now");
    assert_eq!(gt0, 0, "首步 T=0");
    // 首步推进 cohort 的 next_time = T + DEFAULT_DURATION（60）。角色 id 由 running_soft_world("ev",..) 派生为 cevA。
    assert_eq!(st0.timeline.next_time.get("cevA").copied(), Some(60));

    // event 世界 tick 1：最小 next_time = 60 → T=60 → game_time 回写 60（游戏时钟随事件步前进）。
    insert_tick(&state.db, &ev, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &ev, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let gt1 = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=?", &ev).await;
    let st1: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &ev).await.unwrap().narrative_state_json).unwrap();
    assert_eq!(gt1, st1.timeline.now, "game_time 应持续等于 timeline.now");
    assert_eq!(gt1, 60, "第二事件步激活时刻 T=60，game_time 应推进到 60");
    assert!(gt1 > gt0, "event 模式 game_time 应跨 tick 单调推进");

    // interval 世界：走原 run_round，timeline 不被触碰 → game_time 恒为 0。
    insert_tick(&state.db, &iv, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &iv, 0, model.clone()).await.unwrap(), TickStatus::Done);
    let gt_iv = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=?", &iv).await;
    assert_eq!(gt_iv, 0, "interval 世界不推进游戏时钟，game_time 恒为 0");
}

/// event 模式调度器「背靠背」：上一 tick done 且无 outstanding → 立即排下一 tick（不看墙钟 interval）；
/// interval 模式在同一 schedule_due_ticks 轮里未到间隔 → 不排新 tick（退化路径不受影响）。
#[tokio::test]
async fn timeline_mode_event_back_to_back() {
    let state = test_state().await;
    let ev = running_soft_world(&state, "ev", "event").await;
    let iv = running_soft_world(&state, "iv", "interval").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // 两个世界各处理完 tick 0（done）。
    insert_tick(&state.db, &ev, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &ev, 0, model.clone()).await.unwrap(), TickStatus::Done);
    insert_tick(&state.db, &iv, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &iv, 0, model.clone()).await.unwrap(), TickStatus::Done);

    // 调度器轮询：event 世界背靠背排出 tick 1（无 outstanding）；interval 世界未到间隔不排。
    super::schedule_due_ticks(&state).await.unwrap();

    let ev_max = i64_one(
        &state.db,
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=?",
        &ev,
    )
    .await;
    assert_eq!(ev_max, 1, "event 世界上一 tick done → 应背靠背立即排出 tick 1");
    let ev_pending = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_ticks WHERE world_id=? AND tick_no=1 AND status='pending'",
        &ev,
    )
    .await;
    assert_eq!(ev_pending, 1, "背靠背排出的 tick 1 应为 pending 待处理");

    let iv_max = i64_one(
        &state.db,
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=?",
        &iv,
    )
    .await;
    assert_eq!(iv_max, 0, "interval 世界刚建 tick 0，未到墙钟间隔，不应排新 tick");

    // 背靠背排出的 tick 1 可继续处理 → game_time 随之推进（证明 event 世界持续推进）。
    assert_eq!(process_tick_with_model(&state, &ev, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let gt = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=?", &ev).await;
    assert_eq!(gt, 60, "背靠背处理的第二 tick 应把 game_time 推进到 60");

    // 再轮询一次：tick 1 已 done → 继续背靠背排出 tick 2。
    super::schedule_due_ticks(&state).await.unwrap();
    let ev_max2 = i64_one(
        &state.db,
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=?",
        &ev,
    )
    .await;
    assert_eq!(ev_max2, 2, "event 世界应持续背靠背排 tick（tick 2）");
}

/// 纯 NPC 无玩家成员的世界 → member_ids 空短路，跳过（防空跑）：即便 NPC 使活跃卡 ≥2 也不推进。
#[tokio::test]
async fn pure_npc_world_without_members_skips() {
    let state = test_state().await;
    seed_template(&state.db, "tpl-x").await;
    seed_model_routes(&state.db, "test-routes").await;

    let mut p = CreateWorldParams::official("tpl-x", 1, "纯 NPC 世界");
    p.status = Some("running".into());
    p.model_route_version = Some("test-routes".into());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    let wid = create_world(&state.db, p).await.unwrap();

    // 无 world_members；钉两个 NPC（活跃卡将达 2，但无玩家 → 短路跳过）。
    pin_world_characters(&state.db, &wid, &[("npcA", "甲"), ("npcB", "乙")]).await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 0, model).await.unwrap(),
        TickStatus::Skipped("insufficient_members"),
        "无玩家成员的纯 NPC 世界应短路跳过，防空跑"
    );
}

// ==================== P1 Phase 0：放置房终局停机 + 防秒结束 ====================

/// 模板：给定 mainlineNodes（可空 → 空 skeleton）+ endgame 对象（P1 Phase 0 终局配置）。
async fn seed_template_with_endgame(
    db: &AnyPool,
    id: &str,
    room_type: &str,
    mainline: serde_json::Value,
    endgame: serde_json::Value,
) {
    let skeleton = json!({
        "mainlineNodes": mainline,
        "forbiddenPredicates": [],
        "endgame": endgame,
    });
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES (?, '终局模板', ?, ?, '{\"mode\":\"open\"}', 1, 1, 'approved', ?)",
    )
    .bind(id)
    .bind(room_type)
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 建一个 running、指定模板 / timeline_mode / room_type、带 2 名成员的世界（终局测试专用）。
/// 资源 id 以 `tag` 隔离；模板须由调用方先 seed。
async fn running_world_for_endgame(
    state: &AppState,
    tag: &str,
    tpl: &str,
    mode: &str,
    room_type: &str,
) -> String {
    let routes_v = format!("routes-{tag}");
    let (ua, ub) = (format!("u{tag}A"), format!("u{tag}B"));
    let (ca, cb) = (format!("c{tag}A"), format!("c{tag}B"));
    seed_model_routes(&state.db, &routes_v).await;
    seed_user(&state.db, &ua).await;
    seed_user(&state.db, &ub).await;
    seed_char(&state.db, &ca, &ua, "李").await;
    seed_char(&state.db, &cb, &ub, "王").await;

    let mut p = CreateWorldParams::official(tpl.to_string(), 1, "终局测试世界");
    p.status = Some("running".into());
    p.room_type = room_type.into();
    p.model_route_version = Some(routes_v.clone());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    let wid = create_world(&state.db, p).await.unwrap();

    seed_member(&state.db, &wid, &ua, &ca).await;
    seed_member(&state.db, &wid, &ub, &cb).await;

    if mode != "interval" {
        sqlx::query("UPDATE worlds SET timeline_mode=? WHERE id=?")
            .bind(mode)
            .bind(&wid)
            .execute(&state.db)
            .await
            .unwrap();
    }
    wid
}

async fn world_status(db: &AnyPool, wid: &str) -> String {
    text_one(db, "SELECT status FROM worlds WHERE id=?", wid).await
}

/// 终局条件(2) 世界时间上限：idle event 房到 max_world_ticks → end_world（status=ended）+ Concluded。
/// soft 模板（无硬节点）→ 引擎永不判 MainlineDone，世界持续跑到时间上限被 server 终结。
#[tokio::test]
async fn idle_world_concludes_at_max_world_ticks() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-cap",
        "idle",
        json!([{ "id": "n1", "summary": "寒暄", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 2 }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "cap", "tpl-cap", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0/1：未到时间上限（< max=2）→ 正常推进，世界仍 running。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(world_status(&state.db, &wid).await, "running", "tick 0 未到上限，世界仍 running");
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(world_status(&state.db, &wid).await, "running", "tick 1 未到上限，世界仍 running");

    // tick 2：tick_no(2) >= max_world_ticks(2) → 世界时间上限终局 → ended + Concluded。
    insert_tick(&state.db, &wid, 2, 2).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 2, model.clone()).await.unwrap(),
        TickStatus::Concluded,
        "到 max_world_ticks 应返回 Concluded（成功终态）"
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended", "到时间上限世界应 status=ended 停机");
    // 终局与状态 CAS 同事务：本 tick 的状态推进（revision 2→3）与停机同时落库。
    let rev = i64_one(&state.db, "SELECT state_revision FROM worlds WHERE id=?", &wid).await;
    assert_eq!(rev, 3, "终局 tick 的状态 CAS 与 end_world 同事务提交（revision 仍推进到 3）");
}

/// 终局条件(1) 主线走完（P2 引擎信号 MainlineDone 被消费）+ 防秒结束地板 min_world_ticks。
/// P1 调和后「主线」= 里程碑（threshold.is_some()）：低阈值里程碑首 tick 即被回合强度累积推过阈值完成
/// → 引擎产 MainlineDone；但 min_world_ticks=2 地板拦住早期终局；到地板后（含终局短路路径）才停机。
#[tokio::test]
async fn idle_world_concludes_on_mainline_complete_after_floor() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-main",
        "idle",
        // 里程碑：threshold=1.0（2 名成员一回合的强度足以推过）+ 无 advanceWhen 谓词门。
        json!([{ "id": "n1", "summary": "摊牌", "constraint": "soft", "threshold": 1.0 }]),
        json!({ "minWorldTicks": 2, "maxWorldTicks": 100 }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "main", "tpl-main", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0：跑完回合把里程碑 n1 的 milestoneProgress 累积过阈值 → Done → 引擎产 MainlineDone；
    // 但 tick 0 < 地板 2 → 不停机，保持 running。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(world_status(&state.db, &wid).await, "running", "主线已完成但未到地板 → 不秒结束");
    let st0: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    assert_eq!(st0.narrative.outline_nodes[0].status, NodeStatus::Done, "里程碑 n1 首 tick 已达阈值 Done");

    // tick 1：run_event_step 起始即判 MainlineDone → 终局短路（无回合）；仍未到地板 2 → 保持 running（noop）。
    // 短路 tick 不推进 revision，故 base_revision 仍为 1。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(),
        TickStatus::Skipped("terminal"),
        "主线完成 + 未到地板 → 终局短路保持 running（沿用 P2 noop）"
    );
    assert_eq!(world_status(&state.db, &wid).await, "running");

    // tick 2：终局短路 + tick_no(2) >= 地板 2 → 消费 MainlineDone → ended + Concluded。
    insert_tick(&state.db, &wid, 2, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 2, model.clone()).await.unwrap(),
        TickStatus::Concluded,
        "主线完成 + 过地板 → 消费终局信号停机（含终局短路路径）"
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended", "主线走完过地板 → status=ended");
}

/// 防秒结束守卫①：空 skeleton（mainlineNodes=[]）的 idle 房，引擎 is_terminal 因「里程碑集为空」永不判
/// MainlineDone → 绝不因「主线完成」在空集上真空成立而秒结束；只可能在 max_world_ticks 到点被 server 终结。
#[tokio::test]
async fn empty_skeleton_does_not_conclude_early() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-empty",
        "idle",
        json!([]), // 空 skeleton
        json!({ "minWorldTicks": 0, "maxWorldTicks": 5 }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "empty", "tpl-empty", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0：即便地板=0（无地板保护），空 skeleton 也不因「主线完成」秒结束（守卫①）→ Done + running。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(),
        TickStatus::Done,
        "空 skeleton 不得秒结束（is_terminal 因里程碑集为空永不 MainlineDone）"
    );
    assert_eq!(world_status(&state.db, &wid).await, "running", "空 skeleton 首 tick 世界仍 running");

    // 持续推进到 max_world_ticks(5)：ticks 1..=4 保持 running，tick 5 才因世界时间上限被 server 终结。
    for n in 1..=4i64 {
        insert_tick(&state.db, &wid, n, n).await.unwrap();
        assert_eq!(process_tick_with_model(&state, &wid, n, model.clone()).await.unwrap(), TickStatus::Done);
        assert_eq!(world_status(&state.db, &wid).await, "running", "未到上限前空 skeleton 世界持续 running");
    }
    insert_tick(&state.db, &wid, 5, 5).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 5, model.clone()).await.unwrap(),
        TickStatus::Concluded,
        "空 skeleton 世界最终在 max_world_ticks 被兜底终结（不无限跑）"
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended");
}

/// ended 后：schedule_due_ticks 不再排新 tick（status='running' 门），遗留 tick 命中 world_not_running noop。
#[tokio::test]
async fn ended_world_is_not_rescheduled() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-sched",
        "idle",
        json!([{ "id": "n1", "summary": "寒暄", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 1 }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "sched", "tpl-sched", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0 → Done；tick 1 → Concluded（1 >= max 1）→ ended。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Concluded);
    assert_eq!(world_status(&state.db, &wid).await, "ended");

    // 调度器轮询：ended 世界不在 WHERE status='running' 内 → 不排新 tick（max tick_no 仍为 1）。
    super::schedule_due_ticks(&state).await.unwrap();
    let max_tick = i64_one(
        &state.db,
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=?",
        &wid,
    )
    .await;
    assert_eq!(max_tick, 1, "ended 世界不应再被排新 tick");

    // 遗留 tick（补投一个 pending）：process 命中 world_not_running noop，不再跑回合、不重复结算。
    insert_tick(&state.db, &wid, 2, 2).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 2, model.clone()).await.unwrap(),
        TickStatus::Skipped("world_not_running"),
        "ended 世界的遗留 tick 应 world_not_running noop"
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended", "遗留 tick 不改变已 ended 的状态");
}

/// 幂等：end_world_tx 的 WHERE status='running' 保证只结算一次——首次 rows=1，再次 rows=0。
#[tokio::test]
async fn end_world_tx_is_idempotent() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-idem",
        "idle",
        json!([{ "id": "n1", "summary": "寒暄", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 100 }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "idem", "tpl-idem", "event", "idle").await;

    let mut tx = state.db.begin().await.unwrap();
    let r1 = super::end_world_tx(&mut tx, &wid, "time_limit").await.unwrap();
    let r2 = super::end_world_tx(&mut tx, &wid, "time_limit").await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(r1, 1, "首次 end_world 结算 running 世界 → rows=1");
    assert_eq!(r2, 0, "再次 end_world 命中非 running → rows=0（幂等，只结算一次）");
    assert_eq!(world_status(&state.db, &wid).await, "ended");
}

/// 非 idle 房（chapter）严格门：policy.enabled=false → 终局评估全跳过，即便配了极小 max_world_ticks 也不停机。
/// chapter/arena 既有收敛旁路零影响。
#[tokio::test]
async fn non_idle_world_ignores_endgame() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-chap",
        "chapter",
        json!([{ "id": "n1", "summary": "寒暄", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 1 }), // 极小上限，但 room_type=chapter → 不生效
    )
    .await;
    let wid = running_world_for_endgame(&state, "chap", "tpl-chap", "event", "chapter").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // 连跑 3 tick（远超 maxWorldTicks=1）：非 idle 房终局逻辑全跳过 → 全 Done，世界始终 running。
    for n in 0..=2i64 {
        insert_tick(&state.db, &wid, n, n).await.unwrap();
        assert_eq!(
            process_tick_with_model(&state, &wid, n, model.clone()).await.unwrap(),
            TickStatus::Done,
            "非 idle 房不应因 endgame 停机"
        );
        assert_eq!(world_status(&state.db, &wid).await, "running", "非 idle 房始终 running（终局门 room_type=='idle'）");
    }
}

// ---------- P1 Phase 3：关键角色退场 + 终局产出（select_ending / 荣誉奖励红线） ----------

/// 钉住实例装配层 enabled_endings（select_ending 的读取源）。最小 assembled_json 包装（其余段缺省，
/// runtime 读均为 guarded pointer，缺失即退化）。
async fn set_enabled_endings(db: &AnyPool, wid: &str, endings: &[&str]) {
    let assembled = json!({
        "assembly": { "enabledEndings": endings },
        "chapterState": {},
    });
    sqlx::query("UPDATE worlds SET assembled_json=? WHERE id=?")
        .bind(assembled.to_string())
        .bind(wid)
        .execute(db)
        .await
        .unwrap();
}

/// 放置房软主线示例 skeleton（6 个带 threshold + advanceWhen 的里程碑）：与
/// `docs/build/example-idle-skeleton.md` 的样例镜像同一份，作为「可加载 + 结构合法」的测试样例。
/// 关系谓词引用固定角色 id（heroine/player），用于文档展示 advanceWhen 写法；本测试只验证其能被
/// seed_narrative_layer 正确种入（谓词是否命中另由引擎级测试覆盖）。
fn example_idle_skeleton() -> (serde_json::Value, serde_json::Value) {
    let mainline = json!([
        { "id": "firstMeeting",  "summary": "初次照面：两人第一次在同一空间独处",       "constraint": "soft", "threshold": 2.0 },
        { "id": "smallTalk",     "summary": "日常寒暄累积成习惯",                       "constraint": "soft", "threshold": 3.0, "advanceWhen": "relations[heroine->player].affinity > 0.2" },
        { "id": "sharedSecret",  "summary": "有人先卸下防备，交换一个秘密",             "constraint": "soft", "threshold": 4.0, "advanceWhen": "relations[heroine->player].trust > 0.4" },
        { "id": "conflict",      "summary": "一次误会让关系出现裂痕",                   "constraint": "soft", "threshold": 4.0, "advanceWhen": "relations[player->heroine].affinity > 0.5" },
        { "id": "reconcile",     "summary": "裂痕后的和解，关系更进一步",               "constraint": "soft", "threshold": 5.0, "advanceWhen": "relations[heroine->player].trust > 0.6" },
        { "id": "turningPoint",  "summary": "面对去留的抉择，主线收束",                 "constraint": "soft", "threshold": 6.0, "advanceWhen": "relations[heroine->player].affinity > 0.7" },
    ]);
    let endgame = json!({
        "minWorldTicks": 5,
        "maxWorldTicks": 240,
        "keyCharacterIds": ["heroine"],
        "worldTimeLimit": null,
    });
    (mainline, endgame)
}

/// 终局条件(3) 关键角色退场：关键角色永久退场（成员表 left）→ 早于 insufficient_members 门直接终局停机。
/// 覆盖「关键角色离场使在场成员跌破 2 也能收敛」这一必须先于门槛评估的关键路径。
#[tokio::test]
async fn idle_world_concludes_on_key_character_exit() {
    let state = test_state().await;
    // 关键角色 = 成员 A（ckeyA）。maxWorldTicks 极大（不靠时间上限）；floor=0；n1 为无阈值软节点（引擎不判 MainlineDone）。
    seed_template_with_endgame(
        &state.db,
        "tpl-key",
        "idle",
        json!([{ "id": "n1", "summary": "相处", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 100000, "keyCharacterIds": ["ckeyA"] }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "key", "tpl-key", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0：关键角色仍在场 → 正常推进（无里程碑、未到时间上限）→ Done + running。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(world_status(&state.db, &wid).await, "running");

    // 关键角色 A 永久退场（成员表 left）——同时使在场活跃成员跌破 2。
    sqlx::query("UPDATE world_members SET status='left' WHERE world_id=? AND cloud_character_id=?")
        .bind(&wid)
        .bind("ckeyA")
        .execute(&state.db)
        .await
        .unwrap();

    // tick 1：关键角色退场判定先于 insufficient_members 门触发 → 终局停机 → Concluded + ended。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(),
        TickStatus::Concluded,
        "关键角色退场（过地板）→ 消费终局停机（先于 insufficient_members 门）"
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended", "关键角色退场 → status=ended");
    // 终局审计留痕（reason=key_character_exit）。
    let audits =
        i64_one(&state.db, "SELECT COUNT(*) FROM audit_logs WHERE action='world.ended' AND subject=?", &wid)
            .await;
    assert_eq!(audits, 1, "关键角色退场终局写一条审计");
}

/// 终局条件(1) 软主线跑到全里程碑 Done → ended + 终局日报 + select_ending 选定结局落成荣誉。
/// 多里程碑顺序推进（每 tick 至多推首个 Pending 里程碑），最后一个里程碑完成的 tick 经 commit_tick 收敛。
#[tokio::test]
async fn idle_world_concludes_on_full_mainline_with_ending_report() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-full",
        "idle",
        // 3 个纯阈值里程碑（无 advanceWhen 谓词门，保证 mock 回合强度可推过）；阈值 0.5 一回合即达标。
        json!([
            { "id": "m1", "summary": "初遇", "constraint": "soft", "threshold": 0.5 },
            { "id": "m2", "summary": "羁绊", "constraint": "soft", "threshold": 0.5 },
            { "id": "m3", "summary": "抉择", "constraint": "soft", "threshold": 0.5 },
        ]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 100 }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "full", "tpl-full", "event", "idle").await;
    set_enabled_endings(&state.db, &wid, &["golden_reunion", "quiet_parting"]).await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0/1：逐个推进里程碑 m1、m2；未全 Done → 世界仍 running（不秒结束——多里程碑天然拦真空完成）。
    for n in 0..=1i64 {
        insert_tick(&state.db, &wid, n, n).await.unwrap();
        assert_eq!(process_tick_with_model(&state, &wid, n, model.clone()).await.unwrap(), TickStatus::Done);
        assert_eq!(world_status(&state.db, &wid).await, "running");
    }

    // tick 2：推进最后一个里程碑 m3 → 全里程碑 Done → 引擎 MainlineDone → commit_tick 内终局 → Concluded。
    insert_tick(&state.db, &wid, 2, 2).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 2, model.clone()).await.unwrap(),
        TickStatus::Concluded,
        "全里程碑 Done → 终局停机"
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended");

    // 全部里程碑 Done。
    let st: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    assert!(
        st.narrative.outline_nodes.iter().all(|n| n.status == NodeStatus::Done),
        "终局时全部里程碑应 Done"
    );

    // 终局日报：commit_tick 报告循环生成（每成员一份，幂等 per world+char+day）。
    let reports = i64_one(&state.db, "SELECT COUNT(*) FROM daily_reports WHERE world_id=?", &wid).await;
    assert!(reports >= 1, "全里程碑 Done 的终局 tick 应产出终局日报");

    // 终局产出：审计留痕 + select_ending 取 enabled_endings 首个（golden_reunion）落成每成员一枚荣誉。
    let audits =
        i64_one(&state.db, "SELECT COUNT(*) FROM audit_logs WHERE action='world.ended' AND subject=?", &wid)
            .await;
    assert_eq!(audits, 1, "终局审计一条");
    let ending_rewards = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM arena_rewards WHERE kind='ending' AND label='golden_reunion' AND world_id=?",
        &wid,
    )
    .await;
    assert_eq!(ending_rewards, 2, "select_ending 取首个结局 → 每成员一枚终局荣誉");
}

/// 终局奖励红线（§2.5）：终局若发奖，只入 arena_rewards 荣誉旁路——荣誉非战力、无买判定、幂等只发一次。
#[tokio::test]
async fn ending_reward_respects_arena_redline() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-rw",
        "idle",
        json!([{ "id": "n1", "summary": "寒暄", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 1 }), // 到时间上限即终局，快速收敛
    )
    .await;
    let wid = running_world_for_endgame(&state, "rw", "tpl-rw", "event", "idle").await;
    set_enabled_endings(&state.db, &wid, &["honor_ending"]).await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(),
        TickStatus::Concluded
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended");

    // 红线①：终局奖励只入 arena_rewards（荣誉），kind='ending'、label=选定结局；arena_rewards schema 无
    //        任何强度/属性列 → 结构性保证「荣誉非战力」。
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, label FROM arena_rewards WHERE world_id=?")
            .bind(&wid)
            .fetch_all(&state.db)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2, "两名成员各获一枚终局荣誉");
    for (kind, label) in &rows {
        assert_eq!(kind, "ending", "奖励为荣誉类（非强度）");
        assert_eq!(label, "honor_ending", "荣誉 label = select_ending 选定结局");
    }

    // 红线②：无买判定——终局发奖不经任何计费/账本路径（全库无 ledger_entries）。
    let ledger: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ledger_entries")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(ledger.0, 0, "终局发奖不产生任何计费/账本记录（荣誉非交易）");

    // 红线③（幂等）：ended 世界的遗留 tick → world_not_running noop，不重复发奖。
    insert_tick(&state.db, &wid, 2, 2).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 2, model.clone()).await.unwrap(),
        TickStatus::Skipped("world_not_running")
    );
    let after = i64_one(&state.db, "SELECT COUNT(*) FROM arena_rewards WHERE world_id=?", &wid).await;
    assert_eq!(after, 2, "遗留 tick 不重复发奖（幂等）");
}

/// 文档样例自检：放置房软主线示例 skeleton（6 里程碑 + advanceWhen + endgame keyCharacterIds）能被
/// 正确种入——里程碑携带 threshold/advanceWhen，keyCharacterIds 被 load_endgame_policy 读出。
/// 保证 docs/build/example-idle-skeleton.md 的样例是可加载、结构合法的（防样例腐化）。
#[tokio::test]
async fn example_idle_skeleton_seeds_valid_milestones() {
    let state = test_state().await;
    let (mainline, endgame) = example_idle_skeleton();
    seed_template_with_endgame(&state.db, "tpl-example", "idle", mainline, endgame).await;
    let wid = running_world_for_endgame(&state, "ex", "tpl-example", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // 跑一个 tick 触发种子物化；floor=5 → 早期不终局。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);

    // 6 个里程碑全部种入，且均带 threshold（软里程碑）。
    let st: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    let milestones: Vec<_> =
        st.narrative.outline_nodes.iter().filter(|n| n.threshold.is_some()).collect();
    assert_eq!(milestones.len(), 6, "示例 skeleton 应种入 6 个阈值里程碑");
    // 带 advanceWhen 谓词的里程碑（语法合法）应保留谓词。
    let with_gate = milestones.iter().filter(|n| n.advance_when.is_some()).count();
    assert_eq!(with_gate, 5, "示例中 5 个里程碑带合法 advanceWhen 关系谓词门");

    // load_endgame_policy 读出 keyCharacterIds=["heroine"]。
    let world = load_world(&state.db, &wid).await.unwrap();
    let policy = super::load_endgame_policy(&state.db, &world).await.unwrap();
    assert!(policy.enabled, "idle 房终局策略启用");
    assert_eq!(policy.key_character_ids, vec!["heroine".to_string()], "keyCharacterIds 被读出");
    assert_eq!(policy.min_world_ticks, 5);
    assert_eq!(policy.max_world_ticks, 240);
}

/// 装配采样第二环下游生效（outline 侧）：seed_narrative_layer 仅对实例钉住的 selectedMainline 建 outline，
/// 未被选主线节点（n3）不进大纲——否则大纲节点数按模板全量，与通关判定口径不一致。
#[tokio::test]
async fn seed_narrative_layer_filters_outline_to_selected_mainline() {
    let state = test_state().await;
    // 模板：3 主线节点（n1 fated + n2 + n3）。
    let skeleton = json!({
        "mainlineNodes": [
            { "id": "n1", "summary": "开场", "fated": true },
            { "id": "n2", "summary": "中段", "constraint": "soft" },
            { "id": "n3", "summary": "被采样裁掉的支线", "constraint": "soft" }
        ]
    });
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES ('tpl-sample', '采样模板', 'idle', ?, '{\"mode\":\"open\"}', 1, 1, 'approved', ?)",
    )
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();

    seed_model_routes(&state.db, "test-routes").await;
    seed_user(&state.db, "uA").await;
    seed_user(&state.db, "uB").await;
    seed_char(&state.db, "chA", "uA", "李").await;
    seed_char(&state.db, "chB", "uB", "王").await;

    // 实例采样钉住：仅选 n1,n2（n3 被裁）。最小 assembled_json 包装（其余段缺省）。
    let assembled = json!({
        "assembly": { "sampling": { "seed": "deadbeefdeadbeef", "selectedMainline": ["n1", "n2"] } },
        "chapterState": {},
        "templateVersion": 1
    });

    let mut p = CreateWorldParams::official("tpl-sample", 1, "采样世界");
    p.status = Some("running".into());
    p.model_route_version = Some("test-routes".into());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    p.assembled_json = Some(assembled.to_string());
    let wid = create_world(&state.db, p).await.unwrap();
    seed_member(&state.db, &wid, "uA", "chA").await;
    seed_member(&state.db, &wid, "uB", "chB").await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    process_tick_with_model(&state, &wid, 0, model).await.unwrap();

    let w = load_world(&state.db, &wid).await.unwrap();
    let st: NarrativeState = serde_json::from_str(&w.narrative_state_json).unwrap();
    let ids: Vec<&str> = st.narrative.outline_nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec!["n1", "n2"], "outline 应仅含被选主线（模板序），n3 被采样裁掉");
}

// ==================== 缺口②：idle 房通用装配（NPC / 地点 / 装配采样进场） ====================

/// 直接以给定 skeleton 建模板（绕过 admin 的 validate_skeleton_refs，测试可用任意骨架）。
async fn seed_template_custom(db: &AnyPool, id: &str, room_type: &str, skeleton: serde_json::Value) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES (?, '缺口②模板', ?, ?, '{\"mode\":\"open\"}', 1, 1, 'approved', ?)",
    )
    .bind(id)
    .bind(room_type)
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 建一个 running、idle、指定（已 seed 的）模板、带 n 名成员的世界，assembled_json 初始 NULL（未装配）。
/// 资源 id 以 `tag` 隔离；用于缺口② idle 通用装配用例。
async fn running_idle_world_with_members(state: &AppState, tag: &str, tpl: &str, n: usize) -> String {
    let routes_v = format!("routes-{tag}");
    seed_model_routes(&state.db, &routes_v).await;

    let mut p = CreateWorldParams::official(tpl.to_string(), 1, "缺口② idle 世界");
    p.status = Some("running".into());
    p.room_type = "idle".into();
    p.model_route_version = Some(routes_v.clone());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    let wid = create_world(&state.db, p).await.unwrap();

    for i in 0..n {
        let (u, c) = (format!("u{tag}{i}"), format!("c{tag}{i}"));
        seed_user(&state.db, &u).await;
        seed_char(&state.db, &c, &u, &format!("玩家{i}")).await;
        seed_member(&state.db, &wid, &u, &c).await;
    }
    wid
}

/// 装配落地：idle 房模板含 worldCharacters + locations → 首 tick 前通用装配 → assembled_json 从 NULL
/// 变为含 worldCharacterEntries + locationGraph；装配的 NPC 注入 active_cards（产出 actor 含 npc 的 world_events）。
#[tokio::test]
async fn idle_room_assembles_npc_and_locations_on_first_tick() {
    let state = test_state().await;
    let npc: serde_json::Value = serde_json::from_str(&sample_card_json("npc-a", "北境守将")).unwrap();
    let skeleton = json!({
        "mainlineNodes": [{ "id": "n1", "summary": "相遇", "constraint": "soft" }],
        "worldCharacters": [
            { "card": npc, "homeLocation": "north", "carriedItemIds": [], "agendaNodes": [] }
        ],
        "locations": [
            { "id": "hall", "name": "前厅", "connections": ["north"] },
            { "id": "north", "name": "北境", "connections": ["hall"] }
        ]
    });
    seed_template_custom(&state.db, "tpl-idle-asm", "idle", skeleton).await;
    let wid = running_idle_world_with_members(&state, "asm", "tpl-idle-asm", 2).await;

    // 建成时未装配：assembled_json 恒 NULL。
    let before: Option<String> = sqlx::query_scalar("SELECT assembled_json FROM worlds WHERE id=?")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(before.is_none(), "idle 房建成时未装配，assembled_json 应为 NULL");

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    // 装配落地：assembled_json 非 NULL，含 worldCharacterEntries + locationGraph。
    let raw = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=?", &wid).await;
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let entries = v["assembly"]["worldCharacterEntries"].as_array().expect("装配后应含 worldCharacterEntries");
    assert_eq!(entries.len(), 1, "1 个世界 NPC 应装配进 worldCharacterEntries");
    assert_eq!(entries[0]["characterId"], "npc-a");
    let graph = v["assembly"]["locationGraph"].as_array().expect("装配后应含 locationGraph");
    assert_eq!(graph.len(), 2, "2 个地点应装配进 locationGraph");

    // NPC 进 active_cards：参与本回合决策 → 产出 actor 含 npc-a 的 world_events。
    let npc_events = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_events WHERE world_id=? AND actors_json LIKE '%npc-a%'",
        &wid,
    )
    .await;
    assert!(npc_events > 0, "装配的 NPC 应注入 active_cards 参与本回合决策");
}

/// 死锁解除：1 玩家 idle 房 + NPC 模板——装配前 active_cards 只有 1（会命中 insufficient_members），
/// 首 tick 前通用装配注入 NPC 使 active_cards≥2 → tick 正常推进（Done），不再 insufficient_members。
#[tokio::test]
async fn idle_npc_assembly_breaks_insufficient_members_deadlock() {
    let state = test_state().await;
    let npc: serde_json::Value = serde_json::from_str(&sample_card_json("npc-d", "黑衣客")).unwrap();
    let skeleton = json!({
        "mainlineNodes": [{ "id": "n1", "summary": "独处", "constraint": "soft" }],
        // 无地点：NPC home 空 → default_start 空（退化单一场景），仍能凑够碰撞。
        "worldCharacters": [
            { "card": npc, "homeLocation": "", "carriedItemIds": [], "agendaNodes": [] }
        ]
    });
    seed_template_custom(&state.db, "tpl-idle-dl", "idle", skeleton).await;
    // 仅 1 名玩家成员：装配前 active_cards.len()==1。
    let wid = running_idle_world_with_members(&state, "dl", "tpl-idle-dl", 1).await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    let status = process_tick_with_model(&state, &wid, 0, model).await.unwrap();

    assert_ne!(
        status,
        TickStatus::Skipped("insufficient_members"),
        "首 tick 前装配注入 NPC 应打破 active_cards<2 死锁"
    );
    assert_eq!(status, TickStatus::Done, "单玩家 idle + NPC 模板：装配后 active_cards==2 → 正常推进");

    // NPC 确已装配进实例（active_cards 的来源）。
    let raw = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=?", &wid).await;
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["assembly"]["worldCharacterEntries"].as_array().unwrap().len(), 1, "NPC 应装配进实例");
}

/// 幂等：idle 房连跑两 tick，装配仅首 tick 发生一次——第二 tick 因 assembled_json.is_some() 短路 +
/// C-7 CAS（WHERE assembled_json IS NULL），装配段与 assembledAt 逐字节不变（commit_tick 不触碰 assembled_json）。
#[tokio::test]
async fn idle_assembly_is_idempotent_across_ticks() {
    let state = test_state().await;
    let npc: serde_json::Value = serde_json::from_str(&sample_card_json("npc-i", "守夜人")).unwrap();
    let skeleton = json!({
        "mainlineNodes": [{ "id": "n1", "summary": "相处", "constraint": "soft" }],
        "worldCharacters": [
            { "card": npc, "homeLocation": "hall", "carriedItemIds": [], "agendaNodes": [] }
        ],
        "locations": [ { "id": "hall", "name": "前厅", "connections": [] } ]
    });
    seed_template_custom(&state.db, "tpl-idle-idem", "idle", skeleton).await;
    let wid = running_idle_world_with_members(&state, "idem", "tpl-idle-idem", 2).await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0：装配（首次）。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    let raw0 = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=?", &wid).await;
    let v0: serde_json::Value = serde_json::from_str(&raw0).unwrap();

    // tick 1：不得重装（is_some 短路）。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let raw1 = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=?", &wid).await;
    let v1: serde_json::Value = serde_json::from_str(&raw1).unwrap();

    assert_eq!(v0["assembledAt"], v1["assembledAt"], "第二 tick 不得重装（assembledAt 不变）");
    assert_eq!(v0["assembly"], v1["assembly"], "装配段跨 tick 逐字节钉住（不重掷、不覆盖）");
}

/// 装配采样对 idle 生效：超集 idle 模板（isSuperset + storylines + sampling）→ 首 tick 装配走种子采样，
/// 钉住 /assembly/sampling 审计段；seed_narrative_layer 仅对被选主线建 outline（模板全量 5 → 被选 2，含 fated），
/// 证明防刷第二环（装配采样）在 idle 房与 chapter 房同口径生效。
#[tokio::test]
async fn idle_room_assembly_sampling_narrows_outline() {
    let state = test_state().await;
    let npc1: serde_json::Value = serde_json::from_str(&sample_card_json("mnpc-1", "厉无咎")).unwrap();
    let npc2: serde_json::Value = serde_json::from_str(&sample_card_json("mnpc-2", "沈孤鸿")).unwrap();
    let npc3: serde_json::Value = serde_json::from_str(&sample_card_json("mnpc-3", "白清欢")).unwrap();
    let skeleton = json!({
        "sourceWork": { "sourceId": "src_novel", "title": "测试小说" },
        "isSuperset": true,
        "storylines": [
            { "id": "arc-1", "affinity": "strategist", "mainlineNodeIds": ["mn-fate","mn-x1","mn-x2","mn-y"], "hiddenPoolIds": [], "endingIds": ["end-1"] },
            { "id": "arc-2", "affinity": "social",     "mainlineNodeIds": ["mn-z"],                            "hiddenPoolIds": [], "endingIds": ["end-2"] }
        ],
        "mainlineNodes": [
            { "id": "mn-fate", "fated": true, "arcTags": ["arc-1","arc-2"] },
            { "id": "mn-x1", "variantGroup": "vgx", "arcTags": ["arc-1"] },
            { "id": "mn-x2", "variantGroup": "vgx", "arcTags": ["arc-1"] },
            { "id": "mn-y", "arcTags": ["arc-1"] },
            { "id": "mn-z", "arcTags": ["arc-2"] }
        ],
        "endingPool": [
            { "id": "end-1", "affinity": "strategist", "baseWeight": 1.0, "arcTags": ["arc-1"] },
            { "id": "end-2", "affinity": "social",     "baseWeight": 1.0, "arcTags": ["arc-2"] }
        ],
        "worldCharacters": [
            { "card": npc1, "homeLocation": "", "carriedItemIds": [], "agendaNodes": ["mn-fate"] },
            { "card": npc2, "homeLocation": "", "carriedItemIds": [], "agendaNodes": [] },
            { "card": npc3, "homeLocation": "", "carriedItemIds": [], "agendaNodes": [] }
        ],
        "sampling": { "instanceStorylineCount": 1, "instanceMainlineCount": 1, "instanceHiddenCount": 1, "instanceNpcCount": 1 }
    });
    seed_template_custom(&state.db, "tpl-idle-smp", "idle", skeleton).await;
    let wid = running_idle_world_with_members(&state, "smp", "tpl-idle-smp", 2).await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    // 装配采样审计段钉入（16 位十六进制 seed；被选主线 = fated + 1 = 2，模板全量 5）。
    let raw = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=?", &wid).await;
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let seed = v["assembly"]["sampling"]["seed"].as_str().expect("超集 idle 实例应产出采样审计段（seed）");
    assert_eq!(seed.len(), 16, "seed 应为 u64 十六进制");
    let sel = v["assembly"]["sampling"]["selectedMainline"].as_array().expect("应钉住 selectedMainline");
    assert_eq!(sel.len(), 2, "采样后主线 = fated + 1（模板全量 5）");
    assert!(sel.iter().any(|x| x == "mn-fate"), "fated 硬节点必留");

    // 下游生效：seed_narrative_layer 仅对被选主线建 outline（idle 房与 chapter 同口径）。
    let st: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &wid).await.unwrap().narrative_state_json).unwrap();
    let ids: Vec<&str> = st.narrative.outline_nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids.len(), 2, "outline 仅含被选主线（装配采样对 idle 生效），全量 5 被裁到 2: {ids:?}");
    assert!(ids.contains(&"mn-fate"), "outline 应含 fated 主线");
}

// ---------- 引擎 LLM 鲁棒性：max_output_tokens 从世界钉住的 model_routes 读取 ----------

async fn seed_routes_json(db: &AnyPool, version: &str, routes: serde_json::Value) {
    sqlx::query("INSERT INTO model_routes (id, version, routes_json, active, created_at) VALUES (?, ?, ?, 1, ?)")
        .bind(new_id("mr"))
        .bind(version)
        .bind(routes.to_string())
        .bind(now_ms())
        .execute(db)
        .await
        .unwrap();
}

/// 测试点 #8：RoutesConfig 带 maxOutputTokens → 解析出的值等于配置值；缺字段 → 回退默认。
#[tokio::test]
async fn resolve_model_routes_reads_max_output_tokens_from_config() {
    let state = test_state().await;
    let profile =
        json!({ "interface": "OpenAI-compatible", "baseUrl": "http://mock", "apiKey": "k", "model": "m" });

    // 带 maxOutputTokens（camelCase）→ 读取配置值。
    seed_routes_json(&state.db, "v-cfg", json!({ "default": profile, "maxOutputTokens": 4096 })).await;
    let (_routes, max_cfg) =
        super::resolve_model_routes(&state.db, "v-cfg").await.unwrap().expect("应解析出路由");
    assert_eq!(max_cfg, 4096, "应读取世界路由配置的 maxOutputTokens");

    // 缺字段 → 回退 DEFAULT_MAX_OUTPUT_TOKENS（旧世界零改动向后兼容）。
    seed_routes_json(&state.db, "v-def", json!({ "default": profile })).await;
    let (_routes2, max_def) =
        super::resolve_model_routes(&state.db, "v-def").await.unwrap().expect("应解析出路由");
    assert_eq!(max_def, super::DEFAULT_MAX_OUTPUT_TOKENS, "缺字段应回退默认上限");

    // 显式 0 视为无效 → 回退默认（不允许 0 上限把 max_tokens 直接归零）。
    seed_routes_json(&state.db, "v-zero", json!({ "default": profile, "maxOutputTokens": 0 })).await;
    let (_routes3, max_zero) =
        super::resolve_model_routes(&state.db, "v-zero").await.unwrap().expect("应解析出路由");
    assert_eq!(max_zero, super::DEFAULT_MAX_OUTPUT_TOKENS, "maxOutputTokens=0 应回退默认");
}
