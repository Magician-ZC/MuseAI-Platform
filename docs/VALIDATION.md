# VALIDATION.md — 商业验证分阶段计划（T0-T5）

> 总原则：**开发完成度决定"能不能打开"，用户数据决定"该不该打开"。**
> 开发按完整规格（`docs/build/spec-world-ecosystem.md`）继续；发布按本文件分阶段开闸。
> 每个阶段测试的是**商业假设**，不是功能正确性（功能正确性由测试套件与黄金世界回归负责）。

## 0. 三条工程约束（绑定所有后续开发）

1. **未验证功能默认关闭**：经 feature flag / 运营开关 / 数据配置启用（现状抓手：cargo feature
   `billing`/`arena`、admin 建房控制、建房参数；**运行时开关体系基础设施已落地**——
   迁移 `0036` + `server/src/flags/`，按用户/世界/全局三作用域灰度 + 时间窗 + 审计留痕，
   env 作为兜底层，详见 §3.1。已接线 `MUSE_ONBOARDING`（参考接线）+ R3 四条新建件
   （`MUSE_OOC_ANNOTATIONS` / `MUSE_IFLINE_PARALLEL` / `MUSE_SOCIAL_IDENTITY_UNLOCK` /
   `MUSE_LIVE_STAGE`）+ 首个由纯 env 迁入的存量开关 `MUSE_OFFPEAK_SCHEDULING`
   + 被处置内容的卡名读取面闸门 `MUSE_DISPOSAL_NAME_GATE`，
   其余存量开关待逐个迁移。
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
| 已过审内容处置（再审 / 下架 / 恢复） | **Implemented**（0044；四类主体的展示态处置 + 两档可逆性 + 处置台账 + 举报队列跳转落地。2026-07-27 补：位图主体的**再审通道**已接上——`check_image` 入队 + `audit::review` 两条位图回写分支，补的是 0027 注释里记着的那处缺口；人审详情附图，否则只能盲审） | T2 起随开 | **恒开**（合规设施，见 §3.1 末段） |
| 被处置内容的卡名读取面闸门 | **Implemented**（`safety/disposal.rs`；roster / 遗作馆四处 / 社交 / 邀请 / 同源冲突文案共 8 处解引用点。🔴 `world_events` 与已封卷传记快照一个字节不动——闸门停的是「现读现解」，不是回溯改写。替代文本 `暂不可见的角色·<hex>`，前缀参数化 `MUSE_DISPOSAL_DISPLAY_NAME`）| 未定（产品决策） | **关闭**（`MUSE_DISPOSAL_NAME_GATE`）。⚠️ 与上一行的「恒开」**不冲突**：那是处置能力，这是改变运行中世界显示的产品决策，见 §3.1 末段的补记 |
| 处置申诉（被下架的作者的救济路径） | **Implemented**（0045 `disposal_appeals`；作者提交 `POST /assets/characters/{id}/disposal-appeal`、后台队列 + 裁决。改判**复用 `restore` 那一段实现**，不直接写 `approved`。🔴 每**次处置**一次而非每主体一次——恢复后再被下架的作者重新获得申诉权）| T2 起随开 | **恒开**（同为合规设施：一个能被关掉的申诉入口等于没有申诉入口）|
| 生死契约（三档参数化） | Specified | T5 待测 | 关闭（默认庇护/同意制） |
| 副本卡 + 自定义房装配 | Specified（R2） | T4 待测 | 关闭 |
| 赛事直播 / 弹幕 | Implemented（观战）/ **Implemented**（直播场：0042；定档 + 延迟缓冲 + 弹幕，见 §3.4） | T5 待测 | 关闭（`MUSE_LIVE_STAGE`）⚠️ T5 开测前必须先打开，否则 `liveStage` 返回 `entry_not_open` 而非 0% |
| 真人社交解锁 | **Implemented**（0040；解锁门槛 / 拉黑 / 举报队列 / 青少年服务端拒绝 / 「一起死过」凭证） | T4+ 待测 | 关闭（`MUSE_SOCIAL_IDENTITY_UNLOCK`） |
| 人设保险三级出口（事前底线 / 事中注解权 / 事后 if 线） | 事前 engine Implemented · 事中 **Implemented**（0037）· 事后 **Implemented（开局 + 推进）**（0039/0041） | T1 起（注解权是 T1 门槛的测量手段）/ if 线 T3 待测 | 三级**全部默认关闭** |
| 内容中台工业线 | Concept | — | — |

> 🟡 **平台生产数据库（Postgres）测试全量通过 —— 2026-07-27 本地实测（PostgreSQL 16.9）**
>
> 上一版这里是红框「PG 当前不可用」。两条根因**均已收口**，全量用例在 PG 上两个 feature
> 组合都绿，且与 SQLite 逐条同数：
>
> | 组合 | SQLite | Postgres |
> |---|---|---|
> | 默认 features | 784 passed | **784 passed / 0 failed** |
> | `--features billing,arena` | 862 passed | **862 passed / 0 failed** |
>
> ⚠️ 上表是 PG 收口当时（`c5af27b` 前后）的实测值。同一天稍晚 record-and-replay 接线
> 又新增 14 个用例，计数变为 **798 / 876**（PG 侧一并复测仍为 0 failed）。
> **这里刻意不再追平数字**——用例数每批都变，钉死它只会制造下一次的过期。
> 要点是「两个 feature 组合在 PG 上零失败、且与 SQLite 逐条同数」这个**结论**，
> 当前值以 `docs/STARTUP.md` §7 为准。
>
> 两条根因（按发现顺序）：
>
> 1. **占位符方言**（已修）：`sqlx` 的 `Any` 驱动**原样透传 SQL 字符串、不做 `?` → `$N` 改写**
>    （`sqlx-core-0.8.6/src/any/connection/executor.rs` 把 `query.sql()` 直接交给 `PgConnection`），
>    而全仓 900+ 条语句写的是 `?` ⇒ PG 上每条带参查询都是 `42601`。修法：全仓改 `$N`——两库都认
>    （PG 原生；SQLite 把 `$1` 当具名参数、按首次出现顺序派号），约束是**严格顺序编号且不复用编号**。
>    钉在 `testkit::tests::numbered_placeholders_are_portable_but_question_marks_are_not`。
> 2. **`SUM()` 解码**（已修）：PG 下 `SUM(bigint)` 返回 `numeric`，`Any` 驱动不认该类型
>    （`Any driver does not support the Postgres type Numeric`）；SQLite 直接给整数。
>    修法：**投影出来的** `SUM` 一律 `CAST(... AS BIGINT)`（`delta_cents` 等均为 BIGINT，
>    其和为整数值，narrowing 无损；真溢出会报错而非静默截断）。仅出现在 `HAVING` / `WHERE`
>    比较中的 `SUM` 不需要 CAST——不解码到 Rust 侧。
>
>    ⚠️ 这一条是①**遮住**的：占位符 bug 让执行走不到解码那步。生产侧的投影 SUM 早在成本看板
>    那批就已全部 CAST，故 default 那遍先绿；`billing,arena` 独有的 `ledger/` `billing/` `shop/`
>    `arena/` `livegate/` 全是金额聚合模块，从未在 PG 上跑过，是这一条的唯一暴露面。
>
> **按 §0.3 状态语言，「Postgres prod」= `Implemented`。** 只表示「代码写完、测试全绿」——
> **不是** `Production-ready`，更**不是** `Validated`：
>
> - 从未在真实部署 / 真实并发 / 真实数据量下跑过；连接池、超时、迁移锁、故障恢复零验证。
> - ⚠️ **排序稳定性：清单已修；并发项修掉一项、剩一项**（2026-07-27）。PG 对 `ORDER BY` 的并列行
>   不保证顺序，SQLite 则常按 rowid 稳定返回。审出的约 30 处已按三类处置完毕（见 **§3.3**）。
>   两项**并发正确性**（非排序）问题中，`world_events.sequence` 的 `MAX+1` 分配已由迁移 `0043`
>   的发号器表修掉（PG 上有实测对照，见 §3.3 ①）；`sms_challenges` 的「最新那条」仍在。
>   **CI 全绿仍不能证明剩余项是对的。**
>
> CI（`.github/workflows/test.yml` 的 `platform-test`）已把 PG 全量两个 feature 组合都设为
> **阻塞门禁**（`continue-on-error` 已删除）。

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
| 成本仪表 | **Implemented** | `admin_api/dashboards.rs` `cost.*`（今日/趋势/每局/每玩家）；`/admin/worlds` 补 `participantCount`·`successRate`·`todayCostCny`；diagnostics 补金额与用量比。**分摊口径为人均等分**（world_ticks 是整拍口径，无 per-member 分解），局限已写进接口 `notes`。**2026-07-26 补记（R3 成本工程杠杆①）**：迁移 `0038` 给 `world_ticks` 加 `off_peak`·`price_ratio_pct`·`defer_ms` 三列，由 `runtime/mod.rs` 的错峰调度器逐拍写入——成本从此可按「折扣时段 / 原价时段」拆桶，「省了多少」= `Σ cost_tokens × (100-price_ratio_pct)/100 × 单价`，「错峰生效了多少」= `off_peak=1` 占比与 `Σ defer_ms`。**2026-07-26 再补记**：`dashboards.rs` **已接这三列**——`cost.offPeak`（拍/token 占比、估算折让、延后时长、按名义档位分桶）挂在成本趋势那条已有的窗口查询上，未新开路由、未新增迁移、未多发一次 SQL；`cost.trend[]` 逐日拆出 `offPeakTokens`。🔴 单位陷阱已锁进用例：`priceRatioPct` 是**百分数整数**（100=原价）、`priceRatio` 是 0..1 小数，两者同时下发且不得互串。**2026-07-27 补记（#54）**：**if 线开销已并入**——`cost.ifline`（`allTime` 与 `window` 两个口径 + 拍数）与 `cost.combined`（世界线 + if 线合计）。🔴 **`cost.total` 的语义刻意保持不变**（它一直是世界线口径，改写既有字段含义会让所有历史对账失效），要「平台一共花了多少」看 `combined`。⚠️ 两个口径**不可混加**：`total` 是全时段、`trend`/`offPeak` 是 `?costDays=` 窗口内，故 `ifline` 同时给出两者、`combined` 只用 `allTime`——有一条专门的用例（`cost_includes_ifline_without_rewriting_worldline_total`）钉住这一点，实测把 `combined` 改用 window 会立刻转红。漏记成本比记错更危险：它让单位经济学看起来比实际好，而 T3「ARPPU ≥ 3× 模型成本」与 T5「毛利为正」都直接建在这个数上。**2026-07-27 口径修正（#42）**：被内容安全闸/硬约束**阻断的那一拍此前记 `cost_tokens=0`**，而引擎当时已跑完整个回合（导演/决策/仲裁全部烧过 token）——成本因此系统性低估，且越是阻断多的世界低估越重，T3/T5 会在最危险的地方最乐观。现由 `runtime::finish_tick_blocked` 记实测 token 并累计进 `world_budgets`（口径与提交拍、if 线逐字一致）；一次模型都没调过的空转拍仍记 0。叙事 SLO 的拍域另加 `error IS NULL`（`slo::TICK_DOMAIN`）与成本口径分家，阻断拍进成本、不进「无戏份」分母 | 恒开（只读聚合）；错峰写入侧**默认关闭** |
| 错峰调度（成本工程杠杆①） | **Implemented** | 总规格 §17【拍板 16】。`runtime/mod.rs` 的 `offpeak` 模块 + `schedule_due_ticks` 接入：连载/慢炖场的 tick 优先排进折扣时段，窗口内按窗口占全天比例**压缩间隔以保住每日拍数**（不是节奏降档）；🔴 直播场（`room_type='arena'` ∨ `tick_per_day ≥ MUSE_OFFPEAK_LIVE_TICK_PER_DAY`）永不延后；🔴 防饿死兜底 `interval + min(interval×200%, 6h)` 恒有限、首拍绝不延后；折扣时段内按「被压最久」优先入队。时区口径与 `dashboards::utc_day_start_ms` 同源（UTC，窗口字面量解析期一次性折算）。参数与列口径见 `docs/API.md` §3「错峰调度」 | **关闭**（`MUSE_OFFPEAK_SCHEDULING` 默认 0）。**2026-07-26 补记**：已登记进 `KNOWN_FLAGS`，成为**首个由纯 env 迁入开关体系的存量开关**——解析链升为 user > world > global > env > 默认，错峰从「全局一刀切」变为可按世界灰度。`runtime` 侧一行未改（`offpeak::enabled_for_world` 早已写好「已登记走体系、未登记退 env」的分支） |
| Batch API（成本工程杠杆③） | **Specified（未实现）** | 约 5 折，但与现有同步 tick 管线结构性冲突：`run_round` 是**串行**五环节 + 同事务 `commit_tick`，而 Batch 是分钟~小时级异步；一拍需 5 次批往返、`CLAIM_STALE_MS=300000` 会把等批 worker 判成崩溃重排、中间态无持久化（批途中重启 = 半通管线，违反 §5「宁可停拍」）。改造路径：`crates/muse-engine` 把 `run_round` 改成可挂起/可恢复的分步状态机 + `ModelClient` 增 `submit_batch`/`poll_batch`（默认实现回落同步 `complete`，桌面轨零改动），server 侧加中间态表 + 批次协调器 + 降级回落。完整分析见 `server/src/runtime/mod.rs` 的 `offpeak` 模块头 | — （未实现，无开关） |
| 运行时敏感词库 + 语义分类复核 | **第 2 层 Implemented / 第 3 层 Implemented（管线，非防线）** | `safety/lexicon.rs`（复用 `inject.rs` 归一化管线，零宽/同形/全角绕过均被拦）+ `runtime` commit 事务内闸 + `events`/`reports`/`clips`/`arena` 全部读取面过滤。**2026-07-27 补记（第 3 层落地）**：`safety/semantic/` + 迁移 `0046`。形态与原 `TODO(§15-L3)` 逐字一致——`commit_tick` 在 **`tx.commit()` 之后**入队（独立 topic + 独立 worker 池），事务外跑 `check_text`，非 Approved 时 `UPDATE world_events SET moderation` 从 `approved` **收紧**。🔴 `SET` 只有这一列、`WHERE` 钉着 `approved` ⇒ 正文逐字节不变（§0.3）+ 单向棘轮；🔴 provider 故障**先重试、到顶 fail-closed**（收紧为 pending + 无条件进人审），方向不参数化，与 `MUSE_SAFETY_LEXICON` 的 fail-safe「继续过滤」自洽；公开全量 + 私有确定性抽样（域常量 `0x5C`，禁三样）；留痕与入队一律复用 `safety` 既有入口。🔴 **交付的是管线不是防线**：`ModerationProvider` 唯一实现仍是 Dev 桩，真实语义分类一次都没发生，**不得表述为「五层漏斗已完整」/「内容安全已就绪」**。这个事实随数据走三处：`safety_recheck_runs.provider_stub` · 每条 `risk_events.detail_json` 的 `providerStub` · `GET /admin/safety/recheck` 的 `providerStub`/`source`/`honesty[]` | 第 2 层恒开（审核链）；**第 3 层默认关闭**（`MUSE_SAFETY_SEMANTIC_RECHECK`，按世界灰度）——它从未生效过，默认开启等于让「合并代码」直接改变线上行为并开始烧 token（§0.1）。两者默认值相反不矛盾：默认值一律指向「不改变现状」的那一侧 |
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

