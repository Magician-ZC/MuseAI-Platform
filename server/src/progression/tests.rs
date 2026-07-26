//! 历练值 + 卡位制测试（sqlite::memory + oneshot）。覆盖：
//! - grant_mileage_tx 唯一写入路径：只增不减、非正数拒绝、事务回滚不发（结算失败不发历练）；
//! - 卡位解锁阈值边界（499 拒 / 500 过）与 6 上限；总历练只计未撤回卡；
//! - 发布卡位检查：卡位满 409、解锁后可发；mine 回读 mileage；
//! - **红线（grep 级）**：RoundInput 组装处（runtime/mod.rs）与引擎侧不出现任何 mileage 引用——
//!   历练只作准入与解锁，绝不进入引擎决策（叙事平权是产品根基）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::db::now_ms;
use crate::safety::testkit::{seed_user, test_state, token};

// ---------- 脚手架 ----------

/// 播种一枚云端角色（可指定初始历练与撤回态）。
async fn seed_char_with_mileage(
    state: &AppState,
    id: &str,
    owner: &str,
    mileage: i64,
    withdrawn: i64,
) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at, mileage) \
         VALUES ($1, $2, 'local', 1, '{}', 'original', 'approved', $3, $4, $5)",
    )
    .bind(id)
    .bind(owner)
    .bind(withdrawn)
    .bind(now_ms())
    .bind(mileage)
    .execute(&state.db)
    .await
    .expect("seed cloud_character");
}

async fn mileage_of(state: &AppState, char_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT mileage FROM cloud_characters WHERE id = $1")
        .bind(char_id)
        .fetch_one(&state.db)
        .await
        .expect("mileage query")
}

async fn send(state: &AppState, method: &str, uri: &str, user: &str, body: Option<Value>) -> (StatusCode, Value) {
    let tk = token(state, user);
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
    (status, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

fn publish_body(local_card_id: &str) -> Value {
    json!({
        "localCardId": local_card_id,
        "cardJson": { "schemaVersion": 2, "identity": { "name": "测试角色" } },
        "rightsDeclaration": "original",
    })
}

// ---------- grant_mileage_tx：唯一写入路径 ----------

/// 历练累加：同一卡多次发放逐笔累加；非正数（0/负）一律拒绝（历练只增不减）。
#[tokio::test]
async fn grant_mileage_accumulates_and_rejects_non_positive() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_char_with_mileage(&state, "c1", "u1", 0, 0).await;

    let mut tx = state.db.begin().await.unwrap();
    assert_eq!(grant_mileage_tx(&mut tx, "c1", 100, "test").await.unwrap(), 1);
    assert_eq!(grant_mileage_tx(&mut tx, "c1", 50, "test").await.unwrap(), 1);
    assert!(grant_mileage_tx(&mut tx, "c1", 0, "test").await.is_err(), "0 数额应拒绝");
    assert!(grant_mileage_tx(&mut tx, "c1", -10, "test").await.is_err(), "负数额应拒绝");
    // 目标角色不存在 → 0 行跳过不报错（结算容忍 NPC/已删卡）。
    assert_eq!(grant_mileage_tx(&mut tx, "ghost", 10, "test").await.unwrap(), 0);
    tx.commit().await.unwrap();

    assert_eq!(mileage_of(&state, "c1").await, 150, "两笔发放累加为 150");
}

/// 结算失败不发历练：与结算同事务——事务回滚则历练同滚（这是「同一事务」性质的直接证明，
/// 各结算点（章节/终局/arena）都在既有结算事务内调用 grant_mileage_tx）。
#[tokio::test]
async fn grant_mileage_rolls_back_with_failed_settlement() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_char_with_mileage(&state, "c1", "u1", 0, 0).await;

    let mut tx = state.db.begin().await.unwrap();
    assert_eq!(grant_mileage_tx(&mut tx, "c1", 100, "test").await.unwrap(), 1);
    tx.rollback().await.unwrap(); // 模拟结算失败（CAS 不命中 / 发货报错）→ 整笔回滚

    assert_eq!(mileage_of(&state, "c1").await, 0, "结算回滚后历练不得残留");
}

// ---------- 卡位解锁：阈值边界 + 上限 ----------

/// 阈值边界：总历练 499 → 拒（文案含还差多少）；500 → 过（card_slots 3→4）。
#[tokio::test]
async fn unlock_threshold_boundary_499_rejected_500_passes() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_char_with_mileage(&state, "c1", "u1", 499, 0).await;

    let (st, v) = send(&state, "POST", "/api/me/card-slots/unlock", "u1", Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "499 < 500 应拒绝: {v}");
    let msg = v["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("还差 1"), "文案应含还差多少，实际: {msg}");

    // 补到恰好 500 → 解锁成功，第 4 位到手，下一阈值 1500。
    sqlx::query("UPDATE cloud_characters SET mileage = 500 WHERE id = 'c1'")
        .execute(&state.db)
        .await
        .unwrap();
    let (st, v) = send(&state, "POST", "/api/me/card-slots/unlock", "u1", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["cardSlots"], json!(4));
    assert_eq!(v["nextSlotAt"], json!(1500));

    // 立即再解锁：500 < 1500 → 拒。
    let (st, _) = send(&state, "POST", "/api/me/card-slots/unlock", "u1", Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "未达下一阈值不得连续解锁");
}

/// 6 上限：历练充裕也只能解到 6；再解 → 400（文案含上限）。nextSlotAt 到顶为 null。
#[tokio::test]
async fn unlock_stops_at_max_six_slots() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_char_with_mileage(&state, "c1", "u1", 100_000, 0).await;

    for expect in [4i64, 5, 6] {
        let (st, v) = send(&state, "POST", "/api/me/card-slots/unlock", "u1", Some(json!({}))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["cardSlots"], json!(expect));
    }
    let (st, v) = send(&state, "POST", "/api/me/card-slots/unlock", "u1", Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "已到 6 上限应拒绝: {v}");
    assert!(v["error"]["message"].as_str().unwrap_or_default().contains("上限"));

    let (st, v) = send(&state, "GET", "/api/me/progression", "u1", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["cardSlots"], json!(6));
    assert_eq!(v["maxSlots"], json!(6));
    assert!(v["nextSlotAt"].is_null(), "到顶后无下一阈值");
}

