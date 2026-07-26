//! 录制 / 回放接线的用例。
//!
//! 分四层，从「开关判定」一路锁到「黄金世界端到端」：
//!
//! | 层 | 锁什么 |
//! |---|---|
//! | §1 开关判定 | 默认 Off；假值当没设；录制/回放互斥；非法 id、相对目录一律不启用 |
//! | §2 恒等性 | **Off 返回的是同一个 `Arc`**（`ptr_eq`）+ 不建目录、不写文件 |
//! | §3 端到端 | 黄金世界主线跑三遍（关 / 录 / 放），结构化产物**逐字节相等** |
//! | §4 纪律 | 回放未命中**明确失败**、绝不回落；录制产物**不含凭据** |
//!
//! ## ⚠️ 这些用例**不**证明什么（VALIDATION §0.3）
//!
//! §3 里的"模型"是 `golden::ScriptedModel`——**人写的剧本**。它证明的是
//! 「录得下来、放得回去、且默认关闭时管线一字节不变」，**不是**「角色一致性已验证」。
//! 真实模型录制需要用户自己的 API Key（本仓没有），录制入口见
//! [`record_golden_world_with_real_model`]（`#[ignore]`，需用户显式提供凭据才跑）。
//!
//! 录制产物自身也带这条信息：`labels.responseSource` = `scriptedStub` / `real`。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use muse_engine::host::{CancelFlag, HostFs, StdFs};
use muse_engine::model::{ModelCallSpec, ModelClient, ModelInterface, ModelOutput};
use muse_engine::replay::{MatchMode, Recording, RecordingMeta, RECORDING_FORMAT_VERSION};
use muse_engine::EngineError;

use super::*;
use crate::app::AppState;
use crate::db::new_id;
use crate::runtime::golden::{
    golden_snapshot, main_scripted_model, seed_golden_world, GoldenParams, ScriptedModel, MAIN_TICKS,
};
use crate::runtime::tests::test_state;
use crate::runtime::{insert_tick, process_tick_with_model, TickStatus};
use crate::worlds::load_world;

// ============================================================================
// 脚手架
// ============================================================================

/// 只用来占位的 `ModelClient`（§2 恒等性用例不真的跑回合）。
struct NullModel;

#[async_trait]
impl ModelClient for NullModel {
    async fn complete(&self, _spec: &ModelCallSpec, _cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        Err(EngineError::Model { message: "不该被调用".into(), retryable: false })
    }
}

fn tmp_root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(new_id(tag))
}

fn tmp_fs(tag: &str) -> Arc<dyn HostFs> {
    Arc::new(StdFs::new(tmp_root(tag)))
}

/// 驱动一拍：对齐剧本时钟 → 按当前 revision 排 tick → 走生产同路径 `process_tick_with_model`。
/// 与 `golden::drive_tick` 同构，只是模型由调用方给（回放跑时给的是个用不到的占位模型）。
async fn drive_tick(
    state: &AppState,
    scripted: &Arc<ScriptedModel>,
    world_id: &str,
    tick_no: i64,
) -> TickStatus {
    let rev = load_world(&state.db, world_id).await.unwrap().state_revision;
    insert_tick(&state.db, world_id, tick_no, rev).await.unwrap();
    scripted.set_tick(tick_no);
    let mc: Arc<dyn ModelClient> = scripted.clone();
    process_tick_with_model(state, world_id, tick_no, mc).await.unwrap()
}

/// 跑一遍黄金主线并返回 (逐拍状态, 结构化产物快照)。
///
/// 每跑之前清一次进程级僵局账：`stall_tracker()` 按 world_id 分键、跨 state 存活，
/// 三个阶段用的是**同一个 world_id**（不同 id ⇒ 不同 `instance_seed` ⇒ 不同副本 ⇒ 快照没法比），
/// 不清就会出现「阶段 B 带着阶段 A 的 stall_hint 跑」——prompt 变了，回放必然全部未命中。
async fn run_main(state: &AppState, world_id: &str) -> (Vec<TickStatus>, String) {
    crate::runtime::stall_tracker().clear(world_id);
    let scripted = main_scripted_model();
    let mut statuses = Vec::new();
    for tick_no in 0..MAIN_TICKS {
        statuses.push(drive_tick(state, &scripted, world_id, tick_no).await);
    }
    let snap = golden_snapshot(&state.db, world_id).await;
    (statuses, snap)
}

