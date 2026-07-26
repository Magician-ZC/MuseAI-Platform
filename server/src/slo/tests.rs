//! 叙事质量 SLO 单测（sqlite::memory + 真实迁移）。
//!
//! 覆盖：四个可算指标的聚合正确性 · 🔴 NPC 不得污染基尼（红线口径）· 有效戏份口径
//! （consent_request 不算戏）· actors_json 规范化解析（`li` 不被 `lixia` 蹭戏）·
//! 未知 reason 保守计入强制收尾 · 空库不除零不报错 · 三项不可算指标返回明确无数据源标记 ·
//! 扫描上限触发时"明说跳过"而不是给残缺数。
//!
//! 指标**纯函数**本身的口径单测留在 `runtime::golden`（`gini_coefficient_*` /
//! `max_silent_streaks_*` / `conclusion_classification_*`）——那是黄金世界回归的口径基线，
//! 与本文件的"聚合查询正确性"是两层，不重复。

use serde_json::Value;
use sqlx::any::AnyPoolOptions;
use sqlx::AnyPool;

use super::*;

static INIT: std::sync::Once = std::sync::Once::new();

const DAY_MS: i64 = 86_400_000;
/// 固定的窗口基准时刻（不取 now，避免跨日抖动）：2026-07-01T00:00:00Z。
const T0: i64 = 1_782_950_400_000;

async fn test_db() -> AnyPool {
    INIT.call_once(sqlx::any::install_default_drivers);
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

/// 窗口 = [T0, T0+7天)，所有播种时刻取 T0+1天，恒落窗内。
fn cfg() -> SloConfig {
    SloConfig {
        days: 7,
        window_start: T0,
        window_end: T0 + 7 * DAY_MS,
        gini_max: 0.35,
        silent_streak_max: 3,
        scan_row_cap: 50_000,
    }
}

const IN_WINDOW: i64 = T0 + DAY_MS;
const BEFORE_WINDOW: i64 = T0 - 30 * DAY_MS;

// ---------------- 播种助手 ----------------

async fn ins_world(db: &AnyPool, id: &str, status: &str) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, created_at, updated_at) \
         VALUES (?, 'tpl_slo', 1, 'e1', 'p1', 'm1', 'idle', 'SLO 世界', ?, ?, ?)",
    )
    .bind(id)
    .bind(status)
    .bind(IN_WINDOW)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_member(db: &AnyPool, world: &str, character: &str) {
    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, status, joined_at) \
         VALUES (?, ?, ?, ?, 'active', ?)",
    )
    .bind(format!("wm_{world}_{character}"))
    .bind(world)
    .bind(format!("u_{character}"))
    .bind(character)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_contribution(db: &AnyPool, world: &str, character: &str, score_milli: i64, updated_at: i64) {
    sqlx::query(
        "INSERT INTO world_contributions (world_id, character_id, score_milli, milestone_score_milli, \
         settled_at, updated_at) VALUES (?, ?, ?, 0, 0, ?)",
    )
    .bind(world)
    .bind(character)
    .bind(score_milli)
    .bind(updated_at)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_tick(db: &AnyPool, world: &str, tick_no: i64, status: &str, tokens: i64, created_at: i64) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, cost_tokens, created_at) \
         VALUES (?, ?, ?, 0, ?, ?, ?)",
    )
    .bind(format!("tk_{world}_{tick_no}"))
    .bind(world)
    .bind(tick_no)
    .bind(status)
    .bind(tokens)
    .bind(created_at)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_event(db: &AnyPool, world: &str, tick_no: i64, seq: i64, event_type: &str, actors: &[&str]) {
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, occurred_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'public', ?)",
    )
    .bind(format!("ev_{world}_{tick_no}_{seq}"))
    .bind(world)
    .bind(tick_no)
    .bind(seq)
    .bind(format!("dev_{world}_{tick_no}_{seq}"))
    .bind(event_type)
    .bind(serde_json::to_string(actors).unwrap())
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_world_ended_audit(db: &AnyPool, world: &str, reason_with_ending: &str) {
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES (?, 'system', 'system', 'world.ended', ?, ?, ?)",
    )
    .bind(format!("aud_{world}"))
    .bind(world)
    .bind(reason_with_ending)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_character(db: &AnyPool, id: &str, withdrawn: i64) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES (?, ?, 'loc', 1, '{}', 'original', 'approved', ?, ?)",
    )
    .bind(id)
    .bind(format!("u_{id}"))
    .bind(withdrawn)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

