//! 提取任务持久化与断点恢复（规格 §9.3）。文件所有权：agent-E1。
//!
//! 恢复语义：
//! 1) 比对 source fingerprint 与 pipeline_version，不一致 → Conflict（前端提供「基于新版本复制任务」）；
//! 2) 上次崩溃遗留的 Running 章节转为 Pending 可重试；
//! 3) 仅当 discovery 分片存在、内容哈希匹配且 schema 可解析时，Scanned 章节才可跳过；
//! 4) 状态按 revision 原子写入（store::write_json_cas）；取消/重试/重复事件幂等。

use std::path::PathBuf;
use std::sync::Arc;

use crate::host::{EngineEvent, HostFs};
use crate::store::{content_hash, read_json, write_json, write_json_cas};
use crate::EngineError;

use super::types::{ChapterDiscovery, ChapterStatus, ExtractionTask, TaskStage};

pub struct TaskStore {
    fs: Arc<dyn HostFs>,
}

impl TaskStore {
    pub fn new(fs: Arc<dyn HostFs>) -> Self {
        Self { fs }
    }

    pub fn task_path(task_id: &str) -> PathBuf {
        PathBuf::from("character-engine/extraction-tasks").join(format!("{task_id}.json"))
    }

    pub fn discovery_path(task_id: &str, chapter_id: &str) -> PathBuf {
        PathBuf::from("character-engine/extraction-tasks")
            .join(task_id)
            .join(format!("discovery-{chapter_id}.json"))
    }

    pub fn load(&self, task_id: &str) -> Result<ExtractionTask, EngineError> {
        read_json(self.fs.as_ref(), &Self::task_path(task_id))
    }

    pub fn create(&self, task: &ExtractionTask) -> Result<(), EngineError> {
        let path = Self::task_path(&task.task_id);
        if self.fs.exists(&path) {
            return Err(EngineError::Conflict(format!("任务已存在: {}", task.task_id)));
        }
        write_json(self.fs.as_ref(), &path, task)
    }

    /// CAS 更新：闭包内修改任务，revision+1 后原子写回；返回新快照。
    pub fn update(
        &self,
        task_id: &str,
        expected_revision: u64,
        mutate: impl FnOnce(&mut ExtractionTask),
    ) -> Result<ExtractionTask, EngineError> {
        let path = Self::task_path(task_id);
        let mut task: ExtractionTask = read_json(self.fs.as_ref(), &path)?;
        mutate(&mut task);
        write_json_cas(
            self.fs.as_ref(),
            &path,
            expected_revision,
            &mut task,
            |t| t.revision,
            |t| t.revision += 1,
        )?;
        Ok(task)
    }

    /// 恢复前校验（见模块注释 1–3），返回可直接继续执行的任务快照。
    pub fn prepare_resume(
        &self,
        task_id: &str,
        current_fingerprint_hash: &str,
        pipeline_version: &str,
    ) -> Result<ExtractionTask, EngineError> {
        let task = self.load(task_id)?;
        if task.source_fingerprint.content_hash != current_fingerprint_hash {
            return Err(EngineError::Conflict(
                "源文件内容已变化，请基于新版本复制任务后重跑".into(),
            ));
        }
        if task.pipeline_version != pipeline_version {
            return Err(EngineError::Conflict(format!(
                "管线版本不一致（任务 {} vs 当前 {}），请基于新版本复制任务",
                task.pipeline_version, pipeline_version
            )));
        }

        // 先在闭包外做 IO：判定哪些 Scanned 章节分片无效需回退。
        let mut invalid: Vec<u32> = Vec::new();
        let mut has_running = false;
        for c in &task.chapters {
            match c.status {
                ChapterStatus::Running => has_running = true,
                ChapterStatus::Scanned => {
                    if !self.discovery_valid(task_id, &c.id, c.index) {
                        invalid.push(c.index);
                    }
                }
                _ => {}
            }
        }
        if !has_running && invalid.is_empty() {
            return Ok(task); // 无需修正，避免无谓 revision 递增
        }

        self.update(task_id, task.revision, |t| {
            for c in t.chapters.iter_mut() {
                if matches!(c.status, ChapterStatus::Running) || invalid.contains(&c.index) {
                    c.status = ChapterStatus::Pending;
                    c.discovery_store_key = None;
                }
            }
        })
    }

