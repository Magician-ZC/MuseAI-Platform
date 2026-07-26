//! 副本卡资产与合成测试（sqlite::memory + oneshot）。覆盖：
//! - 运营开关默认关闭：端点 404 + **结算不铸卡**（前门 + 产出侧双保险）；
//! - 确定性产出（零 RNG）：同一贡献分重复结算恒得同星级同卡面；查表纯函数重复 200 次一致；
//!   且副本卡命中的档位与 progression 的历练/道具档位**恒为同一档**；
//! - 发放幂等：重复结算不双发（settled_at CAS + `(owner_id, grant_key)` 唯一键）；
//! - 星级封顶不被绕过：卡星级 > 实例星级即整张剔除（不降级、不替换）；
//! - 合成：消耗源卡、铸下一星、事务性（回滚不留痕）、幂等（重复提交不多出卡）、参数化（N / 星级上限）；
//! - **红线（grep 级 + 运行时探测）**：无任何交易/转让路径 · 副本卡不进引擎决策 · 模块零随机数。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::db::now_ms;
use crate::safety::testkit::{seed_user, seed_world, test_state, token};

// ---------------- 脚手架 ----------------

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    user: &str,
    body: Option<Value>,
    idem_key: Option<&str>,
) -> (StatusCode, Value) {
    let tk = token(state, user);
    let app = crate::app::build_router(state.clone());
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tk}"));
    if let Some(k) = idem_key {
        builder = builder.header("Idempotency-Key", k);
    }
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

async fn seed_char(state: &AppState, id: &str, owner: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at, mileage) \
         VALUES ($1, $2, 'local', 1, '{}', 'original', 'approved', 0, $3, 0)",
    )
    .bind(id)
    .bind(owner)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed cloud_character");
}

/// 把公示产出表钉进实例（结算侧的唯一数据源，与真实装配同一字段）。
async fn set_assembled(state: &AppState, world_id: &str, wrapper: Value) {
    sqlx::query("UPDATE worlds SET assembled_json = $1 WHERE id = $2")
        .bind(wrapper.to_string())
        .bind(world_id)
        .execute(&state.db)
        .await
        .expect("set assembled_json");
}

async fn seed_contribution(state: &AppState, world_id: &str, cid: &str, milestone_milli: i64) {
    sqlx::query(
        "INSERT INTO world_contributions \
         (world_id, character_id, score_milli, milestone_score_milli, settled_at, updated_at) \
         VALUES ($1, $2, $3, $4, 0, $5)",
    )
    .bind(world_id)
    .bind(cid)
    .bind(milestone_milli)
    .bind(milestone_milli)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed contribution");
}

/// 走真实结算入口（三层结算 ③ 世界线层）。
async fn settle(state: &AppState, world_id: &str, participants: &[(String, String)]) -> Vec<Value> {
    let mut tx = state.db.begin().await.unwrap();
    let out = crate::progression::settle_worldline_tx(&mut tx, world_id, participants, false)
        .await
        .expect("settle");
    tx.commit().await.unwrap();
    out
}

/// 直接铸卡播种（走唯一写入路径，不绕过任何校验）。
async fn seed_card(state: &AppState, owner: &str, star: i64, key: &str, label: &str) -> String {
    let mut tx = state.db.begin().await.unwrap();
    let id = grant_card_tx(
        &mut tx,
        &NewSubplotCard {
            owner_id: owner,
            star_rating: star,
            label,
            origin_kind: ORIGIN_GRANT,
            grant_key: key.to_string(),
            source_world_id: Some("w_seed"),
            source_template_id: Some("tpl_seed"),
            source_template_version: Some(7),
            synthesized_from: Vec::new(),
        },
    )
    .await
    .expect("seed card")
    .expect("card minted");
    tx.commit().await.unwrap();
    id
}

/// (id, star, label, status) 全量回读，按 id 升序（断言用，顺序稳定）。
async fn cards_of(state: &AppState, owner: &str) -> Vec<(String, i64, String, String)> {
    sqlx::query_as::<_, (String, i64, String, String)>(
        "SELECT id, star_rating, label, status FROM subplot_cards WHERE owner_id = $1 ORDER BY id ASC",
    )
    .bind(owner)
    .fetch_all(&state.db)
    .await
    .expect("cards")
}