/// 把世界钉住的模型路由换成给定凭据（录制脱敏用例 / 真实模型录制入口共用）。
async fn override_routes(state: &AppState, base_url: &str, api_key: &str, model: &str, interface: &str) {
    let routes = json!({
        "default": { "interface": interface, "baseUrl": base_url, "apiKey": api_key, "model": model }
    });
    sqlx::query("UPDATE model_routes SET routes_json = $1")
        .bind(routes.to_string())
        .execute(&state.db)
        .await
        .unwrap();
}

// ============================================================================
// §1 开关判定（纯函数，不碰 env / 不碰全局）
// ============================================================================

#[test]
fn empty_env_is_off_without_any_note() {
    let d = decide_capture(&RawCaptureEnv::default(), "wld-x");
    assert_eq!(d.capture, TickCapture::Off);
    // 🔴 notes 必须为空：默认路径上连一条日志都不该有。
    assert!(d.notes.is_empty(), "默认配置不该产生任何告警：{:?}", d.notes);
}

#[test]
fn falsy_values_are_treated_as_unset() {
    for v in ["", "  ", "0", "false", "off", "OFF", "False"] {
        let env = RawCaptureEnv { record: Some(v.into()), ..Default::default() };
        assert_eq!(
            decide_capture(&env, "wld-x").capture,
            TickCapture::Off,
            "{ENV_RECORD}={v:?} 是人类表达「关掉」的写法，不得被当成一个叫 {v:?} 的录制 id"
        );
    }
}

#[test]
fn record_and_replay_together_disables_both() {
    let env = RawCaptureEnv {
        record: Some("rec-a".into()),
        replay: Some("rec-b".into()),
        ..Default::default()
    };
    let d = decide_capture(&env, "wld-x");
    assert_eq!(d.capture, TickCapture::Off, "语义互斥的两个开关同时打开时，必须两个都不启用");
    assert!(d.notes.iter().any(|n| n.contains("互斥")), "必须说清为什么关掉了：{:?}", d.notes);
}

#[test]
fn illegal_recording_id_disables_capture() {
    // 录制 id 同时是文件名，`../` 这类必须在判定期就被挡掉（引擎侧 HostFs 还有一道）。
    for bad in ["../escape", "a/b", "", ".hidden", "有中文"] {
        let env = RawCaptureEnv { record: Some(bad.into()), ..Default::default() };
        assert_eq!(decide_capture(&env, "wld-x").capture, TickCapture::Off, "非法 id {bad:?} 不得启用录制");
    }
}

#[test]
fn relative_record_dir_disables_capture() {
    let env = RawCaptureEnv {
        record: Some("rec-a".into()),
        dir: Some("recordings".into()),
        ..Default::default()
    };
    let d = decide_capture(&env, "wld-x");
    assert_eq!(d.capture, TickCapture::Off);
    assert!(d.notes.iter().any(|n| n.contains("绝对路径")), "{:?}", d.notes);
}

#[test]
fn world_filter_scopes_capture_to_one_world() {
    let env = RawCaptureEnv {
        record: Some("rec-a".into()),
        world: Some("wld-hit".into()),
        ..Default::default()
    };
    assert!(matches!(decide_capture(&env, "wld-hit").capture, TickCapture::Record { .. }));
    let miss = decide_capture(&env, "wld-other");
    assert_eq!(miss.capture, TickCapture::Off);
    // 「不是这个世界」是正常情况，不是配置错误 —— 每拍刷一条告警毫无信息量。
    assert!(miss.notes.is_empty(), "世界过滤未命中不该告警：{:?}", miss.notes);
}

