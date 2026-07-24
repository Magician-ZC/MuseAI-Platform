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

// ---------- 赛制事件进流：淘汰/胜者作为 public world_event（双硬隔离不泄私密） ----------

#[tokio::test]
async fn settle_emits_elim_and_winner_public_events() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_arena_world(&state.db, "w", "host", "official").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;

    // 淘汰 c1 → 当事人 u1 同意 → 结算落定 → 收敛胜者 c2。
    let (_, v) = post(&state, "/api/arena/w/eliminate", "host", json!({ "cloudCharacterId": "c1" })).await;
    let cid = v["consentId"].as_str().unwrap().to_string();
    respond_consent(&state, "u1", "w", &cid, true).await;
    let (_, settled) = post(&state, "/api/arena/w/settle", "host", json!({})).await;
    assert_eq!(settled["winnerCharId"], "c2");

    // 淘汰/胜者各落一行 public world_event，audience_json IS NULL（双硬隔离天然满足）。
    let elim = count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w' AND event_type='arena_elim' AND visibility='public' AND audience_json IS NULL").await;
    assert_eq!(elim, 1, "淘汰应作为 public 系统事件进流");
    let win = count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w' AND event_type='arena_winner' AND visibility='public' AND audience_json IS NULL").await;
    assert_eq!(win, 1, "胜者应作为 public 系统事件进流");
    // 红线：系统事件不携带任何私有投影。
    let leaked = count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w' AND event_type IN ('arena_elim','arena_winner') AND private_projections_json IS NOT NULL").await;
    assert_eq!(leaked, 0, "赛制系统事件不得含私有投影");

    // 观众经回放能看到淘汰/胜者（public 放行），且携带 characterId。
    let (s, rep) = get(&state, "/api/arena/w/replay", "host").await;
    assert_eq!(s, StatusCode::OK, "body={rep}");
    let evs = rep["events"].as_array().unwrap();
    let elim_ev = evs.iter().find(|e| e["type"] == "arena_elim").expect("回放含淘汰事件");
    assert_eq!(elim_ev["characterId"], "c1");
    assert_eq!(elim_ev["arenaKind"], "elim");
    let win_ev = evs.iter().find(|e| e["type"] == "arena_winner").expect("回放含胜者事件");
    assert_eq!(win_ev["characterId"], "c2");
}

// ---------- 回放端点：从 world_events 重建 public 时间线（seekable，private 不泄） ----------

#[tokio::test]
async fn replay_returns_public_timeline_seekable() {
    let state = test_state().await;
    seed_user(&state.db, "spectator").await;
    seed_arena_world(&state.db, "w", "host", "public").await;

    // 3 条 public（seq 0/1/2）+ 1 条 private（seq 3，含机密摘要）。
    seed_public_event(&state.db, "w", 0, 0, "action", "李上前行礼", None).await;
    seed_public_event(&state.db, "w", 0, 1, "dialogue", "寒暄致意", None).await;
    seed_public_event(&state.db, "w", 1, 2, "action", "王拂袖而去", None).await;
    seed_private_event(&state.db, "w", 3, "机密：王暗藏毒计").await;

    // 首页 limit=2 → 前两条 public，按 sequence 升序，nextCursor=1。
    let (s, p1) = get(&state, "/api/arena/w/replay?limit=2", "spectator").await;
    assert_eq!(s, StatusCode::OK, "body={p1}");
    let evs = p1["events"].as_array().unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0]["sequence"], 0);
    assert_eq!(evs[1]["sequence"], 1);
    assert_eq!(evs[0]["summary"], "李上前行礼");
    assert_eq!(p1["nextCursor"], 1);

    // seek：cursor=1 → 只回 public seq 2（private seq 3 被 visibility 过滤，不进回放）。
    let (_, p2) = get(&state, "/api/arena/w/replay?cursor=1&limit=2", "spectator").await;
    let evs2 = p2["events"].as_array().unwrap();
    assert_eq!(evs2.len(), 1, "剩余仅一条 public；私有事件不进回放");
    assert_eq!(evs2[0]["sequence"], 2);

    // 私有摘要绝不泄露到任一页。
    assert!(!p1.to_string().contains("机密"), "回放不得含私有投影");
    assert!(!p2.to_string().contains("机密"), "回放不得含私有投影");
}

