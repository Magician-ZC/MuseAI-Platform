//! 录制-回放的测试。
//!
//! 覆盖五件事：**录制的规范序与脱敏**、**回放的逐次一致**、**未命中的明确失败**、
//! **差异的定位粒度**、**产物的存取与格式锁**。
//!
//! ⚠️ 这些测试证明的是「工具本身可用且确定」，**不是**「角色一致性已验证」——
//! 后者需要真实模型录制 + 评分口径，两样都不在本 crate 内（VALIDATION §0.3 状态语言）。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::diff::{diff_recordings, DiffOptions, SlotChange};
use super::*;
use crate::host::testing::{CollectEvents, MemFs};
use crate::host::CancelFlag;
use crate::model::{json_call, ModelCallSpec, ModelClient, ModelInterface, ModelOutput, ModelProfile};
use crate::EngineError;

// ============================================================================
// 测试工装：一个「真实模型」替身 + 一条迷你管线
// ============================================================================

/// 本测试里用的假凭据。**任何一份录制产物里都不允许出现它**。
const API_KEY: &str = "sk-live-000111222333444555666777";
const CHARS: [&str; 3] = ["shenyan", "peizhao", "cuie"];

/// 闭包驱动的模型替身（顶替「真实模型」的位置）。
struct FakeModel {
    f: Box<dyn Fn(&ModelCallSpec) -> Result<ModelOutput, EngineError> + Send + Sync>,
}

#[async_trait]
impl ModelClient for FakeModel {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        (self.f)(spec)
    }
}

fn out(content: impl Into<String>) -> Result<ModelOutput, EngineError> {
    Ok(ModelOutput { content: content.into(), input_tokens: Some(100), output_tokens: Some(20) })
}

/// 从 user prompt 里取拍号（测试管线把它写成 `【拍 N】`）。
fn beat_of(user: &str) -> i64 {
    user.split("【拍 ")
        .nth(1)
        .and_then(|t| t.split('】').next())
        .and_then(|t| t.parse().ok())
        .unwrap_or(UNLABELED_BEAT)
}

/// 基线「真实模型」：响应只由 (环节, 角色, 拍) 决定，确定且可复现。
fn base_model() -> Arc<dyn ModelClient> {
    variant_model(None)
}

/// 变体模型：`mutate = Some((拍, 角色))` 时，只把那一拍那个角色的决策答成另一个样子——
/// 用来验证差异比较能否精确指到「哪一拍 · 哪个角色 · 哪个环节」。
fn variant_model(mutate: Option<(i64, &'static str)>) -> Arc<dyn ModelClient> {
    Arc::new(FakeModel {
        f: Box::new(move |spec: &ModelCallSpec| {
            let beat = beat_of(&spec.user);
            let cid = character_of_decide_prompt(&spec.user);
            match spec.agent.as_str() {
                "director" => out(json!({ "situation": format!("拍 {beat}：灯烛初上") }).to_string()),
                "writer" => out(json!({ "prose": format!("拍 {beat}：杯盏交错") }).to_string()),
                "roleDecide" => {
                    let mutated = mutate.is_some_and(|(mb, mc)| mb == beat && mc == cid);
                    let (intent, target) = if mutated {
                        ("改口不认", "cuie")
                    } else {
                        ("按兵不动", "peizhao")
                    };
                    out(json!({
                        "intent": intent,
                        "action": format!("{cid} 在拍 {beat} 端起酒盏"),
                        "speak": { "willSpeak": !mutated, "purpose": "叙旧" },
                        "targets": [target],
                        "duration": 60,
                    })
                    .to_string())
                }
                other => panic!("测试管线未覆盖的环节：{other}"),
            }
        }),
    })
}

fn spec_with(model: &str, agent: &str, character: &str, beat: i64) -> ModelCallSpec {
    let user = if agent == "roleDecide" {
        format!("以下是【仅你（{character}）可见】的信息：【拍 {beat}】席上众人各怀心事。")
    } else {
        format!("【拍 {beat}】当前局面。")
    };
    ModelCallSpec {
        profile: ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "https://api.example.com/v1".into(),
            api_key: API_KEY.into(),
            model: model.into(),
        },
        system: format!("你是 {agent} 环节。"),
        user,
        temperature: 0.0,
        max_output_tokens: 512,
        agent: agent.into(),
        prompt_version: "prompts-v1".into(),
        run_id: "wld-replay-test".into(),
        max_retries: None,
    }
}

