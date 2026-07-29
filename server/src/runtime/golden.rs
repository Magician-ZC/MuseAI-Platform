//! 黄金世界回归（`docs/VALIDATION.md` §4.1 验证基建第一件）。
//!
//! **它测什么**：一个内容固定、模型剧本化、world_id 钉死的标准样板世界，每次换 Prompt / 引擎版本 /
//! server 管线代码后重跑，比对**结构化产物**是否逐字节不变、关键剧情测试点是否仍然跑通、
//! 叙事质量指标与成本是否还在基线区间内。**它测的是"管线不回归"，不是"模型质量"**——
//! 见文件末尾「诚实划界」一节。
//!
//! **为什么落在 `#[cfg(test)]` 子模块**：`server` 是 binary-only crate（无 `lib.rs`），
//! `server/tests/` 集成测试与独立 bin 都访问不到 `process_tick_with_model` / `plan_sampling`
//! 这些 `pub(crate)`/私有 API。建成 `#[cfg(test)] mod golden` 自动进现有 `platform-test`
//! CI job，CI 改动为零（VALIDATION §4.1「落点受限」）。
//!
//! **确定性的四个前提**（缺一不可，改动任何一条都会让回归失去意义）：
//! 1. **world_id 钉死**：`world_id` 进采样种子（`assembly::instance_seed`），`create_world` 用
//!    uuid v4，故本模块**直接 INSERT 固定 id**（抄 `safety::testkit::seed_world` 的写法）。
//!    ⚠️ `stall_tracker()` 是**进程级全局**、按 world_id 分键，故不同剧情测试点必须用**不同**
//!    的固定 world_id，否则跨测试串味。
//! 2. **模型剧本化**：`ScriptedModel` 按 `(环节, tick_no, 角色 id)` 查表返回，零外部依赖。
//! 3. **只比对结构化产物，不比对 prose**：生产写作温度 `temperature_writer: 0.8` 是
//!    `runtime/mod.rs` 里的硬编码字面量，prose 在真实模型下不可能逐字相等；且 prose 从未落库
//!    （VALIDATION §4.2「剧情重复率」缺口）。比对范围 = decisions / outcomes / patch / events /
//!    contributions / cost_tokens / 终局 reason —— 全部经 DB 可观测且在剧本化模型下逐字节相等。
//! 4. **快照剔除墙钟与行主键**：`now_ms()` 时间戳（`occurred_at` / `settled_at` / `started_at`）
//!    与 `new_id()` 行主键（`world_events.id` / `audit_logs.id`）与叙事管线无关，不入快照。
//!    引擎侧的 id 反而**全部确定性**（`patch-{rev}` / `{patch_id}-ev-{seq}` /
//!    `dec:{run_id}:{T}:{cid}`），故它们照常入快照。
//!
//! **fixture 落点**：`golden/cards.json`（固定角色卡）+ `golden/skeleton.json`（固定世界骨架），
//! `include_str!` 编译期内联并随代码入库。**绝不放 `muse-objects/`**（gitignore 的运行时目录）。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::db::now_ms;
use crate::runtime::{insert_tick, process_tick_with_model, TickStatus};
// 叙事质量指标的口径与实现已提升为生产代码（`crate::slo`，VALIDATION §4.2 三件套第二件）——
// 本模块只借用同一套口径，绝不再维护第二份实现：回归与运营看板必须永远算的是同一个数。
use crate::slo::{classify_conclusion, gini_coefficient, is_forced_conclusion, max_silent_streaks, ConclusionKind};
use crate::worlds::load_world;

use muse_engine::character::types::CharacterCardV2;
use muse_engine::host::CancelFlag;
use muse_engine::model::{ModelCallSpec, ModelClient, ModelOutput};
use muse_engine::narrative::types::NarrativeState;
use muse_engine::EngineError;

// 复用 `runtime::tests` 的播种助手（只借可见性，不改逻辑）。
use super::tests::{i64_one, seed_model_routes, seed_user, seed_whisper, test_state, text_one};

// ============================================================================
// §1 固定标识：world_id / 模板 / 版本 —— 一律钉死，任何一处改动都改变回归基线
// ============================================================================

const GOLDEN_TEMPLATE_ID: &str = "golden-tpl-changan";
/// 模板版本进采样种子，必须固定。
const GOLDEN_TEMPLATE_VERSION: i64 = 1;
/// 实例星级（产出封顶输入）：3★ 允许 powerTier ≤ 3 的世界线产出落地。
const GOLDEN_STAR_RATING: i64 = 3;
const GOLDEN_ROUTES_VERSION: &str = "golden-routes-v1";
const GOLDEN_PROMPTS_VERSION: &str = "golden-prompts-v1";

/// 主回归世界（正常结局 + 关系演化 + 指标 + 成本 + 确定性重放共用同一个副本）。
const WORLD_MAIN: &str = "wld-golden-changan-main";
/// 世界线崩塌（BE）：关键角色永久退场。
const WORLD_COLLAPSE: &str = "wld-golden-changan-collapse";
/// 死亡同意门：不可逆结果门控 → 当事人授权 → 落定。
const WORLD_DEATH: &str = "wld-golden-changan-death";
/// 托梦干预：accepted whisper 喂入决策上下文并被消费。
const WORLD_WHISPER: &str = "wld-golden-changan-whisper";

/// 玩家成员：(cloud_character_id, user_id)。数组下标即入场次序，落成**固定且互不相同**的 `joined_at`。
pub(super) const GOLDEN_MEMBERS: &[(&str, &str)] = &[("shenyan", "ushen"), ("peizhao", "upei"), ("cuie", "ucui")];

/// 固定入场时刻基准 + 步长：`joined_at = BASE + idx * STEP`。
///
/// 🔴 **必须钉死且互不相同**。`assembly::load_active_cards` 与 `runtime` 组装成员卡时都用
/// `ORDER BY wm.joined_at ASC` 且**没有次级排序键**；若两名成员的 `joined_at` 撞在同一毫秒
/// （用 `now_ms()` 连续播种时必然如此），行序由数据库决定 ⇒ `perCharacterHooks` /
/// `difficultyNotes` 的顺序在两次重放之间漂移 ⇒ 逐字节比对失败。
/// 这条约束正是本回归第一次跑就抓到的问题，详见文件末尾「已知非确定性来源」。
pub(super) const GOLDEN_JOINED_AT_BASE: i64 = 1_700_000_000_000;
pub(super) const GOLDEN_JOINED_AT_STEP: i64 = 1_000;
/// 世界固有角色（NPC）：参与决策与贡献归因，但**不是 world_member**——算基尼前必须被交集剔除。
pub(super) const GOLDEN_NPC: &str = "lugong";

// ============================================================================
// §2 fixture：固定角色卡 + 固定世界骨架
// ============================================================================

const CARDS_JSON: &str = include_str!("golden/cards.json");
pub(super) const SKELETON_JSON: &str = include_str!("golden/skeleton.json");

/// 取一张固定角色卡（原始 JSON 值）。`__doc` 等注释键不是卡，按 id 精确取用。
pub(super) fn golden_card_value(id: &str) -> Value {
    let all: BTreeMap<String, Value> = serde_json::from_str(CARDS_JSON).expect("cards.json 必须是合法 JSON");
    all.get(id).unwrap_or_else(|| panic!("cards.json 缺少角色卡 {id}")).clone()
}

pub(super) fn golden_card_json(id: &str) -> String {
    let v = golden_card_value(id);
    // 解析一次，确保 fixture 始终是引擎可读的合法 CharacterCardV2（fixture 写错要在这里就炸）。
    let card: CharacterCardV2 =
        serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("角色卡 {id} 不符合 CharacterCardV2: {e}"));
    assert_eq!(card.id, id, "卡内 id 必须与 fixture 键一致");
    serde_json::to_string(&v).expect("卡 JSON 序列化")
}

/// 黄金世界的**可配置产品参数**（VALIDATION §0.2：终局规则一律参数化，禁止写死）。
///
/// 🔴 这四个字段是本 fixture 允许按剧情测试点变化的**全部**内容；骨架里其余一切
/// （主线摘要 / 禁止谓词 / 结局池 / 隐藏池 / 身份池 / 产出表 / 世界固有角色）在所有测试点之间
/// 逐字节一致——这才使"同一个黄金世界的不同剧情分支"这句话成立。
#[derive(Debug, Clone)]
pub(super) struct GoldenParams {
    /// 主线里程碑 m1 的阈值（回合强度累积到此值即完成主线 → 引擎产 MainlineDone）。
    milestone_threshold: f64,
    /// 终局地板：`tick_no < min_world_ticks` 前一律不触发终局（防秒结束）。
    min_world_ticks: i64,
    /// 世界时间上限（回退口径 = tick 计数）：`tick_no >= max_world_ticks` 即强制收尾。
    max_world_ticks: i64,
    /// 关键角色：其永久退场即世界线崩塌（BE）。
    key_character_ids: Vec<&'static str>,
}

impl GoldenParams {
    /// 主回归：三拍把主线里程碑推过阈值（5.00 + 4.75 + 4.50 = 14.25 ≥ 12.0），第三拍即自然收尾。
    pub(super) fn main() -> Self {
        Self {
            milestone_threshold: 12.0,
            min_world_ticks: 1,
            max_world_ticks: 50,
            key_character_ids: Vec::new(),
        }
    }

    /// 崩塌：里程碑高到永远推不完（排除自然收尾干扰），关键角色 cuie 退场即 BE。
    fn collapse() -> Self {
        Self {
            milestone_threshold: 1000.0,
            min_world_ticks: 1,
            max_world_ticks: 50,
            key_character_ids: vec!["cuie"],
        }
    }

