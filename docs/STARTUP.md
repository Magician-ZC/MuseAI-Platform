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
- 迁移(`server/migrations/`,取号看目录里的最大号)启动时自动执行。
- **状态诚实声明**:短信、内容审核(含图审)、实名、TTS、支付当前均为 **Dev 桩**(直过/回显),
  按七档状态语言属 `Implemented`,**不是 `Production-ready`**——上线需接真实 Provider、
  补实名闭环(当前接口存在请求方直提 verified 的 dev 口子)并完成"生成式 AI 服务 + 游戏监管"双轨合规评估。
  发布节奏与门槛见 `docs/VALIDATION.md`。
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
# 然后用 PG 连接串启动后端(注意:仓库无根 workspace,必须先 cd server):
cd server && MUSE_DATABASE_URL=postgres://muse:muse@127.0.0.1:5433/muse cargo run --features billing,arena
```

> 各 Rust crate 独立成包(`server`、`crates/muse-engine`、`src-tauri` 各有自己的 `Cargo.toml`,
> **仓库根没有 workspace `Cargo.toml`**)——所有 cargo 命令必须 `cd` 进对应目录或带 `--manifest-path`,
> `cargo run -p muse-server` 在仓库根会直接报 `could not find Cargo.toml`。

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
| `MUSE_MODERATION_HTTP_ENDPOINT` | 未配置=用 Dev 桩 | 真实内容审核服务商的 HTTP 端点。🔴 **生产必配**——不配则 `ModerationProvider` 仍是 `DevModeration`(只匹配一张小关键词表),§15 五层漏斗的第 3 层**拦不住任何东西**;内容安全是合规主体责任。配了它就要一并配同组的认证/请求体/响应映射变量,**配错即启动失败**(fail-closed,同 `MUSE_CORS_ORIGINS` 的取向)。同组变量与逐条含义见 `server/src/providers/http_moderation.rs` 模块头,那里是唯一权威登记处 |
| `MUSE_TOKEN_CNY_CENTS_PER_1K` | 内置(`runtime/mod.rs:66`) | 每 1K token 折算人民币分。**成本仪表定价基准**(总规格 §17),`world_ticks.cost_tokens` 逐拍记账按此换算 |
| `MUSE_DREAM_QUOTA_PER_STAGE` | `3` | 托梦配额:每卡每阶段条数(总规格 §8)。非正整数/垃圾值一律回落默认——防运营误配把托梦通道锁死 |
| `MUSE_SAFETY_LEXICON` | **开启** | 运行时敏感词库总开关(§15 第 2 层)。**默认开启且应保持开启**——内容安全是合规主体责任下的恒开设施,此开关定位是误伤应急阀,不是灰度位 |
| `MUSE_SAFETY_LEXICON_EXTRA` | 空 | 运营补充敏感词(逗号/分号/换行分隔),归类 `custom`、低危 |
| `MUSE_SAFETY_RUNTIME_AUDIT` | `high` | 运行时命中入人审队列的策略:`high`(仅高危)/`all`/`none`。**命中一律记 risk_events**,本开关只管是否额外入 `audit_queue`(每 tick 每事件都入队会淹掉人审)。配错值回落 `high`,不静默放宽或收紧 |
| `MUSE_IFLINE_ADVANCE_STALE_MS` | `600000`(10min) | if 线推进「在飞」标记的陈旧线(migration 0050)。推进已改异步:端点回 202、模型调用在后台 worker 跑,在飞期间重复点击 409。🔴 **这一列不可省**:`MemQueue` 不持久,进程重启会带走在飞任务而标记已写下——没有陈旧线,这条**付费**的 if 线就永久推不动了(玩家已烧掉副本卡)。⚠️ 陈旧线只是**让玩家能再点一次**,不是自动补上丢掉的那次——补上那次的是下一行的对账补投。同组 `MUSE_IFLINE_WORKERS`(默认 1,**池大小即成本闸**) |
| `MUSE_IFLINE_ADVANCE_SWEEP` | **关闭** | if 线推进的**对账式补偿**(migration 0052)。开着它才会把「在飞标记还在、且已过补投窗口」的行补投回推进队列——即**玩家没再点也补上**丢掉的那一拍。🔴 **它不是调度器**:判据恒为 `advance_requested_at > 0`,而那一列只由玩家点击写下,故它永远只补投「玩家已经点过」的那一次。🔴 **补投是真的调模型、真烧 token**(if 线是付费内容),故有封顶 `MUSE_IFLINE_SWEEP_MAX_REDELIVERIES`(默认 3)——worker 若每次都在清标记前就死掉,无封顶的补投是个无限烧钱的循环;到顶时清标记 + 写 `last_error`,**不静默放弃**。🔴 补投窗口 `MUSE_IFLINE_SWEEP_AFTER_MS`(默认 30min)恒被钳在 `MUSE_IFLINE_ADVANCE_STALE_MS + 1min` 之上(代码保证),配小了会对仍在跑的任务补投、白烧一次调用。🔴 **单实例**:多实例同开会重复补投(同 L3 sweep)。同组 `MUSE_IFLINE_SWEEP_INTERVAL_MS`(默认 5min)/ `MUSE_IFLINE_SWEEP_BATCH`(默认 50) |
| `MUSE_SAFETY_RECHECK_SWEEP` | **关闭** | 第 3 层复核的**补偿轮询**(扫尾未复核拍)。内存队列不持久,进程重启会把在飞的复核任务带走;另有一类拍(tick 走 blocked / cas_conflict 收尾)压根没经过入队那一行。开着它才会按 `world_ticks ⋈ safety_recheck_runs` 对账补投。🔴 **有真实覆盖上限**:只回看 `MUSE_SAFETY_L3_SWEEP_LOOKBACK_MS`(默认 24h),挂机超过这段的拍**永远补不回来**——`GET /api/admin/safety/recheck` 的 `durability.justOutsideWindow` 就是量它的。🔴 **单实例**:多实例同开会重复补投(重复的 provider 调用是真烧的),同 `world_events.sequence` 那条,属发布纪律。调参前缀 `MUSE_SAFETY_L3_SWEEP_*`(间隔 / 宽限 / 回看 / 批量) |
| `MUSE_IMPRINT_CAPACITY` | `12` | 世界线烙印的**恒定容量**(§3.62)。满了之后最旧的一条褪色而非删除。🔴 **它是平权机制的一部分,不是性能参数**:不设容量,老卡就是"烙印更多"而非"烙印更旧",直接滑向养成优势。同时它也是上下文成本的上限,而 12 这个值是拍脑袋的——真实 token 账单出来之前不必当真 |
| `MUSE_FATED_TICK_SPACING` | `600` | 主线节点映射到 DES 时间轴的**宿命时刻间距**(秒)。`chapterOrder × 本值 → due_at`。宿命时刻优先于角色行动被拉到时钟上——原著主线该发生时就会发生,不因所有人都在忙而错过 |
| `MUSE_LIFE_MARKED_AT` / `MUSE_LIFE_STORIED_AT` | `30` / `120` | 生命层两档跃迁(崭新 → 有痕 → 有史)的世界记忆条数门槛。这一层是「用户不能编辑的部分」,即一张卡真正有生命、也**唯一无法被复刻**的地方(别人可以抄走内核,抄不走你的卡活过什么)。⚠️ 两个数都是拍脑袋的——真实值取决于「一局世界平均留下多少条记忆」,而那要等真实模型跑过才有;调它**不改变任何判定**,只改变展示。🔵 这一行 2026-07-29 之前写的是 `3`/`8`,与代码里的 `30`/`120` 对不上——文档基线也会过期,同 CLAUDE.md 顶上那条 |
| `MUSE_OPPORTUNITY_SWING_BP` / `MUSE_FORTUNE_SWING_BP` | `2000` / `5000` | **机缘 / 气运**满档时的摆幅(万分比,即间隙内容条数 +20% / 权重两极化强度 0.5),总规格 §12.5。机缘调间隙内容的**密度**,气运调其**幅度**(两极化,温和与凶险等量抬起)。🔴 **两者作用于世界不作用于角色**,全员共享——不存在"我气运高我拿得多";且**产出封顶与稀有预算不受其影响**(`RARE_TIER`/`RARE_BUDGET`/星级封顶在采样之后执行,气运只决定抽到哪些)。零档(无人带着经历进来) → 恒中性 → 采样产物与本层落地前逐字节相同。⚠️ **2026-07-29 第二版改了语义**:第一版是烙印指纹哈希出来的 ±摆幅(可正可负、会跳);第二版是**由档位线性缩放的单调量**(0 档 → 无效果,满档 → 全摆幅),档位来自各卡烙印计点后的几何阶梯(4/12/28/60/124,封顶 5 档,见 §3.64)。机缘因此变成只增不减,挡住「更多 = 更好」的是「它一个字都不进权重」(`opportunity_never_reaches_the_weights`) |
| `MUSE_WORLDLINE_IMPRINT_CONTEXT` | **关闭** | 世界线烙印进决策上下文(提案第 5 步,按世界解析)。开着它,每张带烙印的卡会在自己的可见上下文里多一格 `yourPast`(它在**别的世界**里经历过什么,按褪色阶梯措辞)。🔴 **同批四件事里只有它带闸**:另外三件(chapterOrder 归一 / 气运机缘量化 / 量化显示)要么是确定性数据处理、要么是只读展示,都有「零输入时逐字节不变」兜底;而这一件**直接改模型 prompt**,既影响 token 账单也影响输出,且**效果无法证伪**(要真验得做同内核/同世界/同种子的 A/B,需要真实模型凭据)。状态 `Integrated`,不是 `Validated`。⚠️ 措辞表(`imprint::PHRASES`,18 句)是工程写的,**没过内容评审**——写歪一句就会把「经历」变成「养成」,而所有既有红线一条都不会红(它们守的是数据通道,这条风险走的是文字) |
| `MUSE_CORS_ORIGINS` | 本地开发六项(见下) | 跨源白名单(逗号分隔)。**三个前端都与 server 不同源**:玩家端 Vite `:1420`、运营后台 Vite `:1430`、Tauri webview(`tauri://localhost` / `https://tauri.localhost`),而 server 在 `:8787`。🔴 **生产必配**——不配则只放行本地开发来源,线上域名会被拦。非法条目跳过并告警,全部非法则退化为「不放行任何跨源」(fail-closed:配错了宁可前端连不上、立刻可见,也不静默放宽成通配)。**刻意不提供通配选项**:这些接口虽有 JWT 鉴权,放开任意源仍是无谓攻击面 |

