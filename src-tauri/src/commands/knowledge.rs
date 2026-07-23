//! P1 命令壳（主循环所有）：知识包导入/蒸馏/检索/绑定/删除。

use muse_engine::knowledge::types::{
    AllowedUse, ChunkStats, KnowledgeBinding, KnowledgePack, PackMode, RetrievedFragment, Retention,
    RightsBasis, UsageLogEntry,
};
use muse_engine::knowledge::{DistillPrompts, KnowledgeSystem};
use muse_engine::model::ModelProfile;
use serde::Deserialize;
use tauri::AppHandle;

use crate::engine_host::build_host;

fn system(app: &AppHandle) -> Result<KnowledgeSystem, String> {
    Ok(KnowledgeSystem::new(build_host(app)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportKnowledgeRequest {
    pub source_path: String,
    pub title: String,
    pub rights_basis: RightsBasis,
    pub allowed_uses: Vec<AllowedUse>,
    pub retention: Retention,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportKnowledgeResponse {
    pub pack: KnowledgePack,
    pub chunk_stats: ChunkStats,
}

#[tauri::command]
pub fn import_knowledge_source(
    app: AppHandle,
    request: ImportKnowledgeRequest,
) -> Result<ImportKnowledgeResponse, String> {
    let (pack, chunk_stats) = system(&app)?
        .import_source(
            &request.source_path,
            &request.title,
            request.rights_basis,
            request.allowed_uses,
            request.retention,
        )
        .map_err(|e| e.to_string())?;
    Ok(ImportKnowledgeResponse { pack, chunk_stats })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistillRequest {
    pub pack_id: String,
    pub mode: PackMode,
    pub profile: ModelProfile,
    /// key: knowledge/mind/value/expression
    pub prompts_by_mode: std::collections::BTreeMap<String, String>,
    pub prompt_version: Option<String>,
}

#[tauri::command]
pub async fn distill_knowledge_pack(app: AppHandle, request: DistillRequest) -> Result<KnowledgePack, String> {
    let sys = system(&app)?;
    let prompts = DistillPrompts {
        system_by_mode: request.prompts_by_mode,
        prompt_version: request.prompt_version.unwrap_or_else(|| "v1".into()),
    };
    let flag = muse_engine::host::CancelFlag::new();
    sys.distill(&request.pack_id, request.mode, &request.profile, &prompts, &flag)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_knowledge(
    app: AppHandle,
    pack_ids: Vec<String>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<RetrievedFragment>, String> {
    system(&app)?.search(&pack_ids, &query, limit.unwrap_or(5)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_knowledge_usage(app: AppHandle, run_id: String) -> Result<Vec<UsageLogEntry>, String> {
    system(&app)?.get_usage(&run_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_knowledge_packs(app: AppHandle) -> Result<Vec<KnowledgePack>, String> {
    system(&app)?.list_packs().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_knowledge_pack(app: AppHandle, pack_id: String) -> Result<(), String> {
    system(&app)?.delete_pack(&pack_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_knowledge_bindings(app: AppHandle) -> Result<Vec<KnowledgeBinding>, String> {
    system(&app)?.list_bindings().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn upsert_knowledge_binding(app: AppHandle, binding: KnowledgeBinding) -> Result<(), String> {
    system(&app)?.upsert_binding(binding).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_knowledge_binding(app: AppHandle, binding_id: String) -> Result<(), String> {
    system(&app)?.remove_binding(&binding_id).map_err(|e| e.to_string())
}