    /// 死亡 / 托梦：终局全部关闭（地板与上限都推到跑不到的地方），只观察本分支自身的行为。
    fn no_endgame() -> Self {
        Self {
            milestone_threshold: 1000.0,
            min_world_ticks: 50,
            max_world_ticks: 50,
            key_character_ids: Vec::new(),
        }
    }
}

/// 组装最终 `skeleton_json`：注入 NPC 卡（角色卡单一事实源在 cards.json）+ 覆写参数化字段。
fn golden_skeleton(params: &GoldenParams) -> String {
    let mut sk: Value = serde_json::from_str(SKELETON_JSON).expect("skeleton.json 必须是合法 JSON");

    // 世界固有角色卡从 cards.json 注入（骨架里留 `"card": null` 占位，避免卡内容两份事实源）。
    sk["worldCharacters"][0]["card"] = golden_card_value(GOLDEN_NPC);

    // 主线里程碑阈值（参数化）。
    sk["mainlineNodes"][0]["threshold"] = json!(params.milestone_threshold);

    // 终局块（参数化）。
    sk["endgame"] = json!({
        "minWorldTicks": params.min_world_ticks,
        "maxWorldTicks": params.max_world_ticks,
        "keyCharacterIds": params.key_character_ids,
    });

    sk.to_string()
}

// ============================================================================
// §3 播种：固定 world_id 的黄金世界（直接 INSERT，绕开 `create_world` 的 uuid）
// ============================================================================

async fn seed_golden_template(db: &AnyPool, params: &GoldenParams) {
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, \
         version, moderation, star_rating, created_at) \
         VALUES ($1, '长安夜宴（黄金世界）', 'idle', $2, '{\"mode\":\"open\"}', 1, $3, 'approved', $4, $5)",
    )
    .bind(GOLDEN_TEMPLATE_ID)
    .bind(golden_skeleton(params))
    .bind(GOLDEN_TEMPLATE_VERSION)
    .bind(GOLDEN_STAR_RATING)
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

async fn seed_golden_char(db: &AnyPool, cid: &str, owner: &str) {
    sqlx::query(
        "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
         rights_declaration, moderation, withdrawn, created_at) \
         VALUES ($1, $2, 'local', 1, $3, 'original', 'approved', 0, $4)",
    )
    .bind(cid)
    .bind(owner)
    .bind(golden_card_json(cid))
    .bind(now_ms())
    .execute(db)
    .await
    .unwrap();
}

/// 建一个 **world_id 钉死**的 running 黄金世界。
///
/// 🔴 不走 `create_world`：它用 `new_id("wld")`（uuid v4），而 world_id 进 `instance_seed`
///（`assembly/mod.rs`），随机 id ⇒ 每次跑的是不同副本 ⇒ 回归无从谈起。此处直接 INSERT，
/// 字段与 `create_world_tx` 一一对应（含 `world_budgets` 那一行，否则预算预检读不到行）。
pub(super) async fn seed_golden_world(state: &AppState, world_id: &str, params: &GoldenParams) {
    let db = &state.db;
    seed_golden_template(db, params).await;
    seed_model_routes(db, GOLDEN_ROUTES_VERSION).await;
    for (cid, uid) in GOLDEN_MEMBERS {
        seed_user(db, uid).await;
        seed_golden_char(db, cid, uid).await;
    }

    let now = now_ms();
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, host_user_id, member_limit, \
         tick_per_day, timeline_mode, lethality, assembled_json, state_revision, narrative_state_json, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'idle', '长安夜宴', 'running', 'official', NULL, 10, 3, 'event', \
         'consent', NULL, 0, '{}', $7, $8)",
    )
    .bind(world_id)
    .bind(GOLDEN_TEMPLATE_ID)
    .bind(GOLDEN_TEMPLATE_VERSION)
    .bind(muse_engine::ENGINE_VERSION)
    .bind(GOLDEN_PROMPTS_VERSION)
    .bind(GOLDEN_ROUTES_VERSION)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO world_budgets (world_id, daily_token_budget, daily_cny_budget_cents, \
         spent_tokens_today, budget_day, fused, updated_at) VALUES ($1, 10000000, 0, 0, '', 0, $2)",
    )
    .bind(world_id)
    .bind(now)
    .execute(db)
    .await
    .unwrap();

    // 入场次序钉死：`joined_at` 互不相同（见 GOLDEN_JOINED_AT_BASE 的红字说明）。
    for (idx, (cid, uid)) in GOLDEN_MEMBERS.iter().enumerate() {
        sqlx::query(
            "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
             VALUES ($1, $2, $3, $4, '{}', 'active', $5)",
        )
        .bind(format!("wm-golden-{idx}"))
        .bind(world_id)
        .bind(uid)
        .bind(cid)
        .bind(GOLDEN_JOINED_AT_BASE + idx as i64 * GOLDEN_JOINED_AT_STEP)
        .execute(db)
        .await
        .unwrap();
    }
}

// ============================================================================
// §4 剧本化模型层：按 (环节, tick_no, 角色 id) 查表，完全确定、零外部依赖
// ============================================================================

/// 剧本键里的通配 tick（"任意拍"）。
const ANY_TICK: i64 = -1;
/// 剧本键里的通配角色（"任意角色 / 与角色无关的环节"）。
const ANY_CID: &str = "";

/// 各环节的固定 token 计量 `(input, output)`。
///
/// **刻意做成逐环节不同**：这样 `cost_tokens` 就不只是"调用次数 × 常数"，而是对**调用构成**敏感的
/// 指纹——有人给回合多加一次审校、或把导演/写作按组放大，成本基线立刻报警。
const AGENT_TOKENS: &[(&str, u32, u32)] = &[
    ("director", 120, 40),
    ("roleDecide", 200, 60),
    ("arbiter", 150, 50),
    ("writer", 260, 180),
    ("critic", 180, 20),
];

pub(super) fn agent_tokens(agent: &str) -> (u32, u32) {
    AGENT_TOKENS
        .iter()
        .find(|(a, _, _)| *a == agent)
        .map(|(_, i, o)| (*i, *o))
        .unwrap_or((10, 10))
}

/// 剧本化 ModelClient。
///
/// 与 `runtime::tests::MockModel`（环节感知、与调用次数解耦）同源，但多两个维度：
/// **拍号**（由驱动方在每拍开始前 `set_tick`）与**角色 id**（从 roleDecide 的 user prompt 头部解析），
/// 于是可以精确编排"第几拍谁做了什么"——正常结局 / 崩塌 / 死亡 / 托梦 / 多人关系五类剧情测试点
/// 就是靠这张表编出来的。
///
/// 🔴 **这不是 record-and-replay**：它回放的是**人写的剧本**，不是真实模型响应。见文件末「诚实划界」。
/// 真正的 record-and-replay 是 `muse_engine::replay`（工具）+ `runtime::record`（接线，默认关闭）；
/// 本类型可以被套进录制器当"待录的模型"，但一份用它录出来的录制里装的仍然只是这张剧本表——
/// 故录制产物带 `labels.responseSource=scriptedStub` 把这件事钉死。
pub(super) struct ScriptedModel {
    tick: AtomicI64,
    /// (agent, tick, cid) → 响应 JSON。查表顺序见 `lookup`。
    script: BTreeMap<(String, i64, String), String>,
    /// 每次调用的 (agent, user prompt)：供托梦用例断言"模型真的看见了托梦文本"。
    captured: Mutex<Vec<(String, String)>>,
}

