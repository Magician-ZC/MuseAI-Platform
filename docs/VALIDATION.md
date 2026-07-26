# VALIDATION.md — 商业验证分阶段计划（T0-T5）

> 总原则：**开发完成度决定"能不能打开"，用户数据决定"该不该打开"。**
> 开发按完整规格（`docs/build/spec-world-ecosystem.md`）继续；发布按本文件分阶段开闸。
> 每个阶段测试的是**商业假设**，不是功能正确性（功能正确性由测试套件与黄金世界回归负责）。

## 0. 三条工程约束（绑定所有后续开发）

1. **未验证功能默认关闭**：经 feature flag / 运营开关 / 数据配置启用（现状抓手：cargo feature
   `billing`/`arena`、admin 建房控制、建房参数；**运行时开关体系基础设施已落地**——
   迁移 `0036` + `server/src/flags/`，按用户/世界/全局三作用域灰度 + 时间窗 + 审计留痕，
   env 作为兜底层，详见 §3.1。已接线 `MUSE_ONBOARDING`（参考接线）+ R3 三条新建件
   （`MUSE_OOC_ANNOTATIONS` / `MUSE_IFLINE_PARALLEL` / `MUSE_SOCIAL_IDENTITY_UNLOCK`）
   + 首个由纯 env 迁入的存量开关 `MUSE_OFFPEAK_SCHEDULING`，其余存量开关待逐个迁移。
   ⚠️ 开关个数以 `flags::KNOWN_FLAGS` 为唯一权威，本文不复述计数——历次加开关都漏改过散落各处的数字）。
2. **产品规则参数化**：托梦配额、死亡规则、奖励系数、成员规模、tick 节奏、历练阈值、
   崩塌惩罚系数等一律可配置，禁止写死。
3. **状态语言七档**，文档与台账统一使用：
   `Concept → Specified → Implemented → Integrated → Production-ready → Validated → Enabled`。
   "已实现（Implemented）"永远不得表述为"已经证明值得上线"。

## 1. 红线 vs 可调参数

- **真红线（锁进所有测试，不随验证改变）**：不卖胜负与数值平权 · 资产单一写入路径与全链审计 ·
  公共事实不可回滚 · 未成年人保护 · 无提现 · AI 生成标识（显式 + 元数据隐式，范围含导出，
  按《AI 生成合成内容标识办法》补全，当前仅显式字段——状态 Implemented 而非 Production-ready）。
- **待验证的默认策略（可调）**：每阶段 3 次托梦、永久死亡/生死契约、同源唯一、三层结算系数、
  世界人数、更新节奏、星级门槛、创作者分成比例、卡位阈值——全部是参数，不是承诺。

## 2. 阶段计划（每阶段：商业假设 → 开放范围 → 预注册门槛 → 继续/调整/停止）

> 门槛数值为初始提案，**开测前可修订、开测后冻结**（预注册纪律：数据出来后不许移动球门）。

### T0 · 核心魔法 —— 价值假设
- **商业问题**：用户看到"角色在我不在场时活了一天"之后，是否相信它活着、并想继续看？
  （这是全部商业模式的地基：没有魔法时刻，后面一切付费理由都不存在。）
- **开放范围**：单人微本 + 1-2 个精制世界；邀请制 ≤100 人；ICP = 18+ 重度 OC/AI 跑团/互动小说用户。
- **暂不验证**：经济、UGC、直播、多人、死亡。
- **门槛**：10 分钟内完成首个微本 ≥70%；完成微本后主动开启第二段 ≥40%；
  "角色像活的"主观评分（5 分制）≥4.0。
- **预案**：不达 → 调整首小时体验/叙事质量（黄金世界回归定位问题），不推翻世界系统；
  连续两批 <50% 完成率 → 停下重做 onboarding，冻结一切平台开发投入。

### T1 · 个人留存 —— 回访假设
- **商业问题**：异步人生（离线推进 + 日报）是否构成真实的回访理由？这是订阅模式的前提。
- **开放范围**：连载场单人/双人世界；日报 + 关键节点推送。
- **暂不验证**：百人世界、交易、赛事。
- **门槛**：第一份日报打开率 ≥50%；激活用户 D7 ≥25%；同一角色进入第二阶段 ≥30%；
  OOC/裁决不公申诉 <10%/阶段；单阶段可变成本 ≤ 预期收入的 20-25%。
- **预案**：日报打不开 → 改关键节点推送/短视频式回顾；OOC 超标 → 人设保险提前实装；
  **二阶段进入率与 D7 双双不成立 → 回退"本地单人持久角色产品"，停止多边平台建设**。
- **测量手段（2026-07-26 补齐）**：「OOC/裁决不公申诉 <10%/阶段」此前**无从判定**——
  全仓没有任何叙事质量申诉表。R3「OOC 注解权」（总规格 §7 人设保险第 2 级，迁移 0037）
  落地后，该门槛由 SLO `oocAppealRate` 直接给出，口径见 §4.2。
  ⚠️ 开测前**必须先把 `MUSE_OOC_ANNOTATIONS` 打开**（默认关闭）：入口没开时该指标返回
  `entry_not_open`（显示 `—`）而非 0%，正是为了防止把「没测过」误读成「没人申诉」而误判通过。

### T2 · 小群体 —— 群体价值假设
- **商业问题**：他人的存在是否让我的角色人生更有价值？（网络效应前提：公平感、
  关系叙事、"我们的角色共历"是否成立。）
- **开放范围**：3-6 人世界；关系图谱观演；同意制契约。
- **暂不验证**：大规模并发、生死状、真人社交解锁。
- **门槛**：多人世界完成率不低于单人世界的 80%；"我的角色仍然重要"主观评分 ≥3.8；
  叙事注意力公平（每角色有效戏份分布的基尼系数 ≤0.35）；因他人角色行为产生的差评 <15%。
- **预案**：曝光不公平 → 缩小规模/分组叙事（参数）；社交摩擦 → 强化角色面具默认。

### T3 · 经济 —— 付费假设
- **商业问题**：用户愿意为什么付钱？（假设：容量与体验——并发世界数、更新频率、
  上下文/档案容量、模型质量——而非胜负与道具。）
- **开放范围**：订阅制灰度（首选）；打赏在观演场景小流量灰度。
- **暂不验证**：创作者提现（永不承诺）、充值道具、赛事打赏规模化。
- **门槛**：激活用户付费转化 ≥5%；首月续订 ≥60%；付费用户 D30 ≥40%；
  ARPPU ≥ 3× 单用户月度模型成本；"花钱了还是输"类客诉 <5%。
- **预案**：转化不足 → 调整订阅分层而非加卖道具；成本失衡 → 成本工程四杠杆加码 + 节奏降档。

### T4 · 平台生态 —— 供给假设
- **商业问题**：外部供给（自定义房、副本卡消费、分成激励）能否在没有提现的前提下自转？
- **开放范围**：自定义房 + 副本卡消费闭环；排行榜/热门；创作激励（平台内权益）。
- **暂不验证**：——（此阶段起全量在测）。
- **门槛**：周活创作者/周活用户 ≥3%；自定义房占总局数 ≥20%；创作者 M2 留存 ≥30%；
  UGC 世界完成率 ≥ 官方世界的 70%。