> 🔴 **配上 `MUSE_MODERATION_HTTP_ENDPOINT` 的那一刻,一件事当场生效、另一件不会——两者极易混淆。**
>
> | 什么 | 由谁控制 | 配了 endpoint 之后 |
> |---|---|---|
> | **静态内容审核**(角色卡 / 世界模板 / 装配钩子 / 入站托梦信) | **没有独立开关**——`safety::moderate_and_queue` 无条件调 `check_text` | **当场切到真实厂商。** 厂商挂了 → 这些上传返回 500 而不是被放行(fail-closed,方向正确,但这是配置当天就能看见的行为变化,不该事后才发现) |
> | **第 3 层运行时语义复核** | `MUSE_SAFETY_SEMANTIC_RECHECK`,**默认关** | **不会自己开始。** 配好 provider ≠ 开始复核,两个开关是分离的 |
> | **第 3 层的投递补漏**(补偿轮询) | `MUSE_SAFETY_RECHECK_SWEEP`,**默认关** | **也不会自己开始**,而且它与上一行**还是分离的**:第 3 层开着、轮询关着 = 复核会跑,但**丢掉的拍没人捡**。这条链一共三个开关,三个都得按 |
>
> ⚠️ 本 provider 是**文本**审核。它刻意**不继承** trait 的 `check_image` 直过默认——那会造出一个
> 自称「已接真实服务」(`is_dev_stub() == false`)却放行每一张图的 provider,正是 `is_dev_stub`
> 这个方法存在要防的假防线。图片默认转**人审**(`Pending`),所以配置当天人审队列会开始积压图片;
> `MUSE_MODERATION_HTTP_IMAGE_FALLBACK=approved` 可以关掉,但那等于显式声明「图片没有机器审核」。
>
> 🔴 **上表不是全部——它是「部署时必须关心」的那些。** 此处原先写着「上表即全部」,
> 而 2026-07-27 用它自己给的校验命令清点:代码里 **105 个** `MUSE_*`,表里 **21 个**,
> 差着 84 个。那句话从写下之后的每一批开发都在让它更不成立。
>
> 剩下的绝大多数是**功能参数**(配额、阈值、窗口、页大小、分成比例……),
> dev 与生产**都有合理默认值,不配也能跑**;把它们全列进这张表,只会淹没掉真正需要
> 在生产改的那几行(`MUSE_DEV` / `MUSE_JWT_SECRET` / `MUSE_DATABASE_URL` /
> `MUSE_CORS_ORIGINS` / `MUSE_LIVEGATE_SECRET`)。
>
> 需要完整清单时**以代码为准**:
>
> ```bash
> grep -rhoE '"MUSE_[A-Z0-9_]+"' server/src crates | tr -d '"' | sort -u
> ```
>
> - **功能开关类**(默认开/关、可按用户/世界灰度)以 `flags::KNOWN_FLAGS` 为唯一权威——
>   那里有每个开关的默认值、归属模块、一句话说明与是否已接线,比任何文档副本都新。
> - **各模块的参数**在其模块头注释里就地说明(如 `runtime::offpeak`、`slo::calibration`、
>   `social`、`livestage`),改参数的人一定会读那里,不一定会读本文。
>
> 同理见 `docs/STARTUP.md` §7(用例基线)、`flags::KNOWN_FLAGS`(开关数)、`docs/API.md`(路由数)
> ——本仓库一律**不复述会过期的清单**,只指向唯一权威源。

