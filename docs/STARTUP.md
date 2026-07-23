# MuseAI-Platform 启动文档

> 本地 AI 角色创作/互动引擎 + 云端角色资产平台。六层架构,双模式(本地免登录 + 平台登录解锁)。
> 本文档覆盖:环境准备、三个可运行目标的启动、配置、数据库、feature 开关、模型接入、冒烟验证、上线前合规门。

---

## 1. 架构总览

| 层 | 目录 | 语言 | 职责 | 对应阶段 |
|---|---|---|---|---|
| **引擎** | `crates/muse-engine` | Rust(宿主无关 crate) | 角色提取 / 知识系统 / 自主叙事回合;桌面与云端共享同一套 | P0-P2 |
| **桌面壳** | `src-tauri` | Rust(Tauri 2) | 本地模式:把引擎命令暴露给前端 | P0-P2 |
| **前端** | `src` | React 19 + TS + Vite | 本地创作 UI + 平台模式页面 | P0-P2 + 平台客户端 |
| **平台后端** | `server` | Rust(axum + sqlx) | 账号/世界运行时/干预/日报/安全/章节房/计费/赛事房 | P3-P6 |
| **管理后台** | `admin` | React + antd(独立 app) | 八模块运营后台 | P3+ |
| **基础设施** | `docker-compose.yml` | PostgreSQL + Redis | 生产环境(dev 可零配置) | — |

**双模式红线**:本地模式(桌面 P0-P2)永不锁登录、永不联网校验;平台能力登录后解锁,与本地能力物理隔离。

---

## 2. 环境准备

| 工具 | 版本(验证于) | 用途 |
|---|---|---|
| Node.js | ≥ 20(验证 22.16) | 前端 / 后台 |
| Rust + Cargo | ≥ 1.80(验证 1.95) | 引擎 / 桌面 / 后端 |
| Tauri 系统依赖 | 平台相关 | 桌面构建(macOS: Xcode CLT;Linux: libwebkit2gtk-4.1-dev 等;见 `.github/workflows`) |
| Docker(可选) | — | 生产态 PG + Redis |

```bash
# 依赖安装
npm ci                    # 前端(项目根)
cd admin && npm ci        # 后台
```

---

## 3. 三个可运行目标

### 3.1 桌面应用(本地模式,P0-P2,零配置)

```bash
npm run tauri dev
```

- 自动起 Vite(端口 **1420**,固定)+ 编译 Rust 后端 + 打开桌面窗口。首次编译 Rust 需几分钟。
- 数据存本地 `~/Documents/MuseAI/`,无需登录、无需服务器。
- AI 功能需先在「设置」页填入自己的 API Key(见 §6)。
- 打包:`npm run tauri build`。

### 3.2 平台后端(P3-P6,dev 零配置)

```bash
cd server && cargo run                          # 仅 P3/P4a/P5(默认 feature)
cd server && cargo run --features billing,arena # 含 P4b 计费 + P6 赛事房
```

- 默认 dev 态:SQLite 内存库 + 内存队列 + Dev providers,**无需任何外部依赖**,监听 `127.0.0.1:8787`。
- 迁移(`server/migrations/0001-0008`)启动时自动执行。
- **feature-gated**:`billing`(计费)、`arena`(赛事房)默认关闭,不进默认构建(合规阶段门未过)。

### 3.3 管理后台

```bash
cd admin && npm run dev        # 端口 1430
```

- 登录:后台登录页用 **dev-login**,密钥默认 `muse-dev-admin`(见 §4 `MUSE_ADMIN_DEV_SECRET`),仅 `MUSE_DEV=1` 下开放;生产 dev-login 直接 403,需真实管理员账号(`users.role`)。
- 构建:`npm run build`。

### 3.4 生产态基础设施(可选)

```bash
docker compose up -d      # PostgreSQL(5433) + Redis(6380)
# 然后用 PG 连接串启动后端:
MUSE_DATABASE_URL=postgres://muse:muse@127.0.0.1:5433/muse cargo run -p muse-server --features billing,arena
```

---

## 4. 配置(环境变量)

后端全部环境变量(`server/src/config.rs` + 各模块),dev 态均有默认值:

| 变量 | 默认 | 说明 |
|---|---|---|
| `MUSE_DATABASE_URL` | `sqlite::memory:` | 数据库。`:memory:` dev 用单永久连接;文件库 `sqlite://muse.db`;生产 `postgres://…` |
| `MUSE_BIND` | `127.0.0.1:8787` | 监听地址 |
| `MUSE_JWT_SECRET` | `dev-secret-change-me` | JWT 签名密钥。**生产必须改** |
| `MUSE_ACCESS_TTL` | `3600` | access token 秒 |
| `MUSE_REFRESH_TTL` | `2592000` | refresh token 秒(30 天) |
| `MUSE_DEV` | `1` | dev 模式:验证码打日志/dev-login 开放/审核直通。**生产设 0** |
| `MUSE_OBJECT_DIR` | `./muse-objects` | 对象存储根(立绘/切片)。已 gitignore |
| `MUSE_ADMIN_DEV_SECRET` | `muse-dev-admin` | 后台 dev-login 密钥(仅 dev) |
| `MUSE_TICK_WORKERS` | `2` | 世界 tick worker 并发数 |
| `MUSE_TICK_INTERVAL_MS` / `MUSE_TICK_POLL_MS` | 内置 | tick 调度间隔 / 轮询间隔 |
| `MUSE_OUTBOX_RESCAN_MS` | `60000` | 通知 outbox 恢复重扫间隔 |
| `MUSE_LIVEGATE_SECRET` | 未配置=fail-closed | 赛事房礼物 webhook 验签(生产必配) |

