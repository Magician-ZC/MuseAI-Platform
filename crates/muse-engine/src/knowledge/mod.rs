//! P1 知识与思维系统：导入 → 切块 → 索引 → 蒸馏 → 绑定 → 检索 → 使用追踪 → 级联删除。
//!
//! 文件所有权：agent-E2。共享类型在 `types.rs`（主循环维护）。
//!
//! 权利边界（规格 §9.4 末段）：`rights_basis` 是用户声明；未含 `send_to_remote_model`
//! 的包，任何会把片段送往远程模型的路径必须被阻止并返回 Validation 错误。

pub mod chunk;
pub mod distill;
pub mod index;
pub mod types;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::host::{CancelFlag, EngineHost};
use crate::model::ModelProfile;
use crate::store::{content_hash, new_id, read_json, write_json};
use crate::EngineError;
use types::*;

pub const CHUNKER_VERSION: &str = "chunker-1";

pub fn pack_path(pack_id: &str) -> PathBuf {
    PathBuf::from("knowledge/packs").join(format!("{pack_id}.json"))
}
pub fn index_path(pack_id: &str) -> PathBuf {
    PathBuf::from("knowledge/index").join(format!("{pack_id}.json"))
}
pub fn bindings_path() -> PathBuf {
    PathBuf::from("knowledge/bindings.json")
}
pub fn usage_path(run_id: &str) -> PathBuf {
    PathBuf::from("knowledge/usage").join(format!("{run_id}.json"))
}
/// managed_copy 保留模式下的源副本受控 key。
fn managed_source_path(pack_id: &str) -> PathBuf {
    PathBuf::from("knowledge/sources").join(format!("{pack_id}.bin"))
}
/// 级联删除后的审计元数据（仅 {packId, deletedAt}）。
fn deleted_path() -> PathBuf {
    PathBuf::from("knowledge/deleted.json")
}

/// 删除审计记录：正文清空后仅保留的必要元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeletedRecord {
    pack_id: String,
    deleted_at: i64,
}

/// 聚合切块统计。
fn chunk_stats(chunks: &[Chunk]) -> ChunkStats {
    ChunkStats {
        chunk_count: chunks.len() as u32,
        total_chars: chunks.iter().map(|c| c.text.chars().count()).sum(),
    }
}

pub struct KnowledgeSystem {
    pub host: Arc<EngineHost>,
}

pub struct DistillPrompts {
    pub system_by_mode: std::collections::BTreeMap<String, String>, // key: PackMode serde 名
    pub prompt_version: String,
}

impl KnowledgeSystem {
    pub fn new(host: Arc<EngineHost>) -> Self {
        Self { host }
    }