> 生产最小改动:`MUSE_DEV=0`、`MUSE_JWT_SECRET=<强随机>`、`MUSE_DATABASE_URL=<postgres>`、`MUSE_LIVEGATE_SECRET=<密钥>`(若开 arena)。

---

## 5. 数据库与迁移

- 迁移文件在 `server/migrations/`,启动自动按版本号顺序执行(sqlx migrate)。
  ⚠️ **此处刻意不写号段范围**:它过期过(一度停在 `0001-0043`),而这是本仓库第五处同类问题
  ——用例基线、开关数、路由数、环境变量清单都栽在「写下时是对的、之后每批开发让它更不对」上。
  **取号一律看目录里的最大号。**
  ⚠️ 下表只列到 `0029`(历史遗留,`0030` 起未补);**迁移清单以目录为准**,取号看目录里的最大号。
- 可移植 SQL 子集(TEXT id / BIGINT 毫秒 / TEXT JSON / INTEGER 布尔),SQLite 与 Postgres 双跑。
- dev 内存库每次启动重建;需要持久化 dev 数据用文件库 `sqlite://muse-dev.db`(已 gitignore)。
- ⚠️ **`0023` 与 `0028` 是空号,不要补**。两者都是并行开发时预留、最终判定不需要建表的:
  `0023` 预留给「运行时内容安全」(复用了 `0001` 就有的 `world_events.moderation` 列);
  `0028` 预留给「成员羁绊字段」(全部是对既有表的 join,无新表新列)。
  sqlx 只要求版本号唯一递增、不要求连续,跳号本身无害;
  但**事后往回插入一个比已应用版本更小的号会让已迁移的库报错**——需要新迁移一律往后取号。

