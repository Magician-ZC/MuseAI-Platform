//! 模型调用抽象：非流式单轮补全 + 严格 JSON 解析与重试。
//!
//! 规格 §8.2：严格 JSON、解析失败重试一次、抽取/决策类 temperature=0、
//! 模型输出不可信（schema 校验只是第一层，业务校验由各管线负责）。

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::host::{CancelFlag, EngineEvent, HostEvents, ModelCallLog};

/// json_call 默认最大尝试次数（含首发）。推理模型（DeepSeek-R1 等）偶发把 max_tokens 全部用于
/// reasoning 段后返回空 content，以及偶发脏 JSON，都是 temperature=0 下重试常能恢复的瞬态；
/// 故默认给足次数兜底。可由 `ModelCallSpec.max_retries` 覆盖。
pub const DEFAULT_MAX_RETRIES: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelInterface {
    #[serde(rename = "OpenAI-compatible")]
    OpenAiCompatible,
    #[serde(rename = "Anthropic-compatible")]
    AnthropicCompatible,
}

/// 一次调用所需的完整模型凭据与参数（引擎无配置状态，全部由调用方传入——沿用现有后端无状态原则）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    pub interface: ModelInterface,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct ModelCallSpec {
    pub profile: ModelProfile,
    pub system: String,
    pub user: String,
    pub temperature: f32,
    pub max_output_tokens: u32,
    /// 观测用：环节名（characterScan / roleDecide / ...）
    pub agent: String,
    /// 观测用：prompt 版本标识
    pub prompt_version: String,
    /// 观测与归组用：运行 id（任务 id / 回合 id）
    pub run_id: String,
    /// 最大尝试次数（含首发）覆盖；`None` → `DEFAULT_MAX_RETRIES`。通用字段：空 content / 脏 JSON /
    /// 可重试模型错误在此次数内重试。向后兼容——既有构造点填 `None` 即用默认。
    pub max_retries: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ModelOutput {
    pub content: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError>;
}

/// 从模型返回文本中剥离 markdown 代码围栏并截取首个 JSON 值。
pub fn extract_json_payload(raw: &str) -> Result<serde_json::Value, EngineError> {
    let trimmed = raw.trim();
    let candidate = if trimmed.starts_with("```") {
        let inner = trimmed.trim_start_matches("```json").trim_start_matches("```");
        inner.trim_end_matches("```").trim()
    } else {
        trimmed
    };
    // 兜底：截取首个 '{' 或 '[' 到最后一个配对收尾符
    let candidate = match (candidate.find(['{', '[']), candidate.rfind(['}', ']'])) {
        (Some(start), Some(end)) if end >= start => &candidate[start..=end],
        _ => candidate,
    };
    serde_json::from_str(candidate).map_err(|e| EngineError::ModelOutput(format!("JSON 解析失败: {e}")))
}

