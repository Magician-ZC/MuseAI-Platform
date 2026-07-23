//! 叙事状态持久化：原子提交（校验 → 应用 → 落盘一体）。文件所有权：agent-E3。
//! 存储：`narrative/<runId>/state.json`、`narrative/<runId>/scenes/<sceneId>.json`。

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::host::HostFs;
use crate::store;
use crate::EngineError;

use super::reducer;
use super::types::{NarrativeState, PatchOp, PatchOperation, SceneRecord, StatePatch};

pub fn state_path(run_id: &str) -> PathBuf {
    PathBuf::from("narrative").join(run_id).join("state.json")
}
pub fn scene_path(run_id: &str, scene_id: &str) -> PathBuf {
    PathBuf::from("narrative").join(run_id).join("scenes").join(format!("{scene_id}.json"))
}
fn scenes_dir(run_id: &str) -> PathBuf {
    PathBuf::from("narrative").join(run_id).join("scenes")
}

pub struct NarrativeStore {
    pub fs: Arc<dyn HostFs>,
}

impl NarrativeStore {
    pub fn new(fs: Arc<dyn HostFs>) -> Self {
        Self { fs }
    }

    pub fn load(&self, run_id: &str) -> Result<NarrativeState, EngineError> {
        crate::store::read_json(self.fs.as_ref(), &state_path(run_id))
    }

    pub fn init(&self, state: &NarrativeState) -> Result<(), EngineError> {
        let path = state_path(&state.run_id);
        if self.fs.exists(&path) {
            return Err(EngineError::Conflict(format!("run 已存在: {}", state.run_id)));
        }
        store::write_json(self.fs.as_ref(), &path, state)
    }

    /// 原子提交：reducer::validate_and_apply → 场景记录与新状态同批落盘（先场景后状态，
    /// 状态写入失败时删除本次场景文件回滚）→ 返回新状态。
    /// 幂等：patch.id 已应用则直接返回当前状态（不重复写场景）。
    /// 必测（§12.5.4）：场景失败时状态不部分提交。
    pub fn commit_scene(
        &self,
        run_id: &str,
        scene: &SceneRecord,
        patch: &StatePatch,
    ) -> Result<NarrativeState, EngineError> {
        let current = self.load(run_id)?;
        // 幂等：已应用则直接返回，不重复写场景。
        if reducer::already_applied(&current, &patch.id) {
            return Ok(current);
        }
        // 先在内存中完成全部校验与应用（失败即整体拒绝，磁盘不动）。
        let new_state = reducer::validate_and_apply(&current, patch)?;

        // 先写场景，再写状态；状态写失败则回滚删除本次场景，磁盘状态维持旧 revision。
        let sp = scene_path(run_id, &scene.scene_id);
        store::write_json(self.fs.as_ref(), &sp, scene)?;
        if let Err(e) = store::write_json(self.fs.as_ref(), &state_path(run_id), &new_state) {
            let _ = self.fs.remove(&sp);
            return Err(e);
        }
        Ok(new_state)
    }

    pub fn load_scene(&self, run_id: &str, scene_id: &str) -> Result<SceneRecord, EngineError> {
        crate::store::read_json(self.fs.as_ref(), &scene_path(run_id, scene_id))
    }

    pub fn list_scene_ids(&self, run_id: &str) -> Result<Vec<String>, EngineError> {
        let mut recs: Vec<(u64, String)> = Vec::new();
        for rel in self.fs.list(&scenes_dir(run_id))? {
            if rel.extension().and_then(|e| e.to_str()) != Some("json") {
                continue; // 跳过 .bak 备份
            }
            let sc: SceneRecord = store::read_json(self.fs.as_ref(), &rel)?;
            recs.push((sc.tick, sc.scene_id));
        }
        recs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        Ok(recs.into_iter().map(|(_, id)| id).collect())
    }

