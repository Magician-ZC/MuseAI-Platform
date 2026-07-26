//! 外部服务 provider 层：短信 / 内容安全 / 支付 / TTS / 对象存储。
//! 全部 trait + Dev 实现（日志态/内存态/本地盘）；真实服务商接入 = 新增实现 + 配置切换，
//! 领域代码只依赖 trait（BUILD-STATUS 约定 8）。

use async_trait::async_trait;
use std::path::PathBuf;

// ---------- 短信 ----------

#[async_trait]
pub trait SmsProvider: Send + Sync {
    async fn send_code(&self, phone: &str, code: &str) -> Result<(), String>;
}

/// dev：验证码打印到日志（tracing info），不外发。
pub struct DevSms;

#[async_trait]
impl SmsProvider for DevSms {
    async fn send_code(&self, phone: &str, code: &str) -> Result<(), String> {
        tracing::info!(phone, code, "DevSms 验证码（dev 模式不外发）");
        Ok(())
    }
}

// ---------- 内容安全 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModerationVerdict {
    Approved,
    /// 需人审
    Pending,
    Rejected,
}

#[async_trait]
pub trait ModerationProvider: Send + Sync {
    /// 文本机审。**这就是总规格 §15 五层漏斗第 3 层（小模型语义分类）的接口位**。
    ///
    /// 两类调用方：
    /// - **静态内容**：`safety::moderate_and_queue`（角色卡/世界模板/装配钩子/入站托梦）+
    ///   `safety::queue_operator_recheck`（已发布内容的运营再审）；
    /// - **运行时产出**：`safety::semantic`（§15 第 3 层异步复核）——公开投影**全量**、
    ///   私有投影**抽样**（抽样率是运营配置，且确定性，禁止写死）。
    ///
    /// 🔴 **约束：运行时复核必须在 tick 事务之外异步执行**。本方法是网络调用，放进
    /// `runtime::commit_tick` 的事务会（a）单连接池下再借连接死锁 PoolTimedOut，
    /// （b）让 tick 事务的持有时长被外部 RTT 绑架。`safety::semantic` 因此从不开事务，
    /// `commit_tick` 只在 `tx.commit()` 之后入一次队。非 Approved 时它把
    /// `world_events.moderation` 从 `approved` **收紧**（不改写已下发内容的正文，仅停止外发），
    /// 配合 §15 第 4 层「直播场延迟 1-2 拍缓冲」留出拦截窗口。
    /// 第 2 层（本地敏感词库，≈0 成本）已在事务内同步生效，见 `safety::lexicon`。
    async fn check_text(&self, text: &str) -> Result<ModerationVerdict, String>;

    /// 这个实现是不是 **Dev 桩**（不做任何真实机审/语义分类）。
    ///
    /// 🔴 **默认 `true`，这个方向是刻意的**：把真实 provider 误标成桩，最坏结果是报告低估了防线强度；
    /// 把桩误标成真实 provider，结果是**一条纯占位的链路被读成已生效的内容安全防线**——
    /// 而内容安全是合规主体责任，后一种误读的代价远大于前者。
    /// 因此接真实服务商时**必须显式覆写为 `false`**，覆写不了就说明还没接上。
    /// （同 `slo::quality::QualitySource::Production` 的「构造不出来 = 没接上」。）
    ///
    /// 这个 bool 不是给日志看的：它随 `safety::semantic` 的每一条风控留痕、
    /// `safety_recheck_runs` 的每一行、运营面 `GET /admin/safety/recheck` 的每一次响应一起走。
    fn is_dev_stub(&self) -> bool {
        true
    }

    /// 图片机审（角色头像等二进制资产）。
    /// dev 默认实现直过（Approved 占位）；生产待接第三方图审（阿里云内容安全 / 网易易盾 图片检测），
    /// 命中涉政/色情/暴恐等应返回 Pending（进人审）或 Rejected（直拒）。
    /// 红线：未过审头像绝不外泄——裁决落 avatar_moderation，读取面（roster / CharacterView）双过滤。
    async fn check_image(&self, _bytes: &[u8]) -> Result<ModerationVerdict, String> {
        Ok(ModerationVerdict::Approved)
    }
}

/// dev：小型关键词表命中 → Pending（进人审队列），否则 Approved。
pub struct DevModeration {
    pub flag_keywords: Vec<String>,
}

impl Default for DevModeration {
    fn default() -> Self {
        Self { flag_keywords: vec!["测试敏感词".into()] }
    }
}

#[async_trait]
impl ModerationProvider for DevModeration {
    async fn check_text(&self, text: &str) -> Result<ModerationVerdict, String> {
        if self.flag_keywords.iter().any(|k| !k.is_empty() && text.contains(k.as_str())) {
            Ok(ModerationVerdict::Pending)
        } else {
            Ok(ModerationVerdict::Approved)
        }
    }
}

// ---------- 支付（P4b 条件性；feature billing 才被装配） ----------

#[async_trait]
pub trait PaymentProvider: Send + Sync {
    /// 创建支付单，返回外部单号；dev 实现立即回调成功
    async fn create_order(&self, order_id: &str, amount_cents: i64) -> Result<String, String>;
}

pub struct DevPayment;

#[async_trait]
impl PaymentProvider for DevPayment {
    async fn create_order(&self, order_id: &str, amount_cents: i64) -> Result<String, String> {
        tracing::info!(order_id, amount_cents, "DevPayment 模拟支付成功");
        Ok(format!("dev-pay-{order_id}"))
    }
}

// ---------- TTS（P6 高光切片） ----------

#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// 返回生成音频的对象存储 key；dev 实现写占位文件
    async fn synthesize(&self, text: &str, voice: &str) -> Result<String, String>;
}

pub struct DevTts {
    pub store: LocalObjectStore,
}

#[async_trait]
impl TtsProvider for DevTts {
    async fn synthesize(&self, text: &str, voice: &str) -> Result<String, String> {
        let key = format!("clips/dev-tts-{}.txt", uuid::Uuid::new_v4().simple());
        self.store
            .put(&key, format!("[dev-tts voice={voice}] {text}").as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(key)
    }
}

// ---------- 对象存储 ----------

#[derive(Clone)]
pub struct LocalObjectStore {
    pub root: PathBuf,
}

impl LocalObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        let path = self.root.join(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)
    }
    pub fn get(&self, key: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.root.join(key))
    }
    pub fn delete(&self, key: &str) -> std::io::Result<()> {
        let p = self.root.join(key);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}