/// 总历练 = owner 全部**未撤回**卡的 mileage 之和：撤回卡不计入；他人卡不计入。
#[tokio::test]
async fn progression_totals_exclude_withdrawn_and_other_owners() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_char_with_mileage(&state, "c1", "u1", 300, 0).await;
    seed_char_with_mileage(&state, "c2", "u1", 400, 1).await; // 已撤回：不计入
    seed_char_with_mileage(&state, "c3", "u2", 900, 0).await; // 他人卡：不计入

    let (st, v) = send(&state, "GET", "/api/me/progression", "u1", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["totalMileage"], json!(300), "撤回卡与他人卡都不计入总历练");
    assert_eq!(v["cardSlots"], json!(3));
    assert_eq!(v["nextSlotAt"], json!(500));

    // 300 < 500：解锁被拒（还差 200）。
    let (st, v) = send(&state, "POST", "/api/me/card-slots/unlock", "u1", Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"].as_str().unwrap_or_default().contains("还差 200"));
}

// ---------- 发布卡位检查 + mine 回读 mileage ----------

/// 卡位满 → 发布 409（文案含 N/N 与解锁指引）；解锁后可发；mine 每项带 mileage。
#[tokio::test]
async fn publish_blocked_when_slots_full_and_allowed_after_unlock() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    // 3 张未撤回卡占满默认 3 卡位；总历练恰 500（够解锁第 4 位）。
    seed_char_with_mileage(&state, "c1", "u1", 200, 0).await;
    seed_char_with_mileage(&state, "c2", "u1", 200, 0).await;
    seed_char_with_mileage(&state, "c3", "u1", 100, 0).await;

    let (st, v) = send(&state, "POST", "/api/assets/characters", "u1", Some(publish_body("card-x"))).await;
    assert_eq!(st, StatusCode::CONFLICT, "卡位已满应 409: {v}");
    let msg = v["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("卡位已满（3/3）"), "文案应含 N/N，实际: {msg}");
    assert!(msg.contains("通过历练可解锁更多卡位"), "文案应含解锁指引，实际: {msg}");

    // 解锁第 4 位（总历练 500 达阈值）→ 发布放行。
    let (st, v) = send(&state, "POST", "/api/me/card-slots/unlock", "u1", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["cardSlots"], json!(4));

    let (st, v) = send(&state, "POST", "/api/assets/characters", "u1", Some(publish_body("card-x"))).await;
    assert_eq!(st, StatusCode::OK, "解锁后应可发布: {v}");
    assert_eq!(v["mileage"], json!(0), "新卡历练从 0 起");

    // mine：4 项，每项都带 mileage；老卡历练如实回读。
    let (st, mine) = send(&state, "GET", "/api/assets/characters/mine", "u1", None).await;
    assert_eq!(st, StatusCode::OK);
    let items = mine.as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert!(items.iter().all(|i| i["mileage"].is_i64()), "mine 每项必须带 mileage: {mine}");
    let c1 = items.iter().find(|i| i["id"] == "c1").expect("c1 在列");
    assert_eq!(c1["mileage"], json!(200), "老卡历练如实回读");

    // 第 4 位也满后（4/4）再发 → 409（撤回可释放，此处不再展开）。
    let (st, _) = send(&state, "POST", "/api/assets/characters", "u1", Some(publish_body("card-y"))).await;
    assert_eq!(st, StatusCode::CONFLICT, "4/4 占满后继续发布仍应 409");
}

/// 撤回释放卡位：3/3 满 → 撤回一张 → 2/3 可再发布。
#[tokio::test]
async fn withdraw_frees_slot_for_publish() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_char_with_mileage(&state, "c1", "u1", 0, 0).await;
    seed_char_with_mileage(&state, "c2", "u1", 0, 0).await;
    seed_char_with_mileage(&state, "c3", "u1", 0, 0).await;

    let (st, _) = send(&state, "POST", "/api/assets/characters", "u1", Some(publish_body("card-x"))).await;
    assert_eq!(st, StatusCode::CONFLICT);

    let (st, _) = send(&state, "POST", "/api/assets/characters/c3/withdraw", "u1", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, v) = send(&state, "POST", "/api/assets/characters", "u1", Some(publish_body("card-x"))).await;
    assert_eq!(st, StatusCode::OK, "撤回释放卡位后应可发布: {v}");
}

// ---------- 红线（grep 级）：历练绝不进入引擎决策 ----------

/// 叙事平权红线：RoundInput 组装处（server/src/runtime/mod.rs）与引擎叙事层
/// （crates/muse-engine/src/narrative/mod.rs，RoundInput/role_decide/仲裁所在）在源码级
/// **不出现任何 mileage 引用**——历练只在结算点发放、只被准入与解锁消费。
/// runtime 的 idle 终局发放刻意收在 progression::settle_idle_world_ending_tx，正是为守住本断言。
#[test]
fn red_line_engine_decision_paths_never_reference_mileage() {
    let runtime_src = include_str!("../runtime/mod.rs");
    assert!(
        !runtime_src.contains("mileage"),
        "runtime/mod.rs（RoundInput 组装处）不得引用 mileage：历练绝不进入引擎决策"
    );
    let engine_narrative_src = include_str!("../../../crates/muse-engine/src/narrative/mod.rs");
    assert!(
        !engine_narrative_src.contains("mileage"),
        "muse-engine narrative（RoundInput/role_decide/仲裁）不得引用 mileage：叙事平权是产品根基"
    );
}

// ==================== R1 三层结算 ③ 世界线层：贡献归因 + 确定性产出表 ====================

use muse_engine::narrative::types::{PatchOperation, SpeakIntent};

fn mk_decision(cid: &str, will_speak: bool) -> RoleDecision {
    RoleDecision {
        decision_id: format!("dec:{cid}"),
        character_id: cid.into(),
        intent: "意图".into(),
        action: "行动".into(),
        speak: SpeakIntent { will_speak, purpose: String::new() },
        targets: vec![],
        acceptable_costs: vec![],
        predictions: vec![],
        duration: 0,
    }
}

fn mk_outcome(cid: &str, result: ArbiterResult) -> ArbiterOutcome {
    ArbiterOutcome {
        decision_id: format!("dec:{cid}"),
        character_id: cid.into(),
        result,
        rule_refs: vec![],
        consequence: String::new(),
    }
}