fn spec(agent: &str, character: &str, beat: i64) -> ModelCallSpec {
    spec_with("model-x", agent, character, beat)
}

/// 能对齐拍号的客户端（录制端与回放端各自实现 `set_beat`，此处统一给驱动器用）。
trait Beated {
    fn beat(&self, b: i64);
}
impl Beated for RecordingClient {
    fn beat(&self, b: i64) {
        self.set_beat(b);
    }
}
impl Beated for ReplayClient {
    fn beat(&self, b: i64) {
        self.set_beat(b);
    }
}

/// 迷你管线：每拍「导演 → 三个角色决策 → 写作」。
///
/// `reverse_chars` 模拟**并发到达序的抖动**（引擎的角色决策是分批并发的，完成顺序不可复现）——
/// 两次跑用相反的角色顺序，录制产物必须仍然逐字节相等。
async fn drive<C: ModelClient + Beated>(
    client: &C,
    beats: i64,
    reverse_chars: bool,
    model: &str,
) -> Vec<String> {
    let mut seen = Vec::new();
    let cancel = CancelFlag::new();
    for b in 0..beats {
        client.beat(b);
        let mut order: Vec<&str> = CHARS.to_vec();
        if reverse_chars {
            order.reverse();
        }
        for (agent, cid) in std::iter::once(("director", ""))
            .chain(order.into_iter().map(|c| ("roleDecide", c)))
            .chain(std::iter::once(("writer", "")))
        {
            let r = client.complete(&spec_with(model, agent, cid, b), &cancel).await;
            seen.push(match r {
                Ok(o) => o.content,
                Err(e) => format!("ERR:{}", e.code()),
            });
        }
    }
    seen
}

/// 录一份基线（3 拍 × 5 次调用 = 15 次）。
async fn record_baseline(id: &str, mutate: Option<(i64, &'static str)>, model: &str) -> Recording {
    let rec = RecordingClient::new(variant_model(mutate), id).with_label("world", "wld-replay-test");
    drive(&rec, 3, false, model).await;
    rec.finish()
}

// ============================================================================
// §1 录制
// ============================================================================

#[tokio::test]
async fn recording_is_written_in_canonical_slot_order() {
    let rec = record_baseline("base", None, "model-x").await;
    assert_eq!(rec.calls.len(), 15, "3 拍 × (导演 + 3 决策 + 写作)");

    // seq = 规范序位置；规范序 = (拍, 角色, 环节, 槽内序号) 全序。
    for (i, c) in rec.calls.iter().enumerate() {
        assert_eq!(c.seq as usize, i);
    }
    let keys: Vec<(i64, String, String, u32)> =
        rec.calls.iter().map(|c| (c.beat, c.character.clone(), c.agent.clone(), c.occurrence)).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "落盘必须是规范序");

    // 拍 0 的五条：无角色的 director/writer 排在有角色的决策之前（"" < 任何角色 id）。
    assert_eq!(
        rec.calls.iter().take(5).map(|c| format!("{}/{}", c.character, c.agent)).collect::<Vec<_>>(),
        vec!["/director", "/writer", "cuie/roleDecide", "peizhao/roleDecide", "shenyan/roleDecide"]
    );
    assert_eq!(rec.meta.models, vec!["model-x"]);
    assert_eq!(rec.meta.engine_version, crate::ENGINE_VERSION);
    assert_eq!(rec.meta.recorded_at_ms, 0, "默认不录墙钟（录了就不可逐字节比对）");
    assert!(rec.meta.warnings.is_empty());
    assert!(rec.validate().is_empty(), "自检应通过：{:?}", rec.validate());
}