    /// 分片有效性：存在 + 可解析 + chapter_index 一致。
    fn discovery_valid(&self, task_id: &str, chapter_id: &str, index: u32) -> bool {
        let path = Self::discovery_path(task_id, chapter_id);
        self.fs
            .read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<ChapterDiscovery>(&b).ok())
            .map(|d| d.chapter_index == index)
            .unwrap_or(false)
    }

    pub fn save_discovery(
        &self,
        task_id: &str,
        chapter_id: &str,
        discovery: &ChapterDiscovery,
    ) -> Result<String, EngineError> {
        let path = Self::discovery_path(task_id, chapter_id);
        let bytes = serde_json::to_vec_pretty(discovery)?;
        self.fs.write_atomic(&path, &bytes)?;
        // 写入后回读校验哈希，确认分片可信。
        let back = self.fs.read(&path)?;
        if content_hash(&back) != content_hash(&bytes) {
            return Err(EngineError::Io(format!("discovery 分片写入校验失败: {chapter_id}")));
        }
        Ok(path.to_string_lossy().to_string())
    }

    pub fn load_discoveries(&self, task: &ExtractionTask) -> Result<Vec<ChapterDiscovery>, EngineError> {
        let mut out = Vec::new();
        for c in &task.chapters {
            if !matches!(c.status, ChapterStatus::Scanned) && c.discovery_store_key.is_none() {
                continue;
            }
            let path = Self::discovery_path(&task.task_id, &c.id);
            if self.fs.exists(&path) {
                out.push(read_json::<ChapterDiscovery>(self.fs.as_ref(), &path)?);
            }
        }
        out.sort_by_key(|d| d.chapter_index);
        Ok(out)
    }

    /// 任务进度事件构造（stage 字符串用 serde 名，progress 由已完成章节比例计算）。
    pub fn progress_event(task: &ExtractionTask, item_id: Option<String>, error: Option<crate::host::EventError>) -> EngineEvent {
        let total = task.chapters.len().max(1) as f32;
        let done = task
            .chapters
            .iter()
            .filter(|c| matches!(c.status, super::types::ChapterStatus::Scanned))
            .count() as f32;
        EngineEvent::Task {
            task_id: task.task_id.clone(),
            revision: task.revision,
            stage: serde_json::to_value(task.stage).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
            item_id,
            progress: if matches!(task.stage, TaskStage::Done) { 1.0 } else { done / total },
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::{ChapterEntry, SourceFingerprint};
    use crate::host::testing::MemFs;

    fn chapter(id: &str, index: u32, status: ChapterStatus) -> ChapterEntry {
        ChapterEntry {
            id: id.into(),
            index,
            title: format!("第{index}章"),
            char_range: (index as usize * 100, index as usize * 100 + 100),
            status,
            attempt: 0,
            discovery_store_key: None,
            error: None,
        }
    }

    fn task(chapters: Vec<ChapterEntry>) -> ExtractionTask {
        ExtractionTask {
            schema_version: 1,
            task_id: "task-1".into(),
            work_title: "书".into(),
            source_path: "/x.txt".into(),
            source_fingerprint: SourceFingerprint {
                size: 10,
                modified_at: 0,
                content_hash: "hash-A".into(),
            },
            pipeline_version: crate::character::PIPELINE_VERSION.into(),
            chapters,
            roster: vec![],
            stage: TaskStage::Scan,
            revision: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn store() -> TaskStore {
        TaskStore::new(Arc::new(MemFs::default()))
    }

    #[test]
    fn create_rejects_duplicate() {
        let s = store();
        let t = task(vec![chapter("ch-0", 0, ChapterStatus::Pending)]);
        s.create(&t).unwrap();
        let err = s.create(&t).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn update_cas_rejects_stale_revision() {
        let s = store();
        let t = task(vec![chapter("ch-0", 0, ChapterStatus::Pending)]);
        s.create(&t).unwrap();
        let updated = s.update("task-1", 0, |t| t.stage = TaskStage::Merge).unwrap();
        assert_eq!(updated.revision, 1);
        let err = s.update("task-1", 0, |t| t.stage = TaskStage::Tiering).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn prepare_resume_rejects_fingerprint_change() {
        let s = store();
        s.create(&task(vec![chapter("ch-0", 0, ChapterStatus::Pending)])).unwrap();
        let err = s.prepare_resume("task-1", "hash-B", crate::character::PIPELINE_VERSION).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn prepare_resume_rejects_pipeline_change() {
        let s = store();
        s.create(&task(vec![chapter("ch-0", 0, ChapterStatus::Pending)])).unwrap();
        let err = s.prepare_resume("task-1", "hash-A", "old-pipeline").unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn prepare_resume_recovers_running_and_invalid_scanned() {
        let s = store();
        // ch-0 running（崩溃遗留）、ch-1 scanned 但无分片（无效）、ch-2 scanned 且分片有效。
        let mut t = task(vec![
            chapter("ch-0", 0, ChapterStatus::Running),
            chapter("ch-1", 1, ChapterStatus::Scanned),
            chapter("ch-2", 2, ChapterStatus::Scanned),
        ]);
        t.chapters[2].discovery_store_key = Some("k".into());
        s.create(&t).unwrap();
        // 为 ch-2 写有效分片。
        s.save_discovery("task-1", "ch-2", &ChapterDiscovery { chapter_index: 2, mentions: vec![] }).unwrap();

        let resumed = s.prepare_resume("task-1", "hash-A", crate::character::PIPELINE_VERSION).unwrap();
        assert_eq!(resumed.chapters[0].status, ChapterStatus::Pending); // running → pending
        assert_eq!(resumed.chapters[1].status, ChapterStatus::Pending); // 无效 scanned → pending
        assert_eq!(resumed.chapters[2].status, ChapterStatus::Scanned); // 有效 → 保留
    }

    #[test]
    fn save_and_load_discovery_roundtrip() {
        let s = store();
        let mut t = task(vec![chapter("ch-0", 0, ChapterStatus::Scanned)]);
        let key = s
            .save_discovery("task-1", "ch-0", &ChapterDiscovery { chapter_index: 0, mentions: vec![] })
            .unwrap();
        t.chapters[0].discovery_store_key = Some(key);
        s.create(&t).unwrap();
        let ds = s.load_discoveries(&t).unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].chapter_index, 0);
    }
}