    /// 章节锁定：把 scene 标记 locked 并写入 authoring.lockedSceneIds（经 reducer 路径）。
    pub fn lock_scenes(
        &self,
        run_id: &str,
        scene_ids: &[String],
    ) -> Result<NarrativeState, EngineError> {
        let current = self.load(run_id)?;
        // 经 reducer 白名单路径追加锁定 id（集合语义去重）。
        let operations = scene_ids
            .iter()
            .map(|id| PatchOperation {
                op: PatchOp::Append,
                path: "authoring.lockedSceneIds".into(),
                value: Some(Value::String(id.clone())),
                precondition: None,
            })
            .collect();
        let patch = StatePatch {
            id: store::new_id("lock"),
            base_revision: current.revision,
            source_decision_ids: vec![],
            operations,
        };
        let new_state = reducer::validate_and_apply(&current, &patch)?;

        // 场景记录标记 locked=true 回写（缺失场景 → NotFound）。
        for id in scene_ids {
            let mut sc = self.load_scene(run_id, id)?;
            sc.locked = true;
            store::write_json(self.fs.as_ref(), &scene_path(run_id, id), &sc)?;
        }
        store::write_json(self.fs.as_ref(), &state_path(run_id), &new_state)?;
        Ok(new_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::MemFs;
    use crate::narrative::types::CharacterState;
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn mem_store() -> NarrativeStore {
        NarrativeStore::new(Arc::new(MemFs::default()))
    }

    fn init_state(run_id: &str) -> NarrativeState {
        let mut s = NarrativeState { schema_version: 1, run_id: run_id.into(), ..Default::default() };
        s.characters.insert("li".into(), CharacterState::default());
        s
    }

    fn goal_patch(id: &str, base: u64, goal: &str) -> StatePatch {
        StatePatch {
            id: id.into(),
            base_revision: base,
            source_decision_ids: vec![],
            operations: vec![PatchOperation {
                op: PatchOp::Append,
                path: "characters.li.goals".into(),
                value: Some(json!(goal)),
                precondition: None,
            }],
        }
    }

    fn scene(id: &str, tick: u64, patch: &StatePatch) -> SceneRecord {
        SceneRecord {
            scene_id: id.into(),
            tick,
            situation: String::new(),
            decisions: vec![],
            outcomes: vec![],
            prose: String::new(),
            events: vec![],
            state_patch: patch.clone(),
            locked: false,
            created_at: 0,
        }
    }

    #[test]
    fn init_rejects_existing() {
        let st = mem_store();
        let s = init_state("run1");
        st.init(&s).unwrap();
        assert_eq!(st.init(&s).unwrap_err().code(), "conflict");
    }

    #[test]
    fn commit_scene_applies_and_lists() {
        let st = mem_store();
        st.init(&init_state("run1")).unwrap();
        let p = goal_patch("p1", 0, "逃出生天");
        let new_state = st.commit_scene("run1", &scene("sc1", 1, &p), &p).unwrap();
        assert_eq!(new_state.revision, 1);
        assert_eq!(new_state.characters["li"].goals, vec!["逃出生天".to_string()]);
        assert_eq!(st.list_scene_ids("run1").unwrap(), vec!["sc1".to_string()]);
        assert!(st.load_scene("run1", "sc1").is_ok());
        // 落盘状态确实为新 revision
        assert_eq!(st.load("run1").unwrap().revision, 1);
    }

    #[test]
    fn commit_scene_idempotent_skips_second() {
        let st = mem_store();
        st.init(&init_state("run1")).unwrap();
        let p = goal_patch("p1", 0, "g");
        st.commit_scene("run1", &scene("sc1", 1, &p), &p).unwrap();
        // 同 patch id、不同场景：幂等短路，不写 sc2、revision 不变。
        let again = st.commit_scene("run1", &scene("sc2", 2, &p), &p).unwrap();
        assert_eq!(again.revision, 1);
        assert_eq!(st.list_scene_ids("run1").unwrap(), vec!["sc1".to_string()]);
    }

    #[test]
    fn list_scene_ids_sorted_by_tick() {
        let st = mem_store();
        st.init(&init_state("run1")).unwrap();
        let p1 = goal_patch("p1", 0, "g1");
        st.commit_scene("run1", &scene("sc-late", 5, &p1), &p1).unwrap();
        let p2 = goal_patch("p2", 1, "g2");
        st.commit_scene("run1", &scene("sc-early", 2, &p2), &p2).unwrap();
        assert_eq!(
            st.list_scene_ids("run1").unwrap(),
            vec!["sc-early".to_string(), "sc-late".to_string()]
        );
    }

    #[test]
    fn lock_scenes_marks_and_records() {
        let st = mem_store();
        st.init(&init_state("run1")).unwrap();
        let p = goal_patch("p1", 0, "g");
        st.commit_scene("run1", &scene("sc1", 1, &p), &p).unwrap();

        let locked = st.lock_scenes("run1", &["sc1".to_string()]).unwrap();
        assert!(locked.authoring.locked_scene_ids.contains(&"sc1".to_string()));
        assert!(st.load_scene("run1", "sc1").unwrap().locked);
        // 幂等：重复锁定同一场景不产生重复项。
        let again = st.lock_scenes("run1", &["sc1".to_string()]).unwrap();
        assert_eq!(again.authoring.locked_scene_ids, vec!["sc1".to_string()]);
    }

    /// 包装 fs：武装后对指定路径的写入注入失败，用于验证 commit_scene 回滚。
    struct FailingFs {
        inner: MemFs,
        fail_path: PathBuf,
        armed: AtomicBool,
    }
    impl HostFs for FailingFs {
        fn data_root(&self) -> PathBuf {
            self.inner.data_root()
        }
        fn read(&self, rel: &Path) -> Result<Vec<u8>, EngineError> {
            self.inner.read(rel)
        }
        fn exists(&self, rel: &Path) -> bool {
            self.inner.exists(rel)
        }
        fn write_atomic(&self, rel: &Path, bytes: &[u8]) -> Result<(), EngineError> {
            if self.armed.load(Ordering::SeqCst) && rel == self.fail_path {
                return Err(EngineError::io("注入写失败"));
            }
            self.inner.write_atomic(rel, bytes)
        }
        fn remove(&self, rel: &Path) -> Result<(), EngineError> {
            self.inner.remove(rel)
        }
        fn list(&self, rel_dir: &Path) -> Result<Vec<PathBuf>, EngineError> {
            self.inner.list(rel_dir)
        }
    }

    #[test]
    fn commit_scene_rolls_back_on_state_write_failure() {
        let fs = Arc::new(FailingFs {
            inner: MemFs::default(),
            fail_path: state_path("run1"),
            armed: AtomicBool::new(false),
        });
        let st = NarrativeStore::new(fs.clone());
        st.init(&init_state("run1")).unwrap(); // 未武装：init 写入成功
        fs.armed.store(true, Ordering::SeqCst); // 武装：后续 state.json 写入失败

        let p = goal_patch("p1", 0, "g");
        let err = st.commit_scene("run1", &scene("sc1", 1, &p), &p).unwrap_err();
        assert_eq!(err.code(), "io");

        // 状态未部分提交：revision 仍为 0，goals 为空。
        let reloaded = st.load("run1").unwrap();
        assert_eq!(reloaded.revision, 0);
        assert!(reloaded.characters["li"].goals.is_empty());
        // 场景文件已回滚删除。
        assert!(st.list_scene_ids("run1").unwrap().is_empty());
        assert!(st.load_scene("run1", "sc1").is_err());
    }
}
