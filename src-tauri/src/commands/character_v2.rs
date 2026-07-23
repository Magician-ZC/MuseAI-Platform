//! P0 命令壳（主循环所有）：提取管线 / 角色评测。业务逻辑全部在 muse-engine，此处只做参数转换与任务生命周期。

use muse_engine::character::types::{
    CharacterCardV2, CoverageReport, ExtractionTask, RosterEntry, StressTestReport, SwapTestReport,
};
use muse_engine::character::{CharacterPrompts, ExtractionPipeline, ExtractionRequest};
use muse_engine::model::ModelProfile;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::engine_host::{build_host, register_cancel, trigger_cancel, unregister_cancel};

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionRequestDto {
    pub work_title: String,
    pub source_path: String,
    pub profile: ModelProfile,
    pub scan_prompt: String,
    pub merge_prompt: String,
    pub tiering_prompt: String,
    pub synthesis_prompt: String,
    pub prompt_version: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub concurrency: Option<usize>,
}

impl ExtractionRequestDto {
    fn into_engine(self) -> ExtractionRequest {
        ExtractionRequest {
            work_title: self.work_title,
            source_path: self.source_path,
            profile: self.profile,
            prompts: CharacterPrompts {
                scan_system: self.scan_prompt,
                merge_system: self.merge_prompt,
                tiering_system: self.tiering_prompt,
                synthesis_system: self.synthesis_prompt,
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

#[tauri::command]
pub async fn start_character_extraction(
    app: AppHandle,
    request: ExtractionRequestDto,
) -> Result<StartedTask, String> {
    let host = build_host(&app)?;
    let pipeline = ExtractionPipeline::new(host);
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
pub fn get_character_extraction_task(app: AppHandle, task_id: String) -> Result<ExtractionTask, String> {
    let pipeline = ExtractionPipeline::new(build_host(&app)?);
    pipeline.get_task(&task_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn confirm_character_roster(
    app: AppHandle,
    task_id: String,
    expected_revision: u64,
    roster: Vec<RosterEntry>,
) -> Result<ExtractionTask, String> {
    let pipeline = ExtractionPipeline::new(build_host(&app)?);
    pipeline.confirm_roster(&task_id, expected_revision, roster).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisStarted {
    pub run_id: String,
}

/// 合成完成后通过 `engine-event`(Narrative payload kind=synthesisDone) 携带卡片 JSON，前端入 partner store。
#[tauri::command]
pub async fn start_character_dna_synthesis(
    app: AppHandle,
    task_id: String,
    request: ExtractionRequestDto,
    keys: Vec<String>,
) -> Result<SynthesisStarted, String> {
    let host = build_host(&app)?;
    let events = host.events.clone();
    let pipeline = ExtractionPipeline::new(host);
    let engine_request = request.into_engine();
    let run_id = format!("synth-{task_id}");
    let flag = register_cancel(&run_id);
    let spawn_run_id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = pipeline.synthesize(&task_id, &engine_request, &keys, &flag).await;
        let payload = match &result {
            Ok(cards) => serde_json::json!({ "kind": "synthesisDone", "taskId": task_id, "cards": cards }),
            Err(e) => serde_json::json!({ "kind": "synthesisFailed", "taskId": task_id, "code": e.code(), "message": e.to_string() }),
        };
        events.emit(muse_engine::host::EngineEvent::Narrative { run_id: spawn_run_id.clone(), payload });
        unregister_cancel(&spawn_run_id);
    });
    Ok(SynthesisStarted { run_id })
}

#[tauri::command]
pub fn cancel_character_extraction(app: AppHandle, task_id: String) -> Result<bool, String> {
    let cancelled = trigger_cancel(&task_id) | trigger_cancel(&format!("synth-{task_id}"));
    let pipeline = ExtractionPipeline::new(build_host(&app)?);
    let _ = pipeline.cancel(&task_id);
    Ok(cancelled)
}

#[tauri::command]
pub fn get_extraction_coverage_report(app: AppHandle, task_id: String) -> Result<CoverageReport, String> {
    let pipeline = ExtractionPipeline::new(build_host(&app)?);
    pipeline.coverage_report(&task_id).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapTestRequestDto {
    pub profile: ModelProfile,
    pub swap_prompt: String,
    pub stress_prompt: String,
    pub prompt_version: Option<String>,
    pub card_a: CharacterCardV2,
    pub card_b: Option<CharacterCardV2>,
    pub scenario: Option<String>,
    pub scenarios: Option<Vec<String>>,
}

#[tauri::command]
pub async fn run_character_swap_test(app: AppHandle, request: SwapTestRequestDto) -> Result<SwapTestReport, String> {
    let host = build_host(&app)?;
    let prompts = muse_engine::character::evaluation::EvalPrompts {
        swap_system: request.swap_prompt,
        stress_system: request.stress_prompt,
        prompt_version: request.prompt_version.unwrap_or_else(|| "v1".into()),
    };
    let card_b = request.card_b.ok_or("互换测试需要两张角色卡")?;
    let scenario = request.scenario.unwrap_or_else(|| "一个陌生的现代城市清晨，两人同时收到一封威胁信。".into());
    let flag = muse_engine::host::CancelFlag::new();
    muse_engine::character::evaluation::run_swap_test(
        &host, &request.profile, &prompts, &request.card_a, &card_b, &scenario, &flag,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_character_stress_test(
    app: AppHandle,
    request: SwapTestRequestDto,
) -> Result<StressTestReport, String> {
    let host = build_host(&app)?;
    let prompts = muse_engine::character::evaluation::EvalPrompts {
        swap_system: request.swap_prompt,
        stress_system: request.stress_prompt,
        prompt_version: request.prompt_version.unwrap_or_else(|| "v1".into()),
    };
    let scenarios = request.scenarios.unwrap_or_default();
    let flag = muse_engine::host::CancelFlag::new();
    muse_engine::character::evaluation::run_stress_test(
        &host, &request.profile, &prompts, &request.card_a, &scenarios, &flag,
    )
    .await
    .map_err(|e| e.to_string())
}
