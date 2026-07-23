# MuseAI P0–P6 全栈构建台账

> 用途：多 agent 构建的唯一进度事实源与模块契约索引。每完成一个模块，负责 agent 在对应行更新状态。
> 规格来源：`docs/character-asset-p0-p2-product-dev-spec.md`（v1.2）+ `docs/platform-world-p3-p6-product-dev-spec.md`（v1.2）
> 构建模式：Fable 5 搭骨架（类型/契约/签名/接线），Opus 多 agent 填实现，禁止改动他人负责的文件。

## ⛔ 硬禁令（所有 agent）

**禁止任何 git 破坏性操作**：不得 `git stash`、`git reset --hard`、`git clean`、`git checkout -- `、`git rm`。原因：`crates/`、`server/`、`admin/` 及多个 `src/` 新文件尚未纳入 git 跟踪，破坏性 git 操作会误删其他 agent 正在写的文件（已发生过一次瞬时清空）。验证只用 `cargo test`/`npm test`/`tsc`，绝不用 git 做对照或清理。只读 git（status/log/diff）允许。

## 全局工程约定（所有 agent 必读）

1. **文件所有权**：只修改分配给你的文件。共享文件（`lib.rs`、`models.rs`、`useSettingsStore.ts`、`App.tsx`、`Cargo.toml`）由主循环统一改，agent 需要新注册项时在报告中列出，不自行编辑。
2. **宿主无关**：`crates/muse-engine` 不得依赖 tauri/axum 任何类型；文件、时钟、事件、模型调用一律走 `host.rs` 的 trait。
3. **严格 JSON**：所有模型调用要求严格 JSON 输出，解析失败重试一次；抽取/决策类 temperature=0。模型输出经 schema 校验 + 字段白名单 + 引用完整性校验后才可用。
4. **版本与原子性**：持久化对象含 `schemaVersion/revision/createdAt/updatedAt`；写入 = 临时文件 + 原子替换 + 保留一份备份。Zustand store 必须配 `version/migrate`。
5. **UI 与错误文案简体中文**；代码注释风格与现有代码一致（少而精）。
6. **测试**：每个模块交付时附带规格中列出的测试；Rust 用内联 `#[cfg(test)]`，前端用 vitest（`src/__tests__/`）。
7. **服务端权威**（server/）：客户端只提交意图；一切状态变更走服务端校验；副作用接口幂等键 + revision CAS。
8. 外部服务（短信/支付/内容安全/TTS/直播）一律 provider trait + `DevProvider` 实现（日志态/内存态），真实接入留配置位。
9. 数据库用 sqlx **运行时查询**（不用 `query!` 宏，避免编译期 DATABASE_URL 依赖）；队列抽象 trait：内存实现（dev/test）+ Redis 实现（prod）。

## 目录结构（目标态）

```text
crates/muse-engine/           # P0-P2 核心，宿主无关（src-tauri 与 server 共享）
server/                       # P3-P6 平台后端（axum + PG + Redis，可 dev 态内存运行）
admin/                        # 管理后台（React+antd+Vite 独立应用）
src-tauri/                    # 桌面壳：thin commands + TauriHost 适配器
src/                          # 桌面/移动前端：P0-P2 UI + 平台模式页面
docker-compose.yml            # PG + Redis（prod-like 本地环境）
```

## 模块清单与状态

状态：`[ ]` 未开始 · `[S]` 骨架完成 · `[I]` 实现中 · `[T]` 实现+测试完成 · `[V]` 集成验证通过

### Wave E — muse-engine（P0–P2 核心）

