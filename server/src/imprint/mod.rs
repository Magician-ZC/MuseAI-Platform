//! 世界线烙印（Worldline Imprint）—— 让「复刻内核 ≠ 复刻这张卡」。
//!
//! 提案与完整设计：`docs/build/spec-worldline-imprint.md`；存储见迁移 0054。
//! 本模块实现该提案的**全部五步**：表 · 确定性派生器 · 进实例种子 · 风化 · 措辞。
//! 第 5 步（进决策上下文）的服务端半边在本模块（[`PHRASES`] / [`phrase_imprints`] /
//! [`worldline_pasts`]），引擎半边是 `decide::append_worldline_imprints` 的 `yourPast`。
//!
//! 🔴 第 5 步**默认关**（`MUSE_WORLDLINE_IMPRINT_CONTEXT`，按世界解析），状态是
//! `Integrated` 而非 `Validated`：它是这套系统里唯一直接改模型 prompt 的一步，
//! 而它的效果**无法证伪**——「这张卡表现得不一样」是主观判断，要真验得做
//! 同内核 / 同世界 / 同种子的 A/B，而那需要真实模型凭据（本仓至今一次真实调用都没发生过）。
//!
//! 本模块另外承载**气运与机缘的点数与档位**（见文件中部那一节）——
//! 同一批烙印行的第二个用途：调这个世界的内容密度与幅度。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 这套系统的地基：**烙印让两张卡「不一样」，但不让任何一张「更强」**
//! ════════════════════════════════════════════════════════════════════════════
//! 最容易滑向的错误是把「经历」做成「成长数值」——那样老卡就比新卡强，
//! 直接撞平台红线第一条（不卖胜负与数值平权）。三条机制保证它不会：
//!
//! 1. **烙印不含优势语义。** `code` 陈述发生过什么，不陈述「因此更擅长什么」。
//!    同一条烙印落在不同内核上会产生**相反**的行为——方向由卡自己的决策模型决定。
//! 2. **烙印不进任何数值。** 它不碰 `RelationState`、不碰 `resources`、不碰任何仲裁判据。
//!    仲裁器读不到它（`arbiter.rs` 只认规则与叙事合理性）。
//! 3. **多不等于强。** [`weather`] 让烙印**恒定容量**：老卡不是「烙印更多」，是「烙印更旧」。
//!
//! 🔵 检验这条红线的一句话：**如果两张卡的烙印互换，谁会变强？**
//! 答案必须是「谁都不会，只是变得不一样」。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 确定性：派生是纯函数，风化也是
//! ════════════════════════════════════════════════════════════════════════════
//! [`derive_imprints`] 与 [`weather`] 都是**纯函数**：同一份 [`WorldFacts`] 恒得同一批烙印，
//! 同一批烙印恒得同一份风化结果。这不是洁癖——第 3 步要把烙印指纹喂进**实例种子**，
//! 而种子是引擎确定性契约的输入。派生只要带上时钟或迭代序，同一个世界重放两次就会抽出不同剧情。
//!
//! ⚠️ 因此本模块**禁止**：`now_ms()` 参与派生判据、HashMap 迭代序驱动输出、浮点比较。

use serde::Serialize;
use serde_json::json;
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use sqlx::{Any, Row, Transaction};

use crate::app::AppState;

use crate::db::{new_id, now_ms};
use crate::error::ApiError;

#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 类目与代码（🔴 中性事实陈述，不含评价词）
// ═══════════════════════════════════════════════════════════════════════════

/// 抉择痕：这张卡在这个世界里做过什么。
pub(crate) const KIND_CHOICE: &str = "choice";
/// 关系痕：与别人之间发生过什么。
pub(crate) const KIND_BOND: &str = "bond";
/// 境遇痕：经历过什么处境。
pub(crate) const KIND_CIRCUMSTANCE: &str = "circumstance";
/// 未竟痕：**什么没发生**。🔵 五类里最有戏的一类——它在下一个世界里会被认出来。
pub(crate) const KIND_UNFINISHED: &str = "unfinished";
/// 见闻痕：见过什么。
pub(crate) const KIND_WITNESS: &str = "witness";

/// 一条烙印（派生产物，未落库）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Imprint {
    pub character_id: String,
    pub kind: &'static str,
    pub code: &'static str,
    /// 确定性细节：**只放数字与枚举**，不放模型文本（那会让派生不可复算）。
    pub detail: serde_json::Value,
}

// ═══════════════════════════════════════════════════════════════════════════
// 派生输入：一次结算里能确定拿到的事实
// ═══════════════════════════════════════════════════════════════════════════

/// 一张卡在这个世界里的确定性事实。
///
/// ⚠️ 字段刻意都是**可从库里直接查出的量**，不含任何需要解析模型输出的东西——
/// 派生器的可复算性全靠这一点。
#[derive(Debug, Clone, Default)]
pub(crate) struct CharacterFacts {
    pub character_id: String,
    /// 终局那一刻仍在场（`world_members.status = 'active'`）。
    pub stayed_to_end: bool,
    /// 退场发生在第几拍（`None` = 没退场）。
    pub left_at_tick: Option<i64>,
    /// 里程碑贡献分（`world_contributions.milestone_score_milli`）。> 0 即推动过。
    pub milestone_score_milli: i64,
    /// 作为 actor 出现在 `world_events` 里的条数。
    pub event_count: i64,
}