- **预案**：供给不足 → 转官方精选 + B2B IP 合作，收缩 UGC 叙事。

### T5 · 规模化 —— 单位经济学假设
- **商业问题**：百人场、直播观演、高并发下，边际成本、审核成本与打赏/订阅收入是否成立？
- **开放范围**：50-100 人世界；直播场 + 弹幕；生死状（成年 + 二次确认）。
- **门槛**：百人场"我的角色仍然重要"评分不低于 T2 基线的 90%；直播场观众→玩家转化 ≥2%；
  内容审核成本 ≤ 生成成本的 5%；平台毛利为正。
- **预案**：百人存在感崩塌 → 世界规模梯度收缩为产品形态（10-20 人为主力）；
  审核成本失控 → 直播延迟拍数上调 + 公开投影降频。

## 3. 双坐标功能台账（初始版，随每次发布更新）

| 功能 | 开发状态 | 验证状态 | 默认开关 |
|---|---|---|---|
| 世界引擎推演（决策/仲裁/写作/关系演化） | Integrated | **黄金世界回归已建（仅管线层）** | T0 起开 |
| 单人微本 + 新手礼包 | Specified | T0 待测 | T0 开 |
| 日报召回 / 关键节点推送 | Implemented / Specified | T1 待测 | T1 开 |
| 托梦（配额参数化） | Implemented（配额 Specified） | T1 待测 | 灰度 |
| 历练 / 卡位 / 星级 / 准入 | Implemented | T1-T2 待测 | T1 开（参数保守） |
| 3-6 人世界 / 关系图谱观演 | Integrated | T2 待测 | T2 开 |
| 经济（打赏/开房/账本/分成） | Implemented（Dev 支付桩） | T3 待测 | 关闭 |
| 申诉复审 / 运行时内容安全 | Implemented / Specified(五层漏斗) | T2 起随开 | 审核链恒开 |
| 生死契约（三档参数化） | Specified | T5 待测 | 关闭（默认庇护/同意制） |
| 副本卡 + 自定义房装配 | Specified（R2） | T4 待测 | 关闭 |
| 赛事直播 / 弹幕 | Implemented（观战）/ Specified（直播场） | T5 待测 | 关闭 |
| 真人社交解锁 | **Implemented**（0040；解锁门槛 / 拉黑 / 举报队列 / 青少年服务端拒绝 / 「一起死过」凭证） | T4+ 待测 | 关闭（`MUSE_SOCIAL_IDENTITY_UNLOCK`） |
| 人设保险三级出口（事前底线 / 事中注解权 / 事后 if 线） | 事前 engine Implemented · 事中 **Implemented**（0037）· 事后 **Implemented（开局 + 推进）**（0039/0041） | T1 起（注解权是 T1 门槛的测量手段）/ if 线 T3 待测 | 三级**全部默认关闭** |
| 内容中台工业线 | Concept | — | — |

> 🔴 **平台生产数据库（Postgres）当前不可用 —— 2026-07-27 本地实测（PostgreSQL 17.5）**
>
> `CLAUDE.md` 与 `docs/STARTUP.md` 写的「SQLite dev / Postgres prod」**在查询层不成立**。
> 根因：`sqlx` 的 `Any` 驱动**原样透传 SQL 字符串、不做 `?` → `$N` 方言改写**
> （`sqlx-core-0.8.6/src/any/connection/executor.rs` 把 `query.sql()` 直接交给 `PgConnection`），
> 而全仓 900+ 条语句写的是 `?`。于是 PG 上**每一条带参数的查询**都是 `42601` 语法错。
> 实测 525/599 条用例失败，数据库错误码分布 **100% 是 `42601`**，无第二种。
>
> - **schema 层是通的**：39 份迁移（`0001-0041`，`0023`/`0028` 是有意空号）在 PG 上逐条通过、
>   建出 66 张表——「双库可移植 SQL 子集」在建表这一层零发现，且已由 CI 阻塞门禁锁住。
> - **查询层是 broken 的**：`MUSE_DATABASE_URL=postgres://...` 从未真正跑通过一次。
>   那 251 个 PASS 全是不碰 DB 的纯函数用例。
> - ⚠️ **这个 bug 遮住了其余所有可移植性问题**——类型强制、布尔/整数混用、`CAST`、
>   排序稳定性，一个都还没机会暴露，因为执行根本走不到那一步。占位符改完后 PG 全量再跑，
>   **预期会暴露第二批真问题**，不要把「改完占位符」当作「PG 可用」。
>
> 修法已实证：`$N` 两个库都认（PG 原生；SQLite 把 `$1` 当具名参数、按首次出现顺序派号），
> 约束是**严格顺序编号且不复用编号**，`.bind()` 的位置绑定即对得上——这条钉在
> `testkit::tests::numbered_placeholders_are_portable_but_question_marks_are_not`，两库都绿。
>
> **在收口之前，平台轨不具备 Postgres 上线条件。** 按 §0.3 状态语言，
> 「Postgres prod」目前是 `Specified`，**不是** `Implemented`。

### 3.1 R1 批次台账（总规格 §19 地基批，**校验于 2026-07-26**）

上表按产品能力分组，漏掉了 R1 的多数工程项。以下按路线图批次补齐——**每项都标了代码锚点，
状态可当场复核**。

> 🔴 **读这张表前先读状态语言**（§0.3）：下表大量出现 `Implemented`。
> **`Implemented` 只表示「代码写完、测试全绿」**，既不是 `Production-ready`，更不是 `Validated`。
> 全部 R1 项**未经任何真实用户数据验证**，开放与否一律按 §2 的 T0-T5 逐层开闸，
> 不得因为这张表变绿就表述为"可上线"。

