//! 仿真试跑工装（总规格 §4「内容中台工业线」流水线第四道：**仿真试跑（自动化压测）**）。
//!
//! ⚠️ 先说清楚一件事实：总规格 §4 与 VALIDATION §4.1 曾把「showcase/scenario 脚本工装」写成
//! 现成依赖。核实（2026-07-26）：**工作树与 git 全历史零命中，它从未存在**。本模块是从零建的第一版。
//!
//! ## 它是什么
//!
//! 一个**批量、参数化、种子可复现**的世界生命周期驱动器：给一组场景配置和一个总种子，
//! 它把每个世界从开局一路驱动到收尾（或到驱动上限），产出**结构化结果**
//! （`SimRunRecord` / `SimSuiteReport`），并据此算出世界质量三指标——
//! **完读率 / 阻断率 / 结局分布**（口径在 `crate::slo::quality`，不在本文件）。
//!
//! ## 它和黄金世界回归（`runtime::golden`）的分工
//!
//! | | `golden` | `simulation`（本模块） |
//! |---|---|---|
//! | 输入 | **人写的固定剧本**（第几拍谁说什么，逐字钉死） | **种子派生的伪随机决策**（同种子恒同结果，换种子换局面） |
//! | 世界数 | 4 个固定 world_id，各测一个剧情点 | 一批（场景 × 种子），可扩到几十上百 |
//! | 断言面 | 结构化产物**逐字节相等** | **总量指标**（三指标计数）跨版本相等 |
//! | 抓什么 | "这条路径的产物变了" | "整批世界的**形状**变了"——完读率掉了 / 阻断率涨了 / 结局塌进一个桶 |
//!
//! 两者**同源不同层**：共用同一份 fixture（`golden/cards.json` + `golden/skeleton.json`，
//! 单一事实源，本模块不另造世界）、同一条生产路径（`process_tick_with_model`）、
//! 同一套指标口径（`crate::slo`）。golden 抓的是"某条路径的字节变了"，本模块抓的是
//! "一整批世界的统计形状变了"——后者能发现前者天然看不见的回归：
//! 一次改动可能让每条固定剧本都照样跑通，却让 30% 的随机局面走进死胡同。
//!
//! ## 🔴 诚实划界（**这一条比本模块的任何代码都重要**）
//!
//! 仿真用的是 **`SimModel`——一个种子驱动的规则化假模型**，全程不调用任何真实模型。因此：
//!
//! - **完读率**在这里测的是「**主线推进 + 终局判定管线**在各种决策组合下能否走到自然收尾」，
//!   **不是**「故事好不好看」。跑出 100% 完读率只说明**管线不卡死**，
//!   **不能**、也**不得**被表述为「内容质量已验证」。
//! - **阻断率**在这里测的是「**规则层**（底线 / 硬节点 / 确定性不变量）在压力下被触发的频率」，
//!   **不是**「模型输出是否合规」——桩永远不会说违规的话。
//! - **结局分布**在这里测的是「**结局加权采样 + 终局判定**的分布形状」，
//!   **不是**「玩家会喜欢哪个结局」。
//! - **内容安全扣留率**在桩下**恒为 0**：桩文本永不命中敏感词库。该通道在仿真里
//!   **只是被计算了，并没有被测试**。
//!
//! 这段话不只写在注释里——`QualitySource::SimulatedStub` 让它随每一份报告 JSON 一起走
//! （`slo::quality` 的 `honesty` 字段）。**数会被复制进评审材料，注释不会。**
//!
//! 要真正度量内容质量，前置件仍是 **record-and-replay 的 `ModelClient`**
//! （VALIDATION §4.1 标注的「唯一真新建件」，至今未建）：先用真实模型把一批世界录下来，
//! 再换 Prompt / 引擎重放录像。**本模块不假装自己是那个东西。**
//!
//! ## 确定性契约
//!
//! - 随机**只**走 `assembly::fnv1a_64` + `assembly::Rng`（SplitMix64），与装配采样同一套实现
//!   （不复制第二份）。域常量 `0x58/0x59/0x5A`，已在 `assembly` 的域常量清单里登记。
//! - **禁三样**：系统随机、浮点 RNG、map 迭代序驱动 RNG。分类抽取一律整数百分位。
//! - `SimModel` 的输出只由 `(套件种子, 场景名, 世界种子, tick_no, 角色 id)` 决定，
//!   **与调用次数、调用顺序、prompt 内容全都无关**——底线重生成会对同一角色同一拍再调一次
//!   `roleDecide`，若输出依赖调用序，重生成就会拿到不同提案，整个回归失去意义。
//! - `world_id` 进 `assembly::instance_seed`，故每个仿真世界的 id **由种子确定性派生**，
//!   不用 uuid（用了就等于每次跑的是不同副本）。
//!
//! ## 不在生产路径上
//!
//! 整个模块 `#[cfg(test)]`：不进二进制、不接路由、不建表、无迁移、无 feature flag
//! （没有对外面，也就没有"默认关闭"可言）。落在 `#[cfg(test)]` 而非 `server/tests/`
//! 的原因同 `golden`：`server` 是 binary-only crate，集成测试够不到 `pub(crate)` 的
//! `process_tick_with_model`。好处是自动进现有 `platform-test` CI job，CI 配置零改动。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::AnyPool;

use crate::app::AppState;
use crate::assembly::{fnv1a_64, Rng};
use crate::db::now_ms;
use crate::runtime::{insert_tick, process_tick_with_model, TickStatus};
use crate::slo::quality::{collect_world_facts, QualitySource, WorldQualityFacts, WorldQualityReport};
use crate::worlds::load_world;

use muse_engine::host::CancelFlag;
use muse_engine::model::{ModelCallSpec, ModelClient, ModelOutput};
use muse_engine::EngineError;

// 与黄金世界回归**共用同一份 fixture 与播种口径**（单一事实源；本模块不另造角色卡/世界骨架）。
use super::golden::{
    agent_tokens, cid_of_decide_prompt, decision_json, golden_card_json, golden_card_value,
    GOLDEN_JOINED_AT_BASE, GOLDEN_JOINED_AT_STEP, GOLDEN_MEMBERS, GOLDEN_NPC, SKELETON_JSON,
};
use super::tests::{seed_model_routes, seed_user, test_state};

// ============================================================================
// §1 确定性：域常量与子流派生
// ============================================================================

/// 世界种子子流（套件种子 → 每个世界的种子）。
const DOMAIN_SIM_WORLD: u64 = 0x58;
/// 角色决策子流。
const DOMAIN_SIM_DECISION: u64 = 0x59;
/// 发言/时长等次级抽取子流（与决策分流，避免"改了发言概率连行动也跟着变"的串扰）。
const DOMAIN_SIM_BEAT: u64 = 0x5A;

/// 派生一条子流：`Rng(fnv1a_64(parts) ^ domain)`，与 `assembly` 的派生形状一致。
fn substream(domain: u64, parts: &[&str]) -> Rng {
    Rng(fnv1a_64(parts.join("\u{1}").as_bytes()) ^ domain)
}

/// 套件种子 + 场景名 + 序号 → 该世界的种子。世界 id 由它派生，故 id 也是确定性的。
fn world_seed(suite_seed: &str, scenario: &str, idx: usize) -> u64 {
    substream(DOMAIN_SIM_WORLD, &[suite_seed, scenario, &idx.to_string()]).next_u64()
}

// ============================================================================
// §2 场景：可参数化的世界配置（VALIDATION §0.2 —— 终局规则一律参数化，禁止写死）
// ============================================================================

/// 决策倾向档：决定 `SimModel` 抽哪一类行动。三档权重之和必须 = 100（有自检用例守着）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Temper {
    friendly_pct: u32,
    hostile_pct: u32,
    neutral_pct: u32,
    /// 发言概率（百分位）。发言与否直接影响回合强度 ⇒ 主线推进速度 ⇒ 能否走到自然收尾。
    speak_pct: u32,
    /// **触碰角色自己卡上底线**的概率（百分位）。只有阻断压测场景 > 0。
    /// 100 = 每人每拍都撞自己的底线 ⇒ 本拍全部提案被拦 ⇒ 整拍 `blocked`。
    breach_pct: u32,
}

impl Temper {
    const fn new(friendly: u32, hostile: u32, neutral: u32, speak: u32, breach: u32) -> Self {
        Self {
            friendly_pct: friendly,
            hostile_pct: hostile,
            neutral_pct: neutral,
            speak_pct: speak,
            breach_pct: breach,
        }
    }
}

