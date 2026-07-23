//! 世界提取任务持久化与断点恢复（仿 `character::task::TaskStore`）。
//!
//! 恢复语义与 character 一致：
//! 1) 比对 source fingerprint 与 pipeline_version，不一致 → Conflict；
//! 2) 上次崩溃遗留的 Running 章节转为 Pending 可重试；
//! 3) 仅当 discovery 分片存在、可解析且 chapter_index 一致时，Scanned 章节才可跳过；
//! 4) 状态按 revision 原子写入（store::write_json_cas）。

use std::path::PathBuf;
use std::sync::Arc;

use crate::character::types::ChapterStatus;
use crate::host::{EngineEvent, HostFs};
use crate::store::{content_hash, read_json, write_json, write_json_cas};
use crate::EngineError;

use super::types::{WorldChapterDiscovery, WorldExtractionTask, WorldStage};

pub struct WorldTaskStore {
    fs: Arc<dyn HostFs>,
}

impl WorldTaskStore {
    pub fn new(fs: Arc<dyn HostFs>) -> Self {
        Self { fs }
    }

    pub fn task_path(task_id: &str) -> PathBuf {
        PathBuf::from("world-engine/extraction-tasks").join(format!("{task_id}.json"))
    }

    pub fn discovery_path(task_id: &str, chapter_id: &str) -> PathBuf {
        PathBuf::from("world-engine/extraction-tasks")
            .join(task_id)
            .join(format!("discovery-{chapter_id}.json"))
    }

    pub fn load(&self, task_id: &str) -> Result<WorldExtractionTask, EngineError> {
        read_json(self.fs.as_ref(), &Self::task_path(task_id))
    }

    pub fn create(&self, task: &WorldExtractionTask) -> Result<(), EngineError> {
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
        mutate: impl FnOnce(&mut WorldExtractionTask),
    ) -> Result<WorldExtractionTask, EngineError> {
        let path = Self::task_path(task_id);
        let mut task: WorldExtractionTask = read_json(self.fs.as_ref(), &path)?;
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
    ) -> Result<WorldExtractionTask, EngineError> {
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
            return Ok(task);
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
            .and_then(|b| serde_json::from_slice::<WorldChapterDiscovery>(&b).ok())
            .map(|d| d.chapter_index == index)
            .unwrap_or(false)
    }

    pub fn save_discovery(
        &self,
        task_id: &str,
        chapter_id: &str,
        discovery: &WorldChapterDiscovery,
    ) -> Result<String, EngineError> {
        let path = Self::discovery_path(task_id, chapter_id);
        let bytes = serde_json::to_vec_pretty(discovery)?;
        self.fs.write_atomic(&path, &bytes)?;
        let back = self.fs.read(&path)?;
        if content_hash(&back) != content_hash(&bytes) {
            return Err(EngineError::Io(format!("discovery 分片写入校验失败: {chapter_id}")));
        }
        Ok(path.to_string_lossy().to_string())
    }

    pub fn load_discoveries(
        &self,
        task: &WorldExtractionTask,
    ) -> Result<Vec<WorldChapterDiscovery>, EngineError> {
        let mut out = Vec::new();
        for c in &task.chapters {
            if !matches!(c.status, ChapterStatus::Scanned) && c.discovery_store_key.is_none() {
                continue;
            }
            let path = Self::discovery_path(&task.task_id, &c.id);
            if self.fs.exists(&path) {
                out.push(read_json::<WorldChapterDiscovery>(self.fs.as_ref(), &path)?);
            }
        }
        out.sort_by_key(|d| d.chapter_index);
        Ok(out)
    }

