//! P0 角色资产基座：全书提取管线 + DNA V2 + 角色评测。
//!
//! 管线（规格 §10.2）：预处理切章 → 逐章发现 → 别名归并 → 证据账本 → 重要度分层
//! → DNA 合成（含矛盾审查）→ 覆盖报告 → 人工确认。全程任务化、可断点恢复、幂等。
//!
//! 文件所有权：agent-E1。共享类型在 `types.rs`（主循环维护，勿改结构）。

pub mod chapters;
pub mod discovery;
pub mod evaluation;
pub mod evidence;
pub mod merge;
pub mod synthesis;
pub mod task;
pub mod tiering;
pub mod types;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::host::{CancelFlag, EngineHost, EventError};
use crate::model::ModelProfile;
use crate::store::{content_hash, new_id};
use crate::EngineError;
use task::TaskStore;
use types::*;

/// 管线版本：切章启发式 / prompt 契约 / 分片格式变化时递增；
/// 恢复任务前与持久化任务中的 pipeline_version 比对，不一致则要求「基于新版本复制任务」。
pub const PIPELINE_VERSION: &str = "p0b-1";

/// 各环节 prompt 由调用方传入（默认值维护在前端 settings store，后端无配置状态）。
#[derive(Debug, Clone)]
pub struct CharacterPrompts {
    pub scan_system: String,
    pub merge_system: String,
    pub tiering_system: String,
    pub synthesis_system: String,
    pub prompt_version: String,
}

#[derive(Debug, Clone)]
pub struct ExtractionRequest {
    pub work_title: String,
    /// 绝对路径：用户选择的 TXT/Markdown 源文件（引擎只读，不落副本；预览截断存分片）
    pub source_path: String,
    pub profile: ModelProfile,
    pub prompts: CharacterPrompts,
    pub temperature: f32,
    pub max_output_tokens: u32,
    /// 逐章扫描并发上限（沿用前端并发设置）
    pub concurrency: usize,
}

/// 提取管线编排器。所有阶段读写统一走 `task::TaskStore`，事件走 host.events。
pub struct ExtractionPipeline {
    pub host: Arc<EngineHost>,
}

impl ExtractionPipeline {
    pub fn new(host: Arc<EngineHost>) -> Self {
        Self { host }
    }

    /// 创建任务：读源文件、指纹、切章、落盘任务文件（stage=Scan），返回任务快照。
    /// 不发起模型调用。
    pub fn create_task(&self, request: &ExtractionRequest) -> Result<ExtractionTask, EngineError> {
        // 源文件是用户选择的宿主外绝对路径（非引擎受控存储），走 std::fs 只读一次；编码兜底 UTF-8 lossy。
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
        let taskv = ExtractionTask {
            schema_version: 1,
            task_id: new_id("task"),
            work_title: request.work_title.clone(),
            source_path: request.source_path.clone(),
            source_fingerprint: SourceFingerprint { size, modified_at, content_hash: fingerprint_hash },
            pipeline_version: PIPELINE_VERSION.to_string(),
            chapters,
            roster: Vec::new(),
            stage: TaskStage::Scan,
            revision: 0,
            created_at: now,
            updated_at: now,
        };
        TaskStore::new(self.host.fs.clone()).create(&taskv)?;
        Ok(taskv)
    }

