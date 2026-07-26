//! 世界生命周期（S2）：大厅列表 / 详情 / 投放(join) / 离场(leave) + 内部 create_world。
//!
//! 端点：
//! GET  /worlds?type=idle|chapter|arena&q=…&sort=new|hot
//!   大厅列表（只出 open/running 且 official/public）：
//!   - q：标题大小写不敏感包含搜索（% _ \ 转义；空串视为无搜索）
//!   - sort=new（默认）：created_at DESC + cursor 分页（现行为）
//!   - sort=hot：热度榜快照（近 48h 事件×1 + 近 7 天打赏×5 + active 成员×2），
//!     每项附 hotScore，LIMIT ≤50，不支持 cursor（nextCursor 恒为 null）
//! GET  /worlds/{id}                      详情（世界书简介/规则/公开阵容/AI 标识展示；含模板 starRating）
//! POST /worlds/{id}/join                 投放角色：AuthUser + Idempotency-Key + cloudCharacterId + boundary
//!   服务端权威校验（§9.6）：角色 approved 且未 withdrawn 且属于本人；人数上限；写 world_members；
//!   防自刷：同一世界每位用户仅可投放一张 active 角色卡（退出后可换卡再进）；
//!   同源唯一（R1）：同一提取源的未编辑原味卡同世界仅可在场一张（编辑过的卡/无指纹卡放行）；
//!   历练准入（波次 3）：模板 star≥3 时投放卡 mileage 须达门槛（3★=300/4★=1000/5★=3000），1-2★ 免检
//! POST /worlds/{id}/leave                离场：置成员 left + 记 left_at 退出时刻（离场事件交由下个 tick 叙事化）
//! POST /worlds/{id}/cover                上传世界封面位图（官方房运营 / 创作者房房主）：
//!   base64 JSON → MIME 白名单 → 1MB 上限 → 对象存储 → 图审 → worlds.cover_* 三列；
//!   🔴 仅机审 approved 才在任何读取面下发 coverUrl（未过审绝不外泄，口径同角色立绘）
//!
//! 官方建房走 admin(S6)，此处提供内部 create_world 供其调用；创建时钉住
//! engine_version/prompt_set_version/model_route_version/template_version（§9.2 版本钉住）。

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Any, AnyPool, Row, Transaction};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;
use crate::providers::ModerationVerdict;

use muse_engine::narrative::types::Lethality;

#[cfg(test)]
mod tests;

// ---------- 生死契约三档（总规格 §11【拍板 24】）：枚举归一 + 运营开关 ----------

/// 落库枚举值：庇护世界（死亡不可能，引擎写作前降级致死行动）。
pub const LETHALITY_SANCTUARY: &str = "sanctuary";
/// 落库枚举值：同意制世界（**默认**，现行机制——不可逆事件当事人临场同意，超时保守）。
pub const LETHALITY_CONSENT: &str = "consent";
/// 落库枚举值：生死状世界（入场即签，事后不再临场征询；仲裁硬约束不变）。
pub const LETHALITY_DEATHMATCH: &str = "deathmatch";

/// 生死状档运营开关环境变量。
const ENV_DEATHMATCH_ENABLED: &str = "MUSE_LETHALITY_DEATHMATCH";
/// 生死状档默认值 = **关闭**。
///
/// 🔴 VALIDATION.md §0.1「未验证功能默认关闭」：永久死亡是待验证的默认策略（不是红线），
/// 且 §2 的阶段计划把生死状明确排在 T2「暂不验证」之后。因此代码合并不等于对用户开放——
/// 必须运营显式打开本开关，生死状档才可能生效。
const DEFAULT_DEATHMATCH_ENABLED: bool = false;

/// 生死状档是否已由运营开启（env 覆盖 + 默认常量，范式同 `runtime::token_cny_cents_per_1k`
/// 与 `interventions::dream_quota_per_stage`——本仓库尚无配置表，env 是当前唯一的运营开关形态；
/// 将来配置表落地后只改本函数内部，调用点与降级语义不变）。
///
/// 开关的定位是**全局急停阀**，不是建房时的一次性校验：它在**读取侧**生效
/// （`effective_lethality`），故关掉之后所有已建的生死状世界**立即**降级为同意制
/// （join 不再要求签署、引擎收到 Consent），再打开则原样恢复。
pub fn deathmatch_enabled() -> bool {
    match std::env::var(ENV_DEATHMATCH_ENABLED) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            // 配错不静默开启高危档：回落默认（关闭）。
            _ => DEFAULT_DEATHMATCH_ENABLED,
        },
        Err(_) => DEFAULT_DEATHMATCH_ENABLED,
    }
}

/// 测试专用：生死状运营开关的 RAII 夹具。
///
/// `MUSE_LETHALITY_DEATHMATCH` 是**进程级** env，而 worlds 与 admin_api 的用例同属一个测试二进制、
/// 默认并发跑，故所有对开关敏感的用例共用**同一把锁**串行化（跨模块可见，故定义在这里而非某个
/// tests 子模块），并在 Drop 时把 env 恢复原状——一个用例绝不把开关状态留给下一个用例。
/// （锁中毒也照常取用，不阻塞后续用例；范式同 `interventions` 的 QUOTA_ENV_LOCK。）
#[cfg(test)]
pub(crate) struct DeathmatchSwitch {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Option<String>,
}

#[cfg(test)]
impl DeathmatchSwitch {
    /// 取锁并把开关置为 on/off；返回值存活期间开关状态稳定。
    pub(crate) fn set(on: bool) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(ENV_DEATHMATCH_ENABLED).ok();
        std::env::set_var(ENV_DEATHMATCH_ENABLED, if on { "1" } else { "0" });
        Self { _guard: guard, prev }
    }
}

#[cfg(test)]
impl Drop for DeathmatchSwitch {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(ENV_DEATHMATCH_ENABLED, v),
            None => std::env::remove_var(ENV_DEATHMATCH_ENABLED),
        }
    }
}

/// 契约档字符串是否为合法枚举值（建房入参校验用）。
pub fn is_valid_lethality(raw: &str) -> bool {
    matches!(raw, LETHALITY_SANCTUARY | LETHALITY_CONSENT | LETHALITY_DEATHMATCH)
}

/// 落库字符串 → **生效**契约档（引擎入参）。join 契约门与 runtime 回灌共用这一个函数，
/// 保证"玩家签的那一档"与"引擎跑的那一档"永远同源，不会一边签生死状一边跑同意制。
///
/// 两处降级（方向恒为更保守，绝不反向）：
/// - 非法/未知值（脏数据、未来枚举回滚）→ `Consent`：默认档即现行机制，不放大死亡权限。
/// - `deathmatch` 但运营开关未开 → `Consent`：未验证功能默认关闭（VALIDATION.md §0.1）。
pub fn effective_lethality(stored: &str) -> Lethality {
    match stored {
        LETHALITY_SANCTUARY => Lethality::Sanctuary,
        LETHALITY_DEATHMATCH if deathmatch_enabled() => Lethality::Deathmatch,
        // deathmatch 但开关未开 → 降级；其余（consent / 非法值）→ 默认档。
        _ => Lethality::Consent,
    }
}

/// 生效契约档的 JSON 投影值（字面量与落库枚举一致）。大厅/详情把它明示给玩家——
/// 「join 前明示」（规格 §11）的前提是**列表页与详情页看得见档位**，冷静提示由前端据此渲染。
/// 投影的是**生效档**而非落库原值：开关未开时玩家看到的就是同意制，所见即所签。
pub fn lethality_label(stored: &str) -> &'static str {
    match effective_lethality(stored) {
        Lethality::Sanctuary => LETHALITY_SANCTUARY,
        Lethality::Consent => LETHALITY_CONSENT,
        Lethality::Deathmatch => LETHALITY_DEATHMATCH,
    }
}

/// 落库前的防御式归一化：非法值兜底为 `consent`（默认行为零变化）。
/// **不在此处应用运营开关**——落库值表达建房方意图，是否生效由读取侧 `effective_lethality` 裁定，
/// 这样开关才是可逆的急停阀而非一次性阉割。
fn normalize_lethality(raw: &str) -> &str {
    if is_valid_lethality(raw) {
        raw
    } else {
        LETHALITY_CONSENT
    }
}

// ---------- 世界封面（客户端设计文档 §6「真实位图封面」，迁移 0027） ----------

/// 世界封面原始字节上限（1MB）。比角色立绘的 512KB 大一档：封面是大厅主视觉与卡片图，
/// 渲染尺寸远大于头像。仍是防滥用硬上限，不是画质目标。
const MAX_COVER_BYTES: usize = 1024 * 1024;

/// 机审通过的落库标记（worlds.cover_moderation / cloud_characters.avatar_moderation 同一取值）。
const MODERATION_APPROVED: &str = "approved";