/// 🔴 到达序抖动不得影响产物：正序跑与倒序跑的录制必须逐字节相等。
/// （引擎的角色决策分批并发，完成顺序取决于真实耗时——这是录制可复现的必要条件。）
#[tokio::test]
async fn recording_is_stable_under_arrival_order_jitter() {
    let a = RecordingClient::new(base_model(), "base");
    drive(&a, 3, false, "model-x").await;
    let b = RecordingClient::new(base_model(), "base");
    drive(&b, 3, true, "model-x").await;
    assert_eq!(a.finish().to_json().unwrap(), b.finish().to_json().unwrap());
}

/// 🔑 凭据绝不入录：api_key 既不在结构里，也不会经 base_url / 错误消息 / 提示词漏出去。
#[tokio::test]
async fn recording_never_leaks_credentials() {
    let leaky: Arc<dyn ModelClient> = Arc::new(FakeModel {
        f: Box::new(|_s| {
            // 有些网关会把整条请求（含 Authorization 头）回显进错误体。
            Err(EngineError::Model {
                message: format!("HTTP 401: {{\"sent\":\"Bearer {API_KEY}\"}}"),
                retryable: false,
            })
        }),
    });
    let rec = RecordingClient::new(leaky, "leaky");
    let mut s = spec("director", "", 0);
    s.profile.base_url = format!("https://user:{API_KEY}@gw.example.com/v1?key={API_KEY}");
    // 连提示词里都塞一份（极端情况：调用方把 key 拼进了 prompt）。
    s.system = format!("系统提示，误带凭据 {API_KEY}");
    let _ = rec.complete(&s, &CancelFlag::new()).await;

    let raw = rec.finish().to_json().unwrap();
    assert!(!raw.contains(API_KEY), "录制产物出现凭据原文：\n{raw}");
    assert!(raw.contains(REDACTED));
    assert!(!raw.contains("apiKey"), "RecordedCall 结构里根本没有凭据字段");
    assert_eq!(sanitize_base_url("https://u:p@h.example.com/v1?key=abc"), "https://<redacted>@h.example.com/v1");
    assert_eq!(sanitize_base_url("https://h.example.com/v1"), "https://h.example.com/v1");
}

/// decide 的 prompt 包裹一旦改变，角色维度会失效。处理方式不是静默退化，而是**留痕**：
/// 告警随录制 JSON 一起走（数会被复制进评审材料，注释不会）。
#[tokio::test]
async fn unparsable_decide_prompt_leaves_a_visible_warning() {
    let rec = RecordingClient::new(base_model(), "wrapper-changed");
    let mut s = spec("roleDecide", "shenyan", 0);
    s.user = "（新包裹）请你决策：【拍 0】".into();
    let _ = rec.complete(&s, &CancelFlag::new()).await;
    let r = rec.finish();
    assert_eq!(r.calls[0].character, "", "解析不到就是空，不猜");
    assert_eq!(r.meta.warnings.len(), 1);
    assert!(r.meta.warnings[0].contains("roleDecide"));
    assert!(!r.validate().is_empty(), "自检要把录制期告警一并抛出来");
}

// ============================================================================
// §2 回放
// ============================================================================

#[tokio::test]
async fn replay_serves_recorded_outputs_exactly() {
    let baseline = record_baseline("base", None, "model-x").await;
    let live = drive(&RecordingClient::new(base_model(), "x"), 3, false, "model-x").await;

    let replay = ReplayClient::new(baseline.clone());
    let replayed = drive(&replay, 3, false, "model-x").await;

    assert_eq!(replayed, live, "回放必须逐次给出与录制时相同的出参");
    let report = replay.report();
    assert_eq!(report.served, 15);
    assert_eq!(report.missed, 0);
    assert!(report.unused.is_empty());
    assert!(report.warnings.is_empty());
    assert!(report.is_exact());
}