/// 严格 JSON 调用：补全 → 解析 → 失败重试（重试不改变输入，temperature=0 下重试确定性一致）。
/// 每次调用通过 `events` 发射 ModelCall 观测日志。尝试次数由 `spec.max_retries` 决定（默认
/// `DEFAULT_MAX_RETRIES`）。三类失败区分对待：
/// - **空 content**（推理模型耗尽 max_tokens 的瞬态成功响应）：`error="empty_content"`，重试；
/// - **非空脏 JSON**（偶发格式抖动）：`error="model_output"`，重试；
/// - **真实模型错误**：可重试（5xx/429）继续重试，非可重试（4xx，如鉴权失败）早退不浪费次数；
///   `Cancelled` 立即透传。
pub async fn json_call<T: DeserializeOwned>(
    client: &dyn ModelClient,
    events: &dyn HostEvents,
    spec: &ModelCallSpec,
    cancel: &CancelFlag,
) -> Result<T, EngineError> {
    let mut last_err: Option<EngineError> = None;
    // 至少一次尝试（防 Some(0) 退化为不调用）。
    let max_attempts = spec.max_retries.unwrap_or(DEFAULT_MAX_RETRIES).max(1);
    for attempt in 0..max_attempts {
        cancel.check()?;
        let started = std::time::Instant::now();
        let result = client.complete(spec, cancel).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(output) => {
                // 空 content：一次成功 HTTP 响应但 content 为空/全空白——推理模型把 max_tokens 全用于
                // reasoning 段的典型症状。明确归类为可重试瞬态，用专门的 "empty_content" 码发日志，
                // 与真实脏 JSON（model_output）区分，供观测/告警面板判「瞬态自愈」vs「真实故障」。
                if output.content.trim().is_empty() {
                    events.emit(EngineEvent::ModelCall(ModelCallLog {
                        run_id: spec.run_id.clone(),
                        agent: spec.agent.clone(),
                        prompt_version: spec.prompt_version.clone(),
                        model_id: spec.profile.model.clone(),
                        input_tokens: output.input_tokens,
                        // 通常 == max_tokens，佐证被 reasoning 段吃光。
                        output_tokens: output.output_tokens,
                        latency_ms,
                        retries: attempt,
                        error: Some("empty_content".to_string()),
                    }));
                    last_err = Some(EngineError::ModelOutput(
                        "模型返回空 content（疑似推理耗尽 max_tokens）".into(),
                    ));
                    continue;
                }
                let parsed = extract_json_payload(&output.content)
                    .and_then(|v| serde_json::from_value::<T>(v).map_err(EngineError::serde));
                events.emit(EngineEvent::ModelCall(ModelCallLog {
                    run_id: spec.run_id.clone(),
                    agent: spec.agent.clone(),
                    prompt_version: spec.prompt_version.clone(),
                    model_id: spec.profile.model.clone(),
                    input_tokens: output.input_tokens,
                    output_tokens: output.output_tokens,
                    latency_ms,
                    retries: attempt,
                    error: parsed.as_ref().err().map(|e| e.code().to_string()),
                }));
                match parsed {
                    Ok(value) => return Ok(value),
                    Err(e) => last_err = Some(e),
                }
            }
            Err(e) => {
                events.emit(EngineEvent::ModelCall(ModelCallLog {
                    run_id: spec.run_id.clone(),
                    agent: spec.agent.clone(),
                    prompt_version: spec.prompt_version.clone(),
                    model_id: spec.profile.model.clone(),
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms,
                    retries: attempt,
                    error: Some(e.code().to_string()),
                }));
                if matches!(e, EngineError::Cancelled) {
                    return Err(e);
                }
                // 非可重试的真实模型错误（4xx，如鉴权失败）早退，不浪费剩余次数；
                // 空 content / 脏 JSON（ModelOutput）与可重试模型错误（5xx/429）恒继续重试。
                if !e.retryable() && !matches!(e, EngineError::ModelOutput(_)) {
                    last_err = Some(e);
                    break;
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| EngineError::ModelOutput("未知模型输出错误".into())))
}

/// 基于 reqwest 的 OpenAI / Anthropic 兼容非流式实现。
#[cfg(feature = "http")]
pub struct HttpModelClient {
    http: reqwest::Client,
}

#[cfg(feature = "http")]
impl HttpModelClient {
    pub fn new() -> Result<Self, EngineError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| EngineError::Model { message: e.to_string(), retryable: false })?;
        Ok(Self { http })
    }

    fn openai_endpoint(base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    fn anthropic_endpoint(base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        if base.ends_with("/messages") {
            base.to_string()
        } else {
            format!("{base}/v1/messages")
        }
    }
}