/// 🔴 封面读取面过滤（红线）：**只有机审 approved 的封面才允许下发 URL**。
///
/// 口径与角色立绘的「头像读取面双过滤」逐字一致（providers/mod.rs `check_image` 注释）：
/// 落库时无论裁决都写 cover_url（人审改判后无需重传），下发时按 cover_moderation 卡门。
/// pending（待人审）与 rejected（直拒）一律视同"没有封面"。
///
/// 另一条硬约束：**绝不下发空串**。无封面 / 未过审 → 返回 None → 调用方**不写该字段**，
/// 让前端走它已实现的"按 world.id 哈希确定性挑一张内置位图"兜底；下发 `""` 会被前端
/// `coverUrl?.trim() || 兜底` 之外的任何直接消费点渲染成碎图。
/// 可见性放宽至 `pub(crate)`（逻辑一字未改）：后台世界列表 `/admin/worlds` 同样是封面读取面，
/// 必须经**这一个**闸门，不得另写一套 approved 判断——两套口径一旦漂移就是未过审图片外泄。
pub(crate) fn visible_cover_url(
    cover_url: Option<String>,
    cover_moderation: Option<&str>,
) -> Option<String> {
    if cover_moderation != Some(MODERATION_APPROVED) {
        return None;
    }
    cover_url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ---------- 下一拍时刻推算（大厅「下次世界时刻」的真实时间戳来源） ----------

/// 调度器墙钟间隔的 env 覆盖名。**必须与 `runtime::schedule_due_ticks` 读的是同一个变量**。
const ENV_TICK_INTERVAL_MS: &str = "MUSE_TICK_INTERVAL_MS";

/// interval 世界的排拍间隔（毫秒）。
///
/// ⚠️ 这是 `runtime::schedule_due_ticks` 里那行
/// `interval_override.unwrap_or_else(|| 86_400_000 / tick_per_day.max(1))` 的**镜像**。
/// 不复用 runtime 的私有实现是因为它内联在调度循环里、没有可调用的公开函数；
/// 二者一旦分叉，玩家看到的"下一拍"就会和真实排拍时刻错位——`tick_interval_ms_mirrors_scheduler`
/// 用例把这条公式钉住，改调度器时该用例会红。
fn tick_interval_ms(tick_per_day: i64) -> i64 {
    std::env::var(ENV_TICK_INTERVAL_MS)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or_else(|| 86_400_000 / tick_per_day.max(1))
}

/// 下一拍的**估算**墙钟时刻（毫秒 epoch）；算不准就返回 `None`（宁可不给，也不给会漂移的假时间）。
///
/// 可推算的唯一情形 = `status == 'running'` **且** `timeline_mode == 'interval'`：
/// 调度器每轮取 `MAX(world_ticks.created_at)`，`now - last >= interval` 即排新拍，
/// 故下一拍最早在 `last + interval` 入队；世界还没有任何 tick 时 `due` 恒真 → 下一拍"就是现在"。
///
/// 三种**不可推算**（一律 None，调用方不下发该字段）：
/// - `status != 'running'`：open / paused / ended 世界压根不进调度器的 `WHERE status='running'`，
///   没有"下一拍"这回事。大厅里的 open 房尤其常见，给个时间纯属编造。
/// - `timeline_mode == 'event'` 且 idle 房：DES 背靠背——上一拍一 done 就立刻排下一拍，
///   节奏由引擎算出的游戏时钟决定，**不依赖墙钟**，无任何墙钟表达式可给。
/// - `timeline_mode == 'event'` 且 chapter/arena 房：新拍全部来自手动端点（arena host_tick /
///   chapter start·advance），由主持人/会话驱动，服务端无从预测。
///
/// **为什么叫 Estimated（字段 `nextTickEstimatedAt`）而不是 `nextTickAt`**——三处已知误差：
/// 1. 调度器是轮询的（`MUSE_TICK_POLL_MS`，默认 5s），实际入队落在 `[t, t + poll)`；
/// 2. 入队 ≠ 出内容：worker 认领 + 引擎跑完一回合还要数秒到数分钟；
/// 3. 世界可能在到点前因终局/暂停停机，那一拍不会发生。
/// 相对 8 小时（tick_per_day=3）量级的间隔，1、2 是分钟级误差；3 是"不发生"而非"晚发生"。
/// 误差方向恒为"不早于"——玩家按它回访不会错过内容，只会稍早到。
///
/// 注：拍与拍的间隔锚在 `created_at`（入队时刻）而非完成时刻，故单拍跑得慢/失败**不会**让后续
/// 排拍时刻整体漂移，估算不随失败累积。
fn next_tick_estimated_at(
    status: &str,
    timeline_mode: &str,
    interval_ms: i64,
    last_tick_created_at: Option<i64>,
    now: i64,
) -> Option<i64> {
    if status != "running" || timeline_mode != "interval" {
        return None;
    }
    // 无历史 tick → 调度器下一轮就排（due 恒真）→ 下一拍即"现在"。
    Some(match last_tick_created_at {
        Some(last) => last.saturating_add(interval_ms),
        None => now,
    })
}

/// 世界行（worlds 表投影，runtime 复用）。
#[derive(Debug, Clone)]
pub struct WorldRow {
    pub id: String,
    pub template_id: String,
    pub template_version: i64,
    pub engine_version: String,
    pub prompt_set_version: String,
    pub model_route_version: String,
    pub room_type: String,
    pub title: String,
    pub status: String,
    pub visibility: String,
    pub host_user_id: Option<String>,
    pub member_limit: i64,
    pub tick_per_day: i64,
    pub state_revision: i64,
    /// 当前叙事状态快照（E4 联编后由 worker 读取用于回合恢复/上下文）。
    pub narrative_state_json: String,
    /// 开局装配结果（钉住）：runtime 首 tick 从中提取硬节点/禁止谓词种子（E-1）。
    pub assembled_json: Option<String>,
    /// 时间线模式（第二块 Phase 2）：'interval'（默认，老世界墙钟固定间隔→run_round）
    /// 或 'event'（放置房 DES：背靠背→run_event_step 调度）。世界级渐进闸。
    pub timeline_mode: String,
    /// 世界游戏时钟快照（= NarrativeState.timeline.now，第二块 Phase 2）：commit_tick 每步回写。
    /// interval 世界恒为 0（不推进时钟）。Phase 2 仅回写、暂无读取方（调度器 T 由引擎从 FS 状态自算），
    /// 保留供后续 Phase/展示层读「当前游戏时刻」而不必反序列化整份 narrative_state_json。
    #[allow(dead_code)]
    pub game_time: i64,
    /// 生死契约档**落库原值**（'sanctuary' | 'consent' | 'deathmatch'，0026）：建房方意图。
    /// 消费方一律经 `effective_lethality(&row.lethality)` 换算生效档（含运营开关降级），
    /// 不要直接字符串比较——那会绕过急停阀。
    pub lethality: String,
    /// 封面回读 URL（0027）。**无论机审裁决如何都落库**，下发面另有 approved 门。
    /// 🔴 消费方一律经 `visible_cover_url(row.cover_url, row.cover_moderation.as_deref())`，
    /// 不要直接读本字段往响应里塞——那会漏出未过审封面。
    pub cover_url: Option<String>,
    /// 封面机审三态（'approved' | 'pending' | 'rejected'；NULL = 从未上传封面）。
    pub cover_moderation: Option<String>,
}

fn map_world(row: &sqlx::any::AnyRow) -> Result<WorldRow, ApiError> {
    Ok(WorldRow {
        id: row.try_get("id")?,
        template_id: row.try_get("template_id")?,
        template_version: row.try_get("template_version")?,
        engine_version: row.try_get("engine_version")?,
        prompt_set_version: row.try_get("prompt_set_version")?,
        model_route_version: row.try_get("model_route_version")?,
        room_type: row.try_get("room_type")?,
        title: row.try_get("title")?,
        status: row.try_get("status")?,
        visibility: row.try_get("visibility")?,
        host_user_id: row.try_get("host_user_id")?,
        member_limit: row.try_get("member_limit")?,
        tick_per_day: row.try_get("tick_per_day")?,
        state_revision: row.try_get("state_revision")?,
        narrative_state_json: row.try_get("narrative_state_json")?,
        assembled_json: row.try_get("assembled_json")?,
        timeline_mode: row.try_get("timeline_mode")?,
        game_time: row.try_get("game_time")?,
        lethality: row.try_get("lethality")?,
        cover_url: row.try_get("cover_url")?,
        cover_moderation: row.try_get("cover_moderation")?,
    })
}

/// 读取世界（不存在 → NotFound）。runtime 与 handler 共用。
pub async fn load_world(db: &AnyPool, id: &str) -> Result<WorldRow, ApiError> {
    let row = sqlx::query("SELECT * FROM worlds WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    map_world(&row)
}

// ---------- 大厅列表 ----------

#[derive(Debug, Deserialize)]
struct ListParams {
    #[serde(rename = "type")]
    room_type: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
    /// 标题搜索：大小写不敏感 LIKE 包含匹配；空串/全空白视为无搜索。
    q: Option<String>,
    /// 排序："new"（默认，created_at DESC + cursor 分页）| "hot"（热度快照）；其余值 400。
    sort: Option<String>,
}

/// cursor 编码为 `{created_at}:{id}`（created_at 无冒号，按首个冒号切分）。
fn parse_cursor(cursor: &str) -> Option<(i64, String)> {
    let (ts, id) = cursor.split_once(':')?;
    Some((ts.parse().ok()?, id.to_string()))
}

/// LIKE 模式转义：% _ \ 前置 \（配合 `ESCAPE '\'`），防用户输入被当通配符误匹配。
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 热度时间窗（毫秒）：事件近 48h、打赏近 7 天。
/// 边界在 Rust 侧按 now_ms 计算后以参数传入——双库约束禁 SQL 日期函数（db.rs）。
const HOT_EVENTS_WINDOW_MS: i64 = 48 * 3600 * 1000;
const HOT_GIFTS_WINDOW_MS: i64 = 7 * 24 * 3600 * 1000;

/// 星级投影子查询（列表 new/hot 共用）：模板行缺失（历史数据）COALESCE 兜底 1★。
const STAR_RATING_SUBQUERY: &str =
    "COALESCE((SELECT t.star_rating FROM world_templates t WHERE t.id = worlds.template_id), 1) AS star_rating";

/// 列表/详情共用的 SELECT 片段：封面两列 + 下一拍推算所需的时间线模式与末拍入队时刻。
/// 末拍时刻用相关子查询取（双库可移植：无 JOIN 去重问题，无方言日期函数）；无 tick → NULL。
const COVER_AND_TICK_COLUMNS: &str = "cover_url, cover_moderation, timeline_mode, \
     (SELECT MAX(t.created_at) FROM world_ticks t WHERE t.world_id = worlds.id) AS last_tick_at";

/// 列表项投影（new/hot 共用；hot 分支再追加 hotScore）。
/// `now` 由调用方一次算好传入——同一页所有世界的"下一拍"基于同一个 now，页内自洽。
fn world_list_item(row: &sqlx::any::AnyRow, id: &str, now: i64) -> Result<Value, ApiError> {
    let status: String = row.try_get("status")?;
    let tick_per_day: i64 = row.try_get("tick_per_day")?;
    let mut item = json!({
        "id": id,
        "roomType": row.try_get::<String, _>("room_type")?,
        "title": row.try_get::<String, _>("title")?,
        "status": &status,
        "visibility": row.try_get::<String, _>("visibility")?,
        "memberLimit": row.try_get::<i64, _>("member_limit")?,
        "memberCount": row.try_get::<i64, _>("member_count")?,
        "tickPerDay": tick_per_day,
        "starRating": row.try_get::<i64, _>("star_rating")?,
        // 生死契约档（§11）：与 starRating 并列的独立维度，供大厅筛选「选难度 = 选星级 × 选契约」。
        "lethality": lethality_label(&row.try_get::<String, _>("lethality")?),
        "aiLabel": { "visible": true },
    });

    // 🔴 封面：仅机审 approved 才写字段；无封面/未过审 → **不写该键**（不是空串、不是 null），
    // 前端据"字段缺席"走它已实现的确定性内置位图兜底。
    let cover_moderation: Option<String> = row.try_get("cover_moderation")?;
    if let Some(url) = visible_cover_url(row.try_get("cover_url")?, cover_moderation.as_deref()) {
        item["coverUrl"] = json!(url);
    }

    // 下一拍估算时刻：算不准（非 running / event 房）→ 不写该键，绝不下发会漂移的假时间。
    let timeline_mode: String = row.try_get("timeline_mode")?;
    let last_tick_at: Option<i64> = row.try_get("last_tick_at")?;
    if let Some(at) = next_tick_estimated_at(
        &status,
        &timeline_mode,
        tick_interval_ms(tick_per_day),
        last_tick_at,
        now,
    ) {
        item["nextTickEstimatedAt"] = json!(at);
    }
    Ok(item)
}

async fn list_worlds(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    // q 归一化：空串/全空白视为无搜索；否则转义为 %包含% 模式。
    // 大小写一致性：SQLite LIKE 对 ASCII 天然不敏感而 PG 敏感，统一 LOWER(title) LIKE LOWER(?) 拉平双库。
    let like = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", escape_like(s)));

    match params.sort.as_deref() {
        None | Some("new") => list_worlds_new(&state, &params, like).await,
        Some("hot") => list_worlds_hot(&state, &params, like).await,
        Some(other) => Err(ApiError::BadRequest(format!("非法 sort 值「{other}」：仅支持 new / hot"))),
    }
}

/// sort=new（默认）：现行为不变——created_at DESC + cursor 分页；q 仅叠加 WHERE。
async fn list_worlds_new(
    state: &AppState,
    params: &ListParams,
    like: Option<String>,
) -> Result<Json<Value>, ApiError> {
    let page = params.limit.unwrap_or(20).clamp(1, 100);
    let now = now_ms();
    // 仅可见世界：open/running 且 official/public。
    let mut sql = format!(
        "SELECT id, room_type, title, status, visibility, member_limit, tick_per_day, created_at, lethality, \
         {COVER_AND_TICK_COLUMNS}, \
         (SELECT COUNT(*) FROM world_members m WHERE m.world_id = worlds.id AND m.status='active') AS member_count, \
         {STAR_RATING_SUBQUERY} \
         FROM worlds \
         WHERE status IN ('open','running') AND visibility IN ('official','public')",
    );
    if params.room_type.is_some() {
        sql.push_str(" AND room_type = ?");
    }
    if like.is_some() {
        sql.push_str(" AND LOWER(title) LIKE LOWER(?) ESCAPE '\\'");
    }
    let cursor = params.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

    let mut q = sqlx::query(&sql);
    if let Some(rt) = &params.room_type {
        q = q.bind(rt);
    }
    if let Some(pat) = &like {
        q = q.bind(pat);
    }
    if let Some((ts, id)) = &cursor {
        q = q.bind(*ts).bind(*ts).bind(id);
    }
    q = q.bind(page + 1);

    let rows = q.fetch_all(&state.db).await?;
    let has_more = rows.len() as i64 > page;
    let mut items = Vec::new();
    let mut next_cursor: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        if i as i64 >= page {
            break;
        }
        let created_at: i64 = row.try_get("created_at")?;
        let id: String = row.try_get("id")?;
        next_cursor = Some(format!("{created_at}:{id}"));
        items.push(world_list_item(row, &id, now)?);
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "worlds": items, "nextCursor": next_cursor })))
}