/// 回放的自证：**录一份回放**，它与原录制的差异必须为空。
#[tokio::test]
async fn recording_a_replay_diffs_clean_against_the_original() {
    let baseline = record_baseline("base", None, "model-x").await;
    let replay = Arc::new(ReplayClient::new(baseline.clone()));
    let rec = RecordingClient::new(replay.clone(), "replayed");
    drive(&rec, 3, false, "model-x").await;

    let d = diff_recordings(&baseline, &rec.finish(), &DiffOptions::default());
    assert!(d.is_identical(), "回放产物与录制不一致：\n{}", d.render_text());
    assert!(d.slots.is_empty());
    assert_eq!(d.summary.identical, 15);
}

/// 🔴 未录制的调用**明确失败**，不静默回落。
#[tokio::test]
async fn unrecorded_call_fails_loudly_and_never_falls_back() {
    let baseline = record_baseline("base", None, "model-x").await;
    let replay = ReplayClient::new(baseline);
    replay.set_beat(9);
    // 拍 9 从未录过 ⇒ prompt 摘要查不到。
    let err = replay.complete(&spec("roleDecide", "shenyan", 9), &CancelFlag::new()).await.unwrap_err();

    assert_eq!(err.code(), "not_found");
    assert!(!err.retryable());
    let msg = err.to_string();
    assert!(msg.contains("回放未命中"), "{msg}");
    assert!(msg.contains("不会回落到真实模型"), "{msg}");

    let report = replay.report();
    assert_eq!(report.missed, 1);
    assert_eq!(report.misses[0].reason, MissReason::NoSuchCall);
    assert_eq!(report.misses[0].character, "shenyan");
    assert_eq!(report.misses[0].beat, 9);
    assert_eq!(report.served, 0);
    assert_eq!(report.unused.len(), 15, "一次没取用 ⇒ 全部登记为未取用");
}

/// 「录制里有，但被消费完了」与「录制里根本没有」是两种不同的故障，报告必须区分。
#[tokio::test]
async fn exhausted_is_distinguished_from_never_recorded() {
    let rec = RecordingClient::new(base_model(), "one-shot");
    let s = spec("director", "", 0);
    let _ = rec.complete(&s, &CancelFlag::new()).await;

    let replay = ReplayClient::new(rec.finish());
    assert!(replay.complete(&s, &CancelFlag::new()).await.is_ok());
    let err = replay.complete(&s, &CancelFlag::new()).await.unwrap_err();
    assert_eq!(err.code(), "not_found");
    let report = replay.report();
    assert_eq!(report.misses[0].reason, MissReason::Exhausted);
    assert!(report.unused.is_empty(), "唯一一条已被取用");
}

/// 未命中要让 `json_call` **早退**：不是 retryable、也不是 ModelOutput，
/// 否则一次「管线多调了一次」会被烧成 4 次重试再报一个误导性的 model_output。
#[tokio::test]
async fn replay_miss_breaks_json_call_early() {
    let empty = Recording {
        meta: RecordingMeta {
            format_version: RECORDING_FORMAT_VERSION,
            recording_id: "empty".into(),
            engine_version: crate::ENGINE_VERSION.into(),
            models: vec![],
            prompt_versions: vec![],
            labels: BTreeMap::new(),
            recorded_at_ms: 0,
            warnings: vec![],
        },
        calls: vec![],
    };
    let replay = ReplayClient::new(empty);
    let events = CollectEvents::default();
    let err = json_call::<Value>(&replay, &events, &spec("director", "", 0), &CancelFlag::new())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert_eq!(events.0.lock().unwrap().len(), 1, "非可重试错误应在首次尝试后早退");
    assert_eq!(replay.report().missed, 1);
}