/// 一个仿真场景 = 一组世界参数 + 一档决策倾向 + 跑多少个副本。
#[derive(Debug, Clone)]
struct SimScenario {
    /// 场景名（进世界 id 与报告，必须稳定）。
    name: &'static str,
    /// 这个场景想压什么（进报告，供读数的人知道该看哪一栏）。
    intent: &'static str,
    temper: Temper,
    /// 主线里程碑阈值：越高越难自然收尾。
    milestone_threshold: f64,
    /// 终局地板：此拍数之前一律不触发终局。
    min_world_ticks: i64,
    /// 世界时间上限：到此强制收尾（`time_cap` / `time_limit`）。
    max_world_ticks: i64,
    /// 关键角色（永久退场即世界线崩塌）。
    key_character_ids: Vec<&'static str>,
    /// 生死契约档（`worlds.lethality`）。
    lethality: &'static str,
    /// 驱动上限：防跑飞。**它不是产品参数**，是工装自保——世界跑到这里还没收尾，
    /// 就以「未收尾」计入完读率分母（这正是要抓的失败形态，不是要绕开的麻烦）。
    max_drive_ticks: i64,
    /// 本场景跑几个副本（每个副本一个派生种子）。
    replicas: usize,
}

/// **标准压测套件**：四个场景，各压一个不同的失败面。
///
/// 参数取值的意图（改动前请连同基线一起复核）：
/// - `cordial`：低冲突、高发言 ⇒ 强度累积快 ⇒ 应当自然收尾。**完读率的正样本**。
/// - `volatile`：高冲突 ⇒ R4 冲突多、模型仲裁多 ⇒ 看敌意局面下管线还能不能收得住。
/// - `attrition`：里程碑阈值高得推不完 + 时间上限低 ⇒ 必被 `time_cap` 掐掉。
///   **强制收尾的正样本**——它证明"完读率不是恒等于 1"，否则这个指标没有分辨力。
/// - `deadlock`：全员每拍触碰自己底线 ⇒ 每拍整拍 `blocked` ⇒ 世界永远推不动。
///   **阻断率的正样本**——一个恒为 0 的阻断率是测不出回归的。
fn standard_suite() -> Vec<SimScenario> {
    vec![
        SimScenario {
            name: "cordial",
            intent: "低冲突高发言：主线应稳定推到自然收尾（完读率正样本）",
            temper: Temper::new(70, 5, 25, 90, 0),
            milestone_threshold: 12.0,
            min_world_ticks: 1,
            max_world_ticks: 24,
            key_character_ids: Vec::new(),
            lethality: "consent",
            max_drive_ticks: 10,
            replicas: 3,
        },
        SimScenario {
            name: "volatile",
            intent: "高冲突：R4 冲突频发、模型仲裁频繁，看敌意局面下能否收住",
            temper: Temper::new(20, 65, 15, 80, 0),
            milestone_threshold: 16.0,
            min_world_ticks: 1,
            max_world_ticks: 24,
            key_character_ids: Vec::new(),
            lethality: "sanctuary",
            max_drive_ticks: 10,
            replicas: 3,
        },
        SimScenario {
            name: "attrition",
            intent: "阈值推不完 + 时间上限低：必被强制收尾（强制收尾正样本）",
            temper: Temper::new(35, 30, 35, 40, 0),
            milestone_threshold: 900.0,
            min_world_ticks: 1,
            max_world_ticks: 3,
            key_character_ids: Vec::new(),
            lethality: "consent",
            max_drive_ticks: 8,
            replicas: 2,
        },
        SimScenario {
            name: "deadlock",
            intent: "全员每拍触碰自己底线：整拍 blocked，世界推不动（阻断率正样本）",
            temper: Temper::new(0, 0, 100, 50, 100),
            milestone_threshold: 12.0,
            min_world_ticks: 1,
            max_world_ticks: 24,
            key_character_ids: Vec::new(),
            lethality: "consent",
            max_drive_ticks: 4,
            replicas: 2,
        },
    ]
}

/// 组装场景的 `skeleton_json`：**只覆写参数化字段**，其余一切沿用 golden fixture 原字节。
fn sim_skeleton(sc: &SimScenario) -> String {
    let mut sk: Value = serde_json::from_str(SKELETON_JSON).expect("skeleton.json 必须是合法 JSON");
    sk["worldCharacters"][0]["card"] = golden_card_value(GOLDEN_NPC);
    sk["mainlineNodes"][0]["threshold"] = json!(sc.milestone_threshold);
    sk["endgame"] = json!({
        "minWorldTicks": sc.min_world_ticks,
        "maxWorldTicks": sc.max_world_ticks,
        "keyCharacterIds": sc.key_character_ids,
    });
    sk.to_string()
}

// ============================================================================
// §3 行动语料：每一条都刻意避开三张关键词表的陷阱
// ============================================================================
//
// 🔴 改这些字符串前必读三处正则，否则会静默改变整批世界的形状（而不是报错）：
//   - `narrative::relation_dynamics::RelationRules`：破裂 > 敌对 > 友善 > 中性（作用于 action+intent）
//   - `narrative::IrreversibleRules`：death / self_death / exit / relation —— 命中即走同意门
//   - `narrative::arbiter::screen_bottom_lines`：命中角色**自己卡上**的底线片段即被拦
//
// 下面的语料满足三条不变式（有 `sim_corpus_avoids_keyword_traps` 用例逐条守着）：
//   ① 友善语料只命中 friendly、不命中 hostile/rupture；敌对语料反之；中性语料三者都不命中；
//   ② 三档语料**都不**命中 death/exit/relation（不可逆）—— 同意门是 golden 的专项，不是压测的噪声；
//   ③ 三档语料**都不**包含 fixture 四张卡的任何一条底线片段 —— 底线只由 `BREACH_ACTIONS` 显式触发。

/// 友善行动（`{t}` 占位目标角色）。
const FRIENDLY_ACTIONS: &[&str] = &[
    "举杯向{t}道谢，把方才那句重话揭过",
    "低声安慰{t}，说这事还有转圜",
    "上前扶住{t}的手腕，护他一护",
    "把手里那半盏酒分享给{t}，示意他坐近些",
    "挺身相救{t}，把他从席角拉回来",
    "向{t}致歉，承认方才言重",
    "与{t}合作，把这本账目当众对完",
];
const FRIENDLY_INTENTS: &[&str] = &["先把场面圆住", "递个台阶", "把话接住"];

/// 敌对行动（不含任何不可逆语义）。
const HOSTILE_ACTIONS: &[&str] = &[
    "抢先把话头夺过来，逼{t}正面回话",
    "抬手挡在{t}身前，不让他再往前一步",
    "当众斥{t}一句，说这话说得太满",
    "揭穿{t}方才那句托辞",
    "阻拦{t}离席，把门口的路堵住",
    "出言威胁{t}，说明日便有人来查",
];
const HOSTILE_INTENTS: &[&str] = &["把话逼到明处", "先立个规矩", "退无可退"];

/// 中性行动（无目标，三张关键词表全不命中）。
const NEUTRAL_ACTIONS: &[&str] = &[
    "端起酒盏，把目光落在灯影里",
    "起身走到窗边，看檐下的雨",
    "在案前坐直，把袖口理平",
    "沉默片刻，等旁人先开口",
    "提壶为满席斟一轮酒",
];
const NEUTRAL_INTENTS: &[&str] = &["再看看", "按兵不动", "等一等"];

/// **触碰自己底线**的行动：每条都逐字重述该角色卡上某条底线的禁止片段。
///
/// 这不是"随便写点狠话"——`screen_bottom_lines` 只认**字面重述**（且命中处前 3 字内无否定字），
/// 所以每条都必须精确对齐 `golden/cards.json` 里那张卡的 `bottomLines`。
/// 卡改了这里就得跟着改，`sim_breach_actions_really_breach` 用例会在第一时间报警。
const BREACH_ACTIONS: &[(&str, &str)] = &[
    // 卡：不牵连不知情的下属
    ("shenyan", "当众牵连不知情的下属，把账目推到他们头上"),
    // 卡：不动手无寸铁的人
    ("peizhao", "上前动手无寸铁的人，一把揪住他的衣领"),
    // 卡：不替人做伪证
    ("cuie", "开口替人做伪证，把没有的事说成有"),
    // 卡：不多记一个字
    ("lugong", "提笔多记一个字，把没说过的话也写进去"),
];

fn breach_action(cid: &str) -> Option<&'static str> {
    BREACH_ACTIONS.iter().find(|(c, _)| *c == cid).map(|(_, a)| *a)
}

// ============================================================================
// §4 `SimModel`：种子驱动的规则化假模型
// ============================================================================