| 迁移 | 内容 |
|---|---|
| `0001_init` | 初始全表(账号/资产/世界/成员/事件/干预/同意/通知/审计/风控/治理) |
| `0002_tick_hardening` | tick 加固(并发领取/幂等) |
| `0003_consent_responses` | 同意响应独立表 |
| `0004_notification_dedupe_index` | 通知去重索引 |
| `0005_chapter_hardening` | 章节房加固 |
| `0006_billing` | 计费索引(feature `billing`) |
| `0007_arena` | 赛事房(feature `arena`) |
| `0008_gift_clips` | 礼物 / 高光切片 |
| `0009_character_manifest` | 角色卡清单与版本钉住 |
| `0010_timeline` | 世界线时间轴 |
| `0011_world_asset_templates` | 创作者世界模板资产 |
| `0012_arena_spectate` | 赛事观战 |
| `0013_creator_economy` | 创作者收益(平台内权益,无提现) |
| `0014_room_revive_pricing` | 开房 / 复活定价参数 |
| `0015_p3_cloud_growth_item_shop` | 云成长与平台道具售卖 |
| `0016_character_avatar` | 角色立绘 |
| `0017_world_discovery` | 世界发现 / 热门 |
| `0018_moderation_appeals` | 审核申诉复审链 |
| `0019_progression` | **历练与卡位**(总规格 §11 底座) |
| `0020_template_star` | **模板星级 curation**(3-5★ 仅运营晋升) |
| `0021_source_fingerprint` | **同源卡同世界唯一**(§7):`cloud_characters` 加提取源指纹 + 原味卡标记 |
| `0022_dream_quota_index` | **托梦配额**(§8)按卡计数索引 `(world_id, character_id, status)` |
| `0024_saga_stage` | **Saga 归组**(§3):`world_templates` 加 `saga_id` + `stage_no`(0023 见上方跳号说明) |
| `0025_worldline_contribution` | **三层结算③世界线层**(§9):贡献归因表(独立于 `narrative_state_json`,绝不回灌引擎) |
| `0026_lethality` | **生死契约三档**(§11):`worlds` 加 `lethality`(默认 `consent`,历史行行为零变化) |
| `0027_world_cover` | 世界封面三列(复刻 0016 立绘范式:object_key/url/moderation,过审才下发) |
| `0029_engagement_invitations` | 发布物浏览·收藏计数(append-only 登记表,无可变计数列即无热点行)+ 房间邀请(默认关闭) |

---

## 6. 模型接入(BYO Key)

- **本地模式**:桌面「设置」页配置——API Key + 接口地址 + 模型名,支持 OpenAI 兼容与 Anthropic 兼容。可为不同环节(角色提取 / 知识蒸馏 / 叙事各环节 / 去 AI 味等)分别配置模型与采样参数。
- **平台后端**:世界按钉住版本从 `model_routes` / `prompt_versions` 表解析模型路由与 prompt(管理后台「模型与 Prompt 治理」配置)。**无模型配置时 tick 安全 no-op 跳过**,不会崩。
- 引擎对模型配置无状态——凭据随请求/世界配置传入。

---

## 7. 冒烟验证

全绿基线(**校验于 2026-07-29（第三轮：chapterOrder 产出 / 气运机缘量化显示 / 道具接口预留 / 烙印进决策上下文）**,数字随开发增长——对不上先确认是新增测试还是漏跑):

> ⚠️ **2026-07-29 有两次反向变动,记在这里免得被误判成「测试跑挂了」**:
> ① 服务端从 1137 降到 1100 —— **`memorial` 整块删除**带走了它自己的 1475 行测试
> (传世卡 · 遗作馆,见 VALIDATION §3.61),同批新增 4 条「一卡一世界」用例;
> ② 前端 87 文件里少了一条旅程入口断言(九个 → 八个),同样是同一次删除的连带。
> 🔵 **基线往下走同样要说清理由**:一个只会上涨的基线,遇到「变少了」时会被默认当成漏跑。

> 🔴 **这里是全仓唯一维护基线数字的地方。** `CLAUDE.md` 与 `docs/VALIDATION.md` 都已改成
> 指向本节、不再各留一份副本——此前四处各写各的,结果全部过期(CLAUDE.md 一度停在
> 464/542/244,实际已是 853/931/287),而**一个看起来精确却是错的基线比没有更糟**:
> 它会让人把「数字对不上」误判成「我把测试跑挂了」。改这几个数时只改这一处。

```bash
# 引擎 + 后端 + 桌面壳
cargo test --manifest-path crates/muse-engine/Cargo.toml          # 333 passed
# 🔵 真实 provider 冒烟（**恒 #[ignore]，绝不进 CI**）：本仓 2026-07-28 之前一次真实模型调用都没发生过
#    key 只从 env 读，不落盘、不进仓库、不进日志
# MUSE_SMOKE_API_KEY=sk-... MUSE_SMOKE_BASE_URL=https://api.deepseek.com/v1 MUSE_SMOKE_MODEL=deepseek-chat \
#   cargo test --manifest-path crates/muse-engine/Cargo.toml real_provider -- --ignored --nocapture
(cd server && cargo test)                                          # 1162 passed(default,含黄金世界回归)
(cd server && cargo test --features billing,arena)                 # 1246 passed
(cd server && cargo test --features billing)                       # 1207 passed（CI 不跑，2026-07-29 手验）
(cd server && cargo test --features arena)                         # 1236 passed（同上）
(cd server && cargo test golden)                                   # 15 passed(13 项 runtime::golden::* + 2 项录放 round-trip)
cargo test --manifest-path src-tauri/Cargo.toml                    # 245 passed
# 前端 + 后台
npm run test                                                       # 567 passed / 87 files（含后台组件用例，见 VALIDATION §3.47 A5）
npx tsc --noEmit                                                   # 0 错误
(cd admin && npx tsc --noEmit && npm run build)                    # 0 错误 + 产出 dist
```

