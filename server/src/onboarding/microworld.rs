//! 单人微本模板（总规格 §13【拍板 21】「5 分钟单人微本」）。
//!
//! 一条 `world_templates` 数据 —— 由代码构造并**幂等入库**（`ensure_template`），不是迁移里的
//! INSERT：骨架里含一张必须能被 `serde_json::from_str::<CharacterCardV2>` 解析的 NPC 卡，
//! 手写 SQL/JSON 写错会静默降级（见 `presets` 模块头），所以一律结构体构造后序列化。
//!
//! ────────────────────────────────────────────────────────────────────────────
//! 🔴 为什么微本**必须自带 NPC**（改本文件前先读完这段）
//! ────────────────────────────────────────────────────────────────────────────
//! `runtime` 的推进门是 `member_ids.is_empty() || active_cards.len() < 2`，而 `active_cards`
//! **把世界固有角色（NPC）也算在内**（装配层 `worldCharacterEntries` 每 tick 注入）。
//! 于是「单角色微本」= **玩家角色数为 1**，不是「世界里只有一个角色」：
//! 玩家 1 张卡 + NPC ≥1 → `active_cards ≥ 2` → 过门。
//! 若骨架不带 `worldCharacters`，单人房会**永远**卡在 `insufficient_members`
//! （`runtime/mod.rs` 装配兜底处的注释已把这个死锁记在案），世界一拍都跑不出来。
//!
//! 本骨架给 **2 个 NPC** 而不是 1 个，两个理由：
//! ① 冗余——NPC 卡在装配层要过 `safety::moderate_and_queue`，未 Approved 的会被跳过；
//!    只放一个 NPC 时任何一次机审抖动都会让新手世界直接卡死。
//! ② 演出——茶棚里有两个各怀心事的人，玩家的卡才有戏可唱，「卡活了」才成立。
//! 代价是每拍多一次 role_decide（平权吃鸡：所有活跃卡逐一决策），见下方成本预算注释。
//!
//! ## 节奏与成本（VALIDATION.md §0.2 产品规则参数化：以下全部可 env 覆盖，禁止写死）
//!
//! - `maxWorldTicks` 默认 3：tick 0/1/2 正常推进，tick 3 触发世界时间上限终局
//!   （口径见 `runtime::reached_time_limit` 与其回归用例 `empty_skeleton_does_not_conclude_early`），
//!   即**最多 4 拍**必然收束——新手绝不会掉进一个跑不完的世界。
//! - `minWorldTicks` 默认 1：防秒结束地板。两个主线里程碑若在首拍就被推完，也要等到第 2 拍才结局，
//!   保证新手至少看到**两拍**演出 + 一次结算。
//! - `tickPerDay` 默认 1440（= 每分钟一拍，`86_400_000 / 1440 = 60_000ms`，与
//!   `runtime::schedule_due_ticks` 的排拍公式同源）→ 2~4 拍约 2~4 分钟跑完，
//!   落在 §13「5 分钟单人微本 / Time-to-first-magic ≤ 10 分钟」的预算内。
//! - 成本量级：每拍 = 1 director + 3 role_decide（玩家 1 + NPC 2）+ 1 arbiter + 1 writer + 1 critic
//!   ≈ 7 次模型调用，4 拍 ≈ 28 次。`world_budgets` 另按 `MUSE_ONBOARDING_TOKEN_BUDGET` /
//!   `MUSE_ONBOARDING_CNY_BUDGET_CENTS` 设**非零**熔断上限（`daily_token_budget=0` 会被 runtime
//!   当作无上限，是成本失控的经典口子，见 `CreateWorldParams::official` 的 B-2 注释）。
//!
//! ## 星级
//!
//! 固定 **1★**：新手门槛最低（`worlds::star_mileage_gate` 对 1-2★ 免历练准入，新卡 mileage=0
//! 必须能进），且产出封顶最保守（`assembly` 按星级剔除高档奖励）。

use serde_json::json;
use sqlx::{AnyPool, Row};

use crate::db::now_ms;
use crate::error::ApiError;

use muse_engine::character::types::*;

/// 微本模板 id（稳定常量：`ensure_template` 按它幂等 upsert，`onboarding_grants` 不存模板 id
/// ——模板是全局单例，不必逐行冗余）。
pub const MICROWORLD_TEMPLATE_ID: &str = "tpl_onboarding_micro";

