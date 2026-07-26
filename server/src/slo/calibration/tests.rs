//! 校准维度读数单测（sqlite::memory + 真实迁移）。
//!
//! 覆盖三条红线各自的锁：
//! - 🔴 **只读、不回灌引擎**：`calibration_readings_never_write_anything` /
//!   `calibration_readings_never_touch_narrative_state` /
//!   `calibration_module_source_contains_no_write_statements`；
//! - 🔴 **三态分得开**：`identity_dimension_separates_three_empty_states` /
//!   `realm_dimension_separates_three_empty_states`；
//! - 🔴 **不给单一综合分**：`calibration_readings_expose_no_composite_score`。
//!
//! 外加两维各自的读数正确性、口径复用（`bulk_world_facts_match_single_world_facts`）、
//! 与超限时"明说跳过"。

use serde_json::Value;
use sqlx::AnyPool;

use super::*;

const DAY_MS: i64 = 86_400_000;
/// 固定窗口基准（不取 now，避免跨日抖动）：2026-07-01T00:00:00Z。
const T0: i64 = 1_782_950_400_000;
const IN_WINDOW: i64 = T0 + DAY_MS;
const BEFORE_WINDOW: i64 = T0 - 30 * DAY_MS;

fn cfg() -> SloConfig {
    SloConfig {
        days: 7,
        window_start: T0,
        window_end: T0 + 7 * DAY_MS,
        gini_max: 0.35,
        ooc_appeal_rate_max: 0.10,
        silent_streak_max: 3,
        scan_row_cap: 50_000,
        calibration_world_cap: 300,
    }
}

async fn test_db() -> AnyPool {
    crate::testkit::test_pool().await
}

// ---------------- 播种助手 ----------------

async fn ins_template(db: &AnyPool, id: &str, skeleton_json: &str) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, \
         official, version, moderation, created_at) \
         VALUES ($1, '校准模板', 'idle', $2, '{\"mode\":\"open\"}', 1, 1, 'approved', $3)",
    )
    .bind(id)
    .bind(skeleton_json)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_world(db: &AnyPool, id: &str, status: &str, assembled: Option<&str>, created_at: i64) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, assembled_json, created_at, updated_at) \
         VALUES ($1, 'tpl_cal', 1, 'e1', 'p1', 'm1', 'idle', '校准世界', $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(status)
    .bind(assembled)
    .bind(created_at)
    .bind(created_at)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_member(db: &AnyPool, world: &str, character: &str) {
    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, status, joined_at) \
         VALUES ($1, $2, $3, $4, 'active', $5)",
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

async fn ins_contribution(db: &AnyPool, world: &str, character: &str, score_milli: i64) {
    sqlx::query(
        "INSERT INTO world_contributions (world_id, character_id, score_milli, \
         milestone_score_milli, settled_at, updated_at) VALUES ($1, $2, $3, 0, 0, $4)",
    )
    .bind(world)
    .bind(character)
    .bind(score_milli)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_tick(db: &AnyPool, world: &str, tick_no: i64, status: &str, error: Option<&str>) {
    sqlx::query(
        "INSERT INTO world_ticks (id, world_id, tick_no, base_revision, status, error, \
         cost_tokens, created_at) VALUES ($1, $2, $3, 0, $4, $5, 100, $6)",
    )
    .bind(format!("tk_{world}_{tick_no}"))
    .bind(world)
    .bind(tick_no)
    .bind(status)
    .bind(error)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_event(db: &AnyPool, world: &str, seq: i64, moderation: &str) {
    sqlx::query(
        "INSERT INTO world_events (id, world_id, tick_no, sequence, domain_event_id, event_type, \
         actors_json, visibility, moderation, occurred_at) \
         VALUES ($1, $2, 1, $3, $4, 'action', '[\"chA\"]', 'public', $5, $6)",
    )
    .bind(format!("ev_{world}_{seq}"))
    .bind(world)
    .bind(seq)
    .bind(format!("dev_{world}_{seq}"))
    .bind(moderation)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

async fn ins_ended_audit(db: &AnyPool, world: &str, reason_with_ending: &str) {
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES ($1, 'system', 'system', 'world.ended', $2, $3, $4)",
    )
    .bind(format!("aud_{world}"))
    .bind(world)
    .bind(reason_with_ending)
    .bind(IN_WINDOW)
    .execute(db)
    .await
    .unwrap();
}

/// 装配产物：身份分配。
fn assembled_identity(pairs: &[(&str, &str)]) -> String {
    let arr: Vec<Vec<&str>> = pairs.iter().map(|(c, i)| vec![*c, *i]).collect();
    serde_json::json!({ "assembly": { "identityAssignments": arr } }).to_string()
}

/// 装配产物：钉住的境界档。
fn assembled_realm(tier_id: &str) -> String {
    serde_json::json!({ "assembly": { "realmTier": { "id": tier_id, "label": "戏服" } } })
        .to_string()
}

const POOL_SKELETON: &str = r#"{"identityPool":[{"id":"lead","quota":1},{"id":"extra","quota":3}]}"#;
const REALM_SKELETON: &str = r#"{"realmTier":{"id":"t_low","label":"低档"}}"#;

fn dim<'a>(v: &'a Value, name: &str) -> &'a Value {
    &v["dimensions"][name]
}

fn identity_row<'a>(v: &'a Value, identity_id: &str) -> &'a Value {
    v["byIdentity"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["identityId"] == identity_id)
        .unwrap_or_else(|| panic!("没有身份桶 {identity_id}"))
}

