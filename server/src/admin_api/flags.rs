//! 运行时开关运营端点（`crate::flags` 的写入面与诊断面）。
//!
//! ```text
//! GET    /admin/flags?flag=                          列出登记表 + 全部记录 + 全局生效值
//! GET    /admin/flags/resolve?flag=&userId=&worldId=  dry-run：这个开关对这个人/这个世界解析成什么、为什么
//! POST   /admin/flags                                设置一条（upsert，唯一键 flag+scope+targetId）
//! DELETE /admin/flags/{id}?reason=                   删除一条（该目标回落到更宽作用域 / env）
//! ```
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 RBAC：**写操作 admin 专属，读放到 operator**
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 写用 `require_role(&admin, &[])`（与 `governance.rs` 的 prompt/model-route 写操作同档），
//! 而不是像世界运营那样放给 `operator`。理由：
//!
//! 1. **爆炸半径**。开关直接决定「用户能看到什么」——打开 `MUSE_ONBOARDING` 就是对一批真实
//!    用户开放一条会发实物（占卡位）、会烧模型预算的动线；打开 `MUSE_LETHALITY_DEATHMATCH`
//!    就是让永久死亡对既有世界立即生效。这比「暂停一个世界」「给模板定星级」高一个数量级，
//!    与「激活一个新 prompt 版本」（admin 专属）同档甚至更高——prompt 换版影响叙事质量，
//!    开关换态影响**功能是否存在**。
//! 2. **不可逆的外部性**。世界暂停可以恢复；开关一旦开过，用户已经领到的礼包、已经签的
//!    生死状、已经看到的功能，关回去也收不回既成事实（公共事实不可回滚，§0.2）。
//! 3. **§0.1 的落点**。「未验证功能默认关闭」这条工程约束若能被任意 operator 撤销，
//!    它就不是约束而是建议。把开闸权限收到最小集合，是这条约束在权限层的兑现。
//!
//! 读放给 `operator` 是因为：运营需要随时看清当前开放范围（尤其**急停时**——
//! 关阀那一刻更需要看得见谁被关了），把读也锁死只会逼人去连数据库。
//! 读端点不泄露任何用户内容，只有开关名/作用域/目标 id/时间窗/理由。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 审计：理由**强制非空**，且每次写都落 `audit_logs`
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 「那天为什么突然开放了」是这套体系必须能回答的问题。因此：
//! - `reason` 空串 → 400（没有「不填理由」这个选项）；
//! - 每次 set/delete 落一条 `audit_logs`，`subject = flag:scope:target`，
//!   `reason` 里带上**变更前后的完整状态**（不是只写新值——只写新值就无法回答「改动了什么」）；
//! - 记录行自身另存 `updated_by`/`updated_at`/`reason`（现状），与 audit_logs（流水）互补。
//!
//! 写成功后**立即** `flags::invalidate(db)`，使本进程下一次读取即生效（不等缓存 TTL）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::flags::{self, FlagCtx, FlagRecord, FlagSource, Resolution};

use super::{audit, require_role, ActionQuery};

// ---------------- 序列化助手 ----------------

fn record_json(r: &FlagRecord) -> Value {
    json!({
        "id": r.id,
        "flag": r.flag,
        "scope": r.scope,
        "targetId": r.target_id,
        "enabled": r.enabled,
        // 0 = 该端不限。
        "startsAt": r.starts_at,
        "endsAt": r.ends_at,
        "updatedBy": r.updated_by,
        "updatedAt": r.updated_at,
        "reason": r.reason,
        "createdAt": r.created_at,
        // 🔴 损坏标记直接下发：这一行会让整个开关 fail-closed 到默认值，运营必须一眼看到。
        "corrupt": r.corrupt,
    })
}