impl ScriptedModel {
    /// 五个环节的兜底响应（合法 JSON，与 `runtime::tests::MockModel` 同款）。
    fn new() -> Self {
        let mut script: BTreeMap<(String, i64, String), String> = BTreeMap::new();
        let mut put = |agent: &str, body: &str| {
            script.insert((agent.to_string(), ANY_TICK, ANY_CID.to_string()), body.to_string());
        };
        put("director", r#"{"situation":"灯烛初上，长安夜宴开席，三人各自落座。"}"#);
        put("arbiter", r#"{"outcomes":[]}"#);
        put("writer", r#"{"prose":"席上灯影摇动，各人心事都压在杯底。"}"#);
        put("critic", r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#);
        put(
            "roleDecide",
            &decision_json("按兵不动", "端起酒盏，不接话头", false, "", &[], 60),
        );
        Self { tick: AtomicI64::new(0), script, captured: Mutex::new(Vec::new()) }
    }

    /// 编排一条剧本：`tick = ANY_TICK` 通配任意拍，`cid = ANY_CID` 通配任意角色。
    fn on(mut self, agent: &str, tick: i64, cid: &str, body: String) -> Self {
        self.script.insert((agent.to_string(), tick, cid.to_string()), body);
        self
    }

    pub(super) fn set_tick(&self, tick: i64) {
        self.tick.store(tick, Ordering::SeqCst);
    }

    pub(super) fn captured(&self) -> Vec<(String, String)> {
        self.captured.lock().unwrap().clone()
    }

    /// 查表：精确 →（本拍任意角色）→（任意拍本角色）→ 兜底。缺表即 panic——剧本必须写全。
    fn lookup(&self, agent: &str, cid: &str) -> String {
        let tick = self.tick.load(Ordering::SeqCst);
        let keys = [
            (agent.to_string(), tick, cid.to_string()),
            (agent.to_string(), tick, ANY_CID.to_string()),
            (agent.to_string(), ANY_TICK, cid.to_string()),
            (agent.to_string(), ANY_TICK, ANY_CID.to_string()),
        ];
        for k in keys {
            if let Some(v) = self.script.get(&k) {
                return v.clone();
            }
        }
        panic!("黄金世界剧本缺表：agent={agent} tick={tick} cid={cid}");
    }
}

/// 从 roleDecide 的 user prompt 头部解析角色 id（`build_decide_user_prompt` 的固定包裹：
/// 「以下是【仅你（<cid>）可见】的信息…」）。非 roleDecide 环节返回空串。
pub(super) fn cid_of_decide_prompt(user: &str) -> String {
    let Some(head) = user.strip_prefix("以下是【仅你（") else {
        return String::new();
    };
    head.split('）').next().unwrap_or_default().to_string()
}

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let cid =
            if spec.agent == "roleDecide" { cid_of_decide_prompt(&spec.user) } else { String::new() };
        self.captured.lock().unwrap().push((spec.agent.clone(), spec.user.clone()));
        let (input_tokens, output_tokens) = agent_tokens(&spec.agent);
        Ok(ModelOutput {
            content: self.lookup(&spec.agent, &cid),
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
        })
    }
}

/// 构造一条合法的 roleDecide 响应（`decision_id`/`character_id` 由引擎代码补齐，不来自模型）。
pub(super) fn decision_json(
    intent: &str,
    action: &str,
    will_speak: bool,
    purpose: &str,
    targets: &[&str],
    duration: i64,
) -> String {
    json!({
        "intent": intent,
        "action": action,
        "speak": { "willSpeak": will_speak, "purpose": purpose },
        "targets": targets,
        "acceptableCosts": [],
        "predictions": [],
        "duration": duration,
    })
    .to_string()
}

/// 驱动一拍：对齐剧本时钟 → 按当前 `state_revision` 排 tick → 走生产同路径 `process_tick_with_model`。
async fn drive_tick(
    state: &AppState,
    model: &Arc<ScriptedModel>,
    world_id: &str,
    tick_no: i64,
) -> TickStatus {
    let rev = load_world(&state.db, world_id).await.unwrap().state_revision;
    insert_tick(&state.db, world_id, tick_no, rev).await.unwrap();
    model.set_tick(tick_no);
    let mc: Arc<dyn ModelClient> = model.clone();
    process_tick_with_model(state, world_id, tick_no, mc).await.unwrap()
}

// ============================================================================
// §5 主回归剧本：三拍成戏，第三拍达成里程碑并自然收尾
// ============================================================================
//
// 编排意图（每一拍都同时服务多个断言）：
//   拍 0 初遇：全员发言 → 强度 5.00；沈砚→裴照【友善·道谢】、裴照→沈砚【中性·回礼】，
//              崔萼与卢内侍无角色目标（不产关系操作，也不制造 R4 冲突 ⇒ 本拍无模型仲裁）。
//   拍 1 交锋：沈砚与崔萼**同时指向裴照** ⇒ 规则层 R4 冲突 ⇒ 升级模型仲裁（本拍多一次 arbiter
//              调用，成本基线因此比其它拍高 200）；沈砚【敌对·挡】、裴照【敌对·逼】、
//              崔萼【友善·相助】。卢内侍不发言 ⇒ 强度 4.75。
//   拍 2 收束：崔萼【友善·救】裴照（另记 debt）、沈砚【友善·赠】崔萼、裴照收手（无目标）。
//              裴照与卢内侍不发言 ⇒ 强度 4.50；累计 14.25 ≥ 阈值 12.0 ⇒ 里程碑达成 ⇒
//              `run_event_step` 回合后复判 MainlineDone ⇒ 与状态 CAS 同事务自然收尾停机。
//
// 🔴 台词用词的硬约束：`relation_dynamics` 与 `IrreversibleRules` 都按**关键词正则**分类
//    action+intent 文本。上面每一句都刻意避开了会串类的词——例如「挡」属敌对词（写在道谢句里
//    会把友善判成敌对）、「决裂」属不可逆关系变更（会触发同意门，把这一拍的行动整个拦下）。
//    改台词前先读 `relation_dynamics::RelationRules` 与 `narrative::IrreversibleRules`。

pub(super) fn main_scripted_model() -> Arc<ScriptedModel> {
    let m = ScriptedModel::new()
        // ---- 拍 0：初遇，全员发言 ----
        .on(
            "roleDecide",
            0,
            "shenyan",
            decision_json("先示好，稳住场面", "举杯向裴照道谢，谢他当年那一次相助", true, "叙旧", &["peizhao"], 60),
        )
        .on(
            "roleDecide",
            0,
            "peizhao",
            decision_json("先看清楚", "坐直身子，向沈砚举杯回礼", true, "试探", &["shenyan"], 60),
        )
        .on(
            "roleDecide",
            0,
            "cuie",
            decision_json("把场面圆住", "起身替满席斟酒，唱一支旧曲", true, "圆场", &[], 60),
        )
        .on(
            "roleDecide",
            0,
            GOLDEN_NPC,
            decision_json("如实记录", "在灯下把席间言语记下", true, "记录", &[], 60),
        )
        // ---- 拍 1：交锋（沈砚与崔萼同时指向裴照 → R4 冲突 → 模型仲裁）----
        .on(
            "roleDecide",
            1,
            "shenyan",
            decision_json("拦住话头", "抢先把话头引开，挡在裴照面前", true, "岔开", &["peizhao"], 60),
        )
        .on(
            "roleDecide",
            1,
            "peizhao",
            decision_json("逼出实话", "拔刀出鞘半寸，逼沈砚正面回话", true, "质问", &["shenyan"], 60),
        )
        .on(
            "roleDecide",
            1,
            "cuie",
            decision_json("替人解围", "出言相助，替裴照解围", true, "解围", &["peizhao"], 60),
        )
        .on(
            "roleDecide",
            1,
            GOLDEN_NPC,
            decision_json("如实记录", "在灯下把方才的争执记下", false, "", &[], 60),
        )
        // ---- 拍 2：收束（救 → debt；赠 → 友善）----
        .on(
            "roleDecide",
            2,
            "shenyan",
            decision_json("交出把柄", "把半枚鱼符赠与崔萼，托她保管", true, "托付", &["cuie"], 60),
        )
        .on(
            "roleDecide",
            2,
            "peizhao",
            decision_json("暂且收手", "收刀入鞘，长出一口气", false, "", &[], 60),
        )
        .on(
            "roleDecide",
            2,
            "cuie",
            decision_json("护住人", "出手相救，把裴照护在身后", true, "护人", &["peizhao"], 60),
        )
        .on(
            "roleDecide",
            2,
            GOLDEN_NPC,
            decision_json("如实记录", "在灯下把这一段记下", false, "", &[], 60),
        );
    Arc::new(m)
}

/// 主回归总拍数：拍 2 里程碑达成即自然收尾（终局与状态 CAS 同事务，不需要额外一拍）。
pub(super) const MAIN_TICKS: i64 = 3;

/// 跑完整条主回归剧本，返回逐拍的 `TickStatus`。
async fn run_main_scenario(state: &AppState, model: &Arc<ScriptedModel>) -> Vec<TickStatus> {
    let mut out = Vec::new();
    for tick_no in 0..MAIN_TICKS {
        out.push(drive_tick(state, model, WORLD_MAIN, tick_no).await);
    }
    out
}

/// 主回归的**逐拍实测成本**（剧本化模型下可解析推导，见 `AGENT_TOKENS`）：
/// - 常规拍 = 导演 160 + 决策 4×260 + 写作 440 + 审校 200 = **1840**
/// - 交锋拍（拍 1，R4 冲突升级模型仲裁）= 1840 + 200 = **2040**
const COST_PLAIN_TICK: i64 = 1840;
const COST_ARBITRATED_TICK: i64 = 2040;
const COST_MAIN_TOTAL: i64 = COST_PLAIN_TICK * 2 + COST_ARBITRATED_TICK;

// ============================================================================
// §6 叙事质量指标（口径与实现在 `crate::slo`，本节只留回归侧的取数适配）
// ============================================================================
//
// 这三个指标（基尼 / 最长连续无戏份 / 收尾分类）曾经只活在本模块的 `#[cfg(test)]` 里，
// 生产代码拿不到。现已整体提升为 `crate::slo`（VALIDATION §4.2 验证基建第二件，进运营看板），
// **口径注释一并搬过去**——那些注释解释的是「为什么这么定义」，比代码本身更值钱。
// 本节只保留三个 `.unwrap()` 适配（回归里数据必然存在，失败即测试该炸），
// 保证「黄金世界回归看到的数」与「运营看板上的数」逐字节同源。

/// 读账本算基尼（单世界口径，见 `slo::world_attention_gini`）。
async fn attention_gini(db: &AnyPool, world_id: &str) -> (f64, usize) {
    crate::slo::world_attention_gini(db, world_id).await.unwrap()
}

/// 读库算最长连续无有效戏份拍数（单世界口径，见 `slo::world_silent_streaks`）。
async fn silent_streaks(db: &AnyPool, world_id: &str) -> BTreeMap<String, i64> {
    crate::slo::world_silent_streaks(db, world_id).await.unwrap()
}

/// 从审计取本世界的收尾 `(reason, ending)`（见 `slo::world_conclusion`）。
async fn conclusion_of(db: &AnyPool, world_id: &str) -> (String, String) {
    crate::slo::world_conclusion(db, world_id).await.unwrap()
}

// ============================================================================
// §7 结构化产物快照（逐字节比对口径）
// ============================================================================

/// 黄金世界跑完之后的**结构化产物快照**。
///
/// 收录（全部在剧本化模型下逐字节确定）：
/// - `world`：status / state_revision / game_time
/// - `narrativeState`：完整 `NarrativeState` —— 含 decisions 落定后的 **relations**、
///   characters、outlineNodes、pacingNotes、milestoneProgress、appliedPatchIds、timeline
/// - `ticks`：逐拍 status / cost_tokens / error（**成本基线**）
/// - `events`：逐条 tick/sequence/domainEventId/type/actors/visibility/audience/投影 summary
/// - `contributions` / `interventions` / `consents` / `conclusion` / `mileage` /
///   `worldlinePayouts` / `arenaRewards`
///
/// 刻意不收：`prose`（写作温度 0.8 硬编码，且从未落库）、`now_ms()` 墙钟、`new_id()` 行主键。
pub(super) async fn golden_snapshot(db: &AnyPool, world_id: &str) -> String {
    let w = load_world(db, world_id).await.unwrap();
    let narrative: Value = serde_json::from_str(&w.narrative_state_json).unwrap_or(Value::Null);

    let ticks: Vec<Value> = sqlx::query(
        "SELECT tick_no, status, cost_tokens, COALESCE(error, '') AS error FROM world_ticks \
         WHERE world_id = $1 ORDER BY tick_no ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        json!({
            "tickNo": r.try_get::<i64, _>("tick_no").unwrap(),
            "status": r.try_get::<String, _>("status").unwrap(),
            "costTokens": r.try_get::<i64, _>("cost_tokens").unwrap(),
            "error": r.try_get::<String, _>("error").unwrap(),
        })
    })
    .collect();

    let events: Vec<Value> = sqlx::query(
        "SELECT tick_no, sequence, domain_event_id, event_type, actors_json, visibility, \
         COALESCE(audience_json, '') AS audience, COALESCE(public_projection_json, '') AS pub_proj, \
         COALESCE(private_projections_json, '') AS priv_proj, moderation, ai_label \
         FROM world_events WHERE world_id = $1 ORDER BY sequence ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        json!({
            "tickNo": r.try_get::<i64, _>("tick_no").unwrap(),
            "sequence": r.try_get::<i64, _>("sequence").unwrap(),
            "domainEventId": r.try_get::<String, _>("domain_event_id").unwrap(),
            "type": r.try_get::<String, _>("event_type").unwrap(),
            "actors": r.try_get::<String, _>("actors_json").unwrap(),
            "visibility": r.try_get::<String, _>("visibility").unwrap(),
            "audience": r.try_get::<String, _>("audience").unwrap(),
            "publicProjection": r.try_get::<String, _>("pub_proj").unwrap(),
            "privateProjections": r.try_get::<String, _>("priv_proj").unwrap(),
            "moderation": r.try_get::<String, _>("moderation").unwrap(),
            "aiLabel": r.try_get::<i64, _>("ai_label").unwrap(),
        })
    })
    .collect();

    let contributions: Vec<Value> = sqlx::query(
        "SELECT character_id, score_milli, milestone_score_milli, settled_at FROM world_contributions \
         WHERE world_id = $1 ORDER BY character_id ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        json!({
            "characterId": r.try_get::<String, _>("character_id").unwrap(),
            "scoreMilli": r.try_get::<i64, _>("score_milli").unwrap(),
            "milestoneScoreMilli": r.try_get::<i64, _>("milestone_score_milli").unwrap(),
            // 只收"是否已结算"，不收墙钟时刻。
            "settled": r.try_get::<i64, _>("settled_at").unwrap() > 0,
        })
    })
    .collect();

    let interventions: Vec<Value> = sqlx::query(
        "SELECT id, character_id, kind, status FROM interventions WHERE world_id = $1 ORDER BY id ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        json!({
            "id": r.try_get::<String, _>("id").unwrap(),
            "characterId": r.try_get::<String, _>("character_id").unwrap(),
            "kind": r.try_get::<String, _>("kind").unwrap(),
            "status": r.try_get::<String, _>("status").unwrap(),
        })
    })
    .collect();

    let consents: Vec<Value> = sqlx::query(
        "SELECT event_kind, subject_character_ids, status FROM consent_requests WHERE world_id = $1 \
         ORDER BY event_kind ASC, subject_character_ids ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        json!({
            "eventKind": r.try_get::<String, _>("event_kind").unwrap(),
            "subjects": r.try_get::<String, _>("subject_character_ids").unwrap(),
            "status": r.try_get::<String, _>("status").unwrap(),
        })
    })
    .collect();

    let mut mileage: Vec<Value> = Vec::new();
    for (cid, _) in GOLDEN_MEMBERS {
        mileage.push(json!({
            "characterId": cid,
            "mileage": i64_one(db, "SELECT mileage FROM cloud_characters WHERE id = $1", cid).await,
        }));
    }

    let payouts: Vec<Value> = sqlx::query(
        "SELECT user_id, item_id, COALESCE(reward_hook_key, '') AS hook FROM backpacks ORDER BY user_id ASC, item_id ASC",
    )
    .fetch_all(db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        json!({
            "userId": r.try_get::<String, _>("user_id").unwrap(),
            "itemId": r.try_get::<String, _>("item_id").unwrap(),
            "rewardHookKey": r.try_get::<String, _>("hook").unwrap(),
        })
    })
    .collect();

    let rewards: Vec<Value> = sqlx::query(
        "SELECT character_id, kind, label FROM arena_rewards WHERE world_id = $1 \
         ORDER BY character_id ASC, kind ASC",
    )
    .bind(world_id)
    .fetch_all(db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        json!({
            "characterId": r.try_get::<String, _>("character_id").unwrap(),
            "kind": r.try_get::<String, _>("kind").unwrap(),
            "label": r.try_get::<String, _>("label").unwrap(),
        })
    })
    .collect();

    let (reason, ending) = conclusion_of(db, world_id).await;

    serde_json::to_string_pretty(&json!({
        "world": {
            "status": w.status,
            "stateRevision": w.state_revision,
            "gameTime": w.game_time,
            "assembled": normalized_assembly(w.assembled_json.as_deref()),
        },
        "narrativeState": narrative,
        "ticks": ticks,
        "events": events,
        "contributions": contributions,
        "interventions": interventions,
        "consents": consents,
        "conclusion": { "reason": reason, "ending": ending },
        "mileage": mileage,
        "worldlinePayouts": payouts,
        "arenaRewards": rewards,
    }))
    .unwrap()
}