| R1 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| 同源卡同世界唯一（提取源指纹 + join 校验） | **Implemented** | 迁移 `0021`；发布物化 `assets/mod.rs`（`source_identity`）；join 校验 `worlds/mod.rs`（星级准入之后、防自刷之前）。仅拦 `pristine=1` 原味卡，**编辑过的卡与无指纹老卡一律放行** | 恒开（拦截即产品规则） |
| 托梦配额（每卡每阶段 N 条） | **Implemented** | `interventions/mod.rs` `dream_quota_per_stage()`（env `MUSE_DREAM_QUOTA_PER_STAGE`，默认 3）；迁移 `0022` 索引。计数覆盖 `status IN ('accepted','applied')`——**只数 accepted 会让托梦被消费后额度白送回来** | 恒开（配额可调） |
| 生死契约三档（join 签署 + 引擎分派） | **engine Implemented / server 未接线** | 引擎 `narrative/types.rs` `Lethality` + `mod.rs` `apply_lethality`（写作前降级，保证正文与事件同口径）+ `gate_consents` 生死状放行。**server 侧 `worlds.lethality` 列、join 签署、未成年门、runtime 回灌全部未做**，runtime 恒传默认档 | **关闭**（恒为同意制，生死状档不对任何世界生效） |
| 身份池进采样域 | **Implemented（分配层）/ 叙事未接线** | `assembly/mod.rs` `IdentitySpec` + `DOMAIN_IDENTITY=0x57` + `assign_identities`，结果钉进 `assembled_json` 的 `/assembly/identityAssignments`。**runtime 尚未读回**，身份目前只存不用、叙事层无效果 | 未启用（模板不声明 identityPool 即零影响） |
| 确定性产出表 + ③世界线层贡献归因 | 见本表下方补记 | 迁移 `0025`；贡献归因表独立于 `narrative_state_json`（回灌引擎会违反平权红线） | — |
| 成本仪表 | **Implemented** | `admin_api/dashboards.rs` `cost.*`（今日/趋势/每局/每玩家）；`/admin/worlds` 补 `participantCount`·`successRate`·`todayCostCny`；diagnostics 补金额与用量比。**分摊口径为人均等分**（world_ticks 是整拍口径，无 per-member 分解），局限已写进接口 `notes`。**2026-07-26 补记（R3 成本工程杠杆①）**：迁移 `0038` 给 `world_ticks` 加 `off_peak`·`price_ratio_pct`·`defer_ms` 三列，由 `runtime/mod.rs` 的错峰调度器逐拍写入——成本从此可按「折扣时段 / 原价时段」拆桶，「省了多少」= `Σ cost_tokens × (100-price_ratio_pct)/100 × 单价`，「错峰生效了多少」= `off_peak=1` 占比与 `Σ defer_ms`。**2026-07-26 再补记**：`dashboards.rs` **已接这三列**——`cost.offPeak`（拍/token 占比、估算折让、延后时长、按名义档位分桶）挂在成本趋势那条已有的窗口查询上，未新开路由、未新增迁移、未多发一次 SQL；`cost.trend[]` 逐日拆出 `offPeakTokens`。🔴 单位陷阱已锁进用例：`priceRatioPct` 是**百分数整数**（100=原价）、`priceRatio` 是 0..1 小数，两者同时下发且不得互串。⚠️ **if 线开销尚未并入**（`ifline_beats.cost_tokens` 走独立端点 `GET /api/admin/iflines/cost`，接入 SQL 与索引均已就绪）。**2026-07-27 口径修正（#42）**：被内容安全闸/硬约束**阻断的那一拍此前记 `cost_tokens=0`**，而引擎当时已跑完整个回合（导演/决策/仲裁全部烧过 token）——成本因此系统性低估，且越是阻断多的世界低估越重，T3/T5 会在最危险的地方最乐观。现由 `runtime::finish_tick_blocked` 记实测 token 并累计进 `world_budgets`（口径与提交拍、if 线逐字一致）；一次模型都没调过的空转拍仍记 0。叙事 SLO 的拍域另加 `error IS NULL`（`slo::TICK_DOMAIN`）与成本口径分家，阻断拍进成本、不进「无戏份」分母 | 恒开（只读聚合）；错峰写入侧**默认关闭** |
| 错峰调度（成本工程杠杆①） | **Implemented** | 总规格 §17【拍板 16】。`runtime/mod.rs` 的 `offpeak` 模块 + `schedule_due_ticks` 接入：连载/慢炖场的 tick 优先排进折扣时段，窗口内按窗口占全天比例**压缩间隔以保住每日拍数**（不是节奏降档）；🔴 直播场（`room_type='arena'` ∨ `tick_per_day ≥ MUSE_OFFPEAK_LIVE_TICK_PER_DAY`）永不延后；🔴 防饿死兜底 `interval + min(interval×200%, 6h)` 恒有限、首拍绝不延后；折扣时段内按「被压最久」优先入队。时区口径与 `dashboards::utc_day_start_ms` 同源（UTC，窗口字面量解析期一次性折算）。参数与列口径见 `docs/API.md` §3「错峰调度」 | **关闭**（`MUSE_OFFPEAK_SCHEDULING` 默认 0）。**2026-07-26 补记**：已登记进 `KNOWN_FLAGS`，成为**首个由纯 env 迁入开关体系的存量开关**——解析链升为 user > world > global > env > 默认，错峰从「全局一刀切」变为可按世界灰度。`runtime` 侧一行未改（`offpeak::enabled_for_world` 早已写好「已登记走体系、未登记退 env」的分支） |
| Batch API（成本工程杠杆③） | **Specified（未实现）** | 约 5 折，但与现有同步 tick 管线结构性冲突：`run_round` 是**串行**五环节 + 同事务 `commit_tick`，而 Batch 是分钟~小时级异步；一拍需 5 次批往返、`CLAIM_STALE_MS=300000` 会把等批 worker 判成崩溃重排、中间态无持久化（批途中重启 = 半通管线，违反 §5「宁可停拍」）。改造路径：`crates/muse-engine` 把 `run_round` 改成可挂起/可恢复的分步状态机 + `ModelClient` 增 `submit_batch`/`poll_batch`（默认实现回落同步 `complete`，桌面轨零改动），server 侧加中间态表 + 批次协调器 + 降级回落。完整分析见 `server/src/runtime/mod.rs` 的 `offpeak` 模块头 | — （未实现，无开关） |
| 运行时敏感词库 + 语义分类钩 | **第 2 层 Implemented / 第 3 层仅接口位** | `safety/lexicon.rs`（复用 `inject.rs` 归一化管线，零宽/同形/全角绕过均被拦）+ `runtime` commit 事务内闸 + `events`/`reports`/`clips`/`arena` 全部读取面过滤。**第 3 层语义分类未实装**（不能进事务，见 `safety/mod.rs` TODO） | 恒开（审核链） |
| Saga 归组字段（saga_id + stage_no） | **Implemented** | 迁移 `0024`；`admin_api/worlds_ops.rs` 建模板成对校验 + `?sagaId=` 阶段列表（按 stage_no 升序、不分页） | 未启用（不填即独立模板） |

**⚠️ 本表初版漏掉的一项（2026-07-26 补记）**：§0.1 原文写着「运行时开关体系**列入 R1 开发**」，
但它既不在总规格 §19 的 R1 清单里，也不在本表初版中——**两边都漏了**。
**基础设施层已于 2026-07-26 落地**（状态由 `Specified` 推进到 `Implemented`），下表已更新。

| R1 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| 运行时开关体系（基础设施 + 1 条参考接线） | **Implemented** | 迁移 `0036`（`runtime_flags`：开关名/作用域/目标/开关位/时间窗/修改人/修改时刻/理由，唯一键 `flag+scope+target_id`）；统一读取入口 `flags/mod.rs` `is_enabled(db, name, ctx)`，解析链 **user > world > global > env > 代码内默认值**；运营面 `admin_api/flags.rs`（GET/POST `/admin/flags`、GET `/admin/flags/resolve` dry-run、DELETE `/admin/flags/{id}`，**写 admin 专属**，全部落 `audit_logs`）。**参考接线仅 `MUSE_ONBOARDING`**（按用户灰度 = T0「邀请制 ≤100 人」的执行手段） | 恒开（体系本身）；**表为空 = 全部现存开关行为逐字不变** |

