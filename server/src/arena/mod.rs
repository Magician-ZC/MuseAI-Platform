//! 赛事房（S5，feature=arena，agent-S5 填；P6 期权）。
//!
//! 待实现端点：
//! POST /arena/{worldId}/host/tick      主播控制台手动/半自动触发回合（节目节奏优先于定时器）
//! GET  /arena/{worldId}/report         透明战报：每回合仲裁 rule_refs + 道具生效记录（对抗"剧本"质疑）
//! POST /arena/{worldId}/revive-match   复活赛（付费边界：可买资格不可买免死，§2.5）
//! 赛制：唯一胜者；胜者奖励荣誉性（称号/立绘框/榜单），非强度性。

use axum::Router;

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    // AGENT-FILL(S5)
    Router::new()
}
