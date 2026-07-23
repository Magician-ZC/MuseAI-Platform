//! P3 Phase4 世界提取管线（WorldExtractionPipeline）：仿 `character::ExtractionPipeline` 的任务化多阶段模式，
//! 从一部原文产出「世界内容超集」（worldCharacters / locations / worldItems / 多剧情线主线段 / 结局池），
//! 对齐 server `assembly::Skeleton` 结构，含足量冗余供下游副本采样（防刷，见 docs/build/rules-anti-farming.md）。
//!
//! 阶段：create_task（切章，复用 `chapters::split_chapters`）→ scan（逐章扫全 kind 世界实体）→
//! merge（character 复用 `merge::rule_merge`/`model_merge`；location/item 走 `entities` 归并；plot/ending 暂存）→
//! tiering（仅 character，复用 `tiering`）→ Review → synthesize_superset（character 复用 `synthesis`；
//! location/item/plot/ending 各自合成 → `superset::assemble` 打超集元数据）。
//!
//! Prompt 全由 caller 传入（`WorldPrompts`，后端无状态）。断点恢复/幂等语义照搬 character。

pub mod discovery;
pub mod entities;
pub mod plot;
pub mod superset;
pub mod task;
pub mod types;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::character::types::{
    ChapterDiscovery, ChapterEntry, ChapterStatus, CharacterMention, DnaStatus, RosterEntry, TaskError,
};
use crate::character::{chapters, evidence, merge, synthesis, tiering, CharacterPrompts};
use crate::host::{CancelFlag, EngineHost, EventError};
use crate::model::ModelProfile;
use crate::store::{content_hash, new_id};
use crate::EngineError;

use entities::EntityMention;
use task::WorldTaskStore;
use types::*;

/// 管线版本：切章启发式 / prompt 契约 / 分片格式变化时递增。
pub const WORLD_PIPELINE_VERSION: &str = "p3w-1";

/// 各环节 prompt 由调用方传入（默认值维护在前端 settings store，后端无配置状态）。
#[derive(Debug, Clone)]
pub struct WorldPrompts {
    pub scan_system: String,
    pub char_merge_system: String,
    pub loc_merge_system: String,
    pub item_merge_system: String,
    pub char_tiering_system: String,
    pub char_synthesis_system: String,
    pub location_synthesis_system: String,
    pub item_synthesis_system: String,
    pub plot_synthesis_system: String,
    pub ending_synthesis_system: String,
    pub prompt_version: String,
}

impl WorldPrompts {
    /// 全环节同一 system（测试便利）。
    pub fn uniform(s: &str) -> Self {
        Self {
            scan_system: s.into(),
            char_merge_system: s.into(),
            loc_merge_system: s.into(),
            item_merge_system: s.into(),
            char_tiering_system: s.into(),
            char_synthesis_system: s.into(),
            location_synthesis_system: s.into(),
            item_synthesis_system: s.into(),
            plot_synthesis_system: s.into(),
            ending_synthesis_system: s.into(),
            prompt_version: "v1".into(),
        }
    }