> CI(`.github/workflows/test.yml`)三个 job 覆盖上述全部:`frontend-test`(前端 test + build)、
> `rust-test`(桌面轨 `src-tauri`)、`platform-test`(引擎 + server 双 feature 组合 + admin 构建 + Postgres 那遍)。
> **`billing`/`arena` 虽默认不进构建,但 CI 单独跑一遍其测试**——feature-gated 代码不进 CI 会在无人察觉中腐化。

### 7.1 Postgres 那遍(生产库形态)

> 🔵 **2026-07-28 第二次实跑记录**：本轮新加的 SQL（健康档三维度 + `UNION` 去重、
> if 线对账）在一次性 PG 16 上跑全量，**新 SQL 一条没挂**；但挂出了**两条只在 PG 上才现形
> 的既有缺陷**（判据写在本地化错误文案上 / `env_guard` 不还原进程级缓存导致全量下 flaky），
> 均已修，PG 全量连跑两遍 1128 passed。详见 `docs/VALIDATION.md` §3.57。
>
> 🔵 **2026-07-28 实跑记录**：本机 PG 上把 CI 的三遍都真跑过（不是静态核对 SQL）——
> `testkit::` schema 层 2 passed、default 1110 passed、`billing,arena` 1193 passed（当时值），
> 与 SQLite 那两遍**逐条一致**。这一轮往 `server` 加了新 SQL（`ifline::sweep` 的对账查询等），
> 那些查询因此是**在真 PG 上验过**的，不只是符合 `$N` + `CAST` 的书面规则。

上面的 server 测试全部跑 `sqlite::memory:`,而**生产跑 Postgres**(`MUSE_DATABASE_URL=postgres://...`)。
建池统一走 `server/src/testkit.rs`,`MUSE_TEST_DATABASE_URL` 决定连哪个库,**不设 = SQLite**,
故上面的命令行为与耗时不变。PG 下每个测试池独占一个 schema(原子计数器取号)实现隔离。

```bash
createdb muse_test
# ✅ 迁移 + schema 隔离
MUSE_TEST_DATABASE_URL=postgres://$USER@localhost:5432/muse_test \
  cargo test --manifest-path server/Cargo.toml 'testkit::'
# ✅ 全量两个 feature 组合(与 SQLite 那遍**通过数相同**——这才是本节要证的事)
MUSE_TEST_DATABASE_URL=postgres://$USER@localhost:5432/muse_test \
  cargo test --manifest-path server/Cargo.toml -- --test-threads=8
MUSE_TEST_DATABASE_URL=postgres://$USER@localhost:5432/muse_test \
  cargo test --manifest-path server/Cargo.toml --features billing,arena -- --test-threads=8
```

> 此处**刻意不再标注通过数**:要证的命题是「PG 与 SQLite **通过数相同**且均为 0 失败」,
> 而不是某个具体数字——后者在 §7 已经维护了一份,这里再写一份就是第二个会过期的副本
> (它确实过期过:一度停在 862/940)。跑完拿这两条的输出与 §7 的数字对一下即可。

> ℹ️ **历史提示已过期,勿再照抄**:本节曾长期写着「全量那遍已知红(251 passed / 525 failed)」,
> 根因是 sqlx 的 `Any` 驱动**原样透传 SQL**、不做 `?` → `$N` 方言改写,而全仓 900+ 条语句写的是
> `?`,PG 上每条带参数的查询都是 42601 语法错。**该项已修完**(调用点全部改为 `$N`),
> CI 的 `platform-test` 也已把 PG 两个 feature 组合设为**阻塞门禁**(`continue-on-error` 已删)。
> 驱动层那条既成事实仍钉在
> `testkit::tests::numbered_placeholders_are_portable_but_question_marks_are_not`。
> ⚠️ 但「全绿」按 §0.3 只到 `Implemented`——排序稳定性与并发正确性的剩余账见
> `docs/VALIDATION.md` §3.3,**CI 绿不等于那些账已清**。
>
> ⚠️ 池默认 `max_connections(1)`(两个库一致,刻意为之)。需要真并发的用例显式走
> `testkit::test_pool_concurrent(n)`,**不要去改默认池**——见 `testkit.rs` 里的理由。
> 跑全量 PG 时给 PG 配足连接数(实测 `--test-threads=8` + `max_connections=400` 够用)。
>
> ⚠️ 同一个 PG 库上不要并行跑两个测试进程(取号器是进程内的,会撞名);需要并行时用
> `MUSE_TEST_SCHEMA_PREFIX` 给各自不同的前缀。
>
> 🔴 **本地反复跑 PG 那遍会把磁盘撑爆——记得定期清数据目录。** 隔离方案是每个测试池独占
> 一个 schema、建池时先 `DROP SCHEMA IF EXISTS` 清同名残留,所以**残留 schema 的数量**上界
> 恒定(= 历史最大并发用例数)。但恒定的是数量,不是**数据目录体积**:一轮全量约 1000 个 schema
> × 66 张表,每张表哪怕只有几十 KB 初始页,几轮下来数据目录就是 GB 级——实测 6 轮累积 **8.1G**,
> 直接把盘写满、导致任何命令都跑不了(连 `df` 都因为要建临时文件而失败)。
> CI 上碰不到这个问题(容器跑完即销毁),**只有本地反复跑才会累积**。
> 跑完记得 `rm -rf` 掉你的 PG 数据目录,或干脆用一次性容器:
>
> ```bash
> docker run --rm -d -p 55432:5432 -e POSTGRES_PASSWORD=x -e LC_MESSAGES=C postgres:16
> ```
>
> ⚠️ `LC_MESSAGES=C` 不是可选项:`testkit` 里钉占位符可移植性的那条断言 grep 的是英文
> `syntax error`,中文 locale 的 PG 会输出「语法错误」导致该用例误红(CI 的 `postgres:16`
> 镜像默认就是 C locale,所以 CI 上不会遇到)。