fn source_json(s: &FlagSource) -> Value {
    let mut out = json!({ "kind": s.kind() });
    match s {
        FlagSource::Db { scope, target_id, record_id } => {
            out["scope"] = json!(scope);
            out["targetId"] = json!(target_id);
            out["recordId"] = json!(record_id);
        }
        FlagSource::Env { raw } => out["raw"] = json!(raw),
        FlagSource::Default => {}
        FlagSource::FailClosed { why } => out["why"] = json!(why),
    }
    out
}

/// 某作用域 + 目标构成的解析上下文（写/删的回执里要报「这条记录当下是否真的在生效」）。
fn ctx_for<'a>(scope: &str, target_id: &'a str) -> FlagCtx<'a> {
    match scope {
        flags::SCOPE_USER => FlagCtx::user(target_id),
        flags::SCOPE_WORLD => FlagCtx::world(target_id),
        _ => FlagCtx::global(),
    }
}

fn resolution_json(r: &Resolution) -> Value {
    json!({
        "flag": r.flag,
        "enabled": r.enabled,
        "source": source_json(&r.source),
        // 命中 key 但不在时间窗内、因而被跳过的记录（诊断「我配了怎么没生效」）。
        "skippedOutOfWindow": r.skipped,
    })
}

// ---------------- GET /admin/flags ----------------

#[derive(Debug, Deserialize)]
pub(super) struct ListQuery {
    flag: Option<String>,
}

/// 列出：① 开关登记表（含代码内默认值、归属模块、是否已接线）；② 全部记录；③ 每个开关的
/// **全局生效值**（ctx 为空的解析结果 = 没有任何用户/世界灰度命中时的大盘状态）。
///
/// ③ 是运营最需要的一列：它回答「现在默认对所有人是开还是关」，而 ① 只是代码里的声明值。
pub(super) async fn list_flags(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;

    let records = flags::list_records(&state.db, q.flag.as_deref()).await?;

    let mut defs = Vec::new();
    for d in flags::KNOWN_FLAGS {
        if let Some(f) = &q.flag {
            if f != d.name {
                continue;
            }
        }
        let res = flags::resolve(&state.db, d.name, FlagCtx::global()).await;
        defs.push(json!({
            "name": d.name,
            "owner": d.owner,
            "desc": d.desc,
            // 代码内声明的默认值 = env 未设时的取值，**也是 fail-closed 时返回的安全值**。
            "defaultEnabled": d.default_enabled,
            // false = 该开关目前仍是纯 env 读取，写记录对它暂时无效（见 flags::MIGRATION_NOTES）。
            "wired": d.wired,
            // 全局生效值（无用户/世界灰度时的大盘状态）。
            "globalEffective": res.enabled,
            "globalSource": source_json(&res.source),
        }));
    }

    Ok(Json(json!({
        "flags": defs,
        "records": records.iter().map(record_json).collect::<Vec<_>>(),
        "scopesByPriority": flags::SCOPES_BY_PRIORITY,
        "notes": [
            "解析优先级：按用户 > 按世界 > 全局 > env > 代码内默认值（窄的赢）",
            "记录为空 = 完全按 env 现有语义，所有模块行为不变",
            "wired=false 的开关尚未接入本体系，写记录对其暂时无效",
            "corrupt 非空的记录会让整个开关 fail-closed 到 defaultEnabled",
            flags::MIGRATION_NOTES,
        ],
    })))
}

// ---------------- GET /admin/flags/resolve ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResolveQuery {
    flag: String,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    world_id: Option<String>,
}

/// dry-run：这个开关对这个用户 / 这个世界解析成什么、**从哪一层来的**。
///
/// 这是事故复盘的主力工具：出问题时先问「那一刻它对这个人是什么值、为什么」，
/// 而不是靠人肉推演四层回落链。只读，零副作用。
pub(super) async fn resolve_flag(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(q): Query<ResolveQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;

    let mut ctx = FlagCtx::global();
    if let Some(u) = &q.user_id {
        ctx = ctx.with_user(u);
    }
    if let Some(w) = &q.world_id {
        ctx = ctx.with_world(w);
    }
    let res = flags::resolve(&state.db, &q.flag, ctx).await;

    let mut out = resolution_json(&res);
    out["ctx"] = json!({ "userId": q.user_id, "worldId": q.world_id });
    Ok(Json(out))
}