    /// 导入源：读文件 → content_hash → 切块 → 建倒排索引 → 写 pack 草稿（mode 待蒸馏时定）。
    /// retention=IndexOnly 时不保留 managed copy（只有切块文本）。
    pub fn import_source(
        &self,
        source_path: &str,
        title: &str,
        rights_basis: RightsBasis,
        allowed_uses: Vec<AllowedUse>,
        retention: Retention,
    ) -> Result<(KnowledgePack, ChunkStats), EngineError> {
        let fs = self.host.fs.as_ref();
        let bytes = fs.read(Path::new(source_path))?;
        let hash = content_hash(&bytes);
        // index_version = sourceHash + ":" + CHUNKER_VERSION（任一变化即重建）。
        let index_version = format!("{hash}:{CHUNKER_VERSION}");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let pack_id = new_id("kp");

        // 同源同版本索引复用：已存在同 contentHash + indexVersion 且索引文件在，则共享其 store key。
        let reuse = self.list_packs()?.into_iter().find(|p| {
            p.source.content_hash == hash
                && p.index_version == index_version
                && fs.exists(Path::new(&p.chunk_index_store_key))
        });
        let (store_key, stats) = if let Some(p) = reuse {
            let idx: ChunkIndex = read_json(fs, Path::new(&p.chunk_index_store_key))?;
            (p.chunk_index_store_key.clone(), chunk_stats(&idx.chunks))
        } else {
            let chunks = chunk::split_chunks(&pack_id, &text);
            let stats = chunk_stats(&chunks);
            let idx = index::build_index(&pack_id, &index_version, CHUNKER_VERSION, chunks);
            let key = index_path(&pack_id).to_string_lossy().into_owned();
            write_json(fs, Path::new(&key), &idx)?;
            (key, stats)
        };

        // managed_copy 保留一份源副本；reference_original / index_only 不落副本。
        if retention == Retention::ManagedCopy {
            fs.write_atomic(&managed_source_path(&pack_id), &bytes)?;
        }

        let pack = KnowledgePack {
            schema_version: 1,
            id: pack_id.clone(),
            title: title.to_string(),
            source: PackSource {
                path: source_path.to_string(),
                author: None,
                content_hash: hash,
                rights_basis,
                allowed_uses,
                user_attested_at: Some(self.host.clock.now_ms()),
                retention,
            },
            mode: PackMode::Knowledge, // 草稿默认知识模式，蒸馏时再定
            distilled: Distilled::default(),
            time_boundary: None,
            chunk_index_store_key: store_key,
            index_version,
            revision: 0,
        };
        write_json(fs, &pack_path(&pack_id), &pack)?;
        Ok((pack, stats))
    }

    /// 蒸馏：mind/value/expression 从切块采样拼上下文做 1 次调用得 Distilled；
    /// knowledge 模式不调用模型（distilled 留空，靠检索）。
    /// 同一来源可蒸出多个包：以 base pack 复制新 id，共享 chunk index store key。
    pub async fn distill(
        &self,
        pack_id: &str,
        mode: PackMode,
        profile: &ModelProfile,
        prompts: &DistillPrompts,
        cancel: &CancelFlag,
    ) -> Result<KnowledgePack, EngineError> {
        let fs = self.host.fs.as_ref();
        let base = self.get_pack(pack_id)?;

        // 权利边界硬规则：mind/value/expression 会把片段送远程模型，未授权即拒绝且不发调用。
        let sends_remote =
            matches!(mode, PackMode::Mind | PackMode::Value | PackMode::Expression);
        if sends_remote && !base.source.allowed_uses.contains(&AllowedUse::SendToRemoteModel) {
            return Err(EngineError::Validation(
                "知识包未授权发送到远程模型（send_to_remote_model），无法蒸馏".into(),
            ));
        }

        let distilled = if sends_remote {
            let idx: ChunkIndex = read_json(fs, Path::new(&base.chunk_index_store_key))?;
            distill::distill_from_chunks(
                self.host.as_ref(),
                profile,
                prompts,
                pack_id,
                mode,
                &idx.chunks,
                cancel,
            )
            .await?
        } else {
            Distilled::default() // knowledge 模式：不调用模型
        };

        let new_pack_id = new_id("kp");
        let pack = KnowledgePack {
            schema_version: 1,
            id: new_pack_id.clone(),
            title: base.title.clone(),
            source: base.source.clone(),
            mode,
            distilled,
            time_boundary: base.time_boundary.clone(),
            chunk_index_store_key: base.chunk_index_store_key.clone(), // 共享索引
            index_version: base.index_version.clone(),
            revision: 0,
        };
        write_json(fs, &pack_path(&new_pack_id), &pack)?;
        Ok(pack)
    }

    /// 检索（MVP：关键词倒排 + 简单 BM25 风格打分；无 embedding 依赖）。
    /// 只在传入 pack_ids 内检索。向后兼容包装：不做时间边界过滤（等价 `search_as_of(.., None)`）。
    pub fn search(
        &self,
        pack_ids: &[String],
        query: &str,
        limit: usize,
    ) -> Result<Vec<RetrievedFragment>, EngineError> {
        self.search_as_of(pack_ids, query, limit, None)
    }

