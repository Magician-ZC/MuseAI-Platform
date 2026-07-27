//! **可配置的 HTTP 内容审核 provider**：把「接真实审核服务商」从写代码降级为填配置。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 它补的是哪个缺口
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `ModerationProvider` 此前唯一的实现是 [`super::DevModeration`]（一张小关键词表，其余直过）。
//! 于是 §15 五层漏斗的**第 3 层管线虽已接通，却拦不住任何东西**（见
//! `safety::semantic` 模块头「先说清楚它现在拦不住任何东西」）。本模块提供第二个实现：
//! endpoint / 认证 / 请求体 / 响应字段映射**全部走环境变量**，因而无需改一行代码即可适配
//! 阿里云内容安全、腾讯云 TMS、百度内容审核、或自建审核服务等任意 HTTP JSON 审核 API。
//!
//! 🔴 **写出 provider ≠ 内容安全已就绪。** 本模块交付的状态是 §0.3 七档里的 `Implemented`：
//! 没有任何真实服务商账号验证过它，因此**不得**表述为「已接入」「五层漏斗已完整」。
//! 真正接上的那一刻由数据自己说话（见下「is_dev_stub 会自动翻面」）。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 未配置 = 行为与今天逐字节一致
//! ════════════════════════════════════════════════════════════════════════════
//!
//! [`from_env`] 在 [`ENV_ENDPOINT`] 未设置时返回 `Ok(None)`，装配侧
//! （`app::AppState::from_env`）随即保留 `DevModeration`。dev 与 CI 都是零配置环境，
//! 因此**本模块在默认构建里一次都不会被实例化**（用例
//! `zero_config_falls_back_to_dev_moderation` 锁住）。§0.1「未验证功能默认关闭」。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 配置形状：请求侧「JSON 模板」，响应侧「字段路径 + 值映射」
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 两侧刻意不对称，因为两侧的难点不同：
//!
//! - **请求侧用整段 JSON 模板**（[`ENV_BODY`]）。各家的请求体形状差异极大——阿里云要把参数
//!   再 JSON 编码一层塞进 `serviceParameters` 字符串，腾讯云要 base64，百度走扁平字段。
//!   任何「固定形状 + 字段名可配」的方案都覆盖不全，而模板可以：运维**从厂商文档里把示例
//!   请求体整段抄过来**，再把待审文本那个值换成占位符即可。三个占位符覆盖了上述三种塞法：
//!
//!   | 占位符 | 展开为 | 典型用途 |
//!   |---|---|---|
//!   | `{{TEXT}}` | 原文（JSON 序列化时自动转义） | 绝大多数 API |
//!   | `{{TEXT_JSON}}` | JSON 转义后的原文，**不带外层引号** | 需要嵌套一层 JSON 字符串（阿里云 `serviceParameters`） |
//!   | `{{TEXT_BASE64}}` | UTF-8 字节的标准 base64 | 腾讯云 `Content` |
//!   | `{{API_KEY}}` | [`ENV_API_KEY`] 的值 | 头 / URL / 体任意处 |
//!
//! - **响应侧用字段路径 + 值映射**（[`ENV_VERDICT_PATH`] + 三张值表）。这里模板帮不上忙——
//!   要做的是「读出来」而不是「拼出去」。路径是点分段，支持数组下标与 `*` 通配
//!   （`data.results.*.suggestion`）；通配到多个标量时按**严重度取最严**
//!   （Rejected > Pending > Approved），这既是保守方向，也让结果与元素顺序无关
//!   （⇒ 不依赖任何 map/array 迭代序，确定性契约第 3 条）。
//!
//! 🔴 **落不到任何一张值表的标签 → 报错，不是「就当通过」，也不假装成 Pending**。
//! 未识别的标签是**配置故障**而非内容判定：伪造一个裁决会让映射打错字这件事表现为
//! 「人审队列莫名其妙被灌满」或更糟的「一切照过」，两种都掩盖了真正的原因。
//! 报错则如实进 `safety_recheck_runs.provider_errors`，且内容仍按 fail-closed 落到 `pending`
//! ——内容结局一样保守，运营拿到的信号却是可区分的。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 fail 方向：只负责「返回裁决或错误」，绝不自己吞掉错误
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 超时 / 重试 / 降级策略**归调用方**（`safety::semantic` 的「窗口期重试 fail-open →
//! 预算耗尽 fail-closed」）。本模块任何一条失败路径——连接失败、超时、非 2xx、响应不是 JSON、
//! 路径取不到标量、标签未映射——一律返回 `Err`，**从不返回 `Ok(Approved)`**。
//!
//! 理由与 `safety::semantic` 模块头同一句话：**若 provider 自己把错误吞成放行，
//! 打掉审核服务就成了绕过第 3 层的手段**，审核链可用性会变成内容安全的上限。
//! 由用例 `red_line_every_failure_mode_errors_never_approves` 逐个失败模式扫死。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 凭据不进日志、不进落库字段
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 强度对齐 `muse_engine::replay` 的 `recording_never_leaks_credentials`（那里对 api_key /
//! URL query secret / userinfo 三种藏法都做了脱敏）。本模块的做法：
//!
//! 1. **结构上没有落点**：配置里存的是**模板**（含 `{{API_KEY}}` 字面量）而不是展开后的串，
//!    凭据只存在 [`HttpModerationConfig::api_key`] 一个字段里，展开发生在请求构造的瞬间。
//!    于是 `Debug` 打印整个配置都不会带出凭据（`Debug` 仍手写覆盖，见下）。
//! 2. **`Debug` 手写**：api_key 恒为 `<redacted>`；endpoint 过 [`sanitize_url`]（剥 query 与
//!    userinfo，同 `replay::sanitize_base_url`）；请求头**只打名字不打值**——因为运维完全可能
//!    把密钥硬编码进头模板而不是用 `{{API_KEY}}`（这种写法在启动校验里也会被抓，见下）。
//! 3. **所有错误串过 [`HttpModerationConfig::redact`]**：把凭据原文与其百分号编码形式
//!    从任意文本里抹成 `<redacted>`。这条针对的是「网关把整条请求（含 Authorization 头）
//!    回显进错误体」这种真实情况——`replay` 的用例就是这么抓到的。
//! 4. **启动即要求凭据长度 ≥ 8**：`replay::redact` 对过短的串不脱敏（避免把正文打成马赛克），
//!    本模块把那条前提变成**启动校验**，于是「太短所以没被脱敏」这个缺口在这里不存在。
//! 5. **响应体片段截断到 [`ERR_BODY_SNIPPET_CHARS`] 字符**：错误里要带一点响应体才可诊断，
//!    但响应体可能回显送审文本（= 用户内容）。截断是这两者之间的取舍，如实登记为边界。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 配错怎么办：**启动即失败**（fail-closed），不运行时降级
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 取向抄 `app::cors_layer` 对 `MUSE_CORS_ORIGINS` 的处理——「宁可前端立刻连不上、一眼可见，
//! 也不静默放宽」。审核链上这个道理更硬：**一个配错的审核 provider 与没有审核的区别，
//! 在看板上是看不出来的**（`providerStub` 会显示 `false`，`honesty[]` 会说「数据源 = 真实
//! provider」），而那正是本模块最该避免的误读。所以本组变量的任何解析失败或自相矛盾一律
//! 让进程起不来，包括：
//!
//! | 配错形态 | 为什么必须启动就拦 |
//! |---|---|
//! | 配了 key / body / path，**唯独没配 endpoint** | 静默留在 Dev 桩上，而运维以为已经接好了——最危险的一种 |
//! | 模板引用 `{{API_KEY}}` 但没配凭据 | 请求会以字面量 `{{API_KEY}}` 发出去 |
//! | 配了凭据但没有任何模板引用它 | 请求会裸奔发出去（厂商回 401 → 全量 fail-closed） |
//! | 凭据 < 8 字符 / 含控制字符 | 脱敏失效；控制字符还能做请求头注入 |
//! | 凭据被硬编码进 endpoint/头/体（而非用占位符） | 脱敏与 `Debug` 保护都绕过去了 |
//! | 送审文本占位符**一个都没出现** | 🔴 每次调用送出去的都是空文本，厂商当然全回 pass——「一切照过」且毫无迹象 |
//! | `APPROVED_VALUES` 为空 | 什么都过不了，人审队列会被灌爆 |
//! | `PENDING_VALUES` 与 `REJECTED_VALUES` 都为空 | 🔴 一个**永远拦不下任何东西**的「真实 provider」，正是 `is_dev_stub()` 想防的那种假防线 |
//! | 同一个值出现在多张表里 | 裁决有歧义 |
//! | 数值型变量解析失败 | 与其静默用默认值，不如让人看见打错的那个字符 |
//!
//! 运行时才可能发现的（响应结构变了、标签没映射到、服务挂了）走 `Err` → 调用方的
//! 重试与 fail-closed，这部分不属于「降级」，属于**如实上报**。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! `is_dev_stub()` 会自动翻面
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 本实现显式覆写 [`ModerationProvider::is_dev_stub`] 为 `false`。那个 bool 不是给日志看的：
//! 它随 `safety_recheck_runs.provider_stub` 每一行、每一条 `risk_events.detail_json`、
//! 以及 `GET /admin/safety/recheck` 的 `providerStub` / `source` / `honesty[]` 一起走。
//! 配置生效的那一刻这些字段全部翻面，领域代码一行不动
//! （用例 `configured_provider_flips_the_stub_fact_end_to_end`）。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 超时
//! ════════════════════════════════════════════════════════════════════════════
//!
//! [`ENV_TIMEOUT_MS`]（默认 5000）是 **reqwest 客户端级**超时，即本模块自己的那道闸。
//! `safety::semantic` 另有一道 `MUSE_SAFETY_L3_TIMEOUT_MS` 包在 `check_text` 外面。
//! 两道都要有，且**内层应当 ≤ 外层**：内层超时能拿到「是哪个阶段慢」的错误信息并及时释放连接，
//! 外层是兜底（防止某个实现根本不认超时而把 worker 永久占住）。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 成本：接的是**合同单价**，不是响应回报的实测计费
//! ════════════════════════════════════════════════════════════════════════════
//!
//! `safety::semantic` 的运营面此前 `cost.ratioAvailable` 恒为 `false`，理由是
//! 「`check_text` 只回裁决、不回 token/费用」。这句话对**主流厂商依然成立**：
//! 阿里云 / 腾讯云 / 百度的文本审核响应里都**没有**任何计费字段，它们按调用次数离线结算。
//! 所以本模块**没有**去响应里抠一个并不存在的字段，而是把缺的那一半补成显式配置：
//! [`ENV_PRICE_CENTS_PER_1K_CALLS`]（合同单价，分 / 1000 次调用）。
//!
//! 配了它，[`ModerationProvider::call_price_cents_per_1k`] 就返回 `Some(..)`，运营面据此算出
//! VALIDATION §2 T5 门槛「审核成本 ≤ 生成成本 5%」的比值；不配则**保持 `false`**，
//! 并在 `why` 里说明缺的是哪一半。🔴 分母侧（`MUSE_TOKEN_CNY_CENTS_PER_1K`）同样**只认显式配置**
//! ——拿代码内的默认估算去算一个 T5 门槛，得到的是「估算的估算」，那正是「假的 5%」。

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;

