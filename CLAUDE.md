# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概览

MuseAI-Platform 是 MuseAI 的**双轨仓库**：
- **本地轨**：Tauri 2 桌面应用——本地 AI 伴侣 / 角色扮演 / 文字冒险 / 穿书互动 + 小说辅助创作。前端 React 19 + TypeScript + Vite + antd v6 + Zustand（`src/`），桌面后端 Rust（`src-tauri/`，lib 名 `tauri_app_lib`）。本地数据存 `~/Documents/MuseAI/`，LLM 用用户自己的 API Key。
- **平台轨**：多人世界平台——`crates/muse-engine`（宿主无关叙事引擎，trait 注入 ModelClient/fs/clock）+ `server/`（axum + sqlx AnyPool，SQLite dev / Postgres prod，迁移 `server/migrations/0001-0020` 启动自动执行）+ `admin/`（运营后台 React）+ `src/pages/platform/`（玩家端）。产品总规格 `docs/build/spec-world-ecosystem.md`，验证节奏 `docs/VALIDATION.md`，启动文档 `docs/STARTUP.md`。

平台轨常用命令：
```bash
cd server && cargo run --features billing,arena                 # 起平台 server(:8787)
cargo test --manifest-path server/Cargo.toml --features billing,arena
cargo test --manifest-path crates/muse-engine/Cargo.toml
cd admin && npm run build                                       # admin 类型检查+构建
```

## 常用命令

```bash
npm run tauri dev        # 启动完整桌面应用（自动起 Vite:1420 + Rust 后端）
npm run test             # 前端测试（vitest run）
npm run test -- src/__tests__/settings-store.test.ts   # 跑单个前端测试文件
npx vitest run -t "测试名"                              # 按测试名过滤
cargo test --manifest-path src-tauri/Cargo.toml         # Rust 测试
cargo test --manifest-path src-tauri/Cargo.toml tool_read_returns_line_numbers  # 单个 Rust 测试
npm run build            # tsc 类型检查 + vite build（没有配置 ESLint，tsc 就是唯一静态检查）
npm run tauri build      # 打生产安装包
```

CI（`.github/workflows/test.yml`）在 push/PR 时跑 `npm run test`、`npm run build` 和 cargo test，改动前本地先过这三样。