/// `assembled_json` 的可比对形态：**只剔除两个墙钟字段**（`assembledAt` 与
/// `chapterState.sessionStartedAt`），其余（采样审计 / 身份分配 / 结局池 / 钩子文本 / NPC 条目 /
/// 产出表快照）全部保留 —— 装配层的确定性正是黄金世界最该锁住的东西之一。
fn normalized_assembly(raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    let Ok(mut v) = serde_json::from_str::<Value>(raw) else {
        return json!(raw);
    };
    if let Some(obj) = v.as_object_mut() {
        obj.remove("assembledAt");
        if let Some(cs) = obj.get_mut("chapterState").and_then(Value::as_object_mut) {
            cs.remove("sessionStartedAt");
        }
    }
    v
}

/// 取某条关系边（`from -> to`）的四个数值，四舍五入到 1e-4 便于断言。
fn relation_of(st: &NarrativeState, from: &str, to: &str) -> Option<(f64, f64, f64, f64)> {
    st.relations.iter().find(|r| r.from == from && r.to == to).map(|r| {
        let q = |x: f32| (x as f64 * 10_000.0).round() / 10_000.0;
        (q(r.trust), q(r.affinity), q(r.fear), q(r.debt))
    })
}

async fn narrative_state_of(db: &AnyPool, world_id: &str) -> NarrativeState {
    serde_json::from_str(&load_world(db, world_id).await.unwrap().narrative_state_json).unwrap()
}

// ============================================================================
// §8 回归用例 —— (1) 确定性
// ============================================================================

/// **回归断言 ①：确定性**。同一个黄金世界（同 world_id / 同角色卡 / 同骨架 / 同模板版本 / 同剧本）
/// 跑两遍，全部结构化产物**逐字节相等**。
///
/// 这一条一旦红，说明管线里混进了随机源、墙钟依赖或迭代序依赖（禁三样：系统随机 / 浮点 RNG /
/// map 迭代序），是最高优先级的回归信号。
#[tokio::test]
async fn golden_world_replay_is_byte_identical() {
    // 两次运行各自一套全新内存库 + 全新引擎 FS 目录，唯一相同的是**钉死的 world_id 与 fixture**。
    let state_a = test_state().await;
    seed_golden_world(&state_a, WORLD_MAIN, &GoldenParams::main()).await;
    let model_a = main_scripted_model();
    let status_a = run_main_scenario(&state_a, &model_a).await;
    let snap_a = golden_snapshot(&state_a.db, WORLD_MAIN).await;

    let state_b = test_state().await;
    seed_golden_world(&state_b, WORLD_MAIN, &GoldenParams::main()).await;
    let model_b = main_scripted_model();
    let status_b = run_main_scenario(&state_b, &model_b).await;
    let snap_b = golden_snapshot(&state_b.db, WORLD_MAIN).await;

    assert_eq!(status_a, status_b, "两次重放的逐拍 TickStatus 必须一致");
    assert_eq!(
        snap_a, snap_b,
        "黄金世界重放必须逐字节相等：结构化产物出现漂移 = 管线引入了随机源 / 墙钟依赖 / 迭代序依赖"
    );

    // 快照不是空壳（防"两边都空所以相等"的假绿）。
    assert!(snap_a.len() > 2000, "快照过小，疑似未真正跑完回合：{} 字节", snap_a.len());
    assert!(snap_a.contains("\"mainline_complete\""), "主回归应自然收尾");
}

