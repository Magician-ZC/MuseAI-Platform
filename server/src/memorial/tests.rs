//! 传世卡 · 封卷与遗作馆测试（sqlite::memory + oneshot）。总规格 §12【拍板 23】。覆盖：
//! - 运营开关默认关闭：四端点 404 + **不发生封卷**（前门 + 状态侧双保险）；
//! - 死亡公共事实核验：无证据拒、只有授权没落定也拒（授权 ≠ 死亡）、两条齐备才封；
//! - 封卷幂等：重复调用不重复归还道具、不重复打印记（DB 状态 CAS）；
//! - 🔴 **传世卡不可再入世界**（红线，真实走 `POST /worlds/{id}/join`，不是模拟）；
//! - 道具归账户背包：`carried|sealed → owned` 且**背包总行数恒等**（不凭空造资产）；
//! - 🔴 「故人」印记**不进 `narrative_state_json`**（红线：那一列每 tick 回灌进引擎）；
//! - 🔴 遗作馆只读：`/memorial/*` 全 GET（路由白名单 + 运行时探测）；
//! - 转世：同内核开新卡不继承死者任何履历（新 id / 零历练 / 空传记 / 无印记）；
//! - 🔴 源码级红线：不改写世界线 · 不 INSERT 背包 · 印记不进引擎 · `withdrawn` 全仓单向。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::db::now_ms;
use crate::safety::testkit::{seed_member, seed_user, seed_world, test_state, token};

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

async fn seed_char(state: &AppState, id: &str, owner: &str, name: &str, mileage: i64) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at, mileage) \
         VALUES (?, ?, 'local', 1, ?, 'original', 'approved', 0, ?, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(json!({ "schemaVersion": 2, "identity": { "name": name } }).to_string())
    .bind(now_ms())
    .bind(mileage)
    .execute(&state.db)
    .await
    .expect("seed cloud_character");
}

/// 把叙事状态整块写进世界（**测试脚手架专用**：模拟引擎 tick 的产出。
/// 生产侧本模块永不写这一列——红线断言 `red_line_never_rewrites_worldline` 守死）。
async fn set_narrative_state(state: &AppState, world_id: &str, st: Value) {
    sqlx::query("UPDATE worlds SET narrative_state_json = ? WHERE id = ?")
        .bind(st.to_string())
        .bind(world_id)
        .execute(&state.db)
        .await
        .expect("set narrative_state_json");
}

async fn narrative_state_of(state: &AppState, world_id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT narrative_state_json FROM worlds WHERE id = ?")
        .bind(world_id)
        .fetch_one(&state.db)
        .await
        .expect("read narrative_state_json")
}

/// 播一条**已获批**的死亡同意（证据 a）。
async fn seed_death_consent(state: &AppState, world_id: &str, subjects: &[&str], status: &str) {
    sqlx::query(
        "INSERT INTO consent_requests (id, world_id, event_kind, subject_character_ids, detail, \
         status, responses_json, expires_at, created_at, resolved_at) \
         VALUES (?, ?, 'death', ?, '致命一击', ?, '{}', ?, ?, ?)",
    )
    .bind(new_id("cr"))
    .bind(world_id)
    .bind(serde_json::to_string(subjects).unwrap())
    .bind(status)
    .bind(now_ms() + 60_000)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed consent");
}

/// 叙事状态：关系 + （可选）尚未落定的 death pending。
fn narrative_with(relations: Value, pending_death_of: Option<&str>) -> Value {
    let pending: Vec<Value> = pending_death_of
        .map(|s| vec![json!({ "subject": s, "eventKind": "death" })])
        .unwrap_or_default();
    json!({
        "schemaVersion": 1,
        "runId": "run-1",
        "revision": 7,
        "relations": relations,
        "narrative": { "pendingConsents": pending },
    })
}

/// 播一件**已携带进某世界**的道具（走 items + backpacks 的真实形态）。
async fn seed_carried_item(state: &AppState, user: &str, item_id: &str, world_id: &str) {
    sqlx::query(
        "INSERT INTO items (id, narrative, effect_tags, origin_world_template_id, cosmology_json, \
         power_tier, created_at) VALUES (?, '一柄旧剑', '[\"sharp\"]', 'tpl', '[]', 2, ?)",
    )
    .bind(item_id)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed item");
    sqlx::query(
        "INSERT INTO backpacks (id, user_id, item_id, acquired_world_id, status, carried_world_id, \
         power_tier_override, effect_tags_override, acquired_at) \
         VALUES (?, ?, ?, ?, 'carried', ?, 1, '[\"dulled\"]', ?)",
    )
    .bind(new_id("bp"))
    .bind(user)
    .bind(item_id)
    .bind(world_id)
    .bind(world_id)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .expect("seed backpack");
}

async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(&state.db).await.expect("count")
}

async fn memorial_status_of(state: &AppState, char_id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT memorial_status FROM cloud_characters WHERE id = ?")
        .bind(char_id)
        .fetch_one(&state.db)
        .await
        .expect("memorial_status")
}

async fn withdrawn_of(state: &AppState, char_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT withdrawn FROM cloud_characters WHERE id = ?")
        .bind(char_id)
        .fetch_one(&state.db)
        .await
        .expect("withdrawn")
}