**存量 env 开关的迁移清单**（尚未接线的仍是 `wired=false`；逐条注意事项见
`server/src/flags/mod.rs` 的 `MIGRATION_NOTES` 文档注释。⚠️ 此处**刻意不写个数**——
它属于本仓库反复栽跟头的那类计数，权威是 `KNOWN_FLAGS` 里 `wired` 的取值）：

- ~~`MUSE_ROOM_INVITATIONS`~~ —— **已迁（2026-07-27）**。当初判断的「第二容易」成立：四个端点
  统一 404、无事务边界问题。🔵 但迁的时候发现原注漏了一条约束：这个开关**不能有 world
  作用域**。原注只说「ctx 用受邀人的 user_id」，而发件侧路径里是有 world 的，照着写很自然会
  顺手传上——那样运营给某个世界单独开闸就会产出**一封谁都答不了的邀请**（发件侧命中 world
  记录而开，收件侧 `/me/invitations` 跨世界、结构上没有 world，落到 global 而关）。
  两条新用例钉住：按人灰度真的生效 · 写 world 记录时两侧**都不开**（不是开一半）。
- ~~`MUSE_LETHALITY_DEATHMATCH`~~ —— **已迁（2026-07-27）**。原注预判的两处口径差异全部命中：
  读取侧（join 契约门 / 引擎回灌 / 列表详情投影 / if 线）按 **world**，建房前门只能按 **global**
  （建房那一刻世界还不存在）。于是「全局关但某世界开、却建不出新的生死场」确实会发生——
  现已写进 `worlds::deathmatch_enabled` 的 ctx 口径表并有用例钉住，不再是「困惑」而是规则。
  🔵 原注没预判到的一条：**`effective_lethality` 不能改成 async**。它的调用点里有列表投影的
  循环体与引擎回灌，将来还可能被搬进结算事务——一旦它自己会查库，任何一次「顺手挪进事务」
  都会在单连接池上自锁，而那种死锁在只跑内存 SQLite 的用例里不一定复现。故改成**收一个
  已解析好的 bool**：调用点必须先 `.await` 一次 `deathmatch_enabled` 才拿得到它，
  事务边界问题因此在**编译期**摆到眼前。这条对下面剩余几个开关同样适用。
- ~~`MUSE_SUBPLOT_CARDS`~~ —— **已迁（2026-07-27）**。原注两条预判命中：两个消费点语义不同
  （端点 404 / 结算跳过不报错，新手礼包也依赖那个「跳过」），且结算路径在事务内。
  🔵 迁的时候多做了一件事：那个 bool 不是裸参数，而是 **`progression::SettlementFlags`**——
  结算事务里挂着的开关不止一个（还有下面的传世卡自动封卷与 BE 传记），一个一个加 bool，
  `settle_idle_world_ending_tx` 迟早变成七个 bool 的函数。后两个迁的时候往它加字段即可。
  🔵 **「事务里解析会自锁」这条现在有可执行证据了**：`resolving_flags_inside_the_transaction_deadlocks_and_fails_closed`
  故意把解析放进事务，量出「挂满连接获取超时 → fail-closed 回落默认（关）→ 一张卡不铸、
  且不报错」。这不是推演——迁移时我就写错过一次，四个用例同时变红且各跑 30 秒才定位到。
  用例走 `testkit::test_pool_short_acquire(300)`，现象不变而耗时从 30s 降到 0.4s。
- ~~`MUSE_MEMORIAL`~~ —— **已迁（2026-07-27）**。端点 404 且封卷本身不发生；封卷在结算事务内，
  与副本卡共用 `SettlementFlags`——**只往结构体加了一个字段**，没有给结算函数新增 bool 参数，
  这正是上一条建那个结构体的目的。⚠️ 原注那条提醒依然没被违反：`MUSE_MEMORIAL_BOND_MIN` /
  `MUSE_MEMORIAL_PAGE_SIZE` 是参数化 env（非布尔），一个都没往 `runtime_flags` 里塞（§0.2）。
  🔵 迁它时故障注入暴露了 `SettlementFlags` 的一个**结构性弱点**：字段全是 `bool`，
  把 `flags.subplot_cards` 误接到自动封卷上照样编译、且在两个开关取值相同的用例里照样绿。
  已补 `settlement_flag_fields_are_not_crosswired`——故意让两者取值相反，走真实结算入口验。
  下一个往这个结构体加字段的人，请一并扩这条用例。
- ~~`MUSE_WORLD_BE_BIOGRAPHY`~~ —— **已迁（2026-07-27）**。`SettlementFlags` 的第三个字段。
  🔵 它是这批里唯一**两侧 ctx 同档**（都按 world）的：传记是**公共事实**（§0.3）不是个人资产，
  按人灰度会出现「同一份封卷 A 看得见 B 看不见」。于是它没有副本卡/传世卡那种
  「产出了但看不见」的不对称。⚠️「关阀期间崩塌的世界不产传记、再打开也不追溯补写」
  这条语义一个字没变——传记是封卷那一刻的快照。
  🔵 迁它时故障注入连抓出**我自己三条假绿的用例**，都是同一个病根——**用例绕过了真正的
  接线点**：① 按世界灰度那条走 `seal()` 辅助函数，验不到 `settle_idle_world_ending_tx`
  里那一行接的是哪个字段；② 读取侧直接调 `be_biography_enabled(..., Some(id))`，
  对「端点里传的是 world 还是 global」完全无感；③ 字段接串那条用 `collapsed = false`，
  BE 传记那一行根本没被走到。全部改成走真实入口（真实结算函数 / 真实 HTTP 端点 /
  `collapsed = true`）后三处注入才红。**教训：验开关接线，必须从接线点的上游进去。**
