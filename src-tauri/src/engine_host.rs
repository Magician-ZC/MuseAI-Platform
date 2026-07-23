//! muse-engine 宿主适配器（主循环所有）：Tauri 事件桥 + 数据根目录 + 取消标志注册表。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use muse_engine::host::{CancelFlag, EngineEvent, EngineHost, HostEvents, StdFs, SystemClock};
use muse_engine::model::HttpModelClient;
use tauri::{AppHandle, Emitter};

/// 引擎事件统一走 `engine-event` Tauri 事件（前端 listen 后按 kind 分派）。
pub struct TauriEvents {
    app: AppHandle,
}

impl HostEvents for TauriEvents {
    fn emit(&self, event: EngineEvent) {
        let _ = self.app.emit("engine-event", &event);
    }
}

/// 构建 EngineHost：数据根 = ~/Documents/MuseAI（与现有工作区一致）。
pub fn build_host(app: &AppHandle) -> Result<Arc<EngineHost>, String> {
    let doc_dir = crate::utils::resolve_document_dir(app)?;
    let root = doc_dir.join("MuseAI");
    let model = HttpModelClient::new().map_err(|e| e.to_string())?;
    Ok(Arc::new(EngineHost {
        fs: Arc::new(StdFs::new(root)),
        clock: Arc::new(SystemClock),
        events: Arc::new(TauriEvents { app: app.clone() }),
        model: Arc::new(model),
    }))
}

/// 取消标志注册表：提取任务 / 叙事回合共用（key = taskId / runId）。
static CANCEL_FLAGS: OnceLock<Mutex<HashMap<String, CancelFlag>>> = OnceLock::new();

pub fn cancel_flags() -> &'static Mutex<HashMap<String, CancelFlag>> {
    CANCEL_FLAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_cancel(key: &str) -> CancelFlag {
    let flag = CancelFlag::new();
    cancel_flags().lock().unwrap().insert(key.to_string(), flag.clone());
    flag
}

pub fn trigger_cancel(key: &str) -> bool {
    if let Some(flag) = cancel_flags().lock().unwrap().get(key) {
        flag.cancel();
        true
    } else {
        false
    }
}

pub fn unregister_cancel(key: &str) {
    cancel_flags().lock().unwrap().remove(key);
}