/// 命中但**非查表字段**漂移（换了模型 / 改了温度）：不阻断回放，但必须进报告——
/// 「这份录制是拿 model-x 录的，你现在配的是 model-y」是使用者必须看见的事实。
#[tokio::test]
async fn drift_of_non_key_fields_is_reported_not_hidden() {
    let baseline = record_baseline("base", None, "model-x").await;
    let replay = ReplayClient::new(baseline);
    let mut s = spec_with("model-y", "director", "", 0);
    s.temperature = 0.8;
    assert!(replay.complete(&s, &CancelFlag::new()).await.is_ok(), "prompt 一致 ⇒ 照常回放");
    let w = replay.report().warnings;
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].field, "model");
    assert_eq!((w[0].recorded.as_str(), w[0].requested.as_str()), ("model-x", "model-y"));
    assert_eq!(w[1].field, "temperature");
    assert_eq!((w[1].recorded.as_str(), w[1].requested.as_str()), ("0", "800"));
}

/// Slot 模式：Prompt 改了还想回放同一批响应时用。命中会带 `promptDigest` 漂移告警——
/// 这种命中**不能**当作「新 Prompt 下模型也会这么答」的证据。
#[tokio::test]
async fn slot_mode_matches_by_slot_and_flags_prompt_drift() {
    let baseline = record_baseline("base", None, "model-x").await;
    let replay = ReplayClient::with_mode(baseline, MatchMode::Slot);
    replay.set_beat(1);
    let mut s = spec("roleDecide", "shenyan", 1);
    s.system = "（新版系统提示）".into();
    let o = replay.complete(&s, &CancelFlag::new()).await.unwrap();
    assert!(o.content.contains("按兵不动"), "按槽位命中，服务的是旧 prompt 下录到的响应");
    let w = replay.report().warnings;
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].field, "promptDigest");
    assert!(w[0].slot.contains("shenyan"));
}

// ============================================================================
// §3 差异比较
// ============================================================================

/// 差异必须落到「哪一拍 · 哪个角色 · 哪个环节 · 哪个字段」。
#[tokio::test]
async fn diff_pinpoints_beat_character_agent_and_field() {
    let baseline = record_baseline("base@model-x", None, "model-x").await;
    let candidate = record_baseline("cand@model-y", Some((1, "shenyan")), "model-y").await;

    let d = diff_recordings(&baseline, &candidate, &DiffOptions::default());
    assert!(!d.is_identical());
    assert_eq!(d.summary.response_changed, 1, "只有一处响应变化");
    assert_eq!(d.summary.identical, 14);
    assert_eq!(d.summary.only_in_baseline, 0);
    assert_eq!(d.summary.only_in_candidate, 0);
    assert_eq!(d.summary.prompt_changed, 0, "prompt 没变，变的是模型");

    // 槽位粒度
    assert_eq!(d.slots.len(), 1);
    let s = &d.slots[0];
    assert_eq!((s.beat, s.character.as_str(), s.agent.as_str()), (1, "shenyan", "roleDecide"));
    assert_eq!(s.change, SlotChange::ResponseChanged);
    assert_eq!(s.label, "拍 1 · 角色 shenyan · 环节 roleDecide");

    // 字段粒度（模型响应是严格 JSON ⇒ 直接给到 JSON 指针路径）
    let paths: Vec<&str> = s.fields.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["/intent", "/speak/willSpeak", "/targets/0"], "字段遍历序固定：对象按 key 排序");
    let intent = s.fields.iter().find(|f| f.path == "/intent").unwrap();
    assert_eq!(intent.before.as_deref(), Some("\"按兵不动\""));
    assert_eq!(intent.after.as_deref(), Some("\"改口不认\""));

    // 分布表：只有 shenyan 这一张卡变了，其余两张一次都没变。
    assert_eq!(d.summary.per_character["shenyan"], super::diff::SlotCounts { total: 3, changed: 1 });
    assert_eq!(d.summary.per_character["peizhao"], super::diff::SlotCounts { total: 3, changed: 0 });
    assert_eq!(d.summary.per_agent["roleDecide"], super::diff::SlotCounts { total: 9, changed: 1 });
    assert_eq!(d.summary.per_beat[&1], super::diff::SlotCounts { total: 5, changed: 1 });

    // 人读报告把同样的三件事说清楚
    let text = d.render_text();
    assert!(text.contains("拍 1 · 角色 shenyan · 环节 roleDecide"), "{text}");
    assert!(text.contains("/intent: \"按兵不动\" → \"改口不认\""), "{text}");
    assert!(text.contains("model-x") && text.contains("model-y"), "{text}");
}