/// 引擎 `narrative::round_intensity` 的**标量口径镜像**（该函数在引擎内私有，此处按同一公式复算）：
/// Σ outcomes 结果权重（Invalid/Blocked 计 0）+ Σ willSpeak 决策互动权重。
fn engine_round_intensity(
    decisions: &[RoleDecision],
    outcomes: &[ArbiterOutcome],
    w: &IntensityWeights,
) -> f64 {
    let mut e = 0.0;
    for o in outcomes {
        e += match o.result {
            ArbiterResult::Success => w.success,
            ArbiterResult::PartialSuccess => w.partial,
            ArbiterResult::Failure => w.failure,
            ArbiterResult::Invalid | ArbiterResult::Blocked => 0.0,
        };
    }
    for d in decisions {
        if d.speak.will_speak {
            e += w.speak;
        }
    }
    e
}

/// 归因口径：逐角色折算正确，且**分项之和恒等于引擎标量**（同一套 IntensityWeights，不改引擎即对齐）。
#[test]
fn contribution_breakdown_matches_engine_scalar_and_splits_per_character() {
    let w = IntensityWeights::default(); // 1.0 / 0.5 / 0.25 / 0.25
    let decisions = vec![
        mk_decision("cA", true),  // +0.25 互动
        mk_decision("cB", false), // 不发言
        mk_decision("cC", true),  // +0.25 互动
        mk_decision("cD", true),  // +0.25 互动（结果 Invalid，仅互动分）
    ];
    let outcomes = vec![
        mk_outcome("cA", ArbiterResult::Success),        // +1.0
        mk_outcome("cB", ArbiterResult::PartialSuccess), // +0.5
        mk_outcome("cC", ArbiterResult::Failure),        // +0.25（失败亦是推进事件）
        mk_outcome("cD", ArbiterResult::Invalid),        // +0（无效不计）
    ];

    let per = contribution_breakdown(&decisions, &outcomes, &w);
    assert!((per["cA"] - 1.25).abs() < 1e-9, "成功 + 发言: {:?}", per);
    assert!((per["cB"] - 0.5).abs() < 1e-9, "部分成功、未发言: {:?}", per);
    assert!((per["cC"] - 0.5).abs() < 1e-9, "失败 0.25 + 发言 0.25: {:?}", per);
    assert!((per["cD"] - 0.25).abs() < 1e-9, "Invalid 不计结果分，仅互动分: {:?}", per);

    let sum: f64 = per.values().sum();
    let scalar = engine_round_intensity(&decisions, &outcomes, &w);
    assert!(
        (sum - scalar).abs() < 1e-9,
        "分项之和必须等于引擎 round_intensity 标量：sum={sum} scalar={scalar}"
    );
}

/// Blocked 结果与零贡献回合：不产生任何贡献分（既不误记、也不报错）。
#[test]
fn contribution_breakdown_ignores_blocked_and_empty_rounds() {
    let w = IntensityWeights::default();
    let decisions = vec![mk_decision("cA", false)];
    let outcomes = vec![mk_outcome("cA", ArbiterResult::Blocked)];
    assert!(contribution_breakdown(&decisions, &outcomes, &w).is_empty(), "Blocked 不计贡献");
    assert!(contribution_breakdown(&[], &[], &w).is_empty(), "空回合不产生条目");
}

/// 参数化生效①：贡献分折算权重可由产出表覆盖（VALIDATION §0.2 产品规则参数化，禁止写死）。
#[test]
fn contribution_weights_are_parameterized() {
    let decisions = vec![mk_decision("cA", true)];
    let outcomes = vec![mk_outcome("cA", ArbiterResult::Failure)];

    let default_score = contribution_breakdown(&decisions, &outcomes, &IntensityWeights::default())["cA"];
    assert!((default_score - 0.5).abs() < 1e-9, "默认权重: 0.25 + 0.25");

    // 运营把"失败"的推进价值调高、把"发言"调零 → 同一批引擎数据得到不同贡献分。
    let tuned = IntensityWeights { success: 2.0, partial: 1.0, failure: 1.0, speak: 0.0 };
    let tuned_score = contribution_breakdown(&decisions, &outcomes, &tuned)["cA"];
    assert!((tuned_score - 1.0).abs() < 1e-9, "调参后应为 1.0（失败 1.0 + 发言 0）: {tuned_score}");
    assert_ne!(
        default_score.to_bits(),
        tuned_score.to_bits(),
        "权重是参数不是常量：改配置必须改变折算结果"
    );
}

/// 「里程碑推动者」判据：只认引擎 patch 里的 `world.milestoneProgress_*` Increment（确定性数据）。
#[test]
fn fed_milestone_reads_engine_patch_only() {
    let op = |op: PatchOp, path: &str| PatchOperation {
        op,
        path: path.into(),
        value: None,
        precondition: None,
    };
    let patch = |ops: Vec<PatchOperation>| StatePatch {
        id: "p".into(),
        base_revision: 1,
        source_decision_ids: vec![],
        operations: ops,
    };

    assert!(fed_milestone(&patch(vec![op(PatchOp::Increment, "world.milestoneProgress_m1")])));
    assert!(
        !fed_milestone(&patch(vec![op(PatchOp::Append, "narrative.pacingNotes")])),
        "只有节拍记录 → 未推进里程碑"
    );
    assert!(
        !fed_milestone(&patch(vec![op(PatchOp::Set, "world.milestoneProgress_m1")])),
        "非 Increment 不算推进（引擎只用 Increment 累积进度）"
    );
    assert!(!fed_milestone(&patch(vec![])), "空 patch 不算推进");
}

/// 确定性产出：同一贡献分 + 同一张表 ⇒ 恒同一档（无 RNG）；档位按"门槛 ≤ 贡献分中最高的一档"命中。
#[test]
fn payout_tier_lookup_is_deterministic_and_rng_free() {
    let table = PayoutTable {
        worldline_tiers: vec![
            PayoutTier { label: "见证".into(), min_score: 1.0, item: None, mileage: 20 },
            PayoutTier { label: "推动".into(), min_score: 3.0, item: None, mileage: 80 },
            PayoutTier { label: "执笔".into(), min_score: 8.0, item: None, mileage: 200 },
        ],
        ..Default::default()
    };

    assert!(resolve_payout_tier(&table, 0.9).is_none(), "低于最低门槛 → 不发放");
    assert_eq!(resolve_payout_tier(&table, 1.0).unwrap().label, "见证", "门槛含等于");
    assert_eq!(resolve_payout_tier(&table, 2.99).unwrap().label, "见证");
    assert_eq!(resolve_payout_tier(&table, 3.0).unwrap().label, "推动");
    assert_eq!(resolve_payout_tier(&table, 7.999).unwrap().label, "推动");
    assert_eq!(resolve_payout_tier(&table, 1_000.0).unwrap().label, "执笔", "超高分仍落最高档，不外溢");

    // 确定性：同一输入重复查 200 次，结果逐次一致（查表发放，不存在爆率/抽卡）。
    let first = resolve_payout_tier(&table, 4.25).unwrap().label.clone();
    for _ in 0..200 {
        assert_eq!(resolve_payout_tier(&table, 4.25).unwrap().label, first);
    }
}