/// sort=hot：热度榜快照。热度分 = 近 48h 事件数×1 + 近 7 天打赏 gift_count 总和×5 + active 成员数×2。
/// 对候选世界（status/visibility/type/q 过滤后）逐行子查询聚合；LIMIT clamp ≤50；
/// 不支持 cursor（快照榜，忽略 cursor 参数，nextCursor 恒为 null）；每项附 hotScore（BIGINT）。
async fn list_worlds_hot(
    state: &AppState,
    params: &ListParams,
    like: Option<String>,
) -> Result<Json<Value>, ApiError> {
    let page = params.limit.unwrap_or(20).clamp(1, 50);
    let now = now_ms();
    let events_since = now - HOT_EVENTS_WINDOW_MS;
    let gifts_since = now - HOT_GIFTS_WINDOW_MS;

    // SUM 可移植性：CAST(COALESCE(SUM(x),0) AS BIGINT)（先例 admin_api/reconcile.rs）；
    // 整体分再 CAST 一次，保证双库返回 BIGINT。gift_events 表恒存在（迁移不随 feature 门控）。
    let mut sql = format!(
        "SELECT id, room_type, title, status, visibility, member_limit, tick_per_day, created_at, lethality, \
         {COVER_AND_TICK_COLUMNS}, \
         (SELECT COUNT(*) FROM world_members m WHERE m.world_id = worlds.id AND m.status='active') AS member_count, \
         {STAR_RATING_SUBQUERY}, \
         CAST( \
           (SELECT COUNT(*) FROM world_events e WHERE e.world_id = worlds.id AND e.occurred_at >= ?) \
           + (SELECT CAST(COALESCE(SUM(g.gift_count),0) AS BIGINT) FROM gift_events g WHERE g.world_id = worlds.id AND g.created_at >= ?) * 5 \
           + (SELECT COUNT(*) FROM world_members m2 WHERE m2.world_id = worlds.id AND m2.status='active') * 2 \
         AS BIGINT) AS hot_score \
         FROM worlds \
         WHERE status IN ('open','running') AND visibility IN ('official','public')",
    );
    if params.room_type.is_some() {
        sql.push_str(" AND room_type = ?");
    }
    if like.is_some() {
        sql.push_str(" AND LOWER(title) LIKE LOWER(?) ESCAPE '\\'");
    }
    sql.push_str(" ORDER BY hot_score DESC, created_at DESC, id DESC LIMIT ?");

    let mut q = sqlx::query(&sql).bind(events_since).bind(gifts_since);
    if let Some(rt) = &params.room_type {
        q = q.bind(rt);
    }
    if let Some(pat) = &like {
        q = q.bind(pat);
    }
    q = q.bind(page);

    let rows = q.fetch_all(&state.db).await?;
    let mut items = Vec::new();
    for row in &rows {
        let id: String = row.try_get("id")?;
        let mut item = world_list_item(row, &id, now)?;
        item["hotScore"] = json!(row.try_get::<i64, _>("hot_score")?);
        items.push(item);
    }
    Ok(Json(json!({ "worlds": items, "nextCursor": null })))
}

// ---------- 世界详情 ----------