/// 一个「已死且死亡已落定」的最小世界：u1/chA 死于 w1，u2/chB 与其有羁绊。
/// 返回后即可直接 `POST /api/me/characters/chA/memorial`。
async fn seed_landed_death(state: &AppState) {
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "u2").await;
    seed_char(state, "chA", "u1", "裴照", 420).await;
    seed_char(state, "chB", "u2", "沈砚", 130).await;
    seed_world(&state.db, "w1", 1, "running").await;
    seed_member(&state.db, "wm1", "w1", "u1", "chA", "active").await;
    seed_member(&state.db, "wm2", "w1", "u2", "chB", "active").await;
    seed_death_consent(state, "w1", &["chA"], "approved").await;
    // pending 已清空 = 引擎已落定（证据 b）。
    set_narrative_state(
        state,
        "w1",
        narrative_with(json!([{ "from": "chB", "to": "chA", "trust": 0.8, "affinity": 0.6, "fear": 0.0, "debt": 0.2 }]), None),
    )
    .await;
}

// ==================== 运营开关（VALIDATION.md §0.1） ====================

/// 🔴 **默认关闭**：不设 env 时四个端点全 404，且**封卷不发生**（卡仍在世）。
/// 非法值同样回落关闭——配错不静默开启未验证的死亡机制。
#[tokio::test]
async fn switch_defaults_off_and_gates_all_endpoints() {
    let state = test_state().await;
    seed_landed_death(&state).await;

    {
        let _sw = MemorialSwitch::cleared();
        assert!(!memorial_enabled(), "传世卡默认必须关闭（死亡属 T5 才验证的范围）");

        for (method, path) in [
            ("GET", "/api/memorial/characters"),
            ("GET", "/api/memorial/characters/chA"),
            ("GET", "/api/me/memorial/marks"),
            ("POST", "/api/me/characters/chA/memorial"),
        ] {
            let (st, _) = send(&state, method, path, "u1", None, None).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "{method} {path} 开关关闭时必须 404（不泄露未开放功能）");
        }
        // 关键：不只是端点 404，**状态一点没动**。
        assert_eq!(memorial_status_of(&state, "chA").await, "living", "开关关闭时不得发生封卷");
        assert_eq!(withdrawn_of(&state, "chA").await, 0, "开关关闭时不得下架卡");
        assert_eq!(count(&state, "SELECT COUNT(*) FROM memorial_marks").await, 0);
    }

    {
        let _sw = MemorialSwitch::raw("maybe");
        assert!(!memorial_enabled(), "非法值必须回落默认（关闭），不得静默开启");
    }
}

// ==================== 死亡公共事实核验（服务端权威） ====================

/// 无任何死亡记录 → 409：封卷只承接真实发生过的死亡，绝不凭客户端一句话捏造。
#[tokio::test]
async fn seal_rejects_character_without_death_record() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_user(&state.db, "u1").await;
    seed_char(&state, "chA", "u1", "裴照", 10).await;

    let (st, body) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::CONFLICT, "没死过的卡不可封卷");
    assert!(
        body["error"]["message"].as_str().unwrap_or_default().contains("没有已落定的死亡记录"),
        "拒绝文案须说明原因：{body}"
    );
    assert_eq!(memorial_status_of(&state, "chA").await, "living");
}

/// **授权 ≠ 死亡**：同意已获批、但引擎还没跑到落定那一拍（pending 仍在）→ 拒绝封卷。
/// 这是 fail-closed 的关键一档：只看「已授权」会把活人误封卷，那是捏造死亡。
#[tokio::test]
async fn seal_rejects_when_consent_approved_but_death_not_landed() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    // 回退到「已授权但未落定」：pending 里还挂着 chA 的 death。
    set_narrative_state(&state, "w1", narrative_with(json!([]), Some("chA"))).await;

    let (st, _) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::CONFLICT, "同意已获批但死亡尚未落定时不得封卷");
    assert_eq!(memorial_status_of(&state, "chA").await, "living", "误封卷 = 捏造死亡");

    // 世界跑到落定那一拍（pending 清空）后，同一请求即可通过。
    set_narrative_state(&state, "w1", narrative_with(json!([]), None)).await;
    let (st, body) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK, "落定后应可封卷：{body}");
    assert_eq!(body["sealed"], json!(true));
}

/// 世界从未跑过任何一拍（`narrative_state_json = '{}'`）→ 保守拒绝：没跑过就不可能死过。
#[tokio::test]
async fn seal_rejects_when_world_never_ticked() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    set_narrative_state(&state, "w1", json!({})).await;

    let (st, _) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::CONFLICT, "空叙事状态查不到落定证据 → 保守拒绝");
}

/// 只能为**自己的**卡封卷（§9.6 服务端权威）。
#[tokio::test]
async fn seal_rejects_other_owner() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;

    let (st, _) = send(&state, "POST", "/api/me/characters/chA/memorial", "u2", None, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "别人的卡不可代为封卷");
    assert_eq!(memorial_status_of(&state, "chA").await, "living");

    let (st, _) = send(&state, "POST", "/api/me/characters/ghost/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "不存在的卡 → 404");
}

// ==================== 封卷：状态转换 + 幂等 ====================

