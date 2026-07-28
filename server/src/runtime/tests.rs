//! S2 运行时联编集成测试（E-1 回归基线）：注入 mock ModelClient，走完整
//! run_round→commit_tick，断言状态跨 tick 累积、事件落库、实测 token 预算累计（B-1）、
//! 只消费本 tick 实际喂入的干预（Q-3）、CAS 冲突终态化（C-2）、原子认领幂等（C-1）。
//!
//! 这补上了此前 86 测试「全部在无模型跳过处提前返回、从不执行 run_round」的最大盲区。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use sqlx::{AnyPool, Row};

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

fn test_config() -> ServerConfig {
    ServerConfig {
        database_url: crate::testkit::test_database_url(),
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

pub(super) async fn test_state() -> AppState {
    AppState::new(crate::testkit::test_pool().await, test_config())
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

pub(super) async fn seed_user(db: &AnyPool, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
         VALUES ($1, '', 0, 'active', $2, $3)",
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
         VALUES ($1, $2, 'local', 1, $3, 'original', 'approved', 0, $4)",
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
         VALUES ($1, $2, $3, $4, '{}', 'active', $5)",
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
         VALUES ($1, '联编模板', 'idle', $2, '{\"mode\":\"open\"}', 1, 1, 'approved', $3)",
    )
    .bind(id)
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 钉住的模型路由版本：default profile 指向 mock（runtime 只用它判定「有模型配置」，真实调用走注入的 mock）。
pub(super) async fn seed_model_routes(db: &AnyPool, version: &str) {
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

pub(super) async fn seed_whisper(db: &AnyPool, id: &str, world_id: &str, user_id: &str, cid: &str, text: &str) {
    sqlx::query(
        "INSERT INTO interventions (id, world_id, user_id, character_id, kind, payload_json, expected_revision, status, created_at) \
         VALUES ($1, $2, $3, $4, 'whisper', $5, 0, 'accepted', $6)",
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

pub(super) async fn i64_one(db: &AnyPool, sql: &str, bind: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).bind(bind).fetch_one(db).await.unwrap()
}

pub(super) async fn text_one(db: &AnyPool, sql: &str, bind: &str) -> String {
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
    let ev0 = i64_one(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id=$1 AND tick_no=0", &wid).await;
    assert!(ev0 > 0, "tick 0 应落库 world_events（2 ActionResolved + 2 DialogueSpoken）");

    // 预算实测计费（B-1）：director+decide×2+writer+critic = 5 次调用 ×(10+20) = 150。
    let spent0 = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=$1", &wid).await;
    assert_eq!(spent0, 150, "预算应按 ModelClient 实测 token 累计（5 调用 ×30）");
    let cost0 =
        sqlx::query_scalar::<_, i64>("SELECT cost_tokens FROM world_ticks WHERE world_id=$1 AND tick_no=0")
            .bind(&wid)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(cost0, 150, "tick 成本记录应为实测 token");

    // Q-3：只消费本 tick 实际喂入的 whisper。
    assert_eq!(text_one(&state.db, "SELECT status FROM interventions WHERE id=$1", "iv-fed").await, "applied");
    assert_eq!(
        text_one(&state.db, "SELECT status FROM interventions WHERE id=$1", "iv-unfed").await,
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
    let ev_total = i64_one(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id=$1", &wid).await;
    assert!(ev_total > ev0, "第二 tick 应追加事件");

    // 预算继续累积。
    let spent1 = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=$1", &wid).await;
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
    let spent_after_first = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=$1", &wid).await;
    assert_eq!(
        process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(),
        TickStatus::Skipped("already_done")
    );
    let spent_after_second = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=$1", &wid).await;
    assert_eq!(spent_after_first, spent_after_second, "重复处理不得二次计费");

    // 陈旧 tick（C-2）：world 已推进到 revision 1，但补投一个 base_revision=0 的 tick_no=1 → 终态跳过，
    // 不留 pending 无限 re-enqueue。
    insert_tick(&state.db, &wid, 1, 0).await.unwrap();
    let stale = process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap();
    assert_eq!(stale, TickStatus::Skipped("superseded"));
    let t1_status = text_one(&state.db, "SELECT status FROM world_ticks WHERE world_id=$1 AND tick_no=1", &wid).await;
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
        "SELECT COUNT(*) FROM consent_requests WHERE world_id=$1 AND status='pending'",
        &wid,
    )
    .await;
    assert_eq!(n_pending, 1, "不可逆行动应触发恰好一条 pending 同意请求");
    let ck = text_one(
        &state.db,
        "SELECT event_kind FROM consent_requests WHERE world_id=$1 AND status='pending'",
        &wid,
    )
    .await;
    assert_eq!(ck, "death", "同意事件类别应为 death");
    let subjects = text_one(
        &state.db,
        "SELECT subject_character_ids FROM consent_requests WHERE world_id=$1 AND status='pending'",
        &wid,
    )
    .await;
    assert!(subjects.contains("chB"), "当事角色应为受害者 chB，got={subjects}");
    // 通知已投递给当事角色主人 uB（同意触发源接通）。
    let n_notif = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM notification_outbox WHERE kind='consent_request' AND user_id=$1",
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
        "SELECT COUNT(*) FROM consent_requests WHERE world_id=$1 AND status='pending'",
        &wid,
    )
    .await;
    assert_eq!(n_pending_again, 1, "重复 tick 不得重复建同意");

    // ===== 当事人 approve（等价 respond 落定；respond 端点在 consents/tests.rs 另有覆盖） =====
    let cid = text_one(
        &state.db,
        "SELECT id FROM consent_requests WHERE world_id=$1 AND status='pending'",
        &wid,
    )
    .await;
    sqlx::query("UPDATE consent_requests SET status='approved', resolved_at=$1 WHERE id=$2")
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
        "SELECT COUNT(*) FROM consent_requests WHERE world_id=$1 AND status='pending'",
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
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
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
        "SELECT COUNT(*) FROM world_events WHERE world_id=$1 AND actors_json LIKE '%npc1%'",
        &wid,
    )
    .await;
    assert!(npc_events > 0, "NPC 应参与本回合决策并产出可广播的 world_events");

    // NPC 无 owner，不是 world_member（故不进 members_projection、无日报投影）。
    let npc_member_rows = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_members WHERE world_id=$1 AND cloud_character_id='npc1'",
        &wid,
    )
    .await;
    assert_eq!(npc_member_rows, 0, "NPC 无 owner，不应是 world_member");

    // 预算实测：3 活跃角色（chA/chB/npc1）→ director + decide×3 + writer + critic = 6 调用 ×30 = 180
    //（对照纯 2 成员基线 150：NPC 计入活跃多一次 decide）。
    let spent = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=$1", &wid).await;
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
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
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
    let spent = i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id=$1", &wid).await;
    assert_eq!(spent, 240, "地点分组 → 导演/写作按组放大（8 调用 ×30）");
}

// ---------- Phase 3：道具事实源单一化（backpack / NPC 携带 → resources）+ 秘境准入端到端 ----------

/// 直接写 items 定义（绕过 grant_item，测试脚手架）。
async fn seed_item(db: &AnyPool, id: &str, effect_tags: &[&str], cosmology: &[&str], tier: i64) {
    sqlx::query(
        "INSERT INTO items (id, narrative, effect_tags, origin_world_template_id, cosmology_json, power_tier, created_at) \
         VALUES ($1, '测试道具', $2, 'tpl-x', $3, $4, $5)",
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
         VALUES ($1, $2, $3, $4, 'carried', $5, $6)",
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
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
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
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
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
         VALUES ($1, '软节点模板', 'idle', $2, '{\"mode\":\"open\"}', 1, 1, 'approved', $3)",
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
        sqlx::query("UPDATE worlds SET timeline_mode=$1 WHERE id=$2")
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
    let gt0 = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=$1", &ev).await;
    let st0: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &ev).await.unwrap().narrative_state_json).unwrap();
    assert_eq!(gt0, st0.timeline.now, "game_time 应等于 timeline.now");
    assert_eq!(gt0, 0, "首步 T=0");
    // 首步推进 cohort 的 next_time = T + DEFAULT_DURATION（60）。角色 id 由 running_soft_world("ev",..) 派生为 cevA。
    assert_eq!(st0.timeline.next_time.get("cevA").copied(), Some(60));

    // event 世界 tick 1：最小 next_time = 60 → T=60 → game_time 回写 60（游戏时钟随事件步前进）。
    insert_tick(&state.db, &ev, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &ev, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let gt1 = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=$1", &ev).await;
    let st1: NarrativeState =
        serde_json::from_str(&load_world(&state.db, &ev).await.unwrap().narrative_state_json).unwrap();
    assert_eq!(gt1, st1.timeline.now, "game_time 应持续等于 timeline.now");
    assert_eq!(gt1, 60, "第二事件步激活时刻 T=60，game_time 应推进到 60");
    assert!(gt1 > gt0, "event 模式 game_time 应跨 tick 单调推进");

    // interval 世界：走原 run_round，timeline 不被触碰 → game_time 恒为 0。
    insert_tick(&state.db, &iv, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &iv, 0, model.clone()).await.unwrap(), TickStatus::Done);
    let gt_iv = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=$1", &iv).await;
    assert_eq!(gt_iv, 0, "interval 世界不推进游戏时钟，game_time 恒为 0");
}

/// event 模式调度器「背靠背」：上一 tick done 且无 outstanding → 立即排下一 tick（不看墙钟 interval）；
/// interval 模式在同一 schedule_due_ticks 轮里未到间隔 → 不排新 tick（退化路径不受影响）。
#[tokio::test]
async fn timeline_mode_event_back_to_back() {
    // 与错峰用例互斥（错峰开关是进程级 env，会改变本用例走的调度分支）。
    let _g = offpeak_fixture::OffPeakSwitch::off();
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
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=$1",
        &ev,
    )
    .await;
    assert_eq!(ev_max, 1, "event 世界上一 tick done → 应背靠背立即排出 tick 1");
    let ev_pending = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1 AND tick_no=1 AND status='pending'",
        &ev,
    )
    .await;
    assert_eq!(ev_pending, 1, "背靠背排出的 tick 1 应为 pending 待处理");

    let iv_max = i64_one(
        &state.db,
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=$1",
        &iv,
    )
    .await;
    assert_eq!(iv_max, 0, "interval 世界刚建 tick 0，未到墙钟间隔，不应排新 tick");

    // 背靠背排出的 tick 1 可继续处理 → game_time 随之推进（证明 event 世界持续推进）。
    assert_eq!(process_tick_with_model(&state, &ev, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let gt = i64_one(&state.db, "SELECT game_time FROM worlds WHERE id=$1", &ev).await;
    assert_eq!(gt, 60, "背靠背处理的第二 tick 应把 game_time 推进到 60");

    // 再轮询一次：tick 1 已 done → 继续背靠背排出 tick 2。
    super::schedule_due_ticks(&state).await.unwrap();
    let ev_max2 = i64_one(
        &state.db,
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=$1",
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
         VALUES ($1, '终局模板', $2, $3, '{\"mode\":\"open\"}', 1, 1, 'approved', $4)",
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
        sqlx::query("UPDATE worlds SET timeline_mode=$1 WHERE id=$2")
            .bind(mode)
            .bind(&wid)
            .execute(&state.db)
            .await
            .unwrap();
    }
    wid
}

async fn world_status(db: &AnyPool, wid: &str) -> String {
    text_one(db, "SELECT status FROM worlds WHERE id=$1", wid).await
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
    let rev = i64_one(&state.db, "SELECT state_revision FROM worlds WHERE id=$1", &wid).await;
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
    let _g = offpeak_fixture::OffPeakSwitch::off();
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
        "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=$1",
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

/// 钉住实例装配层 enabled_endings，**不写 selectedEnding** —— 即修复（任务 #41）之前钉住的老实例
/// 形态，`select_ending` 对它们走**回退口径**（取名单首个）。最小 assembled_json 包装（其余段缺省，
/// runtime 读均为 guarded pointer，缺失即退化）。新实例的正路见 `set_pinned_ending`。
async fn set_enabled_endings(db: &AnyPool, wid: &str, endings: &[&str]) {
    let assembled = json!({
        "assembly": { "enabledEndings": endings },
        "chapterState": {},
    });
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
        .bind(assembled.to_string())
        .bind(wid)
        .execute(db)
        .await
        .unwrap();
}

/// 钉住装配层**定盘**结局（任务 #41 后的正路形态）：`selectedEnding` = 装配层按权重掷点的结果，
/// `enabledEndings` = 台上有哪些。故意让二者首项不同，才能验出 runtime 读的是前者而非后者。
async fn set_pinned_ending(db: &AnyPool, wid: &str, selected: &str, enabled: &[&str]) {
    let assembled = json!({
        "assembly": { "enabledEndings": enabled, "selectedEnding": selected },
        "chapterState": {},
    });
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
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
    sqlx::query("UPDATE world_members SET status='left' WHERE world_id=$1 AND cloud_character_id=$2")
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
        i64_one(&state.db, "SELECT COUNT(*) FROM audit_logs WHERE action='world.ended' AND subject=$1", &wid)
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
    let reports = i64_one(&state.db, "SELECT COUNT(*) FROM daily_reports WHERE world_id=$1", &wid).await;
    assert!(reports >= 1, "全里程碑 Done 的终局 tick 应产出终局日报");

    // 终局产出：审计留痕 + select_ending 落成每成员一枚荣誉。本例 assembled_json 无 selectedEnding
    //（老实例形态）→ 走回退口径取名单首个（golden_reunion）；定盘口径另见 ending_reward_uses_pinned_selected_ending。
    let audits =
        i64_one(&state.db, "SELECT COUNT(*) FROM audit_logs WHERE action='world.ended' AND subject=$1", &wid)
            .await;
    assert_eq!(audits, 1, "终局审计一条");
    let ending_rewards = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM arena_rewards WHERE kind='ending' AND label='golden_reunion' AND world_id=$1",
        &wid,
    )
    .await;
    assert_eq!(ending_rewards, 2, "select_ending 回退口径取名单首个 → 每成员一枚终局荣誉");
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
        sqlx::query_as("SELECT kind, label FROM arena_rewards WHERE world_id=$1")
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
    let after = i64_one(&state.db, "SELECT COUNT(*) FROM arena_rewards WHERE world_id=$1", &wid).await;
    assert_eq!(after, 2, "遗留 tick 不重复发奖（幂等）");
}

/// 任务 #41：runtime 读的是装配层**定盘**的 `selectedEnding`，不是 `enabledEndings` 首个。
/// 故意把定盘结局放在名单**末位** —— 旧行为会发 `first_listed` 的荣誉，新行为发 `pinned_by_dice`。
/// 掷点本身在装配层（有 `assembly::sampling_tests` 专项守），这里只锁 runtime 侧的读取口径。
#[tokio::test]
async fn ending_reward_uses_pinned_selected_ending() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-pin",
        "idle",
        json!([{ "id": "n1", "summary": "寒暄", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 1 }), // 到时间上限即终局，快速收敛
    )
    .await;
    let wid = running_world_for_endgame(&state, "pin", "tpl-pin", "event", "idle").await;
    set_pinned_ending(&state.db, &wid, "pinned_by_dice", &["first_listed", "pinned_by_dice"]).await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Concluded);

    let labels: Vec<(String,)> = sqlx::query_as("SELECT label FROM arena_rewards WHERE world_id=$1")
        .bind(&wid)
        .fetch_all(&state.db)
        .await
        .unwrap();
    assert_eq!(labels.len(), 2, "两名成员各获一枚终局荣誉");
    for (label,) in &labels {
        assert_eq!(label, "pinned_by_dice", "荣誉 label = 装配层定盘结局（非名单首个）");
    }

    // 终局审计的 reason 串里也带定盘结局（运营侧看到的同样是定盘那个）。
    let reason = text_one(
        &state.db,
        "SELECT reason FROM audit_logs WHERE action='world.ended' AND subject=$1",
        &wid,
    )
    .await;
    assert!(reason.contains("pinned_by_dice"), "终局审计应记录定盘结局：{reason}");
    assert!(!reason.contains("first_listed"), "审计不应记录名单首个：{reason}");
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
         VALUES ('tpl-sample', '采样模板', 'idle', $1, '{\"mode\":\"open\"}', 1, 1, 'approved', $2)",
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
         VALUES ($1, '缺口②模板', $2, $3, '{\"mode\":\"open\"}', 1, 1, 'approved', $4)",
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
    let before: Option<String> = sqlx::query_scalar("SELECT assembled_json FROM worlds WHERE id=$1")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(before.is_none(), "idle 房建成时未装配，assembled_json 应为 NULL");

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    // 装配落地：assembled_json 非 NULL，含 worldCharacterEntries + locationGraph。
    let raw = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=$1", &wid).await;
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let entries = v["assembly"]["worldCharacterEntries"].as_array().expect("装配后应含 worldCharacterEntries");
    assert_eq!(entries.len(), 1, "1 个世界 NPC 应装配进 worldCharacterEntries");
    assert_eq!(entries[0]["characterId"], "npc-a");
    let graph = v["assembly"]["locationGraph"].as_array().expect("装配后应含 locationGraph");
    assert_eq!(graph.len(), 2, "2 个地点应装配进 locationGraph");

    // NPC 进 active_cards：参与本回合决策 → 产出 actor 含 npc-a 的 world_events。
    let npc_events = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_events WHERE world_id=$1 AND actors_json LIKE '%npc-a%'",
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
    let raw = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=$1", &wid).await;
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
    let raw0 = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=$1", &wid).await;
    let v0: serde_json::Value = serde_json::from_str(&raw0).unwrap();

    // tick 1：不得重装（is_some 短路）。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let raw1 = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=$1", &wid).await;
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
    let raw = text_one(&state.db, "SELECT assembled_json FROM worlds WHERE id=$1", &wid).await;
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
    sqlx::query("INSERT INTO model_routes (id, version, routes_json, active, created_at) VALUES ($1, $2, $3, 1, $4)")
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
async fn resolve_model_routes_reads_fallback_and_stays_backward_compatible() {
    let state = test_state().await;
    let p = |m: &str| json!({ "interface": "OpenAI-compatible", "baseUrl": "http://mock", "apiKey": "k", "model": m });

    // 🔴 旧 routes_json（无 fallback 字段）→ None，**逐字节向后兼容**。
    // 这一条比「能读出来」更要紧：绝大多数世界都是这个形态，读错了等于给所有世界
    // 悄悄开了一条备用路由（而 `FallbackModelClient` 在 None 时是纯透传）。
    seed_routes_json(&state.db, "v-nofb", json!({ "default": p("main") })).await;
    let (_r, _m, fb) =
        super::resolve_model_routes(&state.db, "v-nofb").await.unwrap().expect("应解析出路由");
    assert!(fb.is_none(), "🔴 未声明 fallback 的世界不得凭空长出一条备用路由");

    // 声明了 → 读出来，且**不是**主路由那一个。
    seed_routes_json(
        &state.db,
        "v-fb",
        json!({ "default": p("main"), "fallback": p("backup") }),
    )
    .await;
    let (routes, _m, fb) =
        super::resolve_model_routes(&state.db, "v-fb").await.unwrap().expect("应解析出路由");
    assert_eq!(routes.default.model, "main");
    assert_eq!(fb.expect("应读出备用路由").model, "backup");
}

/// 🔴 `world_ticks.fallback_used` 的默认值必须是 **0 而不是 NULL**，且存量行也是 0。
/// 0 在这里是**真的**——本列上线前没有回退这回事，不是「不知道」。
#[tokio::test]
async fn fallback_used_defaults_to_zero_for_existing_rows() {
    let state = test_state().await;
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, created_at) \
         VALUES ('wt_fb', 'w_fb', 0, 0, 'pending', 1)",
    )
    .execute(&state.db)
    .await
    .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT fallback_used FROM world_ticks WHERE id = 'wt_fb'")
        .fetch_one(&state.db)
        .await
        .expect("🔴 该列必须 NOT NULL DEFAULT 0 —— NULL 会让「没回退过」与「不知道」混起来");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn resolve_model_routes_reads_max_output_tokens_from_config() {
    let state = test_state().await;
    let profile =
        json!({ "interface": "OpenAI-compatible", "baseUrl": "http://mock", "apiKey": "k", "model": "m" });

    // 带 maxOutputTokens（camelCase）→ 读取配置值。
    seed_routes_json(&state.db, "v-cfg", json!({ "default": profile, "maxOutputTokens": 4096 })).await;
    let (_routes, max_cfg, _fb) =
        super::resolve_model_routes(&state.db, "v-cfg").await.unwrap().expect("应解析出路由");
    assert_eq!(max_cfg, 4096, "应读取世界路由配置的 maxOutputTokens");

    // 缺字段 → 回退 DEFAULT_MAX_OUTPUT_TOKENS（旧世界零改动向后兼容）。
    seed_routes_json(&state.db, "v-def", json!({ "default": profile })).await;
    let (_routes2, max_def, _fb) =
        super::resolve_model_routes(&state.db, "v-def").await.unwrap().expect("应解析出路由");
    assert_eq!(max_def, super::DEFAULT_MAX_OUTPUT_TOKENS, "缺字段应回退默认上限");

    // 显式 0 视为无效 → 回退默认（不允许 0 上限把 max_tokens 直接归零）。
    seed_routes_json(&state.db, "v-zero", json!({ "default": profile, "maxOutputTokens": 0 })).await;
    let (_routes3, max_zero, _fb) =
        super::resolve_model_routes(&state.db, "v-zero").await.unwrap().expect("应解析出路由");
    assert_eq!(max_zero, super::DEFAULT_MAX_OUTPUT_TOKENS, "maxOutputTokens=0 应回退默认");
}

// ---------- P2 Stage3：异步时间线全房型（调度节奏 room_type 解耦 + event 地点碰撞） ----------

/// 调度节奏解耦（本 Stage 核心）：event 房的「背靠背自动排 tick」**仅 idle 放置房**生效；
/// chapter/arena 的 event 房**不被调度器自动推进**——新 tick 只来自手动端点（arena host_tick /
/// chapter start），保 arena「节目节奏优先于定时器」与 chapter「会话驱动」语义。
#[tokio::test]
async fn event_non_idle_manual_only_idle_back_to_back() {
    let _g = offpeak_fixture::OffPeakSwitch::off();
    let state = test_state().await;
    seed_template_soft(&state.db, "tpl-mr").await;
    // 三个 event 房，房型各异（room_type 由 helper 显式落 p.room_type）。
    let idle = running_world_for_endgame(&state, "mri", "tpl-mr", "event", "idle").await;
    let chap = running_world_for_endgame(&state, "mrc", "tpl-mr", "event", "chapter").await;
    let arena = running_world_for_endgame(&state, "mra", "tpl-mr", "event", "arena").await;

    // 首轮调度：idle 无 outstanding → 背靠背排出 tick 0；chapter/arena 不自动排（房型闸）。
    super::schedule_due_ticks(&state).await.unwrap();
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &idle).await,
        1,
        "event×idle 应背靠背自动排出首 tick"
    );
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &chap).await,
        0,
        "event×chapter 不应被调度器自动排 tick（手动端点驱动）"
    );
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &arena).await,
        0,
        "event×arena 不应被调度器自动排 tick（手动端点驱动）"
    );

    // 手动端点排 tick（镜像 chapter start / arena host_tick 内的 schedule_tick）→ 各恰一个 tick。
    assert_eq!(super::schedule_tick(&state, &chap).await.unwrap(), Some(0), "手动端点排下 chapter 首 tick");
    assert_eq!(super::schedule_tick(&state, &arena).await.unwrap(), Some(0), "手动端点排下 arena 首 tick");
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &chap).await,
        1
    );
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &arena).await,
        1
    );

    // 再轮调度：chapter/arena 仍不追加自动 tick（保持手动排的 1 个）；idle 首 tick 仍 pending → 不背靠背再排。
    super::schedule_due_ticks(&state).await.unwrap();
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &chap).await,
        1,
        "调度器不应给 event×chapter 追加自动 tick"
    );
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &arena).await,
        1,
        "调度器不应给 event×arena 追加自动 tick"
    );
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &idle).await,
        1,
        "idle 首 tick 未 done（pending），本轮 outstanding≠0 → 不背靠背再排"
    );
}