后端进程端到端冒烟(dev)。⚠️ **这一步不可省，它验的是单元测试验不到的东西**:
迁移在真实进程里跑通、三条 worker 循环真的起得来、路由装配没冲突、CORS 层挂上了。
「cargo test 全绿」只说明代码逻辑对,不说明**这个二进制能起来**——本轮加了 3 个迁移
(`0048`-`0050`)与 3 条 worker 循环,全靠单测验证,直到实际启动才算数(实测通过,
运行期日志除「Dev 桩」那条预期 WARN 外零 error/panic)。

冒烟时值得顺手确认的几处(都是「空值口径」,最容易在真实响应里退化成 `0`):

```bash
# 开关登记表:未接线数应为 0;审核链只解析 global
curl -s :8787/api/admin/flags -H "Authorization: Bearer <admin>" | grep -o '"wired":false' | wc -l
# 写一条该开关不解析的档 → 400(而不是写进去毫无效果)
curl -sX POST :8787/api/admin/flags -H "Authorization: Bearer <admin>" -H 'Content-Type: application/json' \
  -d '{"flag":"MUSE_SAFETY_LEXICON","scope":"world","targetId":"w1","enabled":false,"reason":"试"}'
# 诊断三项的空值:都应是 null 而不是 0
curl -s ":8787/api/admin/worlds/<id>/diagnostics" -H "Authorization: Bearer <admin>" \
  | python3 -m json.tool | grep -E 'startedAt|activatedBy|lastActivityAt'
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

真实外部服务接入位(替换 Dev 实现):短信(`providers::SmsProvider`)、支付(`PaymentProvider`)、TTS(`TtsProvider`)、直播礼物网关(`livegate`)。

**内容审核(`ModerationProvider`)已不再需要写代码**:`providers::http_moderation::HttpModerationProvider`
把 endpoint / 认证 / 请求体 / 响应字段映射全部做成了环境变量,填配置即可适配阿里云内容安全 /
腾讯云 / 百度 / 自建服务等任意 HTTP JSON 审核 API。

> 🔴 **但「provider 写好了」不等于「内容安全已就绪」。** 截至本次交付,它**没有被任何真实
> 服务商账号验证过**(状态语言七档:`Implemented`,未到 `Validated`)。未配置时装配侧仍保留
> Dev 桩,`is_dev_stub()` 为 `true`,运营面 `GET /admin/safety/recheck` 的 `honesty[]` 会明说
> 这条链拦不住任何东西。配上真实服务商后这些字段自动翻面——**以那组字段为准,不以文档为准**。

### 8.1 工程侧上线门(代码动作,与 §8 的运营动作并列)

§8 列的是**运营/资质**动作。以下是**工程侧**必须完成或必须知情的,来自 2026-07-27 那一轮
排查——它们此前散落在十几条提交信息里,到上线那天没人翻得全,故在此汇总。

**必须配置**(不配即错,且错法各不相同):

| 项 | 不配的后果 |
|---|---|
| `MUSE_DEV=0` | dev-login 开放、验证码打日志、审核直通 |
| `MUSE_JWT_SECRET=<强随机>` | 默认值是公开的 `dev-secret-change-me` |
| `MUSE_DATABASE_URL=postgres://…` | 内存库,重启即空 |
| `MUSE_CORS_ORIGINS=<线上域名>` | **前端在浏览器里一个接口都调不通**(只放行本地开发来源) |
| `MUSE_MODERATION_HTTP_*` | 内容审核仍是 Dev 桩,§15 第 3 层拦不住任何东西 |
| `MUSE_LIVEGATE_SECRET`(若开 arena) | 礼物 webhook 验签 fail-closed |

**必须知情的已知限制**(不是配置能解决的):

1. 🔴 **Postgres 生产路径从未在真实部署下跑过。** 两个 feature 组合的测试在 PG 上全绿
   (占位符与 `SUM()` 返回 `numeric` 两类根因已修完),但那只证明**SQL 可移植**;
   连接池行为、超时、迁移锁、故障恢复**零验证**。按 §0.3 是 `Implemented`,不是 `Production-ready`。
