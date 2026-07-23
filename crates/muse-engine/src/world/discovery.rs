//! 逐章世界实体发现（仿 `character::discovery`）：每章 1 次严格 JSON 调用，抽全 kind mention。

use crate::character::types::ChapterEntry;
use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{WorldChapterDiscovery, WorldEntityKind};
use super::WorldPrompts;

/// surface 最大字符数。
const SURFACE_MAX_CHARS: usize = 40;
/// quote 预览最大字符数。
const QUOTE_MAX_CHARS: usize = 200;
/// 每 mention 证据条数上限。
const EVIDENCE_PER_MENTION_MAX: usize = 20;
/// links 条数上限。
const LINKS_MAX: usize = 20;

/// 扫描单章：组装 user prompt → json_call<WorldChapterDiscovery> → 白名单校验。
/// 反幻觉铁律：quote 必须逐字取自正文；越界条目静默丢弃，不整章失败。
pub async fn scan_world_chapter(
    host: &EngineHost,
    profile: &ModelProfile,
    prompts: &WorldPrompts,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    chapter: &ChapterEntry,
    chapter_body: &str,
    cancel: &CancelFlag,
) -> Result<WorldChapterDiscovery, EngineError> {
    let user = build_scan_prompt(chapter, chapter_body);
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: prompts.scan_system.clone(),
        user,
        temperature,
        max_output_tokens,
        agent: "worldScan".to_string(),
        prompt_version: prompts.prompt_version.clone(),
        run_id: run_id.to_string(),
    };
    let raw: WorldChapterDiscovery =
        json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(sanitize_world_discovery(raw, chapter.index, chapter_body))
}

fn build_scan_prompt(chapter: &ChapterEntry, body: &str) -> String {
    format!(
        "章节序号：{idx}\n章节标题：{title}\n\n【章节正文开始】\n{body}\n【章节正文结束】\n\n\
请仅基于以上正文，尽量穷举本章出现的世界实体：角色/NPC/反派(character)、地点/秘境(location)、\
道具/法宝(item)、剧情节拍(plotBeat)、结局线索(endingClue)。严格输出 JSON：\n\
{{\"chapterIndex\":{idx},\"mentions\":[{{\"kind\":\"character|location|item|plotBeat|endingClue\",\
\"surface\":\"文中名称/称呼\",\"roleHint\":\"定位/体系/倾向提示\",\"links\":[\"关联实体\"],\
\"evidence\":[{{\"kind\":\"action|description|otherView|inference\",\"quote\":\"原文片段(≤200字,必须逐字取自正文)\",\
\"note\":\"简要说明\",\"confidence\":\"high|medium|low\"}}]}}]}}\n\
硬性要求：kind 必须是上述 5 类之一；surface 非空且 ≤ 40 字；quote 必须是正文的连续子串，禁止改写或杜撰；\
正文未出现的内容一律不要输出。为下游副本采样提供足量冗余，尽量穷举全书可用的剧情线/NPC/反派/地点/秘境/隐藏任务/道具/结局。",
        idx = chapter.index,
        title = chapter.title,
        body = body,
    )
}

/// 白名单校验：kind 不可解析 / surface 越界 → 丢弃整条 mention；越界 quote 丢弃该证据。
/// chapter_index 一律以代码为准（不信任模型序号）。
pub fn sanitize_world_discovery(
    mut raw: WorldChapterDiscovery,
    chapter_index: u32,
    body: &str,
) -> WorldChapterDiscovery {
    raw.chapter_index = chapter_index;
    raw.mentions.retain_mut(|m| {
        // kind 未知 → 丢弃。
        if WorldEntityKind::parse(&m.kind).is_none() {
            return false;
        }
        let surface = m.surface.trim().to_string();
        if surface.is_empty() || surface.chars().count() > SURFACE_MAX_CHARS {
            return false;
        }
        m.surface = surface;
        m.evidence.retain(|e| {
            let q = e.quote.trim();
            !q.is_empty() && q.chars().count() <= QUOTE_MAX_CHARS && body.contains(q)
        });
        m.evidence.truncate(EVIDENCE_PER_MENTION_MAX);
        // links 去空 + 截断。
        m.links.retain(|l| !l.trim().is_empty());
        m.links.truncate(LINKS_MAX);
        true
    });
    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::types::ChapterStatus;
    use crate::host::testing::{CollectEvents, FixedClock, MemFs};
    use crate::host::EngineHost;
    use crate::model::testing::ScriptedModel;
    use crate::model::{ModelInterface, ModelProfile};
    use crate::world::WorldPrompts;
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

    fn prompts() -> WorldPrompts {
        WorldPrompts::uniform("sys")
    }

    fn chapter() -> ChapterEntry {
        ChapterEntry {
            id: "ch-1".into(),
            index: 7,
            title: "第八章".into(),
            char_range: (0, 100),
            status: ChapterStatus::Pending,
            attempt: 0,
            discovery_store_key: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn drops_hallucinated_quote_unknown_kind_and_oversize_surface() {
        let body = "谢云走进无尽剑冢，取走焚寂剑。";
        let long_surface = "名".repeat(41);
        let resp = format!(
            r#"{{"chapterIndex":999,"mentions":[
                {{"kind":"character","surface":"谢云","evidence":[
                    {{"kind":"action","quote":"谢云走进无尽剑冢","confidence":"high"}},
                    {{"kind":"action","quote":"谢云飞天遁地","confidence":"low"}}
                ]}},
                {{"kind":"location","surface":"无尽剑冢","roleHint":"秘境","links":["剑冢入口"],"evidence":[]}},
                {{"kind":"unknown","surface":"莫名其妙","evidence":[]}},
                {{"kind":"item","surface":"{long_surface}","evidence":[]}}
            ]}}"#
        );
        let host = host_with(ScriptedModel::new(vec![Ok(resp)]));
        let d = scan_world_chapter(
            &host, &profile(), &prompts(), 0.0, 1024, "wt-1", &chapter(), body, &CancelFlag::new(),
        )
        .await
        .unwrap();
        assert_eq!(d.chapter_index, 7); // 代码覆盖模型序号
        // unknown kind 与超长 surface 丢弃 → 只剩 character + location。
        assert_eq!(d.mentions.len(), 2);
        let ch = d.mentions.iter().find(|m| m.kind == "character").unwrap();
        assert_eq!(ch.evidence.len(), 1); // 杜撰引文丢弃
        assert_eq!(ch.evidence[0].quote, "谢云走进无尽剑冢");
        let loc = d.mentions.iter().find(|m| m.kind == "location").unwrap();
        assert_eq!(loc.links, vec!["剑冢入口".to_string()]);
    }
}
