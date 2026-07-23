//! 宿主能力注入层：文件、时钟、事件、取消。
//!
//! 桌面壳与平台后端分别提供实现；引擎内部与测试使用这里的默认实现。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// 时钟。禁止引擎内部直接取系统时间，便于测试与回放。
pub trait HostClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// 引擎对外发射的事件。宿主决定投递通道（Tauri event / WS / 日志）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EngineEvent {
    /// 任务进度事件（规格 §10.2：至少 taskId/revision/stage/itemId?/progress/error?）
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: String,
        revision: u64,
        stage: String,
        item_id: Option<String>,
        /// 0.0–1.0
        progress: f32,
        error: Option<EventError>,
    },
    /// 模型调用可观测记录（DoD：runId/agent/promptVersion/modelId/token/latency/retry/error；不含完整提示词）
    ModelCall(ModelCallLog),
    /// 叙事领域事件（P2 → 平台 §9.4 的 DomainEvent 由 narrative::types 定义，序列化后装入）
    #[serde(rename_all = "camelCase")]
    Narrative { run_id: String, payload: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl EventError {
    pub fn from_engine(e: &EngineError) -> Self {
        Self { code: e.code().to_string(), message: e.to_string(), retryable: e.retryable() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallLog {
    pub run_id: String,
    pub agent: String,
    pub prompt_version: String,
    pub model_id: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub latency_ms: u64,
    pub retries: u32,
    pub error: Option<String>,
}

pub trait HostEvents: Send + Sync {
    fn emit(&self, event: EngineEvent);
}

/// 文件访问。所有路径相对 `data_root`（如 `~/Documents/MuseAI`）。
pub trait HostFs: Send + Sync {
    fn data_root(&self) -> PathBuf;
    fn read(&self, rel: &Path) -> Result<Vec<u8>, EngineError>;
    fn exists(&self, rel: &Path) -> bool;
    /// 原子写：临时文件 + rename 替换；若目标已存在，先把旧内容备份为 `<name>.bak`（保留最近一份）。
    fn write_atomic(&self, rel: &Path, bytes: &[u8]) -> Result<(), EngineError>;
    fn remove(&self, rel: &Path) -> Result<(), EngineError>;
    fn list(&self, rel_dir: &Path) -> Result<Vec<PathBuf>, EngineError>;
}

/// 协作式取消标志。跨任务传递用 Arc。
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<(), EngineError> {
        if self.is_cancelled() { Err(EngineError::Cancelled) } else { Ok(()) }
    }
}

/// 标准库文件实现：宿主只需给出根目录（不依赖任何框架，src-tauri / server 均可直接使用）。
pub struct StdFs {
    root: PathBuf,
}

impl StdFs {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn abs(&self, rel: &Path) -> Result<PathBuf, EngineError> {
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(EngineError::Validation(format!("非法相对路径: {}", rel.display())));
        }
        Ok(self.root.join(rel))
    }
}

impl HostFs for StdFs {
    fn data_root(&self) -> PathBuf {
        self.root.clone()
    }
    fn read(&self, rel: &Path) -> Result<Vec<u8>, EngineError> {
        Ok(std::fs::read(self.abs(rel)?)?)
    }
    fn exists(&self, rel: &Path) -> bool {
        self.abs(rel).map(|p| p.exists()).unwrap_or(false)
    }
    fn write_atomic(&self, rel: &Path, bytes: &[u8]) -> Result<(), EngineError> {
        let target = self.abs(rel)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if target.exists() {
            let backup = target.with_extension(format!(
                "{}.bak",
                target.extension().and_then(|e| e.to_str()).unwrap_or("dat")
            ));
            let _ = std::fs::copy(&target, &backup);
        }
        let tmp = target.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &target)?;
        Ok(())
    }
    fn remove(&self, rel: &Path) -> Result<(), EngineError> {
        let p = self.abs(rel)?;
        if p.is_dir() {
            std::fs::remove_dir_all(p)?;
        } else if p.exists() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
    fn list(&self, rel_dir: &Path) -> Result<Vec<PathBuf>, EngineError> {
        let dir = self.abs(rel_dir)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if let Ok(rel) = entry.path().strip_prefix(&self.root) {
                out.push(rel.to_path_buf());
            }
        }
        out.sort();
        Ok(out)
    }
}

/// 系统时钟实现。
pub struct SystemClock;

impl HostClock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// 丢弃事件的空实现（测试与纯批处理用）。
pub struct NullEvents;

impl HostEvents for NullEvents {
    fn emit(&self, _event: EngineEvent) {}
}

/// 聚合宿主：引擎各管线的统一注入点。
pub struct EngineHost {
    pub fs: Arc<dyn HostFs>,
    pub clock: Arc<dyn HostClock>,
    pub events: Arc<dyn HostEvents>,
    pub model: Arc<dyn crate::model::ModelClient>,
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 内存文件系统（测试用）。
    #[derive(Default)]
    pub struct MemFs {
        files: Mutex<HashMap<PathBuf, Vec<u8>>>,
    }

    impl HostFs for MemFs {
        fn data_root(&self) -> PathBuf {
            PathBuf::from("/mem")
        }
        fn read(&self, rel: &Path) -> Result<Vec<u8>, EngineError> {
            self.files
                .lock()
                .unwrap()
                .get(rel)
                .cloned()
                .ok_or_else(|| EngineError::NotFound(rel.display().to_string()))
        }
        fn exists(&self, rel: &Path) -> bool {
            self.files.lock().unwrap().contains_key(rel)
        }
        fn write_atomic(&self, rel: &Path, bytes: &[u8]) -> Result<(), EngineError> {
            self.files.lock().unwrap().insert(rel.to_path_buf(), bytes.to_vec());
            Ok(())
        }
        fn remove(&self, rel: &Path) -> Result<(), EngineError> {
            self.files.lock().unwrap().remove(rel);
            Ok(())
        }
        fn list(&self, rel_dir: &Path) -> Result<Vec<PathBuf>, EngineError> {
            let files = self.files.lock().unwrap();
            let mut out: Vec<PathBuf> =
                files.keys().filter(|p| p.starts_with(rel_dir)).cloned().collect();
            out.sort();
            Ok(out)
        }
    }

    /// 固定时钟（测试用）。
    pub struct FixedClock(pub i64);
    impl HostClock for FixedClock {
        fn now_ms(&self) -> i64 {
            self.0
        }
    }

    /// 收集事件（测试用）。
    #[derive(Default)]
    pub struct CollectEvents(pub Mutex<Vec<EngineEvent>>);
    impl HostEvents for CollectEvents {
        fn emit(&self, event: EngineEvent) {
            self.0.lock().unwrap().push(event);
        }
    }
}