/// 决策生成器：输出**只**由 `(世界种子, tick_no, 角色 id)` 决定。
///
/// 🔴 刻意**不**依赖调用次数/顺序/prompt 文本：底线重生成会对同一角色同一拍再发一次
/// `roleDecide`（`MAX_BOTTOM_LINE_REGEN`），若输出依赖调用序，重生成就会拿到**不同**提案，
/// 「同种子恒同结果」当场破裂，整个跨版本回归失去意义。
///
/// 它**不是** record-and-replay：回放的是**规则**，不是真实模型响应。见文件头「诚实划界」。
struct SimModel {
    world_seed: u64,
    temper: Temper,
    /// 可作为行动目标的角色 id（含 NPC），按 fixture 声明序（不是 map 序）。
    cast: Vec<String>,
    tick: AtomicI64,
    /// 纯观测：调用次数。**绝不参与任何抽取**（参与了就等于让输出依赖调用序）。
    calls: AtomicU64,
}

impl SimModel {
    fn new(world_seed: u64, temper: Temper) -> Self {
        let mut cast: Vec<String> = GOLDEN_MEMBERS.iter().map(|(c, _)| c.to_string()).collect();
        cast.push(GOLDEN_NPC.to_string());
        Self { world_seed, temper, cast, tick: AtomicI64::new(0), calls: AtomicU64::new(0) }
    }

    fn set_tick(&self, tick: i64) {
        self.tick.store(tick, Ordering::SeqCst);
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// 生成本拍该角色的决策 JSON。
    fn decide(&self, cid: &str) -> String {
        let tick = self.tick.load(Ordering::SeqCst);
        let seed_s = format!("{:016x}", self.world_seed);
        let tick_s = tick.to_string();
        let mut rng = substream(DOMAIN_SIM_DECISION, &[&seed_s, &tick_s, cid]);
        let mut beat = substream(DOMAIN_SIM_BEAT, &[&seed_s, &tick_s, cid]);

        let t = self.temper;
        // ① 底线触碰（独立掷点，与行动类别分开，改哪个都不影响另一个的取值流）。
        if t.breach_pct > 0 && (beat.below(100) as u32) < t.breach_pct {
            if let Some(action) = breach_action(cid) {
                // 目标留空：底线筛查只看 action，目标与它无关；留空可免掉一层无谓的关系噪声。
                return decision_json("非做不可", action, false, "", &[], 60);
            }
        }

        // ② 行动类别（整数百分位，无浮点 RNG）。
        let roll = rng.below(100) as u32;
        let (actions, intents, needs_target) = if roll < t.friendly_pct {
            (FRIENDLY_ACTIONS, FRIENDLY_INTENTS, true)
        } else if roll < t.friendly_pct + t.hostile_pct {
            (HOSTILE_ACTIONS, HOSTILE_INTENTS, true)
        } else {
            (NEUTRAL_ACTIONS, NEUTRAL_INTENTS, false)
        };

        let action_tpl = actions[rng.below(actions.len())];
        let intent = intents[rng.below(intents.len())];

        // ③ 目标：从阵容里挑一个非己方角色（定序遍历，不用 map 序）。
        let mut targets: Vec<&str> = Vec::new();
        let mut action = action_tpl.to_string();
        if needs_target {
            let others: Vec<&String> = self.cast.iter().filter(|c| c.as_str() != cid).collect();
            if !others.is_empty() {
                let pick = others[rng.below(others.len())].as_str();
                action = action.replace("{t}", pick);
                targets.push(pick);
            } else {
                action = action.replace("{t}", "席间众人");
            }
        }

        // ④ 发言与时长（次级子流）。
        let will_speak = (beat.below(100) as u32) < t.speak_pct;
        let purpose = if will_speak { "把这一句说完" } else { "" };
        let duration = 50 + beat.below(3) as i64 * 10;

        decision_json(intent, &action, will_speak, purpose, &targets, duration)
    }
}

#[async_trait]
impl ModelClient for SimModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let tick = self.tick.load(Ordering::SeqCst);
        let content = match spec.agent.as_str() {
            "roleDecide" => {
                let cid = cid_of_decide_prompt(&spec.user);
                if cid.is_empty() {
                    // prompt 包裹变了 ⇒ 角色维度失效 ⇒ 整批仿真悄悄退化成"所有人同一个决策"。
                    // 这是必须炸出来的静默失效，不是可以兜底的边界情况。
                    panic!(
                        "SimModel 无法从 roleDecide prompt 解析角色 id —— \
                         `decide::build_decide_user_prompt` 的包裹变了？prompt 头部：{}",
                        spec.user.chars().take(40).collect::<String>()
                    );
                }
                self.decide(&cid)
            }
            "director" => json!({
                "situation": format!("长安夜宴第 {tick} 拍：灯烛又添一盏，席上各人心事未散。")
            })
            .to_string(),
            "arbiter" => r#"{"outcomes":[]}"#.to_string(),
            "writer" => json!({
                "prose": format!("第 {tick} 拍：杯盏交错，话在嘴边又咽了回去。")
            })
            .to_string(),
            "critic" => {
                r#"{"characterConsistencyIssues":[],"causalIssues":[],"revisionSuggestions":[]}"#
                    .to_string()
            }
            other => panic!("SimModel 未覆盖的环节：{other}（新增环节须显式编排，不做兜底）"),
        };
        // token 计量沿用 golden 的逐环节常量：成本对**调用构成**敏感，对调用内容不敏感。
        let (input_tokens, output_tokens) = agent_tokens(&spec.agent);
        Ok(ModelOutput { content, input_tokens: Some(input_tokens), output_tokens: Some(output_tokens) })
    }
}

// ============================================================================
// §5 播种：共享 fixture（每库一次） + 逐世界播种
// ============================================================================

/// 「先查再插」的存在性判断。
///
/// **不是**因为 `ON CONFLICT` 被禁——它双库都支持，仓库里已有多处在用（`db.rs` 的可移植子集
/// 禁的是 JSONB / serial / NOW() / `strftime` / `date_trunc` 这类方言）。这里用先查再插，
/// 单纯因为本工装是**单线程离线**播种：一个测试库、一条连接、零并发写入，
/// 查-插之间不存在竞态窗口，而先查再插比 UPSERT 更直白（读代码的人一眼看出"共享 fixture 只播一次"）。
/// 若将来这段被搬进有并发的路径，**必须**换成 UPSERT——那时竞态是真的。
async fn row_exists(db: &AnyPool, sql: &str, bind: &str) -> bool {
    sqlx::query(sql).bind(bind).fetch_optional(db).await.unwrap().is_some()
}

const SIM_ROUTES_VERSION: &str = "sim-routes-v1";
const SIM_PROMPTS_VERSION: &str = "sim-prompts-v1";
const SIM_TEMPLATE_VERSION: i64 = 1;
const SIM_STAR_RATING: i64 = 3;

fn sim_template_id(scenario: &str) -> String {
    format!("sim-tpl-{scenario}")
}

/// 一个测试库里共享的部分：模型路由 + 用户 + 云角色卡（**每库播一次**）。
async fn seed_sim_shared(state: &AppState) {
    let db = &state.db;
    if !row_exists(db, "SELECT 1 FROM model_routes WHERE version = $1", SIM_ROUTES_VERSION).await {
        seed_model_routes(db, SIM_ROUTES_VERSION).await;
    }
    for (cid, uid) in GOLDEN_MEMBERS {
        if !row_exists(db, "SELECT 1 FROM users WHERE id = $1", uid).await {
            seed_user(db, uid).await;
        }
        if !row_exists(db, "SELECT 1 FROM cloud_characters WHERE id = $1", cid).await {
            sqlx::query(
                "INSERT INTO cloud_characters (id, owner_id, local_card_id, version, card_json, \
                 rights_declaration, moderation, withdrawn, created_at) \
                 VALUES ($1, $2, 'local', 1, $3, 'original', 'approved', 0, $4)",
            )
            .bind(*cid)
            .bind(*uid)
            .bind(golden_card_json(cid))
            .bind(now_ms())
            .execute(db)
            .await
            .unwrap();
        }
    }
}

/// 场景模板（每场景一份，同场景的多个副本共用 ⇒ `template_version` 与骨架字节一致）。
async fn seed_sim_template(state: &AppState, sc: &SimScenario) {
    let id = sim_template_id(sc.name);
    if row_exists(&state.db, "SELECT 1 FROM world_templates WHERE id = $1", &id).await {
        return;
    }
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, official, \
         version, moderation, star_rating, created_at) \
         VALUES ($1, $2, 'idle', $3, '{\"mode\":\"open\"}', 1, $4, 'approved', $5, $6)",
    )
    .bind(&id)
    .bind(format!("长安夜宴（仿真·{}）", sc.name))
    .bind(sim_skeleton(sc))
    .bind(SIM_TEMPLATE_VERSION)
    .bind(SIM_STAR_RATING)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .unwrap();
}