> **为什么必须做**：T0-T5 每个阶段都要求「开放范围」可控（如 T0「邀请制 ≤100 人」、
> T3「订阅制灰度」），而 env 开关只有全开/全关两态，**做不到分阶段开闸**。
> 换句话说，验证计划本身依赖这套体系。
>
> 🔴 **引入它没有打开任何东西**（本项最大的风险点，已锁进测试）：迁移 0036 **不插种子数据**，
> `enabled` 列 `DEFAULT 0`，登记表里除审核链外默认值全为 `false`；表为空时解析必然回落 env，
> 于是 9 个现存开关行为零变化。红线用例：`flags::tests::red_line_empty_db_and_no_env_means_disabled`
> / `red_line_only_safety_chain_defaults_on` / `red_line_migration_seeds_no_rows`。
>
> 🔴 **fail-closed**：查库失败 / 记录损坏（作用域非法、`enabled` 非 0-1、时间窗反转/为负）→
> 返回声明的默认值**且不再回落 env**（否则「配坏了」会被静默降级成「按 env 开着」）。
> 「安全」指不扩大用户可见范围的那一侧：未验证开关一律是**关**，`MUSE_SAFETY_LEXICON`
> （审核链，关掉 = 放行敏感词）是**开**。用例 `red_line_corrupt_records_fail_closed`
> / `red_line_query_failure_fails_closed` / `red_line_safety_chain_fails_safe_to_on`。
>
> **与 `prompt_versions.canary_world_ids` 的口径关系**：后者（迁移 0001 就有）是现成的
> 按世界灰度先例，至今**只写不读**、无任何消费方。本体系的 world 作用域与它**口径一致**——
> 都以 `worlds.id` 为灰度单元、都是白名单式「命中即生效」、都**不建外键**（灰度名单是运营意图
> 的记录，级联删除会静默改变开放范围）。**不一致处只在存储形态**：内联 JSON 数组无法承载
> 「谁改的/何时/为什么/时间窗」，也无法按世界索引查询（要全表扫 + JSON 解析），故本体系改为
> 独立行 + 唯一索引。本批次**不改 `prompt_versions`**（给它接消费方会改变引擎选 prompt 的行为，
> 属另一件事）；将来接线时应复用 `runtime_flags` 而不是再造第三套灰度。

**其余 8 个 env 开关的迁移清单**（尚未接线，`wired=false`；逐条注意事项见
`server/src/flags/mod.rs` 的 `MIGRATION_NOTES` 文档注释）：
`MUSE_SUBPLOT_CARDS` · `MUSE_LETHALITY_DEATHMATCH` · `MUSE_ROOM_INVITATIONS` ·
`MUSE_CONTAINER_ASSEMBLY` · `MUSE_MEMORIAL` · `MUSE_WORLD_SERIES_AUTOSCALE` ·
`MUSE_WORLD_BE_BIOGRAPHY` · `MUSE_SAFETY_LEXICON`（🔴 最后迁或不迁）。
共同的坑：**多数消费点在事务内**（结算铸卡 / 封卷 / 扩容判定），`is_enabled` 会查库，
须在进事务前解析一次再把 bool 传进去，否则 SQLite 单连接池自锁。
**一次只迁一个、各自带回归用例**——批量改必然出错，这也是本批次只接一条线的原因。

**仍缺的接线（不在上表，单独跟踪）**：生死契约 server 侧全部 · 身份池叙事回灌 ·
第 3 层语义分类 · 机审耗时打点（`moderationLatency` 全仓无数据源，后台该列恒为 `—`）·
**其余 8 个 env 开关接入运行时开关体系**（清单与注意事项见上）。

> **2026-07-27 订正（读上表前必看）**：上表「身份池进采样域」一行与紧邻的「仍缺的接线」句
> 都写着**身份池叙事未接线 / runtime 尚未读回**，这两处**已过时**——`a962e9a`（R1 收尾）
> 落地了 `runtime::load_identity_display_names`，读回路径现为
> `assembled_json./assembly/identityAssignments` → 他人 brief `唐三（户部主事）` +
> 本人 `RoundInput.self_identities` → 引擎 prompt 上下文，**无开关、恒生效**
> （`runtime/mod.rs:2096` 与 `:2330` 两个调用点）。
> 同理「生死契约三档」一行的「server 未接线」也已过时：`worlds.lethality` 列、join 签署、
> 未成年门、runtime 回灌均已落地（见 `worlds::tests::lethality::*`），**仍关闭的是开关
> `MUSE_LETHALITY_DEATHMATCH`，不是代码**。
>
> 身份池现在的准确分层（人工校准面 `effect` 段以此为准，见 `docs/API.md` §7「人工校准面」）：
> **分配层 Implemented · 叙事感知层 Implemented · 数值层设计上永不生效（§0.1 平权红线）·
> 校准闭环缺失**。最后一项是真缺口：全仓没有任何指标度量「身份池调整 → 戏份分布变化」——
> SLO 的叙事注意力基尼按 `character_id` 聚合，与身份 id 无关，所以运营调完身份池
> **看不到因果**，只能看到分配结果本身。把身份维接进 `slo/` 是独立一步，本批次未做。

### 3.1.1 人工校准面（阶段切分 + 身份池两维，**落地于 2026-07-27**）

§4 末尾登记的「仍未做：人工校准面」中，**阶段切分与身份池两维已落地为只读运营视图**；
**境界档仍未做**，原因不变（`Skeleton` 无字段落点，须先补 schema，会改动装配产物与黄金世界快照）。

| 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| 阶段切分校准视图 | **Implemented（只可视化，不可编辑）** | `server/src/admin_api/calibration.rs` `list_sagas` / `saga_detail`；页面 `admin/src/pages/Calibration.tsx`。诊断项：缺号（**从 1 起算**，故「缺开篇」也报）/ 重号 / 未编号 / 审核态分布 / 星级跨度 / 骨架形状指标 / 世界实例数 | 恒开（只读聚合，同 dashboards） |
| 身份池校准视图 | **Implemented（只可视化，不可编辑）** | 同文件 `list_identity_pools` / `template_identity_pool`。给出池声明、逐身份分配人次 / 覆盖世界 / 填充率、从未被分配的站位、模板已删除的残留身份、在场无站位角色数、集中度基尼（复用 `slo::gini_coefficient`） | 恒开（只读聚合） |
| 境界档校准视图 | **Concept** | 无。`Skeleton` 里没有境界档字段，无数据可展示 | — |

🔴 三条必须一起读的限定：

1. **只可视化，不可编辑**。四个端点无写入、无副作用、不落 `audit_logs`；校准参数的唯一写入路径
   仍是建模板（`POST /admin/world-templates` 的 `sagaId`/`stageNo`/`skeletonJson.identityPool`）。
   响应恒带 `editable:false` + `editPath`，页面直接渲染该字段而非写死文案。
   **本批次不含任何在线调参**，故不需要新开关（§0.1 约束的是写入面）。
