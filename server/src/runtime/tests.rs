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