/// 调用构成变化（多调 / 少调一次）必须显性区分，不能混进「响应变了」。
#[tokio::test]
async fn diff_reports_call_count_changes_on_both_sides() {
    let baseline = record_baseline("base", None, "model-x").await;

    // 候选少跑一拍 ⇒ 5 条只在基线里有。
    let short = RecordingClient::new(base_model(), "short");
    drive(&short, 2, false, "model-x").await;
    let d = diff_recordings(&baseline, &short.finish(), &DiffOptions::default());
    assert_eq!(d.summary.only_in_baseline, 5);
    assert_eq!(d.summary.only_in_candidate, 0);
    assert!(d.slots.iter().all(|s| s.beat != 2 || s.change == SlotChange::OnlyInBaseline));

    // 候选多跑一拍 ⇒ 5 条只在候选里有。
    let long = RecordingClient::new(base_model(), "long");
    drive(&long, 4, false, "model-x").await;
    let d2 = diff_recordings(&baseline, &long.finish(), &DiffOptions::default());
    assert_eq!(d2.summary.only_in_candidate, 5);
    assert_eq!(d2.summary.only_in_baseline, 0);
}

/// 成败翻转（录制时成功、这次报错）是独立分类：它和「答得不一样」根本不是一回事。
#[tokio::test]
async fn diff_flags_status_flip() {
    let baseline = record_baseline("base", None, "model-x").await;
    let failing: Arc<dyn ModelClient> = Arc::new(FakeModel {
        f: Box::new(|s| {
            if s.agent == "director" {
                Err(EngineError::Model { message: "HTTP 503".into(), retryable: true })
            } else {
                out(json!({"prose":"x"}).to_string())
            }
        }),
    });
    let rec = RecordingClient::new(failing, "flip");
    rec.set_beat(0); // 不对齐拍号，这次调用就会落进「未标注」槽，与基线对不上
    let _ = rec.complete(&spec("director", "", 0), &CancelFlag::new()).await;
    let cand = rec.finish();

    let d = diff_recordings(&baseline, &cand, &DiffOptions::default());
    let flip = d.slots.iter().find(|s| s.agent == "director" && s.beat == 0).unwrap();
    assert_eq!(flip.change, SlotChange::StatusChanged);
    assert_eq!(flip.status, Some(("ok".into(), "err".into())));
    assert_eq!(d.summary.status_changed, 1);
}

/// prompt 变了但响应逐字相同 —— 单独一类（改了模板却没改变输出，值得复查）。
#[tokio::test]
async fn diff_separates_prompt_only_change() {
    let a = RecordingClient::new(base_model(), "a");
    let _ = a.complete(&spec("director", "", 0), &CancelFlag::new()).await;

    let fixed: Arc<dyn ModelClient> =
        Arc::new(FakeModel { f: Box::new(|_| out(json!({"situation":"拍 0：灯烛初上"}).to_string())) });
    let b = RecordingClient::new(fixed, "b");
    let mut s = spec("director", "", 0);
    s.system = "（新版系统提示）".into();
    let _ = b.complete(&s, &CancelFlag::new()).await;

    let d = diff_recordings(&a.finish(), &b.finish(), &DiffOptions::default());
    assert_eq!(d.slots.len(), 1);
    assert_eq!(d.slots[0].change, SlotChange::PromptChanged);
    assert!(d.slots[0].prompt_changed);
    assert!(d.slots[0].prompt.is_some());
    assert!(d.is_identical(), "响应没变 ⇒ 不算内容差异");
}