#[tokio::test]
async fn replay_forbidden_for_private_world_non_member() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "stranger").await;
    // private 世界：观战不开放，仅成员/房主可回放（复用 can_view_world 语义）。
    seed_arena_world(&state.db, "w", "host", "private").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;

    let (s1, _) = get(&state, "/api/arena/w/replay", "u1").await;
    assert_eq!(s1, StatusCode::OK, "成员可回放");
    let (s2, _) = get(&state, "/api/arena/w/replay", "stranger").await;
    assert_eq!(s2, StatusCode::FORBIDDEN, "private 世界非成员不得回放");
    let (s3, _) = get(&state, "/api/arena/w/replay", "host").await;
    assert_eq!(s3, StatusCode::OK, "房主可回放");
}

// ---------- 红线：站内打赏只写系统频道，永不触碰 eliminations/winner/interventions ----------

#[tokio::test]
async fn gift_does_not_touch_eliminations_or_winner() {
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_user(&state.db, "viewer").await;
    seed_arena_world(&state.db, "w", "host", "public").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;

    // 建 match（host/tick → running，eliminations '[]'，winner NULL）。
    post(&state, "/api/arena/w/host/tick", "host", json!({})).await;
    let elim_before =
        sqlx::query_scalar::<_, String>("SELECT eliminations_json FROM arena_matches WHERE world_id='w'").fetch_one(&state.db).await.unwrap();
    let winner_before: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT winner_char_id FROM arena_matches WHERE world_id='w'").fetch_one(&state.db).await.unwrap();

    // 观众打赏 rose ×2（命中映射）。
    let (s, g) = post(&state, "/api/arena/w/gift", "viewer", json!({ "sku": "rose", "count": 2 })).await;
    assert_eq!(s, StatusCode::OK, "body={g}");
    assert_eq!(g["mapped"], true);
    assert_eq!(g["boundary"]["buys"], "process_boon");
    assert_eq!(g["boundary"]["notImmunity"], true);
    assert_eq!(g["boundary"]["notFinalVerdict"], true);

    // 红线：eliminations / winner 一字不改。
    let elim_after =
        sqlx::query_scalar::<_, String>("SELECT eliminations_json FROM arena_matches WHERE world_id='w'").fetch_one(&state.db).await.unwrap();
    let winner_after: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT winner_char_id FROM arena_matches WHERE world_id='w'").fetch_one(&state.db).await.unwrap();
    assert_eq!(elim_before, elim_after, "打赏不得改动淘汰");
    assert_eq!(winner_before, winner_after, "打赏不得改动胜者");
    assert!(winner_after.is_none(), "无淘汰仍无胜者");

    // 系统频道确实写入：arena_env_events(gift_boon) + gift_events(via=in_app) + arena_gift 进 public 流。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_env_events WHERE world_id='w' AND kind='gift_boon'").await, 1);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM gift_events WHERE world_id='w' AND via='in_app'").await, 1);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM world_events WHERE world_id='w' AND event_type='arena_gift' AND visibility='public'").await, 1);
    // 红线：打赏绝不进玩家 interventions 通道。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM interventions WHERE world_id='w'").await, 0);
}

// ---------- P2 复活扣费：平台服务不分成 + 红线（买过程不买结果）+ 余额不足拒付 ----------

/// 造带复活定价的世界模板（owner=Some → 创作者模板 official=0；None → 官方模板）。复活不分成，owner 仅用于对照断言。
async fn seed_template_revive(db: &AnyPool, id: &str, owner: Option<&str>, revive_price: i64) {
    let official = if owner.is_some() { 0 } else { 1 };
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, version, moderation, owner_id, revive_price_cents, created_at) \
         VALUES (?, 't', 'arena', '{}', '{\"mode\":\"open\"}', ?, 1, 'approved', ?, ?, ?)",
    )
    .bind(id)
    .bind(official)
    .bind(owner)
    .bind(revive_price)
    .bind(now_ms())
    .execute(db)
    .await
    .expect("seed template revive");
}