2. **身份池视图不构成效果验证**。它回答「分配成了什么样、是否失衡」，**不回答「这样分配更好」**——
   见上方订正框的「校准闭环缺失」。页面把这四层状态放在分布图**之前**显式渲染，正是为了防止
   运营把分布图误读成调参有效的证据。
3. **`Implemented` ≠ 可上线**。视图口径未经真实运营数据检验；缺号/失衡的**阈值**目前没有产品定义
   （页面只呈现事实，不给「合格/不合格」判语）。

### 3.2 R3 批次台账 —— 人设保险三级出口（总规格 §7，**校验于 2026-07-26**）

> 三级出口是同一句话的三种兑现方式：**公共事实不可回滚（§0.3）**，
> 所以玩家买到的从来不是「改写」，而是事前的保护、事中的解释权、事后的**平行线**。

| R3 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| ① 事前 · 底线硬约束 | **engine Implemented** | 卡的 `bottomLines`/`refusalRules`/`immutableCore` 进 critic 底线拦截（`crates/muse-engine`）；server 侧无独立开关 | 随引擎恒开 |
| ② 事中 · OOC 注解权 | **Implemented** | 迁移 `0037`；`server/src/annotations/`；SLO 消费端 `slo::ooc_appeal_block`。世界事实不改，私人传记加批注；复核确认模型错误 → 补偿托梦配额（**加数表**，`interventions` 的计数 SQL 一个字符不改）。兑现补偿**已接线**（`interventions/mod.rs` 配额校验处比较的是 `dream_quota_per_stage() + bonus`，计数 SQL 未动） | **关闭**（`MUSE_OOC_ANNOTATIONS`）⚠️ T1 开测前必须先打开，否则 `oocAppealRate` 返回 `entry_not_open` 而非 0% |
| ③ 事后 · if 线付费副本 | **Implemented（立项与开局 + 推进）** | 迁移 `0039`（`ifline_worlds`）+ **`0041`（`ifline_beats` + 推进态列）**；`server/src/ifline/`（`mod.rs` 立项 · `runner.rs` 推进）；端点见 `docs/API.md` §4「if 线付费副本」。交付面 = 校验 → 烧副本卡 → **逐字节冻结分叉态** → 注册独立实例 → **一拍一拍推进（`sealed → running → ended`）** → 可读可审。终局产物 = 可读的私人传记 + 结局名，**不是资产** | **关闭**（`MUSE_IFLINE_PARALLEL`） |

**③ 的三条结构性红线（都已锁进测试，改动需显式评审）**：

1. 🔴 **if 线不是一行 `worlds`**。一行 `worlds` + `world_members` 会被
   `runtime::commit_tick → end_world_tx → finalize_ending_tx` 自动带进
   `progression::settle_idle_world_ending_tx`（发历练）/ `subplot::settle_subplot_card_tx`（铸卡）/
   `arena_rewards`——历练是准入与卡位解锁的钥匙，于是「花钱开 if 线」立刻等于「花钱买数值」，
   踩穿 §0.1「付费只买体验容量，永不买结果」。放进独立表后那条反哺路径**物理上不存在**。
   用例：`red_line_ifline_is_not_a_world_row` · `red_line_ifline_grants_nothing_back_to_origin`。
2. 🔴 **原世界逐字节不变**。开 if 线前后对 11 张世界线/资产表做逐字节快照比对
   （`red_line_opening_ifline_leaves_worldline_byte_identical`），另有源码级
   `red_line_never_writes_worldline`（本模块对世界线表只有 SELECT）。
3. 🔴 **单人平行线（§14）**。冻结前剥离他人玩家角色（条目 + 关系边 + `knownTo` 引用三处一并清），
   NPC 保留；剥离台账对玩家可见。传世卡不得进 if 线（§12「不可再入世界」，否则 = 付费复活）。
   **推进时每拍再剥一次**（纵深防御三层：组阵容剔除 · 跑前过 `freeze_snapshot` · 逐拍台账落
   `ifline_beats.cast_json`）。用例 `red_line_foreign_players_never_enter_beat_cast`。
4. 🔴 **终局绝不进结算管线**（0041 接线时的头号红线）。`progression::settle_*` /
   `subplot::settle_subplot_card_tx` / `arena_rewards` 一条都不进。推进走 `ifline::runner::commit_beat`，
   与 `runtime::commit_tick` 零交叉；if 线的拍落 `ifline_beats`，**不是 `world_ticks`**。
   用例：运行时 `red_line_ifline_ending_grants_nothing`（跑到终局后 `SUM(mileage)` /
   `subplot_cards` 行数 / `backpacks` / `arena_rewards` / `world_contributions` / `world_ticks` 行数
   **全部零变化**）+ 源码级 `red_line_runner_never_enters_settlement`。
   🔴 **改动此处前先读**：接线时最容易走错的一步是为复用 `process_tick_inner` 而把 if 线塞回
   `worlds`/`world_ticks`——tick 管线与结算管线是**连体的**（CAS 成功即评估终局、终局即结算），
   没有「跑但不结算」的开关可拨。
5. 🔴 **推进后原世界仍逐字节不变**。跑拍会产状态/事件/critic，一个字节都不许流回世界线。
   用例 `red_line_advancing_leaves_worldline_byte_identical`（跑两拍后对 11 张表逐字节比对）。
6. 🔴 **冻结的分叉态永不被推进覆盖**。`snapshot_json` 是分叉点证据，活态另存 `live_state_json`；
   覆盖了 `stateFidelity` 就成了一句无法证伪的话。用例 `snapshot_stays_frozen_while_live_state_advances`。

**⚠️ ③ 的诚实降级（必须写进台账，不能只留在代码注释里）**：规格写「以**某拍**为分叉点」，
但仓库**不存逐拍状态快照**——`world_ticks` 无状态列、`worlds.narrative_state_json` 每拍被 CAS 覆盖、
`world_events` 只是投影文本且引擎 `StatePatch` 从不落库（事件流无法重放中间态）、引擎 FS 只是当前态的物化。
因此本实现**只支持终局分叉**（`fork_point='terminal'`，状态源 = 已 `ended` 世界的 `narrative_state_json`
逐字节复制），**请求中间拍一律 400 并点名唯一可用的终局拍号**，绝不用终局态冒充第 N 拍——
那是在为一个假分叉收费。补齐路径：先加逐拍状态快照表，再扩 `fork_point='tick'`（表结构已留位）。
用例佐证：`red_line_mid_tick_fork_is_rejected_without_touching_resources` ·
`red_line_unsupported_fork_point_kind_is_rejected` · `fork_points_endpoint_declares_the_limitation`。

**③ 的推进（0041）—— 三个必须写进台账的判断**：

