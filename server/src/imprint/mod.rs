//! 世界线烙印（Worldline Imprint）—— 让「复刻内核 ≠ 复刻这张卡」。
//!
//! 提案与完整设计：`docs/build/spec-worldline-imprint.md`；存储见迁移 0054。
//! 本模块实现该提案的**第 1-4 步**：表 · 确定性派生器 · 进实例种子 · 风化。
//! 第 5 步（烙印进决策上下文）**不在本模块**——它的效果只能靠真实模型验证，
//! 而本仓至今一次真实模型调用都没发生过，写了也只能被替身跑。
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
use sqlx::{Any, Row, Transaction};

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