/// running 赛事世界（指向指定模板，携 host + visibility）。
async fn seed_arena_world_tpl(db: &AnyPool, id: &str, host: &str, visibility: &str, template_id: &str) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, host_user_id, member_limit, tick_per_day, \
         state_revision, narrative_state_json, created_at, updated_at) \
         VALUES (?, ?, 1, 'e1', 'p1', 'm1', 'arena', '赛事世界', 'running', ?, ?, 10, 3, 0, '{}', ?, ?)",
    )
    .bind(id)
    .bind(template_id)
    .bind(visibility)
    .bind(host)
    .bind(now_ms())
    .bind(now_ms())
    .execute(db)
    .await
    .expect("seed arena world tpl");
}

/// 充值钱包（镜像 billing 双写）：post_journal + billing_balances 物化，保证 user_wallet == billing_balances。
async fn fund_wallet(db: &AnyPool, uid: &str, amount: i64) {
    let mut tx = db.begin().await.unwrap();
    crate::ledger::post_journal(
        &mut tx,
        "recharge",
        "order",
        "seed",
        None,
        &[
            crate::ledger::Posting {
                account: crate::ledger::AccountRef::UserWallet(uid.to_string()),
                delta_cents: amount,
            },
            crate::ledger::Posting {
                account: crate::ledger::AccountRef::PlatformRechargeSource,
                delta_cents: -amount,
            },
        ],
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO billing_balances (user_id, balance_cents, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET balance_cents = billing_balances.balance_cents + excluded.balance_cents, updated_at = excluded.updated_at",
    )
    .bind(uid)
    .bind(amount)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn acct_balance(db: &AnyPool, account_id: &str) -> i64 {
    let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM ledger_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .unwrap();
    row.map(|(b,)| b).unwrap_or(0)
}

async fn billing_balance(db: &AnyPool, uid: &str) -> i64 {
    let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM billing_balances WHERE user_id = ?")
        .bind(uid)
        .fetch_optional(db)
        .await
        .unwrap();
    row.map(|(b,)| b).unwrap_or(0)
}

/// 红线不变量：每 journal SUM(postings)==0（有借必有贷）。返回不平衡 journal 数（应为 0）。
async fn unbalanced_journals(db: &AnyPool) -> i64 {
    count(
        db,
        "SELECT COUNT(*) FROM (SELECT journal_id FROM ledger_postings GROUP BY journal_id HAVING SUM(delta_cents) <> 0) t",
    )
    .await
}

#[tokio::test]
async fn revive_charge_deducts_all_platform_no_share_and_preserves_verdict() {
    // 复活扣费：观众为已淘汰角色买复活「资格」→ 扣钱包，**全额入平台不分成**（平台服务，charge world_id=None）；
    // 红线：charge 成功 ≠ 免死——绝不撤销既有淘汰、绝不改最终判定（买过程不买结果）。
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "creator").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_user(&state.db, "viewer").await;
    seed_template_revive(&state.db, "tpl_c", Some("creator"), 300).await; // 创作者模板，复活价 300
    seed_arena_world_tpl(&state.db, "w", "host", "official", "tpl_c").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;
    fund_wallet(&state.db, "viewer", 1000).await;

    // 先经同意门控落定淘汰 c1 → 唯一胜者 c2（既有仲裁结果）。
    let (_, v) = post(&state, "/api/arena/w/eliminate", "host", json!({ "cloudCharacterId": "c1" })).await;
    let cid = v["consentId"].as_str().unwrap().to_string();
    respond_consent(&state, "u1", "w", &cid, true).await;
    let (_, settled) = post(&state, "/api/arena/w/settle", "host", json!({})).await;
    assert_eq!(settled["winnerCharId"], "c2");

    // 观众为被淘汰角色 c1 付费买复活赛资格。
    let (s, rv) = post(&state, "/api/arena/w/revive-match", "viewer", json!({ "cloudCharacterId": "c1" })).await;
    assert_eq!(s, StatusCode::OK, "body={rv}");
    assert_eq!(rv["status"], "eligible");
    // 付费边界（诚实标注）：买资格，非免死、不改最终判定。
    assert_eq!(rv["boundary"]["buys"], "revive_eligibility");
    assert_eq!(rv["boundary"]["notImmunity"], true);
    assert_eq!(rv["boundary"]["notFinalVerdict"], true);

    // 扣费：viewer 1000 − 300 = 700；user_wallet == billing_balances 恒等。
    assert_eq!(billing_balance(&state.db, "viewer").await, 700);
    assert_eq!(acct_balance(&state.db, "acct_wallet_viewer").await, 700);
    // 复活是平台服务：全额入平台，**绝不分成给创作者**（即便世界有创作者模板 owner）。
    assert_eq!(acct_balance(&state.db, "acct_platform_revenue").await, 300);
    assert_eq!(acct_balance(&state.db, "acct_creator_creator").await, 0, "复活扣费不得分成给创作者");
    assert_eq!(unbalanced_journals(&state.db).await, 0, "每 journal SUM(postings) 必须为 0");
    // journal reason=revive，ref_id == 复活凭证 id（审计链）。
    let rgid = rv["reviveGrantId"].as_str().unwrap().to_string();
    assert_eq!(
        count(&state.db, &format!("SELECT COUNT(*) FROM ledger_journals WHERE reason='revive' AND ref_id='{rgid}'")).await,
        1,
        "revive journal 应与 grant 通过 ref_id 对齐"
    );

    // 红线：买过程不买结果——charge 成功绝不撤销淘汰、绝不改最终判定。
    let (_, rep) = get(&state, "/api/arena/w/report", "host").await;
    let elim: Vec<String> = serde_json::from_value(rep["match"]["eliminations"].clone()).unwrap();
    assert_eq!(elim, vec!["c1".to_string()], "复活扣费不得撤销淘汰（不免死）");
    assert_eq!(rep["match"]["winnerCharId"], "c2", "复活扣费不得改判（买过程不买结果）");
    // 资格仍只记 eligible（非免死落定）。
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM arena_revive_grants WHERE world_id='w' AND status='eligible'").await,
        1
    );
}