#[test]
fn replay_match_mode_defaults_to_prompt_and_flags_slot() {
    let base = RawCaptureEnv { replay: Some("rec-a".into()), ..Default::default() };
    assert!(matches!(
        decide_capture(&base, "w").capture,
        TickCapture::Replay { match_mode: MatchMode::Prompt, .. }
    ));

    let slot = RawCaptureEnv { match_mode: Some("slot".into()), ..base.clone() };
    let d = decide_capture(&slot, "w");
    assert!(matches!(d.capture, TickCapture::Replay { match_mode: MatchMode::Slot, .. }));
    // slot 命中 ≠「新 Prompt 下模型也会这么答」，这句话必须随判定一起走。
    assert!(d.notes.iter().any(|n| n.contains("slot")), "{:?}", d.notes);

    let junk = RawCaptureEnv { match_mode: Some("fuzzy".into()), ..base };
    let d = decide_capture(&junk, "w");
    assert!(matches!(d.capture, TickCapture::Replay { match_mode: MatchMode::Prompt, .. }));
    assert!(d.notes.iter().any(|n| n.contains("fuzzy")), "{:?}", d.notes);
}

#[test]
fn env_record_never_claims_stub_provenance() {
    // env 开关只可能开在真实服务上；桩来源必须由测试显式声明，不能靠 env 蒙对。
    let env = RawCaptureEnv { record: Some("rec-a".into()), ..Default::default() };
    match decide_capture(&env, "w").capture {
        TickCapture::Record { source, .. } => assert_eq!(source, SOURCE_REAL),
        other => panic!("应为 Record：{other:?}"),
    }
}

// ============================================================================
// §2 恒等性：默认关闭 = 一字节不变（结构性，不是「包装恰好透明」）
// ============================================================================

/// 🔴 **这条是「默认关闭 = 当前行为一字节不变」的结构性证据**。
///
/// 未配置时接线点返回的是**传进去的那一个 `Arc`**，引擎拿到的对象与接线前逐位相同 ——
/// 于是"行为不变"不需要论证"包装层恰好透明"，它是类型层面的恒等。
#[test]
fn off_returns_the_very_same_arc() {
    let world_id = "wld-record-identity";
    assert!(resolve_capture(world_id).is_off(), "未配置的世界必须是 Off");

    let base: Arc<dyn ModelClient> = Arc::new(NullModel);
    let fs = tmp_fs("muse-rec-identity");
    let (out, guard) = wrap_tick_model(world_id, 0, base.clone(), &fs, &[]).unwrap();

    assert!(Arc::ptr_eq(&base, &out), "Off 必须原样返回同一个 Arc，中间不得有任何一层包装");
    drop(guard);

    // 没有会话、没有目录、没有文件。
    assert!(recorded_call_count(world_id).is_none());
    assert!(replay_report(world_id).is_none());
    assert!(!fs.data_root().join(muse_engine::replay::DEFAULT_RECORDING_DIR).exists());
    assert!(flush_recording(world_id).unwrap().is_none(), "无会话时落盘必须是 no-op");
}

/// Off 的守卫 Drop 是彻底的 no-op：不建目录、不落文件、不留会话。
#[tokio::test]
async fn off_leaves_no_artifact_after_a_full_golden_run() {
    let world_id = "wld-record-off-noartifact";
    let state = test_state().await;
    seed_golden_world(&state, world_id, &GoldenParams::main()).await;
    let (statuses, snap) = run_main(&state, world_id).await;

    assert!(!statuses.is_empty());
    assert!(snap.len() > 2000, "快照过小，疑似没真跑回合：{} 字节", snap.len());
    assert!(recorded_call_count(world_id).is_none(), "关闭时不得留下录制会话");

    let world_data = PathBuf::from(&state.config.object_store_dir).join("world-data").join(world_id);
    assert!(world_data.exists(), "引擎数据目录应存在（回合真的跑过）");
    assert!(
        !world_data.join(muse_engine::replay::DEFAULT_RECORDING_DIR).exists(),
        "关闭时**一个字节都不该落**到 recordings/"
    );
}

// ============================================================================
// §3 端到端：黄金世界 关 / 录 / 放 三跑，结构化产物逐字节相等
// ============================================================================