/// 封卷落地：状态转 sealed、`withdrawn=1`（join 拦截点）、记下死于哪个世界。
/// **世界线一个字节不动**（§0.3 公共事实不可回滚）。
#[tokio::test]
async fn seal_flips_state_without_touching_worldline() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    let before_state = narrative_state_of(&state, "w1").await;
    let before_events = count(&state, "SELECT COUNT(*) FROM world_events").await;
    let before_members = count(&state, "SELECT COUNT(*) FROM world_members").await;
    let before_consents =
        count(&state, "SELECT COUNT(*) FROM consent_requests WHERE status = 'approved'").await;

    let (st, body) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["sealed"], json!(true));
    assert_eq!(body["memorialStatus"], json!("sealed"));
    assert_eq!(body["worldId"], json!("w1"));

    assert_eq!(memorial_status_of(&state, "chA").await, "sealed");
    assert_eq!(withdrawn_of(&state, "chA").await, 1, "封卷必须同时置 withdrawn=1：那是 join 的拦截点");

    // 🔴 公共事实不可回滚：世界线四处一字未改。
    assert_eq!(narrative_state_of(&state, "w1").await, before_state, "封卷不得改写 narrative_state_json");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM world_events").await, before_events);
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM world_members").await,
        before_members,
        "足迹是履历的一部分，死亡不删成员行"
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM consent_requests WHERE status = 'approved'").await,
        before_consents
    );
}

/// 🔴 **封卷幂等**：重复调用不重复归还道具、不重复打印记（DB 状态 CAS 是那道闸）。
#[tokio::test]
async fn seal_is_idempotent_and_never_double_grants() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    seed_carried_item(&state, "u1", "it_sword", "w1").await;

    let (st, first) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["sealed"], json!(true));
    assert_eq!(first["itemsReturned"], json!(1), "首次封卷归还 1 件道具");
    assert_eq!(first["marksGranted"], json!(1), "首次封卷打出 1 枚故人印记");

    let backpack_rows = count(&state, "SELECT COUNT(*) FROM backpacks").await;
    let mark_rows = count(&state, "SELECT COUNT(*) FROM memorial_marks").await;
    let sealed_at = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT memorial_sealed_at FROM cloud_characters WHERE id = 'chA'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();

    // 连续重复三次（含带 Idempotency-Key 与不带的混合路径）。
    for i in 0..3 {
        let key = if i == 0 { Some("k-seal") } else { None };
        let (st, again) =
            send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, key).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(again["sealed"], json!(false), "第 {i} 次重复封卷必须报告未发生状态转换");
        assert_eq!(again["itemsReturned"], json!(0), "重复封卷不得再归还道具");
        assert_eq!(again["marksGranted"], json!(0), "重复封卷不得再打印记");
    }

    assert_eq!(count(&state, "SELECT COUNT(*) FROM backpacks").await, backpack_rows, "背包行数不得增长");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM memorial_marks").await, mark_rows, "印记不得重复");
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT memorial_sealed_at FROM cloud_characters WHERE id = 'chA'"
        )
        .fetch_one(&state.db)
        .await
        .unwrap(),
        sealed_at,
        "封卷时刻是一次性的，重复调用不得刷新（否则传记落款会被改写）"
    );
}

/// 直接打 `seal_character_tx`（唯一写入路径）验证 CAS：第二次调用恒返回 `sealed=false`。
#[tokio::test]
async fn seal_tx_cas_is_the_idempotency_gate() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    seed_carried_item(&state, "u1", "it_sword", "w1").await;

    let mut tx = state.db.begin().await.unwrap();
    let first = seal_character_tx(&mut tx, "chA", "u1", "w1").await.unwrap();
    let second = seal_character_tx(&mut tx, "chA", "u1", "w1").await.unwrap();
    // 卡不属于该 owner → 同样 0 行命中（归属条件写在 CAS 的 WHERE 里）。
    let wrong_owner = seal_character_tx(&mut tx, "chB", "u1", "w1").await.unwrap();
    tx.commit().await.unwrap();

    assert!(first.sealed && first.items_returned == 1 && first.marks_granted == 1);
    assert!(!second.sealed && second.items_returned == 0 && second.marks_granted == 0);
    assert!(!wrong_owner.sealed, "非本人的卡 CAS 不命中");
    assert_eq!(memorial_status_of(&state, "chB").await, "living");
}

// ==================== 🔴 红线：传世卡不可再入世界 ====================

/// 🔴 **传世卡不可再入世界**（§12 原文）。真实走 `POST /api/worlds/{id}/join`，
/// 拦截点是 `worlds::join_world` 既有的 `withdrawn != 0` 门——封卷的双写让它天然生效，
/// **`worlds/mod.rs` 一行未改**。
///
/// 用例结构刻意是「同一张卡、同一个世界，封卷前能进、封卷后不能进」——
/// 这样断言的是**封卷造成的差异**，而不是某个世界恰好不可加入。
#[tokio::test]
async fn red_line_memorial_card_can_never_rejoin_a_world() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    seed_world(&state.db, "w2", 0, "open").await;

    // ① 封卷前：同一张卡可以投放进新世界（证明拒绝确实由封卷造成）。
    let (st, body) = send(
        &state,
        "POST",
        "/api/worlds/w2/join",
        "u1",
        Some(json!({ "cloudCharacterId": "chA" })),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "封卷前应可正常投放：{body}");
    // 复位成员行，让第二次 join 走同一条全新路径（而不是命中"已在场"分支）。
    sqlx::query("DELETE FROM world_members WHERE world_id = 'w2'")
        .execute(&state.db)
        .await
        .unwrap();

    // ② 封卷。
    let (st, _) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK);

    // ③ 封卷后：同一张卡再也进不去——任何世界、任何时候。
    for world in ["w1", "w2"] {
        let (st, body) = send(
            &state,
            "POST",
            &format!("/api/worlds/{world}/join"),
            "u1",
            Some(json!({ "cloudCharacterId": "chA" })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT, "传世卡不得再入世界 {world}：{body}");
        assert!(
            body["error"]["message"].as_str().unwrap_or_default().contains("character_withdrawn"),
            "拦截应命中 join 既有的 withdrawn 门（无需改动 worlds/mod.rs），got {body}"
        );
    }
}

