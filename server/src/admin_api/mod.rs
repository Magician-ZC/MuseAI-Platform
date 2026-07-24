//! 管理后台 API（S6）：八模块（平台规格 §3 产品视角 / §10 技术）。
//! 全部 AdminUser 守卫（admin/operator/reviewer/support/finance），每个写操作写 audit_logs 留痕，前缀 /admin。
//!
//! 端点清单（前缀 /api）：
//!   引导：    POST /admin/dev-login（dev 引导登录 → admin token）
//!   用户管理：GET /admin/users?query=&cursor=、POST /admin/users/{id}/ban|unban
//!   内容审核：GET /admin/audit-queue?status=、POST /admin/audit-queue/{id}/approve|reject（回写主体 moderation）
//!   申诉复审：GET /admin/appeals?status=、POST /admin/appeals/{id}/resolve（overturn/uphold，唯一改判路径）
//!   世界运营：GET /admin/worlds?status=、GET /admin/worlds/{id}/diagnostics（脱敏诊断）、
//!            POST /admin/worlds/{id}/pause|resume、POST /admin/worlds（官方建房）、GET/POST /admin/world-templates、
//!            POST /admin/world-templates/{id}/star（星级 curation：3-5★ 唯一晋升路径）
//!   经济运营：GET /admin/economy/overview（真实只读聚合：充值/退款/余额/礼物/订单状态，不建结算）
//!            GET /admin/ledger/reconcile（P4：全账复式恒等 SUM=0 + 账户物化余额对账，finance 只读，无提现）
//!   数据看板：GET /admin/metrics/overview（SQL 聚合）、GET /admin/metrics/trends?days=（按天趋势，UTC 日界）
//!   治理：    GET/POST /admin/prompts、POST /admin/prompts/{id}/activate|canary、
//!            GET/POST /admin/model-routes、POST /admin/model-routes/{id}/activate（一键回滚=激活旧版本）
//!   风控：    GET /admin/risk-events?kind=&cursor=
//!   工单：    GET /admin/data-requests?status=、POST /admin/data-requests/{id}/run

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::AnyPool;

use crate::app::AppState;
use crate::auth::{issue_access, AdminUser, AuthUser};
use crate::db::{new_id, now_ms};
use crate::error::ApiError;

mod audit;
mod dashboards;
mod governance;
mod ops;
mod reconcile;
mod users;
mod worlds_ops;

#[cfg(test)]
mod tests;

/// dev-login 约定密钥（本地/CI 引导用）。可用环境变量 MUSE_ADMIN_DEV_SECRET 覆盖。
const DEFAULT_DEV_ADMIN_SECRET: &str = "muse-dev-admin";

fn dev_admin_secret() -> String {
    std::env::var("MUSE_ADMIN_DEV_SECRET").unwrap_or_else(|_| DEFAULT_DEV_ADMIN_SECRET.to_string())
}

pub fn router() -> Router<AppState> {
    Router::new()
        // 管理员引导登录
        .route("/admin/dev-login", post(dev_login))
        // 用户管理
        .route("/admin/users", get(users::list_users))
        .route("/admin/users/{id}/ban", post(users::ban_user))
        .route("/admin/users/{id}/unban", post(users::unban_user))
        // 内容审核
        .route("/admin/audit-queue", get(audit::list_queue))
        .route("/admin/audit-queue/{id}", get(audit::detail))
        .route("/admin/audit-queue/{id}/approve", post(audit::approve))
        .route("/admin/audit-queue/{id}/reject", post(audit::reject))
        // 申诉复审（内容风控申诉：机审/人审驳回后的唯一改判路径）
        .route("/admin/appeals", get(audit::list_appeals))
        .route("/admin/appeals/{id}/resolve", post(audit::resolve_appeal))
        // 世界运营
        .route("/admin/worlds", get(worlds_ops::list_worlds).post(worlds_ops::create_world))
        .route("/admin/worlds/{id}/diagnostics", get(worlds_ops::diagnostics))
        .route("/admin/worlds/{id}/pause", post(worlds_ops::pause))
        .route("/admin/worlds/{id}/resume", post(worlds_ops::resume))
        .route(
            "/admin/world-templates",
            get(worlds_ops::list_templates).post(worlds_ops::create_template),
        )
        // 模板星级 curation（波次 3）：运营定档 3-5★ 的唯一路径（自动定档封顶 2★）。
        .route("/admin/world-templates/{id}/star", post(worlds_ops::set_template_star))
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
        // 风控
        .route("/admin/risk-events", get(ops::list_risk_events))
        // 客服与工单
        .route("/admin/data-requests", get(ops::list_data_requests))
        .route("/admin/data-requests/{id}/run", post(ops::run_data_request))
}

// ---------------- 共享设施（子模块经 super:: 复用） ----------------

/// S-6 最小权限：端点级 role→action 矩阵。AdminUser 提取器只做粗粒度守卫（是否后台角色），
/// 各 handler 在其上调用本函数做细粒度授权——`admin` 为超级用户放行一切；其余角色须在
/// `allowed` 白名单内，否则 403。矩阵（admin 全权，此处列其余角色）：
///   operator：世界运营（worlds/templates/metrics/governance 只读）
///   reviewer：内容审核（audit-queue、模板/风控只读）
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

/// 审计留痕：所有写操作统一调用，落 audit_logs。
pub(super) async fn audit(
    db: &AnyPool,
    actor: &AuthUser,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, actor_role, action, subject, reason, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
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