#[tokio::test]
async fn revive_insufficient_balance_rejected_zero_side_effects() {
    // 余额不足拒付 → 409，且零副作用（无 grant、无 journal，钱包不动）。
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_user(&state.db, "poor").await;
    seed_template_revive(&state.db, "tpl_off", None, 1000).await; // 复活价 1000
    seed_arena_world_tpl(&state.db, "w", "host", "official", "tpl_off").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;
    // poor 未充值，钱包为 0 < 1000。

    let (s, _v) = post(&state, "/api/arena/w/revive-match", "poor", json!({ "cloudCharacterId": "c1" })).await;
    assert_eq!(s, StatusCode::CONFLICT, "余额不足应 409");

    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM arena_revive_grants WHERE world_id='w'").await,
        0,
        "余额不足不得写复活资格"
    );
    assert_eq!(
        count(&state.db, "SELECT COUNT(*) FROM ledger_journals WHERE reason='revive'").await,
        0,
        "余额不足不得产 revive journal"
    );
    assert_eq!(billing_balance(&state.db, "poor").await, 0, "钱包不动");
}

#[tokio::test]
async fn revive_free_when_price_zero_no_charge() {
    // 未定价（模板缺失/revive_price_cents=0）→ charge no-op：免费复活保留，不产 journal、钱包不动。
    let state = test_state().await;
    seed_user(&state.db, "host").await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_user(&state.db, "viewer").await;
    // seed_arena_world 用 template_id='tpl' 但不建模板行 → revive_price_cents 溯源为 0（免费）。
    seed_arena_world(&state.db, "w", "host", "official").await;
    seed_member(&state.db, "m1", "w", "u1", "c1", "active").await;
    seed_member(&state.db, "m2", "w", "u2", "c2", "active").await;

    let (s, rv) = post(&state, "/api/arena/w/revive-match", "viewer", json!({ "cloudCharacterId": "c1" })).await;
    assert_eq!(s, StatusCode::OK, "body={rv}");
    assert_eq!(rv["status"], "eligible");
    // 免费：无 journal、钱包 0。
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM ledger_journals").await, 0, "免费复活不产 journal");
    assert_eq!(billing_balance(&state.db, "viewer").await, 0);
    assert_eq!(count(&state.db, "SELECT COUNT(*) FROM arena_revive_grants WHERE world_id='w' AND status='eligible'").await, 1);
}