/// 主用例。三个阶段用**同一个 world_id**（world_id 进 `instance_seed`，换 id 就是换副本）、
/// 各自一套全新内存库与全新引擎 FS：
///
/// 1. **关**：默认路径跑一遍 → `snap_off`
/// 2. **录**：同一条路径外面套录制器 → `snap_rec` + 落盘一份录制
/// 3. **放**：从盘上那份录制回放（注入的剧本模型**一次都不会被调用**）→ `snap_replay`
///
/// 断言 `snap_off == snap_rec == snap_replay`：
/// - 前一个等号 = **录制不改变管线产物**（比"包装应该是透明的"这句话强）
/// - 后一个等号 = **回放能原样重建整条回合**，且 `ReplayReport::is_exact()`（零未命中零未取用）
///
/// ⚠️ 这里的响应来自 `ScriptedModel`（人写的剧本），故本用例**不是**任何叙事质量结论。
#[tokio::test]
async fn golden_world_record_replay_round_trip_is_byte_identical() {
    const WORLD: &str = "wld-record-roundtrip";
    const REC_ID: &str = "golden-changan-main.scripted";
    let root = tmp_root("muse-rec-roundtrip");
    let shared_fs: Arc<dyn HostFs> = Arc::new(StdFs::new(root.clone()));

    // ---- 阶段 1：关（默认路径）----
    let state_off = test_state().await;
    seed_golden_world(&state_off, WORLD, &GoldenParams::main()).await;
    let (status_off, snap_off) = run_main(&state_off, WORLD).await;
    assert!(
        !status_off.iter().any(|s| matches!(s, TickStatus::Skipped("blocked"))),
        "主线不该阻断（阻断会污染进程级僵局账，进而改变后两阶段的 prompt）：{status_off:?}"
    );

    // ---- 阶段 2：录 ----
    let state_rec = test_state().await;
    seed_golden_world(&state_rec, WORLD, &GoldenParams::main()).await;
    set_world_capture(
        WORLD,
        TickCapture::Record {
            recording_id: REC_ID.into(),
            root: Some(root.clone()),
            // 🔴 来源如实标注：这一份是剧本桩录的，不是真实模型。
            source: SOURCE_SCRIPTED_STUB.into(),
        },
    );
    let (status_rec, snap_rec) = run_main(&state_rec, WORLD).await;
    let recorded = recorded_call_count(WORLD).expect("录制会话应存在");
    end_world_capture(WORLD);

    assert_eq!(status_off, status_rec, "开录制不得改变逐拍 TickStatus");
    assert_eq!(
        snap_off, snap_rec,
        "🔴 开录制改变了结构化产物 —— 录制器不是透明的，接线泄漏进了产物"
    );
    assert!(recorded >= 15, "三拍黄金主线至少 15 次模型调用（导演/4 决策/写作/审校 ×3），实录 {recorded}");

    // 落盘产物自检 + 溯源信息。
    let saved = load_recording(Some(&root), &shared_fs, REC_ID).unwrap();
    assert!(saved.validate().is_empty(), "录制自检必须干净：{:?}", saved.validate());
    assert_eq!(saved.calls.len(), recorded, "落盘条数必须等于实录条数");
    assert_eq!(
        saved.meta.labels.get(SOURCE_LABEL_KEY).map(String::as_str),
        Some(SOURCE_SCRIPTED_STUB),
        "🔴 来源标签必须随产物走：一份桩录制和一份真实录制长得一模一样，不钉进产物就分不清"
    );
    assert_eq!(saved.meta.labels.get("worldId").map(String::as_str), Some(WORLD));
    assert_eq!(saved.meta.recorded_at_ms, 0, "不录墙钟（录了就没法逐字节比对两份录制）");
    // 拍号对齐：三拍都有调用落在自己的拍上。
    for tick in 0..MAIN_TICKS {
        assert!(saved.calls.iter().any(|c| c.beat == tick), "拍 {tick} 没有任何调用被对齐");
    }
    // 落盘是幂等的（同一份录制序列化两次逐字节相等）——否则"每拍覆写"会让文件内容抖。
    assert_eq!(saved.to_json().unwrap(), saved.to_json().unwrap());

    // ---- 阶段 3：放 ----
    let state_rep = test_state().await;
    seed_golden_world(&state_rep, WORLD, &GoldenParams::main()).await;
    set_world_capture(
        WORLD,
        TickCapture::Replay {
            recording_id: REC_ID.into(),
            root: Some(root.clone()),
            match_mode: MatchMode::Prompt,
        },
    );
    // 注入的剧本模型在回放下**一次都不会被调用**（回放器结构上没有 inner 可回落）。
    crate::runtime::stall_tracker().clear(WORLD);
    let witness = main_scripted_model();
    let mut status_rep = Vec::new();
    for tick_no in 0..MAIN_TICKS {
        status_rep.push(drive_tick(&state_rep, &witness, WORLD, tick_no).await);
    }
    let snap_rep = golden_snapshot(&state_rep.db, WORLD).await;
    let report = replay_report(WORLD).expect("回放会话应存在");
    end_world_capture(WORLD);

    assert!(
        witness.captured().is_empty(),
        "🔴 回放期间注入的模型被调用了 {} 次 —— 说明发生了回落，这次「回放」其实打了真模型",
        witness.captured().len()
    );
    assert_eq!(status_off, status_rep, "回放的逐拍 TickStatus 必须与原跑一致");
    assert_eq!(
        snap_off, snap_rep,
        "🔴 回放没能重建原跑的结构化产物：要么录漏了调用，要么回放查表口径不对"
    );
    assert!(
        report.is_exact(),
        "回放应零未命中零未取用：missed={} unused={:?} warnings={:?}",
        report.missed,
        report.unused,
        report.warnings
    );
    assert_eq!(report.served, recorded, "取用条数应等于录制条数");
}

