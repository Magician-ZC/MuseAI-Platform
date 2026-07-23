//! 计费（S5，feature=billing，agent-S5 填；P4b 条件性——文档阶段门未过，默认不编译不部署）。
//!
//! 原则（平台规格 §2.6）：余额不可提现不可转账；订单/退款/对账幂等；账本双录（orders + ledger_entries）；
//! 用户钱包与创作者结算是两套账（本模块只做用户侧）。
//!
//! 待实现端点：
//! POST /billing/orders {kind, amountCents} + Idempotency-Key → DevPayment 履约 → 余额入账（事务：orders+ledger+balance）
//! GET  /billing/balance
//! POST /billing/refunds {orderId} → 状态机校验后逆向入账

use axum::Router;

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    // AGENT-FILL(S5)
    Router::new()
}
