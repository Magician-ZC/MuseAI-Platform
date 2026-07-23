//! P3 世界提取命令壳（桌面独占，主循环所有）：世界内容超集提取管线。业务逻辑全部在 muse-engine
//! `WorldExtractionPipeline`，此处只做参数转换（DTO→引擎请求）与任务生命周期（spawn / 取消 / 事件回传）。
//! 逐字仿 `character_v2.rs`；差异见各命令注释。世界提取不进 `mobile_server.rs`/`appInvoke` 手机映射。

use muse_engine::character::types::RosterEntry;
use muse_engine::model::ModelProfile;
use muse_engine::world::types::{WorldExtractionTask, WorldRosterEntry};
use muse_engine::world::{
    WorldCoverageReport, WorldExtractionPipeline, WorldExtractionRequest, WorldPrompts,
};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::engine_host::{build_host, register_cancel, trigger_cancel, unregister_cancel};

/// 世界提取请求 DTO（camelCase，镜像引擎 serde）。10 段 system prompt 对应 `WorldPrompts`。
/// `ModelProfile` 直接反序列化前端传入——后端对配置无状态，凭据每次请求组装传入不落盘。
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WorldExtractionRequestDto {
    pub work_title: String,
    pub source_path: String,
    pub profile: ModelProfile,
    pub scan_prompt: String,
    pub char_merge_prompt: String,
    pub loc_merge_prompt: String,
    pub item_merge_prompt: String,
    pub char_tiering_prompt: String,
    pub char_synthesis_prompt: String,
    pub location_synthesis_prompt: String,
    pub item_synthesis_prompt: String,
    pub plot_synthesis_prompt: String,
    pub ending_synthesis_prompt: String,
    pub prompt_version: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub concurrency: Option<usize>,
}

impl WorldExtractionRequestDto {
    fn into_engine(self) -> WorldExtractionRequest {
        WorldExtractionRequest {
            work_title: self.work_title,
            source_path: self.source_path,
            profile: self.profile,
            prompts: WorldPrompts {
                scan_system: self.scan_prompt,
                char_merge_system: self.char_merge_prompt,
                loc_merge_system: self.loc_merge_prompt,
                item_merge_system: self.item_merge_prompt,
                char_tiering_system: self.char_tiering_prompt,
                char_synthesis_system: self.char_synthesis_prompt,
                location_synthesis_system: self.location_synthesis_prompt,
                item_synthesis_system: self.item_synthesis_prompt,
                plot_synthesis_system: self.plot_synthesis_prompt,
                ending_synthesis_system: self.ending_synthesis_prompt,
                prompt_version: self.prompt_version.unwrap_or_else(|| "v1".into()),
            },
            temperature: self.temperature.unwrap_or(0.0),
            max_output_tokens: self.max_output_tokens.unwrap_or(8192),
            concurrency: self.concurrency.unwrap_or(3).clamp(1, 8),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedTask {
    pub task_id: String,
}

/// 起提取任务：切章落盘（同步）后 spawn `run_until_review`（scan→merge→tiering→Review）。
/// 进度事件由引擎内部 `host.events.emit(WorldTaskStore::progress_event(...))`（`EngineEvent::Task`,
/// kind=`task`）发出，命令壳无需额外发事件；前端订阅 `engine-event` 过滤 `kind==='task'`。
#[tauri::command]
pub async fn start_world_extraction(
    app: AppHandle,
    request: WorldExtractionRequestDto,
) -> Result<StartedTask, String> {
    let host = build_host(&app)?;
    let pipeline = WorldExtractionPipeline::new(host);
    let engine_request = request.into_engine();
    let task = pipeline.create_task(&engine_request).map_err(|e| e.to_string())?;
    let task_id = task.task_id.clone();
    let flag = register_cancel(&task_id);
    let spawn_task_id = task_id.clone();
    tauri::async_runtime::spawn(async move {
        let _ = pipeline.run_until_review(&spawn_task_id, &engine_request, &flag).await;
        unregister_cancel(&spawn_task_id);
    });
    Ok(StartedTask { task_id })
}

#[tauri::command]
pub fn get_world_extraction_task(
    app: AppHandle,
    task_id: String,
) -> Result<WorldExtractionTask, String> {
    let pipeline = WorldExtractionPipeline::new(build_host(&app)?);
    pipeline.get_task(&task_id).map_err(|e| e.to_string())
}

/// 用户确认三条 roster（character/location/item）；带 revision CAS + character dna_status 归一。
/// plot_beats/ending_clues 是 Review 阶段自动派生的全书级草稿，用户不确认、随合成透传，不进参数。
#[tauri::command]
pub fn confirm_world_rosters(
    app: AppHandle,
    task_id: String,
    expected_revision: u64,
    characters: Vec<RosterEntry>,
    locations: Vec<WorldRosterEntry>,
    items: Vec<WorldRosterEntry>,
) -> Result<WorldExtractionTask, String> {
    let pipeline = WorldExtractionPipeline::new(build_host(&app)?);
    pipeline
        .confirm_rosters(&task_id, expected_revision, characters, locations, items)
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisStarted {
    pub run_id: String,
}

/// 合成世界内容超集（确认后）。含逐 NPC 串行 DNA 合成 + 地点/道具/剧情/结局多次模型调用，长任务，
/// 故用 spawn+事件回传而非同步 await。进度经 `EngineEvent::Task`；完成时命令壳手工发 `Narrative`
/// （kind=`worldAssembled` 携 `WorldSkeletonDraft`；失败为 `worldSynthesisFailed`），前端据此发布/报错。
#[tauri::command]
pub async fn start_world_synthesis(
    app: AppHandle,
    task_id: String,
    request: WorldExtractionRequestDto,
) -> Result<SynthesisStarted, String> {
    let host = build_host(&app)?;
    let events = host.events.clone();
    let pipeline = WorldExtractionPipeline::new(host);
    let engine_request = request.into_engine();
    let run_id = format!("wsynth-{task_id}");
    let flag = register_cancel(&run_id);
    let spawn_run_id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = pipeline.synthesize_superset(&task_id, &engine_request, &flag).await;
        let payload = match &result {
            Ok(draft) => {
                serde_json::json!({ "kind": "worldAssembled", "taskId": task_id, "draft": draft })
            }
            Err(e) => serde_json::json!({
                "kind": "worldSynthesisFailed",
                "taskId": task_id,
                "code": e.code(),
                "message": e.to_string(),
            }),
        };
        events.emit(muse_engine::host::EngineEvent::Narrative { run_id: spawn_run_id.clone(), payload });
        unregister_cancel(&spawn_run_id);
    });
    Ok(SynthesisStarted { run_id })
}

#[tauri::command]
pub fn cancel_world_extraction(app: AppHandle, task_id: String) -> Result<bool, String> {
    let cancelled = trigger_cancel(&task_id) | trigger_cancel(&format!("wsynth-{task_id}"));
    let pipeline = WorldExtractionPipeline::new(build_host(&app)?);
    let _ = pipeline.cancel(&task_id);
    Ok(cancelled)
}

#[tauri::command]
pub fn get_world_coverage_report(
    app: AppHandle,
    task_id: String,
) -> Result<WorldCoverageReport, String> {
    let pipeline = WorldExtractionPipeline::new(build_host(&app)?);
    pipeline.coverage_report(&task_id).map_err(|e| e.to_string())
}
