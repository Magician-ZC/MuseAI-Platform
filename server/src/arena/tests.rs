//! 赛事房集成测试（sqlite::memory + oneshot）。覆盖：
//! - 主播触发回合（复用 runtime::schedule_tick，no-model 安全）+ 主播/房主守卫；
//! - 淘汰同意门控（补 P4a 缺口）：player 角色淘汰 → create_consent；approved 才落定、pending/declined 保守不落定；
//! - 赛制淘汰收敛唯一胜者 + 胜者荣誉奖励（非强度）；
//! - 透明战报聚合 public world_events（谁做了什么 + 判定依据）+ arena_env_events（礼物/环境）；
//! - 复活赛记资格不免死 + 红线（无免死/无买最终判定端点）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::AnyPool;
use tower::ServiceExt;

use crate::app::AppState;
use crate::db::{new_id, now_ms};
use crate::safety::testkit::{count, seed_member, seed_user, test_state, token};

// ---------- 脚手架 ----------

/// running 赛事世界（带 host_user_id）。
async fn seed_arena_world(db: &AnyPool, id: &str, host: &str, visibility: &str) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, host_user_id, member_limit, tick_per_day, \
         state_revision, narrative_state_json, created_at, updated_at) \
         VALUES (?, 'tpl', 1, 'e1', 'p1', 'm1', 'arena', '赛事世界', 'running', ?, ?, 10, 3, 0, '{}', ?, ?)",
    )
    .bind(id)
    .bind(visibility)
    .bind(host)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .expect("seed arena world");
}

async fn seed_public_event(
    db: &AnyPool,
    world_id: &str,
    tick: i64,
    seq: i64,
    etype: &str,
    summary: &str,
    arbiter_note: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
         visibility, public_projection_json, arbiter_note, occurred_at) \
         VALUES (?, ?, ?, ?, ?, ?, '[\"c1\"]', 'public', ?, ?, ?)",
    )
    .bind(new_id("we"))
    .bind(world_id)
    .bind(tick)
    .bind(seq)
    .bind(new_id("de"))
    .bind(etype)
    .bind(json!({ "summary": summary }).to_string())
    .bind(arbiter_note)
    .bind(now_ms())
    .execute(db)
    .await
    .expect("seed public event");
}

async fn seed_private_event(db: &AnyPool, world_id: &str, seq: i64, secret_summary: &str) {
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, actors_json, \
         visibility, audience_json, private_projections_json, occurred_at) \
         VALUES (?, ?, 0, ?, ?, 'status', '[\"c1\"]', 'private', '[\"someone\"]', ?, ?)",
    )
    .bind(new_id("we"))
    .bind(world_id)
    .bind(seq)
    .bind(new_id("de"))
    .bind(json!([{ "audiencePrincipalIds": ["someone"], "summary": secret_summary }]).to_string())
    .bind(now_ms())
    .execute(db)
    .await
    .expect("seed private event");
}

async fn seed_env(db: &AnyPool, world_id: &str, applied_tick: Option<i64>, kind: &str, payload: Value, agg: i64) {
    sqlx::query(
        "INSERT INTO arena_env_events (id, world_id, applied_tick, kind, payload_json, aggregated_count, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id("env"))
    .bind(world_id)
    .bind(applied_tick)
    .bind(kind)
    .bind(payload.to_string())
    .bind(agg)
    .bind(now_ms())
    .execute(db)
    .await
    .expect("seed env event");
}

/// 带鉴权的 oneshot 请求。
async fn send(state: &AppState, method: &str, uri: &str, user: &str, body: Option<Value>) -> (StatusCode, Value) {
    let tk = token(state, user);
    let app = crate::app::build_router(state.clone());
    let builder = Request::builder().method(method).uri(uri).header("authorization", format!("Bearer {tk}"));
    let request = match body {
        Some(b) => builder.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

async fn post(state: &AppState, uri: &str, user: &str, body: Value) -> (StatusCode, Value) {
    send(state, "POST", uri, user, Some(body)).await
}
async fn get(state: &AppState, uri: &str, user: &str) -> (StatusCode, Value) {
    send(state, "GET", uri, user, None).await
}

/// 通过真实 consents::respond 端点批准/拒绝同意（角色主人）。
async fn respond_consent(state: &AppState, user: &str, world: &str, cid: &str, approve: bool) -> (StatusCode, Value) {
    post(state, &format!("/api/worlds/{world}/consents/{cid}/respond"), user, json!({ "approve": approve })).await
}

// ---------- host/tick：复用 runtime 触发 + 主播守卫 + no-model 安全 ----------

#[tokio::test]
async fn host_tick_reuses_runtime_and_requires_host() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "other").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_arena_world(&state.db, "w", "host", "official").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;

    // 非主播 → 403。
    let (s, _) = post(&state, "/api/arena/w/host/tick", "other", json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // 主播触发 → 200，复用 runtime::schedule_tick 排下一 tick（no-model：worker 会 no-op，本测试不跑 worker）。
    let (s, v) = post(&state, "/api/arena/w/host/tick", "host", json!({})).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["scheduled"], true);
    assert_eq!(v["tickNo"], 0, "首次触发排 tick 0");
    // world_ticks 落了一条 pending（无 LLM 依赖，纯赛制/调度层）。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_ticks WHERE world_id='w'").await, 1);
    // 赛事进入 running。
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT phase FROM arena_matches WHERE world_id='w'").fetch_one(&state.db).await.unwrap(),
        "running"
    );

    // 红线佐证：触发回合绝不设置 winner（胜者只由 settle 收敛产生）。
    let (_, rep) = get(&state, "/api/arena/w/report", "host").await;
    assert!(rep["match"]["winnerCharId"].is_null(), "触发回合不得产生胜者");

    // 再次触发排 tick 1。
    let (_, v2) = post(&state, "/api/arena/w/host/tick", "host", json!({})).await;
    assert_eq!(v2["tickNo"], 1);
}

