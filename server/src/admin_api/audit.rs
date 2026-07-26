//! 内容审核：审核队列（机审结果 + 人审操作）。approve/reject 同步回写主体 moderation，
//! reject 另将理由落 audit_queue.reject_reason（用户侧 status 端点回显）。
//!
//! ⚠️ 本文件只处置**仍在人审队列里**的条目。**已过审内容**的事后处置（再审 / 下架 / 恢复）
//! 在兄弟模块 `super::takedown`（migration 0044）——approve 回写在此加了一道
//! 「不复活已下架主体」的守卫，两处必须一起读。
//! 申诉复审：GET /admin/appeals 列表 + POST /admin/appeals/{id}/resolve（overturn/uphold）——
//! resolve 是机审/人审驳回后的唯一改判路径，必留 audit_logs。
//!
//! ## `world_event` 主体：`world_events.moderation` 上唯一的**放宽**路径（migration 0047）
//!
//! §15 第 2 层（词库高危命中）与第 3 层（语义复核）都把运行时事件送进本队列，但本文件此前
//! 对该主体没有回写分支——人审点「通过」是一次**静默空操作**，事件永久停在 `pending`。
//! 第 3 层是 fail-closed 的（provider 每抖动一次就收紧一批并无条件入队），把这个缺口放大成了
//! 运营侧的实际风险。补它就是在一张**至今只有收紧路径**的表上开第二条写路径，故四条边界：
//!
//! 1. **正文零改写**（§0.3 公共事实不可回滚）：放宽改的是**可见性**，`SET` 列表里只有
//!    `moderation` 一列，事件正文一个字节不动。与第 3 层收紧路径同一条边界。
//! 2. **单向棘轮不变**：收紧仍然只能从 `'approved'` 出发；放宽全仓只有
//!    [`RELAX_WORLD_EVENT_SQL`] 这一条，起点白名单 `IN ('pending', 'rejected')` **写死在 SQL 里**
//!    （于是它永不自我放宽，也永不复活将来可能出现的 `'takedown'` 哨兵）。
//!    两条路径的形状由源码级红线用例
//!    `safety::semantic::tests::red_line_world_events_has_one_ratchet_and_one_guarded_relax` 锁死。
//! 3. **权限两档**（口径抄 0044 的 `restricted` reviewer 可逆 / `removed` admin 专属）：
//!    推翻**机器**收紧是审核队列的本职 → `reviewer`；推翻**人审终判**是另一回事 →
//!    `POST /admin/audit-queue/{id}/reinstate`，**admin 专属**且理由必填。
//! 4. **判据不新造事实源**：「机器收紧 vs 人工驳回」直接读 `audit_queue.status`——机器入队只写
//!    `'open'`，只有 [`review`] 会把它写成 `'approved'` / `'rejected'`，因此
//!    「被人工驳回过」⟺ 存在一行 `subject_kind='world_event' AND status='rejected'`。
//!    见 [`human_rejected_before`]。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::{now_ms, Placeholders};
use crate::error::ApiError;
use crate::safety::disposal::APPROVED;
use crate::safety::WORLD_EVENT_SUBJECT;

use super::{audit, audit_tx, clamp_limit, parse_cursor, require_role, ActionQuery};

#[derive(Debug, Deserialize)]
pub(super) struct QueueQuery {
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// GET /admin/audit-queue?status=（默认 open）：机审预标注 + 待人审列表。
pub(super) async fn list_queue(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<QueueQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let page = clamp_limit(q.limit);
    let status = q.status.unwrap_or_else(|| "open".into());

    // 发号顺序 = 下面 bind 的顺序；cursor 段出不出现要到运行时才知道，编号不能写死。
    let mut ph = Placeholders::new();
    let mut sql = format!(
        "SELECT id, subject_kind, subject_id, subject_world_id, machine_verdict, machine_hits, \
         status, reviewer_id, reviewed_at, created_at FROM audit_queue WHERE status = {}",
        ph.take()
    );
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(&format!(
            " AND (created_at < {} OR (created_at = {} AND id < {}))",
            ph.take(),
            ph.take(),
            ph.take()
        ));
    }
    sql.push_str(&format!(" ORDER BY created_at DESC, id DESC LIMIT {}", ph.take()));

    let mut query = sqlx::query(&sql).bind(&status);
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
        let hits_raw: String = row.try_get("machine_hits")?;
        let hits: Value = serde_json::from_str(&hits_raw).unwrap_or_else(|_| json!([]));
        items.push(json!({
            "id": id,
            "subjectKind": row.try_get::<String, _>("subject_kind")?,
            "subjectId": row.try_get::<String, _>("subject_id")?,
            // world_event 主体的世界维度（其余主体恒 null）：subject_id 跨世界重名，
            // 少了它工作台上两条不同世界的队列项会长得一模一样。见 migration 0047。
            "subjectWorldId": row.try_get::<Option<String>, _>("subject_world_id")?,
            "machineVerdict": row.try_get::<String, _>("machine_verdict")?,
            "machineHits": hits,
            "status": row.try_get::<String, _>("status")?,
            "reviewerId": row.try_get::<Option<String>, _>("reviewer_id")?,
            "reviewedAt": row.try_get::<Option<i64>, _>("reviewed_at")?,
            "createdAt": created_at,
        }));
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "items": items, "nextCursor": next_cursor })))
}

