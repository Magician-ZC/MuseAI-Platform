//! API 错误：稳定 error code，不向客户端暴露内部细节（平台规格 §9.1）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("未认证")]
    Unauthorized,
    #[error("无权限")]
    Forbidden,
    #[error("不存在")]
    NotFound,
    #[error("请求无效: {0}")]
    BadRequest(String),
    #[error("状态冲突: {0}")]
    Conflict(String),
    #[error("幂等键重复但载荷不一致")]
    IdempotencyMismatch,
    #[error("已被风控拦截")]
    RiskBlocked,
    #[error("内部错误")]
    Internal(#[source] anyhow_like::Boxed),
}

/// 轻量内部错误盒（避免引入 anyhow 依赖）。
pub mod anyhow_like {
    pub type Boxed = Box<dyn std::error::Error + Send + Sync>;
}

impl ApiError {
    pub fn internal<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Self::Internal(Box::new(e))
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::BadRequest(_) => "bad_request",
            Self::Conflict(_) => "conflict",
            Self::IdempotencyMismatch => "idempotency_mismatch",
            Self::RiskBlocked => "risk_blocked",
            Self::Internal(_) => "internal",
        }
    }
    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) | Self::IdempotencyMismatch => StatusCode::CONFLICT,
            Self::RiskBlocked => StatusCode::FORBIDDEN,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Internal(e) = &self {
            tracing::error!(error = %e, "internal error");
        }
        let message = match &self {
            Self::Internal(_) => "内部错误".to_string(),
            other => other.to_string(),
        };
        (self.status(), Json(json!({ "error": { "code": self.code(), "message": message } }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::internal(other),
        }
    }
}

impl From<muse_engine::EngineError> for ApiError {
    fn from(e: muse_engine::EngineError) -> Self {
        use muse_engine::EngineError as E;
        match e {
            E::NotFound(_) => Self::NotFound,
            E::Conflict(m) => Self::Conflict(m),
            E::Validation(m) => Self::BadRequest(m),
            other => Self::internal(other),
        }
    }
}