/// 世界读取面的可见性闸：私有世界仅房主 / active 成员可读，其余一律 403。
/// 详情页与 BE 传记页共用**这一个**判定——两处一旦各写一套，私有房的读权限口径必然漂移。
async fn ensure_world_readable(
    db: &AnyPool,
    world: &WorldRow,
    user: &AuthUser,
) -> Result<(), ApiError> {
    if world.visibility != "private" {
        return Ok(());
    }
    let is_host = world.host_user_id.as_deref() == Some(user.user_id.as_str());
    let is_member = sqlx::query(
        "SELECT 1 AS x FROM world_members WHERE world_id = ? AND user_id = ? AND status='active' LIMIT 1",
    )
    .bind(&world.id)
    .bind(&user.user_id)
    .fetch_optional(db)
    .await?
    .is_some();
    if !is_host && !is_member {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

async fn world_detail(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let world = load_world(&state.db, &id).await?;
    ensure_world_readable(&state.db, &world, &user).await?;

    // 公开阵容：active 成员 + 角色公开名（AI 标识）+ 头像（仅过审才带）。
    let member_rows = sqlx::query(
        "SELECT wm.cloud_character_id AS cid, cc.card_json AS card, \
         cc.avatar_url AS avatar_url, cc.avatar_moderation AS avatar_moderation \
         FROM world_members wm JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
         WHERE wm.world_id = ? AND wm.status='active' ORDER BY wm.joined_at ASC",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut roster = Vec::new();
    for r in &member_rows {
        let cid: String = r.try_get("cid")?;
        let card: String = r.try_get("card")?;
        let name = serde_json::from_str::<Value>(&card)
            .ok()
            .and_then(|v| v["identity"]["name"].as_str().map(str::to_string))
            .unwrap_or_default();
        let mut item = json!({ "cloudCharacterId": cid, "name": name, "aiLabel": { "visible": true } });
        // 红线：仅头像机审 approved 才带 avatarUrl，否则不带该字段（前端回退首字头像）。
        let avatar_url: Option<String> = r.try_get("avatar_url")?;
        let avatar_moderation: Option<String> = r.try_get("avatar_moderation")?;
        if avatar_moderation.as_deref() == Some("approved") {
            if let Some(url) = avatar_url {
                item["avatarUrl"] = json!(url);
            }
        }
        roster.push(item);
    }

    // 星级投影（波次 3）：从模板读当前 star_rating；模板行缺失（历史数据）兜底 1★。
    let star_rating: i64 = sqlx::query_scalar("SELECT star_rating FROM world_templates WHERE id = ?")
        .bind(&world.template_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or(1);

    // 下一拍估算：末拍入队时刻 + 排拍间隔（仅 running 的 interval 世界可算，详见 next_tick_estimated_at）。
    let last_tick_at: Option<i64> =
        sqlx::query_scalar("SELECT MAX(created_at) FROM world_ticks WHERE world_id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    let mut detail = json!({
        "id": world.id,
        "title": world.title,
        "roomType": world.room_type,
        "status": world.status,
        "visibility": world.visibility,
        "memberLimit": world.member_limit,
        "memberCount": roster.len(),
        "tickPerDay": world.tick_per_day,
        "starRating": star_rating,
        // 生死契约档（§11【拍板 24】）：与星级正交的风险维度。**join 前明示**的载体——
        // 前端据此在投放前渲染档位说明与冷静提示；生死状档还须玩家二次确认（见 join 的 acceptDeathContract）。
        "lethality": lethality_label(&world.lethality),
        // 生死状世界的入场契约要求（客户端不必硬编码规则）：true 时 join 必须带 acceptDeathContract=true，
        // 且仅已声明成年（真红线 §0.4 未成年禁入生死状）可入。
        "deathContractRequired": lethality_label(&world.lethality) == LETHALITY_DEATHMATCH,
        // 客户端干预用 expectedWorldRevision 做乐观并发校验（C1 集成缝）。
        "stateRevision": world.state_revision,
        "templateId": world.template_id,
        "templateVersion": world.template_version,
        "engineVersion": world.engine_version,
        "promptSetVersion": world.prompt_set_version,
        "modelRouteVersion": world.model_route_version,
        "roster": roster,
        // 合规信息展示（§2.1）：AI 生成标识 + 仲裁公开承诺。
        "aiLabel": { "visible": true },
        "compliance": { "aiGenerated": true, "arbitrationPublic": true },
    });

    // 🔴 封面：与列表面同一个 approved 门；无封面/未过审 → **不写该键**（不是空串）。
    if let Some(url) = visible_cover_url(world.cover_url.clone(), world.cover_moderation.as_deref()) {
        detail["coverUrl"] = json!(url);
    }
    // 下一拍：算不准就不写该键（open/paused/ended 世界、event 房一律缺席）。
    if let Some(at) = next_tick_estimated_at(
        &world.status,
        &world.timeline_mode,
        tick_interval_ms(world.tick_per_day),
        last_tick_at,
        now_ms(),
    ) {
        detail["nextTickEstimatedAt"] = json!(at);
    }
    // 世界系列（§5 自动扩容）：属于已登记系列且开关开启时给出排队指路（第几号 / 还有哪号能进）。
    // 纯读，绝不建房（见 series_view 注释）；开关关闭或未登记 → **不写该键**，前端零感知。
    if let Some(view) = series_view(&state.db, &world).await? {
        detail["series"] = view;
    }
    Ok(Json(detail))
}

// ---------- 世界封面上传（POST /worlds/{id}/cover） ----------

/// 世界封面上传请求（base64 JSON，形态与角色立绘 `POST /assets/characters/{id}/avatar` 逐字一致，
/// 复用现有 JSON 栈，不引入 multipart）。
///
/// 可见性 `pub(crate)`（逻辑一字未改）：admin 官方建房把它作为可选入参内嵌，建房后**原样转交**
/// 下面的 `upload_cover`，两条路径的封面载荷形态因此恒定同源。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoverReq {
    /// 标准 base64 编码的原始图片字节（不含 data: 前缀）。
    pub(crate) image_base64: String,
    /// image/png | image/jpeg | image/webp
    pub(crate) mime: String,
}

/// 可为**官方世界**设封面的角色。
///
/// 官方房由运营建房（admin S6，`visibility='official'` 且 `host_user_id` 为 NULL），封面属于建房动作的
/// 一部分，故归运营。取 admin/operator 两档而非 `AdminUser` 的全集：reviewer/support/finance 分别是
/// 审核、客服、财务角色，不承担内容投放职责——口径同 `admin_api::require_role` 对治理写操作的收窄。
fn can_set_official_cover(role: &str) -> bool {
    matches!(role, "admin" | "operator")
}

/// 封面写权限判定（读取面无关，只管谁能改）。
///
/// - 官方世界（`visibility='official'`）→ 运营角色，见 `can_set_official_cover`。
/// - 创作者世界（public/private，房主建房 `POST /worlds` 落 `host_user_id`）→ **仅房主本人**。
///   运营不在此路径覆盖创作者封面：运营对违规封面的处置手段是审核态（cover_moderation 置非
///   approved 即刻全站不可见），而不是替人换图——换图会让"这张图是谁放的"失去单一责任人。
/// - `host_user_id` 为 NULL 的非官方世界（理论上不存在，防御）→ 无人可设。
fn can_set_cover(world: &WorldRow, user: &AuthUser) -> bool {
    if world.visibility == "official" {
        return can_set_official_cover(&user.role);
    }
    match world.host_user_id.as_deref() {
        Some(host) => host == user.user_id,
        None => false,
    }
}

/// POST /worlds/{id}/cover：上传世界封面位图（权限校验 + 图审 + 行级字段落库）。
///
/// 范式完全照抄 `assets::upload_avatar`（角色立绘）：MIME 白名单 → base64 解码 → 大小上限 →
/// 写对象存储（键 `covers/{world_id}.{ext}`，世界 id 为 128 位随机 uuid，充当能力 URL）→
/// `ModerationProvider::check_image` 图审 → UPDATE cover_object_key / cover_url / cover_moderation。
///
/// 🔴 红线：cover_url 无论裁决都落库（人审改判后无需重传），但**响应与所有读取面仅 approved 才给 URL**
/// （`visible_cover_url`）。私密房不豁免——封面上传与房间可见性无关，恒过机审。
/// 世界不存在 → 404；无权限 → 403（口径同 `world_detail` 对私有房的 Forbidden）。
///
/// 可见性 `pub(crate)`（逻辑一字未改）：admin 官方建房（`admin_api::worlds_ops::create_world`）
/// 建房后**直接调用本函数**完成封面落地，而不是把权限判定/图审/落库复制一份——
/// 复制一份必然与本函数漂移（尤其是 `ModerationProvider::check_image` 这道内容安全闸）。
pub(crate) async fn upload_cover(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<CoverReq>,
) -> Result<Json<Value>, ApiError> {
    let world = load_world(&state.db, &id).await?;
    if !can_set_cover(&world, &user) {
        return Err(ApiError::Forbidden);
    }

    // MIME 白名单（png/jpeg/webp），与角色立绘共用同一张表。
    let ext = crate::assets::image_ext(req.mime.trim()).ok_or_else(|| {
        ApiError::BadRequest("封面格式不支持（仅 image/png、image/jpeg、image/webp）".into())
    })?;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(req.image_base64.trim())
        .map_err(|_| ApiError::BadRequest("封面 base64 解码失败".into()))?;
    if bytes.is_empty() {
        return Err(ApiError::BadRequest("封面数据为空".into()));
    }
    if bytes.len() > MAX_COVER_BYTES {
        return Err(ApiError::BadRequest("封面超过 1MB 上限".into()));
    }

    // 写对象存储：键以世界 id 命名（同世界重传覆盖式更新）。
    let object_key = format!("covers/{id}.{ext}");
    state.objects.put(&object_key, &bytes).map_err(ApiError::internal)?;

    // 图审（dev 直过，prod 待接第三方图审）。裁决三态与角色立绘同源。
    let verdict = state
        .moderation
        .check_image(&bytes)
        .await
        .map_err(|e| ApiError::internal(std::io::Error::other(e)))?;
    let moderation = crate::assets::verdict_str(verdict);
    let cover_url = format!("/api/assets/objects/{object_key}");

    sqlx::query(
        "UPDATE worlds SET cover_object_key = ?, cover_url = ?, cover_moderation = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&object_key)
    .bind(&cover_url)
    .bind(moderation)
    .bind(now_ms())
    .bind(&id)
    .execute(&state.db)
    .await?;

    // 🔴 未过审绝不下发 URL（与列表/详情读取面同一个门）。
    let out_url = if verdict == ModerationVerdict::Approved { Some(cover_url) } else { None };
    Ok(Json(json!({ "worldId": id, "coverUrl": out_url, "moderation": moderation })))
}

// ---------- 投放（join） ----------

// ---------- 历练准入门槛（波次 3 星级第三环）：常量集中区（可调，数值即产品策划口径） ----------

/// 3★ 副本投放卡历练门槛。
const STAR3_MILEAGE_REQUIRED: i64 = 300;
/// 4★ 副本投放卡历练门槛。
const STAR4_MILEAGE_REQUIRED: i64 = 1000;
/// 5★ 副本投放卡历练门槛。
const STAR5_MILEAGE_REQUIRED: i64 = 3000;

/// 模板星级 → 投放卡历练门槛：1-2★ 无门槛；3★=300、4★=1000、5★（及以上防御归并）=3000。
/// 只挡「本次投放的卡」（历练挂卡不挂人，卡是养成容器）；历练仅用于准入，
/// 绝不进引擎决策（progression 模块红线，本函数只在 join 消费）。
fn star_mileage_gate(star: i64) -> Option<i64> {
    match star {
        ..=2 => None,
        3 => Some(STAR3_MILEAGE_REQUIRED),
        4 => Some(STAR4_MILEAGE_REQUIRED),
        _ => Some(STAR5_MILEAGE_REQUIRED),
    }
}

/// 同源冲突拒绝文案里的角色名（`card_json.identity.name`）。
/// 只在拒绝路径调用——正常 join 不反序列化整张卡（同源判定读的是物化列）。
/// 取不到名字（卡结构异常/无名）时兜底为「该角色」，文案仍可读，绝不因取名失败改变判定。
async fn character_display_name(db: &AnyPool, character_id: &str) -> String {
    let card: Option<String> =
        sqlx::query_scalar("SELECT card_json FROM cloud_characters WHERE id = ?")
            .bind(character_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    card.as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|v| v.pointer("/identity/name").and_then(|n| n.as_str()).map(str::to_string))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "该角色".to_string())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JoinRequest {
    cloud_character_id: String,
    #[serde(default)]
    boundary: Value,
    /// 生死状二次确认（§11【拍板 24】「入场二次确认 + 冷静提示」）。
    /// **仅生死状世界要求为 true**；庇护/同意制世界忽略本字段（缺省 false，老客户端零影响）。
    /// 缺确认一律拒绝——签生死状这件事绝不默认代签。
    #[serde(default)]
    accept_death_contract: bool,
}

async fn join_world(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<JoinRequest>,
) -> Result<Json<Value>, ApiError> {
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(
        &serde_json::to_vec(&json!({ "worldId": id, "body": body })).unwrap_or_default(),
    );
    let guard =
        idempotency::guard(&state.db, &user.user_id, "worlds.join", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    let world = load_world(&state.db, &id).await?;
    if !matches!(world.status.as_str(), "open" | "running") {
        return Err(ApiError::Conflict("world_not_joinable".into()));
    }

    // 角色服务端权威校验：属本人 + approved + 未撤回（mileage 供下方星级历练准入，
    // source_fingerprint/pristine 供下方同源唯一校验——同一次查询读出，不新增数据库往返）。
    let ch = sqlx::query(
        "SELECT owner_id, moderation, withdrawn, mileage, source_fingerprint, pristine FROM cloud_characters WHERE id = ?",
    )
    .bind(&body.cloud_character_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let owner_id: String = ch.try_get("owner_id")?;
    let moderation: String = ch.try_get("moderation")?;
    let withdrawn: i64 = ch.try_get("withdrawn")?;
    let mileage: i64 = ch.try_get("mileage")?;
    let source_fingerprint: Option<String> = ch.try_get("source_fingerprint")?;
    let pristine: i64 = ch.try_get("pristine")?;
    if owner_id != user.user_id {
        return Err(ApiError::Forbidden);
    }
    if moderation != "approved" {
        return Err(ApiError::Conflict("character_not_approved".into()));
    }
    if withdrawn != 0 {
        return Err(ApiError::Conflict("character_withdrawn".into()));
    }

    // ---------- 生死契约入场门（§11【拍板 24】，与星级/历练三维正交） ----------
    // 生效档由落库值经运营开关归一：开关未开的生死状降级为同意制 → 本段整体跳过。
    // 庇护/同意制世界（含**全部历史世界**，0026 默认 'consent'）本段零动作，行为与历史完全一致。
    let lethality = effective_lethality(&world.lethality);
    if lethality == Lethality::Deathmatch {
        // 🔴 真红线 §0.4 未成年人保护：**未成年禁入生死状**。
        // fail-closed：仅 age_declared==1（已声明成年）放行；未声明(0)、未成年(2)、用户行缺失
        // 一律 403 —— **年龄未知按未成年处理**，无法可靠判断年龄前绝不放行（口径与 billing 保守拒充一致，
        // 堵住"只拦已声明的未成年"这种空防）。声明入口：POST /auth/age-declaration。
        // 用 403（而非本端点资格类拒绝惯用的 409）：这是永久禁入而非可解冲突，重试不可能通过；
        // 语义与既有红线先例（billing 拒充）逐字一致。前端已从世界详情拿到 deathContractRequired，
        // 可在投放前就把「未成年不可进入生死状世界」讲清楚，不必依赖本响应体文案。
        let age: Option<(i64,)> = sqlx::query_as("SELECT age_declared FROM users WHERE id = ?")
            .bind(&user.user_id)
            .fetch_optional(&state.db)
            .await?;
        if !matches!(age, Some((1,))) {
            return Err(ApiError::Forbidden);
        }
        // 入场即签：明示在前（详情页 lethality/deathContractRequired），确认在此。
        if !body.accept_death_contract {
            return Err(ApiError::Conflict(
                "这是生死状世界：入场即签生死状，你的角色可能真的死去，事后不再逐次征询同意。\
                 请确认知情后带 acceptDeathContract=true 重新投放"
                    .into(),
            ));
        }
    }

    // 历练准入（波次 3 星级门槛，与防自刷同为投放资格校验）：模板 star≥3 时要求**本次投放的卡**
    // 历练达标（1-2★ 无门槛）。模板行缺失（测试/历史数据）按 1★ 兜底 → 无门槛，与老行为一致。
    // 409（Conflict）对齐本端点既有资格类拒绝（character_not_approved / 防自刷）的错误风格。
    let star: i64 = sqlx::query_scalar("SELECT star_rating FROM world_templates WHERE id = ?")
        .bind(&world.template_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or(1);
    if let Some(required) = star_mileage_gate(star) {
        if mileage < required {
            return Err(ApiError::Conflict(format!(
                "该世界为 {star} 星副本，需角色历练 ≥{required}（当前 {mileage}）"
            )));
        }
    }

    // 同源卡同世界唯一（R1，规格 §7「这个世界只有一个唐三」，与上方星级准入同为投放资格类校验）：
    // 同一世界内，同一提取源的**未编辑原味卡**只允许在场一张；撞卡的玩家被引导去编辑出自己的
    // 版本，或换一个世界实例（叙事合理 + 撞卡压力转化为编辑创作激励 + 热门卡分流多实例）。
    //
    // 两条放行硬约束（不得收紧）：
    // - 只拦 pristine=1。玩家编辑过的卡（lifecycle/revision 任一变动）一律放行——「编辑出你自己的
    //   版本」就是本规则的出口，拦掉编辑过的卡等于毁掉这个设计。
    // - 指纹为 NULL 一律放行：纯原创卡本无提取源，迁移前的老卡也没有物化指纹，不得因缺字段拒绝入世。
    //
    // 排除本卡自身 → 同卡重复 join / 复活仍走下方幂等与复活分支，现有行为不回退。
    //
    // 取舍说明（为何不加条件唯一索引，体例同下方防自刷）：「pristine 卡按 fingerprint 唯一」需要
    // 带 WHERE 的部分唯一索引，不在 SQLite/Postgres 双库可移植子集内（0021 迁移只建普通索引）。
    // 应用层校验覆盖正常路径；并发窗口下两张同源原味卡理论上可各落一行 active，但撞进两行也无资损
    // ——同源唯一是叙事体验约束而非结算口径，多出的行不影响结算、可事后治理。
    if pristine == 1 {
        if let Some(fingerprint) = source_fingerprint.as_deref() {
            let same_source: i64 = sqlx::query(
                "SELECT COUNT(*) AS n FROM world_members wm \
                 JOIN cloud_characters cc ON cc.id = wm.cloud_character_id \
                 WHERE wm.world_id = ? AND wm.status = 'active' \
                 AND cc.source_fingerprint = ? AND cc.pristine = 1 \
                 AND wm.cloud_character_id != ?",
            )
            .bind(&id)
            .bind(fingerprint)
            .bind(&body.cloud_character_id)
            .fetch_one(&state.db)
            .await?
            .try_get("n")?;
            if same_source > 0 {
                // 引导型拒绝（面向玩家）→ 用中文长句文案派，与机器码 world_not_joinable 一类区分。
                let name = character_display_name(&state.db, &body.cloud_character_id).await;
                return Err(ApiError::Conflict(format!(
                    "这个世界已经有一个「{name}」了。编辑出你自己的版本，或换一个世界实例"
                )));
            }
        }
    }

    // 防自刷：同一世界每位用户同时仅可投放一张 active 角色卡（多卡进场可抢隐藏任务钩子）。
    // 排除本卡自身 → 同卡重复 join / 同卡复活仍走下方幂等与复活分支，现有行为不回退；
    // 只数 status='active' → 已退出（left/retired）不占名额，退出后可换卡再进。
    //
    // 取舍说明（为何不加迁移）：world_members 唯一索引是卡级 (world_id, cloud_character_id)
    // （0001_init.sql:132），user_id 仅普通索引（:133）。这里不补 (world_id, user_id) 按
    // status='active' 的条件唯一索引——应用层校验已覆盖正常路径；并发窗口下同 user 两卡
    // 同时 join 理论上可各落一行 active，但真撞进两行也无资损：后续结算按任务钩子绑定计，
    // 不按成员行数计，多出的行不影响结算、可事后治理，收益不抵迁移与回填成本。
    let other_active: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM world_members \
         WHERE world_id = ? AND user_id = ? AND status = 'active' AND cloud_character_id != ?",
    )
    .bind(&id)
    .bind(&user.user_id)
    .bind(&body.cloud_character_id)
    .fetch_one(&state.db)
    .await?
    .try_get("n")?;
    if other_active > 0 {
        return Err(ApiError::Conflict("同一世界每位用户仅可投放一张角色卡".into()));
    }

    // 已有成员记录（唯一键 world+character）：active 直接幂等返回；left/retired 复活。
    let existing = sqlx::query(
        "SELECT id, status FROM world_members WHERE world_id = ? AND cloud_character_id = ?",
    )
    .bind(&id)
    .bind(&body.cloud_character_id)
    .fetch_optional(&state.db)
    .await?;

    // C-4：人数上限原子化。旧实现是 count→check→insert 的 TOCTOU（唯一索引只挡同角色重复，挡不住并发凑满）。
    // 改为「带人数子查询守卫的条件写」：limit 判定与写入在同一条语句里求值，rows_affected==0 即满员。
    // （sqlite 语句级原子；postgres 同快照下将 TOCTOU 窗口收敛到单语句，配合唯一索引把越额上限收敛到并发不同角色数。）
    // 本次调用是否真的把一张卡投放进场（新投放 / 复活）。已 active 的幂等重放为 false ——
    // 生死状签署留痕只认真实入场，重放不重复留痕。
    let mut entered_now = false;
    let membership_id: String = if let Some(m) = existing {
        let mid: String = m.try_get("id")?;
        let mstatus: String = m.try_get("status")?;
        if mstatus != "active" {
            // 复活：仅当仍有空位时置 active（人数守卫内嵌）；已满 → world_full。
            // `left_at=NULL`（迁移 0030）：复活本来就已经重写 `joined_at`（成员纪元重置），
            // 若留着上一段的退出时刻会得到 `left_at < joined_at` 的自相矛盾行，污染一切留存/时序统计。
            // 这不违反「公共事实不可回滚」——那条红线约束的是世界的公共叙事事实
            //（world_events / 账本 / 结算），world_members 是可变的成员状态行（status/user_id/joined_at
            // 本来就随复活重写）。
            let res = sqlx::query(
                "UPDATE world_members SET status='active', user_id=?, boundary_json=?, joined_at=?, left_at=NULL \
                 WHERE id=? AND status != 'active' \
                 AND (SELECT COUNT(*) FROM world_members WHERE world_id=? AND status='active') < ?",
            )
            .bind(&user.user_id)
            .bind(body.boundary.to_string())
            .bind(now_ms())
            .bind(&mid)
            .bind(&id)
            .bind(world.member_limit)
            .execute(&state.db)
            .await?;
            if res.rows_affected() == 0 {
                // 满员 → 排队分房（§5）：确保下一号可入实例存在，把它附在错误码后。资格校验一条不少
                // 地留在本函数里——扩容只回答"去哪个实例"。
                return Err(world_full_conflict(&state.db, &world).await);
            }
            entered_now = true;
        }
        // 已 active：幂等，无需再判上限。
        mid
    } else {
        let mid = new_id("wm");
        // 条件插入：仅当活跃人数 < 上限时落一行（SELECT 常量 + WHERE 子查询守卫）。
        let res = sqlx::query(
            "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
             SELECT ?, ?, ?, ?, ?, 'active', ? \
             WHERE (SELECT COUNT(*) FROM world_members WHERE world_id=? AND status='active') < ?",
        )
        .bind(&mid)
        .bind(&id)
        .bind(&user.user_id)
        .bind(&body.cloud_character_id)
        .bind(body.boundary.to_string())
        .bind(now_ms())
        .bind(&id)
        .bind(world.member_limit)
        .execute(&state.db)
        .await;
        match res {
            // 满员 → 排队分房（§5），同上：只指路，不代投放。
            Ok(r) if r.rows_affected() == 0 => {
                return Err(world_full_conflict(&state.db, &world).await)
            }
            Ok(_) => entered_now = true,
            // 并发下同角色抢插：唯一索引兜底 → 视为已在场（幂等成功），非本次入场，不留痕。
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {}
            Err(e) => return Err(e.into()),
        }
        mid
    };

    // 生死状签署留痕（§0.2「资产单一写入路径与全链审计」的同一口径：高风险不可逆授权必须可溯）。
    // 每次真实入场落一条 audit_logs——"谁、哪张卡、什么时候、在哪个世界签了生死状"，
    // 事后死亡争议与未成年申诉都靠这条痕对质。actor_role 记 'user'（玩家自签，非运营代操作）。
    // 位置在成员写入之后：被人数上限/资格门拒掉的请求不会留下"签过"的假痕。
    if lethality == Lethality::Deathmatch && entered_now {
        sqlx::query(
            "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
             VALUES (?, ?, 'user', 'world.death_contract_signed', ?, ?, ?)",
        )
        .bind(new_id("aud"))
        .bind(&user.user_id)
        .bind(&id)
        .bind(format!(
            "lethality=deathmatch|character={}|membership={}",
            body.cloud_character_id, membership_id
        ))
        .bind(now_ms())
        .execute(&state.db)
        .await?;
    }

    let response = json!({
        "membershipId": membership_id,
        "worldId": id,
        "cloudCharacterId": body.cloud_character_id,
        "status": "active",
        // 回执明示本次入场生效的契约档（生死状时即"你已签署"的凭据面）。
        "lethality": lethality_label(&world.lethality),
    });
    guard.store_response(&state.db, &response.to_string()).await?;
    Ok(Json(response))
}

// ---------- 离场（leave） ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaveRequest {
    cloud_character_id: String,
}

/// 离场：置 `status='left'` 并**记下退出时刻** `left_at`（迁移 0030）。
///
/// `left_at` 补的是 `docs/VALIDATION.md` §4.2「用户跳过/退出率」那一格的「⚠️ 半」——此前只有
/// `joined_at`，退出不留时刻，于是退出率只能拿当前 status 算一个截面，画不出留存曲线、
/// 做不了任何时序分析。
///
/// **幂等且不覆盖首次时刻**：UPDATE 的 `status='active'` 守卫使重复 leave 的第二次
/// `rows_affected=0` → 直接 404 返回，绝不会用第二次的墙钟把首次退出时刻改掉
/// （「只增不改」——已经发生的退出是既成事实）。
async fn leave_world(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<LeaveRequest>,
) -> Result<Json<Value>, ApiError> {
    let res = sqlx::query(
        "UPDATE world_members SET status='left', left_at=? \
         WHERE world_id=? AND cloud_character_id=? AND user_id=? AND status='active'",
    )
    .bind(now_ms())
    .bind(&id)
    .bind(&body.cloud_character_id)
    .bind(&user.user_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    // 离场事件的叙事化在下个 tick 由 runtime 处理（仅在场成员参与回合）。
    Ok(Json(json!({ "worldId": id, "cloudCharacterId": body.cloud_character_id, "status": "left" })))
}

// ---------- 内部建房（供 admin S6 调用） ----------

/// 创建世界参数。版本字段留 None 时由 create_world 解析当前 active 版本并钉住。
/// （供 admin S6 官方建房调用；本 crate 内目前仅测试消费）
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CreateWorldParams {
    pub template_id: String,
    pub template_version: i64,
    pub room_type: String,
    pub title: String,
    pub visibility: String,
    pub host_user_id: Option<String>,
    pub member_limit: i64,
    pub tick_per_day: i64,
    pub daily_token_budget: i64,
    pub daily_cny_budget_cents: i64,
    pub status: Option<String>,
    /// 时间线模式：'interval'（默认）或 'event'（放置房 DES）。落 worlds.timeline_mode 列，供调度分派。
    pub timeline_mode: String,
    /// 生死契约档：'sanctuary' | 'consent'（默认）| 'deathmatch'。落 worlds.lethality 列（0026）。
    ///
    /// **由建房方显式指定，星级不自动决定档位**（规格 §11 的「1-2★ 庇护 / 3★ 同意制 / 4-5★ 生死状可选」
    /// 是运营默认映射建议，明确要求"可分离、可配置"）：星级 = 内容规格，契约 = 风险档，两维正交，
    /// 同一星级可同时开庇护场与生死场（"选难度 = 选星级 × 选生死契约"）。把映射写死成代码规则
    /// 会直接堵死这个产品形态，故本层只收显式入参；缺省 = 同意制（现行机制，行为零变化）。
    pub lethality: String,
    pub engine_version: Option<String>,
    pub prompt_set_version: Option<String>,
    pub model_route_version: Option<String>,
    pub assembled_json: Option<String>,
    pub initial_state_json: Option<String>,
}

#[allow(dead_code)]
impl CreateWorldParams {
    /// 官方放置世界最小参数（其余默认）。
    /// B-2：官方建房必须带非零 token 预算 + 非零 cny 上限——否则 world_budgets 视为无上限（成本失控）。
    pub fn official(template_id: impl Into<String>, template_version: i64, title: impl Into<String>) -> Self {
        Self {
            template_id: template_id.into(),
            template_version,
            room_type: "idle".into(),
            title: title.into(),
            visibility: "official".into(),
            host_user_id: None,
            member_limit: 10,
            tick_per_day: 3,
            // 非零默认预算（daily_token_budget=0 会被 runtime 当作"无上限"）：给官方房一个保守的日 token 上限
            // 与 cny 熔断维度。运营可在 admin 建房时覆盖为具体额度。
            daily_token_budget: 200_000,
            daily_cny_budget_cents: 2_000,
            status: None,
            timeline_mode: "interval".into(),
            // 默认同意制 = 现行机制（未验证功能默认关闭）：不显式指定就没有任何行为变化。
            lethality: LETHALITY_CONSENT.into(),
            engine_version: None,
            prompt_set_version: None,
            model_route_version: None,
            assembled_json: None,
            initial_state_json: None,
        }
    }
}

#[allow(dead_code)]
async fn active_version_tx(tx: &mut Transaction<'_, Any>, table: &str) -> Result<Option<String>, ApiError> {
    let sql = format!("SELECT version FROM {table} WHERE active = 1 ORDER BY created_at DESC LIMIT 1");
    let row = sqlx::query(&sql).fetch_optional(&mut **tx).await?;
    Ok(match row {
        Some(r) => Some(r.try_get("version")?),
        None => None,
    })
}

/// 建房（事务版）：钉住引擎/prompt/模型/模板版本，写 worlds + world_budgets，返回 world_id。
/// **在调用方已开启的事务内执行**——P4b 房主建房把它与开房费 `ledger::charge` 组进同一 tx，
/// charge 失败即随 tx 回滚（零副作用，无 world/budget/journal 残留）；charge 的 resolve_share 需 world 已在 tx 内落库，
/// 故建房必须先于 charge。官方建房经下面的 `create_world` 薄封装（自开自提交 tx）。
#[allow(dead_code)]
pub async fn create_world_tx(tx: &mut Transaction<'_, Any>, p: CreateWorldParams) -> Result<String, ApiError> {
    let engine_version = match p.engine_version {
        Some(v) => v,
        None => muse_engine::ENGINE_VERSION.to_string(),
    };
    let prompt_set_version = match p.prompt_set_version {
        Some(v) => v,
        None => active_version_tx(tx, "prompt_versions").await?.unwrap_or_else(|| "dev-none".into()),
    };
    let model_route_version = match p.model_route_version {
        Some(v) => v,
        None => active_version_tx(tx, "model_routes").await?.unwrap_or_else(|| "dev-none".into()),
    };
    let now = now_ms();
    let id = new_id("wld");
    let status = p.status.unwrap_or_else(|| "open".into());
    // 防御式归一化：admin 入口已做枚举校验，但 P4b 房主建房亦复用；非法值兜底为 interval，
    // 保证落库的 timeline_mode 恒为调度器可分派的合法枚举（interval/event）。
    let timeline_mode = if matches!(p.timeline_mode.as_str(), "interval" | "event") {
        p.timeline_mode.as_str()
    } else {
        "interval"
    };
    // 同上：admin 入口已做枚举校验 + 运营开关校验，此处仅兜非法值 → consent，
    // 保证落库的 lethality 恒为合法枚举（房主建房路径不暴露本参数，恒取默认同意制）。
    let lethality = normalize_lethality(&p.lethality);

    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, host_user_id, member_limit, \
         tick_per_day, timeline_mode, lethality, assembled_json, state_revision, narrative_state_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&p.template_id)
    .bind(p.template_version)
    .bind(&engine_version)
    .bind(&prompt_set_version)
    .bind(&model_route_version)
    .bind(&p.room_type)
    .bind(&p.title)
    .bind(&status)
    .bind(&p.visibility)
    .bind(&p.host_user_id)
    .bind(p.member_limit)
    .bind(p.tick_per_day)
    .bind(timeline_mode)
    .bind(lethality)
    .bind(&p.assembled_json)
    .bind(p.initial_state_json.unwrap_or_else(|| "{}".into()))
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO world_budgets (world_id, daily_token_budget, daily_cny_budget_cents, \
         spent_tokens_today, budget_day, fused, updated_at) VALUES (?, ?, ?, 0, '', 0, ?)",
    )
    .bind(&id)
    .bind(p.daily_token_budget)
    .bind(p.daily_cny_budget_cents)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    Ok(id)
}

/// 建房（薄封装）：自开自提交事务调 `create_world_tx`。官方建房（admin S6）及 test 复用本签名。
/// 房主建房走 `POST /worlds` 的 `create_room`（把建房 + 开房费 charge 组进同一 tx，不走此封装）。
#[allow(dead_code)]
pub async fn create_world(db: &AnyPool, p: CreateWorldParams) -> Result<String, ApiError> {
    let mut tx = db.begin().await?;
    let id = create_world_tx(&mut tx, p).await?;
    tx.commit().await?;
    Ok(id)
}

// ===== WORLD-SERIES-REGION-BEGIN =====
// ======================= 世界系列自动扩容（总规格 §5「世界系列自动扩容【新增】」） =======================
//
// 规格原文：**series 排队分房层——1 号实例满员自动开 2 号。运营基建，建房参数复制 + 排队队列。**
//
// ── 触发条件 ─────────────────────────────────────────────────────────────────
// 唯一触发点是 `join_world` 的**满员分支**（`world_full`，人数上限原子守卫返回 0 行的那一刻）。
// 不在 tick/定时任务里预扩容：没人来敲门就不该多开一个烧预算的世界。
//
// ── 排队队列的形态：**指路，不占座** ────────────────────────────────────────
// 满员时服务端做两件事，然后仍然返回 409：
//   ① 确保「下一号可入实例」存在（已有可入的就指向它，没有才开新号）；
//   ② 把它的 id 附在错误码后：`world_full|next={worldId}`（老客户端按 `contains("world_full")`
//      匹配，行为零变化；新客户端可一键跳转）。同一信息也在 `GET /worlds/{id}` 的 `series` 段。
//
// 🔴 **为什么绝不替玩家把卡投进 2 号**（这是本模块最重要的一条）：
// 扩容只解决"去哪个实例"，不解决"能不能进"。join 的资格校验有一半是**按世界**判定的——
// 同源唯一（这个世界里已经有一个唐三了吗）、生死契约签署留痕（在**哪个**世界签的）、
// 人数上限、防自刷（同世界一人一卡）。要在扩容路径上代投放，就必须把这些校验复制一份；
// 复制一份必然与 join 漂移，漂移即红线破口。因此扩容路径**对 `world_members` 零写入**
// （源码断言 `series_expansion_never_writes_world_members`），玩家带着同一张卡对新实例
// 重新 join，全套服务端权威校验原封不动再跑一遍。
//
// ── 两道默认关闭闸（VALIDATION.md §0.1） ────────────────────────────────────
//   闸一：env 开关 `MUSE_WORLD_SERIES_AUTOSCALE`，**默认关闭**，全局急停阀，作用在**读取侧**——
//         关掉之后既有系列立即停止扩容（已开出的实例不受影响，那是世界不是功能），再打开原样恢复。
//   闸二：系列**必须显式登记**（`world_series` 表，运营建 1 号房时带 `series` 参数）。
//         未登记的世界——含全部历史世界与全部玩家自建房——永不扩容，行为零变化。
// ⚠️ 沿用现有 env 范式（同 `deathmatch_enabled` / `subplot_cards_enabled`）：本仓库尚无配置表，
//    做不到按世界灰度，这是已知缺口；将来配置表落地只改本节函数内部，调用点与降级语义不变。

/// 世界系列自动扩容的运营开关环境变量。
const ENV_SERIES_AUTOSCALE: &str = "MUSE_WORLD_SERIES_AUTOSCALE";
/// 自动扩容默认值 = **关闭**。
const DEFAULT_SERIES_AUTOSCALE_ENABLED: bool = false;

/// 系列实例数**全局硬顶**的 env 名（§0.2 参数化，禁写死）。
const ENV_SERIES_MAX_INSTANCES: &str = "MUSE_WORLD_SERIES_MAX_INSTANCES";
/// 全局硬顶默认值：一个系列最多 10 个实例（含 1 号）。
const DEFAULT_SERIES_MAX_INSTANCES: i64 = 10;
/// 硬顶的合法区间：至少 1（= 只有 1 号、等于不扩容），最多 200（再多调度器与成本盘都不该扛）。
const MIN_SERIES_MAX_INSTANCES: i64 = 1;
const HARD_SERIES_MAX_INSTANCES: i64 = 200;

/// 系列状态：可继续扩容。
const SERIES_STATUS_ACTIVE: &str = "active";

/// 自动扩容是否已由运营开启（env 覆盖 + 默认常量，范式同 `deathmatch_enabled`）。
///
/// 开关是**全局急停阀**，生效在读取侧（`ensure_next_series_instance` / `series_view` 第一行）：
/// 关掉即刻停止一切扩容与排队指路，再打开则原样恢复——可逆，不是一次性阉割。
pub fn series_autoscale_enabled() -> bool {
    match std::env::var(ENV_SERIES_AUTOSCALE) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            // 配错不静默开启会自动建世界的能力：回落默认（关闭）。
            _ => DEFAULT_SERIES_AUTOSCALE_ENABLED,
        },
        Err(_) => DEFAULT_SERIES_AUTOSCALE_ENABLED,
    }
}

/// 系列实例数的全局硬顶（运营可调；缺失/非法回落默认，再 clamp 进安全区间）。
pub(crate) fn series_max_instances_cap() -> i64 {
    std::env::var(ENV_SERIES_MAX_INSTANCES)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_SERIES_MAX_INSTANCES)
        .clamp(MIN_SERIES_MAX_INSTANCES, HARD_SERIES_MAX_INSTANCES)
}

