//! 处置申诉的**后台面**：队列 + 裁决（migration 0045）。作者提交面在 `assets::disposal_appeal`。
//!
//! ## 为什么不复用 `moderation_appeals`（0018）
//!
//! 三条理由写在 migration 0045 的头注释里（键的形状 / 改判动作 / 受理条件），此处只重复最要命
//! 的那一条：0018 的 `resolve_appeal` 改判是直接 `UPDATE ... SET moderation = 'approved'`。
//! 处置申诉若走那条路，等于**绕过 `restore` 的可逆性台阶与权限台阶**把下架撤销掉，
//! 还会留下一条自称仍在下架的 `content_takedowns` 记录——那正是 0044 给 `audit::review`
//! 加守卫要防的洞。所以本模块的改判**只调 [`super::restore_in_tx`]**（全仓唯一的恢复实现）。
//!
//! ## 三种裁决结果，其中一种不是"决定"而是"事实"
//!
//! | 决定 | 处置态 | 结果 |
//! |---|---|---|
//! | `uphold` | 任意 | 维持处置，展示态一个字节不动 |
//! | `overturn` | `restricted` | 走 `restore_in_tx` 恢复到 `prev_moderation` |
//! | `overturn` | `removed` | **409**：永久移除不可恢复（口径同 `restore`，不给不可逆开后门） |
//! | `overturn` | `restored` | 允许，但**不再动状态**（内容已经回来了），回执 `restored:false` + `alreadyRestored:true` |
//!
//! 最后一行是刻意的。运营在申诉裁决之前自己把内容恢复了是常态（申诉往往只是催办）；此时若
//! 报 409，这条申诉会永远卡在 `pending` —— 一个既不能裁决也不会消失的队列项。作者要的结果
//! 已经达成，如实记 `overturned` 并说明「状态本就已恢复」，比制造一条僵尸行诚实。
//!
//! ## 🔴 权限与留痕
//!
//! 走既有矩阵：`reviewer`（与 `restore` 同一档——两者做的是同一件事，只是发起人不同）。
//! 裁决与留痕同事务：`audit_logs`（`content.appeal_overturn` / `content.appeal_uphold`）+
//! `risk_events`（经 `safety::record_risk_tx`）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::db::{now_ms, Placeholders};
use crate::error::ApiError;

use super::super::{audit_tx, clamp_limit, parse_cursor, require_role};
use super::{
    fetch_takedown, restore_in_tx, spec_of, subject_dimensions, validate_reason, APPROVED,
    DISPOSAL_RISK_KIND, STATE_REMOVED, STATE_RESTORED, STATE_RESTRICTED,
};

/// 申诉状态三态。
pub(crate) const APPEAL_PENDING: &str = "pending";
pub(crate) const APPEAL_UPHELD: &str = "upheld";
pub(crate) const APPEAL_OVERTURNED: &str = "overturned";

/// 可申诉的主体类型（作者提交面共用本白名单）。
///
/// 只有这两类判定得出「作者是谁」——都挂在 `cloud_characters.owner_id` 上。
/// `world_cover` 属于世界（房主口径另说）、`world_templates` 根本没有 owner 列，
/// 强行给它们开申诉入口只会得到一个**没人有资格提交**的端点。这是如实的范围，不是遗漏；
/// 缺口登记在 `docs/VALIDATION.md`。
pub(crate) const APPEALABLE_SUBJECT_KINDS: &[&str] = &["character", "character_avatar"];

const APPEAL_COLUMNS: &str = "id, takedown_id, disposal_at, subject_kind, subject_id, owner_id, \
     appeal_text, disposal_state, status, resolution_reason, reviewer_id, created_at, resolved_at";

/// 申诉行 → camelCase JSON。
///
/// 🔴 这里**没有** `content_takedowns.reason`，且不得加：那是运营内部处置备注（口径同
/// `audit_logs.reason`）。本函数被作者侧回显复用，加一列就等于把内部备注推给作者。
/// 后台需要内部理由时另行 JOIN（见 [`list_appeals`] 的 `disposal` 段）。
pub(crate) fn appeal_json(row: &sqlx::any::AnyRow) -> Result<Value, ApiError> {
    Ok(json!({
        "id": row.try_get::<String, _>("id")?,
        "takedownId": row.try_get::<String, _>("takedown_id")?,
        "disposalAt": row.try_get::<i64, _>("disposal_at")?,
        "subjectKind": row.try_get::<String, _>("subject_kind")?,
        "subjectId": row.try_get::<String, _>("subject_id")?,
        "ownerId": row.try_get::<String, _>("owner_id")?,
        "appealText": row.try_get::<String, _>("appeal_text")?,
        "disposalState": row.try_get::<String, _>("disposal_state")?,
        "status": row.try_get::<String, _>("status")?,
        "resolutionReason": row.try_get::<Option<String>, _>("resolution_reason")?,
        "reviewerId": row.try_get::<Option<String>, _>("reviewer_id")?,
        "createdAt": row.try_get::<i64, _>("created_at")?,
        "resolvedAt": row.try_get::<Option<i64>, _>("resolved_at")?,
    }))
}