/// 参数化生效②：改产出表 → 同一贡献分得到不同产出（表是数据，不是代码常量）。
#[test]
fn payout_table_is_parameterized() {
    let lenient = PayoutTable {
        worldline_tiers: vec![PayoutTier {
            label: "宽松".into(),
            min_score: 1.0,
            item: None,
            mileage: 10,
        }],
        ..Default::default()
    };
    let strict = PayoutTable {
        worldline_tiers: vec![PayoutTier {
            label: "严格".into(),
            min_score: 50.0,
            item: None,
            mileage: 999,
        }],
        ..Default::default()
    };
    assert_eq!(resolve_payout_tier(&lenient, 2.0).unwrap().mileage, 10);
    assert!(resolve_payout_tier(&strict, 2.0).is_none(), "同一贡献分在更严格的表下不发放");
}

/// 崩塌系数是具名常量且可被模板覆盖；`is_collapse_reason` 只认关键角色退场类终局。
#[test]
fn collapse_policy_constants_and_reasons() {
    assert_eq!(COLLAPSE_BASELINE_FACTOR, 0.5, "① 保底层崩塌减半");
    assert_eq!(COLLAPSE_WORLDLINE_FACTOR, 0.0, "③ 世界线层崩塌归零");

    // 默认值来自 progression 常量（单一事实源，不散落魔数）。
    let default_ctx =
        PayoutContext { table: None, star_rating: 1, subplot_cards: Default::default() };
    assert_eq!(default_ctx.baseline_factor(true), COLLAPSE_BASELINE_FACTOR);
    assert_eq!(default_ctx.worldline_factor(true), COLLAPSE_WORLDLINE_FACTOR);
    assert_eq!(default_ctx.baseline_factor(false), 1.0, "正常收束不打折");
    assert_eq!(default_ctx.worldline_factor(false), 1.0);

    // 参数化：模板可覆盖两个系数（VALIDATION §0.2）。
    let tuned = PayoutContext {
        table: Some(PayoutTable {
            collapse: crate::assembly::CollapsePolicy { baseline_factor: 0.25, worldline_factor: 0.5 },
            ..Default::default()
        }),
        star_rating: 3,
        subplot_cards: Default::default(),
    };
    assert_eq!(tuned.baseline_factor(true), 0.25);
    assert_eq!(tuned.worldline_factor(true), 0.5);

    assert!(is_collapse_reason("key_character_exit"), "关键角色退场 = 世界线崩塌");
    for ok in ["mainline_complete", "time_cap", "time_limit", "starved"] {
        assert!(!is_collapse_reason(ok), "{ok} 是正常收束，不是崩塌");
    }
}

/// 结算上下文的保守退化：无 assembled_json / 无 payoutTable / 坏 JSON → 不发放 + 1★ 最保守封顶。
#[test]
fn payout_context_degrades_conservatively() {
    for raw in [None, Some("not json"), Some("{}"), Some(r#"{"assembly":{}}"#)] {
        let ctx = payout_context_from_wrapper(raw);
        assert!(ctx.table.is_none(), "无产出表 → ③ 层默认关闭: {raw:?}");
        assert_eq!(ctx.star_rating, 1, "星级缺失按最保守 1★ 封顶: {raw:?}");
    }
    let ctx = payout_context_from_wrapper(Some(
        r#"{"starRating":4,"assembly":{"payoutTable":{"worldlineTiers":[{"label":"甲","minScore":2,"mileage":30}]}}}"#,
    ));
    assert_eq!(ctx.star_rating, 4);
    assert_eq!(ctx.table.as_ref().unwrap().worldline_tiers.len(), 1);
    // 未声明的权重字段回落引擎默认值（口径一致）。
    assert_eq!(ctx.weights().success, IntensityWeights::default().success);
}

/// R2 产出接点：同一份 `assembled_json` 同时解析出**历练/道具视图**与**副本卡视图**——
/// 一次读取、同一个 `worldlineTiers` 数组、同一档。未声明 `subplotCard` 的表 → 副本卡视图为空
/// （数据侧默认关闭）；解析失败/无表 → 两个视图一起退化为"什么都不发"。
/// （铸卡行为本身的用例在 `subplot::tests`，此处只钉住结算上下文的接点契约。）
#[test]
fn payout_context_carries_subplot_card_view_of_the_same_table() {
    // 无 assembled_json / 坏 JSON / 无产出表 → 副本卡视图同样为空。
    for raw in [None, Some("not json"), Some("{}"), Some(r#"{"assembly":{}}"#)] {
        let ctx = payout_context_from_wrapper(raw);
        assert!(ctx.subplot_cards.resolve(1_000.0).is_none(), "无表时副本卡视图必须为空: {raw:?}");
    }

    // 声明了 subplotCard 的档 → 两个视图命中同一档；未声明的档 → 只有历练/道具。
    let raw = json!({
        "starRating": 4,
        "assembly": { "payoutTable": { "worldlineTiers": [
            { "label": "见证", "minScore": 1.0, "mileage": 20 },
            { "label": "推动", "minScore": 3.0, "mileage": 80,
              "subplotCard": { "starRating": 2, "label": "星陨之夜" } }
        ]}}
    })
    .to_string();
    let ctx = payout_context_from_wrapper(Some(&raw));
    let table = ctx.table.as_ref().expect("产出表");
    assert_eq!(resolve_payout_tier(table, 2.0).unwrap().label, "见证");
    assert!(ctx.subplot_cards.resolve(2.0).is_none(), "该档未声明副本卡 → 不发卡");
    assert_eq!(resolve_payout_tier(table, 5.0).unwrap().label, "推动");
    assert!(ctx.subplot_cards.resolve(5.0).is_some(), "同一档在副本卡视图里也必须命中");
}

/// 产出表的**确定性完整性**由建模板期校验前置守住（`assembly::validate_skeleton_refs` 第 5 段）：
/// 门槛重复 = 同一贡献分对应多档 = 产出非确定，必须在入库前拒绝；权重/系数越界同理。
/// 断言放在本模块是因为它守的是 ③ 层"同分同产出"契约（结算侧的前置条件），非装配逻辑本身。
#[test]
fn payout_table_integrity_is_enforced_at_template_build_time() {
    let ok = json!({
        "payoutTable": { "worldlineTiers": [
            { "label": "甲", "minScore": 1.0, "mileage": 10 },
            { "label": "乙", "minScore": 3.0, "mileage": 30 }
        ]}
    });
    assert!(crate::assembly::validate_skeleton_refs(&ok).is_ok());

    // 门槛重复 → 拒绝。
    let dup = json!({
        "payoutTable": { "worldlineTiers": [
            { "label": "甲", "minScore": 2.0, "mileage": 10 },
            { "label": "乙", "minScore": 2.0, "mileage": 30 }
        ]}
    });
    let err = crate::assembly::validate_skeleton_refs(&dup).unwrap_err();
    assert!(err.contains("门槛重复"), "{err}");

    // 负门槛 / 负权重 / 越界崩塌系数 / 空道具 id → 拒绝。
    for bad in [
        json!({ "payoutTable": { "worldlineTiers": [{ "label": "甲", "minScore": -1.0 }] } }),
        json!({ "payoutTable": { "contributionWeights": { "success": -1.0 } } }),
        json!({ "payoutTable": { "collapse": { "baselineFactor": 2.0 } } }),
        json!({ "payoutTable": { "worldlineTiers": [{ "label": "甲", "minScore": 1.0,
            "item": { "id": "  ", "narrative": "x", "effectTags": [],
                      "origin": { "worldTemplateId": "t", "cosmology": [], "powerTier": 1 } } }] } }),
    ] {
        assert!(crate::assembly::validate_skeleton_refs(&bad).is_err(), "应被拒绝: {bad}");
    }

    // 老模板（无 payoutTable）一律放行，零影响。
    assert!(crate::assembly::validate_skeleton_refs(&json!({ "mainlineNodes": [] })).is_ok());
}

// ==================== BE 结局传记（总规格 §9「世界线崩塌」） ====================
//
// 覆盖：开关关闭不产出 · 崩塌终局产出 · **正常终局不产出** ·
// **内容全部来自确定性数据（无模型现编的责任归属，红线）** · 幂等 ·
// **不改写任何世界线数据（红线）** · 摘要长度参数化。

/// 造一张 4★ 模板（星级投影的来源）。
async fn seed_be_template(state: &AppState, id: &str) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, \
         version, moderation, star_rating, created_at) \
         VALUES ($1, '黑角域篇', 'idle', '{}', '{}', 1, 3, 'approved', 4, $2)",
    )
    .bind(id)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed template");
}

