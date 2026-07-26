//! 已过审内容的事后处置：**再审 / 下架 / 恢复**（migration 0044）。
//!
//! ## 补的是哪个缺口
//!
//! `audit_queue` 的 approve/reject 只作用于**仍在人审队列里**的条目。角色卡、立绘、世界封面、
//! 世界模板一旦过审就再也拿不下来——而举报队列（`social/`）处理的恰恰是「**已经在线上的内容
//! 出了问题**」。它的跳转清单里一度如实写着这是一处诚实空缺，本模块是那处空缺的实现。
//!
//! 这既是内容安全的闭环，也是合规主体责任要求的处置能力：平台必须能对已发布内容做事后处置。
//!
//! ## 🔴 下架的是「展示」，不是「已发生的世界事实」
//!
//! §0.3 公共事实不可回滚是平台红线。一张角色卡被下架，**不意味着它参演过的世界事实要被抹掉**——
//! 那些 `world_events` 已经落定。所以本模块的写入面只有四处，全部是**展示态列**：
//!
//! | kind | 表 | 展示态列 | 该列既有的读取面闸门 |
//! |---|---|---|---|
//! | `character` | `cloud_characters` | `moderation` | `worlds::join_world` 非 approved → 409；`invitations` 接受邀请同判 |
//! | `character_avatar` | `cloud_characters` | `avatar_moderation` | `CharacterView` / world roster / backpack 三处「仅 approved 才下发 avatarUrl」 |
//! | `world_cover` | `worlds` | `cover_moderation` | `worlds::visible_cover_url`（大厅 / 世界详情 / 后台世界列表共用的唯一闸门） |
//! | `world_template` | `world_templates` | `moderation` | `worlds::create_room` 非 approved → 409；`assembly` 蓝图解引用同判 |
//!
//! 加上处置台账 `content_takedowns`、审计 `audit_logs`、风控 `risk_events`、人审队列 `audit_queue`
//! ——**七张表，一张世界线表都不在其中**。`world_events` / `world_ticks` / `world_members` /
//! `world_contributions` / `world_biographies` 一个字节都不动，由
//! `tests::takedown_and_restore_leave_worldline_byte_identical`（逐字节快照）与
//! `tests::red_line_module_never_writes_worldline_tables`（源码级）双重守死。
//!
//! ## 为什么写既有的 `moderation` 列，而不是加一列新的过滤条件
//!
//! **失效安全**。上面四个闸门判的都是「等于 `approved`」，因此把列置成任何非 approved 值都会
//! 自动关闭它们——不需要逐个读取面补 `WHERE` 条件，也就不存在「漏改一处 = 下架静默失效」的风险面。
//! 反过来若新增一列 `taken_down`，每一处读取面都得记得 `AND taken_down = 0`，而**漏掉的那一处
//! 就是下架无效的那一处**，且不会有任何报错告诉你。
//!
//! 下架写入的哨兵值是 [`TAKEDOWN`]（`'takedown'`，不是 `'rejected'`）：`'rejected'` 是**发布时**
//! 被驳回的语义，`admin_api::audit::resolve_appeal` 的改判路径只翻转处于 `'rejected'` 的维度，
//! 复用它会让下架被申诉流程悄悄改判掉，且从此分不清「从未过审」与「过审后被下架」。
//!
//! ## 可逆性：两档，权限台阶不同
//!
//! 一个**不可逆**的下架按钮在误操作时是灾难；一个**可逆**的下架在被要求永久移除时又不够。
//! 因此两档并存，由 `permanent` 参数选择：
//!
//! - `restricted`（默认）：可恢复。`prev_moderation` 记住下架前的值，恢复即写回。
//!   下与恢复都是 `reviewer`——误操作的自愈成本必须低于犯错成本。
//! - `removed`：不可恢复，`restore` 恒 409。**admin 专属**（不可逆动作要更高的门槛），
//!   且位图主体（立绘 / 封面）连对象存储字节一并删除——被要求删除时只改标志位不算删除。
//!   🔴 文本主体（`card_json` / `skeleton_json`）**不删字节**：运行中的世界仍引用那份不可变
//!   快照，删了会让运行中的世界崩掉。这是「下架展示面 ≠ 抹掉世界事实」的同一条边界。
//!
//! ## 运行中的世界怎么办：**现状与选项，本模块不替产品做决定**
//!
//! 实测现状（非推测）：`cloud_characters.moderation` 是**入场时**的闸（`worlds/mod.rs` join），
//! 一旦 `world_members` 行存在，`assembly::load_active_cards` / `runtime::process_tick_inner` /
//! world roster / 世界传记都**不再复查** `moderation` 就直接读 `card_json`。因此：
//!
//! - 下架**不会**让运行中的世界崩掉（引擎输入不变，拍照常跑）；
//! - 下架也**不会**把这张卡从运行中的世界里赶出去（它继续演）。
//!
//! 本模块保持该现状，并在处置回执里把「这张卡当前在哪些运行中的世界」直接列出来
//! （`affectedRunningWorlds`），让运营立刻能走既有入口。可选路线，按后果从轻到重：
//!
//! | 选项 | 做法 | 代价 |
//! |---|---|---|
//! | (a) 现状 | 只断新入场与展示面 | 违规卡在存量世界里继续产出 |
//! | (b) 暂停世界 | 既有 `POST /admin/worlds/{id}/pause` | 整局停摆，牵连同局其他玩家 |
//! | (c) 出场降级 | 引擎输入处加「成员卡非 approved 则本拍不出场」 | 改运行中世界的叙事，需产品拍板 |
//! | (d) 强制离场 | 改 `world_members.status` | **动世界线相关表**，需红线评审 |
//!
//! (c)(d) 都不是本模块能自作主张的事，故一行都不写。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::db::{new_id, now_ms, Placeholders};
use crate::error::ApiError;