/// 一个世界结算时的全部事实。
#[derive(Debug, Clone, Default)]
pub(crate) struct WorldFacts {
    pub world_id: String,
    /// 世界线崩塌（非正常收束）。
    pub collapsed: bool,
    /// 这个世界一共跑了几拍。
    pub total_ticks: i64,
    /// 🔴 必须按 `character_id` **升序**传入——派生输出的顺序进指纹，顺序变了种子就变了。
    /// 由 [`collect_world_facts_tx`] 保证（SQL `ORDER BY`），调用方自己构造时也必须保证。
    pub characters: Vec<CharacterFacts>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 第 2 步：派生器（纯函数）
// ═══════════════════════════════════════════════════════════════════════════

/// 从一个世界的确定性事实派生烙印。
///
/// **纯函数**：同一份 `facts` 恒得同一批烙印（含顺序）。
///
/// 每张卡产出 1-3 条。刻意不产更多——烙印的价值在**稀缺且具体**，
/// 一局给五条会让 [`weather`] 的容量在两三个世界内就被填满，老烙印全被挤成模糊倾向。
pub(crate) fn derive_imprints(facts: &WorldFacts) -> Vec<Imprint> {
    let mut out = Vec::new();
    for c in &facts.characters {
        let cid = c.character_id.clone();

        // ── 境遇痕：走完 or 中途退场（互斥，必出其一） ──────────────────────
        if c.stayed_to_end {
            out.push(Imprint {
                character_id: cid.clone(),
                kind: KIND_CIRCUMSTANCE,
                code: "walked_to_the_end",
                detail: json!({ "totalTicks": facts.total_ticks }),
            });
        } else {
            out.push(Imprint {
                character_id: cid.clone(),
                kind: KIND_CIRCUMSTANCE,
                code: "left_midway",
                // 「在第几拍、共几拍」——这两个数一起才说明「走了多远」。
                detail: json!({ "atTick": c.left_at_tick, "totalTicks": facts.total_ticks }),
            });
        }

        // ── 境遇痕：经历过世界线崩塌 ────────────────────────────────────────
        if facts.collapsed {
            out.push(Imprint {
                character_id: cid.clone(),
                kind: KIND_CIRCUMSTANCE,
                code: "witnessed_collapse",
                detail: json!({}),
            });
        }

        // ── 抉择痕 / 未竟痕：推动过里程碑，还是一个都没推动 ──────────────────
        //
        // 🔴 这一对是**互斥**的，且**未竟那一半才是重点**：其余各类记的是「发生了什么」，
        // 未竟痕记的是「**什么没发生**」——一个已经在这张卡面前的机会，它没走到。
        // 它在下一个世界里会被认出来，而它**完全不构成优势**（敏感不等于做得成）。
        if c.milestone_score_milli > 0 {
            out.push(Imprint {
                character_id: cid.clone(),
                kind: KIND_CHOICE,
                code: "pushed_a_milestone",
                detail: json!({ "scoreMilli": c.milestone_score_milli }),
            });
        } else {
            out.push(Imprint {
                character_id: cid.clone(),
                kind: KIND_UNFINISHED,
                code: "no_milestone_reached",
                detail: json!({ "totalTicks": facts.total_ticks }),
            });
        }

        // ── 见闻痕：在这个世界的公共事实里留下过痕迹 ────────────────────────
        //
        // ⚠️ 只记「有没有」，**不记多少**：把次数做成阈值就会变成「活跃度」，
        // 而活跃度是一种绩效评价——那正是 §8「结算不设勤奋度指标」明令排除的东西。
        if c.event_count > 0 {
            out.push(Imprint {
                character_id: cid,
                kind: KIND_WITNESS,
                code: "left_a_trace",
                detail: json!({}),
            });
        }
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// 第 4 步：风化（恒定容量，旧烙印褪色而不是被删）
// ═══════════════════════════════════════════════════════════════════════════

/// 烙印容量：超出的部分不删，而是**褪色**（见 [`WeatherStage`]）。
///
/// ⚠️ 12 是拍脑袋的，不是算出来的——它该取多少取决于真实的 token 账单，
/// 而本仓至今一次真实模型调用都没发生过。参数化（§0.2）以便有数据后就能调。
const DEFAULT_IMPRINT_CAPACITY: usize = 12;
const ENV_IMPRINT_CAPACITY: &str = "MUSE_IMPRINT_CAPACITY";

/// 每一档褪色保留多少条。`fresh` 之外的都进阶梯，越旧越模糊、越省位。
const FRESH_RATIO: usize = 2; // 容量的 1/2 保持「具体事实」

fn imprint_capacity() -> usize {
    std::env::var(ENV_IMPRINT_CAPACITY)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_IMPRINT_CAPACITY)
}

/// 褪色档。模拟真实记忆的形状：具体 → 概括 → 只剩滋味 → 沉底。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WeatherStage {
    /// 具体事实：「在燕南篇，他在断桥前退了一步」
    Fresh,
    /// 概括：「他有过一次退让，并为此付出过代价」
    Faded,
    /// 只剩滋味：「他知道退让是什么滋味」
    Distant,
    /// 沉底：不再单独占位（并入聚合倾向）。
    Settled,
}

/// 一条已风化的烙印。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WeatheredImprint {
    pub seq: i64,
    pub kind: String,
    pub code: String,
    pub stage: WeatherStage,
}