    /// 复用 character 子管线（merge/tiering/synthesis）所需的 `CharacterPrompts` 适配。
    fn as_character_prompts(&self) -> CharacterPrompts {
        CharacterPrompts {
            scan_system: self.scan_system.clone(),
            merge_system: self.char_merge_system.clone(),
            tiering_system: self.char_tiering_system.clone(),
            synthesis_system: self.char_synthesis_system.clone(),
            prompt_version: self.prompt_version.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorldExtractionRequest {
    pub work_title: String,
    pub source_path: String,
    pub profile: ModelProfile,
    pub prompts: WorldPrompts,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub concurrency: usize,
}

/// 世界提取管线编排器。所有阶段读写统一走 `WorldTaskStore`，事件走 host.events。
pub struct WorldExtractionPipeline {
    pub host: Arc<EngineHost>,
}

impl WorldExtractionPipeline {
    pub fn new(host: Arc<EngineHost>) -> Self {
        Self { host }
    }

    /// 创建任务：读源文件、指纹、切章（复用 character 切章）、落盘（stage=Scan）。不发起模型调用。
    pub fn create_task(
        &self,
        request: &WorldExtractionRequest,
    ) -> Result<WorldExtractionTask, EngineError> {
        let bytes = std::fs::read(&request.source_path).map_err(EngineError::io)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let fingerprint_hash = content_hash(&bytes);
        let (size, modified_at) = match std::fs::metadata(&request.source_path) {
            Ok(md) => {
                let modified = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or_else(|| self.host.clock.now_ms());
                (md.len(), modified)
            }
            Err(_) => (bytes.len() as u64, self.host.clock.now_ms()),
        };

        let chapters = chapters::split_chapters(&text, 8000)?;
        if chapters.is_empty() {
            return Err(EngineError::Validation("源文件为空或无可提取内容".into()));
        }

        let now = self.host.clock.now_ms();
        let taskv = WorldExtractionTask {
            schema_version: 1,
            task_id: new_id("wtask"),
            work_title: request.work_title.clone(),
            source_path: request.source_path.clone(),
            source_fingerprint: crate::character::types::SourceFingerprint {
                size,
                modified_at,
                content_hash: fingerprint_hash,
            },
            pipeline_version: WORLD_PIPELINE_VERSION.to_string(),
            chapters,
            character_roster: Vec::new(),
            location_roster: Vec::new(),
            item_roster: Vec::new(),
            plot_beats: Vec::new(),
            ending_clues: Vec::new(),
            stage: WorldStage::Scan,
            revision: 0,
            created_at: now,
            updated_at: now,
        };
        WorldTaskStore::new(self.host.fs.clone()).create(&taskv)?;
        Ok(taskv)
    }

    /// 运行/恢复任务至 Review：scan（并发逐章）→ merge（分维度）→ tiering（仅 character）。
    pub async fn run_until_review(
        &self,
        task_id: &str,
        request: &WorldExtractionRequest,
        cancel: &CancelFlag,
    ) -> Result<WorldExtractionTask, EngineError> {
        let host: &EngineHost = self.host.as_ref();
        let store = WorldTaskStore::new(self.host.fs.clone());

        let bytes = std::fs::read(&request.source_path).map_err(EngineError::io)?;
        let cur_hash = content_hash(&bytes);
        let text = String::from_utf8_lossy(&bytes).into_owned();

        let mut cur = store.prepare_resume(task_id, &cur_hash, WORLD_PIPELINE_VERSION)?;

        // 已过归并阶段 → 幂等返回。
        if matches!(
            cur.stage,
            WorldStage::Review | WorldStage::Synthesis | WorldStage::Assembled | WorldStage::Done
        ) {
            return Ok(cur);
        }
        if !matches!(cur.stage, WorldStage::Scan) {
            cur = store.update(task_id, cur.revision, |t| t.stage = WorldStage::Scan)?;
        }

        // 待扫描章节：预取正文切片。
        let scan_list: Vec<(usize, ChapterEntry, String)> = cur
            .chapters
            .iter()
            .enumerate()
            .filter(|(_, c)| !matches!(c.status, ChapterStatus::Scanned))
            .map(|(i, c)| (i, c.clone(), chapters::chapter_text(&text, c.char_range).to_string()))
            .collect();

        if !scan_list.is_empty() {
            let profile = &request.profile;
            let prompts = &request.prompts;
            let temperature = request.temperature;
            let max = request.max_output_tokens;
            let mut pipeline_err: Option<EngineError> = None;
            let mut cancelled = false;

            let futures: Vec<_> = scan_list
                .iter()
                .map(|(idx, ch, body)| async move {
                    let r = discovery::scan_world_chapter(
                        host, profile, prompts, temperature, max, task_id, ch, body.as_str(), cancel,
                    )
                    .await;
                    (*idx, r)
                })
                .collect();

            crate::character::run_bounded_each(futures, request.concurrency, |(chap_idx, res)| {
                if pipeline_err.is_some() {
                    return;
                }
                let chapter_id = cur.chapters[chap_idx].id.clone();
                match res {
                    Ok(discovery) => {
                        let saved =
                            store.save_discovery(task_id, &chapter_id, &discovery).and_then(|key| {
                                store.update(task_id, cur.revision, move |t| {
                                    if let Some(c) = t.chapters.get_mut(chap_idx) {
                                        c.status = ChapterStatus::Scanned;
                                        c.discovery_store_key = Some(key);
                                        c.error = None;
                                    }
                                })
                            });
                        match saved {
                            Ok(updated) => {
                                cur = updated;
                                host.events.emit(WorldTaskStore::progress_event(&cur, Some(chapter_id), None));
                            }
                            Err(e) => pipeline_err = Some(e),
                        }
                    }
                    Err(EngineError::Cancelled) => cancelled = true,
                    Err(e) => {
                        let te = TaskError { code: e.code().into(), message: e.to_string(), retryable: e.retryable() };
                        let ev = EventError::from_engine(&e);
                        match store.update(task_id, cur.revision, move |t| {
                            if let Some(c) = t.chapters.get_mut(chap_idx) {
                                c.status = ChapterStatus::Failed;
                                c.attempt += 1;
                                c.error = Some(te);
                            }
                        }) {
                            Ok(updated) => {
                                cur = updated;
                                host.events.emit(WorldTaskStore::progress_event(&cur, Some(chapter_id), Some(ev)));
                            }
                            Err(e2) => pipeline_err = Some(e2),
                        }
                    }
                }
            })
            .await;

            if let Some(e) = pipeline_err {
                return Err(e);
            }
            if cancelled {
                let updated = store.update(task_id, cur.revision, |t| t.stage = WorldStage::Cancelled)?;
                host.events.emit(WorldTaskStore::progress_event(&updated, None, None));
                return Err(EngineError::Cancelled);
            }
        }

        // 重载；有失败章 → 停在 Scan 等待重试。
        cur = store.load(task_id)?;
        if cur.chapters.iter().any(|c| !matches!(c.status, ChapterStatus::Scanned)) {
            return Ok(cur);
        }

        // ---- 归并（分维度）----
        cancel.check()?;
        cur = store.update(task_id, cur.revision, |t| t.stage = WorldStage::Merge)?;
        host.events.emit(WorldTaskStore::progress_event(&cur, None, None));
        let discoveries = store.load_discoveries(&cur)?;
        let split = split_by_kind(&discoveries);

        // character：复用 character 归并链。
        let char_prompts = request.prompts.as_character_prompts();
        let (resolved, unresolved) = merge::rule_merge(&split.char_discoveries);
        let samples = build_context_samples(&split.char_discoveries);
        let model_entries = merge::model_merge(
            host, &request.profile, &char_prompts, task_id, unresolved, &samples, cancel,
        )
        .await?;
        let mut character_roster = combine_roster(resolved, model_entries);

        // location / item：走 entities 归并（rule + 模型兜底）。
        let location_roster = self
            .merge_entities(&request.prompts.loc_merge_system, request, task_id, &split.loc_mentions, cancel)
            .await?;
        let item_roster = self
            .merge_entities(&request.prompts.item_merge_system, request, task_id, &split.item_mentions, cancel)
            .await?;

        // ---- 分层（仅 character）----
        cancel.check()?;
        cur = store.update(task_id, cur.revision, |t| t.stage = WorldStage::Tiering)?;
        host.events.emit(WorldTaskStore::progress_event(&cur, None, None));
        tiering::score_and_tier(&mut character_roster, &split.char_discoveries);
        tiering::review_boundaries(host, &request.profile, &char_prompts, task_id, &mut character_roster, cancel)
            .await?;

        // 写回四 roster + plot/ending 暂存 → Review。
        cur = store.update(task_id, cur.revision, |t| {
            t.character_roster = character_roster;
            t.location_roster = location_roster;
            t.item_roster = item_roster;
            t.plot_beats = split.plot_beats;
            t.ending_clues = split.ending_clues;
            t.stage = WorldStage::Review;
        })?;
        host.events.emit(WorldTaskStore::progress_event(&cur, None, None));
        Ok(cur)
    }

    /// location/item 维度归并：规则 union-find + 模型兜底（未决簇为空时无模型调用）。
    async fn merge_entities(
        &self,
        merge_system: &str,
        request: &WorldExtractionRequest,
        task_id: &str,
        mentions: &[EntityMention],
        cancel: &CancelFlag,
    ) -> Result<Vec<WorldRosterEntry>, EngineError> {
        let host: &EngineHost = self.host.as_ref();
        let (resolved, unresolved) = entities::rule_merge_entities(mentions);
        let secret: std::collections::BTreeSet<String> = mentions
            .iter()
            .filter(|m| m.is_secret_realm)
            .map(|m| m.surface.clone())
            .collect();
        let model_entries = entities::model_merge_entities(
            host,
            &request.profile,
            merge_system,
            &request.prompts.prompt_version,
            task_id,
            unresolved,
            &secret,
            cancel,
        )
        .await?;
        Ok(combine_world_roster(resolved, model_entries))
    }

    /// 用户确认三 roster（character 复用 dna_status 归一）；带 revision CAS。
    pub fn confirm_rosters(
        &self,
        task_id: &str,
        expected_revision: u64,
        characters: Vec<RosterEntry>,
        locations: Vec<WorldRosterEntry>,
        items: Vec<WorldRosterEntry>,
    ) -> Result<WorldExtractionTask, EngineError> {
        let normalized: Vec<RosterEntry> = characters
            .into_iter()
            .map(|mut e| {
                if !e.user_confirmed {
                    e.dna_status = DnaStatus::Skipped;
                } else if matches!(e.dna_status, DnaStatus::Skipped) {
                    e.dna_status = DnaStatus::Pending;
                }
                e
            })
            .collect();
        WorldTaskStore::new(self.host.fs.clone()).update(task_id, expected_revision, move |t| {
            t.character_roster = normalized;
            t.location_roster = locations;
            t.item_roster = items;
        })
    }

    /// 合成世界内容超集（确认后）：character 复用证据账本 + DNA 合成；location/item/plot/ending 各自合成；
    /// 汇总 `superset::assemble` 打超集元数据。产 `WorldSkeletonDraft`（NPC 卡 lifecycle=Draft）。
    pub async fn synthesize_superset(
        &self,
        task_id: &str,
        request: &WorldExtractionRequest,
        cancel: &CancelFlag,
    ) -> Result<WorldSkeletonDraft, EngineError> {
        let host: &EngineHost = self.host.as_ref();
        let store = WorldTaskStore::new(self.host.fs.clone());
        let mut task = store.load(task_id)?;

        task = store.update(task_id, task.revision, |t| t.stage = WorldStage::Synthesis)?;
        host.events.emit(WorldTaskStore::progress_event(&task, None, None));

        let profile = &request.profile;
        let prompts = &request.prompts;
        let temperature = request.temperature;
        let max = request.max_output_tokens;
        let source_title = request.work_title.as_str();
        let source_id = task.source_fingerprint.content_hash.clone();

        // ---- 道具目录（先合成：locations/plot 需引用 item id）----
        let confirmed_items: Vec<WorldRosterEntry> =
            task.item_roster.iter().filter(|e| e.user_confirmed).cloned().collect();
        let world_items = entities::synthesize_items(
            host, profile, &prompts.item_synthesis_system, &prompts.prompt_version, temperature, max,
            task_id, &confirmed_items, source_title, cancel,
        )
        .await?;
        let item_ids: Vec<String> = world_items.iter().map(|i| i.id.clone()).collect();

        // ---- 地点图 ----
        let confirmed_locs: Vec<WorldRosterEntry> =
            task.location_roster.iter().filter(|e| e.user_confirmed).cloned().collect();
        let locations = entities::synthesize_locations(
            host, profile, &prompts.location_synthesis_system, &prompts.prompt_version, temperature, max,
            task_id, &confirmed_locs, &item_ids, source_title, cancel,
        )
        .await?;

        // ---- 世界固有角色（NPC/反派）：复用证据账本 + DNA 合成 ----
        let discoveries = store.load_discoveries(&task)?;
        let split = split_by_kind(&discoveries);
        let offsets: Vec<(usize, usize)> = task.chapters.iter().map(|c| c.char_range).collect();
        let target_entries: Vec<RosterEntry> = task
            .character_roster
            .iter()
            .filter(|e| e.user_confirmed && !matches!(e.dna_status, DnaStatus::Skipped))
            .cloned()
            .collect();

        let mut world_characters: Vec<WorldCharacterDraft> = Vec::new();
        let mut synth_status: Vec<(String, DnaStatus)> = Vec::new();
        let mut cancelled = false;
        if !target_entries.is_empty() {
            let char_prompts = prompts.as_character_prompts();
            let ledgers = evidence::build_ledgers(
                &self.host.fs, host.clock.now_ms(), &source_id, &target_entries, &split.char_discoveries, &offsets,
            )?;
            for (i, entry) in target_entries.iter().enumerate() {
                let ledger = &ledgers[i].0;
                match synthesis::synthesize_character(
                    host, profile, &char_prompts, temperature, max, task_id, entry, ledger, source_title, cancel,
                )
                .await
                {
                    Ok(card) => {
                        synth_status.push((entry.key.clone(), DnaStatus::Generated));
                        world_characters.push(WorldCharacterDraft {
                            card,
                            home_location: String::new(),
                            carried_item_ids: Vec::new(),
                            agenda_nodes: Vec::new(),
                        });
                    }
                    Err(EngineError::Cancelled) => {
                        cancelled = true;
                        break;
                    }
                    Err(_) => synth_status.push((entry.key.clone(), DnaStatus::Failed)),
                }
            }
        }
        if cancelled {
            return Err(EngineError::Cancelled);
        }

        // ---- 剧情线 + 结局 ----
        let plotv = plot::synthesize_mainline(
            host, profile, &prompts.plot_synthesis_system, &prompts.prompt_version, temperature, max,
            task_id, &task.plot_beats, &item_ids, source_title, cancel,
        )
        .await?;
        let ending_pool = plot::synthesize_endings(
            host, profile, &prompts.ending_synthesis_system, &prompts.prompt_version, temperature, max,
            task_id, &task.ending_clues, source_title, cancel,
        )
        .await?;

        // ---- 超集装配 ----
        let draft = superset::assemble(superset::SupersetInput {
            source_work: SkeletonSourceDraft { source_id, title: source_title.to_string() },
            world_characters,
            locations,
            world_items,
            mainline_nodes: plotv.mainline_nodes,
            hidden_content_pool: plotv.hidden_content_pool,
            side_hook_pool: plotv.side_hook_pool,
            ending_pool,
            storylines: plotv.storylines,
        });

        // 回写 character dna_status + 进 Assembled。
        task = store.update(task_id, task.revision, |t| {
            for (key, st) in &synth_status {
                if let Some(e) = t.character_roster.iter_mut().find(|e| &e.key == key) {
                    e.dna_status = *st;
                }
            }
            t.stage = WorldStage::Assembled;
        })?;
        host.events.emit(WorldTaskStore::progress_event(&task, None, None));

        Ok(draft)
    }

    /// 覆盖报告（纯聚合，无模型调用）。
    pub fn coverage_report(&self, task_id: &str) -> Result<WorldCoverageReport, EngineError> {
        let store = WorldTaskStore::new(self.host.fs.clone());
        let task = store.load(task_id)?;
        let scanned = task.chapters.iter().filter(|c| matches!(c.status, ChapterStatus::Scanned)).count() as u32;
        let total = task.chapters.len() as u32;
        let failed: Vec<u32> = task
            .chapters
            .iter()
            .filter(|c| matches!(c.status, ChapterStatus::Failed))
            .map(|c| c.index)
            .collect();
        Ok(WorldCoverageReport {
            scanned_chapters: scanned,
            total_chapters: total,
            failed_chapters: failed,
            character_roster_size: task.character_roster.len() as u32,
            location_roster_size: task.location_roster.len() as u32,
            item_roster_size: task.item_roster.len() as u32,
        })
    }

    pub fn cancel(&self, task_id: &str) -> Result<bool, EngineError> {
        let store = WorldTaskStore::new(self.host.fs.clone());
        let task = store.load(task_id)?;
        match task.stage {
            WorldStage::Done | WorldStage::Assembled => Ok(false),
            WorldStage::Cancelled => Ok(true),
            _ => {
                let updated = store.update(task_id, task.revision, |t| t.stage = WorldStage::Cancelled)?;
                self.host.events.emit(WorldTaskStore::progress_event(&updated, None, None));
                Ok(true)
            }
        }
    }

    pub fn get_task(&self, task_id: &str) -> Result<WorldExtractionTask, EngineError> {
        WorldTaskStore::new(self.host.fs.clone()).load(task_id)
    }
}

/// 世界提取覆盖报告。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldCoverageReport {
    pub scanned_chapters: u32,
    pub total_chapters: u32,
    pub failed_chapters: Vec<u32>,
    pub character_roster_size: u32,
    pub location_roster_size: u32,
    pub item_roster_size: u32,
}

/// 逐 kind 分流后的中间产物。
struct KindSplit {
    char_discoveries: Vec<ChapterDiscovery>,
    loc_mentions: Vec<EntityMention>,
    item_mentions: Vec<EntityMention>,
    plot_beats: Vec<PlotBeatDraft>,
    ending_clues: Vec<EndingClueDraft>,
}

/// 按 kind 分流：character→ChapterDiscovery（复用 character 归并/分层/合成）；location/item→EntityMention；
/// plotBeat/endingClue→全书级草稿。
fn split_by_kind(discoveries: &[WorldChapterDiscovery]) -> KindSplit {
    let mut char_discoveries: Vec<ChapterDiscovery> = Vec::new();
    let mut loc_mentions: Vec<EntityMention> = Vec::new();
    let mut item_mentions: Vec<EntityMention> = Vec::new();
    let mut plot_beats: Vec<PlotBeatDraft> = Vec::new();
    let mut ending_clues: Vec<EndingClueDraft> = Vec::new();

    for d in discoveries {
        let mut char_mentions: Vec<CharacterMention> = Vec::new();
        for m in &d.mentions {
            match WorldEntityKind::parse(&m.kind) {
                Some(WorldEntityKind::Character) => char_mentions.push(CharacterMention {
                    surface: m.surface.clone(),
                    role_hint: m.role_hint.clone(),
                    evidence: m.evidence.clone(),
                }),
                Some(WorldEntityKind::Location) => loc_mentions.push(EntityMention {
                    surface: m.surface.clone(),
                    is_secret_realm: m.role_hint.contains("秘境"),
                }),
                Some(WorldEntityKind::Item) => item_mentions.push(EntityMention {
                    surface: m.surface.clone(),
                    is_secret_realm: false,
                }),
                Some(WorldEntityKind::PlotBeat) => plot_beats.push(PlotBeatDraft {
                    surface: m.surface.clone(),
                    chapter_index: d.chapter_index,
                    links: m.links.clone(),
                    tension: m.role_hint.clone(),
                    is_hidden: m.role_hint.contains("隐藏"),
                }),
                Some(WorldEntityKind::EndingClue) => ending_clues.push(EndingClueDraft {
                    surface: m.surface.clone(),
                    affinity_hint: m.role_hint.clone(),
                    chapter_index: d.chapter_index,
                }),
                None => {}
            }
        }
        // 即使无角色也保留空 discovery，保证章序连续（tiering 共现统计按 chapter_index）。
        char_discoveries.push(ChapterDiscovery { chapter_index: d.chapter_index, mentions: char_mentions });
    }

    KindSplit { char_discoveries, loc_mentions, item_mentions, plot_beats, ending_clues }
}

/// 合并 character 规则簇与模型簇（按 key 去重，别名/来源并集）。
fn combine_roster(resolved: Vec<RosterEntry>, model: Vec<RosterEntry>) -> Vec<RosterEntry> {
    let mut map: BTreeMap<String, RosterEntry> = BTreeMap::new();
    for e in resolved.into_iter().chain(model.into_iter()) {
        map.entry(e.key.clone())
            .and_modify(|existing| {
                for a in &e.aliases {
                    if !existing.aliases.contains(a) {
                        existing.aliases.push(a.clone());
                    }
                }
                for m in &e.merged_from {
                    if !existing.merged_from.contains(m) {
                        existing.merged_from.push(m.clone());
                    }
                }
            })
            .or_insert(e);
    }
    map.into_values().collect()
}

/// 合并 entity 规则簇与模型簇（按 key 去重，别名/来源/秘境标记并集）。
fn combine_world_roster(
    resolved: Vec<WorldRosterEntry>,
    model: Vec<WorldRosterEntry>,
) -> Vec<WorldRosterEntry> {
    let mut map: BTreeMap<String, WorldRosterEntry> = BTreeMap::new();
    for e in resolved.into_iter().chain(model.into_iter()) {
        map.entry(e.key.clone())
            .and_modify(|existing| {
                for a in &e.aliases {
                    if !existing.aliases.contains(a) {
                        existing.aliases.push(a.clone());
                    }
                }
                for m in &e.merged_from {
                    if !existing.merged_from.contains(m) {
                        existing.merged_from.push(m.clone());
                    }
                }
                existing.is_secret_realm = existing.is_secret_realm || e.is_secret_realm;
            })
            .or_insert(e);
    }
    map.into_values().collect()
}

/// 为模型归并准备每个 surface 的少量上下文样本。
fn build_context_samples(discoveries: &[ChapterDiscovery]) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for d in discoveries {
        for m in &d.mentions {
            let entry = map.entry(m.surface.clone()).or_default();
            for e in &m.evidence {
                if entry.len() >= 3 {
                    break;
                }
                let s = if !e.note.is_empty() { e.note.clone() } else { e.quote.clone() };
                if !s.is_empty() {
                    entry.push(s);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests;