use super::{ModerationProvider, ModerationVerdict};

#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 环境变量（本模块是这组变量的唯一权威登记处）
// ═══════════════════════════════════════════════════════════════════════════

/// 审核服务 URL。**空 = 整个 provider 不启用**（回落 `DevModeration`）。可含 `{{API_KEY}}`
/// 与送审文本占位符（放进 URL 的占位符一律被**百分号编码**——URL 里放未编码的正文
/// （含空格 / 中文 / `&`）会直接把请求打散）。
pub const ENV_ENDPOINT: &str = "MUSE_MODERATION_HTTP_ENDPOINT";
/// HTTP 方法：`POST`（默认）/ `PUT` / `GET`。
pub const ENV_METHOD: &str = "MUSE_MODERATION_HTTP_METHOD";
/// 凭据。**唯一的密钥字段**；在 URL / 头 / 体里用 `{{API_KEY}}` 引用。长度须 ≥ 8。
pub const ENV_API_KEY: &str = "MUSE_MODERATION_HTTP_API_KEY";
/// 附加请求头，`Name: Value` 每行一条（也可用 `|` 分隔）。
/// 未设置且配了凭据时默认 `Authorization: Bearer {{API_KEY}}`。
pub const ENV_HEADERS: &str = "MUSE_MODERATION_HTTP_HEADERS";
/// 请求体 JSON 模板。默认 `{"text":"{{TEXT}}"}`。GET 时必须留空。
pub const ENV_BODY: &str = "MUSE_MODERATION_HTTP_BODY";
/// 响应里裁决标签的点分路径，支持数组下标与 `*` 通配。**必填**。
pub const ENV_VERDICT_PATH: &str = "MUSE_MODERATION_HTTP_VERDICT_PATH";
/// 判为 Approved 的标签值（逗号分隔，大小写不敏感）。
pub const ENV_APPROVED_VALUES: &str = "MUSE_MODERATION_HTTP_APPROVED_VALUES";
/// 判为 Pending（进人审）的标签值。
pub const ENV_PENDING_VALUES: &str = "MUSE_MODERATION_HTTP_PENDING_VALUES";
/// 判为 Rejected（直拒）的标签值。
pub const ENV_REJECTED_VALUES: &str = "MUSE_MODERATION_HTTP_REJECTED_VALUES";
/// 单次 HTTP 调用超时（毫秒），默认 5000。
pub const ENV_TIMEOUT_MS: &str = "MUSE_MODERATION_HTTP_TIMEOUT_MS";
/// 客户端截断长度（字符）。**默认 0 = 不截断**——让厂商自己的长度上限去拒绝，
/// 那条拒绝是一次 `Err`（→ fail-closed），比悄悄送半截文本过审安全。
pub const ENV_MAX_CHARS: &str = "MUSE_MODERATION_HTTP_MAX_CHARS";
/// 图片机审的处置：`pending`（默认）/ `approved` / `error`。见 [`ImageFallback`]。
pub const ENV_IMAGE_FALLBACK: &str = "MUSE_MODERATION_HTTP_IMAGE_FALLBACK";
/// 合同单价（分 / 1000 次调用）。配了才算得出 T5 成本比值。
pub const ENV_PRICE_CENTS_PER_1K_CALLS: &str = "MUSE_MODERATION_HTTP_PRICE_CENTS_PER_1K_CALLS";

