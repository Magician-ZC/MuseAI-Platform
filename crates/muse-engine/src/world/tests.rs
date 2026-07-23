//! WorldExtractionPipeline 全链路测试（ScriptedModel 按调用顺序脚本化各阶段）。

use super::*;
use crate::character::types::{CardLifecycle, RosterTier};
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

fn request(source_path: String) -> WorldExtractionRequest {
    WorldExtractionRequest {
        work_title: "剑冢录".into(),
        source_path,
        profile: ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "u".into(),
            api_key: "k".into(),
            model: "m".into(),
        },
        prompts: WorldPrompts::uniform("s"),
        temperature: 0.0,
        max_output_tokens: 2048,
        concurrency: 2,
    }
}

// 两章书：ch0 含角色/地点/道具/剧情节拍，ch1 含角色/结局线索。各章 >50 字避免超短合并。
fn write_book() -> tempfile::NamedTempFile {
    let pad = "他沉默良久环顾四周反复思量始终不语".repeat(4);
    let text = format!(
        "第一章\n谢云走进无尽剑冢，取走焚寂剑，剑冢试炼开启。{pad}\n第二章\n谢云布局，最终同归于尽。{pad}"
    );
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(text.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

// scan 响应：quote 均逐字取自对应章正文。
fn scan0() -> String {
    r#"{"chapterIndex":0,"mentions":[
        {"kind":"character","surface":"谢云","roleHint":"反派","evidence":[{"kind":"action","quote":"谢云走进无尽剑冢","confidence":"high"}]},
        {"kind":"location","surface":"无尽剑冢","roleHint":"秘境","links":["剑冢入口"],"evidence":[{"kind":"description","quote":"谢云走进无尽剑冢","confidence":"high"}]},
        {"kind":"item","surface":"焚寂剑","roleHint":"cultivation","evidence":[{"kind":"action","quote":"取走焚寂剑","confidence":"high"}]},
        {"kind":"plotBeat","surface":"剑冢试炼","evidence":[{"kind":"description","quote":"剑冢试炼开启","confidence":"medium"}]}
    ]}"#.into()
}
fn scan1() -> String {
    r#"{"chapterIndex":1,"mentions":[
        {"kind":"character","surface":"谢云","roleHint":"反派","evidence":[{"kind":"action","quote":"谢云布局","confidence":"high"}]},
        {"kind":"endingClue","surface":"同归于尽","roleHint":"combat","evidence":[{"kind":"inference","quote":"同归于尽","confidence":"medium"}]}
    ]}"#.into()
}
fn item_synth() -> String {
    r#"{"worldItems":[{"id":"itm-fenji","narrative":"焚寂，会呼吸的凶剑","effectTags":["advantage:combat"],"origin":{"cosmology":["cultivation"],"powerTier":4}}]}"#.into()
}
fn loc_synth() -> String {
    r#"{"locations":[{"id":"loc-jiantomb","name":"无尽剑冢","connections":[],"isSecretRealm":true,"gate":{"requiredItemIds":[],"requiredCosmologies":["cultivation"],"maxPowerTier":4},"residentItemIds":["itm-fenji"]}]}"#.into()
}
fn char_synth() -> String {
    r#"{"identity":{"narrativeRole":"villain"},"dramaticCore":{"coreContradiction":"复仇执念","coreFear":"被遗忘"},"agency":{"longTermAgenda":"血洗剑派"}}"#.into()
}
fn plot_synth() -> String {
    r#"{
        "mainlineNodes":[
            {"id":"mn-1","fated":true,"variantGroup":"vg-trial","arcTags":["arc-revenge"]},
            {"id":"mn-2","fated":false,"variantGroup":"vg-trial","arcTags":["arc-revenge"]},
            {"id":"mn-3","fated":false,"arcTags":["arc-revenge"]}
        ],
        "hiddenContentPool":[
            {"id":"hc-1","themes":["复仇"],"template":"{name}发现{seed}","rewardItemRef":"itm-fenji","variantGroup":"vg-secret","arcTags":["arc-revenge"]},
            {"id":"hc-2","themes":["背叛"],"template":"{name}遭遇{seed}","variantGroup":"vg-secret","arcTags":["arc-revenge"]}
        ],
        "sideHookPool":[{"id":"sh-1","themes":[],"template":"支线钩子","arcTags":["arc-revenge"]}],
        "storylines":[{"id":"arc-revenge","summary":"复仇线","mainlineNodeIds":["mn-1","mn-2","mn-3"],"hiddenPoolIds":["hc-1","hc-2"],"endingIds":["end-1"],"affinity":"combat"}]
    }"#.into()
}
fn ending_synth() -> String {
    r#"{"endingPool":[{"id":"end-1","affinity":"combat","baseWeight":1.0,"arcTags":["arc-revenge"]}]}"#.into()
}