2. ⚠️ **`MemQueue` 不持久**——四个 topic **各有各的补偿,不是只有一条**。
   （🔴 **2026-07-29 订正**：本条原先只写了第 3 层那条 `MUSE_SAFETY_RECHECK_SWEEP`,
   读起来像「MemQueue 丢包只有一条默认关的补救」。逐条核过之后那是**低估了现状**——
   而一条会被误读的声明和一条过期的声明一样危险:它会让人去补一个已经补过的洞,
   或者反过来,以为整条链都像第 3 层那样默认关着。）

   | topic | 补偿 | 默认 | 回看上限 |
   |---|---|---|---|
   | `world_tick` | `scheduler_loop`(`runtime`,无条件 spawn):超时 `running` 回收成 `pending` + 滞留 `pending` 重投 | **常开,无开关** | 无 |
   | `notify` | `spawn_outbox_worker` 的 `rescan_pending`:每 60s 重扫全部 `status='pending' AND due_at<=now` | **常开,无开关** | **无**(判据是 DB 行状态,比持久队列还强) |
   | `ifline_advance` | `MUSE_IFLINE_ADVANCE_SWEEP`(迁移 0052) | 关 | 有补投封顶 |
   | `safety_semantic_recheck` | `MUSE_SAFETY_RECHECK_SWEEP` | 关 | **24h 硬上限** |

   下面三条讲的是**最后那一行**（第 3 层那条）。它补的是**下游**:按
   `world_ticks ⋈ safety_recheck_runs` 对账,把「没有终局复核行」的拍补投回去。
   它同时覆盖持久队列覆盖不了的「压根没入队」——包括一类此前无人登记的漏:
   tick 走 blocked / cas_conflict 收尾时**根本没执行到入队那一行**。

   🔵 **顺带把「要不要接 Redis」这件事的结论记下来,免得下一轮再查一遍**:
   逐条核过后**现在不该接**,三条理由——① `Queue` trait 的 `push`/`pop`
   **没有错误通道**(返回 `()` / `String`),Redis 的网络失败只能被吞,
   等于用一个**新的、跨网络的静默丢弃**换掉一个已被上表四条对账覆盖的进程内丢弃,方向是负的;
   ② 这条链上最常见的失败是「压根没入队」(开关当时关着 / 走了没有入队那行的分支 /
   `push_json` 序列化失败被吞),**队列换成什么都关不掉**;
   ③ `docker-compose.yml` 里那个 redis 是裸镜像、AOF 关着,照它接上去会得到一个
   「看起来接了持久队列、实际仍丢最后一个快照窗口」的系统——一个**假的安全声明**,比不接更糟。
   真要接,前置条件是先给 trait 加错误通道、给 compose 开 AOF、给 CI 加 redis service。

   上线要知道三件事:
   - 🔴 **默认关**。第 3 层开着、轮询关着 = 复核会跑,但丢掉的拍没人捡。这条链**三个开关**
     (provider 配置 / `MUSE_SAFETY_SEMANTIC_RECHECK` / `MUSE_SAFETY_RECHECK_SWEEP`)都得按。
   - 🔴 **回看窗口是硬上限**(`MUSE_SAFETY_L3_SWEEP_LOOKBACK_MS`,默认 24h)。挂机超过这段的拍
     **永远补不回来**。`GET /api/admin/safety/recheck` 的 `durability.justOutsideWindow > 0`
     就是「这个值配短了」的直接证据——它现算,不看轮询自己的记账,所以轮询死了它照样会涨。
   - 🔴 **单实例假设**。多实例同开会重复补投,重复的 provider 调用是真烧的(同第 3 条,发布纪律)。
3. 🔴 **多实例滚动发布期间 `world_events.sequence` 仍可能撞号**:发号器(迁移 `0043`)解决的是
   同版本并发,而「旧实例继续往已登记世界写」是**发布纪律**问题,迁移解决不了。
   撞号的后果是 WS 断线补偿会永久漏掉一条事件。
4. ✅ **排序稳定性:约 30 处已全部处置**(`docs/VALIDATION.md` §3.3),此处**不是**遗留项——
   本条初稿误写成「仍有未修项」,是照着中途报告写的,特此订正。
   唯一保留未改的是 `auth/mod.rs:304` 的 `sms_challenges`「最新那条」,那是**有意的第 3 类处置**
   (不补假确定性):该表无单调列、`id` 是 uuid v4,补键只会把「不稳定的任意」变成「稳定的任意」,
   语义上仍不是「最新那条」;并列的真正来源是限频检查的 TOCTOU,属**写入侧**问题,
   两条候选修法都要迁移且会在存量重复数据上失败,而整条短信通道本就是 Dev 桩不可上线。
   🔴 **在改写入侧之前,不得给它补 `id DESC` 充数。**
5. ⚠️ **`card_json` 读取面闸门默认关闭**(`MUSE_DISPOSAL_NAME_GATE`)。开着才会让被下架的卡
   在存量世界的 roster/传记里显示占位名;关着时下架只断得掉「进新世界」与立绘下发。
   开不开是产品决策(它改变运行中世界里每个玩家看到的内容)。
6. ⚠️ **功能开关一律默认关**,按 `docs/VALIDATION.md` §2 的 T0-T5 逐层开闸,
   不要一次全开——清单以 `flags::KNOWN_FLAGS` 为准。

**上线前应当跑一遍**:

```bash
# 1. 在真实 PG 上跑全量(不是内存 SQLite)
MUSE_TEST_DATABASE_URL=postgres://…  cargo test --manifest-path server/Cargo.toml
MUSE_TEST_DATABASE_URL=postgres://…  cargo test --manifest-path server/Cargo.toml --features billing,arena
# 2. 黄金世界回归(换模型 / Prompt / 引擎版本后必跑)
cargo test --manifest-path server/Cargo.toml golden
# 3. 配好审核 provider 后,确认它真的翻面了(而不是仍在用 Dev 桩)
curl -H "Authorization: Bearer <admin>" localhost:8787/api/admin/safety/recheck | grep providerStub
# 4. 确认第 3 层没有在悄悄漏拍(即使还没开轮询,这个数也是真的)
curl -H "Authorization: Bearer <admin>" localhost:8787/api/admin/safety/recheck \
  | python3 -m json.tool | grep -A4 '"durability"'
```

