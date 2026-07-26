//! 直播场测试（`sqlite::memory` + oneshot 真实路由）。总规格 §2 场次节奏三档 + §15 第 4 层。
//!
//! 覆盖：
//!
//! - **默认关闭**：七个端点全 404，且**一行都不落库**（前门 + 状态侧双保险）；
//! - **① 定档**：预告提前量参数化并强制 · 节目单只列已到预告时刻的场次 · 状态机单向 · 容量;
//! - **② 延迟缓冲**（重头）：
//!   - 播出面落后世界 N 拍；
//!   - 🔴 **世界成员的读取面不延迟**（延后当事人 = 让世界停摆）；
//!   - 🔴 **上调延迟不回撤已播出的拍**（单调水位线）；
//!   - 收播后过宽限期放尾拍；
//!   - 🔴 **撤下一条 → `world_events` 逐字节不变**，且成员读取面照旧可见；
//!   - 播出前 / 播出后撤下如实标注 `preemptive`；
//! - **③ 弹幕**：成年门（403 + 零副作用）· 词库命中打码且不外发 · 限频 429（被拒的照样计数）·
//!   🔴 **锚定播出拍而不是世界当前拍**（结构上不可能剧透）· 🔴 **永不进 `world_events`** ·
//!   面具不泄露真身 · 复合游标不丢行；
//! - **④ 转化度量**：三态（`entry_not_open` / `no_data_in_window` / `ok`）+ 分子分母口径
//!   （`was_player` 冻结 · `joined_at > 首次观看` 的严格方向）；
//! - 🔴 **源码级红线**：本模块不写 `world_events` · 错峰「直播场永不延后」判据未被改动。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::{Column, Row};
use tower::ServiceExt;

use super::*;
use crate::safety::testkit::{seed_member, seed_user, seed_world, test_state, token};

// ═══════════════════════════════════════════════════════════════════════════
// 脚手架
// ═══════════════════════════════════════════════════════════════════════════

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    tk: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let app = crate::app::build_router(state.clone());
    let builder =
        Request::builder().method(method).uri(uri).header(header::AUTHORIZATION, format!("Bearer {tk}"));
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

fn admin_token(state: &AppState, role: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, &format!("adm_{role}"), role, 3600).unwrap()
}

/// 🔴 打开开关走 **`runtime_flags` DB 记录**而不是 env：env 是进程级的，本模块用例与其它模块
/// 同属一个测试二进制、默认并发跑，改 env 必须共用全局锁才不串味。写一条 DB 记录只影响本用例
/// 自己的内存库，天然无需加锁（范式抄 `annotations::tests::open_flag`）。
async fn open_flag(state: &AppState, scope: &str, target: &str) {
    sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) \
         VALUES ($1, $2, $3, $4, 1, 0, 0, 'test', $5, '用例开闸', $6)",
    )
    .bind(new_id("rf"))
    .bind(ENV_LIVE_STAGE)
    .bind(scope)
    .bind(target)
    .bind(now_ms())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    crate::flags::invalidate(&state.db);
}

async fn seed_tick(state: &AppState, world: &str, tick_no: i64, status: &str) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
         VALUES ($1, $2, $3, 0, $4, 120, $5)",
    )
    .bind(format!("tk_{world}_{tick_no}"))
    .bind(world)
    .bind(tick_no)
    .bind(status)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 落一条 public 且过审的世界事件（= 直播播出面的候选）。
async fn seed_event(state: &AppState, world: &str, tick_no: i64, seq: i64, summary: &str) -> String {
    let id = format!("ev_{world}_{seq}");
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, public_projection_json, moderation, ai_label, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, 'action', '[\"c1\"]', 'public', $6, 'approved', 1, $7)",
    )
    .bind(&id)
    .bind(world)
    .bind(tick_no)
    .bind(seq)
    .bind(format!("dev_{world}_{seq}"))
    .bind(json!({ "summary": summary }).to_string())
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    id
}

/// 建一场并直接置为 live（省去两次 admin 调用；状态机本身另有专门用例）。
async fn seed_live_session(state: &AppState, id: &str, world: &str, delay_ticks: i64, capacity: i64) {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO live_sessions (id, world_id, title, status, announce_at, starts_at, ends_at, \
         delay_ticks, published_high_tick, capacity, created_by, started_at, ended_at, created_at, updated_at) \
         VALUES ($1, $2, '今晚 2 小时', 'live', $3, $4, 0, $5, -1, $6, 'adm', $7, 0, $8, $9)",
    )
    .bind(id)
    .bind(world)
    .bind(now - 7_200_000)
    .bind(now - 3_600_000)
    .bind(delay_ticks)
    .bind(capacity)
    .bind(now - 3_600_000)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await
    .unwrap();
}

async fn set_age(state: &AppState, user: &str, age_declared: i64) {
    sqlx::query("UPDATE users SET age_declared = $1 WHERE id = $2")
        .bind(age_declared)
        .bind(user)
        .execute(&state.db)
        .await
        .unwrap();
}

async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(&state.db).await.unwrap()
}

/// 播出面里的事件 summary 列表（按播出顺序）。
fn summaries(body: &Value) -> Vec<String> {
    body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["summary"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// 一张表的逐字节快照（红线用例用；列名 + 值全量）。抄 `annotations::tests` 的口径。
async fn snapshot(state: &AppState, table: &str) -> Vec<String> {
    let rows = sqlx::query(&format!("SELECT * FROM {table}")).fetch_all(&state.db).await.unwrap();
    let mut out: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut cells: Vec<String> = Vec::new();
            for c in r.columns() {
                let name = c.name();
                let v = r
                    .try_get::<String, _>(name)
                    .or_else(|_| r.try_get::<i64, _>(name).map(|n| n.to_string()))
                    .or_else(|_| r.try_get::<f64, _>(name).map(|n| n.to_string()))
                    .unwrap_or_else(|_| "<null>".into());
                cells.push(format!("{name}={v}"));
            }
            cells.join("|")
        })
        .collect();
    out.sort();
    out
}