/// GET /admin/audit-queue/{id}：审核详情（§10 审核工作台）。
/// character 主体附「卡片全文 cardJson + 可审计 manifest + 同作者历史 authorHistory」，
/// 供人审直接对照，无需再逐字段拉取。reviewer/admin 守卫。
pub(super) async fn detail(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;

    let row = sqlx::query(
        "SELECT id, subject_kind, subject_id, subject_world_id, machine_verdict, machine_hits, \
         status, reviewer_id, reviewed_at, created_at FROM audit_queue WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let subject_world_id: Option<String> = row.try_get("subject_world_id")?;
    let hits_raw: String = row.try_get("machine_hits")?;
    let hits: Value = serde_json::from_str(&hits_raw).unwrap_or_else(|_| json!([]));

    // 基础队列字段 + character 专属附加字段占位（非 character 主体保持空值）。
    let mut out = json!({
        "id": row.try_get::<String, _>("id")?,
        "subjectKind": subject_kind,
        "subjectId": subject_id,
        "subjectWorldId": subject_world_id,
        "machineVerdict": row.try_get::<String, _>("machine_verdict")?,
        "machineHits": hits,
        "status": row.try_get::<String, _>("status")?,
        "reviewerId": row.try_get::<Option<String>, _>("reviewer_id")?,
        "reviewedAt": row.try_get::<Option<i64>, _>("reviewed_at")?,
        "createdAt": row.try_get::<i64, _>("created_at")?,
        "cardJson": Value::Null,
        "manifest": Value::Null,
        "authorHistory": json!([]),
        "subjectImageUrl": Value::Null,
        "subjectEvent": Value::Null,
    });

    // 位图主体（立绘 / 世界封面）：把图本身给人审看。
    //
    // 🔴 没有这一段，位图入队就是让人**盲审**——工作台上只有一行 `subjectKind=character_avatar`
    // 和一个空的命中列表，人审无从判断该通过还是驳回，于是「有再审通道」在实践中等于没有。
    // 下发范围与 `cardJson`（整张卡原文）一致：本端点已经是 reviewer 守卫的后台面，
    // 审核者要看的正是未过审的那份内容——玩家侧的「仅 approved 才下发」闸门不受影响。
    if let Some((table, url_column)) = match subject_kind.as_str() {
        "character_avatar" => Some(("cloud_characters", "avatar_url")),
        "world_cover" => Some(("worlds", "cover_url")),
        _ => None,
    } {
        // 表名/列名来自上面这个静态白名单，主体 id 走 $1 绑定。
        let url: Option<Option<String>> =
            sqlx::query_scalar(&format!("SELECT {url_column} FROM {table} WHERE id = $1"))
                .bind(&subject_id)
                .fetch_optional(&state.db)
                .await?;
        if let Some(url) = url.flatten().filter(|u| !u.trim().is_empty()) {
            out["subjectImageUrl"] = json!(url);
        }
    }

    // 运行时世界事件：把事件本身给人审看，理由与上面的位图分支逐字相同——
    // 没有这一段，工作台上只有一行 `subjectKind=world_event` 和一个 `{"layer":3}` 的命中载荷，
    // 人审无从判断该通过还是驳回。另附 `humanRejectedBefore`：它决定这一行该点 approve
    // 还是该走 reinstate（admin 档），让台阶在工作台上就是可见的，而不是点了才 409。
    if subject_kind == WORLD_EVENT_SUBJECT {
        match resolve_world_event(&state.db, &subject_id, subject_world_id.as_deref()).await {
            Ok((event_id, world_id, moderation)) => {
                let erow = sqlx::query(
                    "SELECT tick_no, sequence, event_type, visibility, public_projection_json, \
                     private_projections_json, arbiter_note, occurred_at FROM world_events WHERE id = $1",
                )
                .bind(&event_id)
                .fetch_one(&state.db)
                .await?;
                out["subjectEvent"] = json!({
                    "eventId": event_id,
                    "worldId": world_id,
                    "moderation": moderation,
                    "tickNo": erow.try_get::<i64, _>("tick_no")?,
                    "sequence": erow.try_get::<i64, _>("sequence")?,
                    "eventType": erow.try_get::<String, _>("event_type")?,
                    "visibility": erow.try_get::<String, _>("visibility")?,
                    "publicProjection": erow.try_get::<Option<String>, _>("public_projection_json")?,
                    "privateProjections": erow.try_get::<Option<String>, _>("private_projections_json")?,
                    "arbiterNote": erow.try_get::<Option<String>, _>("arbiter_note")?,
                    "occurredAt": erow.try_get::<i64, _>("occurred_at")?,
                    "humanRejectedBefore": human_rejected_before(&state.db, &subject_id, &world_id).await?,
                });
            }
            // 定位不到（世界已清理 / 存量行跨世界重名）不是 500：如实把原因摆给人审，
            // 队列行本身仍要能打开，否则连「关掉它」都做不到。
            Err(e) => {
                out["subjectEvent"] = json!({ "unresolved": e.to_string() });
            }
        }
    }

    if subject_kind == "character" {
        if let Some(crow) =
            sqlx::query("SELECT owner_id, card_json, manifest_json FROM cloud_characters WHERE id = $1")
                .bind(&subject_id)
                .fetch_optional(&state.db)
                .await?
        {
            let owner_id: String = crow.try_get("owner_id")?;
            let card_text: String = crow.try_get("card_json")?;
            let manifest_text: Option<String> = crow.try_get("manifest_json")?;
            // 卡片全文（非第三人称摘要——人审需看原文判定）。
            out["cardJson"] = serde_json::from_str(&card_text).unwrap_or(Value::Null);
            out["manifest"] =
                manifest_text.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);

            // 同作者历史：同 owner 的其他云端角色（不含当前主体），供判断作者一贯性。
            let hist = sqlx::query(
                "SELECT id, version, moderation, created_at FROM cloud_characters \
                 WHERE owner_id = $1 AND id != $2 ORDER BY created_at DESC, version DESC, id DESC",
            )
            .bind(&owner_id)
            .bind(&subject_id)
            .fetch_all(&state.db)
            .await?;
            let history: Vec<Value> = hist
                .iter()
                .map(|r| {
                    json!({
                        "id": r.try_get::<String, _>("id").unwrap_or_default(),
                        "version": r.try_get::<i64, _>("version").unwrap_or_default(),
                        "moderation": r.try_get::<String, _>("moderation").unwrap_or_default(),
                        "createdAt": r.try_get::<i64, _>("created_at").unwrap_or_default(),
                    })
                })
                .collect();
            out["authorHistory"] = json!(history);
        }
    }

    Ok(Json(out))
}