// ============================================================================
// §4 纪律：回放不回落 · 凭据不入录
// ============================================================================

/// 🔴 **回放未命中必须明确失败，绝不回落到真实模型。**
///
/// 造法：给一份**空录制**（合法格式、零条调用），任何调用都必然未命中。期望：
/// 1. 注入的剧本模型**一次都没被调用**（真回落的话它会被调用）；
/// 2. 未命中被记进 `ReplayReport.misses`；
/// 3. tick 走到失败终态，而不是"看起来跑通了"。
#[tokio::test]
async fn replay_miss_fails_the_tick_and_never_falls_back() {
    const WORLD: &str = "wld-record-miss";
    const REC_ID: &str = "empty-recording";
    let root = tmp_root("muse-rec-miss");
    let fs: Arc<dyn HostFs> = Arc::new(StdFs::new(root.clone()));

    let empty = Recording {
        meta: RecordingMeta {
            format_version: RECORDING_FORMAT_VERSION,
            recording_id: REC_ID.into(),
            engine_version: muse_engine::ENGINE_VERSION.into(),
            models: Vec::new(),
            prompt_versions: Vec::new(),
            labels: Default::default(),
            recorded_at_ms: 0,
            warnings: Vec::new(),
        },
        calls: Vec::new(),
    };
    empty.save(fs.as_ref(), std::path::Path::new(muse_engine::replay::DEFAULT_RECORDING_DIR)).unwrap();

    let state = test_state().await;
    seed_golden_world(&state, WORLD, &GoldenParams::main()).await;
    set_world_capture(
        WORLD,
        TickCapture::Replay { recording_id: REC_ID.into(), root: Some(root), match_mode: MatchMode::Prompt },
    );
    crate::runtime::stall_tracker().clear(WORLD);

    let witness = main_scripted_model();
    let status = drive_tick(&state, &witness, WORLD, 0).await;
    let report = replay_report(WORLD).expect("回放会话应存在");
    end_world_capture(WORLD);

    assert!(
        witness.captured().is_empty(),
        "🔴 未命中时打到了真实（此处为注入的剧本）模型 —— 静默回落会让一次「回放」偷偷变成一次真实调用"
    );
    assert!(report.missed > 0, "未命中必须被记账：{report:?}");
    assert_eq!(report.served, 0);
    assert_eq!(
        status,
        TickStatus::Failed,
        "回放放不出来时 tick 必须失败 —— 「看起来跑通了」比失败更坏"
    );
}