/// 钉 2 地点图 + 2 NPC 落在 north；玩家默认起点 = 首个非秘境地点（id 序 hall<north → hall）。
async fn pin_two_locations_with_npcs(db: &AnyPool, world_id: &str) {
    let n1: serde_json::Value = serde_json::from_str(&sample_card_json("npcN1", "北境甲")).unwrap();
    let n2: serde_json::Value = serde_json::from_str(&sample_card_json("npcN2", "北境乙")).unwrap();
    let assembled = json!({
        "assembly": {
            "worldCharacterEntries": [
                { "characterId": "npcN1", "card": n1, "location": "north", "carriedItems": [] },
                { "characterId": "npcN2", "card": n2, "location": "north", "carriedItems": [] }
            ],
            "locationGraph": [
                { "id": "hall", "name": "前厅", "connections": ["north"] },
                { "id": "north", "name": "北境", "connections": ["hall"] }
            ]
        }
    });
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
        .bind(assembled.to_string())
        .bind(world_id)
        .execute(db)
        .await
        .unwrap();
}

/// event×arena 地点碰撞：同一 event 步的 cohort 恒同一 location（逐地点串行），跨步各自独立 revision。
/// 首步锚地点 = 字典序最小空闲角色 location = hall（ccolA<npcN1）→ cohort 仅 hall 玩家；NPC 留待次步在 north 成组。
#[tokio::test]
async fn event_arena_collision_by_location() {
    let state = test_state().await;
    seed_template_soft(&state.db, "tpl-col").await;
    let wid = running_world_for_endgame(&state, "col", "tpl-col", "event", "arena").await;
    pin_two_locations_with_npcs(&state.db, &wid).await;

    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // event 步 0：min next_time=0（全体首步入场），锚 = hall → cohort 仅 hall 的两玩家（NPC 未激活）。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    let w0 = load_world(&state.db, &wid).await.unwrap();
    let rev0 = w0.state_revision;
    let st0: NarrativeState = serde_json::from_str(&w0.narrative_state_json).unwrap();
    let mut act0: Vec<String> = st0.timeline.next_time.keys().cloned().collect();
    act0.sort();
    assert_eq!(
        act0,
        vec!["ccolA".to_string(), "ccolB".to_string()],
        "首步 cohort 仅 hall 的两玩家推进 next_time；north 的 NPC 未在同步激活"
    );
    for c in &act0 {
        assert_eq!(st0.characters[c].location, "hall", "首步 cohort 恒同一 location=hall");
    }

    // event 步 1：min next_time=0（NPC 缺席 next_time→now=0），锚 = north → cohort 仅 north 的 2 NPC。
    insert_tick(&state.db, &wid, 1, rev0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let w1 = load_world(&state.db, &wid).await.unwrap();
    let rev1 = w1.state_revision;
    let st1: NarrativeState = serde_json::from_str(&w1.narrative_state_json).unwrap();
    assert!(
        st1.timeline.next_time.contains_key("npcN1") && st1.timeline.next_time.contains_key("npcN2"),
        "第二步激活 north 的 2 NPC（新入 next_time）"
    );
    assert_eq!(st1.characters["npcN1"].location, "north", "第二步 cohort 恒同一 location=north");
    assert_eq!(st1.characters["npcN2"].location, "north");
    assert!(rev1 > rev0, "逐地点串行 → 两步各自独立 revision（rev1 > rev0）");
}

// ---------- 僵局打破提示（B. stall hint）：StallTracker 计数/清零/阈值触发（不跑 LLM） ----------

#[test]
fn stall_tracker_streak_below_threshold_no_hint() {
    let t = crate::runtime::StallTracker::default();
    // 初始无账 → 无提示。
    assert!(t.hint("w-a").is_none());
    // 1 次 blocked（streak=1 < 2）→ 仍无提示。
    t.record_blocked("w-a", "仲裁阻断：甲的行动与硬约束冲突");
    assert!(t.hint("w-a").is_none(), "streak=1 未达阈值不应给提示");
}

#[test]
fn stall_tracker_hint_at_threshold_with_streak_and_reason() {
    let t = crate::runtime::StallTracker::default();
    t.record_blocked("w-b", "仲裁阻断：旧原因");
    t.record_blocked("w-b", "仲裁阻断：乙的行动与硬约束冲突");
    // streak=2 达阈值 → 提示含「最近一次」原因与连续次数。
    let hint = t.hint("w-b").expect("streak=2 应给提示");
    assert!(hint.contains("仲裁阻断：乙的行动与硬约束冲突"), "应含最近原因：{hint}");
    assert!(hint.contains('2'), "应含连续次数：{hint}");
    assert!(!hint.contains("旧原因"), "只保留最近原因：{hint}");
    // 继续 blocked：streak=3 更新次数。
    t.record_blocked("w-b", "仲裁阻断：乙的行动与硬约束冲突");
    assert!(t.hint("w-b").unwrap().contains('3'));
}

#[test]
fn stall_tracker_clear_resets_streak() {
    let t = crate::runtime::StallTracker::default();
    t.record_blocked("w-c", "原因");
    t.record_blocked("w-c", "原因");
    assert!(t.hint("w-c").is_some());
    // 提交成功 → 清零；再次 blocked 从 1 重新计数。
    t.clear("w-c");
    assert!(t.hint("w-c").is_none(), "清零后不应再有提示");
    t.record_blocked("w-c", "原因");
    assert!(t.hint("w-c").is_none(), "清零后重新计数，1 次未达阈值");
}

#[test]
fn stall_tracker_isolated_per_world() {
    let t = crate::runtime::StallTracker::default();
    t.record_blocked("w-d1", "原因一");
    t.record_blocked("w-d1", "原因一");
    // 世界间互不串账。
    assert!(t.hint("w-d1").is_some());
    assert!(t.hint("w-d2").is_none());
    t.clear("w-d2");
    assert!(t.hint("w-d1").is_some(), "清别的世界不影响本世界");
}

// ==================== 波次 2：idle 终局历练（每在场卡 +60，与终局停机同事务、只发一次） ====================

/// idle 房终局：真正结算（Concluded）那一次，每张在场卡 +60，与 end_world/终局产出同事务；
/// ended 后的遗留 tick（world_not_running noop）不重复结算、不再发历练。
/// 发放实现收在 progression 模块（settle_idle_world_ending_tx）——runtime/mod.rs 源码级不引用
/// 历练字段（红线 grep 断言见 progression::tests）。
#[tokio::test]
async fn idle_world_ending_grants_growth_to_present_cards_once() {
    let state = test_state().await;
    seed_template_with_endgame(
        &state.db,
        "tpl-mlg",
        "idle",
        json!([{ "id": "n1", "summary": "寒暄", "constraint": "soft" }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 1 }),
    )
    .await;
    let wid = running_world_for_endgame(&state, "mlg", "tpl-mlg", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0：未到上限 → Done，世界仍 running，不发终局历练。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "cmlgA").await, 0);
    assert_eq!(i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "cmlgB").await, 0);

    // tick 1：到 max_world_ticks → Concluded（真正结算那一次）→ 两张在场卡各 +60。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(),
        TickStatus::Concluded
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended");
    assert_eq!(
        i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "cmlgA").await,
        60,
        "idle 终局每在场卡 +60（与终局停机同事务）"
    );
    assert_eq!(
        i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "cmlgB").await,
        60,
        "两张在场卡都应获得终局历练"
    );

    // 遗留 tick：world_not_running noop → 不重复结算、不双发历练。
    insert_tick(&state.db, &wid, 2, 2).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 2, model.clone()).await.unwrap(),
        TickStatus::Skipped("world_not_running")
    );
    assert_eq!(i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "cmlgA").await, 60);
    assert_eq!(i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "cmlgB").await, 60);
}