/// POST /admin/audit-queue/{id}/approve?reason=
pub(super) async fn approve(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    review(&state, &admin.0, &id, "approved", q.reason()).await
}

/// POST /admin/audit-queue/{id}/reject?reason=
pub(super) async fn reject(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    review(&state, &admin.0, &id, "rejected", q.reason()).await
}

/// 队列主体 → 裁决回写的 `(表, 展示态列)`。返回 `None` = 该主体**不走这条通用回写**。
///
/// 🔴 **本表是「哪些主体可以入队」的事实上限**。往 `audit_queue` 塞一个既不在这里、也不在
/// [`review`] 的 `world_event` 专用分支里的 `subject_kind`，等于制造一条人审点了通过/驳回
/// 也无处落地的**死队列项**——迁移 0027 的注释正是因为这个原因，让世界封面的机审裁决一直
/// 不入队。所以扩队列主体和扩回写必须同一批做：`safety::queue_operator_recheck_image`
/// 与下面两条位图分支是一对，缺任何一半都是回归。
///
/// ⚠️ **`world_event` 刻意不在本表里**，不是漏了。通用回写的形状是
/// `UPDATE {表} SET {列} = $1 WHERE id = $2`，而 `world_event` 的 `subject_id` 存的是
/// `domain_event_id`（**不是** `world_events.id`，且跨世界重名），套用通用形状会误伤别的世界。
/// 它走 [`review_world_event`]：先按 `(world_id, domain_event_id)` 定位主键，再分方向写。
///
/// 位图两列可空（无立绘 / 无封面的行留 NULL），调用点的下架守卫因此必须写成 NULL 安全形式。
pub(super) fn writeback_target(subject_kind: &str) -> Option<(&'static str, &'static str)> {
    match subject_kind {
        "character" => Some(("cloud_characters", "moderation")),
        // "template"（admin 官方模板）与 "world_template"（创作者 /assets/worlds 资产）同落 world_templates。
        "template" | "world_template" => Some(("world_templates", "moderation")),
        // 位图两类：迁移 0016 / 0027 起就有展示态列与读取面闸门，缺的一直是人审入队 + 回写这一段。
        "character_avatar" => Some(("cloud_characters", "avatar_moderation")),
        "world_cover" => Some(("worlds", "cover_moderation")),
        // world_event 走专用分支（见上）；intervention 等主体的回写路径随对应模块接入。
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// world_event 主体的裁决回写（migration 0047）
// ═══════════════════════════════════════════════════════════════════════════

/// `world_events.moderation` 的两个「未过审」取值。放宽路径的**起点白名单**。
const MOD_PENDING: &str = "pending";
const MOD_REJECTED: &str = "rejected";
const RELAXABLE_FROM: [&str; 2] = [MOD_PENDING, MOD_REJECTED];

/// 🔴 **全仓唯一的放宽语句**（`world_events` 上第二条写路径，方向与第 3 层棘轮相反）。
///
/// 四条守卫，缺一条都不成立：
/// - `SET moderation = 'approved'` 是**字面量**，不是绑定值 → 这条语句只可能写 `'approved'`；
///   `SET` 列表里只有这一列 → 事件正文一个字节不动（§0.3）。
/// - `WHERE id = $1` 按**主键**点名一行。绝不写 `WHERE domain_event_id = ...`：
///   引擎按 `patch-{base_revision}-ev-{seq}` 生成它，两个世界在同一 revision 上逐字重名，
///   那样会把别的世界里正被拦下的事件一并放行（0047 文件头有完整推导）。
/// - `AND moderation = $2` 是 CAS：绑定值取自本次请求刚读到的当前态，期间被并发改过就命中 0 行。
/// - `AND moderation IN ('pending', 'rejected')` 是**起点白名单**，写死在 SQL 文本里而不是
///   只靠 Rust 侧校验：于是这条语句在任何调用姿势下都不可能从 `'approved'` 自我放宽，
///   也不可能复活将来可能出现的 `'takedown'` 哨兵（0044 的下架不写 `world_events`，
///   但守卫按「白名单」而不是「黑名单」写，才不会随将来新增的哨兵值失效）。
///   ⚠️ 刻意不写成 `moderation <> 'approved'`：黑名单形式在列为 NULL 时求值为 NULL、
///   静默命中 0 行（本列 `NOT NULL`，但 0044 那一批就是在可空列上踩了这个坑）。
const RELAX_WORLD_EVENT_SQL: &str = "UPDATE world_events SET moderation = 'approved' WHERE id = $1 AND moderation = $2 AND moderation IN ('pending', 'rejected')";

/// 人审驳回一条**当前仍在外发**的事件（见 [`review_world_event`] 的方向表）。
///
/// 形状与第 3 层的机器棘轮**完全一致**——`SET` 只有 `moderation`，`WHERE` 钉着 `'approved'`。
/// 它是收紧，不是新形状：红线用例对「收紧只能从 approved 出发」的断言同样覆盖它。
const TIGHTEN_WORLD_EVENT_SQL: &str = "UPDATE world_events SET moderation = 'rejected' WHERE id = $1 AND moderation = 'approved'";

/// 本路径的风控留痕 kind。与机审的 `lexicon` / `semantic` 分开：那两个是机器判定，
/// 这个是**人推翻机器**（或 admin 推翻人），混在一起会让风控面上的机审命中率失真。
const WORLD_EVENT_RISK_KIND: &str = "world_event_moderation";

/// 放宽的权限台阶。口径抄 migration 0044 的 `restricted`（reviewer 可逆）/ `removed`（admin 专属）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// 推翻**机器**收紧 —— 人审队列的本职。
    Reviewer,
    /// 推翻**人审终判** —— 更高台阶，admin 专属且理由必填。
    Admin,
}

impl Tier {
    fn as_str(self) -> &'static str {
        match self {
            Tier::Reviewer => "reviewer",
            Tier::Admin => "admin",
        }
    }
}