/// 一个「世界已跑到第 N 拍、每拍一条 public 事件」的直播场。
async fn seed_world_with_ticks(state: &AppState, world: &str, ticks: i64) {
    seed_world(&state.db, world, 0, "running").await;
    for t in 0..ticks {
        seed_tick(state, world, t, "done").await;
        seed_event(state, world, t, t, &format!("第{t}拍")).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 未验证功能默认关闭（VALIDATION §0.1）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 开关默认关闭 → **七个端点全 404，且一行都不落库**。
///
/// 「零副作用」是这条约束的实质：只回 404 但顺手记了观众足迹 / 落了弹幕，等于功能其实开着。
#[tokio::test]
async fn red_line_disabled_by_default_every_endpoint_404_with_zero_side_effect() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 3).await;
    seed_live_session(&state, "lvs1", "w1", 2, 0).await;
    let tk = token(&state, "u1");
    let op = admin_token(&state, "operator");
    let rv = admin_token(&state, "reviewer");

    let cases: Vec<(&str, &str, &str, Option<Value>)> = vec![
        ("GET", "/api/live/sessions", tk.as_str(), None),
        ("GET", "/api/live/sessions/lvs1", tk.as_str(), None),
        ("GET", "/api/live/sessions/lvs1/feed", tk.as_str(), None),
        ("GET", "/api/live/sessions/lvs1/danmaku", tk.as_str(), None),
        ("POST", "/api/live/sessions/lvs1/danmaku", tk.as_str(), Some(json!({"body": "开场了"}))),
        (
            "POST",
            "/api/admin/live/sessions",
            op.as_str(),
            Some(json!({"worldId": "w1", "startsAt": now_ms() + 86_400_000})),
        ),
        ("POST", "/api/admin/live/sessions/lvs1", op.as_str(), Some(json!({"status": "ended"}))),
        (
            "POST",
            "/api/admin/live/sessions/lvs1/withhold",
            rv.as_str(),
            Some(json!({"eventId": "ev_w1_0"})),
        ),
    ];
    for (method, uri, t, body) in cases {
        let (st, _) = send(&state, method, uri, t, body).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{method} {uri} 在开关关闭时必须 404");
    }

    // 状态侧：四张表一行都不许有（观众足迹、弹幕、撤下、场次都不得被上面的调用改动）。
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_viewers").await, 0, "🔴 关闭时记了观众足迹");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_danmaku").await, 0, "🔴 关闭时落了弹幕");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_withholds").await, 0, "🔴 关闭时撤下了内容");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM live_sessions").await,
        1,
        "🔴 关闭时新建了场次（只应有用例自己播种的那一条）"
    );
    assert_eq!(count(&state, "SELECT COUNT(*) FROM audit_logs").await, 0, "🔴 关闭时写了审计");
}

// ═══════════════════════════════════════════════════════════════════════════
// ① 定档
// ═══════════════════════════════════════════════════════════════════════════

