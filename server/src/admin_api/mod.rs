//! 管理后台 API（S6）：八模块（平台规格 §3 产品视角 / §10 技术）。
//! 全部 AdminUser 守卫（admin/operator/reviewer/support/finance），每个写操作写 audit_logs 留痕，前缀 /admin。
//!
//! 端点清单（前缀 /api）：
//!   引导：    POST /admin/dev-login（dev 引导登录 → admin token）
//!   用户管理：GET /admin/users?query=&cursor=、POST /admin/users/{id}/ban|unban
//!   内容审核：GET /admin/audit-queue?status=、POST /admin/audit-queue/{id}/approve|reject（回写主体 moderation）
//!            POST /admin/audit-queue/{id}/reinstate（**admin 专属**，理由必填）——`world_event` 主体
//!            被人审驳回后的放行台阶。approve 只推翻**机器**收紧；推翻**人审终判**抬到 admin 档，
//!            两档不共用一个按钮（口径同 0044 的 restricted/removed）。见 audit.rs 模块头
//!   申诉复审：GET /admin/appeals?status=、POST /admin/appeals/{id}/resolve（overturn/uphold，唯一改判路径）
//!   已过审内容处置：GET /admin/content/takedowns?state=&kind=、GET /admin/content/{kind}/{id}、
//!            POST /admin/content/{kind}/{id}/recheck|takedown|restore（migration 0044）
//!            —— 前四项作用于**仍在队列里**的条目，这一组作用于**已经在线上**的内容。
//!            🔴 处置的是展示面（主体的审核态列），已落定的世界事实一个字节不动，见 takedown.rs 模块头
//!   处置申诉：GET /admin/content/appeals?status=&kind=、POST /admin/content/appeals/{id}/resolve
//!            （migration 0045）——被处置的作者对**处置本身**提异议。与「申诉复审」分立：
//!            那条受理发布期驳回、改判即写 approved；这条受理过审后被处置、改判走 restore 台阶
//!   世界运营：GET /admin/worlds?status=、GET /admin/worlds/{id}/diagnostics（脱敏诊断）、
//!            POST /admin/worlds/{id}/pause|resume、POST /admin/worlds（官方建房）、GET/POST /admin/world-templates、
//!            POST /admin/world-templates/{id}/star（星级 curation：3-5★ 唯一晋升路径）
//!   人工校准：GET /admin/sagas（阶段切分总览）、GET /admin/sagas/{sagaId}（逐阶段结构）、
//!            GET /admin/identity-pools（声明身份池的模板目录）、
//!            GET /admin/world-templates/{id}/identity-pool（身份池声明 + 实际分配分布）
//!            —— 四个端点**全只读**，不提供在线调参（响应恒带 editable:false）
//!   经济运营：GET /admin/economy/overview（真实只读聚合：充值/退款/余额/礼物/订单状态，不建结算）
//!            GET /admin/ledger/reconcile（P4：全账复式恒等 SUM=0 + 账户物化余额对账，finance 只读，无提现）
//!   数据看板：GET /admin/metrics/overview（SQL 聚合）、GET /admin/metrics/trends?days=（按天趋势，UTC 日界）
//!   治理：    GET/POST /admin/prompts、POST /admin/prompts/{id}/activate|canary、
//!            GET/POST /admin/model-routes、POST /admin/model-routes/{id}/activate（一键回滚=激活旧版本）
//!   运行时开关：GET/POST /admin/flags、GET /admin/flags/resolve（dry-run）、DELETE /admin/flags/{id}
//!            （VALIDATION §0.1 的运营面：按用户/世界/全局三作用域灰度；**写 admin 专属**）
//!   风控：    GET /admin/risk-events?kind=&cursor=
//!   内容安全第 3 层：GET /admin/safety/recheck?since=&until=（语义分类异步复核的运行台账 +
//!            成本读数；路由定义在 `safety::semantic`，此处聚合挂载）
//!            🔴 响应里的 `providerStub` / `honesty[]` 会告诉你：这条链路当前跑的是 Dev 桩
//!   工单：    GET /admin/data-requests?status=、POST /admin/data-requests/{id}/run

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::app::AppState;
use crate::auth::{issue_access, AdminUser, AuthUser};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;