fn realm_row<'a>(v: &'a Value, tier_id: &str) -> &'a Value {
    v["byRealmTier"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tierId"] == tier_id)
        .unwrap_or_else(|| panic!("没有戏服桶 {tier_id}"))
}

fn close(actual: &Value, expect: f64) {
    let got = actual.as_f64().unwrap_or_else(|| panic!("不是数字：{actual}"));
    assert!((got - expect).abs() < 1e-9, "期望 {expect}，实得 {got}");
}

// ============================================================================
// §1 纯函数：解析退化 + 相对份额归一化
// ============================================================================

/// 三份装配解析实现（runtime / admin 观测面 / 本模块）共同的退化契约：
/// 未装配 / 坏 JSON / 无该键 / 单条损坏 一律**退化而不 panic**，且坏条目只跳过自己。
#[test]
fn assembly_parsers_degrade_defensively() {
    assert!(parse_assignments(None).is_empty(), "未装配 → 空");
    assert!(parse_assignments(Some("{")).is_empty(), "坏 JSON → 空");
    assert!(parse_assignments(Some(r#"{"assembly":{}}"#)).is_empty(), "无该键 → 空");
    assert!(
        parse_assignments(Some(r#"{"assembly":{"identityAssignments":"x"}}"#)).is_empty(),
        "类型不符 → 空"
    );
    assert_eq!(
        parse_assignments(Some(
            r#"{"assembly":{"identityAssignments":[["chA","lead"],["chB"],"x",["","y"],["chC","extra"]]}}"#
        )),
        vec![("chA".to_string(), "lead".to_string()), ("chC".to_string(), "extra".to_string())],
        "坏条目逐条跳过，好条目一条不少"
    );

    assert!(parse_pinned_realm_id(None).is_none());
    assert!(parse_pinned_realm_id(Some("{")).is_none(), "坏 JSON → None");
    assert!(parse_pinned_realm_id(Some(r#"{"assembly":{}}"#)).is_none(), "无该键 → None（存量常态）");
    assert!(
        parse_pinned_realm_id(Some(r#"{"assembly":{"realmTier":{"id":"  "}}}"#)).is_none(),
        "空 id 钉不住任何东西 → None"
    );
    assert_eq!(
        parse_pinned_realm_id(Some(r#"{"assembly":{"realmTier":{"id":"t1"}}}"#)),
        Some("t1".to_string())
    );
}

/// 相对份额必须**先按世界归一化**：跨世界直接比原始分测的是世界寿命，不是身份失衡。
/// 退化世界（单人 / 全员零分）返回 None——「一个人的世界」与「还没开演」都没有公平度可言。
#[test]
fn relative_shares_normalize_within_world_and_skip_degenerate_worlds() {
    let long_world: BTreeMap<String, i64> =
        [("a".to_string(), 8_000_i64), ("b".to_string(), 2_000)].into_iter().collect();
    let short_world: BTreeMap<String, i64> =
        [("c".to_string(), 80_i64), ("d".to_string(), 20)].into_iter().collect();
    let long = relative_shares(&long_world).unwrap();
    let short = relative_shares(&short_world).unwrap();
    assert!((long["a"] - 1.6).abs() < 1e-9, "8:2 的世界里，多的那位拿到均分的 1.6 倍");
    assert!((long["b"] - 0.4).abs() < 1e-9);
    assert!(
        (long["a"] - short["c"]).abs() < 1e-9,
        "原始分差 100 倍、比例相同的两个世界必须给出同一个相对份额 —— 否则测的是世界寿命"
    );

    let solo: BTreeMap<String, i64> = [("a".to_string(), 5_i64)].into_iter().collect();
    assert!(relative_shares(&solo).is_none(), "单人世界份额恒为 1，无信息");
    let silent: BTreeMap<String, i64> =
        [("a".to_string(), 0_i64), ("b".to_string(), 0)].into_iter().collect();
    assert!(relative_shares(&silent).is_none(), "全员零分是「没开演」，不是「不公平」");
}

// ============================================================================
// §2 🔴 红线一：只读，绝不回灌引擎
// ============================================================================

/// 源码级锁：本模块**一条写语句都不许有**。
///
/// 理由与 `world_contributions` 独立于 `narrative_state_json` 单独建表同源（迁移 0025）：
/// 一旦按身份分组的戏份差被写回世界状态或进入引擎判定输入，「身份影响判定」就成立了，
/// 直接违反 §0.1 平权红线。注释行不计（模块头本身要谈论这件事）。
#[test]
fn calibration_module_source_contains_no_write_statements() {
    const SRC: &str = include_str!("../calibration.rs");
    let code: String = SRC
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for kw in ["INSERT INTO", "UPDATE ", "DELETE FROM", "ON CONFLICT"] {
        assert!(!code.contains(kw), "校准读数模块出现写语句关键字 `{kw}` —— 只读红线被破了");
    }
}

/// 跑完整套读数之后，库里**逐字节不变**。
#[tokio::test]
async fn calibration_readings_never_write_anything() {
    let db = test_db().await;
    seed_identity_scenario(&db).await;
    seed_realm_scenario(&db).await;

    let before = snapshot(&db).await;
    let out = calibration_readings(&db, &cfg()).await.unwrap();
    assert_eq!(out["status"], "ok", "先确认这一跑真的算出了东西，否则「没变」毫无意义");
    let after = snapshot(&db).await;
    assert_eq!(before, after, "校准读数只读：库内容必须逐字节不变");
}

/// 🔴 专项锁：`worlds.narrative_state_json` 与 `state_revision` 一个字节都不许动。
///
/// 这是「不回灌引擎」最直接的物理面——引擎每拍读的就是这两列。读数写进去就等于让
/// 结算侧数值进入引擎决策，`world_contributions` 当初单独建表要躲的正是这件事。
#[tokio::test]
async fn calibration_readings_never_touch_narrative_state() {
    let db = test_db().await;
    seed_identity_scenario(&db).await;

    let before = narrative_state_dump(&db).await;
    assert!(!before.is_empty(), "样本里得有世界，否则这个断言是空的");
    let _ = calibration_readings(&db, &cfg()).await.unwrap();
    assert_eq!(before, narrative_state_dump(&db).await, "narrative_state_json / state_revision 不许被读数改写");

    // 反向锁：读数产物里的任何数字都不得出现在世界状态里（即"没有人顺手把它 upsert 回去"）。
    for (_, state, _) in narrative_state_dump(&db).await {
        assert!(
            !state.contains("relativeShare") && !state.contains("meanShareGini"),
            "读数字段名出现在 narrative_state_json 里 —— 回灌发生了"
        );
    }
}

async fn snapshot(db: &AnyPool) -> String {
    let mut s = String::new();
    for r in sqlx::query(
        "SELECT id, status, state_revision, narrative_state_json, COALESCE(assembled_json, '') AS a \
         FROM worlds ORDER BY id ASC",
    )
    .fetch_all(db)
    .await
    .unwrap()
    {
        s.push_str(&format!(
            "w|{}|{}|{}|{}|{}\n",
            r.try_get::<String, _>("id").unwrap(),
            r.try_get::<String, _>("status").unwrap(),
            r.try_get::<i64, _>("state_revision").unwrap(),
            r.try_get::<String, _>("narrative_state_json").unwrap(),
            r.try_get::<String, _>("a").unwrap(),
        ));
    }
    for r in sqlx::query(
        "SELECT world_id, character_id, score_milli, updated_at FROM world_contributions \
         ORDER BY world_id ASC, character_id ASC",
    )
    .fetch_all(db)
    .await
    .unwrap()
    {
        s.push_str(&format!(
            "c|{}|{}|{}|{}\n",
            r.try_get::<String, _>("world_id").unwrap(),
            r.try_get::<String, _>("character_id").unwrap(),
            r.try_get::<i64, _>("score_milli").unwrap(),
            r.try_get::<i64, _>("updated_at").unwrap(),
        ));
    }
    for r in sqlx::query("SELECT id, status, COALESCE(error, '') AS e FROM world_ticks ORDER BY id ASC")
        .fetch_all(db)
        .await
        .unwrap()
    {
        s.push_str(&format!(
            "t|{}|{}|{}\n",
            r.try_get::<String, _>("id").unwrap(),
            r.try_get::<String, _>("status").unwrap(),
            r.try_get::<String, _>("e").unwrap(),
        ));
    }
    for r in sqlx::query("SELECT id, action, subject, reason FROM audit_logs ORDER BY id ASC")
        .fetch_all(db)
        .await
        .unwrap()
    {
        s.push_str(&format!(
            "a|{}|{}|{}|{}\n",
            r.try_get::<String, _>("id").unwrap(),
            r.try_get::<String, _>("action").unwrap(),
            r.try_get::<String, _>("subject").unwrap(),
            r.try_get::<String, _>("reason").unwrap(),
        ));
    }
    s
}

async fn narrative_state_dump(db: &AnyPool) -> Vec<(String, String, i64)> {
    sqlx::query("SELECT id, narrative_state_json, state_revision FROM worlds ORDER BY id ASC")
        .fetch_all(db)
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.try_get::<String, _>("id").unwrap(),
                r.try_get::<String, _>("narrative_state_json").unwrap(),
                r.try_get::<i64, _>("state_revision").unwrap(),
            )
        })
        .collect()
}

// ============================================================================
// §3 🔴 红线三：不给单一综合分
// ============================================================================

/// 校准是多目标的（公平 vs 戏剧性）。给一个「越高越好」的综合分会诱导运营去优化那个数字本身，
/// 所以：① 两维在 `ok` 态**都没有**一个代表全维的标量 `value`；② 全树不许出现判语式字段名。
#[tokio::test]
async fn calibration_readings_expose_no_composite_score() {
    let db = test_db().await;
    seed_identity_scenario(&db).await;
    seed_realm_scenario(&db).await;
    let out = calibration_readings(&db, &cfg()).await.unwrap();

    for name in ["identityShareBalance", "realmTierWorldQuality"] {
        let d = dim(&out, name);
        assert_eq!(d["status"], "ok", "{name} 应算得出真数");
        assert!(
            d.get("value").is_none() || d["value"].is_null(),
            "{name} 在 ok 态不得有代表整维的标量 value —— 那就是综合评分"
        );
    }

    // 判语式字段名黑名单：出现即说明有人在给「配得对不对」下结论。
    const FORBIDDEN: &[&str] = &[
        "score", "overallscore", "compositescore", "calibrationscore", "healthscore",
        "grade", "rating", "verdict", "recommendation", "passed", "pass", "rank", "best", "worst",
    ];
    let mut keys: Vec<String> = Vec::new();
    collect_keys(&out, &mut keys);
    for k in &keys {
        let lower = k.to_ascii_lowercase();
        assert!(
            !FORBIDDEN.contains(&lower.as_str()),
            "读数出现判语式字段 `{k}` —— 本模块只给分维度的事实，不给判语"
        );
    }
}

fn collect_keys(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, sub) in m {
                out.push(k.clone());
                collect_keys(sub, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|s| collect_keys(s, out)),
        _ => {}
    }
}

// ============================================================================
// §4 身份维
// ============================================================================

/// 场景：两个世界，`lead` 恒拿 8 成戏、`extra` 恒拿 2 成；第三个世界里 `extra` 一分没挣到
/// （**连贡献分行都没有**）；第四个世界有一位在场但没拿到站位的成员。
async fn seed_identity_scenario(db: &AnyPool) {
    ins_template(db, "tpl_pool", POOL_SKELETON).await;

    for w in ["w_id1", "w_id2"] {
        ins_world(db, w, "running", Some(&assembled_identity(&[("chA", "lead"), ("chB", "extra")])), IN_WINDOW).await;
        ins_member(db, w, "chA").await;
        ins_member(db, w, "chB").await;
        ins_contribution(db, w, "chA", 8_000).await;
        ins_contribution(db, w, "chB", 2_000).await;
    }

    // 🔴 chD 一分没挣到 → `world_contributions` 里**根本没有这一行**（挣到分才落行）。
    ins_world(db, "w_id3", "running", Some(&assembled_identity(&[("chC", "lead"), ("chD", "extra")])), IN_WINDOW).await;
    ins_member(db, "w_id3", "chC").await;
    ins_member(db, "w_id3", "chD").await;
    ins_contribution(db, "w_id3", "chC", 5_000).await;

    // chF 在场但那轮装配没给站位 → 进 (unassigned) 对照桶。
    ins_world(db, "w_id4", "running", Some(&assembled_identity(&[("chE", "lead")])), IN_WINDOW).await;
    ins_member(db, "w_id4", "chE").await;
    ins_member(db, "w_id4", "chF").await;
    ins_contribution(db, "w_id4", "chE", 3_000).await;
    ins_contribution(db, "w_id4", "chF", 1_000).await;
}

/// 身份维的核心问题：**某个身份是不是系统性拿到更少戏份**。
#[tokio::test]
async fn identity_dimension_detects_a_systematically_starved_identity() {
    let db = test_db().await;
    seed_identity_scenario(&db).await;
    let out = calibration_readings(&db, &cfg()).await.unwrap();
    let d = dim(&out, "identityShareBalance");

    assert_eq!(d["status"], "ok");
    assert_eq!(d["worldsWithAssignments"], 4);
    assert_eq!(d["worldsCounted"], 4);

    let lead = identity_row(d, "lead");
    let extra = identity_row(d, "extra");
    assert_eq!(lead["observations"], 4);
    assert_eq!(extra["observations"], 3);
    // lead: 1.6, 1.6, 2.0（w_id3 里对手 0 分）, 1.5（w_id4） → 均值 1.675
    close(&lead["meanRelativeShare"], (1.6 + 1.6 + 2.0 + 1.5) / 4.0);
    // extra: 0.4, 0.4, 0.0 → 均值 0.2666…
    close(&extra["meanRelativeShare"], (0.4 + 0.4 + 0.0) / 3.0);
    assert_eq!(
        d["lowestMeanIdentity"]["identityId"], "extra",
        "systematically starved 的那个身份必须能被直接指出来"
    );
    assert_eq!(d["highestMeanIdentity"]["identityId"], "lead");
    assert!(
        d["meanShareGini"].as_f64().unwrap() > 0.0,
        "两个身份平均份额差这么多，各身份之间的集中度不可能是 0"
    );

    // (unassigned) 是对照桶：在场但没站位的 chF 拿到 0.5 倍均分。
    let un = identity_row(d, IDENTITY_UNASSIGNED);
    assert_eq!(un["observations"], 1);
    close(&un["meanRelativeShare"], 0.5);
    close(&d["unassignedMeanRelativeShare"], 0.5);
    assert_eq!(d["identitiesObserved"], 2, "(unassigned) 不是配置出来的身份，不计入身份数");
}

/// 🔴 口径差：分母是 `world_members` 全集，**没有贡献分行的成员按 0 分计入**。
///
/// §4 `attentionGini` 走的是交集，看不见「一分没挣到」的人——而那正是本读数要抓的。
/// 若哪天有人把这里也改成交集，`extra` 的均值会从 0.267 跳到 0.4，
/// 「有个人被彻底晾着」这条信息当场消失。
#[tokio::test]
async fn identity_dimension_counts_members_without_contribution_rows_as_zero() {
    let db = test_db().await;
    ins_template(&db, "tpl_pool", POOL_SKELETON).await;
    ins_world(&db, "w_z", "running", Some(&assembled_identity(&[("chC", "lead"), ("chD", "extra")])), IN_WINDOW).await;
    ins_member(&db, "w_z", "chC").await;
    ins_member(&db, "w_z", "chD").await;
    ins_contribution(&db, "w_z", "chC", 5_000).await;
    // chD 没有贡献分行。

    // 前置事实核对：交集口径下这个人根本不存在。
    let joined: i64 = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS n FROM world_contributions wc \
         JOIN world_members wm ON wm.world_id = wc.world_id AND wm.cloud_character_id = wc.character_id \
         WHERE wc.world_id = 'w_z'",
    )
    .fetch_one(&db)
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(joined, 1, "交集口径只看得见 1 个人");

    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d = dim(&d, "identityShareBalance");
    let extra = identity_row(d, "extra");
    assert_eq!(extra["observations"], 1, "零分成员必须进样本");
    close(&extra["meanRelativeShare"], 0.0);
    assert_eq!(extra["zeroScoreObservations"], 1);
    close(&extra["zeroScoreRate"], 1.0);
    close(&identity_row(d, "lead")["meanRelativeShare"], 2.0);
}

/// 🔴 三态：入口没开过 / 窗口零样本 / 真数（可以是 0）——三者绝不可混同。
#[tokio::test]
async fn identity_dimension_separates_three_empty_states() {
    // ① 从未有模板声明 identityPool → entry_not_open（哪怕库里已经有世界）。
    let db = test_db().await;
    ins_template(&db, "tpl_bare", r#"{"mainlineNodes":[]}"#).await;
    ins_world(&db, "w_a", "running", None, IN_WINDOW).await;
    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d0 = dim(&d, "identityShareBalance");
    assert_eq!(d0["status"], "entry_not_open");
    assert!(d0["value"].is_null(), "入口没开过 → value 必须是 null（后台显示 —）");
    assert!(d0.get("observations").is_none(), "入口没开过时不许报任何计数，那会被当成 0 读");

    // ② 声明过、但窗口内没有可比观察 → no_data_in_window（计数照发，让人看出是哪一种空）。
    let db = test_db().await;
    ins_template(&db, "tpl_pool", POOL_SKELETON).await;
    ins_world(&db, "w_b", "running", Some(&assembled_identity(&[("chA", "lead"), ("chB", "extra")])), IN_WINDOW).await;
    ins_member(&db, "w_b", "chA").await;
    ins_member(&db, "w_b", "chB").await; // 谁都没挣到分
    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d1 = dim(&d, "identityShareBalance");
    assert_eq!(d1["status"], "no_data_in_window");
    assert!(d1["value"].is_null());
    assert_eq!(d1["worldsWithAssignments"], 1);
    assert_eq!(d1["worldsWithoutScore"], 1, "全员零分要与「单人世界」分开计数");
    assert_eq!(d1["observations"], 0);

    // ③ 真数可以是 0：完全均分的世界里各身份均值都是 1.0，那是 ok 不是空。
    let db = test_db().await;
    ins_template(&db, "tpl_pool", POOL_SKELETON).await;
    ins_world(&db, "w_c", "running", Some(&assembled_identity(&[("chA", "lead"), ("chB", "extra")])), IN_WINDOW).await;
    ins_member(&db, "w_c", "chA").await;
    ins_member(&db, "w_c", "chB").await;
    ins_contribution(&db, "w_c", "chA", 500).await;
    ins_contribution(&db, "w_c", "chB", 500).await;
    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d2 = dim(&d, "identityShareBalance");
    assert_eq!(d2["status"], "ok");
    close(&d2["meanShareGini"], 0.0);
    close(&identity_row(d2, "lead")["meanRelativeShare"], 1.0);
    close(&identity_row(d2, "extra")["meanRelativeShare"], 1.0);
}

/// 窗口是 cohort 口径（`worlds.created_at` 落窗）：窗口外开出的世界不进样本。
#[tokio::test]
async fn cohort_window_excludes_worlds_created_outside_it() {
    let db = test_db().await;
    ins_template(&db, "tpl_pool", POOL_SKELETON).await;
    ins_world(&db, "w_old", "running", Some(&assembled_identity(&[("chA", "lead"), ("chB", "extra")])), BEFORE_WINDOW).await;
    ins_member(&db, "w_old", "chA").await;
    ins_member(&db, "w_old", "chB").await;
    ins_contribution(&db, "w_old", "chA", 9_000).await;
    ins_contribution(&db, "w_old", "chB", 1_000).await;

    let out = calibration_readings(&db, &cfg()).await.unwrap();
    assert_eq!(out["worldsScanned"], 0);
    assert_eq!(dim(&out, "identityShareBalance")["status"], "no_data_in_window");
    assert_eq!(dim(&out, "realmTierWorldQuality")["status"], "entry_not_open", "这个库没有模板声明过 realmTier");
}

// ============================================================================
// §5 戏服维
// ============================================================================

/// 场景：`t_low` 两个世界（一个自然收尾且有阻断拍、一个还没跑完）、`t_high` 一个被掐掉的世界、
/// 外加一个没钉戏服的对照世界。
async fn seed_realm_scenario(db: &AnyPool) {
    ins_template(db, "tpl_realm", REALM_SKELETON).await;

    ins_world(db, "w_r1", "ended", Some(&assembled_realm("t_low")), IN_WINDOW).await;
    ins_ended_audit(db, "w_r1", "mainline_complete|ending=e_alliance").await;
    for i in 1..=5 {
        ins_tick(db, "w_r1", i, "done", None).await;
    }
    ins_tick(db, "w_r1", 6, "done", Some("blocked")).await;
    ins_event(db, "w_r1", 1, "approved").await;
    ins_event(db, "w_r1", 2, "pending").await;

    ins_world(db, "w_r2", "running", Some(&assembled_realm("t_low")), IN_WINDOW).await;
    ins_tick(db, "w_r2", 1, "done", None).await;

    ins_world(db, "w_r3", "ended", Some(&assembled_realm("t_high")), IN_WINDOW).await;
    ins_ended_audit(db, "w_r3", "starved").await;
    ins_tick(db, "w_r3", 1, "done", None).await;

    ins_world(db, "w_r4", "ended", None, IN_WINDOW).await;
    ins_ended_audit(db, "w_r4", "mainline_complete|ending=e_silence").await;
    ins_tick(db, "w_r4", 1, "done", None).await;
}

/// 戏服维读数 = 按钉住的戏服分桶的世界质量三指标；`(none)` 是对照桶，不得丢弃。
#[tokio::test]
async fn realm_dimension_groups_worlds_by_costume_and_keeps_the_untiered_control_bucket() {
    let db = test_db().await;
    seed_realm_scenario(&db).await;
    let out = calibration_readings(&db, &cfg()).await.unwrap();
    let d = dim(&out, "realmTierWorldQuality");

    assert_eq!(d["status"], "ok");
    assert_eq!(d["worldsScanned"], 4);
    assert_eq!(d["tiersObserved"], 2, "t_low 与 t_high；(none) 不算一件戏服");
    assert_eq!(d["worldsWithoutTier"], 1);

    let low = realm_row(d, "t_low");
    assert_eq!(low["worlds"], 2);
    assert_eq!(low["completion"]["natural"], 1);
    assert_eq!(low["completion"]["unfinished"], 1);
    close(&low["completion"]["completionRate"], 0.5);
    assert_eq!(low["blocking"]["blockedTicks"], 1);
    assert_eq!(low["blocking"]["engineTicks"], 7, "提交 6 + 阻断 1");
    close(&low["blocking"]["blockedRate"], 1.0 / 7.0);
    assert_eq!(low["blocking"]["eventsWithheld"], 1);
    close(&low["blocking"]["withheldRate"], 0.5);
    assert_eq!(low["endings"]["distinctEndings"], 1);

    let high = realm_row(d, "t_high");
    assert_eq!(high["worlds"], 1);
    assert_eq!(high["completion"]["forced"], 1, "starved = 强制收尾");
    close(&high["completion"]["completionRate"], 0.0);

    let none = realm_row(d, REALM_NONE);
    assert_eq!(none["worlds"], 1, "没钉戏服的世界是唯一的参照系，必须留着");
    close(&none["completion"]["completionRate"], 1.0);
}

/// 🔴 形状锁：戏服维**没有组内分布**（§6 全员统一），只能跨世界对比。
///
/// 谁若把身份维的分布形状套上来，会得到一个恒为 0 的假指标。这条用例盯住两件事：
/// 桶内不出现任何组内分布字段；`shapeRationale` 把「为什么是这个形状」写在响应里
/// （数会被复制走，注释不会）。
#[tokio::test]
async fn realm_dimension_has_no_within_group_distribution_by_design() {
    let db = test_db().await;
    seed_realm_scenario(&db).await;
    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d = dim(&d, "realmTierWorldQuality");

    let rationale = d["shapeRationale"].as_str().unwrap();
    assert!(rationale.contains("全员统一"), "响应必须自带形状理由");
    assert!(rationale.contains("跨世界对比"));
    assert!(d.get("byIdentity").is_none(), "戏服没有站位，不该有 byIdentity");
    for row in d["byRealmTier"].as_array().unwrap() {
        assert!(row.get("distribution").is_none(), "戏服桶内没有分布可看");
        assert!(row.get("meanRelativeShare").is_none(), "相对份额是身份维的单位，不属于戏服维");
        // 桶内允许出现的基尼只有一个：结局集中度（那是跨世界的结局分布，不是组内成员分布）。
        assert!(row["endings"]["concentrationGini"].is_number());
    }
}

/// 🔴 三态：入口没开过 / 窗口零样本 / 真数。
#[tokio::test]
async fn realm_dimension_separates_three_empty_states() {
    // ① 从未有模板声明 realmTier。
    let db = test_db().await;
    ins_template(&db, "tpl_bare", r#"{"mainlineNodes":[]}"#).await;
    ins_world(&db, "w_a", "ended", None, IN_WINDOW).await;
    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d0 = dim(&d, "realmTierWorldQuality");
    assert_eq!(d0["status"], "entry_not_open");
    assert!(d0["value"].is_null());
    assert!(d0.get("byRealmTier").is_none(), "入口没开过时不许发桶，空桶会被当成「都一样」读");

    // ② 声明过、但窗口内一个世界都没开出来。
    let db = test_db().await;
    ins_template(&db, "tpl_realm", REALM_SKELETON).await;
    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d1 = dim(&d, "realmTierWorldQuality");
    assert_eq!(d1["status"], "no_data_in_window");
    assert!(d1["value"].is_null());
    assert_eq!(d1["worldsScanned"], 0);

    // ③ 声明过、窗口内有世界但一个都没钉住戏服 → 这是**真数**（说明戏服根本没跑起来），
    //    不是零样本：全部落进 (none) 桶，tiersObserved = 0。
    let db = test_db().await;
    ins_template(&db, "tpl_realm", REALM_SKELETON).await;
    ins_world(&db, "w_c", "ended", None, IN_WINDOW).await;
    ins_ended_audit(&db, "w_c", "mainline_complete|ending=e1").await;
    let d = calibration_readings(&db, &cfg()).await.unwrap();
    let d2 = dim(&d, "realmTierWorldQuality");
    assert_eq!(d2["status"], "ok");
    assert_eq!(d2["tiersObserved"], 0);
    assert_eq!(d2["worldsWithoutTier"], 1);
    close(&realm_row(d2, REALM_NONE)["completion"]["completionRate"], 1.0);
}

/// 🔴 口径复用锁：批量取事实与单世界取事实**必须逐字段相等**。
///
/// 一旦两条路径漂移，表现是「同一批世界在看板与仿真回归里阻断率不一样」——
/// 那种 bug 不会有人从数字上看出来，只能靠这条用例挡住。
#[tokio::test]
async fn bulk_world_facts_match_single_world_facts() {
    let db = test_db().await;
    seed_realm_scenario(&db).await;

    let mut want: Vec<WorldQualityFacts> = Vec::new();
    let mut statuses: BTreeMap<String, String> = BTreeMap::new();
    for id in ["w_r1", "w_r2", "w_r3", "w_r4"] {
        let f = quality::collect_world_facts(&db, id).await.unwrap();
        statuses.insert(id.to_string(), f.status.clone());
        want.push(f);
    }
    let got = quality::collect_world_facts_bulk(&db, &statuses).await.unwrap();
    assert_eq!(got, want, "批量版与单世界版必须算出同一份事实");
}

// ============================================================================
// §6 保护被轮询端点：超限明说跳过
// ============================================================================

/// 世界数超过上限时**明说跳过**，不给残缺数——同 §4 逐行扫描指标的 `skipped_too_large`。
#[tokio::test]
async fn calibration_readings_skip_instead_of_guessing_when_over_cap() {
    let db = test_db().await;
    seed_realm_scenario(&db).await;
    let mut c = cfg();
    c.calibration_world_cap = 2; // 场景里有 4 个世界

    let out = calibration_readings(&db, &c).await.unwrap();
    assert_eq!(out["status"], "skipped_too_large");
    assert_eq!(out["worldScanCap"], 2);
    for name in ["identityShareBalance", "realmTierWorldQuality"] {
        let d = dim(&out, name);
        assert_eq!(d["status"], "skipped_too_large");
        assert!(d["value"].is_null(), "{name} 超限时不许给数");
        assert!(d["reason"].as_str().unwrap().contains("上限"));
    }
}

/// 空库：不除零、不 panic，两维各自给出自己的空态。
#[tokio::test]
async fn calibration_readings_are_zero_safe_on_empty_platform() {
    let db = test_db().await;
    let out = calibration_readings(&db, &cfg()).await.unwrap();
    assert_eq!(out["status"], "ok");
    assert_eq!(out["worldsScanned"], 0);
    for name in ["identityShareBalance", "realmTierWorldQuality"] {
        assert_eq!(dim(&out, name)["status"], "entry_not_open", "空库连模板都没有 → 这一维从未开工");
    }
}