// ==================== R1 三层结算 ③ 世界线层：贡献归因 → 公示产出表 → 确定发放 ====================

/// 模板：主线里程碑 + endgame + **公示产出表**（骨架内声明，装配时钉进 assembled_json）+ 模板星级。
/// 走 skeleton → assemble_instance → assembled_json 全链路，验证产出表不是测试凭空塞进去的。
async fn seed_template_worldline(
    db: &AnyPool,
    id: &str,
    mainline: serde_json::Value,
    endgame: serde_json::Value,
    payout: serde_json::Value,
    star_rating: i64,
) {
    let skeleton = json!({
        "mainlineNodes": mainline,
        "forbiddenPredicates": [],
        "endgame": endgame,
        "payoutTable": payout,
    });
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, \
         version, moderation, star_rating, created_at) \
         VALUES ($1, '产出表模板', 'idle', $2, '{\"mode\":\"open\"}', 1, 1, 'approved', $3, $4)",
    )
    .bind(id)
    .bind(skeleton.to_string())
    .bind(star_rating)
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 两档公示产出表：见证（≥1.0 → 20 历练）/ 推动（≥3.0 → 80 历练 + powerTier 2 的世界线结晶）。
fn two_tier_payout(template_id: &str) -> serde_json::Value {
    json!({
        "worldlineTiers": [
            { "label": "见证", "minScore": 1.0, "mileage": 20 },
            { "label": "推动", "minScore": 3.0, "mileage": 80,
              "item": { "id": "wl_relic", "narrative": "世界线结晶", "effectTags": ["memento:worldline"],
                        "origin": { "worldTemplateId": template_id, "cosmology": ["myth"], "powerTier": 2 } } },
        ]
    })
}

/// (score_milli, milestone_score_milli, settled_at)
async fn contribution_of(db: &AnyPool, wid: &str, cid: &str) -> (i64, i64, i64) {
    sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT score_milli, milestone_score_milli, settled_at FROM world_contributions \
         WHERE world_id=$1 AND character_id=$2",
    )
    .bind(wid)
    .bind(cid)
    .fetch_optional(db)
    .await
    .unwrap()
    .unwrap_or((0, 0, 0))
}

async fn backpack_hook_count(db: &AnyPool, user: &str, hook_key: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM backpacks WHERE user_id=$1 AND reward_hook_key=$2")
        .bind(user)
        .bind(hook_key)
        .fetch_one(db)
        .await
        .unwrap()
}

/// ③ 世界线层主链路：每 tick 逐角色贡献分入独立账本 world_contributions（**不进 narrative_state_json**）
/// → 终局时按贡献分查**公示产出表**确定发放（历练 + 稀有产出，零随机数）→ 留全链审计。
/// 同贡献分的两张卡必得完全相同的产出（确定性）。
#[tokio::test]
async fn worldline_settlement_pays_by_public_payout_table() {
    let state = test_state().await;
    seed_template_worldline(
        &state.db,
        "tpl-wl",
        // 高阈值里程碑：永远推不完 → 引擎不判 MainlineDone，世界跑到 maxWorldTicks 收束；
        // 但每 tick 的强度都被喂进 milestoneProgress → 在场者即"里程碑推动者"。
        json!([{ "id": "m1", "summary": "共谋", "constraint": "soft", "threshold": 100.0 }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 2 }),
        two_tier_payout("tpl-wl"),
        3,
    )
    .await;
    let wid = running_world_for_endgame(&state, "wl", "tpl-wl", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // tick 0：每角色 Success(1.0) + willSpeak(0.25) = 1.25，且喂进里程碑 → 两列同步累积。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(
        contribution_of(&state.db, &wid, "cwlA").await,
        (1250, 1250, 0),
        "tick 0 应记 1.25 分（×1000 定点），里程碑列同步，未结算"
    );

    // 产出表随实例钉住（skeleton → assemble_instance → assembled_json），不是运行期临时拼的。
    let raw: String = sqlx::query_scalar("SELECT assembled_json FROM worlds WHERE id=$1")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    let wrapper: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        wrapper["assembly"]["payoutTable"]["worldlineTiers"].as_array().unwrap().len(),
        2,
        "公示产出表应随实例钉进 assembled_json"
    );
    assert_eq!(wrapper["starRating"], json!(3));

    // 贡献分绝不进引擎状态（平权红线）：narrative_state_json 里不得出现贡献账本痕迹。
    let ns: String = sqlx::query_scalar("SELECT narrative_state_json FROM worlds WHERE id=$1")
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(
        !ns.contains("contribution") && !ns.contains("payout"),
        "贡献分/产出绝不能写进会回灌引擎的 narrative_state_json（§0.1 平权红线）"
    );

    // tick 1：累积到 2.5。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(contribution_of(&state.db, &wid, "cwlA").await.1, 2500);

    // tick 2：到 maxWorldTicks → 正常收束（非崩塌）。本拍的贡献先入账（3.75）再结算 → 命中"推动"档。
    insert_tick(&state.db, &wid, 2, 2).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 2, model.clone()).await.unwrap(),
        TickStatus::Concluded
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended");

    for (cid, uid) in [("cwlA", "uwlA"), ("cwlB", "uwlB")] {
        let (score, milestone, settled) = contribution_of(&state.db, &wid, cid).await;
        assert_eq!(milestone, 3750, "{cid} 三拍各 1.25 分，终局那拍必须计入");
        assert_eq!(score, milestone, "本例每拍都喂了里程碑，全量与里程碑口径应一致");
        assert!(settled > 0, "{cid} 结算后应打上 settled_at 幂等标记");
        // ① 保底 60 + ③ 推动档 80。
        assert_eq!(
            i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", cid).await,
            140,
            "{cid} 应得 ① 出席 60 + ③ 世界线「推动」档 80"
        );
        assert_eq!(
            backpack_hook_count(&state.db, uid, &format!("{wid}:{cid}:worldline")).await,
            1,
            "{uid} 应收到该档的世界线产出（发给卡的主人）"
        );
    }

    // 全链审计留痕（§0.2）。
    assert_eq!(
        i64_one(
            &state.db,
            "SELECT COUNT(*) FROM audit_logs WHERE action='world.worldline_settled' AND subject=$1",
            &wid
        )
        .await,
        1,
        "③ 层结算应留一条可溯审计"
    );

    // 无抽卡：产出既不经计费也无任何随机来源——两张同分卡拿到逐字段一致的产出。
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM ledger_entries", "").await,
        0,
        "确定性产出不经任何计费/账本路径"
    );
}

/// 世界线崩塌（关键角色退场）：③ 归零 · ① 减半 · ② 已锁定产出原样保留。
#[tokio::test]
async fn worldline_collapse_zeroes_tier3_halves_baseline_and_keeps_locked_items() {
    let state = test_state().await;
    seed_template_worldline(
        &state.db,
        "tpl-col",
        json!([{ "id": "m1", "summary": "共谋", "constraint": "soft", "threshold": 100.0 }]),
        // 关键角色 ccolA 退场即崩塌；maxWorldTicks 极大，排除时间上限干扰。
        json!({ "minWorldTicks": 0, "maxWorldTicks": 100000, "keyCharacterIds": ["ccolA"] }),
        two_tier_payout("tpl-col"),
        3,
    )
    .await;
    let wid = running_world_for_endgame(&state, "col", "tpl-col", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    // ② 成就层：崩塌前已「完成即锁定」的钩子产出（与本层无关的既有发货路径）。
    let locked = crate::admission::ItemDefinition {
        id: "locked_hook_item".into(),
        narrative: "已锁定的钩子信物".into(),
        effect_tags: vec!["memento:hook".into()],
        origin: crate::admission::ItemOrigin {
            world_template_id: "tpl-col".into(),
            cosmology: vec!["myth".into()],
            power_tier: 1,
        },
    };
    crate::backpack::grant_item(&state.db, "ucolB", &locked, &wid).await.unwrap();

    // tick 0：正常推进，两角色各累积 1.25 分（≥ 见证档 1.0 门槛，正常收束下本可得 ③ 产出）。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    assert_eq!(contribution_of(&state.db, &wid, "ccolB").await.1, 1250);

    // 关键角色 ccolA 永久退场 → 世界线崩塌。
    sqlx::query("UPDATE world_members SET status='left' WHERE world_id=$1 AND cloud_character_id=$2")
        .bind(&wid)
        .bind("ccolA")
        .execute(&state.db)
        .await
        .unwrap();

    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(),
        TickStatus::Concluded
    );
    assert_eq!(world_status(&state.db, &wid).await, "ended");

    // ① 减半：出席产出 60 → 30（不是 0，也不是 60，更不含任何 ③ 层加成）。
    assert_eq!(
        i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "ccolB").await,
        30,
        "崩塌 → ① 保底层减半（60×0.5），且 ③ 归零（否则会是 30+20 或 60+20）"
    );
    // ③ 归零：不发任何世界线产出，但结算占位已落（不留重复结算空子）。
    assert_eq!(
        backpack_hook_count(&state.db, "ucolB", &format!("{wid}:ccolB:worldline")).await,
        0,
        "崩塌 → ③ 世界线层不发放任何产出"
    );
    assert!(contribution_of(&state.db, &wid, "ccolB").await.2 > 0, "崩塌也要打幂等标记");
    assert_eq!(
        i64_one(
            &state.db,
            "SELECT COUNT(*) FROM audit_logs WHERE action='world.worldline_settled' AND subject=$1",
            &wid
        )
        .await,
        0,
        "③ 归零时无产出可审计"
    );
    // ② 保留：崩塌不回收任何已锁定产出（锁定语义正为此设计——本层根本不参与崩塌折算）。
    let locked_kept: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM backpacks WHERE user_id=$1 AND item_id=$2")
            .bind("ucolB")
            .bind("locked_hook_item")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(locked_kept, 1, "崩塌 → ② 已锁定的钩子产出原样保留，一件不回收");
}

/// 产出封顶不可绕过：产出表声明的道具 powerTier > 实例星级 → 剔除（不降级、不替换），历练照发。
#[tokio::test]
async fn worldline_payout_never_exceeds_star_rating_power_tier_cap() {
    let state = test_state().await;
    seed_template_worldline(
        &state.db,
        "tpl-cap3",
        json!([{ "id": "m1", "summary": "共谋", "constraint": "soft", "threshold": 100.0 }]),
        json!({ "minWorldTicks": 0, "maxWorldTicks": 1 }),
        json!({
            "worldlineTiers": [
                { "label": "越顶档", "minScore": 1.0, "mileage": 50,
                  "item": { "id": "over_tier_relic", "narrative": "超规格神兵", "effectTags": ["advantage:combat"],
                            "origin": { "worldTemplateId": "tpl-cap3", "cosmology": ["myth"], "powerTier": 4 } } },
            ]
        }),
        1, // 1★ 实例：powerTier 4 的产出必须被剔除
    )
    .await;
    let wid = running_world_for_endgame(&state, "cap3", "tpl-cap3", "event", "idle").await;
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 10, output_tokens: 20 });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(),
        TickStatus::Concluded
    );

    // 历练照发（① 60 + ③ 50），但超顶道具一件都没进包。
    assert_eq!(
        i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id=$1", "ccap3A").await,
        110
    );
    assert_eq!(
        backpack_hook_count(&state.db, "ucap3A", &format!("{wid}:ccap3A:worldline")).await,
        0,
        "powerTier 4 > 1★ → 产出封顶剔除，绝不绕过"
    );
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM items WHERE id=$1", "over_tier_relic").await,
        0,
        "超顶道具连定义都不该落库（grant_item_tx 根本没被调用）"
    );
}

// ---------- 身份池叙事接线（总规格 §5【拍板 4、5】：身份 = 开局站位） ----------
//
// 🔴 平权红线（§0.1）：身份**只**进感知层 `other_cards_brief`，不携带数值差异 / 准入门槛 /
//    产出加成 / 难度优待 / 叙事特权，且**绝不进 active_cards**（角色卡不可变快照）。
//    下方 `identity_never_pollutes_active_cards_redline` 就是这条红线的断言。

use crate::runtime::{
    brief_with_identity, load_identity_display_names, parse_identity_assignments, parse_identity_labels,
};

/// 捕获每次 roleDecide 的 user prompt（内含 `assemble_visible_context` 的可见上下文 JSON），
/// 供断言"他人如何被本角色感知"。其余环节与 MockModel 同款合法占位 JSON。
struct CapturingMock {
    decide_prompts: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl ModelClient for CapturingMock {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        if spec.agent == "roleDecide" {
            self.decide_prompts.lock().unwrap().push(spec.user.clone());
        }
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"堂前灯火通明，众人各自落座。"}"#,
            "roleDecide" => r#"{"intent":"观望","action":"上前拱手行礼","speak":{"willSpeak":true,"purpose":"寒暄"},"targets":[],"acceptableCosts":[],"predictions":[]}"#,
            "arbiter" => r#"{"outcomes":[]}"#,
            "writer" => r#"{"prose":"堂中礼数周全，暗流未起。"}"#,
            "critic" => r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#,
            _ => "{}",
        };
        Ok(ModelOutput { content: content.to_string(), input_tokens: Some(10), output_tokens: Some(20) })
    }
}

/// 从 roleDecide user prompt 中切出可见上下文 JSON（`build_decide_user_prompt` 的固定包裹）。
fn decide_ctx(user: &str) -> serde_json::Value {
    let start = user.find('{').expect("可见上下文 JSON 起点");
    let end = user.rfind("\n\n请完全代入").expect("可见上下文 JSON 终点");
    serde_json::from_str(&user[start..end]).expect("可见上下文必须是合法 JSON")
}

/// 取指定角色本次 tick 的可见上下文（`ctx["you"] == cid`）。
fn ctx_of(prompts: &[String], cid: &str) -> serde_json::Value {
    prompts
        .iter()
        .map(|p| decide_ctx(p))
        .find(|c| c["you"] == json!(cid))
        .unwrap_or_else(|| panic!("未捕获到 {cid} 的决策上下文"))
}

/// 带 identityPool 的模板（label 齐全）+ 一个软主线节点（不秒终局）。
async fn seed_template_with_identity_pool(db: &AnyPool, id: &str, pool: serde_json::Value) {
    let skeleton = json!({
        "mainlineNodes": [{ "id": "n1", "summary": "堂前议事", "fated": false, "constraint": "soft" }],
        "forbiddenPredicates": [],
        "identityPool": pool,
    });
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES ($1, '身份池模板', 'idle', $2, '{\"mode\":\"open\"}', 1, 1, 'approved', $3)",
    )
    .bind(id)
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

fn standard_identity_pool() -> serde_json::Value {
    json!([
        { "id": "official", "label": "户部主事", "quota": 1, "themes": ["朝堂"] },
        { "id": "merchant", "label": "漕帮商贾", "quota": 1, "themes": ["行商"] }
    ])
}