// ---------------- POST /admin/flags ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetFlagReq {
    flag: String,
    scope: String,
    #[serde(default)]
    target_id: Option<String>,
    enabled: bool,
    /// 生效窗左端（毫秒）；缺省/0 = 立即生效。
    #[serde(default)]
    starts_at: Option<i64>,
    /// 生效窗右端（毫秒）；缺省/0 = 永不过期。
    #[serde(default)]
    ends_at: Option<i64>,
    /// 🔴 变更理由，**必填非空**。
    #[serde(default)]
    reason: Option<String>,
}

/// 设置一条开关记录（upsert）。**admin 专属**（见模块头 RBAC）。
pub(super) async fn set_flag(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(req): Json<SetFlagReq>,
) -> Result<Json<Value>, ApiError> {
    // 🔴 开关写操作 admin 专属：爆炸半径 = 用户可见范围，见模块头。
    require_role(&admin, &[])?;

    // ── 白名单校验：开关名必须已登记 ────────────────────────────────────────
    let Some(def) = flags::find_flag(&req.flag) else {
        let known: Vec<&str> = flags::KNOWN_FLAGS.iter().map(|f| f.name).collect();
        return Err(ApiError::BadRequest(format!(
            "未登记的开关名「{}」。已登记：{}",
            req.flag,
            known.join(", ")
        )));
    };

    // ── 作用域与目标 id ────────────────────────────────────────────────────
    if !flags::SCOPES_BY_PRIORITY.contains(&req.scope.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "作用域非法「{}」。可选：{}",
            req.scope,
            flags::SCOPES_BY_PRIORITY.join(", ")
        )));
    }
    let target_id = req.target_id.clone().unwrap_or_default().trim().to_string();
    if req.scope == flags::SCOPE_GLOBAL {
        if !target_id.is_empty() {
            return Err(ApiError::BadRequest("global 作用域不接受 targetId".into()));
        }
    } else if target_id.is_empty() {
        return Err(ApiError::BadRequest(format!("{} 作用域必须给 targetId", req.scope)));
    }

    // 🔴 **写入期校验目标存在，但删除期不做级联**（与迁移 0036 注释的「不建外键」不矛盾）：
    // 写入期校验挡的是**打错 id**——灰度名单里一个错字会表现为「配了但毫无效果」，
    // 是这套体系最难自查的失败模式。而级联删除会**静默改变开放范围**（删个测试世界顺手
    // 把灰度删了），那是另一回事，坚决不做。残留行由运营在列表页自行清理。
    match req.scope.as_str() {
        flags::SCOPE_USER => {
            let ok = sqlx::query_scalar::<_, i64>("SELECT 1 FROM users WHERE id = $1")
                .bind(&target_id)
                .fetch_optional(&state.db)
                .await?
                .is_some();
            if !ok {
                return Err(ApiError::BadRequest(format!("用户不存在：{target_id}")));
            }
        }
        flags::SCOPE_WORLD => {
            let ok = sqlx::query_scalar::<_, i64>("SELECT 1 FROM worlds WHERE id = $1")
                .bind(&target_id)
                .fetch_optional(&state.db)
                .await?
                .is_some();
            if !ok {
                return Err(ApiError::BadRequest(format!("世界不存在：{target_id}")));
            }
        }
        _ => {}
    }

    // ── 时间窗 ────────────────────────────────────────────────────────────
    let starts_at = req.starts_at.unwrap_or(0);
    let ends_at = req.ends_at.unwrap_or(0);
    if starts_at < 0 || ends_at < 0 {
        return Err(ApiError::BadRequest("时间窗不得为负".into()));
    }
    if starts_at > 0 && ends_at > 0 && starts_at >= ends_at {
        return Err(ApiError::BadRequest(format!(
            "时间窗反转：startsAt({starts_at}) 必须早于 endsAt({ends_at})"
        )));
    }

    // ── 🔴 理由必填 ───────────────────────────────────────────────────────
    let reason = req.reason.clone().unwrap_or_default().trim().to_string();
    if reason.is_empty() {
        return Err(ApiError::BadRequest(
            "reason 必填：开关变更直接改变用户可见范围，无理由的变更无法复盘".into(),
        ));
    }

    // ── 变更前快照（审计要能回答「改动了什么」，只写新值是不够的） ──────────
    let before = flags::list_records(&state.db, Some(&req.flag))
        .await?
        .into_iter()
        .find(|r| r.scope == req.scope && r.target_id == target_id);
    let before_desc = match &before {
        Some(r) => format!("{}[{}~{}]", if r.enabled { "on" } else { "off" }, r.starts_at, r.ends_at),
        None => "<无记录>".to_string(),
    };

    let id = flags::set_flag(
        &state.db,
        flags::SetFlag {
            flag: &req.flag,
            scope: &req.scope,
            target_id: &target_id,
            enabled: req.enabled,
            starts_at,
            ends_at,
            actor_id: &admin.0.user_id,
            reason: &reason,
        },
    )
    .await?;

    // 🔴 立即失效：运营点完开关，本进程下一次读取即生效（不等 TTL）。
    flags::invalidate(&state.db);

    audit(
        &state.db,
        &admin.0,
        "flag.set",
        &format!("{}:{}:{}", req.flag, req.scope, target_id),
        &format!(
            "{} -> {}[{}~{}] | {}",
            before_desc,
            if req.enabled { "on" } else { "off" },
            starts_at,
            ends_at,
            reason
        ),
    )
    .await?;

    // 回执带上「这条记录当下是否真的在生效」——写完就能看到时间窗有没有填对。
    let effective_now = flags::resolve(&state.db, &req.flag, ctx_for(&req.scope, &target_id)).await;

    Ok(Json(json!({
        "id": id,
        "flag": req.flag,
        "scope": req.scope,
        "targetId": target_id,
        "enabled": req.enabled,
        "startsAt": starts_at,
        "endsAt": ends_at,
        "wired": def.wired,
        "effectiveNow": resolution_json(&effective_now),
    })))
}