/// 回放配置指向一份不存在的录制 → **本拍直接失败**，不得悄悄改用真实模型。
#[tokio::test]
async fn replay_of_missing_recording_errors_instead_of_using_the_real_model() {
    let world_id = "wld-record-missing-file";
    set_world_capture(
        world_id,
        TickCapture::Replay {
            recording_id: "does-not-exist".into(),
            root: Some(tmp_root("muse-rec-missing")),
            match_mode: MatchMode::Prompt,
        },
    );
    let base: Arc<dyn ModelClient> = Arc::new(NullModel);
    let fs = tmp_fs("muse-rec-missing-world");
    let code = match wrap_tick_model(world_id, 0, base, &fs, &[]) {
        Ok(_) => panic!("🔴 录制文件不存在时接线仍然成功了 —— 那意味着这一拍会去打真实模型"),
        Err(e) => e.code(),
    };
    end_world_capture(world_id);
    assert_eq!(code, "internal", "回放接线失败必须是错误，不是降级");
}

/// 🔑 **凭据绝不进录制产物**。
///
/// 把世界路由换成一份带"各种藏 key 姿势"的凭据（api_key / userinfo / query），跑完录制后
/// 直接对**落盘文件的字节**做断言 —— 不是对结构体，是对真正会被拷走的那份东西。
#[tokio::test]
async fn recording_bytes_never_contain_credentials() {
    const WORLD: &str = "wld-record-redaction";
    const REC_ID: &str = "redaction-check";
    const API_KEY: &str = "sk-museai-unit-test-DEADBEEFDEADBEEF";
    const URL_SECRET: &str = "sk-in-query-should-not-appear";
    const USERINFO: &str = "leakuser:leakpass";
    let root = tmp_root("muse-rec-redaction");

    let state = test_state().await;
    seed_golden_world(&state, WORLD, &GoldenParams::main()).await;
    override_routes(
        &state,
        &format!("http://{USERINFO}@mock.invalid/v1?key={URL_SECRET}"),
        API_KEY,
        "mock-model",
        "OpenAI-compatible",
    )
    .await;
    set_world_capture(
        WORLD,
        TickCapture::Record {
            recording_id: REC_ID.into(),
            root: Some(root.clone()),
            source: SOURCE_SCRIPTED_STUB.into(),
        },
    );
    crate::runtime::stall_tracker().clear(WORLD);
    let scripted = main_scripted_model();
    drive_tick(&state, &scripted, WORLD, 0).await;
    end_world_capture(WORLD);

    let path = root.join(muse_engine::replay::DEFAULT_RECORDING_DIR).join(format!("{REC_ID}.json"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {}: {e}", path.display()));
    assert!(raw.len() > 500, "录制文件过小，疑似没录到东西");
    for secret in [API_KEY, URL_SECRET, USERINFO] {
        assert!(!raw.contains(secret), "🔑 录制产物里出现了凭据片段 {secret:?}：{}", path.display());
    }
    assert!(raw.contains("mock.invalid"), "host 应保留（那是「录的是哪个服务」，不敏感且对比时有用）");
}

// ============================================================================
// §5 录制入口（真实模型）—— 默认 `#[ignore]`，需用户显式提供自己的 API Key
// ============================================================================

/// **拿真实模型录一份黄金世界。**
///
/// ```bash
/// MUSE_RECORD_BASE_URL=https://api.example.com/v1 \
/// MUSE_RECORD_API_KEY=sk-你自己的key \
/// MUSE_RECORD_MODEL=your-model-id \
/// MUSE_RECORD_DIR=$PWD/muse-objects/recordings \
///   cargo test --manifest-path server/Cargo.toml \
///     -- --ignored --nocapture record_golden_world_with_real_model
/// ```
///
/// 可选：`MUSE_RECORD_INTERFACE`（`OpenAI-compatible`（缺省）/ `Anthropic-compatible`）、
/// `MUSE_RECORD_ID`（录制 id，缺省 `golden-changan-main.real`）。
///
/// 跑完会打印**绝对路径**与自检结果。产物默认落在 `muse-objects/`（已 gitignore）。
/// 之后换 Prompt / 换引擎版本时：
/// `MUSE_TICK_REPLAY=<id> MUSE_TICK_RECORD_DIR=<同一个目录>` 起 server 即可对着它回放；
/// 换模型再录一份，用 `muse_engine::replay::diff::diff_recordings` 对齐到
/// 「哪一拍 · 哪个角色 · 哪个环节」比差异。
///
/// 🔴 **本用例跑绿 ≠ 角色一致性已验证**：它只产出「一份真实模型的录制」。
/// 「差异多大算 OOC」这条质量口径**还不存在**，得有了它才谈得上结论。
#[tokio::test]
#[ignore = "需要用户自己的模型 API Key（本仓不持有任何凭据）；用 --ignored 显式跑"]
async fn record_golden_world_with_real_model() {
    const WORLD: &str = "wld-golden-changan-record";
    let Ok(base_url) = std::env::var("MUSE_RECORD_BASE_URL") else {
        panic!("缺 MUSE_RECORD_BASE_URL —— 本用例需要你自己的模型服务地址与 Key，仓库里没有也不该有");
    };
    let api_key = std::env::var("MUSE_RECORD_API_KEY").expect("缺 MUSE_RECORD_API_KEY");
    let model = std::env::var("MUSE_RECORD_MODEL").expect("缺 MUSE_RECORD_MODEL");
    let interface =
        std::env::var("MUSE_RECORD_INTERFACE").unwrap_or_else(|_| "OpenAI-compatible".to_string());
    let rec_id = std::env::var("MUSE_RECORD_ID").unwrap_or_else(|_| "golden-changan-main.real".to_string());
    let root = PathBuf::from(
        std::env::var("MUSE_RECORD_DIR")
            .unwrap_or_else(|_| tmp_root("muse-rec-real").to_string_lossy().into_owned()),
    );
    assert!(root.is_absolute(), "MUSE_RECORD_DIR 必须是绝对路径：{}", root.display());
    assert!(
        matches!(interface.as_str(), "OpenAI-compatible" | "Anthropic-compatible"),
        "MUSE_RECORD_INTERFACE 只认 OpenAI-compatible / Anthropic-compatible，收到 {interface:?}"
    );

    let state = test_state().await;
    seed_golden_world(&state, WORLD, &GoldenParams::main()).await;
    override_routes(&state, &base_url, &api_key, &model, &interface).await;
    set_world_capture(
        WORLD,
        TickCapture::Record {
            recording_id: rec_id.clone(),
            root: Some(root.clone()),
            // 🔴 真实模型 —— 这一份才是「换了模型角色还是不是它自己」的可用基线素材。
            source: SOURCE_REAL.into(),
        },
    );
    crate::runtime::stall_tracker().clear(WORLD);

    // 走**生产入口** `process_tick`：它内部造 `HttpModelClient`，不注入任何 mock。
    // 拍号对齐由接线点自动完成（`RecordingClient::set_beat`），无需外部驱动。
    let mut statuses = Vec::new();
    for tick_no in 0..MAIN_TICKS {
        let rev = load_world(&state.db, WORLD).await.unwrap().state_revision;
        insert_tick(&state.db, WORLD, tick_no, rev).await.unwrap();
        statuses.push(crate::runtime::process_tick(&state, WORLD, tick_no).await.unwrap());
    }
    let path = flush_recording(WORLD).unwrap().expect("应有录制会话");
    let snapshot = recording_snapshot(WORLD).expect("应有录制会话");
    end_world_capture(WORLD);

    let issues = snapshot.validate();
    println!("─────────────────────────────────────────────");
    println!("录制产物: {}", path.display());
    println!("逐拍状态: {statuses:?}");
    println!("调用条数: {}", snapshot.calls.len());
    println!("模型     : {:?}", snapshot.meta.models);
    println!("自检     : {}", if issues.is_empty() { "干净".into() } else { issues.join(" | ") });
    println!("⚠️ 这是一份录制，不是一个结论：「差异多大算 OOC」的质量口径尚未定义。");
    println!("─────────────────────────────────────────────");

    assert!(!snapshot.calls.is_empty(), "一次调用都没录到：检查模型服务是否可达 / 路由是否生效");
    assert!(issues.is_empty(), "录制自检有问题：{issues:?}");
    assert!(
        snapshot.meta.labels.get(SOURCE_LABEL_KEY).map(String::as_str) == Some(SOURCE_REAL),
        "真实录制必须标注 responseSource=real"
    );
    // 接口标注如实入录（换模型对比时靠它区分是不是同一类接口）。
    let want = if interface == "Anthropic-compatible" {
        ModelInterface::AnthropicCompatible
    } else {
        ModelInterface::OpenAiCompatible
    };
    assert!(snapshot.calls.iter().all(|c| c.interface == want));
}
