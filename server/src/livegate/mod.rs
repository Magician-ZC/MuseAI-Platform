//! 直播网关（S5，feature=arena，agent-S5 填）：观众礼物 → 场内道具/环境事件映射。
//!
//! POST /livegate/webhook   直播平台礼物回调（dev 态：签名校验开关 + 模拟事件端点）
//!   → 礼物 SKU → 道具映射表（配置）→ interventions(kind=item, 系统代投) → 下一回合环境输入
//! 聚合：同回合同 SKU 合并计数，防事件风暴。

use axum::Router;

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    // AGENT-FILL(S5)
    Router::new()
}