    /// 带时间边界确定性过滤的检索（规格 §4.3 第 5 条 / §11.4「时间边界过滤生效」）。
    ///
    /// `as_of` = 当前叙事时代（由调用方按在场角色所处时代传入）。若某包带 `time_boundary`
    /// 且其时代**晚于** `as_of`，则该整包不返回——角色不应引用晚于其所处时代的知识
    /// （越界次数目标 0）。这是引擎侧的强制过滤，不再下放提示词层。
    ///
    /// 比较策略（确定性、可测）：`time_boundary` 与 `as_of` 均可解析为整数（年份/纪元序，
    /// 支持负数表示公元前）时按数值比较；否则退回字典序比较。切块无时代元数据，故按整包过滤。
    pub fn search_as_of(
        &self,
        pack_ids: &[String],
        query: &str,
        limit: usize,
        as_of: Option<&str>,
    ) -> Result<Vec<RetrievedFragment>, EngineError> {
        let fs = self.host.fs.as_ref();
        let mut all: Vec<RetrievedFragment> = Vec::new();
        for pid in pack_ids {
            // 已删除/缺失的包直接跳过（正文不可访问）。
            let pack = match self.get_pack(pid) {
                Ok(p) => p,
                Err(_) => continue,
            };
            // 时间边界确定性过滤：包时代晚于当前叙事时代 → 整包不可引用。
            if let (Some(as_of), Some(boundary)) = (as_of, pack.time_boundary.as_deref()) {
                if era_after(boundary, as_of) {
                    continue;
                }
            }
            let idx: ChunkIndex = match read_json(fs, Path::new(&pack.chunk_index_store_key)) {
                Ok(i) => i,
                Err(_) => continue,
            };
            let mut frags = index::query_index(&idx, &pack.title, query, limit)?;
            // 归属到被检索的包（蒸馏副本共享索引时索引内 pack_id 为 base）。
            for f in &mut frags {
                f.pack_id = pack.id.clone();
            }
            all.extend(frags);
        }
        // 多包合并：分数降序，稳定 tie-break（pack_id, ordinal），取 top-limit。
        all.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.pack_id.cmp(&b.pack_id))
                .then(a.ordinal.cmp(&b.ordinal))
        });
        all.truncate(limit);
        Ok(all)
    }

    pub fn record_usage(&self, entry: &UsageLogEntry) -> Result<(), EngineError> {
        let fs = self.host.fs.as_ref();
        let path = usage_path(&entry.run_id);
        // 同 runId 追加。
        let mut entries: Vec<UsageLogEntry> =
            if fs.exists(&path) { read_json(fs, &path)? } else { Vec::new() };
        entries.push(entry.clone());
        write_json(fs, &path, &entries)
    }

    pub fn get_usage(&self, run_id: &str) -> Result<Vec<UsageLogEntry>, EngineError> {
        let fs = self.host.fs.as_ref();
        let path = usage_path(run_id);
        if fs.exists(&path) {
            read_json(fs, &path)
        } else {
            Ok(Vec::new())
        }
    }

    /// 绑定 CRUD（绑定独立存储，避免共享包被某故事配置污染）。
    pub fn list_bindings(&self) -> Result<Vec<KnowledgeBinding>, EngineError> {
        let fs = self.host.fs.as_ref();
        let path = bindings_path();
        if fs.exists(&path) {
            read_json(fs, &path)
        } else {
            Ok(Vec::new())
        }
    }
    pub fn upsert_binding(&self, binding: KnowledgeBinding) -> Result<(), EngineError> {
        let fs = self.host.fs.as_ref();
        let mut all = self.list_bindings()?;
        if let Some(slot) = all.iter_mut().find(|b| b.id == binding.id) {
            *slot = binding;
        } else {
            all.push(binding);
        }
        write_json(fs, &bindings_path(), &all)
    }
    pub fn remove_binding(&self, binding_id: &str) -> Result<(), EngineError> {
        let fs = self.host.fs.as_ref();
        let mut all = self.list_bindings()?;
        all.retain(|b| b.id != binding_id);
        write_json(fs, &bindings_path(), &all)
    }

    /// 级联删除（规格 §4.3.6 / §11.1）：包、切块索引、使用日志正文、绑定全部删除；
    /// 仅保留必要审计元数据（删除时间与 pack_id）。
    pub fn delete_pack(&self, pack_id: &str) -> Result<(), EngineError> {
        let fs = self.host.fs.as_ref();
        let pack = self.get_pack(pack_id)?; // 不存在 → NotFound

        // 1) 删包正文。
        fs.remove(&pack_path(pack_id))?;
        // 2) 删 managed 源副本（若有）。
        if pack.source.retention == Retention::ManagedCopy {
            let _ = fs.remove(&managed_source_path(pack_id));
        }
        // 3) 索引：仅当无其他包共享同一 store key 时删除（蒸馏副本共享保护）。
        let still_shared = self
            .list_packs()?
            .iter()
            .any(|p| p.chunk_index_store_key == pack.chunk_index_store_key);
        if !still_shared {
            let _ = fs.remove(Path::new(&pack.chunk_index_store_key));
        }
        // 4) 绑定：移除该包所有绑定。
        let bindings: Vec<KnowledgeBinding> =
            self.list_bindings()?.into_iter().filter(|b| b.pack_id != pack_id).collect();
        write_json(fs, &bindings_path(), &bindings)?;
        // 5) 使用日志：清除该包正文引用。
        self.purge_usage_for_pack(pack_id)?;
        // 6) 审计元数据。
        self.append_deleted_audit(pack_id)?;
        Ok(())
    }

    /// 从所有使用日志中剔除指定包的片段引用；引用清空的条目整条移除。
    fn purge_usage_for_pack(&self, pack_id: &str) -> Result<(), EngineError> {
        let fs = self.host.fs.as_ref();
        for path in fs.list(&PathBuf::from("knowledge/usage"))? {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let mut entries: Vec<UsageLogEntry> = match read_json(fs, &path) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let before = entries.len();
            let mut touched = false;
            for e in &mut entries {
                let n = e.fragments.len();
                e.fragments.retain(|f| f.pack_id != pack_id);
                if e.fragments.len() != n {
                    touched = true;
                }
            }
            entries.retain(|e| !e.fragments.is_empty());
            if touched || entries.len() != before {
                write_json(fs, &path, &entries)?;
            }
        }
        Ok(())
    }

    /// 追加删除审计记录。
    fn append_deleted_audit(&self, pack_id: &str) -> Result<(), EngineError> {
        let fs = self.host.fs.as_ref();
        let path = deleted_path();
        let mut audit: Vec<DeletedRecord> =
            if fs.exists(&path) { read_json(fs, &path)? } else { Vec::new() };
        audit.push(DeletedRecord {
            pack_id: pack_id.to_string(),
            deleted_at: self.host.clock.now_ms(),
        });
        write_json(fs, &path, &audit)
    }

    pub fn get_pack(&self, pack_id: &str) -> Result<KnowledgePack, EngineError> {
        crate::store::read_json(self.host.fs.as_ref(), &pack_path(pack_id))
    }
    pub fn list_packs(&self) -> Result<Vec<KnowledgePack>, EngineError> {
        let fs = self.host.fs.as_ref();
        let mut out: Vec<KnowledgePack> = Vec::new();
        for path in fs.list(&PathBuf::from("knowledge/packs"))? {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(pack) = read_json::<KnowledgePack>(fs, &path) {
                out.push(pack);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id)); // 确定性顺序
        Ok(out)
    }
}