/// 起一个「模板声明 identityPool + 实例已钉住分配结果」的 running 世界（chA=official / chB=merchant）。
/// 预钉 assembled_json ⇒ 5.5 段装配短路，分配结果就是本用例钉的这份（与装配层解耦）。
async fn running_world_with_identities(
    state: &AppState,
    tag: &str,
    pool: serde_json::Value,
    assignments: serde_json::Value,
) -> String {
    let tpl = format!("tpl-ident-{tag}");
    seed_template_with_identity_pool(&state.db, &tpl, pool).await;
    seed_model_routes(&state.db, "test-routes").await;
    seed_user(&state.db, "uA").await;
    seed_user(&state.db, "uB").await;
    seed_char(&state.db, "chA", "uA", "李").await;
    seed_char(&state.db, "chB", "uB", "王").await;

    let mut p = CreateWorldParams::official(&tpl, 1, "身份池世界");
    p.status = Some("running".into());
    p.model_route_version = Some("test-routes".into());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    let wid = create_world(&state.db, p).await.unwrap();
    seed_member(&state.db, &wid, "uA", "chA").await;
    seed_member(&state.db, &wid, "uB", "chB").await;

    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
        .bind(json!({ "assembly": { "identityAssignments": assignments } }).to_string())
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    wid
}

/// 有身份分配 → 感知层带上开局站位；且**跨 tick 恒等**（确定性：同输入同输出）。
#[tokio::test]
async fn identity_assignments_decorate_other_cards_brief() {
    let state = test_state().await;
    let wid = running_world_with_identities(
        &state,
        "basic",
        standard_identity_pool(),
        json!([["chA", "official"], ["chB", "merchant"]]),
    )
    .await;
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);

    let tick0 = prompts.lock().unwrap().clone();
    // chA 眼里的 chB：名字 + 开局站位（others 剔除自己 ⇒ 只有 chB）。
    assert_eq!(
        ctx_of(&tick0, "chA")["others"],
        json!({ "chB": "王（漕帮商贾）" }),
        "身份展示名必须织进 other_cards_brief"
    );
    assert_eq!(ctx_of(&tick0, "chB")["others"], json!({ "chA": "李（户部主事）" }));

    // 确定性：同一 (world_id, 阵容, template_version) 下一 tick 得到逐字节相同的身份呈现。
    prompts.lock().unwrap().clear();
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model.clone()).await.unwrap(), TickStatus::Done);
    let tick1 = prompts.lock().unwrap().clone();
    assert_eq!(
        ctx_of(&tick1, "chA")["others"],
        ctx_of(&tick0, "chA")["others"],
        "身份呈现必须确定性：同输入同输出，不得随 tick / 迭代序漂移"
    );
}

/// 🔴 红线：身份**绝不**进 active_cards——角色卡快照一个字节都不许被改。
/// 断言口径：本人可见上下文里的 `yourDna.identity.name`（直接来自 active_cards 的卡）必须是原始卡名；
/// DB 里的 card_json 也必须原样。
#[tokio::test]
async fn identity_never_pollutes_active_cards_redline() {
    let state = test_state().await;
    let wid = running_world_with_identities(
        &state,
        "redline",
        standard_identity_pool(),
        json!([["chA", "official"], ["chB", "merchant"]]),
    )
    .await;
    let before = text_one(&state.db, "SELECT card_json FROM cloud_characters WHERE id=$1", "chA").await;
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    let captured = prompts.lock().unwrap().clone();
    let a = ctx_of(&captured, "chA");
    assert_eq!(
        a["yourDna"]["identity"]["name"],
        json!("李"),
        "红线：角色卡快照（active_cards）不得被身份污染"
    );
    assert!(
        !a["yourDna"].to_string().contains("户部主事"),
        "红线：身份不得出现在角色卡任何字段里"
    );
    assert_eq!(
        text_one(&state.db, "SELECT card_json FROM cloud_characters WHERE id=$1", "chA").await,
        before,
        "红线：云端角色卡原文必须逐字节不变"
    );
}

/// 有身份分配 → **角色本人**也看得见自己的开局站位（`RoundInput.self_identities` 回灌）。
///
/// 这是本节修的产品缺陷：引擎 `decide` 组装可见上下文时恒剔除自己，故 brief 里的身份
/// 只有别人看得见，角色本人反而感知不到自己的站位。
#[tokio::test]
async fn identity_assignments_reach_own_decide_context() {
    let state = test_state().await;
    let wid = running_world_with_identities(
        &state,
        "self",
        standard_identity_pool(),
        json!([["chA", "official"], ["chB", "merchant"]]),
    )
    .await;
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);

    let tick0 = prompts.lock().unwrap().clone();
    let a = ctx_of(&tick0, "chA");
    let b = ctx_of(&tick0, "chB");
    assert_eq!(a["yourIdentity"]["display"], json!("户部主事"), "本人必须看得见自己的开局站位");
    assert_eq!(b["yourIdentity"]["display"], json!("漕帮商贾"));
    assert!(
        a["yourIdentity"]["note"].as_str().unwrap_or_default().contains("开局站位"),
        "措辞必须让模型明白这是开局站位"
    );
    // 与感知层同源同值：别人怎么看你（brief）和你知道自己是谁（self_identities）不可能错位。
    assert_eq!(b["others"], json!({ "chA": "李（户部主事）" }));
    // 信息边界：他人的自身身份条目绝不越界（他人身份只经 brief 呈现）。
    assert!(a["yourIdentity"].to_string().contains("户部主事"));
    assert!(!a["yourIdentity"].to_string().contains("漕帮商贾"), "chA 不得看见 chB 的自身身份");
    assert!(!b["yourIdentity"].to_string().contains("户部主事"), "chB 不得看见 chA 的自身身份");
    // 🔴 红线：身份**只**进感知层，绝不进角色卡快照（active_cards → yourDna）。
    assert_eq!(a["yourDna"]["identity"]["name"], json!("李"));
    assert!(!a["yourDna"].to_string().contains("户部主事"), "红线：身份不得进角色卡");

    // 确定性：下一 tick 逐字节相同（同 world_id / 阵容 / 模板版本 → 同一份身份呈现）。
    prompts.lock().unwrap().clear();
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model).await.unwrap(), TickStatus::Done);
    assert_eq!(
        ctx_of(&prompts.lock().unwrap().clone(), "chA")["yourIdentity"],
        a["yourIdentity"],
        "自身身份呈现必须确定性：不得随 tick / 迭代序漂移"
    );
}

/// 老世界（模板无 identityPool、实例无 identityAssignments）→ 感知层逐字段与接线前一致。
#[tokio::test]
async fn legacy_world_without_identity_pool_is_byte_identical() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await; // tpl-x：无 identityPool、assembled_json 由装配层产
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    let captured = prompts.lock().unwrap().clone();
    assert_eq!(ctx_of(&captured, "chA")["others"], json!({ "chB": "王" }), "老世界：brief 只放名字");
    assert_eq!(ctx_of(&captured, "chB")["others"], json!({ "chA": "李" }), "老世界：brief 只放名字");
}

/// 老世界的**自身身份通道**同样完全退化：上下文里根本不出现 `yourIdentity`（不传即零变化）。
#[tokio::test]
async fn legacy_world_has_no_self_identity_field() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await; // tpl-x：无 identityPool
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    let captured = prompts.lock().unwrap().clone();
    for cid in ["chA", "chB"] {
        assert!(
            ctx_of(&captured, cid).get("yourIdentity").is_none(),
            "老世界：{cid} 的上下文里不得出现 yourIdentity"
        );
    }
}

/// `assembled_json` 结构损坏 → 自身身份通道静默退化为空，**不 panic、不阻断 tick**。
/// （与 brief 同一份 `load_identity_display_names` 读回结果，故退化口径必然一致。）
#[tokio::test]
async fn broken_assembled_json_leaves_self_identity_absent() {
    let state = test_state().await;
    let wid = running_world_with_identities(
        &state,
        "broken-self",
        standard_identity_pool(),
        json!("这不是数组"),
    )
    .await;
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    // ① identityAssignments 字段类型不符。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(),
        TickStatus::Done,
        "结构损坏不得阻断 tick"
    );
    assert!(
        ctx_of(&prompts.lock().unwrap().clone(), "chA").get("yourIdentity").is_none(),
        "结构损坏 → 自身身份字段完全不出现"
    );

    // ② 整段 assembled_json 直接是非 JSON 文本。
    sqlx::query("UPDATE worlds SET assembled_json='{ 这不是 JSON' WHERE id=$1")
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    prompts.lock().unwrap().clear();
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model).await.unwrap(),
        TickStatus::Done,
        "assembled_json 非 JSON 同样静默退化，不阻断 tick"
    );
    assert!(ctx_of(&prompts.lock().unwrap().clone(), "chA").get("yourIdentity").is_none());
}

/// 分配只覆盖一部分成员（池配额 < 人数）→ 未分配者不出现该字段，已分配者照常可见；
/// 退化是**逐角色**的，不是全有全无。
#[tokio::test]
async fn partial_assignment_degrades_per_character() {
    let state = test_state().await;
    let wid = running_world_with_identities(
        &state,
        "partial",
        standard_identity_pool(),
        json!([["chA", "official"]]), // chB 未分配
    )
    .await;
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    let captured = prompts.lock().unwrap().clone();
    assert_eq!(ctx_of(&captured, "chA")["yourIdentity"]["display"], json!("户部主事"));
    assert!(ctx_of(&captured, "chB").get("yourIdentity").is_none(), "未分配 → 字段完全不出现");
    // 感知层同口径：chB 无身份 → 别人看他也只是名字。
    assert_eq!(ctx_of(&captured, "chA")["others"], json!({ "chB": "王" }));
}

/// `label` 为空 → 展示名回落身份 `id`（模板字段说明的既定口径）。
#[tokio::test]
async fn empty_label_falls_back_to_identity_id() {
    let state = test_state().await;
    let wid = running_world_with_identities(
        &state,
        "fallback",
        json!([
            { "id": "official", "label": "", "quota": 1 },
            { "id": "merchant", "quota": 1 }
        ]),
        json!([["chA", "official"], ["chB", "merchant"]]),
    )
    .await;
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    let captured = prompts.lock().unwrap().clone();
    assert_eq!(ctx_of(&captured, "chA")["others"], json!({ "chB": "王（merchant）" }), "label 缺失 → 回落 id");
    assert_eq!(ctx_of(&captured, "chB")["others"], json!({ "chA": "李（official）" }), "label 为空串 → 回落 id");
}

/// `assembled_json` 结构损坏（identityAssignments 不是数组 / 条目不是二元组）→ 静默退化，
/// 不 panic、不阻断 tick，感知层与老世界完全一致。
#[tokio::test]
async fn broken_assembled_json_degrades_silently_without_blocking_tick() {
    let state = test_state().await;
    let wid = running_world_with_identities(
        &state,
        "broken",
        standard_identity_pool(),
        json!("这不是数组"),
    )
    .await;
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model: Arc<dyn ModelClient> = Arc::new(CapturingMock { decide_prompts: prompts.clone() });

    // ① identityAssignments 字段类型不符（字符串而非数组）。
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(),
        TickStatus::Done,
        "结构损坏不得阻断 tick"
    );
    assert_eq!(ctx_of(&prompts.lock().unwrap().clone(), "chA")["others"], json!({ "chB": "王" }));

    // ② 再叠一层：整段 assembled_json 直接写成非 JSON 文本。
    sqlx::query("UPDATE worlds SET assembled_json='{ 这不是 JSON' WHERE id=$1")
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    prompts.lock().unwrap().clear();
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model).await.unwrap(),
        TickStatus::Done,
        "assembled_json 非 JSON 同样静默退化"
    );
    assert_eq!(ctx_of(&prompts.lock().unwrap().clone(), "chA")["others"], json!({ "chB": "王" }));
}

// ---------- 纯函数层：解析 / 退化 / 拼接 ----------

#[test]
fn parse_identity_assignments_is_defensive() {
    assert!(parse_identity_assignments(None).is_empty(), "无 assembled_json → 退化");
    assert!(parse_identity_assignments(Some("{ 坏 JSON")).is_empty(), "非 JSON → 退化");
    assert!(parse_identity_assignments(Some("{}")).is_empty(), "无 assembly 段 → 退化");
    assert!(
        parse_identity_assignments(Some(&json!({ "assembly": {} }).to_string())).is_empty(),
        "无 identityAssignments 字段（老实例 skip_serializing_if）→ 退化"
    );
    assert!(
        parse_identity_assignments(Some(
            &json!({ "assembly": { "identityAssignments": "x" } }).to_string()
        ))
        .is_empty(),
        "字段类型不符 → 退化"
    );
    // 损坏条目逐条跳过，合法条目照收（顺序 = 装配层钉住的 cid 升序，确定性来源）。
    let raw = json!({ "assembly": { "identityAssignments": [
        ["chA", "official"],
        ["chB"],                 // 缺元素
        ["", "merchant"],        // 空 cid
        ["chC", "  "],           // 空身份 id
        "不是数组",
        [1, 2],                  // 非字符串
        ["chD", "wanderer"],
    ] } })
    .to_string();
    assert_eq!(
        parse_identity_assignments(Some(&raw)),
        vec![("chA".to_string(), "official".to_string()), ("chD".to_string(), "wanderer".to_string())],
    );
}

#[test]
fn parse_identity_labels_prefers_label_then_id() {
    assert!(parse_identity_labels("{ 坏 JSON").is_empty());
    assert!(parse_identity_labels("{}").is_empty(), "模板未声明 identityPool → 空表 → 完全退化");
    let pool = json!({ "identityPool": [
        { "id": "official", "label": "户部主事" },
        { "id": "merchant", "label": "   " },   // 空白 label → 回落 id
        { "id": "wanderer" },                    // 无 label → 回落 id
        { "label": "无 id 不可分配" },            // 无 id → 跳过
    ] })
    .to_string();
    let labels = parse_identity_labels(&pool);
    assert_eq!(labels.get("official").map(String::as_str), Some("户部主事"));
    assert_eq!(labels.get("merchant").map(String::as_str), Some("merchant"));
    assert_eq!(labels.get("wanderer").map(String::as_str), Some("wanderer"));
    assert_eq!(labels.len(), 3, "无 id 条目不得入表");
}

#[test]
fn brief_with_identity_degrades_to_plain_name() {
    let label = "户部主事".to_string();
    assert_eq!(brief_with_identity("唐三", Some(&label)), "唐三（户部主事）");
    assert_eq!(brief_with_identity("唐三", None), "唐三", "无身份 → 与接线前逐字节一致");
    assert_eq!(brief_with_identity("唐三", Some(&"  ".to_string())), "唐三", "空白展示名 → 退化");
    assert_eq!(brief_with_identity("", Some(&label)), "户部主事", "卡名为空 → 不产孤零零的括号");
}

/// 分配非空但模板已不再声明 identityPool（模板被改过）→ 完全退化，不回落到裸 id。
#[tokio::test]
async fn assignments_without_pool_in_template_degrade_completely() {
    let state = test_state().await;
    seed_template(&state.db, "tpl-nopool").await; // 无 identityPool
    let mut p = CreateWorldParams::official("tpl-nopool", 1, "无池世界");
    p.status = Some("running".into());
    let wid = create_world(&state.db, p).await.unwrap();
    sqlx::query("UPDATE worlds SET assembled_json=$1 WHERE id=$2")
        .bind(json!({ "assembly": { "identityAssignments": [["chA", "official"]] } }).to_string())
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    let world = load_world(&state.db, &wid).await.unwrap();
    assert!(
        load_identity_display_names(&state.db, &world).await.is_empty(),
        "模板无 identityPool → 完全退化（不回落裸 id）"
    );

    // 模板行整个查不到（脏数据）同样退化。
    sqlx::query("UPDATE worlds SET template_id='tpl-missing' WHERE id=$1")
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    let world = load_world(&state.db, &wid).await.unwrap();
    assert!(load_identity_display_names(&state.db, &world).await.is_empty(), "模板缺失 → 退化，不 panic");
}