/// 造一枚带名字的云端角色（足迹里的「角色面具名」来源）。
async fn seed_named_char(state: &AppState, id: &str, owner: &str, name: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at, mileage) \
         VALUES ($1, $2, 'local', 1, $3, 'original', 'approved', 0, $4, 0)",
    )
    .bind(id)
    .bind(owner)
    .bind(json!({ "identity": { "name": name } }).to_string())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed named char");
}

/// 造一个「跑过一段、然后终局」的世界：世界行 + 两位成员 + 贡献账本 + 三拍 + 三条事件。
/// 全是既有确定性数据，传记只对它们做只读汇总。**不含** `world.ended` 审计痕（由各用例自行写入）。
async fn seed_played_world(state: &AppState) -> String {
    seed_be_template(state, "tpl_be").await;
    let wid = crate::worlds::create_world(
        &state.db,
        crate::worlds::CreateWorldParams::official("tpl_be", 3, "黑角域篇"),
    )
    .await
    .expect("create world");

    seed_user(&state.db, "u_be1").await;
    seed_user(&state.db, "u_be2").await;
    seed_named_char(state, "c_be1", "u_be1", "唐三").await;
    seed_named_char(state, "c_be2", "u_be2", "小舞").await;
    for (mid, uid, cid, status, joined, left) in [
        ("wm_be1", "u_be1", "c_be1", "active", 1_000i64, None::<i64>),
        ("wm_be2", "u_be2", "c_be2", "left", 2_000i64, Some(9_000i64)),
    ] {
        sqlx::query(
            "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, \
             status, joined_at, left_at) VALUES ($1, $2, $3, $4, '{}', $5, $6, $7)",
        )
        .bind(mid)
        .bind(&wid)
        .bind(uid)
        .bind(cid)
        .bind(status)
        .bind(joined)
        .bind(left)
        .execute(&state.db)
        .await
        .expect("seed member");
    }
    for (cid, score, milestone) in [("c_be1", 4_500i64, 3_000i64), ("c_be2", 1_500i64, 0i64)] {
        sqlx::query(
            "INSERT INTO world_contributions (world_id, character_id, score_milli, \
             milestone_score_milli, settled_at, updated_at) VALUES ($1, $2, $3, $4, 0, $5)",
        )
        .bind(&wid)
        .bind(cid)
        .bind(score)
        .bind(milestone)
        .bind(now_ms())
        .execute(&state.db)
        .await
        .expect("seed contribution");
    }
    for (tid, no, status) in [("tk1", 1i64, "done"), ("tk2", 2i64, "done"), ("tk3", 3i64, "failed")] {
        sqlx::query(
            "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
             VALUES ($1, $2, $3, 0, $4, 10, $5)",
        )
        .bind(tid)
        .bind(&wid)
        .bind(no)
        .bind(status)
        .bind(now_ms())
        .execute(&state.db)
        .await
        .expect("seed tick");
    }
    for (eid, seq, kind) in [("ev1", 1i64, "dialogue"), ("ev2", 2i64, "dialogue"), ("ev3", 3i64, "action")] {
        sqlx::query(
            "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
             actors_json, visibility, occurred_at) VALUES ($1, $2, 1, $3, $4, $5, '[]', 'public', $6)",
        )
        .bind(eid)
        .bind(&wid)
        .bind(seq)
        .bind(eid)
        .bind(kind)
        .bind(now_ms())
        .execute(&state.db)
        .await
        .expect("seed event");
    }
    wid
}

/// 写一条 runtime 口径的终局审计痕（格式 `{reason}|ending={ending}`）。
async fn seed_world_ended_audit(state: &AppState, world_id: &str, reason: &str, ending: &str) {
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES ($1, 'system', 'system', 'world.ended', $2, $3, $4)",
    )
    .bind(crate::db::new_id("aud"))
    .bind(world_id)
    .bind(format!("{reason}|ending={ending}"))
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed world.ended audit");
}