/// 本组全部变量。用于「配了一半」检测：ENDPOINT 空但其它任一非空 → 启动失败。
pub const ALL_ENVS: [&str; 13] = [
    ENV_ENDPOINT,
    ENV_METHOD,
    ENV_API_KEY,
    ENV_HEADERS,
    ENV_BODY,
    ENV_VERDICT_PATH,
    ENV_APPROVED_VALUES,
    ENV_PENDING_VALUES,
    ENV_REJECTED_VALUES,
    ENV_TIMEOUT_MS,
    ENV_MAX_CHARS,
    ENV_IMAGE_FALLBACK,
    ENV_PRICE_CENTS_PER_1K_CALLS,
];

const DEFAULT_BODY: &str = r#"{"text":"{{TEXT}}"}"#;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const TIMEOUT_MS_MIN: u64 = 1;
const TIMEOUT_MS_MAX: u64 = 120_000;
/// 凭据最短长度。低于它 [`HttpModerationConfig::redact`] 会失效（同 `replay::redact` 的下限）。
const MIN_API_KEY_LEN: usize = 8;
/// 错误信息里携带的响应体片段上限（字符）。
const ERR_BODY_SNIPPET_CHARS: usize = 200;

/// 凭据被抹掉后的占位（与 `muse_engine::replay::REDACTED` 同形，便于跨模块 grep）。
pub const REDACTED: &str = "<redacted>";

const PH_API_KEY: &str = "{{API_KEY}}";
const PH_TEXT: &str = "{{TEXT}}";
const PH_TEXT_JSON: &str = "{{TEXT_JSON}}";
const PH_TEXT_BASE64: &str = "{{TEXT_BASE64}}";
/// 送审文本的三种占位形式。**至少要出现一个**，否则送出去的是空文本（见模块头配错表）。
const TEXT_PLACEHOLDERS: [&str; 3] = [PH_TEXT_BASE64, PH_TEXT_JSON, PH_TEXT];

// ═══════════════════════════════════════════════════════════════════════════
// 图片机审的处置
// ═══════════════════════════════════════════════════════════════════════════