/// 世界 id：**由种子确定性派生**（world_id 进 `instance_seed`，用 uuid 就等于每次跑不同副本）。
fn sim_world_id(scenario: &str, seed: u64) -> String {
    format!("wld-sim-{scenario}-{seed:016x}")
}

/// 建一个 running 的仿真世界（字段与 `create_world_tx` 一一对应；直接 INSERT 以钉死 world_id）。
async fn seed_sim_world(state: &AppState, sc: &SimScenario, world_id: &str) {
    let db = &state.db;
    let now = now_ms();
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, host_user_id, member_limit, \
         tick_per_day, timeline_mode, lethality, assembled_json, state_revision, narrative_state_json, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'idle', '长安夜宴', 'running', 'official', NULL, 10, 3, 'event', \
         $7, NULL, 0, '{}', $8, $9)",
    )
    .bind(world_id)
    .bind(sim_template_id(sc.name))
    .bind(SIM_TEMPLATE_VERSION)
    .bind(muse_engine::ENGINE_VERSION)
    .bind(SIM_PROMPTS_VERSION)
    .bind(SIM_ROUTES_VERSION)
    .bind(sc.lethality)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO world_budgets (world_id, daily_token_budget, daily_cny_budget_cents, \
         spent_tokens_today, budget_day, fused, updated_at) VALUES ($1, 100000000, 0, 0, '', 0, $2)",
    )
    .bind(world_id)
    .bind(now)
    .execute(db)
    .await
    .unwrap();

    // 🔴 `joined_at` 必须**互不相同**：`load_active_cards` 的 `ORDER BY joined_at` 无次级排序键，
    // 撞毫秒时行序由数据库决定 ⇒ 装配产物顺序在重放间漂移（golden 文件末「已知非确定性来源」登记在案）。
    for (idx, (cid, uid)) in GOLDEN_MEMBERS.iter().enumerate() {
        sqlx::query(
            "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
             VALUES ($1, $2, $3, $4, '{}', 'active', $5)",
        )
        .bind(format!("wm-{world_id}-{idx}"))
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
// §6 驱动：一个世界从开局到收尾
// ============================================================================

/// 一次世界试跑的结构化结果。
#[derive(Debug, Clone)]
struct SimRunRecord {
    scenario: &'static str,
    world_id: String,
    seed: u64,
    /// 逐拍驱动结果（`TickStatus` 的稳定机读串）。
    tick_outcomes: Vec<String>,
    /// 驱动结束的原因（`concluded` / `fused` / `failed` / `not_running` / `drive_cap`）。
    stop_reason: &'static str,
    /// 模型调用总次数（调用构成指纹；桩下与内容无关，只反映管线形状）。
    model_calls: u64,
    /// 实测 token 累计。
    spent_tokens: i64,
    facts: WorldQualityFacts,
}

impl SimRunRecord {
    fn to_json(&self) -> Value {
        json!({
            "scenario": self.scenario,
            "worldId": self.world_id,
            "seed": format!("{:016x}", self.seed),
            "tickOutcomes": self.tick_outcomes,
            "stopReason": self.stop_reason,
            "modelCalls": self.model_calls,
            "spentTokens": self.spent_tokens,
            "status": self.facts.status,
            "conclusionReason": self.facts.conclusion_reason,
            "endingId": self.facts.ending_id,
            "committedTicks": self.facts.committed_ticks,
            "blockedTicks": self.facts.blocked_ticks,
            "terminalTicks": self.facts.terminal_ticks,
            "gatedTicks": self.facts.gated_ticks,
            "failedTicks": self.facts.failed_ticks,
            "eventsTotal": self.facts.events_total,
            "eventsWithheld": self.facts.events_withheld,
        })
    }
}

/// `TickStatus` → 稳定机读串（进结构化结果，前端/基线按它分桶，不要改）。
fn tick_outcome_str(s: &TickStatus) -> String {
    match s {
        TickStatus::Done => "done".to_string(),
        TickStatus::Skipped(r) => format!("skipped:{r}"),
        TickStatus::Fused => "fused".to_string(),
        TickStatus::Failed => "failed".to_string(),
        TickStatus::Concluded => "concluded".to_string(),
    }
}

/// 驱动一拍：读当前 `state_revision` → 排 tick → 走**生产同路径** `process_tick_with_model`。
async fn drive_tick(
    state: &AppState,
    model: &Arc<SimModel>,
    world_id: &str,
    tick_no: i64,
) -> TickStatus {
    let rev = load_world(&state.db, world_id).await.unwrap().state_revision;
    insert_tick(&state.db, world_id, tick_no, rev).await.unwrap();
    model.set_tick(tick_no);
    let mc: Arc<dyn ModelClient> = model.clone();
    process_tick_with_model(state, world_id, tick_no, mc).await.unwrap()
}

/// 跑一个世界：**从开局到收尾**（或到驱动上限）。
async fn run_world(state: &AppState, sc: &SimScenario, seed: u64) -> SimRunRecord {
    let world_id = sim_world_id(sc.name, seed);
    seed_sim_template(state, sc).await;
    seed_sim_world(state, sc, &world_id).await;

    let model = Arc::new(SimModel::new(seed, sc.temper));
    let mut outcomes: Vec<String> = Vec::new();
    let mut stop_reason = "drive_cap";

    for tick_no in 0..sc.max_drive_ticks {
        let status = drive_tick(state, &model, &world_id, tick_no).await;
        outcomes.push(tick_outcome_str(&status));
        match status {
            TickStatus::Concluded => {
                stop_reason = "concluded";
                break;
            }
            TickStatus::Fused => {
                stop_reason = "fused";
                break;
            }
            TickStatus::Failed => {
                stop_reason = "failed";
                break;
            }
            // 世界已停机（并发结算 / 已 ended）：再排拍没有意义。
            TickStatus::Skipped("world_not_running") | TickStatus::Skipped("terminal") => {
                stop_reason = "not_running";
                break;
            }
            _ => {}
        }
    }

    let spent_tokens: i64 =
        sqlx::query_scalar("SELECT spent_tokens_today FROM world_budgets WHERE world_id = $1")
            .bind(&world_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    let facts = collect_world_facts(&state.db, &world_id).await.unwrap();

    SimRunRecord {
        scenario: sc.name,
        world_id,
        seed,
        tick_outcomes: outcomes,
        stop_reason,
        model_calls: model.calls(),
        spent_tokens,
        facts,
    }
}

/// 一次套件试跑的结构化结果。
struct SimSuiteReport {
    suite_seed: String,
    runs: Vec<SimRunRecord>,
    quality: WorldQualityReport,
}

impl SimSuiteReport {
    /// **人读面**：逐世界明细 + 三指标 + 诚实边界。
    #[allow(dead_code)] // 打印用（`cargo test -- --nocapture` 时看形状），不参与断言。
    fn to_json(&self) -> Value {
        // 场景意图随报告一起走：读数的人得知道每个场景是**故意**压什么的，
        // 否则 `attrition` 的 0% 完读率会被读成事故，而它恰恰是编排出来的负样本。
        let scenarios: Vec<Value> = standard_suite()
            .iter()
            .map(|sc| json!({ "scenario": sc.name, "intent": sc.intent, "replicas": sc.replicas }))
            .collect();
        json!({
            "suiteSeed": self.suite_seed,
            "engineVersion": muse_engine::ENGINE_VERSION,
            "scenarios": scenarios,
            "runs": self.runs.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
            "quality": self.quality.to_json(QualitySource::SimulatedStub),
        })
    }

    /// **机比面（跨版本回归比对口径）**：三指标计数 + 逐世界的收尾/阻断形状。
    ///
    /// 收录规则：只收**整数与稳定枚举串**。刻意不收 `worldId`（含种子哈希，换套件种子即全变）、
    /// 不收 `modelCalls`/`spentTokens`（它们是成本基线，属 golden 的逐拍精确断言范畴，
    /// 混进来会让"完读率掉了"和"多调了一次审校"报同一个错）。
    fn baseline_json(&self) -> Value {
        let per_scenario: Vec<Value> = {
            let mut by: BTreeMap<&str, Vec<&SimRunRecord>> = BTreeMap::new();
            for r in &self.runs {
                by.entry(r.scenario).or_default().push(r);
            }
            by.into_iter()
                .map(|(name, rs)| {
                    let mut stop_reasons: BTreeMap<&str, i64> = BTreeMap::new();
                    let mut conclusion_reasons: BTreeMap<String, i64> = BTreeMap::new();
                    for r in &rs {
                        *stop_reasons.entry(r.stop_reason).or_insert(0) += 1;
                        let k = if r.facts.conclusion_reason.is_empty() {
                            "(unfinished)".to_string()
                        } else {
                            r.facts.conclusion_reason.clone()
                        };
                        *conclusion_reasons.entry(k).or_insert(0) += 1;
                    }
                    json!({
                        "scenario": name,
                        "replicas": rs.len(),
                        "stopReasons": stop_reasons,
                        "conclusionReasons": conclusion_reasons,
                        "committedTicks": rs.iter().map(|r| r.facts.committed_ticks).sum::<i64>(),
                        "blockedTicks": rs.iter().map(|r| r.facts.blocked_ticks).sum::<i64>(),
                        "eventsTotal": rs.iter().map(|r| r.facts.events_total).sum::<i64>(),
                    })
                })
                .collect()
        };
        json!({
            "suiteSeed": self.suite_seed,
            "quality": self.quality.baseline_json(),
            "perScenario": per_scenario,
        })
    }
}

/// 跑整套：**场景 × 副本**，全部落在同一个库里（模板/角色卡共享，世界互不干扰）。
async fn run_suite(state: &AppState, suite_seed: &str, scenarios: &[SimScenario]) -> SimSuiteReport {
    seed_sim_shared(state).await;
    let mut runs: Vec<SimRunRecord> = Vec::new();
    for sc in scenarios {
        for idx in 0..sc.replicas {
            let seed = world_seed(suite_seed, sc.name, idx);
            runs.push(run_world(state, sc, seed).await);
        }
    }
    let facts: Vec<WorldQualityFacts> = runs.iter().map(|r| r.facts.clone()).collect();
    let quality = WorldQualityReport::of(&facts);
    SimSuiteReport { suite_seed: suite_seed.to_string(), runs, quality }
}

// ============================================================================
// §7 跨版本基线：`simulation/baseline.json`
// ============================================================================

/// 基线文件（随代码入库；`include_str!` 编译期内联，绝不放运行时目录）。
const BASELINE_JSON: &str = include_str!("simulation/baseline.json");

/// 标准套件的种子。**改它等于换一整批世界**，基线必须同步重算。
const STANDARD_SUITE_SEED: &str = "museai-sim-2026-07-26";

/// 重算基线的落盘路径（仅 `MUSEAI_SIM_UPDATE_BASELINE=1` 时写）。
fn baseline_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/simulation/baseline.json")
}

// ============================================================================
// §8 用例 —— (1) 语料自检：压测的"自变量"必须真的是自变量
// ============================================================================

/// 行动语料必须落在预期的语义类别里，且不越界踩到不可逆/底线。
///
/// 这是**整批仿真的地基**：语料一旦静默串类（比如某条"友善"句子里混进一个敌对词），
/// 关系演化、R4 冲突、乃至最终的完读率都会跟着变，而三指标只会显示"数变了"，
/// 根本指不出是语料的问题。所以在这里逐条钉死。
#[test]
fn sim_corpus_avoids_keyword_traps() {
    // 与 `relation_dynamics::RelationRules` / `IrreversibleRules` 逐字同源（改一处必须改两处；
    // 引擎侧是私有类型，无法直接引用，故此处显式抄写并由本用例充当"抄错就报警"的哨兵）。
    let rupture = ["背叛", "叛变", "叛逃", "反目成仇", "反目", "决裂", "绝交", "断绝"];
    let hostile = [
        "攻击", "袭", "杀", "伤", "威胁", "抢", "夺", "斥", "骗", "偷", "揭穿", "对抗", "阻拦",
        "挡", "逼", "囚",
    ];
    let friendly = [
        "帮", "助", "救", "护", "赠", "送", "安慰", "陪", "合作", "结盟", "道谢", "致歉", "坦白",
        "信任", "分享",
    ];
    let death = [
        "杀死", "杀掉", "杀了", "杀害", "处死", "赐死", "斩杀", "毒死", "勒死", "绞死", "自尽",
        "自刎", "殉", "同归于尽",
    ];
    let exit = [
        "流放", "放逐", "逐出", "驱逐", "永远离开", "远走高飞", "退隐", "归隐", "遁入空门",
        "出走", "永别",
    ];
    let hit = |text: &str, set: &[&str]| set.iter().any(|k| text.contains(k));

    // 友善语料：命中 friendly，不命中 hostile/rupture。
    for (action, intent) in FRIENDLY_ACTIONS.iter().flat_map(|a| FRIENDLY_INTENTS.iter().map(move |i| (*a, *i))) {
        let text = format!("{action}{intent}");
        assert!(hit(&text, &friendly), "友善语料未命中友善词：{text}");
        assert!(!hit(&text, &hostile), "友善语料混进敌对词（会被判成敌对）：{text}");
        assert!(!hit(&text, &rupture), "友善语料混进破裂词：{text}");
    }
    // 敌对语料：命中 hostile，不命中 rupture（破裂优先级更高，会盖掉敌对）。
    for (action, intent) in HOSTILE_ACTIONS.iter().flat_map(|a| HOSTILE_INTENTS.iter().map(move |i| (*a, *i))) {
        let text = format!("{action}{intent}");
        assert!(hit(&text, &hostile), "敌对语料未命中敌对词：{text}");
        assert!(!hit(&text, &rupture), "敌对语料混进破裂词（会被判成破裂）：{text}");
    }
    // 中性语料：三张表全不命中。
    for (action, intent) in NEUTRAL_ACTIONS.iter().flat_map(|a| NEUTRAL_INTENTS.iter().map(move |i| (*a, *i))) {
        let text = format!("{action}{intent}");
        assert!(!hit(&text, &friendly), "中性语料混进友善词：{text}");
        assert!(!hit(&text, &hostile), "中性语料混进敌对词：{text}");
        assert!(!hit(&text, &rupture), "中性语料混进破裂词：{text}");
    }
    // 三档语料一律不得触发不可逆（同意门是 golden 的专项，不是压测的噪声源）。
    for action in FRIENDLY_ACTIONS.iter().chain(HOSTILE_ACTIONS).chain(NEUTRAL_ACTIONS) {
        assert!(!hit(action, &death), "常规语料含致死词，会触发同意门：{action}");
        assert!(!hit(action, &exit), "常规语料含退场词，会触发同意门：{action}");
        assert!(!hit(action, &rupture), "常规语料含关系破裂词，会触发同意门：{action}");
    }

    // 三档权重之和必须 = 100，否则中性档会静默吃掉溢出的百分位。
    for sc in standard_suite() {
        let t = sc.temper;
        assert_eq!(
            t.friendly_pct + t.hostile_pct + t.neutral_pct,
            100,
            "场景 {} 的三档权重之和必须为 100",
            sc.name
        );
        assert!(t.speak_pct <= 100 && t.breach_pct <= 100, "场景 {} 的百分位越界", sc.name);
        assert!(sc.max_drive_ticks > 0 && sc.replicas > 0, "场景 {} 的驱动上限/副本数必须为正", sc.name);
    }
}

/// 常规语料**不得**踩到 fixture 四张卡的底线；`BREACH_ACTIONS` 则必须真的踩到。
///
/// 前半条守的是"阻断率的自变量干净"：常规场景里出现的阻断必须来自真实回归，不是语料噪声。
/// 后半条守的是"阻断率的正样本有效"：卡改了、底线改了、`forbidden_needles` 的切法改了，
/// `deadlock` 场景就会静默退化成"没有任何阻断"，而基线只会显示 blockedTicks 从 8 变成 0——
/// 那时候没人知道是引擎修好了还是压测失效了。所以在这里用**引擎的真函数**判定。
#[test]
fn sim_breach_actions_really_breach() {
    use muse_engine::character::types::CharacterCardV2;
    use muse_engine::narrative::arbiter::{card_bottom_lines, screen_bottom_lines};
    use muse_engine::narrative::types::{RoleDecision, SpeakIntent};

    let mut lines: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut all_cids: Vec<String> = GOLDEN_MEMBERS.iter().map(|(c, _)| c.to_string()).collect();
    all_cids.push(GOLDEN_NPC.to_string());
    for cid in &all_cids {
        let card: CharacterCardV2 = serde_json::from_value(golden_card_value(cid)).unwrap();
        lines.insert(cid.clone(), card_bottom_lines(&card));
    }

    let decision = |cid: &str, action: &str| RoleDecision {
        decision_id: format!("d-{cid}"),
        character_id: cid.to_string(),
        intent: String::new(),
        action: action.to_string(),
        speak: SpeakIntent { will_speak: false, purpose: String::new() },
        targets: Vec::new(),
        acceptable_costs: Vec::new(),
        predictions: Vec::new(),
        duration: 60,
    };

    // ① 每个角色都有一条**可用**的 breach 行动，且真被引擎判为违反底线。
    for cid in &all_cids {
        let action = breach_action(cid)
            .unwrap_or_else(|| panic!("角色 {cid} 缺 BREACH_ACTIONS 条目 —— deadlock 场景会退化"));
        let hits = screen_bottom_lines(&lines, &[decision(cid, action)]);
        assert_eq!(
            hits.len(),
            1,
            "角色 {cid} 的 breach 行动没有被 `screen_bottom_lines` 判为违反底线：{action}\n\
             （卡上的底线改了？`forbidden_needles` 的切法改了？）"
        );
    }

    // ② 全员同时 breach ⇒ 命中数 = 决策数 ⇒ 引擎会判整拍 blocked（`run_round` 的 ③ 分支）。
    let all: Vec<RoleDecision> =
        all_cids.iter().map(|c| decision(c, breach_action(c).unwrap())).collect();
    assert_eq!(
        screen_bottom_lines(&lines, &all).len(),
        all.len(),
        "全员 breach 必须**全部**命中，少一个就不会触发整拍 blocked"
    );

    // ③ 常规语料一条都不许命中任何角色的底线（含替换目标后的形态）。
    for cid in &all_cids {
        for tpl in FRIENDLY_ACTIONS.iter().chain(HOSTILE_ACTIONS).chain(NEUTRAL_ACTIONS) {
            for target in all_cids.iter().chain(std::iter::once(&"席间众人".to_string())) {
                let action = tpl.replace("{t}", target);
                assert!(
                    screen_bottom_lines(&lines, &[decision(cid, &action)]).is_empty(),
                    "常规语料踩到了 {cid} 的底线（阻断率会被语料噪声污染）：{action}"
                );
            }
        }
    }
}

// ============================================================================
// §9 用例 —— (2) 确定性：同种子恒同结果
// ============================================================================

/// 同一个种子跑两遍，**逐字节相同**。
///
/// 不可复现的压测结果没有任何回归价值——"完读率从 80% 掉到 60%"如果可能只是随机抖动，
/// 这个数就永远不能用来卡门槛。故这条是三指标能被当作门槛的**前提**，不是锦上添花。
///
/// 用**独立的库**跑两遍同一个 world_id。⚠️ `stall_tracker()` 是进程级全局、按 world_id 分键，
/// 第一遍留下的连续 blocked 计数会漏进第二遍，使第二遍的导演 prompt 携带 `stall_hint`。
/// 这里之所以仍然逐字节相等，是因为 **`SimModel` 的输出与 prompt 内容完全无关**——
/// 这正是它被设计成"只看 (种子, tick, cid)"的原因之一。换成真实模型时这条不再成立。
#[tokio::test]
async fn simulation_is_reproducible_under_the_same_seed() {
    let scenarios = vec![standard_suite()[0].clone(), standard_suite()[3].clone()];

    let a = {
        let state = test_state().await;
        run_suite(&state, "repro-seed-A", &scenarios).await
    };
    let b = {
        let state = test_state().await;
        run_suite(&state, "repro-seed-A", &scenarios).await
    };

    assert_eq!(
        a.baseline_json(),
        b.baseline_json(),
        "同种子两遍跑出了不同的三指标 —— 确定性契约破裂，仿真结果不可作为回归依据"
    );
    let ja: Vec<Value> = a.runs.iter().map(|r| r.to_json()).collect();
    let jb: Vec<Value> = b.runs.iter().map(|r| r.to_json()).collect();
    assert_eq!(ja, jb, "逐世界明细也必须逐字节相同（含逐拍结果与 token 计量）");
}

/// 换种子必须换局面：否则「种子」这个自变量是假的，一套种子等于一个世界。
#[tokio::test]
async fn different_seeds_produce_different_worlds() {
    let scenarios = vec![standard_suite()[0].clone()];
    let a = {
        let state = test_state().await;
        run_suite(&state, "diverge-seed-A", &scenarios).await
    };
    let b = {
        let state = test_state().await;
        run_suite(&state, "diverge-seed-B", &scenarios).await
    };
    let shape = |r: &SimSuiteReport| -> Vec<Value> {
        r.runs.iter().map(|x| json!([&x.tick_outcomes, x.facts.events_total, &x.facts.ending_id])).collect()
    };
    assert_ne!(
        shape(&a),
        shape(&b),
        "两个不同套件种子跑出了完全一样的世界形状 —— 种子没有真的进采样，压测覆盖面是假的"
    );
}

// ============================================================================
// §10 用例 —— (3) 三指标有分辨力（每个指标都必须能被推动）
// ============================================================================

/// 三个指标都必须**既能取到好值、也能取到坏值**。
///
/// 一个恒为 100% 的完读率、恒为 0 的阻断率、只有一个桶的结局分布，都测不出任何回归——
/// 它们看起来永远绿，直到某天真的坏了也依然绿。所以标准套件被刻意编排成
/// 「三种失败形态各有正样本」，本用例就是那份编排的守护者。
#[tokio::test]
async fn standard_suite_metrics_have_discriminating_power() {
    let state = test_state().await;
    let report = run_suite(&state, STANDARD_SUITE_SEED, &standard_suite()).await;
    let q = &report.quality;

    // ---- 完读率：既不是 0 也不是 1 ----
    let c = &q.completion;
    assert_eq!(c.worlds, 10, "标准套件 = 3+3+2+2 个世界");
    assert!(
        c.natural > 0,
        "没有任何世界自然收尾 —— 完读率恒为 0，指标失去上行分辨力。逐世界明细：{}",
        serde_json::to_string_pretty(&report.to_json()).unwrap()
    );
    assert!(
        c.natural < c.worlds,
        "所有世界都自然收尾 —— 完读率恒为 1，指标失去下行分辨力（attrition 场景应当被强制收尾）"
    );
    assert!(c.forced + c.unfinished > 0, "必须存在非自然收尾的世界作为负样本");

    // ---- 阻断率：deadlock 场景必须真的产生阻断 ----
    let b = &q.blocking;
    assert!(
        b.blocked_ticks > 0,
        "阻断率恒为 0 —— deadlock 场景没有触发整拍 blocked，压测正样本失效"
    );
    assert!(
        b.blocked_ticks < b.engine_ticks,
        "所有跑过的拍都被阻断 —— 说明常规场景也在阻断，语料或规则层出了问题"
    );
    assert_eq!(
        b.events_withheld, 0,
        "桩文本不该命中敏感词库；非 0 说明语料里混进了敏感词（也说明这个通道在仿真里测不了内容安全）"
    );

    // ---- 结局分布：桶必须分得开（真实结局 / 收尾无结局 / 未收尾三态不混同）----
    let e = &q.endings;
    assert!(e.by_ending.len() >= 2, "结局分布只有一个桶 —— 分布形状没有信息量：{:?}", e.by_ending);
    assert!(e.by_kind.len() >= 2, "收尾类型只有一种 —— 自然/强制没有同时出现：{:?}", e.by_kind);
    assert!(e.with_ending > 0, "没有任何世界落到真实结局上：{:?}", e.by_ending);
    // ---- 结局这一维必须真的随实例种子分叉（总规格 §5「一个模板，千个平行世界」）----
    // 本套件所有世界共用同一模板 + 同一阵容，唯一变量是实例种子。若这里塌回 1，说明结局选择又一次
    // 把种子丢了（历史缺陷：`select_ending` 取 `enabledEndings` 首个，任务 #41 已修：装配层
    // `pick_ending` 按权重掷点定盘 → `/assembly/selectedEnding`）。
    assert!(
        e.distinct_endings >= 2,
        "同模板同阵容的一批世界只落到一个结局 —— 结局选择丢了实例种子（§5 在结局维不成立）：{:?}",
        e.by_ending
    );

    // ---- 完读率与强制收尾率的分母差异必须实际发生（口径纪律的端到端证据）----
    if c.unfinished > 0 {
        assert!(
            c.completion_rate() < 1.0 - c.forced_rate() + 1e-9,
            "存在未收尾世界时，完读率必须严格小于「1 − 强制收尾率」——\
             两者分母不同，这个差值正是「跑不完」被 §4.2 口径漏掉的部分"
        );
    }

    // 打印一份人读报告（`cargo test -- --nocapture` 可见），诚实边界随报告一起走。
    println!("{}", serde_json::to_string_pretty(&report.to_json()).unwrap());
}

// ============================================================================
// §10.1 结局随实例种子分叉（原「已登记发现」，任务 #41 已修，本节改为守护用例）
// ============================================================================

/// **端到端守护：同模板 + 同阵容下，结局必须随实例种子分叉**（总规格 §5「一个模板，千个平行世界」）。
///
/// 本用例的前身是一条**登记在案的生产缺陷**（`ending_selection_ignores_instance_seed_registered_finding`）：
/// 那时 `runtime::select_ending` 取 `enabledEndings` 的第一个元素，不掷点、不看 `instance_seed`，
/// 于是无论换多少个种子，所有世界都落到同一个结局，结局分布恒为单峰。
///
/// 修法（任务 #41）：掷点收在**装配层**——`assembly::weight_endings_scored` 把权重连同名单一起交出，
/// `assembly::pick_ending` 在 `Rng(instance_seed ^ DOMAIN_ENDING)` 子流上按权重抽一个，钉进
/// `/assembly/selectedEnding`；`runtime::select_ending` 只读它（缺失才回退到旧口径的首个，
/// 服务修复前已钉住的老实例）。
///
/// 本用例守两件事，缺一不可：
/// 1. 装配确实筛出了 ≥2 个候选（否则"能不能选出花样"根本无从观察，是模板问题不是选择器问题）；
/// 2. 4 个只有种子不同的世界，落到的结局**不止一个**。
#[tokio::test]
async fn ending_selection_varies_with_instance_seed() {
    let state = test_state().await;
    seed_sim_shared(&state).await;
    let sc = standard_suite()[0].clone();

    let mut endings: BTreeMap<String, i64> = BTreeMap::new();
    let mut enabled_counts: Vec<usize> = Vec::new();
    for idx in 0..4usize {
        let seed = world_seed("ending-diversity-probe", sc.name, idx);
        let record = run_world(&state, &sc, seed).await;
        assert_eq!(record.facts.status, "ended", "探针世界必须跑到收尾才能观察结局");
        *endings.entry(record.facts.ending_id.clone()).or_insert(0) += 1;

        let assembled: Option<String> =
            sqlx::query_scalar("SELECT assembled_json FROM worlds WHERE id = $1")
                .bind(&record.world_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        let v: Value = serde_json::from_str(&assembled.expect("已跑过的世界必有装配产物")).unwrap();
        let enabled: Vec<String> = v
            .pointer("/assembly/enabledEndings")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        enabled_counts.push(enabled.len());

        // 掷点只能在**已启用**的结局里选 —— 权重语义（未启用者永不上场）的端到端证据。
        let selected = v
            .pointer("/assembly/selectedEnding")
            .and_then(Value::as_str)
            .expect("装配层必须钉住 selectedEnding");
        assert!(
            enabled.iter().any(|e| e == selected),
            "定盘结局不在已启用名单内：selected={selected} enabled={enabled:?}"
        );
        assert_eq!(record.facts.ending_id, selected, "世界落定的结局 = 装配层定盘的那一个");
    }

    assert!(
        enabled_counts.iter().all(|n| *n >= 2),
        "装配筛出的候选结局不足 2 个，本用例观察不到「有得选」：{enabled_counts:?}"
    );
    assert!(
        endings.len() >= 2,
        "只有种子不同的一批世界全落到同一个结局 —— 结局选择又把 instance_seed 丢了（§5 在结局维不成立）。\
         实测分布：{endings:?}"
    );
}

// ============================================================================
// §10.2 阻断拍的成本必须记真（任务 #42）
// ============================================================================

/// **端到端守护：被阻断的那一拍照样烧了 token，账上必须有它**（任务 #42）。
///
/// 前身也是一条生产缺陷：`finish_tick_noop` 无条件写 `cost_tokens=0`，而 `blocked` 拍
/// 走的正是它。可这一拍引擎已经跑完了整个回合——导演、逐角色决策、仲裁、可能的底线重生成
/// 全都发出去了；被拦下的是**提交**，不是**计算**。于是 `deadlock` 场景 8 个阻断拍
/// 在账上一分钱不花，`world_budgets.spent_tokens_today` 恒为 0。
///
/// 这不是一个内部指标的小数点问题：低估是**结构性偏向**的——越是被阻断多的世界
///（往往正是内容风险高、审核成本也高的那些）低估得越厉害，于是 `docs/VALIDATION.md`
/// 的 T3 / T5 两道商业门槛会在最危险的地方最乐观。
///
/// 本用例守四件事：
/// 1. `deadlock` 世界一拍都没提交（`committed_ticks == 0`）——保证下面看到的钱**只可能**来自阻断拍；
/// 2. 每个阻断拍的 `cost_tokens > 0`；
/// 3. `world_budgets.spent_tokens_today` == 逐拍成本之和（tick 账与预算账不许分叉，熔断读的是后者）；
/// 4. 🔴 **叙事 SLO 的拍域不许被这笔钱污染**：阻断拍进了成本，但绝不能进「无戏份」的分母
///    （见 `slo::TICK_DOMAIN` 的第三个条件）。一次改动同时推动成本上行、叙事指标不动，才是对的。
#[tokio::test]
async fn blocked_ticks_record_real_token_cost_without_polluting_narrative_slo() {
    let state = test_state().await;
    seed_sim_shared(&state).await;
    let sc = standard_suite()
        .into_iter()
        .find(|s| s.name == "deadlock")
        .expect("标准套件必须保留 deadlock 场景（阻断率与本用例的正样本）");
    let seed = world_seed("blocked-cost-probe", sc.name, 0);
    let record = run_world(&state, &sc, seed).await;

    // 1) 一拍都没提交 —— 后面看到的所有成本都只能来自阻断拍。
    assert!(record.facts.blocked_ticks > 0, "deadlock 场景必须真的产生阻断拍：{:?}", record.tick_outcomes);
    assert_eq!(
        record.facts.committed_ticks, 0,
        "deadlock 世界不该有任何提交拍，否则本用例分不清成本来自哪一类拍：{:?}",
        record.tick_outcomes
    );
    assert_eq!(record.facts.events_total, 0, "阻断拍不提交状态，也就不该落任何 world_events");

    // 2) 逐拍成本：阻断拍 > 0，其余（空转拍）恒 0。
    use sqlx::Row as _;
    let rows: Vec<(i64, i64, String)> = sqlx::query(
        "SELECT tick_no, cost_tokens, COALESCE(error, '') AS err FROM world_ticks \
         WHERE world_id = $1 ORDER BY tick_no ASC",
    )
    .bind(&record.world_id)
    .fetch_all(&state.db)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        (
            r.try_get::<i64, _>("tick_no").unwrap(),
            r.try_get::<i64, _>("cost_tokens").unwrap(),
            r.try_get::<String, _>("err").unwrap(),
        )
    })
    .collect();
    let blocked: Vec<&(i64, i64, String)> = rows.iter().filter(|(_, _, e)| e == "blocked").collect();
    assert_eq!(blocked.len() as i64, record.facts.blocked_ticks);
    for (tick_no, cost, _) in &blocked {
        assert!(
            *cost > 0,
            "阻断拍 {tick_no} 的成本记成了 {cost} —— 引擎跑完了整个回合，这一拍真的烧了 token（#42）"
        );
    }
    for (tick_no, cost, err) in rows.iter().filter(|(_, _, e)| e != "blocked") {
        assert_eq!(*cost, 0, "没跑过模型的拍 {tick_no}（{err}）必须记 0，记非 0 就是反方向的虚报");
    }

    // 3) tick 账 == 预算账（熔断读的是预算账；两者分叉 = 熔断器对阻断世界失效）。
    let total: i64 = rows.iter().map(|(_, c, _)| *c).sum();
    assert!(total > 0, "整个世界的账上一分钱都没有 —— 这正是 #42 的现场");
    assert_eq!(record.spent_tokens, total, "world_budgets.spent_tokens_today 必须等于逐拍成本之和");

    // 4) 叙事 SLO 拍域不被污染：这些拍有成本、但没演出来，不该进「无戏份」的分母。
    let streaks = crate::slo::world_silent_streaks(&state.db, &record.world_id).await.unwrap();
    assert!(
        streaks.values().all(|v| *v == 0),
        "阻断拍进了叙事 SLO 拍域 —— 成本口径与叙事口径必须分家，\
         否则「全员被晾着」会随阻断率一起虚涨：{streaks:?}"
    );

    // 5) 反方向的守卫：**没跑过模型的拍必须仍记 0**，一刀切改成非 0 就是反方向的虚报。
    //    这里用同一个世界现造一个空转拍（把世界置 paused → 下一拍命中 `world_not_running`，
    //    在引擎构建之前就返回），断言它既不记成本、也不动预算。
    sqlx::query("UPDATE worlds SET status='paused' WHERE id = $1")
        .bind(&record.world_id)
        .execute(&state.db)
        .await
        .unwrap();
    let noop_tick = sc.max_drive_ticks;
    let status = drive_tick(&state, &SimModel::new(seed, sc.temper).into(), &record.world_id, noop_tick).await;
    assert_eq!(status, TickStatus::Skipped("world_not_running"), "paused 世界的拍应命中空转分支");
    let noop_cost: i64 =
        sqlx::query_scalar("SELECT cost_tokens FROM world_ticks WHERE world_id = $1 AND tick_no = $2")
            .bind(&record.world_id)
            .bind(noop_tick)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(noop_cost, 0, "一次模型都没调过的拍必须记 0（#42 的反向边界）");
    let spent_after: i64 =
        sqlx::query_scalar("SELECT spent_tokens_today FROM world_budgets WHERE world_id = $1")
            .bind(&record.world_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(spent_after, total, "空转拍不该动日预算");
}

// ============================================================================
// §11 用例 —— (4) 跨版本回归：与committed 基线比对
// ============================================================================

/// **世界质量回归**：标准套件的三指标必须与入库基线逐字节一致。
///
/// 这是本模块的产出面——"这次改动让完读率掉了 / 阻断率涨了 / 结局塌进一个桶"
/// 在这里变成一次红测试，而不是上线三周后从运营数据里发现。
///
/// **基线怎么更新**：确认差异是**预期的产品/引擎变化**（而不是回归）之后，跑
/// ```text
/// MUSEAI_SIM_UPDATE_BASELINE=1 cargo test --manifest-path server/Cargo.toml simulation_
/// ```
/// 它会重写 `server/src/runtime/simulation/baseline.json`。**改基线必须和改动同一个提交**，
/// 且提交信息要写清"为什么这三个数应该变"——一个没有解释就被刷新的基线等于没有基线。
#[tokio::test]
async fn simulation_quality_matches_committed_baseline() {
    let state = test_state().await;
    let report = run_suite(&state, STANDARD_SUITE_SEED, &standard_suite()).await;
    let actual = report.baseline_json();

    if std::env::var("MUSEAI_SIM_UPDATE_BASELINE").as_deref() == Ok("1") {
        let doc = json!({
            "__doc": "世界质量回归基线（完读率/阻断率/结局分布）。由 `simulation_quality_matches_committed_baseline` \
                      在 MUSEAI_SIM_UPDATE_BASELINE=1 时重写。🔴 只收整数计数，不收比率（浮点不可逐字节比对）。\
                      🔴 数据源是种子驱动的假模型，这三个数**不度量内容质量**，见 runtime/simulation.rs 文件头「诚实划界」。",
            "meta": {
                "note": "meta 不参与比对，只供人读：引擎版本变了但三指标没变，是好消息，不该逼着刷基线。",
                "engineVersion": muse_engine::ENGINE_VERSION,
                "suiteSeed": STANDARD_SUITE_SEED,
            },
            "baseline": actual,
        });
        std::fs::write(baseline_path(), format!("{}\n", serde_json::to_string_pretty(&doc).unwrap()))
            .expect("写基线文件失败");
        println!("已重写基线：{}", baseline_path().display());
        return;
    }

    let stored: Value = serde_json::from_str(BASELINE_JSON).expect("baseline.json 必须是合法 JSON");
    let expected = &stored["baseline"];
    let stored_engine = stored["meta"]["engineVersion"].as_str().unwrap_or("?");

    assert_eq!(
        &actual,
        expected,
        "\n世界质量三指标与入库基线不一致。\n\
         基线引擎版本 = {stored_engine}，当前 = {}\n\
         实测：\n{}\n\
         这**不一定**是 bug——也可能是一次预期内的产品/引擎变化。请先判断：\n\
           · 完读率下降 ⇒ 主线推进或终局判定变难了（或某些局面开始走不下去）\n\
           · 阻断率上升 ⇒ 规则层（底线/硬节点/不变量）拦得更多了\n\
           · 结局分布塌陷 ⇒ 结局加权采样退化，一个模板只产一种结局\n\
         确认是预期变化后，用 MUSEAI_SIM_UPDATE_BASELINE=1 重跑本测试刷新基线，\n\
         并在同一个提交里写清为什么该变。\n",
        muse_engine::ENGINE_VERSION,
        serde_json::to_string_pretty(&actual).unwrap(),
    );
}

// ============================================================================
// §12 用例 —— (5) 单世界全生命周期：工装的最小可用形态
// ============================================================================

/// 单个世界从开局跑到自然收尾，产出结构化结果。
///
/// 这是工装的"最小可用形态"——总规格 §4 要的「脚本化跑通一个世界的完整生命周期」。
/// 上面的套件只是它的批量化。
#[tokio::test]
async fn single_world_runs_full_lifecycle_and_reports_structured_result() {
    let state = test_state().await;
    seed_sim_shared(&state).await;
    let sc = standard_suite()[0].clone();
    let seed = world_seed("lifecycle-seed", sc.name, 0);
    let record = run_world(&state, &sc, seed).await;

    assert_eq!(record.stop_reason, "concluded", "低冲突场景应当跑到自然收尾：{:?}", record.tick_outcomes);
    assert_eq!(record.facts.status, "ended");
    assert_eq!(record.facts.conclusion_reason, "mainline_complete");
    assert!(record.facts.is_complete_read(), "自然收尾 + 已停机 = 完读");
    assert!(record.facts.committed_ticks > 0, "必须真的跑过回合，不能是一路跳过");
    assert_eq!(record.facts.blocked_ticks, 0, "低冲突场景不该出现阻断");
    assert!(record.facts.events_total > 0, "必须产出叙事事件");
    assert!(record.spent_tokens > 0, "必须有实测 token 计量（预算通路走通）");
    assert!(record.model_calls > 0);

    // 结构化结果本身必须可序列化、字段齐全（下游要拿它做报表）。
    let j = record.to_json();
    for key in [
        "scenario", "worldId", "seed", "tickOutcomes", "stopReason", "modelCalls", "spentTokens",
        "status", "conclusionReason", "endingId", "committedTicks", "blockedTicks", "eventsTotal",
    ] {
        assert!(j.get(key).is_some(), "结构化结果缺字段 {key}");
    }

    // 单世界也能直接出三指标（口径与批量完全一致，只是分母是 1）。
    let q = WorldQualityReport::of(&[record.facts.clone()]);
    assert_eq!(q.completion.worlds, 1);
    assert_eq!(q.completion.natural, 1);
    assert!((q.completion.completion_rate() - 1.0).abs() < 1e-9);
    let human = q.to_json(QualitySource::SimulatedStub);
    assert_eq!(human["source"], "simulated-stub", "报告必须标明数据源是桩，不是生产");
}

// ============================================================================
// 诚实划界（与文件头呼应，放在末尾是为了让读完实现的人最后再看一遍）
// ============================================================================
//
// ✅ **本模块测得了**：
//    - 一个世界能否**在各种决策组合下**从开局走到收尾（而不只是在一条人写的剧本上）
//    - 规则层（底线筛查 / 硬节点 / 确定性不变量）在压力下的触发频率
//    - 终局判定三条路径（主线完成 / 时间上限 / 关键角色退场）在批量下的分布
//    - 上述三者的**跨版本漂移**（基线比对）
//
// ❌ **本模块测不了**（与 golden 的缺口同源，不重复声称）：
//    - OOC / 文本质量 / 剧情重复率：决策来自规则化的桩，不是模型
//    - 内容安全第 2 层：桩文本永不命中词库，`eventsWithheld` 恒 0（是"没测"，不是"没问题"）
//    - 换模型的真实成本：token 是逐环节常数，只反映**调用构成**
//    - 真实玩家行为：仿真里没有玩家干预、没有退出、没有托梦（这些 golden 各有专项）
//
// 🔜 **下一件事**：record-and-replay 的 `ModelClient`。有了它，本模块的场景表可以先用真实模型
//    录一批，之后换 Prompt / 引擎重放录像——**那时**这三个指标才同时含有内容质量成分，
//    也**只有那时**才谈得上「叙事质量已验证」。在它建成之前，本模块的任何数字都不得这样表述。