async fn card_count(state: &AppState) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subplot_cards")
        .fetch_one(&state.db)
        .await
        .unwrap()
}

/// 三档产出表：1.0 见证（无卡）/ 3.0 推动（2★ 卡）/ 8.0 执笔（5★ 卡）。实例星级由入参给。
fn wrapper_with_cards(instance_star: i64) -> Value {
    json!({
        "starRating": instance_star,
        "assembly": { "payoutTable": { "worldlineTiers": [
            { "label": "见证", "minScore": 1.0, "mileage": 20 },
            { "label": "推动", "minScore": 3.0, "mileage": 80,
              "subplotCard": { "starRating": 2, "label": "星陨之夜" } },
            { "label": "执笔", "minScore": 8.0, "mileage": 200,
              "subplotCard": { "starRating": 5, "label": "执笔者" } }
        ]}}
    })
}

/// 播种一个「产出表已声明副本卡」的世界 + 一张在场卡，返回 (world_id, character_id)。
async fn seed_settlement_world(
    state: &AppState,
    world_id: &str,
    user: &str,
    cid: &str,
    instance_star: i64,
    milestone_milli: i64,
) {
    seed_user(&state.db, user).await;
    seed_char(state, cid, user).await;
    seed_world(&state.db, world_id, 1, "running").await;
    set_assembled(state, world_id, wrapper_with_cards(instance_star)).await;
    seed_contribution(state, world_id, cid, milestone_milli).await;
}

// ==================== 运营开关：默认关闭（VALIDATION §0.1） ====================

/// 默认关闭：两个端点全 404（功能不存在，不泄露"平台有这个未开放功能"）；打开后恢复。
#[tokio::test]
async fn switch_defaults_off_and_gates_all_endpoints() {
    let state = test_state().await;
    seed_user(&state.db, "u_off").await;
    {
        // env 未设置时的**默认值**必须是关闭。
        let _sw = SubplotSwitch::cleared();
        assert!(
            !subplot_cards_enabled(),
            "副本卡必须默认关闭（未验证功能默认关闭；副本卡属 VALIDATION §2 T4 才验证的范围）"
        );
    }
    {
        let _sw = SubplotSwitch::set(false);
        let (st, _) = send(&state, "GET", "/api/me/subplot-cards", "u_off", None, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "关闭时读端点必须 404");
        let (st, _) = send(
            &state,
            "POST",
            "/api/me/subplot-cards/synthesize",
            "u_off",
            Some(json!({ "cardIds": ["a", "b", "c"] })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "关闭时写端点必须 404");
    }
    {
        let _sw = SubplotSwitch::set(true);
        let (st, v) = send(&state, "GET", "/api/me/subplot-cards", "u_off", None, None).await;
        assert_eq!(st, StatusCode::OK, "打开后原样恢复: {v}");
        assert_eq!(v["cards"], json!([]));
        // 合成规则随响应公示：参数在服务端，前端不得写死副本。
        assert_eq!(v["synthesisRule"]["sourceCount"], json!(3));
        assert_eq!(v["synthesisRule"]["maxStarRating"], json!(5));
    }
    // 非法开关值不静默开启。
    {
        let _sw = SubplotSwitch::raw("maybe");
        assert!(!subplot_cards_enabled(), "非法开关值须回落关闭");
    }
}

/// 开关关闭时**结算不铸卡**（前门 404 之外的第二道保险：产出路径根本不经过端点）。
/// ③ 层的历练/道具照常发放——开关只关副本卡，不影响既有链路。
#[tokio::test]
async fn settlement_grants_no_card_while_switch_off() {
    let state = test_state().await;
    seed_settlement_world(&state, "w_off", "u1", "c1", 3, 4_000).await;

    let _sw = SubplotSwitch::set(false);
    let granted = settle(&state, "w_off", &[("c1".into(), "u1".into())]).await;

    assert_eq!(granted.len(), 1, "③ 层照常结算");
    assert_eq!(granted[0]["tier"], json!("推动"));
    assert_eq!(granted[0]["mileage"], json!(80), "历练不受副本卡开关影响");
    assert!(granted[0]["subplotCard"].is_null(), "开关关闭时不得铸卡: {granted:?}");
    assert_eq!(card_count(&state).await, 0, "库里一张卡都不该有");
}

// ==================== 确定性产出：零 RNG（§10 拍板 17 / §16 去抽卡化） ====================

/// 同一贡献分 ⇒ 恒得同一张卡（同星级、同卡面）。跑多轮独立结算，逐轮断言完全一致。
/// 张力来自"能否推动主线"的过程不确定性，不来自开箱随机数——这条不成立即为合规风险。
#[tokio::test]
async fn settlement_payout_is_deterministic_for_equal_scores() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;

    let mut observed: Vec<(i64, String)> = Vec::new();
    for i in 0..6 {
        let (w, u, c) = (format!("w_det{i}"), format!("u_det{i}"), format!("c_det{i}"));
        // 同一张产出表、同一实例星级、**同一贡献分**（4.0）。
        seed_settlement_world(&state, &w, &u, &c, 3, 4_000).await;
        let granted = settle(&state, &w, &[(c.clone(), u.clone())]).await;
        let card = &granted[0]["subplotCard"];
        assert!(!card.is_null(), "第 {i} 轮应铸卡: {granted:?}");
        observed.push((card["starRating"].as_i64().unwrap(), card["label"].as_str().unwrap().into()));
    }
    assert!(
        observed.iter().all(|o| *o == observed[0]),
        "同一贡献分必须恒得同一产出（零 RNG）：{observed:?}"
    );
    assert_eq!(observed[0], (2, "星陨之夜".to_string()));
}

