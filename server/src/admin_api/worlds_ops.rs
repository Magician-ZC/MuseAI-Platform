//! 世界运营：活跃世界监控、脱敏卡死诊断、暂停/恢复、官方建房、模板库。

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::db::{new_id, now_ms, Placeholders};
use crate::error::ApiError;
use crate::worlds::{
    create_world as create_world_inner, deathmatch_enabled, enroll_series, is_valid_lethality,
    load_world, series_autoscale_enabled, series_max_instances_cap, upload_cover, CoverReq,
    CreateWorldParams, LETHALITY_DEATHMATCH,
};

use super::dashboards::{cents_to_cny, tokens_to_cents, utc_day_start_ms, DAY_MS};
use super::{audit, clamp_limit, parse_cursor, require_role, ActionQuery};

// ---------------- 世界监控列表 ----------------

#[derive(Debug, Deserialize)]
pub(super) struct WorldListQuery {
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// 页内世界的在场人数：`world_members` 里 status='active' 的成员数。
/// 一次 GROUP BY + IN(页内 id) 取回整页——**绝不逐世界发 SQL**（本端点被后台轮询，N+1 会随页大小放大 QPS）。
async fn active_member_counts(db: &AnyPool, ids: &[String]) -> Result<HashMap<String, i64>, ApiError> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    // 整条语句只有这一串参数，故从 $1 起顺序发号，与下面 bind 的循环顺序一致。
    let placeholders = Placeholders::new().list(ids.len());
    let sql = format!(
        "SELECT world_id, COUNT(*) AS n FROM world_members \
         WHERE status = 'active' AND world_id IN ({placeholders}) GROUP BY world_id"
    );
    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id.as_str());
    }
    for r in query.fetch_all(db).await? {
        out.insert(r.try_get::<String, _>("world_id")?, r.try_get::<i64, _>("n")?);
    }
    Ok(out)
}

/// 页内世界的 tick 聚合：`(今日 token, done 数, failed 数)`，一次 GROUP BY + IN 取整页。
///
/// 今日窗口 `[day_start, day_end)` 由**调用方在 Rust 侧算好毫秒区间**再绑参，SQL 只做 BIGINT 范围比较——
/// `strftime`/`date_trunc` 是方言特有的，SQLite/Postgres 双跑必须避开（db.rs 可移植 SQL 子集约定）。
/// 日界取 UTC，与 `dashboards::metrics_trends`、`world_budgets.budget_day` 完全同一套口径。
/// SUM 一律 CAST(... AS BIGINT)：PG 下 SUM(bigint) 返回 numeric，不 CAST 解码会炸。
async fn tick_stats_by_world(
    db: &AnyPool,
    ids: &[String],
    day_start: i64,
    day_end: i64,
) -> Result<HashMap<String, (i64, i64, i64)>, ApiError> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    // 发号顺序 = 下面 bind 的顺序：先日界两端，再整串 ids。
    let mut ph = Placeholders::new();
    let (day_from, day_to) = (ph.take(), ph.take());
    let placeholders = ph.list(ids.len());
    let sql = format!(
        "SELECT world_id, \
         CAST(COALESCE(SUM(CASE WHEN created_at >= {day_from} AND created_at < {day_to} THEN cost_tokens ELSE 0 END), 0) AS BIGINT) AS today_tokens, \
         CAST(COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) AS BIGINT) AS done_n, \
         CAST(COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS BIGINT) AS failed_n \
         FROM world_ticks WHERE world_id IN ({placeholders}) GROUP BY world_id"
    );
    let mut query = sqlx::query(&sql).bind(day_start).bind(day_end);
    for id in ids {
        query = query.bind(id.as_str());
    }
    for r in query.fetch_all(db).await? {
        out.insert(
            r.try_get::<String, _>("world_id")?,
            (
                r.try_get::<i64, _>("today_tokens")?,
                r.try_get::<i64, _>("done_n")?,
                r.try_get::<i64, _>("failed_n")?,
            ),
        );
    }
    Ok(out)
}