// ============================================================================
// §9 回归用例 —— (2) 关键剧情测试点：正常结局
// ============================================================================

/// **剧情测试点 ①：正常结局（自然收尾）**。三拍把主线里程碑推过阈值 → `run_event_step` 回合后
/// 复判 MainlineDone → `mainline_complete` 与状态 CAS 同事务停机 → 三层结算按**公示产出表**
/// 确定发放（零随机）。
#[tokio::test]
async fn golden_world_reaches_natural_ending_and_settles() {
    let state = test_state().await;
    seed_golden_world(&state, WORLD_MAIN, &GoldenParams::main()).await;
    let model = main_scripted_model();
    let statuses = run_main_scenario(&state, &model).await;

    assert_eq!(
        statuses,
        vec![TickStatus::Done, TickStatus::Done, TickStatus::Concluded],
        "三拍成戏，第三拍把里程碑推过阈值并在同一事务内自然收尾"
    );

    // 世界停机 + 收尾类型 = 自然。
    assert_eq!(text_one(&state.db, "SELECT status FROM worlds WHERE id = $1", WORLD_MAIN).await, "ended");
    let (reason, ending) = conclusion_of(&state.db, WORLD_MAIN).await;
    assert_eq!(reason, "mainline_complete", "主线走完 = 自然收尾");
    assert!(!is_forced_conclusion(&reason), "自然收尾不计入强制收尾率");
    assert!(!ending.is_empty() && ending != "none", "装配层应确定性选出一个结局：{ending}");

    // 结局定盘链路（任务 #41）：终局落定的结局 = 装配层 `selectedEnding`（`DOMAIN_ENDING` 子流按权重
    // 掷点），且必须落在 `enabledEndings` 之内 —— 掷点不得选出未启用的结局。不钉具体 id：钉了就等于
    // 把「掷点结果」写死成常量，换种子/换阵容都得改测试，反而掩盖分布变化。
    let assembled: Value = serde_json::from_str(
        &load_world(&state.db, WORLD_MAIN).await.unwrap().assembled_json.expect("黄金世界必有装配产物"),
    )
    .unwrap();
    let enabled: Vec<String> = assembled
        .pointer("/assembly/enabledEndings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let selected = assembled
        .pointer("/assembly/selectedEnding")
        .and_then(Value::as_str)
        .expect("装配层必须钉住 selectedEnding")
        .to_string();
    assert!(enabled.len() >= 2, "黄金骨架结局池应有多于一个候选，否则这条断言观察不到「有得选」：{enabled:?}");
    assert!(enabled.contains(&selected), "掷点只能在已启用结局中选：selected={selected} enabled={enabled:?}");
    assert_eq!(ending, selected, "终局落定的结局 = 装配层定盘的结局（runtime 只读不掷点）");

    // 里程碑确实被推过阈值（不是靠时间上限蒙混过关）。
    let st = narrative_state_of(&state.db, WORLD_MAIN).await;
    let m1 = st.narrative.outline_nodes.iter().find(|n| n.id == "m1").expect("m1 应随冷启动种子入状态");
    assert_eq!(m1.status, muse_engine::narrative::types::NodeStatus::Done);
    let progress = st.world.get("milestoneProgress_m1").and_then(Value::as_f64).unwrap_or(0.0);
    assert!(
        (progress - 14.25).abs() < 1e-6,
        "三拍强度累积应为 5.00 + 4.75 + 4.50 = 14.25，实测 {progress}"
    );
    // 禁止谓词随种子进入状态且未被放宽（约束仍在）。
    assert_eq!(st.narrative.forbidden_predicates.len(), 1);
    assert_eq!(st.narrative.forbidden_predicates[0].id, "f1");

    // ③ 世界线层：三名成员贡献分各 ≥3.0 → 命中「推动」档（历练 80 + powerTier 2 的产出，
    // 3★ 实例不触封顶）；① 出席保底 60 ⇒ 每张卡 140。
    for (cid, uid) in GOLDEN_MEMBERS {
        assert_eq!(
            i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id = $1", cid).await,
            140,
            "{cid} 应得 ① 出席 60 + ③ 推动档 80"
        );
        let hook = format!("{WORLD_MAIN}:{cid}:worldline");
        let granted: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM backpacks WHERE user_id = $1 AND reward_hook_key = $2")
                .bind(uid)
                .bind(&hook)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(granted, 1, "{uid} 应收到「推动」档的世界线产出（发给卡的主人）");
    }

    // 🔴 平权红线：贡献分/产出绝不写进会回灌引擎的 narrative_state_json。
    let raw = text_one(&state.db, "SELECT narrative_state_json FROM worlds WHERE id = $1", WORLD_MAIN).await;
    assert!(
        !raw.contains("contribution") && !raw.contains("payout") && !raw.contains("mileage"),
        "结算侧数值绝不能进入引擎状态（VALIDATION §0.1 平权红线）"
    );

    // 确定性产出不经任何计费/账本路径（无抽卡、无购买）。
    assert_eq!(i64_one(&state.db, "SELECT COUNT(*) FROM ledger_entries", "").await, 0);
}

// ============================================================================
// §10 回归用例 —— (3) 关键剧情测试点：BE / 世界线崩塌
// ============================================================================

/// **剧情测试点 ②：崩塌 BE**。关键角色（崔萼）永久退场 → `key_character_exit` 强制收尾 →
/// ③ 世界线层归零 · ① 通关奖励归零（崩塌 = 没走完）· 结算幂等标记仍然落下（不留重复结算空子）。
#[tokio::test]
async fn golden_world_collapse_is_forced_and_zeroes_worldline() {
    let state = test_state().await;
    seed_golden_world(&state, WORLD_COLLAPSE, &GoldenParams::collapse()).await;
    let model = main_scripted_model();

    // 拍 0：正常推进，三名成员各累积 1.25 分（≥「见证」档 1.0；正常收束下本可得 ③ 产出）。
    model.set_tick(0);
    assert_eq!(drive_tick(&state, &model, WORLD_COLLAPSE, 0).await, TickStatus::Done);
    for (cid, _) in GOLDEN_MEMBERS {
        let s: i64 = sqlx::query_scalar(
            "SELECT score_milli FROM world_contributions WHERE world_id = $1 AND character_id = $2",
        )
        .bind(WORLD_COLLAPSE)
        .bind(cid)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(s, 1250, "{cid} 拍 0 应记 1.25 分（Success 1.0 + willSpeak 0.25，×1000 定点）");
    }

    // 关键角色永久退场 → 世界线崩塌。
    sqlx::query("UPDATE world_members SET status = 'left' WHERE world_id = $1 AND cloud_character_id = $2")
        .bind(WORLD_COLLAPSE)
        .bind("cuie")
        .execute(&state.db)
        .await
        .unwrap();

    // 拍 1：关键角色退场门在跑回合之前命中 → 直接停机（不白跑一回合）。
    assert_eq!(drive_tick(&state, &model, WORLD_COLLAPSE, 1).await, TickStatus::Concluded);
    assert_eq!(text_one(&state.db, "SELECT status FROM worlds WHERE id = $1", WORLD_COLLAPSE).await, "ended");

    let (reason, _) = conclusion_of(&state.db, WORLD_COLLAPSE).await;
    assert_eq!(reason, "key_character_exit");
    assert_eq!(classify_conclusion(&reason), ConclusionKind::Collapsed);
    assert!(is_forced_conclusion(&reason), "崩塌属于强制收尾（不是自然结局）");

    // ① 归零：崩塌没走完世界线 ⇒ 无通关奖励；③ 归零：不发任何世界线产出，但幂等标记照落。
    for cid in ["shenyan", "peizhao"] {
        assert_eq!(
            i64_one(&state.db, "SELECT mileage FROM cloud_characters WHERE id = $1", cid).await,
            0,
            "{cid} 崩塌 → ① 通关奖励归零（没走完世界线）且 ③ 归零"
        );
        let settled: i64 = sqlx::query_scalar(
            "SELECT settled_at FROM world_contributions WHERE world_id = $1 AND character_id = $2",
        )
        .bind(WORLD_COLLAPSE)
        .bind(cid)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert!(settled > 0, "{cid} 崩塌也要打幂等结算标记");
    }
    let worldline_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM backpacks WHERE reward_hook_key LIKE $1",
    )
    .bind(format!("{WORLD_COLLAPSE}:%"))
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(worldline_items, 0, "崩塌 → ③ 世界线层不发放任何产出");
    assert_eq!(
        i64_one(
            &state.db,
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'world.worldline_settled' AND subject = $1",
            WORLD_COLLAPSE
        )
        .await,
        0,
        "③ 归零时无产出可审计"
    );
}

// ============================================================================
// §11 回归用例 —— (4) 关键剧情测试点：死亡（含同意门）
// ============================================================================

/// 致死剧本：沈砚对裴照施加不可逆的致命行动；其余角色按兵不动（默认剧本）。
fn lethal_scripted_model() -> Arc<ScriptedModel> {
    Arc::new(ScriptedModel::new().on(
        "roleDecide",
        ANY_TICK,
        "shenyan",
        decision_json("斩草除根", "拔剑当场杀死裴照", false, "", &["peizhao"], 60),
    ))
}