/// 查表纯函数确定性：同一分数重复查 200 次结果逐次一致；门槛语义"含等于、取最高档"。
/// 更低的分落更低的档、低于最低门槛不发——**没有保底、没有爆率**。
#[tokio::test]
async fn card_tier_lookup_is_deterministic_and_rng_free() {
    let payouts = SubplotCardPayouts::from_wrapper_value(Some(&wrapper_with_cards(5)));

    assert!(payouts.resolve(0.9).is_none(), "低于最低门槛 → 无档");
    assert!(payouts.resolve(1.0).is_none(), "命中'见证'档，但该档未声明副本卡 → 不发卡");
    assert_eq!(payouts.resolve(3.0).unwrap().1.star_rating, 2, "门槛含等于");
    assert_eq!(payouts.resolve(7.999).unwrap().1.label, "星陨之夜");
    assert_eq!(payouts.resolve(8.0).unwrap().1.star_rating, 5);
    assert_eq!(payouts.resolve(1_000.0).unwrap().1.star_rating, 5, "超高分仍落最高档，不外溢");

    let first = payouts.resolve(4.25).unwrap().1.star_rating;
    for _ in 0..200 {
        assert_eq!(payouts.resolve(4.25).unwrap().1.star_rating, first, "重复查表必须逐次一致");
    }
}

/// 副本卡命中的档位与 progression（历练/道具）命中的档位**恒为同一档**。
/// 两侧各自解析同一个 `worldlineTiers` 数组，任何一侧改了查表规则，本断言立刻炸。
#[test]
fn card_tier_matches_progression_tier() {
    let wrapper = wrapper_with_cards(5);
    let raw = wrapper.to_string();
    let ctx = crate::progression::payout_context_from_wrapper(Some(&raw));
    let table = ctx.table.as_ref().expect("产出表");

    for score in [0.0, 0.999, 1.0, 2.5, 3.0, 4.25, 7.999, 8.0, 50.0] {
        let progression_tier = crate::progression::resolve_payout_tier(table, score).map(|t| &t.label);
        let card_tier = ctx.subplot_cards.resolve(score).map(|(t, _)| &t.label);
        // 副本卡侧只在"该档声明了卡"时返回；命中档位一致时二者的 label 必须相同。
        if let Some(card_label) = card_tier {
            assert_eq!(
                Some(card_label),
                progression_tier,
                "score={score} 两侧命中的档位必须是同一档（否则'同分同产出'不成立）"
            );
        }
    }
    // "见证"档（1.0-2.999）在 progression 侧命中、在副本卡侧无卡：这是数据声明，不是档位漂移。
    assert!(crate::progression::resolve_payout_tier(table, 2.0).is_some());
    assert!(ctx.subplot_cards.resolve(2.0).is_none());
}

