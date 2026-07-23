//! P2 命令壳（主循环所有）：叙事运行初始化/预估/回合/快照分支/锁定。

use std::collections::BTreeMap;

use muse_engine::character::types::CharacterCardV2;
use muse_engine::knowledge::types::RetrievedFragment;
use muse_engine::model::ModelProfile;
use muse_engine::narrative::state::NarrativeStore;
use muse_engine::narrative::types::{
    CostEstimate, NarrativeState, RoundBudget, RunMode, SceneRecord,
};
use muse_engine::narrative::{ModelRoutes, NarrativeEngine, NarrativePrompts, RoundInput};
use serde::Deserialize;
use tauri::AppHandle;

use crate::engine_host::{build_host, register_cancel, trigger_cancel, unregister_cancel};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NarrativePromptsDto {
    pub director: String,
    pub decide: String,
    pub arbiter: String,
    pub writer: String,
    pub critic: String,
    pub prompt_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoutesDto {
    pub default: ModelProfile,
    pub decide: Option<ModelProfile>,
    pub arbiter: Option<ModelProfile>,
    pub writer: Option<ModelProfile>,
    pub critic: Option<ModelProfile>,
    pub director: Option<ModelProfile>,
}

impl ModelRoutesDto {
    fn into_engine(self) -> ModelRoutes {
        ModelRoutes {
            default: self.default,
            decide: self.decide,
            arbiter: self.arbiter,
            writer: self.writer,
            critic: self.critic,
            director: self.director,
        }
    }
}

#[tauri::command]
pub fn narrative_init_run(app: AppHandle, state: NarrativeState) -> Result<NarrativeState, String> {
    let host = build_host(&app)?;
    let store = NarrativeStore::new(host.fs.clone());
    store.init(&state).map_err(|e| e.to_string())?;
    Ok(state)
}

#[tauri::command]
pub fn narrative_get_state(app: AppHandle, run_id: String) -> Result<NarrativeState, String> {
    let host = build_host(&app)?;
    NarrativeStore::new(host.fs.clone()).load(&run_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn narrative_estimate(
    app: AppHandle,
    active_count: u32,
    max_output_tokens: u32,
    scenes: u32,
) -> Result<CostEstimate, String> {
    let engine = NarrativeEngine::new(build_host(&app)?);
    Ok(engine.estimate(active_count, max_output_tokens, scenes))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundRequestDto {
    pub run_id: String,
    pub mode: RunMode,
    pub routes: ModelRoutesDto,
    pub prompts: NarrativePromptsDto,
    pub active_cards: BTreeMap<String, CharacterCardV2>,
    pub other_cards_brief: BTreeMap<String, String>,
    pub whispers: BTreeMap<String, String>,
    pub fragments: BTreeMap<String, Vec<RetrievedFragment>>,
    pub temperature_decide: Option<f32>,
    pub temperature_writer: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub budget: RoundBudget,
    /// 已获批的不可逆结果 subject（可选；桌面壳默认空 = 全部门控，需显式授权才落定）
    #[serde(default)]
    pub approved_consents: Option<Vec<String>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundStarted {
    pub round_id: String,
}

/// 回合结果经 `engine-event`(Narrative payload kind=roundDone/roundBlocked/roundFailed) 下发。
#[tauri::command]
pub async fn start_narrative_round(app: AppHandle, request: RoundRequestDto) -> Result<RoundStarted, String> {
    let host = build_host(&app)?;
    let events = host.events.clone();
    let engine = NarrativeEngine::new(host);
    let round_id = format!("round-{}", uuid_like());
    let flag = register_cancel(&round_id);
    let spawn_round_id = round_id.clone();
    tauri::async_runtime::spawn(async move {
        let routes = request.routes.into_engine();
        let prompts = NarrativePrompts {
            director_system: request.prompts.director,
            decide_system: request.prompts.decide,
            arbiter_system: request.prompts.arbiter,
            writer_system: request.prompts.writer,
            critic_system: request.prompts.critic,
            prompt_version: request.prompts.prompt_version.unwrap_or_else(|| "v1".into()),
        };
        let input = RoundInput {
            run_id: request.run_id.clone(),
            mode: request.mode,
            active_cards: request.active_cards,
            other_cards_brief: request.other_cards_brief,
            whispers: request.whispers,
            fragments: request.fragments,
            temperature_decide: request.temperature_decide.unwrap_or(0.0),
            temperature_writer: request.temperature_writer.unwrap_or(0.8),
            max_output_tokens: request.max_output_tokens.unwrap_or(8192),
            budget: request.budget,
            approved_consents: request.approved_consents.unwrap_or_default(),
        };
        let result = engine.run_round(&routes, &prompts, input, &flag).await;
        let payload = match &result {
            Ok(outcome) => {
                if let Some(reason) = &outcome.blocked {
                    serde_json::json!({ "kind": "roundBlocked", "runId": request.run_id, "reason": reason })
                } else {
                    serde_json::json!({
                        "kind": "roundDone",
                        "runId": request.run_id,
                        "sceneId": outcome.scene.scene_id,
                        "scene": outcome.scene,
                        "critic": outcome.critic,
                        "spentTokens": outcome.budget.spent_tokens,
                    })
                }
            }
            Err(e) => serde_json::json!({ "kind": "roundFailed", "runId": request.run_id, "code": e.code(), "message": e.to_string() }),
        };
        events.emit(muse_engine::host::EngineEvent::Narrative { run_id: spawn_round_id.clone(), payload });
        unregister_cancel(&spawn_round_id);
    });
    Ok(RoundStarted { round_id })
}

#[tauri::command]
pub fn cancel_narrative_round(round_id: String) -> bool {
    trigger_cancel(&round_id)
}

#[tauri::command]
pub fn narrative_list_scenes(app: AppHandle, run_id: String) -> Result<Vec<String>, String> {
    let host = build_host(&app)?;
    NarrativeStore::new(host.fs.clone()).list_scene_ids(&run_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn narrative_get_scene(app: AppHandle, run_id: String, scene_id: String) -> Result<SceneRecord, String> {
    let host = build_host(&app)?;
    NarrativeStore::new(host.fs.clone()).load_scene(&run_id, &scene_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn narrative_lock_scenes(
    app: AppHandle,
    run_id: String,
    scene_ids: Vec<String>,
) -> Result<NarrativeState, String> {
    let host = build_host(&app)?;
    NarrativeStore::new(host.fs.clone()).lock_scenes(&run_id, &scene_ids).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn narrative_take_snapshot(
    app: AppHandle,
    run_id: String,
    at_scene_id: String,
) -> Result<muse_engine::narrative::snapshot::Snapshot, String> {
    let host = build_host(&app)?;
    let state = NarrativeStore::new(host.fs.clone()).load(&run_id).map_err(|e| e.to_string())?;
    muse_engine::narrative::snapshot::take_snapshot(&host.fs, host.clock.now_ms(), &at_scene_id, &state)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn narrative_branch(
    app: AppHandle,
    snapshot_id: String,
    source_run_id: String,
    new_run_id: String,
) -> Result<NarrativeState, String> {
    let host = build_host(&app)?;
    muse_engine::narrative::snapshot::branch_from(
        &host.fs,
        host.clock.now_ms(),
        &snapshot_id,
        &source_run_id,
        &new_run_id,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn narrative_list_snapshots(
    app: AppHandle,
    run_id: String,
) -> Result<Vec<muse_engine::narrative::snapshot::Snapshot>, String> {
    let host = build_host(&app)?;
    muse_engine::narrative::snapshot::list_snapshots(&host.fs, &run_id).map_err(|e| e.to_string())
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{n:x}")
}