/// 🔴 守住上面那道门的**前提**：`withdrawn` 在全仓是**单向**的。
/// 只要有任何一处把它置回 0，「传世卡不可再入世界」就出现复活侧门。
#[test]
fn red_line_withdrawn_is_one_way_across_the_repo() {
    // 覆盖所有会写 cloud_characters 的模块（grep 全仓核实过的写入点集合）。
    let sources: &[(&str, &str)] = &[
        ("memorial/mod.rs", include_str!("mod.rs")),
        ("assets/mod.rs", include_str!("../assets/mod.rs")),
        ("assets/worlds.rs", include_str!("../assets/worlds.rs")),
        ("admin_api/audit.rs", include_str!("../admin_api/audit.rs")),
        ("worlds/mod.rs", include_str!("../worlds/mod.rs")),
        ("progression/mod.rs", include_str!("../progression/mod.rs")),
        ("onboarding/mod.rs", include_str!("../onboarding/mod.rs")),
        ("backpack/mod.rs", include_str!("../backpack/mod.rs")),
    ];
    for (name, src) in sources {
        for banned in ["SET withdrawn = 0", "SET withdrawn=0", "withdrawn = 0 WHERE", "withdrawn=0 WHERE"] {
            assert!(
                !src.contains(banned),
                "{name} 出现 `{banned}`：withdrawn 必须是单向门，取消下架 = 传世卡复活侧门（§12「不是复活」）"
            );
        }
    }
}

// ==================== 道具归账户背包 ====================

/// 道具归账户背包：`carried|sealed → owned`，清 `carried_world_id` 与 S-5 降档覆盖。
/// 🔴 关键断言是**背包总行数恒等**——归还是解除携带，不是再发一次货
/// （道具本为账户资产；再发一次会让一次死亡把道具变成两件，违反 §0.2 资产守恒）。
/// 同时：**别的世界**、**别人**的携带道具一件不碰。
#[tokio::test]
async fn items_return_to_account_backpack_without_minting() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    seed_world(&state.db, "w2", 0, "open").await;

    seed_carried_item(&state, "u1", "it_sword", "w1").await; // 死者在 w1 携带 → 应归还
    seed_carried_item(&state, "u1", "it_ring", "w2").await; // 同一人在别的世界携带 → 不碰
    seed_carried_item(&state, "u2", "it_fan", "w1").await; // 同世界别人的道具 → 不碰
    // 封存档（admission=Sealed）同样应归还：人不在了，封存也该回账户。
    sqlx::query("UPDATE backpacks SET status = 'sealed' WHERE item_id = 'it_sword'")
        .execute(&state.db)
        .await
        .unwrap();

    let total_before = count(&state, "SELECT COUNT(*) FROM backpacks").await;

    let (st, body) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["itemsReturned"], json!(1));

    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM backpacks").await,
        total_before,
        "🔴 归还不得凭空多出背包行：道具本为账户资产，再发一次就是造资产"
    );
    let returned = count(
        &state,
        "SELECT COUNT(*) FROM backpacks WHERE item_id = 'it_sword' AND status = 'owned' \
         AND carried_world_id IS NULL AND power_tier_override IS NULL AND effect_tags_override IS NULL",
    )
    .await;
    assert_eq!(returned, 1, "死者的道具应回到账户背包（owned）并清掉世界内的降档覆盖");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM backpacks WHERE item_id = 'it_ring' AND status = 'carried'").await,
        1,
        "别的世界里携带的道具不受影响"
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM backpacks WHERE item_id = 'it_fan' AND status = 'carried'").await,
        1,
        "别人的道具一件不碰"
    );
}

// ==================== 「故人」印记 ====================