    /// 任务进度事件（stage 字符串用 serde 名，progress 由已扫描章节比例计算）。
    pub fn progress_event(
        task: &WorldExtractionTask,
        item_id: Option<String>,
        error: Option<crate::host::EventError>,
    ) -> EngineEvent {
        let total = task.chapters.len().max(1) as f32;
        let done = task
            .chapters
            .iter()
            .filter(|c| matches!(c.status, ChapterStatus::Scanned))
            .count() as f32;
        EngineEvent::Task {
            task_id: task.task_id.clone(),
            revision: task.revision,
            stage: serde_json::to_value(task.stage)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            item_id,
            progress: if matches!(task.stage, WorldStage::Done | WorldStage::Assembled) {
                1.0
            } else {
                done / total
            },
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::{ChapterEntry, SourceFingerprint};
    use crate::host::testing::MemFs;
    use crate::world::WORLD_PIPELINE_VERSION;

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

    fn task(chapters: Vec<ChapterEntry>) -> WorldExtractionTask {
        WorldExtractionTask {
            schema_version: 1,
            task_id: "wtask-1".into(),
            work_title: "书".into(),
            source_path: "/x.txt".into(),
            source_fingerprint: SourceFingerprint {
                size: 10,
                modified_at: 0,
                content_hash: "hash-A".into(),
            },
            pipeline_version: WORLD_PIPELINE_VERSION.into(),
            chapters,
            character_roster: vec![],
            location_roster: vec![],
            item_roster: vec![],
            plot_beats: vec![],
            ending_clues: vec![],
            stage: WorldStage::Scan,
            revision: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn store() -> WorldTaskStore {
        WorldTaskStore::new(Arc::new(MemFs::default()))
    }

    #[test]
    fn create_rejects_duplicate() {
        let s = store();
        let t = task(vec![chapter("ch-0", 0, ChapterStatus::Pending)]);
        s.create(&t).unwrap();
        assert_eq!(s.create(&t).unwrap_err().code(), "conflict");
    }

    #[test]
    fn update_cas_rejects_stale_revision() {
        let s = store();
        s.create(&task(vec![chapter("ch-0", 0, ChapterStatus::Pending)])).unwrap();
        let updated = s.update("wtask-1", 0, |t| t.stage = WorldStage::Merge).unwrap();
        assert_eq!(updated.revision, 1);
        assert_eq!(s.update("wtask-1", 0, |t| t.stage = WorldStage::Tiering).unwrap_err().code(), "conflict");
    }

    #[test]
    fn prepare_resume_rejects_fingerprint_and_pipeline_change() {
        let s = store();
        s.create(&task(vec![chapter("ch-0", 0, ChapterStatus::Pending)])).unwrap();
        assert_eq!(s.prepare_resume("wtask-1", "hash-B", WORLD_PIPELINE_VERSION).unwrap_err().code(), "conflict");
        assert_eq!(s.prepare_resume("wtask-1", "hash-A", "old-pipeline").unwrap_err().code(), "conflict");
    }

    #[test]
    fn prepare_resume_recovers_running_and_invalid_scanned() {
        let s = store();
        let mut t = task(vec![
            chapter("ch-0", 0, ChapterStatus::Running),
            chapter("ch-1", 1, ChapterStatus::Scanned),
            chapter("ch-2", 2, ChapterStatus::Scanned),
        ]);
        t.chapters[2].discovery_store_key = Some("k".into());
        s.create(&t).unwrap();
        s.save_discovery("wtask-1", "ch-2", &WorldChapterDiscovery { chapter_index: 2, mentions: vec![] }).unwrap();

        let resumed = s.prepare_resume("wtask-1", "hash-A", WORLD_PIPELINE_VERSION).unwrap();
        assert_eq!(resumed.chapters[0].status, ChapterStatus::Pending);
        assert_eq!(resumed.chapters[1].status, ChapterStatus::Pending);
        assert_eq!(resumed.chapters[2].status, ChapterStatus::Scanned);
    }

    #[test]
    fn save_and_load_discovery_roundtrip() {
        let s = store();
        let mut t = task(vec![chapter("ch-0", 0, ChapterStatus::Scanned)]);
        let key = s
            .save_discovery("wtask-1", "ch-0", &WorldChapterDiscovery { chapter_index: 0, mentions: vec![] })
            .unwrap();
        t.chapters[0].discovery_store_key = Some(key);
        s.create(&t).unwrap();
        let ds = s.load_discoveries(&t).unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].chapter_index, 0);
    }
}
