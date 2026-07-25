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
         VALUES (?, ?, 'local', 1, '{}', 'original', 'approved', ?, ?, ?)",
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
    sqlx::query_scalar::<_, i64>("SELECT mileage FROM cloud_characters WHERE id = ?")
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
    let default_ctx = PayoutContext { table: None, star_rating: 1 };
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