- **成本记在哪**：if 线跑拍烧 token 但**不能写 `world_ticks`**（写进去就等于接回那条自动结算链路）。
  故 `ifline_beats.cost_tokens`（逐拍实测，共用 `runtime::TokenMeter`，与 `world_ticks.cost_tokens`
  口径逐字一致故可比）+ `ifline_worlds.cost_tokens_total`（实例累计，两处互为对账）+ 运营端点
  `GET /api/admin/iflines/cost`。⚠️ **现状**：`admin_api::dashboards` 的主成本看板**尚未并入 if 线开销**
  （只 SUM `world_ticks.cost_tokens`）——本批次未动 `dashboards.rs`（并行批次在改）。
  接入是一句 SQL（索引 `idx_ifline_beats_created` 已建好）：
  `SELECT SUM(cost_tokens) FROM ifline_beats WHERE created_at >= ? AND created_at < ?`。
  这件事同时写在 `/api/admin/iflines/cost` 响应的 `dashboardIntegration` 字段里，不靠人记。
- **SLO 归属：不并入世界线 SLO**。五项指标度量的是**多人世界线**：基尼（单人样本恒为满分 →
  **稀释真实的多人不公平，让指标失去报警能力**）/ 无戏份率（单人线结构上不可能有人没戏份）/
  二次入世率（if 线没有「入世」这件事）/ 收尾率（if 线常由拍数上限强制收尾，与「叙事弧完成」
  不是同一件事）——**四项全部排除**。仅「状态-文本矛盾」同质，故逐拍存 `ifline_beats.critic_json`
  供将来做**独立**读数，不并进世界线池子。工程上本就默认排除（`slo/` 取数口径是
  `world_ticks.status='done' AND cost_tokens>0`），本批次是把这个默认变成**有意的决定并写下来**。
  本批次不动 `slo/`。用例 `ifline_beats_never_enter_worldline_slo_input`。
- **终局产物只能是内容**：`ifline_beats.prose` 按拍序拼成的私人传记 + `endingReason`/`endingLabel`
  两个字符串。读取面恒带 `isContentOnly:true` / `grantedAssets:[]`；审计 `ifline.ended` 的 reason 里
  明写 `grantedAssets=none|settlementEntered=none|worldlineChanged=false`。
  🔴 if 线里主角「死了」**不会封卷传世卡**（封卷是 `UPDATE cloud_characters`，属被禁写入）——
  既不能复活（0039 已挡传世卡入场），也不会杀死你在真实世界线的卡。

**⚠️ ③ 的遗留（0041 未做，需在生产化前解决）**：

1. **推进端点在请求内同步调用模型**，长回合 = 长连接请求。生产化应改为「入队 + 后台 worker +
   轮询/推送」（`queue` 模块已具备该能力）。本批次未做的原因：if 线默认关闭、状态只标到
   `Implemented`，加一条独立 worker 循环会显著放大改动面且需单独评审。
2. **主成本看板未并入 if 线开销**（见上，接入方式与索引均已就绪）。
3. **分叉点仍只支持终局**（0039 的诚实降级，未变；补齐路径见上）。

if 线现在是「可开、可跑、可读、可审、可查成本」，但仍**不是「可上线」**——
**状态语言按 §0.3：`Implemented`，不是 `Production-ready`，更不是 `Validated`**。

> **纪律提醒**：本节存在的意义是 §4.3 那条"发布评审以台账为准，禁止口头'已完成'"。
> 台账漏项 = 评审失去依据，与状态写错同等严重。改 R1 相关代码时同步改本表。

## 4. 验证基建三件套（优先于新增功能）

1. **黄金世界回归**：一个公版/原创标准样板（固定角色卡 + 世界模板 + 20-30 个关键剧情测试点，
   覆盖正常结局/BE/死亡/托梦/多人关系）；每次换模型/Prompt/引擎版本重跑，对比 OOC、
   剧情重复、角色曝光公平、成本变化。

   **技术底座（2026-07-26 核实，勘误：此前写作「现有 showcase/scenario 工装」——
   该工装在工作树与 git 全历史均零命中，从未存在。它在总规格 §4 内容中台流水线里是
   `Concept` 状态的待建项，被误转述为现成依赖）**：
   - ✅ **确定性采样**（已有）：种子 `H(world_id‖阵容指纹‖template_version)`
     `assembly/mod.rs`，跨版本测试向量 `prng_test_vectors`，禁三样（系统随机/浮点 RNG/map 迭代序）
   - ✅ **mock 注入的 tick 联编**（已有）：`process_tick_with_model` + `runtime/tests.rs`
     的全套播种助手与环节感知假模型，40+ 测试已验证
   - ✅ **逐字节比对范式**（已有）：`degradation_is_deterministic_across_runs`、`run_confluence_scenario`
   - 🟡 **回放式 ModelClient**（唯一真新建件，2026-07-26 建成，状态 `Implemented`）：
     `crates/muse-engine/src/replay/`——`RecordingClient`（包装任意 `ModelClient`，入参/出参落盘，
     凭据不入录）· `ReplayClient`（**结构上没有 inner 字段**，未命中返回 `NotFound` 而非静默回落真实模型）·
     `diff_recordings`（两份录制对齐到「哪一拍 · 哪个角色 · 哪个环节」再给字段级差异）。
     `ModelClient` trait **零改动**（纯包装，两个宿主无需同步）。
     🔴 交付的是**能回答「换了模型角色还是不是它自己」的工具**，不是那个问题的**答案**——
     它至今**未接线**到黄金世界回归/仿真工装（要动 `runtime/mod.rs`），也**没有**任何真实模型录制入库，
     故本条**不得**被读作「角色一致性已验证」
   - ⚠️ **落点受限**：`server` 是 binary-only crate（无 `lib.rs`），`server/tests/` 与独立 bin
     都访问不到 `pub(crate)` 的 runtime/assembly API——回归只能建成 `#[cfg(test)]` 子模块，
     好处是自动进现有 `platform-test` CI job，CI 改动为零
   - ⚠️ **两条须先拍板**：生产写作温度 0.8 且是硬编码字面量 → prose 无法逐字比对，
     建议只比对结构化产物（decisions/outcomes/patch/events/contributions/cost，这些已能逐字节相等）；
     world_id 进采样种子 → 回归必须钉死 world_id，否则每次跑的是不同副本

   **落地状态（2026-07-26）**：`server/src/runtime/golden.rs`（`#[cfg(test)]` 子模块，
   自动进 `platform-test` CI job，CI 配置零改动）。fixture《长安夜宴》原创虚构、无版权素材，
   3 玩家 + 1 NPC，覆盖五类剧情测试点（正常结局 / 崩塌 BE / 死亡含同意门 / 托梦 / 多人关系）。

   🔴 **它测得了什么、测不了什么（务必分清，不得把"回归全绿"读成"叙事质量有保障"）**：
   - ✅ **测得了「管线不回归」**：确定性装配、种子回灌、决策定序、规则层 R1-R6 与模型仲裁升级条件、
     关系演化、不可逆同意门全链、托梦投喂、状态 CAS / 事件投影 / 内容安全第 2 层 / 预算实测计费、
     三条终局判定与三层结算（含崩塌折算与产出封顶）。同一世界两遍跑出的结构化产物**逐字节相等**。
   - ❌ **测不了「叙事质量」**：OOC（决策来自人写的剧本，按定义永不 OOC）· 剧情重复率与文本质量
     （prose 从未落库 + 写作温度硬编码 0.8）· 换模型的真实成本变化（token 是剧本常数，
     只反映调用构成变化）。补这一栏的前提件 record-and-replay `ModelClient` **已建**
     （`muse_engine::replay`，`Implemented`），但**尚未接进本回归**：黄金世界至今仍只跑人写的剧本。

   **首次运行即抓到一个真问题**：`load_active_cards` 的 `ORDER BY joined_at` 缺次级排序键，
   两成员撞同一毫秒时装配产物顺序在重放间漂移（生产并发 join 同样可能撞）。回归侧已用固定
   `joined_at` 绕开；根治需补次级键，但会改变已有世界的 `assembled_json` 字节，属破坏性变更待评审。