/// 🔴 **印记不进 `narrative_state_json`**（平权红线）：那一列每 tick 原样回灌进引擎
/// `RoundInput.state`，写进去就等于把结算侧记账喂给决策。印记必须落独立表。
#[tokio::test]
async fn red_line_departed_marks_never_touch_narrative_state() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    let before = narrative_state_of(&state, "w1").await;

    let (st, body) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["marksGranted"], json!(1));

    let after = narrative_state_of(&state, "w1").await;
    assert_eq!(after, before, "🔴 叙事状态必须逐字节不变（含 revision）——它每 tick 回灌进引擎");
    for banned in ["memorial", "departed", "故人", "mark"] {
        assert!(!after.contains(banned), "叙事状态不得出现 `{banned}` 的任何痕迹");
    }

    // 印记确实落在独立表里，且只有「谁记得谁、在哪、何时」——没有任何强度/加成字段。
    let row = sqlx::query(
        "SELECT character_id, owner_id, deceased_character_id, world_id, kind FROM memorial_marks",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(row.try_get::<String, _>("character_id").unwrap(), "chB");
    assert_eq!(row.try_get::<String, _>("owner_id").unwrap(), "u2");
    assert_eq!(row.try_get::<String, _>("deceased_character_id").unwrap(), "chA");
    assert_eq!(row.try_get::<String, _>("world_id").unwrap(), "w1");
    assert_eq!(row.try_get::<String, _>("kind").unwrap(), "departed");

    // 读取面：印记的主人能看到「我的角色记得谁」。
    let (st, marks) = send(&state, "GET", "/api/me/memorial/marks", "u2", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(marks["marks"].as_array().unwrap().len(), 1);
    assert_eq!(marks["marks"][0]["character"]["name"], json!("沈砚"));
    assert_eq!(marks["marks"][0]["departed"]["name"], json!("裴照"));
    // 别人的印记不串号。
    let (_, mine) = send(&state, "GET", "/api/me/memorial/marks", "u1", None, None).await;
    assert_eq!(mine["marks"].as_array().unwrap().len(), 0, "死者主人自己不会拿到印记");
}

/// 印记的三条筛选：双向羁绊都算 · 敌对关系（负值）同样算 · NPC 与已封卷的卡不接收印记。
#[tokio::test]
async fn departed_marks_cover_both_directions_and_skip_non_living() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    seed_user(&state.db, "u3").await;
    seed_char(&state, "chC", "u3", "崔九", 0).await;
    seed_char(&state, "chD", "u3", "已故者", 0).await;
    // chD 先行封卷 → 只读的传世卡不再接收印记。
    sqlx::query("UPDATE cloud_characters SET memorial_status = 'sealed', withdrawn = 1 WHERE id = 'chD'")
        .execute(&state.db)
        .await
        .unwrap();

    set_narrative_state(
        &state,
        "w1",
        narrative_with(
            json!([
                // 反向（死者 → 对方）同样算：那是同一段羁绊。
                { "from": "chA", "to": "chB", "trust": 0.5, "affinity": 0.5, "fear": 0.0, "debt": 0.0 },
                // 宿敌（全负）也是羁绊：「你的死成为别人故事的一部分」对敌人同样成立。
                { "from": "chC", "to": "chA", "trust": -0.9, "affinity": -0.8, "fear": 0.0, "debt": 0.0 },
                // NPC（无 cloud_characters 行）→ 天然跳过。
                { "from": "npc_wangpo", "to": "chA", "trust": 0.9, "affinity": 0.9, "fear": 0.0, "debt": 0.0 },
                // 已封卷的传世卡 → 不接收（改写只读卡与「传世卡只读」冲突）。
                { "from": "chD", "to": "chA", "trust": 0.9, "affinity": 0.9, "fear": 0.0, "debt": 0.0 },
                // 与死者无关的关系 → 不产生任何印记。
                { "from": "chB", "to": "chC", "trust": 0.9, "affinity": 0.9, "fear": 0.0, "debt": 0.0 },
            ]),
            None,
        ),
    )
    .await;

    let (st, body) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["marksGranted"], json!(2), "只有 chB（反向羁绊）与 chC（宿敌）该拿到印记");

    let holders: Vec<String> =
        sqlx::query_scalar("SELECT character_id FROM memorial_marks ORDER BY character_id ASC")
            .fetch_all(&state.db)
            .await
            .unwrap();
    assert_eq!(holders, vec!["chB".to_string(), "chC".to_string()]);
}

/// 羁绊阈值可参数化（VALIDATION.md §0.2「产品规则参数化，禁止写死」）：
/// 调高 `MUSE_MEMORIAL_BOND_MIN` 后，浅关系不再够格拿印记。
#[tokio::test]
async fn bond_threshold_is_parameterized() {
    let state = test_state().await;
    let _sw = MemorialSwitch::with(true, &[("MUSE_MEMORIAL_BOND_MIN", "0.7")]);
    assert!((bond_min_intensity() - 0.7).abs() < f64::EPSILON);
    seed_landed_death(&state).await;
    set_narrative_state(
        &state,
        "w1",
        narrative_with(
            json!([{ "from": "chB", "to": "chA", "trust": 0.1, "affinity": 0.2, "fear": 0.0, "debt": 0.0 }]),
            None,
        ),
    )
    .await;

    let (st, body) = send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["marksGranted"], json!(0), "强度 0.2 < 阈值 0.7 → 不够格算「故人」");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM memorial_marks").await, 0);
}

// ==================== 遗作馆（只读陈列） ====================