mod audit;
mod calibration;
mod dashboards;
mod flags;
mod governance;
mod ops;
mod reconcile;
mod takedown;
mod users;
mod worlds_ops;

#[cfg(test)]
mod tests;

/// 处置申诉的两件**作者侧**需要的东西（提交面在 `assets`，裁决面在本模块）。
///
/// 从这里再导出而不是让 `assets` 直接查表，是为了让「申诉行长什么样」只有一处定义——
/// 尤其是那处定义**刻意不含** `content_takedowns.reason`（运营内部备注不进作者侧）。
pub(crate) use takedown::appeals::{
    latest_for_subjects as disposal_appeal_view, APPEALABLE_SUBJECT_KINDS,
};

/// dev-login 约定密钥（本地/CI 引导用）。可用环境变量 MUSE_ADMIN_DEV_SECRET 覆盖。
const DEFAULT_DEV_ADMIN_SECRET: &str = "muse-dev-admin";

fn dev_admin_secret() -> String {
    std::env::var("MUSE_ADMIN_DEV_SECRET").unwrap_or_else(|_| DEFAULT_DEV_ADMIN_SECRET.to_string())
}

pub fn router() -> Router<AppState> {
    Router::new()
        // 管理员引导登录
        .route("/admin/dev-login", post(dev_login))
        .route("/admin/me", get(admin_me))
        .route("/admin/me/pending", get(admin_me_pending))
        // 用户管理
        .route("/admin/users", get(users::list_users))
        .route("/admin/users/{id}/ban", post(users::ban_user))
        .route("/admin/users/{id}/unban", post(users::unban_user))
        // 内容审核
        .route("/admin/audit-queue", get(audit::list_queue))
        .route("/admin/audit-queue/{id}", get(audit::detail))
        .route("/admin/audit-queue/{id}/approve", post(audit::approve))
        .route("/admin/audit-queue/{id}/reject", post(audit::reject))
        .route("/admin/audit-queue/{id}/reinstate", post(audit::reinstate))
        // 申诉复审（内容风控申诉：机审/人审驳回后的唯一改判路径）
        .route("/admin/appeals", get(audit::list_appeals))
        .route("/admin/appeals/{id}/resolve", post(audit::resolve_appeal))
        // 已过审内容处置（migration 0044）：再审 / 下架 / 恢复。
        // 台账 `/admin/content/takedowns` 比 `/admin/content/{kind}/{id}` 少一段，
        // 两者在 matchit 里根本不在同一层，不存在「静态段被参数段吃掉」的歧义。
        .route("/admin/content/takedowns", get(takedown::list_takedowns))
        // 处置申诉（migration 0045）：被下架的作者对**处置本身**提异议的裁决面。
        // 与 `/admin/appeals`（0018 发布期驳回申诉）分立——改判动作不同，见 takedown/appeals.rs 模块头。
        // 两条静态段 `takedowns` / `appeals` 与参数段 `{kind}` 在 matchit 里各自匹配，无歧义。
        .route("/admin/content/appeals", get(takedown::appeals::list_appeals))
        .route("/admin/content/appeals/{id}/resolve", post(takedown::appeals::resolve_appeal))
        .route("/admin/content/{kind}/{id}", get(takedown::subject_status))
        .route("/admin/content/{kind}/{id}/recheck", post(takedown::recheck))
        .route("/admin/content/{kind}/{id}/takedown", post(takedown::takedown))
        .route("/admin/content/{kind}/{id}/restore", post(takedown::restore))
        // 世界运营
        .route("/admin/worlds", get(worlds_ops::list_worlds).post(worlds_ops::create_world))
        // 平台级健康汇总：修的是「三档由前端按已加载那一页现算、翻页没翻完就偏小」。
        .route("/admin/worlds/summary", get(worlds_ops::worlds_summary))
        .route("/admin/worlds/{id}/diagnostics", get(worlds_ops::diagnostics))
        .route("/admin/worlds/{id}/pause", post(worlds_ops::pause))
        .route("/admin/worlds/{id}/resume", post(worlds_ops::resume))
        .route(
            "/admin/world-templates",
            get(worlds_ops::list_templates).post(worlds_ops::create_template),
        )
        // 模板星级 curation（波次 3）：运营定档 3-5★ 的唯一路径（自动定档封顶 2★）。
        .route("/admin/world-templates/{id}/star", post(worlds_ops::set_template_star))
        // 人工校准面（总规格 §79/§83 流水线第一环）：阶段切分 + 身份池 + 境界档三维，**全只读**。
        // 与 `{id}/star` 同为 `/admin/world-templates/{id}/…` 的静态子节点，matchit 各自匹配。
        .route("/admin/sagas", get(calibration::list_sagas))
        .route("/admin/sagas/{saga_id}", get(calibration::saga_detail))
        .route("/admin/identity-pools", get(calibration::list_identity_pools))
        .route(
            "/admin/world-templates/{id}/identity-pool",
            get(calibration::template_identity_pool),
        )
        .route("/admin/realm-tiers", get(calibration::list_realm_tiers))
        .route(
            "/admin/world-templates/{id}/realm-tier",
            get(calibration::template_realm_tier),
        )
        // 经济运营（真实只读聚合）
        .route("/admin/economy/overview", get(dashboards::economy_overview))
        // 财务对账（P4 合规增强）：全账复式恒等 + 账户物化余额对账（finance/admin 只读，无提现）
        .route("/admin/ledger/reconcile", get(reconcile::ledger_reconcile))
        // 数据看板
        .route("/admin/metrics/overview", get(dashboards::metrics_overview))
        .route("/admin/metrics/trends", get(dashboards::metrics_trends))
        // 模型与 Prompt 治理
        .route("/admin/prompts", get(governance::list_prompts).post(governance::create_prompt))
        .route("/admin/prompts/{id}/activate", post(governance::activate_prompt))
        .route("/admin/prompts/{id}/canary", post(governance::canary_prompt))
        .route(
            "/admin/model-routes",
            get(governance::list_routes).post(governance::create_route),
        )
        .route("/admin/model-routes/{id}/activate", post(governance::activate_route))
        // 运行时开关（VALIDATION §0.1 的运营面；写 admin 专属，读 operator，理由见 flags.rs 模块头）
        .route("/admin/flags", get(flags::list_flags).post(flags::set_flag))
        // dry-run 诊断：静态段 `resolve` 与下一行的 `{id}` 是 matchit 的兄弟节点，静态优先。
        .route("/admin/flags/resolve", get(flags::resolve_flag))
        .route("/admin/flags/{id}", axum::routing::delete(flags::delete_flag))
        // 风控
        .route("/admin/risk-events", get(ops::list_risk_events))
        // 客服与工单
        .route("/admin/data-requests", get(ops::list_data_requests))
        .route("/admin/data-requests/{id}/run", post(ops::run_data_request))
        // §15 第 3 层语义复核的运营读数（GET /admin/safety/recheck）。
        // 路由定义留在 `safety::semantic` 自己那边（与写台账的 SQL、诚实边界文案同处一个文件，
        // 免得「数怎么来的」和「数怎么解释」分居两地），此处只做聚合挂载 —— 不动 `app.rs`。
        .merge(crate::safety::semantic::admin_router())
}

