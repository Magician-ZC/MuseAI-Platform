use thiserror::Error;

/// 引擎统一错误。`retryable` 语义：调用方可以在不改变输入的情况下重试。
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("序列化错误: {0}")]
    Serde(String),
    #[error("模型调用失败: {message}")]
    Model { message: String, retryable: bool },
    #[error("模型输出不符合要求: {0}")]
    ModelOutput(String),
    #[error("任务已取消")]
    Cancelled,
    #[error("状态冲突: {0}")]
    Conflict(String),
    #[error("校验失败: {0}")]
    Validation(String),
    #[error("未找到: {0}")]
    NotFound(String),
    #[error("预算已耗尽: {0}")]
    BudgetExhausted(String),
}

impl EngineError {
    pub fn io(e: impl std::fmt::Display) -> Self {
        Self::Io(e.to_string())
    }
    pub fn serde(e: impl std::fmt::Display) -> Self {
        Self::Serde(e.to_string())
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Model { retryable: true, .. })
    }
    /// 稳定错误码：跨进程传输与前端分支判断用，不携带内部细节。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Serde(_) => "serde",
            Self::Model { .. } => "model",
            Self::ModelOutput(_) => "model_output",
            Self::Cancelled => "cancelled",
            Self::Conflict(_) => "conflict",
            Self::Validation(_) => "validation",
            Self::NotFound(_) => "not_found",
            Self::BudgetExhausted(_) => "budget",
        }
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e.to_string())
    }
}