fn m<'a>(slo: &'a Value, metric: &str) -> &'a Value {
    &slo["metrics"][metric]
}

// ============================================================================
// 指标 ①：叙事注意力基尼
// ============================================================================

/// 🔴 红线口径：NPC（世界固有角色）也入 `world_contributions`，不与 `world_members` 取交集
/// 就会把"NPC 拿了多少戏"算进玩家公平度。本用例造一个 NPC 独占大头的世界——
/// 取交集后三名玩家均分（基尼≈0，达标），不取交集则会被 NPC 拉爆。
#[tokio::test]
async fn attention_gini_excludes_world_controlled_npc() {
    let db = test_db().await;
    ins_world(&db, "w_npc", "running").await;
    for c in ["c_a", "c_b", "c_c"] {
        ins_member(&db, "w_npc", c).await;
        ins_contribution(&db, "w_npc", c, 3000, IN_WINDOW).await;
    }
    // NPC 无 owner、不是 world_member，却在贡献账本里独占大头。
    ins_contribution(&db, "w_npc", "npc_boss", 90_000, IN_WINDOW).await;

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let g = m(&slo, "attentionGini");
    assert_eq!(g["status"], "ok");
    assert_eq!(g["worldsCounted"], 1);
    assert_eq!(g["worstWorlds"][0]["members"], 3, "统计口径必须只含玩家成员（NPC 已被交集剔除）");
    let gini = g["worstWorlds"][0]["gini"].as_f64().unwrap();
    assert!(gini.abs() < 1e-9, "三名玩家均分应得基尼 0，实测 {gini}（不为 0 说明 NPC 混进来了）");
    assert_eq!(g["worldsOverThreshold"], 0);

    // 反证：同一批分数把 NPC 算进去会越过 T2 门槛——两种口径结论确实不同。
    let with_npc = gini_coefficient(&[3000, 3000, 3000, 90_000]);
    assert!(with_npc > 0.35, "把 NPC 算进玩家公平度会得到完全不同的结论：{with_npc}");
}