- ~~`MUSE_WORLD_SERIES_AUTOSCALE`~~ —— **已迁（2026-07-27）**。原注对「语义重叠」的提醒直接
  决定了结论：它**只接 global 档，不给 world 档**——这是这批里唯一**主动放弃灰度粒度**的一个。
  逐系列的闸已经是 `world_series.status`，再加一档就是第三道容易被忘记的闸；且系列是**一串**
  世界实例，按世界灰度会让它半开半关（1 号能指到 2 号、2 号却开不出 3 号）。
  🔴 **两道闸都开才扩容**，有用例钉住「全局开关不是逐系列闸的旁路」。
  ⚠️ 原注另一句「扩容判定在 join 的事务路径上」**不准确**，已改正：
  `ensure_next_series_instance` 由 `world_full_conflict` 调用，那是撞满员后的 **409 构造路径**，
  join 的事务此时还没开。照着原注去做一次 bool 穿透是白费功夫——**迁移清单本身也会过期，
  动手前先按当前代码复核一遍**。
- 未迁：`MUSE_CONTAINER_ASSEMBLY` · `MUSE_SAFETY_LEXICON`（🔴 最后迁或不迁）。

共同的坑：**多数消费点在事务内**（结算铸卡 / 封卷 / 扩容判定），`is_enabled` 会查库，
须在进事务前解析一次再把 bool 传进去，否则 SQLite 单连接池自锁。
**一次只迁一个、各自带回归用例**——批量改必然出错。

> 🔴 **合规设施不进开关体系**（§0.1 的边界，不是它的例外）。§0.1 管的是「**未验证的产品功能**
> 不得随代码合并自动对用户开放」；**内容安全的处置能力不是产品功能**，它是主体责任要求平台
> 必须具备的能力。已过审内容的再审 / 下架 / 恢复（migration 0044，`admin_api/takedown.rs`）
> 因此**不登记开关、恒开**，定位同 `MUSE_SAFETY_LEXICON`——理由也同构：一个能被关掉的下架入口，
> 在真正需要它的那一刻恰好可能是关的，而那一刻通常没有第二条路。
>
> 它也不构成「未验证功能自动开放」：这组端点**只对后台角色开放**（reviewer / 永久移除 admin 专属），
> 对玩家侧零新增可见能力；被处置内容的作者只多收到一条状态告知。换言之它**收窄**用户可见范围，
> 与 §0.1 要防的方向相反。
>
> ⚠️ **2026-07-27 补记：这条边界不覆盖「被处置内容的卡名读取面闸门」**
> （`MUSE_DISPOSAL_NAME_GATE`，`server/src/safety/disposal.rs`，已登记且**默认关闭**）。
> 两者容易被当成一件事，但性质相反：
>
> | | 处置能力（0044） | 卡名读取面闸门 |
> |---|---|---|
> | 谁受影响 | 只有后台角色多了一个入口 | **运行中世界里的每个玩家**：昨天还在的名字今天变成中性占位 |
> | 方向 | 收窄用户可见范围 | 改变已有玩家看到的内容 |
> | 是什么问题 | 合规主体责任要求平台具备的能力 | 「什么时候开、开了给玩家看什么」——产品决策 |
>
> 所以「处置能力恒开、不登记开关」与「这条闸门登记且默认关闭」不矛盾，是同一条 §0.1 判据
> （会不会改变用户看到的东西）在两件事上的两个结论。工程侧交付的是**能力**：闸门关闭时各读取面
> 输出逐字节维持现状（`red_line_disabled_gate_is_byte_identical_to_today` /
> `red_line_disabled_gate_leaves_the_memorial_hall_byte_identical`），产品决定何时开。

**仍缺的接线（不在上表，单独跟踪）**：生死契约 server 侧全部 · 身份池叙事回灌 ·
~~第 3 层语义分类~~（2026-07-27 已实装管线，见上表；**真实 provider 仍缺** —— 现跑 Dev 桩，
拦截能力为零，接线不等于生效）·
~~机审耗时打点（`moderationLatency` 全仓无数据源）~~ —— **数据源已补**（migration `0049`：
`safety_recheck_runs.provider_ms`，只累加 `check_text` 两端的时钟差，超时/报错的调用照算），
读取面在 `GET /admin/safety/recheck` 的 `providerLatency`。
🔴 **但后台那一列仍恒为 `—`，且短期内应当继续如此**：provider 是 Dev 桩（恒 0 的「审核延迟」
在看板上与「审核非常快」长得一样）· 第 3 层默认关（按世界聚合是一片 null）· 该列只覆盖
运行时投影这条链（静态审核走 `moderate_and_queue` 不落本表，摆进世界列表会被读成该世界的
机审总体 SLA）。⚠️ 同表的 `latency_ms` 是**一次尝试全程**（含 DB 与记账），**不是**那一列的
数据源——拿它除以调用数得到的数系统性偏大，却看起来完全合理 ·
~~**人审对 `world_event` 主体无回写路径**~~ —— **已闭合**（migration `0047`）：
`admin_api::audit::writeback_target` 现在返回回写坐标，`world_events` 上因此有了全仓
**唯一一条放宽语句**（`SET` 只有 `moderation` 一列、按主键点名、CAS、起点白名单写死在 SQL 里），
权限分 reviewer / admin 两档，三条写入路径由源码级红线用例全仓盘点 ·
**其余存量 env 开关接入运行时开关体系**（副本卡 / 生死状档 / 房间邀请 / 传世卡 / BE 传记 / 系列扩容已迁，清单与注意事项见上）·
**处置申诉只覆盖 `character` / `character_avatar` 两类主体**（如实的范围，不是遗漏：
`world_cover` 属于世界、`world_templates` 根本没有 owner 列，给它们开申诉入口只会得到一个
没人有资格提交的端点；要覆盖需先定义"世界封面 / 模板的作者是谁"这条产品口径）。

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

### 3.1.1 人工校准面（阶段切分 + 身份池 + 境界档三维，**落地于 2026-07-27**）

§4 末尾登记的「仍未做：人工校准面」**三维已全部落地为只读运营视图**。
境界档原本卡在「`Skeleton` 无字段落点」，本次补齐 schema（`assembly::RealmTier`）——
补的是 `Option` + `skip_serializing_if` 字段，模板不声明时 `assembled_json` 逐字节不变，
**黄金世界快照因此未变**（取证方式见本节末「境界档的三条限定」第 1 条）。

| 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| 阶段切分校准视图 | **Implemented（只可视化，不可编辑）** | `server/src/admin_api/calibration.rs` `list_sagas` / `saga_detail`；页面 `admin/src/pages/Calibration.tsx`。诊断项：缺号（**从 1 起算**，故「缺开篇」也报）/ 重号 / 未编号 / 审核态分布 / 星级跨度 / 骨架形状指标 / 世界实例数 | 恒开（只读聚合，同 dashboards） |
| 身份池校准视图 | **Implemented（只可视化，不可编辑）** | 同文件 `list_identity_pools` / `template_identity_pool`。给出池声明、逐身份分配人次 / 覆盖世界 / 填充率、从未被分配的站位、模板已删除的残留身份、在场无站位角色数、集中度基尼（复用 `slo::gini_coefficient`） | 恒开（只读聚合） |
| 境界档校准视图 | **Implemented（只可视化，不可编辑）** | 同文件 `list_realm_tiers` / `template_realm_tier`；schema 在 `server/src/assembly/mod.rs` `RealmTier`。给出戏服声明（档名 / 体系 / 题材 / 冲突烈度 / 入场导演设定 / 风味翻译提示）、同系列各阶对照（缺档 / 复用同一档 / 跨体系）、实例钉住情况（含钉着旧档的实例） | 恒开（只读聚合） |

🔴 三条必须一起读的限定：

1. **只可视化，不可编辑**。全部端点无写入、无副作用、不落 `audit_logs`；校准参数的唯一写入路径
   仍是建模板（`POST /admin/world-templates` 的 `sagaId`/`stageNo`/`skeletonJson.identityPool`/
   `skeletonJson.realmTier`）。
   响应恒带 `editable:false` + `editPath`，页面直接渲染该字段而非写死文案。
   **本批次不含任何在线调参**，故不需要新开关（§0.1 约束的是写入面）。
2. **身份池视图不构成效果验证**。它回答「分配成了什么样、是否失衡」，**不回答「这样分配更好」**——
   见上方订正框的「校准闭环缺失」。页面把这四层状态放在分布图**之前**显式渲染，正是为了防止
   运营把分布图误读成调参有效的证据。
3. **`Implemented` ≠ 可上线**。视图口径未经真实运营数据检验；缺号/失衡的**阈值**目前没有产品定义
   （页面只呈现事实，不给「合格/不合格」判语）。

🔴 **境界档的三条限定**（总规格 §6【拍板 3】戏服原则；比身份池还多一层缺口）：

1. **未声明即零影响，且已逐字节取证**。`Skeleton.realm_tier` 与 `AssembledInstance.realm_tier`
   都是 `Option` + `skip_serializing_if`（同 `payoutTable` 范式）。取证方式：开 pristine HEAD 与
   「仅含本次改动」两棵 git worktree，注入**同一段**探针 dump 四个钉死 `world_id` 的黄金世界产物
   —— 两侧输出 md5 相同、`cmp` 逐字节相等。再在改动侧给黄金骨架加上 `realmTier`
   重跑，逐路径比对的结果是：**新增路径全部落在 `/assembly/realmTier` 子树下，取值改变的路径为 0 条**
   （storyline / mainline / hidden / ending / NPC / 地点 / 身份分配 / 结局定盘 / 逐拍状态 /
   事件流 / 贡献分 / 终局全部未动）。仓库内另有 `realm_tier_does_not_disturb_any_sampling_dimension`
   与 `realm_tier_absent_keeps_assembled_json_byte_identical` 两条测试常驻守护。
2. **叙事感知层是缺的——这一维目前对玩家零可见**。`runtime` 不读
   `assembled_json./assembly/realmTier`，`briefing` 与 `flavorNotes` 进不了任何引擎上下文。
   所以状态只到 **Implemented**，**不是 Integrated**：调整境界档在玩家侧观察不到任何变化，
   校准面的「已声明 / 已钉住」只证明数据在库里。接通叙事层要改 `runtime/`，是独立一步。
3. **零数值是红线，不是风格**。§6「跨体系靠风味翻译，不靠数值换算」+ §0.1 平权：`RealmTier`
   全字段是字符串 / 字符串数组，`realm_tier_carries_no_numeric_field` 逐字段断言序列化产物里
   没有任何数字——给它加 `level`/`powerTier` 就等于把「选阶段」变成「选强度」，须显式评审。
   另：`conflictIntensity: "lethal"` **不是死亡开关**（世界是否致命由建房参数 `lethality` 与 §11
   独立决定），`genre: "history"` 的严审提示**未接进审核链路**（状态 `Concept`，纯人工提示）。