/// 走一次封卷（独立事务，模拟结算事务内的调用）。
async fn seal(state: &AppState, world_id: &str, collapsed: bool) {
    let ctx = payout_context_from_wrapper(None);
    let mut tx = state.db.begin().await.unwrap();
    seal_be_biography_tx(&mut tx, world_id, collapsed, &ctx).await.expect("seal");
    tx.commit().await.unwrap();
}

async fn biography_rows(state: &AppState, world_id: &str) -> Vec<(String, String, String, String, i64)> {
    sqlx::query_as("SELECT kind, terminal_reason, ending_id, summary_json, sealed_at FROM world_biographies WHERE world_id = $1")
        .bind(world_id)
        .fetch_all(&state.db)
        .await
        .expect("read biography")
}

async fn biography_summary(state: &AppState, world_id: &str) -> Value {
    let rows = biography_rows(state, world_id).await;
    assert_eq!(rows.len(), 1, "应恰有一份传记");
    serde_json::from_str(&rows[0].3).expect("summary 应为合法 JSON")
}

/// 世界线数据的全量快照（红线断言用）：五张世界线表 + 角色卡的资产列。
async fn worldline_snapshot(state: &AppState, world_id: &str) -> String {
    let world: Vec<(i64, String, String, Option<String>, String, i64)> = sqlx::query_as(
        "SELECT state_revision, status, narrative_state_json, assembled_json, title, updated_at \
         FROM worlds WHERE id = $1",
    )
    .bind(world_id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    let events: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, event_type, visibility, moderation, occurred_at FROM world_events \
         WHERE world_id = $1 ORDER BY id ASC",
    )
    .bind(world_id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    let ticks: Vec<(String, i64, String, i64)> = sqlx::query_as(
        "SELECT id, tick_no, status, cost_tokens FROM world_ticks WHERE world_id = $1 ORDER BY id ASC",
    )
    .bind(world_id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    let contributions: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT character_id, score_milli, milestone_score_milli, settled_at FROM world_contributions \
         WHERE world_id = $1 ORDER BY character_id ASC",
    )
    .bind(world_id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    let members: Vec<(String, String, String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT id, cloud_character_id, status, joined_at, left_at FROM world_members \
         WHERE world_id = $1 ORDER BY id ASC",
    )
    .bind(world_id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    let chars: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT id, mileage, withdrawn FROM cloud_characters ORDER BY id ASC")
            .fetch_all(&state.db)
            .await
            .unwrap();
    format!("{world:?}|{events:?}|{ticks:?}|{contributions:?}|{members:?}|{chars:?}")
}

/// 🔴 开关关闭 → 崩塌也不产出传记（VALIDATION.md §0.1 未验证功能默认关闭）。
#[tokio::test]
async fn be_biography_not_produced_when_switch_off() {
    let _sw = BiographySwitch::set(false);
    let state = test_state().await;
    let wid = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &wid, "key_character_exit", "none").await;

    seal(&state, &wid, true).await;
    assert!(biography_rows(&state, &wid).await.is_empty(), "关阀期间不得产出传记");
}

/// 崩塌终局 → 产出传记，且内容是对既有数据的**只读汇总**（世界线摘要 + 崩塌原因 + 参与者足迹）。
#[tokio::test]
async fn be_biography_sealed_on_collapse() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;
    let wid = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &wid, "key_character_exit", "ending_dark").await;

    seal(&state, &wid, true).await;
    let rows = biography_rows(&state, &wid).await;
    assert_eq!(rows.len(), 1, "崩塌应封卷出一份传记");
    assert_eq!(rows[0].0, "be");
    assert_eq!(rows[0].1, "key_character_exit");
    assert_eq!(rows[0].2, "ending_dark");

    let s = biography_summary(&state, &wid).await;
    // 世界元信息
    assert_eq!(s["world"]["id"], json!(wid));
    assert_eq!(s["world"]["templateId"], json!("tpl_be"));
    assert_eq!(s["world"]["starRating"], json!(4), "星级取自模板（确定性）");
    // 世界线摘要（计量口径）
    assert_eq!(s["worldline"]["totalTicks"], json!(3));
    assert_eq!(s["worldline"]["lastTickNo"], json!(3));
    assert_eq!(s["worldline"]["totalEvents"], json!(3));
    assert_eq!(s["worldline"]["eventTypeTotal"], json!(2));
    assert_eq!(s["worldline"]["eventsByType"][0]["eventType"], json!("dialogue"), "按次数降序");
    assert_eq!(s["worldline"]["eventsByType"][0]["count"], json!(2));
    assert_eq!(s["worldline"]["contributionMilliTotal"], json!(6_000));
    assert_eq!(s["worldline"]["milestoneMilliTotal"], json!(3_000));
    // 参与者足迹（含离场者——足迹是履历的一部分）
    assert_eq!(s["footprints"]["total"], json!(2));
    assert_eq!(s["footprints"]["truncated"], json!(false));
    assert_eq!(s["footprints"]["items"][0]["characterId"], json!("c_be1"));
    assert_eq!(s["footprints"]["items"][0]["name"], json!("唐三"));
    assert_eq!(s["footprints"]["items"][0]["milestoneMilli"], json!(3_000));
    assert_eq!(s["footprints"]["items"][1]["status"], json!("left"));
    assert_eq!(s["footprints"]["items"][1]["leftAt"], json!(9_000));
    // 🔴 不下发真人身份（§14 恨隔面具原则）
    let raw = serde_json::to_string(&s).unwrap();
    assert!(!raw.contains("u_be1") && !raw.contains("u_be2"), "传记不得含真人 user_id: {raw}");
    // 三层结算的崩塌系数（§9：①减半 ③归零 ②已锁定保留）
    assert_eq!(s["settlement"]["baselineFactor"], json!(COLLAPSE_BASELINE_FACTOR));
    assert_eq!(s["settlement"]["worldlineFactor"], json!(COLLAPSE_WORLDLINE_FACTOR));
    // 封卷本身留痕（§0.2 全链审计）
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'world.be_biography_sealed' AND subject = $1",
    )
    .bind(&wid)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

