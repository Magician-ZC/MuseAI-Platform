//! 场景快照 / 分支 / 回滚（规格 §5.2 / §12.5.6）。文件所有权：agent-E3。
//! 存储：`narrative/<runId>/snapshots/<snapshotId>.json`（完整 NarrativeState + 场景游标）。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::host::HostFs;
use crate::store;
use crate::EngineError;

use super::state::state_path;
use super::types::NarrativeState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub schema_version: u32, // 1
    pub snapshot_id: String,
    pub run_id: String,
    pub at_scene_id: String,
    pub state: NarrativeState,
    pub created_at: i64,
}

pub fn snapshot_path(run_id: &str, snapshot_id: &str) -> PathBuf {
    PathBuf::from("narrative").join(run_id).join("snapshots").join(format!("{snapshot_id}.json"))
}

fn snapshots_dir(run_id: &str) -> PathBuf {
    PathBuf::from("narrative").join(run_id).join("snapshots")
}

pub fn take_snapshot(
    fs: &Arc<dyn HostFs>,
    now_ms: i64,
    at_scene_id: &str,
    state: &NarrativeState,
) -> Result<Snapshot, EngineError> {
    let snap = Snapshot {
        schema_version: 1,
        snapshot_id: store::new_id("snap"),
        run_id: state.run_id.clone(),
        at_scene_id: at_scene_id.to_string(),
        state: state.clone(),
        created_at: now_ms,
    };
    store::write_json(fs.as_ref(), &snapshot_path(&snap.run_id, &snap.snapshot_id), &snap)?;
    Ok(snap)
}

/// 分支：从快照复制出新 runId 的状态（revision 归 0，branchSnapshotIds 记录来源）；
/// 原 run 不受影响。锁定场景列表随状态带入（锁定内容不可被新分支改写为「已发生的历史」）。
pub fn branch_from(
    fs: &Arc<dyn HostFs>,
    now_ms: i64,
    snapshot_id: &str,
    source_run_id: &str,
    new_run_id: &str,
) -> Result<NarrativeState, EngineError> {
    let _ = now_ms;
    // 目标 run 已存在则拒绝，避免静默覆盖。
    if fs.exists(&state_path(new_run_id)) {
        return Err(EngineError::Conflict(format!("目标 run 已存在: {new_run_id}")));
    }
    let snap: Snapshot = store::read_json(fs.as_ref(), &snapshot_path(source_run_id, snapshot_id))?;

    // clone-on-branch：新状态是快照的独立副本，对它的修改不回流源 run。
    let mut next = snap.state.clone();
    next.run_id = new_run_id.to_string();
    next.revision = 0;
    // 幂等账属旧 run，新分支重置。lockedSceneIds 随 state 自然带入。
    next.world.remove("appliedPatchIds");
    if !next.authoring.branch_snapshot_ids.contains(&snapshot_id.to_string()) {
        next.authoring.branch_snapshot_ids.push(snapshot_id.to_string());
    }

    store::write_json(fs.as_ref(), &state_path(new_run_id), &next)?;
    Ok(next)
}

pub fn list_snapshots(fs: &Arc<dyn HostFs>, run_id: &str) -> Result<Vec<Snapshot>, EngineError> {
    let mut out = Vec::new();
    for rel in fs.list(&snapshots_dir(run_id))? {
        // 只取 .json（跳过原子写留下的 .bak 备份）。
        if rel.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let snap: Snapshot = store::read_json(fs.as_ref(), &rel)?;
        out.push(snap);
    }
    out.sort_by(|a, b| {
        a.created_at.cmp(&b.created_at).then_with(|| a.snapshot_id.cmp(&b.snapshot_id))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::MemFs;

    fn fs() -> Arc<dyn HostFs> {
        Arc::new(MemFs::default())
    }

    fn state(run_id: &str, revision: u64) -> NarrativeState {
        NarrativeState { schema_version: 1, run_id: run_id.into(), revision, ..Default::default() }
    }

    #[test]
    fn snapshot_roundtrip_and_list_sorted() {
        let fs = fs();
        let s = state("run1", 3);
        let a = take_snapshot(&fs, 100, "sc1", &s).unwrap();
        let b = take_snapshot(&fs, 200, "sc2", &s).unwrap();
        let list = list_snapshots(&fs, "run1").unwrap();
        assert_eq!(list.len(), 2);
        // 按 created_at 升序
        assert_eq!(list[0].snapshot_id, a.snapshot_id);
        assert_eq!(list[1].snapshot_id, b.snapshot_id);
        assert_eq!(list[0].at_scene_id, "sc1");
    }

    #[test]
    fn branch_isolates_source_and_carries_locks() {
        let fs = fs();
        let mut s = state("src", 5);
        s.authoring.locked_scene_ids.push("scene-锁".into());
        // 源 run 落盘 + 快照
        store::write_json(fs.as_ref(), &state_path("src"), &s).unwrap();
        let snap = take_snapshot(&fs, 100, "sc9", &s).unwrap();

        let branched = branch_from(&fs, 200, &snap.snapshot_id, "src", "dst").unwrap();
        assert_eq!(branched.run_id, "dst");
        assert_eq!(branched.revision, 0); // revision 归 0
        assert_eq!(branched.authoring.locked_scene_ids, vec!["scene-锁".to_string()]); // 锁定带入
        assert!(branched.authoring.branch_snapshot_ids.contains(&snap.snapshot_id)); // 记录来源

        // 源 run 状态不受影响
        let src_reload: NarrativeState = store::read_json(fs.as_ref(), &state_path("src")).unwrap();
        assert_eq!(src_reload.revision, 5);
        assert_eq!(src_reload.run_id, "src");
        assert!(src_reload.authoring.branch_snapshot_ids.is_empty());
    }

    #[test]
    fn branch_rejects_existing_target() {
        let fs = fs();
        let s = state("src", 1);
        let snap = take_snapshot(&fs, 100, "sc", &s).unwrap();
        store::write_json(fs.as_ref(), &state_path("dst"), &state("dst", 0)).unwrap();
        let err = branch_from(&fs, 200, &snap.snapshot_id, "src", "dst").unwrap_err();
        assert_eq!(err.code(), "conflict");
    }
}