/// GET /admin/worlds?status=&cursor=：全量世界监控（含预算/熔断态；不限可见性）。
///
/// 除世界主表字段外，另给运营表格三列派生数据（R1 成本仪表，§17【拍板 16】）：
/// `participantCount` 在场人数 / `successRate` tick 成功率 / `todayCostCents|todayCostCny` 今日成本。
/// 三列各来自一条页内 GROUP BY 聚合，总查询数恒为 3（列表 1 + 聚合 2），与页大小无关。
///
/// 🔴 封面 `coverUrl`（迁移 0027）：本端点同样是**封面读取面**，一律经 `worlds::visible_cover_url`
/// 这一个闸门——后台不豁免机审，运营列表看到的和玩家大厅看到的必须是同一套「什么算可见封面」。
/// 无封面/未过审 → **不下发该键**（不是空串、不是 null），前端据"键缺席"走确定性内置位图兜底。
pub(super) async fn list_worlds(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<WorldListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let page = clamp_limit(q.limit);
    let mut sql = String::from(
        "SELECT w.id, w.title, w.room_type, w.status, w.visibility, w.member_limit, \
         w.tick_per_day, w.template_id, w.template_version, w.engine_version, w.prompt_set_version, \
         w.model_route_version, w.state_revision, w.created_at, \
         w.cover_url, w.cover_moderation, \
         COALESCE(b.spent_tokens_today, 0) AS spent_tokens_today, \
         COALESCE(b.daily_token_budget, 0) AS daily_token_budget, \
         COALESCE(b.fused, 0) AS fused \
         FROM worlds w LEFT JOIN world_budgets b ON b.world_id = w.id WHERE 1=1",
    );
    // 发号顺序 = 下面 bind 的顺序；status/cursor 段出不出现要到运行时才知道。
    let mut ph = Placeholders::new();
    if q.status.is_some() {
        sql.push_str(&format!(" AND w.status = {}", ph.take()));
    }
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if cursor.is_some() {
        sql.push_str(&format!(
            " AND (w.created_at < {} OR (w.created_at = {} AND w.id < {}))",
            ph.take(),
            ph.take(),
            ph.take()
        ));
    }
    sql.push_str(&format!(" ORDER BY w.created_at DESC, w.id DESC LIMIT {}", ph.take()));

    let mut query = sqlx::query(&sql);
    if let Some(s) = &q.status {
        query = query.bind(s);
    }
    if let Some((ts, id)) = &cursor {
        query = query.bind(*ts).bind(*ts).bind(id);
    }
    query = query.bind(page + 1);

    let rows = query.fetch_all(&state.db).await?;
    let has_more = rows.len() as i64 > page;
    let page_n = (rows.len() as i64).min(page).max(0) as usize;

    // 页内 id → 两条补充聚合（在场人数 / tick 统计），先取 id 再一次性聚合，避免 N+1。
    let mut ids: Vec<String> = Vec::with_capacity(page_n);
    for row in rows.iter().take(page_n) {
        ids.push(row.try_get::<String, _>("id")?);
    }
    let day_start = utc_day_start_ms(now_ms());
    let participants = active_member_counts(&state.db, &ids).await?;
    let tick_stats = tick_stats_by_world(&state.db, &ids, day_start, day_start + DAY_MS).await?;

    let mut items = Vec::new();
    let mut next_cursor: Option<String> = None;
    for row in rows.iter().take(page_n) {
        let id: String = row.try_get("id")?;
        let created_at: i64 = row.try_get("created_at")?;
        next_cursor = Some(format!("{created_at}:{id}"));
        // 在场人数：无成员行的世界即 0（0 是真实答案，不是缺数据）。
        let participant_count = participants.get(&id).copied().unwrap_or(0);
        let (today_tokens, done_n, failed_n) =
            tick_stats.get(&id).copied().unwrap_or((0, 0, 0));
        let today_cents = tokens_to_cents(today_tokens);
        // 成功率口径：**已终结 tick** 中 done 的占比，即 done/(done+failed)，取值 0..1（不是百分数）。
        // pending/running 不进分母——它们还没有结果，计入会把「排队中」误报成「失败」。
        // 注意与 /admin/metrics/overview 的全局 ticks.successRate 不同：那个分母是全部 tick 行。
        // 尚无已终结 tick（新房/只排了队）→ null，表示「暂无数据」，前端显示 —，不得当 0% 渲染。
        let terminal = done_n + failed_n;
        let success_rate: Option<f64> =
            if terminal > 0 { Some(done_n as f64 / terminal as f64) } else { None };
        let mut item = json!({
            "id": id,
            "title": row.try_get::<String, _>("title")?,
            "roomType": row.try_get::<String, _>("room_type")?,
            "status": row.try_get::<String, _>("status")?,
            "visibility": row.try_get::<String, _>("visibility")?,
            "memberLimit": row.try_get::<i64, _>("member_limit")?,
            "tickPerDay": row.try_get::<i64, _>("tick_per_day")?,
            "templateId": row.try_get::<String, _>("template_id")?,
            "templateVersion": row.try_get::<i64, _>("template_version")?,
            "engineVersion": row.try_get::<String, _>("engine_version")?,
            "promptSetVersion": row.try_get::<String, _>("prompt_set_version")?,
            "modelRouteVersion": row.try_get::<String, _>("model_route_version")?,
            "stateRevision": row.try_get::<i64, _>("state_revision")?,
            "spentTokensToday": row.try_get::<i64, _>("spent_tokens_today")?,
            "dailyTokenBudget": row.try_get::<i64, _>("daily_token_budget")?,
            "fused": row.try_get::<i64, _>("fused")? != 0,
            "createdAt": created_at,
            // ---- R1 成本仪表补充列（§17【拍板 16】）----
            "participantCount": participant_count,
            "successRate": success_rate,
            "todayTokens": today_tokens,
            "todayCostCents": today_cents,
            "todayCostCny": cents_to_cny(today_cents),
            // moderationLatency —— **仍不下发**，但理由已经和当初不一样了，故整条重写。
            //
            // 此处原文是 `TODO(数据源缺失)`：「需要每次机审调用的耗时，**全仓无任何一处记录它**」。
            // 那句话在写下时成立，**现在不成立了**：migration 0049 给 `safety_recheck_runs` 加了
            // `provider_ms` 列，只累加 `check_text` 两端的时钟差（超时/报错的调用照算），
            // 读取面是 `GET /api/admin/safety/recheck` 的 `providerLatency.avgMsPerCall`。
            // ⚠️ 顺带说清一个容易被误用的近似：同表的 `latency_ms` 是**一次尝试全程**
            //（含候选装载、收紧 UPDATE、记险、入队），拿它除以调用数得到的数混着 DB 往返——
            // 系统性偏大，却**看起来完全合理**，比 audit_queue 那种一眼假的数更难识破。
            //
            // 🔴 数有了仍不下发，是因为下发的前提是**另外三件事**，一件都还没成立：
            // ① provider 仍是 Dev 桩（本地关键词匹配），耗时恒为 ~0 —— 一个恒 0 的「审核延迟」
            //    在看板上与「审核非常快」长得一模一样，而真相是一次真实审核都没发生过；
            // ② 第 3 层默认关闭，多数世界一行台账都不会有 —— 按世界聚合会得到一片 null，
            //    在前端与「这个世界审核很快」难以区分；
            // ③ 该列只覆盖**运行时投影**这条链，静态内容审核（角色卡 / 世界模板 / 装配钩子 /
            //    入站托梦信，走 `safety::moderate_and_queue`）不落这张表，也不挂在某个世界上——
            //    把只覆盖一条链的数摆进世界列表，读者会当成那个世界的机审总体 SLA。
            // 三条任一成立，本字段就该继续空缺（前端 null 判定 → 显示 —）：
            // 诚实空缺胜过假数字（VALIDATION §0.3）。
        });
        // 🔴 封面：复用 worlds 侧那一个 approved 闸门（不在此另写判断），未过审等同没有封面。
        let cover_moderation: Option<String> = row.try_get("cover_moderation")?;
        if let Some(url) =
            crate::worlds::visible_cover_url(row.try_get("cover_url")?, cover_moderation.as_deref())
        {
            item["coverUrl"] = json!(url);
        }
        items.push(item);
    }
    if !has_more {
        next_cursor = None;
    }
    Ok(Json(json!({ "worlds": items, "nextCursor": next_cursor })))
}