// ---------------- DELETE /admin/flags/{id} ----------------

/// 删除一条记录：该目标**回落到更宽作用域 / env**（不是「强制关闭」）。**admin 专属**。
pub(super) async fn delete_flag(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    Query(q): Query<ActionQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &[])?;

    let reason = q.reason().trim().to_string();
    if reason.is_empty() {
        return Err(ApiError::BadRequest(
            "reason 必填：开关变更直接改变用户可见范围，无理由的变更无法复盘".into(),
        ));
    }

    // 先取快照，否则删完就没法在审计里说清「删掉的是什么」。
    let rec = flags::get_record(&state.db, &id).await?.ok_or(ApiError::NotFound)?;
    flags::delete_record(&state.db, &id).await?;
    flags::invalidate(&state.db);

    audit(
        &state.db,
        &admin.0,
        "flag.delete",
        &format!("{}:{}:{}", rec.flag, rec.scope, rec.target_id),
        &format!(
            "删除 {}[{}~{}]（回落到更宽作用域/env） | {}",
            if rec.enabled { "on" } else { "off" },
            rec.starts_at,
            rec.ends_at,
            reason
        ),
    )
    .await?;

    // 删除后该目标解析成什么（回执直接告诉运营「现在它变成了什么」）。
    let after = flags::resolve(&state.db, &rec.flag, ctx_for(&rec.scope, &rec.target_id)).await;

    Ok(Json(json!({
        "deleted": true,
        "id": id,
        "flag": rec.flag,
        "scope": rec.scope,
        "targetId": rec.target_id,
        "fallsBackTo": resolution_json(&after),
    })))
}