/// 作者侧回显用：取该主体最新的一条申诉（全序取一行，无则 `None`）。
pub(crate) async fn latest_for_subjects(
    db: &sqlx::AnyPool,
    subject_id: &str,
) -> Result<Option<Value>, ApiError> {
    // 同一张卡的卡文与立绘各有各的处置，作者侧只回显「最近发生的那一条」——
    // 全序 `created_at DESC, id DESC`，同毫秒并列时不加次级键会在两次请求间跳变。
    //
    // ⚠️ 占位符**按在 SQL 文本里出现的先后发号**，不是按写代码的先后（SQLite 把 `$1` 当具名
    // 参数、按首次出现顺序派号，见 `db.rs` 模块头与 `testkit` 的可移植性用例）。所以
    // `subject_id` 的号必须先取——它在 WHERE 里排在 `IN (...)` 前面。反过来写在 PG 上照样对，
    // 在 SQLite 上会把参数错位绑上去，且两边都不报错。
    let mut ph = Placeholders::new();
    let subject_ph = ph.take();
    let kinds = ph.list(APPEALABLE_SUBJECT_KINDS.len());
    let sql = format!(
        "SELECT {APPEAL_COLUMNS} FROM disposal_appeals \
         WHERE subject_id = {subject_ph} AND subject_kind IN ({kinds}) \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    );
    // 发号顺序 = bind 顺序：先 subject_id，后 kinds 列表。
    let mut q = sqlx::query(&sql).bind(subject_id);
    for k in APPEALABLE_SUBJECT_KINDS {
        q = q.bind(*k);
    }
    let row = q.fetch_optional(db).await?;
    row.as_ref().map(appeal_json).transpose()
}

// ═══════════════════════════════════════════════════════════════════════════
// GET：申诉队列
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub(in crate::admin_api) struct AppealsQuery {
    status: Option<String>,
    kind: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// GET /admin/content/appeals?status=&kind=&cursor=&limit=：处置申诉队列（复合游标全序翻页）。
///
/// 每条附 `disposal` 段（当前处置态 + **运营内部处置理由**）——这是后台面，人审要据以判断
/// 「当初为什么下架」才裁决得了。该段绝不出现在作者侧回显里（见 [`appeal_json`] 注释）。
pub(in crate::admin_api) async fn list_appeals(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<AppealsQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    let page = clamp_limit(q.limit);

    let status = q.status.unwrap_or_else(|| APPEAL_PENDING.into());
    if !matches!(status.as_str(), APPEAL_PENDING | APPEAL_UPHELD | APPEAL_OVERTURNED | "all") {
        return Err(ApiError::BadRequest(
            "status 仅支持 pending / upheld / overturned / all".into(),
        ));
    }
    if let Some(k) = q.kind.as_deref() {
        // 未知 kind → 400 而不是静默空列表（空列表会被读成「这类内容没人申诉过」）。
        if !APPEALABLE_SUBJECT_KINDS.contains(&k) {
            return Err(ApiError::BadRequest(format!(
                "kind 仅支持 {}",
                APPEALABLE_SUBJECT_KINDS.join(" / ")
            )));
        }
    }

    // 发号顺序 = 下面 bind 的顺序；筛选段与游标段出不出现要到运行时才知道，编号不能写死。
    let mut ph = Placeholders::new();
    let mut sql = format!("SELECT {APPEAL_COLUMNS} FROM disposal_appeals WHERE 1 = 1");
    if status != "all" {
        sql.push_str(&format!(" AND status = {}", ph.take()));
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
    if status != "all" {
        query = query.bind(&status);
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

        let mut item = appeal_json(row)?;
        let kind: String = row.try_get("subject_kind")?;
        let subject_id: String = row.try_get("subject_id")?;
        // 后台专属：当前处置态 + 运营内部理由（`content_takedowns` 全行，含 `reason`）。
        item["disposal"] = fetch_takedown(&state.db, &kind, &subject_id).await?.unwrap_or(Value::Null);
        items.push(item);
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "items": items, "nextCursor": next_cursor })))
}

// ═══════════════════════════════════════════════════════════════════════════
// POST：裁决
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::admin_api) struct ResolveReq {
    decision: String,
    #[serde(default)]
    reason: String,
}