发布：推 `v*` tag 触发 `release.yml` 三平台打包。发版需同步改三处版本号：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`（提交习惯为 `release: vX.Y.Z`）。

## 架构

### 双宿主运行时：桌面 Tauri + 手机局域网浏览器

同一套前端跑在两种宿主里：桌面 Tauri webview，以及内嵌 axum 服务器（`src-tauri/src/mobile_server.rs`，在 `lib.rs` setup 中启动）服务的手机浏览器。**`src/utils/runtime.ts` 是这条缝的核心**：

- `appInvoke(cmd, args)`：桌面走 Tauri `invoke`，手机映射到 `/api/mobile/*` HTTP 接口（带 token 鉴权：URL 参数 → 内存 → `X-Mobile-Token` 头，服务端另存 HttpOnly cookie）。
- `listenStream(runId, ...)`：桌面监听 Tauri 事件 `agent-chat-stream`，手机走 SSE `/api/mobile/stream`。
- `isMobile()`（UA + 屏宽）决定 `App.tsx` 渲染 `MobileShell` + `Mobile*` 页面还是 `AppShell` + 桌面页面。

**给手机端加命令需要改三处**：Rust Tauri command（`lib.rs` 注册）+ `mobile_server.rs` 的 axum 路由 + `appInvoke` 的 switch 分支。只在桌面用的命令则直接 `invoke`，不必进 `appInvoke` 的类型表。

### Agent 循环在 Rust 侧

所有 LLM 调用都在后端完成，前端只发起/渲染流。入口 `start_chat_completion_stream`（`agent/sessions.rs`）按 run_id spawn 异步任务，依据 `request.model_interface` 分发到 `run_openai_agent_loop` 或 `run_anthropic_agent_loop`（`agent/mod.rs`）。每轮循环：组装 system prompt → 上下文压缩（`llm/mod.rs`，默认 20 轮阈值，token 按 4 字符/token 估算裁剪历史）→ 流式请求 → 执行工具调用 → 继续，直到无工具调用或超轮次上限。事件经 `emit_chat_event` 同时发往 Tauri 事件和手机 SSE 分发器。活跃流记录在 `ActiveStreams` state，`stop_chat_stream` 按 run_id abort。

请求（`ChatStreamRequest`，`models.rs`）自带完整模型凭据和采样参数——**后端对配置无状态**，配置全部由前端 store 组装传入。

### Agent 工具与技能

`tools/registry.rs` 定义工具集（read/write/edit/bash/grep/glob/skill/subagent/todo），按请求的 `allowed_tools` 过滤（如冒险模式只开放部分工具）。bash 工具有危险命令黑名单（`dangerous_command_reason`）+ 用户授权握手：前端通过 `resolve_bash_permission` 命令回填 `lib.rs` 里 `bash_permission_channels` 的 oneshot 通道。技能（写作提示词包）打包自 `src-tauri/resources/skills/`（fanqie-* 系列），也支持用户导入。

### 状态持久化

- 前端 Zustand store 通过 `createDiskStorage(name)`（`src/stores/diskStorage.ts`）→ `load_app_state`/`save_app_state` 命令 → `~/Documents/MuseAI/config/<name>.json` 持久化。手机端同样的 store 走 HTTP，因此手机和桌面共享同一份数据。
- 会话存为 `agent-sessions/{session,partner-session,story-session}-*.json`，带 `session_kind`（chat/story/bookTravel）过滤。
- 角色卡/世界书在 `partner-store` 中；手机端修改后后端发 `partner-store-updated` 事件，`App.tsx` 监听并回灌桌面端 store（`utils/partnerStoreSync.ts`）。

### 设置与多 Agent 配置

`useSettingsStore`（约 1400 行）持有：模型配置列表 `models`（每个标注 OpenAI-compatible / Anthropic-compatible）、按 agentId 的 `agentConfigs`（temperature、maxContextTokens、thinkingDepth 等，agentId 如 `partnerChat`、`storyAgent`、book-travel 各角色）、以及每个功能模块可编辑/可重置的 system prompt 全集。新增一个 AI 功能通常意味着：加一条 prompt 字段 + set/reset action + Settings 页 UI + agentConfig。

### 功能域 → 页面 → Rust 命令对应

- **Chat（伴侣聊天）/ Adventure（冒险跑团）/ Story + BookTravelMaterials（穿书）**：穿书有独立管线 `book_travel.rs`（素材装配、入场导演、场景规划/写作、记忆摘要、结局判定等专用命令）。
- **Background（背景设定）**：`generate_background_*` 从原文提取世界书/角色卡（要求模型输出严格 JSON）。
- **Bond（羁绊）**：`analyze_character_memory`、`optimize_character_memories` 归档记忆写回角色卡。
- **Outline（大纲）**：含反向大纲分布式分析（`start_reverse_outline_analysis` 等）。
- **Works（作品）**：文件树 + Markdown 编辑器（CodeMirror），版本历史在 `commands/versions.rs`（各文件同目录 `.versions/`）。

## 测试要点

- 前端测试集中在 `src/__tests__/`，jsdom + globals，setup 为 `src/test/setup.ts`：已全局 mock `@tauri-apps/api/core` 的 `invoke`（默认 resolve undefined，测试里用 `vi.mocked(invoke)` 覆盖返回值）、localStorage、ResizeObserver、matchMedia。
- `isTauriHost()`/`isMobile()` 在测试环境默认按「桌面 + Tauri」处理；要测手机端流程需设置 `(globalThis as any).__TEST_MOBILE_BYPASS__ = true`。
- Rust 测试是各文件内联的 `#[cfg(test)]` 模块（lib.rs、sessions.rs、registry.rs 等）。

## 约定

- UI 文案和后端错误信息均为简体中文。
- 面向用户的功能文档在 `README.md`（中文）/`README_EN.md`，数据目录结构以 README「数据存储说明」为准。
- Vite dev 端口固定 1420（strictPort），被占用会直接失败。

## 平台轨工程三约束（绑定所有平台开发，详见 docs/VALIDATION.md）

1. **未验证功能默认关闭**：新功能经 feature flag / 运营开关 / 数据配置启用，不随代码合并自动对用户开放。
2. **产品规则参数化**：托梦配额、死亡规则、奖励系数、成员规模、tick 节奏等一律可配置，禁止写死。
3. **状态语言七档**：`Concept → Specified → Implemented → Integrated → Production-ready → Validated → Enabled`；"测试全绿/已实现"不得表述为"可上线/已验证"。Dev 桩（短信/审核/实名/TTS/支付）不可上线。
4. 平台红线（不卖胜负与数值平权 / 资产单一写入 / 公共事实不可回滚 / 未成年保护 / 无提现 / AI 标识）锁进测试，任何改动需显式评审。