| 模块 | 文件 | 内容 | 状态 | 负责 |
|---|---|---|---|---|
| E0 crate 骨架 | `crates/muse-engine/{Cargo.toml,src/lib.rs,src/host.rs,src/model.rs,src/error.rs,src/store.rs}` | trait 层：HostFs/HostClock/HostEvents/ModelClient；原子写与版本化存储工具；OpenAI/Anthropic 兼容非流式 JSON 调用 | [T] | 主循环 |
| E1 角色管线 | `crates/muse-engine/src/character/*` | 章节切分/逐章发现/别名归并/证据账本/分层/DNA 合成/覆盖报告/任务模型（fingerprint+断点+幂等） | [T] | agent-E1（42 测试绿，全 crate 101 绿）。边界：源文件走 std::fs（宿主外绝对路径）；证据 locator 近似值 |
| E2 知识系统 | `crates/muse-engine/src/knowledge/*` | 切块/倒排索引/检索(关键词 MVP+可选重排)/四类蒸馏/绑定/使用日志/级联删除 | [T] | agent-E2（27 测试绿，隔离验证；待全 crate 编译） |
| E3 叙事状态 | `crates/muse-engine/src/narrative/{types,state,reducer,constraints,snapshot}.rs` | NarrativeState 五层/StatePatch 白名单校验/revision CAS/原子提交/快照分支/禁止谓词 DSL | [T] | agent-E3 |
| E4 叙事回合 | `crates/muse-engine/src/narrative/{mod,decide,arbiter,continuity}.rs` | 大纲约束解析/决策上下文白名单组装/role_decide 协议/规则+模型仲裁/确定性不变量检查/critic/回合编排/预算硬停 | [V] | agent-E4（run_round 全流程 + 信息边界隔离 + 不变量 I1-I4）。**全 crate 136 测试绿，src-tauri 接线编译通过** |

### Wave D — 桌面端接入（P0–P2 产品化）

| 模块 | 文件 | 内容 | 状态 | 负责 |
|---|---|---|---|---|
| D0 壳与注册 | `src-tauri/src/engine_host.rs`、`src-tauri/src/commands/{character_v2,knowledge,narrative}.rs`、`lib.rs` | TauriHost 适配器；thin command 壳；注册 | [ ] | 主循环 |
| D1 V2 类型与迁移 | `src/utils/characterCardV2.ts` | 规格 §9.1 全类型/V1→V2 迁移/draft-reviewed-ready 校验 | [T] | agent-D1 |
| D2 工具与评测 | `src/utils/{characterEvaluation,storyConstraints}.ts` | 互换/压力测试组装与报告；大纲约束解析 | [T] | agent-D1 |
| D3 前端 store | `src/stores/{useExtractionStore,useKnowledgePackStore,useCharacterRuntimeStore}.ts` + `usePartnerStore.ts` 分流 | 任务状态/知识包/运行状态；V2 分流与显式升级 | [I] | agent-D3 |
| D4 UI | ExtractionWizard/KnowledgePackManager/CharacterCardV2Editor + Story/Background/Settings 扩展 | 八阶段向导/知识包管理/V2 编辑器/观察与章节草稿模式/14 环节设置卡 | [T] | agent-D4（24 测试隔离绿,tsc 0 err,集成缝 subscribe 已落实） |
| D5 settings | `useSettingsStore.ts` 新 prompt+agentConfig+可选 modelId | 14 个新 Agent 的默认 prompt 与配置、按环节模型路由 | [T] | agent-D5（30 测试绿，v20→21）。注：Settings.tsx 硬编码 agentId 卡片，新环节 UI 待 D4 加 |

### Wave S — server（P3–P6 平台后端）