/// 定档提前量是**参数**（§0.2）且被强制：预告太贴近开播 → 400，一行不落库。
/// 另测：开播时刻必须在未来。
#[tokio::test]
async fn announce_lead_is_parameterized_and_enforced() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world(&state.db, "w1", 0, "running").await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let op = admin_token(&state, "operator");

    let starts = now_ms() + 86_400_000;
    // 预告只提前 1 秒 → 拒。
    let (st, b) = send(
        &state,
        "POST",
        "/api/admin/live/sessions",
        &op,
        Some(json!({"worldId": "w1", "startsAt": starts, "announceAt": starts - 1000})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "提前量不足必须拒：{b}");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_sessions").await, 0, "被拒请求零副作用");

    // 开播时刻在过去 → 拒（定档的意义是"提前知道"）。
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/live/sessions",
        &op,
        Some(json!({"worldId": "w1", "startsAt": now_ms() - 1000})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 不传 announceAt → 默认 = 开播时刻 - 提前量下限，且延迟拍数取参数默认值。
    let (st, b) = send(
        &state,
        "POST",
        "/api/admin/live/sessions",
        &op,
        Some(json!({"worldId": "w1", "startsAt": starts, "title": "今晚 2 小时"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["announceAt"].as_i64().unwrap(), starts - super::announce_lead_ms());
    assert_eq!(b["broadcast"]["delayTicks"], json!(super::default_delay_ticks()));
    assert_eq!(b["status"], json!("scheduled"));
    // 定档必须留审计。
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM audit_logs WHERE action='live.session.create'").await,
        1
    );
}

/// 节目单**只列已到预告时刻**的场次：定档的产品意义就是"到点了才公开"。
#[tokio::test]
async fn lineup_lists_only_announced_sessions() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world(&state.db, "w1", 0, "running").await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let op = admin_token(&state, "operator");
    let tk = token(&state, "u1");

    let lead = super::announce_lead_ms();
    let now = now_ms();
    // 今晚场：预告时刻已过（显式给一个过去的 announceAt，提前量仍远超下限）。
    // 明天场：预告时刻仍在未来（缺省 announceAt = startsAt - lead）。
    let cases = [
        (now + lead + 60_000, Some(now - 1000), "今晚场"),
        (now + lead + 86_400_000, None, "明天场"),
    ];
    for (starts, announce, title) in cases {
        let mut body = json!({"worldId": "w1", "startsAt": starts, "title": title});
        if let Some(a) = announce {
            body["announceAt"] = json!(a);
        }
        let (st, b) = send(&state, "POST", "/api/admin/live/sessions", &op, Some(body)).await;
        assert_eq!(st, StatusCode::OK, "{b}");
    }

    let (st, b) = send(&state, "GET", "/api/live/sessions", &tk, None).await;
    assert_eq!(st, StatusCode::OK);
    let titles: Vec<&str> =
        b["sessions"].as_array().unwrap().iter().map(|s| s["title"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["今晚场"], "未到预告时刻的场次不得出现在节目单：{b}");
    // 节目单也对观众公开延迟拍数——直播场明说自己是延迟的。
    assert!(b["sessions"][0]["delayTicks"].is_i64());
}

/// 状态机单向：`scheduled → live → ended`，回头路一律 409。
#[tokio::test]
async fn session_status_machine_is_one_way() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world(&state.db, "w1", 0, "running").await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let op = admin_token(&state, "operator");

    let (_, created) = send(
        &state,
        "POST",
        "/api/admin/live/sessions",
        &op,
        Some(json!({"worldId": "w1", "startsAt": now_ms() + 86_400_000})),
    )
    .await;
    let sid = created["id"].as_str().unwrap().to_string();
    let uri = format!("/api/admin/live/sessions/{sid}");

    // scheduled → ended 不合法（没开播过怎么收播）。
    let (st, _) = send(&state, "POST", &uri, &op, Some(json!({"status": "ended"}))).await;
    assert_eq!(st, StatusCode::CONFLICT);

    let (st, b) = send(&state, "POST", &uri, &op, Some(json!({"status": "live"}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["status"], json!("live"));
    assert!(b["startedAt"].as_i64().unwrap() > 0, "开播时刻必须与状态同一条 UPDATE 写入");

    let (st, b) = send(&state, "POST", &uri, &op, Some(json!({"status": "ended"}))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(b["endedAt"].as_i64().unwrap() > 0, "收播时刻决定尾拍何时放行，不得为 0");

    // ended 是终局。
    let (st, _) = send(&state, "POST", &uri, &op, Some(json!({"status": "live"}))).await;
    assert_eq!(st, StatusCode::CONFLICT);
}

/// 场次容量是**参数**且被强制：满员后新观众 409，老观众照旧可进。
#[tokio::test]
async fn session_capacity_is_enforced_for_new_viewers_only() {
    let state = test_state().await;
    for u in ["u1", "u2"] {
        seed_user(&state.db, u).await;
    }
    seed_world_with_ticks(&state, "w1", 3).await;
    seed_live_session(&state, "lvs1", "w1", 0, 1).await; // 容量 1
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let (st, _) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, "u1"), None).await;
    assert_eq!(st, StatusCode::OK);
    // 老观众重复进场不受容量影响。
    let (st, _) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, "u1"), None).await;
    assert_eq!(st, StatusCode::OK);
    // 新观众撞上限。
    let (st, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, "u2"), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_viewers").await, 1, "被拒进场不得留足迹");
}

// ═══════════════════════════════════════════════════════════════════════════
// ② 延迟缓冲（本批次的核心）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 播出面落后世界 N 拍；且**世界成员的读取面一拍不延**。
///
/// 后半句是「时间差不造成事实错乱」的第一条：延后当事人等于让世界停摆——他们的角色正在
/// 经历这些事，不可能等审核。延迟只作用于世界**外**的观众。
#[tokio::test]
async fn delay_buffer_holds_recent_ticks_but_never_delays_world_members() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "player").await;
    seed_world_with_ticks(&state, "w1", 6).await; // 拍 0..5
    seed_member(&state.db, "m1", "w1", "player", "c1", "active").await;
    seed_live_session(&state, "lvs1", "w1", 2, 0).await; // 延迟 2 拍
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let (st, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, "u1"), None).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["broadcast"]["worldTickNow"], json!(5), "世界已跑到第 5 拍");
    assert_eq!(b["broadcast"]["publishedThroughTick"], json!(3), "5 - 延迟 2 = 播到第 3 拍");
    assert_eq!(b["broadcast"]["pendingTicks"], json!(2), "缓冲里压着 2 拍");
    assert_eq!(
        summaries(&b),
        vec!["第0拍", "第1拍", "第2拍", "第3拍"],
        "第 4、5 拍还在审核窗口里，不得播出：{b}"
    );

    // 🔴 同一世界的成员读取面：一拍不延，全部 6 条都在。
    let (st, ev) = send(&state, "GET", "/api/worlds/w1/events", &token(&state, "player"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        ev["events"].as_array().unwrap().len(),
        6,
        "🔴 延迟缓冲外溢到了世界成员的读取面——那是让世界停摆：{ev}"
    );
}