/// 本模块是**文本**审核 provider。图片审核是另一套厂商 API（多为 URL/base64 上传 + 异步任务），
/// 不在本次交付范围内。
///
/// 🔴 因此**不能**继承 `ModerationProvider::check_image` 的直过默认实现：那会造出一个
/// 自称 `is_dev_stub() == false`、却对图片一律放行的 provider——正是 `is_dev_stub` 的注释里
/// 说的「一条纯占位的链路被读成已生效的防线」。默认取 [`ImageFallback::Pending`]：
/// 文本已接、图片未接 ⇒ 图片进人审，如实、保守，且不阻断上传流程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFallback {
    /// 默认：一律进人审队列（`ModerationVerdict::Pending`）。
    Pending,
    /// 图审由平台外的流程覆盖时才用：直过。**开这个等于承认图片没有机审**。
    Approved,
    /// 最严：直接报错。调用方（`assets` 上传路径）会因此 500，即立绘上传不可用。
    Error,
}

impl ImageFallback {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "error" => Ok(Self::Error),
            other => Err(format!(
                "{ENV_IMAGE_FALLBACK} 只接受 pending / approved / error，实际「{other}」"
            )),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 响应字段路径
// ═══════════════════════════════════════════════════════════════════════════

/// 点分路径的一段。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// 对象键。
    Key(String),
    /// 数组下标；取不到数组时回落为同名对象键（有些 API 真的用 `"0"` 当键）。
    Index(usize, String),
    /// `*`：数组全部元素 / 对象全部值。
    Any,
}

fn parse_path(raw: &str) -> Result<Vec<Seg>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{ENV_VERDICT_PATH} 必填：不知道从响应哪个字段读裁决"));
    }
    let mut out = Vec::new();
    for part in raw.split('.') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("{ENV_VERDICT_PATH}「{raw}」里有空路径段"));
        }
        out.push(if part == "*" {
            Seg::Any
        } else if let Ok(i) = part.parse::<usize>() {
            Seg::Index(i, part.to_string())
        } else {
            Seg::Key(part.to_string())
        });
    }
    Ok(out)
}

/// 沿路径下降，返回全部命中的节点。
///
/// `Seg::Any` 遇对象时按**键排序**取值（`serde_json::Map` 默认已是 `BTreeMap`，这里再排一次
/// 是把「不依赖 map 迭代序」写成代码而不是注释）。下游按严重度取最严，本就与顺序无关。
fn resolve<'a>(root: &'a Value, path: &[Seg]) -> Vec<&'a Value> {
    let mut cur: Vec<&Value> = vec![root];
    for seg in path {
        let mut next: Vec<&Value> = Vec::new();
        for node in cur {
            match seg {
                Seg::Key(k) => {
                    if let Some(v) = node.get(k.as_str()) {
                        next.push(v);
                    }
                }
                Seg::Index(i, raw) => {
                    if let Some(v) = node.as_array().and_then(|a| a.get(*i)) {
                        next.push(v);
                    } else if let Some(v) = node.get(raw.as_str()) {
                        next.push(v);
                    }
                }
                Seg::Any => match node {
                    Value::Array(a) => next.extend(a.iter()),
                    Value::Object(o) => {
                        let mut keys: Vec<&String> = o.keys().collect();
                        keys.sort();
                        next.extend(keys.into_iter().filter_map(|k| o.get(k)));
                    }
                    _ => {}
                },
            }
        }
        cur = next;
        if cur.is_empty() {
            break;
        }
    }
    cur
}

/// 标量 → 可比对的文本。非标量（数组 / 对象 / null）不参与映射。
fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// 严重度序：Rejected > Pending > Approved。用于 `*` 通配到多个标量时取最严。
fn severity(v: ModerationVerdict) -> u8 {
    match v {
        ModerationVerdict::Approved => 0,
        ModerationVerdict::Pending => 1,
        ModerationVerdict::Rejected => 2,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 脱敏helpers
// ═══════════════════════════════════════════════════════════════════════════

/// URL 脱敏：剥掉 `?query`（有网关把 key 塞在 query 里）与 `user:pass@` userinfo。
/// 保留 scheme + host + path——那是「配的是哪个服务」这条信息，可诊断且不敏感。
///
/// 口径抄 `muse_engine::replay::sanitize_base_url`；此处**重新实现而不是复用**，
/// 是为了不让 server 的凭据脱敏依赖另一个 crate 的私有演进节奏（这两处必须各自恒真）。
/// 与那份实现的**一处收窄**：`@` 只在 authority 段（`://` 到第一个 `/` 之间）里找，
/// 于是 `https://h.example.com/u/@bob` 这种路径里的 `@` 不会被误当成 userinfo 打成马赛克。
pub fn sanitize_url(raw: &str) -> String {
    let no_query = raw.split(['?', '#']).next().unwrap_or("").to_string();
    let Some(sep) = no_query.find("://") else {
        return no_query;
    };
    let (scheme, rest) = no_query.split_at(sep + 3);
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].find('@') {
        Some(at) => format!("{scheme}{REDACTED}@{}", &rest[at + 1..]),
        None => no_query,
    }
}

/// 百分号编码（unreserved 集之外一律 `%XX`）。用于把占位符安全地放进 URL。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 把 `s` 序列化成 JSON 字符串后**去掉外层引号**（用于嵌套进一层 JSON 编码的字符串）。
fn json_escaped(s: &str) -> String {
    let quoted = Value::String(s.to_string()).to_string();
    quoted[1..quoted.len() - 1].to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// 配置
// ═══════════════════════════════════════════════════════════════════════════

/// 已校验的 HTTP 审核 provider 配置。
///
/// 🔴 **存的是模板不是展开结果**：`{{API_KEY}}` 保持字面量，凭据只在 [`Self::api_key`]
/// 一处。于是除该字段外，整个结构体打印出来都不含凭据（`Debug` 仍手写覆盖，见其实现）。
#[derive(Clone)]
pub struct HttpModerationConfig {
    endpoint_template: String,
    method: reqwest::Method,
    /// `(名, 值模板)`，顺序即发送顺序。
    header_templates: Vec<(String, String)>,
    /// `None` = 该方法不带体（GET）。
    body_template: Option<Value>,
    verdict_path: Vec<Seg>,
    approved: Vec<String>,
    pending: Vec<String>,
    rejected: Vec<String>,
    timeout: Duration,
    /// 0 = 不截断。
    max_chars: usize,
    image_fallback: ImageFallback,
    price_cents_per_1k_calls: Option<i64>,
    /// 🔴 唯一的凭据落点。空串 = 未配置认证。
    api_key: String,
}

impl fmt::Debug for HttpModerationConfig {
    /// 🔴 手写：凭据恒为 `<redacted>`，endpoint 过 [`sanitize_url`]，请求头**只出名字不出值**
    /// （运维可能把密钥硬编码进头模板——那种写法在启动校验里会被拒，但 `Debug` 不该指望校验）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpModerationConfig")
            .field("endpoint", &sanitize_url(&self.endpoint_template))
            .field("method", &self.method.as_str())
            .field(
                "headerNames",
                &self.header_templates.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            )
            .field("hasBody", &self.body_template.is_some())
            .field("verdictPath", &self.verdict_path)
            .field("approved", &self.approved)
            .field("pending", &self.pending)
            .field("rejected", &self.rejected)
            .field("timeoutMs", &(self.timeout.as_millis() as u64))
            .field("maxChars", &self.max_chars)
            .field("imageFallback", &self.image_fallback)
            .field("priceCentsPer1kCalls", &self.price_cents_per_1k_calls)
            .field("apiKey", &if self.api_key.is_empty() { "<unset>" } else { REDACTED })
            .finish()
    }
}

/// 逗号分隔的值表 → 归一化（trim + 小写）后的列表。
fn value_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `Name: Value` 逐行（或 `|` 分隔）→ 头模板表。
fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for line in raw.split(['\n', '|']) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("{ENV_HEADERS} 里「{line}」不是 `Name: Value` 形式"));
        };
        let (name, value) = (name.trim(), value.trim());
        reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("{ENV_HEADERS} 里「{name}」不是合法请求头名"))?;
        // 校验的是**模板**：占位符是可见 ASCII，展开后仍合法（凭据字符集也在启动时校验过）。
        reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| format!("{ENV_HEADERS} 里「{name}」的值含非法字符（控制字符？）"))?;
        out.push((name.to_string(), value.to_string()));
    }
    Ok(out)
}