// ---------------- 共享设施（子模块经 super:: 复用） ----------------

/// S-6 最小权限：端点级 role→action 矩阵。AdminUser 提取器只做粗粒度守卫（是否后台角色），
/// 各 handler 在其上调用本函数做细粒度授权——`admin` 为超级用户放行一切；其余角色须在
/// `allowed` 白名单内，否则 403。矩阵（admin 全权，此处列其余角色）：
///   operator：世界运营（worlds/templates/metrics/governance 只读）
///   reviewer：内容审核（audit-queue、已过审内容的再审/下架/恢复、模板/风控只读）。
///            🔴 例外：**永久移除**（`takedown?permanent=true`）走 `require_role(&admin, &[])`
///            = admin 专属——不可逆处置比可逆处置门槛更高，否则两档在实践中会退化成一档。
///   support ：客服（用户管理、工单、风控只读）
///   finance ：经济只读（economy/metrics）
pub(super) fn require_role(admin: &AdminUser, allowed: &[&str]) -> Result<(), ApiError> {
    let role = admin.0.role.as_str();
    if role == "admin" || allowed.contains(&role) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// audit_logs 的写入语句。`audit` 与 `audit_tx` 共用同一份字面量——留痕字段口径只有一处，
/// 免得池上写的和事务里写的哪天悄悄长歪。
const AUDIT_INSERT: &str =
    "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7)";

/// 审计留痕：所有写操作统一调用，落 audit_logs。
pub(super) async fn audit(
    db: &AnyPool,
    actor: &AuthUser,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(AUDIT_INSERT)
        .bind(new_id("aud"))
        .bind(&actor.user_id)
        .bind(&actor.role)
        .bind(action)
        .bind(subject)
        .bind(reason)
        .bind(now_ms())
        .execute(db)
        .await?;
    Ok(())
}

/// `audit` 的事务内变体：在调用方已开启的 tx 内写 `audit_logs`，与业务副作用**原子**。
///
/// 用于「处置与留痕必须同成同败」的写操作（`takedown` 的下架 / 恢复）：留痕若能在处置成功后
/// 单独失败，就会产生一条**查不到是谁下的**的下架——而对不可逆处置来说，那等于没有留痕。
/// 单连接池（测试 / SQLite dev）下也必须复用 tx，否则再借连接会死锁 PoolTimedOut
/// （同 `safety::record_risk_tx` 的注释）。
pub(super) async fn audit_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    actor: &AuthUser,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(AUDIT_INSERT)
        .bind(new_id("aud"))
        .bind(&actor.user_id)
        .bind(&actor.role)
        .bind(action)
        .bind(subject)
        .bind(reason)
        .bind(now_ms())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// cursor 编码为 `{created_at}:{id}`（created_at 为纯数字，按首个冒号切分）。
pub(super) fn parse_cursor(cursor: &str) -> Option<(i64, String)> {
    let (ts, id) = cursor.split_once(':')?;
    Some((ts.parse().ok()?, id.to_string()))
}

pub(super) fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(20).clamp(1, 100)
}

