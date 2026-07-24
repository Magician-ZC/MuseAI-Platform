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
