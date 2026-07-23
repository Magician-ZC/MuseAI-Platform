//! 高光切片管线（S5，feature=arena，agent-S5 填）：TTS(DevTts) + 条漫式分镜脚本 → 占位产物入对象存储。
//! 高光判定：仲裁标记 impact 大的事件；按需生成，不进 tick 关键路径。

use crate::app::AppState;
use crate::error::ApiError;

pub async fn generate_clip(state: &AppState, world_id: &str, event_id: &str) -> Result<String, ApiError> {
    let _ = (state, world_id, event_id);
    todo!("agent-S5: generate_clip → 返回对象存储 key")
}