/// 测试专用：扩容相关 env 的 RAII 夹具（范式与 `DeathmatchSwitch` 逐字一致，见其注释）。
/// 跨模块可见（admin_api 的建系列用例同样需要它），故定义在这里。
#[cfg(test)]
pub(crate) struct SeriesSwitch {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Vec<(&'static str, Option<String>)>,
}

#[cfg(test)]
impl SeriesSwitch {
    /// 只置总开关。
    pub(crate) fn set(on: bool) -> Self {
        Self::with(on, &[])
    }

    /// 置总开关 + 若干额外 env（如 `MUSE_WORLD_SERIES_MAX_INSTANCES`）。
    /// ⚠️ `extra` 里不要再放总开关键，否则 Drop 时同键恢复两次会把状态留给下一个用例。
    pub(crate) fn with(on: bool, extra: &[(&'static str, &str)]) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut prev = vec![(ENV_SERIES_AUTOSCALE, std::env::var(ENV_SERIES_AUTOSCALE).ok())];
        std::env::set_var(ENV_SERIES_AUTOSCALE, if on { "1" } else { "0" });
        for (k, v) in extra {
            prev.push((k, std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        Self { _guard: guard, prev }
    }
}

#[cfg(test)]
impl Drop for SeriesSwitch {
    fn drop(&mut self) {
        for (k, v) in &self.prev {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// 世界系列行（`world_series` 投影）。
#[derive(Debug, Clone)]
pub(crate) struct SeriesRow {
    pub(crate) id: String,
    /// 1 号实例——**建房参数的唯一复制源**（见迁移 0035 注释：锚死 1 号，队列多长都不漂）。
    pub(crate) origin_world_id: String,
    /// 该系列自带的实例数上限（运营建系列时设）。
    pub(crate) max_instances: i64,
    pub(crate) status: String,
}

impl SeriesRow {
    /// 生效上限 = 系列自带上限 ∧ 全局 env 硬顶，**取小**。两道都可调，任一收紧立即生效。
    fn effective_max_instances(&self) -> i64 {
        self.max_instances.clamp(0, series_max_instances_cap())
    }
}

/// 把一个世界登记为**系列源头（1 号实例）**。幂等：已登记 → 原样返回既有系列 id，不新建、不改参数。
///
/// 由 admin 官方建房（`admin_api::worlds_ops::create_world` 的可选 `series` 入参）调用。
/// 这是「逐系列显式开闸」那一道闸：没走过这个函数的世界，永远不会长出下一号实例。
pub(crate) async fn enroll_series(
    db: &AnyPool,
    world_id: &str,
    template_id: &str,
    max_instances: i64,
) -> Result<String, ApiError> {
    // 已登记（作为任一系列的任一号）→ 幂等返回它所属系列。
    if let Some((series, _)) = load_series_of_world(db, world_id).await? {
        return Ok(series.id);
    }
    let now = now_ms();
    let series_id = new_id("wsr");
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO world_series (id, origin_world_id, template_id, max_instances, status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&series_id)
    .bind(world_id)
    .bind(template_id)
    .bind(max_instances)
    .bind(SERIES_STATUS_ACTIVE)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO world_series_instances (series_id, instance_no, world_id, created_at) VALUES (?, 1, ?, ?)",
    )
    .bind(&series_id)
    .bind(world_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(series_id)
}

/// 按世界反查它所属的系列与号数；未登记 → `None`（绝大多数世界走这一支，零成本）。
pub(crate) async fn load_series_of_world(
    db: &AnyPool,
    world_id: &str,
) -> Result<Option<(SeriesRow, i64)>, ApiError> {
    let row = sqlx::query(
        "SELECT s.id AS sid, s.origin_world_id, s.max_instances, s.status, i.instance_no \
         FROM world_series_instances i JOIN world_series s ON s.id = i.series_id \
         WHERE i.world_id = ?",
    )
    .bind(world_id)
    .fetch_optional(db)
    .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    Ok(Some((
        SeriesRow {
            id: r.try_get("sid")?,
            origin_world_id: r.try_get("origin_world_id")?,
            max_instances: r.try_get("max_instances")?,
            status: r.try_get("status")?,
        },
        r.try_get("instance_no")?,
    )))
}

/// 系列里「还能进人」的实例（号数升序取第一个），排除 `exclude_world_id`。
///
/// 判据 = `status IN ('open','running')` 且 active 成员数 < member_limit——与 join 的人数守卫
/// **同一个口径**（成员计数条件逐字相同），故这里说"能进"与 join 真的能进不会打架。
/// 号数升序 = 队列语义：低号先填满，不会让人散在一堆半空的房里。
async fn find_open_instance(
    db: &AnyPool,
    series_id: &str,
    exclude_world_id: &str,
) -> Result<Option<(String, i64)>, ApiError> {
    let rows = sqlx::query(
        "SELECT i.instance_no, w.id AS wid, w.member_limit, \
         (SELECT COUNT(*) FROM world_members m WHERE m.world_id = w.id AND m.status='active') AS member_count \
         FROM world_series_instances i JOIN worlds w ON w.id = i.world_id \
         WHERE i.series_id = ? AND w.status IN ('open','running') \
         ORDER BY i.instance_no ASC",
    )
    .bind(series_id)
    .fetch_all(db)
    .await?;
    for r in &rows {
        let wid: String = r.try_get("wid")?;
        if wid == exclude_world_id {
            continue;
        }
        let limit: i64 = r.try_get("member_limit")?;
        let count: i64 = r.try_get("member_count")?;
        if count < limit {
            return Ok(Some((wid, r.try_get("instance_no")?)));
        }
    }
    Ok(None)
}

/// 系列当前最大号数（空系列 → 0）。
async fn max_instance_no(db: &AnyPool, series_id: &str) -> Result<i64, ApiError> {
    let n: Option<i64> = sqlx::query_scalar(
        "SELECT instance_no FROM world_series_instances WHERE series_id = ? ORDER BY instance_no DESC LIMIT 1",
    )
    .bind(series_id)
    .fetch_optional(db)
    .await?;
    Ok(n.unwrap_or(0))
}

/// 开出系列的第 `next_no` 号实例：**逐字段复制 1 号实例的建房参数**。
///
/// 复制的东西（= "建房参数"的全集，与 `CreateWorldParams` 一一对应）：
/// 模板 id 与钉住的模板版本 · 房型 · 可见性 · 主播 · 人数上限 · tick 节奏 · 时间线模式 ·
/// **生死契约档** · 日 token 预算与日 cny 熔断额（读 `world_budgets`）·
/// **钉住的 engine / prompt_set / model_route 三版本**。
///
/// 🔴 三版本为什么也照抄而不重新解析当前 active 版本：一个系列是**同一批可比的场次**，
/// 2 号若跑在新引擎/新 prompt 上，玩家在"同一个世界"里会得到两种体验，运营也没法把两号的
/// 数据放在一起看。要换版本就新建一个系列（新 1 号），而不是让队列在中途悄悄换轨。
///
/// 🔴 **不复制 `assembled_json`**：这不是参数，是**采样结果**。规格 §5 明写「实例 = 剧情超集的
/// 一次确定性采样，种子 = H(world_id‖阵容指纹‖template_version)」「1 号世界 10 个官员、
/// 2 号世界 1 个，皆由实例种子决定」——照抄采样等于把"一个模板，千个平行世界"抄成千个同一个世界，
/// 也抄掉了身份分布这重防刷。新实例留空，首次使用时由装配层按它自己的种子装配。
///
/// 状态：照抄 1 号的 status（`open`/`running`），1 号已不在这两态（paused/ended）时回落 `open`
/// ——新号不该一出生就是不可调度的僵尸房。标题在 1 号标题后缀号数以便大厅区分（**唯一有意为之的差异**）。
///
/// 幂等：世界与号数登记在**同一事务**内落库。并发者撞 `(series_id, instance_no)` 主键 → 整笔回滚
/// （新世界一并消失，不留孤儿房）→ 返回 `Ok(None)`，由调用方重查队列拿赢家开的那一号。
async fn spawn_next_instance(
    db: &AnyPool,
    series: &SeriesRow,
    next_no: i64,
) -> Result<Option<String>, ApiError> {
    let origin = load_world(db, &series.origin_world_id).await?;
    // 预算两列不在 worlds 表里，单独读一次（缺行 → 用 official() 的保守默认，绝不落 0：
    // daily_token_budget=0 会被 runtime 当作"无上限"，那是成本失控）。
    let budget = sqlx::query(
        "SELECT daily_token_budget, daily_cny_budget_cents FROM world_budgets WHERE world_id = ?",
    )
    .bind(&series.origin_world_id)
    .fetch_optional(db)
    .await?;
    let (daily_token_budget, daily_cny_budget_cents) = match &budget {
        Some(b) => (b.try_get::<i64, _>("daily_token_budget")?, b.try_get::<i64, _>("daily_cny_budget_cents")?),
        None => (200_000, 2_000),
    };
    let status = if matches!(origin.status.as_str(), "open" | "running") {
        origin.status.clone()
    } else {
        "open".to_string()
    };

    let params = CreateWorldParams {
        template_id: origin.template_id.clone(),
        template_version: origin.template_version,
        room_type: origin.room_type.clone(),
        title: format!("{} #{next_no}", origin.title),
        visibility: origin.visibility.clone(),
        host_user_id: origin.host_user_id.clone(),
        member_limit: origin.member_limit,
        tick_per_day: origin.tick_per_day,
        daily_token_budget,
        daily_cny_budget_cents,
        status: Some(status),
        timeline_mode: origin.timeline_mode.clone(),
        // 落库原值照抄（不是 effective_lethality）：生效档由读取侧的运营开关裁定，
        // 抄生效档会让"开关关着时扩容出来的房"永久丢掉建房方的生死状意图。
        lethality: origin.lethality.clone(),
        engine_version: Some(origin.engine_version.clone()),
        prompt_set_version: Some(origin.prompt_set_version.clone()),
        model_route_version: Some(origin.model_route_version.clone()),
        // 采样结果不复制（见上）。
        assembled_json: None,
        initial_state_json: None,
    };

    let now = now_ms();
    let mut tx = db.begin().await?;
    let new_world_id = create_world_tx(&mut tx, params).await?;
    let res = sqlx::query(
        "INSERT INTO world_series_instances (series_id, instance_no, world_id, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&series.id)
    .bind(next_no)
    .bind(&new_world_id)
    .bind(now)
    .execute(&mut *tx)
    .await;
    match res {
        Ok(_) => {}
        // 并发抢号：整笔回滚（世界一并消失），调用方重查队列。
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            tx.rollback().await?;
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    }
    tx.commit().await?;

    // 全链审计（§0.2）："哪个系列在什么时候自动开了第几号、复制自哪一号"。actor 记 system——
    // 这是系统按规则开的房，不是某个运营点的按钮，责任链指向系列登记那次运营动作。
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES (?, 'system', 'system', 'world.series_expanded', ?, ?, ?)",
    )
    .bind(new_id("aud"))
    .bind(&new_world_id)
    .bind(format!(
        "series={}|instanceNo={next_no}|clonedFrom={}",
        series.id, series.origin_world_id
    ))
    .bind(now)
    .execute(db)
    .await?;

    Ok(Some(new_world_id))
}

/// 排队分房：确保 `world` 所属系列存在「下一号可入实例」，返回它的 `(worldId, instanceNo)`。
///
/// 返回 `None` 的全部情形（每一种都是"不扩容"的正当理由，调用方一律照常回 `world_full`）：
/// 开关关闭 · 世界未登记进任何系列 · 系列已 `closed` · **已达生效上限** · 并发抢号失败且队列仍无空位。
///
/// 🔴 本函数**不写 `world_members`**，也**不做任何资格判定**——见本节顶部的红线说明。
pub(crate) async fn ensure_next_series_instance(
    db: &AnyPool,
    world: &WorldRow,
) -> Result<Option<(String, i64)>, ApiError> {
    if !series_autoscale_enabled() {
        return Ok(None);
    }
    let Some((series, _self_no)) = load_series_of_world(db, &world.id).await? else {
        return Ok(None);
    };
    if series.status != SERIES_STATUS_ACTIVE {
        return Ok(None);
    }
    // ① 队列里已有可入实例 → 直接指路，**不建新号**（这就是"不重复建"的主路径：
    //    并发满员的 N 个请求里，第一个开出 2 号，其余 N-1 个在这里就命中它了）。
    if let Some(found) = find_open_instance(db, &series.id, &world.id).await? {
        return Ok(Some(found));
    }
    // ② 没有空位 → 开下一号，但先过上限。
    let next_no = max_instance_no(db, &series.id).await? + 1;
    if next_no > series.effective_max_instances() {
        tracing::info!(
            series = %series.id,
            next_no,
            max = series.effective_max_instances(),
            "世界系列已达实例数上限，不再扩容"
        );
        return Ok(None);
    }
    match spawn_next_instance(db, &series, next_no).await? {
        Some(wid) => Ok(Some((wid, next_no))),
        // 抢号失败 = 别人刚开出这一号 → 重查队列（此时它一定是空的，必然命中）。
        None => find_open_instance(db, &series.id, &world.id).await,
    }
}

/// 满员 → 409 `world_full`，并顺带做排队分房（见本节顶部）。
///
/// 错误码保持 `world_full` **前缀**不变：既有客户端按 `contains("world_full")` 匹配（见
/// `src/stores/usePlatformStore.ts`），追加的 `|next={worldId}` 对它们透明。
/// 扩容出任何差错都**不改变"满员"这个事实**：吞掉错误、照回原样 409，绝不把一次分房失败
/// 升级成 500 让玩家连"人满了"都看不到。
async fn world_full_conflict(db: &AnyPool, world: &WorldRow) -> ApiError {
    match ensure_next_series_instance(db, world).await {
        Ok(Some((next_id, _no))) => ApiError::Conflict(format!("world_full|next={next_id}")),
        Ok(None) => ApiError::Conflict("world_full".into()),
        Err(e) => {
            tracing::warn!(world_id = %world.id, error = %e, "世界系列扩容失败，按普通满员返回");
            ApiError::Conflict("world_full".into())
        }
    }
}

/// 世界详情里的 `series` 段（**纯读**）：告诉玩家这是系列的第几号、还有没有别的号可以进。
///
/// 🔴 与 `ensure_next_series_instance` 的关键差别：**本函数绝不建世界**。
/// GET 是幂等读，一次刷新详情页就自动开一个新世界是不可接受的副作用；
/// 新号只在真的有人撞满员（POST join）时才开。
async fn series_view(db: &AnyPool, world: &WorldRow) -> Result<Option<Value>, ApiError> {
    if !series_autoscale_enabled() {
        return Ok(None);
    }
    let Some((series, instance_no)) = load_series_of_world(db, &world.id).await? else {
        return Ok(None);
    };
    let instance_count = max_instance_no(db, &series.id).await?;
    let next_open = find_open_instance(db, &series.id, &world.id).await?;
    let mut view = json!({
        "seriesId": series.id,
        "instanceNo": instance_no,
        "instanceCount": instance_count,
        "maxInstances": series.effective_max_instances(),
        "status": series.status,
    });
    if let Some((wid, no)) = next_open {
        view["nextOpenWorldId"] = json!(wid);
        view["nextOpenInstanceNo"] = json!(no);
    }
    Ok(Some(view))
}

/// 运营诊断用的系列视图（`GET /admin/worlds/{id}/diagnostics` 的 `series` 段）。
///
/// 与玩家侧 `series_view` 的两处差别，都是刻意的：
/// - **不受 env 开关门控**：开关关着时运营更需要看见"这个世界登记在哪个系列、队列多长"，
///   否则急停阀一拉，整条队列在后台就凭空消失了。开关状态另作 `autoscaleEnabled` 字段明示。
/// - 给出 `maxInstancesConfigured`（系列自带上限）与 `maxInstancesEffective`（∧ 全局硬顶后的生效值）
///   两个数：二者不一致即"全局硬顶正在压着这条队列"，运营一眼可辨。
///
/// 纯读，同样绝不建房。
pub(crate) async fn series_admin_view(
    db: &AnyPool,
    world_id: &str,
) -> Result<Option<Value>, ApiError> {
    let Some((series, instance_no)) = load_series_of_world(db, world_id).await? else {
        return Ok(None);
    };
    Ok(Some(json!({
        "seriesId": series.id,
        "originWorldId": series.origin_world_id,
        "instanceNo": instance_no,
        "instanceCount": max_instance_no(db, &series.id).await?,
        "maxInstancesConfigured": series.max_instances,
        "maxInstancesEffective": series.effective_max_instances(),
        "globalCap": series_max_instances_cap(),
        "status": series.status,
        "autoscaleEnabled": series_autoscale_enabled(),
    })))
}

// ===== WORLD-SERIES-REGION-END =====

// ---------- BE 结局传记读取面（总规格 §9；产出侧在 `progression::seal_be_biography_tx`） ----------

/// GET /worlds/{id}/biography：读一个世界的 **BE 结局传记**（崩塌封卷）。
///
/// 「坏结局也是内容，封卷收藏」需要一个读取面，就是这里。传记是**只读汇总**，本端点也只有 SELECT。
///
/// 两层降级（口径同 subplot 的 `ensure_enabled`）：
/// - 运营开关 `MUSE_WORLD_BE_BIOGRAPHY` 未开 → 404（功能不存在，不泄露"平台有这个未开放功能"）；
/// - 世界没有传记（正常终局 / 尚未终局 / 关阀期间崩塌的）→ 404。
///
/// 可见性沿用世界详情那一套：私有世界仅房主/成员可读（`ensure_world_readable`），
/// 官方/公开世界任何登录用户可读——BE 传记本就是给人看的封卷。
async fn world_biography(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !crate::progression::be_biography_enabled() {
        return Err(ApiError::NotFound);
    }
    let world = load_world(&state.db, &id).await?;
    ensure_world_readable(&state.db, &world, &user).await?;

    let row = sqlx::query(
        "SELECT kind, terminal_reason, ending_id, summary_json, sealed_at FROM world_biographies WHERE world_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    let summary_raw: String = row.try_get("summary_json")?;
    Ok(Json(json!({
        "worldId": id,
        "kind": row.try_get::<String, _>("kind")?,
        "terminalReason": row.try_get::<String, _>("terminal_reason")?,
        "endingId": row.try_get::<String, _>("ending_id")?,
        "sealedAt": row.try_get::<i64, _>("sealed_at")?,
        // 解析失败（理论上不可能：写入即 serde 序列化）→ 原样给字符串，绝不吞掉封卷内容。
        "summary": serde_json::from_str::<Value>(&summary_raw).unwrap_or(json!(summary_raw)),
        "aiLabel": { "visible": true },
    })))
}

pub fn router() -> Router<AppState> {
    // 房主建房 POST /worlds 携开房费 charge（P4b），依赖 `ledger`（feature=billing/arena 才装配）；
    // 无经济 feature 时不暴露该端点（GET /worlds 大厅列表恒在）。feature 一致，见 app.rs / ledger 门控。
    #[cfg(any(feature = "billing", feature = "arena"))]
    let worlds_route = get(list_worlds).post(create_room);
    #[cfg(not(any(feature = "billing", feature = "arena")))]
    let worlds_route = get(list_worlds);

    Router::new()
        .route("/worlds", worlds_route)
        .route("/worlds/{id}", get(world_detail))
        .route("/worlds/{id}/join", post(join_world))
        .route("/worlds/{id}/leave", post(leave_world))
        // 封面上传恒在（不随经济 feature 门控）：封面是展示层资产，与账本无关。
        .route("/worlds/{id}/cover", post(upload_cover))
        // BE 结局传记读取面（§9）。开关默认关闭时恒 404。
        .route("/worlds/{id}/biography", get(world_biography))
}

// ---------- 房主建房（POST /worlds）+ 开房费 charge（P4b/P2，feature=billing/arena） ----------

/// 房主建房请求。`templateId` 必填（用哪个模板建房，决定 room_type/版本/开房费/分成对手方）；
/// `visibility` 仅 public/private（official 是运营专属，房主不可自建官方房）；其余留空取默认。
#[cfg(any(feature = "billing", feature = "arena"))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateRoomReq {
    template_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    member_limit: Option<i64>,
    #[serde(default)]
    tick_per_day: Option<i64>,
}

/// POST /worlds：房主建房 + 开房费扣费（单事务，账本红线集中在 ledger::charge）。
///
/// 流程：模板校验（存在 + approved + 未撤回，读 owner/room_type/版本/开房费）→ 幂等 guard →
///   开事务 → `create_world_tx`（先建房，charge 分成溯源需 world 已落库）→
///   `ledger::charge(host, 开房费, "room_open", world_id=Some(新世界))`（分成给模板 owner；
///   自打赏防刷/未成年 owner 挂平台/取整余数归平台/SUM=0 全在 charge 内守；余额不足 409 → tx 回滚零副作用）→ 提交。
///
/// 红线：
/// - 建房**不设年龄硬门**（建房 ≠ 充值；但消费余额只能来自已 age-gate 的充值 → 未成年余额恒 0 →
///   开房费 > 0 时必然余额不足 409；免费房 room_open_price==0 走 charge no-op 仍可建）。
/// - 分成认 **template.owner_id**（创作者），非 worlds.host_user_id（房主）；官方模板 owner NULL → 全额平台。
/// - 免费房（开房费 0）保留：charge no-op 不产 journal。
#[cfg(any(feature = "billing", feature = "arena"))]
async fn create_room(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<CreateRoomReq>,
) -> Result<Json<Value>, ApiError> {
    // 模板校验（读只在 pool 上，先于 tx；释放连接后再 begin，单连接池不自锁）。
    let tpl = sqlx::query(
        "SELECT title, room_type, version, moderation, COALESCE(withdrawn, 0) AS withdrawn, \
         COALESCE(room_open_price_cents, 0) AS price FROM world_templates WHERE id = ?",
    )
    .bind(&body.template_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let moderation: String = tpl.try_get("moderation")?;
    let withdrawn: i64 = tpl.try_get("withdrawn")?;
    if moderation != "approved" {
        return Err(ApiError::Conflict("template_not_approved".into()));
    }
    if withdrawn != 0 {
        return Err(ApiError::Conflict("template_withdrawn".into()));
    }
    let tpl_title: String = tpl.try_get("title")?;
    let room_type: String = tpl.try_get("room_type")?;
    let template_version: i64 = tpl.try_get("version")?;
    let room_open_price: i64 = tpl.try_get("price")?;

    // 房主建房可见性仅 public/private（official 运营专属）。缺省 private。
    let visibility = match body.visibility.as_deref() {
        Some("public") => "public",
        None | Some("private") => "private",
        Some(_) => return Err(ApiError::BadRequest("visibility 仅支持 public/private".into())),
    };
    let title = match &body.title {
        Some(t) if !t.trim().is_empty() => t.clone(),
        _ => tpl_title,
    };
    let member_limit = body.member_limit.unwrap_or(10).clamp(1, 100);
    let tick_per_day = body.tick_per_day.unwrap_or(3).clamp(1, 100);

    // 幂等：同 key 同载荷 → 缓存返回（不双扣不双建）。
    let idem_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&body).unwrap_or_default());
    let guard = idempotency::guard(&state.db, &user.user_id, "worlds.create", idem_key, &payload_hash).await?;
    if let Some(cached) = &guard.cached_response {
        return Ok(Json(serde_json::from_str(cached).unwrap_or(json!({}))));
    }

    let params = CreateWorldParams {
        template_id: body.template_id.clone(),
        template_version,
        room_type: room_type.clone(),
        title,
        visibility: visibility.into(),
        host_user_id: Some(user.user_id.clone()),
        member_limit,
        tick_per_day,
        // 房主房沿用保守默认预算（B-2：非零 token/cny 上限，避免成本失控）。
        daily_token_budget: 200_000,
        daily_cny_budget_cents: 2_000,
        status: None,
        timeline_mode: "interval".into(),
        // 房主建房暂不开放契约档选择：自定义房沿用同意制（规格 §11 的生死状先在官方场验证，
        // 玩家自建生死场需先有房主侧的明示/确认/未成年门 UI 与运营审核，属后续波次）。
        lethality: LETHALITY_CONSENT.into(),
        engine_version: None,
        prompt_set_version: None,
        model_route_version: None,
        assembled_json: None,
        initial_state_json: None,
    };

    // 单事务：建房 + 开房费 charge 原子。先建房（charge 溯源分成需 world 已在 tx 内），再 charge。
    let mut tx = state.db.begin().await?;
    let world_id = create_world_tx(&mut tx, params).await?;
    let receipt = crate::ledger::charge(
        &mut tx,
        &user.user_id,
        room_open_price,
        "room_open",
        "world",
        &world_id,
        Some(&world_id),
    )
    .await?;
    tx.commit().await?;

    let resp = json!({
        "worldId": world_id,
        "templateId": body.template_id,
        "roomType": room_type,
        "visibility": visibility,
        "hostUserId": user.user_id,
        "roomOpenPriceCents": room_open_price,
        // 开房费分账明细（诚实标注）：创作者分成 + 平台抽成（自打赏/官方模板/未成年 owner → 创作者 0）。
        "charge": {
            "chargedCents": receipt.charged_cents,
            "creatorEarningsCents": receipt.creator_earnings_cents,
            "platformRevenueCents": receipt.platform_revenue_cents,
        },
    });
    guard.store_response(&state.db, &resp.to_string()).await?;
    Ok(Json(resp))
}