// ---------- 淘汰同意门控：approved 才落定、pending 不落定；收敛唯一胜者 + 荣誉奖励 ----------

#[tokio::test]
async fn elimination_gated_by_consent_then_converges_to_unique_winner() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_arena_world(&state.db, "w", "host", "official").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;

    // 主播裁定淘汰 player-owned 角色 c1 → 触发 consents::create_consent，台账 pending_consent，**不**立即落定。
    let (s, v) = post(&state, "/api/arena/w/eliminate", "host", json!({ "cloudCharacterId": "c1" })).await;
    assert_eq!(s, StatusCode::OK, "body={v}");
    assert_eq!(v["status"], "pending_consent");
    let cid = v["consentId"].as_str().expect("consentId").to_string();
    // 补缺口证据：确实建了一条 consent_requests（permanent_exit）。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM consent_requests WHERE event_kind='permanent_exit'").await,
        1
    );
    // 尚未落定：eliminations 空。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_matches WHERE world_id='w' AND eliminations_json='[]'").await, 1);

    // 同意未批准前 settle → 保守不落定（pending）。
    let (_, v) = post(&state, "/api/arena/w/settle", "host", json!({})).await;
    assert_eq!(v["eliminations"].as_array().unwrap().len(), 0, "pending 同意不得落定淘汰");
    assert!(v["winnerCharId"].is_null());

    // 当事角色主人 u1 批准同意。
    let (s, resp) = respond_consent(&state, "u1", "w", &cid, true).await;
    assert_eq!(s, StatusCode::OK, "body={resp}");
    assert_eq!(resp["status"], "approved");

    // 再 settle → c1 落定淘汰；roster{c1,c2} 收敛到唯一胜者 c2；phase=concluded。
    let (_, v) = post(&state, "/api/arena/w/settle", "host", json!({})).await;
    let elim: Vec<String> = serde_json::from_value(v["eliminations"].clone()).unwrap();
    assert_eq!(elim, vec!["c1".to_string()], "approved 后才落定淘汰");
    assert_eq!(v["winnerCharId"], "c2", "淘汰收敛到 1 人即唯一胜者");
    assert_eq!(v["phase"], "concluded");

    // 台账状态推进为 eliminated。
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM arena_eliminations WHERE world_id='w' AND character_id='c1'").fetch_one(&state.db).await.unwrap(),
        "eliminated"
    );
    // 胜者荣誉奖励（非强度）：只发称号（title），无任何强度字段。
    let reward_kind = sqlx::query_scalar::<_, String>("SELECT kind FROM arena_rewards WHERE world_id='w' AND character_id='c2'").fetch_one(&state.db).await.unwrap();
    assert_eq!(reward_kind, "title", "胜者奖励为荣誉性称号，非强度");
}

// ---------- 拒绝/超时保守：declined → 不落定淘汰 ----------

#[tokio::test]
async fn declined_consent_is_conservative_not_eliminated() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_arena_world(&state.db, "w", "host", "official").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;

    let (_, v) = post(&state, "/api/arena/w/eliminate", "host", json!({ "cloudCharacterId": "c1" })).await;
    let cid = v["consentId"].as_str().unwrap().to_string();

    // 当事人拒绝 → declined。
    let (_, resp) = respond_consent(&state, "u1", "w", &cid, false).await;
    assert_eq!(resp["status"], "declined");

    // settle → 保守：不落定淘汰（spared），无胜者。
    let (_, v) = post(&state, "/api/arena/w/settle", "host", json!({})).await;
    assert_eq!(v["eliminations"].as_array().unwrap().len(), 0, "declined 保守不落定");
    assert!(v["winnerCharId"].is_null(), "无淘汰则无唯一胜者");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM arena_eliminations WHERE world_id='w' AND character_id='c1'").fetch_one(&state.db).await.unwrap(),
        "spared"
    );
}

// ---------- 透明战报：聚合 world_events + 判定依据 + arena_env_events；不出私有 ----------

