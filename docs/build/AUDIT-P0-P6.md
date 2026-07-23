# MuseAI-Platform 全项目验收审计(P0-P6)

> 四个域并行只读审计(A1 引擎 / A2 前端桌面 / A3 P3-P5 服务端 / A4 P4b-P6+后台+平台前端),对照两份 dev spec 逐条核验。
> **总判定:忠实实现 spec,整体符合度约 90-95%。所有红线在代码+测试双重落实;无 Critical、无跨用户隐私泄漏、无资金逻辑错。** 缺口集中在呈现层未竣工 + 两个可小修项 + 若干已知 seam。

## 测试基线(全绿)
| 域 | 结果 |
|---|---|
| muse-engine | 136 passed / 0 failed |
| server(default) | 125 passed / 0 failed |
| server(`--features billing,arena`) | 150 passed / 0 failed |
| 前端 | 421 测试 + tsc 0 错误 |
| admin | tsc 0 + build 产出 dist |

## 各域符合度
- **A1 引擎 ~95%**:P2 自主叙事 100% 覆盖(回合 8 环节/信息边界铁律/StatePatch 白名单+拒绝矩阵/四不变量/原子提交回滚/blocked 非伪造);DNA V2 逐字段一致;宿主无关真实兑现(grep tauri|axum 零命中)。无 scope creep。
- **A2 前端桌面 ~92%**:三层(类型/store/UI + 命令壳)逐条对齐;三方 serde 一致;三分离存储物理落实;四 store 配 version/migrate;合成→partner 集成缝闭环。宿主无关领先于 spec(已抽独立 crate)。
- **A3 服务端 P3/P4a/P5 忠实**:**P4a-HARDENING 的 20 项加固逐一在代码+migration+测试验证为实体**;引擎联编 BLOCKER 真接通(真实 run_round 集成测试);受众双层硬隔离健全。
- **A4 P4b/P6/后台/平台前端 ~90%**:计费+赛事红线代码+测试双重落实;feature-gated 严格;后台八模块齐全(最小权限+审计+Prompt 治理版本化/灰度/回滚);契约两端精确对齐。**正面发现:S3 集成缝已闭合(runtime tick 确实调 expire_stale_consents + generate_report)。**

## 红线核对(全部通过)
余额不可提现/转账(无端点+404 测试)· 账本双录恒等式(测试断言)· 创作者结算不混 wallet · 买过程不买结果 · 无免死道具/端点(404 测试)· 胜者奖励荣誉非强度 · 淘汰不可逆→同意门控(approved 才落定)· 礼物走系统通道不走玩家干预 · 受众投影双层硬隔离 · 服务端权威(物品单一写入路径)· StatePatch 唯一状态变更路径。

## 缺口登记(去重综合,按处置分类)

### A. 建议就地小修(2 项)
| # | 缺口 | 严重度 | 域 | spec | 证据 |
|---|---|---|---|---|---|
| 1 | **互换测试编辑器 invoke 参数与 Rust DTO 不匹配**(运行时反序列化失败;tsc 因松 module augmentation 而过) | 真实 bug | 前端 | §3.4-5 | `CharacterCardV2Editor.tsx:41-44,176` vs `character_v2.rs:138` |
| 2 | **未成年拒充为潜在空防**(仅拦 age_declared==2,默认 0/未声明可充;§2.2 要求无法判断年龄前保守限充) | 合规邻接 | server billing | §2.2 | `billing/mod.rs:117-124` |

### B. Backlog(功能未竣工/需跨层)
| # | 缺口 | 严重度 | 域 | spec |
|---|---|---|---|---|
| 3 | 放置房同意触发源未接通(引擎从不产 ConsentRequested;赛事房已补,放置房仍缺) | ⚠️ 功能 | 引擎+server | §2.4 |
| 4 | P1 时间边界过滤未在引擎强制(仅存储,下放提示词层,无测试) | ⚠️ 功能 | 引擎 | §4.3.5 |
| 5 | L1 势力地图未实现(无渲染无占位) | ❌ 功能 | 平台前端 | §2.7/§11 |
| 6 | 关系图谱/状态面板数据源为事件共现启发式(非权威 relations/state) | ⚠️ 功能 | server+前端 | §11 |
| 7 | 证据级 conflictsWith 矛盾标记未回写 | ⚠️ 次要 | 引擎 | §9.1 |
| 8 | 压力测试有命令无 UI 触发点 | ⚠️ 次要 | 前端 | §3.2 |
| 9 | 后台前端 RBAC 未生效(后端 require_role 已强制,属前端纵深/UX) | ⚠️ 纵深 | admin | §3 |
| 10 | 审核工作台缺卡片全文+同作者历史 | ⚠️ 次要 | admin+server | §10 |
| 11 | 可审计 manifest(§2.3)未物化(仅存 card_json+rights) | ⚠️ 次要 | server | §2.3 |

### C. 可接受(spec 授权退化 / 阶段延期 / seam)
- §9.4 restricted 中间可见层折叠为 public/private(private 硬隔离完整,非安全缺口)。
- §11.1 查询改写/向量混合/模型重排未做(spec 明列的本地退化路径)。
- §15 移动端只读路由 0 条(spec 开放问题 5 承认的延期项)。
- 礼物→LLM 回合真实注入、复活/礼物实际扣费、L2 生图、L3 切片 live 触发、生产管理员登录(/auth/login 恒发 user)、诊断深看双人授权——均文档化 seam,与 BUILD 一致。
- 外部服务(短信/审核/支付/TTS/直播)DevProvider;合规门(备案/版号/牌照)是运营动作。

## 结论
项目在代码层面忠实实现了 P0-P6 的确定性正确性核心与全部红线,加固到位。距"面向公众上线"的差距是:①两个可小修项;②呈现层 L1 未竣工(势力地图/权威数据源)与放置房同意触发源等功能 backlog;③一批运营与合规动作(见 `docs/STARTUP.md` §8)。