**本次未做（登记为下一步）**：境界档接进 `runtime` 叙事上下文；把境界维接进 `slo/` 形成校准闭环；
题材 → 审核档位的真实联动。三项都不在本批次范围内。

> **2026-07-27 更新（境界档叙事层接通，上段第一项已兑现）**
>
> 「境界档接进 `runtime` 叙事上下文」这一项**已完成**，故上面「境界档的三条限定」第 2 条
> （叙事感知层是缺的 / 对玩家零可见 / 状态只到 `Implemented`）**不再成立**，其余两条一字不改。
>
> **接在哪一步**：只接 §6 原文点名的那一步——「**入场导演统一设定**」。
> 链路 `skeleton.realmTier` →（装配钉住）`assembled_json./assembly/realmTier`
> → `runtime::parse_realm_costume` → `RoundInput.realm_costume`
> → 引擎 `call_director` 的设局 prompt。**七个字段里只有 `briefing` 与 `flavorNotes` 进模型上下文**；
> `id`/`label` 留作审计与展示，`cosmology`/`genre` 只是标注，**`conflictIntensity` 刻意不进**
> （它不是生死开关，生死由建房参数 `lethality` 与 §11 独立决定）。
> 多地点组时每组导演拿的是**同一件**戏服（§6 全员统一；分地点分化就成了数值差）。
>
> **状态：`Integrated`，不是更高**（§0.3 七档）。它改变的只有「这一篇被怎么描写」，
> 且**校准闭环仍然缺失**——没有任何指标能回答「换一件戏服，叙事真的变了吗」。
> 没有任何真实用户数据，故不得读作 Production-ready / Validated / Enabled
> （`realm_tier_effect_states_narrative_layer_is_integrated` 顺带把这个天花板锁进测试）。
>
> **红线守卫（新增用例）**：
> | 用例 | 锁住什么 |
> |---|---|
> | `runtime::tests::realm_tier_reaches_only_the_director_prompt` | 端到端：戏服逐字进导演 prompt；决策/仲裁/写作/审校四环节一个字看不到；`lethal`/`cultivation`/档位 id 不进任何上下文；世界状态与 `world_events` 里不留痕 |
> | `narrative::tests::realm_costume_only_reaches_director` | 引擎侧同一条边界 + 免责话术（「不得据此判定谁能赢」）必须在场 |
> | `narrative::tests::realm_costume_never_reaches_state_or_events` | 戏服不进 StatePatch / DomainEvent / 世界状态 / DNA 卡 / `CharacterState.resources`（引擎判定域） |
> | `narrative::tests::realm_costume_carries_no_numeric_field` | 零数值红线的引擎端（server 端是既有的 `realm_tier_carries_no_numeric_field`） |
> | `runtime::tests::world_without_realm_tier_keeps_director_prompt_clean` + `absent_or_blank_realm_costume_keeps_director_prompt_byte_identical` | **未声明 / 空戏服 → 导演 prompt 逐字节不变**（黄金骨架正属此类） |
>
> **黄金世界快照仍未变**：黄金骨架不声明 `realmTier`，`golden` 那 14 项零改动通过，
> 本次**没有刷任何基线**（若它变了即说明接线泄漏到了默认路径，属 bug 而非需要更新基线）。
>
> **仍未做**：把境界维接进 `slo/` 形成校准闭环；题材 → 审核档位的真实联动；
> if 线是否继承原世界那件戏服（当前恒 `None`，与接线前一致）。

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
  `GET /api/admin/iflines/cost`。✅ **已并入主看板**（此条原登记为遗留，2026-07-27 补上）：
  `/admin/metrics/overview` 现有 `cost.ifline`（allTime / window）+ `cost.combined`（世界线 + if 线合计）。
  🔴 但 `cost.total` 的语义**一个字没改**，仍是世界线口径——把 if 线悄悄加进去，等于让所有历史
  对账在同一个字段名下换了含义，而看板上完全看不出发生过这件事。平台总开销读 `cost.combined`。
  这件事写在 `/api/admin/iflines/cost` 响应的 `dashboardIntegration` 字段里，不靠人记。
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
2. ~~**主成本看板未并入 if 线开销**~~ —— **已闭合**（见上）。保留条目而不是删掉：
   遗留清单上「一条曾经在过」本身是有信息的，直接抹掉会让后来人以为这里从没漏过账。
3. **分叉点仍只支持终局**（0039 的诚实降级，未变；补齐路径见上）。

if 线现在是「可开、可跑、可读、可审、可查成本」，但仍**不是「可上线」**——
**状态语言按 §0.3：`Implemented`，不是 `Production-ready`，更不是 `Validated`**。

> **纪律提醒**：本节存在的意义是 §4.3 那条"发布评审以台账为准，禁止口头'已完成'"。
> 台账漏项 = 评审失去依据，与状态写错同等严重。改 R1 相关代码时同步改本表。

### 3.3 Postgres 排序稳定性（登记 2026-07-27 · **排序清单已修；并发项 ① 已修、② 仍在**）

PG 对 `ORDER BY` 并列行的顺序**不作任何保证**（可随计划、并行度、物理页序变化）；SQLite 则
常按 rowid 稳定返回。首轮审计出的约 30 处已全部处置（下表），处置状态 **`Implemented`**。

> 🔴 **「测试全绿」仍不是本节的证据。** 排序 bug 是非确定性的，绿只说明这一轮没抽中。
> 本次的证据不是「跑绿了」，而是：**每处补的次级键都能构造出让旧 SQL 变红的用例**，
> 且新增的 5 个用例已逐个验证过「回退到旧 SQL 即失败」（见下方「怎么验的」）。
> 本节剩余的两项**不是排序问题**，是并发正确性问题，方案见文末。

**处置口径分三类**（不一刀切）：

1. **补唯一次级键** —— 排序语义本就清楚，只缺 tie-breaker。补主键或其他唯一列。
2. **换用对的列** —— 排序键本身选错了，给错的列打补丁只会把错误固化。
3. **不补假确定性** —— 表里根本没有能表达该语义的列时，补键只会把「不稳定的任意」
   变成「稳定的任意」。这种要么改写入侧（迁移），要么保持原样并把原因写在代码里。

#### 已修清单

| 站点 | 类 | 处置 |
|---|---|---|
| `reports/mod.rs` 日报素材 | **2** | `ORDER BY occurred_at ASC` → **`sequence ASC, id ASC`**。整批事件共用一个 `now_ms()`，`occurred_at` 批内恒为常量，排不动任何东西；`sequence` 才是世界内的因果序 |
| `auth/mod.rs` 发码限频 | **2** | `ORDER BY created_at DESC LIMIT 1` → **`SELECT MAX(created_at)`**。这里要的是一个**值**不是一**行**，聚合口径下"行序"根本不存在 |
| `runtime/mod.rs` 托梦投喂 | 1 | `+ id ASC`。多条 whisper 按行序拼接进 Q-3 prompt，行序变即模型输入变 |
| `runtime/mod.rs` 携带道具物化 | 1 | `+ b.id`。产物进 `CharacterState.resources`，是引擎判定输入 |
| `backpack/mod.rs` `my_backpack` | 1 | `+ b.id DESC`。`my_memberships` 已修站点的同胞，首轮漏了 |
| `admin_api/dashboards.rs` 成本榜 | 1 | `+ world_id ASC`。按 `SUM()` 排序 ⇒ 零成本世界结构性全部并列，Rust 侧再切 `COST_TOP_N` ⇒ 榜单**成员**都是任意的 |
| `notifications/mod.rs` outbox 重扫 | 1 | `+ id ASC`。`due_at` 是排定时刻，整批同值是常态；单键 + `LIMIT 500` ⇒ 待发超 500 时有饥饿风险 |
| `progression/mod.rs` BE 死因 | 1 | `+ id ASC`。注释自称"唯一确定性事实源"，排序键并列即证伪该自称 |
| `admin_api/users.rs` KYC 状态 | 1△ | `+ r.id DESC`。**保留了语义缺口**：该表无单调列，补键只保证「稳定」不保证「最新」；本列只进展示、不进判定，故先取确定性 |
| `memorial/mod.rs` 我的印记 | 1 | `+ deceased_character_id ASC`（补齐唯一键 `(character_id, deceased_character_id)`） |
| `social/mod.rs` ×5、`consents`、`interventions`、`invitations` ×2、`arena`、`livegate`、`assets` ×2、`admin_api/audit`、`admin_api/governance` ×2、`worlds::active_version_tx` | 1 | 一律补主键 `id` 作末位键 |
| `worlds/mod.rs` `find_open_instance` | — | **误报，未改**。`world_series_instances` 主键 `(series_id, instance_no)`，查询已按 `series_id` 等值过滤 ⇒ `instance_no` 在结果集内唯一，本就是全序。已在源码注释里钉死结论，防下一个人"顺手补一个 id" |

**游标分页三处（`notifications` / `reports` / `social` 举报队列）不是补键能修的**：
末行 `created_at` 当游标 + `created_at < cursor` 的**严格小于**，在并列组横跨页边界时会把
整组同值行**永久跳过**——这不是顺序抖动，是**数据丢失**，且两个库上都会发生
（SQLite 只是每次丢同一条，更难被发现）。已改为**复合游标 keyset**
`(created_at, id)`，工具与推导见 `server/src/pagination.rs` 模块头注释。
**向后兼容**：新增可选入参 `cursorId`；不传时退化为原来的单列语义，逐字节等价，
旧客户端零行为变化（用例直接断言了这一点）。响应新增 `nextCursorId`。
举报队列那处后果最重：举报是安全通道，被跳过的一条运营**永远看不见** = 永远不会被处置。

#### 怎么验的（这是本节唯一的证据，不是"CI 绿了"）

- 新增 5 个用例：`reports`×2、`backpack`×1、`social`×1、`auth`×1，另加 `pagination` 单元测试 2 个。
- 每个用例的 fixture 都**构造成让旧 SQL 必然失败**，并逐个实测过：把 SQL 回退到改动前，
  `highlights_follow_event_sequence_not_the_wall_clock` 得到 `[seq-3, seq-4, seq-2, seq-1]`、
  `keyset_cursor_recovers_the_row...` 第二页为 0 行、
  `backpack_ties_...` 得到升序而非降序——三处均确认变红，改回后变绿。