#[tokio::test]
async fn report_aggregates_public_events_rules_and_env() {
    let state = test_state().await;
    seed_user(&state.db, "spectator").await;
    seed_arena_world(&state.db, "w", "host", "public").await;

    // tick 0：两条 public 事件（含判定依据 arbiter_note）；tick 1：一条。
    seed_public_event(&state.db, "w", 0, 0, "action", "李上前行礼", Some("rule:target;rule:resource")).await;
    seed_public_event(&state.db, "w", 0, 1, "dialogue", "寒暄致意", None).await;
    seed_public_event(&state.db, "w", 1, 2, "action", "王拂袖而去", Some("rule:mind_control")).await;
    // 私有事件不得进透明战报（对抗剧本质疑靠的是可公开验证的公共日志）。
    seed_private_event(&state.db, "w", 3, "机密：王暗藏毒计").await;

    // 礼物/环境：applied_tick=0 的 gift_boon（聚合 3 次）+ 一条尚未注入回合的 seam 记录（applied_tick NULL）。
    seed_env(&state.db, "w", Some(0), "gift_boon", json!({ "sku": "rocket", "boon": "storm_env" }), 3).await;
    seed_env(&state.db, "w", None, "gift_boon", json!({ "sku": "rose", "boon": "calm_env" }), 1).await;

    let (s, rep) = get(&state, "/api/arena/w/report", "spectator").await;
    assert_eq!(s, StatusCode::OK, "body={rep}");

    // 回合聚合：tick 0 两条、tick 1 一条。
    let rounds = rep["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0]["tick"], 0);
    assert_eq!(rounds[0]["events"].as_array().unwrap().len(), 2);
    assert_eq!(rounds[0]["events"][0]["summary"], "李上前行礼");
    // 判定依据（rule_refs）由 arbiter_note 拆分而来。
    let refs: Vec<String> = serde_json::from_value(rounds[0]["events"][0]["ruleRefs"].clone()).unwrap();
    assert_eq!(refs, vec!["rule:target".to_string(), "rule:resource".to_string()]);
    // tick 0 的礼物 boon 挂到该回合。
    assert_eq!(rounds[0]["env"].as_array().unwrap().len(), 1);
    assert_eq!(rounds[0]["env"][0]["kind"], "gift_boon");
    assert_eq!(rounds[0]["env"][0]["aggregatedCount"], 3);

    // 全量环境日志含尚未注入回合的 seam 记录。
    assert_eq!(rep["environment"].as_array().unwrap().len(), 2);

    // 合规承诺展示（对抗「是不是剧本」）。
    assert_eq!(rep["compliance"]["arbitrationPublic"], true);
    assert_eq!(rep["compliance"]["aiGenerated"], true);

    // 不泄露私有投影/隐藏推理。
    assert!(!rep.to_string().contains("机密"), "透明战报不得含私有投影");
}

// ---------- 复活赛：记资格不免死；红线（无免死/无买最终判定端点） ----------

#[tokio::test]
async fn revive_records_eligibility_only_not_immunity() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_user(&state.db, "viewer").await;
    seed_arena_world(&state.db, "w", "host", "official").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;

    // 先把 c1 经同意门控落定淘汰 → 胜者 c2。
    let (_, v) = post(&state, "/api/arena/w/eliminate", "host", json!({ "cloudCharacterId": "c1" })).await;
    let cid = v["consentId"].as_str().unwrap().to_string();
    respond_consent(&state, "u1", "w", &cid, true).await;
    let (_, settled) = post(&state, "/api/arena/w/settle", "host", json!({})).await;
    assert_eq!(settled["winnerCharId"], "c2");

    // 观众为被淘汰角色 c1 购买复活赛资格 → 只记 eligibility，明确不是免死/不改最终判定。
    let (s, rv) = post(&state, "/api/arena/w/revive-match", "viewer", json!({ "cloudCharacterId": "c1" })).await;
    assert_eq!(s, StatusCode::OK, "body={rv}");
    assert_eq!(rv["status"], "eligible");
    assert_eq!(rv["boundary"]["notImmunity"], true);
    assert_eq!(rv["boundary"]["notFinalVerdict"], true);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_revive_grants WHERE world_id='w' AND status='eligible'").await, 1);

    // 红线：复活资格绝不撤销既有淘汰、绝不改动最终判定。
    let (_, rep) = get(&state, "/api/arena/w/report", "host").await;
    let elim: Vec<String> = serde_json::from_value(rep["match"]["eliminations"].clone()).unwrap();
    assert_eq!(elim, vec!["c1".to_string()], "买资格不得撤销淘汰（不免死）");
    assert_eq!(rep["match"]["winnerCharId"], "c2", "买资格不得改动最终判定（不买结果）");

    // 非参赛角色不可购买资格。
    let (s, _) = post(&state, "/api/arena/w/revive-match", "viewer", json!({ "cloudCharacterId": "ghost" })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn red_line_no_immunity_or_buy_verdict_endpoints() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_arena_world(&state.db, "w", "host", "official").await;

    // 不存在任何「免死」或「买胜负」端点：这些路径不应被路由（404），杜绝买结果/免死。
    for path in ["/api/arena/w/immunity", "/api/arena/w/exempt-death", "/api/arena/w/set-winner", "/api/arena/w/buy-verdict"] {
        let (s, _) = post(&state, path, "host", json!({})).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "不得存在免死/买最终判定端点：{path}");
    }
}