第 3 条是**唯一能证明内容审核真的接上了**的检查:`providerStub` 为 `false`、`source` 为
`production`,且 `honesty[]` 不再说「拦不住任何东西」。**以这组字段为准,不以任何文档为准。**

第 4 条看 `durability`:`enabledWorldTicks` 是**真缺口**(那些世界开着第 3 层却没有终局复核行),
`justOutsideWindow > 0` 说明已经有拍掉出补偿窗口、再也补不回来。这两个数**从数据现算**,
不读轮询自己的记账——所以它们在轮询关着、挂了、压根没部署的情况下同样有效,
可以拿来决定「要不要开这个开关」,而不是开了才知道有没有用。

---

## 9. 已知 seam(明确标注,待接)

> 📄 **每一条的选项、代价与建议见 `docs/build/open-decisions.md`**。那份文档做的事是
> 「让决定变得可以做」——本节只登记**有哪些**,不复述怎么权衡(复述必然与那边漂移)。

- ~~**礼物→LLM 回合真实注入**~~ —— **已定并已实现**(2026-07-28,open-decisions §5 选项 A:
  观众打赏买到的是一个**被看见的机会**,不是任何形式的优势)。
  🔴 上一版本条目里那句「`RoundInput` 现有字段里没有环境事件位」**已经不成立**——
  现在有 `ambient_events`。留着这行订正是因为:**一条会过期的声明,过期之后比没有更糟**
  (它会让人以为还有事没做,或者更糟——以为红线还没被处理过)。
  形态、两道红线与默认关闭见 `docs/VALIDATION.md` §3.41。
- ~~**复活/礼物实际扣费**~~ —— **早已接完,本条是过期声明**(2026-07-27 核准订正)。
  复活走 `arena::revive` 的 `ledger::charge`(P2 就做了,与写 grant 同一事务原子,
  余额不足 409 零副作用);站内打赏走 `livegate` 的 `ledger::charge`(world→模板作者分成、
  自打赏防刷);外部 webhook **刻意不站内二次扣费**(观众已在直播平台付过,那是红线)。
- **placement 房同意触发源**:赛事房淘汰处已补同意门控;placement 房的死亡/永久关系
  **没有触发源**——不是门控缺失,是引擎侧还没有产生这类事件的叙事条件。待叙事迭代。
- **L2/L3 视觉呈现**:当前 L0 文字流 + L1 结构化卡片(事件卡/关系图谱/状态面板);
  立绘/切片为 DevProvider 占位。⚠️ 它**等的是凭据不是决定**,同真实审核服务商。
- **创作者结算**:与用户钱包是两套账,本期只做用户侧。🔴 若涉及真实提现,
  它本身就是对「无提现」这条**平台红线的修改**,必须走显式评审。
- ~~**模型备用路由**~~ —— **已定并已实现**(2026-07-27,migration `0051`)。
  `routes_json` 可声明单个 `fallback` profile;**只在传输层失败时回退**
  (内容错误绝不回退——那会给「模型持续输出坏 JSON」装一个成本放大器);
  成本相加,回退次数落 `world_ticks.fallback_used`。做成 `ModelClient` 包装器,引擎一行没改。
  ⚠️ 运营面「路由错误率」那一栏仍是 `—`:数据源有了,但分母口径(每拍/每次调用/每世界)
  还要定一次,见 open-decisions §4 尾部。

---

## 10. 文档索引

- `PRODUCT.md` — 产品定位一页纸(指向总规格)
- `docs/API.md` — **平台后端 API 清单**(鉴权级别、feature 门控、admin 角色矩阵。此处曾写「84 条路由」,是本仓库第六处过期计数——路由数以 `app.rs` 为准,本文不复述)
- `docs/design/README.md` — **客户端与管理后台界面设计索引**(设计决策、页面规格、实现映射、验收图)
- `docs/build/open-decisions.md` — **待拍板清单**(每项:现状/缺什么/选项与代价/建议/什么证据能 settle 它)。🔴 它记的是「等决定」不是「等做」——一项定了就该从那里消失、在别处成为规则
- `docs/VALIDATION.md` — **商业验证分阶段计划**(T0-T5、双坐标功能台账、工程三约束、七档状态语言)
- `docs/build/spec-world-ecosystem.md` — **世界生态总规格 v2**(24 条拍板,产品宪法+系统设计+衔接表+路线图,唯一权威产品文档)
- `docs/build/rules-anti-farming.md` — 防刷/反重复收益规则(有效规则)
- `docs/build/spec-subplot-cards.md` — 自定义房装配技术附录(命名空间/四段式种子/缝合;其 UGC 假设已被总规格取代)
- `docs/build/example-idle-skeleton.md` — 软主线 skeleton 创作示例(内容制作参考)
- `CLAUDE.md` — 仓库结构与惯例(给 AI 协作者)

> 历史阶段文档(P0-P6 各期规格/台账/验收/试玩记录)已于 2026-07-25 清理,需要时查 git 历史(提交 `8bdea60` 之前)。