/// 遗作馆只陈列传世卡；详情出「累计人生」四段（历练 / 传记 / 足迹 / 谁还记得他），
/// 且**不出任何真人身份**（§14 角色面具）。
#[tokio::test]
async fn memorial_hall_shows_only_sealed_cards_with_full_life_ledger() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    seed_world(&state.db, "w0", 0, "ended").await;
    seed_member(&state.db, "wm0", "w0", "u1", "chA", "left").await;

    // 封卷前：遗作馆空、详情 404（在世的卡不在墓园里）。
    let (_, hall) = send(&state, "GET", "/api/memorial/characters", "u1", None, None).await;
    assert_eq!(hall["total"], json!(0));
    let (st, _) = send(&state, "GET", "/api/memorial/characters/chA", "u1", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "在世的卡不该出现在遗作馆");

    send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;

    let (st, hall) = send(&state, "GET", "/api/memorial/characters", "u2", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(hall["total"], json!(1));
    assert_eq!(hall["characters"][0]["id"], json!("chA"));
    assert_eq!(hall["characters"][0]["name"], json!("裴照"));
    assert_eq!(hall["characters"][0]["mileage"], json!(420), "历练是显性资产，陈列出来");
    assert!(!hall.to_string().contains("u1"), "🔴 角色面具：遗作馆不得出现主人身份");

    let (st, detail) = send(&state, "GET", "/api/memorial/characters/chA", "u2", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["readOnly"], json!(true));
    assert_eq!(detail["memorialStatus"], json!("sealed"));
    assert_eq!(detail["mileage"], json!(420));
    assert_eq!(detail["biography"]["identity"]["name"], json!("裴照"));
    assert_eq!(detail["biography"]["sealedIn"]["worldId"], json!("w1"));
    // 足迹：走过的两个世界都在（含已退场的——履历不因死亡消失）。
    let feet = detail["footprints"].as_array().unwrap();
    assert_eq!(feet.len(), 2, "足迹应含全部走过的世界：{detail}");
    assert!(feet.iter().any(|f| f["worldId"] == json!("w0") && f["status"] == json!("left")));
    assert!(feet.iter().any(|f| f["worldId"] == json!("w1")));
    // 羁绊：谁还记得他。
    assert_eq!(detail["remembrance"]["rememberedBy"][0]["characterId"], json!("chB"));
    assert_eq!(detail["remembrance"]["rememberedBy"][0]["name"], json!("沈砚"));
    assert!(!detail.to_string().contains("u2"), "🔴 角色面具：详情同样不出主人身份");
}

/// 🔴 **遗作馆只读**：`/memorial/*` 下只注册 GET，没有任何编辑/删除传世卡的端点。
/// 三重锁：① 路由白名单（本模块只注册这四条）；② 写方法只有那一条**不在** `/memorial` 下的封卷；
/// ③ 运行时探测——写形态的 URL 一律打不通。
#[tokio::test]
async fn red_line_memorial_hall_is_read_only() {
    let src = include_str!("mod.rs");

    // ① + ②：逐条核对每条路由声明的「路径 + 方法」。
    // ⚠️ 本注释刻意不写出路由宏的字面形态：`docs/API.md` §8 的路由普查是
    //    `grep -rhoE '\.route\("[^"]+"' server/src`，注释里写出来会被算成一条真实路由（虚增计数）。
    let mut declared: Vec<(String, String)> = Vec::new();
    for chunk in src.split(".route(\"").skip(1) {
        let path = chunk.split('"').next().unwrap_or_default().to_string();
        let method = chunk
            .split_once("\", ")
            .and_then(|(_, rest)| rest.split('(').next())
            .unwrap_or_default()
            .trim()
            .to_string();
        declared.push((path, method));
    }
    assert_eq!(
        declared,
        vec![
            ("/memorial/characters".to_string(), "get".to_string()),
            ("/memorial/characters/{id}".to_string(), "get".to_string()),
            ("/me/memorial/marks".to_string(), "get".to_string()),
            ("/me/characters/{id}/memorial".to_string(), "post".to_string()),
        ],
        "传世卡模块只允许这四条路由；任何新增端点都需显式评审"
    );
    for (path, method) in &declared {
        if path.starts_with("/memorial") {
            assert_eq!(method, "get", "遗作馆是陈列，{path} 只能是 GET（传世卡只读）");
        }
    }

    // ③ 运行时探测：写形态的遗作馆 URL 一律打不通（404 路由不存在 / 405 方法不允许）。
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;

    for (method, path) in [
        ("POST", "/api/memorial/characters"),
        ("PUT", "/api/memorial/characters/chA"),
        ("PATCH", "/api/memorial/characters/chA"),
        ("DELETE", "/api/memorial/characters/chA"),
        ("POST", "/api/memorial/characters/chA/epitaph"),
        ("POST", "/api/memorial/characters/chA/revive"),
        ("DELETE", "/api/me/memorial/marks"),
    ] {
        let (st, _) = send(&state, method, path, "u1", Some(json!({})), None).await;
        assert!(
            st == StatusCode::NOT_FOUND || st == StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} 必须打不通（传世卡只读、且没有复活路径），got {st}"
        );
    }
    // 探测之后传世卡一字未改。
    assert_eq!(memorial_status_of(&state, "chA").await, "sealed");
    assert_eq!(withdrawn_of(&state, "chA").await, 1);
}

// ==================== 转世：内核可复制，履历不可复制 ====================

