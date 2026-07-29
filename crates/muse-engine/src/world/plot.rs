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
{{\"mainlineNodes\":[{{\"id\":\"mn-1\",\"fated\":true,\"chapterOrder\":1,\"variantGroup\":null,\"arcTags\":[\"arc-1\"],\
\"advanceWhen\":\"world.密道位置已知 == true\"}}],\
\"hiddenContentPool\":[{{\"id\":\"hc-1\",\"themes\":[\"复仇\"],\"template\":\"{{name}}发现{{seed}}\",\
\"rewardItemRef\":\"itm-xxx\",\"variantGroup\":\"vg-1\",\"arcTags\":[\"arc-1\"],\
\"producesWorldFacts\":[{{\"key\":\"密道位置已知\",\"value\":true}}]}}],\
\"sideHookPool\":[{{\"id\":\"sh-1\",\"themes\":[],\"template\":\"...\",\"arcTags\":[\"arc-1\"]}}],\
\"storylines\":[{{\"id\":\"arc-1\",\"summary\":\"...\",\"mainlineNodeIds\":[\"mn-1\"],\"hiddenPoolIds\":[\"hc-1\"],\
\"endingIds\":[\"end-1\"],\"affinity\":\"combat\"}}]}}\n\
要求：所有 id 全局唯一且非空；同一 variantGroup 内的条目互斥（采样每组至多取一），高价值奖励分散在不同 variantGroup；\
rewardItemRef 只引用已存在的道具 id。\n\
chapterOrder：**只给 fated=true 的节点**，表示这件事在原著里排第几，从 1 开始连续编号（不是章号），\
同时发生的给同一个号；顺序照上面节拍列表的章序。fated=false 的节点不要给它。\n\
\n\
【支线通向主线】这一项决定了这个世界好不好玩，请认真组织：\n\
- producesWorldFacts：某条支线**了结之后世界上会多出的事实**，如 \"密道位置已知\"、\"汉奸身份已揭穿\"。\
  key 只能是一段中文或英文词（**不能含 . 和 [**），value 缺省 true。绝大多数支线不留痕，就不要写这一项。\n\
- advanceWhen：某个主线节点**还需要什么条件才能推过去**，写 world.<key> == <值>，\
  其中 key 必须是上面某条支线真的会产出的那个 key。\n\
- 🔴 只支持「或」不支持「与」：多条路任一条通用 `A || B` 连起来（如 \
  \"world.密道位置已知 == true || world.正门令牌已到手 == true\"）。\
  **务必给关键主线节点写成多条路**——只有一条路时，没走那条支线的人会卡死在这里，而现场没有人能放水。\n\
- 🔴 advanceWhen 引用的每一个 key，都**必须**有某条支线的 producesWorldFacts 产出它。\
  引用一个没人产出的 key，那扇门就永远打不开，而这个世界看起来一切正常。",
        title = source_title,
        list = list,
        items = item_ids,
    );
    let spec = ModelCallSpec {
        max_retries: None,
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
        max_retries: None,
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
