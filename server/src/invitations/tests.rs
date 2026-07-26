//! 房间邀请集成测试（sqlite::memory + oneshot）。
//!
//! 守护三条硬约束（见 `mod.rs` 顶部）：
//! ① 邀请不是特权通道——accepted 不写成员表，入场仍走 join 的全部校验（红线组）；
//! ② 社交防火墙——响应体永不出现真人身份（§14）；
//! ③ 未验证功能默认关闭——开关默认 off，端点不可用且历史邀请读取侧降级（VALIDATION.md §0.1）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::app::{build_router, AppState};
use crate::db::now_ms;
use crate::invitations::{invitations_enabled, InvitationSwitch, ENV_INVITATIONS_ENABLED};
use crate::safety::testkit::{seed_member, test_state, token};
use crate::worlds::{
    create_world, CreateWorldParams, DeathmatchSwitch, LETHALITY_CONSENT, LETHALITY_DEATHMATCH,
};

use muse_engine::character::types::{CardLifecycle, CharacterCardV2, Identity};

// ---------------- 脚手架 ----------------

/// 造用户并指定年龄声明（0 未声明 / 1 成年 / 2 未成年）。
/// 未声明与未成年在生死状门前**一视同仁**（fail-closed，口径同 join）。
async fn seed_user_age(state: &AppState, id: &str, age: i64) {
    sqlx::query(
        "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
         VALUES (?, '', ?, 'active', ?, ?)",
    )
    .bind(id)
    .bind(age)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

fn card_json(id: &str, name: &str) -> String {
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

/// 已过审、未撤回的云角色卡。
async fn seed_char(state: &AppState, id: &str, owner: &str, name: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES (?, ?, 'local', 1, ?, 'original', 'approved', 0, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(card_json(id, name))
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 建一个指定契约档的世界（落库值 = 建房意图；是否生效由运营开关裁定）。
async fn seed_world(state: &AppState, title: &str, lethality: &str) -> String {
    let mut p = CreateWorldParams::official("tpl", 1, title);
    p.lethality = lethality.into();
    create_world(&state.db, p).await.unwrap()
}

async fn post(app: &axum::Router, uri: &str, tk: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tk}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn get(app: &axum::Router, uri: &str, tk: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tk}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn member_count(state: &AppState, world_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM world_members WHERE world_id = ? AND status = 'active'",
    )
    .bind(world_id)
    .fetch_one(&state.db)
    .await
    .unwrap()
}

async fn invitation_count(state: &AppState) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM room_invitations")
        .fetch_one(&state.db)
        .await
        .unwrap()
}

/// 标准场景：房主 usrHost（成年，已投放 chHost）在世界里，被邀请者 usrGuest 持 chGuest（未入场）。
/// 返回 (state, app, world_id)。
async fn scene(lethality: &str, guest_age: i64) -> (AppState, axum::Router, String) {
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user_age(&state, "usrHost", 1).await;
    seed_user_age(&state, "usrGuest", guest_age).await;
    seed_char(&state, "chHost", "usrHost", "墨白").await;
    seed_char(&state, "chGuest", "usrGuest", "青禾").await;
    let wid = seed_world(&state, "星火酒馆", lethality).await;
    seed_member(&state.db, "wm-host", &wid, "usrHost", "chHost", "active").await;
    (state, app, wid)
}

// ==================== ③ 未验证功能默认关闭（VALIDATION.md §0.1）====================

#[test]
fn switch_defaults_to_off() {
    let _sw = InvitationSwitch::set(false);
    std::env::remove_var(ENV_INVITATIONS_ENABLED);
    assert!(!invitations_enabled(), "房间邀请必须默认关闭（未验证功能默认关闭）");
    std::env::set_var(ENV_INVITATIONS_ENABLED, "maybe");
    assert!(!invitations_enabled(), "非法开关值须回落关闭，不得静默开启社交通道");
    std::env::set_var(ENV_INVITATIONS_ENABLED, "on");
    assert!(invitations_enabled(), "显式 on 应开启");
}

#[tokio::test]
async fn endpoints_unavailable_when_switch_off() {
    let _sw = InvitationSwitch::set(false);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let (st, _) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &host,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "开关关闭时发出邀请端点须不可用");

    let (st, _) = get(&app, &format!("/api/worlds/{wid}/invitations"), &host).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "发件箱须不可用");
    let (st, _) = get(&app, "/api/me/invitations", &guest).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "收件箱须不可用");
    let (st, _) =
        post(&app, "/api/me/invitations/inv_x/respond", &guest, json!({ "accept": true })).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "响应端点须不可用");
    assert_eq!(invitation_count(&state).await, 0, "关闭时不得落任何邀请行");
}