    /// 运行/恢复任务至「等待人工确认」阶段：
    /// scan（并发逐章，跳过已 scanned 且分片校验通过的章）→ merge（规则+模型）→ tiering。
    /// 恢复语义（§9.3）：先校验 fingerprint 与 pipeline_version；running 残留转为可重试。
    pub async fn run_until_review(
        &self,
        task_id: &str,
        request: &ExtractionRequest,
        cancel: &CancelFlag,
    ) -> Result<ExtractionTask, EngineError> {
        let host: &EngineHost = self.host.as_ref();
        let store = TaskStore::new(self.host.fs.clone());

        // 读源文件并校验指纹（源变化则拒绝沿用旧结果）。
        let bytes = std::fs::read(&request.source_path).map_err(EngineError::io)?;
        let cur_hash = content_hash(&bytes);
        let text = String::from_utf8_lossy(&bytes).into_owned();

        let mut cur = store.prepare_resume(task_id, &cur_hash, PIPELINE_VERSION)?;

        // 已过归并阶段 → 幂等返回，避免重算覆盖用户编辑。
        if matches!(cur.stage, TaskStage::Review | TaskStage::Synthesis | TaskStage::Done) {
            return Ok(cur);
        }
        if !matches!(cur.stage, TaskStage::Scan) {
            cur = store.update(task_id, cur.revision, |t| t.stage = TaskStage::Scan)?;
        }

        // 待扫描章节（非 Scanned）：预取正文切片。
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
                    let r = discovery::scan_chapter(
                        host, profile, prompts, temperature, max, task_id, ch, body.as_str(), cancel,
                    )
                    .await;
                    (*idx, r)
                })
                .collect();

            // 逐章完成即持久化分片 + 更新任务 + 发进度事件（单任务串行回调，无数据竞争）。
            run_bounded_each(futures, request.concurrency, |(chap_idx, res)| {
                if pipeline_err.is_some() {
                    return;
                }
                let chapter_id = cur.chapters[chap_idx].id.clone();
                match res {
                    Ok(discovery) => {
                        let saved = store.save_discovery(task_id, &chapter_id, &discovery).and_then(|key| {
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
                                host.events.emit(TaskStore::progress_event(&cur, Some(chapter_id), None));
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
                                host.events.emit(TaskStore::progress_event(&cur, Some(chapter_id), Some(ev)));
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
                let updated = store.update(task_id, cur.revision, |t| t.stage = TaskStage::Cancelled)?;
                host.events.emit(TaskStore::progress_event(&updated, None, None));
                return Err(EngineError::Cancelled);
            }
        }

        // 重载最新快照；有失败章 → 停在 Scan 等待重试，不进入归并。
        cur = store.load(task_id)?;
        if cur.chapters.iter().any(|c| !matches!(c.status, ChapterStatus::Scanned)) {
            return Ok(cur);
        }

        // ---- 归并 ----
        cancel.check()?;
        cur = store.update(task_id, cur.revision, |t| t.stage = TaskStage::Merge)?;
        host.events.emit(TaskStore::progress_event(&cur, None, None));
        let discoveries = store.load_discoveries(&cur)?;
        let (resolved, unresolved) = merge::rule_merge(&discoveries);
        let samples = build_context_samples(&discoveries);
        let model_entries = merge::model_merge(
            host, &request.profile, &request.prompts, task_id, unresolved, &samples, cancel,
        )
        .await?;
        let mut roster = combine_roster(resolved, model_entries);

        // ---- 分层 ----
        cancel.check()?;
        cur = store.update(task_id, cur.revision, |t| t.stage = TaskStage::Tiering)?;
        host.events.emit(TaskStore::progress_event(&cur, None, None));
        tiering::score_and_tier(&mut roster, &discoveries);
        tiering::review_boundaries(host, &request.profile, &request.prompts, task_id, &mut roster, cancel)
            .await?;

        // 写回 roster + 进入 Review。
        cur = store.update(task_id, cur.revision, |t| {
            t.roster = roster;
            t.stage = TaskStage::Review;
        })?;
        host.events.emit(TaskStore::progress_event(&cur, None, None));
        Ok(cur)
    }

    /// 用户确认清单（归并结果 + 入库勾选）；写回 roster（user_confirmed / tier / dna_status=Skipped）。
    /// 带 revision CAS。
    pub fn confirm_roster(
        &self,
        task_id: &str,
        expected_revision: u64,
        roster: Vec<RosterEntry>,
    ) -> Result<ExtractionTask, EngineError> {
        // 未勾选入库 → Skipped；勾选但曾 Skipped → 复位 Pending。
        let normalized: Vec<RosterEntry> = roster
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
        TaskStore::new(self.host.fs.clone()).update(task_id, expected_revision, move |t| {
            t.roster = normalized;
        })
    }

    /// DNA 合成：对勾选角色并发合成（每角色 1–2 次调用，证据超长先做摘要分片），
    /// 支持只重试指定 keys；产出 CharacterCardV2(draft) + 证据账本文件。
    pub async fn synthesize(
        &self,
        task_id: &str,
        request: &ExtractionRequest,
        keys: &[String],
        cancel: &CancelFlag,
    ) -> Result<Vec<CharacterCardV2>, EngineError> {
        let host: &EngineHost = self.host.as_ref();
        let store = TaskStore::new(self.host.fs.clone());
        let mut task = store.load(task_id)?;

        // 目标角色：显式 keys 优先，否则所有 user_confirmed 且未 Skipped。
        let target_keys: Vec<String> = if keys.is_empty() {
            task.roster
                .iter()
                .filter(|e| e.user_confirmed && !matches!(e.dna_status, DnaStatus::Skipped))
                .map(|e| e.key.clone())
                .collect()
        } else {
            keys.to_vec()
        };
        if target_keys.is_empty() {
            return Ok(Vec::new());
        }

        task = store.update(task_id, task.revision, |t| t.stage = TaskStage::Synthesis)?;
        host.events.emit(TaskStore::progress_event(&task, None, None));

        let discoveries = store.load_discoveries(&task)?;
        let offsets: Vec<(usize, usize)> = task.chapters.iter().map(|c| c.char_range).collect();
        let source_id = task.source_fingerprint.content_hash.clone();
        let target_entries: Vec<RosterEntry> =
            task.roster.iter().filter(|e| target_keys.contains(&e.key)).cloned().collect();

        // 账本写盘（同时得到与卡一致的 index）。
        let ledgers = evidence::build_ledgers(
            &self.host.fs,
            host.clock.now_ms(),
            &source_id,
            &target_entries,
            &discoveries,
            &offsets,
        )?;

        let profile = &request.profile;
        let prompts = &request.prompts;
        let temperature = request.temperature;
        let max = request.max_output_tokens;
        let source_title = request.work_title.as_str();

        let mut results: Vec<(String, Result<CharacterCardV2, EngineError>)> = Vec::new();
        let futures: Vec<_> = (0..target_entries.len())
            .map(|i| {
                let entry = &target_entries[i];
                let ledger = &ledgers[i].0;
                async move {
                    let r = synthesis::synthesize_character(
                        host, profile, prompts, temperature, max, task_id, entry, ledger, source_title, cancel,
                    )
                    .await;
                    (entry.key.clone(), r)
                }
            })
            .collect();
        run_bounded_each(futures, request.concurrency, |item| results.push(item)).await;

        // 汇总状态；有取消则整体判为取消。
        let mut cards: Vec<CharacterCardV2> = Vec::new();
        let mut status_updates: Vec<(String, DnaStatus)> = Vec::new();
        let mut cancelled = false;
        for (key, r) in results {
            match r {
                Ok(card) => {
                    status_updates.push((key, DnaStatus::Generated));
                    cards.push(card);
                }
                Err(EngineError::Cancelled) => cancelled = true,
                Err(_) => status_updates.push((key, DnaStatus::Failed)),
            }
        }
        task = store.update(task_id, task.revision, |t| {
            for (key, st) in &status_updates {
                if let Some(e) = t.roster.iter_mut().find(|e| &e.key == key) {
                    e.dna_status = *st;
                }
            }
            // 全部生成或跳过 → Done。
            if t.roster.iter().all(|e| matches!(e.dna_status, DnaStatus::Generated | DnaStatus::Skipped)) {
                t.stage = TaskStage::Done;
            }
        })?;
        host.events.emit(TaskStore::progress_event(&task, None, None));

        if cancelled {
            return Err(EngineError::Cancelled);
        }
        Ok(cards)
    }

    /// 覆盖报告（纯聚合，无模型调用）。
    pub fn coverage_report(&self, task_id: &str) -> Result<CoverageReport, EngineError> {
        let store = TaskStore::new(self.host.fs.clone());
        let task = store.load(task_id)?;
        let scanned = task.chapters.iter().filter(|c| matches!(c.status, ChapterStatus::Scanned)).count() as u32;
        let total = task.chapters.len() as u32;
        let failed: Vec<u32> = task
            .chapters
            .iter()
            .filter(|c| matches!(c.status, ChapterStatus::Failed))
            .map(|c| c.index)
            .collect();

        let discoveries = store.load_discoveries(&task).unwrap_or_default();

        // 未决别名：出现在正文却未归入任何 roster 角色的 surface。
        let covered: BTreeSet<String> = task
            .roster
            .iter()
            .flat_map(|e| {
                e.merged_from
                    .iter()
                    .cloned()
                    .chain(e.aliases.iter().cloned())
                    .chain(std::iter::once(e.canonical_name.clone()))
            })
            .collect();
        let mut unresolved: BTreeSet<String> = BTreeSet::new();
        for d in &discoveries {
            for m in &d.mentions {
                if !covered.contains(&m.surface) {
                    unresolved.insert(m.surface.clone());
                }
            }
        }

        // 低置信字段：多数证据为 low 的角色（近似「关键字段证据不足」）。
        let mut low_confidence_fields: Vec<String> = Vec::new();
        for e in &task.roster {
            let names: BTreeSet<&str> = e
                .aliases
                .iter()
                .chain(e.merged_from.iter())
                .map(String::as_str)
                .chain(std::iter::once(e.canonical_name.as_str()))
                .collect();
            let (mut total_ev, mut low_ev) = (0u32, 0u32);
            for d in &discoveries {
                for m in &d.mentions {
                    if names.contains(m.surface.as_str()) {
                        for ev in &m.evidence {
                            total_ev += 1;
                            if matches!(ev.confidence, Confidence::Low) {
                                low_ev += 1;
                            }
                        }
                    }
                }
            }
            if total_ev > 0 && low_ev * 2 > total_ev {
                low_confidence_fields.push(e.canonical_name.clone());
            }
        }

        Ok(CoverageReport {
            scanned_chapters: scanned,
            total_chapters: total,
            failed_chapters: failed,
            roster_size: task.roster.len() as u32,
            unresolved_aliases: unresolved.into_iter().collect(),
            low_confidence_fields,
        })
    }

    pub fn cancel(&self, task_id: &str) -> Result<bool, EngineError> {
        let store = TaskStore::new(self.host.fs.clone());
        let task = store.load(task_id)?;
        match task.stage {
            TaskStage::Done => Ok(false),          // 已完成不可取消
            TaskStage::Cancelled => Ok(true),      // 幂等
            _ => {
                let updated = store.update(task_id, task.revision, |t| t.stage = TaskStage::Cancelled)?;
                self.host.events.emit(TaskStore::progress_event(&updated, None, None));
                Ok(true)
            }
        }
    }

    pub fn get_task(&self, task_id: &str) -> Result<ExtractionTask, EngineError> {
        TaskStore::new(self.host.fs.clone()).load(task_id)
    }
}

/// 合并规则簇与模型簇：按 key 去重，别名与来源并集。
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

/// 单任务内并发驱动一批 future，并发上限 limit，每个完成即回调（保持宿主无关，不依赖 tokio rt / futures-util）。
/// 用 poll_fn 手动轮询：Pending 时透传真实 waker，同步完成时在同一 poll 内继续补位，避免丢唤醒。
///
/// `pub(crate)`：world 提取管线（`crate::world`）逐章扫描并发复用同一驱动，避免复制一份。
pub(crate) async fn run_bounded_each<F, C>(futures: Vec<F>, limit: usize, mut on_complete: C)
where
    F: std::future::Future,
    C: FnMut(F::Output),
{
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::task::Poll;

    let limit = limit.max(1);
    let mut slots: Vec<Option<Pin<Box<F>>>> = futures.into_iter().map(|f| Some(Box::pin(f))).collect();
    let total = slots.len();
    if total == 0 {
        return;
    }
    let mut in_flight: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut done = 0usize;

    poll_fn(move |cx| {
        loop {
            while in_flight.len() < limit && next < total {
                in_flight.push(next);
                next += 1;
            }
            if in_flight.is_empty() {
                return Poll::Ready(());
            }
            let mut progressed = false;
            let mut still: Vec<usize> = Vec::with_capacity(in_flight.len());
            for idx in std::mem::take(&mut in_flight) {
                let poll_res = {
                    let fut = slots[idx].as_mut().expect("slot live");
                    fut.as_mut().poll(cx)
                };
                match poll_res {
                    Poll::Ready(out) => {
                        slots[idx] = None;
                        done += 1;
                        progressed = true;
                        on_complete(out);
                    }
                    Poll::Pending => still.push(idx),
                }
            }
            in_flight = still;
            if done == total && in_flight.is_empty() {
                return Poll::Ready(());
            }
            if !progressed {
                return Poll::Pending; // 所有在飞任务已注册 waker
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::{EngineEvent, EngineHost};
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use std::io::Write;

    fn make_host(model: ScriptedModel) -> Arc<EngineHost> {
        Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1_000)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(model),
        })
    }

    fn request(source_path: String) -> ExtractionRequest {
        ExtractionRequest {
            work_title: "测试书".into(),
            source_path,
            profile: ModelProfile {
                interface: ModelInterface::OpenAiCompatible,
                base_url: "u".into(),
                api_key: "k".into(),
                model: "m".into(),
            },
            prompts: CharacterPrompts {
                scan_system: "s".into(),
                merge_system: "s".into(),
                tiering_system: "s".into(),
                synthesis_system: "s".into(),
                prompt_version: "v1".into(),
            },
            temperature: 0.0,
            max_output_tokens: 2048,
            concurrency: 2,
        }
    }

    // 两章书：ch0 含「甲走进房间」，ch1 含「乙推开门」。各章 >50 字避免超短合并。
    fn write_book() -> tempfile::NamedTempFile {
        let pad = "他沉默良久环顾四周反复思量始终不语".repeat(4);
        let text = format!("第一章\n甲走进房间，{pad}\n第二章\n乙推开门，{pad}");
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    // 各角色单次出场单条证据 → 均判 Extra（同层，无分层边界，跳过分层模型调用）。
    fn scan_resp(idx: u32, surface: &str, quote: &str) -> String {
        format!(
            r#"{{"chapterIndex":{idx},"mentions":[{{"surface":"{surface}","evidence":[{{"kind":"action","quote":"{quote}","confidence":"high"}}]}}]}}"#
        )
    }

    fn synth_resp() -> String {
        r#"{"dramaticCore":{"coreContradiction":"测试内核"}}"#.into()
    }

    #[test]
    fn create_task_splits_and_persists() {
        let book = write_book();
        let host = make_host(ScriptedModel::new(vec![]));
        let pipe = ExtractionPipeline::new(host.clone());
        let task = pipe.create_task(&request(book.path().to_string_lossy().to_string())).unwrap();
        assert_eq!(task.chapters.len(), 2);
        assert!(matches!(task.stage, TaskStage::Scan));
        assert!(!task.source_fingerprint.content_hash.is_empty());
        // 落盘可回读。
        assert_eq!(pipe.get_task(&task.task_id).unwrap().chapters.len(), 2);
    }

    #[tokio::test]
    async fn full_pipeline_scan_merge_tier_then_synthesize() {
        let book = write_book();
        // 顺序：scan0, scan1, synth, synth（归并/分层无模型调用）。
        let host = make_host(ScriptedModel::new(vec![
            Ok(scan_resp(0, "甲", "甲走进房间")),
            Ok(scan_resp(1, "乙", "乙推开门")),
            Ok(synth_resp()),
            Ok(synth_resp()),
        ]));
        let pipe = ExtractionPipeline::new(host.clone());
        let req = request(book.path().to_string_lossy().to_string());
        let task = pipe.create_task(&req).unwrap();

        let reviewed = pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap();
        assert!(matches!(reviewed.stage, TaskStage::Review));
        assert_eq!(reviewed.roster.len(), 2);
        assert!(reviewed.chapters.iter().all(|c| matches!(c.status, ChapterStatus::Scanned)));

        // 覆盖报告：2/2 已扫描。
        let cov = pipe.coverage_report(&task.task_id).unwrap();
        assert_eq!(cov.scanned_chapters, 2);
        assert_eq!(cov.total_chapters, 2);
        assert_eq!(cov.roster_size, 2);
        assert!(cov.failed_chapters.is_empty());

        // 确认全部入库。
        let roster: Vec<RosterEntry> =
            reviewed.roster.iter().cloned().map(|mut e| { e.user_confirmed = true; e }).collect();
        let confirmed = pipe.confirm_roster(&task.task_id, reviewed.revision, roster).unwrap();
        assert!(confirmed.roster.iter().all(|e| e.user_confirmed));

        // 合成两张 Draft 卡。
        let cards = pipe.synthesize(&task.task_id, &req, &[], &CancelFlag::new()).await.unwrap();
        assert_eq!(cards.len(), 2);
        assert!(cards.iter().all(|c| matches!(c.lifecycle, CardLifecycle::Draft)));
        assert!(cards.iter().all(|c| c.dramatic_core.core_contradiction == "测试内核"));

        // 任务进入 Done，dna 全部生成。
        let done = pipe.get_task(&task.task_id).unwrap();
        assert!(matches!(done.stage, TaskStage::Done));
        assert!(done.roster.iter().all(|e| matches!(e.dna_status, DnaStatus::Generated)));
    }

    #[tokio::test]
    async fn rerun_is_idempotent_and_skips_rescan() {
        let book = write_book();
        // 仅提供 2 条 scan 响应；若二次运行重扫会耗尽脚本报错。
        let host = make_host(ScriptedModel::new(vec![
            Ok(scan_resp(0, "甲", "甲走进房间")),
            Ok(scan_resp(1, "乙", "乙推开门")),
        ]));
        let pipe = ExtractionPipeline::new(host.clone());
        let req = request(book.path().to_string_lossy().to_string());
        let task = pipe.create_task(&req).unwrap();
        let first = pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap();
        assert!(matches!(first.stage, TaskStage::Review));
        // 二次运行：全部 Scanned + stage=Review → 幂等直接返回，不再扫描。
        let second = pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap();
        assert!(matches!(second.stage, TaskStage::Review));
        assert_eq!(second.roster.len(), 2);
    }

    #[tokio::test]
    async fn cancel_marks_stage_and_is_idempotent() {
        let book = write_book();
        let host = make_host(ScriptedModel::new(vec![]));
        let pipe = ExtractionPipeline::new(host.clone());
        let task = pipe.create_task(&request(book.path().to_string_lossy().to_string())).unwrap();
        assert!(pipe.cancel(&task.task_id).unwrap());
        assert!(matches!(pipe.get_task(&task.task_id).unwrap().stage, TaskStage::Cancelled));
        assert!(pipe.cancel(&task.task_id).unwrap()); // 幂等
    }

    #[tokio::test]
    async fn precancelled_run_stops_and_marks_cancelled() {
        let book = write_book();
        let host = make_host(ScriptedModel::new(vec![]));
        let pipe = ExtractionPipeline::new(host.clone());
        let req = request(book.path().to_string_lossy().to_string());
        let task = pipe.create_task(&req).unwrap();
        let cancel = CancelFlag::new();
        cancel.cancel(); // 预取消
        let err = pipe.run_until_review(&task.task_id, &req, &cancel).await.unwrap_err();
        assert_eq!(err.code(), "cancelled");
        assert!(matches!(pipe.get_task(&task.task_id).unwrap().stage, TaskStage::Cancelled));
    }

    #[test]
    fn progress_event_emitted_on_cancel() {
        let book = write_book();
        let collect = Arc::new(CollectEvents::default());
        let host = Arc::new(EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1_000)),
            events: collect.clone(),
            model: Arc::new(ScriptedModel::new(vec![])),
        });
        let pipe = ExtractionPipeline::new(host);
        let task = pipe.create_task(&request(book.path().to_string_lossy().to_string())).unwrap();
        pipe.cancel(&task.task_id).unwrap();
        let evs = collect.0.lock().unwrap();
        assert!(evs.iter().any(|e| matches!(e, EngineEvent::Task { .. })));
    }
}
