//! 安全与风控（S3）：角色卡/文本注入检测 + 分层机审管道 + risk_events（平台规格 §14）。
//!
//! - `detect_injection(text)` / `card_scan_text(json)`：注入检测引擎，见 `inject` 子模块。
//!   归一化（去零宽/全角折叠/同形字映射/折叠空白）→ 语义拼接（卡片字段值，非序列化 JSON）→
//!   紧凑串匹配 + 句式判别（第二人称祈使 vs 第三人称叙述）。返回命中规则名 + 命中片段。
//!   短语黑名单仅作辅助信号，生产应叠加模型/分类器主闸（见 inject 模块头注释）。
//! - `moderate_and_queue`：provider 机审 + 注入检测的**唯一入队/记险方**——
//!   Approved 直过 / Pending（含注入命中）进 audit_queue / Rejected 直拒；命中或非过写 risk_events。
//!   调用方（assets/interventions/assembly）只取其返回裁决，不得再自行 INSERT audit_queue / risk。
//! - `record_risk`：统一风控事件落库（其他模块复用；签名稳定）。

use crate::app::AppState;
use crate::error::ApiError;
use crate::providers::ModerationVerdict;

mod inject;
pub use inject::{card_scan_text, detect_injection, InjectionHit};

#[cfg(test)]
pub(crate) mod testkit;

/// 机审 + 注入检测的分层管道（§14）。**唯一**入队/记险方。
///
/// 组合判定：provider Rejected → Rejected（直拒）；provider Pending 或有注入命中 → Pending（进人审队列）；
/// 否则 Approved。副作用（单一写入点，调用方不得重复）：
/// - Pending → INSERT audit_queue（status='open'，带 machine_verdict/machine_hits）；
/// - 非 Approved 或有注入命中 → record_risk（有命中记 'injection'，否则 'moderation'）。
///
/// 返回的裁决已折叠注入命中（Approved+命中 → Pending），调用方据此写自己的领域态即可。
pub async fn moderate_and_queue(
    state: &AppState,
    subject_kind: &str,
    subject_id: &str,
    text: &str,
) -> Result<ModerationVerdict, ApiError> {
    let hits = detect_injection(text);
    let provider = state
        .moderation
        .check_text(text)
        .await
        .map_err(|e| ApiError::internal(std::io::Error::other(e)))?;

    let verdict = match provider {
        ModerationVerdict::Rejected => ModerationVerdict::Rejected,
        ModerationVerdict::Pending => ModerationVerdict::Pending,
        ModerationVerdict::Approved if !hits.is_empty() => ModerationVerdict::Pending,
        ModerationVerdict::Approved => ModerationVerdict::Approved,
    };

    if verdict == ModerationVerdict::Pending {
        sqlx::query(
            "INSERT INTO audit_queue (id, subject_kind, subject_id, machine_verdict, machine_hits, status, created_at) \
             VALUES (?, ?, ?, ?, ?, 'open', ?)",
        )
        .bind(crate::db::new_id("aq"))
        .bind(subject_kind)
        .bind(subject_id)
        .bind(verdict_str(verdict))
        .bind(serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()))
        .bind(crate::db::now_ms())
        .execute(&state.db)
        .await?;
    }

    if verdict != ModerationVerdict::Approved || !hits.is_empty() {
        let kind = if hits.is_empty() { "moderation" } else { "injection" };
        record_risk(
            &state.db,
            None,
            None,
            kind,
            serde_json::json!({
                "subjectKind": subject_kind,
                "subjectId": subject_id,
                "verdict": verdict_str(verdict),
                "providerVerdict": verdict_str(provider),
                "hits": hits,
            }),
        )
        .await?;
    }

    Ok(verdict)
}

pub async fn record_risk(
    db: &sqlx::AnyPool,
    user_id: Option<&str>,
    world_id: Option<&str>,
    kind: &str,
    detail: serde_json::Value,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO risk_events (id, user_id, world_id, kind, detail_json, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(crate::db::new_id("risk"))
    .bind(user_id)
    .bind(world_id)
    .bind(kind)
    .bind(detail.to_string())
    .bind(crate::db::now_ms())
    .execute(db)
    .await?;
    Ok(())
}

pub fn verdict_str(v: ModerationVerdict) -> &'static str {
    match v {
        ModerationVerdict::Approved => "approved",
        ModerationVerdict::Pending => "pending",
        ModerationVerdict::Rejected => "rejected",
    }
}