// ---------------- 脱敏诊断 ----------------

/// GET /admin/worlds/{id}/diagnostics：卡死诊断视图。
/// 脱敏：只出调用元数据/tick 错误码/预算/规则命中(风控计数)/事件审核态，
/// 不返回任何私密叙事内容（public/private 投影一律不暴露，§10）。
pub(super) async fn diagnostics(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let world = load_world(&state.db, &id).await?; // 不存在 → NotFound
    // 生效档要照**这个世界**的开关算（world > global > env > 默认）——运营看的就是
    // 「落库意图 vs 实际生效」这一对，用全局值算会让按世界的急停阀在诊断页上看不见。
    let dm_on = deathmatch_enabled(&state.db, Some(&id)).await;

    // 最近 10 个 tick 的元数据（含错误码），不含叙事产物。
    let tick_rows = sqlx::query(
        "SELECT tick_no, status, error, cost_tokens, started_at, finished_at, created_at \
         FROM world_ticks WHERE world_id = $1 ORDER BY tick_no DESC LIMIT 10",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut ticks = Vec::new();
    for r in &tick_rows {
        ticks.push(json!({
            "tickNo": r.try_get::<i64, _>("tick_no")?,
            "status": r.try_get::<String, _>("status")?,
            "error": r.try_get::<Option<String>, _>("error")?,
            "costTokens": r.try_get::<i64, _>("cost_tokens")?,
            "startedAt": r.try_get::<Option<i64>, _>("started_at")?,
            "finishedAt": r.try_get::<Option<i64>, _>("finished_at")?,
            "createdAt": r.try_get::<i64, _>("created_at")?,
        }));
    }

    // 预算/熔断态。除原始列外补齐**金额换算与用量比**，让诊断栏的预算条不必在前端硬编码金额/百分比。
    // 换算单价与公式与 runtime 熔断同源（dashboards::tokens_to_cents ← MUSE_TOKEN_CNY_CENTS_PER_1K）。
    let budget = sqlx::query(
        "SELECT daily_token_budget, daily_cny_budget_cents, spent_tokens_today, budget_day, fused \
         FROM world_budgets WHERE world_id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;
    let budget_json = match budget {
        Some(b) => {
            let daily_tokens: i64 = b.try_get("daily_token_budget")?;
            let daily_cents: i64 = b.try_get("daily_cny_budget_cents")?;
            let spent_tokens: i64 = b.try_get("spent_tokens_today")?;
            let budget_day: String = b.try_get("budget_day")?;
            // budget_day 是 runtime 写的 UTC 日标签；跨日后计数器要等下一拍才归零，
            // 所以陈旧日期下的 spent_* 属于**过去某天**，前端不得当"今日"展示。
            let day_is_today = budget_day == crate::runtime::day_string(now_ms());
            // 有效今日消耗：日标签非今天 → 0（与 runtime 下一拍的重置口径一致）。
            let effective_tokens = if day_is_today { spent_tokens } else { 0 };
            let spent_cents = tokens_to_cents(effective_tokens);
            // 用量比 0..1：预算未设（<=0）→ null（没有上限就没有"用了百分之多少"）。
            let token_ratio: Option<f64> = (daily_tokens > 0)
                .then(|| effective_tokens as f64 / daily_tokens as f64);
            let cny_ratio: Option<f64> =
                (daily_cents > 0).then(|| spent_cents as f64 / daily_cents as f64);
            // 真正决定熔断的是两条线里先到的那条，故合并用量比取二者较大值。
            let usage_ratio: Option<f64> = match (token_ratio, cny_ratio) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            };
            json!({
                "dailyTokenBudget": daily_tokens,
                "dailyCnyBudgetCents": daily_cents,
                "dailyCnyBudget": cents_to_cny(daily_cents),
                "spentTokensToday": spent_tokens,
                "spentTokensTodayEffective": effective_tokens,
                "spentCnyCents": spent_cents,
                "spentCny": cents_to_cny(spent_cents),
                "centsPer1kTokens": super::dashboards::token_cny_cents_per_1k(),
                "budgetDay": budget_day,
                "budgetDayIsToday": day_is_today,
                "tokenUsageRatio": token_ratio,
                "cnyUsageRatio": cny_ratio,
                "usageRatio": usage_ratio,
                "fused": b.try_get::<i64, _>("fused")? != 0,
            })
        }
        None => Value::Null,
    };

    // 规则命中：本世界风控事件按 kind 聚合计数（不出 detail_json 内容）。
    let risk_rows = sqlx::query(
        "SELECT kind, COUNT(*) AS n FROM risk_events WHERE world_id = $1 GROUP BY kind",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut risk_counts = Vec::new();
    for r in &risk_rows {
        risk_counts.push(json!({
            "kind": r.try_get::<String, _>("kind")?,
            "count": r.try_get::<i64, _>("n")?,
        }));
    }

    // 事件审核态计数（仅数量，不含投影内容）。
    let ev_rows = sqlx::query(
        "SELECT moderation, COUNT(*) AS n FROM world_events WHERE world_id = $1 GROUP BY moderation",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let mut ev_by_moderation = Vec::new();
    let mut ev_total = 0i64;
    for r in &ev_rows {
        let n: i64 = r.try_get("n")?;
        ev_total += n;
        ev_by_moderation.push(json!({
            "moderation": r.try_get::<String, _>("moderation")?,
            "count": n,
        }));
    }

    // 世界系列（§5 自动扩容）：这是第几号 / 队列多长 / 生效上限多少 / 急停阀开着吗。
    // **不受 env 开关门控**（见 `worlds::series_admin_view`）：关阀时运营更需要看得见这条队列。
    // 未登记进任何系列 → null（绝大多数世界，含全部玩家自建房）。
    let series_json = crate::worlds::series_admin_view(&state.db, &id).await?.unwrap_or(Value::Null);

    // BE 结局传记（§9）：崩塌世界的封卷是否已产出。只回**元信息**，正文另走玩家读取面
    // `GET /worlds/{id}/biography`——诊断视图是脱敏面，不在这里塞叙事汇总。
    let biography_json = match sqlx::query(
        "SELECT kind, terminal_reason, ending_id, sealed_at FROM world_biographies WHERE world_id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    {
        Some(b) => json!({
            "kind": b.try_get::<String, _>("kind")?,
            "terminalReason": b.try_get::<String, _>("terminal_reason")?,
            "endingId": b.try_get::<String, _>("ending_id")?,
            "sealedAt": b.try_get::<i64, _>("sealed_at")?,
        }),
        None => Value::Null,
    };

    Ok(Json(json!({
        "world": {
            "id": world.id,
            "title": world.title,
            "status": world.status,
            "visibility": world.visibility,
            "roomType": world.room_type,
            "stateRevision": world.state_revision,
            "engineVersion": world.engine_version,
            "promptSetVersion": world.prompt_set_version,
            "modelRouteVersion": world.model_route_version,
            "templateId": world.template_id,
            "templateVersion": world.template_version,
            // 生死契约档（§11）：lethality = 落库原值（建房意图）；effectiveLethality = 实际生效档。
            // 二者不一致即「运营开关正在把生死状降级为同意制」，运营一眼可辨急停阀是否生效。
            "lethality": world.lethality,
            "effectiveLethality": crate::worlds::lethality_label(&world.lethality, dm_on),
        },
        "ticks": ticks,
        "budget": budget_json,
        "series": series_json,
        "beBiography": biography_json,
        "riskEventCounts": risk_counts,
        "eventStats": { "total": ev_total, "byModeration": ev_by_moderation },
        "redactionNote": "诊断视图脱敏：不含私密叙事/投影内容；查看必要内容需另行授权（§10）。",
    })))
}

// ---------------- 暂停 / 恢复 ----------------

/// POST /admin/worlds/{id}/pause?reason=
pub(super) async fn pause(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let world = load_world(&state.db, &id).await?;
    if !matches!(world.status.as_str(), "open" | "running") {
        return Err(ApiError::Conflict("world_not_pausable".into()));
    }
    set_world_status(&state, &admin.0, &id, "paused", "world.pause", q.reason()).await
}

/// POST /admin/worlds/{id}/resume?reason=（paused → running，恢复 tick 调度）。
pub(super) async fn resume(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let world = load_world(&state.db, &id).await?;
    if world.status != "paused" {
        return Err(ApiError::Conflict("world_not_paused".into()));
    }
    set_world_status(&state, &admin.0, &id, "running", "world.resume", q.reason()).await
}

async fn set_world_status(
    state: &AppState,
    actor: &AuthUser,
    id: &str,
    status: &str,
    action: &str,
    reason: &str,
) -> Result<Json<Value>, ApiError> {
    sqlx::query("UPDATE worlds SET status = $1, updated_at = $2 WHERE id = $3")
        .bind(status)
        .bind(now_ms())
        .bind(id)
        .execute(&state.db)
        .await?;
    audit(&state.db, actor, action, id, reason).await?;
    Ok(Json(json!({ "id": id, "status": status })))
}

// ---------------- 官方建房 ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateWorldReq {
    template_id: String,
    #[serde(default = "default_template_version")]
    template_version: i64,
    title: String,
    room_type: Option<String>,
    visibility: Option<String>,
    member_limit: Option<i64>,
    tick_per_day: Option<i64>,
    daily_token_budget: Option<i64>,
    daily_cny_budget_cents: Option<i64>,
    status: Option<String>,
    /// 时间线模式：'interval'（默认，墙钟固定间隔排 tick）或 'event'（放置房 DES 背靠背推进）。
    /// 省略 = interval（向后兼容，老行为不变）。
    timeline_mode: Option<String>,
    /// 平台指派主播：赛事房（arena）必须指定；idle/chapter 可选。
    host_user_id: Option<String>,
    /// 生死契约档（总规格 §11【拍板 24】）：'sanctuary' | 'consent' | 'deathmatch'。
    /// 省略 = consent（同意制，现行机制，老行为不变）。
    ///
    /// **由运营显式指定，星级不自动决定档位**：规格给的「1-2★ 庇护 / 3★ 同意制 / 4-5★ 生死状可选、
    /// 官方赛事强制」是默认映射建议，规格同时明确要求"可分离、可配置"——同一星级要能同时开
    /// 庇护场与生死场（同剧情、不同契约、不同产出表），把映射写成代码规则会直接堵死这个产品形态。
    /// 故此处不做任何星级联动，只收显式入参。
    lethality: Option<String>,
    /// 可选封面（迁移 0027）：与 `POST /worlds/{id}/cover` **同一个载荷类型**（imageBase64 + mime），
    /// 让运营一次调用建完房即带图，不必再单独调一次上传端点。
    ///
    /// 落地方式是「建房后内部调用同一段逻辑」——见 `create_world` 里对 `worlds::upload_cover` 的调用：
    /// MIME 白名单、1MB 上限、对象存储写入、`ModerationProvider::check_image` 图审、三列落库、
    /// 未过审不回传 URL，全部由那一个函数负责，此处不复制任何一条。
    #[serde(default)]
    cover: Option<CoverReq>,
    /// 世界系列自动扩容（总规格 §5「世界系列自动扩容【新增】」）。
    ///
    /// 带上本段 = 把这个新世界登记为**系列的 1 号实例**：此后它满员时，服务端会按**它的建房参数**
    /// 自动开 2 号、3 号……直到上限。不带 = 普通世界，永不扩容（默认，行为与本字段落地前一致）。
    ///
    /// 🔴 这是「未验证功能默认关闭」的**数据侧那一层**（VALIDATION.md §0.1）：
    /// 打开 env 总开关并不会让全站满员世界都开始扩容，只有**在这里显式登记过**的世界才会。
    #[serde(default)]
    series: Option<SeriesReq>,
}

/// 建房时的系列登记入参。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SeriesReq {
    /// 该系列最多开到几号（**含 1 号**）。§0.2 参数化：逐系列可设，不写死在代码里。
    /// 另有全局硬顶 env `MUSE_WORLD_SERIES_MAX_INSTANCES`（默认 10），生效上限取二者较小值。
    max_instances: i64,
}

fn default_template_version() -> i64 {
    1
}

/// POST /admin/worlds：官方放置世界。调 worlds::create_world 建房（钉住引擎/prompt/模型/模板版本 + 预算）。
///
/// 可带 `cover`（迁移 0027）一次建完带图的房：建房落库后**内部调用 `worlds::upload_cover`**，
/// 权限（`can_set_cover`：官方房归运营）、图审、落库、未过审不回传 URL 全部由那一个函数裁定，
/// 本端点不复制任何一条判断。RBAC 不变：建房仍是 operator（admin 直通）。
pub(super) async fn create_world(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(req): Json<CreateWorldReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    if req.title.trim().is_empty() || req.template_id.trim().is_empty() {
        return Err(ApiError::BadRequest("title 与 templateId 必填".into()));
    }
    let mut p = CreateWorldParams::official(req.template_id, req.template_version, req.title);
    if let Some(rt) = req.room_type {
        if !matches!(rt.as_str(), "idle" | "chapter" | "arena") {
            return Err(ApiError::BadRequest("roomType 非法".into()));
        }
        p.room_type = rt;
    }
    if let Some(v) = req.visibility {
        // 枚举校验（对齐 worlds.visibility 约定），避免自由文本落库污染大厅可见性过滤。
        if !matches!(v.as_str(), "official" | "public" | "private") {
            return Err(ApiError::BadRequest("visibility 非法".into()));
        }
        p.visibility = v;
    }
    if let Some(m) = req.member_limit {
        p.member_limit = m;
    }
    if let Some(t) = req.tick_per_day {
        p.tick_per_day = t;
    }
    if let Some(b) = req.daily_token_budget {
        p.daily_token_budget = b;
    }
    if let Some(c) = req.daily_cny_budget_cents {
        p.daily_cny_budget_cents = c;
    }
    if let Some(s) = req.status {
        // 建房仅允许起始态（open/running）；paused/ended 非法（避免建出不可调度的僵尸房）。
        if !matches!(s.as_str(), "open" | "running") {
            return Err(ApiError::BadRequest("status 非法（建房仅允许 open/running）".into()));
        }
        p.status = Some(s);
    }
    if let Some(tm) = req.timeline_mode {
        if !matches!(tm.as_str(), "interval" | "event") {
            return Err(ApiError::BadRequest("timelineMode 非法（仅 interval/event）".into()));
        }
        // event × {idle, chapter, arena} 全允许（P2 Stage3 建房闸放宽）：event 对 chapter/arena 表示
        // **引擎走 DES 地点碰撞**（run_event_step → select_cohort 同 location + 时间窗碰撞，房型无关）；
        // 调度节奏仍由端点驱动（arena host/tick、chapter start），schedule_due_ticks 的背靠背自治仅 idle
        //（runtime/mod.rs），故 chapter/arena event 房「手动排 tick 才推进」，保 arena 节目节奏 / chapter 会话驱动；
        // 终局仍 idle-gated（load_endgame_policy 门 room_type=='idle'），chapter/arena 的引擎终局天然不触发
        //（skeleton 硬节点 threshold=None → 里程碑集空 → 恒不发 MainlineDone），结算走各自 finish/settle 端点。
        // interval 仍为默认，老世界零影响。
        p.timeline_mode = tm;
    }
    // 平台指派主播：写入 host_user_id。赛事房若无主播，require_host 恒 Forbidden、
    // 主播控制台整条回路（host/tick/eliminate/settle）不可用，故 arena 必填。
    if let Some(h) = req.host_user_id {
        let h = h.trim().to_string();
        if h.is_empty() {
            return Err(ApiError::BadRequest("hostUserId 不能为空".into()));
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(&h)
            .fetch_one(&state.db)
            .await?;
        if exists == 0 {
            return Err(ApiError::BadRequest("hostUserId 对应用户不存在".into()));
        }
        p.host_user_id = Some(h);
    }
    if let Some(l) = req.lethality {
        if !is_valid_lethality(&l) {
            return Err(ApiError::BadRequest(
                "lethality 非法（仅 sanctuary/consent/deathmatch）".into(),
            ));
        }
        // 🔴 未验证功能默认关闭（VALIDATION.md §0.1）：生死状档是待验证的默认策略，
        // 代码合并不等于对用户开放——必须运营先打开开关（MUSE_LETHALITY_DEATHMATCH），才允许建生死场。
        // 建房这一道是"前门拒绝"（建不出来），读取侧 worlds::effective_lethality 还有一道
        // "后门降级"（开关关掉后既有生死场立即按同意制跑），两道都在，开关才是真的急停阀。
        //
        // 🔴 **这一处只能用 global 作用域**：建房那一刻世界还不存在，没有 world 可解析。
        // 于是「全局关、但世界 W 单独开」时 W 里的契约照常生效，而新的生死场建不出来——
        // 两者都对，它们回答的是两个问题（见 `worlds::deathmatch_enabled` 的 ctx 口径表）。
        if l == LETHALITY_DEATHMATCH && !deathmatch_enabled(&state.db, None).await {
            return Err(ApiError::BadRequest(
                "生死状档尚未开启：该档属未验证功能，需运营先显式打开开关（MUSE_LETHALITY_DEATHMATCH）后方可建生死场"
                    .into(),
            ));
        }
        p.lethality = l;
    }
    if p.room_type == "arena" && p.host_user_id.is_none() {
        return Err(ApiError::BadRequest(
            "赛事房（arena）必须指定 hostUserId（平台指派主播）".into(),
        ));
    }
    // 🔴 系列登记的**前门校验**（未验证功能默认关闭，VALIDATION.md §0.1）：
    // 开关未开时连"登记"都不允许——否则会攒下一堆一开阀就同时开始扩容的世界。
    // 读取侧另有一道降级（`worlds::ensure_next_series_instance` 第一行：关阀即停扩容），
    // 两道都在，开关才是真的急停阀（口径与生死状档逐字一致）。
    let series_req = match &req.series {
        None => None,
        Some(s) => {
            // ctx 恒为 global（本开关刻意不设 world 档，见 `worlds::series_autoscale_enabled`）。
            if !series_autoscale_enabled(&state.db).await {
                return Err(ApiError::BadRequest(
                    "世界系列自动扩容尚未开启：该能力属未验证功能，需运营先显式打开开关（MUSE_WORLD_SERIES_AUTOSCALE）后方可登记系列"
                        .into(),
                ));
            }
            let cap = series_max_instances_cap();
            if !(1..=cap).contains(&s.max_instances) {
                return Err(ApiError::BadRequest(format!(
                    "series.maxInstances 非法：须在 1-{cap} 之间（上界为全局硬顶 MUSE_WORLD_SERIES_MAX_INSTANCES）"
                )));
            }
            Some(s.max_instances)
        }
    };
    // 系列登记需要模板 id，而 p 随后被 create_world_inner 消耗，故先留一份。
    let series_template_id = p.template_id.clone();
    // 建房留痕带上契约档：生死场是高风险配置，"谁在什么时候把哪个世界建成生死场"必须可溯。
    let host_note = match &p.host_user_id {
        Some(h) => format!("official world, host={h}, lethality={}", p.lethality),
        None => format!("official world, lethality={}", p.lethality),
    };

    let lethality = p.lethality.clone();
    let world_id = create_world_inner(&state.db, p).await?;

    let mut out = json!({ "worldId": &world_id, "lethality": lethality });

    // 系列登记（§5）：把新世界登记为 1 号实例。**幂等**（同一世界重复登记命中既有系列，不新建）。
    // 位置在建房之后：系列的参数复制源是这个已落库的世界行本身。
    let mut series_note = String::new();
    if let Some(max_instances) = series_req {
        let series_id =
            enroll_series(&state.db, &world_id, &series_template_id, max_instances).await?;
        out["seriesId"] = json!(&series_id);
        out["seriesInstanceNo"] = json!(1);
        out["seriesMaxInstances"] = json!(max_instances);
        series_note = format!(", series={series_id}(maxInstances={max_instances})");
    }
    // 封面（可选）：**世界已落库**，故封面环节一律不把整个请求判失败——
    // 建房已写 worlds + world_budgets 且不可回滚，若此处返回 4xx，运营多半会重试建房，
    // 结果是留下一个又一个无人认领的重复世界（比"房建好了但图没上"糟得多）。
    // 因此：成功 → 回传裁决（过审才带 URL）；失败 → 回传 coverError，运营据此单独重传
    // `POST /worlds/{id}/cover` 即可。两条分支都进 audit_logs 的建房留痕，不静默吞掉。
    let mut cover_note = String::new();
    if let Some(cover) = req.cover {
        let attempt =
            upload_cover(State(state.clone()), admin.0.clone(), Path(world_id.clone()), Json(cover));
        match attempt.await {
            Ok(Json(res)) => {
                let moderation = res["moderation"].as_str().unwrap_or("unknown").to_string();
                // 🔴 未过审绝不下发 URL：upload_cover 已卡门（非 approved 时回执 coverUrl 为 null），
                // 此处仅在拿到真 URL 时才写该键，口径与列表投影一致（无 URL → 键缺席）。
                if let Some(url) = res["coverUrl"].as_str() {
                    out["coverUrl"] = json!(url);
                }
                out["coverModeration"] = json!(&moderation);
                cover_note = format!(", cover={moderation}");
            }
            Err(e) => {
                let code = e.code();
                // 内部错误不外泄细节（口径同 ApiError::into_response）。
                let message = match &e {
                    ApiError::Internal(_) => "内部错误".to_string(),
                    other => other.to_string(),
                };
                out["coverError"] = json!({ "code": code, "message": message });
                cover_note = format!(", cover_error={code}");
            }
        }
    }
    audit(
        &state.db,
        &admin.0,
        "world.create",
        &world_id,
        &format!("{host_note}{cover_note}{series_note}"),
    )
    .await?;
    Ok(Json(out))
}

// ---------------- 世界模板库 ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TemplateListQuery {
    moderation: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
    /// R1 Saga 归组（总规格 §3）：按世界系列筛选。传空串等价于不筛（不做「筛出独立模板」用途）。
    saga_id: Option<String>,
}

/// GET /admin/world-templates?moderation=&cursor=&sagaId=
///
/// 传 sagaId 时切换为**阶段列表**语义：只返回该 Saga 的模板，并按 stage_no 升序（剧情顺序）
/// 而非 created_at 降序——阶段的自然序是剧情推进顺序，不是录入时间。
pub(super) async fn list_templates(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<TemplateListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator", "reviewer"])?;
    let page = clamp_limit(q.limit);
    let saga_filter = q.saga_id.as_deref().filter(|s| !s.trim().is_empty());
    let mut sql = String::from(
        "SELECT id, title, room_type, skeleton_json, admission_json, official, version, \
         moderation, star_rating, star_source, saga_id, stage_no, created_at \
         FROM world_templates WHERE 1=1",
    );
    // 发号顺序 = 下面 bind 的顺序；moderation/saga/cursor 三段各自可有可无，
    // 且 ORDER BY 二选一——编号只能在运行时随分支长出来。
    let mut ph = Placeholders::new();
    if q.moderation.is_some() {
        sql.push_str(&format!(" AND moderation = {}", ph.take()));
    }
    if saga_filter.is_some() {
        sql.push_str(&format!(" AND saga_id = {}", ph.take()));
    }
    // 阶段列表按剧情顺序；普通列表沿用既有游标分页（created_at DESC + id DESC）。
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    if saga_filter.is_none() && cursor.is_some() {
        sql.push_str(&format!(
            " AND (created_at < {} OR (created_at = {} AND id < {}))",
            ph.take(),
            ph.take(),
            ph.take()
        ));
    }
    if saga_filter.is_some() {
        sql.push_str(&format!(" ORDER BY stage_no ASC, id ASC LIMIT {}", ph.take()));
    } else {
        sql.push_str(&format!(" ORDER BY created_at DESC, id DESC LIMIT {}", ph.take()));
    }

    let mut query = sqlx::query(&sql);
    if let Some(m) = &q.moderation {
        query = query.bind(m);
    }
    if let Some(s) = saga_filter {
        query = query.bind(s);
    }
    if saga_filter.is_none() {
        if let Some((ts, id)) = &cursor {
            query = query.bind(*ts).bind(*ts).bind(id);
        }
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
        let skeleton_raw: String = row.try_get("skeleton_json")?;
        let admission_raw: String = row.try_get("admission_json")?;
        items.push(json!({
            "id": id,
            "title": row.try_get::<String, _>("title")?,
            "roomType": row.try_get::<String, _>("room_type")?,
            "skeletonJson": serde_json::from_str::<Value>(&skeleton_raw).unwrap_or(Value::Null),
            "admissionJson": serde_json::from_str::<Value>(&admission_raw).unwrap_or(Value::Null),
            "official": row.try_get::<i64, _>("official")? != 0,
            "version": row.try_get::<i64, _>("version")?,
            "moderation": row.try_get::<String, _>("moderation")?,
            "starRating": row.try_get::<i64, _>("star_rating")?,
            "starSource": row.try_get::<String, _>("star_source")?,
            "sagaId": row.try_get::<String, _>("saga_id")?,
            "stageNo": row.try_get::<i64, _>("stage_no")?,
            "createdAt": created_at,
        }));
    }
    // 阶段列表按 stage_no 排序，created_at 游标对它无意义（会跳阶段）——单个 Saga 的阶段数
    // 由剧情结构决定（量级十几个），一页即可，故不提供游标。
    if !has_more || saga_filter.is_some() {
        next_cursor = None;
    }
    Ok(Json(json!({ "templates": items, "nextCursor": next_cursor })))
}

// ---------------- 模板星级 curation（波次 3：运营定档） ----------------

/// 运营定档星级范围（1..=5）；自动定档只能给到 2★，3-5★ 唯此端点可授予（数据晋升）。
const CURATED_STAR_MIN: i64 = 1;
const CURATED_STAR_MAX: i64 = 5;
/// 定档理由长度上限（字符数）；理由必填（audit_logs 留痕的最小信息量）。
const STAR_REASON_MAX_CHARS: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetStarReq {
    star: i64,
    reason: String,
}

/// POST /admin/world-templates/{id}/star：运营定档（operator，admin 直通）。
/// body {star: 1..=5, reason: 1..=500 字符} → star_rating=star + star_source='curated'，
/// audit_logs 留痕（action 'template_star'，reason 原样入档）。范围/理由非法 400、模板不存在 404。
pub(super) async fn set_template_star(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Json(req): Json<SetStarReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    if !(CURATED_STAR_MIN..=CURATED_STAR_MAX).contains(&req.star) {
        return Err(ApiError::BadRequest(format!(
            "star 非法：星级须在 {CURATED_STAR_MIN}-{CURATED_STAR_MAX} 之间"
        )));
    }
    let reason = req.reason.trim();
    if reason.is_empty() || reason.chars().count() > STAR_REASON_MAX_CHARS {
        return Err(ApiError::BadRequest(format!(
            "reason 非法：定档理由须为 1-{STAR_REASON_MAX_CHARS} 字符"
        )));
    }
    let res = sqlx::query("UPDATE world_templates SET star_rating = $1, star_source = 'curated' WHERE id = $2")
        .bind(req.star)
        .bind(&id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit(&state.db, &admin.0, "template_star", &id, reason).await?;
    Ok(Json(json!({ "templateId": id, "starRating": req.star, "starSource": "curated" })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateTemplateReq {
    title: String,
    room_type: String,
    skeleton_json: Value,
    admission_json: Option<Value>,
    /// R1 Saga 归组（总规格 §3）。留空 = 独立模板（默认，行为与本字段落地前完全一致）。
    saga_id: Option<String>,
    /// 阶段序号，仅在 saga_id 非空时有意义（1 起）。
    stage_no: Option<i64>,
}

/// 单个 Saga 的阶段序号上限。剧情结构决定阶段数（分卷检测 + 运营校准），量级十几个；
/// 设上限是防运营误填（如把字数当阶段号），不是产品规则。
const STAGE_NO_MAX: i64 = 999;

/// POST /admin/world-templates：新建模板（skeleton_json 结构校验 + 进入审核态/审核队列）。
pub(super) async fn create_template(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(req): Json<CreateTemplateReq>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    if req.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title 必填".into()));
    }
    if !matches!(req.room_type.as_str(), "idle" | "chapter" | "arena") {
        return Err(ApiError::BadRequest("roomType 非法".into()));
    }
    // skeleton_json 校验：必须为对象（主线硬节点/结局池/隐藏内容池/装配规则的容器）。
    if !req.skeleton_json.is_object() {
        return Err(ApiError::BadRequest("skeletonJson 必须是 JSON 对象".into()));
    }
    // Phase 3：引用完整性校验——reward_item_ref / connections / residentItemIds / carried_item_ids /
    // gate.requiredItemIds 须能在 world_items / locations 目录解引用，gate.requiredCosmologies ∈ 官方枚举。
    // 建模板期前置拦截坏引用，避免装配/运行时静默退化（防御式解析吞掉 → 数据错误难发现）。
    // 🔴 容器装配的建模板前门只能取 **global** 档：建模板时**没有世界**（模板是世界的蓝图）。
    // 装配期那一侧按世界解析，两处口径不同的理由见 `assembly::container_assembly_enabled`。
    let container_on = crate::assembly::container_assembly_enabled(&state.db, None).await;
    if let Err(msg) = crate::assembly::validate_skeleton_refs(&req.skeleton_json, container_on) {
        return Err(ApiError::BadRequest(msg));
    }
    let admission = req.admission_json.unwrap_or_else(|| json!({ "mode": "open" }));
    if !admission.is_object() {
        return Err(ApiError::BadRequest("admissionJson 必须是 JSON 对象".into()));
    }
    // Saga 归组（总规格 §3）：saga_id 与 stage_no 必须成对——只给其一是运营录入错误，
    // 静默接受会产出「有系列无阶段」或「有阶段无系列」的孤儿模板，阶段列表页无法归组。
    let saga_id = req.saga_id.as_deref().map(str::trim).unwrap_or("");
    let stage_no = req.stage_no.unwrap_or(0);
    if saga_id.is_empty() && stage_no != 0 {
        return Err(ApiError::BadRequest(
            "stageNo 需与 sagaId 同时提供：阶段序号只在世界系列内有意义".into(),
        ));
    }
    if !saga_id.is_empty() && !(1..=STAGE_NO_MAX).contains(&stage_no) {
        return Err(ApiError::BadRequest(format!(
            "stageNo 非法：属于世界系列的模板须提供 1-{STAGE_NO_MAX} 的阶段序号"
        )));
    }

    let id = new_id("tpl");
    let now = now_ms();
    // 新模板进入待审核态（官方模板亦走审核工作台）。
    sqlx::query(
        "INSERT INTO world_templates (id, title, room_type, skeleton_json, admission_json, \
         official, version, moderation, saga_id, stage_no, created_at) \
         VALUES ($1, $2, $3, $4, $5, 1, 1, 'pending', $6, $7, $8)",
    )
    .bind(&id)
    .bind(req.title.trim())
    .bind(&req.room_type)
    .bind(req.skeleton_json.to_string())
    .bind(admission.to_string())
    .bind(saga_id)
    .bind(stage_no)
    .bind(now)
    .execute(&state.db)
    .await?;

    // 登记到审核队列，供审核工作台 approve/reject（回写 world_templates.moderation）。
    sqlx::query(
        "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, machine_hits, \
         status, created_at) VALUES ($1, 'template', $2, 'pending', '[]', 'open', $3)",
    )
    .bind(new_id("aq"))
    .bind(&id)
    .bind(now)
    .execute(&state.db)
    .await?;

    audit(&state.db, &admin.0, "template.create", &id, "").await?;
    Ok(Json(json!({ "templateId": id, "moderation": "pending" })))
}