use super::{audit_tx, clamp_limit, parse_cursor, require_role};

// ═══════════════════════════════════════════════════════════════════════════
// 常量与主体白名单
// ═══════════════════════════════════════════════════════════════════════════

/// 展示态列被下架时写入的哨兵值。**必须是非 `'approved'` 值**——四个读取面闸门判的都是
/// 「等于 approved」，本模块的全部拦截力都由这一条不变式提供。
pub(super) const TAKEDOWN: &str = "takedown";

/// 处置台账 `state` 的三个取值。
const STATE_RESTRICTED: &str = "restricted";
const STATE_REMOVED: &str = "removed";
const STATE_RESTORED: &str = "restored";

/// 只有当前展示态是它的主体才谈得上「下架」（本模块补的是**已过审**内容的入口）。
const APPROVED: &str = "approved";

/// 处置理由长度上限（口径同申诉复审 / 举报处置）。
const REASON_MAX_CHARS: usize = 500;

/// 回执里列出的受影响运行中世界条数上限（总数另给 `affectedRunningWorldCount`，不截断计数）。
const AFFECTED_WORLDS_LIMIT: i64 = 50;

/// 风控留痕 kind（`risk_events.kind`）。处置动作与机审判定分开计，否则风控面上的机审命中率会失真。
const DISPOSAL_RISK_KIND: &str = "content_disposal";

/// 一类可处置主体的静态描述。
///
/// 🔴 `table` / `moderation_column` / `object_key_column` 会被 `format!` 拼进 SQL——它们
/// **只能**来自本文件的 [`SUBJECTS`] 常量数组，绝不接受任何请求字段。路径上的 `kind` 先经
/// [`spec_of`] 在白名单里查表，查不到直接 400，因此请求内容永远不会到达 SQL 文本里。
struct SubjectSpec {
    kind: &'static str,
    label: &'static str,
    table: &'static str,
    moderation_column: &'static str,
    /// 位图主体的对象键列；永久移除时据此删除字节。文本主体为 `None`（不删字节，见模块头）。
    object_key_column: Option<&'static str>,
    /// 该主体送人审队列时用的 `audit_queue.subject_kind`。
    ///
    /// 🔴 必须是 `admin_api::audit::review` 的 `match subject_kind` **认识**的值，否则人审
    /// 点了通过/驳回也无处回写主体，等于制造一条永远悬空的队列项（0027 迁移注释里记着封面
    /// 正是因为这个原因至今不入队）。位图主体为 `None` = 没有文本再审通道，见 [`recheck`]。
    recheck_queue_kind: Option<&'static str>,
}

const SUBJECTS: &[SubjectSpec] = &[
    SubjectSpec {
        kind: "character",
        label: "角色卡",
        table: "cloud_characters",
        moderation_column: "moderation",
        object_key_column: None,
        recheck_queue_kind: Some("character"),
    },
    SubjectSpec {
        kind: "character_avatar",
        label: "角色立绘",
        table: "cloud_characters",
        moderation_column: "avatar_moderation",
        object_key_column: Some("avatar_object_key"),
        // 立绘是位图：`ModerationProvider::check_text` 对它无意义，没有文本再审通道。
        recheck_queue_kind: None,
    },
    SubjectSpec {
        kind: "world_cover",
        label: "世界封面",
        table: "worlds",
        moderation_column: "cover_moderation",
        object_key_column: Some("cover_object_key"),
        recheck_queue_kind: None,
    },
    SubjectSpec {
        kind: "world_template",
        label: "世界模板",
        table: "world_templates",
        moderation_column: "moderation",
        object_key_column: None,
        // 创作者资产与官方模板同落 world_templates，audit::review 两个字面量都认。
        recheck_queue_kind: Some("world_template"),
    },
];