// ---------- 测试 1：create_task 切章持久化 ----------
#[test]
fn create_task_splits_and_persists() {
    let book = write_book();
    let host = make_host(ScriptedModel::new(vec![]));
    let pipe = WorldExtractionPipeline::new(host);
    let task = pipe.create_task(&request(book.path().to_string_lossy().to_string())).unwrap();
    assert_eq!(task.chapters.len(), 2);
    assert!(matches!(task.stage, WorldStage::Scan));
    assert!(!task.source_fingerprint.content_hash.is_empty());
    assert_eq!(pipe.get_task(&task.task_id).unwrap().chapters.len(), 2);
}

// ---------- 测试 2/4/7/9：全链路 + 多维度分流 + 反派分层 + 超集元数据 ----------
#[tokio::test]
async fn full_pipeline_produces_world_superset() {
    let book = write_book();
    let host = make_host(ScriptedModel::new(vec![
        Ok(scan0()),
        Ok(scan1()),
        Ok(item_synth()),
        Ok(loc_synth()),
        Ok(char_synth()),
        Ok(plot_synth()),
        Ok(ending_synth()),
    ]));
    let pipe = WorldExtractionPipeline::new(host);
    let req = request(book.path().to_string_lossy().to_string());
    let task = pipe.create_task(&req).unwrap();

    let reviewed = pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap();
    assert!(matches!(reviewed.stage, WorldStage::Review));
    assert!(reviewed.chapters.iter().all(|c| matches!(c.status, ChapterStatus::Scanned)));

    // 测试 4：多维度分流。
    assert_eq!(reviewed.character_roster.len(), 1);
    assert_eq!(reviewed.character_roster[0].canonical_name, "谢云");
    assert_eq!(reviewed.location_roster.len(), 1);
    assert_eq!(reviewed.location_roster[0].canonical_name, "无尽剑冢");
    assert!(reviewed.location_roster[0].is_secret_realm); // 秘境提示落位
    assert_eq!(reviewed.item_roster.len(), 1);
    assert_eq!(reviewed.item_roster[0].canonical_name, "焚寂剑");
    assert_eq!(reviewed.plot_beats.len(), 1);
    assert_eq!(reviewed.plot_beats[0].surface, "剑冢试炼");
    assert_eq!(reviewed.ending_clues.len(), 1);
    assert_eq!(reviewed.ending_clues[0].surface, "同归于尽");

    // 测试 7：高出场反派 → Core。
    assert_eq!(reviewed.character_roster[0].tier, RosterTier::Core);

    // 覆盖报告。
    let cov = pipe.coverage_report(&task.task_id).unwrap();
    assert_eq!(cov.scanned_chapters, 2);
    assert_eq!(cov.total_chapters, 2);
    assert_eq!(cov.character_roster_size, 1);
    assert_eq!(cov.location_roster_size, 1);
    assert_eq!(cov.item_roster_size, 1);

    // 确认三 roster 全部入超集。
    let chars: Vec<_> = reviewed.character_roster.iter().cloned().map(confirm_char).collect();
    let locs: Vec<_> = reviewed.location_roster.iter().cloned().map(confirm_entity).collect();
    let items: Vec<_> = reviewed.item_roster.iter().cloned().map(confirm_entity).collect();
    let confirmed = pipe.confirm_rosters(&task.task_id, reviewed.revision, chars, locs, items).unwrap();
    assert!(confirmed.character_roster.iter().all(|e| e.user_confirmed));

    // 合成超集。
    let draft = pipe.synthesize_superset(&task.task_id, &req, &CancelFlag::new()).await.unwrap();

    // 测试 2：各维度非空，NPC 卡 Draft。
    assert_eq!(draft.world_characters.len(), 1);
    assert!(matches!(draft.world_characters[0].card.lifecycle, CardLifecycle::Draft));
    assert_eq!(draft.world_characters[0].card.dramatic_core.core_contradiction, "复仇执念");
    assert_eq!(draft.world_characters[0].card.identity.name, "谢云");
    assert_eq!(draft.locations.len(), 1);
    assert!(draft.locations[0].is_secret_realm);
    assert_eq!(draft.locations[0].resident_item_ids, vec!["itm-fenji".to_string()]); // 合法引用保留
    assert_eq!(draft.world_items.len(), 1);
    assert_eq!(draft.world_items[0].id, "itm-fenji");
    assert_eq!(draft.mainline_nodes.len(), 3);
    assert_eq!(draft.ending_pool.len(), 1);

    // 测试 9：超集元数据。
    assert!(draft.is_superset);
    assert_eq!(draft.storylines.len(), 1);
    let arc = &draft.storylines[0];
    assert_eq!(arc.id, "arc-revenge");
    assert_eq!(arc.mainline_node_ids.len(), 3); // 引用自洽（无悬空）
    assert_eq!(arc.hidden_pool_ids.len(), 2);
    assert_eq!(arc.ending_ids, vec!["end-1".to_string()]);
    // variantGroup 每组 ≥2 成员：vg-trial(mn-1,mn-2)、vg-secret(hc-1,hc-2)。
    assert_eq!(draft.mainline_nodes[0].variant_group, Some("vg-trial".to_string()));
    assert_eq!(draft.mainline_nodes[1].variant_group, Some("vg-trial".to_string()));
    assert_eq!(draft.mainline_nodes[2].variant_group, None); // 未分组
    assert_eq!(draft.hidden_content_pool[0].variant_group, Some("vg-secret".to_string()));
    assert!(group_members_ge2(&draft));
    // hidden pool 合法 rewardItemRef 保留。
    assert_eq!(draft.hidden_content_pool[0].reward_item_ref, Some("itm-fenji".to_string()));
    // sampling 冗余倍率有效。
    assert!(draft.sampling.redundancy_ratio > 0.0);
    assert!(draft.sampling.instance_mainline_count >= 1);

    // 任务进入 Assembled。
    assert!(matches!(pipe.get_task(&task.task_id).unwrap().stage, WorldStage::Assembled));

    // 超集序列化字段名对齐 server Skeleton（camelCase）。
    let json = serde_json::to_value(&draft).unwrap();
    assert!(json.get("worldCharacters").is_some());
    assert!(json.get("worldItems").is_some());
    assert!(json.get("mainlineNodes").is_some());
    assert!(json.get("hiddenContentPool").is_some());
    assert!(json.get("endingPool").is_some());
    assert_eq!(json.get("isSuperset").and_then(|v| v.as_bool()), Some(true));
}