/// **剧情测试点 ③：死亡 + 同意门**。不可逆结果先被门控（不落定、产 `ConsentRequested`、
/// 建同意请求、通知当事角色主人）；当事人授权后回灌 `approved_consents` 才落定并清账。
///
/// 顺带端到端验证「连续无有效戏份」指标：被门控那一拍，行动者的行动**根本没有发生**，
/// 于是他在那一拍没有任何叙事事件 —— 指标必须抓到这一拍（`consent_request` 不算戏份）。
#[tokio::test]
async fn golden_world_death_requires_consent_then_lands() {
    let state = test_state().await;
    seed_golden_world(&state, WORLD_DEATH, &GoldenParams::no_endgame()).await;
    let model = lethal_scripted_model();

    // ---- 拍 0：门控，死亡不落定 ----
    assert_eq!(
        drive_tick(&state, &model, WORLD_DEATH, 0).await,
        TickStatus::Done,
        "不可逆结果被门控，但其余行动仍照常提交（非 blocked/failed）"
    );

    assert_eq!(
        i64_one(
            &state.db,
            "SELECT COUNT(*) FROM consent_requests WHERE world_id = $1 AND status = 'pending'",
            WORLD_DEATH
        )
        .await,
        1,
        "应恰好建一条待授权的同意请求（同 subject 多事件被幂等去重）"
    );
    let kind = text_one(
        &state.db,
        "SELECT event_kind FROM consent_requests WHERE world_id = $1 AND status = 'pending'",
        WORLD_DEATH,
    )
    .await;
    assert_eq!(kind, "death");
    let subjects = text_one(
        &state.db,
        "SELECT subject_character_ids FROM consent_requests WHERE world_id = $1 AND status = 'pending'",
        WORLD_DEATH,
    )
    .await;
    assert!(subjects.contains("peizhao"), "当事角色应为受害者裴照，got={subjects}");
    assert!(
        i64_one(
            &state.db,
            "SELECT COUNT(*) FROM notification_outbox WHERE kind = 'consent_request' AND user_id = $1",
            "upei"
        )
        .await
            >= 1,
        "应通知当事角色的主人来响应"
    );

    let st0 = narrative_state_of(&state.db, WORLD_DEATH).await;
    assert!(
        st0.narrative
            .pending_consents
            .iter()
            .any(|p| p.subject == "peizhao" && p.event_kind == "death"),
        "未获批的死亡必须记入 pending_consents（门控证据）"
    );

    // 被门控的行动者这一拍没有任何叙事事件（consent_request 不算戏份）。
    let streaks = silent_streaks(&state.db, WORLD_DEATH).await;
    assert_eq!(
        streaks.get("shenyan").copied(),
        Some(1),
        "被同意门拦下那一拍 = 行动者的一拍无有效戏份，指标必须抓到：{streaks:?}"
    );

    // ---- 当事人授权（等价于 respond 落定；respond 端点在 consents/tests.rs 另有覆盖）----
    sqlx::query("UPDATE consent_requests SET status = 'approved', resolved_at = $1 WHERE world_id = $2")
        .bind(now_ms())
        .bind(WORLD_DEATH)
        .execute(&state.db)
        .await
        .unwrap();

    // ---- 拍 1：approved_consents 回灌 → 落定 + 清账 ----
    assert_eq!(drive_tick(&state, &model, WORLD_DEATH, 1).await, TickStatus::Done);
    let st1 = narrative_state_of(&state.db, WORLD_DEATH).await;
    assert!(
        !st1.narrative.pending_consents.iter().any(|p| p.subject == "peizhao"),
        "获批后不可逆结果应落定并清除对应 pending_consents"
    );
    assert_eq!(
        i64_one(
            &state.db,
            "SELECT COUNT(*) FROM consent_requests WHERE world_id = $1 AND status = 'pending'",
            WORLD_DEATH
        )
        .await,
        0,
        "落定回合不产 ConsentRequested，也不该残留 pending"
    );
    // 落定那一拍行动者重新有戏 → 最长无戏份拍数仍是 1（不再增长）。
    let streaks1 = silent_streaks(&state.db, WORLD_DEATH).await;
    assert_eq!(streaks1.get("shenyan").copied(), Some(1), "落定后行动者恢复戏份：{streaks1:?}");
}

// ============================================================================
// §12 回归用例 —— (5) 关键剧情测试点：托梦干预
// ============================================================================

/// **剧情测试点 ④：托梦生效**。投给在场角色的 accepted whisper 必须**真的出现在该角色的决策上下文里**
/// （不只是被标 applied），并且只对该角色可见；投给非在场角色的 whisper 既不喂入也不消费。
#[tokio::test]
async fn golden_world_whisper_is_fed_into_decision_context_and_consumed() {
    const WHISPER_TEXT: &str = "别把那本账册交出去，今夜有人在等你露破绽。";
    let state = test_state().await;
    seed_golden_world(&state, WORLD_WHISPER, &GoldenParams::no_endgame()).await;

    seed_whisper(&state.db, "iv-fed", WORLD_WHISPER, "ushen", "shenyan", WHISPER_TEXT).await;
    seed_whisper(&state.db, "iv-unfed", WORLD_WHISPER, "ushen", "ghostcid", "无处投递").await;

    let model = main_scripted_model();
    assert_eq!(drive_tick(&state, &model, WORLD_WHISPER, 0).await, TickStatus::Done);

    // 模型确实"看见"了托梦，且只有沈砚看得见。
    let decide_prompts: Vec<(String, String)> =
        model.captured().into_iter().filter(|(agent, _)| agent == "roleDecide").collect();
    assert_eq!(decide_prompts.len(), 4, "三名玩家 + 一名世界固有角色，各一次决策调用");
    let mut seen_by = Vec::new();
    for (_, user) in &decide_prompts {
        if user.contains(WHISPER_TEXT) {
            seen_by.push(cid_of_decide_prompt(user));
        }
    }
    assert_eq!(seen_by, vec!["shenyan".to_string()], "托梦只能进入目标角色的可见上下文");
    let shen_prompt = decide_prompts
        .iter()
        .find(|(_, u)| cid_of_decide_prompt(u) == "shenyan")
        .map(|(_, u)| u.clone())
        .expect("应捕获到沈砚的决策上下文");
    assert!(shen_prompt.contains("\"whisper\""), "托梦应以 whisper 字段挂进可见上下文");

    // Q-3：只消费本拍真正喂入的干预。
    assert_eq!(text_one(&state.db, "SELECT status FROM interventions WHERE id = $1", "iv-fed").await, "applied");
    assert_eq!(
        text_one(&state.db, "SELECT status FROM interventions WHERE id = $1", "iv-unfed").await,
        "accepted",
        "非在场角色的托梦不得被 blanket 标 applied"
    );
}

// ============================================================================
// §13 回归用例 —— (6) 关键剧情测试点：多人关系演化
// ============================================================================

/// **剧情测试点 ⑤：多人关系演化**。关系数值由本回合已落定的 decisions/outcomes **确定性推导**
/// （`relation_dynamics`，无模型调用、无随机源），三拍之后关系图应呈现可解释的形态：
/// 友善累积好感与信任、敌对推高畏惧、施救记欠、无目标者不建边。
///
/// 断言的是**方向与序关系**而非某个魔数：平衡参数（`FRIENDLY_BI_AFFINITY` 等）是产品可调参数
/// （VALIDATION §0.2），调参不该让回归变红；真正不能变的是"这段互动会把关系推向哪一侧"。
/// 逐字节层面的锁定由 `golden_world_replay_is_byte_identical` 的快照负责。
#[tokio::test]
async fn golden_world_multiparty_relations_evolve_deterministically() {
    let state = test_state().await;
    seed_golden_world(&state, WORLD_MAIN, &GoldenParams::main()).await;
    let model = main_scripted_model();
    run_main_scenario(&state, &model).await;
    let st = narrative_state_of(&state.db, WORLD_MAIN).await;

    // 关系图真的长出来了（此前 `build_patch` 从不产关系操作 → 关系图恒空、关系谓词永不命中，
    // 是历史回归点），且**只**长在真正互动过的三对之间，双向共 6 条边。
    assert!(!st.relations.is_empty(), "三拍互动之后关系图不应为空");
    let edges: Vec<(String, String)> =
        st.relations.iter().map(|r| (r.from.clone(), r.to.clone())).collect();
    assert_eq!(edges.len(), 6, "沈砚↔裴照 / 崔萼↔裴照 / 沈砚↔崔萼 三对，双向共 6 条边：{edges:?}");

    let (_, s2p_aff, s2p_fear, _) = relation_of(&st, "shenyan", "peizhao").expect("沈砚→裴照 应建边");
    let (_, p2s_aff, p2s_fear, _) = relation_of(&st, "peizhao", "shenyan").expect("裴照→沈砚 应建边");
    let (_, s2c_aff, s2c_fear, _) = relation_of(&st, "shenyan", "cuie").expect("沈砚→崔萼 应建边");
    let (_, c2p_aff, _, c2p_debt) = relation_of(&st, "cuie", "peizhao").expect("崔萼→裴照 应建边");
    let (_, p2c_aff, _, p2c_debt) = relation_of(&st, "peizhao", "cuie").expect("裴照→崔萼 应建边");

    // 敌对一拍在**双向**都留下畏惧（沈砚拦、裴照逼，互为行动者与承受者）；
    // 而全程只有友善互动的沈砚↔崔萼一线畏惧恒为 0 —— 畏惧不会凭空溢出到无关的边上。
    assert!(s2p_fear > 0.0 && p2s_fear > 0.0, "互相敌对应双向生畏：{s2p_fear} / {p2s_fear}");
    assert_eq!(s2c_fear, 0.0, "从未被敌对指向的关系不应产生畏惧：{s2c_fear}");

    // 敌对过的一线好感明显低于纯友善的一线（先友后敌被抵消回近零，赠礼一线仍为正）。
    assert!(
        s2p_aff < s2c_aff && p2s_aff < s2c_aff,
        "敌对过的关系好感应低于纯友善关系：{s2p_aff} / {p2s_aff} < {s2c_aff}"
    );
    assert!(s2c_aff > 0.0, "【赠】类友善行动应推高好感：{s2c_aff}");

    // 连续施助（拍 1 相助 + 拍 2 相救）推高双向好感；且「救」只让**被救者**记欠，施救者不记。
    assert!(c2p_aff > 0.0 && p2c_aff > 0.0, "连续施助应推高双向好感：{c2p_aff} / {p2c_aff}");
    assert!(p2c_debt > 0.0, "「救」类行动应让被救者对施救者记欠（debt）：{p2c_debt}");
    assert_eq!(c2p_debt, 0.0, "施救者不该反过来记欠：{c2p_debt}");

    // 世界固有角色全程无角色目标 ⇒ 不建任何边（确定性推导不会凭空造关系）。
    assert!(
        !st.relations.iter().any(|r| r.from == GOLDEN_NPC || r.to == GOLDEN_NPC),
        "无角色目标的行动不得产生关系边：{edges:?}"
    );
}