/// 模板标题（同时是世界实例的默认标题）。
pub const MICROWORLD_TITLE: &str = "渡口茶棚的一夜";

/// 微本星级：恒 1★。
pub const MICROWORLD_STAR: i64 = 1;

// ---------- 参数化（env 覆盖，非法/缺省回落默认；范式同 invitations::parse_positive） ----------

const DEFAULT_MIN_TICKS: i64 = 1;
const DEFAULT_MAX_TICKS: i64 = 3;
const DEFAULT_TICK_PER_DAY: i64 = 1440;
const DEFAULT_TOKEN_BUDGET: i64 = 80_000;
const DEFAULT_CNY_BUDGET_CENTS: i64 = 20;

fn env_positive(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// 终局地板（拍）。
pub fn min_world_ticks() -> i64 {
    // 允许 0（无地板）故不能用 env_positive；但仍拒负数。
    std::env::var("MUSE_ONBOARDING_MIN_TICKS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(DEFAULT_MIN_TICKS)
}

/// 世界时间上限（拍）：`tick_no >= 本值` 即终局，兜底保证微本必然收束。
pub fn max_world_ticks() -> i64 {
    env_positive("MUSE_ONBOARDING_MAX_TICKS", DEFAULT_MAX_TICKS)
}

/// 微本排拍密度（拍/天）。
pub fn tick_per_day() -> i64 {
    env_positive("MUSE_ONBOARDING_TICK_PER_DAY", DEFAULT_TICK_PER_DAY)
}

/// 微本日 token 熔断上限（**必须非零**）。
pub fn daily_token_budget() -> i64 {
    env_positive("MUSE_ONBOARDING_TOKEN_BUDGET", DEFAULT_TOKEN_BUDGET)
}

/// 微本日成本熔断上限（分，**必须非零**）。
pub fn daily_cny_budget_cents() -> i64 {
    env_positive("MUSE_ONBOARDING_CNY_BUDGET_CENTS", DEFAULT_CNY_BUDGET_CENTS)
}

// ---------- NPC（世界固有角色） ----------

fn s(v: &str) -> String {
    v.to_string()
}

fn vs(items: &[&str]) -> Vec<String> {
    items.iter().map(|x| x.to_string()).collect()
}

fn npc_base(id: &str, name: &str, role: &str) -> CharacterCardV2 {
    CharacterCardV2 {
        schema_version: 2,
        id: s(id),
        lifecycle: CardLifecycle::Ready,
        identity: Identity {
            name: s(name),
            aliases: Vec::new(),
            narrative_role: Some(s(role)),
            importance: Importance::Major,
            source_work: None,
            legacy_v1_fields: None,
        },
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
    }
}

/// NPC ①：摆渡的老人。功能 = 抛出请托、推动主线一。
fn npc_ferryman() -> CharacterCardV2 {
    let mut c = npc_base("npc_onb_ferryman", "渡口老翁", "摆渡的");
    c.dramatic_core = DramaticCore {
        core_contradiction: s("他送了半辈子人过河，却不肯说自己在等谁回来"),
        surface_goal: s("在天亮前把一样东西托付给一个还肯信人的过客"),
        hidden_need: s("确认自己等的那件事不是白等"),
        denied_desire: Some(s("上一次船，自己过一回河")),
        core_fear: s("托付错了人，那样东西就再也回不去"),
        stakes: s("这一夜过去，渡口就要换人了"),
        bottom_lines: vs(&["不勉强不愿意的人", "不把实情说满"]),
        self_deception: None,
    };
    c.decision_model = DecisionModel {
        value_priorities: vs(&["托付之事", "过客的安危", "自己的名声"]),
        risk_appetite: s("耐心试探，宁可错过也不冒失"),
        default_strategies: vs(&["先讲一段旧事看对方反应", "把选择权交回给对方"]),
        escalation_path: vs(&["旁敲侧击", "说破一半", "把东西直接推到桌上"]),
        sacrifice_order: vs(&["时间", "旧物", "实情"]),
        known_biases: vs(&["偏爱沉默寡言的人"]),
        decision_rules: Vec::new(),
    };
    c.expression_fingerprint = ExpressionFingerprint {
        sentence_rhythm: s("慢，句尾常常拖一口气"),
        metaphor_sources: vs(&["水", "船", "夜色"]),
        say_vs_think_gap: s("嘴上说「随口一提」，其实每句都在试探"),
        signature_gestures: vs(&["用篙尖敲两下地"]),
        ..Default::default()
    };
    c.agency = Agency {
        initiative_triggers: vs(&["有生面孔在棚里坐下", "听见有人提起对岸"]),
        default_plans: vs(&["先给来客倒一碗热水"]),
        long_term_agenda: s("把手里那件旧物交出去，然后收篙"),
        leverage: vs(&["知道这条河上每一处能靠岸的地方"]),
        plot_seeds: vs(&["一只从不离身的旧木匣", "他记得每个渡过河却没再回来的人"]),
        refusal_rules: vs(&["不替人渡来路不明的货"]),
    };
    c
}

/// NPC ②：兜售旧物的行脚商。功能 = 制造张力、推动主线二（渡船来时的取舍）。
fn npc_pedlar() -> CharacterCardV2 {
    let mut c = npc_base("npc_onb_pedlar", "灰袍行脚商", "卖旧物的");
    c.dramatic_core = DramaticCore {
        core_contradiction: s("他靠贱买贵卖过活，却总在最后关头把最贵的那件送出去"),
        surface_goal: s("赶在渡船来之前把担子里的东西脱手"),
        hidden_need: s("有人认出他从前是做什么的"),
        denied_desire: Some(s("回到那个不必背着担子的日子")),
        core_fear: s("被人当成骗子，连解释的机会都没有"),
        stakes: s("误了这班船，他就得在渡口过冬"),
        bottom_lines: vs(&["不卖假货", "不趁人之危抬价"]),
        self_deception: Some(s("他说做生意讲究缘分，其实是不敢定价")),
    };
    c.decision_model = DecisionModel {
        value_priorities: vs(&["脱手担子", "留住往后的路", "眼前的价钱"]),
        risk_appetite: s("敢开高价，也敢当场折价"),
        default_strategies: vs(&["先夸对方眼力", "把货一件件摊开慢慢挑"]),
        escalation_path: vs(&["热络招呼", "降价", "干脆白送再讨个人情"]),
        sacrifice_order: vs(&["利钱", "货", "面子"]),
        known_biases: vs(&["把好奇当成买意"]),
        decision_rules: Vec::new(),
    };
    c.expression_fingerprint = ExpressionFingerprint {
        sentence_rhythm: s("急，一句叠一句，爱插市井口语"),
        metaphor_sources: vs(&["担子", "秤星", "路上的风"]),
        humor_style: Some(s("自嘲，把窘境说成笑话")),
        say_vs_think_gap: s("嘴上说「不值钱」，眼睛却盯着对方的手"),
        signature_gestures: vs(&["说话时把担绳在手上绕一圈"]),
        ..Default::default()
    };
    c.agency = Agency {
        initiative_triggers: vs(&["有人多看了他的担子一眼", "听见渡船的动静"]),
        default_plans: vs(&["先把最不值钱的一件摆在最上面"]),
        long_term_agenda: s("凑够盘缠，走完这条路"),
        leverage: vs(&["认得沿途所有当铺的规矩"]),
        plot_seeds: vs(&["担子最底下压着一件他从不报价的东西", "他背上那道疤与渡口的旧事对得上"]),
        refusal_rules: vs(&["不卖来路不明的东西"]),
    };
    c
}

// ---------- 骨架 ----------

/// 构造微本骨架 JSON（`world_templates.skeleton_json`）。
///
/// 结构对齐 `assembly::Skeleton` 与 `runtime::seed_narrative_layer` / `load_endgame_policy`：
/// - `mainlineNodes`：两个 **fated 里程碑**（`fated=true` → 硬约束；`threshold` → 里程碑进度门）。
///   两个都推完 → 引擎 `MainlineDone` → 过 `minWorldTicks` 地板即结局（快路径）；
///   推不完也无所谓，`maxWorldTicks` 兜底收束（慢路径）。两条路都通向「有始有终」。
/// - `worldCharacters`：2 个 NPC（见模块头「为什么必须自带 NPC」）。
/// - `locations`：**恰好一个**非秘境地点。刻意不给第二个——引擎按地点分组算碰撞，
///   两个地点会把 1 玩家 + 2 NPC 拆成互不见面的小组，微本立刻没戏可演。
/// - `hiddenContentPool`：三条隐藏支线，装配层按玩家卡的执念/恐惧词条加权挑 1 条绑定
///   （`assembly::rank_pool_items`，best-effort：无命中也会挑排序首个），这就是「这个世界认得我这张卡」的第一印象。
/// - `endgame`：`minWorldTicks` / `maxWorldTicks` 由 env 参数化。
/// - 刻意**不声明** `payoutTable`：确定性产出表属 §10，未验证功能默认关闭 —— 无表则 ③ 世界线层
///   只累计贡献分、不发放。微本的结算教学靠 ① 保底层历练（`progression::settle_idle_world_ending_tx`）。
/// - 刻意**不声明** `isSuperset`/`storylines`/`sampling`：微本内容池极小，走装配退化路径（全量装配）
///   即可，采样只会把 NPC 抽没。
pub fn skeleton_json() -> String {
    let sk = json!({
        "mainlineNodes": [
            {
                "id": "onb_m1",
                "summary": "茶棚里的某个人把一件不肯说破的事推到了你面前——接，还是不接。",
                "fated": true,
                "threshold": 1.0
            },
            {
                "id": "onb_m2",
                "summary": "天亮前渡船靠岸，走或留必须当场作答，没有第三种回答。",
                "fated": true,
                "threshold": 1.0
            }
        ],
        // 禁止谓词留空：微本是教学场，不引入额外硬约束（谓词语法非法会被 seed 静默丢弃，
        // 与其埋一条可能写错的规则，不如不写）。
        "forbiddenPredicates": [],
        "endingPool": [
            { "id": "onb_end_take", "baseWeight": 1.0 },
            { "id": "onb_end_leave", "baseWeight": 1.0 }
        ],
        "hiddenContentPool": [
            {
                "id": "onb_h_lamp",
                "themes": ["旧事", "亏欠", "记性", "账"],
                "template": "棚角那盏灯的灯罩上有一行被烟熏黑的小字，凑近看，和「{seed}」对得上。",
                "difficultyBase": 0.2
            },
            {
                "id": "onb_h_box",
                "themes": ["托付", "信任", "承诺", "路"],
                "template": "老翁的木匣搁在桌下，锁扣是开的——{name} 一眼看出，那把锁根本没打算锁住谁。",
                "difficultyBase": 0.3
            },
            {
                "id": "onb_h_scar",
                "themes": ["身份", "旧名号", "来路", "消息"],
                "template": "行脚商弯腰时露出的旧疤，和 {name} 心里那件「{fear}」隐隐连成一条线。",
                "difficultyBase": 0.3
            }
        ],
        "sideHookPool": [],
        // 微本不带世界道具目录：道具产出属 §10 确定性产出表与副本卡资产（R2 另一项，见模块 TODO），
        // 此处留空即「不发任何道具」，与资产单一写入路径（grant_item_tx）不冲突。
        "worldItems": [],
        "worldCharacters": [
            {
                "card": npc_ferryman(),
                "homeLocation": "onb_loc_teahouse",
                "carriedItemIds": [],
                "agendaNodes": ["onb_m1"]
            },
            {
                "card": npc_pedlar(),
                "homeLocation": "onb_loc_teahouse",
                "carriedItemIds": [],
                "agendaNodes": ["onb_m2"]
            }
        ],
        "locations": [
            {
                "id": "onb_loc_teahouse",
                "name": "渡口茶棚",
                "connections": [],
                "isSecretRealm": false,
                "residentItemIds": []
            }
        ],
        "assemblyRules": { "hiddenPerCharacter": 1, "endingWeightThreshold": 0.5 },
        "endgame": {
            "minWorldTicks": min_world_ticks(),
            "maxWorldTicks": max_world_ticks(),
            // keyCharacterIds 留空：微本只有一名玩家，把他设成关键角色等于「一退场就崩塌结算」，
            // 对新手是纯负体验；离场由 leave + 时间上限自然收束即可。
            "keyCharacterIds": []
        }
    });
    sk.to_string()
}

// ---------- 幂等入库 ----------

/// 幂等确保微本模板在库（返回当前 `version`，供世界实例钉住）。
///
/// 三条路径：
/// - 无行 → INSERT（version=1）。并发下第二个 INSERT 撞主键 → 回落到「读回」。
/// - 有行且骨架逐字节相同 → 直接返回现有 version（绝大多数调用走这条，零写入）。
/// - 有行但骨架不同（env 参数被运营改过 / 卡库改版）→ UPDATE 并 **version+1**。
///   版本递增不是形式主义：`worlds.template_version` 钉住的是「建房那一刻的模板」，
///   老世界继续按老版本跑（`create_world_tx` 的 §9.2 版本钉住），新世界才用新骨架。
///   UPDATE 带 `WHERE version = 旧值` 的 CAS，并发下输的一方读回即可，绝不双跳版本号。
///
/// 为什么模板是「代码 ensure」而不是「迁移 INSERT」：骨架里含结构体序列化出来的 NPC 卡
/// （见模块头），且 endgame 参数由 env 决定 —— 两者都不是迁移能表达的静态字面量。
pub async fn ensure_template(db: &AnyPool) -> Result<i64, ApiError> {
    let desired = skeleton_json();

    if let Some(v) = read_template(db, &desired).await? {
        return Ok(v);
    }

    // 无行 → 尝试插入。
    let res = sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, \
         version, moderation, withdrawn, star_rating, star_source, created_at) \
         VALUES ($1, $2, 'idle', $3, '{\"mode\":\"open\"}', 1, 1, 'approved', 0, $4, 'curated', $5)",
    )
    .bind(MICROWORLD_TEMPLATE_ID)
    .bind(MICROWORLD_TITLE)
    .bind(&desired)
    .bind(MICROWORLD_STAR)
    .bind(now_ms())
    .execute(db)
    .await;
    match res {
        Ok(_) => return Ok(1),
        // 并发抢插：另一方已建 → 读回。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {}
        Err(e) => return Err(e.into()),
    }
    read_template(db, &desired)
        .await?
        .ok_or_else(|| ApiError::Conflict("微本模板正在初始化，请稍后重试".into()))
}