- 双库全量：SQLite `883`（`billing,arena`）/ `805`（default）/ golden `14`，**数量与断言零变化**；
  同一套用例在**一次性 Postgres 16 实例**上跑出**完全相同**的 `883` / `805`。
  （PG 实例建在临时目录、跑完立即 `pg_ctl stop` + `rm -rf`，未在仓库或系统留任何数据目录。）
- golden 基线与 `runtime/simulation/baseline.json` **一个字节未动**。

#### 两项并发正确性问题（**不是排序**）：① 已修，② 仍在

**① `world_events.sequence` 的分配没有并发安全 —— 已修（迁移 `0043`，状态 `Implemented`）**

原口径：`events::insert_events_tx` 与 `events::persist_and_broadcast_public_event` 各自在事务内
跑 `SELECT COALESCE(MAX(sequence),-1)+1` 读-改-写分配，而 `idx_world_events_world(world_id, sequence)`
**非唯一**。SQLite 的单写者锁让它事实上串行；**PG 在 READ COMMITTED 下不会**——
`SELECT MAX()` 不取任何锁，两个并发事务读到同一个 `MAX` 就会写出同号，且**静默提交**。

- **竞态是真实可达的，不是理论**：`arena/mod.rs:85`（`arena` feature 下的礼物/赛事事件）
  是玩家/运营触发的 HTTP 路径，与 `runtime::commit_tick` 的批量落库并行，二者写同一个世界。
- **爆炸半径 = 反向把并列引入所有依赖 sequence 唯一性的站点**：`clips/mod.rs:36` 的"后来者胜"
  高光规则、`arena/mod.rs:240/:367`、`slo/mod.rs:253`，最重的是 `events/mod.rs`
  的 `VISIBLE_EVENTS_SQL`——它拿 `sequence > $cursor` 当 WS 断线补偿游标，撞号意味着
  两条事件里有一条**永远不会补给重连的客户端**。
- 🔴 **只加 `UNIQUE(world_id, sequence)` 是错的修法**（故未采用）：它把静默损坏换成 23505，
  而输的那一方是**一整个 tick 的 commit 事务回滚**（模型已跑完、token 已烧掉）；
  且迁移在存量已有重复数据时会直接失败 = 服务起不来。

**落地方案**：迁移 `0043_world_event_seq` 加发号器表 `world_event_seq(world_id PK, next_seq)`，
全仓唯一分配入口收敛为 `events::allocate_sequences_tx`，同事务三步领号
`INSERT ... ON CONFLICT(world_id) DO NOTHING` → `UPDATE ... SET next_seq = next_seq + n`
→ `SELECT next_seq`。正确性来自第二步 `UPDATE` 的**行级排他锁**（并发事务阻塞到前者提交/回滚，
再在更新后的值上叠加，无丢失更新），不是唯一约束；两个库都有这条语义，
故不需要 `FOR UPDATE`（PG 方言）也不需要 `RETURNING`（SQLite 3.35+）。
自增与事务同生共死 ⇒ **不产生空洞**（与 PG 原生 sequence 不同）。

- **存量回填**：迁移第二条语句 `INSERT INTO world_event_seq SELECT world_id, MAX(sequence)+1
  FROM world_events GROUP BY world_id`。不回填 = 升级后第一条事件拿 0，把整段历史重新撞一遍，
  比原 bug 更严重。**迁移完成到服务接客之间没有窗口**（`db::connect()` 先跑完 `migrate!()`，
  之后 `main` 才 bind 端口）；唯一残余窗口是**多实例滚动发布**，兜底在代码侧——
  `allocate_sequences_tx` 建行时初值取 `COALESCE((SELECT MAX(sequence)+1 ...), 0)` 而非常数 0，
  任何尚未登记进发号器的世界首次分配即自动对齐到历史之后。
  「旧实例继续往**已登记**世界写」这一种仍会撞号，处置是发布纪律（先停旧再放新），不是迁移能解决的。
- **存量重复数据普查**：仓内唯一持久库 `server/muse-demo.db`（dev 演示 SQLite）26 条事件 / 1 个世界，
  `sequence` 0-25 全互异，**无重复**；生产 PG 部署尚不存在（T0 未开测）。即撞号事故迄今未实际发生，
  修的是**打开 PG 那一刻就会踩到**的雷。回填取 `MAX` 对已有重复也是安全的（继续往后发即可）。
- **锁的持有范围**：从 `insert_events_tx` 到 `tx.commit()`。`commit_tick` 的事务在**模型调用之后**
  才开启，整段纯 DB 语句零外部 IO（第 3 层语义审核是网络调用，早已被明确排除在事务外）；
  分配点又排在该事务尾部（CAS → 贡献 → 终局 → world_ticks → critic → **落库** → 干预 → 预算 → commit），
  持锁期间只剩两条小 UPDATE。代价是同一世界的事件写入串行化——语义上本就该串行。
- 唯一索引作为**兜底**可以在此之后再加（仍需先扫存量重复），本批次未加。

**怎么验的（证据是双库对照，不是"CI 绿了"）**：新增多连接测试池 `testkit::test_pool_concurrent(n)`
——🔴 **没有动默认 `test_pool` 的 `max_connections(1)`**（那个 1 是刻意的，改了会让大量既有用例
以与被测点无关的理由失败）。并发用例 `concurrent_sequence_allocation_never_collides_or_gaps`
起 24 个任务对**同一个世界**并发落 48 条事件，且**两个分配站点同时打**（半数走
`persist_events` = `commit_tick` 批量侧，半数走 `persist_and_broadcast_public_event`
= `arena::emit_arena_event` 的 HTTP 侧），断言「无重复 + 无空洞 + 总数正确」。
在**一次性 Postgres 16.9 实例**上把分配临时回退成旧口径：48 条事件只拿到 **23 个不同的号**
（25 条撞号），用例立刻变红；改回发号器后是连续的 0..47，重复跑 5 次稳定全绿。
**同一次回退在 SQLite 上那三条断言全绿**——单写者锁把读-改-写事实上串行掉了，
这正是「SQLite 绿不能作为并发证据」的直接实证。
双库全量同源对照：`billing,arena` 与 default 两个 feature 组合在 SQLite 与 PG 上跑出**相同的通过数**，
golden `14`，`runtime/simulation/baseline.json` 与 golden 基线**一个字节未动**。
（PG 实例的数据目录建在临时目录、跑完 `pg_ctl stop` + `rm -rf`，未在仓库或系统留任何残留。）

⚠️ 按 §0.3，本项状态是 **`Implemented`**：压测通过 ≠ 生产验证。真实并发规模、真实数据量、
真实部署下的锁竞争与迁移锁行为**一律未验证**，不得表述为「并发安全已验证」。

**② `sms_challenges` 的「最新那条」（`auth/mod.rs:304`，登录时校验哪条 OTP hash）**

该表无单调列、`id` 是 uuid v4。**已按第 3 类处置：不补假确定性，原样保留并在代码里写清原因。**
同一站点的**限频**那半边（原 `:248`）已按第 2 类真正修掉——它只需要一个值，换成 `MAX()` 后
"行序"不复存在。剩下的 `:304` 确实需要定位到具体一行，而并列的唯一来源是限频检查的 TOCTOU
（两个并发请求都读到"无近期记录"，各插一行）——同样是**写入侧**问题。两条候选：
(a) `UNIQUE(phone, created_at)` + 把冲突映射成 409 限频，让同毫秒并列**物理上不可能**，
    于是 `created_at DESC` 天然成为全序（代价：迁移会在存量重复数据上失败，且把竞态从静默变成写错误）；
(b) 给表加真正的单调序列列（与 ① 是同一道题，同样的代价）。
两条都需要迁移，且整条短信通道当前是 Dev 桩（`DevSms`，按 §0.3 本就不可上线），
故排在 ① 之后。**在此之前不得给它补 `id DESC` 充数。**

### 3.4 R3 收官批次台账 —— 直播场（总规格 §2 + §15 第 4 层，**落地于 2026-07-27**）

> R3 路线图（总规格 §19）的最后一项：**直播场（定档调度 + 延迟缓冲 + 弹幕）**。
> 它同时是 T5「50-100 人世界；直播场 + 弹幕」这条开放范围里除生死状之外的全部内容。

| R3 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| ① 定档 | **Implemented** | 迁移 `0042`（`live_sessions`）；`server/src/livestage/`。预告时刻 + 开播时刻 + 场次容量；状态机单向 `scheduled → live \| canceled`、`live → ended`（CAS 落库）；节目单只列已到预告时刻的场次。定档提前量参数化 `MUSE_LIVE_ANNOUNCE_LEAD_MS` | **关闭**（`MUSE_LIVE_STAGE`） |
| ② 延迟缓冲 | **Implemented** | 播出水位线 `max(最新 done 拍 - delay_ticks, published_high_tick)`；撤下面 `live_withholds`。🔴 **是内容安全机制不是体验设计** —— 它就是 `safety/mod.rs` 的 `TODO(§15-L3)` 原文里等的那个「拦截窗口」 | 同上 |
| ③ 弹幕 | **Implemented** | `live_danmaku`；过 `safety::mask` + `safety::moderate_and_queue`（静态 UGC 唯一入队/记险入口）+ 限频 429 + 成年门 403 + §14 面具。🔴 **永不进 `world_events`** | 同上 |
| ④ 转化度量 | **Implemented** | `live_viewers`（**新建的数据源**）+ `livestage::conversion_block` → `/admin/metrics/overview` 的 `liveStage` 顶层键 | 同上（指标本身只读恒算） |

**② 延迟缓冲的四条结构性要点（都已锁进测试，改动需显式评审）**：

1. 🔴 **待播内容不另存副本**。一拍提交时 `world_events` 已是世界事实（§0.3），延迟的是**公开投影的
   播出时刻**，不是事实本身。建一张 `pending_broadcast_events` 副本表会立刻产生两个事实源
   （副本写失败 = 世界演过了而直播永远缺一拍；副本被改写 = 观众看到的与世界记载的不是同一件事）——
   **那才是事实错乱**。现在一份内容一处存储，播出面只多一条水位线。
2. 🔴 **延迟只作用于世界外**。世界成员的 `/worlds/{id}/events` 一拍不延——他们的角色正在经历这些事，
   延后当事人等于让世界停摆。用例 `delay_buffer_holds_recent_ticks_but_never_delays_world_members`。
3. 🔴 **已播出的不缩回**。`published_high_tick` 是单调下界，于是 T5 预案「审核成本失控 → **直播延迟
   拍数上调**」这个旋钮**只勒住未来**：把 `delayTicks` 从 1 调到 5 不会让已在观众屏幕上滚过去的
   4 拍从播出面消失（那是对已公开内容的回滚）。用例
   `raising_delay_ticks_never_retracts_already_published_ticks`。