/// 同一对录制比两次，输出逐字节相同（禁三样的直接体现）。
#[tokio::test]
async fn diff_is_deterministic() {
    let a = record_baseline("a", None, "model-x").await;
    let b = record_baseline("b", Some((2, "cuie")), "model-y").await;
    let opts = DiffOptions::default();
    assert_eq!(
        diff_recordings(&a, &b, &opts).to_json().unwrap(),
        diff_recordings(&a, &b, &opts).to_json().unwrap()
    );
    assert_eq!(diff_recordings(&a, &b, &opts).render_text(), diff_recordings(&a, &b, &opts).render_text());
}

/// 非 JSON 响应（写作环节的真实模型偶尔给纯文本）退回文本差异，且中文按**字符**切不炸。
#[test]
fn text_delta_is_char_safe_for_chinese() {
    let a = Recording {
        meta: meta("a"),
        calls: vec![call("writer", "", 0, "灯影摇动，杯底压着心事")],
    };
    let b = Recording {
        meta: meta("b"),
        calls: vec![call("writer", "", 0, "灯影摇动，杯底压着旧账")],
    };
    let d = diff_recordings(&a, &b, &DiffOptions::default());
    let t = d.slots[0].response.as_ref().unwrap();
    assert_eq!(t.first_diff_char, 9, "「心」是第 10 个字符（下标 9）——按字符切，不按字节");
    assert_eq!(t.baseline_chars, 11);
    assert!(t.baseline_excerpt.contains("心事"));
    assert!(t.candidate_excerpt.contains("旧账"));
    assert!(d.slots[0].fields.is_empty(), "非 JSON ⇒ 没有字段级差异，只给文本差异");
}

fn meta(id: &str) -> RecordingMeta {
    RecordingMeta {
        format_version: RECORDING_FORMAT_VERSION,
        recording_id: id.into(),
        engine_version: crate::ENGINE_VERSION.into(),
        models: vec!["model-x".into()],
        prompt_versions: vec!["prompts-v1".into()],
        labels: BTreeMap::new(),
        recorded_at_ms: 0,
        warnings: vec![],
    }
}

fn call(agent: &str, character: &str, beat: i64, content: &str) -> RecordedCall {
    let (system, user) = (format!("你是 {agent} 环节。"), format!("【拍 {beat}】当前局面。"));
    RecordedCall {
        seq: 0,
        beat,
        character: character.into(),
        agent: agent.into(),
        occurrence: 0,
        run_id: "wld-replay-test".into(),
        prompt_version: "prompts-v1".into(),
        model: "model-x".into(),
        interface: ModelInterface::OpenAiCompatible,
        base_url: "https://api.example.com/v1".into(),
        temperature_milli: 0,
        max_output_tokens: 512,
        prompt_digest: content_digest(&[agent, &system, &user]),
        system,
        user,
        outcome: RecordedOutcome::Ok {
            content: content.into(),
            input_tokens: Some(100),
            output_tokens: Some(20),
        },
    }
}

// ============================================================================
// §4 产物存取与格式锁
// ============================================================================