/// 数据侧默认关闭：产出表未声明 `subplotCard` → 该档不发卡（开闸靠运营录数据，不靠代码合并）。
#[tokio::test]
async fn no_card_without_payout_table_declaration() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_nod").await;
    seed_char(&state, "c_nod", "u_nod").await;
    seed_world(&state.db, "w_nod", 1, "running").await;
    set_assembled(
        &state,
        "w_nod",
        json!({ "starRating": 5, "assembly": { "payoutTable": { "worldlineTiers": [
            { "label": "推动", "minScore": 1.0, "mileage": 80 }
        ]}}}),
    )
    .await;
    seed_contribution(&state, "w_nod", "c_nod", 9_000).await;

    let granted = settle(&state, "w_nod", &[("c_nod".into(), "u_nod".into())]).await;
    assert_eq!(granted[0]["mileage"], json!(80));
    assert!(granted[0]["subplotCard"].is_null(), "未声明副本卡的档位不得发卡");
    assert_eq!(card_count(&state).await, 0);
}

// ==================== 发放幂等（§0.2 资产单一写入路径自带幂等键） ====================

/// 重复结算不双发：settled_at CAS（防线②）挡住第二次；即便直闯写入路径，
/// `(owner_id, grant_key)` 唯一键（防线③）也只让一张卡落地。
#[tokio::test]
async fn settlement_card_grant_is_idempotent() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_settlement_world(&state, "w_idem", "u_idem", "c_idem", 3, 4_000).await;
    let participants = vec![("c_idem".to_string(), "u_idem".to_string())];

    let first = settle(&state, "w_idem", &participants).await;
    assert!(!first[0]["subplotCard"].is_null());
    assert_eq!(card_count(&state).await, 1);

    // 防线②：再次结算 → settled_at 已非 0 → 一件不发。
    let again = settle(&state, "w_idem", &participants).await;
    assert!(again.is_empty(), "重复结算必须一件不发: {again:?}");
    assert_eq!(card_count(&state).await, 1, "重复结算不得二次铸卡");

    // 防线③：绕过 CAS 直闯唯一写入路径，同 grant_key → 幂等返回 None，库里仍只有一张。
    let mut tx = state.db.begin().await.unwrap();
    let dup = grant_card_tx(
        &mut tx,
        &NewSubplotCard {
            owner_id: "u_idem",
            star_rating: 2,
            label: "星陨之夜",
            origin_kind: ORIGIN_SETTLEMENT,
            grant_key: format!("{ORIGIN_SETTLEMENT}:w_idem:c_idem:worldline"),
            source_world_id: Some("w_idem"),
            source_template_id: None,
            source_template_version: None,
            synthesized_from: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert!(dup.is_none(), "同幂等键必须判为已发放，不二次铸卡");
    drop(tx);
    assert_eq!(card_count(&state).await, 1);
}

/// 结算失败不发卡：与结算同事务——事务回滚则卡同滚（"同事务"性质的直接证明）。
#[tokio::test]
async fn card_grant_rolls_back_with_failed_settlement() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_settlement_world(&state, "w_rb", "u_rb", "c_rb", 3, 4_000).await;

    let mut tx = state.db.begin().await.unwrap();
    let granted =
        crate::progression::settle_worldline_tx(&mut tx, "w_rb", &[("c_rb".into(), "u_rb".into())], false)
            .await
            .unwrap();
    assert!(!granted[0]["subplotCard"].is_null(), "事务内已铸出");
    tx.rollback().await.unwrap(); // 模拟结算失败（CAS 不命中 / 后续发货报错）

    assert_eq!(card_count(&state).await, 0, "结算回滚后副本卡不得残留");
}