/// 🔴 **上调延迟拍数不回撤已播出的拍**（单调水位线）。
///
/// 这正是 VALIDATION §2 T5 预案「审核成本失控 → 直播延迟拍数上调」那个旋钮的安全性证明：
/// 上调只勒住未来。若没有 `published_high_tick`，把延迟从 0 调到 4 会让已经在观众屏幕上
/// 滚过去的 4 拍**从播出面消失**——那是对已公开内容的回滚。
#[tokio::test]
async fn raising_delay_ticks_never_retracts_already_published_ticks() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 6).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await; // 先零延迟：全部播出
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (_, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
    assert_eq!(b["broadcast"]["publishedThroughTick"], json!(5), "零延迟 → 播到最新拍");
    assert_eq!(summaries(&b).len(), 6);

    // 运营上调延迟到 4 拍（审核成本失控预案）。
    let op = admin_token(&state, "operator");
    let (st, b) = send(
        &state,
        "POST",
        "/api/admin/live/sessions/lvs1",
        &op,
        Some(json!({"delayTicks": 4, "reason": "审核成本失控"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["broadcast"]["delayTicks"], json!(4));
    assert_eq!(
        b["broadcast"]["publishedThroughTick"],
        json!(5),
        "🔴 上调延迟把已播出的水位线拉回去了 —— 已公开内容被回滚"
    );

    // 观众侧同样看不到回撤。
    let (_, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
    assert_eq!(summaries(&b).len(), 6, "已播出的 6 条一条都不许消失：{b}");

    // 但**新拍**确实被勒住了：世界再跑两拍，播出面不动（5+2=7 - 4 = 3 < 已播 5）。
    for t in 6..8 {
        seed_tick(&state, "w1", t, "done").await;
        seed_event(&state, "w1", t, t, &format!("第{t}拍")).await;
    }
    let (_, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
    assert_eq!(b["broadcast"]["worldTickNow"], json!(7));
    assert_eq!(b["broadcast"]["publishedThroughTick"], json!(5), "上调后新拍必须被勒住");
    assert_eq!(summaries(&b).len(), 6, "新拍不得播出");
    assert_eq!(b["broadcast"]["pendingTicks"], json!(2));
}

/// 收播后过了**尾拍放行宽限**，缓冲里剩的最后 N 拍才放出去。
///
/// 不放行的话，世界不再产新拍，最后 N 拍会被永久卡在缓冲里——观众永远看不到结局。
/// 宽限本身是参数（`MUSE_LIVE_DRAIN_GRACE_MS`），用例用回拨 `ended_at` 的方式模拟"已过宽限"，
/// 避免改进程级 env（会与并发用例互踩）。
#[tokio::test]
async fn ended_session_drains_the_tail_only_after_the_grace_window() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 5).await; // 拍 0..4
    seed_live_session(&state, "lvs1", "w1", 2, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");
    let op = admin_token(&state, "operator");

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/live/sessions/lvs1",
        &op,
        Some(json!({"status": "ended", "reason": "收播"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 刚收播（宽限未过）：尾 2 拍仍压着。
    let (_, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
    assert_eq!(b["broadcast"]["publishedThroughTick"], json!(2), "宽限内不得提前放尾拍：{b}");

    // 把收播时刻回拨到宽限之外。
    sqlx::query("UPDATE live_sessions SET ended_at = $1 WHERE id = 'lvs1'")
        .bind(now_ms() - super::drain_grace_ms() - 1000)
        .execute(&state.db)
        .await
        .unwrap();
    let (_, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
    assert_eq!(b["broadcast"]["publishedThroughTick"], json!(4), "过了宽限尾拍必须放行：{b}");
    assert_eq!(summaries(&b).len(), 5);
}

/// 未开播 / 已取消的场次**一拍不播**（定档尚未生效）。
#[tokio::test]
async fn scheduled_and_canceled_sessions_broadcast_nothing() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 4).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    sqlx::query("UPDATE live_sessions SET status = 'scheduled' WHERE id = 'lvs1'")
        .execute(&state.db)
        .await
        .unwrap();
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let (st, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, "u1"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["broadcast"]["publishedThroughTick"], json!(null), "未开播时无播出边界");
    assert!(summaries(&b).is_empty(), "未开播不得播出任何内容：{b}");
}

/// 🔴 **撤下一条 → `world_events` 逐字节不变**，且世界成员的读取面照旧可见。
///
/// 这是「审核不通过怎么处理」的核心答案：**不外发 ≠ 回滚**。撤下写的是 `live_withholds`
/// 独立表，世界事实与当事人视角完全不受影响（§0.3 公共事实不可回滚）。
#[tokio::test]
async fn withholding_leaves_the_worldline_byte_identical_and_scoped_to_this_session() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_user(&state.db, "player").await;
    seed_world_with_ticks(&state, "w1", 4).await;
    seed_member(&state.db, "m1", "w1", "player", "c1", "active").await;
    seed_live_session(&state, "lvs1", "w1", 1, 0).await; // 延迟 1 → 播到第 2 拍
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");
    let rv = admin_token(&state, "reviewer");

    let before = snapshot(&state, "world_events").await;

    // 第 2 拍尚未被任何观众拉取过 → published_high_tick 仍是 -1 → 播出前拦下。
    let (st, b) = send(
        &state,
        "POST",
        "/api/admin/live/sessions/lvs1/withhold",
        &rv,
        Some(json!({"eventId": "ev_w1_2", "reason": "疑似违规"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["preemptive"], json!(true), "缓冲窗口内撤下必须标为 preemptive：{b}");

    // 🔴 世界事实逐字节不变。
    assert_eq!(before, snapshot(&state, "world_events").await, "🔴 撤下改写了世界事实");

    // 播出面不再有它。
    let (_, feed) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
    assert_eq!(summaries(&feed), vec!["第0拍", "第1拍"], "被撤下的事件不得播出：{feed}");

    // 🔴 世界成员的读取面照旧看得见——把它从当事人眼前抹掉才是事实错乱。
    let (_, ev) = send(&state, "GET", "/api/worlds/w1/events", &token(&state, "player"), None).await;
    let ids: Vec<&str> =
        ev["events"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"ev_w1_2"), "🔴 撤下外溢到了世界成员的读取面：{ev}");

    // 撤下必须留审计；重复撤下幂等（不产生第二行）。
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM audit_logs WHERE action='live.event.withhold'").await,
        1
    );
    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/live/sessions/lvs1/withhold",
        &rv,
        Some(json!({"eventId": "ev_w1_2"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_withholds").await, 1, "重复撤下必须幂等");
}

/// 播出**之后**才撤下 → 如实标注 `preemptive=false`，不假装能收回已经看见的。
///
/// `withheldPreemptiveRate` 低于 1 就是「延迟拍数配得不够」的直接证据（T5 预案的上调依据）。
#[tokio::test]
async fn withholding_after_publication_is_reported_honestly() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 3).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await; // 零延迟 = 没有缓冲窗口
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    // 观众先看了一遍（水位线推进到 2）。
    let (_, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
    assert_eq!(b["broadcast"]["publishedThroughTick"], json!(2));

    let (st, b) = send(
        &state,
        "POST",
        "/api/admin/live/sessions/lvs1/withhold",
        &admin_token(&state, "reviewer"),
        Some(json!({"eventId": "ev_w1_1", "reason": "事后发现违规"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["preemptive"], json!(false), "已播出的必须诚实标注：{b}");
    assert!(
        b["notes"][0].as_str().unwrap().contains("播出后撤下"),
        "回执必须明说收不回已经看见的：{b}"
    );
}

/// 撤下别的世界的事件 → 404（不给用一场直播的运营权限去动另一个世界的播出面）。
#[tokio::test]
async fn withhold_rejects_events_from_another_world() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_world_with_ticks(&state, "w2", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let (st, _) = send(
        &state,
        "POST",
        "/api/admin/live/sessions/lvs1/withhold",
        &admin_token(&state, "reviewer"),
        Some(json!({"eventId": "ev_w2_0"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_withholds").await, 0);
}

/// 播出面只出 `public` + `approved`（双硬隔离 + 审核门，与观战/回放同口径）。
#[tokio::test]
async fn broadcast_face_never_leaks_private_or_unapproved_events() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world(&state.db, "w1", 0, "running").await;
    seed_tick(&state, "w1", 0, "done").await;
    seed_event(&state, "w1", 0, 0, "公开且过审").await;
    // 私有投影
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, audience_json, private_projections_json, moderation, ai_label, occurred_at) \
         VALUES ('ev_priv', 'w1', 0, 1, 'dev_priv', 'dialogue', '[\"c1\"]', 'private', '[\"player\"]', \
                 '[{\"summary\":\"私密心事\"}]', 'approved', 1, $1)",
    )
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    // 公开但未过审
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, public_projection_json, moderation, ai_label, occurred_at) \
         VALUES ('ev_pend', 'w1', 0, 2, 'dev_pend', 'action', '[\"c1\"]', 'public', \
                 '{\"summary\":\"被闸拦下的\"}', 'pending', 1, $1)",
    )
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let (_, b) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, "u1"), None).await;
    assert_eq!(summaries(&b), vec!["公开且过审"], "私有投影与未过审事件都不得进播出面：{b}");
}

// ═══════════════════════════════════════════════════════════════════════════
// ③ 弹幕
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **未成年门：403 且零副作用**（真红线 §0.4）。口径与 `social::ensure_adult_social` 一致。
/// 未成年**可以看**（观战本就开放），只是不能发。
#[tokio::test]
async fn red_line_minor_cannot_post_danmaku_but_can_still_watch() {
    let state = test_state().await;
    seed_user(&state.db, "minor").await;
    set_age(&state, "minor", 2).await; // 已声明未成年
    seed_user(&state.db, "undeclared").await;
    set_age(&state, "undeclared", 0).await; // 未声明
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    for u in ["minor", "undeclared"] {
        let tk = token(&state, u);
        let (st, _) = send(
            &state,
            "POST",
            "/api/live/sessions/lvs1/danmaku",
            &tk,
            Some(json!({"body": "开场了"})),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{u} 必须被年龄门拒（fail-closed：未声明也拒）");
        // ⚠️ 但看得见：年龄门挡的是新增的公开发言面，不是基本的观看。
        let (st, _) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await;
        assert_eq!(st, StatusCode::OK, "{u} 观看直播不应被年龄门挡住");
    }

    // 🔴 零副作用：一条弹幕都没落库、没进机审队列、没记险。
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_danmaku").await, 0);
    assert_eq!(count(&state, "SELECT COUNT(*) FROM audit_queue").await, 0);
    assert_eq!(count(&state, "SELECT COUNT(*) FROM risk_events").await, 0);
}

/// 🔴 **弹幕锚定的是「观众看到的那一拍」，不是世界当前拍。**
///
/// 这是「时间差不造成事实错乱」的关键一步：观众评论的永远是他当下看见的内容，
/// 于是弹幕在结构上不可能"剧透"尚未播出的拍。锚点由服务端算，客户端传不进来。
#[tokio::test]
async fn danmaku_anchors_to_the_published_tick_not_the_world_tick() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 6).await; // 世界跑到第 5 拍
    seed_live_session(&state, "lvs1", "w1", 2, 0).await; // 播到第 3 拍
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    // 客户端就算硬塞 anchorTick 也没用——请求体里根本没这个字段，服务端自己算。
    let (st, b) = send(
        &state,
        "POST",
        "/api/live/sessions/lvs1/danmaku",
        &tk,
        Some(json!({"body": "这一拍太精彩了", "anchorTick": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["anchorTick"], json!(3), "🔴 弹幕锚到了尚未播出的拍 = 替世界剧透：{b}");
    assert_eq!(b["isWorldFact"], json!(false));

    // 回放按 anchorTick 对齐。
    let (_, list) = send(&state, "GET", "/api/live/sessions/lvs1/danmaku?anchorTick=3", &tk, None).await;
    assert_eq!(list["danmaku"].as_array().unwrap().len(), 1);
    let (_, list) = send(&state, "GET", "/api/live/sessions/lvs1/danmaku?anchorTick=5", &tk, None).await;
    assert!(list["danmaku"].as_array().unwrap().is_empty(), "第 5 拍还没播，不该有弹幕");
}

/// 词库命中 → **就地打码 + 置 pending + 不外发**；面具不泄露真身。
#[tokio::test]
async fn danmaku_hitting_the_lexicon_is_masked_and_withheld_from_the_stream() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let (st, b) = send(
        &state,
        "POST",
        "/api/live/sessions/lvs1/danmaku",
        &tk,
        Some(json!({"body": "这个主播真是傻逼"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["moderation"], json!("pending"), "命中词库必须转人审而不是打码放行：{b}");
    assert_eq!(b["delivered"], json!(false), "未过审弹幕不得外发");
    assert!(!b["body"].as_str().unwrap().contains('傻'), "落库文本必须是打码后的：{b}");

    // 落库了（人审改判后无需玩家重发），但读取面不出。
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_danmaku").await, 1);
    let (_, list) = send(&state, "GET", "/api/live/sessions/lvs1/danmaku", &tk, None).await;
    assert!(list["danmaku"].as_array().unwrap().is_empty(), "未过审弹幕不得出现在列表：{list}");

    // 干净的弹幕正常放行。
    let (_, ok) = send(
        &state,
        "POST",
        "/api/live/sessions/lvs1/danmaku",
        &tk,
        Some(json!({"body": "这段写得真好"})),
    )
    .await;
    assert_eq!(ok["moderation"], json!("approved"));
    assert_eq!(ok["delivered"], json!(true));

    // 🔴 §14 面具：响应体里只有场次内代号，没有 userId / 昵称 / 手机号。
    let (_, list) = send(&state, "GET", "/api/live/sessions/lvs1/danmaku", &tk, None).await;
    let raw = list.to_string();
    assert!(raw.contains("观众"), "必须有面具代号：{raw}");
    assert!(!raw.contains("u1"), "🔴 弹幕读取面泄露了真人 id：{raw}");
    assert!(list["danmaku"][0].get("userId").is_none());
}

/// 限频：窗口内超额 → 429，且**被拒的弹幕照样计数**（否则可以靠发违规内容白嫖额度）。
/// 频率上限与窗口均为参数（§0.2）。
#[tokio::test]
async fn danmaku_rate_limit_returns_429_and_counts_rejected_ones() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    let rate = super::danmaku_rate_per_window();
    // 前 rate 条里掺一条会被拒的：它照样占额度。
    for i in 0..rate {
        let body = if i == 0 { "你这个傻逼".to_string() } else { format!("第{i}条") };
        let (st, _) = send(
            &state,
            "POST",
            "/api/live/sessions/lvs1/danmaku",
            &tk,
            Some(json!({ "body": body })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "第 {i} 条不应被限频");
    }
    let (st, b) = send(
        &state,
        "POST",
        "/api/live/sessions/lvs1/danmaku",
        &tk,
        Some(json!({"body": "再来一条"})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "超额必须 429（不是 409）：{b}");
    assert_eq!(b["error"]["code"], json!("too_many_requests"));
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM live_danmaku").await,
        rate,
        "被限频的那条不得落库（零副作用）"
    );
}

/// 正文校验：空 → 400；超长 → 400（长度上限是参数）。两者都零副作用。
#[tokio::test]
async fn danmaku_body_is_validated_against_parameterized_limits() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    for body in ["   ", &"啊".repeat(super::danmaku_max_len() + 1)] {
        let (st, _) = send(
            &state,
            "POST",
            "/api/live/sessions/lvs1/danmaku",
            &tk,
            Some(json!({ "body": body })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_danmaku").await, 0);
}

/// 弹幕只在**直播中**可发（未开播 / 已收播 → 409，无处锚定）。
#[tokio::test]
async fn danmaku_is_only_accepted_while_the_session_is_live() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    sqlx::query("UPDATE live_sessions SET status = 'scheduled' WHERE id = 'lvs1'")
        .execute(&state.db)
        .await
        .unwrap();
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let (st, _) = send(
        &state,
        "POST",
        "/api/live/sessions/lvs1/danmaku",
        &token(&state, "u1"),
        Some(json!({"body": "还没开播"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_danmaku").await, 0);
}

/// 🔴 **弹幕永不进 `world_events`**：不是世界事实，不进战报 / 回放 / 日报 / 引擎决策。
#[tokio::test]
async fn red_line_danmaku_never_becomes_a_world_fact() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 3).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let before = snapshot(&state, "world_events").await;
    let (st, _) = send(
        &state,
        "POST",
        "/api/live/sessions/lvs1/danmaku",
        &token(&state, "u1"),
        Some(json!({"body": "观众说了一句话"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(before, snapshot(&state, "world_events").await, "🔴 弹幕写进了世界事实");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM world_events WHERE public_projection_json LIKE '%观众说了一句话%'")
            .await,
        0
    );
}

/// 🔴 弹幕列表用**复合游标**：同毫秒并列行横跨页边界时一条都不能丢。
///
/// 一场直播里几十个人同时发弹幕，同毫秒并列是常态——正是 `crate::pagination` 模块头
/// 描述的那种「单列游标永久丢行」的重灾区。
#[tokio::test]
async fn danmaku_pagination_uses_a_composite_cursor_and_never_drops_rows() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");

    // 5 条**同一毫秒**的弹幕（直接落库，绕开限频与时钟抖动）。
    let same_ms = now_ms();
    for i in 0..5 {
        sqlx::query(
            "INSERT INTO live_danmaku (id, session_id, world_id, user_id, display_name, body, \
             anchor_tick, moderation, created_at) \
             VALUES ($1, 'lvs1', 'w1', 'u1', '观众0000', $2, 1, 'approved', $3)",
        )
        .bind(format!("dmk_{i}"))
        .bind(format!("第{i}条"))
        .bind(same_ms)
        .execute(&state.db)
        .await
        .unwrap();
    }

    // 每页 2 条翻完，收集全部 id。
    let mut seen: Vec<String> = Vec::new();
    let mut uri = "/api/live/sessions/lvs1/danmaku?limit=2".to_string();
    for _ in 0..5 {
        let (st, b) = send(&state, "GET", &uri, &tk, None).await;
        assert_eq!(st, StatusCode::OK);
        let page = b["danmaku"].as_array().unwrap();
        if page.is_empty() {
            break;
        }
        for d in page {
            seen.push(d["id"].as_str().unwrap().to_string());
        }
        let (c, cid) = (b["nextCursor"].as_i64().unwrap(), b["nextCursorId"].as_str().unwrap());
        uri = format!("/api/live/sessions/lvs1/danmaku?limit=2&cursor={c}&cursorId={cid}");
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5, "🔴 同毫秒并列行在翻页时丢了：{seen:?}");
}

// ═══════════════════════════════════════════════════════════════════════════
// ④ 转化度量（T5 门槛「观众→玩家转化 ≥2%」）
// ═══════════════════════════════════════════════════════════════════════════

const DAY: i64 = 86_400_000;

fn window() -> (i64, i64) {
    let now = now_ms();
    (now - 30 * DAY, now + DAY)
}

/// 🔴 入口没开过 → `entry_not_open`（`—`），**不是 0%**。
///
/// 本功能默认关闭，此时窗口内一个观众足迹都不会有。报 0% 会让 T5 门槛「转化 ≥2%」
/// 被误判为**不通过**——一个看起来糟透了、实际上什么都没测的数。
#[tokio::test]
async fn conversion_reports_entry_not_open_instead_of_a_fake_zero() {
    let state = test_state().await;
    let (s, e) = window();
    let m = conversion_block(&state.db, s, e).await.unwrap();
    assert_eq!(m["status"], json!("entry_not_open"), "{m}");
    assert_eq!(m["value"], json!(null), "🔴 入口没开过必须是 null，显示 0% 即误报");
}

/// 入口开着但窗口内零观众 → `no_data_in_window`（`—`），与上一条是两个必须分得开的状态。
#[tokio::test]
async fn conversion_distinguishes_an_empty_window_from_nobody_converting() {
    let state = test_state().await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let (s, e) = window();
    let m = conversion_block(&state.db, s, e).await.unwrap();
    assert_eq!(m["status"], json!("no_data_in_window"), "{m}");
    assert_eq!(m["value"], json!(null));
    assert_eq!(m["viewersCounted"], json!(0));
}

/// 有观众、没人转化 → `ok` + `value=0.0`（**真的 0%**，与上面两态互不混淆）。
#[tokio::test]
async fn conversion_zero_is_a_real_zero_once_there_are_viewers() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    let (st, _) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, "u1"), None).await;
    assert_eq!(st, StatusCode::OK);

    let (s, e) = window();
    let m = conversion_block(&state.db, s, e).await.unwrap();
    assert_eq!(m["status"], json!("ok"), "{m}");
    assert_eq!(m["value"], json!(0.0));
    assert_eq!(m["viewersCounted"], json!(1), "看过直播的非玩家进分母");
    assert_eq!(m["convertedCount"], json!(0));
    assert_eq!(m["belowThreshold"], json!(true), "T5 门槛是下限 ≥2%，0% 必须标为未达标");
}

/// 分子分母口径三条：
/// - 看完**之后**入场 → 计入分子；
/// - 看之**前**就已入场（`was_player=1`）→ **不进分母**（老玩家来看直播不是待转化的观众）；
/// - 先入场后看直播（`joined_at < first_seen_at`）→ 进分母但**不进分子**（那是留存不是拉新）。
#[tokio::test]
async fn conversion_numerator_and_denominator_follow_the_documented_semantics() {
    let state = test_state().await;
    for u in ["conv", "veteran", "already"] {
        seed_user(&state.db, u).await;
    }
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let now = now_ms();

    // veteran：看直播之前就已经是玩家 → was_player 会被冻结为 1。
    seed_member(&state.db, "m_vet", "w1", "veteran", "c_vet", "active").await;

    // 三个人各看一次直播（走真实端点，足迹与 was_player 由端点写）。
    for u in ["conv", "veteran", "already"] {
        let (st, _) = send(&state, "GET", "/api/live/sessions/lvs1/feed", &token(&state, u), None).await;
        assert_eq!(st, StatusCode::OK);
    }
    // already：足迹在，但他的入场发生在**观看之前**（回拨 joined_at）。
    seed_member(&state.db, "m_alr", "w1", "already", "c_alr", "active").await;
    sqlx::query("UPDATE world_members SET joined_at = $1 WHERE id = 'm_alr'")
        .bind(now - 10 * DAY)
        .execute(&state.db)
        .await
        .unwrap();
    // conv：看完之后入场 —— 这才是"直播带来的转化"。
    seed_member(&state.db, "m_conv", "w1", "conv", "c_conv", "active").await;
    sqlx::query("UPDATE world_members SET joined_at = $1 WHERE id = 'm_conv'")
        .bind(now + 1000)
        .execute(&state.db)
        .await
        .unwrap();

    let (s, e) = window();
    let m = conversion_block(&state.db, s, e).await.unwrap();
    assert_eq!(m["status"], json!("ok"), "{m}");
    assert_eq!(m["viewersCounted"], json!(2), "veteran 是老玩家，不进分母：{m}");
    assert_eq!(m["convertedCount"], json!(1), "只有 conv 是真转化（already 是先入场后看直播）：{m}");
    assert_eq!(m["value"], json!(0.5));
    assert!(
        m["convertedCount"].as_i64().unwrap() <= m["viewersCounted"].as_i64().unwrap(),
        "分子必须 ≤ 分母（否则会出现 >100% 的转化率）"
    );
}

/// 同一个人看多场只算一个人（按 `user_id` 去重取 `MIN(first_seen_at)`）——
/// 否则重度观众会把分母灌水、转化率被稀释。
#[tokio::test]
async fn conversion_deduplicates_a_viewer_across_sessions() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 2).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await;
    seed_live_session(&state, "lvs2", "w1", 0, 0).await;
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;

    for sid in ["lvs1", "lvs2"] {
        let (st, _) =
            send(&state, "GET", &format!("/api/live/sessions/{sid}/feed"), &token(&state, "u1"), None)
                .await;
        assert_eq!(st, StatusCode::OK);
    }
    assert_eq!(count(&state, "SELECT COUNT(*) FROM live_viewers").await, 2, "两场各一行足迹");

    let (s, e) = window();
    let m = conversion_block(&state.db, s, e).await.unwrap();
    assert_eq!(m["viewersCounted"], json!(1), "同一个人看两场只算一个人：{m}");
}

/// 转化率挂在 `/admin/metrics/overview` 的 `liveStage` 顶层键上（后台一屏可见）。
#[tokio::test]
async fn conversion_is_exposed_on_the_admin_overview() {
    let state = test_state().await;
    let (st, b) =
        send(&state, "GET", "/api/admin/metrics/overview", &admin_token(&state, "operator"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["liveStage"]["metric"], json!("liveViewerToPlayerConversion"), "{b}");
    // 默认关闭 → 三态里的第一态。
    assert_eq!(b["liveStage"]["status"], json!("entry_not_open"));
    assert_eq!(b["liveStage"]["value"], json!(null));
    // `?slo=0` 的减负开关同样覆盖本段，且与三态互不混淆。
    let (_, b) = send(
        &state,
        "GET",
        "/api/admin/metrics/overview?slo=0",
        &admin_token(&state, "operator"),
        None,
    )
    .await;
    assert_eq!(b["liveStage"]["status"], json!("skipped_by_request"));
}

/// 延迟缓冲的**有效性**度量：`withheldPreemptiveRate` = 播出前拦住的占比。
/// 低于 1 就是"延迟拍数配得不够"的直接证据（T5 预案的上调依据）。
#[tokio::test]
async fn conversion_block_reports_delay_buffer_effectiveness() {
    let state = test_state().await;
    seed_user(&state.db, "u1").await;
    seed_world_with_ticks(&state, "w1", 4).await;
    seed_live_session(&state, "lvs1", "w1", 0, 0).await; // 零延迟 → 撤下必然是事后
    open_flag(&state, crate::flags::SCOPE_GLOBAL, "").await;
    let tk = token(&state, "u1");
    let rv = admin_token(&state, "reviewer");

    send(&state, "GET", "/api/live/sessions/lvs1/feed", &tk, None).await; // 推进水位线
    for eid in ["ev_w1_0", "ev_w1_1"] {
        let (st, _) = send(
            &state,
            "POST",
            "/api/admin/live/sessions/lvs1/withhold",
            &rv,
            Some(json!({ "eventId": eid })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }

    let (s, e) = window();
    let m = conversion_block(&state.db, s, e).await.unwrap();
    assert_eq!(m["withheldTotal"], json!(2));
    assert_eq!(m["withheldPreemptive"], json!(0), "零延迟 → 一条都没能在播出前拦住");
    assert_eq!(m["withheldPreemptiveRate"], json!(0.0), "{m}");
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 源码级红线
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 本模块对 `world_events` **只读不写**。
///
/// 一旦有人为了"撤下"而顺手写一条 `UPDATE world_events SET moderation=...`，
/// 世界事实就被播出层改写了（§0.3），且参赛者会突然看不见自己刚经历过的事。
/// 撤下的正确落点是 `live_withholds` 独立表。
#[test]
fn red_line_module_never_writes_world_events() {
    // 扫的是**代码**不是注释：本文件的模块头就在反复讲"不得 UPDATE world_events"，
    // 连注释一起扫会把这条红线的说明本身判成违规。
    let code = strip_comments(include_str!("mod.rs"));
    for forbidden in ["INSERT INTO world_events", "UPDATE world_events", "DELETE FROM world_events"] {
        assert!(
            !code.contains(forbidden),
            "🔴 直播场模块出现了对世界事实的写入「{forbidden}」——撤下应写 live_withholds，不得改写 world_events"
        );
    }
    // 读是允许且必须的（播出面要从这里取事件）。
    assert!(code.contains("FROM world_events"), "播出面本就要读 world_events");
}

/// 去掉行注释后的源码（源码级红线用例专用）。
/// 本仓库的 SQL 字面量里不含 `//`，故按首个 `//` 截断是安全的。
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 🔴 错峰调度的「**直播场永不延后**」判据未被本批次改动。
///
/// `runtime::offpeak::Config::is_live_room` 的两条判据必须仍是**世界自身的节奏属性**
/// （`room_type` / `tick_per_day`），不得因为本模块引入了 `live_sessions` 就变成
/// 「有没有定档记录」——那会让运营建一条定档记录就顺手改掉一个世界的调度行为，
/// 是两个不该耦合的旋钮。同时本模块不得反向去碰调度器。
#[test]
fn red_line_offpeak_live_exemption_untouched() {
    let runtime_src = include_str!("../runtime/mod.rs");
    assert!(
        runtime_src.contains(r#"room_type == "arena" || tick_per_day >= self.live_tick_per_day"#),
        "🔴 错峰的直播场豁免判据被改了：直播是定时的，延后它等于毁掉「今晚 2 小时跑完一阶段」"
    );
    assert!(
        !runtime_src.contains("live_sessions"),
        "🔴 调度器读到了播出层的定档表——播出排期与引擎拍排期必须解耦"
    );
    let code = strip_comments(include_str!("mod.rs"));
    for forbidden in ["schedule_tick", "schedule_due_ticks", "world_ticks SET", "INSERT INTO world_ticks"] {
        assert!(!code.contains(forbidden), "🔴 直播场模块动了引擎拍的排期或记账：{forbidden}");
    }
}

/// 参数化自检（§0.2）：延迟拍数 / 提前量 / 限频 / 容量 / 宽限 / 门槛全部可配且默认值保守。
#[test]
fn every_product_rule_is_a_parameter_with_a_conservative_default() {
    // §15 第 4 层原文「延迟 1-2 拍」——默认取上限。
    assert_eq!(DEFAULT_DELAY_TICKS, 2);
    assert!((1..=2).contains(&DEFAULT_DELAY_TICKS), "默认延迟须落在规格给的 1-2 拍区间内");
    // T5 门槛原文「≥2%」作为默认值而非常量语义。
    assert!((DEFAULT_CONVERSION_MIN - 0.02).abs() < f64::EPSILON);
    // 负值配置不得静默变成"提前播"。
    assert_eq!(default_delay_ticks().min(0), 0);
    // 每个参数都有 env 名（改一个数不需要改代码）。
    for name in [
        ENV_DELAY_TICKS,
        ENV_ANNOUNCE_LEAD_MS,
        ENV_SESSION_CAPACITY,
        ENV_DRAIN_GRACE_MS,
        ENV_DANMAKU_RATE,
        ENV_DANMAKU_WINDOW_MS,
        ENV_DANMAKU_MAX_LEN,
        ENV_CONVERSION_MIN,
    ] {
        assert!(name.starts_with("MUSE_LIVE_"), "参数 env 名须统一前缀：{name}");
    }
}

/// 面具的三条性质：同场稳定 · 跨场不可关联 · 不含真身。
#[test]
fn masked_handle_is_stable_per_session_and_unlinkable_across_sessions() {
    let a = masked_handle("lvs1", "u1");
    assert_eq!(a, masked_handle("lvs1", "u1"), "同场同人必须稳定");
    assert_ne!(a, masked_handle("lvs2", "u1"), "跨场必须不可关联");
    assert_ne!(a, masked_handle("lvs1", "u2"));
    assert!(a.starts_with("观众") && !a.contains("u1"));
}

/// 状态机表：合法迁移是白名单，其余一律拒（含自环与回头路）。
#[test]
fn transition_table_is_a_one_way_whitelist() {
    assert!(transition_allowed(STATUS_SCHEDULED, STATUS_LIVE));
    assert!(transition_allowed(STATUS_SCHEDULED, STATUS_CANCELED));
    assert!(transition_allowed(STATUS_LIVE, STATUS_ENDED));
    for (from, to) in [
        (STATUS_LIVE, STATUS_SCHEDULED),
        (STATUS_ENDED, STATUS_LIVE),
        (STATUS_CANCELED, STATUS_LIVE),
        (STATUS_LIVE, STATUS_LIVE),
        (STATUS_ENDED, STATUS_ENDED),
    ] {
        assert!(!transition_allowed(from, to), "{from} → {to} 不该被允许");
    }
}