/// 读现有模板行：骨架一致 → 返回 version；不一致 → 就地升版后返回新 version；无行 → None。
async fn read_template(db: &AnyPool, desired: &str) -> Result<Option<i64>, ApiError> {
    let Some(row) = sqlx::query("SELECT skeleton_json, version FROM world_templates WHERE id = $1")
        .bind(MICROWORLD_TEMPLATE_ID)
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    let stored: String = row.try_get("skeleton_json")?;
    let version: i64 = row.try_get("version")?;
    if stored == desired {
        return Ok(Some(version));
    }
    // 骨架变了 → 升版（CAS：只有拿到旧 version 的那一方写得进去）。
    let updated = sqlx::query(
        "UPDATE world_templates SET skeleton_json = $1, version = $2, title = $3, star_rating = $4, \
         star_source = 'curated', moderation = 'approved', withdrawn = 0 \
         WHERE id = $5 AND version = $6",
    )
    .bind(desired)
    .bind(version + 1)
    .bind(MICROWORLD_TITLE)
    .bind(MICROWORLD_STAR)
    .bind(MICROWORLD_TEMPLATE_ID)
    .bind(version)
    .execute(db)
    .await?;
    if updated.rows_affected() > 0 {
        return Ok(Some(version + 1));
    }
    // 输了 CAS：另一方已经升过版，读回它的结果（不再递归升版，避免版本号打架）。
    let v: Option<i64> = sqlx::query_scalar("SELECT version FROM world_templates WHERE id = $1")
        .bind(MICROWORLD_TEMPLATE_ID)
        .fetch_optional(db)
        .await?;
    Ok(v)
}

/// 骨架里全部 NPC 卡的机审文本（供测试做注入自检；生产路径由装配层 `moderate_and_queue` 把关）。
#[cfg(test)]
pub fn npc_cards() -> Vec<CharacterCardV2> {
    vec![npc_ferryman(), npc_pedlar()]
}

/// 骨架反序列化自检用：把骨架读成弱类型 Value（测试断言 worldCharacters 结构正确）。
#[cfg(test)]
pub fn skeleton_value() -> serde_json::Value {
    serde_json::from_str(&skeleton_json()).expect("骨架必须是合法 JSON")
}