> 生产最小改动:`MUSE_DEV=0`、`MUSE_JWT_SECRET=<强随机>`、`MUSE_DATABASE_URL=<postgres>`、`MUSE_LIVEGATE_SECRET=<密钥>`(若开 arena)。

---

## 5. 数据库与迁移

- 迁移文件 `server/migrations/0001-0008.sql`,启动自动按版本号顺序执行(sqlx migrate)。
- 可移植 SQL 子集(TEXT id / BIGINT 毫秒 / TEXT JSON / INTEGER 布尔),SQLite 与 Postgres 双跑。
- `0001` 初始全表;`0002-0005` P4a/P5 加固(tick/同意独立表/通知去重/章节);`0006` 计费索引;`0007` 赛事房;`0008` 礼物/切片。
- dev 内存库每次启动重建;需要持久化 dev 数据用文件库 `sqlite://muse-dev.db`(已 gitignore)。

---

## 6. 模型接入(BYO Key)

- **本地模式**:桌面「设置」页配置——API Key + 接口地址 + 模型名,支持 OpenAI 兼容与 Anthropic 兼容。可为不同环节(角色提取 / 知识蒸馏 / 叙事各环节 / 去 AI 味等)分别配置模型与采样参数。
- **平台后端**:世界按钉住版本从 `model_routes` / `prompt_versions` 表解析模型路由与 prompt(管理后台「模型与 Prompt 治理」配置)。**无模型配置时 tick 安全 no-op 跳过**,不会崩。
- 引擎对模型配置无状态——凭据随请求/世界配置传入。

---

## 7. 冒烟验证

```bash
# 引擎 + 后端 + 桌面壳(全绿基线)
cargo test --manifest-path crates/muse-engine/Cargo.toml          # 136
(cd server && cargo test)                                          # 125(default)
(cd server && cargo test --features billing,arena)                 # 150
cargo check --manifest-path src-tauri/Cargo.toml                   # 编译
# 前端 + 后台
npm run test -- --maxWorkers=2                                     # 421
npx tsc --noEmit                                                   # 0 错误
(cd admin && npm run build)                                        # 产出 dist
```

后端进程端到端冒烟(dev):

```bash
cd server && MUSE_DEV=1 cargo run --features billing,arena &
# 后台登录
curl -sX POST 127.0.0.1:8787/api/admin/dev-login -H 'Content-Type: application/json' -d '{"secret":"muse-dev-admin"}'
# 平台注册→登录→充值(dev 验证码在响应 devCode 里)
curl -sX POST 127.0.0.1:8787/api/auth/challenge -H 'Content-Type: application/json' -d '{"phone":"13800138000"}'
# → 用返回的 devCode 调 /api/auth/login,拿 accessToken 后 /api/billing/balance、/api/billing/orders …
```

---

## 8. 上线前合规门(运营动作,非代码)

代码用 DevProvider + 预留真实接入位实现;**面向公众上线前必须完成**:

| 门 | 触发 |
|---|---|
| 经营主体 + ICP 备案 + 增值电信评估 | 服务对外 |
| 生成式 AI 服务备案 / 算法备案 / 安全评估 | 平台代调模型向公众生成 |
| AI 生成内容标识 | 全部世界内容(代码已内置标识位) |
| 实名 + 未成年人保护 | 账号/付费 |
| **支付牌照 / 结算资质** | P4b 计费收费 |
| **网络游戏版号评估** | P6 赛事房(账号成长+道具+付费+竞技) |
| 拟人化互动服务管理办法评估 | 平台世界公测 |
| 直播平台玩法审核 + 主播协议 | P6 赛事房礼物 |

真实外部服务接入位(替换 Dev 实现):短信(`providers::SmsProvider`)、内容审核(`ModerationProvider`)、支付(`PaymentProvider`)、TTS(`TtsProvider`)、直播礼物网关(`livegate`)。

---

## 9. 已知 seam(明确标注,待接)

- **礼物→LLM 回合真实注入**:gift boon 记入 `arena_env_events` + 进战报,注入引擎回合需 `RoundInput` 扩展。
- **复活/礼物实际扣费**:记账已在,实际扣费经 billing 集成(跨 feature)。
- **placement 房同意触发源**:赛事房淘汰处已补同意门控;placement 房的死亡/永久关系触发待叙事迭代。
- **L2/L3 视觉呈现**:当前 L0 文字流 + L1 结构化卡片(事件卡/关系图谱/状态面板);立绘/切片为 DevProvider 占位。
- **创作者结算**:与用户钱包是两套账,本期只做用户侧。

---

## 10. 文档索引

- `docs/character-asset-p0-p2-product-dev-spec.md` — P0-P2 引擎产品/开发规格
- `docs/platform-world-p3-p6-product-dev-spec.md` — P3-P6 平台产品/开发规格
- `docs/build/BUILD-STATUS.md` — 全栈构建台账(含工程约定、git 禁令)
- `docs/build/P4a-HARDENING.md` — P4a/P5 整体重审 triage 与加固记录
- `docs/build/P4b-P6-BUILD.md` — P4b 计费 + P6 赛事房开发契约与验证
- `CLAUDE.md` — 仓库结构与惯例(给 AI 协作者)