4. 🔴 **审核不过 = 不外发，不是回滚**。人工撤下写 `live_withholds` **独立表**，`world_events`
   **逐字节不动**，战报 / 回放 / 日报 / 成员读取面全部不受影响。回执如实标注 `preemptive`
   （播出前拦下 / 播出后撤下——后者明说「收不回已经看见的」）。用例
   `withholding_leaves_the_worldline_byte_identical_and_scoped_to_this_session`。

**🔴 与错峰调度那条既有红线的关系**：`runtime::offpeak` 的「直播场（`room_type='arena'` ∨
`tick_per_day ≥ MUSE_OFFPEAK_LIVE_TICK_PER_DAY`）永不延后」**一行未改**，且**不得**改成
「有没有定档记录」——豁免判据必须是世界自身的节奏属性，否则运营建一条定档就顺手改掉一个世界的
调度行为，那是两个不该耦合的旋钮。播出排期（`live_sessions`）与引擎拍排期（`schedule_due_ticks`）
输入完全不相交，双向源码级用例 `red_line_offpeak_live_exemption_untouched` 钉住。

**测量手段（2026-07-27 补齐）**：T5 门槛「**直播场观众→玩家转化 ≥2%**」此前**无从判定**——
全仓没有任何直播观看埋点（`world_members` 只记入场的玩家，`world_events` 只记世界内发生的事，
观众来过一次不留任何痕迹）。这与 T1「OOC 申诉率」曾经的处境完全同构：
**门槛写了却没有数据源，就等于测不了**。现由 `live_viewers` 提供数据源，指标口径见
`docs/API.md` §6「直播场」小节。
⚠️ 开测前**必须先把 `MUSE_LIVE_STAGE` 打开**（默认关闭）：入口没开时该指标返回
`entry_not_open`（显示 `—`）而非 0%，正是为了防止把「没测过」误读成「没人转化」而误判为不通过。
另有 `withheldPreemptiveRate`（撤下里播出前拦住的占比）直接作为「延迟拍数够不够」的判据 ——
它 < 1 就是 T5 预案「上调延迟拍数」该被触发的信号，`danmakuBlocked/danmakuTotal` 则是
「内容审核成本 ≤ 生成成本的 5%」那条门槛的一手输入。

**2026-07-27 再补齐（第 3 层落地）**：那条成本门槛的**运行时侧**分子此前同样无数据源——
审核链一次调用都没被计过数。现由 `safety_recheck_runs`（迁移 `0046`）逐次尝试记下
送审条数 / 送审字符数 / 命中数 / 重试次数 / 耗时，读数在 `GET /admin/safety/recheck`；
分母侧（生成成本）一直在 `world_ticks.cost_tokens` 里。
🔴 但**比值本身仍算不出来，端点也不假装能算**（`cost.ratioAvailable` 恒为 `false`）：
`ModerationProvider::check_text` 只回裁决、不回 token 也不回费用，而 Dev 桩的调用成本恒为 0。
交付的是**调用量口径**，换算成钱要等真实 provider 的计价。
⚠️ 同理，第 3 层的 `interceptedBeforeBroadcast`（收紧发生在播出水位线之前的条数）与
`withheldPreemptiveRate` 是**同一个判据的两条独立证据**（一条来自自动链、一条来自人工撤下），
两者都 < 1 才说明延迟拍数真的不够；只看其中一条会漏判。

### 3.5 社交举报队列的运营前端（真人社交解锁的治理闭环，**落地于 2026-07-27**）

> 迁移 `0040` 交付了完整的举报 API（`server/src/social/`），但**没有后台界面**——举报单只能靠
> 直接调接口处置。举报是安全通道：**进得来、处置不进去** 等于队列积压且无人知道。本项补的是这一环。

| 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| 举报队列前端（列表 / 筛选 / 详情 / 复核处置 / 跳转处置入口） | **Implemented** | `admin/src/pages/SocialReports.tsx` + `.css`；RBAC 模块 `social`（`admin/src/rbac.ts`，可见角色 reviewer/support/admin，与 `social::require_report_handler` 逐字对齐） | 随 `MUSE_SOCIAL_IDENTITY_UNLOCK`（**关闭**）；关闭时端点 404 → 整页「功能未开启」空态 |
| 队列筛选下推 SQL（status / category / subjectKind） | **Implemented** | `social::list_reports_admin`，三个筛选值走白名单；🔴 **未知值 400 而非静默空列表**——`?status=Pending` 拼错若返回空队列，运营读到的是「没有积压」 | 同上 |
| 队列形状聚合（积压 / 类别分布 / 最久未处理 / 达阈值对象数） | **Implemented** | `GET /admin/social/reports/summary`（只读聚合，每项一次 `GROUP BY`）。指标**不受分页与筛选影响**——拿已加载那一页数出来的「待处理 12 条」会被读成队列只剩 12 条 | 同上 |

**🔴 处置边界（本项最重要的一条，改动需显式评审）**：后端 `resolve` 刻意**只改举报单状态**，
真实处置（封禁 / 内容驳回 / 申诉改判）走各自既有路径，各带自己的权限矩阵与审计。
前端**尊重这条边界**：详情抽屉里的处置动作是**跳转 + 回填**（`/users?query=<被举报人>`、
`/risk?kind=social_report_threshold`、`/audit`），**一条新的写路径都没有开**；
且跳转按钮受**前端 RBAC 收敛**——reviewer 看得见举报队列但进不去用户管理，那他就不该有一个
能点的封禁按钮（后端仍二次校验，前端做的是纵深与诚实）。把处置塞进举报接口 = 给封禁开一条
绕过既有权限矩阵的侧门。

**分页**：复合游标 `(created_at, id)`（`server/src/pagination.rs`）。举报是**批量同毫秒写入**的
典型场景，单列游标下横跨页边界的并列组会被永久跳过 = 那几条举报永远不会被处置。
另补「末页回 `nextCursor: null`」（多取一行判定，口径同 `/admin/risk-events`）：
只按「末行有没有」发游标的话「加载更多」永远在，运营**分不清翻完了没有**——而这正是队列唯一要回答的问题。

**§14 恨隔面具原则**：玩家侧任何接口都不下发被举报人的真人 id（连举报回执都不给）。
真人 id 与举报正文只在这一处运营复核档出现，走 reviewer/support 鉴权，处置写
`audit_logs('social.report_resolved')`；界面把这件事**显式写出来**，不默默展示。

**未成年保护**：举报与拉黑**不设年龄门**（关掉等于让未成年无法自保）。前端**没有**反向给它加限制——
不对举报人做任何年龄相关的过滤、降权或排序，这条写在页面口径脚注里以防后人"顺手补齐"。

⚠️ 状态止于 **Implemented**：页面结构、口径与权限收敛已落地并有后端用例守护，但
① ~~「后端未挂 CORS」这条全后台共性问题仍在~~ —— **已解除**（`app.rs` 挂了 `CorsLayer`，
白名单走 `MUSE_CORS_ORIGINS`、配错即启动失败；设计文档 §13.4 已划掉该条）。
⚠️ 但**本页的带数据验收是在那之前做的**，走的是临时同源代理，故「验收过」与「在正式跨源
形态下验收过」仍是两件事——这条不是可以顺手删掉的历史，删了就等于把一次代理下的验收
记成了正式验收；② 队列本身尚无真实举报数据（功能默认关闭），
**没有任何运营吞吐 / 处置时效的经验值**，因此不构成 `Production-ready`，更不是 `Validated`。

### 3.6 §15 第 3 层的投递可靠性（补偿轮询，**落地于 2026-07-27** · migration `0048`）

> 第 3 层（语义复核，`0046`）从一开始就在模块头如实登记着一条遗留：`MemQueue` 是**进程内内存
> 队列、不持久**，进程重启会把在飞的复核任务带走，那一拍的事件停在 `approved` 且**再无人复核**。
> 原登记给了两条闭合路径（接 Redis / 加补偿轮询）。本项走的是后者。

| 项 | 开发状态 | 代码锚点 | 默认开关 |
|---|---|---|---|
| 对账 + 补投（`world_ticks ⋈ safety_recheck_runs`） | **Implemented** | `server/src/safety/semantic/sweep.rs`；索引 migration `0048`；`main.rs` 起单循环 | `MUSE_SAFETY_RECHECK_SWEEP`（**关闭**，全局档） |
| 缺口读数（`durability` 块） | **Implemented** | `sweep::gap_report` → `GET /api/admin/safety/recheck`。**现算，不读轮询自己的记账** | 无开关（只读） |
| 「看过了、无候选」落终局行 | **Implemented** | `semantic::run_recheck` 的无候选分支补 `persist_run`（开关关闭那一种**仍不落库**，两种 skip 的口径故意分开） | 随第 3 层 |

**🔴 为什么是对账而不是持久队列（本项最重要的一条）**：持久队列保证的是「**已经入队**的任务不丢」，
而这条链上更常见的失败是「**根本没入队**」——开关当时是关的、`push_json` 序列化失败被静默吞掉、
以及一类此前**没有任何地方登记过**的漏：`runtime` 里 blocked / cas_conflict 两条收尾路径也把
`world_ticks.status` 写成 `'done'`，但它们**不经过** `commit_tick` 末尾那行 `enqueue_after_commit`，
那些拍带着 approved 事件却从未被第 3 层看过一眼。队列对这些一无所知，因为待办从没进过它的视野。
轮询不问「任务在哪」，它问「**这一拍到底被复核过没有**」——那个洞是写这条对账查询时才查出来的。

**🔴 无状态是刻意的**：轮询不建表、不留游标、不维护计数器。若它记自己的账，那它一旦死掉
（panic / 开关被误关 / 部署漏了这个进程），那些数字会**冻结在健康的样子**而真实缺口在背后继续长。
现算的缺口数是「轮询死了也照样变大」的量——运营面读到的是**病情**，不是病历。
这也意味着 `durability` 在**开关关着时同样有效**，于是「要不要开它」有据可依，而不是开了才知道。

**⚠️ 三条如实登记的边界**（写在模块头 + `durability.honesty[]` + STARTUP §8.1，三处同源）：
① **回看窗口是硬上限**（默认 24h），挂机超过这段的拍**永远补不回来**——`justOutsideWindow` 就是量它的；
② **单实例假设**，多实例同开会重复补投（重复的 provider 调用是真烧的），同 `world_events.sequence` 那条，属发布纪律；
③ **补偿路径比正常重试粗**：只能重查整拍，因为细粒度的 `retry_ids` 名单随丢失的任务一起没了。