/// 写操作的可选审计理由（走 query，避免强制携带请求体）。
#[derive(Debug, Deserialize)]
pub(super) struct ActionQuery {
    #[serde(default)]
    reason: Option<String>,
}

impl ActionQuery {
    pub(super) fn reason(&self) -> &str {
        self.reason.as_deref().unwrap_or("")
    }
}

// ---------------- 管理员引导登录 ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevLoginReq {
    #[serde(default)]
    secret: String,
}

/// POST /admin/dev-login：dev 态引导登录，校验约定 secret → 签发 admin token 供后台联调。
///
/// 生产（dev_mode=false）此端点直接 403 禁用。
/// TODO(prod seeding)：生产真实管理员账号靠 users.role='admin'——由运维经受控迁移/CLI
/// 将指定账号提权（例：`UPDATE users SET role='admin' WHERE phone=?`），随后走正式登录签发
/// 携带该 role 的 access token（注：当前 /auth/login 恒发 role='user'，生产接入真实管理员
/// 登录时需由 auth 侧读取 users.role 后签发对应 role——属 auth 模块职责，此处仅说明约定）。
/// GET /admin/me：当前登录的后台账号是谁。
///
/// ═══════════════════════════════════════════════════════════════════════════
/// 为什么这个端点值得单独存在
/// ═══════════════════════════════════════════════════════════════════════════
/// 后台外壳右上角此前挂着一条 `TODO(接口缺字段): GET /admin/me（displayName / avatarUrl）`，
/// 于是正式模式下只能显示一个角色名（「运营」），**看不出自己登录的是哪个账号**。
/// 在一个所有写操作都落 `audit_logs`（`actor_id` + `actor_role`）的后台里，
/// 「我现在是谁」不是装饰：处置、改判、开关、定档全都记在这个人头上，
/// 而运营常常同时开着好几个环境的标签页。
///
/// 🔴 **只回身份，不回权限清单。** 前端的 `rbac.ts` 已按 role 做可见性映射，
/// 这里再下发一份「你能做什么」就会变成同一判定的第二份拷贝——而它的漂移方向是
/// 「界面显示你能点、服务端 403」。角色是唯一事实，能力由两侧各自从角色推导。
///
/// ⚠️ **`avatarUrl` 仍然不下发**，因为它真的不存在：`users` 表只有 `nickname`
/// （0001_init.sql），后台账号从来没有过头像字段。造一个空字段下发只会让下一个人
/// 以为「接上了但没数据」。要加得先加列——那是产品决定，不是补一个 TODO。
///
/// 🔵 dev 引导登录（`POST /admin/dev-login`）签发的 `admin_dev` **在 `users` 表里没有行**，
/// 本端点必须能活着回来：查不到时 `nickname` 为空、`status` 为 `unknown`，
/// 而不是 404。否则 dev 联调时右上角会变成一个报错。
async fn admin_me(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT nickname, status FROM users WHERE id = $1")
        .bind(&admin.0.user_id)
        .fetch_optional(&state.db)
        .await?;
    let (nickname, status) = match row {
        Some(r) => (r.try_get::<String, _>("nickname")?, r.try_get::<String, _>("status")?),
        None => (String::new(), "unknown".to_string()),
    };
    Ok(Json(json!({
        "userId": admin.0.user_id,
        // 🔴 角色取自 **token**（`AdminUser` 提取器已校验过它在后台角色集合内），
        // 不取自 users 表：token 里的那个才是这次请求实际被授权的角色，
        // 也是 audit_logs 会记下的那个。两者若漂开（改了库没重新登录），
        // 界面必须显示**正在生效**的那个，否则运营会以为自己已经降权了而其实没有。
        "role": admin.0.role,
        "nickname": nickname,
        "status": status,
    })))
}