// ---------- 测试 8：恢复幂等，不重扫 ----------
#[tokio::test]
async fn rerun_is_idempotent_and_skips_rescan() {
    let book = write_book();
    // 仅 2 条 scan 响应；若二次运行重扫会耗尽脚本报错。
    let host = make_host(ScriptedModel::new(vec![Ok(scan0()), Ok(scan1())]));
    let pipe = WorldExtractionPipeline::new(host);
    let req = request(book.path().to_string_lossy().to_string());
    let task = pipe.create_task(&req).unwrap();
    let first = pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap();
    assert!(matches!(first.stage, WorldStage::Review));
    let second = pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap();
    assert!(matches!(second.stage, WorldStage::Review));
    assert_eq!(second.character_roster.len(), 1);
}

// ---------- 测试：fingerprint 变化 → Conflict ----------
#[tokio::test]
async fn source_change_rejected_on_resume() {
    let book = write_book();
    let host = make_host(ScriptedModel::new(vec![Ok(scan0()), Ok(scan1())]));
    let pipe = WorldExtractionPipeline::new(host);
    let req = request(book.path().to_string_lossy().to_string());
    let task = pipe.create_task(&req).unwrap();
    pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap();
    // 改写源文件 → 指纹变化。
    {
        let mut f = std::fs::OpenOptions::new().write(true).truncate(true).open(book.path()).unwrap();
        f.write_all("第一章\n完全不同的内容用于改变指纹哈希值一二三四五六七八九十".as_bytes()).unwrap();
        f.flush().unwrap();
    }
    let err = pipe.run_until_review(&task.task_id, &req, &CancelFlag::new()).await.unwrap_err();
    assert_eq!(err.code(), "conflict");
}