#[cfg(feature = "http")]
#[async_trait]
impl ModelClient for HttpModelClient {
    async fn complete(&self, spec: &ModelCallSpec, cancel: &CancelFlag) -> Result<ModelOutput, EngineError> {
        cancel.check()?;
        let request = match spec.profile.interface {
            ModelInterface::OpenAiCompatible => self
                .http
                .post(Self::openai_endpoint(&spec.profile.base_url))
                .bearer_auth(&spec.profile.api_key)
                .json(&serde_json::json!({
                    "model": spec.profile.model,
                    "messages": [
                        {"role": "system", "content": spec.system},
                        {"role": "user", "content": spec.user}
                    ],
                    "temperature": spec.temperature,
                    "max_tokens": spec.max_output_tokens,
                    "stream": false,
                })),
            ModelInterface::AnthropicCompatible => self
                .http
                .post(Self::anthropic_endpoint(&spec.profile.base_url))
                .header("x-api-key", &spec.profile.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&serde_json::json!({
                    "model": spec.profile.model,
                    "system": spec.system,
                    "messages": [{"role": "user", "content": spec.user}],
                    "temperature": spec.temperature,
                    "max_tokens": spec.max_output_tokens,
                    "stream": false,
                })),
        };

        let send = request.send();
        // 协作式取消：轮询取消位，避免长连接占死任务
        let response = tokio::select! {
            r = send => r.map_err(|e| EngineError::Model { message: e.to_string(), retryable: true })?,
            _ = async {
                loop {
                    if cancel.is_cancelled() { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            } => return Err(EngineError::Cancelled),
        };

        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| EngineError::Model { message: e.to_string(), retryable: true })?;
        if !status.is_success() {
            return Err(EngineError::Model {
                message: format!("HTTP {status}: {body}"),
                retryable: status.is_server_error() || status.as_u16() == 429,
            });
        }

        match spec.profile.interface {
            ModelInterface::OpenAiCompatible => {
                let content = body["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                Ok(ModelOutput {
                    content,
                    input_tokens: body["usage"]["prompt_tokens"].as_u64().map(|v| v as u32),
                    output_tokens: body["usage"]["completion_tokens"].as_u64().map(|v| v as u32),
                })
            }
            ModelInterface::AnthropicCompatible => {
                let content = body["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                Ok(ModelOutput {
                    content,
                    input_tokens: body["usage"]["input_tokens"].as_u64().map(|v| v as u32),
                    output_tokens: body["usage"]["output_tokens"].as_u64().map(|v| v as u32),
                })
            }
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    /// 脚本化模型客户端：按顺序返回预置响应（测试用）。
    pub struct ScriptedModel {
        responses: Mutex<Vec<Result<String, EngineError>>>,
    }

    impl ScriptedModel {
        pub fn new(responses: Vec<Result<String, EngineError>>) -> Self {
            Self { responses: Mutex::new(responses) }
        }
    }

    #[async_trait]
    impl ModelClient for ScriptedModel {
        async fn complete(
            &self,
            _spec: &ModelCallSpec,
            cancel: &CancelFlag,
        ) -> Result<ModelOutput, EngineError> {
            cancel.check()?;
            let mut lock = self.responses.lock().unwrap();
            if lock.is_empty() {
                return Err(EngineError::Model { message: "脚本响应耗尽".into(), retryable: false });
            }
            lock.remove(0).map(|content| ModelOutput { content, input_tokens: Some(1), output_tokens: Some(1) })
        }
    }

    #[test]
    fn extract_json_handles_fenced_output() {
        let value = extract_json_payload("```json\n{\"a\":1}\n```").unwrap();
        assert_eq!(value["a"], 1);
    }

    #[test]
    fn extract_json_handles_prefixed_prose() {
        let value = extract_json_payload("好的，结果如下：{\"a\":[1,2]}").unwrap();
        assert_eq!(value["a"][1], 2);
    }

    fn test_spec() -> ModelCallSpec {
        ModelCallSpec {
            profile: ModelProfile {
                interface: ModelInterface::OpenAiCompatible,
                base_url: "http://x".into(),
                api_key: "k".into(),
                model: "m".into(),
            },
            system: "s".into(),
            user: "u".into(),
            temperature: 0.0,
            max_output_tokens: 64,
            agent: "test".into(),
            prompt_version: "v1".into(),
            run_id: "run".into(),
            max_retries: None,
        }
    }

    fn model_call_logs(events: &crate::host::testing::CollectEvents) -> Vec<ModelCallLog> {
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::ModelCall(l) => Some(l.clone()),
                _ => None,
            })
            .collect()
    }

    // 测试点 #1：空 content 重试后成功——前两次空、末次有效 JSON → 最终 Ok，日志前两次 empty_content、
    // 末次 error=None，retries 递增。
    #[tokio::test]
    async fn json_call_retries_empty_content_then_succeeds() {
        let model = ScriptedModel::new(vec![
            Ok(String::new()),
            Ok("   ".to_string()),
            Ok(r#"{"a":1}"#.to_string()),
        ]);
        let events = crate::host::testing::CollectEvents::default();
        let v: serde_json::Value =
            json_call(&model, &events, &test_spec(), &CancelFlag::new()).await.unwrap();
        assert_eq!(v["a"], 1);
        let logs = model_call_logs(&events);
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].error.as_deref(), Some("empty_content"));
        assert_eq!(logs[0].retries, 0);
        assert_eq!(logs[1].error.as_deref(), Some("empty_content"));
        assert_eq!(logs[1].retries, 1);
        assert_eq!(logs[2].error, None);
        assert_eq!(logs[2].retries, 2);
    }

    // 测试点 #2：空 content 与脏 JSON 分类可区分。
    #[tokio::test]
    async fn json_call_distinguishes_empty_from_dirty_json() {
        // 空 content → empty_content 语义。
        let empty = ScriptedModel::new(vec![Ok(String::new())]);
        let ev_empty = crate::host::testing::CollectEvents::default();
        let mut spec = test_spec();
        spec.max_retries = Some(1);
        let err_empty =
            json_call::<serde_json::Value>(&empty, &ev_empty, &spec, &CancelFlag::new()).await.unwrap_err();
        assert_eq!(err_empty.code(), "model_output");
        assert_eq!(model_call_logs(&ev_empty)[0].error.as_deref(), Some("empty_content"));

        // 非空脏 JSON → model_output（走 parse 失败路径），error 码不同于 empty_content。
        let dirty = ScriptedModel::new(vec![Ok("这不是 JSON".to_string())]);
        let ev_dirty = crate::host::testing::CollectEvents::default();
        let err_dirty =
            json_call::<serde_json::Value>(&dirty, &ev_dirty, &spec, &CancelFlag::new()).await.unwrap_err();
        assert_eq!(err_dirty.code(), "model_output");
        let dirty_code = model_call_logs(&ev_dirty)[0].error.clone();
        assert_eq!(dirty_code.as_deref(), Some("model_output"));
        assert_ne!(dirty_code.as_deref(), Some("empty_content"));
    }

    // 测试点 #9：非可重试错误早退——首次即返回，不耗满 DEFAULT_MAX_RETRIES。
    #[tokio::test]
    async fn json_call_non_retryable_error_breaks_early() {
        let model = ScriptedModel::new(vec![
            Err(EngineError::Model { message: "401 未授权".into(), retryable: false }),
            Ok(r#"{"a":1}"#.to_string()), // 不应被消费
        ]);
        let events = crate::host::testing::CollectEvents::default();
        let err = json_call::<serde_json::Value>(&model, &events, &test_spec(), &CancelFlag::new())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "model");
        assert_eq!(model_call_logs(&events).len(), 1, "非可重试错误应在首次尝试后早退");
    }

    // 可重试模型错误在次数内重试后成功（5xx/429 语义）。
    #[tokio::test]
    async fn json_call_retries_retryable_model_error_then_succeeds() {
        let model = ScriptedModel::new(vec![
            Err(EngineError::Model { message: "503".into(), retryable: true }),
            Ok(r#"{"a":2}"#.to_string()),
        ]);
        let events = crate::host::testing::CollectEvents::default();
        let v: serde_json::Value =
            json_call(&model, &events, &test_spec(), &CancelFlag::new()).await.unwrap();
        assert_eq!(v["a"], 2);
        assert_eq!(model_call_logs(&events).len(), 2);
    }
}