/// 幂等键为空 / 星级越界 → 直接拒绝（不静默钳位：钳位会把配置错误变成真实资产）。
#[tokio::test]
async fn grant_card_rejects_empty_key_and_out_of_range_star() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_bad").await;
    let mut tx = state.db.begin().await.unwrap();
    let mk = |star: i64, key: &str| NewSubplotCard {
        owner_id: "u_bad",
        star_rating: star,
        label: "x",
        origin_kind: ORIGIN_GRANT,
        grant_key: key.to_string(),
        source_world_id: None,
        source_template_id: None,
        source_template_version: None,
        synthesized_from: Vec::new(),
    };
    assert!(grant_card_tx(&mut tx, &mk(1, "  ")).await.is_err(), "空幂等键必须拒绝");
    assert!(grant_card_tx(&mut tx, &mk(0, "k0")).await.is_err(), "0★ 非法");
    assert!(grant_card_tx(&mut tx, &mk(6, "k6")).await.is_err(), "超上限星级非法");
    assert!(grant_card_tx(&mut tx, &mk(5, "k5")).await.unwrap().is_some(), "5★ 在上限内");
    drop(tx);
}

// ==================== 星级封顶不被绕过（与装配层同口径） ====================

/// 卡星级 > 实例星级 → **整张剔除**（不降级、不替换），与 `assembly` 的 culled_over_tier 同口径。
/// 同一张表在 5★ 实例上则如数发放——证明剔除是封顶判定，不是"这一档本来就没卡"。
#[tokio::test]
async fn settlement_card_respects_star_cap() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;

    // 3★ 实例 + 高分（10.0 → "执笔"档，卡 5★）→ 超顶剔除，但历练/档位照常。
    seed_settlement_world(&state, "w_cap", "u_cap", "c_cap", 3, 10_000).await;
    let granted = settle(&state, "w_cap", &[("c_cap".into(), "u_cap".into())]).await;
    assert_eq!(granted[0]["tier"], json!("执笔"));
    assert_eq!(granted[0]["mileage"], json!(200), "封顶只剔除卡，不影响历练");
    assert!(granted[0]["subplotCard"].is_null(), "5★ 卡在 3★ 实例上必须被剔除");
    assert_eq!(card_count(&state).await, 0);

    // 同一张表、同一分数，换 5★ 实例 → 如数发放（证明上面剔除的是封顶，不是数据缺失）。
    seed_settlement_world(&state, "w_cap5", "u_cap5", "c_cap5", 5, 10_000).await;
    let granted = settle(&state, "w_cap5", &[("c_cap5".into(), "u_cap5".into())]).await;
    assert_eq!(granted[0]["subplotCard"]["starRating"], json!(5));
    assert_eq!(card_count(&state).await, 1);
}

// ==================== 同星合成：回收口 ====================

/// 3×2★ → 1×3★：源卡销毁（软删可溯 + consumed_into 指向新卡）、新卡入手、总量净减 2。
#[tokio::test]
async fn synthesis_consumes_sources_and_mints_next_star() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_syn").await;
    let mut ids = Vec::new();
    for i in 0..3 {
        ids.push(seed_card(&state, "u_syn", 2, &format!("seed{i}"), "星陨之夜").await);
    }

    let (st, v) = send(
        &state,
        "POST",
        "/api/me/subplot-cards/synthesize",
        "u_syn",
        Some(json!({ "cardIds": ids })),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["card"]["starRating"], json!(3), "3×2★ → 1×3★");
    assert_eq!(v["card"]["originKind"], json!("synthesis"));
    assert_eq!(v["card"]["label"], json!("星陨之夜"), "蓝图来源确定性继承（id 升序首张）");
    let new_id = v["card"]["id"].as_str().unwrap().to_string();

    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(v["card"]["synthesizedFrom"], json!(sorted), "血缘按 id 升序全量留存");

    // 源卡全部 consumed 且指向新卡；在手的只剩新卡一张。
    let all = cards_of(&state, "u_syn").await;
    assert_eq!(all.len(), 4, "软删不物理删行：回收口必须可溯");
    assert_eq!(all.iter().filter(|c| c.3 == "consumed").count(), 3);
    assert_eq!(all.iter().filter(|c| c.3 == "owned").count(), 1);
    let into: Vec<String> = sqlx::query_scalar(
        "SELECT consumed_into FROM subplot_cards WHERE owner_id='u_syn' AND status='consumed'",
    )
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert!(into.iter().all(|x| *x == new_id), "源卡的 consumed_into 必须指向真实新卡");

    // 列表默认只出在手的卡；status=all 可回看已熔材料。
    let (st, list) = send(&state, "GET", "/api/me/subplot-cards", "u_syn", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["cards"].as_array().unwrap().len(), 1);
    assert_eq!(list["cards"][0]["starRating"], json!(3));
    assert_eq!(list["cards"][0]["source"]["templateId"], json!("tpl_seed"), "来源随卡走");
    let (_, all_list) = send(&state, "GET", "/api/me/subplot-cards?status=all", "u_syn", None, None).await;
    assert_eq!(all_list["cards"].as_array().unwrap().len(), 4);

    // 审计留痕（§0.2 全链审计）。
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'subplot.card_synthesized' AND subject = $1",
    )
    .bind(&new_id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(n, 1, "回收口的每一次销毁都要留痕");
}