/// POST /admin/content/appeals/{id}/resolve body {decision:'overturn'|'uphold', reason}：处置申诉裁决。
///
/// `reviewer`——与 `restore` 同一档：改判做的就是恢复，不该比运营自己点恢复更难或更容易。
///
/// `reason` 是**写给作者的答复**，会经作者侧 status 端点回显（口径同 0018 的 `resolution_reason`）。
/// 它与 `content_takedowns.reason`（运营内部备注、永不回显）是两个东西，不得互相顶替。
pub(in crate::admin_api) async fn resolve_appeal(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(req): Json<ResolveReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["reviewer"])?;
    if !matches!(req.decision.as_str(), "overturn" | "uphold") {
        return Err(ApiError::BadRequest("decision 仅支持 overturn/uphold".into()));
    }
    let reason = validate_reason(&req.reason)?;

    let row = sqlx::query(
        "SELECT subject_kind, subject_id, status FROM disposal_appeals WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let subject_kind: String = row.try_get("subject_kind")?;
    let subject_id: String = row.try_get("subject_id")?;
    let cur_status: String = row.try_get("status")?;
    if cur_status != APPEAL_PENDING {
        return Err(ApiError::Conflict("该申诉已处理，不可重复裁决".into()));
    }
    let spec = spec_of(&subject_kind)?;

    let overturn = req.decision == "overturn";
    // 处置台账的**当前**态（可能与申诉提交时的快照不同：运营期间可能已升级或已恢复）。
    let disposal = fetch_takedown(&state.db, spec.kind, &subject_id).await?;
    let disposal_state =
        disposal.as_ref().and_then(|v| v["state"].as_str()).unwrap_or("").to_string();
    let prev = disposal
        .as_ref()
        .and_then(|v| v["prevModeration"].as_str())
        .unwrap_or(APPROVED)
        .to_string();

    // 改判前置：只有 `restricted` 才真的要动状态。
    let mut restored = false;
    let mut already_restored = false;
    if overturn {
        match disposal_state.as_str() {
            STATE_RESTRICTED => restored = true,
            // 不可逆就是不可逆——口径与 `restore` 逐字一致，不给永久移除开一条申诉侧门。
            STATE_REMOVED => {
                return Err(ApiError::Conflict(
                    "该内容已被永久移除，不可恢复（如需重新上线请由作者重新发布）；\
                     如认为处置有误，可选择维持并在答复里说明"
                        .into(),
                ))
            }
            // 运营已自行恢复：作者要的结果已达成，如实记 overturned 而不制造一条卡死的 pending 行。
            STATE_RESTORED => already_restored = true,
            _ => {
                return Err(ApiError::Conflict(format!(
                    "该{}当前没有处于生效中的处置，无从改判",
                    spec.label
                )))
            }
        }
    }

    let (new_status, action) = if overturn {
        (APPEAL_OVERTURNED, "content.appeal_overturn")
    } else {
        (APPEAL_UPHELD, "content.appeal_uphold")
    };
    let now = now_ms();

    // 维度在开事务前取完（事务内再借连接会在单连接池下死锁 PoolTimedOut）。
    let (user_dim, world_dim) = subject_dimensions(&state.db, spec, &subject_id).await?;

    let mut tx = state.db.begin().await?;
    if restored {
        // 🔴 恢复只有一份实现（`restore_in_tx`）：展示态写回 `prev_moderation` + 台账翻
        // `restored` + 审计 + 风控，一步不落，且与本次裁决同成同败。
        restore_in_tx(
            &mut tx,
            spec,
            &subject_id,
            &admin.0,
            action,
            &reason,
            &prev,
            &(user_dim.clone(), world_dim.clone()),
            now,
        )
        .await?;
    }

    // 裁决落库（CAS 到 pending：并发双裁只有一次能成）。
    let res = sqlx::query(
        "UPDATE disposal_appeals SET status = $1, resolution_reason = $2, reviewer_id = $3, \
         resolved_at = $4 WHERE id = $5 AND status = $6",
    )
    .bind(new_status)
    .bind(&reason)
    .bind(&admin.0.user_id)
    .bind(now)
    .bind(&id)
    .bind(APPEAL_PENDING)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(ApiError::Conflict("该申诉已被并发裁决，请刷新后重试".into()));
    }

    // `restore_in_tx` 已经为「恢复」这个动作写过一份审计与风控；此处再写一条的主体是**申诉裁决**
    // 本身（含 uphold 与「已恢复」两种不动状态的情形），两条不重复：一条答「内容为什么回来了」，
    // 一条答「这次申诉的结论是什么」。
    audit_tx(&mut tx, &admin.0, action, &format!("appeal:{id}"), &reason).await?;
    crate::safety::record_risk_tx(
        &mut tx,
        user_dim.as_deref(),
        world_dim.as_deref(),
        DISPOSAL_RISK_KIND,
        json!({
            "action": action,
            "appealId": id,
            "subjectKind": spec.kind,
            "subjectId": subject_id,
            "decision": req.decision,
            "disposalState": disposal_state,
            "restored": restored,
            "alreadyRestored": already_restored,
            "actorId": admin.0.user_id,
            "actorRole": admin.0.role,
            "reason": reason,
        }),
    )
    .await?;
    tx.commit().await?;

    let row = sqlx::query(&format!("SELECT {APPEAL_COLUMNS} FROM disposal_appeals WHERE id = $1"))
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    let mut out = appeal_json(&row)?;
    out["restored"] = json!(restored);
    out["alreadyRestored"] = json!(already_restored);
    // 只有真的恢复了才报展示态；否则 null（空串会被读成「展示态是空的」）。
    out["moderation"] = if restored { json!(prev) } else { Value::Null };
    // 口径同处置回执：改判恢复的是展示，已落定的世界事实一个字节不动。
    out["worldlineUntouched"] = json!(true);
    Ok(Json(out))
}