// ---------- 叙事 critic 报告落库（叙事质量 SLO「状态-文本矛盾率」数据源，docs/VALIDATION.md §4.2 ①） ----------
//
// 🔴 平权红线（§0.1）：critic 是**对模型自身产出的评价**，只进独立表 `world_tick_critic`，
//    **绝不进 `worlds.narrative_state_json`** —— 那份 JSON 每 tick 回灌进引擎 `RoundInput.state`，
//    写进去即「引擎读自己的评分再决策」。下方 `critic_report_never_reaches_engine_state_redline`
//    就是这条红线的断言（口径同 world_contributions / mileage）。

use crate::runtime::critic_persist_from_env_value;

/// 产出**非空** critic 报告的 mock：其余环节与 `MockModel` 同款合法占位 JSON。
struct CriticIssuesMock;

/// 与 mock 返回值逐字一致的期望值（断言用，避免两处漂移）。
const CRITIC_CONSISTENCY: [&str; 2] = ["李在第 2 段忽然改用现代口语，与卡上的文言底色不符", "王的怯懦设定与其强硬发言矛盾"];
const CRITIC_CAUSAL: [&str; 1] = ["门在上一句还是锁着的，这一句人已经在屋内"];
const CRITIC_SUGGESTION: [&str; 1] = ["补一句开锁动作，或把入场改为窗口"];

#[async_trait]
impl ModelClient for CriticIssuesMock {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"密室之中，烛火摇曳，两人对坐。"}"#.to_string(),
            "roleDecide" => r#"{"intent":"观望","action":"上前拱手行礼","speak":{"willSpeak":true,"purpose":"寒暄"},"targets":[],"acceptableCosts":[],"predictions":[]}"#.to_string(),
            "arbiter" => r#"{"outcomes":[]}"#.to_string(),
            "writer" => r#"{"prose":"两位大臣于烛下各怀心事。"}"#.to_string(),
            "critic" => json!({
                "characterConsistencyIssues": CRITIC_CONSISTENCY,
                "causalIssues": CRITIC_CAUSAL,
                "revisionSuggestions": CRITIC_SUGGESTION,
            })
            .to_string(),
            _ => "{}".to_string(),
        };
        Ok(ModelOutput { content, input_tokens: Some(5), output_tokens: Some(5) })
    }
}

async fn critic_row(db: &AnyPool, world_id: &str, tick_no: i64) -> (i64, i64, i64, String) {
    sqlx::query_as::<_, (i64, i64, i64, String)>(
        "SELECT consistency_issue_count, causal_issue_count, revision_suggestion_count, report_json \
         FROM world_tick_critic WHERE world_id=$1 AND tick_no=$2",
    )
    .bind(world_id)
    .bind(tick_no)
    .fetch_one(db)
    .await
    .expect("本 tick 应有 critic 行")
}

/// 落库正确 + **结构化字段可读回**：三条列表逐字还原成 `CriticReport`，计数列与列表长度一致。
#[tokio::test]
async fn critic_report_is_persisted_with_structured_fields_readable_back() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    let model: Arc<dyn ModelClient> = Arc::new(CriticIssuesMock);

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);

    let (c_cnt, causal_cnt, sug_cnt, report_json) = critic_row(&state.db, &wid, 0).await;
    // 计数列是 SLO 聚合的口径（纯 SQL，不解析 JSON）。
    assert_eq!(c_cnt, 2, "人物一致性问题计数");
    assert_eq!(causal_cnt, 1, "因果问题计数");
    assert_eq!(sug_cnt, 1, "修订建议计数");

    // 结构化读回：serde 直还原为引擎的 CriticReport，逐条文本不丢、不截断、不改序。
    let back: muse_engine::narrative::continuity::CriticReport =
        serde_json::from_str(&report_json).expect("report_json 必须能读回 CriticReport");
    assert_eq!(back.character_consistency_issues, CRITIC_CONSISTENCY.map(String::from).to_vec());
    assert_eq!(back.causal_issues, CRITIC_CAUSAL.map(String::from).to_vec());
    assert_eq!(back.revision_suggestions, CRITIC_SUGGESTION.map(String::from).to_vec());
    // 计数列与列表长度必须自洽（否则 SLO 分子会与原始数据脱节）。
    assert_eq!(back.character_consistency_issues.len() as i64, c_cnt);
    assert_eq!(back.causal_issues.len() as i64, causal_cnt);
    assert_eq!(back.revision_suggestions.len() as i64, sug_cnt);

    // 跨 tick 各落一行，(world_id, tick_no) 一一对应。
    insert_tick(&state.db, &wid, 1, 1).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 1, model).await.unwrap(), TickStatus::Done);
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_tick_critic WHERE world_id=$1", &wid).await,
        2,
        "两个已提交 tick 各落一行"
    );
    let _ = critic_row(&state.db, &wid, 1).await;
}

/// 🔴 **红线断言**：critic 文本绝不进 `worlds.narrative_state_json`（那份 JSON 每 tick 回灌进引擎），
/// 也不进任何对玩家下发的事件投影。同 `world_contributions` / `mileage` 的隔离口径。
#[tokio::test]
async fn critic_report_never_reaches_engine_state_redline() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    let model: Arc<dyn ModelClient> = Arc::new(CriticIssuesMock);
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model).await.unwrap(), TickStatus::Done);

    // ① 权威叙事状态（下一 tick 会被原样回灌进引擎 RoundInput.state）里不得出现任何 critic 内容。
    let w = load_world(&state.db, &wid).await.unwrap();
    for probe in CRITIC_CONSISTENCY.iter().chain(CRITIC_CAUSAL.iter()).chain(CRITIC_SUGGESTION.iter()) {
        assert!(
            !w.narrative_state_json.contains(probe),
            "critic 文本渗入 narrative_state_json = 引擎读自己的评分再决策（平权红线）：{probe}"
        );
    }
    for key in ["criticReport", "characterConsistencyIssues", "causalIssues", "revisionSuggestions"] {
        assert!(!w.narrative_state_json.contains(key), "critic 字段名不得出现在引擎状态：{key}");
    }

    // ② 也不得混进对外事件投影（critic 是内部观测，不是公共事实）。
    let events_dump: String = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM world_events WHERE world_id=$1 AND (COALESCE(public_projection_json,'') LIKE '%现代口语%' \
         OR COALESCE(private_projections_json,'') LIKE '%现代口语%')",
    )
    .bind(&wid)
    .fetch_one(&state.db)
    .await
    .unwrap()
    .to_string();
    assert_eq!(events_dump, "0", "critic 文本不得出现在任何事件投影");

    // ③ critic 确实落到了它该在的地方（防"两边都没有"的假绿）。
    assert_eq!(i64_one(&state.db, "SELECT COUNT(*) FROM world_tick_critic WHERE world_id=$1", &wid).await, 1);
}

/// **分母**：critic 跑过但一条问题都没有时**仍要落一行**（计数全 0）——否则「干净的 tick」与
/// 「压根没落库的历史 tick」在库里无法区分，矛盾率的分母就永远算不准。
/// 同时校验：未提交的 tick（superseded/未跑回合）不留孤儿观测行（同事务的直接后果）。
#[tokio::test]
async fn clean_critic_still_writes_a_row_and_uncommitted_ticks_write_none() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;
    // MockModel 的 critic 三条列表全空。
    let model: Arc<dyn ModelClient> = Arc::new(MockModel { input_tokens: 5, output_tokens: 5 });

    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, model.clone()).await.unwrap(), TickStatus::Done);

    let (c, causal, sug, report_json) = critic_row(&state.db, &wid, 0).await;
    assert_eq!((c, causal, sug), (0, 0, 0), "干净回合三项计数为 0，但**行必须存在**（分母）");
    let back: muse_engine::narrative::continuity::CriticReport = serde_json::from_str(&report_json).unwrap();
    assert!(back.character_consistency_issues.is_empty() && back.causal_issues.is_empty());

    // 陈旧 tick（base_revision 不匹配 → superseded，从未跑回合、从未提交）→ 不留 critic 行。
    insert_tick(&state.db, &wid, 1, 0).await.unwrap();
    assert_eq!(
        process_tick_with_model(&state, &wid, 1, model).await.unwrap(),
        TickStatus::Skipped("superseded")
    );
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_tick_critic WHERE world_id=$1", &wid).await,
        1,
        "未提交回合不得留下孤儿 critic 行（与状态 CAS 同事务的直接后果）"
    );

    // 分子/分母都能纯 SQL 算出来：矛盾率 = 有问题的 tick / critic 跑过的 tick。
    let with_issues = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_tick_critic WHERE world_id=$1 AND (consistency_issue_count > 0 OR causal_issue_count > 0)",
        &wid,
    )
    .await;
    assert_eq!(with_issues, 0, "本用例全干净 → 矛盾率分子为 0");
}