/// 🔴 **同内核开新卡 = 转世（双胞胎），不是复活**：新卡是全新的卡，
/// **不继承死者的任何履历**——新 id、零历练、空传记、无足迹、无印记，也不在遗作馆里。
/// 死者那一边同样一字未改（履历不可复制 = 也不可被搬走）。
#[tokio::test]
async fn reincarnated_card_inherits_nothing_from_the_deceased() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    send(&state, "POST", "/api/me/characters/chA/memorial", "u1", None, None).await;

    // 走真实发布端点开一张同内核（同名同设定）的新卡——这就是「转世」。
    let (st, published) = send(
        &state,
        "POST",
        "/api/assets/characters",
        "u1",
        Some(json!({
            "localCardId": "local-reborn",
            "cardJson": { "schemaVersion": 2, "identity": { "name": "裴照" } },
            "rightsDeclaration": "original",
        })),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "转世应能正常开新卡（封卷腾出了卡位）：{published}");
    let reborn = published["id"].as_str().expect("新卡 id").to_string();
    assert_ne!(reborn, "chA", "转世是新卡，不是同一张卡");

    let row = sqlx::query(
        "SELECT mileage, memorial_status, memorial_sealed_at, memorial_world_id \
         FROM cloud_characters WHERE id = ?",
    )
    .bind(&reborn)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(row.try_get::<i64, _>("mileage").unwrap(), 0, "转世卡零历练——履历不可复制");
    assert_eq!(row.try_get::<String, _>("memorial_status").unwrap(), "living", "它没死过那一次");
    assert!(row.try_get::<Option<i64>, _>("memorial_sealed_at").unwrap().is_none(), "空传记");
    assert!(row.try_get::<Option<String>, _>("memorial_world_id").unwrap().is_none());

    // 无足迹、无印记：死者的羁绊不会跟着转世卡走。
    assert_eq!(
        count(&state, &format!("SELECT COUNT(*) FROM world_members WHERE cloud_character_id = '{reborn}'")).await,
        0,
        "转世卡没有任何足迹"
    );
    assert_eq!(
        count(&state, &format!("SELECT COUNT(*) FROM memorial_marks WHERE deceased_character_id = '{reborn}'")).await,
        0
    );

    // 遗作馆只有死者那一张；死者的履历一字未改。
    let (_, hall) = send(&state, "GET", "/api/memorial/characters", "u1", None, None).await;
    assert_eq!(hall["total"], json!(1), "转世卡不进遗作馆（它还活着）");
    assert_eq!(hall["characters"][0]["id"], json!("chA"));
    let (_, detail) = send(&state, "GET", "/api/memorial/characters/chA", "u1", None, None).await;
    assert_eq!(detail["mileage"], json!(420), "死者的历练不因转世被搬走或清零");
    assert_eq!(detail["remembrance"]["rememberedBy"][0]["characterId"], json!("chB"));

    // 转世卡自身在遗作馆查不到（它在世）。
    let (st, _) =
        send(&state, "GET", &format!("/api/memorial/characters/{reborn}"), "u1", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

// ==================== 🔴 源码级红线断言 ====================

/// 剥掉注释行，只留**可执行代码**再做 grep 断言。
///
/// 必要性：本模块的注释大量**引用**被禁的名字来解释「为什么不这么做」
/// （如「归还不是 `grant_item_tx` 发货」「故人印记不是 buff」）。
/// 不剥注释的话，红线断言会把「写下了禁令」本身当成「违反了禁令」，
/// 逼着后来人删掉解释才能过测——那恰好是最不该发生的事。
/// `prefixes` 为该语言的行注释前缀（Rust `//`、SQL `--`）。
fn code_only(src: &str, prefixes: &[&str]) -> String {
    src.lines()
        .filter(|line| {
            let t = line.trim_start();
            !prefixes.iter().any(|p| t.starts_with(p))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 🔴 **公共事实不可回滚**（§0.3）：封卷不改写世界线——本模块源码里不存在任何
/// 写 `worlds` / `world_events` / `consent_requests` / 删 `world_members` 的 SQL。
/// （测试脚手架里的 `UPDATE worlds` 在 tests.rs，不在被检查的 mod.rs 内。）
#[test]
fn red_line_never_rewrites_worldline() {
    let src = include_str!("mod.rs");
    for banned in [
        "UPDATE worlds",
        "INSERT INTO worlds",
        "DELETE FROM worlds",
        "INSERT INTO world_events",
        "UPDATE world_events",
        "DELETE FROM world_events",
        "UPDATE consent_requests",
        "INSERT INTO consent_requests",
        "DELETE FROM consent_requests",
        "DELETE FROM world_members",
        "UPDATE world_members",
    ] {
        assert!(
            !src.contains(banned),
            "传世卡模块出现 `{banned}`：死亡是已落定的公共事实，封卷只改卡的状态，绝不改写世界线（§0.3）"
        );
    }
    assert!(
        !src.contains("narrative_state_json = ") && !src.contains("SET narrative_state_json"),
        "绝不写 narrative_state_json：那一列每 tick 回灌进引擎（§0.1 平权红线）"
    );
}

/// 🔴 **不凭空造资产**（§0.2 资产单一写入路径）：归还道具只能是 UPDATE（解除携带），
/// 绝不 INSERT `backpacks`——道具本为账户资产，再发一次货会让一次死亡把道具变成两件。
#[test]
fn red_line_never_mints_items() {
    let src = code_only(include_str!("mod.rs"), &["//"]);
    assert!(!src.contains("INSERT INTO backpacks"), "传世卡模块不得新增背包行（那是发货，不是归还）");
    assert!(!src.contains("INSERT INTO items"), "传世卡模块不得新增物品定义");
    assert!(
        !src.contains("grant_item_tx("),
        "归还 ≠ 发货：`grant_item_tx` 是 INSERT 类发货路径，对本就在账户里的道具再发一次即造资产"
    );
    // 归还只能是 UPDATE：把携带态改回 owned。
    assert!(src.contains("UPDATE backpacks"));
    // 唯一允许新增的资产表是 memorial_marks（无强度/加成列，见迁移 0034）。
    assert!(src.contains("INSERT INTO memorial_marks"));
}

/// 🔴 **传世卡状态与「故人」印记不进引擎决策**（§0.1 平权宪法）：
/// 引擎叙事层与 `RoundInput` 组装处源码级零 `memorial` 引用。口径与历练 mileage / 副本卡逐字一致。
#[test]
fn red_line_marks_never_enter_engine_decision() {
    let runtime_src = include_str!("../runtime/mod.rs");
    assert!(
        !runtime_src.contains("memorial"),
        "runtime/mod.rs（RoundInput 组装处）不得引用传世卡：传世卡状态与故人印记绝不进引擎决策"
    );
    let engine_narrative_src = include_str!("../../../crates/muse-engine/src/narrative/mod.rs");
    assert!(
        !engine_narrative_src.contains("memorial"),
        "muse-engine narrative（RoundInput/role_decide/仲裁）不得引用传世卡"
    );
    let engine_decide_src = include_str!("../../../crates/muse-engine/src/narrative/decide.rs");
    assert!(!engine_decide_src.contains("memorial"), "role_decide 不得引用传世卡");
}

/// 🔴 **无隐藏数值**（§12「全是显性资产，无隐藏数值」）：
/// 迁移与模块里都不存在任何加成/系数/权重形态的字段。
#[test]
fn red_line_no_hidden_numbers() {
    let migration =
        code_only(include_str!("../../migrations/0034_memorial.sql"), &["--"]).to_ascii_lowercase();
    for banned in ["bonus", "multiplier", "factor", "buff", "weight", "power_tier", "score"] {
        assert!(
            !migration.contains(banned),
            "0034 迁移出现疑似数值列 `{banned}`：传世卡的价值全是显性资产，不得引入隐藏加成"
        );
    }
    let src = code_only(include_str!("mod.rs"), &["//"]).to_ascii_lowercase();
    for banned in ["bonus", "multiplier", "buff", "power_tier +", "score"] {
        assert!(!src.contains(banned), "传世卡模块出现 `{banned}`：故人印记是叙事印记，不是 buff");
    }
}

// ==================== 结算侧自动封卷（#29 接线层） ====================

/// 终局结算时自动封卷本世界已死的卡，口径与玩家主动认领**完全一致**（两条证据缺一不可）。
#[tokio::test]
async fn auto_seal_at_settlement_matches_manual_claim_criteria() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    // seed_landed_death 已造好：chA（u1，已死已落定）与 chB（u2，同世界但没死）。
    seed_landed_death(&state).await;

    let mut tx = state.db.begin().await.unwrap();
    let sealed = super::auto_seal_dead_participants_tx(
        &mut tx,
        "w1",
        &[("chA".into(), "u1".into()), ("chB".into(), "u2".into())],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(sealed, 1, "只有确已死亡的卡被封卷");
    assert_eq!(memorial_status_of(&state, "chA").await, "sealed");
    assert_eq!(
        memorial_status_of(&state, "chB").await,
        "living",
        "🔴 同一世界的活人绝不能被顺手封卷——结算名单包含全体参与者，逐个核验缺一不可"
    );
}

/// **授权 ≠ 死亡** 在自动路径上同样成立：pending 未清空时不封卷（否则就是捏造死亡）。
#[tokio::test]
async fn auto_seal_respects_the_not_yet_landed_guard() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;
    // 回退到「已授权但未落定」。
    set_narrative_state(&state, "w1", narrative_with(json!([]), Some("chA"))).await;

    let mut tx = state.db.begin().await.unwrap();
    let sealed = super::auto_seal_dead_participants_tx(&mut tx, "w1", &[("chA".into(), "u1".into())])
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(sealed, 0, "同意已获批但引擎尚未落定 → 不得自动封卷");
    assert_eq!(memorial_status_of(&state, "chA").await, "living");
}

/// 幂等：世界重复结算、或玩家已主动认领过，都不会重复封卷。
#[tokio::test]
async fn auto_seal_is_idempotent_across_repeated_settlements() {
    let state = test_state().await;
    let _sw = MemorialSwitch::set(true);
    seed_landed_death(&state).await;

    let run = |db: sqlx::AnyPool| async move {
        let mut tx = db.begin().await.unwrap();
        let n = super::auto_seal_dead_participants_tx(&mut tx, "w1", &[("chA".into(), "u1".into())])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        n
    };
    assert_eq!(run(state.db.clone()).await, 1, "首次封卷");
    assert_eq!(run(state.db.clone()).await, 0, "重复结算不得二次封卷（CAS 短路）");
    assert_eq!(memorial_status_of(&state, "chA").await, "sealed");
}

/// 开关关闭时整段短路：一张卡都不封，状态一点没动。
#[tokio::test]
async fn auto_seal_is_disabled_with_the_switch_off() {
    let state = test_state().await;
    seed_landed_death(&state).await;
    let _sw = MemorialSwitch::set(false);

    let mut tx = state.db.begin().await.unwrap();
    let sealed = super::auto_seal_dead_participants_tx(&mut tx, "w1", &[("chA".into(), "u1".into())])
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(sealed, 0);
    assert_eq!(memorial_status_of(&state, "chA").await, "living");
}
