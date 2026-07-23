//! 逐章角色发现（规格 §10.2 阶段 2）：每章 1 次严格 JSON 调用。文件所有权：agent-E1。

use crate::host::CancelFlag;
use crate::host::EngineHost;
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{ChapterDiscovery, ChapterEntry};
use super::CharacterPrompts;

/// surface 最大字符数。
const SURFACE_MAX_CHARS: usize = 40;
/// quote 预览最大字符数。
const QUOTE_MAX_CHARS: usize = 200;
/// 每角色证据条数上限。
const EVIDENCE_PER_MENTION_MAX: usize = 20;

/// 扫描单章：组装 user prompt（章节标题+正文）→ json_call<ChapterDiscovery> → 白名单校验。
/// 校验规则：surface 非空且 ≤ 40 字；quote ≤ 200 字且必须是章节文本的子串（防幻觉引文）；
/// evidence 每角色 ≤ 20 条；违规条目丢弃并计入告警，不整章失败。
pub async fn scan_chapter(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &CharacterPrompts,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    chapter: &ChapterEntry,
    chapter_body: &str,
    cancel: &CancelFlag,
) -> Result<ChapterDiscovery, EngineError> {
    let user = build_scan_prompt(chapter, chapter_body);
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.scan_system.clone(),
        user,
        temperature,
        max_output_tokens,
        agent: "characterScan".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };
    let raw: ChapterDiscovery = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(sanitize_discovery(raw, chapter.index, chapter_body))
}

fn build_scan_prompt(chapter: &ChapterEntry, body: &str) -> String {
    format!(
        "章节序号：{idx}\n章节标题：{title}\n\n【章节正文开始】\n{body}\n【章节正文结束】\n\n\
请仅基于以上正文，列出本章出现的每个角色及其行为/选择/情绪/关系/表达样本，严格输出 JSON：\n\
{{\"chapterIndex\":{idx},\"mentions\":[{{\"surface\":\"文中对该角色的称呼\",\"roleHint\":\"角色定位\",\
\"evidence\":[{{\"kind\":\"action|description|otherView|inference\",\"quote\":\"原文片段(≤200字,必须逐字取自正文)\",\
\"note\":\"简要说明\",\"confidence\":\"high|medium|low\"}}]}}]}}\n\
硬性要求：surface 非空且 ≤ 40 字；quote 必须是正文的连续子串，禁止改写或杜撰；正文未出现的内容一律不要输出。",
        idx = chapter.index,
        title = chapter.title,
        body = body,
    )
}

/// 白名单校验：越界条目静默丢弃，不使整章失败。chapter_index 一律以代码为准。
fn sanitize_discovery(mut raw: ChapterDiscovery, chapter_index: u32, body: &str) -> ChapterDiscovery {
    raw.chapter_index = chapter_index; // 不信任模型返回的序号
    raw.mentions.retain_mut(|m| {
        let surface = m.surface.trim().to_string();
        if surface.is_empty() || surface.chars().count() > SURFACE_MAX_CHARS {
            return false;
        }
        m.surface = surface;
        // 逐条证据校验：quote 必须逐字来自正文且不超长。
        m.evidence.retain(|e| {
            let q = e.quote.trim();
            !q.is_empty() && q.chars().count() <= QUOTE_MAX_CHARS && body.contains(q)
        });
        m.evidence.truncate(EVIDENCE_PER_MENTION_MAX);
        true
    });
    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::{EngineHost, NullEvents};
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use std::sync::Arc;

    fn host_with(model: ScriptedModel) -> EngineHost {
        EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(1_000)),
            events: Arc::new(CollectEvents::default()),
            model: Arc::new(model),
        }
    }

    fn profile() -> ModelProfile {
        ModelProfile {
            interface: ModelInterface::OpenAiCompatible,
            base_url: "http://x".into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    fn prompts() -> CharacterPrompts {
        CharacterPrompts {
            scan_system: "sys".into(),
            merge_system: "sys".into(),
            tiering_system: "sys".into(),
            synthesis_system: "sys".into(),
            prompt_version: "v1".into(),
        }
    }

    fn chapter() -> ChapterEntry {
        ChapterEntry {
            id: "ch-1".into(),
            index: 7,
            title: "第八章".into(),
            char_range: (0, 100),
            status: crate::character::types::ChapterStatus::Pending,
            attempt: 0,
            discovery_store_key: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn drops_hallucinated_quote_and_overrides_index() {
        let body = "林冲提枪上马，怒喝一声。";
        // 一条 quote 来自正文（保留），一条杜撰（丢弃）；chapterIndex 故意给错。
        let resp = r#"{"chapterIndex":999,"mentions":[
            {"surface":"林冲","roleHint":"主角","evidence":[
                {"kind":"action","quote":"林冲提枪上马","note":"","confidence":"high"},
                {"kind":"action","quote":"林冲空中飞行三百里","note":"","confidence":"low"}
            ]}
        ]}"#;
        let host = host_with(ScriptedModel::new(vec![Ok(resp.into())]));
        let d = scan_chapter(&host, &profile(), &prompts(), 0.0, 1024, "task-1", &chapter(), body, &CancelFlag::new())
            .await
            .unwrap();
        assert_eq!(d.chapter_index, 7); // 代码覆盖模型序号
        assert_eq!(d.mentions.len(), 1);
        assert_eq!(d.mentions[0].evidence.len(), 1); // 杜撰引文被丢弃
        assert_eq!(d.mentions[0].evidence[0].quote, "林冲提枪上马");
    }

    #[tokio::test]
    async fn drops_oversize_surface_and_caps_evidence() {
        let body = "话音".repeat(50);
        let quote = "话音话音"; // 正文子串
        let long_surface = "名".repeat(41); // 超过 40 字
        let mut evs = String::new();
        for _ in 0..30 {
            evs.push_str(&format!(r#"{{"kind":"description","quote":"{quote}","confidence":"medium"}},"#));
        }
        let evs = evs.trim_end_matches(',');
        let resp = format!(
            r#"{{"chapterIndex":0,"mentions":[
                {{"surface":"{long_surface}","evidence":[]}},
                {{"surface":"甲","evidence":[{evs}]}}
            ]}}"#
        );
        let host = host_with(ScriptedModel::new(vec![Ok(resp)]));
        let d = scan_chapter(&host, &profile(), &prompts(), 0.0, 1024, "task-1", &chapter(), &body, &CancelFlag::new())
            .await
            .unwrap();
        assert_eq!(d.mentions.len(), 1); // 超长 surface 丢弃
        assert_eq!(d.mentions[0].surface, "甲");
        assert_eq!(d.mentions[0].evidence.len(), EVIDENCE_PER_MENTION_MAX); // 截断到 20
    }

    #[tokio::test]
    async fn parse_failure_retries_once_then_errors() {
        // 两次都返回非 JSON → json_call 重试一次后报 model_output。
        let host = EngineHost {
            fs: Arc::new(MemFs::default()),
            clock: Arc::new(FixedClock(0)),
            events: Arc::new(NullEvents),
            model: Arc::new(ScriptedModel::new(vec![Ok("非 JSON".into()), Ok("还是非 JSON".into())])),
        };
        let err = scan_chapter(&host, &profile(), &prompts(), 0.0, 1024, "t", &chapter(), "正文", &CancelFlag::new())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "model_output");
    }
}