/// 🔴 **正常终局不产出**：主线走完 / 时间上限 / 无可调度角色都不是 BE——只有崩塌才有 BE 传记。
#[tokio::test]
async fn be_biography_not_produced_on_normal_ending() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;
    for reason in ["mainline_complete", "time_cap", "starved", "time_limit"] {
        let wid = crate::worlds::create_world(
            &state.db,
            crate::worlds::CreateWorldParams::official("tpl_x", 1, "正常收束"),
        )
        .await
        .unwrap();
        seed_world_ended_audit(&state, &wid, reason, "ending_ok").await;
        // 调用方按 is_collapse_reason 判定，正常收束传 false。
        assert!(!is_collapse_reason(reason), "{reason} 不是崩塌");
        seal(&state, &wid, false).await;
        assert!(biography_rows(&state, &wid).await.is_empty(), "{reason} 不得产出 BE 传记");
    }
}

/// 🔴 无冤案：崩塌原因必须有**确定性出处**。
/// (a) 没有 `world.ended` 审计痕 → 不产出（不许靠推断补一个死因）；
/// (b) 审计痕说的是正常收束（与结算侧口径不一致）→ 不产出。
#[tokio::test]
async fn be_biography_requires_deterministic_collapse_evidence() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;

    // (a) 无审计痕
    let w1 = seed_played_world(&state).await;
    seal(&state, &w1, true).await;
    assert!(biography_rows(&state, &w1).await.is_empty(), "无 world.ended 痕不得封卷");

    // (b) 审计痕 reason 不在崩塌白名单
    let w2 = crate::worlds::create_world(
        &state.db,
        crate::worlds::CreateWorldParams::official("tpl_be", 3, "口径不一致"),
    )
    .await
    .unwrap();
    seed_world_ended_audit(&state, &w2, "mainline_complete", "none").await;
    seal(&state, &w2, true).await;
    assert!(biography_rows(&state, &w2).await.is_empty(), "审计痕与结算口径不一致时不得封卷");
}

/// 🔴 传记内容全部来自确定性数据：崩塌原因文案取自**固定字典**，不含模型现编的责任归属；
/// 同一份数据两次封卷得到**逐字节相同**的摘要（零随机、零模型的直接证明）。
#[tokio::test]
async fn be_biography_content_is_deterministic_and_blame_free() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;
    let wid = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &wid, "key_character_exit", "ending_dark").await;

    seal(&state, &wid, true).await;
    let first = biography_rows(&state, &wid).await[0].3.clone();
    let s: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(
        s["collapse"]["reasonLabel"],
        json!(collapse_reason_label("key_character_exit")),
        "崩塌文案必须是代码里的固定字典串"
    );
    assert_eq!(s["collapse"]["modelGenerated"], json!(false), "🔴 传记不得含模型生成内容");
    assert_eq!(s["collapse"]["blameAssigned"], json!(false), "🔴 传记不做责任归属判定");
    assert_eq!(
        s["collapse"]["auditReason"],
        json!("key_character_exit|ending=ending_dark"),
        "原始审计痕原样附上，任何人可回 audit_logs 对质"
    );
    assert_eq!(
        s["collapse"]["source"],
        json!("runtime::terminal_reason + audit_logs(action='world.ended')")
    );

    // 重算一次：删掉封卷行再封一次，摘要必须逐字节相同。
    sqlx::query("DELETE FROM world_biographies WHERE world_id = $1")
        .bind(&wid)
        .execute(&state.db)
        .await
        .unwrap();
    seal(&state, &wid, true).await;
    let second = biography_rows(&state, &wid).await[0].3.clone();
    assert_eq!(first, second, "同一份数据两次封卷必须得到逐字节相同的摘要");
}

/// 幂等：重复触发不重复产传记（内容与封卷时刻都不变）。
#[tokio::test]
async fn be_biography_is_idempotent() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;
    let wid = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &wid, "key_character_exit", "none").await;

    seal(&state, &wid, true).await;
    let first = biography_rows(&state, &wid).await;
    seal(&state, &wid, true).await;
    seal(&state, &wid, true).await;
    let again = biography_rows(&state, &wid).await;
    assert_eq!(again.len(), 1, "重复触发不得重复产传记");
    assert_eq!(first[0].4, again[0].4, "封卷时刻不得被后续调用改写");
    assert_eq!(first[0].3, again[0].3, "封卷内容不得被后续调用改写");
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'world.be_biography_sealed' AND subject = $1",
    )
    .bind(&wid)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(audited, 1, "封卷留痕也只应有一条");
}

/// 🔴 **公共事实不可回滚（§0.3）**：封卷是只读汇总——前后对五张世界线表 + 角色资产列做全量快照，
/// 必须逐字节相等（传记不改写任何既有事实）。
#[tokio::test]
async fn be_biography_never_mutates_worldline() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;
    let wid = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &wid, "key_character_exit", "ending_dark").await;

    let before = worldline_snapshot(&state, &wid).await;
    seal(&state, &wid, true).await;
    let after = worldline_snapshot(&state, &wid).await;
    assert_eq!(before, after, "🔴 封卷不得改写任何世界线数据（公共事实不可回滚）");
    assert_eq!(biography_rows(&state, &wid).await.len(), 1, "但传记确实产出了");
}

/// 摘要长度参数化（§0.2 禁写死）：足迹条数上限可配，超出部分截断且 `truncated` 如实标注。
#[tokio::test]
async fn be_biography_footprint_limit_is_configurable() {
    let _sw = BiographySwitch::with(true, &[("MUSE_BE_BIO_MAX_FOOTPRINTS", "1")]);
    let state = test_state().await;
    let wid = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &wid, "key_character_exit", "none").await;

    seal(&state, &wid, true).await;
    let s = biography_summary(&state, &wid).await;
    assert_eq!(s["footprints"]["items"].as_array().unwrap().len(), 1, "上限=1 时只留一条足迹");
    assert_eq!(s["footprints"]["total"], json!(2), "总数如实给出");
    assert_eq!(s["footprints"]["truncated"], json!(true), "截断必须可见，不装作没有");
}