fn spec_of(kind: &str) -> Result<&'static SubjectSpec, ApiError> {
    SUBJECTS.iter().find(|s| s.kind == kind).ok_or_else(|| {
        let known: Vec<&str> = SUBJECTS.iter().map(|s| s.kind).collect();
        ApiError::BadRequest(format!("未知处置主体 {kind:?}；支持：{}", known.join(" / ")))
    })
}

/// 处置理由：必填、trim 后 1..=500 字符。理由不是可选的——没有理由的处置无法复盘。
fn validate_reason(raw: &str) -> Result<String, ApiError> {
    let reason = raw.trim().to_string();
    let n = reason.chars().count();
    if n == 0 || n > REASON_MAX_CHARS {
        return Err(ApiError::BadRequest(format!("处置理由必填且不超过 {REASON_MAX_CHARS} 字符")));
    }
    Ok(reason)
}

// ═══════════════════════════════════════════════════════════════════════════
// 请求 / 响应
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DisposalReq {
    #[serde(default)]
    reason: String,
    /// `true` = 永久移除（不可恢复，admin 专属）。缺省 / `false` = 可恢复下架。
    #[serde(default)]
    permanent: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReasonOnlyReq {
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct LedgerQuery {
    state: Option<String>,
    kind: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// 台账行 → camelCase JSON（列表与单主体查询共用同一形状）。
fn takedown_json(row: &sqlx::any::AnyRow) -> Result<Value, ApiError> {
    let state: String = row.try_get("state")?;
    Ok(json!({
        "id": row.try_get::<String, _>("id")?,
        "subjectKind": row.try_get::<String, _>("subject_kind")?,
        "subjectId": row.try_get::<String, _>("subject_id")?,
        "state": state,
        // 可恢复性是**派生**而非独立字段：只有 restricted 能恢复，前端不必自己记规则。
        "reversible": state == STATE_RESTRICTED,
        "prevModeration": row.try_get::<String, _>("prev_moderation")?,
        "reason": row.try_get::<String, _>("reason")?,
        "actorId": row.try_get::<String, _>("actor_id")?,
        "actorRole": row.try_get::<String, _>("actor_role")?,
        "bytesPurged": row.try_get::<i64, _>("bytes_purged")? != 0,
        "createdAt": row.try_get::<i64, _>("created_at")?,
        "restoredAt": row.try_get::<Option<i64>, _>("restored_at")?,
        "restoredBy": row.try_get::<Option<String>, _>("restored_by")?,
        "restoreReason": row.try_get::<Option<String>, _>("restore_reason")?,
    }))
}

const TAKEDOWN_COLUMNS: &str = "id, subject_kind, subject_id, state, prev_moderation, reason, \
     actor_id, actor_role, bytes_purged, created_at, restored_at, restored_by, restore_reason";

async fn fetch_takedown(
    db: &sqlx::AnyPool,
    kind: &str,
    subject_id: &str,
) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {TAKEDOWN_COLUMNS} FROM content_takedowns WHERE subject_kind = $1 AND subject_id = $2"
    ))
    .bind(kind)
    .bind(subject_id)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(takedown_json).transpose()
}

// ═══════════════════════════════════════════════════════════════════════════
// 主体状态读取
// ═══════════════════════════════════════════════════════════════════════════

/// 读主体当前展示态。行不存在 → `None`；行在但列为 NULL（无立绘 / 无封面）→ `Some(None)`。
///
/// 两者必须分开：行不存在是 404（主体不存在），列为 NULL 是 409（该主体没有这项资产可下架）。
async fn current_moderation(
    db: &sqlx::AnyPool,
    spec: &SubjectSpec,
    id: &str,
) -> Result<Option<Option<String>>, ApiError> {
    // 表名/列名来自 SUBJECTS 静态白名单（见 SubjectSpec 注释），主体 id 走 $1 绑定。
    let row = sqlx::query(&format!(
        "SELECT {col} AS moderation FROM {table} WHERE id = $1",
        col = spec.moderation_column,
        table = spec.table
    ))
    .bind(id)
    .fetch_optional(db)
    .await?;
    match row {
        None => Ok(None),
        Some(r) => Ok(Some(r.try_get::<Option<String>, _>("moderation")?)),
    }
}

/// 主体归属：`character*` → owner 用户 id；`world_cover` → 世界 id。用于风控留痕的维度。
/// 模板无用户维度（官方/创作者两种来源，`world_templates` 无 owner 列）→ 两维皆空。
async fn subject_dimensions(
    db: &sqlx::AnyPool,
    spec: &SubjectSpec,
    id: &str,
) -> Result<(Option<String>, Option<String>), ApiError> {
    match spec.kind {
        "character" | "character_avatar" => {
            let owner: Option<String> =
                sqlx::query_scalar("SELECT owner_id FROM cloud_characters WHERE id = $1")
                    .bind(id)
                    .fetch_optional(db)
                    .await?;
            Ok((owner, None))
        }
        "world_cover" => Ok((None, Some(id.to_string()))),
        _ => Ok((None, None)),
    }
}