/// GET /admin/me/pending：**这个角色现在有多少事等着处理**。
///
/// ═══════════════════════════════════════════════════════════════════════════
/// 它替换掉的是一个恒为 0 的假红点
/// ═══════════════════════════════════════════════════════════════════════════
/// 后台外壳右上角的铃铛此前挂着 `TODO(接口缺字段): 未读通知数`，
/// 正式模式恒显示 0（design preview 里写死 12）。那条 TODO 的措辞其实指错了方向：
/// **后台从来就没有「通知」这个概念**——`notification_outbox` 是玩家侧的
/// （`user_id` + 日报 / 同意征询 / 邀请），运营从不收信。
/// 所以正确的做法不是给后台造一套通知，而是回答铃铛真正被问的那个问题：
/// **「有什么在等我？」**
///
/// ═══════════════════════════════════════════════════════════════════════════
/// 🔴 只回**这个角色能处置**的队列
/// ═══════════════════════════════════════════════════════════════════════════
/// 每条队列的角色门与它自己的**处置端点**逐一对齐（不是另编一张表）：
///
/// | 队列 | 处置端点 | 角色门 |
/// |---|---|---|
/// | 内容审核 `audit_queue(status='open')` | `audit::approve` / `reject` | `reviewer` |
/// | 审核申诉 `moderation_appeals(pending)` | `audit::resolve_appeal` | `reviewer` |
/// | 处置申诉 `disposal_appeals(pending)` | `takedown::appeals::resolve_appeal` | `reviewer` |
/// | 社交举报 `social_reports(pending)` | `social` 后台处置 | `reviewer` + `support` |
/// | 数据请求 `data_requests(pending/running)` | `ops::run_data_request` | `support` |
///
/// 给一个角色报它打不开的队列，等于在催他做一件点进去就 403 的事。
/// `admin` 是超级用户（与 `require_role` 同语义），看全部。
///
/// ⚠️ **刻意不含 `risk_events`**：那张表是**流水**不是队列——它没有「已处理」状态
/// （`ops::list_risk_events` 只读、没有配对的处置端点），计数只会单调上涨，
/// 挂上红点就是一个永远消不掉的角标，一周之后没人会再看它。
/// 要把它变成队列得先加处置状态，那是产品决定。
async fn admin_me_pending(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    let role = admin.0.role.as_str();
    let is_admin = role == "admin";
    let db = &state.db;

    // (key, 中文名, 落地模块, 是否属于本角色, SQL)
    let specs: [(&str, &str, &str, bool, &str); 5] = [
        (
            "audit",
            "内容审核队列",
            "audit",
            is_admin || role == "reviewer",
            "SELECT COUNT(*) AS n FROM audit_queue WHERE status = 'open'",
        ),
        (
            "appeals",
            "审核申诉",
            "audit",
            is_admin || role == "reviewer",
            "SELECT COUNT(*) AS n FROM moderation_appeals WHERE status = 'pending'",
        ),
        (
            "disposalAppeals",
            "处置申诉",
            "audit",
            is_admin || role == "reviewer",
            "SELECT COUNT(*) AS n FROM disposal_appeals WHERE status = 'pending'",
        ),
        (
            "socialReports",
            "社交举报",
            "social",
            is_admin || role == "reviewer" || role == "support",
            "SELECT COUNT(*) AS n FROM social_reports WHERE status = 'pending'",
        ),
        (
            "dataRequests",
            "数据请求",
            // 🔴 落地模块是**客服与工单**（`Tickets.tsx` 调 `/admin/data-requests`），不是风控。
            // 第一版写成 `risk` —— 那样红点点进去会落到一个根本没有这条队列的页面，
            // 而「点进去什么都没有」比不给红点更糟：它会让人以为队列已经被别人清掉了。
            "tickets",
            is_admin || role == "support",
            "SELECT COUNT(*) AS n FROM data_requests WHERE status IN ('pending','running')",
        ),
    ];

    let mut queues: Vec<Value> = Vec::new();
    let mut total: i64 = 0;
    for (key, label, module, mine, sql) in specs {
        if !mine {
            continue;
        }
        let n: i64 = sqlx::query_scalar(sql).fetch_one(db).await?;
        total += n;
        queues.push(json!({ "key": key, "label": label, "module": module, "count": n }));
    }

    Ok(Json(json!({ "total": total, "queues": queues })))
}

async fn dev_login(
    State(state): State<AppState>,
    Json(req): Json<DevLoginReq>,
) -> Result<Json<Value>, ApiError> {
    if !state.config.dev_mode {
        return Err(ApiError::Forbidden);
    }
    if req.secret != dev_admin_secret() {
        return Err(ApiError::Unauthorized);
    }
    let admin_id = "admin_dev";
    let token =
        issue_access(&state.config.jwt_secret, admin_id, "admin", state.config.access_ttl_secs)?;
    let actor = AuthUser { user_id: admin_id.to_string(), role: "admin".to_string() };
    audit(&state.db, &actor, "admin.dev_login", admin_id, "dev bootstrap").await?;
    Ok(Json(json!({
        "accessToken": token,
        "role": "admin",
        "userId": admin_id,
    })))
}