/// 逐世界基尼 + 平台级分布：单成员世界单列（n=1 时基尼恒 0，混进均值会把公平度洗白），
/// 越线世界进榜单，窗口外的世界不参与。
#[tokio::test]
async fn attention_gini_aggregates_per_world_and_platform_distribution() {
    let db = test_db().await;
    // 公平世界：三人 3000/3000/3000 → 0。
    ins_world(&db, "w_fair", "running").await;
    for c in ["f_a", "f_b", "f_c"] {
        ins_member(&db, "w_fair", c).await;
        ins_contribution(&db, "w_fair", c, 3000, IN_WINDOW).await;
    }
    // 独占世界：8000/1000/1000 → 0.467 > 0.35。
    ins_world(&db, "w_hog", "running").await;
    for (c, s) in [("h_a", 8000i64), ("h_b", 1000), ("h_c", 1000)] {
        ins_member(&db, "w_hog", c).await;
        ins_contribution(&db, "w_hog", c, s, IN_WINDOW).await;
    }
    // 单人世界：不参与门槛判定。
    ins_world(&db, "w_solo", "running").await;
    ins_member(&db, "w_solo", "s_a").await;
    ins_contribution(&db, "w_solo", "s_a", 5000, IN_WINDOW).await;
    // 窗口外世界：不该出现在统计里。
    ins_world(&db, "w_old", "ended").await;
    for (c, s) in [("o_a", 9000i64), ("o_b", 100)] {
        ins_member(&db, "w_old", c).await;
        ins_contribution(&db, "w_old", c, s, BEFORE_WINDOW).await;
    }

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let g = m(&slo, "attentionGini");
    assert_eq!(g["worldsCounted"], 2, "只有 w_fair / w_hog 参与（单人世界单列、窗口外剔除）: {g}");
    assert_eq!(g["worldsSingleMember"], 1);
    assert_eq!(g["worldsOverThreshold"], 1);
    assert_eq!(g["overThresholdRate"], 0.5);
    assert_eq!(g["threshold"], 0.35);
    assert_eq!(g["worstWorlds"][0]["worldId"], "w_hog", "最不公平的世界排榜首");
    assert!(g["worstWorlds"][0]["gini"].as_f64().unwrap() > 0.35);
    assert!(g["worstWorlds"][1]["gini"].as_f64().unwrap().abs() < 1e-9);
    assert!(!g["worstWorlds"].as_array().unwrap().iter().any(|w| w["worldId"] == "w_old"));

    // 平台级分布：桶边界含门槛，桶内世界数之和 == worldsCounted。
    let dist = g["distribution"].as_array().unwrap();
    let sum: i64 = dist.iter().map(|b| b["worlds"].as_i64().unwrap()).sum();
    assert_eq!(sum, 2, "分布桶之和必须等于参与统计的世界数: {dist:?}");
    assert!(g["medianGini"].as_f64().unwrap().abs() < 1e-9);
    assert!(g["maxGini"].as_f64().unwrap() > 0.35);
}

// ============================================================================
// 指标 ②：最长连续无有效戏份拍数
// ============================================================================

/// 口径三件事一次验：**最长连续**（不是总缺席）· 有效戏份只认 action/dialogue
/// （`consent_request` 不算戏——被同意门拦下的那一拍恰恰什么也没演成）· 拍域只含真跑过的拍。
#[tokio::test]
async fn silent_streak_counts_longest_gap_and_ignores_process_events() {
    let db = test_db().await;
    ins_world(&db, "w_s", "running").await;
    for c in ["s_always", "s_gap", "s_consent"] {
        ins_member(&db, "w_s", c).await;
    }
    // 拍域：0..4 共 5 拍（另加一条 failed 拍与一条零成本拍，均不入拍域）。
    for t in 0..5 {
        ins_tick(&db, "w_s", t, "done", 1000, IN_WINDOW).await;
    }
    ins_tick(&db, "w_s", 5, "failed", 0, IN_WINDOW).await;
    ins_tick(&db, "w_s", 6, "done", 0, IN_WINDOW).await;

    // s_always：每拍都有戏 → 0。
    for t in 0..5 {
        ins_event(&db, "w_s", t, t * 10, "action", &["s_always"]).await;
    }
    // s_gap：只在 0 / 3 / 4 有戏 → 空 1、2 两拍，最长连续 2。
    for t in [0i64, 3, 4] {
        ins_event(&db, "w_s", t, t * 10 + 1, "dialogue", &["s_gap"]).await;
    }
    // s_consent：每拍都被同意门拦下（consent_request 不构成戏份）→ 全 5 拍无戏。
    for t in 0..5 {
        ins_event(&db, "w_s", t, t * 10 + 2, "consent_request", &["s_consent"]).await;
    }

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let s = m(&slo, "silentStreak");
    assert_eq!(s["status"], "ok");
    assert_eq!(s["worldsCounted"], 1);
    assert_eq!(s["ticksCounted"], 5, "failed 拍与零成本拍不入拍域: {s}");
    assert_eq!(s["membersCounted"], 3);
    assert_eq!(s["maxStreak"], 5, "consent_request 不算戏份，该成员应为全程无戏");

    let worst = s["worstMembers"].as_array().unwrap();
    let find = |cid: &str| {
        worst.iter().find(|w| w["characterId"] == cid).unwrap_or_else(|| panic!("{cid} 应在榜单里")).clone()
    };
    assert_eq!(find("s_consent")["streak"], 5);
    assert_eq!(find("s_gap")["streak"], 2, "口径是最长连续无戏份，不是总缺席拍数");
    assert_eq!(find("s_always")["streak"], 0);

    // 门槛 3：只有 s_consent(5) 越线。
    assert_eq!(s["threshold"], 3);
    assert_eq!(s["membersOverThreshold"], 1);
    assert_eq!(s["overThresholdRate"], 1.0 / 3.0);
}