/// 开关是参数（§0.2），**默认开启**：观测数据不属 §0.1「对用户开放的功能」，
/// 默认关掉等于把补缺口这件事本身关掉。配错值一律回落开启（不静默丢数据）。
#[test]
fn critic_persist_switch_defaults_on_and_misconfig_keeps_recording() {
    assert!(critic_persist_from_env_value(None), "未配置 → 默认开启");
    for off in ["0", "false", "off", "OFF", " False "] {
        assert!(!critic_persist_from_env_value(Some(off)), "{off} 应关闭");
    }
    for on in ["1", "true", "on", "", "yes", "随便写的值"] {
        assert!(critic_persist_from_env_value(Some(on)), "{on} 应保持开启（配错不静默丢数据）");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// R3 成本工程杠杆①：错峰 / Batch 调度器（总规格 §17【拍板 16】）
// ═══════════════════════════════════════════════════════════════════════════
//
// 本节覆盖任务书列出的全部必测项：
//   · 开关关闭时调度行为逐字不变（回归保护）
//   · 折扣时段内优先领取
//   · 🔴 直播场不受错峰影响（红线）
//   · 🔴 兜底防饿死（超时无视时段照跑，红线）
//   · 时区口径正确（与 dashboards 的 UTC 日界同源）
//   · 并发领取不重复
// Batch API（杠杆③）本批次未实现（理由见 `runtime::offpeak` 模块头的可行性分析），
// 故无对应用例——**不给没有实现的东西写"通过"的测试**。

/// 错峰相关 env 的 RAII 夹具。
///
/// 🔴 **为什么需要一把全局锁**：`MUSE_OFFPEAK_*` 是**进程级** env，而 `schedule_due_ticks`
/// 是所有调度用例的公共入口——某个用例把错峰打开的瞬间，并发跑着的 `timeline_mode_event_back_to_back`
/// 等用例就会走进错峰分支。故**所有调用 `schedule_due_ticks` 的用例都必须持有这把锁**
/// （不需要错峰的用例用 `OffPeakSwitch::off()`）。范式同 `onboarding::OnboardingSwitch`。
pub(super) mod offpeak_fixture {
    use crate::runtime::offpeak;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 本夹具会动到的全部 env（Drop 时逐个恢复原值）。
    const KEYS: &[&str] = &[
        offpeak::ENV_ENABLED,
        offpeak::ENV_WINDOWS,
        offpeak::ENV_TZ_OFFSET_MIN,
        offpeak::ENV_DISCOUNT_PCT,
        offpeak::ENV_MAX_DEFER_PCT,
        offpeak::ENV_MAX_DEFER_MS,
        offpeak::ENV_MIN_INTERVAL_MS,
        offpeak::ENV_LIVE_TICK_PER_DAY,
        "MUSE_TICK_INTERVAL_MS",
    ];

    pub(crate) struct OffPeakSwitch {
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: Vec<(&'static str, Option<String>)>,
    }

    impl OffPeakSwitch {
        /// 关闭态（清空全部错峰 env）：给不测错峰、但会调 `schedule_due_ticks` 的用例用。
        pub(crate) fn off() -> Self {
            Self::with(&[])
        }

        /// 按给定 env 键值开一把夹具；未列出的错峰 env 一律清空（避免用例间串味）。
        pub(crate) fn with(extra: &[(&'static str, String)]) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = KEYS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in KEYS {
                std::env::remove_var(k);
            }
            for (k, v) in extra {
                std::env::set_var(k, v);
            }
            Self { _guard: guard, prev }
        }
    }

    impl Drop for OffPeakSwitch {
        fn drop(&mut self) {
            for (k, v) in &self.prev {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// 构造一个**包含此刻**的 UTC 折扣时段字面量（前后各留 2 小时）。
    /// 用例不能注入时钟（`schedule_due_ticks` 内部取 `now_ms()`），故反过来按真实时刻造窗口——
    /// 无论测试在一天中的哪一刻跑都成立。跨零点时自然写成 `22:30-02:30`，正好也压到跨零点分支。
    pub(crate) fn window_containing_now() -> String {
        window_around(super::now_ms(), -2 * 3_600_000, 2 * 3_600_000)
    }

    /// 构造一个**不含此刻**的 UTC 折扣时段字面量（此刻之后 6 小时起、共 4 小时）。
    pub(crate) fn window_excluding_now() -> String {
        window_around(super::now_ms(), 6 * 3_600_000, 10 * 3_600_000)
    }

    fn window_around(now: i64, from: i64, to: i64) -> String {
        let day = offpeak::DAY_MS;
        let o = offpeak::utc_ms_of_day(now);
        let fmt = |ms: i64| {
            let ms = ms.rem_euclid(day);
            format!("{:02}:{:02}", ms / 3_600_000, (ms % 3_600_000) / 60_000)
        };
        format!("{}-{}", fmt(o + from), fmt(o + to))
    }
}

use offpeak_fixture::{window_containing_now, window_excluding_now, OffPeakSwitch};

// ---------- 纯函数层：配置解析 / 时区口径 / 裁决 ----------

/// **时区口径**：`offpeak::utc_ms_of_day` 必须与 `admin_api::dashboards::utc_day_start_ms`
/// 是同一套日界（全仓唯一一套 UTC 口径）。这里用 chrono 独立复算那个函数的定义再比对——
/// 任何一侧改口径本用例立刻红，杜绝"第二套时区口径"。
#[test]
fn offpeak_utc_day_offset_matches_dashboard_day_boundary() {
    // dashboards::utc_day_start_ms 的定义（逐字复算，该函数是 pub(super) 无法跨模块引用）。
    fn dashboard_day_start(ms: i64) -> i64 {
        use chrono::NaiveTime;
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .map(|d| d.date_naive().and_time(NaiveTime::MIN).and_utc().timestamp_millis())
            .unwrap_or(0)
    }
    for t in [
        0i64,
        1,
        86_399_999,
        86_400_000,
        1_700_000_000_000,
        1_767_225_599_999,
        now_ms(),
    ] {
        assert_eq!(
            super::offpeak::utc_ms_of_day(t),
            t - dashboard_day_start(t),
            "{t} 的当日偏移必须等于 ms - dashboards 的 UTC 日界"
        );
    }
    // 纪元前（负时间戳）也不能出现负偏移，否则窗口判定会静默失灵。
    assert_eq!(super::offpeak::utc_ms_of_day(-1), 86_399_999, "负时间戳应回绕到当日末尾");
}

/// 窗口解析：跨零点拆段 · 时区折算只发生在解析期 · 重叠合并 · 非法条目只丢自己。
#[test]
fn offpeak_window_parsing_handles_wraparound_timezone_and_merge() {
    let h = 3_600_000i64;
    // 普通窗口。
    assert_eq!(super::offpeak::parse_windows("01:00-03:00", 0), vec![(h, 3 * h)]);
    // 跨零点 → 拆两段。
    assert_eq!(
        super::offpeak::parse_windows("22:00-02:00", 0),
        vec![(0, 2 * h), (22 * h, super::offpeak::DAY_MS)]
    );
    // 时区折算：北京时间 00:30-08:30（offset=480 分钟）→ UTC 16:30-00:30，即跨零点两段。
    assert_eq!(
        super::offpeak::parse_windows("00:30-08:30", 480),
        vec![(0, 30 * 60_000), (16 * h + 30 * 60_000, super::offpeak::DAY_MS)]
    );
    // 与直接按 UTC 书写等价 —— 证明「同一时段的两种写法落到同一套 UTC 口径」。
    assert_eq!(
        super::offpeak::parse_windows("00:30-08:30", 480),
        super::offpeak::parse_windows("16:30-00:30", 0)
    );
    // 重叠合并 + 相接合并。
    assert_eq!(super::offpeak::parse_windows("01:00-03:00,02:00-05:00", 0), vec![(h, 5 * h)]);
    assert_eq!(super::offpeak::parse_windows("01:00-02:00,02:00-03:00", 0), vec![(h, 3 * h)]);
    // 非法条目只丢自己：起止相同（歧义）、缺分隔符、时分越界、非数字。
    assert_eq!(
        super::offpeak::parse_windows("01:00-01:00,坏的,25:00-26:00,04:61-05:00,06:00-07:00", 0),
        vec![(6 * h, 7 * h)]
    );
    // `24:00` 作为终点合法。
    assert_eq!(super::offpeak::parse_windows("23:00-24:00", 0), vec![(23 * h, super::offpeak::DAY_MS)]);
}

/// 🔴 **配错窗口的后果必须是「功能不生效」，不是「所有世界永远被延后」**。
#[test]
fn offpeak_config_degrades_to_disabled_when_no_window_parses() {
    let _g = OffPeakSwitch::with(&[(super::offpeak::ENV_WINDOWS, "全是垃圾,01:00-01:00".into())]);
    assert!(super::offpeak::Config::from_env().is_none(), "一条窗口都解析不出来 → 整个错峰退化为关闭");
}

/// 默认配置自洽：默认窗口能解析、默认折扣是 5 折、默认阈值把连载场留在错峰范围内。
#[test]
fn offpeak_defaults_are_self_consistent() {
    let _g = OffPeakSwitch::off();
    let cfg = super::offpeak::Config::from_env().expect("默认窗口必须可解析");
    assert_eq!(cfg.window_total_ms, 8 * 3_600_000, "默认窗口（UTC 16:30-00:30）总长 8 小时");
    assert_eq!(cfg.discount_pct, super::offpeak::DEFAULT_DISCOUNT_PCT);
    // §2 三档：连载场上限 24 拍/天必须仍在错峰范围内；密集拍（≥48）才算直播场。
    assert!(!cfg.is_live_room("idle", 24), "每小时一拍的连载场不得被当成直播场");
    assert!(!cfg.is_live_room("chapter", 3), "慢炖节奏的章节房不得被当成直播场");
    assert!(cfg.is_live_room("idle", 48), "≥48 拍/天（密集拍）视为直播场");
    assert!(cfg.is_live_room("arena", 1), "赛事房恒为直播场，与节奏无关");
    // 🔴 开关本身默认关闭（§0.1）。
    assert!(!super::offpeak::env_bool(super::offpeak::ENV_ENABLED, super::offpeak::DEFAULT_ENABLED));
    assert!(!super::offpeak::DEFAULT_ENABLED, "错峰默认必须关闭");
}

/// 窗口内间隔压缩：**保住每天的拍数不变**（否则错峰就悄悄变成了节奏降档）。
#[test]
fn offpeak_compression_preserves_daily_tick_count() {
    let day = super::offpeak::DAY_MS;
    // ⚠️ `OffPeakSwitch` 内部是不可重入的 std Mutex：两段配置必须各自成块，不能同时持有两把。
    {
        let _g = OffPeakSwitch::with(&[(super::offpeak::ENV_WINDOWS, "16:00-24:00".into())]); // 8h 窗口
        let cfg = super::offpeak::Config::from_env().unwrap();
        // 24 拍/天（连载场，interval=1h）→ 窗口内间隔 20 分钟 → 8h 窗口正好还是 24 拍。
        let interval = day / 24;
        let compressed = cfg.compressed_interval(interval);
        assert_eq!(compressed, 20 * 60_000);
        assert_eq!(cfg.window_total_ms / compressed, day / interval, "窗口内拍数 = 原每日拍数");
        // 4 拍/天（慢炖场，interval=6h）→ 窗口内 2h → 8h 窗口仍是 4 拍。
        assert_eq!(cfg.compressed_interval(day / 4), 2 * 3_600_000);
    }
    // 地板保护：窗口极窄时不得压出突发风暴。
    {
        let _g = OffPeakSwitch::with(&[
            (super::offpeak::ENV_WINDOWS, "00:00-00:10".into()),
            (super::offpeak::ENV_MIN_INTERVAL_MS, "300000".into()),
        ]);
        let narrow = super::offpeak::Config::from_env().unwrap();
        assert_eq!(narrow.compressed_interval(day / 24), 300_000, "压缩结果不得低于地板");
        // 地板比原间隔还大时以原间隔为准（钳制区间不得反转）。
        assert_eq!(narrow.compressed_interval(60_000), 60_000);
    }
}

/// 🔴 **防饿死兜底的数学保证**：延后预算恒有限，且取「比例」与「绝对上限」中的较小者
/// （阈值越小触发越早 = 世界越不容易被饿着）。
#[test]
fn offpeak_defer_budget_is_finite_and_takes_the_smaller_bound() {
    let _g = OffPeakSwitch::off();
    let cfg = super::offpeak::Config::from_env().unwrap();
    let h = 3_600_000i64;
    // interval=1h：比例 2h < 绝对上限 6h → 取 2h，最长静默 3h。
    assert_eq!(cfg.defer_budget(h), 2 * h);
    assert_eq!(cfg.max_gap(h), 3 * h);
    // interval=6h：比例 12h > 绝对上限 6h → 取 6h，最长静默 12h。
    assert_eq!(cfg.defer_budget(6 * h), 6 * h);
    assert_eq!(cfg.max_gap(6 * h), 12 * h);
    // 任何 interval 下预算都不超过绝对上限 → max_gap 恒有限。
    for i in [1i64, 1000, h, 6 * h, super::offpeak::DAY_MS, i64::MAX / 4] {
        assert!(cfg.defer_budget(i) <= cfg.max_defer_ms, "预算必须被绝对上限封顶");
        assert!(cfg.max_gap(i) >= i, "兜底线不得早于一个正常间隔（那等于错峰从不生效）");
    }
}

/// 裁决核心：未到点 → Idle；窗口内 → 带折扣标记排；窗口外未超兜底 → Defer。
#[test]
fn offpeak_plan_defers_outside_window_and_discounts_inside() {
    let _g = OffPeakSwitch::with(&[(super::offpeak::ENV_WINDOWS, "00:00-08:00".into())]);
    let cfg = super::offpeak::Config::from_env().unwrap();
    let h = 3_600_000i64;
    let day0 = 1_767_225_600_000i64; // 某个 UTC 零点。
    let inside = day0 + 3 * h; // 03:00 UTC，窗口内
    let outside = day0 + 12 * h; // 12:00 UTC，窗口外
    let interval = h;

    // 窗口内：压缩后间隔 = 1h × 8/24 = 20min。刚过 10 分钟 → 还没到点。
    assert_eq!(
        super::offpeak::plan_interval(&cfg, false, inside, Some(inside - 10 * 60_000), interval, 0),
        super::offpeak::Verdict::Idle
    );
    // 窗口内且到点 → 排，并打上折扣档位。
    assert_eq!(
        super::offpeak::plan_interval(&cfg, false, inside, Some(inside - 25 * 60_000), interval, 7_000),
        super::offpeak::Verdict::Schedule(super::offpeak::TickMark {
            off_peak: true,
            price_ratio_pct: cfg.discount_pct,
            defer_ms: 7_000,
        })
    );
    // 窗口外：按原 interval 判到点（不压缩），到点但未超兜底 → 延后。
    assert_eq!(
        super::offpeak::plan_interval(&cfg, false, outside, Some(outside - 30 * 60_000), interval, 0),
        super::offpeak::Verdict::Idle,
        "窗口外必须按原 interval 判到点，不得用压缩间隔提前触发"
    );
    assert_eq!(
        super::offpeak::plan_interval(&cfg, false, outside, Some(outside - 90 * 60_000), interval, 0),
        super::offpeak::Verdict::Defer
    );
}

/// 🔴 **红线：直播场不得延后**（§2「一晚跑完一阶段 + 弹幕实时」是它的产品定义）。
/// 直播场在窗口外照排、且不打折扣标记（没享折扣就不能记账成享了）；也不受窗口内压缩影响。
#[test]
fn offpeak_never_defers_live_room() {
    let _g = OffPeakSwitch::with(&[(super::offpeak::ENV_WINDOWS, "00:00-08:00".into())]);
    let cfg = super::offpeak::Config::from_env().unwrap();
    let h = 3_600_000i64;
    let day0 = 1_767_225_600_000i64;
    let outside = day0 + 12 * h;
    let interval = 5 * 60_000; // 密集拍

    // 窗口外 + 到点 → 直播场照排，标记中性。
    assert_eq!(
        super::offpeak::plan_interval(&cfg, true, outside, Some(outside - interval), interval, 0),
        super::offpeak::Verdict::Schedule(super::offpeak::TickMark::default())
    );
    // 窗口外 + 未到点 → 仍是 Idle（豁免不等于加速）。
    assert_eq!(
        super::offpeak::plan_interval(&cfg, true, outside, Some(outside - 1), interval, 0),
        super::offpeak::Verdict::Idle
    );
    // event 模式（背靠背）同样豁免。
    assert_eq!(
        super::offpeak::plan_event(&cfg, true, outside, Some(outside - 1), 0),
        super::offpeak::Verdict::Schedule(super::offpeak::TickMark::default())
    );
    // 🔴 穷举：直播场在一天里的**任何**时刻都不可能得到 Defer。
    for step in 0..(24 * 4) {
        let t = day0 + step * 15 * 60_000;
        for last in [None, Some(t - 1), Some(t - interval), Some(t - 10 * h)] {
            assert_ne!(
                super::offpeak::plan_interval(&cfg, true, t, last, interval, 0),
                super::offpeak::Verdict::Defer,
                "直播场在 {t} 被延后了，红线破了"
            );
            assert_ne!(
                super::offpeak::plan_event(&cfg, true, t, last, 0),
                super::offpeak::Verdict::Defer,
                "直播场（event）在 {t} 被延后了，红线破了"
            );
        }
    }
}

/// 🔴 **红线：不得饿死世界**。窗口外静默超过 `interval + 延后预算` → 无视时段照跑，
/// 且标记按原价（就是原价跑的，不许虚报省钱）。首拍同样绝不延后。
#[test]
fn offpeak_starvation_guard_fires_outside_window() {
    let _g = OffPeakSwitch::with(&[(super::offpeak::ENV_WINDOWS, "00:00-08:00".into())]);
    let cfg = super::offpeak::Config::from_env().unwrap();
    let h = 3_600_000i64;
    let day0 = 1_767_225_600_000i64;
    let outside = day0 + 12 * h;
    let interval = h;
    let gap = cfg.max_gap(interval); // 1h + 2h = 3h

    // 差一毫秒 → 仍延后。
    assert_eq!(
        super::offpeak::plan_interval(&cfg, false, outside, Some(outside - gap + 1), interval, 0),
        super::offpeak::Verdict::Defer
    );
    // 刚好触线 → 照跑，原价标记 + 如实记录被压了多久。
    assert_eq!(
        super::offpeak::plan_interval(&cfg, false, outside, Some(outside - gap), interval, gap - interval),
        super::offpeak::Verdict::Schedule(super::offpeak::TickMark {
            off_peak: false,
            price_ratio_pct: 100,
            defer_ms: gap - interval,
        })
    );
    // 🔴 首拍（世界一拍都没有）绝不延后——那是玩家建完房的第一印象。
    assert!(matches!(
        super::offpeak::plan_interval(&cfg, false, outside, None, interval, 0),
        super::offpeak::Verdict::Schedule(_)
    ));
    assert!(matches!(
        super::offpeak::plan_event(&cfg, false, outside, None, 0),
        super::offpeak::Verdict::Schedule(_)
    ));
    // event 模式的兜底走绝对预算（默认 6h）。
    assert_eq!(
        super::offpeak::plan_event(&cfg, false, outside, Some(outside - cfg.max_defer_ms + 1), 0),
        super::offpeak::Verdict::Defer
    );
    assert!(matches!(
        super::offpeak::plan_event(&cfg, false, outside, Some(outside - cfg.max_defer_ms), 0),
        super::offpeak::Verdict::Schedule(_)
    ));
    // 🔴 穷举：非直播场在窗口外静默满兜底线后，一天里**任何**时刻都必须照跑。
    for step in 0..(24 * 4) {
        let t = day0 + step * 15 * 60_000;
        assert!(
            matches!(
                super::offpeak::plan_interval(&cfg, false, t, Some(t - gap), interval, 0),
                super::offpeak::Verdict::Schedule(_)
            ),
            "{t}：静默已达兜底线仍不排，世界会被饿死"
        );
    }
}

/// 延后账：首次延后开始计时，取走即清零（度量「错峰生效了多少」的唯一数据源）。
#[test]
fn offpeak_defer_tracker_measures_held_duration() {
    let t = super::offpeak::defer_tracker();
    let wid = format!("w-defer-{}", new_id("t"));
    assert_eq!(t.held_ms(&wid, 1_000), 0, "从未延后 → 0");
    t.mark(&wid, 1_000);
    t.mark(&wid, 5_000); // 二次延后不刷新起点。
    assert_eq!(t.held_ms(&wid, 9_000), 8_000);
    assert_eq!(t.take(&wid, 9_000), 8_000);
    assert_eq!(t.held_ms(&wid, 9_000), 0, "取走后清零");
    assert_eq!(t.take(&wid, 9_000), 0, "重复取走恒为 0");
    t.clear(&wid);
}

// ---------- 集成层：走真 DB 的 schedule_due_ticks ----------

/// 建一个 interval 模式、running、指定房型与节奏的世界（错峰集成用例专用）。
async fn offpeak_world(state: &AppState, tag: &str, room_type: &str, tick_per_day: i64) -> String {
    let tpl = format!("tpl-{tag}");
    let routes_v = format!("routes-{tag}");
    let (ua, ub) = (format!("u{tag}A"), format!("u{tag}B"));
    let (ca, cb) = (format!("c{tag}A"), format!("c{tag}B"));
    seed_template(&state.db, &tpl).await;
    seed_model_routes(&state.db, &routes_v).await;
    seed_user(&state.db, &ua).await;
    seed_user(&state.db, &ub).await;
    seed_char(&state.db, &ca, &ua, "李").await;
    seed_char(&state.db, &cb, &ub, "王").await;

    let mut p = CreateWorldParams::official(tpl.clone(), 1, "错峰测试世界");
    p.status = Some("running".into());
    p.room_type = room_type.into();
    p.tick_per_day = tick_per_day;
    p.model_route_version = Some(routes_v.clone());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    let wid = create_world(&state.db, p).await.unwrap();
    seed_member(&state.db, &wid, &ua, &ca).await;
    seed_member(&state.db, &wid, &ub, &cb).await;
    wid
}

/// 直接落一条 done 状态的历史 tick，用于把「上一拍在多久以前」钉死。
async fn seed_done_tick_at(db: &AnyPool, world_id: &str, tick_no: i64, created_at: i64) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
         VALUES ($1, $2, $3, 0, 'done', 0, $4)",
    )
    .bind(new_id("tick"))
    .bind(world_id)
    .bind(tick_no)
    .bind(created_at)
    .execute(db)
    .await
    .unwrap();
}

async fn max_tick_no(db: &AnyPool, world_id: &str) -> i64 {
    i64_one(db, "SELECT COALESCE(MAX(tick_no), -1) FROM world_ticks WHERE world_id=$1", world_id).await
}

/// 🔴 **回归保护：开关关闭时调度行为逐字不变**。
/// 关闭态下同时验证 interval 房与 event×idle 房两条分支，并确认新增三列全为中性值。
#[tokio::test]
async fn offpeak_disabled_keeps_scheduling_byte_identical() {
    // 窗口设成「不含此刻」——若错峰真被误开，这些世界就会被延后，用例立刻红。
    let _g = OffPeakSwitch::with(&[(super::offpeak::ENV_WINDOWS, window_excluding_now())]);
    let state = test_state().await;
    let iv = offpeak_world(&state, "offd", "idle", 24).await;
    let now = now_ms();
    seed_done_tick_at(&state.db, &iv, 0, now - 2 * 3_600_000).await;

    super::schedule_due_ticks(&state).await.unwrap();
    assert_eq!(max_tick_no(&state.db, &iv).await, 1, "开关关闭 → 到点即排，与接线前一致");

    // 新增三列必须是中性值（既有 cost.* 聚合看到的世界完全没变）。
    let row = sqlx::query("SELECT off_peak, price_ratio_pct, defer_ms FROM world_ticks WHERE world_id=$1 AND tick_no=1")
        .bind(&iv)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(row.try_get::<i64, _>("off_peak").unwrap(), 0);
    assert_eq!(row.try_get::<i64, _>("price_ratio_pct").unwrap(), 100);
    assert_eq!(row.try_get::<i64, _>("defer_ms").unwrap(), 0);

    // 未到点的世界仍然不排（退化路径也不变）。
    let iv2 = offpeak_world(&state, "offd2", "idle", 24).await;
    seed_done_tick_at(&state.db, &iv2, 0, now).await;
    super::schedule_due_ticks(&state).await.unwrap();
    assert_eq!(max_tick_no(&state.db, &iv2).await, 0, "未到点不应排新 tick");
}

/// 集成主链路：窗口外延后 → 窗口开启后排出，并把折扣档位与被压时长写进逐拍台账。
#[tokio::test]
async fn offpeak_defers_outside_window_then_schedules_inside() {
    let state = test_state().await;
    let wid;
    {
        // 阶段一：窗口不含此刻 → 到点也不排。
        let _g = OffPeakSwitch::with(&[
            (super::offpeak::ENV_ENABLED, "1".into()),
            (super::offpeak::ENV_WINDOWS, window_excluding_now()),
        ]);
        wid = offpeak_world(&state, "offa", "idle", 24).await;
        seed_done_tick_at(&state.db, &wid, 0, now_ms() - 2 * 3_600_000).await;
        super::schedule_due_ticks(&state).await.unwrap();
        assert_eq!(max_tick_no(&state.db, &wid).await, 0, "原价时段应延后，不排新 tick");
        assert!(
            super::offpeak::defer_tracker().held_ms(&wid, now_ms()) >= 0,
            "延后账应已开始计时"
        );
    }
    {
        // 阶段二：窗口含此刻 → 立刻排出，并带折扣标记。
        let _g = OffPeakSwitch::with(&[
            (super::offpeak::ENV_ENABLED, "1".into()),
            (super::offpeak::ENV_WINDOWS, window_containing_now()),
            (super::offpeak::ENV_DISCOUNT_PCT, "40".into()),
        ]);
        super::schedule_due_ticks(&state).await.unwrap();
        assert_eq!(max_tick_no(&state.db, &wid).await, 1, "折扣时段应把被压的拍排出来");
        let row = sqlx::query(
            "SELECT off_peak, price_ratio_pct FROM world_ticks WHERE world_id=$1 AND tick_no=1",
        )
        .bind(&wid)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(row.try_get::<i64, _>("off_peak").unwrap(), 1, "应标记为折扣时段拍");
        assert_eq!(row.try_get::<i64, _>("price_ratio_pct").unwrap(), 40, "应记下命中的名义档位");
    }
    super::offpeak::defer_tracker().clear(&wid);
}

/// 🔴 集成红线：**直播场不受错峰影响**。同一轮调度里，连载场被延后而两类直播场照排。
#[tokio::test]
async fn offpeak_live_rooms_are_scheduled_even_outside_window() {
    let _g = OffPeakSwitch::with(&[
        (super::offpeak::ENV_ENABLED, "1".into()),
        (super::offpeak::ENV_WINDOWS, window_excluding_now()),
    ]);
    let state = test_state().await;
    let now = now_ms();
    // 连载场（24 拍/天）：应被延后。
    let serial = offpeak_world(&state, "offs", "idle", 24).await;
    // 直播场之一：赛事房（房型判据）。
    let arena = offpeak_world(&state, "offv", "arena", 24).await;
    // 直播场之二：密集拍（节奏判据，96 拍/天 = 每 15 分钟一拍）。
    let dense = offpeak_world(&state, "offn", "chapter", 96).await;
    for w in [&serial, &arena, &dense] {
        seed_done_tick_at(&state.db, w, 0, now - 2 * 3_600_000).await;
    }

    super::schedule_due_ticks(&state).await.unwrap();

    assert_eq!(max_tick_no(&state.db, &serial).await, 0, "连载场在原价时段应被延后");
    assert_eq!(max_tick_no(&state.db, &arena).await, 1, "🔴 赛事直播场不得被延后");
    assert_eq!(max_tick_no(&state.db, &dense).await, 1, "🔴 密集拍直播场不得被延后");
    // 直播场的拍不得被记成「享了折扣」。
    for w in [&arena, &dense] {
        let op = i64_one(&state.db, "SELECT off_peak FROM world_ticks WHERE world_id=$1 AND tick_no=1", w).await;
        assert_eq!(op, 0, "直播场没享折扣，不许记成享了");
    }
    super::offpeak::defer_tracker().clear(&serial);
}

/// 🔴 集成红线：**兜底防饿死**。窗口外静默超过兜底线的世界照排，同轮里刚到点的世界仍被延后。
#[tokio::test]
async fn offpeak_starvation_guard_schedules_outside_window() {
    let _g = OffPeakSwitch::with(&[
        (super::offpeak::ENV_ENABLED, "1".into()),
        (super::offpeak::ENV_WINDOWS, window_excluding_now()),
    ]);
    let state = test_state().await;
    let now = now_ms();
    // interval=1h（24 拍/天），兜底线 = 1h + min(2h, 6h) = 3h。
    let starved = offpeak_world(&state, "offh", "idle", 24).await;
    let fresh = offpeak_world(&state, "offf", "idle", 24).await;
    seed_done_tick_at(&state.db, &starved, 0, now - 4 * 3_600_000).await; // 超兜底线
    seed_done_tick_at(&state.db, &fresh, 0, now - 90 * 60_000).await; // 到点但未超

    super::schedule_due_ticks(&state).await.unwrap();

    assert_eq!(max_tick_no(&state.db, &starved).await, 1, "🔴 静默超兜底线必须无视时段照跑");
    assert_eq!(max_tick_no(&state.db, &fresh).await, 0, "未超兜底线仍应延后");
    // 兜底跑出来的拍是原价跑的，不许记成折扣。
    let op = i64_one(
        &state.db,
        "SELECT off_peak FROM world_ticks WHERE world_id=$1 AND tick_no=1",
        &starved,
    )
    .await;
    assert_eq!(op, 0, "兜底拍按原价记账");
    // 🔴 从没跑过拍的新世界（首拍）也必须立即排。
    let brand_new = offpeak_world(&state, "offb", "idle", 24).await;
    super::schedule_due_ticks(&state).await.unwrap();
    assert_eq!(max_tick_no(&state.db, &brand_new).await, 0, "首拍应被排出（tick_no 从 0 起）");
    super::offpeak::defer_tracker().clear(&fresh);
}

/// **折扣时段优先领取**：被压得最久的世界先入队（先入队 = 先被 worker 领走）。
/// 无人被延后时排序是稳定的空操作，这一点由 `offpeak_disabled_keeps_scheduling_byte_identical` 覆盖。
#[tokio::test]
async fn offpeak_priority_orders_longest_deferred_first() {
    let state = test_state().await;
    let (a, b, c);
    {
        // 阶段一：三个世界都在原价时段被延后，人为拉开它们的被压时长。
        let _g = OffPeakSwitch::with(&[
            (super::offpeak::ENV_ENABLED, "1".into()),
            (super::offpeak::ENV_WINDOWS, window_excluding_now()),
        ]);
        let now = now_ms();
        a = offpeak_world(&state, "offp1", "idle", 24).await;
        b = offpeak_world(&state, "offp2", "idle", 24).await;
        c = offpeak_world(&state, "offp3", "idle", 24).await;
        for w in [&a, &b, &c] {
            seed_done_tick_at(&state.db, w, 0, now - 2 * 3_600_000).await;
        }
        super::schedule_due_ticks(&state).await.unwrap();
        for w in [&a, &b, &c] {
            assert_eq!(max_tick_no(&state.db, w).await, 0, "三个世界都应先被延后");
        }
        // 人为把 b 的延后起点推早（= 被压得最久），c 次之，a 最短。
        super::offpeak::defer_tracker().clear(&b);
        super::offpeak::defer_tracker().mark(&b, now - 60 * 60_000);
        super::offpeak::defer_tracker().clear(&c);
        super::offpeak::defer_tracker().mark(&c, now - 30 * 60_000);
    }
    {
        // 阶段二：窗口开启 → 三个都排出，但入队顺序按被压时长降序（b → c → a）。
        let _g = OffPeakSwitch::with(&[
            (super::offpeak::ENV_ENABLED, "1".into()),
            (super::offpeak::ENV_WINDOWS, window_containing_now()),
        ]);
        super::schedule_due_ticks(&state).await.unwrap();
        // 队列空时 `pop` 会永久挂起（见 queue::MemQueue），故套超时兜底：少于 3 条即断言失败而非卡死。
        let mut order: Vec<String> = Vec::new();
        for _ in 0..3 {
            let popped = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                crate::queue::pop_json::<super::TickJob>(&*state.queue, "world_tick"),
            )
            .await
            .expect("折扣时段应把三个被压的世界全部入队");
            order.push(popped.expect("队列载荷应可反序列化").world_id);
        }
        assert_eq!(order, vec![b.clone(), c.clone(), a.clone()], "应按被压时长降序入队");
        // 被压时长如实落进逐拍台账（>0 才说明错峰真的生效过）。
        let dm = i64_one(&state.db, "SELECT defer_ms FROM world_ticks WHERE world_id=$1 AND tick_no=1", &b).await;
        assert!(dm >= 60 * 60_000, "b 的 defer_ms 应≥1 小时，实际 {dm}");
    }
    for w in [&a, &b, &c] {
        super::offpeak::defer_tracker().clear(w);
    }
}

/// **并发领取不重复**：同一世界被多轮/多并发调度只可能排出一拍
/// （`world_ticks(world_id, tick_no)` 唯一索引 + 带标记插入走同一条幂等路径）。
#[tokio::test]
async fn offpeak_concurrent_scheduling_never_duplicates_ticks() {
    let _g = OffPeakSwitch::with(&[
        (super::offpeak::ENV_ENABLED, "1".into()),
        (super::offpeak::ENV_WINDOWS, window_containing_now()),
    ]);
    let state = test_state().await;
    let wid = offpeak_world(&state, "offc", "idle", 24).await;
    seed_done_tick_at(&state.db, &wid, 0, now_ms() - 6 * 3_600_000).await;

    // 同一轮里连排三次：第一次插入成功，后两次因「上一拍就是刚插的」不再到点。
    for _ in 0..3 {
        super::schedule_due_ticks(&state).await.unwrap();
    }
    assert_eq!(
        i64_one(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id=$1", &wid).await,
        2,
        "重复轮询不得排出重复拍"
    );
    // 带标记插入本身幂等（并发下唯一索引兜底）。
    assert!(!super::insert_tick_marked(&state.db, &wid, 1, 0, super::TickMark::default()).await.unwrap());
    assert!(
        !super::insert_tick_marked(
            &state.db,
            &wid,
            1,
            0,
            super::TickMark { off_peak: true, price_ratio_pct: 50, defer_ms: 9 }
        )
        .await
        .unwrap(),
        "已存在的 tick_no 不得被带标记插入覆盖"
    );
    super::offpeak::defer_tracker().clear(&wid);
}

/// event×idle（背靠背慢炖场）同样受错峰约束：原价时段停排，折扣时段恢复背靠背。
#[tokio::test]
async fn offpeak_applies_to_back_to_back_idle_rooms() {
    let state = test_state().await;
    let wid;
    {
        let _g = OffPeakSwitch::with(&[
            (super::offpeak::ENV_ENABLED, "1".into()),
            (super::offpeak::ENV_WINDOWS, window_excluding_now()),
        ]);
        wid = offpeak_world(&state, "offe", "idle", 3).await;
        sqlx::query("UPDATE worlds SET timeline_mode='event' WHERE id=$1")
            .bind(&wid)
            .execute(&state.db)
            .await
            .unwrap();
        // 上一拍就在刚才 → 背靠背本应立刻续拍，错峰把它压住。
        seed_done_tick_at(&state.db, &wid, 0, now_ms()).await;
        super::schedule_due_ticks(&state).await.unwrap();
        assert_eq!(max_tick_no(&state.db, &wid).await, 0, "原价时段的背靠背世界应停排");
    }
    {
        let _g = OffPeakSwitch::with(&[
            (super::offpeak::ENV_ENABLED, "1".into()),
            (super::offpeak::ENV_WINDOWS, window_containing_now()),
        ]);
        super::schedule_due_ticks(&state).await.unwrap();
        assert_eq!(max_tick_no(&state.db, &wid).await, 1, "折扣时段应恢复背靠背");
        let op = i64_one(&state.db, "SELECT off_peak FROM world_ticks WHERE world_id=$1 AND tick_no=1", &wid).await;
        assert_eq!(op, 1);
    }
    super::offpeak::defer_tracker().clear(&wid);
}

/// 错峰路径不叠拍：上一拍还在飞（pending/running）时不排新拍——窗口内间隔被压缩后，
/// 排出一拍 `base_revision` 已过期的 tick 只会白占拍位（C-2 cas_conflict 终态化）。
#[tokio::test]
async fn offpeak_does_not_stack_ticks_on_unfinished_round() {
    let _g = OffPeakSwitch::with(&[
        (super::offpeak::ENV_ENABLED, "1".into()),
        (super::offpeak::ENV_WINDOWS, window_containing_now()),
    ]);
    let state = test_state().await;
    let wid = offpeak_world(&state, "offk", "idle", 24).await;
    // 一拍 pending 且已滞留很久（早该到点了）。
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, created_at) \
         VALUES ($1, $2, 0, 0, 'pending', $3)",
    )
    .bind(new_id("tick"))
    .bind(&wid)
    .bind(now_ms() - 6 * 3_600_000)
    .execute(&state.db)
    .await
    .unwrap();

    super::schedule_due_ticks(&state).await.unwrap();
    assert_eq!(max_tick_no(&state.db, &wid).await, 0, "在飞的拍未完成前不得叠新拍");

    // 该拍收尾后，下一轮立刻排出（不叠拍 ≠ 卡死）。
    sqlx::query("UPDATE world_ticks SET status='done' WHERE world_id=$1 AND tick_no=0")
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    super::schedule_due_ticks(&state).await.unwrap();
    assert_eq!(max_tick_no(&state.db, &wid).await, 1, "在飞拍收尾后应立即续排");
    super::offpeak::defer_tracker().clear(&wid);
}

// ============================================================================
// 境界档叙事接线（总规格 §6【拍板 3】「戏服原则——境界即布景」）
// ============================================================================
//
// 链路全长：模板 `skeleton_json.realmTier` → 装配钉住 `assembled_json./assembly/realmTier`
// → `runtime::parse_realm_costume` → `RoundInput.realm_costume` → 引擎 `call_director`。
// 下面三条用例分别钉死这条链的**通**、**只通到导演为止**、以及**未声明时逐字节不通**。

/// 记录每次模型调用的 `(agent, user)`，返回与 `MockModel` 同款的合法 JSON。
/// 用它才能断言「戏服进了哪个环节的 prompt、没进哪个」——`MockModel` 不留痕，问不出这件事。
#[derive(Default)]
struct RecordingMockModel {
    calls: std::sync::Mutex<Vec<(String, String)>>,
}

impl RecordingMockModel {
    fn user_of(&self, agent: &str) -> String {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .find(|(a, _)| a == agent)
            .map(|(_, u)| u.clone())
            .unwrap_or_else(|| panic!("未捕获到 {agent} 环节的调用"))
    }

    /// 除 `agent` 外的全部调用（用于断言戏服没有外溢到别的环节）。
    fn users_except(&self, agent: &str) -> Vec<(String, String)> {
        self.calls.lock().unwrap().iter().filter(|(a, _)| a != agent).cloned().collect()
    }
}

#[async_trait]
impl ModelClient for RecordingMockModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        self.calls.lock().unwrap().push((spec.agent.clone(), spec.user.clone()));
        let content = match spec.agent.as_str() {
            "director" => r#"{"situation":"密室之中，烛火摇曳，两人对坐。"}"#,
            "roleDecide" => r#"{"intent":"观望","action":"上前拱手行礼","speak":{"willSpeak":true,"purpose":"寒暄"},"targets":[],"acceptableCosts":[],"predictions":[]}"#,
            "arbiter" => r#"{"outcomes":[]}"#,
            "writer" => r#"{"prose":"两位大臣于烛下各怀心事，礼数周全。"}"#,
            "critic" => r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#,
            _ => "{}",
        };
        Ok(ModelOutput { content: content.to_string(), input_tokens: Some(10), output_tokens: Some(20) })
    }
}

/// 声明了境界档的模板（§6：阶段模板天然携带戏服）。其余形状与 `seed_template` 一致。
async fn seed_dressed_template(db: &AnyPool, id: &str) {
    let skeleton = json!({
        "mainlineNodes": [{ "id": "n1", "summary": "两位大臣在密室摊牌", "fated": true }],
        "realmTier": {
            "id": "tier-douwang",
            "label": "斗王档",
            "cosmology": "cultivation",
            "genre": "xuanhuan",
            // 🔴 刻意填 lethal：它是**题材标注**，绝不是生死开关（世界是否致命由建房参数
            //    lethality 独立决定）。下面的用例据此断言它不进模型上下文。
            "conflictIntensity": "lethal",
            "briefing": "本篇全员领斗王档戏服：能御空短距、能扛一记斗皇余威，仅此而已。",
            "flavorNotes": ["魂技译为斗气招式风味，内核不变"]
        }
    });
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, created_at) \
         VALUES ($1, '戏服模板', 'idle', $2, '{\"mode\":\"open\"}', 1, 1, 'approved', $3)",
    )
    .bind(id)
    .bind(skeleton.to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 建一个 running、模板声明了境界档、带 2 名成员的 idle 世界（首 tick 自动装配 → 钉住戏服）。
async fn dressed_world_with_two_members(state: &AppState) -> String {
    seed_dressed_template(&state.db, "tpl-realm").await;
    seed_model_routes(&state.db, "realm-routes").await;
    seed_user(&state.db, "uR1").await;
    seed_user(&state.db, "uR2").await;
    seed_char(&state.db, "chR1", "uR1", "李").await;
    seed_char(&state.db, "chR2", "uR2", "王").await;

    let mut p = CreateWorldParams::official("tpl-realm", 1, "戏服测试世界");
    p.status = Some("running".into());
    p.model_route_version = Some("realm-routes".into());
    p.prompt_set_version = Some("test-prompts".into());
    p.member_limit = 10;
    p.daily_token_budget = 1_000_000;
    p.daily_cny_budget_cents = 0;
    let wid = create_world(&state.db, p).await.unwrap();
    seed_member(&state.db, &wid, "uR1", "chR1").await;
    seed_member(&state.db, &wid, "uR2", "chR2").await;
    wid
}

/// 🔴 端到端：模板声明的戏服真的穿到了这一拍上——**且只穿在入场导演身上**。
///
/// 正面：`briefing` 与 `flavorNotes` 逐字进导演 prompt（§6「入场导演统一设定」「风味翻译」）。
/// 反面（红线）：
///   ① 决策 / 仲裁 / 写作 / 审校四个环节一个字都看不到它——漏进决策就成了「你现在是斗王」的能力暗示；
///   ② `conflictIntensity: lethal` **不进任何 prompt**：它是题材标注，不是生死开关（§11 的
///      `lethality` 才是），让一个叙事标注去撬动生死观感属平权红线违规；
///   ③ 落库的世界状态与公共事实（world_events）里不得出现戏服的任何一个字——布景不留痕，
///      更不会变成谁的持有事实 / 判定输入。
#[tokio::test]
async fn realm_tier_reaches_only_the_director_prompt() {
    let state = test_state().await;
    let wid = dressed_world_with_two_members(&state).await;

    let model = Arc::new(RecordingMockModel::default());
    let mc: Arc<dyn ModelClient> = model.clone();
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, mc).await.unwrap(), TickStatus::Done);

    // 前提自检：首 tick 兜底装配把戏服钉进了 assembled_json（否则本用例是空跑）。
    let assembled = load_world(&state.db, &wid).await.unwrap().assembled_json.unwrap();
    assert!(assembled.contains("\"realmTier\""), "首 tick 应把模板声明的境界档钉住：{assembled}");

    // 正面：戏服进入场导演。
    let director = model.user_of("director");
    assert!(director.contains("本篇戏服（全员统一，同一水位）"), "导演应看到戏服段：{director}");
    assert!(director.contains("能扛一记斗皇余威"), "briefing 必须逐字进导演 prompt：{director}");
    assert!(
        director.contains("跨体系风味翻译：魂技译为斗气招式风味，内核不变"),
        "flavorNotes 必须进导演 prompt：{director}"
    );
    assert!(director.contains("不得据此判定谁能赢"), "必须带「只改描写、不改胜负」的免责话术");

    // 红线①：戏服不外溢到其它环节。
    for (agent, user) in model.users_except("director") {
        assert!(!user.contains("斗王"), "戏服泄漏到了 {agent} 环节：{user}");
        assert!(!user.contains("斗气招式"), "风味翻译泄漏到了 {agent} 环节：{user}");
    }
    // 红线②：题材标注三件套（含 lethal）一个都不进模型上下文——只有 briefing/flavorNotes 进。
    for (agent, user) in model.calls.lock().unwrap().iter() {
        assert!(!user.contains("lethal"), "conflictIntensity 不得进 {agent} 的上下文：{user}");
        assert!(!user.contains("cultivation"), "cosmology 不得进 {agent} 的上下文：{user}");
        assert!(!user.contains("tier-douwang"), "档位 id 是审计键，不得进 {agent} 的上下文：{user}");
    }

    // 红线③：布景不留痕——世界状态与公共事实里都没有它。
    let w = load_world(&state.db, &wid).await.unwrap();
    assert!(!w.narrative_state_json.contains("斗王"), "红线：戏服渗进了 narrative_state_json");
    let leaked = i64_one(
        &state.db,
        "SELECT COUNT(*) FROM world_events WHERE world_id=$1 AND \
         (COALESCE(public_projection_json,'') LIKE '%斗王%' OR COALESCE(private_projections_json,'') LIKE '%斗王%')",
        &wid,
    )
    .await;
    assert_eq!(leaked, 0, "红线：戏服不得出现在公共事实 / 私有投影里");
}