/// 这次处置会影响到哪些**运行中**的世界（现状陈述，不是处置动作——见模块头「运行中的世界怎么办」）。
///
/// 返回 `(总数, 前 N 条明细)`。总数走独立 `COUNT(*)`，不由明细条数推断——截断后的条数会被读成
/// 「只影响 50 个世界」，而那正是运营需要据以决定是否暂停世界的数字。
async fn affected_running_worlds(
    db: &sqlx::AnyPool,
    spec: &SubjectSpec,
    id: &str,
) -> Result<(i64, Vec<Value>), ApiError> {
    // 各主体到「运行中世界」的连法不同，但两条 SQL 的形状一致：一条 COUNT，一条全序 LIMIT。
    let (count_sql, list_sql) = match spec.kind {
        "character" | "character_avatar" => (
            "SELECT CAST(COUNT(*) AS BIGINT) FROM world_members wm JOIN worlds w ON w.id = wm.world_id \
             WHERE wm.cloud_character_id = $1 AND wm.status = 'active' AND w.status IN ('open', 'running')",
            "SELECT w.id AS id, w.title AS title, w.status AS status FROM world_members wm \
             JOIN worlds w ON w.id = wm.world_id \
             WHERE wm.cloud_character_id = $1 AND wm.status = 'active' AND w.status IN ('open', 'running') \
             ORDER BY w.created_at DESC, w.id DESC LIMIT $2",
        ),
        "world_cover" => (
            "SELECT CAST(COUNT(*) AS BIGINT) FROM worlds WHERE id = $1 AND status IN ('open', 'running')",
            "SELECT id AS id, title AS title, status AS status FROM worlds \
             WHERE id = $1 AND status IN ('open', 'running') ORDER BY created_at DESC, id DESC LIMIT $2",
        ),
        _ => (
            "SELECT CAST(COUNT(*) AS BIGINT) FROM worlds WHERE template_id = $1 AND status IN ('open', 'running')",
            "SELECT id AS id, title AS title, status AS status FROM worlds \
             WHERE template_id = $1 AND status IN ('open', 'running') \
             ORDER BY created_at DESC, id DESC LIMIT $2",
        ),
    };

    let total: i64 = sqlx::query_scalar(count_sql).bind(id).fetch_one(db).await?;
    let rows = sqlx::query(list_sql).bind(id).bind(AFFECTED_WORLDS_LIMIT).fetch_all(db).await?;
    let items = rows
        .iter()
        .map(|r| {
            Ok(json!({
                "id": r.try_get::<String, _>("id")?,
                "title": r.try_get::<String, _>("title")?,
                "status": r.try_get::<String, _>("status")?,
            }))
        })
        .collect::<Result<Vec<Value>, ApiError>>()?;
    Ok((total, items))
}

/// 处置回执统一附带的口径自述。写进响应体而不是只写文档：运营在做处置的那一刻要看得到边界。
fn disposal_notes(affected: i64) -> Vec<&'static str> {
    let mut notes = vec![
        "下架作用于展示面（该主体的审核态列），已落定的世界事实（world_events / world_ticks / \
         world_members / world_contributions / world_biographies）一个字节都不改。",
        "作用效果：不再可被选入新世界、不再随读取面下发；已在进行中的世界不受影响。",
    ];
    if affected > 0 {
        notes.push(
            "🔴 该主体当前仍在运行中的世界里：入场闸只在入场时判一次，因此它会继续参演。\
             要立即中止，走既有入口 POST /admin/worlds/{id}/pause 暂停对应世界（本端点不代做——\
             强制离场会改动世界线相关表，需红线评审）。",
        );
    }
    notes
}

// ═══════════════════════════════════════════════════════════════════════════
// GET：处置台账 / 单主体处置态
// ═══════════════════════════════════════════════════════════════════════════