#[tokio::test]
async fn recording_round_trips_through_host_fs() {
    let fs = MemFs::default();
    let rec = record_baseline("golden-changan-main@model-x", None, "model-x").await;
    let rel = rec.save(&fs, Path::new(DEFAULT_RECORDING_DIR)).unwrap();
    assert_eq!(rel, Path::new("recordings/golden-changan-main@model-x.json"));

    let listed = Recording::list(&fs, Path::new(DEFAULT_RECORDING_DIR)).unwrap();
    assert_eq!(listed, vec![rel.clone()]);
    assert_eq!(Recording::load(&fs, &rel).unwrap(), rec);
}

#[test]
fn recording_id_cannot_escape_the_recordings_dir() {
    for bad in ["../evil", "a/b", ".hidden", "", "空格 id"] {
        assert!(validate_recording_id(bad).is_err(), "{bad:?} 不应通过");
    }
    for good in ["golden-changan-main@model-x", "sim_v2.1"] {
        assert!(validate_recording_id(good).is_ok(), "{good:?} 应通过");
    }
}

#[test]
fn stale_format_version_is_refused_not_half_parsed() {
    let mut r = Recording { meta: meta("old"), calls: vec![] };
    r.meta.format_version = RECORDING_FORMAT_VERSION + 1;
    let raw = r.to_json().unwrap();
    let err = Recording::from_json(&raw).unwrap_err();
    assert_eq!(err.code(), "validation");
    assert!(err.to_string().contains("格式版本不匹配"));
}

#[test]
fn validate_catches_hand_edited_recordings() {
    let mut r = Recording { meta: meta("t"), calls: vec![call("director", "", 0, "{}")] };
    r.calls[0].user = "被手改过的 prompt".into();
    let issues = r.validate();
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("promptDigest"));
}

/// **格式锁**：随代码入库的样例录制必须逐字节等于当前结构的规范序列化。
/// 改了录制结构而没更新它 ⇒ 这条红 ⇒ 逼人显式确认「旧录制作废」这件事。
///
/// 重新生成：`MUSE_UPDATE_REPLAY_FIXTURE=1 cargo test --manifest-path crates/muse-engine/Cargo.toml replay`
#[tokio::test]
async fn sample_fixture_is_the_canonical_serialization() {
    let rec = sample_recording().await;
    let json = rec.to_json().unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/replay/fixtures/sample-recording.json");
    if std::env::var("MUSE_UPDATE_REPLAY_FIXTURE").is_ok() {
        // 重新生成模式：写完就退出。`include_str!` 是编译期内联的，本进程里读到的仍是旧内容，
        // 此时比对必然假红 —— 校验交给下一次普通 `cargo test`（那才是真正的格式锁）。
        std::fs::write(&path, format!("{json}\n")).unwrap();
        eprintln!("已重新生成 {}，请再跑一次 cargo test 完成校验", path.display());
        return;
    }
    let fixture = include_str!("fixtures/sample-recording.json");
    assert_eq!(fixture.trim_end(), json, "样例录制与当前结构不符（见本测试上方注释）");

    // fixture 自身必须是可加载、可回放的（不是一坨只能看的 JSON）。
    let parsed = Recording::from_json(fixture).unwrap();
    assert!(parsed.validate().is_empty(), "{:?}", parsed.validate());
    let replay = ReplayClient::new(parsed);
    let o = replay.complete(&spec("director", "", 0), &CancelFlag::new()).await.unwrap();
    assert!(o.content.contains("灯烛初上"));
}

/// 样例录制：一拍 × (导演 + 一个角色决策)，够小到能人读、够全到覆盖两类槽位。
async fn sample_recording() -> Recording {
    let rec = RecordingClient::new(base_model(), "sample-changan@model-x")
        .with_label("__doc", "样例录制（格式说明用）：录制-回放 ModelClient 的产物长这样")
        .with_label("world", "wld-sample");
    rec.set_beat(0);
    let cancel = CancelFlag::new();
    let _ = rec.complete(&spec("director", "", 0), &cancel).await;
    let _ = rec.complete(&spec("roleDecide", "shenyan", 0), &cancel).await;
    rec.finish()
}