fn parse_num<T: std::str::FromStr>(raw: &str, env: &str) -> Result<T, String> {
    raw.trim().parse::<T>().map_err(|_| format!("{env}「{raw}」不是合法数字"))
}

impl HttpModerationConfig {
    /// 从任意「变量名 → 值」查表构建。
    ///
    /// - `Ok(None)`：整组变量一个都没配 ⇒ 不启用，调用方保留 `DevModeration`。
    /// - `Ok(Some(cfg))`：配置完整且自洽。
    /// - `Err(msg)`：配错了 ⇒ **调用方应当让进程起不来**（理由见模块头「配错怎么办」）。
    ///
    /// 取查表闭包而不是直接读 `std::env`，是为了让用例能在**不碰进程级 env** 的前提下
    /// 逐条驱动校验规则（env 是进程级的，设了会与并发用例互踩——同 `safety::semantic`
    /// 的用例用 `runtime_flags` 而不是 env 开开关）。
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let raw = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());

        let Some(endpoint_template) = raw(ENV_ENDPOINT) else {
            // 🔴 配了一半：没有 endpoint 却配了别的 ⇒ 静默留在 Dev 桩上是最危险的失败模式。
            if let Some(k) = ALL_ENVS.iter().find(|k| **k != ENV_ENDPOINT && raw(k).is_some()) {
                return Err(format!(
                    "{k} 已配置但 {ENV_ENDPOINT} 为空——这会静默回落到 Dev 桩（DevModeration），\
                     而运营面会显示「未接入」之外的一切都正常。要么把 endpoint 配上，要么把这组变量全部清空。"
                ));
            }
            return Ok(None);
        };

        // ── endpoint ────────────────────────────────────────────────────────
        {
            // 用一个不含占位符的探针 URL 校验形状（占位符本身不是合法 URL 字符）。
            let probe = endpoint_template
                .replace(PH_API_KEY, "K")
                .replace(PH_TEXT_BASE64, "T")
                .replace(PH_TEXT_JSON, "T")
                .replace(PH_TEXT, "T");
            let url = reqwest::Url::parse(&probe)
                .map_err(|e| format!("{ENV_ENDPOINT} 不是合法 URL：{e}"))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(format!(
                    "{ENV_ENDPOINT} 的 scheme 必须是 http/https，实际「{}」",
                    url.scheme()
                ));
            }
        }

        // ── method ──────────────────────────────────────────────────────────
        let method_raw = raw(ENV_METHOD).unwrap_or_else(|| "POST".to_string()).to_ascii_uppercase();
        let method = match method_raw.as_str() {
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "GET" => reqwest::Method::GET,
            other => {
                return Err(format!("{ENV_METHOD} 只支持 POST / PUT / GET，实际「{other}」"))
            }
        };

        // ── 凭据 ────────────────────────────────────────────────────────────
        let api_key = raw(ENV_API_KEY).unwrap_or_default();
        if !api_key.is_empty() {
            if api_key.chars().count() < MIN_API_KEY_LEN {
                return Err(format!(
                    "{ENV_API_KEY} 短于 {MIN_API_KEY_LEN} 字符：过短的串不会被脱敏\
                     （否则会把正文误伤成马赛克），于是它会原样出现在日志里。"
                ));
            }
            if let Some(bad) = api_key.chars().find(|c| !c.is_ascii_graphic()) {
                return Err(format!(
                    "{ENV_API_KEY} 含非可见 ASCII 字符（U+{:04X}）：控制字符可用于请求头注入。",
                    bad as u32
                ));
            }
        }

        // ── 请求头 ──────────────────────────────────────────────────────────
        let header_templates = match raw(ENV_HEADERS) {
            Some(s) => parse_headers(&s)?,
            None if !api_key.is_empty() => {
                vec![("Authorization".to_string(), format!("Bearer {PH_API_KEY}"))]
            }
            None => Vec::new(),
        };

        // ── 请求体 ──────────────────────────────────────────────────────────
        let body_raw = raw(ENV_BODY);
        let body_template = match (&method, body_raw) {
            (&reqwest::Method::GET, Some(_)) => {
                return Err(format!("{ENV_METHOD}=GET 时 {ENV_BODY} 必须留空（GET 不带请求体）"))
            }
            (&reqwest::Method::GET, None) => None,
            (_, Some(s)) => Some(
                serde_json::from_str::<Value>(&s)
                    .map_err(|e| format!("{ENV_BODY} 不是合法 JSON：{e}"))?,
            ),
            (_, None) => Some(
                serde_json::from_str::<Value>(DEFAULT_BODY).expect("DEFAULT_BODY 是合法 JSON"),
            ),
        };

        // ── 占位符自洽性（模块头配错表的第 2、3、5、6 行） ──────────────────
        let all_templates = {
            let mut v = vec![endpoint_template.clone()];
            v.extend(header_templates.iter().map(|(k, val)| format!("{k}: {val}")));
            if let Some(b) = &body_template {
                v.push(b.to_string());
            }
            v.join("\n")
        };
        let references_key = all_templates.contains(PH_API_KEY);
        if references_key && api_key.is_empty() {
            return Err(format!(
                "模板里引用了 {PH_API_KEY} 但 {ENV_API_KEY} 为空——请求会带着字面量 `{PH_API_KEY}` 发出去。"
            ));
        }
        // 🔴 「硬编码」先于「没引用」判：硬编码的那份配置同时满足两个条件，而前者是更具体的
        // 诊断（运维要改的是写法，不是补一个引用）。顺序反了会给出误导性的错误信息。
        if !api_key.is_empty() && all_templates.contains(api_key.as_str()) {
            return Err(format!(
                "凭据被硬编码进了 endpoint / {ENV_HEADERS} / {ENV_BODY}——请改用 {PH_API_KEY} 占位符，\
                 否则 Debug 打印与脱敏保护都绕不过去。"
            ));
        }
        if !references_key && !api_key.is_empty() {
            return Err(format!(
                "配置了 {ENV_API_KEY} 但 endpoint / {ENV_HEADERS} / {ENV_BODY} 里没有任何 {PH_API_KEY} \
                 引用它——请求会裸奔发出去，厂商回 401 ⇒ 全量 fail-closed。"
            ));
        }
        if !TEXT_PLACEHOLDERS.iter().any(|p| all_templates.contains(p)) {
            return Err(format!(
                "请求模板里一个送审文本占位符都没有（{}）——每次调用送出去的都是空文本，\
                 厂商当然全回「通过」。这是「一切照过且毫无迹象」，比不接审核更危险。",
                TEXT_PLACEHOLDERS.join(" / ")
            ));
        }

        // ── 裁决路径与值映射 ────────────────────────────────────────────────
        let verdict_path = parse_path(&raw(ENV_VERDICT_PATH).unwrap_or_default())?;
        let approved = value_list(&raw(ENV_APPROVED_VALUES).unwrap_or_default());
        let pending = value_list(&raw(ENV_PENDING_VALUES).unwrap_or_default());
        let rejected = value_list(&raw(ENV_REJECTED_VALUES).unwrap_or_default());
        if approved.is_empty() {
            return Err(format!("{ENV_APPROVED_VALUES} 为空：那样没有任何内容能过审。"));
        }
        if pending.is_empty() && rejected.is_empty() {
            return Err(format!(
                "{ENV_PENDING_VALUES} 与 {ENV_REJECTED_VALUES} 同时为空：这会造出一个\
                 **永远拦不下任何东西**的「真实 provider」——而它的 is_dev_stub() 是 false，\
                 看板上会显示成已生效的防线。"
            ));
        }
        for (a, an, b, bn) in [
            (&approved, ENV_APPROVED_VALUES, &pending, ENV_PENDING_VALUES),
            (&approved, ENV_APPROVED_VALUES, &rejected, ENV_REJECTED_VALUES),
            (&pending, ENV_PENDING_VALUES, &rejected, ENV_REJECTED_VALUES),
        ] {
            if let Some(dup) = a.iter().find(|v| b.contains(v)) {
                return Err(format!("标签「{dup}」同时出现在 {an} 与 {bn} 里，裁决有歧义。"));
            }
        }

        // ── 数值型（解析失败一律启动失败，不静默回落默认值） ────────────────
        let timeout_ms = match raw(ENV_TIMEOUT_MS) {
            Some(s) => parse_num::<u64>(&s, ENV_TIMEOUT_MS)?,
            None => DEFAULT_TIMEOUT_MS,
        };
        if !(TIMEOUT_MS_MIN..=TIMEOUT_MS_MAX).contains(&timeout_ms) {
            return Err(format!(
                "{ENV_TIMEOUT_MS} 须在 {TIMEOUT_MS_MIN}..={TIMEOUT_MS_MAX} 毫秒之间，实际 {timeout_ms}"
            ));
        }
        let max_chars = match raw(ENV_MAX_CHARS) {
            Some(s) => parse_num::<usize>(&s, ENV_MAX_CHARS)?,
            None => 0,
        };
        let price_cents_per_1k_calls = match raw(ENV_PRICE_CENTS_PER_1K_CALLS) {
            Some(s) => {
                let v = parse_num::<i64>(&s, ENV_PRICE_CENTS_PER_1K_CALLS)?;
                if v <= 0 {
                    return Err(format!("{ENV_PRICE_CENTS_PER_1K_CALLS} 须为正整数，实际 {v}"));
                }
                Some(v)
            }
            None => None,
        };
        let image_fallback = ImageFallback::parse(&raw(ENV_IMAGE_FALLBACK).unwrap_or_default())?;

        Ok(Some(Self {
            endpoint_template,
            method,
            header_templates,
            body_template,
            verdict_path,
            approved,
            pending,
            rejected,
            timeout: Duration::from_millis(timeout_ms),
            max_chars,
            image_fallback,
            price_cents_per_1k_calls,
            api_key,
        }))
    }

    /// 读进程环境变量。见 [`Self::from_lookup`] 的三种返回。
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_lookup(&|k| std::env::var(k).ok())
    }

    /// 🔴 把凭据原文（及其百分号编码形式）从任意文本里抹掉。
    ///
    /// 针对的是「网关把整条请求（含 Authorization 头 / URL query）回显进错误体」这种真实情况。
    /// 与 `replay::redact` 的差别：那里对 < 8 字符的串放弃脱敏，而本模块把「≥ 8 字符」
    /// 变成了**启动校验**（见 [`MIN_API_KEY_LEN`]），于是这里没有放弃分支。
    pub fn redact(&self, text: &str) -> String {
        if self.api_key.is_empty() {
            return text.to_string();
        }
        let once = text.replace(self.api_key.as_str(), REDACTED);
        let encoded = percent_encode(&self.api_key);
        if encoded == self.api_key {
            once
        } else {
            once.replace(encoded.as_str(), REDACTED)
        }
    }

    /// 启动日志用的一行自述。**已脱敏**（endpoint 剥 query/userinfo，不出凭据、不出头值）。
    pub fn describe(&self) -> String {
        format!(
            "HTTP 内容审核 provider：{} {} · 裁决路径 {} · 超时 {}ms · 请求头 [{}] · 图片处置 {:?} · 单价 {}",
            self.method.as_str(),
            sanitize_url(&self.endpoint_template),
            self.verdict_path
                .iter()
                .map(|s| match s {
                    Seg::Key(k) => k.clone(),
                    Seg::Index(_, r) => r.clone(),
                    Seg::Any => "*".to_string(),
                })
                .collect::<Vec<_>>()
                .join("."),
            self.timeout.as_millis(),
            self.header_templates.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", "),
            self.image_fallback,
            match self.price_cents_per_1k_calls {
                Some(p) => format!("{p} 分/千次"),
                None => "未配置（T5 成本比值仍算不出）".to_string(),
            }
        )
    }

    /// 把裁决标签映射成 [`ModerationVerdict`]。未映射到任何一张表 → `Err`（见模块头）。
    fn map_label(&self, label: &str) -> Result<ModerationVerdict, String> {
        let k = label.trim().to_ascii_lowercase();
        if self.approved.iter().any(|v| *v == k) {
            Ok(ModerationVerdict::Approved)
        } else if self.rejected.iter().any(|v| *v == k) {
            Ok(ModerationVerdict::Rejected)
        } else if self.pending.iter().any(|v| *v == k) {
            Ok(ModerationVerdict::Pending)
        } else {
            Err(format!(
                "审核服务返回了未映射的裁决标签「{label}」——请把它补进 {ENV_APPROVED_VALUES} / \
                 {ENV_PENDING_VALUES} / {ENV_REJECTED_VALUES} 之一。\
                 🔴 本次调用按错误处理（绝不猜成通过）。"
            ))
        }
    }

    /// 从响应 JSON 里读出裁决。`*` 命中多个标量时**取最严**。
    fn verdict_from(&self, body: &Value) -> Result<ModerationVerdict, String> {
        let nodes = resolve(body, &self.verdict_path);
        let labels: Vec<String> = nodes.iter().filter_map(|v| scalar_text(v)).collect();
        if labels.is_empty() {
            return Err(format!(
                "按 {ENV_VERDICT_PATH} 在响应里取不到任何标量裁决——响应结构与配置对不上。"
            ));
        }
        let mut worst = ModerationVerdict::Approved;
        for l in &labels {
            let v = self.map_label(l)?;
            if severity(v) > severity(worst) {
                worst = v;
            }
        }
        Ok(worst)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 模板展开
// ═══════════════════════════════════════════════════════════════════════════

/// 一次调用的展开上下文。
struct Render<'a> {
    text: &'a str,
    api_key: &'a str,
}