/// GET /admin/content/takedowns?state=&kind=&cursor=&limit=：处置台账（复合游标全序翻页）。
pub(super) async fn list_takedowns(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<LedgerQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let page = clamp_limit(q.limit);

    let state_filter = q.state.unwrap_or_else(|| "all".into());
    if !matches!(state_filter.as_str(), STATE_RESTRICTED | STATE_REMOVED | STATE_RESTORED | "all") {
        return Err(ApiError::BadRequest(
            "state 仅支持 restricted / removed / restored / all".into(),
        ));
    }
    if let Some(k) = q.kind.as_deref() {
        // 未知 kind 直接 400 而不是静默返回空列表：空列表会被读成「这类内容没有被处置过」。
        spec_of(k)?;
    }

    // 发号顺序 = 下面 bind 的顺序；筛选段与游标段出不出现要到运行时才知道，编号不能写死。
    let mut ph = Placeholders::new();
    let mut sql = format!("SELECT {TAKEDOWN_COLUMNS} FROM content_takedowns WHERE 1 = 1");
    if state_filter != "all" {
        sql.push_str(&format!(" AND state = {}", ph.take()));
    }
    if q.kind.is_some() {
        sql.push_str(&format!(" AND subject_kind = {}", ph.take()));
    }
    // 🔴 复合游标：`created_at` 单列会在同毫秒并列横跨页边界时**整组丢行**（见 pagination.rs）。
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(&format!(
            " AND (created_at < {} OR (created_at = {} AND id < {}))",
            ph.take(),
            ph.take(),
            ph.take()
        ));
    }
    // ORDER BY 全序：次级键 id 保证并列行有确定次序，游标才切得开。
    sql.push_str(&format!(" ORDER BY created_at DESC, id DESC LIMIT {}", ph.take()));

    let mut query = sqlx::query(&sql);
    if state_filter != "all" {
        query = query.bind(&state_filter);
    }
    if let Some(k) = q.kind.as_deref() {
        query = query.bind(k);
    }
    if let Some((ts, id)) = &cursor {
        query = query.bind(*ts).bind(*ts).bind(id);
    }
    query = query.bind(page + 1);

    let rows = query.fetch_all(&state.db).await?;
    let has_more = rows.len() as i64 > page;
    let mut items = Vec::new();
    let mut next_cursor: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        if i as i64 >= page {
            break;
        }
        let id: String = row.try_get("id")?;
        let created_at: i64 = row.try_get("created_at")?;
        next_cursor = Some(format!("{created_at}:{id}"));
        items.push(takedown_json(row)?);
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "items": items, "nextCursor": next_cursor })))
}