| 模块 | 文件 | 内容 | 状态 | 负责 |
|---|---|---|---|---|
| S0 服务骨架 | `server/{Cargo.toml,src/main.rs,src/app.rs,src/config.rs,src/db/*,src/providers/*,migrations/*}` | axum 装配/配置/迁移 SQL/provider traits(短信/审核/支付/TTS/存储)+Dev 实现/队列 trait(内存+Redis) | [V] | 主循环（server 全 crate 86 测试绿） |
| S1 账号与资产 | `server/src/{auth,assets}/*` | 验证码登录/JWT+refresh/年龄分层占位/角色不可变版本发布/审核队列/撤回删除导出 | [T] | agent-S1（20 测试绿）。集成修复：users 加 role 列、db.rs memory 单连接 |
| S2 世界与运行时 | `server/src/{worlds,runtime,events}/*` | 世界生命周期/tick 调度+worker/预算熔断/版本钉住/DomainEvent→WorldEvent 受众投影/WS 推送 | [T] | agent-S2（9 集成测试绿，受众隔离双层）。create_world 导出供 S6 |
| S3 干预与通知 | `server/src/{interventions,consents,notifications,reports,safety}/*` | 托梦/道具意图校验/同意状态机/通知 outbox/日报生成/注入检测/机审 | [T] | agent-S3（25 测试绿）。**集成缝：runtime 每 tick 需调 consents::expire_stale_consents + 每日 reports::generate_report；道具准入待 S4** |
| S4 章节房 | `server/src/{admission,assembly,backpack,chapters}/*` | 物品体系标签与准入策略(open/denylist/allowlist+translate)/开局装配器/跨世界背包/离线夹层 | [T] | agent-S4（20 测试绿）。carry 已调 check_admission；interventions 道具准入接线待集成 pass |
| （暂缓）赛事房 | `server/src/{arena,livegate,clips}/*`（feature `arena`） | 赛事 tick/礼物网关/透明战报/高光切片 | [-] | P6 期权：骨架+flag 已建，需直播协议+版号评估才填充 |
| （暂缓）计费 | `server/src/billing/*`（feature `billing`） | 订单/余额/退款/账本双录/幂等履约 | [-] | P4b 期权：骨架+flag 已建，需付费验证+支付合规才填充 |

### Wave A — admin（管理后台）

| 模块 | 文件 | 内容 | 状态 | 负责 |
|---|---|---|---|---|
| A0 应用骨架 | `admin/{package.json,vite.config.ts,src/main.tsx,src/App.tsx,src/api.ts}` | 登录/RBAC 路由/API client | [V] | 主循环 |
| A1 八模块页面 | `admin/src/pages/*` | 用户/审核/世界运营/经济/看板/Prompt 治理/风控/工单 | [T] | agent-A1（tsc 0 err,build 通过,接 25 端点）。高级运营功能待 S6 补端点 |

### Wave C — 客户端平台模式

| 模块 | 文件 | 内容 | 状态 | 负责 |
|---|---|---|---|---|
| C0 网络与鉴权 | `src/utils/cloudApi.ts`、`src/stores/useAuthStore.ts` | cloudFetch(token 刷新/幂等键)/cloudStream(WS)/登录态 | [V] | 主循环 |
| C1 平台页面 | `src/pages/platform/*`（Hall/WorldRoom/DailyReport/CharacterPublish/MyWorlds/Spectate） | 平台模式全部页面 + `usePlatformStore/useWalletStore` | [ ] | agent-C1 |

### Wave V — 集成验证

| 项 | 内容 | 状态 |
|---|---|---|
| V1 | `cargo test`（muse-engine 136 + server 86）全绿；src-tauri check 通过 | [V] |
| V2 | `npm run test` 403 passed(66 文件) + 前端 tsc 0 err；admin tsc+build 通过 | [V] |
| V3 | server dev 二进制启动冒烟通过（dev-login 签 admin JWT / 后台看板 DB 聚合 / 401 守卫 / 发码写库）；桌面 src-tauri 编译通过 | [V] |
| V4 | 关键规格测试点抽查（秘密隔离/StatePatch 拒绝矩阵/断点恢复/准入策略/受众投影隔离）——各 agent 均含专测 | [V] |

## 明确的降级与标注（诚实边界）

- 短信/支付/内容安全/TTS/直播网关 = DevProvider（日志/内存态），真实接入是配置期工作
- 备案/版号/实名服务等合规事项是运营动作，代码只预留接口与开关
- P4b/P5/P6 的付费闭环以 feature flag 默认关闭交付，符合两份文档的阶段门精神