/// `actors_json` 必须**规范化解析**：`LIKE '%li%'` 会让 `li` 被 `lixia` 的出场蹭到戏份。
#[tokio::test]
async fn silent_streak_actor_parse_is_normalized_not_substring() {
    let db = test_db().await;
    ins_world(&db, "w_prefix", "running").await;
    ins_member(&db, "w_prefix", "li").await;
    ins_member(&db, "w_prefix", "lixia").await;
    ins_tick(&db, "w_prefix", 0, "done", 100, IN_WINDOW).await;
    ins_event(&db, "w_prefix", 0, 0, "action", &["lixia"]).await;

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let worst = m(&slo, "silentStreak")["worstMembers"].as_array().unwrap().clone();
    let streak = |cid: &str| {
        worst.iter().find(|w| w["characterId"] == cid).unwrap()["streak"].as_i64().unwrap()
    };
    assert_eq!(streak("li"), 1, "`li` 不得被 `lixia` 的出场蹭到戏份");
    assert_eq!(streak("lixia"), 0);
}

/// 拍域按窗口滚动：窗口外的拍不参与（否则"最近有没有人被晾着"会被历史稀释）。
#[tokio::test]
async fn silent_streak_respects_rolling_window() {
    let db = test_db().await;
    ins_world(&db, "w_win", "running").await;
    ins_member(&db, "w_win", "wc_a").await;
    // 窗口外 10 拍全无戏；窗口内 1 拍有戏。
    for t in 0..10 {
        ins_tick(&db, "w_win", t, "done", 100, BEFORE_WINDOW).await;
    }
    ins_tick(&db, "w_win", 10, "done", 100, IN_WINDOW).await;
    ins_event(&db, "w_win", 10, 0, "action", &["wc_a"]).await;

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let s = m(&slo, "silentStreak");
    assert_eq!(s["ticksCounted"], 1, "窗口外的拍不入拍域");
    assert_eq!(s["maxStreak"], 0);
}

// ============================================================================
// 指标 ③：强制收尾率
// ============================================================================

/// 分类 + 分母口径 + 未知保守：`mainline_complete` 自然，其余（含未知 reason 与
/// **没有审计行的已结束世界**）全部计入强制。
#[tokio::test]
async fn forced_conclusion_rate_counts_unknown_as_forced() {
    let db = test_db().await;
    // 5 个已结束世界 + 1 个仍在跑（不进分母）。
    for w in ["e_nat", "e_cap", "e_collapse", "e_weird", "e_noaudit"] {
        ins_world(&db, w, "ended").await;
    }
    ins_world(&db, "e_running", "running").await;

    ins_world_ended_audit(&db, "e_nat", "mainline_complete|ending=peace").await;
    ins_world_ended_audit(&db, "e_cap", "time_cap|ending=none").await;
    ins_world_ended_audit(&db, "e_collapse", "key_character_exit|ending=none").await;
    ins_world_ended_audit(&db, "e_weird", "something_new|ending=none").await;
    // e_noaudit 刻意不写审计行 → unaccounted。

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let f = m(&slo, "forcedConclusionRate");
    assert_eq!(f["status"], "ok");
    assert_eq!(f["endedWorlds"], 5, "分母 = worlds.status='ended'，running 世界不进分母");
    assert_eq!(f["classifiedWorlds"], 4);
    assert_eq!(f["unaccountedWorlds"], 1);
    assert_eq!(f["byKind"]["natural"], 1);
    assert_eq!(f["byKind"]["forced"], 1);
    assert_eq!(f["byKind"]["collapsed"], 1);
    assert_eq!(f["byKind"]["unknown"], 2, "未知 reason + 无审计行的已结束世界都算 unknown");
    assert_eq!(f["forcedWorlds"], 4, "自然收尾只有 mainline_complete 一种");
    assert_eq!(f["forcedRate"], 0.8);

    // byReason 保留 ending 与分类，供后台下钻。
    let by_reason = f["byReason"].as_array().unwrap();
    let nat = by_reason.iter().find(|r| r["reason"] == "mainline_complete").unwrap();
    assert_eq!(nat["ending"], "peace");
    assert_eq!(nat["kind"], "natural");
    assert_eq!(nat["forced"], false);
    assert!(by_reason.iter().find(|r| r["reason"] == "something_new").unwrap()["forced"]
        .as_bool()
        .unwrap());
}