// ============================================================================
// §14 回归用例 —— (7) 指标基线 + 成本基线
// ============================================================================

/// **回归断言 ③/④：指标基线与成本基线**。
///
/// - 基尼系数（叙事注意力公平）：断言落在 T2 门槛 `≤0.35` 内，且**大于 0**——
///   恒等于 0 往往意味着账本根本没记，或统计口径把差异抹平了。
/// - 连续无有效戏份拍数：黄金世界基线是 0（三名成员每拍都有戏）。
/// - 收尾类型：自然收尾，不计入强制收尾率。
/// - `cost_tokens`：逐拍**精确等于**剧本化模型的预期值（剧本模型 token 可控，见 `AGENT_TOKENS`）。
#[tokio::test]
async fn golden_world_metrics_and_cost_baseline() {
    let state = test_state().await;
    seed_golden_world(&state, WORLD_MAIN, &GoldenParams::main()).await;
    let model = main_scripted_model();
    run_main_scenario(&state, &model).await;

    // ---- 成本基线（逐拍精确）----
    let costs: Vec<(i64, i64)> = sqlx::query(
        "SELECT tick_no, cost_tokens FROM world_ticks WHERE world_id = $1 ORDER BY tick_no ASC",
    )
    .bind(WORLD_MAIN)
    .fetch_all(&state.db)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.try_get::<i64, _>("tick_no").unwrap(), r.try_get::<i64, _>("cost_tokens").unwrap()))
    .collect();
    assert_eq!(
        costs,
        vec![(0, COST_PLAIN_TICK), (1, COST_ARBITRATED_TICK), (2, COST_PLAIN_TICK)],
        "逐拍成本应精确等于剧本预期：常规拍 = 导演 160 + 决策 4×260 + 写作 440 + 审校 200；\
         交锋拍（R4 冲突升级模型仲裁）多 200"
    );
    assert_eq!(
        i64_one(&state.db, "SELECT spent_tokens_today FROM world_budgets WHERE world_id = $1", WORLD_MAIN)
            .await,
        COST_MAIN_TOTAL,
        "预算累计应等于各拍实测 token 之和"
    );

    // ---- 基尼系数（叙事注意力公平，T2 门槛 ≤0.35）----
    let (gini, counted) = attention_gini(&state.db, WORLD_MAIN).await;
    assert_eq!(counted, GOLDEN_MEMBERS.len(), "统计口径必须只含玩家成员（NPC 已被交集剔除）");
    assert!(gini <= 0.35, "叙事注意力基尼系数应满足 T2 门槛 ≤0.35，实测 {gini}");
    assert!(gini > 0.0, "剧本刻意让戏份分布不完全均等，基尼恒为 0 说明账本或口径出了问题：{gini}");

    // 🔴 NPC 陷阱：世界固有角色**也入 world_contributions**，不取交集就会污染公平度。
    let npc_score: i64 = sqlx::query_scalar(
        "SELECT score_milli FROM world_contributions WHERE world_id = $1 AND character_id = $2",
    )
    .bind(WORLD_MAIN)
    .bind(GOLDEN_NPC)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert!(npc_score > 0, "NPC 确实在账本里（这正是必须取交集的原因）");
    let npc_is_member: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM world_members WHERE world_id = $1 AND cloud_character_id = $2",
    )
    .bind(WORLD_MAIN)
    .bind(GOLDEN_NPC)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(npc_is_member, 0, "NPC 无 owner，不是 world_member");

    // ---- 连续无有效戏份拍数 ----
    let streaks = silent_streaks(&state.db, WORLD_MAIN).await;
    assert_eq!(streaks.len(), GOLDEN_MEMBERS.len());
    assert!(
        streaks.values().all(|v| *v == 0),
        "黄金世界基线：三名成员每一拍都有有效戏份，无人被冷落：{streaks:?}"
    );

    // ---- 收尾类型 ----
    let (reason, _) = conclusion_of(&state.db, WORLD_MAIN).await;
    assert_eq!(classify_conclusion(&reason), ConclusionKind::Natural);
    assert!(!is_forced_conclusion(&reason));
}

// ============================================================================
// §15 指标纯函数单测（不落库、不起引擎，专测口径本身）
// ============================================================================
//
// 被测函数现居 `crate::slo`（生产代码）。这几个用例**刻意留在黄金世界回归里**：
// 它们是回归基线的一部分——口径被改动时，`cargo test golden` 就该红。
// `slo::tests` 另测聚合查询与后台响应形态，两层不重复。

#[test]
fn gini_coefficient_covers_edge_and_typical_distributions() {
    // 空集 / 全零：没开演不等于不公平。
    assert_eq!(gini_coefficient(&[]), 0.0);
    assert_eq!(gini_coefficient(&[0, 0, 0]), 0.0);
    // 完全均分 → 0。
    assert_eq!(gini_coefficient(&[100, 100, 100]), 0.0);
    // 一人独占 n 人份 → (n-1)/n（基尼的解析上界）。
    let g = gini_coefficient(&[100, 0, 0, 0]);
    assert!((g - 0.75).abs() < 1e-9, "四人一人独占应为 0.75，实测 {g}");
    // 单人世界：无从比较 → 0。
    assert_eq!(gini_coefficient(&[42]), 0.0);
    // 尺度不变性：整体等比放大不改变基尼。
    let a = gini_coefficient(&[1000, 3000, 6000]);
    let b = gini_coefficient(&[2000, 6000, 12000]);
    assert!((a - b).abs() < 1e-12, "基尼应对尺度不变：{a} vs {b}");
    // 单调性：分布越集中，基尼越大。
    assert!(gini_coefficient(&[1, 1, 8]) > gini_coefficient(&[2, 3, 5]));
    // T2 门槛的判定示例：三人世界里一人拿走八成戏份（0.467）已越线，六成（0.267）尚未越线——
    // 门槛 0.35 落在这两者之间，正是"有人明显独占戏份"与"分布略有倾斜"的分界。
    assert!(gini_coefficient(&[800, 100, 100]) > 0.35);
    assert!(gini_coefficient(&[600, 200, 200]) < 0.35);
    // 主回归的实际分布（沈砚/裴照/崔萼 = 3750/3500/3750 milli）远在门槛内。
    let main = gini_coefficient(&[3750, 3500, 3750]);
    assert!(main > 0.0 && main < 0.02, "主回归基尼实测 {main}");
}

#[test]
fn gini_excludes_world_controlled_npc() {
    // 同一批戏份，把 NPC 算进来会得到不同的公平度 —— 这正是「须先与 world_members 取交集」的原因。
    let members_only = gini_coefficient(&[3750, 3500, 3750]);
    let with_npc = gini_coefficient(&[3750, 3500, 3750, 3250]);
    assert!(
        (members_only - with_npc).abs() > 1e-6,
        "把 NPC 算进玩家公平度会得到不同结论：{members_only} vs {with_npc}"
    );
}

#[test]
fn max_silent_streaks_counts_longest_gap_not_total() {
    let members = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let ticks = vec![0, 1, 2, 3, 4];
    let mut app: BTreeSet<(i64, String)> = BTreeSet::new();
    // a：每拍都有戏。
    for t in &ticks {
        app.insert((*t, "a".to_string()));
    }
    // b：0、3、4 有戏 ⇒ 中间空了 1、2 两拍（最长连续 2，总缺席也是 2）。
    for t in [0, 3, 4] {
        app.insert((t, "b".to_string()));
    }
    // c：只有第 2 拍有戏 ⇒ 空档 {0,1} 与 {3,4}，最长连续 2，但总缺席 4。
    app.insert((2, "c".to_string()));

    let s = max_silent_streaks(&members, &ticks, &app);
    assert_eq!(s.get("a").copied(), Some(0));
    assert_eq!(s.get("b").copied(), Some(2));
    assert_eq!(
        s.get("c").copied(),
        Some(2),
        "口径是**最长连续**无戏份，不是总缺席拍数（c 缺席 4 拍但最长连续只有 2）"
    );

    // 全程无戏 → 等于拍数；无拍可算 → 0。
    let none: BTreeSet<(i64, String)> = BTreeSet::new();
    assert_eq!(max_silent_streaks(&members, &ticks, &none).get("a").copied(), Some(5));
    assert_eq!(max_silent_streaks(&members, &[], &app).get("a").copied(), Some(0));
    // 前缀 id 不得互相误判（现有 `LIKE '%cid%'` 查询的已知缺陷，本函数用规范化解析规避）。
    let pair = vec!["li".to_string(), "lixia".to_string()];
    let mut only_lixia: BTreeSet<(i64, String)> = BTreeSet::new();
    only_lixia.insert((0, "lixia".to_string()));
    let s2 = max_silent_streaks(&pair, &[0], &only_lixia);
    assert_eq!(s2.get("li").copied(), Some(1), "`li` 不得被 `lixia` 的出场蹭到戏份");
    assert_eq!(s2.get("lixia").copied(), Some(0));
}