**验证**：12 条新用例，SQLite 与 Postgres 双库同通过数（1008 / 1086）。
7 处故障注入逐条确认用例真的会红：续号→重号 · 无候选不落账 · 去掉 grace 上界 ·
去掉逐世界开关校验 · `skipped` 移出终局集合 · 去掉在飞去重 · 去掉回看下界。
🔵 其中「`skipped` 移出终局集合」那次注入暴露了**我自己写的一条用例是假绿**——
它的收敛断言被 grace 窗口顺带盖住了，证不出想证的那一条，已修（把台账行的时间推到 grace 之外再断言）。

⚠️ 状态止于 **Implemented**：默认关闭、从未在真实部署下跑过，且它补的那条链（第 3 层）本身
仍是 Dev 桩——**补一条拦不住东西的链路的投递可靠性，不等于内容安全变强了**。
它此刻的价值是把一个**静默的洞**变成一个**有读数的洞**（`durability` 在开关关着时就能读）。

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
     ~~它至今**未接线**到黄金世界回归/仿真工装（要动 `runtime/mod.rs`）~~（**已过期，接线于
     2026-07-27 完成，见下一条**），也**没有**任何真实模型录制入库，
     故本条**不得**被读作「角色一致性已验证」
   - 🟡 **录制-回放接线**（2026-07-27，任务 #46，状态 `Implemented`）：`server/src/runtime/record.rs`，
     接点 = `runtime::process_tick_inner` 第 9 步（模型客户端在整条 tick 路径上的**唯一出口**，
     故生产 `process_tick` 与注入 `process_tick_with_model` 一并覆盖）。
     **默认关闭**（§0.1）：未配置时接线点返回**传进去的那一个 `Arc`**（`Arc::ptr_eq` 成立，
     中间没有任何一层包装），默认路径逐字节零变化 —— golden 基线与 `simulation/baseline.json`
     一个字节未动。开关 `MUSE_TICK_RECORD` / `MUSE_TICK_REPLAY`（互斥）/ `MUSE_TICK_REPLAY_MATCH` /
     `MUSE_TICK_RECORD_DIR` / `MUSE_TICK_RECORD_WORLD`，另有进程内按 world 覆盖供测试与录制入口用。
     🔴 **录制失败降级、回放失败不降级**：录制出问题记 warn 退回真实模型（观测面不得弄挂世界）；
     回放加载失败**直接让本拍失败** —— 回放一旦降级成真实模型，那次对比结果就是假的。
     🔴 **仍不得读作「角色一致性已验证」**：本仓**没有任何一份真实模型录制**（需运营方自带 API Key），
     「差异多大算 OOC / 退化」的评分口径**也还没有**。录制产物带 `labels.responseSource`
     （`scriptedStub` / `real`）把「这份录制里装的是谁的响应」钉进产物本身，
     同 `QualitySource::SimulatedStub` 的做法 —— **数会被复制进评审材料，文档不会。**
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
     **补记（2026-07-27，任务 #46）**：接线已通（`runtime::record`，见上一条），黄金世界现在
     **可以**被录制 / 回放，端到端有用例锁死「关 / 录 / 放三跑，结构化产物逐字节相等」。
     🔴 但这一栏 ❌ **一条都没变**：默认关闭，回归跑的仍是 `ScriptedModel`；
     **零真实录制 + 无 OOC 评分口径**，两样缺一都不足以谈叙事质量。

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
**接线已通**（`runtime::record`，2026-07-27 任务 #46，`Implemented`，默认关闭），
**但仿真仍全程跑桩**——本节的三指标口径与「诚实划界」**一字未变**，
`simulation/baseline.json` 也一个字节没动（默认关闭时接线点原样返回同一个 `Arc`）。
🔴 还差的是**真实录制**（需运营方自带 API Key，本仓零份）与**质量口径**（「差异多大算 OOC」尚未定义），
两样都到位之前不得据此谈内容质量。

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

> **2026-07-27 更新**：上段「仍未做」的**可视化部分已全部兑现**——阶段切分 / 身份池 / **境界档**
> 三维的只读校准视图均已落地（`server/src/admin_api/calibration.rs` +
> `admin/src/pages/Calibration.tsx`，台账见 §3.1.1）。
> 上段说的「境界档在 `Skeleton` 里没有字段落点」**已不再成立**：schema 已补为
> `assembly::RealmTier`（`Option` + `skip_serializing_if`，未声明时装配产物逐字节不变）。
> 另注意上段说的是「可视化**与调参**」，三维**都只做了可视化，没做调参**：全部端点只读，
> 校准参数的写入路径仍是建模板。境界档另有一层更弱的限定——`runtime` 还不读它，
> 这一维目前**对玩家零可见**（§3.1.1「境界档的三条限定」第 2 条）。
>
> **2026-07-27 再更新**：最后那句「`runtime` 还不读它 / 对玩家零可见」**已不再成立**——
> 境界档的 `briefing` 与 `flavorNotes` 已接进每拍的入场导演 prompt，
> 叙事感知层由 `Missing` 升到 `Integrated`（详见 §3.1.1 末尾的接通更新块）。
> **只可视化、不可调参**这一条不变；**校准闭环仍然缺失**这一条也不变。

### 4.5 录制-回放接线（新增于 2026-07-27 · 任务 #46 · 状态 `Implemented`）

§4.1 「唯一真新建件」的第二段：工具（2026-07-26）→ **接线（本节）** → 真实录制与质量口径（**未做**）。

**落点**：`server/src/runtime/record.rs`（生产代码，非 `#[cfg(test)]`）+
`server/src/runtime/record/tests.rs`。接点 = `runtime::process_tick_inner` 第 9 步。
无路由、无迁移、无新表。

| 项 | 内容 |
|---|---|
| **接在哪** | tick 路径上模型客户端的**唯一出口**，故 `process_tick`（生产，内部造 `HttpModelClient`）与 `process_tick_with_model`（注入，golden / simulation）一并覆盖 |
| **默认** | **关闭**。未配置时接线点返回**传进去的那一个 `Arc`**（`Arc::ptr_eq` 成立，中间没有任何一层包装）——「默认路径零变化」是类型层面的恒等，不依赖「包装恰好透明」 |
| **开关** | `MUSE_TICK_RECORD=<id>` · `MUSE_TICK_REPLAY=<id>`（与前者互斥，同时设 = 两个都不启用）· `MUSE_TICK_REPLAY_MATCH=prompt\|slot` · `MUSE_TICK_RECORD_DIR=<绝对路径>` · `MUSE_TICK_RECORD_WORLD=<worldId>`。空串 / `0` / `false` / `off` 一律当未设置。env **只在进程首次用到时读一次** |
| **产物** | `<dir>/recordings/<id>.json`，每拍覆写为全量。`dir` 缺省 = 该世界的引擎数据目录（在 gitignore 的 `muse-objects/` 下） |
| **降级纪律** | 录制失败 → 记 warn、退回真实模型，**绝不阻断 tick**（观测面不得弄挂世界）；回放加载/未命中 → **本拍失败**，🔴 **绝不回落真实模型**——回落会让一次「回放」偷偷变成一次真实调用，那份对比结果就是假的 |
| **内存** | 录制全量驻留进程内存，故有 `MAX_RECORDED_CALLS`（20 000）上限；触顶后停止录制（已录部分保留在盘上），而不是慢性 OOM |

**跑一次录制（需自带模型凭据，仓库不持有任何 Key）**：

```bash
MUSE_RECORD_BASE_URL=https://api.example.com/v1 \
MUSE_RECORD_API_KEY=sk-你自己的key \
MUSE_RECORD_MODEL=your-model-id \
MUSE_RECORD_DIR=$PWD/muse-objects/recordings \
  cargo test --manifest-path server/Cargo.toml \
    -- --ignored --nocapture record_golden_world_with_real_model
```

跑完打印产物**绝对路径** + 调用条数 + 自检结果。之后换 Prompt / 换引擎版本对着它回放：
`MUSE_TICK_REPLAY=<id> MUSE_TICK_RECORD_DIR=<同一目录>` 起 server；换模型再录一份，用
`muse_engine::replay::diff::diff_recordings` 对齐到「哪一拍 · 哪个角色 · 哪个环节」比字段级差异。

**默认关闭如何被证明**（两层，均为阻塞用例）：

1. `record::tests::off_returns_the_very_same_arc` —— `Arc::ptr_eq` 恒等 + 不建目录不落文件。
   **可证伪**：在 Off 分支塞任何一层包装，该用例立刻转红（实测过）。
2. `record::tests::golden_world_record_replay_round_trip_is_byte_identical` —— 黄金世界主线
   **关 / 录 / 放三跑**，`snap_off == snap_rec == snap_replay` 逐字节相等，且回放期间
   注入的剧本模型 **`captured()` 为空**（真发生回落它就会被调用）。
3. `runtime::golden` 全部 12 项与 `simulation/baseline.json` **一个字节未动**。

🔴 **本节交付的是工具接线，不是任何叙事质量结论。** 至今：
**零真实模型录制**（需运营方自带 API Key）+ **无 OOC / 退化的评分口径**。
两样都不到位，故本节**不得**被读作「角色一致性已验证」或「已建立基线」。
录制产物自带 `labels.responseSource`（`scriptedStub` / `real`）——一份桩录制和一份真实录制
长得一模一样，不把来源钉进产物本身，半年后就会有人拿桩当基线
（同 `QualitySource::SimulatedStub` 的做法：**数会被复制进评审材料，文档不会**）。

### 4.6 校准维度读数（新增于 2026-07-27 · 任务 #53 · 状态 `Implemented`）

内容中台的生产流水线是**人工校准 → 仿真试跑 → 世界质量回归**（总规格 §79/§83）。
前两环已建成（§4.4 + `admin_api::calibration` 三维视图），但**闭环缺最后一环**：
§4.2 那八项一律按平台 / 按 `character_id` 聚合，与**运营调的那个旋钮**（身份 id、境界档）无关，
于是「这样配是不是更好」在指标结构上根本问不出来——两处 `effect.calibrationLoop` 因此长期写着 `Missing`。

**落点**：`server/src/slo/calibration.rs`（只读聚合）→ `/admin/metrics/overview` 的
`narrativeSlo.calibration`（与 `metrics` 并列的兄弟键，`?slo=0` 一并跳过）。
无迁移、无新列、无新路由：读的全是既有数据（`worlds.assembled_json` 的
`/assembly/identityAssignments` 与 `/assembly/realmTier`、`world_members`、`world_contributions`、
`world_ticks`、`world_events`、`audit_logs`）。