/// 合成幂等：重复提交同一组源卡不得凭空多出卡。
/// - 裸重放 → 源卡已 consumed → 409，卡数不变；
/// - 带 Idempotency-Key 重放 → 直接回缓存响应（不重放事务），卡数不变。
#[tokio::test]
async fn synthesis_is_idempotent_on_repeat() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_rep").await;
    let mut ids = Vec::new();
    for i in 0..3 {
        ids.push(seed_card(&state, "u_rep", 2, &format!("seed{i}"), "甲").await);
    }
    let body = json!({ "cardIds": ids });

    let (st, first) = send(
        &state,
        "POST",
        "/api/me/subplot-cards/synthesize",
        "u_rep",
        Some(body.clone()),
        Some("k-syn-1"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{first}");
    assert_eq!(card_count(&state).await, 4);

    // 同 key 同载荷 → 缓存响应，事务不重放。
    let (st, cached) = send(
        &state,
        "POST",
        "/api/me/subplot-cards/synthesize",
        "u_rep",
        Some(body.clone()),
        Some("k-syn-1"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cached["card"]["id"], first["card"]["id"], "同幂等键必须回同一张卡");
    assert_eq!(card_count(&state).await, 4, "幂等重放不得多出卡");

    // 换 key 裸重放 → 源卡已熔 → 409，卡数仍不变（防线②源卡 CAS）。
    let (st, v) =
        send(&state, "POST", "/api/me/subplot-cards/synthesize", "u_rep", Some(body), Some("k-syn-2")).await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(card_count(&state).await, 4, "重复提交不得凭空多出卡");

    // 提交顺序不同不算不同载荷（幂等键按升序 id 算哈希）。
    let mut shuffled_order = ids.clone();
    shuffled_order.reverse();
    let (st, _) = send(
        &state,
        "POST",
        "/api/me/subplot-cards/synthesize",
        "u_rep",
        Some(json!({ "cardIds": shuffled_order })),
        Some("k-syn-1"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "同一组卡换个提交顺序仍是同一载荷 → 回缓存");
    assert_eq!(card_count(&state).await, 4);
}

/// 合成是事务性的：铸卡与销毁源卡同生共死——回滚后既无新卡、源卡也原封不动在手。
#[tokio::test]
async fn synthesis_writes_roll_back_as_one_unit() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_tx").await;
    let a = seed_card(&state, "u_tx", 2, "s0", "甲").await;

    let mut tx = state.db.begin().await.unwrap();
    let minted = grant_card_tx(
        &mut tx,
        &NewSubplotCard {
            owner_id: "u_tx",
            star_rating: 3,
            label: "甲",
            origin_kind: ORIGIN_SYNTHESIS,
            grant_key: format!("{ORIGIN_SYNTHESIS}:{a}"),
            source_world_id: None,
            source_template_id: None,
            source_template_version: None,
            synthesized_from: vec![a.clone()],
        },
    )
    .await
    .unwrap()
    .unwrap();
    sqlx::query("UPDATE subplot_cards SET status='consumed', consumed_at=$1, consumed_into=$2 WHERE id=$3")
        .bind(now_ms())
        .bind(&minted)
        .bind(&a)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap(); // 任一步失败 → 整笔回滚

    let all = cards_of(&state, "u_tx").await;
    assert_eq!(all.len(), 1, "回滚后不得留下新卡");
    assert_eq!(all[0].3, "owned", "回滚后源卡必须原封不动在手（绝不'熔了但没出卡'）");
}

/// 前门校验：张数不符 / 重复 id / 异星 / 他人卡 / 已熔卡 —— 一律拒绝，且**任何一张卡都不动**。
#[tokio::test]
async fn synthesis_rejects_bad_recipes_without_touching_any_card() {
    let _sw = SubplotSwitch::set(true);
    let state = test_state().await;
    seed_user(&state.db, "u_a").await;
    seed_user(&state.db, "u_b").await;
    let a1 = seed_card(&state, "u_a", 2, "a1", "甲").await;
    let a2 = seed_card(&state, "u_a", 2, "a2", "甲").await;
    let a3 = seed_card(&state, "u_a", 3, "a3", "乙").await; // 异星
    let b1 = seed_card(&state, "u_b", 2, "b1", "丙").await; // 他人卡

    let post = |body: Value| {
        let state = state.clone();
        async move {
            send(&state, "POST", "/api/me/subplot-cards/synthesize", "u_a", Some(body), None).await
        }
    };

    let (st, v) = post(json!({ "cardIds": [a1.clone(), a2.clone()] })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "张数不符: {v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("需要 3 张"));

    let (st, _) = post(json!({ "cardIds": [a1.clone(), a1.clone(), a2.clone()] })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "重复 id 必须拒绝（否则一张卡能当三张用）");

    let (st, v) = post(json!({ "cardIds": [a1.clone(), a2.clone(), a3.clone()] })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "异星必须拒绝: {v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("同星级"));

    let (st, v) = post(json!({ "cardIds": [a1.clone(), a2.clone(), b1.clone()] })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "他人卡必须拒绝（owner 隔离，不泄露他人资产）: {v}");

    let (st, _) = post(json!({ "cardIds": [a1.clone(), a2.clone(), "sc_ghost"] })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "不存在的卡必须拒绝");

    // 全程一张卡都没被动过。
    let all = cards_of(&state, "u_a").await;
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|c| c.3 == "owned"), "失败的合成不得改动任何一张源卡: {all:?}");
    assert_eq!(cards_of(&state, "u_b").await[0].3, "owned", "他人卡更不该被动");
}

/// 参数化（VALIDATION §0.2 禁写死）：配方张数 N 与星级上限均由运营 env 决定。
#[tokio::test]
async fn synthesis_recipe_and_star_cap_are_parameterized() {
    let state = test_state().await;
    seed_user(&state.db, "u_par").await;

    // N=2：两张 2★ 即可合成 3★（默认 3 张是初值，不是承诺）。
    {
        let _sw = SubplotSwitch::with(true, &[("MUSE_SUBPLOT_SYNTHESIS_N", "2")]);
        assert_eq!(synthesis_source_count(), 2);
        let p1 = seed_card(&state, "u_par", 2, "p1", "甲").await;
        let p2 = seed_card(&state, "u_par", 2, "p2", "甲").await;
        let (st, v) = send(
            &state,
            "POST",
            "/api/me/subplot-cards/synthesize",
            "u_par",
            Some(json!({ "cardIds": [p1, p2] })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["card"]["starRating"], json!(3));
        assert_eq!(v["sourceCount"], json!(2));
    }
    // 星级上限 = 2 时，2★ 已到顶不可再合成（回收口在顶端自然封口）。
    {
        let _sw = SubplotSwitch::with(true, &[("MUSE_SUBPLOT_MAX_STAR", "2")]);
        assert_eq!(max_star_rating(), 2);
        let q1 = seed_card(&state, "u_par", 2, "q1", "甲").await;
        let q2 = seed_card(&state, "u_par", 2, "q2", "甲").await;
        let q3 = seed_card(&state, "u_par", 2, "q3", "甲").await;
        let (st, v) = send(
            &state,
            "POST",
            "/api/me/subplot-cards/synthesize",
            "u_par",
            Some(json!({ "cardIds": [q1, q2, q3] })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"]["message"].as_str().unwrap().contains("星级上限"));
    }
    // 非法/越界 env 回落安全区间（配错不静默放开经济参数）。
    {
        let _sw = SubplotSwitch::with(
            true,
            &[("MUSE_SUBPLOT_SYNTHESIS_N", "999"), ("MUSE_SUBPLOT_MAX_STAR", "abc")],
        );
        assert_eq!(synthesis_source_count(), MAX_SYNTHESIS_SOURCE_COUNT, "越界 clamp 进安全区间");
        assert_eq!(max_star_rating(), DEFAULT_MAX_STAR_RATING, "非法值回落默认");
    }
}

// ==================== 红线断言 ====================

/// 🔴 **无提现红线下不实现任何交易/转让路径**（§10「玩家间交易暂不开」+ §0.5）。
/// 三重锁：① 路由白名单（本模块只注册这两条路径）；② 源码级无任何改写 `owner_id` 的 SQL；
/// ③ 运行时探测——即便开关全开，转让形态的 URL 也一律 404（路由压根不存在）。
#[tokio::test]
async fn red_line_no_transfer_or_trade_path() {
    let src = include_str!("mod.rs");

    let routes: Vec<&str> = src.split(".route(\"").skip(1).filter_map(|s| s.split('"').next()).collect();
    assert_eq!(
        routes,
        vec!["/me/subplot-cards", "/me/subplot-cards/synthesize"],
        "副本卡模块只允许注册「查询」与「合成」两条路径；任何新增端点都需显式评审"
    );

    assert!(
        !src.contains("SET owner_id"),
        "owner_id 只在 INSERT 时写入：任何改写它的 SQL 都是转让侧门（RMT 风险）"
    );

    let state = test_state().await;
    seed_user(&state.db, "u_red").await;
    let _sw = SubplotSwitch::set(true);
    let card = seed_card(&state, "u_red", 2, "red1", "甲").await;
    for path in [
        format!("/api/me/subplot-cards/{card}/transfer"),
        format!("/api/me/subplot-cards/{card}/gift"),
        format!("/api/me/subplot-cards/{card}/sell"),
        "/api/me/subplot-cards/market".to_string(),
        "/api/subplot-cards/trades".to_string(),
    ] {
        let (st, _) = send(&state, "POST", &path, "u_red", Some(json!({})), None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path} 必须不存在（无提现红线下道具交易 = RMT 侧门）");
    }
}

/// 🔴 **副本卡永不加战力**（§0.1 平权宪法）：引擎决策路径源码级零 `subplot` 引用。
/// 口径与历练 mileage 的红线断言逐字一致——资产只决定产出与内容装配，绝不进 role_decide / 仲裁。
#[test]
fn red_line_subplot_cards_never_enter_engine_decision() {
    let runtime_src = include_str!("../runtime/mod.rs");
    assert!(
        !runtime_src.contains("subplot"),
        "runtime/mod.rs（RoundInput 组装处）不得引用副本卡：资产绝不进入引擎决策"
    );
    let engine_narrative_src = include_str!("../../../crates/muse-engine/src/narrative/mod.rs");
    assert!(
        !engine_narrative_src.contains("subplot"),
        "muse-engine narrative（RoundInput/role_decide/仲裁）不得引用副本卡：叙事平权是产品根基"
    );
    let engine_decide_src = include_str!("../../../crates/muse-engine/src/narrative/decide.rs");
    assert!(!engine_decide_src.contains("subplot"), "role_decide 不得引用副本卡");

    let src = include_str!("mod.rs");
    assert!(
        !src.contains("narrative_state_json"),
        "本模块不得写世界叙事状态列（那一列每拍回灌进引擎，写进去即把资产喂给决策）"
    );
}

/// 🔴 **零 RNG**（§10 拍板 17 / §16 去抽卡化是定性防线的关键）：模块源码不含任何随机数 API。
/// 有随机就有爆率，有爆率就是抽卡——这是合规定性问题，不是玩法偏好问题。
#[test]
fn red_line_module_is_rng_free() {
    let src = include_str!("mod.rs").to_ascii_lowercase();
    for banned in ["rand::", "random", "shuffle", "thread_rng", "gen_range", "choose(", "sample("] {
        assert!(!src.contains(banned), "副本卡模块出现随机数 API `{banned}`：确定性产出是合规红线");
    }
}