#[test]
fn conclusion_classification_matches_terminal_reason_vocabulary() {
    // `runtime::terminal_reason` 的三个串。
    assert_eq!(classify_conclusion("mainline_complete"), ConclusionKind::Natural);
    assert_eq!(classify_conclusion("time_cap"), ConclusionKind::Forced);
    assert_eq!(classify_conclusion("starved"), ConclusionKind::Forced);
    // runtime 另外两条终局路径。
    assert_eq!(classify_conclusion("time_limit"), ConclusionKind::Forced);
    assert_eq!(classify_conclusion("key_character_exit"), ConclusionKind::Collapsed);
    // 未收尾 / 未知一律保守：不算自然收尾。
    assert_eq!(classify_conclusion(""), ConclusionKind::Unknown);
    assert_eq!(classify_conclusion("something_new"), ConclusionKind::Unknown);

    assert!(!is_forced_conclusion("mainline_complete"));
    for r in ["time_cap", "time_limit", "starved", "key_character_exit", "", "something_new"] {
        assert!(is_forced_conclusion(r), "`{r}` 应计入强制收尾率");
    }
}

// ============================================================================
// §16 fixture 自检：基线内容本身必须始终合法可读
// ============================================================================

#[test]
fn golden_fixture_is_well_formed() {
    // 四张卡都能解析成引擎角色卡（fixture 写坏要在这里就炸，而不是在某个集成用例的深处）。
    for (cid, _) in GOLDEN_MEMBERS {
        let _ = golden_card_json(cid);
    }
    let _ = golden_card_json(GOLDEN_NPC);

    // 骨架组装后：NPC 卡已注入、参数化字段已覆写、其余内容与文件一致。
    let params = GoldenParams::main();
    let sk: Value = serde_json::from_str(&golden_skeleton(&params)).unwrap();
    assert_eq!(sk["worldCharacters"][0]["card"]["id"], json!(GOLDEN_NPC), "NPC 卡应从 cards.json 注入");
    assert_eq!(sk["mainlineNodes"][0]["threshold"], json!(params.milestone_threshold));
    assert_eq!(sk["endgame"]["minWorldTicks"], json!(params.min_world_ticks));
    assert_eq!(sk["endgame"]["maxWorldTicks"], json!(params.max_world_ticks));

    // 三条剧情参数只动 threshold/endgame，叙事内容在测试点之间逐字节一致。
    let strip = |p: &GoldenParams| {
        let mut v: Value = serde_json::from_str(&golden_skeleton(p)).unwrap();
        v["mainlineNodes"][0]["threshold"] = Value::Null;
        v["endgame"] = Value::Null;
        v.to_string()
    };
    assert_eq!(strip(&GoldenParams::main()), strip(&GoldenParams::collapse()));
    assert_eq!(strip(&GoldenParams::main()), strip(&GoldenParams::no_endgame()));

    // 骨架声明的一切都在（结局池 / 隐藏池 / 身份池 / 产出表 / 禁止谓词 / 世界固有角色）。
    assert_eq!(sk["endingPool"].as_array().unwrap().len(), 3);
    assert_eq!(sk["hiddenContentPool"].as_array().unwrap().len(), 2);
    assert_eq!(sk["identityPool"].as_array().unwrap().len(), 3);
    assert_eq!(sk["payoutTable"]["worldlineTiers"].as_array().unwrap().len(), 2);
    assert_eq!(sk["forbiddenPredicates"].as_array().unwrap().len(), 1);

    // 剧本化模型的 token 表覆盖全部五个环节（成本基线的推导前提）。
    for agent in ["director", "roleDecide", "arbiter", "writer", "critic"] {
        assert_ne!(agent_tokens(agent), (10, 10), "{agent} 必须有显式 token 计量");
    }

    // 各剧情测试点的 world_id 互不相同 —— `stall_tracker()` 是进程级全局、按 world_id 分键，
    // 复用同一个 id 会让并发跑的用例互相串味，破坏确定性。
    let ids: BTreeSet<&str> = [WORLD_MAIN, WORLD_COLLAPSE, WORLD_DEATH, WORLD_WHISPER].into_iter().collect();
    assert_eq!(ids.len(), 4, "剧情测试点必须各用一个固定且互不相同的 world_id");
}

// ============================================================================
// 已知非确定性来源（本回归第一次跑就抓到，登记在案）
// ============================================================================
//
// **`ORDER BY joined_at` 没有次级排序键**（`assembly::load_active_cards` 与
// `runtime::process_tick_inner` 组装成员卡处）。两名成员的 `joined_at` 落在同一毫秒时，
// 行序由数据库决定，于是 `assembled_json` 里的 `perCharacterHooks` / `difficultyNotes`
// 顺序在两次运行之间漂移。用 `now_ms()` 连续播种时**必然**撞毫秒；生产上并发 join
// 也可能撞。本模块的处理是**钉死 fixture 侧的 `joined_at`**（`GOLDEN_JOINED_AT_BASE`），
// 把它从回归的自变量里拿掉 —— **没有改生产代码**（本次改动范围不含 assembly/worlds）。
//
// 影响面评估：目前仅波及装配产物中两个**展示/审计用**数组的顺序，不改变钩子内容本身，
// 也不进引擎决策，故不构成正确性缺陷；但它确实让"同一副本可 replay"这句话打了折扣。
// 若要根治，正确做法是给两处查询补 `, wm.cloud_character_id ASC` 次级键 ——
// 那是一次独立的生产改动，须单独评审（会改变已有世界的装配产物字节，属破坏性变更）。
//
// ============================================================================
// 诚实划界：本模块测得了什么、测不了什么
// ============================================================================
//
// ✅ **测得了（"管线不回归"）**——以下任一处坏掉，上面的用例会红：
//    - 确定性采样 / 装配（`instance_seed`、身份分配、结局加权、钩子嵌入）
//    - 冷启动种子（骨架 → 大纲节点 / 禁止谓词）与每 tick 回灌（DB ↔ 引擎 FS 单一事实源）
//    - 决策定序、规则层仲裁（R1-R6）、模型仲裁的升级条件、关系演化的确定性推导
//    - 不可逆结果同意门（门控 → 请求 → 通知 → 授权 → 落定 → 清账）
//    - 托梦投递与消费（Q-3：只消费本拍真正喂入的）
//    - 状态 CAS / 事件投影 / 内容安全第 2 层 / 预算实测计费
//    - 终局判定（主线完成 / 时间上限 / 关键角色退场）与三层结算（含崩塌折算与产出封顶）
//    - 调用构成层面的成本（逐拍 token 精确基线）
//
// ❌ **测不了（需要真实模型才能测）**——这些是 VALIDATION §4.1 里"对比 OOC / 剧情重复 / 叙事质量"
//    的部分，本模块**不声称**覆盖：
//    - **OOC（角色演得像不像）**：需要真实模型输出 + 人评/自动评分。本模块的决策是人写的剧本，
//      按定义永远"不 OOC"。
//    - **剧情重复率 / 文本质量**：prose 从未落库（§4.2），且写作温度 0.8 硬编码，
//      逐字比对在真实模型下不成立。
//    - **模型换代的真实成本变化**：这里的 token 是剧本里写死的常数，只能反映**调用构成**变化，
//      反映不了"换了个模型，同样的局面它多说了三倍的话"。
//
// 🔜 **补上 ❌ 那一栏的前提：record-and-replay 的 `ModelClient`**（VALIDATION §4.1 标注的
//    "唯一真新建件"）。进度分三段，**现在停在第 2 段**：
//
//    1. ✅ **工具**（2026-07-26，`muse_engine::replay`）：`RecordingClient` 包装任意 `ModelClient`
//       录入参/出参；`ReplayClient` 结构上**没有 inner 字段**，未命中返回 `NotFound` 而不是回落真实模型；
//       `diff_recordings` 把两份录制对齐到「哪一拍 · 哪个角色 · 哪个环节」再给字段级差异。
//    2. ✅ **接线**（2026-07-27，任务 #46，`runtime::record`）：接在 `process_tick_inner` 第 9 步
//       ——模型客户端在整条 tick 路径上的唯一出口，故 `process_tick`（生产）与本模块用的
//       `process_tick_with_model`（注入）都被覆盖。**默认关闭**：未配置时接线点原样返回同一个 `Arc`
//       （`Arc::ptr_eq` 成立，中间没有任何包装），所以本模块的基线**一个字节都没动**。
//       端到端由 `record::tests::golden_world_record_replay_round_trip_is_byte_identical` 锁死：
//       黄金世界主线跑三遍（关 / 录 / 放），结构化产物逐字节相等。
//    3. ❌ **真实录制与质量口径**：本仓**没有任何一份真实模型录制**（那需要用户自己的 API Key），
//       「差异多大算 OOC」这条评分口径**也还没有**。
//
//    ⚠️ 所以上面那一栏 ❌ **一条都没变**：本模块至今仍只跑人写的剧本，
//    「回归全绿」依旧**不得**被读成「角色一致性已验证」。录制入口（需自带 Key）：
//    `cargo test --manifest-path server/Cargo.toml -- --ignored record_golden_world_with_real_model`。