/// 🔴 源码级红线（只读区）：BE 传记区对世界线表只有 SELECT，没有任何 UPDATE/DELETE；
/// 也不含任何模型/provider 调用（崩塌原因不许模型现编）。
#[test]
fn be_biography_region_is_read_only_and_model_free() {
    let src = include_str!("mod.rs");
    let begin = src.find("// ===== BE-BIOGRAPHY-READONLY-REGION-BEGIN =====").expect("缺少只读区起始标记");
    let end = src.find("// ===== BE-BIOGRAPHY-READONLY-REGION-END =====").expect("缺少只读区结束标记");
    let region = &src[begin..end];

    for table in ["worlds", "world_events", "world_ticks", "world_contributions", "world_members", "cloud_characters"] {
        for verb in ["UPDATE", "DELETE FROM"] {
            let needle = format!("{verb} {table}");
            assert!(
                !region.contains(&needle),
                "🔴 BE 传记只读区出现「{needle}」：传记是对既有事实的只读汇总，不得改写世界线（§0.3）"
            );
        }
    }
    // 唯一允许的写入是传记自己那一行 + 审计痕。
    assert!(region.contains("INSERT INTO world_biographies"), "封卷应写 world_biographies");
    assert!(region.contains("INSERT INTO audit_logs"), "封卷应留审计痕");

    for forbidden in [
        "ModelClient",
        "providers::",
        "state.models",
        "moderation.check",
        "complete_json",
        "muse_engine::narrative::run",
    ] {
        assert!(
            !region.contains(forbidden),
            "🔴 BE 传记只读区出现模型/provider 调用「{forbidden}」：崩塌原因必须来自确定性数据，不许模型现编"
        );
    }
}

/// 产出接点确实**接在结算路径上**：走 `settle_idle_world_ending_tx`（runtime 终局真正调用的那一个），
/// 而不是只有单测直接调封卷函数。崩塌时 ① 保底减半发放与传记封卷在**同一笔结算**里发生；
/// 正常终局走同一个函数则不封卷。
#[tokio::test]
async fn settlement_path_seals_biography_only_on_collapse() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;
    let collapsed_world = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &collapsed_world, "key_character_exit", "ending_dark").await;
    let participants =
        vec![("c_be1".to_string(), "u_be1".to_string()), ("c_be2".to_string(), "u_be2".to_string())];

    let mut tx = state.db.begin().await.unwrap();
    settle_idle_world_ending_tx(&mut tx, &collapsed_world, &participants, true).await.unwrap();
    tx.commit().await.unwrap();

    // ① 保底层减半（§9 崩塌：③归零 + ①减半 + ②已锁定保留）。
    let halved = (MILEAGE_IDLE_ENDING as f64 * COLLAPSE_BASELINE_FACTOR).round() as i64;
    assert_eq!(mileage_of(&state, "c_be1").await, halved, "崩塌时保底层减半");
    assert_eq!(biography_rows(&state, &collapsed_world).await.len(), 1, "结算路径应封出 BE 传记");

    // 正常终局：同一个结算函数，不封卷。
    let normal_world = crate::worlds::create_world(
        &state.db,
        crate::worlds::CreateWorldParams::official("tpl_be", 3, "正常收束"),
    )
    .await
    .unwrap();
    seed_world_ended_audit(&state, &normal_world, "mainline_complete", "ending_ok").await;
    let mut tx = state.db.begin().await.unwrap();
    settle_idle_world_ending_tx(&mut tx, &normal_world, &participants, false).await.unwrap();
    tx.commit().await.unwrap();
    assert!(biography_rows(&state, &normal_world).await.is_empty(), "正常终局不产出 BE 传记");
}

/// 与结算同事务：结算回滚则传记同滚——绝不出现「奖罚没落地但墓志铭已刻好」。
#[tokio::test]
async fn be_biography_rolls_back_with_failed_settlement() {
    let _sw = BiographySwitch::set(true);
    let state = test_state().await;
    let wid = seed_played_world(&state).await;
    seed_world_ended_audit(&state, &wid, "key_character_exit", "none").await;

    let mut tx = state.db.begin().await.unwrap();
    settle_idle_world_ending_tx(
        &mut tx,
        &wid,
        &[("c_be1".to_string(), "u_be1".to_string())],
        true,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap(); // 模拟结算失败

    assert!(biography_rows(&state, &wid).await.is_empty(), "结算回滚后传记不得残留");
    assert_eq!(mileage_of(&state, "c_be1").await, 0, "结算回滚后历练不得残留");
}

/// 🔴 §12【拍板 23】「死亡 = 传记封卷，**不是资产清零**」：传世卡的历练必须继续算数。
///
/// 封卷会把 `withdrawn` 置 1（`memorial` 模块复用 join 那道门），若 `total_mileage` 只看
/// `withdrawn = 0`，角色一死玩家的**卡位解锁进度就会倒退**——那正是该节明令禁止的资产清零。
/// 本用例同时钉住与「主动撤回」的区别：撤回是玩家自己收回卡（可逆），不算；封卷不可逆，要算。
#[tokio::test]
async fn sealed_memorial_cards_keep_counting_toward_mileage() {
    let state = test_state().await;
    // 在世 300 + 封卷 500 + 主动撤回 700。
    seed_char_with_mileage(&state, "c_living", "u1", 300, 0).await;
    seed_char_with_mileage(&state, "c_sealed", "u1", 500, 1).await;
    seed_char_with_mileage(&state, "c_withdrawn", "u1", 700, 1).await;
    sqlx::query("UPDATE cloud_characters SET memorial_status = 'sealed' WHERE id = 'c_sealed'")
        .execute(&state.db)
        .await
        .unwrap();

    let total = super::total_mileage(&state.db, "u1").await.unwrap();
    assert_eq!(
        total, 800,
        "在世 300 + 传世 500 = 800；主动撤回的 700 不算。\n\
         若得 300 说明传世卡被漏掉了——角色一死就把玩家的卡位进度打回去，违反 §12；\n\
         若得 1500 说明主动撤回也被算进来了，那是另一个方向的错。"
    );
}

/// 与上一条互为边界：传世卡**不再占卡位**。
///
/// 两个统计口径刻意不同——`total_mileage` 算历练（传世卡算），`count_active_cards` 算容器占用
/// （传世卡不算，它已不可再入世界）。一句话：**不再占位，但挣来的历练永远算数。**
#[tokio::test]
async fn sealed_memorial_cards_no_longer_occupy_a_card_slot() {
    let state = test_state().await;
    seed_char_with_mileage(&state, "c_alive", "u2", 0, 0).await;
    seed_char_with_mileage(&state, "c_gone", "u2", 0, 1).await;
    sqlx::query("UPDATE cloud_characters SET memorial_status = 'sealed' WHERE id = 'c_gone'")
        .execute(&state.db)
        .await
        .unwrap();

    let n = super::count_active_cards(&state.db, "u2").await.unwrap();
    assert_eq!(n, 1, "传世卡不可再入世界，不该继续占着卡位——这里必须保持 withdrawn = 0 口径");
}