**两维形状刻意不同构**（把后者套成前者会得到一个恒为 0 的假指标）：

| 维 | 形状 | 为什么是这个形状 | 读数 |
|---|---|---|---|
| 身份维（§5） | **组内分布** | 身份是**各不相同**的开局站位，一个世界里同时存在多个 → 有组内可比性 | 每身份的**相对均分倍率** `(score ÷ 世界总分) × 世界成员数`，1.0 = 恰好均分。均值 / 中位数 / 极值 / 零分观察数，外加各身份**均值之间**的集中度基尼，以及 `(unassigned)` 对照桶。直接回答「某个身份是不是系统性拿到更少戏份」 |
| 戏服维（§6【拍板 3】） | **跨世界对比** | 境界档**全员统一**：一个世界只有一件戏服、无池、无配额、装配层零抽样 ⇒ **没有组内分布**（组内基尼恒为 0） | 按钉住的 `realmTier.id` 分桶，各桶各自报 §4.4 的世界质量三指标（完读 / 阻断 / 结局分布）。`(none)` = 未钉戏服的**对照桶**，是「配了戏服的世界」唯一的参照系 |

**三条纪律（各自有用例锁着）**：

1. 🔴 **只读，绝不回灌引擎**。本模块没有一条 `INSERT/UPDATE/DELETE`，产物只作 JSON 返回。
   理由与 `world_contributions` 独立于 `narrative_state_json` 单独建表**完全同源**（迁移 0025）：
   一旦按身份分组的戏份差进了引擎判定输入，「身份影响判定」就成立，直接违反 §0.1 平权红线。
   锁：`calibration_readings_never_write_anything`（跑完库内容逐字节不变）·
   `calibration_readings_never_touch_narrative_state`（`narrative_state_json` / `state_revision` 专项）·
   `calibration_module_source_contains_no_write_statements`（源码级，注释行不计）。
2. 🔴 **四态分得开**：`entry_not_open`（**这一维从未被任何模板配置过** → `—`，且不发任何计数，
   发了会被当成 0 读）/ `no_data_in_window`（配置过但零样本 → `—`，计数照发以便区分
   「没世界」还是「有世界但都没分配 / 都没挣到分」）/ `insufficient_sample`（有样本但
   `n < minN` → 显示「样本不足（n=…）」，见 §4.6.1）/ `ok`（真数，**可以是 0**）。
   前两态口径抄 §4.2 `oocAppealRate`——直接报 0 会得到「看起来棒极了、实际上什么都没测」的数，
   而门槛判定恰恰要拿它决定继续/调整/停止。
3. 🔴 **不给「越高越好」的单一分数**。校准是**多目标**的（公平 vs 戏剧性：把戏份摊平到各身份
   均值全是 1.0 就没有主角了），综合评分会诱导运营去优化那个数字本身。两维在 `ok` 态都没有
   代表整维的标量 `value`，全树也不出现 `score` / `grade` / `verdict` / `recommendation` 一类判语字段。
   锁：`calibration_readings_expose_no_composite_score`。

**口径复用，不另立第二套**：集中度走 `slo::gini_coefficient`（与叙事注意力基尼同一实现），
三指标走 `slo::quality`（与 §4.4 仿真回归**算同一个数**）。为避免 N+1，新增了
`quality::collect_world_facts_bulk`，与单世界版共用同一套分类规则（`add_tick_bucket` / `parse_ended_reason`），
两条路径不漂移由 `bulk_world_facts_match_single_world_facts` 锁住。

**一处刻意的口径差异（不是 bug，是这个指标存在的理由）**：身份维的分母是 `world_members` **全集**，
无贡献分行的成员按 **0 分**计入；而 §4.2 `attentionGini` 走的是 `world_contributions ∩ world_members`。
`world_contributions` 是**挣到分才落行**的，交集口径因此**看不见「一分没挣到」的人**——
而「某个身份是不是系统性拿不到戏」正要靠这些人才答得了。两个数**不可互相校验**。
窗口口径也不同：本节是 cohort（`worlds.created_at` 落窗，两维共用一批），`attentionGini` 是
「窗口内有贡献分更新的世界」。

#### 4.6.1 样本量与不确定性（补于 2026-07-27 · 任务 #55 · 仍是 `Implemented`）

上一版读数有一个会**直接导致误判**的缺口：`meanShareGini` 在 **3 个观察**和 **300 个观察**上
长得一模一样。运营拿它调参，就是在追噪声——而这些读数存在的全部理由就是给运营调参用。
本节补的三件事**没有新迁移、没有新路由、没有新 SQL**（纯 Rust 侧统计与序列化）：

| 补丁 | 内容 |
|---|---|
| **每个读数随身带 n** | 读数一律是信封：`value`（可据此调参的读数，样本不足时为 `null`）/ `pointEstimate`（原始算术，永远给）/ `n` / `minN` / `ci95` / `ciNote`。**`n` 与 `value` 在同一个对象里是刻意的**——取值必须穿过信封，「拿到一个比例却不知道它压在几个观察上」在结构上不可能发生。锁：`every_reading_carries_its_own_sample_size`（形状锁：树里每个含 `pointEstimate` 的对象都必须带 `n`/`minN`/`status`，列举法会漏掉下一个新加的读数） |
| **最小样本量约定** | `MUSE_SLO_CALIBRATION_MIN_N`（默认 **30**）+ `MUSE_SLO_CALIBRATION_MIN_GROUPS`（默认 **2**，集中度类专用）。低于门槛 → 第四态 `insufficient_sample`：`value=null`，但 `pointEstimate`/`n`/`ci95` 照给。🔴 **样本不足不是 0 也不是空**——它比两个空态**多**给数据而不是少给 |
| **不确定性度量** | 比例类读数（零分率 / 完读率 / 强制收尾率 / 阻断率 / 扣留率）带 **95% Wilson 得分区间**。选 Wilson 不选正态近似：后者在 `p̂` 贴边时区间**塌成一个点**（3 个观察全零分 → 100%，区间 `[1,1]`），偏偏那正是最该提示「这是噪声」的地方 |

**默认 30 的依据**（三条，写进响应的 `sampleFloor.rationale` 里——数会被复制走，文档不会）：
① 最坏情形 `p̂=0.5` 下 95% Wilson 半宽：n=3 → ±0.37、n=10 → ±0.26、**n=30 → ±0.17**、n=100 → ±0.10；
30 大致是「区间首次窄过运营真会据此行动的效应量（两档差 20 个百分点）」的位置。
② n=30 时单个观察最多把比例挪 3.3 个百分点，n=3 时是 33 个百分点——「一条数据翻转结论」在 30 上不再发生。
③ 30 同时是 CLT 的教科书惯例门槛。🔴 这是**默认值不是物理常量**（同 `attentionGiniMax`）：
预注册纪律要求门槛「开测前可改、开测后冻结」。**过了 30 也不等于结论成立**——那是区间要回答的问题。

**基尼为什么不给区间**（判断写在代码 `GINI_CI_NOTE` 与响应的 `ciNote` 里，两处同源）：
① **bootstrap 不做**——要随机重采样，违反本模块的确定性契约（禁系统随机 / 浮点 RNG；
同一份数据必须永远算出同一个数，否则「调参前后差了多少」无从谈起）；
② **jackknife 技术上可做（留一、无随机），但答错了问题**——它重采样的是**分组**，
而身份池 / 结局池是配置出来的**总体**、不是抽样得到的样本；真正的抽样波动在**组内**。
留一组区间会给出一个看起来很权威、实则没人问过的数，比不给更危险；
③ **delta 法**能把组内标准误传播上去，但基尼在并列值处不可导、值域有界、分组数常只有 2-8，
正是正态近似最不成立的区间。**替代方案**：改报样本量本身——`n`（分组数）+ `sampleN`
（门槛盯的观察数：身份维取「最弱那条腿」，结局维取落到真实结局的世界数）+ 极值。
`n=3` 与 `n=300` 因此长得不一样，这就是本批要的效果。锁：
`mean_share_gini_distinguishes_a_handful_of_observations_from_many`。

均值类读数（`meanRelativeShare`）同样不给区间，理由不同：观察**不独立**（同一世界内份额之和
恒等于成员数，且同一世界同时向多个身份桶供数），iid 区间会系统性低估宽度；改给
`n` / `worlds`（聚类数）/ `sd` / 中位数 / 极值。比例类读数的 Wilson 区间也带同一条读法说明
（拍 / 事件按世界聚类 ⇒ 真实区间更宽，**当宽度下界读**）。

🔴 **有区间不等于有判语**：本节**不给**「显著 / 不显著」的布尔，不给「差异是否成立」的结论。
「差 0.1 算不算差」取决于运营在权衡什么，不取决于 p 值。**给区间，让人自己看。**
锁：`confidence_intervals_come_without_a_significance_verdict`（区间对象只许有
`low`/`high`/`method`/`level` 四个键，多一个就是在下结论）。

🔴 **补了样本量不等于校准闭环成立**：本节让「什么时候还不能看这个数」变得可判，
状态仍是 `Implemented`。

🔴 **本节交付的是读数，不是校准闭环的结论。** 两处 `effect.calibrationLoop` 从 `Missing` 改为
**`Implemented`** 而不是更高，是因为七档里 `Implemented` 的含义正是「代码落地并有测试覆盖」：
**能测了 ≠ 已验证配得对**。闭环真正成立要等运营据此调过参、并在下一批世界上看到因果——
那才是 `Validated`。在此之前，本节的任何数字**不得**被表述为「校准闭环已建立」或
「身份池/境界档已调好」。读数本身也拒绝下这个判语：它只给分维度的事实。

## 5. AI 失败安全降级（写入门槛，因公共事实不可回滚）

模型超时/审核阻断/低质输出时，按序：延后此拍 → 使用过审过渡事件 → 缩短上下文重仲裁 →
暂停世界并说明原因 → 人工复核队列。**宁可停拍，不让失败输出成为永久公共事实。**

## 6. 不可逆投入冻结清单（验证通过前不做）

大规模版权采购 · 长期直播平台合作合同 · 大规模人工审核团队 · 创作者结算与税务系统 ·
大规模算力预付 · 对外承诺赛事/经济上线日期。（代码可复用，合同与人力不可回收。）

## 7. 本地端获客飞轮

本地创建角色 → 长期聊天积累人设 → **一键发布云端副本（字段级上传选择权，上传前逐字段展示）**
→ 进入公共世界 → 经历写回本地角色档案。本地隐私承诺不变（见 README「双轨定位」）。