/// 时代序比较：`boundary` 是否严格晚于 `as_of`。两者均能解析为整数（年份/纪元序，负数=公元前）
/// 时按数值比较，否则退回字典序。纯函数、确定性，供时间边界过滤复用。
fn era_after(boundary: &str, as_of: &str) -> bool {
    let b = boundary.trim();
    let a = as_of.trim();
    match (b.parse::<i64>(), a.parse::<i64>()) {
        (Ok(bn), Ok(an)) => bn > an,
        _ => b > a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::{EngineEvent, EngineHost};
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};

    fn dummy_profile() -> ModelProfile {
        ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "http://x".into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    fn make_host(responses: Vec<Result<String, EngineError>>) -> (Arc<EngineHost>, Arc<CollectEvents>) {
        let fs = Arc::new(MemFs::default());
        let events = Arc::new(CollectEvents::default());
        let model = Arc::new(ScriptedModel::new(responses));
        let host = Arc::new(EngineHost {
            fs,
            clock: Arc::new(FixedClock(1_000)),
            events: events.clone(),
            model,
        });
        (host, events)
    }

    fn put_source(host: &EngineHost, key: &str, content: &str) {
        host.fs.write_atomic(Path::new(key), content.as_bytes()).unwrap();
    }

    fn model_calls(events: &CollectEvents) -> usize {
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::ModelCall(_)))
            .count()
    }

    fn mind_prompts() -> DistillPrompts {
        DistillPrompts {
            system_by_mode: [("mind".to_string(), "你是分析方法蒸馏器".to_string())]
                .into_iter()
                .collect(),
            prompt_version: "v1".into(),
        }
    }

    // ---- 导入 / 索引复用 ----

    #[test]
    fn import_builds_pack_and_index() {
        let (host, _) = make_host(vec![]);
        put_source(&host, "a.txt", &"内容段落".repeat(500));
        let ks = KnowledgeSystem::new(host.clone());
        let (pack, stats) = ks
            .import_source("a.txt", "标题", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        assert!(stats.chunk_count >= 1);
        assert_eq!(pack.mode, PackMode::Knowledge);
        assert!(pack.index_version.ends_with(CHUNKER_VERSION));
        assert!(host.fs.exists(&pack_path(&pack.id)));
        assert!(host.fs.exists(Path::new(&pack.chunk_index_store_key)));
    }

    #[test]
    fn reimport_same_source_reuses_index() {
        let (host, _) = make_host(vec![]);
        put_source(&host, "a.txt", "复用测试的相同内容。");
        let ks = KnowledgeSystem::new(host.clone());
        let (a, _) = ks
            .import_source("a.txt", "A", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        let (b, _) = ks
            .import_source("a.txt", "B", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a.index_version, b.index_version);
        assert_eq!(a.chunk_index_store_key, b.chunk_index_store_key); // 共享同一索引文件
    }

    // ---- 权利边界硬规则 ----

    #[tokio::test]
    async fn distill_without_remote_permission_rejected_and_no_call() {
        let (host, events) = make_host(vec![]); // 空脚本：一旦调用即耗尽
        put_source(&host, "s.txt", &"拿破仑在滑铁卢的战术决策与失误。".repeat(30));
        let ks = KnowledgeSystem::new(host.clone());
        let (pack, _) = ks
            .import_source("s.txt", "战史", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        let err = ks
            .distill(&pack.id, PackMode::Mind, &dummy_profile(), &mind_prompts(), &CancelFlag::new())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "validation");
        assert_eq!(model_calls(&events), 0); // 未发任何模型调用
    }

    // ---- 蒸馏成功 / schema 校验 / 共享索引 ----

    #[tokio::test]
    async fn distill_mind_produces_heuristics_and_shares_index() {
        let good =
            r#"{"decisionHeuristics":[{"when":"面对不确定","prefer":"先枚举证据","avoid":"臆断"}]}"#
                .to_string();
        let (host, events) = make_host(vec![Ok(good)]);
        put_source(&host, "s.txt", &"孙子曰：兵者，国之大事，死生之地。".repeat(50));
        let ks = KnowledgeSystem::new(host.clone());
        let (base, _) = ks
            .import_source(
                "s.txt",
                "兵法",
                RightsBasis::Owned,
                vec![AllowedUse::Retrieve, AllowedUse::SendToRemoteModel],
                Retention::ManagedCopy,
            )
            .unwrap();
        let pack = ks
            .distill(&base.id, PackMode::Mind, &dummy_profile(), &mind_prompts(), &CancelFlag::new())
            .await
            .unwrap();
        assert_eq!(pack.mode, PackMode::Mind);
        assert!(!pack.distilled.decision_heuristics.as_ref().unwrap().is_empty());
        assert_ne!(pack.id, base.id); // 复制新 id
        assert_eq!(pack.chunk_index_store_key, base.chunk_index_store_key); // 共享索引
        assert_eq!(model_calls(&events), 1);
    }

    #[tokio::test]
    async fn distill_retries_on_missing_required_field() {
        let bad = r#"{"principles":["x"]}"#.to_string(); // mind 缺 decisionHeuristics
        let good = r#"{"decisionHeuristics":[{"when":"w","prefer":"p"}]}"#.to_string();
        let (host, events) = make_host(vec![Ok(bad), Ok(good)]);
        put_source(&host, "s.txt", &"内容".repeat(200));
        let ks = KnowledgeSystem::new(host.clone());
        let (base, _) = ks
            .import_source("s.txt", "t", RightsBasis::Owned, vec![AllowedUse::SendToRemoteModel], Retention::IndexOnly)
            .unwrap();
        let pack = ks
            .distill(&base.id, PackMode::Mind, &dummy_profile(), &mind_prompts(), &CancelFlag::new())
            .await
            .unwrap();
        assert!(pack.distilled.decision_heuristics.is_some());
        assert_eq!(model_calls(&events), 2); // 一次坏一次好
    }

    // ---- 检索作用域与绑定隔离 ----

    #[test]
    fn search_scoped_to_requested_packs_and_bindings() {
        let (host, _) = make_host(vec![]);
        put_source(&host, "a.txt", "军师推演战术与战略，粮草与地形皆需谋划。");
        put_source(&host, "b.txt", "厨师研究战术般的火候，处理苹果与面粉。");
        let ks = KnowledgeSystem::new(host.clone());
        let (a, _) = ks
            .import_source("a.txt", "A", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        let (b, _) = ks
            .import_source("b.txt", "B", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        let bind = |id: &str, pack: &str, ch: &str, on: bool| KnowledgeBinding {
            id: id.into(),
            pack_id: pack.into(),
            character_id: ch.into(),
            story_id: None,
            influence: 1.0,
            enabled: on,
            conflict_policy: ConflictPolicy::CharacterCoreWins,
        };
        ks.upsert_binding(bind("bd-a", &a.id, "charX", true)).unwrap();
        ks.upsert_binding(bind("bd-b", &b.id, "charY", true)).unwrap();

        // charX 只能看到 A 的片段，B 的 0 次进入。
        let packs_x: Vec<String> = ks
            .list_bindings()
            .unwrap()
            .into_iter()
            .filter(|bd| bd.enabled && bd.character_id == "charX")
            .map(|bd| bd.pack_id)
            .collect();
        let frags = ks.search(&packs_x, "战术", 5).unwrap();
        assert!(!frags.is_empty());
        assert!(frags.iter().all(|f| f.pack_id == a.id));

        // 停用 A 绑定 → 组装结果不含其片段。
        ks.upsert_binding(bind("bd-a", &a.id, "charX", false)).unwrap();
        let packs_x2: Vec<String> = ks
            .list_bindings()
            .unwrap()
            .into_iter()
            .filter(|bd| bd.enabled && bd.character_id == "charX")
            .map(|bd| bd.pack_id)
            .collect();
        assert!(packs_x2.is_empty());
        assert!(ks.search(&packs_x2, "战术", 5).unwrap().is_empty());
    }

    // ---- 时间边界确定性过滤（§4.3 第 5 条 / §11.4）----

    #[test]
    fn search_filters_pack_beyond_character_era() {
        let (host, _) = make_host(vec![]);
        put_source(&host, "modern.txt", "量子计算与互联网时代的战术推演与信息作战分析。");
        let ks = KnowledgeSystem::new(host.clone());
        let (mut pack, _) = ks
            .import_source("modern.txt", "现代科技", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        // 标注该包内容时代为公元 2000 年（晚于身处 1800 年的角色）。
        pack.time_boundary = Some("2000".into());
        write_json(host.fs.as_ref(), &pack_path(&pack.id), &pack).unwrap();
        let ids = vec![pack.id.clone()];

        // as_of=1800：包时代 2000 晚于角色时代 1800 → 整包不返回（越界 0）。
        assert!(ks.search_as_of(&ids, "战术", 5, Some("1800")).unwrap().is_empty());
        // as_of=2100：包时代 2000 不晚于 2100 → 正常返回。
        assert!(!ks.search_as_of(&ids, "战术", 5, Some("2100")).unwrap().is_empty());
        // 无 as_of（向后兼容 search）：不过滤 → 正常返回。
        assert!(!ks.search(&ids, "战术", 5).unwrap().is_empty());
    }

    #[test]
    fn search_as_of_pack_without_boundary_never_filtered_and_partial_filter() {
        let (host, _) = make_host(vec![]);
        // A 无时间边界；B 时代晚于角色。
        put_source(&host, "a.txt", "古代兵法中的战术与阵法。");
        put_source(&host, "b.txt", "近代火器战术的革新历程。");
        let ks = KnowledgeSystem::new(host.clone());
        let (a, _) = ks
            .import_source("a.txt", "A", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        let (mut b, _) = ks
            .import_source("b.txt", "B", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::IndexOnly)
            .unwrap();
        b.time_boundary = Some("1900".into());
        write_json(host.fs.as_ref(), &pack_path(&b.id), &b).unwrap();

        // as_of=1600：A（无边界）保留，B（1900>1600）被过滤——只返回 A 的片段。
        let ids = vec![a.id.clone(), b.id.clone()];
        let frags = ks.search_as_of(&ids, "战术", 5, Some("1600")).unwrap();
        assert!(!frags.is_empty());
        assert!(frags.iter().all(|f| f.pack_id == a.id), "B 应被时间边界过滤");
    }

    // ---- 使用日志追加 / 溯源 ----

    #[test]
    fn usage_appends_per_run() {
        let (host, _) = make_host(vec![]);
        let ks = KnowledgeSystem::new(host.clone());
        let e1 = UsageLogEntry {
            run_id: "run-1".into(),
            scene_id: "s1".into(),
            character_id: "c1".into(),
            fragments: vec![UsageFragmentRef { pack_id: "p".into(), chunk_id: "p#0".into(), ordinal: 0 }],
            at: 1,
        };
        let mut e2 = e1.clone();
        e2.scene_id = "s2".into();
        ks.record_usage(&e1).unwrap();
        ks.record_usage(&e2).unwrap();
        assert_eq!(ks.get_usage("run-1").unwrap().len(), 2);
        assert!(ks.get_usage("run-x").unwrap().is_empty());
    }

    // ---- 绑定 CRUD ----

    #[test]
    fn binding_crud() {
        let (host, _) = make_host(vec![]);
        let ks = KnowledgeSystem::new(host.clone());
        let mk = |on: bool| KnowledgeBinding {
            id: "bd".into(),
            pack_id: "p".into(),
            character_id: "c".into(),
            story_id: None,
            influence: 0.5,
            enabled: on,
            conflict_policy: ConflictPolicy::AskUser,
        };
        ks.upsert_binding(mk(true)).unwrap();
        assert_eq!(ks.list_bindings().unwrap().len(), 1);
        ks.upsert_binding(mk(false)).unwrap(); // 同 id 更新
        let all = ks.list_bindings().unwrap();
        assert_eq!(all.len(), 1);
        assert!(!all[0].enabled);
        ks.remove_binding("bd").unwrap();
        assert!(ks.list_bindings().unwrap().is_empty());
    }

    // ---- 级联删除 ----

    #[test]
    fn delete_pack_cascades_and_keeps_audit() {
        let (host, _) = make_host(vec![]);
        put_source(&host, "a.txt", "军师推演战术与战略。");
        let ks = KnowledgeSystem::new(host.clone());
        let (a, _) = ks
            .import_source("a.txt", "A", RightsBasis::Owned, vec![AllowedUse::Retrieve], Retention::ManagedCopy)
            .unwrap();
        ks.upsert_binding(KnowledgeBinding {
            id: "bd".into(),
            pack_id: a.id.clone(),
            character_id: "c".into(),
            story_id: None,
            influence: 1.0,
            enabled: true,
            conflict_policy: ConflictPolicy::CharacterCoreWins,
        })
        .unwrap();
        ks.record_usage(&UsageLogEntry {
            run_id: "run-1".into(),
            scene_id: "s".into(),
            character_id: "c".into(),
            fragments: vec![UsageFragmentRef { pack_id: a.id.clone(), chunk_id: "x".into(), ordinal: 0 }],
            at: 1,
        })
        .unwrap();

        assert!(host.fs.exists(Path::new(&a.chunk_index_store_key)));
        assert!(host.fs.exists(&managed_source_path(&a.id)));

        ks.delete_pack(&a.id).unwrap();

        // 正文全部不可访问
        assert_eq!(ks.get_pack(&a.id).unwrap_err().code(), "not_found");
        assert!(!host.fs.exists(Path::new(&a.chunk_index_store_key)));
        assert!(!host.fs.exists(&managed_source_path(&a.id)));
        // 绑定同步移除
        assert!(ks.list_bindings().unwrap().is_empty());
        // 使用日志中该包引用清空（该条仅含 A → 整条移除）
        assert!(ks.get_usage("run-1").unwrap().is_empty());
        // 检索入口不可再访问
        assert!(ks.search(&[a.id.clone()], "战术", 5).unwrap().is_empty());
        // 仅保留 {packId, deletedAt} 审计
        let audit: Vec<DeletedRecord> = read_json(host.fs.as_ref(), &deleted_path()).unwrap();
        assert!(audit.iter().any(|r| r.pack_id == a.id && r.deleted_at == 1_000));
    }

    #[test]
    fn delete_keeps_shared_index_for_other_packs() {
        // A 与其蒸馏副本共享索引：删 A 后索引仍应保留给副本使用。
        let (host, _) = make_host(vec![]);
        put_source(&host, "a.txt", "共享索引的战术资料内容。");
        let ks = KnowledgeSystem::new(host.clone());
        let (a, _) = ks
            .import_source(
                "a.txt",
                "A",
                RightsBasis::Owned,
                vec![AllowedUse::Retrieve, AllowedUse::SendToRemoteModel],
                Retention::IndexOnly,
            )
            .unwrap();
        // 手工造一个共享同一 store key 的副本（模拟蒸馏产物）。
        let mut copy = a.clone();
        copy.id = "kp-copy".into();
        write_json(host.fs.as_ref(), &pack_path(&copy.id), &copy).unwrap();

        ks.delete_pack(&a.id).unwrap();
        assert!(host.fs.exists(Path::new(&a.chunk_index_store_key))); // 索引保留
        assert!(!ks.search(&["kp-copy".to_string()], "战术", 5).unwrap().is_empty());
    }
}