impl Render<'_> {
    /// 展开进 **JSON 字符串值**：`{{TEXT}}` 放原文（序列化时由 serde 转义），
    /// `{{TEXT_JSON}}` 放 JSON 转义后的原文（供嵌套一层 JSON 字符串用），
    /// `{{TEXT_BASE64}}` 放 base64。三者互不前缀（都以 `}}` 收尾），故替换顺序不影响结果，
    /// 仍按长到短写，读代码的人不必自己论证这一点。
    fn plain(&self, t: &str) -> String {
        t.replace(PH_TEXT_BASE64, &base64::engine::general_purpose::STANDARD.encode(self.text))
            .replace(PH_TEXT_JSON, &json_escaped(self.text))
            .replace(PH_TEXT, self.text)
            .replace(PH_API_KEY, self.api_key)
    }

    /// 展开进 **URL**：一律百分号编码——URL 里放未编码的正文（含空格 / 中文 / `&`）会直接把
    /// 请求打散。
    fn url(&self, t: &str) -> String {
        t.replace(PH_TEXT_BASE64, &percent_encode(&base64::engine::general_purpose::STANDARD.encode(self.text)))
            .replace(PH_TEXT_JSON, &percent_encode(&json_escaped(self.text)))
            .replace(PH_TEXT, &percent_encode(self.text))
            .replace(PH_API_KEY, &percent_encode(self.api_key))
    }

    /// 递归展开 JSON 模板：**只展开字符串值，不动键名**（键名是协议字段，不该被数据改写）。
    fn json(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.plain(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.json(x)).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, x)| (k.clone(), self.json(x))).collect())
            }
            other => other.clone(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Provider
// ═══════════════════════════════════════════════════════════════════════════

/// 配置驱动的 HTTP 内容审核 provider。构造成功即代表配置自洽（见 [`HttpModerationConfig`]）。
#[derive(Debug)]
pub struct HttpModerationProvider {
    cfg: HttpModerationConfig,
    client: reqwest::Client,
}

impl HttpModerationProvider {
    pub fn new(cfg: HttpModerationConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| format!("构建 HTTP 客户端失败：{e}"))?;
        Ok(Self { cfg, client })
    }

    /// 生产装配入口。`Ok(None)` = 未配置 ⇒ 调用方保留 `DevModeration`；
    /// `Err` = 配错了 ⇒ 调用方应当让进程起不来。
    pub fn from_env() -> Result<Option<Self>, String> {
        match HttpModerationConfig::from_env()? {
            Some(cfg) => Ok(Some(Self::new(cfg)?)),
            None => Ok(None),
        }
    }

    pub fn config(&self) -> &HttpModerationConfig {
        &self.cfg
    }

    /// 送审文本：按 [`ENV_MAX_CHARS`] 截断（0 = 不截断）。
    fn prepare(&self, text: &str) -> String {
        if self.cfg.max_chars == 0 {
            return text.to_string();
        }
        let total = text.chars().count();
        if total <= self.cfg.max_chars {
            return text.to_string();
        }
        // ⚠️ 截断意味着尾部内容**未被审核**。默认不开启正是因为这个；开了就要知道代价，
        // 故留一条只带长度、不带内容的告警。
        tracing::warn!(
            total_chars = total,
            kept_chars = self.cfg.max_chars,
            "内容审核送审文本被 {ENV_MAX_CHARS} 截断——尾部未经审核"
        );
        text.chars().take(self.cfg.max_chars).collect()
    }

    /// 真正发一次请求。**任何失败都返回 `Err`**（见模块头 fail 方向）。
    async fn call(&self, text: &str) -> Result<ModerationVerdict, String> {
        let prepared = self.prepare(text);
        let r = Render { text: &prepared, api_key: &self.cfg.api_key };

        let url = r.url(&self.cfg.endpoint_template);
        let mut req = self.client.request(self.cfg.method.clone(), &url);
        let mut has_content_type = false;
        for (name, tmpl) in &self.cfg.header_templates {
            if name.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            req = req.header(name.as_str(), r.plain(tmpl));
        }
        if let Some(tmpl) = &self.cfg.body_template {
            if !has_content_type {
                req = req.header("Content-Type", "application/json");
            }
            req = req.body(r.json(tmpl).to_string());
        }

        let resp = req
            .send()
            .await
            // reqwest 的错误串里带 URL（key 可能在 query 里），必须过脱敏。
            .map_err(|e| self.cfg.redact(&format!("内容审核服务请求失败：{e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| self.cfg.redact(&format!("读取内容审核响应失败：{e}")))?;

        if !status.is_success() {
            return Err(self.cfg.redact(&format!(
                "内容审核服务返回 {status}：{}",
                snippet(&body)
            )));
        }
        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            self.cfg.redact(&format!("内容审核响应不是 JSON（{e}）：{}", snippet(&body)))
        })?;
        self.cfg.verdict_from(&parsed).map_err(|e| self.cfg.redact(&e))
    }
}

/// 错误里携带的响应体片段：截断到 [`ERR_BODY_SNIPPET_CHARS`] 字符。
/// 响应体可能回显送审文本（= 用户内容），可诊断性与「别把内容抄进日志」之间的取舍。
fn snippet(body: &str) -> String {
    let s: String = body.chars().take(ERR_BODY_SNIPPET_CHARS).collect();
    if body.chars().count() > ERR_BODY_SNIPPET_CHARS {
        format!("{s}…（已截断）")
    } else {
        s
    }
}

#[async_trait]
impl ModerationProvider for HttpModerationProvider {
    async fn check_text(&self, text: &str) -> Result<ModerationVerdict, String> {
        self.call(text).await
    }

    /// 🔴 **显式覆写为 `false`**：本实现接的是真实服务商。
    /// 这个 bool 随 `safety_recheck_runs.provider_stub`、`risk_events.detail_json.providerStub`、
    /// `GET /admin/safety/recheck` 的 `providerStub` / `source` / `honesty[]` 一起走——
    /// 配置生效的那一刻它们全部翻面，领域代码一行不动。
    fn is_dev_stub(&self) -> bool {
        false
    }

    /// 图片机审：本模块只接文本，故按 [`ImageFallback`] 处置（默认进人审）。
    /// 🔴 **绝不继承 trait 的直过默认实现**——那会造出一个自称非桩、却对图片一律放行的 provider。
    async fn check_image(&self, _bytes: &[u8]) -> Result<ModerationVerdict, String> {
        match self.cfg.image_fallback {
            ImageFallback::Pending => Ok(ModerationVerdict::Pending),
            ImageFallback::Approved => Ok(ModerationVerdict::Approved),
            ImageFallback::Error => Err(format!(
                "图片机审未接入（{ENV_IMAGE_FALLBACK}=error）：本 provider 只接文本审核 API。"
            )),
        }
    }

    fn call_price_cents_per_1k(&self) -> Option<i64> {
        self.cfg.price_cents_per_1k_calls
    }
}
