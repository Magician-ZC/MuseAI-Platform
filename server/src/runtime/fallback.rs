//! 模型**备用路由**：主路由传输层失败时改用备用 profile 再试一次。
//!
//! 落地的是 `docs/build/open-decisions.md` §4 的三条决定（形状 / 触发条件 / 成本口径）。
//! 那份文档把它列为「本清单里最接近可以直接做的一项」——因为三条都是纯工程与成本口径，
//! 不需要产品输入。本模块就是照那三条写的，**没有改口径**。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 为什么是一个 `ModelClient` 包装器，而不是改引擎
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `ModelCallSpec.profile` 是**调用参数**，不是客户端的构造参数。于是「换一个 profile 再试」
//! 完全可以在 `ModelClient` 这一层做完：包装器收到 spec，先用 `spec.profile` 打，
//! 失败了把 `profile` 换成备用的再打一次。**引擎一行都不用改**，也不知道有这回事。
//!
//! 这不只是省事。路由与回退是**运营配置**（哪个厂商、挂了顶谁），而引擎是宿主无关的叙事内核
//! （`crates/muse-engine` 连数据库都不认）。把回退策略放进引擎，等于让内核长出一块只有平台
//! 才用得上的运营逻辑，桌面轨（同一个 crate）会白白背上它。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 只在**传输层失败**时回退，绝不在内容错误时回退
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 引擎的错误类型正好把两者分开了，不需要猜：
//!
//! | 错误 | 含义 | 回退？ |
//! |---|---|---|
//! | `EngineError::Model { retryable: true }` | 连接失败 / 超时 / 5xx / 429 | ✅ |
//! | `EngineError::Model { retryable: false }` | 401、参数非法…… | ❌ 换个 profile 也一样错 |
//! | `EngineError::ModelOutput(..)` | 返回了，但 JSON 解析不出来 | ❌ **见下** |
//! | `EngineError::Cancelled` | 调用方取消 | ❌ |
//!
//! `ModelOutput` 那一行是本模块最重要的一条决定。一个**持续输出坏 JSON** 的主模型
//! （提示词写坏了、模型版本变了、上下文超长被截断——都很常见）会让每一次调用都触发一次回退，
//! 于是**每一拍的成本翻倍，而且没有任何报错**：拍照样成功、内容照样出得来，
//! 只是账单悄悄变成两倍。回退在这里不是容错，是**成本放大器**。
//!
//! ⚠️ 对照：`HttpModelClient` 自己**会**对 `ModelOutput` 做重试（同一个 profile 重发几次），
//! 那是对的——同一个模型重掷一次骰子，成本可控且往往能过。「换一家再试」则不同：
//! 它假设的是「这家挂了」，而坏 JSON 恰恰说明这家**没挂**。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 成本相加，且「走没走备用」必须在数据里查得到
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 回退意味着同一拍出现两次调用。成本口径取**相加**（真实开销），不是只记成功那次——
//! 后者会**系统性低报**，而低报的成本看板比没有成本看板更危险。
//!
//! 光相加还不够：账单变贵时没人分得清是「模型涨价了」还是「一直在回退」。故本模块把
//! 回退次数计进 [`FallbackMeter`]，由 `runtime` 落进 `world_ticks.fallback_used`
//! （migration `0051`）。运营面的「路由错误率」用**那一列**算，而不是拿 tick 失败率顶替:
//! 回退成功的拍**不算失败**，但它正是「路由在出问题」的证据——用失败率去看，恰好把它漏掉。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use muse_engine::error::EngineError;
use muse_engine::host::CancelFlag;
use muse_engine::model::{ModelCallSpec, ModelClient, ModelOutput, ModelProfile};

/// 本拍走了几次备用路由。`runtime` 在拍提交时读它、落 `world_ticks.fallback_used`。
///
/// 用计数而不是 bool：一拍里有多个环节（decide / arbiter / writer / critic / director），
/// 「五个环节全走了备用」与「只有一个环节抖了一下」是两种完全不同的健康状况，
/// 压成 bool 就分不出来了。
#[derive(Debug, Default)]
pub(crate) struct FallbackMeter(AtomicU64);