// ---------- 测试 10：取消 ----------
#[tokio::test]
async fn precancelled_run_stops_and_marks_cancelled() {
    let book = write_book();
    let host = make_host(ScriptedModel::new(vec![]));
    let pipe = WorldExtractionPipeline::new(host);
    let req = request(book.path().to_string_lossy().to_string());
    let task = pipe.create_task(&req).unwrap();
    let cancel = CancelFlag::new();
    cancel.cancel();
    let err = pipe.run_until_review(&task.task_id, &req, &cancel).await.unwrap_err();
    assert_eq!(err.code(), "cancelled");
    assert!(matches!(pipe.get_task(&task.task_id).unwrap().stage, WorldStage::Cancelled));
}

#[test]
fn cancel_marks_stage_and_emits_event() {
    let book = write_book();
    let collect = Arc::new(CollectEvents::default());
    let host = Arc::new(EngineHost {
        fs: Arc::new(MemFs::default()),
        clock: Arc::new(FixedClock(1_000)),
        events: collect.clone(),
        model: Arc::new(ScriptedModel::new(vec![])),
    });
    let pipe = WorldExtractionPipeline::new(host);
    let task = pipe.create_task(&request(book.path().to_string_lossy().to_string())).unwrap();
    assert!(pipe.cancel(&task.task_id).unwrap());
    assert!(matches!(pipe.get_task(&task.task_id).unwrap().stage, WorldStage::Cancelled));
    assert!(pipe.cancel(&task.task_id).unwrap()); // 幂等
    let evs = collect.0.lock().unwrap();
    assert!(evs.iter().any(|e| matches!(e, EngineEvent::Task { .. })));
}

fn confirm_char(mut e: RosterEntry) -> RosterEntry {
    e.user_confirmed = true;
    e
}
fn confirm_entity(mut e: WorldRosterEntry) -> WorldRosterEntry {
    e.user_confirmed = true;
    e
}

/// 校验每个具名 variantGroup 至少 2 成员（跨全维度统计）。
fn group_members_ge2(draft: &WorldSkeletonDraft) -> bool {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for n in &draft.mainline_nodes {
        if let Some(g) = &n.variant_group {
            *counts.entry(g.clone()).or_default() += 1;
        }
    }
    for p in draft.hidden_content_pool.iter().chain(draft.side_hook_pool.iter()) {
        if let Some(g) = &p.variant_group {
            *counts.entry(g.clone()).or_default() += 1;
        }
    }
    for e in &draft.ending_pool {
        if let Some(g) = &e.variant_group {
            *counts.entry(g.clone()).or_default() += 1;
        }
    }
    counts.values().all(|&c| c >= 2)
}