#[tokio::test]
async fn switch_off_degrades_existing_invitations_on_read_side() {
    // 读取侧降级（双保险）：开关是可逆急停阀——关掉之后，**已存在的**邀请也立即读不出、响应不了。
    let state = test_state().await;
    let app = build_router(state.clone());
    seed_user_age(&state, "usrHost", 1).await;
    seed_user_age(&state, "usrGuest", 1).await;
    seed_char(&state, "chHost", "usrHost", "墨白").await;
    seed_char(&state, "chGuest", "usrGuest", "青禾").await;
    let wid = seed_world(&state, "星火酒馆", LETHALITY_CONSENT).await;
    seed_member(&state.db, "wm-host", &wid, "usrHost", "chHost", "active").await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let iid = {
        let _sw = InvitationSwitch::set(true);
        let (st, v) = post(
            &app,
            &format!("/api/worlds/{wid}/invitations"),
            &host,
            json!({ "targetCharacterId": "chGuest" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        v["id"].as_str().unwrap().to_string()
    };

    let _sw = InvitationSwitch::set(false);
    let (st, _) = get(&app, "/api/me/invitations", &guest).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "关掉开关后历史邀请须读不出");
    let (st, _) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": true }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "关掉开关后历史邀请须响应不了");
}

// ==================== 主流程：发出 / 接受 / 拒绝 ====================

#[tokio::test]
async fn invite_accept_flow() {
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let (st, inv) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &host,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{inv}");
    assert_eq!(inv["status"], "pending");
    assert_eq!(inv["worldTitle"], "星火酒馆");
    assert_eq!(inv["inviteeCharacterName"], "青禾", "只以角色面具示人");
    let iid = inv["id"].as_str().unwrap().to_string();

    // 收件箱可见，且带邀请人的**角色面具名**（前端「房主：墨白」）。
    let (st, inbox) = get(&app, "/api/me/invitations", &guest).await;
    assert_eq!(st, StatusCode::OK, "{inbox}");
    let items = inbox["invitations"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], iid.as_str());
    assert_eq!(items[0]["inviterCharacterName"], "墨白");
    assert_eq!(items[0]["worldTitle"], "星火酒馆");

    // 发件箱只出自己发的。
    let (st, sent) = get(&app, &format!("/api/worlds/{wid}/invitations"), &host).await;
    assert_eq!(st, StatusCode::OK, "{sent}");
    assert_eq!(sent["invitations"].as_array().unwrap().len(), 1);

    // 接受 → accepted + next 指引（明示还要自行 join）。
    let (st, r) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": true }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["status"], "accepted");
    assert_eq!(r["next"]["path"], format!("/api/worlds/{wid}/join"));

    // 幂等重放：再响应一次不改终局。
    let (st, r2) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": false }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r2}");
    assert_eq!(r2["status"], "accepted", "已解决的邀请不得被二次改写");

    // 接受后收件箱 pending 清空（默认过滤 pending）。
    let (_st, inbox2) = get(&app, "/api/me/invitations", &guest).await;
    assert_eq!(inbox2["invitations"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn decline_is_final_and_blocks_reinvite() {
    // 防骚扰：被邀请者可拒绝，且拒绝即终局——同一邀请人不得再把同一角色请进同一世界。
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");
    let uri = format!("/api/worlds/{wid}/invitations");

    let (_st, inv) = post(&app, &uri, &host, json!({ "targetCharacterId": "chGuest" })).await;
    let iid = inv["id"].as_str().unwrap().to_string();

    let (st, r) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": false }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["status"], "declined");

    let (st, again) = post(&app, &uri, &host, json!({ "targetCharacterId": "chGuest" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "被拒后不得再次邀请: {again}");
    assert_eq!(invitation_count(&state).await, 1, "被拒后重邀不得落新行");
}

#[tokio::test]
async fn duplicate_invite_is_idempotent() {
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    let host = token(&state, "usrHost");
    let uri = format!("/api/worlds/{wid}/invitations");

    let (st1, a) = post(&app, &uri, &host, json!({ "targetCharacterId": "chGuest" })).await;
    let (st2, b) = post(&app, &uri, &host, json!({ "targetCharacterId": "chGuest" })).await;
    assert_eq!(st1, StatusCode::OK, "{a}");
    assert_eq!(st2, StatusCode::OK, "{b}");
    assert_eq!(a["id"], b["id"], "重复邀请须复用同一条 pending");
    assert_eq!(invitation_count(&state).await, 1, "不得落第二行");
    // 通知也只发一条（去重键含邀请 id）→ 无法靠反复调用制造通知轰炸。
    let n = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM notification_outbox WHERE kind = 'room_invitation'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(n, 1, "重复邀请不得重复通知");
}

#[tokio::test]
async fn only_invitee_can_respond() {
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    seed_user_age(&state, "usrThird", 1).await;
    let host = token(&state, "usrHost");
    let third = token(&state, "usrThird");

    let (_st, inv) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &host,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();

    // 第三人响应他人邀请 → 404（既挡越权，也不泄露该邀请存在）。
    let (st, _) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &third,
        json!({ "accept": true }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "非收件人不得接受他人邀请");
    // 邀请人自己也不能替对方接受。
    let (st, _) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &host,
        json!({ "accept": true }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "邀请人不得代收件人接受");

    let status: String =
        sqlx::query_scalar("SELECT status FROM room_invitations WHERE id = ?")
            .bind(&iid)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(status, "pending", "越权响应须零副作用");
}

#[tokio::test]
async fn non_member_cannot_invite() {
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    seed_user_age(&state, "usrStranger", 1).await;
    let stranger = token(&state, "usrStranger");
    let (st, v) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &stranger,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "路人不得拿别人的房当骚扰通道: {v}");
}

#[tokio::test]
async fn daily_limit_caps_invitations() {
    // 防骚扰：每人每日发出总量上限（跨世界合计），参数化 env 可调。
    let _sw = InvitationSwitch::with(true, &[("MUSE_INVITE_DAILY_LIMIT", "1")]);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    seed_user_age(&state, "usrGuest2", 1).await;
    seed_char(&state, "chGuest2", "usrGuest2", "白露").await;
    let host = token(&state, "usrHost");
    let uri = format!("/api/worlds/{wid}/invitations");

    let (st, _) = post(&app, &uri, &host, json!({ "targetCharacterId": "chGuest" })).await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = post(&app, &uri, &host, json!({ "targetCharacterId": "chGuest2" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "超出日配额须被拒: {v}");
    assert_eq!(invitation_count(&state).await, 1);
}

// ==================== ② 社交防火墙（§14 恨隔面具原则）====================

#[tokio::test]
async fn responses_never_leak_real_identity() {
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let (_st, inv) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &host,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    let (_st, inbox) = get(&app, "/api/me/invitations", &guest).await;
    let (_st, sent) = get(&app, &format!("/api/worlds/{wid}/invitations"), &host).await;

    for (name, body) in [("create", &inv), ("inbox", &inbox), ("sent", &sent)] {
        let text = body.to_string();
        assert!(!text.contains("usrGuest"), "{name} 响应泄露了被邀请者的真人 id: {text}");
        assert!(!text.contains("usrHost"), "{name} 响应泄露了邀请人的真人 id: {text}");
        assert!(!text.contains("inviterUserId"), "{name} 不得下发 inviterUserId: {text}");
        assert!(!text.contains("inviteeUserId"), "{name} 不得下发 inviteeUserId: {text}");
        assert!(!text.contains("phone"), "{name} 不得下发手机号字段: {text}");
    }

    // 通知 payload 同样只含世界维度与角色面具。
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM notification_outbox WHERE kind = 'room_invitation'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert!(!payload.contains("usrHost"), "通知 payload 泄露真人 id: {payload}");
    assert!(payload.contains("墨白"), "通知应带邀请人的角色面具名: {payload}");
}

// ==================== 🔴 ① 邀请不是特权通道：接受后仍走完整 join 校验 ====================

#[test]
fn module_never_writes_world_members() {
    // 结构性红线：邀请模块**没有**任何写成员表的语句 —— "接受邀请仍走完整 join 校验"
    // 不靠人工纪律，靠源码里根本不存在旁路写入。
    // needle 由片段拼出，避免断言字符串自身命中被扫描的源码。
    let src = include_str!("mod.rs");
    let table = "world_members";
    for verb in ["INSERT INTO", "UPDATE", "DELETE FROM"] {
        let needle = format!("{verb} {table}");
        assert!(
            !src.contains(&needle),
            "invitations 模块出现了对成员表的写入（`{needle}`）—— 邀请一旦能直接入场，\
             同源唯一/防自刷/星级准入/生死契约/未成年门就全被绕过"
        );
    }
    // 反向确认扫描确实作用在真源码上（避免 include_str! 拿到空内容导致断言空转）。
    assert!(src.contains("room_invitations"), "源码扫描目标不正确");
}

#[tokio::test]
async fn accept_does_not_enter_world() {
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");
    let before = member_count(&state, &wid).await;

    let (_st, inv) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &host,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    let (st, r) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": true }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(member_count(&state, &wid).await, before, "接受邀请绝不得直接落成员行");

    // 真正入场仍要自己调 join —— 调完才有成员行。
    let (st, j) = post(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &guest,
        json!({ "cloudCharacterId": "chGuest" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(member_count(&state, &wid).await, before + 1);
}

#[tokio::test]
async fn accepted_invitation_still_blocks_minor_from_deathmatch_join() {
    // 🔴 真红线 §0.4：未成年被邀请进生死状世界，**接受了邀请也仍然进不去**。
    // 构造：邀请与接受发生在运营开关关闭时（生死状降级为同意制，邀请合法）；
    // 随后运营打开生死状开关 —— 手里攥着 accepted 邀请的未成年去 join，照样被 403 挡死。
    // 加锁顺序固定：先 InvitationSwitch，再 DeathmatchSwitch（见 InvitationSwitch 文档）。
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_DEATHMATCH, 0).await; // guest 年龄未声明 = 按未成年处理
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let iid = {
        let _dm = DeathmatchSwitch::set(false); // 生效档降级为同意制 → 邀请前门放行
        let (st, inv) = post(
            &app,
            &format!("/api/worlds/{wid}/invitations"),
            &host,
            json!({ "targetCharacterId": "chGuest" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{inv}");
        let iid = inv["id"].as_str().unwrap().to_string();
        let (st, r) = post(
            &app,
            &format!("/api/me/invitations/{iid}/respond"),
            &guest,
            json!({ "accept": true }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        assert_eq!(r["status"], "accepted");
        iid
    };

    // 运营打开生死状档 → join 的未成年红线门生效。
    let _dm = DeathmatchSwitch::set(true);
    let (st, j) = post(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &guest,
        json!({ "cloudCharacterId": "chGuest", "acceptDeathContract": true }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "未成年持 accepted 邀请仍须被生死状门拒绝: {j}");
    assert_eq!(member_count(&state, &wid).await, 1, "被拒即零副作用（只剩房主一行）");

    // 邀请本身仍是 accepted —— 邀请状态与入场资格是两件事，邀请永远不代表通行证。
    let status: String = sqlx::query_scalar("SELECT status FROM room_invitations WHERE id = ?")
        .bind(&iid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(status, "accepted");
}

#[tokio::test]
async fn accepted_invitation_still_requires_death_contract_signature() {
    // 🔴 成年人拿着 accepted 邀请去生死状世界，仍必须二次签署生死状（join 的契约门未被绕过）。
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_DEATHMATCH, 1).await; // guest 已声明成年
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let iid = {
        let _dm = DeathmatchSwitch::set(false);
        let (_st, inv) = post(
            &app,
            &format!("/api/worlds/{wid}/invitations"),
            &host,
            json!({ "targetCharacterId": "chGuest" }),
        )
        .await;
        inv["id"].as_str().unwrap().to_string()
    };
    {
        let _dm = DeathmatchSwitch::set(false);
        let (_st, r) = post(
            &app,
            &format!("/api/me/invitations/{iid}/respond"),
            &guest,
            json!({ "accept": true }),
        )
        .await;
        assert_eq!(r["status"], "accepted");
    }

    let _dm = DeathmatchSwitch::set(true);
    // 缺二次确认 → 409（accepted 邀请不构成代签）。
    let (st, j) = post(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &guest,
        json!({ "cloudCharacterId": "chGuest" }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "accepted 邀请不得替玩家签生死状: {j}");
    // 带上确认 → 才进得去。
    let (st, j2) = post(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &guest,
        json!({ "cloudCharacterId": "chGuest", "acceptDeathContract": true }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{j2}");
}

#[tokio::test]
async fn accepted_invitation_still_subject_to_one_card_per_user() {
    // 🔴 防自刷（同一世界每位用户仅一张 active 卡）在邀请路径下同样生效。
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_CONSENT, 1).await;
    seed_char(&state, "chGuestB", "usrGuest", "青禾之影").await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let (_st, inv) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &host,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    let (_st, _r) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": true }),
    )
    .await;

    let (st, _) = post(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &guest,
        json!({ "cloudCharacterId": "chGuest" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // 第二张卡 → 仍被防自刷拦下（邀请没有给任何额外名额）。
    let (st, j) = post(
        &app,
        &format!("/api/worlds/{wid}/join"),
        &guest,
        json!({ "cloudCharacterId": "chGuestB" }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "邀请不得绕过一人一卡防自刷: {j}");
    assert_eq!(member_count(&state, &wid).await, 2, "房主 + 客人各一张");
}

// ==================== 🔴 未成年保护：前门拒绝 + 读取侧复查 ====================

#[tokio::test]
async fn minor_cannot_be_invited_to_deathmatch_world() {
    // 前门：生效档为生死状时，未声明成年者根本收不到邀请（不制造诱导入口）。
    let _sw = InvitationSwitch::set(true);
    let _dm = DeathmatchSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_DEATHMATCH, 2).await; // 明确未成年
    let host = token(&state, "usrHost");

    let (st, v) = post(
        &app,
        &format!("/api/worlds/{wid}/invitations"),
        &host,
        json!({ "targetCharacterId": "chGuest" }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "未成年不得被邀请进生死状世界: {v}");
    // 拒绝文案不得暴露"因为对方未成年"——否则端点变成年龄探测器。
    let msg = v["error"]["message"].as_str().unwrap_or("");
    assert!(!msg.contains("未成年"), "拒绝文案不得泄露被邀请者的年龄声明: {msg}");
    assert_eq!(invitation_count(&state).await, 0, "被拒即零副作用");
}

#[tokio::test]
async fn accept_rechecks_age_gate_when_deathmatch_switch_flips_on() {
    // 读取侧复查（双保险）：邀请建立于开关关闭期；运营打开后，未成年**接受**这一步就先被挡住。
    let _sw = InvitationSwitch::set(true);
    let (state, app, wid) = scene(LETHALITY_DEATHMATCH, 0).await;
    let host = token(&state, "usrHost");
    let guest = token(&state, "usrGuest");

    let iid = {
        let _dm = DeathmatchSwitch::set(false);
        let (st, inv) = post(
            &app,
            &format!("/api/worlds/{wid}/invitations"),
            &host,
            json!({ "targetCharacterId": "chGuest" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{inv}");
        inv["id"].as_str().unwrap().to_string()
    };

    let _dm = DeathmatchSwitch::set(true);
    let (st, r) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": true }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "开关打开后未成年不得接受生死状世界的邀请: {r}");
    let status: String = sqlx::query_scalar("SELECT status FROM room_invitations WHERE id = ?")
        .bind(&iid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(status, "pending", "被拒的接受不得改写邀请状态");

    // 但拒绝这条路始终畅通（被邀请者永远有退出通道）。
    let (st, r2) = post(
        &app,
        &format!("/api/me/invitations/{iid}/respond"),
        &guest,
        json!({ "accept": false }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r2}");
    assert_eq!(r2["status"], "declined");
}