2. **叙事质量 SLO**（正式监控，进运营看板）。**数据可得性核实于 2026-07-26——
   八项里现在有六项算得出来（原为四项；`stateTextContradictionRate` 随迁移 0030、
   `oocAppealRate` 随迁移 0037 先后补齐），仍别把这张表当成现成能力**：

   | 指标 | 现在能算？ | 数据源 / 缺什么 |
   |---|---|---|
   | 每角色有效事件分布（基尼系数） | ✅ | `world_contributions.score_milli`（迁移 0025）就是逐角色有效戏份的权威账本，与引擎 `round_intensity` 总额恒等。**算基尼前须与 `world_members` 取交集**（NPC 也入表）。这是 T2 门槛「基尼 ≤0.35」的直接数据源，**当前无任何代码在算** |
   | 角色连续 N 拍无有效戏份比例 | ✅ | `world_events.actors_json` × `tick_no` 对 `world_members` 全集做差集。注意 `actors_json` 是 JSON 文本，现有查询用 `LIKE '%cid%'`，大规模统计需改规范化解析 |
   | 强制收尾率 | ✅ | `terminal_reason()` 产 `mainline_complete`/`time_cap`/`starved`，落 `world_ticks.error` 与 `audit_logs('world.ended')`。强制 = time_cap/time_limit/starved，自然 = mainline_complete |
   | 同角色二次入世率 | ✅ | `world_members` 唯一索引 `(world_id, cloud_character_id)` + `joined_at`，`COUNT(DISTINCT world_id) >= 2` |
   | 状态-文本矛盾率 | ✅（2026-07-26 补齐） | `world_tick_critic`（迁移 0030）落 `CriticReport` 三类问题的计数列 + 完整 `report_json`。**每个已提交 tick 恒落一行（哪怕三列全空）**——行的存在本身就是分母；只在有问题时才写，「跑了但干净」与「从未落库」在库里长得一样，分母永远算不准。写在 commit 事务内：崩在 commit 之后、写 critic 之前，得到的不是「少一条数据」而是「一个看起来很干净的 tick」，比缺数据更坏 |
   | 剧情重复率 | ✅（2026-07-26 补齐） | `event_summary` 取值表从 3 键扩到 6 键，补 `consequence`/`purpose`/`action`。**只取一个字段、不拼接**——两段模型文本拼一起会把同一件事的措辞重复计入，正是本指标最怕的噪声源。新增文本走原有内容安全闸（在闸之前进投影，有红线用例验证无旁路） |
   | 用户跳过/退出率 | ⚠️ 半（退出已补齐） | `world_members.left_at`（迁移 0030）在 leave 时写入、复活时清 NULL（否则会出现 `left_at < joined_at` 的自相矛盾行）。**历史行不回填**（NULL = 退出时刻未知，不是没退出）→ 统计留存曲线须显式排除 `status <> 'active' AND left_at IS NULL` 的盲区行。「跳过」仍无埋点 |
   | OOC 申诉率 | ✅（2026-07-26 补齐） | `ooc_appeals`（迁移 0037，R3「OOC 注解权」= 总规格 §7 人设保险第 2 级）。此前是八项里**唯一未解**的一项——`moderation_appeals` 是**内容风控申诉**（只允许 rejected 的卡/头像、每主体终身一次），与「角色演得不像/裁决不公」零关系，**不得拿来充数**；现已按「唯一真新建件」的判断真的新建。口径：**分母** = 窗口内演过戏（`world_ticks.status='done' AND cost_tokens>0`）的世界 × 其 `world_members` 行（= 「角色×阶段」对，阶段口径同托梦配额「一个 world 实例 = 一个阶段」）；**分子** = 窗口内申诉按 `(worldId, characterId)` 去重后的对数，并施加与分母相同的两个 EXISTS，故 分子 ≤ 分母 恒成立。🔴 **三态必须分得开**：`entry_not_open`（入口从未开放 → `—`）/ `no_data_in_window`（零样本 → `—`）/ `ok`（真数，可以是 0%）——本功能**默认关闭**，若直接报 0% 会得到「看起来棒极了、实际上什么都没测」的数，而 T1 恰恰要拿它决定继续/调整/停止。另出 `confirmedRate`（坐实率 = 确认模型错误 / 已复核）：**申诉率 ≠ 坐实率**，前者是「多少人不满」，后者是「其中多少确实是模型的错」 |

   > 还有一处同类浪费未处理：`ModelCallLog` 的分环节 token（agent/model_id/latency/retries）
   > 被 `TokenMeter` 求和成标量即丢弃，导致成本做不了「decide 换便宜模型省了多少」的分环节归因。
3. **台账维护**：每次合并/发布更新 §3 表；发布评审以台账为准，禁止口头"已完成"。

### 4.4 世界质量仿真回归（新增于 2026-07-26 · 状态 `Implemented`）

总规格 §4「内容中台工业线」第四道工序「仿真试跑（自动化压测）」+ 增量投入项
「仿真质检自动化（世界质量回归：完读率/阻断率/结局分布）」的第一版落地。

**落点**（均不在生产路径上，无路由、无迁移、无 feature flag）：

| 件 | 位置 | 性质 |
|---|---|---|
| 仿真试跑工装 | `server/src/runtime/simulation.rs` | `#[cfg(test)]`，自动进 `platform-test` CI job |
| 三指标口径 | `server/src/slo/quality.rs` | 生产代码（`pub(crate)`，暂无路由消费方），与 §4.2 同源 |
| 跨版本基线 | `server/src/runtime/simulation/baseline.json` | 随代码入库，`include_str!` 内联 |

**与 §4.1 黄金世界回归的分工**：golden 抓「某条固定路径的产物字节变了」；本件抓
「一整批世界的统计形状变了」。共用同一份 fixture（`golden/cards.json` + `golden/skeleton.json`）、
同一条生产路径（`process_tick_with_model`）、同一套分类口径（`crate::slo`）——不另起一套。

**三指标口径**（分子/分母的圈法是新增的，分类算法一律复用 §4.2）：