/// GET /admin/content/{kind}/{id}：单主体处置态（举报队列跳转过来的落地查询）。
///
/// 主体不存在 → 404。从未被处置过 → `takedown: null`（这是真实答案，不是缺数据）。
pub(super) async fn subject_status(
    State(state): State<AppState>,
    admin: AdminUser,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let spec = spec_of(&kind)?;
    let moderation = current_moderation(&state.db, spec, &id).await?.ok_or(ApiError::NotFound)?;
    let (affected_count, affected) = affected_running_worlds(&state.db, spec, &id).await?;

    Ok(Json(json!({
        "subjectKind": spec.kind,
        "subjectLabel": spec.label,
        "subjectId": id,
        "moderation": moderation,
        "takenDown": moderation.as_deref() == Some(TAKEDOWN),
        // 只有当前处于 approved 的主体谈得上「下架」——本模块补的是已过审内容的入口。
        "canTakedown": moderation.as_deref() == Some(APPROVED),
        "canRecheck": spec.recheck_queue_kind.is_some(),
        "takedown": fetch_takedown(&state.db, spec.kind, &id).await?.unwrap_or(Value::Null),
        "affectedRunningWorldCount": affected_count,
        "affectedRunningWorlds": affected,
        "worldlineUntouched": true,
        "notes": disposal_notes(affected_count),
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST：下架
// ═══════════════════════════════════════════════════════════════════════════

/// POST /admin/content/{kind}/{id}/takedown body {reason, permanent?}：下架已过审内容。
///
/// 权限台阶（走既有 `require_role` 矩阵，**不新造角色**）：
/// - 可恢复下架 → `reviewer`（与 audit-queue 的 approve/reject 同一档，都是内容裁决）；
/// - 永久移除 → **admin 专属**（`require_role(&admin, &[])`，口径同治理写操作）。不可逆的动作
///   必须比可逆的动作更难触发，否则「两档」在实践中会退化成一档。
///
/// 允许的状态迁移（其余一律 409，附当前态）：
/// - `approved` → `restricted` / `removed`；
/// - `restricted` → `removed`（**升级为永久**；`prev_moderation` 保持首次下架时记下的值，
///   否则升级后恢复点会变成 `'takedown'` 自身）。
///
/// 降级（`removed` → `restricted`）不提供：那等于给不可逆开一条后门。
pub(super) async fn takedown(
    State(state): State<AppState>,
    admin: AdminUser,
    Path((kind, id)): Path<(String, String)>,
    Json(req): Json<DisposalReq>,
) -> Result<Json<Value>, ApiError> {
    let spec = spec_of(&kind)?;
    if req.permanent {
        require_role(&admin, &[])?; // 不可逆处置 admin 专属。
    } else {
        require_role(&admin, &["reviewer"])?;
    }
    let reason = validate_reason(&req.reason)?;

    let cur = current_moderation(&state.db, spec, &id).await?.ok_or(ApiError::NotFound)?;
    let cur = cur.ok_or_else(|| {
        ApiError::Conflict(format!("该主体没有可下架的{}（审核态为空）", spec.label))
    })?;

    let existing = fetch_takedown(&state.db, spec.kind, &id).await?;
    let existing_state = existing.as_ref().and_then(|v| v["state"].as_str()).unwrap_or("");
    let new_state = if req.permanent { STATE_REMOVED } else { STATE_RESTRICTED };

    // 状态机：approved 是唯一的正常入口；takedown→removed 的升级是唯一的例外。
    let prev_moderation = if cur == APPROVED {
        APPROVED.to_string()
    } else if cur == TAKEDOWN && existing_state == STATE_RESTRICTED && req.permanent {
        existing
            .as_ref()
            .and_then(|v| v["prevModeration"].as_str())
            .unwrap_or(APPROVED)
            .to_string()
    } else if cur == TAKEDOWN {
        return Err(ApiError::Conflict(format!(
            "该{}已处于下架状态（{}）",
            spec.label,
            if existing_state.is_empty() { TAKEDOWN } else { existing_state }
        )));
    } else {
        return Err(ApiError::Conflict(format!(
            "仅可下架已过审内容，该{}当前审核态为 {cur:?}",
            spec.label
        )));
    };

    let (user_dim, world_dim) = subject_dimensions(&state.db, spec, &id).await?;
    let (affected_count, affected) = affected_running_worlds(&state.db, spec, &id).await?;
    let now = now_ms();

    let mut tx = state.db.begin().await?;
    // CAS：以读到的当前值为条件写，命中 0 行 = 期间被别人改过 → 409，不覆盖别人的处置。
    let res = sqlx::query(&format!(
        "UPDATE {table} SET {col} = $1 WHERE id = $2 AND {col} = $3",
        table = spec.table,
        col = spec.moderation_column
    ))
    .bind(TAKEDOWN)
    .bind(&id)
    .bind(&cur)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(ApiError::Conflict("该主体审核态已被并发修改，请刷新后重试".into()));
    }

    // 台账 upsert：每主体只留当前一行；完整历史在 audit_logs / risk_events。
    // ON CONFLICT + excluded 两个库都支持（PG 原生；SQLite 3.24+ upsert）。
    sqlx::query(
        "INSERT INTO content_takedowns \
         (id, subject_kind, subject_id, state, prev_moderation, reason, actor_id, actor_role, \
          bytes_purged, created_at, restored_at, restored_by, restore_reason) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, $9, NULL, NULL, NULL) \
         ON CONFLICT (subject_kind, subject_id) DO UPDATE SET \
           state = excluded.state, prev_moderation = excluded.prev_moderation, \
           reason = excluded.reason, actor_id = excluded.actor_id, actor_role = excluded.actor_role, \
           bytes_purged = excluded.bytes_purged, created_at = excluded.created_at, \
           restored_at = NULL, restored_by = NULL, restore_reason = NULL",
    )
    .bind(new_id("ctd"))
    .bind(spec.kind)
    .bind(&id)
    .bind(new_state)
    .bind(&prev_moderation)
    .bind(&reason)
    .bind(&admin.0.user_id)
    .bind(&admin.0.role)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    let action = if req.permanent { "content.takedown_permanent" } else { "content.takedown" };
    audit_tx(&mut tx, &admin.0, action, &format!("{}:{}", spec.kind, id), &reason).await?;
    crate::safety::record_risk_tx(
        &mut tx,
        user_dim.as_deref(),
        world_dim.as_deref(),
        DISPOSAL_RISK_KIND,
        json!({
            "action": action,
            "subjectKind": spec.kind,
            "subjectId": id,
            "state": new_state,
            "prevModeration": prev_moderation,
            "actorId": admin.0.user_id,
            "actorRole": admin.0.role,
            "reason": reason,
            "affectedRunningWorldCount": affected_count,
        }),
    )
    .await?;
    tx.commit().await?;

    // 位图主体的永久移除：删除对象存储字节。
    // 🔴 **在事务提交之后**做：文件删除回滚不了，放进事务里一旦回滚就会留下「库里没下架、
    // 字节已经没了」的错位。反过来（已提交但删字节失败）只是留了一份读取面到不了的孤儿字节，
    // 由回执 bytesPurged=false 如实告知，运营可重试——两种失败模式里这一种明显更轻。
    let mut bytes_purged = false;
    if req.permanent {
        if let Some(key_col) = spec.object_key_column {
            let key: Option<Option<String>> = sqlx::query_scalar(&format!(
                "SELECT {key_col} FROM {table} WHERE id = $1",
                table = spec.table
            ))
            .bind(&id)
            .fetch_optional(&state.db)
            .await?;
            if let Some(key) = key.flatten().filter(|k| !k.trim().is_empty()) {
                match state.objects.delete(&key) {
                    Ok(()) => {
                        bytes_purged = true;
                        sqlx::query(
                            "UPDATE content_takedowns SET bytes_purged = 1 \
                             WHERE subject_kind = $1 AND subject_id = $2",
                        )
                        .bind(spec.kind)
                        .bind(&id)
                        .execute(&state.db)
                        .await?;
                    }
                    Err(e) => {
                        // 不 500：展示面已经关了（下架已生效），字节没删干净是可重试的次要失败。
                        tracing::warn!(kind = spec.kind, subject = %id, error = %e, "永久移除时删除对象字节失败");
                    }
                }
            }
        }
    }

    Ok(Json(json!({
        "subjectKind": spec.kind,
        "subjectLabel": spec.label,
        "subjectId": id,
        "state": new_state,
        "moderation": TAKEDOWN,
        "prevModeration": prev_moderation,
        "reversible": new_state == STATE_RESTRICTED,
        "bytesPurged": bytes_purged,
        "createdAt": now,
        "affectedRunningWorldCount": affected_count,
        "affectedRunningWorlds": affected,
        // 回执明说世界线没动（口径同 OOC 复核回执的 worldlineChanged:false）。
        "worldlineUntouched": true,
        "notes": disposal_notes(affected_count),
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST：恢复
// ═══════════════════════════════════════════════════════════════════════════

/// POST /admin/content/{kind}/{id}/restore body {reason}：恢复可恢复下架的内容。
///
/// `reviewer`——与下架同一档：误操作的自愈成本必须低于犯错成本，否则运营会因为「下了就撤不回」
/// 而不敢用下架，那才是真正的内容安全风险。
///
/// 恢复写回的是台账里的 `prev_moderation`，不是常量 `'approved'`：恢复的语义是「回到下架前的
/// 那个状态」，而不是「顺手放行」。`removed` 恒 409（不可逆就是不可逆）。
pub(super) async fn restore(
    State(state): State<AppState>,
    admin: AdminUser,
    Path((kind, id)): Path<(String, String)>,
    Json(req): Json<ReasonOnlyReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let spec = spec_of(&kind)?;
    let reason = validate_reason(&req.reason)?;

    let row = fetch_takedown(&state.db, spec.kind, &id)
        .await?
        .ok_or_else(|| ApiError::Conflict(format!("该{}没有处置记录，无从恢复", spec.label)))?;
    match row["state"].as_str().unwrap_or("") {
        STATE_RESTRICTED => {}
        STATE_REMOVED => {
            return Err(ApiError::Conflict(
                "该内容已被永久移除，不可恢复（永久移除是不可逆处置；如需重新上线请由作者重新发布）"
                    .into(),
            ))
        }
        _ => return Err(ApiError::Conflict(format!("该{}当前不处于下架状态", spec.label))),
    }
    let prev = row["prevModeration"].as_str().unwrap_or(APPROVED).to_string();

    let (user_dim, world_dim) = subject_dimensions(&state.db, spec, &id).await?;
    let now = now_ms();

    let mut tx = state.db.begin().await?;
    // CAS 到 'takedown'：若期间人审在队列里把它判成了 rejected，这里命中 0 行 → 409，
    // 恢复不会把一个刚被驳回的主体重新点亮。
    let res = sqlx::query(&format!(
        "UPDATE {table} SET {col} = $1 WHERE id = $2 AND {col} = $3",
        table = spec.table,
        col = spec.moderation_column
    ))
    .bind(&prev)
    .bind(&id)
    .bind(TAKEDOWN)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(ApiError::Conflict("该主体审核态已被并发修改，请刷新后重试".into()));
    }

    let res = sqlx::query(
        "UPDATE content_takedowns SET state = $1, restored_at = $2, restored_by = $3, \
         restore_reason = $4 WHERE subject_kind = $5 AND subject_id = $6 AND state = $7",
    )
    .bind(STATE_RESTORED)
    .bind(now)
    .bind(&admin.0.user_id)
    .bind(&reason)
    .bind(spec.kind)
    .bind(&id)
    .bind(STATE_RESTRICTED)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(ApiError::Conflict("该处置记录已被并发修改，请刷新后重试".into()));
    }

    audit_tx(&mut tx, &admin.0, "content.restore", &format!("{}:{}", spec.kind, id), &reason)
        .await?;
    crate::safety::record_risk_tx(
        &mut tx,
        user_dim.as_deref(),
        world_dim.as_deref(),
        DISPOSAL_RISK_KIND,
        json!({
            "action": "content.restore",
            "subjectKind": spec.kind,
            "subjectId": id,
            "state": STATE_RESTORED,
            "restoredTo": prev,
            "actorId": admin.0.user_id,
            "actorRole": admin.0.role,
            "reason": reason,
        }),
    )
    .await?;
    tx.commit().await?;

    Ok(Json(json!({
        "subjectKind": spec.kind,
        "subjectLabel": spec.label,
        "subjectId": id,
        "state": STATE_RESTORED,
        "moderation": prev,
        "restoredAt": now,
        "worldlineUntouched": true,
    })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST：再审
// ═══════════════════════════════════════════════════════════════════════════

/// POST /admin/content/{kind}/{id}/recheck body {reason}：把已过审内容送回人审队列。
///
/// **不改展示态**——再审期间内容照常在线。要立刻断掉展示走 `takedown`，两个动作正交：
/// 「需要人再看一眼」与「先拿下来」是两个不同的判断，捆在一起会逼运营在证据不足时二选一。
///
/// 入队本身走 `safety::queue_operator_recheck`（风控留痕的三条入口之一），本模块**不直接**
/// INSERT `audit_queue` / `risk_events`。随后人审在既有工作台上 approve/reject，裁决经
/// `audit::review` 回写主体 moderation——闭环由既有路径完成，不另造一套。
///
/// 前置条件是当前展示态 `approved`：
/// - 已下架的主体不允许再审。原因是 `audit::review` 的 approve 分支会把主体写成 `'approved'`，
///   那会**绕过 `restore` 的可逆性台阶**把下架悄悄撤销掉。（另有一道保险在 `audit::review`
///   里：approve 回写带 `AND moderation <> 'takedown'` 守卫。前置条件与守卫是同一件事的两道锁。）
/// - 位图主体（立绘 / 封面）没有文本再审通道：`ModerationProvider::check_text` 对图片无意义，
///   而 `check_image` 至今没有人审入队路径（0027 迁移注释里记着封面因此不入队）。
///   对它们只提供下架，且如实报 400 而不是假装排了队。
pub(super) async fn recheck(
    State(state): State<AppState>,
    admin: AdminUser,
    Path((kind, id)): Path<(String, String)>,
    Json(req): Json<ReasonOnlyReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let spec = spec_of(&kind)?;
    let reason = validate_reason(&req.reason)?;

    let queue_kind = spec.recheck_queue_kind.ok_or_else(|| {
        ApiError::BadRequest(format!(
            "{}是位图资产，暂无文本再审通道（图片机审 check_image 尚无人审入队路径）；\
             如需处置请直接下架",
            spec.label
        ))
    })?;

    let cur = current_moderation(&state.db, spec, &id).await?.ok_or(ApiError::NotFound)?;
    let cur = cur.unwrap_or_default();
    if cur != APPROVED {
        return Err(ApiError::Conflict(format!(
            "仅可对已过审内容发起再审，该{}当前审核态为 {cur:?}",
            spec.label
        )));
    }

    // 送审文本必须与**发布时机审看的那一段逐字一致**，否则两次机审看的不是同一份内容，
    // 「上次过了这次没过」就无从归因。故复用发布路径的同两个拼接函数。
    let scan_text = match spec.kind {
        "character" => {
            let card: String =
                sqlx::query_scalar("SELECT card_json FROM cloud_characters WHERE id = $1")
                    .bind(&id)
                    .fetch_optional(&state.db)
                    .await?
                    .ok_or(ApiError::NotFound)?;
            let card: Value = serde_json::from_str(&card).unwrap_or(Value::Null);
            crate::safety::card_scan_text(&card)
        }
        _ => {
            let skeleton: String =
                sqlx::query_scalar("SELECT skeleton_json FROM world_templates WHERE id = $1")
                    .bind(&id)
                    .fetch_optional(&state.db)
                    .await?
                    .ok_or(ApiError::NotFound)?;
            let skeleton: Value = serde_json::from_str(&skeleton).unwrap_or(Value::Null);
            crate::assets::worlds::world_scan_text(&skeleton)
        }
    };

    let (queue_id, verdict, created) = crate::safety::queue_operator_recheck(
        &state,
        queue_kind,
        &id,
        &scan_text,
        &admin.0.user_id,
        &reason,
    )
    .await?;

    super::audit(
        &state.db,
        &admin.0,
        "content.recheck",
        &format!("{}:{}", spec.kind, id),
        &reason,
    )
    .await?;

    Ok(Json(json!({
        "subjectKind": spec.kind,
        "subjectLabel": spec.label,
        "subjectId": id,
        "queueId": queue_id,
        "queueSubjectKind": queue_kind,
        "created": created,
        "machineVerdict": crate::safety::verdict_str(verdict),
        // 再审不动展示态——写进回执，避免被读成「送审 = 已下架」。
        "moderation": APPROVED,
        "takenDown": false,
        "worldlineUntouched": true,
        "notes": [
            "再审只把内容送回人审队列，不改变它的展示态——再审期间内容照常在线。",
            "需要立刻断掉展示请另行调用 takedown；两个动作正交。",
            "人审在既有审核工作台裁决（POST /admin/audit-queue/{id}/approve|reject），裁决回写主体审核态。",
            "同一主体已有待审队列行时不重复入队（回执 created=false），避免同一张卡被多人举报后刷屏人审队列。",
        ],
    })))
}

#[cfg(test)]
mod tests;