#[test]
fn parse_ended_reason_splits_reason_and_ending() {
    assert_eq!(
        parse_ended_reason("mainline_complete|ending=peace"),
        ("mainline_complete".into(), "peace".into())
    );
    assert_eq!(parse_ended_reason("time_cap|ending=none"), ("time_cap".into(), "none".into()));
    // 没有分隔符（历史行/异常写入）：整串即 reason，不猜 ending。
    assert_eq!(parse_ended_reason("starved"), ("starved".into(), String::new()));
    assert_eq!(parse_ended_reason(""), (String::new(), String::new()));
}

// ============================================================================
// 指标 ④：同角色二次入世率
// ============================================================================

#[tokio::test]
async fn repeat_entry_rate_counts_characters_in_two_or_more_worlds() {
    let db = test_db().await;
    for w in ["r_w1", "r_w2", "r_w3"] {
        ins_world(&db, w, "running").await;
    }
    // 4 张未下架卡 + 1 张已下架卡（下架卡不进分母）。
    for c in ["r_a", "r_b", "r_c", "r_d"] {
        ins_character(&db, c, 0).await;
    }
    ins_character(&db, "r_gone", 1).await;

    // r_a：三个世界；r_b：两个世界；r_c：一个世界；r_d：从未入世。
    ins_member(&db, "r_w1", "r_a").await;
    ins_member(&db, "r_w2", "r_a").await;
    ins_member(&db, "r_w3", "r_a").await;
    ins_member(&db, "r_w1", "r_b").await;
    ins_member(&db, "r_w2", "r_b").await;
    ins_member(&db, "r_w1", "r_c").await;

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let r = m(&slo, "repeatEntryRate");
    assert_eq!(r["status"], "ok");
    assert_eq!(r["charactersTotal"], 4, "分母 = cloud_characters.withdrawn=0");
    assert_eq!(r["charactersEverJoined"], 3);
    assert_eq!(r["charactersTwoPlusWorlds"], 2, "r_a / r_b 二次入世");
    assert_eq!(r["repeatEntryRate"], 0.5);
    assert!((r["repeatAmongJoinedRate"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-12);
}

// ============================================================================
// 🔴 不可算的三项 + 无数据边界
// ============================================================================

/// 三项没有数据源的 SLO 必须**显式标注**：`status=no_data_source` + `value=null` +
/// 说得清"为什么算不了""补它要什么"。绝不允许退化成 0 或空对象——后台显示 `—` 与显示 `0%`
/// 是两个完全不同的经营判断。
#[tokio::test]
async fn unavailable_metrics_are_marked_not_zeroed() {
    let db = test_db().await;
    let slo = narrative_slo(&db, &cfg()).await.unwrap();

    // stateTextContradictionRate 已于 2026-07-26 随 CriticReport 落库（迁移 0030）转为可算，
    // 从本清单移出——清单缩短是数据源到位的**预期结果**，不是断言被放宽。
    let expected = ["oocAppealRate", "plotRepetitionRate"];
    let listed: Vec<&str> =
        slo["unavailable"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(listed, expected, "无数据源清单必须完整且稳定");

    for key in expected {
        let x = m(&slo, key);
        assert_eq!(x["status"], "no_data_source", "{key} 必须标为无数据源");
        assert!(x["value"].is_null(), "{key} 的值必须是 null，不许是 0：{x}");
        assert!(x["reason"].as_str().unwrap().len() > 20, "{key} 必须说清为什么算不了");
        assert!(x["blockedBy"].as_str().unwrap().len() > 10, "{key} 必须说清补齐它需要什么");
        assert!(x.get("rate").is_none() && x.get("count").is_none(), "{key} 不得混入任何看起来像数的字段");
    }

    // OOC 申诉率的原因必须点名 moderation_appeals 是内容风控申诉（这是最容易被误用的表）。
    assert!(m(&slo, "oocAppealRate")["reason"].as_str().unwrap().contains("moderation_appeals"));
    // 状态-文本矛盾率：CriticReport 已于 2026-07-26 落库（迁移 0030），本项**已脱离无数据源清单**。
    // 这条断言守的是「转正后不许回退成 no_data_source」——数据源没了要么修数据源，不许悄悄退回标注。
    let contradiction = m(&slo, "stateTextContradictionRate");
    assert_ne!(
        contradiction["status"], "no_data_source",
        "CriticReport 已落库，本项不得再标无数据源：{contradiction}"
    );
    assert!(
        contradiction.get("reason").is_none() && contradiction.get("blockedBy").is_none(),
        "已可算的指标不该带 reason/blockedBy（那是无数据源专用字段）：{contradiction}"
    );
}

/// 空库：不除零、不 panic、不报错，四个可算指标一律 status=ok 且计数为 0，
/// 均值类给 null（"没数据"不是"均值为 0"）。
#[tokio::test]
async fn empty_database_is_zero_safe() {
    let db = test_db().await;
    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    assert_eq!(slo["status"], "ok");

    let g = m(&slo, "attentionGini");
    assert_eq!(g["status"], "ok");
    assert_eq!(g["worldsCounted"], 0);
    assert_eq!(g["overThresholdRate"], 0.0);
    assert!(g["meanGini"].is_null() && g["medianGini"].is_null() && g["maxGini"].is_null());
    assert!(g["worstWorlds"].as_array().unwrap().is_empty());

    let s = m(&slo, "silentStreak");
    assert_eq!(s["membersCounted"], 0);
    assert_eq!(s["overThresholdRate"], 0.0);
    assert!(s["meanStreak"].is_null());

    let f = m(&slo, "forcedConclusionRate");
    assert_eq!(f["endedWorlds"], 0);
    assert_eq!(f["forcedRate"], 0.0, "无已结束世界时强制收尾率为 0/0 → 0，不得 NaN");

    let r = m(&slo, "repeatEntryRate");
    assert_eq!(r["repeatEntryRate"], 0.0);
    assert_eq!(r["repeatAmongJoinedRate"], 0.0);
}

/// 世界有成员但一拍未跑 / 全员零贡献：仍不除零、不误报不公平。
#[tokio::test]
async fn world_without_ticks_or_scores_does_not_break_metrics() {
    let db = test_db().await;
    ins_world(&db, "w_idle", "open").await;
    for c in ["i_a", "i_b"] {
        ins_member(&db, "w_idle", c).await;
        ins_contribution(&db, "w_idle", c, 0, IN_WINDOW).await; // 全零：没开演，不是不公平
    }

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    assert_eq!(m(&slo, "attentionGini")["worldsCounted"], 1);
    assert_eq!(m(&slo, "attentionGini")["worstWorlds"][0]["gini"], 0.0);
    assert_eq!(m(&slo, "attentionGini")["worldsOverThreshold"], 0);
    // 无拍域 → 该世界不进无戏份统计（没有回合就没有"被晾着"这个量）。
    assert_eq!(m(&slo, "silentStreak")["worldsCounted"], 0);
}

// ============================================================================
// 性能护栏：超上限"明说跳过"，不给残缺数、不拖垮后台
// ============================================================================

#[tokio::test]
async fn scan_row_cap_skips_metric_instead_of_returning_partial_numbers() {
    let db = test_db().await;
    ins_world(&db, "w_big", "running").await;
    for c in ["b_a", "b_b", "b_c"] {
        ins_member(&db, "w_big", c).await;
        ins_contribution(&db, "w_big", c, 1000, IN_WINDOW).await;
    }
    for t in 0..3 {
        ins_tick(&db, "w_big", t, "done", 100, IN_WINDOW).await;
        ins_event(&db, "w_big", t, t, "action", &["b_a"]).await;
    }

    let mut c = cfg();
    c.scan_row_cap = 2; // 三行贡献 / 三行拍 均超限
    let slo = narrative_slo(&db, &c).await.unwrap();

    for key in ["attentionGini", "silentStreak"] {
        let x = m(&slo, key);
        assert_eq!(x["status"], "skipped_too_large", "{key} 超上限应明说跳过：{x}");
        assert!(x["value"].is_null(), "{key} 超上限时不得给任何残缺数字");
        assert_eq!(x["rowCap"], 2);
        assert!(x["reason"].as_str().unwrap().contains("sloDays"), "应告诉运营怎么办");
    }
    // 两个 GROUP BY 指标与行数无关，照常出数。
    assert_eq!(m(&slo, "forcedConclusionRate")["status"], "ok");
    assert_eq!(m(&slo, "repeatEntryRate")["status"], "ok");
}

/// `?slo=0` 的跳过态与"无数据源""超上限"三态互不混淆。
#[test]
fn skipped_by_request_is_a_distinct_state() {
    let v = skipped_by_request(30);
    assert_eq!(v["status"], "skipped_by_request");
    assert_eq!(v["windowDays"], 30);
    assert_eq!(v["metrics"].as_object().unwrap().len(), 0);
    assert_eq!(v["unavailable"].as_array().unwrap().len(), 2);
}

/// 参数化：门槛与上限来自 env（VALIDATION §0.2 禁写死），且回显在响应里可自证。
#[tokio::test]
async fn thresholds_are_configurable_and_echoed() {
    let db = test_db().await;
    let mut c = cfg();
    c.gini_max = 0.20;
    c.silent_streak_max = 1;
    let slo = narrative_slo(&db, &c).await.unwrap();
    assert_eq!(slo["thresholds"]["attentionGiniMax"], 0.20);
    assert_eq!(slo["thresholds"]["silentStreakTicks"], 1);
    assert_eq!(slo["thresholds"]["scanRowCap"], 50_000);
    assert_eq!(m(&slo, "attentionGini")["threshold"], 0.20);
    assert_eq!(m(&slo, "silentStreak")["threshold"], 1);
    // 默认值即 VALIDATION 的门槛数值。
    let d = SloConfig::from_env(7, T0, T0 + 7 * DAY_MS);
    assert_eq!(d.gini_max, 0.35, "默认门槛必须是 T2 的 0.35");
    assert_eq!(d.silent_streak_max, 3);
}

// ============================================================================
// 单世界口径（黄金世界回归的消费面）
// ============================================================================

#[tokio::test]
async fn world_level_helpers_match_platform_definitions() {
    let db = test_db().await;
    ins_world(&db, "w_one", "ended").await;
    for (c, s) in [("o_a", 8000i64), ("o_b", 1000), ("o_c", 1000)] {
        ins_member(&db, "w_one", c).await;
        ins_contribution(&db, "w_one", c, s, BEFORE_WINDOW).await; // 单世界口径不看窗口
    }
    ins_contribution(&db, "w_one", "npc_x", 50_000, BEFORE_WINDOW).await;
    for t in 0..3 {
        ins_tick(&db, "w_one", t, "done", 100, BEFORE_WINDOW).await;
    }
    ins_event(&db, "w_one", 0, 0, "action", &["o_a", "o_b", "o_c"]).await;
    ins_world_ended_audit(&db, "w_one", "time_cap|ending=none").await;

    let (gini, counted) = world_attention_gini(&db, "w_one").await.unwrap();
    assert_eq!(counted, 3, "NPC 被交集剔除");
    assert!(gini > 0.35);

    let streaks = world_silent_streaks(&db, "w_one").await.unwrap();
    assert_eq!(streaks.len(), 3);
    assert!(streaks.values().all(|v| *v == 2), "第 1、2 拍全员无戏 → 最长连续 2：{streaks:?}");

    let (reason, ending) = world_conclusion(&db, "w_one").await.unwrap();
    assert_eq!(reason, "time_cap");
    assert_eq!(ending, "none");
    assert!(is_forced_conclusion(&reason));

    // 无审计行的世界：不 panic、返回空串（= Unknown → 保守计入强制）。
    let (r2, e2) = world_conclusion(&db, "w_missing").await.unwrap();
    assert!(r2.is_empty() && e2.is_empty());
    assert!(is_forced_conclusion(&r2));
}

/// 状态-文本矛盾率：分子只认一致性/因果问题，revisionSuggestions 不计入。
///
/// 分母是「窗口内已提交且落了 critic 行的拍」——`world_tick_critic` 每个已提交 tick 恒落一行
/// （哪怕三列全空），所以「跑了但很干净」与「从未落库」在库里能分开，分母才可信。
#[tokio::test]
async fn contradiction_rate_counts_only_real_contradictions() {
    let db = test_db().await;
    // 四拍：干净 / 一致性问题 / 因果问题 / 只有修订建议。
    for (tick, consistency, causal, suggestions) in
        [(0i64, 0i64, 0i64, 0i64), (1, 2, 0, 0), (2, 0, 1, 3), (3, 0, 0, 5)]
    {
        sqlx::query(
            "INSERT INTO world_tick_critic (world_id, tick_no, consistency_issue_count, \
             causal_issue_count, revision_suggestion_count, report_json, created_at) \
             VALUES ('w1', ?, ?, ?, ?, '{}', ?)",
        )
        .bind(tick)
        .bind(consistency)
        .bind(causal)
        .bind(suggestions)
        .bind(T0 + 1000) // 落在 cfg() 的窗口内
        .execute(&db)
        .await
        .unwrap();
    }

    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let x = m(&slo, "stateTextContradictionRate");
    assert_eq!(x["status"], "ok");
    assert_eq!(x["ticksTotal"], 4, "分母 = 全部已提交拍，含干净的那些");
    assert_eq!(x["ticksFlagged"], 2, "只有一致性/因果问题的两拍算矛盾");
    assert_eq!(
        x["value"].as_f64().unwrap(),
        0.5,
        "第 4 拍只有修订建议——那是「可以更好」不是「自相矛盾」，不得计入：{x}"
    );
    assert_eq!(x["consistencyIssues"], 2);
    assert_eq!(x["causalIssues"], 1);
}

/// 窗口内一拍未跑：显示「没测过」而不是「矛盾率 0」——与 §5 无数据源同一条纪律。
#[tokio::test]
async fn contradiction_rate_distinguishes_untested_from_zero() {
    let db = test_db().await;
    let slo = narrative_slo(&db, &cfg()).await.unwrap();
    let x = m(&slo, "stateTextContradictionRate");
    assert_eq!(x["status"], "no_data_in_window");
    assert!(x["value"].is_null(), "没测过必须是 null，显示 0% 会被读成「一次矛盾都没有」：{x}");
}