| 指标 | 分子 | 分母 | 关键纪律 |
|---|---|---|---|
| **完读率** `completionRate` | 自然收尾（`mainline_complete`）且 `worlds.status='ended'` 的世界 | **纳入本批的全部世界，含未收尾** | 🔴 **完读率 ≠ 1 − 强制收尾率**。§4.2 `forcedConclusionRate` 的分母只含 `status='ended'`，「跑不完」的世界在它眼里根本不存在；90 个卡半路 + 10 个自然收尾 ⇒ 强制收尾率 0%（看着完美）、完读率 10%（真相）。两个数都输出以便对账，**不得混用** |
| **阻断率** `blockedRate` | `world_ticks.error='blocked'`（引擎跑完整回合但拒绝提交） | **真正跑完整回合**的拍 = 提交 + 阻断 | 终局短路 / 前置门 / failed / 未终结拍**一律不进分母**（进去只会稀释），但**照样报出来**——排除不等于隐藏。内容安全扣留（`world_events.moderation<>'approved'`）是**独立第二通道**，分开报，合成一个数就再也分不清「世界卡住了」和「世界在说不该说的话」 |
| **结局分布** `endingDistribution` | 按 `audit_logs('world.ended').reason` 的 `\|ending=` 后缀分桶 | — | 三态分得开：真实结局 / `(none)` 收尾无结局 / `(unfinished)` 未收尾，**均不丢弃**。集中度复用 `slo::gini_coefficient`（与叙事注意力基尼同一实现） |

🔴 **诚实边界（不得省略转述）**：仿真跑的是**种子驱动的规则化假模型**，全程不调用任何真实模型。

- 完读率在此测的是**主线推进 + 终局判定管线在各种决策组合下能否走到自然收尾**，**不是**内容好不好看。
  跑出 100% 完读率只说明管线不卡死，**不得**表述为「内容质量已验证」。
- 阻断率在此测的是**规则层（底线/硬节点/不变量）的触发频率**，不是模型输出是否合规。
- 结局分布在此测的是**加权采样与终局判定的分布形状**，不是玩家会喜欢哪个结局。
- 内容安全扣留率在桩下**恒为 0**：桩文本永不命中词库 —— 该通道**只是被计算了，并没有被测试**。

这段话不只在文档里：`slo::quality::QualitySource::SimulatedStub` 让它作为 `honesty` 字段随每一份
报告 JSON 一起走（**数会被复制进评审材料，文档不会**）。补齐内容质量成分的前置件是 §4.1 标注的
record-and-replay `ModelClient`：**工具已建**（`muse_engine::replay`，2026-07-26，`Implemented`），
**但仿真仍全程跑桩**——本节的三指标口径与「诚实划界」一字未变，接线之前不得据此谈内容质量。

**当前基线**（`museai-sim-2026-07-26` 种子 · 4 场景 · 10 个世界 · 引擎 0.1.0）：
完读 6 / 强制收尾 2（`time_limit`）/ 未收尾 2 ⇒ 完读率 60%；阻断 8 拍 / 引擎拍 57 ⇒ 阻断率 14%；
结局落桶 `e_purge` × 4 + `e_alliance` × 2 + `e_silence` × 2 + `(unfinished)` × 2（distinctEndings = 3）。
四个场景刻意各压一个失败面
（`cordial` 完读正样本 · `volatile` 高冲突 · `attrition` 强制收尾正样本 · `deadlock` 阻断正样本），
以保证三个指标**都不会恒定**——一个恒为 100% 的完读率和恒为 0 的阻断率测不出任何回归。

**基线更新纪律**：确认差异是预期的产品/引擎变化后，
`MUSEAI_SIM_UPDATE_BASELINE=1 cargo test --manifest-path server/Cargo.toml simulation_` 重写基线，
**必须与改动同一提交**，且提交信息写清「为什么这三个数应该变」——无解释就被刷新的基线等于没有基线。

**本件首次运行即抓到一个生产缺陷，现已修复**（任务 #41，状态 `Implemented`）：**结局选择不进实例采样**。
旧行为：`weight_endings` 无 RNG（按权重过阈值筛，返回池声明序子集），`runtime::select_ending` 取
`enabledEndings` 的**第一个元素**、不掷点、不看 `instance_seed` ⇒ 同模板同阵容下所有实例落同一个结局，
总规格 §5「一个模板，千个平行世界」在结局维上不成立（首版基线 `e_alliance` × 8、distinctEndings = 1
即此缺陷的读数）。修法：掷点收在**装配层**——`assembly::weight_endings_scored` 把权重连同名单一起交出，
`assembly::pick_ending` 在既有 `Rng(instance_seed ^ DOMAIN_ENDING)` 子流（不新开域，`0x5C` 仍空）上
按权重抽一个，钉进 `/assembly/selectedEnding` 随实例不变；`runtime::select_ending` 只读它，
缺该键才回退旧口径首个（**已钉住的老实例不因一次代码变更改写结局**）。权重语义不变：未启用、
零权（含负 / NaN）的结局永不被选中，有 `assembly` 层专项用例锁。守护用例
`runtime::simulation::ending_selection_varies_with_instance_seed` +
`standard_suite_metrics_have_discriminating_power` 的 `distinctEndings >= 2` 断言。
🔴 状态止于 `Implemented`：分布形状出自种子驱动的假模型，**不度量内容质量**，
「玩家会不会觉得结局分得开」仍未验证。

**仍未做**：人工校准面（阶段切分/身份池/境界档的 admin 可视化与调参）。
其中**境界档在 `Skeleton` 里没有任何字段落点**（`identity_pool` 有、`payout_table` 有、境界档没有），
故校准面在补上 schema 之前无数据可展示——这是它的真前置件，不是 UI 工作量问题。

> **2026-07-27 更新**：上段「仍未做」已部分兑现——**阶段切分与身份池两维**的只读校准视图已落地
> （`server/src/admin_api/calibration.rs` + `admin/src/pages/Calibration.tsx`，台账见 §3.1.1）。
> **境界档仍未做，原因与上段完全一致**（无 schema 落点）。
> 另注意上段说的是「可视化**与调参**」，本次**只做了可视化，没做调参**：四个端点全只读，
> 校准参数的写入路径仍是建模板。

## 5. AI 失败安全降级（写入门槛，因公共事实不可回滚）

模型超时/审核阻断/低质输出时，按序：延后此拍 → 使用过审过渡事件 → 缩短上下文重仲裁 →
暂停世界并说明原因 → 人工复核队列。**宁可停拍，不让失败输出成为永久公共事实。**

## 6. 不可逆投入冻结清单（验证通过前不做）

大规模版权采购 · 长期直播平台合作合同 · 大规模人工审核团队 · 创作者结算与税务系统 ·
大规模算力预付 · 对外承诺赛事/经济上线日期。（代码可复用，合同与人力不可回收。）

## 7. 本地端获客飞轮

本地创建角色 → 长期聊天积累人设 → **一键发布云端副本（字段级上传选择权，上传前逐字段展示）**
→ 进入公共世界 → 经历写回本地角色档案。本地隐私承诺不变（见 README「双轨定位」）。
