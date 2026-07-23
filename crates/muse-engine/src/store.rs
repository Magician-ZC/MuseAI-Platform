//! 版本化 JSON 持久化工具：schemaVersion 检查、revision CAS、原子写、损坏回退。

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::EngineError;
use crate::host::HostFs;

/// 读取 JSON 对象；主文件损坏时尝试 `.bak` 备份（规格 §8.2.7：失败回退到上一个可读快照）。
pub fn read_json<T: DeserializeOwned>(fs: &dyn HostFs, rel: &Path) -> Result<T, EngineError> {
    match fs.read(rel).and_then(|b| serde_json::from_slice::<T>(&b).map_err(EngineError::serde)) {
        Ok(v) => Ok(v),
        Err(primary_err) => {
            let backup = rel.with_extension(format!(
                "{}.bak",
                rel.extension().and_then(|e| e.to_str()).unwrap_or("dat")
            ));
            if fs.exists(&backup) {
                let bytes = fs.read(&backup)?;
                serde_json::from_slice::<T>(&bytes).map_err(EngineError::serde)
            } else {
                Err(primary_err)
            }
        }
    }
}

pub fn write_json<T: Serialize>(fs: &dyn HostFs, rel: &Path, value: &T) -> Result<(), EngineError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    fs.write_atomic(rel, &bytes)
}

/// revision 比较交换写入：读取当前 revision，若与 expected 不一致则拒绝（并发保护）。
/// `bump` 负责在写入前把对象 revision +1。
pub fn write_json_cas<T>(
    fs: &dyn HostFs,
    rel: &Path,
    expected_revision: u64,
    value: &mut T,
    read_revision: impl Fn(&T) -> u64,
    bump: impl Fn(&mut T),
) -> Result<(), EngineError>
where
    T: Serialize + DeserializeOwned,
{
    if fs.exists(rel) {
        let current: T = read_json(fs, rel)?;
        let current_rev = read_revision(&current);
        if current_rev != expected_revision {
            return Err(EngineError::Conflict(format!(
                "revision 冲突: 期望 {expected_revision}, 实际 {current_rev}"
            )));
        }
    } else if expected_revision != 0 {
        return Err(EngineError::Conflict(format!(
            "revision 冲突: 期望 {expected_revision}, 对象不存在"
        )));
    }
    bump(value);
    write_json(fs, rel, value)
}

/// 内容哈希（sha256 hex），用于源指纹与索引键。
pub fn content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::MemFs;
    use serde::Deserialize;
    use std::path::PathBuf;

    #[derive(Serialize, Deserialize, Clone)]
    struct Doc {
        revision: u64,
        value: String,
    }

    #[test]
    fn cas_rejects_stale_revision() {
        let fs = MemFs::default();
        let rel = PathBuf::from("doc.json");
        let mut doc = Doc { revision: 0, value: "a".into() };
        write_json_cas(&fs, &rel, 0, &mut doc, |d| d.revision, |d| d.revision += 1).unwrap();
        assert_eq!(doc.revision, 1);

        let mut stale = Doc { revision: 0, value: "b".into() };
        let err = write_json_cas(&fs, &rel, 0, &mut stale, |d| d.revision, |d| d.revision += 1)
            .unwrap_err();
        assert_eq!(err.code(), "conflict");
    }
}
