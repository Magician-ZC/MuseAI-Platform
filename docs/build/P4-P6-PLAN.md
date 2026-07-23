# MuseAI-Platform — P4-P6 整体重审与开发计划

> 新项目：从 MuseAI 基线 fork（P0-P5 已完成，muse-engine 136 / server 86 / 前端 403 测试绿）。
> 授权：yejiming 已授权基于本项目商业化平台开发。
> 本阶段目标：**整体重审 P4-P6** —— 审查加固已完成的 P4a/P5，补齐 P4b 计费 + P6 赛事房。

## 范围

| 子期 | 现状 | 本阶段动作 |
|---|---|---|
| P4a 放置房 | 已实现（worlds/runtime/events/interventions/consents/notifications/reports/safety + 平台前端） | **重审加固**：正确性/并发/安全/受众隔离 |
| P5 章节房 | 已实现（admission/assembly/backpack/chapters） | **重审加固**：准入/装配/背包服务端权威 |
| 后台/客户端 | admin_api + A1 后台 + C1 平台前端已实现 | **重审加固**：契约一致性/Local-first/权限 |
| P4b 计费 | 仅骨架 + `billing` flag | **新开发**：订单/余额/退款/账本双录/幂等履约（DevPayment） |
| P6 赛事房 | 仅骨架 + `arena` flag | **新开发**：赛事 tick/主播控制台/礼物网关/透明战报/高光切片（DevTts） |

## 合规边界（写死在实现里，不假装就绪）

- 外部服务一律 DevProvider（日志/内存/占位）+ 真实接入位 + 注释标注。
- 合规门是**运营动作**，非代码：生成式 AI 备案、网络游戏版号（P6）、支付牌照/结算资质（P4b）、拟人化互动服务评估、实名与未成年人保护。代码只预留开关与接口。
- P4b/P6 默认 feature-gated（`billing`/`arena`），不进默认构建；红线（不可提现/不可转账/不购买胜负/不做免死道具）写进实现与测试。

## Agent 编排

- **审查 wave（R1-R3，只读）**：分域审 P4a/P5 已有代码，产出分级问题清单（Critical/High/Medium/Low + file:line + 建议）。
- **加固 wave**：据审查清单修复确定性问题，测试守护。
- **新开发 wave（P4b/P6）**：billing + arena/livegate/clips + 前端/后台对应页面。
- **验证**：三 crate cargo test + 前端 npm test/tsc + admin build + server 二进制冒烟。

## 台账

沿用 `docs/build/BUILD-STATUS.md` 的约定（含 ⛔ git 破坏性操作禁令、服务端权威、Dev provider、sqlx 运行时查询）。本阶段进度追加记录于本文件末尾。
