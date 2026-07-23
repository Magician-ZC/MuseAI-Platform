//! 全书节拍/结局合成（§防刷 ①）：把逐章暂存的 plot_beats/ending_clues 汇成
//! mainlineNodes + hiddenContentPool + sideHookPool + storylines（一次调用）与 endingPool（一次调用）。

use serde::Deserialize;

use crate::host::{CancelFlag, EngineHost};
use crate::model::{json_call, ModelCallSpec, ModelProfile};
use crate::EngineError;

use super::types::{
    EndingCandidateDraft, EndingClueDraft, MainlineNodeDraft, PlotBeatDraft, PoolItemDraft, Storyline,
};

/// plot 合成产物（mainline + 两池 + 剧情线分组）。
pub struct PlotSynthesis {
    pub mainline_nodes: Vec<MainlineNodeDraft>,
    pub hidden_content_pool: Vec<PoolItemDraft>,
    pub side_hook_pool: Vec<PoolItemDraft>,
    pub storylines: Vec<Storyline>,
}

/// 合成主线/隐藏/剧情线：一次模型调用。产出足量冗余供下游副本采样（互斥弧 + variantGroup）。
pub async fn synthesize_mainline(
    host: &EngineHost,
    profile: &ModelProfile,
    system: &str,
    prompt_version: &str,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    beats: &[PlotBeatDraft],
    item_ids: &[String],
    source_title: &str,
    cancel: &CancelFlag,
) -> Result<PlotSynthesis, EngineError> {
    if beats.is_empty() {
        return Ok(PlotSynthesis {
            mainline_nodes: Vec::new(),
            hidden_content_pool: Vec::new(),
            side_hook_pool: Vec::new(),
            storylines: Vec::new(),
        });
    }
    let list = beats
        .iter()
        .map(|b| {
            format!(
                "- {}（第{}章{}{}）",
                b.surface,
                b.chapter_index,
                if b.is_hidden { "，隐藏" } else { "" },
                if b.tension.is_empty() { String::new() } else { format!("，{}", b.tension) },
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "作品：{title}\n以下是从原文提取的剧情节拍（按章序）：\n{list}\n\
可引用的道具 id：{items:?}\n\n\
把它们组织为多条互斥/并行剧情线的内容超集（供下游副本采样，须足量冗余），严格输出 JSON：\n\
{{\"mainlineNodes\":[{{\"id\":\"mn-1\",\"fated\":true,\"variantGroup\":null,\"arcTags\":[\"arc-1\"]}}],\
\"hiddenContentPool\":[{{\"id\":\"hc-1\",\"themes\":[\"复仇\"],\"template\":\"{{name}}发现{{seed}}\",\
\"rewardItemRef\":\"itm-xxx\",\"variantGroup\":\"vg-1\",\"arcTags\":[\"arc-1\"]}}],\
\"sideHookPool\":[{{\"id\":\"sh-1\",\"themes\":[],\"template\":\"...\",\"arcTags\":[\"arc-1\"]}}],\
\"storylines\":[{{\"id\":\"arc-1\",\"summary\":\"...\",\"mainlineNodeIds\":[\"mn-1\"],\"hiddenPoolIds\":[\"hc-1\"],\
\"endingIds\":[\"end-1\"],\"affinity\":\"combat\"}}]}}\n\
要求：所有 id 全局唯一且非空；同一 variantGroup 内的条目互斥（采样每组至多取一），高价值奖励分散在不同 variantGroup；\
rewardItemRef 只引用已存在的道具 id。",
        title = source_title,
        list = list,
        items = item_ids,
    );
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: system.to_string(),
        user,
        temperature,
        max_output_tokens,
        agent: "worldPlotSynthesis".to_string(),
        prompt_version: prompt_version.to_string(),
        run_id: run_id.to_string(),
    };
    let resp: PlotResponse = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(PlotSynthesis {
        mainline_nodes: resp.mainline_nodes.into_iter().filter(|n| !n.id.trim().is_empty()).collect(),
        hidden_content_pool: resp.hidden_content_pool.into_iter().filter(|p| !p.id.trim().is_empty()).collect(),
        side_hook_pool: resp.side_hook_pool.into_iter().filter(|p| !p.id.trim().is_empty()).collect(),
        storylines: resp.storylines.into_iter().filter(|s| !s.id.trim().is_empty()).collect(),
    })
}

/// 合成结局池：一次模型调用。
pub async fn synthesize_endings(
    host: &EngineHost,
    profile: &ModelProfile,
    system: &str,
    prompt_version: &str,
    temperature: f32,
    max_output_tokens: u32,
    run_id: &str,
    clues: &[EndingClueDraft],
    source_title: &str,
    cancel: &CancelFlag,
) -> Result<Vec<EndingCandidateDraft>, EngineError> {
    if clues.is_empty() {
        return Ok(Vec::new());
    }
    let list = clues
        .iter()
        .map(|c| {
            format!(
                "- {}（第{}章{}）",
                c.surface,
                c.chapter_index,
                if c.affinity_hint.is_empty() { String::new() } else { format!("，倾向{}", c.affinity_hint) },
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "作品：{title}\n以下是从原文提取的结局线索：\n{list}\n\n\
合成结局候选池，严格输出 JSON：\n\
{{\"endingPool\":[{{\"id\":\"end-1\",\"affinity\":\"combat\",\"baseWeight\":1.0,\"arcTags\":[\"arc-1\"]}}]}}\n\
要求：id 全局唯一且非空；affinity ∈ strategist|combat|social 或省略；baseWeight 为正数。",
        title = source_title,
        list = list,
    );
    let spec = ModelCallSpec {
        profile: profile.clone(),
        system: system.to_string(),
        user,
        temperature,
        max_output_tokens,
        agent: "worldEndingSynthesis".to_string(),
        prompt_version: prompt_version.to_string(),
        run_id: run_id.to_string(),
    };
    let resp: EndingResponse = json_call(host.model.as_ref(), host.events.as_ref(), &spec, cancel).await?;
    Ok(resp.ending_pool.into_iter().filter(|e| !e.id.trim().is_empty()).collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlotResponse {
    #[serde(default)]
    mainline_nodes: Vec<MainlineNodeDraft>,
    #[serde(default)]
    hidden_content_pool: Vec<PoolItemDraft>,
    #[serde(default)]
    side_hook_pool: Vec<PoolItemDraft>,
    #[serde(default)]
    storylines: Vec<Storyline>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EndingResponse {
    #[serde(default)]
    ending_pool: Vec<EndingCandidateDraft>,
}