/// 队列行 → `(world_events.id, world_id, 当前 moderation)`。
///
/// 🔴 `audit_queue.subject_id` 对本主体存的是 **`domain_event_id`**，它由引擎按
/// `patch-{base_revision}-ev-{seq}` 生成——确定性、不含世界维度，于是两个世界在同一 revision
/// 上产出的事件 id **逐字相同**（两个新世界的第一拍都会有 `patch-0-ev-0`）。所以定位必须带上
/// 世界维度，回写必须按主键。世界维度来自 migration 0047 新增的 `audit_queue.subject_world_id`。
///
/// ⚠️ 0047 之前入队的存量行该列为 NULL，退化为按 `domain_event_id` 全库定位；
/// **命中多于一行即拒绝，绝不猜**（猜错的代价是放行另一个世界里正被拦下的内容）。
async fn resolve_world_event(
    db: &sqlx::AnyPool,
    domain_event_id: &str,
    world_id: Option<&str>,
) -> Result<(String, String, String), ApiError> {
    // 发号顺序 = 下面 bind 的顺序；世界段出不出现取决于存量行有没有那一列，编号不能写死。
    let mut ph = Placeholders::new();
    let mut sql = String::from("SELECT id, world_id, moderation FROM world_events WHERE ");
    if world_id.is_some() {
        sql.push_str(&format!("world_id = {} AND ", ph.take()));
    }
    sql.push_str(&format!("domain_event_id = {}", ph.take()));
    // ORDER BY 全序（id 是主键）；取 2 行只为判「是不是唯一命中」。
    sql.push_str(" ORDER BY id ASC LIMIT 2");

    let mut q = sqlx::query(&sql);
    if let Some(w) = world_id {
        q = q.bind(w);
    }
    let rows = q.bind(domain_event_id).fetch_all(db).await?;

    match rows.len() {
        0 => Err(ApiError::Conflict(
            "该队列行指向的世界事件不存在（世界可能已被清理）；无处回写，请直接关闭该队列行".into(),
        )),
        1 => Ok((
            rows[0].try_get("id")?,
            rows[0].try_get("world_id")?,
            rows[0].try_get("moderation")?,
        )),
        _ => Err(ApiError::Conflict(
            "该队列行没有记录世界维度（0047 之前入队），而 domainEventId 在多个世界里重名，\
             无法确定要回写哪一条；请按 risk_events 里的 worldId 人工核对后处置"
                .into(),
        )),
    }
}

/// 判据：这条事件**被人工驳回过**吗？
///
/// 🔴 不新增 provenance 列，直接读 `audit_queue.status` —— 它本来就是人审动作的台账：
/// 机器入队（`safety::insert_runtime_audit` / `moderate_and_queue`）只写 `'open'`，
/// 全仓只有 [`review`] 会把它改成 `'approved'` / `'rejected'`。于是
/// `status = 'rejected'` 是**人点过驳回**的充要标记，与「机器把它收紧成了 `rejected`」
/// （那只体现在 `world_events.moderation`，不体现在这里）天然分得开。
///
/// ⚠️ NULL 安全：0047 之前的存量行 `subject_world_id IS NULL`。写成 `subject_world_id = $3`
/// 会**静默漏掉**这些行，于是一条被人驳回过的事件会被 reviewer 档悄悄放行——正是放宽方向上
/// 最不该出的错。故写成 `IS NULL OR =`：判不出世界就**算命中**（fail-closed，逼它走 admin 档）。
async fn human_rejected_before(
    db: &sqlx::AnyPool,
    domain_event_id: &str,
    world_id: &str,
) -> Result<bool, ApiError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM audit_queue WHERE subject_kind = $1 \
         AND subject_id = $2 AND status = 'rejected' \
         AND (subject_world_id IS NULL OR subject_world_id = $3)",
    )
    .bind(WORLD_EVENT_SUBJECT)
    .bind(domain_event_id)
    .bind(world_id)
    .fetch_one(db)
    .await?;
    Ok(n > 0)
}