impl FallbackMeter {
    pub(crate) fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed) as i64
    }
    fn bump(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// 带备用路由的 `ModelClient` 包装器。
///
/// `fallback` 为 `None` 时**逐字节退化为直接透传**（不多一次判断以外的任何行为差异）——
/// 那是绝大多数世界的情形：`routes_json` 里没声明 `fallback` 就没有备用路由，
/// 与本模块接线前完全一致。
pub(crate) struct FallbackModelClient {
    inner: Arc<dyn ModelClient>,
    fallback: Option<ModelProfile>,
    meter: Arc<FallbackMeter>,
}

impl FallbackModelClient {
    pub(crate) fn new(
        inner: Arc<dyn ModelClient>,
        fallback: Option<ModelProfile>,
        meter: Arc<FallbackMeter>,
    ) -> Self {
        Self { inner, fallback, meter }
    }
}

/// 这个错误该不该换一家再试。
///
/// 抽成独立函数是为了让判据**可单测、可被红线用例引用**——它是本模块唯一一条
/// 「选错了要付真代价」的逻辑（见模块头「成本放大器」那段）。
fn should_fall_back(e: &EngineError) -> bool {
    matches!(e, EngineError::Model { retryable: true, .. })
}

#[async_trait]
impl ModelClient for FallbackModelClient {
    async fn complete(
        &self,
        spec: &ModelCallSpec,
        cancel: &CancelFlag,
    ) -> Result<ModelOutput, EngineError> {
        let first = self.inner.complete(spec, cancel).await;
        let Some(fb) = self.fallback.as_ref() else {
            // 没配备用路由 → 原样返回。与接线前逐字节一致。
            return first;
        };
        let Err(e) = first else {
            return first;
        };
        if !should_fall_back(&e) {
            // 🔴 内容错误（ModelOutput）与不可重试错误（401 等）**不换家**：
            // 前者说明这家没挂（成本放大器），后者换谁都一样错。
            return Err(e);
        }
        tracing::warn!(
            agent = %spec.agent,
            run_id = %spec.run_id,
            error = %e,
            "主模型路由传输层失败，改用备用路由重试一次（成本相加，计入 world_ticks.fallback_used）"
        );
        self.meter.bump();
        // 只换 profile，其余（system / user / 温度 / 上限 / 观测字段）逐字节沿用——
        // 换家不是换提示词，那会让「备用路由的产物」与主路由不可比。
        let mut retry = spec.clone();
        retry.profile = fb.clone();
        self.inner.complete(&retry, cancel).await.map_err(|e2| {
            // 🔴 两边都失败时**报备用那次的错**，但把主路由的错一并带上：
            // 只报后者会让排查从「主模型为什么挂」跳到「备用为什么挂」，而病因在前一个。
            EngineError::Model {
                message: format!("主路由失败（{e}）；备用路由也失败（{e2}）"),
                retryable: matches!(e2, EngineError::Model { retryable: true, .. }),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 按剧本依次返回结果，并记下每次收到的 profile 名（验「换的是哪一家」）。
    struct Scripted {
        script: Mutex<Vec<Result<ModelOutput, EngineError>>>,
        seen: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(script: Vec<Result<ModelOutput, EngineError>>) -> Arc<Self> {
            Arc::new(Self { script: Mutex::new(script), seen: Mutex::new(Vec::new()) })
        }
        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelClient for Scripted {
        async fn complete(
            &self,
            spec: &ModelCallSpec,
            _c: &CancelFlag,
        ) -> Result<ModelOutput, EngineError> {
            self.seen.lock().unwrap().push(spec.profile.model.clone());
            let mut s = self.script.lock().unwrap();
            if s.is_empty() {
                return Err(EngineError::Model { message: "剧本耗尽".into(), retryable: false });
            }
            s.remove(0)
        }
    }

    fn profile(model: &str) -> ModelProfile {
        ModelProfile {
            interface: muse_engine::model::ModelInterface::OpenAiCompatible,
            base_url: "http://localhost".into(),
            api_key: "k".into(),
            model: model.into(),
        }
    }

    fn spec() -> ModelCallSpec {
        ModelCallSpec {
            profile: profile("primary"),
            system: "s".into(),
            user: "u".into(),
            temperature: 0.7,
            max_output_tokens: 100,
            agent: "writer".into(),
            prompt_version: "p1".into(),
            run_id: "r1".into(),
            max_retries: None,
        }
    }

    fn ok() -> Result<ModelOutput, EngineError> {
        Ok(ModelOutput { content: "x".into(), input_tokens: Some(1), output_tokens: Some(1) })
    }

    async fn run(
        script: Vec<Result<ModelOutput, EngineError>>,
        fb: Option<ModelProfile>,
    ) -> (Result<ModelOutput, EngineError>, Vec<String>, i64) {
        let inner = Scripted::new(script);
        let meter = Arc::new(FallbackMeter::default());
        let c = FallbackModelClient::new(inner.clone(), fb, meter.clone());
        let r = c.complete(&spec(), &CancelFlag::new()).await;
        (r, inner.seen(), meter.get())
    }

    /// 🔴 **传输层失败才换家**（`retryable: true`）。
    #[tokio::test]
    async fn transport_failure_falls_back_to_the_backup_profile() {
        let (r, seen, n) = run(
            vec![Err(EngineError::Model { message: "503".into(), retryable: true }), ok()],
            Some(profile("backup")),
        )
        .await;
        assert!(r.is_ok());
        assert_eq!(seen, vec!["primary", "backup"], "第二次必须打**备用**那一家");
        assert_eq!(n, 1, "计数要落，否则账单变贵时分不清是涨价还是一直在回退");
    }

    /// 🔴 **内容错误绝不换家**——这是本模块最重要的一条决定。
    ///
    /// 一个持续输出坏 JSON 的主模型会让每次调用都触发回退，于是每一拍成本翻倍，
    /// 而且**没有任何报错**（拍照样成功、内容照样出得来），账单悄悄变两倍。
    /// 回退在这里不是容错，是成本放大器。
    #[tokio::test]
    async fn a_bad_json_response_never_triggers_a_fallback() {
        let (r, seen, n) =
            run(vec![Err(EngineError::ModelOutput("JSON 解析失败".into()))], Some(profile("backup")))
                .await;
        assert!(matches!(r, Err(EngineError::ModelOutput(_))), "原样把内容错误抛出去");
        assert_eq!(seen, vec!["primary"], "🔴 只打了主路由一次——换家等于给坏 JSON 装成本放大器");
        assert_eq!(n, 0);
    }

    /// 不可重试的失败（401 等）也不换家：换谁都一样错，白烧一次调用。
    #[tokio::test]
    async fn a_non_retryable_failure_never_triggers_a_fallback() {
        let (r, seen, n) = run(
            vec![Err(EngineError::Model { message: "401 未授权".into(), retryable: false })],
            Some(profile("backup")),
        )
        .await;
        assert!(r.is_err());
        assert_eq!(seen, vec!["primary"], "🔴 401 换一家还是 401，只是多烧一次");
        assert_eq!(n, 0);
    }

    /// 没配备用路由 → **逐字节退化为直接透传**（绝大多数世界的情形）。
    #[tokio::test]
    async fn without_a_configured_fallback_it_is_a_pass_through() {
        let (r, seen, n) =
            run(vec![Err(EngineError::Model { message: "503".into(), retryable: true })], None).await;
        assert!(r.is_err());
        assert_eq!(seen, vec!["primary"], "没配备用就只打一次，与接线前一致");
        assert_eq!(n, 0);
    }

    /// 两边都挂 → 报**备用那次**的错，但把主路由的病因一并带上。
    #[tokio::test]
    async fn when_both_fail_the_message_keeps_the_primary_cause() {
        let (r, _seen, n) = run(
            vec![
                Err(EngineError::Model { message: "主 503".into(), retryable: true }),
                Err(EngineError::Model { message: "备 502".into(), retryable: true }),
            ],
            Some(profile("backup")),
        )
        .await;
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("主 503"), "🔴 病因在主路由，只报备用的错会把排查带偏: {msg}");
        assert!(msg.contains("备 502"), "{msg}");
        assert_eq!(n, 1, "尝试过就要计数——两边都挂更是路由在出问题的证据");
    }

    /// 换家**只换 profile**，提示词与采样参数逐字节沿用——否则备用产物与主路由不可比。
    #[tokio::test]
    async fn falling_back_changes_only_the_profile() {
        struct Capture(Mutex<Vec<ModelCallSpec>>);
        #[async_trait]
        impl ModelClient for Capture {
            async fn complete(
                &self,
                spec: &ModelCallSpec,
                _c: &CancelFlag,
            ) -> Result<ModelOutput, EngineError> {
                self.0.lock().unwrap().push(spec.clone());
                if self.0.lock().unwrap().len() == 1 {
                    Err(EngineError::Model { message: "503".into(), retryable: true })
                } else {
                    Ok(ModelOutput { content: "x".into(), input_tokens: Some(1), output_tokens: Some(1) })
                }
            }
        }
        let cap = Arc::new(Capture(Mutex::new(Vec::new())));
        let c = FallbackModelClient::new(
            cap.clone(),
            Some(profile("backup")),
            Arc::new(FallbackMeter::default()),
        );
        c.complete(&spec(), &CancelFlag::new()).await.unwrap();
        let calls = cap.0.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].profile.model, "backup");
        for (a, b) in [
            (&calls[0].system, &calls[1].system),
            (&calls[0].user, &calls[1].user),
            (&calls[0].agent, &calls[1].agent),
            (&calls[0].prompt_version, &calls[1].prompt_version),
        ] {
            assert_eq!(a, b, "换家不是换提示词");
        }
        assert_eq!(calls[0].temperature, calls[1].temperature);
        assert_eq!(calls[0].max_output_tokens, calls[1].max_output_tokens);
    }
}