/// 按「越旧越模糊」把烙印分档。
///
/// 🔴 **确定性**：只看**条数与顺序**，不看时间。若按时间分档，同一张卡在两次 replay 里
/// 会得到不同的上下文，破坏引擎的确定性契约。
///
/// 🔴 **老卡不会更强，只会更模糊**：容量恒定，新烙印进来时把旧的往后推一档。
/// 一张跑过 20 个世界的卡与一张跑过 3 个世界的卡，**占位一样多**。
///
/// 入参必须按 `seq` **升序**（最旧在前）。
pub(crate) fn weather(imprints: &[(i64, String, String)]) -> Vec<WeatheredImprint> {
    let cap = imprint_capacity();
    let n = imprints.len();
    let fresh_cap = (cap / FRESH_RATIO).max(1);

    imprints
        .iter()
        .enumerate()
        .map(|(i, (seq, kind, code))| {
            // 从最新往回数：最新的 fresh_cap 条是 Fresh，再往前依次 Faded / Distant，
            // 超出容量的一律 Settled。
            let from_newest = n - 1 - i;
            let stage = if from_newest < fresh_cap {
                WeatherStage::Fresh
            } else if from_newest < cap * 3 / 4 {
                WeatherStage::Faded
            } else if from_newest < cap {
                WeatherStage::Distant
            } else {
                WeatherStage::Settled
            };
            WeatheredImprint { seq: *seq, kind: kind.clone(), code: code.clone(), stage }
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// 第 3 步：烙印指纹（进实例种子）
// ═══════════════════════════════════════════════════════════════════════════

/// 一组卡的烙印指纹：**排序后**的 `卡id:类:码` 以 `\n` 连接。
///
/// 🔴 **空集合恒返回空串**，而空串在 `assembly::resolve_instance_seed` 里会走
/// 与接线前**逐字节相同**的路径。这条是黄金世界回归能继续绿的全部依据：
/// 没有烙印的世界（全新库、全新卡）种子一个 bit 都不变。
///
/// 🔴 **排序不可省**：SQL 的返回序、HashMap 的迭代序都不是确定的，
/// 而这个串要进种子——序变了，同一个世界重放两次会抽出不同剧情。
pub(crate) fn imprint_fingerprint(rows: &[(String, String, String)]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut items: Vec<String> =
        rows.iter().map(|(cid, kind, code)| format!("{cid}:{kind}:{code}")).collect();
    items.sort_unstable();
    items.dedup();
    items.join("\n")
}

// ═══════════════════════════════════════════════════════════════════════════
// 第 5 步：措辞（烙印 → 进决策上下文的句子）
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 **这张表是这套系统里唯一有内容风险的地方。**
// 前四步全是确定性数据处理，写歪了顶多是 bug；这张表写歪了会直接把「经历」变成「养成」——
// 只要有一句写成「他因此更擅长临阵决断」，模型就会照着演，而所有既有的红线
// （不进 resources、不进仲裁、恒定容量……）**一条都不会红**。
//
// 于是措辞的铁律只有一条：**陈述发生了什么，不陈述因此获得了什么。**
// 由 `phrases_state_what_happened_not_what_it_grants` 扫描全表守着。
//
// ⚠️ 三档文案共 18 句，是工程写的，没过内容评审。规格 §4.3 已记这条风险
// （「写得不好会让所有卡的上下文读起来像同一个人」）——这批只是让链路跑通的占位，
// 要真上线得有人重写一遍。

/// `code` → 三档褪色措辞：`[具体事实, 概括, 只剩滋味]`。
///
/// 🔴 越往后越模糊、越短——这既是记忆的真实形状，也是**恒定上下文成本**的落点。
/// 🔴 也是「老卡不会更强，只会更模糊」那条平权论证在文案层的兑现。
const PHRASES: &[(&str, [&str; 3])] = &[
    (
        "walked_to_the_end",
        ["他在一个世界里从头走到了尽头。", "他有过一次走到底的经历。", "他知道走到尽头的滋味。"],
    ),
    (
        "left_midway",
        ["他在一个世界走到一半时离开了。", "他有过一次中途抽身的经历。", "他知道半路走开的滋味。"],
    ),
    (
        "witnessed_collapse",
        ["他见过一个世界线整个崩塌。", "他经历过一次崩塌。", "他知道崩塌的滋味。"],
    ),
    (
        "pushed_a_milestone",
        ["他亲手推动过一件足以改写局面的事。", "他推动过一件改写局面的事。", "他知道推动大事的滋味。"],
    ),
    (
        "no_milestone_reached",
        [
            "他曾走到一件足以改写局面的事跟前，最终没有做成。",
            "他有过一次差一步没走到。",
            "他知道差一步的滋味。",
        ],
    ),
    (
        "left_a_trace",
        ["他在一个世界的公共记载里留下过痕迹。", "他在某处留下过痕迹。", "他知道被记住的滋味。"],
    ),
];

/// 已风化的烙印 → 喂给引擎的句子表（**新的在前**，与上下文里那句「越靠后的越久远」一致）。
///
/// **纯函数**：同一批烙印恒得同一份句子（含顺序）。这不是洁癖——它进模型 prompt，
/// 而 prompt 变了同一个世界重放两次就不是同一件事了。
///
/// 未登记的 `code` **跳过**（fail-closed）：新加一类烙印时必须显式补措辞，
/// 忘了补的后果是「这条不出现」，而不是「输出一句 `walked_to_the_end`」这种鬼东西。
///
/// `Settled`（沉底）不单独占位，聚合成一句带条数的底色——这正是风化机制存在的理由。
pub(crate) fn phrase_imprints(weathered: &[WeatheredImprint]) -> Vec<String> {
    let mut out = Vec::new();
    let mut settled = 0usize;
    for w in weathered.iter().rev() {
        let tier = match w.stage {
            WeatherStage::Fresh => 0,
            WeatherStage::Faded => 1,
            WeatherStage::Distant => 2,
            WeatherStage::Settled => {
                settled += 1;
                continue;
            }
        };
        if let Some((_, p)) = PHRASES.iter().find(|(code, _)| *code == w.code) {
            out.push(p[tier].to_string());
        }
    }
    if settled > 0 {
        out.push(format!("此外还有 {settled} 段更早的经历，已经说不清楚了，只剩下一点底色。"));
    }
    out
}

/// 读一批卡的烙印并措辞好，供 runtime 注入 `RoundInput.worldline_imprints`。
///
/// 🔴 **每张卡只得到自己那一条**（返回值按卡 id 分格），信息隔离铁律在数据层就成立，
/// 不指望引擎那头去切。
///
/// 无烙印的卡**不进表**（而不是进一个空 Vec）：引擎那边 `get(cid)` 落空 → 追加函数
/// 原样返回 → 上下文与接线前逐字节一致。
pub(crate) async fn worldline_pasts(
    db: &sqlx::AnyPool,
    character_ids: &[String],
) -> Result<std::collections::BTreeMap<String, Vec<String>>, ApiError> {
    let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for cid in character_ids {
        // 🔴 `ORDER BY seq ASC` 不可省：`weather` 要求最旧在前，序错了褪色档就全反了。
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT seq, kind, code FROM character_imprints WHERE character_id = $1 ORDER BY seq ASC",
        )
        .bind(cid)
        .fetch_all(db)
        .await?;
        if rows.is_empty() {
            continue;
        }
        let lines = phrase_imprints(&weather(&rows));
        if !lines.is_empty() {
            out.insert(cid.clone(), lines);
        }
    }
    Ok(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// 气运与机缘的**点数与档位**（2026-07-29 第二版：从哈希改成可解释的量化值）
// ═══════════════════════════════════════════════════════════════════════════
//
// 第一版把整份烙印指纹哈希一下取高低位——确定性有了，但它有三个毛病，
// 而产品要「量化显示，好知道哪张卡带来什么」的时候，三个全都挡在路上：
//
// 1. **不可解释**：换一条烙印，两个数会**跳**（哈希无序）。玩家看不出「我做了什么让它变的」。
// 2. **不单调**：跑更多世界不一定让数变大，也可能变小——「很难增加」这句话根本无从谈起。
// 3. **不可分解**：它是整个房间的一个哈希，没法说「这张卡贡献了多少」，也就没法比卡。
//
// 第二版：**按烙印类目计点 → 几何阶梯换档 → 封顶**。三条毛病一次解决，
// 且确定性反而更强（纯整数运算，不经浮点、不经哈希）。
//
// ══════════════════════════════════════════════════════════════════════════
// 🔴 「量化」和「更强」之间那条线画在哪
// ══════════════════════════════════════════════════════════════════════════
// 产品要的是「知道哪张角色卡更好」。**在这套系统里没有「更好」这个量**，
// 有的是「这张卡会把世界变成什么样」：
//
// | 数 | 高了会怎样 | 是优势吗 |
// |---|---|---|
// | **机缘** | 主线间隙里的事**更密** | ❌ 抽的是同一个池，好事和麻烦等比例变多 |
// | **气运** | 那些事**更两极** | ❌ 可能捡到不该捡的，也可能撞上不该撞的 |
//
// 于是「哪张卡更好」的诚实答案是：**没有更好的卡，只有会把世界变成不同样子的卡**。
// 一张机缘满档、气运零档的卡带来「热闹但平稳」的世界；反过来带来「安静但极端」的世界。
// 玩家真正需要看的就是这个，而它恰好不违宪。
//
// 另外三条锁（与第一版相同，一条不减）：
// - 世界级、全员共享 ⇒ 不存在「我气运高我拿得多」；
// - 产出封顶与稀有预算一个字不动（`RARE_TIER` / `RARE_BUDGET` / 星级封顶都在气运**之后**）；
// - 档位**有顶**（[`SWING_MAX_LEVEL`]）⇒ 老卡最终都撞到同一个顶，不会无限拉开。
//   没有顶的刻度迟早会被当成战力表——这条与 [`LifeStage`] 的封顶是同一个理由。

/// 一张卡在两个方向上的累计点数。两个方向都不是「好」的方向。
///
/// 也用作道具授予点数的载体（见 [`SwingGrants`]）——授予的就是同一种点数，
/// 走同一道阶梯、撞同一个顶。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Swing {
    /// 气运点：世界内容的**幅度**（离常态多远，两个方向都算）。
    pub fortune: i64,
    /// 机缘点：世界内容的**密度**（多久来一件事）。
    pub opportunity: i64,
}

/// 🔮 **预留接口，本轮恒空**：道具直接给某张卡加点。
///
/// 产品说「后续可能会出直接加数值的道具，那个接口只需要预留即可」。预留的形态是这个
/// 「卡 id → 授予点数」的表，而**生产路径唯一的构造方式是 [`no_swing_grants`]**
/// （红线 `no_production_path_grants_swing_points_yet` 锁着）。
///
/// ══════════════════════════════════════════════════════════════════════════
/// 🔴 将来真接线时，这两条必须先答，缺一不可
/// ══════════════════════════════════════════════════════════════════════════
/// 1. **道具能加速到顶，不能突破顶。** 授予的点数与经历得来的点数走**同一道阶梯**，
///    因此 [`SWING_MAX_LEVEL`] 对它一样有效（红线 `granted_points_cannot_break_the_ceiling`）。
///    这一条把「花钱买上限」这个方向从设计层堵死——买到的只是**更快到达**，
///    而到达之后与一个跑了 80 局的人**完全一样**。
/// 2. **它买到的是「世界变得不一样」，不是「我更容易赢」。** 气运机缘是世界级、全员共享的，
///    买家掏钱改变的是整个房间的内容形态，对同房所有人一视同仁；产出封顶一个字不动。
///    若将来有人想把它做成「只对我生效」，那就是另一套东西，且当场违宪。
pub(crate) type SwingGrants = std::collections::BTreeMap<String, Swing>;

/// 生产路径唯一的授予来源：**没有授予**。
pub(crate) fn no_swing_grants() -> SwingGrants {
    SwingGrants::new()
}

/// 档位封顶。**这个顶不是偷懒，是红线**——见本节开头第三条锁。
pub(crate) const SWING_MAX_LEVEL: i64 = 5;

/// 第 1 档需要的点数；之后每一档的**增量翻倍**。
///
/// ⚠️ 4 是拍脑袋的，和 [`DEFAULT_LIFE_MARKED_AT`] 一样——它该取多少取决于
/// 「一局世界平均给一张卡留下多少条烙印」，而那个数要等真实模型跑过才有。
const SWING_STEP: i64 = 4;

/// 到达第 `level` 档需要的**累计**点数：`STEP × (2^level − 1)` ⇒ 4 / 12 / 28 / 60 / 124。
///
/// 产品要求「这两个数值必须设置的很难增加」，这道几何阶梯就是那个要求的落点：
/// [`derive_imprints`] 一局给一张卡 1-3 条烙印，分摊到两个方向后每局每向 1-2 点
/// ⇒ 满档要跑**六十局以上**，而且越往后越慢（每档的门槛是上一档增量的两倍）。
///
/// 🔵 「六十局」不是拍的，是 `maxing_out_must_take_dozens_of_worlds` 按派生器的真实规则
/// 算出来的——而且那条用例会在**有人给 `derive_imprints` 加一类烙印**时变红，
/// 那正是这个数最可能被静默改掉的方式（加烙印看起来与气运机缘毫无关系）。
pub(crate) fn swing_threshold(level: i64) -> i64 {
    if level <= 0 {
        return 0;
    }
    SWING_STEP * ((1i64 << level.min(SWING_MAX_LEVEL)) - 1)
}

/// 累计点数 → 档位（0..=[`SWING_MAX_LEVEL`]）。**纯整数，无浮点、无哈希。**
pub(crate) fn swing_level(points: i64) -> i64 {
    let mut level = 0;
    while level < SWING_MAX_LEVEL && points >= swing_threshold(level + 1) {
        level += 1;
    }
    level
}

/// 一条烙印落在哪个方向上。
///
/// 🔴 这个映射是**语义的，不是数值的**：它说的是「这段经历让世界变密还是变极端」，
/// 不说「因此这张卡更擅长什么」。同一条烙印不会让任何人更强。
///
/// 未知类目**不计点**（fail-closed）：新加一类烙印时必须显式决定它落在哪一边，
/// 忘了决定的后果是「不生效」而不是「悄悄改变所有老卡的档位」。
fn axis_of(kind: &str) -> Option<bool /* true = 气运 */> {
    match kind {
        // 做过事、见过世面、有过牵扯 → 事更容易找上门（密度）。
        KIND_CHOICE | KIND_WITNESS | KIND_BOND => Some(false),
        // 经历过的处境、差一步没走到的事 → 世界更两极（幅度）。
        KIND_CIRCUMSTANCE | KIND_UNFINISHED => Some(true),
        _ => None,
    }
}

/// 按卡汇总点数。`rows` 为 `(卡id, kind, code)`，**顺序无关**（计数可交换）。
///
/// 🔵 与 [`imprint_fingerprint`] 的一个关键差别：那边 `dedup` 去重（同一条烙印进种子只算一次），
/// 这边**不去重**——一张卡在三个世界里都「走到了终局」是三段经历，不是一段。
pub(crate) fn swing_points_by_card(
    rows: &[(String, String, String)],
    grants: &SwingGrants,
) -> std::collections::BTreeMap<String, Swing> {
    let mut out: std::collections::BTreeMap<String, Swing> = Default::default();
    for (cid, kind, _code) in rows {
        let e = out.entry(cid.clone()).or_default();
        match axis_of(kind) {
            Some(true) => e.fortune += 1,
            Some(false) => e.opportunity += 1,
            None => {}
        }
    }
    for (cid, g) in grants {
        let e = out.entry(cid.clone()).or_default();
        e.fortune += g.fortune;
        e.opportunity += g.opportunity;
    }
    out
}

/// **每张卡自己的**气运档与机缘档（`0..=SWING_MAX_LEVEL`），按卡 id 分格。
///
/// ══════════════════════════════════════════════════════════════════════════
/// 🔴 产品口径（2026-07-29 二次澄清）：**这两个数对自己有影响，对世界没有影响。**
/// ══════════════════════════════════════════════════════════════════════════
/// 上一版这里是 `world_swing_levels`——把各卡档位**平均**成一个世界级的数。
/// 那是理解错了需求：产品说的一直是「这张卡的气运/机缘」，
/// 「气运高的可以降低遇到 boss 或获取宝物的难度」这句话里的主语从头到尾都是角色。
/// 我当时把它改成世界级，理由是「作用于世界就不构成个体优势，红线更好过」——
/// 🔵 **那是把红线的成本转嫁给了产品**。该做的是照需求做、把风险说清楚。
///
/// 🔴 **只返回名册里的卡**。`grants` 是外部给的表（道具接口，本轮恒空），
/// 若它带进一张不在这个房间里的卡，那张卡不得凭空出现在结果里——
/// 上一版做世界平均时，外来卡会被算进分子；现在结构上进不来。
///
/// 无烙印的卡返回 `(0, 0)`（而不是不出现）：调用方拿到零档 = 两处恒等，
/// 与本层落地前逐字节相同。**给全** 比 **给一半** 好——调用方不必区分「没有这张卡」
/// 和「这张卡是零档」，那正是最容易写出分支不一致的地方。
pub(crate) fn card_swing_levels(
    rows: &[(String, String, String)],
    roster: &[String],
    grants: &SwingGrants,
) -> std::collections::BTreeMap<String, (i64, i64)> {
    let by_card = swing_points_by_card(rows, grants);
    roster
        .iter()
        .map(|cid| {
            let s = by_card.get(cid).copied().unwrap_or_default();
            (cid.clone(), (swing_level(s.fortune), swing_level(s.opportunity)))
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// 落库（事务内，append-only，幂等靠唯一索引）
// ═══════════════════════════════════════════════════════════════════════════

/// 收集一个世界的确定性事实。**在结算事务内调用**（与结算同成同败）。
pub(crate) async fn collect_world_facts_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    collapsed: bool,
) -> Result<WorldFacts, ApiError> {
    let total_ticks: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM world_ticks WHERE world_id = $1 AND status = 'done'")
            .bind(world_id)
            .fetch_one(&mut **tx)
            .await
            .unwrap_or(0);

    // 🔴 `ORDER BY cloud_character_id` 不可省：这一行的顺序最终会进实例种子。
    let rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, wm.status AS st, \
                COALESCE(wc.milestone_score_milli, 0) AS ms \
         FROM world_members wm \
         LEFT JOIN world_contributions wc \
           ON wc.world_id = wm.world_id AND wc.character_id = wm.cloud_character_id \
         WHERE wm.world_id = $1 \
         ORDER BY wm.cloud_character_id ASC",
    )
    .bind(world_id)
    .fetch_all(&mut **tx)
    .await?;

    let mut characters = Vec::with_capacity(rows.len());
    for r in &rows {
        let cid: String = r.try_get("cid")?;
        let status: String = r.try_get("st")?;
        let ms: i64 = r.try_get("ms").unwrap_or(0);
        let event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE world_id = $1 AND actors_json LIKE $2",
        )
        .bind(world_id)
        .bind(format!("%{cid}%"))
        .fetch_one(&mut **tx)
        .await
        .unwrap_or(0);
        characters.push(CharacterFacts {
            character_id: cid,
            stayed_to_end: status == "active",
            // ⚠️ `left_at` 是毫秒时刻不是拍号；本轮不换算（换算要按 tick 的时间轴反查，
            // 而那条时间轴在 `world_ticks` 里可能有空洞）。先记 None，细节里只出总拍数。
            left_at_tick: None,
            milestone_score_milli: ms,
            event_count,
        });
    }

    Ok(WorldFacts { world_id: world_id.to_string(), collapsed, total_ticks, characters })
}

/// 把派生出的烙印落库。**append-only + 幂等**（唯一索引裁决重复）。
///
/// 返回真正新写入的条数（重入时为 0）。
pub(crate) async fn record_imprints_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    imprints: &[Imprint],
) -> Result<u64, ApiError> {
    let now = now_ms();
    let mut written = 0u64;
    for im in imprints {
        // 「这张卡的第几条」——在同一事务内取 MAX+1。
        // 🔴 这里不需要发号器表（对照 `world_events.sequence` 那次）：并发面完全不同——
        // 那是全世界共写一条序列，这里是「一张卡在一次结算里」，而**一张卡同一时刻
        // 只能在一个世界**（`worlds::join_world` 的闸）已经把并发写入收敛掉了。
        let next_seq: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM character_imprints WHERE character_id = $1",
        )
        .bind(&im.character_id)
        .fetch_one(&mut **tx)
        .await
        .unwrap_or(1);

        let res = sqlx::query(
            "INSERT INTO character_imprints \
             (id, character_id, world_id, seq, kind, code, detail_json, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(new_id("imp"))
        .bind(&im.character_id)
        .bind(world_id)
        .bind(next_seq)
        .bind(im.kind)
        .bind(im.code)
        .bind(im.detail.to_string())
        .bind(now)
        .execute(&mut **tx)
        .await;

        match res {
            Ok(_) => written += 1,
            // 唯一索引冲突 = 这条烙印已经存在（结算重入）。**这是正常路径，不是错误。**
            Err(e) if is_unique_violation(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(written)
}

/// 唯一约束冲突的双库判定（SQLite `UNIQUE constraint failed` / Postgres `23505`）。
fn is_unique_violation(e: &sqlx::Error) -> bool {
    let msg = e.to_string().to_ascii_lowercase();
    msg.contains("unique") || msg.contains("23505") || msg.contains("duplicate")
}

/// 读一组卡的全部烙印（供指纹与风化用）。返回 `(character_id, kind, code)`，按确定序。
pub(crate) async fn load_imprints_for_cards(
    db: &sqlx::AnyPool,
    character_ids: &[String],
) -> Result<Vec<(String, String, String)>, ApiError> {
    if character_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for cid in character_ids {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT kind, code FROM character_imprints WHERE character_id = $1 ORDER BY seq ASC",
        )
        .bind(cid)
        .fetch_all(db)
        .await?;
        for (kind, code) in rows {
            out.push((cid.clone(), kind, code));
        }
    }
    Ok(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// 世界记忆层（迁移 0055）：一张卡在**某一个世界**里的经历，世界结束时定格
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 与烙印的分工（产品 2026-07-29 拍板）：
// - **单个世界的经历不改变角色卡的内核**，只是多加一层关于那个世界的记忆（本段）；
// - 记忆累积够多，才影响「用户不能编辑的部分」（生命层，见 `life_stage`）。
//
// 数据源是引擎的 `narrative.pacingNotes`——它每拍记录每一条仲裁结果
// （格式 `{角色id}｜{结果}｜{后果}`），而在 2026-07-29 之前**全仓没有任何生产代码读它**。
// 这一段与引擎侧 `assemble_visible_context` 的 `yourMemory` 是同一份数据的两个出口：
// 一个在世界**运行期间**喂给角色（让它记得），一个在世界**结束时**定格（让它带走）。

/// 从引擎状态里切出某张卡的记忆条目。
///
/// 🔴 与引擎 `decide.rs` 的 `yourMemory` **同一条过滤口径**（`{cid}｜` 前缀）：
/// 分隔符必须进前缀，否则 id `A` 会把 `AB` 的条目也算走。
/// 两处口径必须一致——不一致时会出现「角色运行期间记得的事，结算后没留下」这种最难查的偏差。
pub(crate) fn memories_of(pacing_notes: &[String], character_id: &str) -> Vec<String> {
    let prefix = format!("{character_id}｜");
    pacing_notes.iter().filter(|n| n.starts_with(&prefix)).cloned().collect()
}

/// 定格这个世界里各张卡的记忆。**append-only + 幂等**（唯一索引裁决重入）。
///
/// ⚠️ `seq` 按**数组下标**给：记忆天然按发生先后排列，不需要发号器，也不该重排。
pub(crate) async fn record_world_memories_tx(
    tx: &mut Transaction<'_, Any>,
    world_id: &str,
    pacing_notes: &[String],
    character_ids: &[String],
) -> Result<u64, ApiError> {
    let now = now_ms();
    let mut written = 0u64;
    for cid in character_ids {
        for (i, note) in memories_of(pacing_notes, cid).into_iter().enumerate() {
            let res = sqlx::query(
                "INSERT INTO character_world_memories \
                 (id, character_id, world_id, seq, note, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(new_id("mem"))
            .bind(cid)
            .bind(world_id)
            .bind(i as i64 + 1)
            .bind(&note)
            .bind(now)
            .execute(&mut **tx)
            .await;
            match res {
                Ok(_) => written += 1,
                // 唯一冲突 = 这条记忆已经定格过（结算重入）。正常路径。
                Err(e) if is_unique_violation(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(written)
}

/// 这张卡一共积累了多少条世界记忆（生命层的阈值判据，见 `life_stage`）。
pub(crate) async fn memory_count(db: &sqlx::AnyPool, character_id: &str) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM character_world_memories WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_one(db)
    .await
    .unwrap_or(0))
}

// ═══════════════════════════════════════════════════════════════════════════
// 生命层：记忆累积到一定程度之后，这张卡身上开始有一些**用户改不了**的东西
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 产品定性（2026-07-29 拍板）：
//   「单个世界的经历不会影响角色卡的内核，只是给角色卡多加了一层关于这个世界的记忆而已。
//     当积累够多后，才会影响用户不能编辑的部分内容，这部分内容需要独立于内核可编辑的内容，
//     而且是一张角色卡真正有生命的地方。」
//
// 于是一张卡分三层，各有各的写入方与可见性：
//
// | 层 | 谁写 | 用户可编辑 | 随卡导出 | 能被复刻吗 |
// |---|---|---|---|---|
// | 内核（`card_json`） | 用户 | ✅ | ✅ | ✅ **可以** |
// | 世界记忆（0055） | 系统（该世界确定性事实） | ❌ | ❌ | ❌ |
// | **生命层**（本段） | 系统（记忆累积派生） | ❌ | ❌ | ❌ |
//
// 🔵 「用户不能编辑」不只是防作弊：它让生命层成为**唯一无法被复刻的东西**。
// 别人可以抄走你的内核，抄不走你的卡活过什么——这就是那句「真正有生命的地方」的工程含义。
//
// ═══════════════════════════════════════════════════════════════════════════
// 🔴 它凭什么不违平权红线
// ═══════════════════════════════════════════════════════════════════════════
// 与烙印同一条论证，且更强，因为生命层**连上下文都不进**（本轮）：
// 1. 它是**派生只读量**，不写回卡、不进 `RoundInput`、不进任何仲裁判据；
// 2. 阶位只表示「这张卡活过多久」，**不表示它更强**——高阶位不带任何优待；
// 3. 阶位有**封顶**（`LifeStage::Storied` 之上没有了），老卡不会无限拉开。
//
// 🔵 检验：**两张卡的生命阶位互换，谁会变强？** 答案必须是「谁都不会」。
// 本轮它唯一的出口是玩家自己的展示面（「这张卡活过 N 个世界」），
// 任何「据阶位改判定 / 改产出 / 开权限 / 调难度」的下游都是红线违规。

/// 生命阶位。**只表示活过多久，不表示更强。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LifeStage {
    /// 崭新：还没有足够的经历。
    Blank,
    /// 有痕：开始有说得出来的经历。
    Marked,
    /// 有史：经历成型，这张卡有自己的过去了。
    Storied,
}

/// 进入「有痕」需要的记忆条数（§0.2 参数化）。
const DEFAULT_LIFE_MARKED_AT: i64 = 30;
/// 进入「有史」需要的记忆条数。
const DEFAULT_LIFE_STORIED_AT: i64 = 120;
const ENV_LIFE_MARKED_AT: &str = "MUSE_LIFE_MARKED_AT";
const ENV_LIFE_STORIED_AT: &str = "MUSE_LIFE_STORIED_AT";

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// 由记忆条数派生生命阶位。**纯函数**。
///
/// ⚠️ 两个阈值都是拍脑袋的（30 / 120），不是算出来的——它们该取多少取决于
/// 「一局世界平均留下多少条记忆」，而那个数要等真实模型跑过才有。
/// 参数化（§0.2）以便有数据后就能调，且**调它不改变任何判定**，只改变展示。
///
/// 🔴 阶位**封顶**：`Storied` 之上没有了。这不是偷懒——没有封顶，
/// 生命层就会变成一条无限增长的刻度，而任何无限刻度迟早会被人当成战力表。
pub(crate) fn life_stage(memory_count: i64) -> LifeStage {
    let marked = env_i64(ENV_LIFE_MARKED_AT, DEFAULT_LIFE_MARKED_AT);
    let storied = env_i64(ENV_LIFE_STORIED_AT, DEFAULT_LIFE_STORIED_AT).max(marked + 1);
    if memory_count >= storied {
        LifeStage::Storied
    } else if memory_count >= marked {
        LifeStage::Marked
    } else {
        LifeStage::Blank
    }
}

/// 一张卡的生命层快照（只读派生，无存储）。
///
/// 🔴 **没有存储**是刻意的：生命层是记忆的**函数**，不是另一份状态。
/// 存下来就会出现「记忆和阶位对不上」的第三种事实，而那种不一致没有任何办法自愈。
pub(crate) async fn life_snapshot(
    db: &sqlx::AnyPool,
    character_id: &str,
) -> Result<serde_json::Value, ApiError> {
    let memories = memory_count(db, character_id).await?;
    let worlds: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT world_id) FROM character_world_memories WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_one(db)
    .await
    .unwrap_or(0);
    let imprints: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM character_imprints WHERE character_id = $1")
            .bind(character_id)
            .fetch_one(db)
            .await
            .unwrap_or(0);
    Ok(json!({
        "stage": life_stage(memories),
        "memories": memories,
        "worlds": worlds,
        "imprints": imprints,
        // 🔴 这句话是给下游看的，不是给玩家看的：它是这一层的红线，写在数据里比写在文档里更难被绕过。
        "note": "生命层只表示这张卡活过多久，不表示它更强。它由经历派生、用户不可编辑、不随卡导出，\
也不进入任何引擎判定——据它改判定、改产出、开权限或调难度都是平权红线违规。",
    }))
}

/// 一个方向的展示形态：**当前在哪一档、离下一档还差多少**。
///
/// 🔴 「还差多少」是这次量化显示的重点，不是「有多少点」。产品要的是玩家能看出
/// 「这两个数很难增加」——只给一个点数看不出来，给出「下一档在 60 点、你在 31 点」才看得出来。
///
/// 顶档时 `nextAt` / `toNext` 都是 `null` 而不是某个够不着的大数：
/// 展示层拿到大数会算出「还差一万点」，那是在暗示这条刻度还能往上，而它不能。
fn axis_view(points: i64) -> serde_json::Value {
    let level = swing_level(points);
    if level >= SWING_MAX_LEVEL {
        return json!({ "level": level, "max": SWING_MAX_LEVEL, "points": points,
                       "nextAt": serde_json::Value::Null, "toNext": serde_json::Value::Null });
    }
    let next = swing_threshold(level + 1);
    // `levelAt` 是**当前这一档的**门槛：展示层要画「这一档走了多少」就必须有它，
    // 否则只能拿总点数除以下一档门槛，画出来的进度条在每次升档时会往回跳。
    json!({ "level": level, "max": SWING_MAX_LEVEL, "points": points,
            "levelAt": swing_threshold(level), "nextAt": next, "toNext": (next - points).max(0) })
}

/// 一张卡的气运与机缘快照（只读派生，无存储——同 [`life_snapshot`] 的理由）。
///
/// 🔴 授予点数恒空（[`no_swing_grants`]）：道具加值是预留接口，本轮不接线。
pub(crate) async fn swing_snapshot(
    db: &sqlx::AnyPool,
    character_id: &str,
) -> Result<serde_json::Value, ApiError> {
    let kinds: Vec<(String,)> =
        sqlx::query_as("SELECT kind FROM character_imprints WHERE character_id = $1")
            .bind(character_id)
            .fetch_all(db)
            .await?;
    let rows: Vec<(String, String, String)> = kinds
        .into_iter()
        .map(|(kind,)| (character_id.to_string(), kind, String::new()))
        .collect();
    let s = swing_points_by_card(&rows, &no_swing_grants()).get(character_id).copied().unwrap_or_default();
    Ok(json!({
        "fortune": axis_view(s.fortune),
        "opportunity": axis_view(s.opportunity),
        // 🔴 这段话是产品定义的一部分，不是 UI 文案的装饰。产品原话要「知道哪张角色卡更好」，
        // 而这套系统里**没有「更好」这个量**——写在数据里比写在文档里更难被下游误用。
        "note": "气运与机缘不是战力，是「你这张卡会遇到什么样的事」：\
机缘高，主线间隙里的事更常找上你；气运高，那些事更两极——可能捡到不该捡的，也可能撞上不该撞的。\
它们不改变任何产出上限（这一局最多有几件好东西，在你入场之前就定死了），也不让你更容易赢。\
比较两张卡时看的是「它们会遇到什么样的事」，不是「谁更强」。",
        "howItGrows": "只能靠经历长，且越往后越慢（每一档的门槛是上一档增量的两倍），并且有封顶。",
    }))
}

// ═══════════════════════════════════════════════════════════════════════════
// 玩家读取面：我的这张卡活过什么
// ═══════════════════════════════════════════════════════════════════════════

/// GET /me/characters/{id}/life：这张卡的生命层 + 世界记忆。
///
/// 🔴 **只读，无任何写入端点**——生命层与记忆都由系统按确定性事实派生，
/// 客户端一个字节都写不进来（同「资产单一写入路径」红线的取向）。
/// 这也是它不可伪造、因而不可复刻的全部依据。
///
/// 归属校验：只能看自己的卡。**404 而不是 403**——不向外泄露「这张卡存在但不是你的」
/// （口径同 `subplot::load_container_cards` 与 `assets` 的既有做法）。
async fn my_character_life(
    State(state): State<AppState>,
    user: crate::auth::AuthUser,
    Path(character_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_id FROM cloud_characters WHERE id = $1")
            .bind(&character_id)
            .fetch_optional(&state.db)
            .await?;
    match owner {
        Some(o) if o == user.user_id => {}
        _ => return Err(ApiError::NotFound),
    }

    let life = life_snapshot(&state.db, &character_id).await?;
    // 世界记忆按 (world, seq) 定序取回；分世界成组——一段经历属于一个世界，
    // 摊平成一条流水会让「它在哪个世界里做的这件事」丢掉。
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT world_id, note FROM character_world_memories \
         WHERE character_id = $1 ORDER BY world_id ASC, seq ASC",
    )
    .bind(&character_id)
    .fetch_all(&state.db)
    .await?;
    let mut by_world: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (w, n) in rows {
        by_world.entry(w).or_default().push(n);
    }
    let worlds: Vec<serde_json::Value> = by_world
        .into_iter()
        .map(|(w, notes)| json!({ "worldId": w, "notes": notes }))
        .collect();

    let swing = swing_snapshot(&state.db, &character_id).await?;
    Ok(Json(json!({ "characterId": character_id, "life": life, "swing": swing, "memories": worlds })))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/me/characters/{id}/life", get(my_character_life))
}
