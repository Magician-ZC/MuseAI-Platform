//! 模型调用抽象：非流式单轮补全 + 严格 JSON 解析与重试。
//!
//! 规格 §8.2：严格 JSON、解析失败重试一次、抽取/决策类 temperature=0、
//! 模型输出不可信（schema 校验只是第一层，业务校验由各管线负责）。

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::host::{CancelFlag, EngineEvent, HostEvents, ModelCallLog};

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

/// 严格 JSON 调用：补全 → 解析 → 失败重试一次（重试不改变输入）。
/// 每次调用通过 `events` 发射 ModelCall 观测日志。
pub async fn json_call<T: DeserializeOwned>(
    client: &dyn ModelClient,
    events: &dyn HostEvents,
    spec: &ModelCallSpec,
    cancel: &CancelFlag,
) -> Result<T, EngineError> {
    let mut last_err: Option<EngineError> = None;
    for attempt in 0..2u32 {
        cancel.check()?;
        let started = std::time::Instant::now();
        let result = client.complete(spec, cancel).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(output) => {
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
}