/// `world_event` 主体的裁决回写。**放宽与收紧各只有一条语句**，方向表：
///
/// | 裁决 | 事件当前态 | 数据面动作 |
/// |---|---|---|
/// | approve | `pending` / `rejected` | [`RELAX_WORLD_EVENT_SQL`]（唯一放宽路径） |
/// | approve | `approved` | 无（幂等；另一条队列行已放行过它） |
/// | reject  | `approved` | [`TIGHTEN_WORLD_EVENT_SQL`]（收紧，形状同机器棘轮） |
/// | reject  | `pending` / `rejected` | **无**，且这不是静默空操作 —— 见下 |
///
/// 🔴 最后一格要说清楚：机器入队时**必定已经把事件收紧了**（第 2 层落库前置 `pending`，
/// 第 3 层 `tighten` 成功才入队），所以 reject 撞上的常态就是「早已不外发」。此时再写一次
/// `pending → rejected` 在**任何读取面上都不产生差别**（口径全是 `= 'approved'`），
/// 却要在红线表上多开一条写路径。人审的这次终判落在 `audit_queue.status='rejected'` 上，
/// 而那一行正是 [`human_rejected_before`] 的判据、也是 reviewer 档从此对该事件失效的开关——
/// **它有实际效力，不是空操作**。回执里 `tightened:false` + `moderation` 给真实值如实说明。
///
/// 三段副作用（`world_events` / `audit_queue` / `audit_logs` + `risk_events`）**同一事务**：
/// 放宽比收紧敏感，绝不允许出现「内容放出去了但查不到是谁放的」。
#[allow(clippy::too_many_arguments)]
async fn review_world_event(
    state: &AppState,
    actor: &AuthUser,
    queue_id: &str,
    queue_status: &str,
    subject_id: &str,
    subject_world_id: Option<&str>,
    verdict: &str,
    reason: &str,
    tier: Tier,
) -> Result<Json<Value>, ApiError> {
    let (event_id, world_id, cur) =
        resolve_world_event(&state.db, subject_id, subject_world_id).await?;
    let human_rejected = human_rejected_before(&state.db, subject_id, &world_id).await?;
    let relaxing = verdict == APPROVED;

    // 🔴 权限台阶：reviewer 只能推翻**机器**。人审的终判要另一档权限，不与「推翻机器」共用按钮。
    if relaxing && tier == Tier::Reviewer && human_rejected {
        return Err(ApiError::Conflict(
            "该事件此前已被人审驳回：推翻机器收紧是审核队列的本职，推翻人审终判不是。\
             如确需放行，走 POST /admin/audit-queue/{id}/reinstate（admin 专属，理由必填）"
                .into(),
        ));
    }
    // 取值白名单：只认 approved / pending / rejected 三态。将来若有第四个哨兵值落到本列，
    // 这里会 409 而不是被某条守卫「碰巧」放过去。
    if cur != APPROVED && !RELAXABLE_FROM.contains(&cur.as_str()) {
        return Err(ApiError::Conflict(format!(
            "该事件当前审核态为 {cur:?}，不在人审可处置的取值内（approved / pending / rejected）"
        )));
    }

    let mut tx = state.db.begin().await?;
    let mut relaxed = false;
    let mut tightened = false;
    let mut moderation = cur.clone();

    if relaxing {
        if RELAXABLE_FROM.contains(&cur.as_str()) {
            let res = sqlx::query(RELAX_WORLD_EVENT_SQL)
                .bind(&event_id)
                .bind(&cur)
                .execute(&mut *tx)
                .await?;
            if res.rows_affected() == 0 {
                tx.rollback().await.ok();
                return Err(ApiError::Conflict("该事件审核态已被并发修改，请刷新后重试".into()));
            }
            relaxed = true;
        }
        moderation = APPROVED.to_string();
    } else if cur == APPROVED {
        let res = sqlx::query(TIGHTEN_WORLD_EVENT_SQL).bind(&event_id).execute(&mut *tx).await?;
        if res.rows_affected() == 0 {
            tx.rollback().await.ok();
            return Err(ApiError::Conflict("该事件审核态已被并发修改，请刷新后重试".into()));
        }
        tightened = true;
        moderation = MOD_REJECTED.to_string();
    }

    // 队列行状态。
    // 🔴 **已是 `rejected` 的行不动**（reinstate 可以作用在它上面）：那一行正是
    // [`human_rejected_before`] 的判据，把它改写成 approved 等于亲手拆掉自己的 tier-2 台阶，
    // 而且会抹掉「有人驳回过」这条处置历史。reinstate 的留痕落在 audit_logs / risk_events。
    let mut queue_status_after = queue_status.to_string();
    if queue_status == "open" {
        let reject_reason: Option<&str> =
            if !relaxing && !reason.trim().is_empty() { Some(reason) } else { None };
        let res = sqlx::query(
            "UPDATE audit_queue SET status = $1, reviewer_id = $2, reviewed_at = $3, \
             reject_reason = $4 WHERE id = $5 AND status = 'open'",
        )
        .bind(verdict)
        .bind(&actor.user_id)
        .bind(now_ms())
        .bind(reject_reason)
        .bind(queue_id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            tx.rollback().await.ok();
            return Err(ApiError::Conflict("already_reviewed".into()));
        }
        queue_status_after = verdict.to_string();
    }

    let action = match (tier, relaxing) {
        (Tier::Admin, _) => "audit.world_event_reinstate",
        (Tier::Reviewer, true) => "audit.approved",
        (Tier::Reviewer, false) => "audit.rejected",
    };
    audit_tx(&mut tx, actor, action, &format!("{WORLD_EVENT_SUBJECT}:{subject_id}"), reason).await?;
    // 🔴 风控留痕走 `safety::record_risk_tx`（既有入口），本模块不自己 INSERT risk_events。
    crate::safety::record_risk_tx(
        &mut tx,
        None,
        Some(&world_id),
        WORLD_EVENT_RISK_KIND,
        json!({
            "action": action,
            "tier": tier.as_str(),
            "subjectKind": WORLD_EVENT_SUBJECT,
            "subjectId": subject_id,
            "eventId": event_id,
            "queueId": queue_id,
            "previousModeration": cur,
            "moderation": moderation,
            "relaxed": relaxed,
            "tightened": tightened,
            "humanRejectedBefore": human_rejected,
            // 🔴 §0.3：改的是可见性，不是事实。留痕里也不复制正文（留痕不是内容副本）。
            "bodyRewritten": false,
            "actorId": actor.user_id,
            "actorRole": actor.role,
            "reason": reason,
        }),
    )
    .await?;
    tx.commit().await?;

    let mut notes = vec![
        "回写只改 world_events.moderation 一列（可见性）；事件正文一个字节不动（§0.3 公共事实不可回滚）。",
    ];
    if relaxing && !relaxed {
        notes.push("该事件此前已处于 approved（另一条队列行已放行过它），本次裁决只登记，不重复改数据面。");
    }
    if !relaxing && !tightened {
        notes.push(
            "该事件早已不外发（机器入队前必定已收紧），故本次驳回不改数据面；\
             人审终判落在队列行 status='rejected' 上——它使该事件从此不能再由 reviewer 档放行。",
        );
    }
    if human_rejected {
        notes.push("该事件有人审驳回记录：后续放行一律走 POST /admin/audit-queue/{id}/reinstate（admin 专属）。");
    }

    Ok(Json(json!({
        "id": queue_id,
        "status": queue_status_after,
        "subjectKind": WORLD_EVENT_SUBJECT,
        "subjectId": subject_id,
        "subjectWorldId": world_id,
        "eventId": event_id,
        "previousModeration": cur,
        "moderation": moderation,
        "relaxed": relaxed,
        "tightened": tightened,
        "humanRejectedBefore": human_rejected,
        "tier": tier.as_str(),
        "bodyRewritten": false,
        "notes": notes,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReinstateReq {
    #[serde(default)]
    reason: String,
}

/// POST /admin/audit-queue/{id}/reinstate body {reason}：**推翻人审终判**，放行一条世界事件。
///
/// 🔴 **admin 专属**，且理由必填（复用 `takedown::validate_reason` 的同一条 1..=500 规则）。
/// 台阶设计与 migration 0044 同源：可逆/低风险的动作给 reviewer（`restricted` 下架、
/// 队列里推翻机器收紧），更重的动作抬到 admin（`removed` 永久移除、推翻人的终判）。
/// 两档若共用一个按钮，「两档」在实践中会退化成一档。
///
/// 与 approve 的区别只有两处：**谁能点**、以及**判据是否放行它**。数据面走的是同一条
/// [`RELAX_WORLD_EVENT_SQL`]——放宽路径全仓只有一条，不因为多了个入口就多一份口径。
///
/// 队列行状态：`open` → 置为 `approved`；已是 `rejected` 的行**不动**（见
/// [`review_world_event`] 里的理由）；已是 `approved` → 409（无可 reinstate 的东西）。
pub(super) async fn reinstate(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(req): Json<ReinstateReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &[])?; // 推翻人审终判 = admin 专属。
    let reason = super::takedown::validate_reason(&req.reason)?;

    let row = sqlx::query(
        "SELECT subject_kind, subject_id, subject_world_id, status FROM audit_queue WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let subject_world_id: Option<String> = row.try_get("subject_world_id")?;
    let cur_status: String = row.try_get("status")?;

    if subject_kind != WORLD_EVENT_SUBJECT {
        // 其余主体的驳回改判有各自的既有路径，不在这里开第二条。
        return Err(ApiError::BadRequest(format!(
            "reinstate 只受理 world_event 主体，该队列行是 {subject_kind:?}；\
             角色卡 / 立绘的驳回改判走 POST /admin/appeals/{{id}}/resolve，\
             已过审内容的下架恢复走 POST /admin/content/{{kind}}/{{id}}/restore"
        )));
    }
    if cur_status == APPROVED {
        return Err(ApiError::Conflict("该队列行已是通过态，没有需要 reinstate 的驳回".into()));
    }

    review_world_event(
        &state,
        &admin.0,
        &id,
        &cur_status,
        &subject_id,
        subject_world_id.as_deref(),
        APPROVED,
        &reason,
        Tier::Admin,
    )
    .await
}

async fn review(
    state: &AppState,
    actor: &AuthUser,
    queue_id: &str,
    verdict: &str,
    reason: &str,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT subject_kind, subject_id, subject_world_id, status FROM audit_queue WHERE id = $1",
    )
    .bind(queue_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let subject_world_id: Option<String> = row.try_get("subject_world_id")?;
    let cur_status: String = row.try_get("status")?;
    if cur_status != "open" {
        return Err(ApiError::Conflict("already_reviewed".into()));
    }

    // 🔴 `world_event` 不走下面的通用回写（它的 subject_id 是 domain_event_id，跨世界重名，
    // 套 `WHERE id = $2` 会误伤别的世界），改走带方向与权限台阶的专用分支。
    if subject_kind == WORLD_EVENT_SUBJECT {
        return review_world_event(
            state,
            actor,
            queue_id,
            &cur_status,
            &subject_id,
            subject_world_id.as_deref(),
            verdict,
            reason,
            Tier::Reviewer,
        )
        .await;
    }

    // 人审驳回理由同步落队列行 reject_reason（供用户侧 status 端点回显）；approve 不写（保持 NULL）。
    let reject_reason: Option<&str> =
        if verdict == "rejected" && !reason.trim().is_empty() { Some(reason) } else { None };
    sqlx::query(
        "UPDATE audit_queue SET status = $1, reviewer_id = $2, reviewed_at = $3, reject_reason = $4 WHERE id = $5",
    )
    .bind(verdict)
    .bind(&actor.user_id)
    .bind(now_ms())
    .bind(reject_reason)
    .bind(queue_id)
    .execute(&state.db)
    .await?;

    // 回写主体展示态。四类主体各落一张表的一列，见 `writeback_target`。
    let moderation = if verdict == "approved" { "approved" } else { "rejected" };

    if let Some((table, column)) = writeback_target(&subject_kind) {
        // 🔴 **已被运营下架的主体不得经人审队列悄悄复活**（migration 0044 / `super::takedown`）。
        // 场景：先对已过审内容发起再审（入队），期间又把它下架了，随后有人在工作台点「通过」——
        // 若不设防，回写会把展示态写回 `'approved'`，等于**绕过 `restore` 的可逆性台阶与
        // 权限台阶**把下架撤销掉，而且 `content_takedowns` 里还留着一条自称仍在下架的记录。
        //
        // 守卫只加在 **approve 方向**：reject 写 `'rejected'` 是比下架更强的处置，让它落地是对的。
        // 恢复展示的唯一路径是 POST /admin/content/{kind}/{id}/restore。
        //
        // ⚠️ 位图两列（`avatar_moderation` / `cover_moderation`）**可空**，而 `col <> 'takedown'`
        // 在 col 为 NULL 时求值为 NULL 而非 TRUE，整条 WHERE 会静默命中 0 行——那会表现为
        // 「人审点了通过但什么都没发生」。故写成 NULL 安全形式（两个库口径一致）。
        let takedown_guard = if verdict == "approved" {
            format!(
                " AND ({column} IS NULL OR {column} <> '{}')",
                crate::safety::disposal::TAKEDOWN
            )
        } else {
            String::new()
        };
        // 表名/列名只可能来自 `writeback_target` 的静态白名单，请求字段永远不进 SQL 文本。
        sqlx::query(&format!(
            "UPDATE {table} SET {column} = $1 WHERE id = $2{takedown_guard}"
        ))
        .bind(moderation)
        .bind(&subject_id)
        .execute(&state.db)
        .await?;
    }

    audit(
        &state.db,
        actor,
        &format!("audit.{verdict}"),
        &format!("{subject_kind}:{subject_id}"),
        reason,
    )
    .await?;

    Ok(Json(json!({
        "id": queue_id,
        "status": verdict,
        "subjectKind": subject_kind,
        "subjectId": subject_id,
        "moderation": moderation,
    })))
}

// ---------------- 申诉复审（内容风控申诉，reviewer/admin） ----------------

#[derive(Debug, Deserialize)]
pub(super) struct AppealsQuery {
    status: Option<String>,
}

/// 申诉行 → camelCase JSON（列表与 resolve 响应共用同一形状）。
fn appeal_json(row: &sqlx::any::AnyRow) -> Result<Value, ApiError> {
    Ok(json!({
        "id": row.try_get::<String, _>("id")?,
        "subjectKind": row.try_get::<String, _>("subject_kind")?,
        "subjectId": row.try_get::<String, _>("subject_id")?,
        "ownerId": row.try_get::<String, _>("owner_id")?,
        "appealText": row.try_get::<String, _>("appeal_text")?,
        "status": row.try_get::<String, _>("status")?,
        "resolutionReason": row.try_get::<Option<String>, _>("resolution_reason")?,
        "reviewerId": row.try_get::<Option<String>, _>("reviewer_id")?,
        "createdAt": row.try_get::<i64, _>("created_at")?,
        "resolvedAt": row.try_get::<Option<i64>, _>("resolved_at")?,
    }))
}

/// GET /admin/appeals?status=pending|upheld|overturned|all（默认 pending）：申诉列表 + 主体摘要。
/// character 主体摘要：名字（card_json identity.name）、moderation、avatar_moderation、owner_id。
pub(super) async fn list_appeals(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<AppealsQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let status = q.status.unwrap_or_else(|| "pending".into());
    if !matches!(status.as_str(), "pending" | "upheld" | "overturned" | "all") {
        return Err(ApiError::BadRequest("status 仅支持 pending/upheld/overturned/all".into()));
    }

    let mut sql = String::from(
        "SELECT id, subject_kind, subject_id, owner_id, appeal_text, status, resolution_reason, \
         reviewer_id, created_at, resolved_at FROM moderation_appeals",
    );
    if status != "all" {
        sql.push_str(&format!(" WHERE status = {}", Placeholders::new().take()));
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC");
    let mut query = sqlx::query(&sql);
    if status != "all" {
        query = query.bind(&status);
    }
    let rows = query.fetch_all(&state.db).await?;

    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut item = appeal_json(row)?;
        let subject_kind: String = row.try_get("subject_kind")?;
        let subject_id: String = row.try_get("subject_id")?;
        // 主体摘要：当前仅 character；主体缺失（已删除）留 null，申诉行本身仍可见。
        let mut subject = Value::Null;
        if subject_kind == "character" {
            if let Some(crow) = sqlx::query(
                "SELECT owner_id, card_json, moderation, avatar_moderation FROM cloud_characters WHERE id = $1",
            )
            .bind(&subject_id)
            .fetch_optional(&state.db)
            .await?
            {
                let card_text: String = crow.try_get("card_json")?;
                let name = serde_json::from_str::<Value>(&card_text)
                    .ok()
                    .and_then(|c| c["identity"]["name"].as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                subject = json!({
                    "name": name,
                    "moderation": crow.try_get::<String, _>("moderation")?,
                    "avatarModeration": crow.try_get::<Option<String>, _>("avatar_moderation")?,
                    "ownerId": crow.try_get::<String, _>("owner_id")?,
                });
            }
        }
        item["subject"] = subject;
        items.push(item);
    }
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResolveAppealReq {
    decision: String,
    #[serde(default)]
    reason: String,
}

/// POST /admin/appeals/{id}/resolve body {decision:'overturn'|'uphold', reason}：申诉复审裁决。
///
/// overturn 是驳回后的**唯一改判路径**：只翻转「当时处于 rejected 的那个维度」——
/// 卡 moderation=='rejected' 则改卡为 approved；仅当卡不处于 rejected 而头像
/// avatar_moderation=='rejected' 时才改头像为 approved。不整体放行（卡与头像分开审、分开改判，
/// 避免申诉卡文案却顺带放行未过审头像）。uphold 维持原判，任何 moderation 不动。
/// 两者都：更新申诉行 + audit_logs 留痕（appeal_overturn/appeal_uphold）。
pub(super) async fn resolve_appeal(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(req): Json<ResolveAppealReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    if !matches!(req.decision.as_str(), "overturn" | "uphold") {
        return Err(ApiError::BadRequest("decision 仅支持 overturn/uphold".into()));
    }
    let reason = req.reason.trim().to_string();
    let reason_chars = reason.chars().count();
    if reason_chars == 0 || reason_chars > 500 {
        return Err(ApiError::BadRequest("复审理由必填且不超过 500 字符".into()));
    }

    let row = sqlx::query("SELECT subject_kind, subject_id, status FROM moderation_appeals WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let cur_status: String = row.try_get("status")?;
    if cur_status != "pending" {
        return Err(ApiError::Conflict("该申诉已处理，不可重复裁决".into()));
    }

    if req.decision == "overturn" && subject_kind == "character" {
        // 改判只翻转当时处于 rejected 的那个维度（见函数注释）：卡优先，头像仅在卡未被驳回时翻转。
        let dims: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT moderation, avatar_moderation FROM cloud_characters WHERE id = $1")
                .bind(&subject_id)
                .fetch_optional(&state.db)
                .await?;
        if let Some((moderation, avatar_moderation)) = dims {
            if moderation == "rejected" {
                sqlx::query("UPDATE cloud_characters SET moderation = 'approved' WHERE id = $1")
                    .bind(&subject_id)
                    .execute(&state.db)
                    .await?;
            } else if avatar_moderation.as_deref() == Some("rejected") {
                sqlx::query("UPDATE cloud_characters SET avatar_moderation = 'approved' WHERE id = $1")
                    .bind(&subject_id)
                    .execute(&state.db)
                    .await?;
            }
        }
    }
    // uphold：主体 moderation 一律不动（维持原判）。

    let (new_status, action) = if req.decision == "overturn" {
        ("overturned", "appeal_overturn")
    } else {
        ("upheld", "appeal_uphold")
    };
    sqlx::query(
        "UPDATE moderation_appeals SET status = $1, resolution_reason = $2, reviewer_id = $3, resolved_at = $4 WHERE id = $5",
    )
    .bind(new_status)
    .bind(&reason)
    .bind(&admin.0.user_id)
    .bind(now_ms())
    .bind(&id)
    .execute(&state.db)
    .await?;

    audit(&state.db, &admin.0, action, &format!("{subject_kind}:{subject_id}"), &reason).await?;

    let row = sqlx::query(
        "SELECT id, subject_kind, subject_id, owner_id, appeal_text, status, resolution_reason, \
         reviewer_id, created_at, resolved_at FROM moderation_appeals WHERE id = $1",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(appeal_json(&row)?))
}