/// 未声明境界档的世界：导演 prompt 里**一个字节都不多**（黄金骨架正是这一类，
/// 它保证了 golden 基线不会因本次接线漂移）。
#[tokio::test]
async fn world_without_realm_tier_keeps_director_prompt_clean() {
    let state = test_state().await;
    let wid = running_world_with_two_members(&state).await;

    let model = Arc::new(RecordingMockModel::default());
    let mc: Arc<dyn ModelClient> = model.clone();
    insert_tick(&state.db, &wid, 0, 0).await.unwrap();
    assert_eq!(process_tick_with_model(&state, &wid, 0, mc).await.unwrap(), TickStatus::Done);

    let director = model.user_of("director");
    assert!(!director.contains("本篇戏服"), "模板没声明戏服 → 导演 prompt 不得出现戏服段：{director}");
    assert!(!director.contains("风味翻译"), "同上：{director}");
}

/// `parse_realm_costume` 的退化契约：读不到 / 读不懂 / 读到一件没词儿的戏服 → 一律 `None`
/// （= 与接线前逐字节一致），且任何输入都不 panic。
#[test]
fn parse_realm_costume_degrades_to_none_on_anything_unusable() {
    use super::parse_realm_costume as parse;

    assert!(parse(None).is_none(), "未装配 → None");
    assert!(parse(Some("不是 JSON")).is_none(), "坏 JSON → None，不 panic");
    assert!(parse(Some(r#"{"assembly":{}}"#)).is_none(), "无 realmTier 键（绝大多数存量实例）→ None");
    assert!(parse(Some(r#"{"assembly":{"realmTier":[]}}"#)).is_none(), "数组 → None（境界不是池）");
    assert!(parse(Some(r#"{"assembly":{"realmTier":"斗王档"}}"#)).is_none(), "字符串 → None");
    // 声明了档位却没写任何文案：导演无话可说 → 等价于未声明，绝不生成一个空标题。
    assert!(
        parse(Some(r#"{"assembly":{"realmTier":{"id":"t","label":"斗王档","conflictIntensity":"lethal"}}}"#)).is_none(),
        "只有 id/label/枚举、没有 briefing/flavorNotes → None"
    );
    assert!(
        parse(Some(r#"{"assembly":{"realmTier":{"id":"t","briefing":"   ","flavorNotes":["","  "]}}}"#)).is_none(),
        "全空白文案 → None"
    );
}

/// 解析出来的戏服**只含两段文案**：其余五个字段（id / label / cosmology / genre /
/// conflictIntensity）一律留在服务端，不进模型上下文。这是「哪些字段该被模型看到」的单一守卫。
#[test]
fn parse_realm_costume_keeps_only_briefing_and_flavor_notes() {
    let c = super::parse_realm_costume(Some(
        r#"{"assembly":{"realmTier":{"id":"tier-douwang","label":"斗王档","cosmology":"cultivation",
            "genre":"xuanhuan","conflictIntensity":"lethal","briefing":"  全员同一水位，仅此而已  ",
            "flavorNotes":["魂技译为斗气招式风味"," ","内核不变"]}}}"#,
    ))
    .expect("声明了文案就应解出戏服");

    assert_eq!(c.briefing, "全员同一水位，仅此而已", "两端空白应裁掉（prompt 拼接口径稳定）");
    assert_eq!(c.flavor_notes, vec!["魂技译为斗气招式风味".to_string(), "内核不变".to_string()], "空条目丢弃");

    // 🔴 平权红线：进模型的这份结构里一个数字都没有，也没有任何档位 / 体系 / 烈度标注。
    let v = serde_json::to_value(&c).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(obj.len(), 2, "戏服进模型的字段只有 briefing / flavorNotes：{v}");
    let dumped = v.to_string();
    for leaked in ["tier-douwang", "斗王档", "cultivation", "xuanhuan", "lethal"] {
        assert!(!dumped.contains(leaked), "「{leaked}」不该进模型上下文：{dumped}");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 观众礼物 → 引擎展示层（open-decisions §5 选项 A，2026-07-28 拍板）
// ═══════════════════════════════════════════════════════════════════════════

async fn mk_env_table(db: &AnyPool) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS arena_env_events (\
         id TEXT PRIMARY KEY, world_id TEXT NOT NULL, applied_tick INTEGER, kind TEXT NOT NULL, \
         payload_json TEXT NOT NULL DEFAULT '{}', aggregated_count INTEGER NOT NULL DEFAULT 1, \
         created_at BIGINT NOT NULL)",
    )
    .execute(db)
    .await
    .ok();
}

async fn seed_gift(db: &AnyPool, id: &str, world: &str, payload: &str, count: i64, at: i64) {
    sqlx::query(
        "INSERT INTO arena_env_events (id, world_id, applied_tick, kind, payload_json, aggregated_count, created_at) \
         VALUES ($1, $2, NULL, 'gift_boon', $3, $4, $5)",
    )
    .bind(id)
    .bind(world)
    .bind(payload)
    .bind(count)
    .bind(at)
    .execute(db)
    .await
    .unwrap();
}

/// 🔴 **开关没开 → 恒空**。这一项一旦被玩家感知为「打赏有用」，撤回等于承认平台卖过优势，
/// 所以它必须是**有人按过开关**才生效，而不是一次代码合并的副作用（§0.1 在这里赌注最大）。
#[tokio::test]
async fn audience_gifts_do_not_reach_the_engine_until_someone_turns_the_flag_on() {
    let state = test_state().await;
    mk_env_table(&state.db).await;
    seed_gift(&state.db, "g1", "w1", r#"{"label":"有人送上一束火把"}"#, 3, 100).await;

    let (events, ids) = super::load_pending_ambient(&state, "w1").await;
    assert!(events.is_empty(), "🔴 开关未开时礼物绝不许进引擎上下文：{events:?}");
    assert!(ids.is_empty(), "开关未开时也不该有任何行被标记");
}

/// 开关打开后：只取 `label`、**绝不取 `boon`**、按 `created_at` 定序、封顶。
#[tokio::test]
async fn once_enabled_only_the_display_label_reaches_the_engine() {
    let state = test_state().await;
    mk_env_table(&state.db).await;
    crate::flags::set_flag(
        &state.db,
        crate::flags::SetFlag {
            flag: "MUSE_AMBIENT_GIFT_EVENTS",
            scope: "global",
            target_id: "",
            enabled: true,
            starts_at: 0,
            ends_at: 0,
            actor_id: "test",
            reason: "用例",
        },
    )
    .await
    .unwrap();

    // 后送的排在后面；带 boon 效果语义的那条，boon 不许出现在任何地方。
    seed_gift(&state.db, "g2", "w1", r#"{"label":"第二个","boon":{"kind":"advantage","effectTag":"reroll"}}"#, 1, 200).await;
    seed_gift(&state.db, "g1", "w1", r#"{"label":"第一个"}"#, 5, 100).await;
    // 空 label / 别的世界 → 都不该进来。
    seed_gift(&state.db, "g3", "w1", r#"{"label":"   "}"#, 1, 300).await;
    seed_gift(&state.db, "g4", "w2", r#"{"label":"别人的世界"}"#, 1, 50).await;

    let (events, ids) = super::load_pending_ambient(&state, "w1").await;
    let labels: Vec<&str> = events.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, vec!["第一个", "第二个"], "按 created_at 定序，空白与他人世界剔除");
    assert_eq!(events[0].count, 5, "count 是聚合计数（不是强度）");
    assert_eq!(ids, vec!["g1".to_string(), "g2".to_string()], "回写用的 id 与事件一一对应");

    // 🔴 boon 的效果语义一个字都不许流到引擎：喂给模型就等于在暗示「这个礼物该起什么作用」，
    // 而本字段的全部约定是「它不起任何作用」。
    let dumped = format!("{events:?}");
    for leaked in ["advantage", "reroll", "boon", "effectTag"] {
        assert!(
            !dumped.contains(leaked),
            "🔴 礼物的效果语义 `{leaked}` 流进了引擎入参：{dumped}"
        );
    }
}
