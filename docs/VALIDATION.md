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
| Batch API（成本工程杠杆③） | **Specified（未实现）· 已挪进 `open-decisions.md` §9（2026-07-28）** —— 逐条核过后它**不是「只差写」**：真正省钱的形态是跨世界同环节合批，那会让世界的推进节奏被同批世界耦合（产品决定，非性能调优）；且唯一的真实 `ModelClient` 是同步的 `HttpModelClient`，没有厂商凭据就**无法验证**（同 §8 与「黄金世界真实录制」）。 | 约 5 折，但与现有同步 tick 管线结构性冲突：`run_round` 是**串行**五环节 + 同事务 `commit_tick`，而 Batch 是分钟~小时级异步；一拍需 5 次批往返、`CLAIM_STALE_MS=300000` 会把等批 worker 判成崩溃重排、中间态无持久化（批途中重启 = 半通管线，违反 §5「宁可停拍」）。改造路径：`crates/muse-engine` 把 `run_round` 改成可挂起/可恢复的分步状态机 + `ModelClient` 增 `submit_batch`/`poll_batch`（默认实现回落同步 `complete`，桌面轨零改动），server 侧加中间态表 + 批次协调器 + 降级回落。完整分析见 `server/src/runtime/mod.rs` 的 `offpeak` 模块头 | — （未实现，无开关） |
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
- ~~`MUSE_CONTAINER_ASSEMBLY`~~ —— **已迁（2026-07-27）**，清单上最后一个被迁的。
  装配期按 **world**（世界已存在，「先在一个房试自定义房装配」是自然单位），建模板前门只能按
  **global**（建模板时没有世界，模板是世界的蓝图）。两侧不同在这里**不会半开半关**：
  全局关时模板根本声明不了 refs，也就没有哪个世界会有 refs 可装。
  ⚠️ 原注「装配期在 `assemble_instance` 内，同样要注意事务边界」**实际不需要**：
  `load_container_cards` 的唯一调用点在 C-7 那次 CAS 占位写入**之前**。
  `validate_container_refs` 收 bool 保持同步，是为了让它继续是**纯校验函数**（可任意复用），
  不是因为事务边界。
- ~~`MUSE_SAFETY_LEXICON`~~ —— **已迁（2026-07-27）**，理由与别的开关都不同。原注判它
  「最后迁或干脆不迁」，依据是①事务内查库风险最大 ②收益最小。重新评估后仍然迁：
  ① **已被解掉**（闸收已解析好的 bool，`commit_tick` 在 `db.begin()` 之前解析——这套做法是
  迁前面几个开关时才成型的，原注写下时还没有）；② **收益被低估**——原注只想到「灰度」，
  而对一个**审核链的急停阀**来说最重要的收益是**留痕**：env 改一行就能关掉全平台的敏感词
  过滤，**没有任何审计记录**；接进体系后每次变更都落 `audit_logs`。
  🔴「谁在什么时候关掉了内容过滤」必须查得到，这本身就是合规主体责任的一部分。
  原注的两个迁移前提逐字执行：只允许 global（写入端点直接 400），且有单独红线用例
  `red_line_lexicon_never_fails_open` 逐条注入损坏态 / 窗口外 / 错作用域 / 查库失败，
  断言每一条都回到「继续过滤」。

**同批交付的一件基础设施**：`FlagDef.scopes` —— 每个开关声明它**实际解析**哪几档作用域
（不是「哪几档合法」）。此前写入端点只校验作用域名合法，于是给一个只读 global 的开关写一条
world 记录会**写得进去、且毫无效果**——而 `admin_api::flags::set_flag` 自己的注释就把这种情形
称作「这套体系最难自查的失败模式」。现在直接 400 并告知它实际读哪几档。
🔵 这条校验上线后**第一个抓到的是仓库里的既有用例**：`set_flag_is_upsert_not_append` 一直在给
`MUSE_ONBOARDING` 写 world 档记录，那是一条永远不生效的记录。

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
~~**其余存量 env 开关接入运行时开关体系**~~ —— **已完成**：登记表里的存量 env 开关**全部迁完**（含原判「不迁」的审核链，理由重估见上）·
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
>
> ⚠️ **上面这句已过期**（更正于 2026-07-28）：身份维读数**早已上线**——
> `slo::calibration` 的 `identityShareBalance`（按身份 id 分组，给「相对均分倍率」均值与零分观察数，
> 随身带观察数 n / 最小样本量 minN / 95% 置信区间），出口在
> `GET /admin/metrics/overview` 的 `narrativeSlo.calibration.dimensions.identityShareBalance`。
> `admin_api::calibration` 那个端点的 `effect.calibrationLoop` 也早已是 `Implemented`。
>
> 🔴 这句过期的话**不是无害的**：它是「还差什么」清单的一部分，有人（包括我）会照着它去补，
> 于是差点造出同一读数的第二份实现——那正是本文件反复在修的缺陷形态。
> 现已加源码级闸门 `slo::tests::validation_doc_mentions_every_calibration_dimension`：
> `slo::calibration` 里每多一个维度读数，本文件没提到它就 CI 红。

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

> ⚠️ **三项里前两项都已兑现**（更正于 2026-07-28）：第一项见下面那条 2026-07-27 更新；
> **第二项（境界维接 `slo/`）同样早已上线**——`realmTierWorldQuality`（「戏服维：境界档 × 世界质量」，
> 跨世界分桶对比，三指标复用 `slo::quality` 与仿真试跑算同一个数）。
> 只有第三项「题材 → 审核档位」仍未做，且它卡在**该写成什么**（哪个题材对应哪档审核是产品决策），
> 不是卡在怎么写——见上文 `genre: "history"` 那条标着 `Concept` 的注。

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
> **仍未做**：~~把境界维接进 `slo/` 形成校准闭环~~（**已上线**，`realmTierWorldQuality`，
> 更正于 2026-07-28）；题材 → 审核档位的真实联动（卡在「该写成什么」，是产品决策）；
> if 线是否继承原世界那件戏服（当前恒 `None`，与接线前一致——同样是产品决策）。

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

1. ~~**推进端点在请求内同步调用模型**~~ —— **已改（2026-07-27，migration `0050`）**。
   原记的「不做」理由是「加一条独立 worker 循环会显著放大改动面且需单独评审」，那条已不成立：
   同一形态（独立 topic + 独立 worker 池 + 池大小即成本闸）在 §15 第 3 层与其补偿轮询上
   已跑过两遍。而问题本身是真的：一次回合几十秒起步，中间占着 HTTP 连接——代理超时、
   客户端重试、移动网络切换，每一样都会把「已经烧掉的 token」变成「玩家看到失败」，
   而 if 线是**付费**内容。
   - **契约翻成 202 + 轮询**，且直接翻、不加开关并存两条路：if 线默认关闭、玩家端**一行都还没接**
     （全仓 `src/` 搜不到 `iflines`）。要改契约，此刻代价最低；同一端点维持两种返回形态才是真正的债。
   - 🔴 **「玩家拉动」这条设计没变**：入队只由玩家点击触发，没有任何调度器会碰 `ifline_worlds`
     （源码级红线 `red_line_no_scheduler_ever_touches_iflines`，并钉住「入队点只有一处」）。
   - 🔴 异步化**新引入**两个失败形态，连同一起做的：① 同步版本当场返回的拒绝理由改成异步后
     没有去处 → 落 `last_error` 随读取面下发（不落就是静默失败）；② 重复点击此前由
     `(ifline_id, beat_no)` 唯一键天然承担，异步后会排两份烧两遍 → 请求层 CAS 闸。
   - 🔴 那道闸自己又会引入一个**比原问题更糟的死锁**：`MemQueue` 不持久，进程重启带走任务而
     标记已写下，只认 `= 0` 的话这条**付费**的线永久推不动。故 CAS 恒带陈旧线一支
     （`MUSE_IFLINE_ADVANCE_STALE_MS`，默认 10 分钟）。⚠️ 陈旧线只是**让玩家能再点一次**，
     不是把丢掉的那次补上——对账式重投（§15 第 3 层 `sweep` 那种形态）if 线尚未做，见下条。
   - 🔵 端点这一层此前**几乎没有覆盖**：既有用例全是直接调 `runner::advance_one_beat`，
     所以整个同步契约被翻掉时一条都没红。补了 6 条，逐条钉异步化新引入的失败形态。
     六处故障注入中有两处第一轮漏网（成功路径没覆盖 / 断言恒真），均已补。
1b. ~~**if 线没有对账式补偿**~~ —— **已闭合（2026-07-27，migration `0052`）**。
   陈旧线保证玩家不被锁死（安全底线），对账保证「玩家没再点也能补上」（体验），
   0050 那批先做了前者，现在补后者：`ifline::sweep`，默认关闭（`MUSE_IFLINE_ADVANCE_SWEEP`）。

   判据比 §15 第 3 层那个 sweep 简单得多——**不需要**对账 `beat_count` 与 `ifline_beats`，
   因为 0050 已经把出口收严了：`run_advance_job` 的**任何出口都清 `advance_requested_at`**。
   于是「标记还在且已过补投窗口」⇒ 这次推进确实丢了、且没人会重试它。三种成因
   （进程重启 / 读行失败早退 / 收尾写库失败）归到同一条判据，动作也相同，不必区分。

   - 🔴 **补投必须有封顶**（`0052` 加 `advance_sweep_count`）：补投是**真的调模型**（付费内容、
     真 token）。worker 若每次都在清标记前就死掉（panic / OOM / 被 kill），「查到就补投」
     会变成**无限烧钱的循环**，而且每一轮看起来都很正常。§15 第 3 层不需要这一列，是因为
     它补投用 `attempt = MAX+1`，既有重试预算天然封顶；if 线没有尝试台账（失败那次压根不落行）。
   - 🔴 **到顶不是静默放弃**：清标记（玩家立刻能再点，不必再等陈旧线）+ 把原因写进
     `last_error`。静默放弃会把「补偿机制」变成一个更难查的静默失败——那正是 0050 在防的东西。
   - 🔴 **补投窗口恒 ≥ 请求层陈旧线 + 1 分钟，由代码保证**（`sweep_after_ms()` 取 `max`），
     不靠运营记得配对。配小了会对**仍在跑**的任务补投：唯一键挡得住第二次落库，
     但那次模型调用是真烧掉的。
   - 🔴 **它不是调度器**：判据恒为 `advance_requested_at > 0`，而那一列**只由玩家点击写下**。
     既有源码红线 `red_line_no_scheduler_ever_touches_iflines` 顺势扩展——原本断言
     `push_json` 只有一处，现在两处（端点 + 补投）各自说得出理由，并额外钉住补投的取数判据
     必须带 `advance_requested_at > 0`。行为面另有一条用例：玩家没点过的线，多久都不补投。

   六处故障注入实测均变红（去掉「玩家点过」判据 → 行为红线与源码红线**同时**红 /
   拿掉封顶 / 封顶时静默 / 窗口不钳 / 落定不清零计数）。

   ⚠️ 写用例时先挂死过一次：`MemQueue::pop` 在空队列上**无限等待**（它是给 worker 循环用的），
   拿它直接断言「没有入队」会把用例挂死。helper 已改成带超时，并把这条记在注释里。
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

> 📄 **上面这些遗留里,凡是「不知道该写成什么」而不是「不知道怎么写」的,已抽进
> `docs/build/open-decisions.md`**。把两者混在一起会让「还没做」与「还没定」看起来是
> 一回事——前者是欠账,后者是等一个人拍板,处理方式完全不同。

### 3.6.1 未成年人保护的判据收归一处（2026-07-27 · 真红线 §0.4）

> 用「靠注释维持的重复判据」这条线索扫全仓，扫到的第一处不是别的，是**未成年人保护**。

`users.age_declared` 的「已声明成年」判定此前在**六个模块**里各写了一遍：
`worlds`（生死状入场门）· `invitations`（未成年不收生死状邀请）· `social`（青少年模式限真人社交）·
`livestage`（弹幕成年门）· `ledger`（未成年创作者不分账）· `billing`（保守拒充）。

六处**行为恰好一致**，但那是巧合维持的：三处硬编码字面量 `1`，两处各自定义了一份
`AGE_DECLARED_ADULT` 常量，还有一处 SQL 文本略有不同。

🔴 **一条真红线靠六份手抄保持一致**。只要有一处被改成 `!= 2`（看起来更"直觉"：
不是未成年就放行），**未声明年龄的账号立刻全部变成成年**——而那个模块自己的用例照样绿。

已收归 `auth::is_declared_adult`：

- **一份实现**，泛型 `Executor` —— `ledger` 在事务里判、其余在池上判，写成「池版 + 事务版」
  两份就又是一处靠注释同步的重复判据（`memorial` 的死亡判据正是那样，且已被故障注入抓到过）。
- 🔴 **fail-closed 三条**：行缺失 / 取值不是 1（含将来新增的取值）/ **查库失败** → 一律不放行。
  白名单不是黑名单；一次数据库抖动不该把未成年保护整个关掉。
- ⚠️ 只回**判据**不决定后果：`worlds`/`social`/`livestage`/`billing` 用它拒绝，
  而 `ledger` 用它把创作者分成全额留在平台（不是拒绝交易）。后果各归各的调用点。
- 源码级红线 `red_line_only_one_place_reads_the_age_declaration` 扫全仓生产代码，
  再长出第二处读取即红。

🔵 **那条红线用例上线当场抓出第 6 处**（`billing`）——我自己按 SQL 字面量扫的那一遍漏了它。
四处故障注入全部被抓：改成 `!= 2` · 行缺失当成年 · 查库失败当成年 · 某模块又抄一份。

### 3.6.2 「曾经开过吗」判据收归一处 + 源码级扫描器收归一处（2026-07-27）

沿同一条线索扫到的第二处：**`entry_ever_open` 在四个模块里各写了一遍**
（`annotations` / `ifline` / `livestage` / `social`），四份都**绕过 `flags` 直接查
`runtime_flags`**——等于在「开关的唯一读取入口」旁边开了四条旁路。
其中一处的文档还明写着「口径逐字抄 `annotations::entry_ever_open`」，
**手抄被当成了实现方式**。

🔴 它决定的是两件**不会立刻显形**的事：
- **指标诚不诚实**：三态判定（`entry_not_open` / `no_data_in_window` / `ok`）。
  漂移了指标照样出数，只是那个数是假的——而 T1/T5 门槛要拿它决定继续/调整/停止。
- **审核闭环开不开得了**：按世界灰度开着时弹幕/举报会真实落库，而按全局解析会把运营面判成
  404——弹幕进得来撤不下去、举报进得来处置不进去。漂移了运营面照样 404，只是本该可见。

已收归 `flags::entry_ever_open(db, flag)`，并加源码级红线
`red_line_only_flags_queries_the_runtime_flags_table`（豁免 `flags` 自身与 admin 的开关 CRUD 面）。

**顺带收归的第二件事：源码级扫描器本身。**

写上面那条红线时它当场误报了 `assembly/mod.rs`——因为既有扫描器按 `"\nmod tests {"`
截断，而本仓库的内联测试模块**不止叫 `tests`**：`app::cors_tests`、
`assembly::sampling_tests` / `container_tests` / `member_order_tests` 都是。
按名字截断的扫描器会把这些文件的**测试代码当成生产代码扫**。

已把扫描逻辑收进 `testkit::production_sources()`：按**花括号配平**剥离
`#[cfg(test)] mod X { .. }`。⚠️ 也**不能**按「第一个 `#[cfg(test)]`」截断——
本仓库有若干测试专用夹具（`InvitationSwitch` / `ContainerSwitch` 等）定义在文件中段，
其后仍有生产代码，截断会让扫描形同虚设。

🔵 故障注入连扫描器一起验：把它退回「按 mod tests 截断」→ 红线立刻红。

### 3.7 server↔engine 的 JSON 边界：手读字符串键的地方（登记于 2026-07-27）

> `narrative_state_json` 是 server 与 `muse-engine` 之间**唯一靠 JSON 传递、且部分消费方
> 按字符串键手读**的边界。手读的地方**没有编译期检查**：引擎侧改一个字段名，读侧看到的
> 只是「取不到值」，而各自的用例都会绿——两边各按自己的假设测了自己那一半。

| 消费方 | 读法 | 失败方向 | 现状 |
|---|---|---|---|
| `events::world_state_summary` | **类型化**（`serde_json::from_str::<NarrativeState>` 后访问 `rel.known_to`） | 编译期就挡住 | ✅ **这是正确范式** |
| `memorial`（死亡证据 (b)） | 手读 `narrative.pendingConsents[].{subject,eventKind}` | 🔴 **fail-open**：键不存在与「已落定」无法区分 → 卡仅凭同意就被封卷 | ✅ 已用真实引擎类型钉住（4 条用例）+ 两份拷贝同步的源码级不变式 |
| `social::bond_between` | 手读 `relations[].{from,to,trust,affinity,fear,debt}` | 🔴 **fail-open 且撞隐私红线**：字段漂移 → 全读 0.0 → `hostile=false`，一票否决静默失效，而 `died_together` 是**独立**解锁路径仍会放行 ⇒ **敌对线的两人能互解真人身份**（违反 §14） | ✅ 已用真实引擎类型钉住（3 条用例） |

**为什么不直接把手读改成类型化**（那样结构上就没有这个问题）：两者的**容错语义不同**。
手读是逐字段宽容的（缺一个字段只影响那一个维度）；类型化是整体严格的——
`RelationState.trust` 等没有 `#[serde(default)]`，一份缺字段的历史/异常状态会让**整次反序列化失败**，
于是所有关系一起消失。在隐私判定这条路径上，把「少读到一个维度」升级成「看不到任何关系」
是另一种事故。故本批次选择**用测试钉住线上形状**，而不是改容错语义；
真要改成类型化，需要先给引擎侧字段补 `#[serde(default)]` 并单独评审。

🔵 三处故障注入实测：给引擎侧 `trust` / `fear` 改名 → 立刻红。
⚠️ 而给 `RelationState` **去掉 `rename_all`** 不会红——那是**正确的**：它读到的几个字段都是单词，
camelCase 与 snake_case 同形。`known_to` 才受影响，而那一处是类型化读的。

### 3.8 运营→server 的 JSON 边界：`skeleton_json` 的顶层键（**落地于 2026-07-27**）

§3.7 记的是 server↔engine 那条边界。同一类问题在**另一条**边界上更严重，因为那一侧
写 JSON 的是**运营的人**，而不是编译器管着的代码：`world_templates.skeleton_json`。

**在此之前，没有任何一处知道这份 JSON 的合法顶层键集**——一半在 `assembly::Skeleton`
（20 个字段，`Deserialize` + camelCase，模块私有），另一半散在手读点：`runtime` 按字符串键读
`endgame`（min/maxWorldTicks，**决定世界何时结束**）与 `forbiddenPredicates`（**禁止谓词**）。
两边互不认识对方的键。于是拼错一个顶层键的后果是**全程零报错**：

| 拼错的键 | 静默后果 |
|---|---|
| `mainlineNodes` | 无主线大纲；`chapters::mainline_node_count` 归 0 → 通关判定退化 |
| `endgame` | 世界结束条件退回默认，运营设定的场次长度失效 |
| `forbiddenPredicates` | **禁止谓词失效，失败方向是放行** |
| `payoutTable` / `identityPool` | 产出表 / 身份维度整个消失 |

⚠️ 这**不是假想**：`admin_api::tests::template_create_and_review_flow` 里当"合法模板"用了很久的
骨架是 `{ "mainNodes": [], "endings": [] }`——两个键都是编的（真名 `mainlineNodes` / `endingPool`），
却一路 200 建成模板、全绿通过。加上这道闸的当天它就红了。

**做法**：`assembly::SKELETON_TOP_LEVEL_KEYS` 成为唯一的合法键清单（含两个非 `Skeleton` 的手读键，
各带注释说明谁读），`validate_skeleton_refs` 增加第 0 段——未知顶层键 → 建模板期 400 并点名，
带「是不是想写 X？」的归一化建议（去大小写与下划线，故 `mainLineNodes` / `mainline_nodes` 都能指回）。
`__` 前缀（`__doc` 注释键）豁免。**拒绝而非警告**：没人读的键只可能是拼错或残留。

🔴 **第 0 段必须在「解析不出结构化骨架就放行」那条既有防御分支之前**，否则同时写错类型的骨架会
绕过它——而写错一处的人通常不止写错一处。用例 `top_level_key_check_runs_before_the_lenient_parse_bailout`。

🔴 **刻意不用 `#[serde(deny_unknown_fields)]`**：那会让整个 `Skeleton` 解析失败，反而触发上面
那条防御分支、把校验整体关掉——比现状更糟。

**三条写入路径盘了一遍**（`INSERT INTO world_templates` 的全部生产码出处）：
`POST /admin/world-templates` 与 `POST /assets/worlds`（创作者发布）本来就调 `validate_skeleton_refs`，
自动获得这道闸；`onboarding::microworld::ensure_template` **直接 INSERT、不过闸**，
故补了等价用例 `microworld_skeleton_would_pass_the_create_template_gate`——微本骨架由代码生成、
只有取值受 env 参数化（键集是静态的），用例钉住即够，不必付运行时校验的代价。

**存量模板不会被回头校验**（闸是本次才加的）。它们的唯一发现途径是运营台的骨架形状表新增的
`shape.unknownTopLevelKeys`（`GET /admin/sagas/{sagaId}`）——因为这类错的症状与「运营就是没写主线」
完全一样：一排 0、零报错，两者必须在同一屏上可分辨。

**再下一层：`mainlineNodes[]`（同一批次补上）**。这一层的知识裂得比顶层更开——
`struct MainlineNode` 只认 `id` / `fated` / `variantGroup` / `arcTags`（装配层要的），
而 `runtime` 从**同一批对象**上手读另外四个：`summary` / `constraint` / `threshold` / `advanceWhen`。
两侧各测各的一半，都绿。失败方向全部朝坏的一边：

| 拼错的键 | 静默后果 |
|---|---|
| `constraint` | 落到 `_ => Soft` 分支 ⇒ **本该 `hard` 的硬约束静默降级** |
| `advanceWhen` | 推进谓词悄悄消失，节点退回纯阈值门 |
| `threshold` | 里程碑节点变回普通节点 |
| `summary` | 大纲节点文本为空——模型拿不到这个节点是干什么的 |
| `fated` / `variantGroup` | 宿命节点不再宿命 / 互斥失效，同组变体可能同时出现 |

**第三层：`forbiddenPredicates[]`（同批次）**。它**没有任何类型化读者**——`Skeleton` 压根没这个字段，
只有 `runtime` 手读 `id` / `expression` / `reason` 三个。失败方向是三层里最坏的：`expression` 拼错 →
`let (Some(id), Some(expr)) = … else { continue }` ⇒ **整条禁止谓词被丢弃**，世界照常开，
那条内容约束从来没生效过，且没有任何日志。

**最后收成一张表：`assembly::SKELETON_KEY_SETS`（按路径索引，覆盖全部 32 层）。**
路径语法 `""` = 顶层、`a.b` = 对象字段、`a[]` = 数组元素，例如 `payoutTable.worldlineTiers[].item.origin`。
建模板期校验（`unknown_skeleton_keys` 取第一条拒请求）与运营台发现面（取全部给运营看）
**共用同一份遍历实现**。各抄一份的话，加一层就得记得改两处，而漏改的表现是
「运营台说这个模板没问题」——那正是本节在修的缺陷，不能在修它的提交里再造一个。

**未登记的路径不校验，不是拒绝。** 这是一道会拒请求的闸，对着看不懂的结构乱拒是生产事故；
漏检一层只是维持现状、不构成回归。漏登记改由**源码层**变响，见下。

登记时又撞见同一形状两次，一并收进并集：
- `sampling`：装配层 `SamplingSpec` 认五个「每副本抽多少」，创作者发布端
  `assets::worlds::SamplingView` 认 `redundancyRatio`（超集冗余门），两边都不知道并集
  ——这条是**上线注册表时被仓库里的真实测试骨架当场打出来的**，不是看出来的。
- `storylines[].summary`：`StorylineSpec` 里根本没有这个字段，只有 `world_scan_text` 读它。

「是不是想写 X？」的建议**用编辑距离**（阈值 `max(1, len/4)`），不只归一化：`constrait`
（漏一个 `n`）归一化后仍不等于 `constraint`，而漏 / 多 / 错一个字母恰是最常见的拼法错误。
没有这个提示，报错就只是「这个键没人读」，运营还得自己猜正确写法。

**两条源码层红线是这套东西的护栏**——没有它们，`SKELETON_KEY_SETS` 就是「靠人记得更新」的表，
而「记得更新」这个假设正是它下面每一层缺陷的共同成因：

| 红线 | 防什么 | 故障注入实测 |
|---|---|---|
| `registered_key_sets_cover_every_struct_field` | 给结构体加字段忘了加注册表 → 用了新字段的**合法模板会被拒** | 给 `SamplingSpec` 加一个字段 → 红 |
| `every_skeleton_struct_is_registered` | 加了一层嵌套结构体忘了登记 → 那层**不被校验、拼错继续静默且无征兆** | 给 `Skeleton` 加一个新结构体字段 → 红 |

红线的 schema 源码取自**三处**：`assembly/mod.rs`、`admission/mod.rs`、
以及 `crates/muse-engine/src/narrative/types.rs`（`include_str!` 跨 crate 读）。
后两处是跨模块 / **跨 crate** 契约——正是 §3.7 那一类「引擎侧改个字段名，server 只表现为读不到值」。
实测：把 `requiredEffectTags` 从注册表摘掉，红线点名 `LocationGate` 的这个字段，
证明跨 crate 那一路是真的在读引擎源码。

解析器**不假设**「线上名 = 字段名的 camelCase」：显式 `#[serde(rename = "X")]` 优先
（引擎里已有一处这样的字段，当前从 `Skeleton` 不可达）。实测给可达结构体加 `rename` → 红线报的是
重命名后的名字。

⚠️ **仍存的边界（别读成"拼错已经不会发生了"）**：
- **存量模板不会被回头校验**，闸只在写入路径上。存量的发现面是运营台的
  `shape.unknownTopLevelKeys` / `shape.unknownNestedKeys` 两栏——**看得见，但不会自动修**。
- `worldCharacters[].card` 刻意**不下钻**（`SKELETON_OPAQUE_PATHS`）：它的 schema 是引擎的
  `CharacterCardV2`，拿骨架这一侧的键集去校验只会把合法角色卡拒掉。
- 红线读的是**源码文本**，不是类型系统。它挡得住加字段 / 加结构体 / 显式 rename，
  挡不住把 schema 挪进一个它没读的文件——那种情况下 `struct_fields` 找不到该结构体会直接 panic 报名，
  也是响的，但要靠人看懂那句提示。

⚠️ 另一条边界：**存量模板不会被回头校验**，闸只在写入路径上。存量的发现面是运营台的
`shape.unknownTopLevelKeys` / `shape.unknownNestedKeys` 两栏——**看得见，但不会自动修**。

### 3.47 工程侧欠账表：这一轮**留下了什么没做完**（**2026-07-28**）

前面 14 节记的是「查出什么、改了什么」。这一节反过来记**没做完的**——
把散落在各节里的「⚠️ 没验到」「本轮没做」「恒 `#[ignore]`」逐条收拢成一张可核对的表。

⚠️ 建这张表时**当场抓到了它本该抓的第一件事**：§3.41 实现了礼物注入，
而**三处声明还在说「没做」**——`arena` 模块头 seam、`arena` 行内注释、
`STARTUP` §9 seam 清单（那条甚至明写「`RoundInput` 现有字段里没有环境事件位」，
**那句话已经是错的**）。三处已订正。
这正是本仓反复吃亏的「会过期的清单」：**一条过期的声明比没有更糟**——
它会让人以为还有事没做，或者更糟：以为那条红线还没被处理过。

#### A. 工程欠账（我能做，本轮没做）

| # | 欠什么 | 出处 | 关掉它需要什么 |
|---|---|---|---|
| ~~A1~~ | ~~「绝不 blanket 标 `applied_tick`」没有端到端用例~~ | §3.41 | **已关（同日）**：见下方「A1 的关法」 |
| ~~A2~~ | ~~`write_atomic` 的 rename 失败清理路径没验到~~ | §3.35 | **已关**：把目标路径建成**非空目录**，rename 必然失败——不需要跨文件系统 |
| ~~A3~~ | ~~卡录入时不该产出 1-2 字的 secret~~ | §3.44 | 🔴 **这一条本身写错了，已撤**：见下方「A3 撤回」 |
| A6 | `narrative.pacingNotes` **无界增长**（每回合每个 outcome 追加一条，永不清理、无上限） | 本节 | 定一个上限或滚动窗口；先确认它值不值得改（见下方「A6 的实情」） |
| ~~A4~~ | ~~`loadedPath` 是纯纵深防御，注入证明不了它在挡什么~~ | §3.38 | **已关**：判据早就抽成了纯函数 `shouldPersist`，**直接调它**即可，不必构造慢重渲染 |
| A5 | `admin/` **没有任何测试基建**（只有 tsc） | §3.37 | 引入 vitest —— **要加依赖，属决定不是实现，不擅自做**。⚠️ 但它的**具体风险**已用不加依赖的办法关掉，见下方「A5 的部分关法」 |

#### A3 撤回：**这条欠账的前提是错的**

写 A3 时我假设「1-2 字的 secret 是作者在卡里写的」，于是提议「在卡发布面加长度校验」。
去做的时候才发现：**`CharacterCardV2` 根本没有 `secrets` 字段**。
`CharacterState.secrets` 只可能由 `StatePatch` 写入（`reducer` 路径白名单里
`characters.<id>.secrets` 可写），而 §3.34 已经查实
**`build_patch` 是模型派生操作的唯一生产者**——它压根不写 `secrets`。
世界装配（`runtime`）也不种它。

也就是说：**生产路径上没有任何地方会产出一条 1-2 字的 secret**。
§3.44 那道长度闸仍然该有（它挡的是「万一有」，且代价为零），
但「再加一道录入期的门」是在给一个**不存在的入口**装锁。

🔵 这是欠账表的第一次自检，而它抓到的第一件事是**表里有一条是我编的**——
不是恶意，是写表时把「合理的推测」写成了「待办」。
教训直接写进表的用法：**每条欠账都要带出处，而出处要能被走一遍**。
A3 走了一遍就塌了；A1 走了一遍就关了（见下）。

#### A6 的实情：`pacingNotes` 每拍必然增长，但先别急着改

`build_patch` 每回合对**每个 outcome** 追加一条 `narrative.pacingNotes`，
**永不清理、无条数上限**。一个跑 500 拍、3 个角色的世界会攒下约 1500 条。

⚠️ 但先把影响说准，别把它说得比实际严重：

- **不进 prompt**。`assemble_visible_context` 只取 `state.world` 与角色自己那一格，
  `state.narrative` 整段不进上下文——所以它**不吃 token**。
- 影响的是 `worlds.narrative_state_json` 的体积，而那一列**每拍全量读写**。
  按上面的量级是几十到上百 KB，真实但不致命。
- endgame 策略有 max ticks 封顶，所以增长有天花板，不是无限。

故本轮**不改**：改它要定「保留多少条 / 按什么滚动」，而那个数没有依据——
和 §1 的阈值一样，属于「等真实运营数据」而不是「等写代码」。
先登记，等有了跑够拍数的真实世界再看它的实际体积曲线。

#### 🔴 A2 / A4 的共同教训：**「造不出来」我说了三次，三次都是错的**

| 欠账 | 我登记时写的 | 实际的构造 |
|---|---|---|
| A1 | 「要造跑完整一拍并在中途插入礼物的夹具」 | `AMBIENT_MAX` 本身就制造了「没喂进去的礼物」 |
| A2 | 「需要跨文件系统或权限构造」 | 把目标路径建成**非空目录**，rename 必然 EISDIR |
| A4 | 「需要能让重渲染慢于 800ms 的构造」 | 判据早就抽成了纯函数 `shouldPersist`，**直接调它** |

三次都不是「做不到」，是**没走一遍就下了结论**。而 A3 走一遍是塌了（前提就错）。
四条欠账走了四遍：**三关一撤，零条真的做不到**。

这条比那四个用例本身重要：**「这个测不了」是一句需要证据的断言，不是一句可以顺口说的话。**
写欠账表的价值不在记下来，在**逼自己回头走一遍**——而走一遍的成本，
远低于让一条错误的「做不到」在表里躺着。

#### 🔴 顺带查出并修掉：我自己弄坏了桌面轨的编译

做 A2 时 `cargo test src-tauri` 直接编译失败：**§3.41 加 `RoundInput.ambient_events` 时
漏了桌面轨的构造点**（`src-tauri/src/commands/narrative.rs`）——引擎和 server 都改了，
桌面壳没有。

⚠️ 更该记的是**为什么隔了这么久才发现**：那之后的几次验证我跑的是
「engine + server + clippy」，**没跑 src-tauri**——因为改动"看起来"只碰了平台轨。
CI 的 `rust-test` job 会拦住它，但那要等推上去。
教训：**跨 crate 的类型改动，验证面必须回到全量**，不能按"我改了哪里"裁剪。

已补上（桌面轨恒 `Vec::new()`——本地单机没有观众，不是"暂未接线"）。

#### A1 的关法：**不需要「中途插一条礼物」的夹具**

原以为要造「跑完整一拍并在中途插入礼物」才测得了，其实不用——
`AMBIENT_MAX` 本身就制造了这个情形：**播种超过上限的礼物，超出的那些必然没进这一拍的上下文**。
若提交时 blanket 标记，它们会一起被标上；正确实现下它们必须原样留着 `NULL`。

🔵 这一条值得记下来的不是结论而是过程：**「造不出来」有时只是没找对构造角度**。
把欠账写进表里的好处正是这个——回头再看一眼，往往就看见了。

🔵 故障注入两处全红：改成 blanket 标全部未落地行 → 红；一条都不标 → 红。

#### A5 的部分关法：门钉在服务端，不加依赖

A5 的本体（给 `admin/` 加测试框架）是一次依赖决定，**不擅自做**。
但它真正的风险是**前后端状态集漂移**：§3.36 把状态扩到七态、§3.37 后台才第一次渲染它们，
于是「服务端单方面加一个状态」会让界面上冒出原始英文码（`no_data_yet` 而不是「至今零样本」）
——不是崩溃，是**悄悄变难读**，最不容易被发现的一类退化。而 `tsc` 拦不住它（运行期字符串查表）。

新用例 `every_status_the_server_can_emit_is_known_to_the_admin_console`：
从**服务端源码里实际出现的状态字面量**出发（不手列表），`include_str!` 读后台源码解析
`SLO_STATUS_LABEL`，逐个要求后台认识。服务端新加状态就红——遗漏往红的方向失败。
手法与 `admin_slo_table_reads_only_fields_the_server_actually_sends` 同源。

🔵 故障注入三处全红：服务端加一个后台不认识的新状态 → 红；后台删掉一条标签 → 红；
后台那张表被改名（解析口径失效）→ 红。

#### B. 等凭据（代码已就位，跑不了）

| # | 用例 | 等什么 |
|---|---|---|
| B1 | `model::real_provider_smoke`（恒 `#[ignore]`） | 任一 OpenAI 兼容 API Key。**本仓至今一次真实模型调用都没发生过** |
| B2 | `runtime::record::record_golden_world_with_real_model`（恒 `#[ignore]`） | 同上。黄金世界目前全是脚本替身录制 |
| B3 | 内容审核 Dev 桩 → 真防线 | 审核服务商账号 |
| B4 | L2/L3 立绘 / 图像审核 | 图像生成 + 审核服务商账号 |

🔴 B1/B2 的意义要说清楚：`HttpModelClient` 是全仓**唯一真实**的 `ModelClient` 实现，
另外 8 个全是测试替身或包装器。「OpenAI 兼容分支组装得对不对、鉴权头对不对、
响应解析对不对」目前**全靠读代码**。

#### C. 等产品拍板（`open-decisions.md` 剩 5 项）

不在这张表里复述——那份文档是唯一权威，复述必然漂移。
本轮已核销 §3（Saga 粒度）、§5（礼物注入），另定了 §1 的维度与 §2 的 debt 口径。

#### 这张表**自己**的失效方式

它会过期。所以：**关掉一条就删一条**，不要打勾留着——
勾过的行会慢慢变成噪声，而噪声堆到一定量，整张表就没人看了。
判断一条是否还成立的办法，是去看它指向的那一节（每行都带出处）。

### 3.46 发明那道闸的模块

### 3.46 发明那道闸的模块，自己另外两条规则没用它（**2026-07-28**）

顺着 §3.44/§3.45 那条线索把引擎里**所有**对模型自由文本做字面/正则匹配的地方扫了一遍。
第四、五处在 `arbiter` **自己**身上。

#### 缺陷

`arbiter` 是这道闸的发明者——R7 的 `negated_before` 注释标题就叫「**误伤控制闸③**」。
但同一个模块的另外两条规则没用它：

| 规则 | 模型可能写的 | 判成 | 后果 |
|---|---|---|---|
| R1 资源 | 「我**不**动用那把断刃」「他**没有**拿出令牌」 | `Invalid(rule:resource)` | **这一拍的行动被吃掉**，而他说的恰恰是不做 |
| R3 读心/强制 | 「我**不会**逼他说出秘密」「**绝不**窥探他的内心」 | `Invalid(rule:mind_control)` | 同上 |

🔴 这是 §3.8.1 形态 (a) 最隐蔽的一种：**不是两个模块各写一份，而是一个模块里
做对了一处、漏了两处**。「有没有别人也需要这个判断」比「这个判断写得对不对」更难想到。

#### ⚠️ R5 **刻意不加**，理由必须写下来

`threatens_hard`（硬节点威胁）同样是 `is_match` 无否定闸，但它**不改**：
它的作用是把可疑决策**交给模型层判**，而不是自己下结论。
多匹配 = 多一次模型调用（成本），少匹配 = 硬节点失去保护——**两个方向不对称，宁可多送**。
R1/R3 与它的区别是「命中即 Invalid」，误判直接吃掉一个行动。
不写下这条区别的话，下一个人会把闸「顺手补齐」，而那会让硬节点保护变松。

#### 判据收敛成**全 crate 唯一一份**

`affirmative_match` 从 §3.45 时放在 `relation_dynamics` 的本地副本，
本节挪进 `arbiter`（与 `negated_before` 同处）并开 `pub(crate)`，
`relation_dynamics` 改为调用它。四个调用点，一份实现。

⚠️ 这不是洁癖：四处各写一份的话，否定字表与窗口长度迟早漂开，
**而漂开的那一刻没有任何症状**——这正是本轮反复遇到的那个形态。

🔵 故障注入四处全红：R1 去掉否定跳过 → 红；R3 退回 `is_match` → 红；
`affirmative_match` 改成「整句有否定就全放过」（过度收紧）→ **反向用例**红；
R1/R3 整个关掉（把两条规则拆了）→ **反向用例**红。
⚠️ 有一次注入的基线跑了个空过滤器（`cargo test -- a b` 一条都没匹配上，显示
「0 passed; 307 filtered out」而我差点当成绿），已按单名过滤重跑。

### 3.45 「我永不背叛你」被读成了关系破裂（**2026-07-28**）

扫引擎 `relation_dynamics`（关系演化）。这是**同一个家族的第三处**，而且这一处
不是误伤——是**把意思读反**。

#### 缺陷

关系分类是纯关键词匹配（`is_match`）作用在 `action + intent` 自由文本上，
而规则里全是**单字**关键词：`杀` `伤` `挡` `逼` `骗` `救` `帮` `送`……
`action`/`intent` 是模型自由输出，否定式在里面极其常见。于是：

| 模型写的 | 判成 | 后果 |
|---|---|---|
| 「我**永不**背叛你」 | Rupture（**最强档**） | trust/affinity **各 −0.50 双向** + fear +0.15 |
| 「我**不会**伤你」 | Hostile | 双向减分 + fear |
| 「**拒绝**攻击王五」 | Hostile | 同上 |
| 「**别**杀他」 | Hostile | 同上 |

关系数值**跨拍累积**，且下游连着羁绊解锁门（`server::social`，§3.39 刚动过）、
传世印记、社交可见性。一次误判的 Rupture 要许多次友善行为才抵得回来——
而它的起因是一句立誓。

#### 判据**复用** `arbiter`，不另写一份

`arbiter` 早有这道闸（**误伤控制闸③**：「命中片段之前 N 个字内若出现否定字，
视为角色正在**拒绝**做这件事」）。这里把 `negated_before` 开成 `pub(crate)` 复用，
**没有抄一份否定字表**——抄一份的话，两处的字表与窗口长度迟早漂开，
**而漂开的那一刻没有任何症状**（§3.8.1 形态 (a)）。

⚠️ 语义是「**只要有一处命中未被否定就算命中**」，不是「文本里有否定就整句放过」：
「他背叛了我，我不会背叛他」仍然是破裂——**前半句是真的**。这一条由单独用例钉住，
因为过度收紧和原缺陷一样是错的，只是方向相反。

#### 三处同源，一处对两处错——这个统计本身是结论

| 模块 | 单字/短片段字面匹配 | 长度闸 | 否定闸 |
|---|---|---|---|
| `arbiter::screen_bottom_lines` | 有 | ✅ `MIN_FORBIDDEN_CHARS` | ✅ `negated_before` |
| `continuity` I1（§3.44） | 有 | ❌ **缺**（已补） | 不适用（比对的是私密原文） |
| `relation_dynamics`（本节） | 有 | 不适用（正则枚举） | ❌ **缺**（已补，复用） |

**同一类判断在三个模块里各写了一遍，只有最早那个把闸配齐了。**
这就是为什么本轮一律选择「把判据挪成共用」而不是「再写一个正确的副本」。

🔵 故障注入三处全红：退回 `is_match`（改动前形态）→ 红；改成「整句有否定就全放过」
（过度收紧）→ **反向用例**红；`classify` 恒 `Neutral`（把关系演化整个关掉）→ 红。
⚠️ 第三条第一次注入**只杀了 `rupture` 分支**（`hostile` 那个 `else if` 仍可达），
用例照常绿——那次注入证明不了任何事，已重做。

### 3.44 一个字的 secret 能把一个世界**永久卡死**（**2026-07-28**）

扫引擎 `continuity`（确定性不变量 I1-I4）。I2/I3/I4 查下来是稳的
（引用完整性、在场判定的地点分组退化、锁定场景只许追加不许改写，都对）。
缺陷在 **I1**——那条防私密逐字泄露的。

#### 缺陷

I1 是 `prose.contains(secret)`，**字面子串匹配**，而唯一的前置过滤是 `!s.is_empty()`。

一条 1-2 个字的 secret（模型抽卡完全可能产出「钱」「他」「毒」）会命中**几乎任何一段正文**。
而 I1 的失败后果是**整回合阻断、不提交任何状态**；secrets 又持久存在于 `NarrativeState` 里——

> 于是下一拍再算一次、再阻断一次。**那个世界永远出不来。**
> 症状是「每一拍都 blocked」，而原因是一个字。

（顺带一提：「连续 blocked」正是 §3.40 刚加的那个健康档维度要抓的形态。
两件事在同一天从两个方向撞上——一个建了探测器，一个是它会探测到的病。）

#### 这不是新判断，是把已经学过的一课补到第二处

`arbiter::MIN_FORBIDDEN_CHARS`（同为 3）早就写着同一件事，标题就叫**误伤控制闸②**：
「低于此长度的底线（「不逃」→「逃」）字面匹配会大面积误伤」。

同一个判断此前**只有一处做对了**（§3.8.1 形态 (a)），而**做错的那一处失败后果更重**——
`arbiter` 那边误伤一个动作，`continuity` 这边是整个世界停摆。

#### ⚠️ 代价说清楚

短于门槛的 secret **不再被 I1 检查**。这是有意的取舍：一个 1-2 字的字符串出现在正文里
**本来也不构成「逐字泄露」**——它不携带关于那条私密的任何信息（「钱」出现在任何一段
江湖叙事里都毫不意外）。也就是说这段区间里，检查的保护价值 ≈ 0 而误伤率 ≈ 100%。
真正该管的是**卡录入时就不该产出这种 secret**，那属于另一道门（同 §3.43 的思路：
在有人的时候拒绝）——本轮没做，记在这里。

🔵 故障注入四处全红：去掉长度闸（退回改动前）→ 红；门槛调到 4（恰好 3 字的真 secret 漏掉）
→ **反向用例**红；整个 I1 删掉（把门拆了）→ 2 条红；
把 `chars().count()` 写成 `len()`（**中文按字节算，一个汉字就够 3 字节**，闸等于没有）→ 红。
最后那条尤其值得留着：它是这类长度闸在中文语境下最容易出的错，而且看起来完全正常。

### 3.43 写错一条世界不变量，**没有任何人会知道**（**2026-07-28**）

接着扫引擎 `constraints`（受限谓词 DSL）。求值面本身是稳的：
`reducer` 用 `?` 传播（禁止谓词求值出错 → 整个 patch 拒绝），
`advance_when` 用 `.unwrap_or(false)`（推进门求值出错 → 不推进）——
**两处方向相反但都指向「不做那件事」**，是对的。

缺陷在**它上游**。

#### 一条断了的链

`forbiddenPredicates` 是**世界不变量**：reducer 每次应用 patch 后逐条求值，
命中即整个 patch 拒绝——它是把「这个世界里不许发生的事」挡在公共事实之外的机制。

而一条**语法非法**的谓词会：

| 关口 | 结果 |
|---|---|
| `POST /assets/worlds` 发布 | ✅ 通过（**发布期压根不校验谓词语法**） |
| 人工校准 | ✅ 通过（看不出来） |
| 仿真试跑 | ✅ 通过（世界跑起来一切正常） |
| 安全审 | ✅ 通过 |
| **开世界（`runtime`）** | 🔴 **静默 `continue` 掉**——没有日志、没有计数 |

结果：模板作者以为「角色不得知道 X」已经生效，而那个世界**根本没有这条不变量**，
**没有任何人能看出来**。一个永远不会被发现的缺口。

`advanceWhen` 同形状，且失效方向同样不安全：丢掉推进门会让世界**更容易**走到终局。

#### 改法：**在有人的时候拒绝，而不是在没人的时候静默**

正门挪到**发布期**（`validate_superset` 直接 400，点名是哪一条 + 说清后果）——
作者当场就能改。运行时那两处静默 `continue` 改成带 id / 表达式 / 解析错误的 `warn!`，
作为**存量模板**的兜底：那些模板发布时还没有这道门。

⚠️ 报错文案本身被用例钉住：必须说清**后果**（「会被静默丢弃，这个世界少一条不变量」/
「丢掉推进门会更容易走到终局」）而不只是「语法错」。
只说语法错的话，作者会当成一个格式挑剔顺手绕过去——而这一条的全部要害是
**它失效时没有症状**。

🔵 故障注入四处全红：整块删掉发布期校验（退回改动前）→ 红；改成无条件拒绝
（**合法谓词也发不出去、功能废掉**）→ 反向用例红；报错不说后果 → 红；
`advanceWhen` 报错不说失效方向 → 红。

### 3.42 仲裁静默失效时，战报会说「判定依据：model:arbiter」（**2026-07-28**）

按覆盖图开工引擎 `arbiter`（仲裁——产品最核心的判定，也正是 §3.41 那条礼物红线
声称「不得引用」的地方）。

#### 缺陷

`model_arbitrate` 对模型漏判的决策兜底为 `Success`。兜底本身可以讨论，
**但兜底出来的结果与真判过的结果写着同一条 `rule_refs: ["model:arbiter"]`**。

而 `rule_refs` 是**透明战报的「判定依据」那一栏**——`arena::report` 直接读它，
那一栏存在的全部意义是回答观众「这是不是剧本」。于是：

> 模型返回一个**合法但空**的 `outcomes`（prompt 改坏 / 模型换版 / JSON 结构漂移
> ——最常见的故障形态），症状是**这一拍所有人做什么都成功**，
> 而战报逐条声称「判定依据：model:arbiter」。
>
> 读起来像「今天裁决很宽松」，不像「裁决根本没跑」。

又一次同一个形态：**两件截然不同的事在数据上长得一模一样**（§3.36 的 0 与 —、
§3.40 的 0 与「未启用」）。而这一次它落在**专门用来自证清白的那个面**上。

#### 改法：**只改可见性，不改口径**

- 兜底的那一条改写 `model:arbiter:unjudged`，真判过的仍是 `model:arbiter`。
- 兜底条数 > 0 时发一条 `arbiterUnjudged` 可观测记录（带 `unjudged` / `pending` 两个数）。

⚠️ **兜底值仍然是 `Success`**。「模型漏判该算成功还是失败」是**产品口径**：
改成 Failure 会让一次模型抖动变成「全场都失败」，未必更好。
这里只把**沉默**变成可见——有了标记，「兜底了多少条」才第一次成为可数、可报警、
**可用来定那个口径**的量。这也是为什么记录里必须带条数：
「漏了一条」和「整个环节没跑」是完全不同的两件事。

#### 🔵 顺带记一次自己踩的坑

第一版断言写的是 `format!("{evs:?}").contains("\"unjudged\":2")`——**恒不命中**，
因为 Debug 输出里数字长成 `Number(2)` 不是 JSON。它当场就红了。
危险的不是红，是**修它的两种方式**：改成走结构（做对了），
或者把 `contains` 放宽成只找 `arbiterUnjudged`（那就悄悄变成空断言，
数字对不对再也没人管）。这正是 §3.8.1「红线会撒谎」最容易发生的时刻——
**一条红着的断言，最省事的修法往往就是把它变成永远绿的那一条。**

🔵 故障注入五处全红：退回改动前（共用同一条依据）→ 红；把 `rule_refs` 恒置成 unjudged
（每条真裁决都自称没判过，**比原缺陷更糟**）→ 反向用例红；去掉可观测记录 → 红；
告警恒发（没兜底也发，狼来了）→ 反向用例红；记录里不带条数 → 红。

### 3.41 拍板落地 ③：观众礼物进引擎——**买的是「被看见」，不是影响力**（**2026-07-28**）

产品拍的第四项，也是清单里唯一压在**「不卖胜负与数值平权」**红线上的一项：
`open-decisions.md` §5 选项 **A**。

#### 为什么这一项和别的都不一样

它是全清单里唯一一个「**实现了再撤回代价极高**」的：礼物一旦被玩家感知为「打赏有用」，
撤回就等于**承认平台卖过优势**。所以顺序必须是「先定边界、再写代码」，
而不是「先做出来再看要不要收」。

#### 落地形态（与 `self_identities` 同等强度的边界声明）

- 引擎 `RoundInput.ambient_events: Vec<AmbientEvent>`。
  🔴 **命名刻意不叫 `boons`**——`boon`（增益）这个词本身就暗示数值好处，而它恰恰不是增益。
  **名字是给下一个人看的第一道边界。**
- 只流向 `decide::assemble_visible_context` 的展示层 JSON（`ambient` 一格）；
  仲裁 / 确定性不变量 / reducer / `StatePatch` / 同意门控 / 关系演化 / 里程碑强度**一律不引用**，
  也绝不写回 `NarrativeState`。
- **所有角色看到同一份**（`Vec`，不是 per-character map）：按角色分发会立刻造出
  「谁的观众多谁看到更多」这条可优化的差异通道。
- 只取礼物的 `label`，**绝不取 `boon`**——那里面是 `kind`/`effectTag` 之类的效果语义，
  喂给模型就等于在暗示「这个礼物该起什么作用」。
- **战报显式标注**：提交时按 id 精确回写 `arena_env_events.applied_tick`，
  而透明战报本来就读这一列 → 「这一拍场上有观众送的东西」对**所有观众**可见。
  不标注就是隐性优待。
- **默认关闭**（`MUSE_AMBIENT_GIFT_EVENTS`）：必须**有人按过开关**才生效。

#### 🔴 note 里那两句话是判据的一部分，不是文案

上下文里附的 note 明写「**不给任何人优势**」「**送得多不等于更管用**」，并由用例断言其存在。
理由：**模型会自己脑补数值语义**。不把这句话写死，`times: 5` 就会被演成「效果更强」——
那时红线不是被代码破的，是被**沉默**破的。

#### 两道门，各封一半，缺一不可

| 门 | 封的是 | **拦不住**的是 |
|---|---|---|
| 源码级红线（**排除表**：除登记的两个文件外，任何生产源码出现 `ambient_events`/`AmbientEvent` 即红，且按**处数**记） | 「别的地方**引用**了它」——读必然出现标识符，源码扫描能穷尽，行为用例做不到 | 在**允许的文件里**把礼物拼进 `situation` |
| 行为用例 | 「加与不加礼物，上下文除 `ambient` 外**逐键相同**」 | 别的文件新增引用 |

这是本轮第一次明确写下**两种判据的互补关系**：源码扫描回答「有没有人碰它」，
行为断言回答「碰的那一处有没有越界」。任何一条单独用都有一个明显的绕过方式。

🔵 故障注入**六处全红**：把礼物拼进 `situation` → 行为用例红；note 去掉「送得多不等于更管用」→ 红；
在仲裁里引用它 → 源码红线红；把登记处数放宽 1 留后门 → 红；去掉运行时开关判断 → 红；
把 `boon` 效果语义一起喂给模型 → 红。

⚠️ **一处没验到，如实记下**：「绝不 blanket 标记 `applied_tick`」（礼物可能在这一拍
**跑的过程中**送到，它没进过这一拍的上下文；blanket 会让战报声称一件没发生过的事）
目前只有注释和实现，**没有端到端用例**——要造它需要跑完整一拍并在中途插入一条礼物。

### 3.40 拍板落地 ②：健康档三个新维度——装上了，**默认一个都不生效**（**2026-07-28**）

产品当天拍的第三项（open-decisions §1）：「需关注」除预算逼近上限外，
再加 **tick 失败率 / 尾部连续 blocked / 停摆时长**。

#### ⚠️ 回执本身是含混的，先如实记下

那一问是多选，选项里**同时勾了三个维度和「不加，只保留预算维度」**——两者互斥。
落地按「三个都加、但**一律默认关闭**」处理，这样两种读法都成立：要用就开一个开关，
要「先看真实运营数据再说」就什么都不做。若本意是后者，删掉三条 `FlagDef` 即可，
没有别处依赖它们。**这一段不该被读成「已澄清」——它是记下来等回话的。**

#### 三条默认关闭的理由，与本仓别的开关都不同

其它默认关闭的开关挡的是「功能静默上线」或「静默烧 token」。这三条**既不改变用户可见范围、
也不烧任何 token**——它们改的是运营看板上「需关注」那个数**的含义**。
默认开着合并意味着：没有人按过开关，而某天早上那个数字自己变大了，
运营会去追一个并不存在的事故。§0.1 在这里挡的是**口径静默漂移**。

`flags::KNOWN_FLAGS` 的计数棘轮 18 → 21 如期变红，逼了一次人工评审——**它就是干这个的**。

#### 🔴 `attention` 的口径一个字没动

新维度进的是**新字段** `attentionAny`（server 侧 UNION **去重**），
`attention` 仍然只按预算规则。既有消费者读到的数不会因为运营按了个开关就改变含义。
「哪些维度该进头条数字」本身是 §1 剩下的那半个问题，本批不替产品定。

#### 三条判据里最容易写错的地方，各由一条用例钉着

| 维度 | 陷阱 | 用例 |
|---|---|---|
| 停摆 | `MAX(world_ticks.created_at)` 对新世界是 NULL，不用 `worlds.created_at` 兜底的话，**五分钟前建的世界立刻亮黄灯** | `a_brand_new_world_is_not_counted_as_stalled` |
| 连续 blocked | 写成「窗口内 blocked 占比」的话，「上周堵过三拍、之后一直正常」——**已经好了**的世界——会继续亮灯 | `the_blocked_streak_counts_only_the_tail_not_history` |
| 失败率 | 没有样本下限的话，「只跑了 1 拍且失败」= 100% 失败率，**每个第一拍失败的新世界都会亮灯** | `the_failure_rate_needs_a_minimum_sample_before_it_lights_up` |

连续 blocked 用的是**真·尾部连续**（相关子查询取「最后一个非 blocked 拍之后的全部 blocked 拍」），
不是占比近似。代价是这条查询比另两条重——这也是它必须挂在开关后面的第二个理由。

另两条：`attentionReasons` 各条**互相重叠、不可相加**（`attentionAny` 才是总数，由
`attention_any_deduplicates_worlds_that_hit_several_rules` 钉住）；未启用的维度
`count` 是 **`null` 不是 0**——0 会被读成「这一维一个世界都没有」，
同 §3.36 那条「显示 — 与显示 0% 是两个判断」。

🔵 故障注入五处全红：停摆去掉 `worlds.created_at` 兜底 → 红；连续 blocked 去掉尾部子查询
（退化成占比）→ 红；失败率去掉样本下限 → 红；`attentionAny` 改 `UNION ALL`（不去重）→ 红；
未启用维度报 0 而不是 null → 红。

#### 顺带补了两处「算了却没人看」

- **后台渲染**：`attentionReasons` 此前在 `WorldsMonitorConsole.tsx` 的 TS 接口里声明了，
  **却从没被显示过**——又一次 §3.37 那个形态。现在渲染成「需关注构成」，
  且**遍历 server 实际下发的每一条**（码没登记就原样显示码），server 新加一维不会被静默漏掉。
- **`docs/API.md` 补登**：`GET /admin/worlds/summary` 2026-07-27 就上线了，
  **一直没进接口表**——正是 CLAUDE.md 那句「改路由必须同步改它」在防的事。

### 3.39 拍板落地 ①：Saga 粒度定了，羁绊 `debt` 不再计入正向（**2026-07-28**）

产品当天拍了四项（§1/§2/§3/§5），本节记前两项的落地。

#### §3 Saga 粒度 = A：一个世界实例 = 一个阶段

从 `TODO(saga)` 升格为写进**总规格 §3** 与 `interventions` 模块头的明确决定，
`open-decisions.md` §3 按其自身的退出条件整节核销。「连载」由**开新实例 + 阶段间继承**实现。

代码里的 `TODO(saga)` 撤掉，换成一段「万一将来推翻，代价是什么」——
**留着是为了说清代价，不是待办**。代价那句话本身值得记住：
「阶段」在系统里有 5 个以上消费点（托梦配额 / 结算 / 贡献分 / SLO 窗口 / 校准视图），
改粒度等于**同时改这 5 处的含义**；且存量 `interventions` 行没有 `stage_no`，
迁移那一刻「历史托梦算在哪个阶段」无解。

#### §2 羁绊 `debt` 不计入正向分

解锁门的语义写明是「双向自愿」（跨边取 min 就是为这个），而 `debt` 是**义务**不是意愿。
落地为 `MUSE_SOCIAL_BOND_COUNTS_DEBT`（默认 `false`，§0.2 参数化，旧口径一键取回）。

#### 🔵 一个本来会被当成「决定与规格冲突」的东西，查下来不冲突

表面看这与规格「救命属正向线」直接打架——救命之恩在引擎里**正是** `debt`。
查引擎实际写什么：`relation_dynamics` 对「救」类命中时是
`affinity +0.08` **双向**、`trust +0.06` **双向**，另加 `debt +0.10`
**只加在被救者→救人者单边**。而羁绊分**跨边取 min**——最小值只可能来自
**救人者那条边**，那条边上压根没有 debt。

**所以纯救命场景下，计不计 debt 算出来的分一模一样。** 救命之恩仍是正向线，
只是由双向的 trust/affinity 承载，不由单边的 debt 承载。
真正被挡掉的只有「双方互相亏欠、但彼此并不亲近」这一类——那正是该挡的。

⚠️ 这个结论**不是推理出来就算数的**：用例 `rescue_bond_is_identical_whether_debt_counts_or_not`
**直接调引擎的 `derive_relation_ops`** 生成一次真实救援，先断言引擎侧的结构事实
（debt 单边、trust/affinity 双向），再断言服务端侧两种口径同分。
哪天引擎改成「救类只记 debt、不抬 trust/affinity」，上面那段论证就不成立，
而这条用例会立刻红——**那正是该回到产品面重议的时刻**，而不是让一条过时的论证继续躺在注释里。

🔵 故障注入三处全红：`bond_between` 忽略开关（永远计入 debt）→ 红；
引擎把救类 debt 改成双向 → 结构断言红；引擎把救类的 trust/affinity 增量清零
（论证前提被推翻）→ 红。

### 3.38 「打完最后一句、立刻点开下一章」——那段字**一个都没落盘**（**2026-07-28**）

按覆盖图第四项开工：前端 `components`（13259 行）。先扫高危面：
`dangerouslySetInnerHTML` **零处**，编辑器的 stale-response 竞态早有 `requestId` 守着，
删除类调用都有确认。缺陷在**自动保存的防抖**上。

#### 缺陷

`MarkdownEditorImpl` 的防抖保存写成：

```js
const saveTimer = window.setTimeout(() => invoke('write_file', …), 800);
return () => { window.clearTimeout(saveTimer); };   // ← 依赖里有 content
```

依赖里有 `content`，意味着**每敲一个键**都重建一次计时器；计时器只在停止输入 800ms 后才响。
而清理函数是**纯 `clearTimeout`，没有任何补写**。于是：

> 打完最后一句 → 立刻在文件树里点开下一章（或离开作品页 / 关窗）
> → 清理跑了，计时器被清掉，**这一轮打的字一次都没落盘，界面上也没有任何提示**。

丢的是用户刚写的正文，而且他不会知道——`saveStatus` 那一刻正好被新文件的加载覆盖掉。

🔵 改动前实测：切文件与卸载两个场景下 `write_file` 调用数都是 **0**。

#### 改法

1. **补写 effect**，依赖里**没有 `content`**，所以清理只在**换文件或卸载**时跑一次
   ——正好是那两个丢数据的窗口；内容从 ref 取，永远是最新的一份。那里没有 800ms 可等，直接落盘。
2. 判据抽成纯函数 `shouldPersist`，防抖与补写**共用**。两处各写一遍的话将来只会改其中一处，
   而它们一个管「正常节奏下的保存」、一个管「最后 800 毫秒」，判据不一致的后果
   恰恰只在切文件的瞬间发作（形态 (a)）。
3. state 里加 `loadedPath`：**内容和它来自哪个文件一起走**。

#### 关于第 3 条，诚实说明

`text-load-start` 刻意保留上一个文件的 `content`（换文件时编辑区不闪空白），
于是「`filePath` 已是新文件、`content` 还是旧文件的」这一帧**真实存在**，
防抖 effect 在那一帧看到的正是 `pathToSave = 新文件` + `contentToSave = 旧内容`。

⚠️ **但故障注入证明不了它今天在挡什么**：单独删掉 `loadedPath` 那条，用例**是绿的**
（`loading` / `readError` 各自也拦得住；那一帧之后立刻有一次重渲染把计时器清掉，
而重渲染在任何现实情形下都远快于 800ms）。删掉两条才红。
留着它的理由不是「它今天在挡什么」，而是把「不许写」从**几个否定条件恰好都成立**
换成**一句肯定的来源断言**。这一条按纵深防御记账，**不计入「修好的缺陷」**。

#### 判据

四条用例：切文件补写（且**必须落到原来那个文件**）· 卸载补写 ·
**反向配对**「没有未保存改动时离开不许写盘」（只测「要补写」的话，写成无条件 `write_file`
也能全绿，那会在每次切文件时重写文件、刷新 mtime、和别处的写抢）·
读取失败时那句「**读取文件失败**: …」是提示文案不是正文，绝不许被写回文件。

🔵 故障注入：去掉补写 effect（改动前形态）→ **2 条红**；补写改成无条件 → **3 条红**；
同时去掉 `loadedPath` 与 `readError` → 红。
⚠️ 另有两次尝试**没有构成有效注入**，如实记下：想把补写的目标路径换成「当前 filePath」
来演示写错文件，但清理函数运行在**新 effect 之前**，那一刻所有能拿到的路径来源
（闭包、`useSyncedRef`）**都还是旧值**——注入写出来是等价表达式，证明不了任何事。

### 3.37 那七项 SLO **后台一项都没显示过**——算了、发了、扔了（**2026-07-28**）

修完 §3.36 之后顺手核了一件事：那七项指标在后台长什么样。答案是**不长什么样**。

`admin/` 全仓搜 `attentionGini` / `silentStreak` / `forcedConclusionRate` /
`repeatEntryRate` / `stateTextContradictionRate` / `oocAppealRate` / `plotRepetitionRate`
——**七个名字一次都没出现过**。`Metrics.tsx` 里 `NarrativeSlo` 这个 TS 接口只声明了两个字段：

```ts
interface NarrativeSlo { status: string; calibration?: CalibrationReadings | null; }
```

也就是说 `/admin/metrics/overview` 每被轮询一次就跑约 18 条 SQL 把七项指标算出来、
连同注释一起下发，然后**前端只取 `calibration` 一个键，其余整段丢掉**。

#### 这属于第三种缺陷形态

服务端那一侧是对的（口径、状态机、性能护栏、红线一应俱全，测试也厚）；
后台那一侧也是对的（它渲染的东西都渲染对了）。**中间那道缝没有主人**——§3.8.1 形态 (c)。
后果是 §4.2「验证基建三件套第二件」、T1/T2 门槛的**唯一测量实现**，运营看不见。
§3.36 刚修好的七态契约，此前也没有任何消费者。

#### 改法

`Metrics.tsx` 补一段「叙事质量 SLO」表格（指标 / 读数 / 样本 / 门槛）。

🔴 **渲染遍历服务端实际下发的每一个键，不是前端写死的七项**：前端那张 `SLO_HEADLINE`
只做**增强**（这项的头条数字取哪个字段、样本量取哪个字段），没登记的指标照样出现在表里——
标题走服务端自带的 `title`，数值回落到通用的 `value`，另标一个「新指标」标签。
写成「渲染这七项」的话，服务端上线第八项时后台会**静默地不显示它**，
而这一段的全部意义就是让指标能被人看见。

🔴 **`status ≠ ok` 一律不画数字**，显示 `—` + 中文状态标签，原因进 tooltip。
这是 §3.36 那条红线在前端这一侧的对应纪律。

#### 接缝本身也有了一道门，钉在**服务端**

`admin/` 没有任何测试基建（只有 `tsc`），而 tsc 拦不住这类错——指标块在 TS 里带索引签名，
读一个不存在的字段类型上完全合法，运行时只是渲染成 `—`。于是**「字段名对不上」
和「零样本」在界面上长得一模一样**。

故新用例 `admin_slo_table_reads_only_fields_the_server_actually_sends` 放在 server 侧：
`include_str!` 读后台源码 → 解析出 `SLO_HEADLINE` 的登记 → 播种把多数指标推到 `ok` →
断言每条登记的字段在实际下发的块里存在。判据只管 `status == "ok"` 的块，
与后台的渲染分支逐字对应；并断言**至少 3 项真的 ok、至少解析出 6 条登记**，
否则这道门会在空库上或解析失效后静默变成空断言。

🔵 故障注入四处全红：服务端把 `forcedRate` 改名 → 红；前端登记一个服务端没有的指标名 → 红；
服务端删掉样本量字段 `worldsCounted` → 红；后台那张表被改名（解析口径失效）→ 红。

### 3.36 SLO 看板在**空平台上报「0%」**——而 0 的欺骗方向不统一（**2026-07-28**）

按覆盖图第三项开工：server `slo`（「只读聚合，风险低于前两者，但它是运营看板的唯一数据源，
**算错比缺失更糟**」）。

#### 缺陷

模块头自己写着这条规矩：

> 🔴 不可算的指标必须显式标注为「无数据源」而不是 0 或空——后台显示 `—` 与显示 `0%`
> 是两个完全不同的经营判断，**混同即事故**。

而在**空库**上实测（探针跑一遍 `narrative_slo`），六个可算指标里**四个**违反了它：

| 指标 | 空库实测 | 会被读成 | 真相 |
|---|---|---|---|
| `attentionGini` | `status: ok`、`overThresholdRate: 0` | 没有世界越过公平门槛 | 一个多人世界都没有 |
| `silentStreak` | `status: ok`、`overThresholdRate: 0` | 没人被晾着 | 没有任何成员进过统计 |
| `forcedConclusionRate` | `status: ok`、`forcedRate: 0` | **收尾都很自然，一切健康** | 一个世界都没结束过 |
| `repeatEntryRate` | `status: ok`、`repeatEntryRate: 0` | **留存崩了，要立刻处理** | 一张云角色卡都还没有 |

另外两个（`stateTextContradictionRate` / `oocAppealRate`）**做对了**，
`calibration` 子模块（更晚写的）也做对了——它的 `proportion_reading` 信封甚至专门写了
「取值必须穿过这层信封，于是『拿到一个比例却不知道它压在几个观察上』在结构上不可能发生」。
错的是四个更早写的块。

🔴 **最后两行是这条缺陷的要害**：同一个空库，`forcedRate: 0` 看起来是好消息、
`repeatEntryRate: 0` 看起来是紧急事故。所以「运营记得往好里读」或者「往坏里读」
**都救不了**——只有 `null` 是对的。

#### 它为什么活到今天：**两道用例把错的答案各钉了一遍**

`slo::tests::empty_database_is_zero_safe` 断言 `forcedRate == 0.0`，理由写的是
「0/0 → 0，**不得 NaN**」。躲开 NaN 是对的，但它顺手把错的答案钉住了。
`admin_api::tests::narrative_slo_is_zero_safe_on_empty_platform` 在 HTTP 出口上又钉了一遍。

于是改对任何一处都会被另一处判红，而两处都写着「这是对的」。
这是 §3.8.1「红线会撒谎」的一个新变体：**用例是绿的，但绿的理由不是它要守的那件事**，
而且这次它还主动挡住了修复。

#### 改法

新增 `slo::rate_or_null`（分母 0 → `null`），四个块的 `status` 改为按分母判定。
状态集合从「三态」扩到**七态**并写进模块头的表：

`ok`（有样本，**可以是真的 0%**）/ `no_data_in_window`（分窗口，本窗口零样本）/
`no_data_yet`（**全生命周期口径**，至今零样本）/ `entry_not_open` / `no_data_source` /
`skipped_too_large` / `skipped_by_request`。

⚠️ `no_data_in_window` 与 `no_data_yet` 分开不是文字游戏：告诉运营「近 7 天没有数据」
而这个指标压根不切窗口，会把人送去改窗口选择器——而那改不动任何东西。

`slo::quality` 的人读面（`to_json`）同一形状一并改掉（4 个比例 + `topEndingShare`）。
⚠️ 但 `concentrationGini` **不改**：`gini_coefficient` 的「空集/全零集 → 0.0」
（「没开演不是不公平」）是多个调用点共享的既定契约，单独在一个渲染出口改判空，
会让同一个函数在不同出口给出不同语义。判空线索由紧邻的 `worldsWithEnding` / `distinctEndings` 给出。

#### 判据遍历**实际下发的全部指标**，不是手列四项

手列的话，将来第八项指标上线时不在清单里 → 静默放行。新用例遍历 `metrics` 对象的每一个键，
新指标一上线就自动进入判据；比率字段按 `Rate` / `Share` 后缀识别，并断言**至少认出 4 个**
——否则命名一变，这道门会静默变成空断言。

配一条**反向用例**：分母不为零时 0 就是真的 0，必须照常报出来。
只断言「空库要 null」的话，把所有比率恒置 null 也能全绿。

🔵 故障注入四处全红：`forcedRate` 退回 `rate()` → 红；`status` 退回恒 `ok`（比率仍判空）→ 红；
`rate_or_null` 改成恒 `null`（把真数也吞掉）→ **反向用例**红；
往 `metrics` 塞一个 `status: "ok"` + `someRate: 0.0` 的新指标（模拟第八项上线）→ 红。

#### 顺带查实、**没有改**的

`gini_coefficient`（i128 累加不溢出、空集契约明确）、`percentile`（空集 → None）、
`over_cap`（`LIMIT cap+1` 溢出探测）、`classify_conclusion`（未知 reason 保守计入强制）、
`calibration::proportion_reading`（信封形态本就正确）。
`slo` 模块「只读、不回灌引擎」那条红线也复核过：全模块无 INSERT/UPDATE/DELETE。

### 3.35 桌面端每一次覆盖用户数据的落盘，中途死掉都会**把数据截没**（**2026-07-28**）

按覆盖图第二项开工：前端 `stores`（持久化面，「数据丢失类缺陷通常住在这里」）。
缺陷确实在这条缝上，但住在 Rust 那一侧。

#### 缺陷

`createDiskStorage` → `save_app_state` → `save_app_state_path` 最后一句是 `fs::write`，
而 `fs::write` 的语义是「**先把目标截成 0 字节，再往里写**」。中途进程没了
（崩溃 / Cmd-Q / force quit / 被 kill / OOM），留下的就是一个**截断或全空**的文件。

这不是理论窗口——zustand persist 在**每次状态变更时**落盘，也就是正常使用中一直在写。
而受害者是这个仓库里最不可再生的两份数据：

| 文件 | 内容 |
|---|---|
| `config/partner-store.json` | 全部角色卡 + 世界书 |
| `config/settings-store.json` | 全部模型配置、API Key、每一条自定义 prompt |

🔴 更糟的是**损坏会自我固化**：下次启动截断的 JSON 解析失败 → persist 静默回落到初始状态 →
用户随手一改，第一次 `setItem` 就把那份空状态原样写回文件。至此原始内容没有第二份。

全仓**一个原子写辅助都没有**，20 处落盘全是 `fs::write`。

#### 改法

新增 `utils::write_atomic`：写**同目录**临时文件 → `rename` 覆盖。rename 是原子的，
任何时刻看到的要么是完整旧内容、要么是完整新内容。11 处「覆盖既有用户数据」的落盘全部改走它
（app state、会话保存/改名、手机端两处对应写、作品正文、版本索引 ×3、write/edit 工具、作品评分）。

⚠️ **不做 fsync，这是有意的**：本函数消掉的是**进程死亡**窗口，不是**断电**窗口。
后者要在 rename 前 `sync_all()`，而那在 macOS 上是 `F_FULLFSYNC`（整盘刷），
按「每次状态变更一次」的频率摊到设置页的每一次输入上会变成肉眼可见的卡顿。
用一次真实的交互退化去换一个文件系统本身已大幅缓解的窗口（APFS 与 ext4 `data=ordered`
都对「写完即 rename 覆盖」有强制落盘的启发式），不划算。**这条边界写在函数头注释里**，
不是留给下一个人自己去猜。

⚠️ 临时文件名后缀**不是 `.json`**：会话目录与配置目录的枚举器都按 `extension() == "json"`
过滤，残留的临时文件不会被当成一条坏会话列出来。名字带 pid + 进程内递增序号——
桌面端与手机端可能同时保存同一个 store，共用一个固定临时名会让两次写交叉后
rename 出一份**混合内容**。序号用 `AtomicU64` 而非随机数/时间戳（§4「禁三样」同口径）。

#### 红线是**排除表**，方向和 §3.8.1 那条一致

这道门要防的是**将来新写的落盘点忘了走原子写**。写成「以下这些点必须原子」的话，
新增的第 N+1 处不在清单里 → 静默放行，门等于不存在。反过来「除了这几处已知安全的，
一律不许出现 `fs::write`」让**遗漏往红的方向失败**。

表里按**处数**而不是只按文件名登记：只记文件名的话，往一个已豁免文件里新加一处
`fs::write` 会被顺带放行。4 条豁免（crawler 抓取正文 ×4、新建空文件 ×2、
两处唯一新路径导出、反向大纲）各自写清了「为什么这处安全」。

🔵 故障注入四处全红：`save_app_state` 退回 `fs::write` → 红；`write_atomic` 退化成原地写 →
inode 用例红（旧句柄读到新内容）；给已豁免的 `crawler.rs` 加第 5 处 → 处数对不上，红；
EXEMPT 里留一条已不存在的文件 → 表自检红。

⚠️ **没验到的一处**：rename 失败时清理临时文件那条路径——测试里造不出「文件写成了但
rename 失败」的情形（需要跨文件系统或权限构造），如实记在这里。

### 3.34 引擎 `narrative`：状态写入面**没有缺陷**，但两句注释变成了强制（**2026-07-28**）

按 §3.33 覆盖图的第一项开工：引擎 `narrative`（回合循环 / 仲裁 / 结局选取）。
先查最要紧的一处——**模型输出如何变成状态写入**（不可信输入直达状态）。

#### 查下来：这一面是稳的，一处没改

| 问题 | 答案 |
|---|---|
| 模型能写任意路径吗？ | **不能。** `parse_path` 是严格白名单，末尾 `Err("未知根段")` |
| 能删掉禁止谓词再为所欲为吗？ | **不能。** `narrative.` 只放行 `outlineNodes[].status` / `foreshadowing` / `pacingNotes`，`forbiddenPredicates` **不在可写面里** |
| 禁止谓词求值出错会放行吗？ | **不会。** `?` 传播 → 整个 patch 拒绝（fail-closed） |
| 模型能直接提 operations 吗？ | **不能。** 它只交结构化的 decisions/outcomes，**全部 `PatchOperation` 由 `build_patch` 构造**。全仓另两处构造 `StatePatch` 的地方：一处是硬编码的 `authoring.lockedSceneIds` 追加，一处 operations 为空 |
| 重复应用 / 并发写？ | 幂等（patch id）+ revision CAS + clone-on-apply |

#### 但两句注释只是约定，现在变成强制

`build_patch` 里写着「**Increment 单调不减，进度键仅经此路径写入**」。那句话**今天成立**
（见上表最后一行），但没有任何东西守着它。而 `world.<key>` 是**开放键空间**——
一旦将来有人让模型直接提 operations（作为「优化」），一条
`Set world.milestoneProgress_m1 = 999` 就能跳过本该逐回合累积的节奏、直接把放置房推到终局。

`apply_world` 加一条**键级特例**：`milestoneProgress_*` 只许 `Increment`。
结构上只剩累加，那条路走不通。

⚠️ **只挡这一个前缀**，不挡整个 `world.*`——`world` 本就是给叙事用的开放键空间，
一律限制会把正常的世界状态写入也挡掉。这一条由「普通 world 键不得被误挡」的用例钉住。

🔵 故障注入三处全红，**两处是过度收紧方向**：去掉键级特例（改动前形态）→ 红；
把限制扩到整个 `world.*` → 误挡用例红；前缀判据写成 `contains("milestone")` →
`world.milestoneSummary` 被误挡，红。

### 3.33 覆盖图：这轮巡检**扫过哪些面、没扫哪些面**（**2026-07-28**）

我在 §3.32 末尾写过「未扫过的面我不知道有多少」。那句话本身是可以消除的空白，
这一节把它变成一张图——**给下一个人（或下一轮的我）一个能据以决策的起点**，
而不是继续随机抽查。

⚠️ **「查过」与「改过」是两回事**，下表分开标：查过没改的那些，结论是「无缺陷」，
本身也是结果（省下一个人重查）。

| 轨 | 模块 | 行数 | 状态 |
|---|---|---:|---|
| server | `admin_api` `runtime` `assembly` `safety` `ifline` `assets` | 30k | ✅ 查过并改过 |
| server | `worlds` `events` `social` `subplot` `backpack` `shop` `billing` `ledger` `progression` `chapters` `arena` `livegate` `clips` `reports` `memorial` `onboarding` `invitations` `interventions` `annotations` `consents` `flags` `idempotency` | — | ✅ 查过（红线扫描面覆盖：读取面过滤 / 资产单一写入 / 事实不可删 / 确定性 / 幂等 / 无提现 / AI 标识 / feature 门控），**多数结论为无缺陷** |
| server | `slo` | 3258 | ✅ 查过（§3.36）：空平台把「没测过」报成 `0%`，四个块已修；纯函数面复核无缺陷 |
| 引擎 | `lib`（新增红线）· 全 crate 的 `unwrap`/`expect` 面 · 依赖表 | — | ✅ 查过并改过 |
| 引擎 | `narrative` 的**状态写入面**（`reducer` / `parse_path` / `build_patch` / patch 产地） | — | ✅ 查过（§3.34），**无缺陷**；顺带把两句注释变成强制 |
| 引擎 | `arbiter` 的**模型层出口**（漏判兜底与判定依据） | — | ✅ 查过（§3.42）：兜底与真裁决共用同一条依据，已修 |
| 引擎 | `narrative` 其余（回合编排 · `arbiter` 规则层 · `constraints` · `relation_dynamics` · `continuity`）· `character` 3811 · `world` 2370 · `knowledge` 1593 · `replay` 1454 | 17k | ⬜ **未逐条扫过** |
| 桌面轨 | `agent` `commands` `mobile_server` `llm` `tools` `models` `utils` `crawler` | 16k | ✅ 查过并改过 |
| 桌面轨 | `book_travel` 1538 · `lib` 392 · `fs_commands` 297 | 2.2k | ⬜ **未扫过**（`fs_commands::rename_item_cmd` 单独看过，结论见 §3.26） |
| 前端 | `utils/runtime.ts` · `pages/MobileHome` · `utils/bookTravelMaterials` | — | ✅ 查过并改过 |
| 前端 | `stores` 的**持久化面**（`diskStorage` → `save_app_state` 这条缝） | — | ✅ 查过（§3.35），缺陷在 Rust 那一侧：落盘不原子 |
| 前端 | `components` 的**高危面**（内联 HTML / 竞态 / 删除确认）与**编辑器持久化面** | — | ✅ 查过（§3.38）：防抖保存丢最后一段，已修 |
| 前端 | `components` 其余（多为渲染） · `stores` 其余（各 store 的状态迁移 / 合并语义） · `pages` 其余 | 20k+ | ⬜ **未扫过** |

**按「还没扫且体量大且逻辑要紧」排，下一轮的候选顺序**：

1. ~~引擎 `narrative`~~ —— 状态写入面已查（§3.34，**无缺陷**，顺带把两句注释变成强制）；
   其余（回合编排 / `arbiter` / `constraints` / `relation_dynamics` / `continuity`）仍未逐条扫过。
2. ~~前端 `stores`（持久化面）~~ —— 已查（§3.35）。**猜对了地方，猜错了一侧**：
   数据丢失缺陷确实在这条缝上，但住在 Rust 的 `fs::write`（落盘不原子），
   前端 `diskStorage` 那 36 行本身是干净的。各 store 自身的状态迁移 / 合并语义仍未扫。
3. ~~server `slo`~~ —— 已查（§3.36）。**「算错比缺失更糟」这句话是对的**：
   六个可算指标里四个在空平台上报 `0%` 而不是 `—`，且被两道用例各钉了一遍。已修。
4. ~~前端 `components`~~ —— 高危面（`dangerouslySetInnerHTML` / 竞态 / 删除确认）已扫，
   编辑器的持久化面已查并修（§3.38）。其余组件多为渲染，判定密度最低。
6. **仍未扫**：引擎 `narrative` 其余面 · 各前端 store 自身的状态迁移与合并语义 ·
   桌面轨 `book_travel`（1538 行）· `pages` 其余。
5. 引擎 `narrative` 的其余面（回合编排 / `arbiter` / `constraints` / `relation_dynamics`）·
   各前端 store 自身的状态迁移与合并语义 · 桌面轨 `book_travel`（1538 行）。

⚠️ **这张图不是「剩下的都有问题」**，也不是「打勾的都没问题」——
它只说明**哪些面被这一轮的方法过过一遍**。已扫面里相当一部分结论是「无缺陷」（见各节），
未扫面里也可能一个缺陷都没有。它的用处是：**下次不必从零决定看哪里。**

### 3.32 引擎的确定性此前**一道闸都没有**——我说「已收敛」时漏了整个 crate（**2026-07-28**）

⚠️ 先纠正自己：上一轮我说「能靠查代码独立推进的部分已经收敛」。
**那句话不成立**——`crates/muse-engine`（20789 行、产品核心）我整个巡检里没扫过，
只在跨 crate 红线里碰过它的类型。说收敛之前应当先查，我没查。

扫完的结果分两半：

**一、生产码里的 `unwrap` / `expect`：没有缺陷。**
逐个看过——字面量正则的构造（编译期常量、运行期不会失败，`arbiter.rs` 自己写了注释说明）、
测试替身里的锁、以及 `chapters.rs` 那个 `merged.last_mut().unwrap()`（同条件里就有
`!merged.is_empty()` 守卫）。`chapters.rs` 解析的是**用户提供的小说正文**，
而它的注释已写明「脏文本绝不进 `Regex::new`」。**一处没改。**

**二、确定性契约在引擎侧一道闸都没有。**
§3.13 那条「禁三样」红线走 `testkit::production_sources()`，而那个函数读的是
`server` 的 `src` —— **引擎不在覆盖内**。而采样、仲裁、结局选取全在引擎里。

补两条，**强弱各一**：

| 红线 | 挡什么 |
|---|---|
| `no_irreproducible_randomness_in_engine_sources` | 扫引擎源码。豁免两处并写明理由：`host.rs` 的 `SystemClock` **是注入用的时钟实现本身**（引擎的设计正是「时钟由宿主注入」）、`model.rs` 的 `Instant::now` 只用于测耗时不进决策 |
| `engine_has_no_random_number_dependency` | 扫 `Cargo.toml`。**没有 `rand` 依赖，`thread_rng` 根本引不进来** |

后者是故障注入教我的：往 `narrative` 里塞 `rand::thread_rng()` **编译直接失败**——
引擎压根没有那个依赖。那比源码扫描更强，于是把它也钉住（真要引入随机数库是一次评审，
不是加一行依赖）。

🔵 注入：豁免多加一个不需要的文件 → 红；给引擎加 `rand` 依赖 → 红。

⚠️ **一个如实记的否定结果**：把剥测试模块退化成「按第一个 `#[cfg(test)]` 截断」，
**本 crate 上没红**。量过：正确剥后保留 60% 字符、退化后 59%——引擎的测试模块几乎都在
文件末尾，中段夹具极少，那种退化在这里确实只丢约 1%。我**没有**为一个检测不出来的差异
造断言；保持配平写法是因为**将来**可能出现中段夹具（server 那边就有），不是因为现在有。

### 3.8.1 §3.9–§3.31 的可复用结论（**先读这节，别读 23 篇事故报告**）

2026-07-27/28 的巡检往下面加了 23 节。它们各自是**事故记录**（这一处出了什么、怎么验的），
有价值但读起来是流水账。下一个人真正需要的是**方法**，抽在这里；细节仍在各节原处。

⚠️ 本节刻意**不复述任何计数**（缺陷数 / 红线数）——那属于本仓反复栽跟头的那类数字。

#### 一、缺陷长什么样：三种形状，一种比一种难看见

| 形状 | 意思 | 例 |
|---|---|---|
| **同一判定的 N 份拷贝** | 同一条规则被手抄在多处，漂开就出事 | 密钥抹除与回写保护（§3.19）、声明侧与执行侧的工具白名单（§3.24）、名字校验三份（§3.26） |
| **判定对受影响的人不可见** | 判断做了，但当事人看不到 | 无人读取的键只在接口里返回、页面不画（§3.17 前身）、分成比例对创作者不可见 |
| **🔴 接缝**：每一层单看都对，**层与层之间没人管** | 最难查的一种 | 令牌服务端裁了前端没裁（§3.18 / §3.23）、限制在父级设了子级不继承（§3.27）、保护在选择器里不在应用器里（§3.28） |

**第三种最值得优先找**：前两种在单个文件里读得出来，第三种要同时看两处才看得见。
本 session 后半程用「找接缝」当假设，连续三轮命中。

#### 二、包含表 vs 排除表：这个方向决定失败朝哪边倒

反复出现的同一个取舍——**列出「要处理的」还是列出「不处理的」**：

- 机审文本（§3.9）、密钥抹除（§3.19）、确定性扫描（§3.13）：**必须排除表**
  （漏一项 = 内容绕过机审 / 密钥出本机 / 不可复现随机潜伏），
- 建模板期键校验（§3.9 骨架部分）：**必须包含表**
  （这是会拒请求的闸，对着看不懂的结构乱拒是生产事故）。

判据不是「哪个更全」，而是**漏掉一项时朝哪边失败**。

#### 三、红线本身会骗人——本 session 撞了两次

| 弱点 | 症状 | 正确写法 |
|---|---|---|
| `contains("某字符串")` | 那个词可能因为**非守卫的原因**出现（业务分支、SELECT 列表） | 断言它出现在**哪个位置**（`WHERE` 之后 / 否定形式的守卫），见 §3.29 §3.30 |
| 扫描面手工列举 | 新文件不在列表里 → 红线对它静默失效 | 从**目录**核对扫描面（§3.13） |
| 豁免名单 | 可以被悄悄扩大 | 给豁免**计数**（§3.16 §3.30） |

**故障注入连着几次没红时，该怀疑的是断言，不是注入。**

#### 四、写源码级扫描器：仓里已有正确的，别手搓

`testkit::production_sources()`（花括号配平剥测试模块）。本 session 我手搓了四版临时扫描器，
四版各有 bug 且**是同几种**——完整清单与教训见 §3.31 与 `idempotency.rs` 的用例注释。

#### 五、结论为「无缺陷」时，把它记下来

本轮有相当一部分小节的结论是**没有缺陷**（未成年保护 §3.12、feature 门控 §3.14、
两套 agent 循环与会话面 §3.29、幂等 §3.31……）。记它们有两个用处：
下一个人不必重查；以及**说明「N 处手工判定」不必然是缺陷**——得看那 N 处的差异有没有理由
（§3.12 的拉黑/举报不设年龄门就是有理由的，「顺手统一」反而会伤人）。

---

### 3.31 幂等：钱路径**没有缺口**；而钉住它的红线第一版结构上看不见建房端点（**2026-07-28**）

`idempotency::guard` 是**手工挂在各端点上**的，钱路径漏一处就是重复扣费/重复发放
（客户端重试、移动网络切换、用户连点都会触发）。

**核的结论：没有缺口。** 十个会动钱/资产的 POST 端点全都挂了守卫：
`revive_match` · `publish` · `chapter_finish` · `open_ifline` · `spectator_gift` ·
`claim_gift` · `buy_cloud_growth` · `buy_item` · `synthesize` · `create_room`。

#### 🔴 但红线第一版数出来只有 9 个——漏的正好是 `create_room`（建房收开房费）

原因是**结构性**的：那条路由的 handler 是个**变量**——
`let worlds_route = get(list_worlds).post(create_room);` → `.route("/worlds", worlds_route)`。
按 `.route(` 段找 handler 的扫描器**看不见它**。而它恰恰是最该被这条红线覆盖的那类端点。
改成扫全仓的 `post(<标识符>)` 之后才对上 10 个。

#### ⚠️ 这一节真正的产出：别再手搓源码扫描器

核这件事时我用 Python 临时写了四版取数，四版各有 bug，而且是**同几种**：

| bug | 本 session 犯了几次 |
|---|---|
| 按第一个 `#[cfg(test)]` 截断（中段有测试夹具，其后仍是生产码） | 2 |
| 用 `.route("` 直接匹配（长路由是跨行写的） | 2 |
| `[^)]*?post\(` 跨不过 `get(x).post(y)` 里的第一个 `)` | 1 |
| 函数体取到「下一个 `async fn` 为止」（窗口混进别人的代码） | 1 |
| 按**字节**截断窗口（中文注释里切在多字节字符中间，直接 panic） | 1 |

每一次都是靠故障注入才发现的。**仓里本来就有正确的取数**
（`testkit::production_sources`，花括号配平剥测试模块）——本节的红线用它，
并把这张表写进用例注释，省得下一个人手搓第五版。

🔵 故障注入两处全红：拿掉 `create_room` 的幂等守卫 → 点名它；
把扫描器退回「按路由找」→ 棘轮 10→9 当场红（即第一版的盲区本身被钉住了）。

### 3.30 回头复查自己写的红线：`contains` 型断言的第二处弱点（**2026-07-28**）

上一节末尾我写了「那些 `contains` 型红线值得复查，那是下一轮的事」。这一节做那件事。

逐条过了本 session 写的红线，找同一种弱点（**断言的字符串可能因为非守卫的原因出现**）。
多数是结构性断言（路径集合、字段闭包、索引先后、存在性缺席），不受影响。
**找到一处**：§3.10 那条读取面红线断言「SQL 里含 `moderation`」——

`clips` 有一条 `SELECT ..., moderation FROM world_events WHERE id = $1 AND world_id = $2`，
把它放在**列表里**而不是过滤条件里。断言被满足，而 SQL 层一条过滤都没有。

那条查询**本身是安全的**（按 id 单取、Rust 侧复核，且 `clips::tests` 有
「即便被指名也拒绝」的行为用例钉着），但**红线分不出这两种机制**——
Rust 侧那道检查被删掉，红线也不会红。等于对它没有覆盖。

修：断言 `moderation` 必须出现在 **`WHERE` 之后**；Rust 侧复核那条显式登记为豁免，
并在注释里指明**钉住它的那条行为用例**（豁免不是免检，是换了个地方检）。
另加「未过滤的查询恰好一条」的计数。

🔵 注入实测：把 `moderation` 从 `WHERE` 挪进 `SELECT` 列表 → **红**（这正是旧版漏掉的那一支）。

⚠️ **一处推理修正，如实记**：我原以为「把豁免条件放宽成 `true`」能把红线整个关掉，
实测**单独放宽并不红、也确实无害**——计数数的是「SQL 里没有过滤」的查询数，
那是 **SQL 的性质**，与豁免条件无关。真正危险的是**组合**（放宽豁免 + 新增未过滤查询），
那时计数 1→2 当场红（组合注入实测）。这段推理过程写进了代码注释。

**两次撞见同一类弱断言之后的结论**（已写进两处用例注释）：
`contains` 型红线要问「这个词出现在**哪**」，而不是「出现**没有**」。

### 3.29 三处接缝核过**都干净**；而钉住它的那条红线我改了四版才真的成立（**2026-07-28**）

继续按「缺陷在两层之间」找接缝。这一轮**没有找到缺陷**——三处都是干净的，如实记：

| 接缝 | 结论 |
|---|---|
| OpenAI 与 Anthropic 两套 agent 循环 | ✅ 所有闸对称（压缩 / 裁剪 / 工具过滤 / 轮次上限 / 系统提示词）。唯一不对称的是工具定义函数——格式不同，且**两者都走** `filtered_agent_tool_definitions` |
| 手机端会话的读 / 写不对称 | ✅ `save` 比桌面多三道限制是**有意的**（手机是受限面）；而 `list`/`load` 带**同样**的限制，`load` 还在读回后再查一次 `session_kind`（纵深） |
| 另两个带 id 的会话端点（analyze-memory / archive） | ✅ 同一道限制都在 |

那道限制是**手工抄在五处**的，所以核完把它钉住（不是修缺陷，是防下一个：
第六个碰会话的端点漏抄，手机端就能读到不该看的会话，而没有任何用例会红）。

#### 🔴 但那条红线我改了四版，前三版都是**假绿**——这一段比上面的结论更值得读

| 版本 | 问题 | 怎么发现的 |
|---|---|---|
| ① | 按第一个 `#[cfg(test)]` 截断源码 | **踩了我自己在 `testkit::production_sources` 里写明的坑**（文件中段有测试夹具，其后仍是生产码）。扫出 0 个端点 |
| ② | 用 `.route("` 直接匹配 | 长路由是**跨行**写的（`.route(\n  "/api/..."`），全部漏掉。**同一个错本 session 犯了第二次**（第一次是手工 grep 路由） |
| ③ | 函数体取到「下一个 `async fn` 为止」 | handler 相邻或位于文件末尾时，窗口里混进**别人**的检查，漏抄的那个被顶成绿。改成花括号配平 |
| ④ | 断言「函数体里出现 `partner-session-`」 | **太松**：`archive_session_memory` 里另有一句 `if id.starts_with("partner-session-")` 的**业务分支**，守卫被删、分支还在，照样绿 |

第四版才是对的：断言**否定形式的守卫**（`!x.starts_with(..)` / `!= ".."`），
而不是「这个字符串出现过」。五个端点逐个删守卫，全部变红。

**教训写在用例注释里**：`contains("某字符串")` 型的红线看起来在检查，实际可能只是在检查
「这个词还在文件里」。故障注入连着三次没红时，该怀疑的是断言而不是注入。

### 3.28 上下文压缩：保护在**选择器**里、不在**应用器**里（**修于 2026-07-28**）

按上一节末尾那个观察（「缺陷都在两层之间」）继续找接缝。这一处正是同一形状。

`tool` 消息必须紧跟在声明了对应 `tool_calls` 的 assistant 之后，否则 OpenAI / Anthropic
**整个请求 400**，玩家看到「生成失败」。

**边界选择器是安全的**（两条路都查过）：
- `select_turn_based_compaction_boundary` 取第 5 个 `user` 的前一条 → 保留段从 `user` 开始；
- `select_compaction_boundary` 里有 `while start > 0 && history[start].role == "tool" { start -= 1 }`。

**但应用器 `effective_history_with_compaction` 没有这道跳过。** 它用的不是「刚算出来的边界」，
而是**存下来的** `compacted_through_message_id` / `compacted_through_index`
（历史被编辑过、或记录由更早版本写下时，可能落到别处），直接 `index + 1` 取后缀。

已用探针实测：边界落在带 `tool_calls` 的 assistant 上时，应用器吐出
`["user"(摘要), "tool"(孤儿), "assistant"]`。

⚠️ **`trim_history_to_context_budget` 救不了这一支**：它只剥**索引 0** 的 `tool`，
而索引 0 此时是压缩摘要那条 `user` 消息。两道保护各自看着都对，接缝处没人管。

修：应用器补上同样的跳过。**向前跳（丢掉孤儿）而不是向后退（把 assistant 拉回来）**
——那个 assistant 已经在摘要里了，拉回来等于同一段内容出现两次。

🔵 故障注入三处全红：退回不跳过（改动前形态）→ 红；
跳过写成无条件 `+1` → **正常消息被吃掉**，红；写成只跳一次（`if` 而非 `while`）→
第二个孤儿残留，红。

### 3.27 🔴 子代理不继承父级的工具白名单——一次 `subagent` 调用绕过整套限制（**修于 2026-07-28**）

查 agent 文件工具的工作区限制时顺出来的，比原目标严重。

**先看限制本身是完整的**：`read`/`grep`/`glob` 过 `ensure_read_path_allowed`，
`write`/`edit` 过 `ensure_write_path_allowed`，`bash` 过黑名单 + 授权握手，
`skill` 是在已发现集合里查名字（不拼路径）。前端各模式的白名单也是分层的：
伴侣聊天 `[]`（一个工具都没有）、写作 agent 有文件工具但**刻意不含 `bash`**。

**但 `AgentRunOptions::subagent()` 里写死 `allowed_tools: None`——即「全部工具放行」。**

于是：写作 agent 的白名单是 `[read, write, edit, grep, glob, skill, subagent, todo]`，
**没有 `bash`**，却**有 `subagent`**。它调一次 `subagent`，子代理拿到的是「全部工具」，
其中就有 `bash`。**「不给这个模式 bash」这条限制，被一次工具调用绕过了。**

⚠️ 子代理的任务文本来自父模型的一次工具调用——**被读入文档里的提示注入可以指定它**。
这不是理论路径：agent 会 `read` 工作区里的文件，而工作区可以是用户从别处拿到的项目。

修：`subagent()` 接收并继承父级的 `allowed_tools`。
- 父级 `None`（不限制）→ 子级仍 `None`，**既有行为逐字不变**；
- 父级有白名单 → 子级同一份；
- `excluded_tools` 仍钉着 `subagent`（不得递归派生），这一条不因继承而放宽。

🔵 故障注入三处全红：退回写死 `None`（改动前形态）→ 红；
**有参数但调用点传 `None`**（更隐蔽的退化，对玩家后果一样）→ 接线红线红；
继承时顺手放开递归 → 红。

用例用的是写作 agent **真实**下发的那份白名单（逐字照抄自 `AgentChat.tsx`），
不是构造的样例——构造的样例证明不了「线上那个模式确实被绕过」。

### 3.26 路径拼接面扫完：会话与状态**无缺陷**，crawler 的书名漏了 `.`/`..`（**2026-07-28**）

上一节末尾我写了「路径穿越这条线扫完了」——**那句话当时没有依据**。这一节把它真正扫完：
列出 `src-tauri` 生产码里**全部**把变量 `join` 进路径的位置，逐个看实质。

**结论：三处无缺陷，一处小缺口，一处附注。**

| 位置 | 结论 |
|---|---|
| 会话 读 / 写 / 删（`agent/sessions.rs`） | ✅ 三个入口**都**过 `sanitize_session_id`（含删除走的 `agent_session_path`）。无缺陷 |
| 应用状态 `load/save_app_state_path` | ✅ 两处都查 `/`、`\`、`..`。`/api/mobile/state/{name}` 那条局域网入口因此安全 |
| `crawler::sanitize_filename` | ⚠️ **有缺口**，见下 |
| `fs_commands::rename_item_cmd` | 附注：`new_name` 未校验，但来源是**用户自己在文件树里输入的名字**，且 agent 工具集里没有 rename——属自伤而非越权。**不当缺陷处理**，如实记 |

**crawler 的缺口**：书名/章节名来自**远端页面标题**（真正的外部输入）。
`sanitize_filename` 把 `/` `\` 等换成全角（穿越确实挡住了），但 `.` 与 `..` 它管不着——
而 `crawl_*_book` 的 `book_folder = target_path.join(&safe_novel_name)` 那一处**没有拼扩展名**，
于是书名恰好是 `..` 时，整本书会写到用户所选目录的**上一级**。空名会让 `join("")` 退化成目录本身，
以 `.` 开头则写出隐藏项。已收口（换成安全值而不是拒绝——见下）。

#### 🔴 全仓现在有**三份**名字校验，严格度不同，**不该合并**

已把这张表写进 `utils.rs` 的注释，防的是将来有人「顺手统一」：

| 判据 | 用于 | 严格度 | 为什么 |
|---|---|---|---|
| `validated_path_component` | 技能名、版本 id | 任意字符集，只挡穿越 | 技能名可以是中文、版本备注可带空格；白名单会挡掉合法输入 |
| `sanitize_session_id` | 会话 id | ASCII 字母数字 + `-` `_` | id 是**机器生成**的，可以也应该更严 |
| `crawler::sanitize_filename` | 远端书名 | **不拒绝，改写** | 面对远端标题，拒绝等于抓取失败 |

差异不是历史遗留，是**输入来源不同**（人写的名字 / 机器生成的 id / 远端内容）。

🔵 故障注入三处全红：去掉 `.`/`..` 收口（改动前形态）→ 红；
为防穿越把非法字符整个删掉 → **正常书名被改坏**，红；分隔符不再替换 → 红。

### 3.25 版本历史的 `version_id` 未校验——构造过的 meta.json 可读/删任意文件（**修于 2026-07-28**）

技能包那处（§3.22）之后，按同一形状把「外部来的名字被 `join` 进路径」的地方查了一遍。
`commands/workspace.rs` 的 `load/save_app_state_path` **是校验了的**（两处都查 `/`、`\`、`..`），
`/api/mobile/state/{name}` 那条局域网入口因此安全。

但 `commands/versions.rs` 没有：

```rust
fn get_version_file_path(file_path: &Path, version_id: &str) -> PathBuf {
    parent.join(".versions").join(file_name).join(version_id)   // version_id 未校验
}
```

`read_file_version` 会 `read_to_string`、`delete_file_version` 会 `remove_file`。
**写入侧生成的是 uuid（安全），问题在读回侧信任了 `.versions/<文件名>/meta.json` 里写着的 id**
——而那个文件在**用户打开的工作区文件夹**里（比如从网上拿到的项目）。
构造 `id: "../../../x"` 即可读到 / 删掉 `.versions` 之外的东西。

修：判据提到 `utils::validated_path_component` **一处共用**，
技能包那两个入口也改为复用它（不在仓里留第二份同判据）。三个调用点：
技能导入、技能删除、版本文件路径。

🔵 故障注入四处全红：版本路径退回不校验（改动前形态）→ 红；
共用判据改用白名单字符集 → **版本与技能两侧同时红**（证明确实共用）；
去掉承重的分隔符检查 → 红。

⚠️ 沿用 §3.22 的取舍并写进共用判据的注释：**不限制字符集**（版本备注可以是中文、可以带空格），
以及 `components().count()` 那一条**不承重**（已实测），免得有人以为每条都被验证过。

### 3.24 工具能力闸：执行侧**有闸**（无漏洞），但声明侧另抄了一份判据（**2026-07-28**）

`allowed_tools` 是按请求限制能力的闸（冒险模式只开放部分工具）。查它有两个问题要分开回答：

**① 执行时到底查不查？——查。没有漏洞。**
`execute_agent_tool_inner` 的**第一句**就是 `options.allows_tool(tool_name)`，
模型指名一个没被声明的工具（幻觉或提示注入）会被当场挡下。

⚠️ 我一度以为「`allows_tool` 只在测试里被调用」并准备按缺陷处理——那是**我读错了自己的 grep 输出**
（`head` 把结果截断在 models.rs 的测试行）。重查后确认执行侧的闸存在。
如实记下来：这一步差点报出一个假发现，**截断过的输出不能用来下结论**。

**② 同一判据有两份实现。** 执行侧调 `allows_tool`，声明侧
（`filtered_agent_tool_definitions`）内联抄了一遍。今天两份逻辑相同，但漂移方向**不对称**：

- 声明侧变宽 → 工具被声明、执行时被拒 → 报错可见，**安全**；
- **执行侧变宽而声明侧不动 → 执行允许的比声明的多**。模型平时不会调没声明的工具，
  但提示注入可以指名一个，那时挡它的只剩那道已经被放宽的闸。

已让声明侧直接复用 `allows_tool`（顺带去掉每次比较的 `to_string()` 分配）。
另加两条用例：逐个工具比对两侧结论必须一致；源码层钉住执行侧的闸在 `match tool_name` **之前**。

🔵 故障注入三处全红：声明侧退回内联拷贝并改宽 → 红；删掉执行侧的闸 → 红；
把 `allows_tool` 的白名单判断去掉 → **既有用例**红。

### 3.23 手机端令牌整个会话留在地址栏——清理只挂在失败分支上（**修于 2026-07-28**）

接着 §3.18（服务端把令牌吐给局域网）往前端查同一条线。

手机端是扫码打开 `http://<内网 IP>:<端口>/?token=xxx` 进来的，**令牌就写在 URL 里**。
服务端在首次加载 `/` 时把它落成 `HttpOnly; SameSite=Lax` cookie，此后请求靠 cookie
（`credentials: 'same-origin'`）即可——**URL 里那份已经不需要了**。
`runtime.ts` 的注释也是这么写的：「Keep the URL token only in memory」。

但清理函数 `clearMobileToken()` 全仓**只在 `MobileHome` 的 catch 分支被调用**
——也就是**只有令牌无效时才抹**。验证成功的那条路上，令牌整个会话都留在地址栏：
浏览器历史、书签、截图与投屏、以及「把这个链接发给另一台设备」，每一样都会把它一起带走。
而拿到令牌的人可以调用手机端全部接口。

修：拆成两个语义。`stripMobileTokenFromUrl()` 只抹 URL（验证成功后调），
`clearMobileToken()` 保持登出语义（内存 + URL，失效时调）。
⚠️ **成功路径上不能连内存一起清**：cookie 生效前的那一瞬还要靠内存里那份做回退，
所以是两个函数而不是复用一个。

🔵 故障注入三处全红：成功路径不再清理（改动前的真实状态）→ 红；
清理时顺手把内存令牌也清掉 → 红；把其它查询参数一并抹掉 → 红。

⚠️ 除了行为用例，另加一条**接线红线**：光有清理函数没用，它必须真的出现在
`setConnectionStatus('verified')` 之后、且不在 catch 之后——因为改动前的状态恰恰是
「函数存在、只是没挂在成功路径上」。

### 3.22 技能包的名字直接当目录名用——导入即可写出 skills 目录（**修于 2026-07-28**）

技能包**支持用户导入**（CLAUDE.md 明写；从网上拿到的技能包也算），
而 `SKILL.md` 的 frontmatter 里那个 `name` **未经任何校验**就被拼进路径：

```rust
let dest = skills_dir.join(&skill.name);   // import_skill
...
.join("skills").join(&name);               // delete_skill → fs::remove_dir_all
```

于是：
- `name: ../../../Documents` → 导入时写到 skills 目录**之外**；随后在 UI 里点删除，
  `remove_dir_all` 把那个目录树删掉。
- `name: /Users/x/.ssh` → **Rust 的 `Path::join` 遇到绝对路径会整个替换基路径**，连 `..` 都不需要。

修：两个碰文件系统的入口共用同一道校验 `validated_skill_dir_name`
（非空、不含分隔符、不是 `.`/`..`、不以 `.` 开头、必须是单个分量）。

⚠️ **刻意不用白名单字符集**：技能名可以是中文、可以带空格。用字符集会把合法技能挡在外面，
而挡住穿越并不需要那么严。这一条由「合法名不得被误拒」的用例钉住
（`我的写作技能` / `My Skill 2` / `技能-第2版`）。

🔵 故障注入三处全红：校验退回「什么都不查」（改动前形态）→ 红；
去掉承重的分隔符检查 → 红；改用白名单字符集 → **误拒用例红**。

⚠️ **一条如实记的负结果**：`is_absolute() || components().count() != 1` 那一条
**在本平台上不承重**——单独去掉它用例仍全绿（绝对路径必含分隔符，先被上一条拦下）。
留着是因为它表达的是意图而非手段，但已在代码注释里注明它没被验证过，
免得下一个人以为每一条都是承重的。

（写这次注入还发现自己的 shell 转义写错过一次，那次注入根本没改到代码却显示「ok」——
**注入显示绿时要先确认它真的改到了东西**。）

### 3.21 危险命令黑名单只认一种拼法，`rm -fr` 直接执行不问用户（**修于 2026-07-28**）

bash 工具的护栏是**黑名单 + 用户授权握手**：命中黑名单才弹确认，没命中就直接跑。
也就是说**漏一条 = 那条命令无声执行**，失败方向朝外。

实测（用等价正则逐条跑正负样本，不靠读代码推断）——下面这些**全都没被拦**：

| 没拦下的 | 为什么它不是「绕过技巧」 |
|---|---|
| `rm -fr /x` | 与 `rm -rf` 是同样两个标志，只是换个顺序 |
| `rm -r -f /x` | 分开写，等价 |
| `rm --recursive --force /x` | 长选项，等价 |
| `curl x \| sh` | 安装脚本的**标准**写法就是 `\| sh`；原先只认 `bash` |
| `wget x \| sh` · `curl x \| zsh` | 同上，shell 家族只认了一个名字 |
| `cat x > /dev/nvme0n1` | 原先只认 `sd[a-z]`；而**现代机器的系统盘几乎都是 NVMe**，虚拟机是 `vd*`，树莓派是 `mmcblk*` |

这些都是人平时的写法。黑名单只认其中一种拼法，它挡住的比看起来少得多。

修：`rm` 的「递归+强制」改为覆盖四种写法（同束里 r 前/f 前、分开写的两个方向，含长选项），
`[^|;&]*` 把匹配限制在同一条命令内（避免 `rm x && ls -rf` 这类跨命令误判）；
下载管道认整个 shell 家族；块设备覆盖 `sd/nvme/vd/hd/mmcblk`。

⚠️ **两个方向都钉住了**。补漏的同时加了一条「日常命令不得被误拦」的用例
（`rm -r build`、`rm -f tmp.txt`、`docker rm -f x`、`grep -rf`、`echo > /dev/null` 等）——
一道天天误报的闸会被人学会无脑点确认，那比没有闸更糟（与 clippy 那道门的取舍是同一件事）。

🔵 故障注入四处全红：三条分别退回改动前的形态（只认 `-rf` / 只认 `| bash` / 只认 `sd[a-z]`）→
各自点名漏掉的那条命令；第四条把 `rm` 判据放宽成「见 rm 就拦」→ **误拦用例红**。

⚠️ **本节不宣称补全了黑名单**——黑名单天然不完备。这里修的是「**已知的常规写法**别再漏」，
不是「挡得住有意绕过的人」。

### 3.20 手机端收不到 `start` 事件：SSE 通道注册晚于 run 起跑（**修于 2026-07-28**）

`dispatch_stream_event` 在 SSE 调度表里查不到 run_id 时**静默丢弃**事件
（`if let Some(sender)` 没有 else 分支）。这本身是合理的——桌面态本来就没有 SSE 订阅者——
但**正因为如此，注册的时机就成了正确性问题**：早一步事件都在，晚一步事件就没了。

而 `start_run_endpoint` 此前的顺序是：

1. `start_chat_stream_inner(...)` 起 run（**派生任务的第一句就是 emit `start` 事件**）
2. 拿到 run_id 之后，才 `sse_dispatcher().insert(run_id, tx)`

两行之间任务已经在跑，于是手机端**收不到 `start`**（以及此后到注册完成为止的一切事件）。
注册之后的事件不会丢——`UnboundedSender` 会一直缓冲到玩家真的来订阅。

修：给 `start_chat_stream_inner` 加一个 **pre-start 钩子**（`_with` 变体），手机端在钩子里注册通道，
钩子调用点在 `spawn` **之前**。
⚠️ 用钩子而不是「无条件注册」：无条件注册会让**桌面**每次跑 run 都挂一个没人读的
`UnboundedSender`，整段 run 的事件全缓冲在内存里直到 `clean_stream`——
为修手机端的丢事件而给桌面端加一份无人消费的缓冲，是拆东墙补西墙。

两条用例：一条把「未注册即静默丢弃」这个失败模式**本身**钉下来（否则下面那条顺序红线的意义写不明白），
一条在源码层钉顺序（体例同 `lexicon` 那条闸的 `resolve < begin < apply`）。

#### 🔴 顺序红线第一版是**恒真**的，故障注入当场证明

`include_str!("mobile_server.rs")` 把**测试自身的文本**也包了进来，而断言里逐字出现
`start_chat_stream_inner_with(` 这个字面量——于是 `find` 永远命中自己写的那句话，
**把接线改回改动前的形态，它照样绿**。剥掉 `#[cfg(test)]` 之后重做注入才变红。

这条值得单独记：**源码级红线扫自己所在的文件时，会读到断言自身的字面量**。
本 session 的 server 侧红线都走 `testkit::production_sources()`（会剥测试模块），
不会有这个问题；`src-tauri` 没有那套设施，手写 `include_str!` 就踩上了。

⚠️ 另有一处注入（给静默丢弃补兜底）**没能验成**：兜底新建的通道随即被用例自己的 `insert` 覆盖，
观测结果不变。如实记下——那次注入没有证明任何东西。

### 3.19 密钥抹除与回写保护是**两份手工拷贝**，漂开就会抹掉用户的 API Key（**修于 2026-07-28**）

接着 §3.18 往同一文件里查。`mobile_server` 里有一对**配套**的判断，各写了一遍
`k == "llmApiKey" || k == "apiKey"`：

- `sanitize_settings_state` —— 发给手机的设置里，**抹掉**密钥；
- `merge_settings_preserving_keys` —— 手机回写时，**跳过**空密钥（免得覆盖桌面的真 key）。

**两份拷贝一旦漂开，后果很具体**：桌面有 `newSecretKey: "sk-真的"` → 抹除认得它、发给手机是 `""`
→ 回写保护**不认得**它 → 手机一存设置，桌面那把真 key 就被空串覆盖。
也就是**用户在手机上打开一次设置，桌面的 API Key 当场丢失**。

今天两份是一致的（都只有那两个字段），所以**没有活跃 bug**；但这是本文件反复登记的那个形状，
且失败模式是不可逆的数据丢失。已收归 `is_secret_settings_key` 一处。

顺带把判据从**逐字段列举**改成**按模式**（`apikey` / `secret` / `password` / `credential` /
`accesskey` / `privatekey` / `authtoken`，大小写无关的子串）：新增 `azureApiKey` 这类字段
**默认就被抹**，不必有人记得来改。方向与 §3.9 的机审那次相同——漏抹的代价是密钥出本机，
多抹的代价是多抹一两个无关字段，两者不对称。

⚠️ **同时加了一道类型闸：只抹字符串值**。模式匹配会命中 `isSecretRealm` 这类布尔字段，
而抹除写的是空串——把布尔/数字改成空串会让读它的那一侧类型崩掉。
「该抹的别漏」和「不该抹的别抹坏」都要。

🔵 故障注入三处全红：
- 让回写保护退回逐字段列举（即改动前的形态）→ **端到端用例红**（桌面 key 被抹掉）；
- 判据退回逐字段列举 → 红（新密钥字段不再被抹）；
- 去掉类型闸 → 红（布尔被抹成空串）。

### 3.18 🔴 手机服务的免鉴权状态端点把访问令牌吐给了局域网（**修于 2026-07-28**）

本 session 最严重的一处。

`src-tauri/src/mobile_server.rs` 的 token 鉴权是**中间件 + 公开路径排除表**（设计是对的）：
`/`、`/assets/*`、`/api/mobile/status` 免鉴权，其余全查 token
（`X-Mobile-Token` 头 / `?token=` / cookie 三选一）。

问题出在那个免鉴权的 `status` 上：

```rust
async fn get_mobile_status() -> impl IntoResponse {
    let status = get_status();
    axum::Json(status)          // ← MobileServiceStatus 里装着 token
}
```

`MobileServiceStatus { is_running, url, token, error }` 是 `Serialize` 的，
`token` 就是访问令牌，而 `url` 是 `http://<lan_ip>:<port>/?token=<token>`。
于是**局域网上任何人 `GET /api/mobile/status` 就能拿到令牌**，
再用它调用其余全部端点：会话列表、会话正文、起停对话、应用状态……
**整套 token 鉴权被这一个端点旁路掉。**

🔴 形状还是那个老形状：**同一个结构体服务两类信任级别不同的读者**。
桌面侧 `Settings.tsx` 用 Tauri `invoke` 拿完整结构渲染二维码/URL——那一路只在本机进程内，
是正当的、**未改动**；HTTP 这一路必须裁剪。

修：公开端点只回 `{ isRunning, error }`。
⚠️ **裁掉的不只是 `token`，还有 `url`**——它把 token 写在 query 里，只删 token 是没用的。
前端类型也按「两者的交集」收窄（需要 token/url 的桌面页直接 `invoke`）。

🔵 故障注入两处全红：
- 还原成改动前的 `Json(status)` → 红（用例断言响应体里**一个字节都找不到**那个令牌，
  而不是逐字段断言——逐字段的写法在将来加字段时会漏）；
- **只删 `token`、保留 `url` 的半吊子修法** → **照样红**。这一条是本次注入里最该有的一条。

⚠️ 手机端在应用内**没有任何代码调用**这个端点（`appInvoke('get_mobile_service_status')`
全仓只有类型定义、switch 分支与用例引用它），也就是说这个泄露一直存在、却没有任何功能依赖它
——「没人用」不等于「不可达」，它是公开路由。

### 3.17 桌面轨：`appInvoke` 三处手工同步里，类型表多出一个 `read_file`（**修于 2026-07-28**）

本 session 首次扫**桌面轨**。CLAUDE.md 明写着一个三处手工同步的形状：
「给手机端加命令需要改三处：Tauri command 注册（`lib.rs`）+ `mobile_server.rs` 的 axum 路由 +
`appInvoke` 的 switch 分支」——手工维护的 N 处判定，正是本文件反复登记的那个形状。

**先核对，两处是干净的**：`appInvoke` 映射出去的 7 条 `/api/mobile/*` 路径，服务端全都提供
（我第一次提取路由只拿到 8 条、以为对不上，是**我的正则漏了跨行 `.route(`**——
半份清单不能下结论）；switch 的 `default` 分支是 `throw`（不是静默返回），也对。

**第三处有问题**：TypeScript 的 `AppInvokeCommands` 类型表声明 15 个命令，switch 只有 14 个分支
——**`read_file` 在表里、没有分支**。`switch (cmd)` 不是穷尽检查，少一个 `case` 照样编译过，
手机端调它会「类型检查通过、运行时抛异常」。

🔴 **而它不该被补上分支，该从表里删掉**：`read_file` 读的是**任意路径**。进 `appInvoke` 的类型表
等于宣告「手机端也支持」，而支持就要在 `mobile_server.rs` 上开对应路由——
那是**在局域网上开任意文件读取**（`~/.ssh/id_rsa` 之类）。CLAUDE.md 的约定本来就是
「只在桌面用的命令直接 `invoke`，不必进 `appInvoke` 的类型表」。

**当前不可达**（唯一调用方 `bookTravelMaterials.ts` 只被桌面页 `BookTravelMaterials.tsx` 用，
手机端只渲染 MobileHome/Chat/Story/Bond），所以这是**潜在**缺陷而非活跃 bug。
但那条声明是一张邀请函：下一个人看到「表里有、switch 没有」，最自然的动作就是补分支。

已改为直接 `invoke` + 从类型表删除，并加 `src/__tests__/runtime-appinvoke-contract.test.ts`
钉四条：声明⊆分支、分支⊆声明、解析器没坏、**`read_file` 永不回到表里**。

🔵 故障注入两处全红：把 `read_file` 放回类型表（即今天之前的真实状态）→ 两条红；
删掉一个 switch 分支 → 红。

### 3.16 「无提现」红线：核过没有出口，但守它的只有一句注释和五个猜出来的 URI（**2026-07-28**）

**核的结论：没有提现出口。** `ledger_accounts.withdrawable` 全仓只有一处写入
（`ensure_account` 建户 INSERT，字面量 0），没有任何 UPDATE；路由里也没有提现语义的路径。

但守着它的东西不够：

| 原有的守卫 | 它挡不住什么 |
|---|---|
| `ensure_account` 上一句注释「红线：`withdrawable` 恒 0」 | 任何新写入路径。而 `GET /me/earnings` 是**读库**回这个标志的（不是硬编码 false），写成 1 就当场在读取面上开了提现出口 |
| `no_withdraw_or_payout_endpoints`（试 5 个**猜出来**的 URI 是否 404） | 换个名字的端点。注入 `/me/wallet/cashout` 实测：新红线红，**这条旧用例照样绿** |

补两条源码级红线：`withdrawable` 的写语句必须**恰好一条**且取值是字面量 `0`
（绑定参数 `$N` 也不行——那意味着「取决于运行时」，正是这条红线不允许的）；
路由注册里不得出现提现语义的词。原来那条按 URI 试 404 的用例**保留**——
一个查「路由表里有没有」、一个查「打过去到底通不通」，都要。

#### ⚠️ 写这条红线时连撞两个「一词两义」，两个都值得记

1. **`withdraw`**：`cloud_characters.withdrawn` = **停止投放**（下架一张已发布的卡 / 世界模板，
   `memorial` 封卷也复用它），与钱无关；`ledger_accounts.withdrawable` = **可提现**，
   才是 §0.5 管的那个。红线按**完整路径**豁免资产义的两条，不按词豁免——
   按词豁免等于把 `withdraw` 整个从黑名单里拿掉，`/me/earnings/withdraw` 也会被放过。
2. **`payout`**：`/assembly/payoutTable` 是 `assembled_json` 里的 **JSON 指针**，
   指「产出表」（战利品），不是付款。第一版按「以 `/` 开头的字面量」筛路径，当场误报了它；
   改为只扫真正的 `.route("…")` 注册才对。

🔵 **这个歧义本身是有信息的**：有人为了核「无提现」去 grep `withdraw`，会先撞见一堆资产下架
——既可能误判成「有提现出口」，也可能因为「看起来都是资产的」而漏掉真的那个。

🔵 三处故障注入全红：建户的 `withdrawable` 改成绑定参数 → 红；加一条 `UPDATE … SET withdrawable = 1` → 红；
注册 `/me/wallet/cashout` → 红（且旧用例绿，见上表）。

### 3.15 AI 生成标识「已实现」只对一部分读取面成立（**修于 2026-07-28**）

总规格的六条平台红线里，第 6 条写着「显式标注（aiLabel，**已实现**）」。核过之后那句话是**过宽的**：

| 读取面 | 改动前 | 内容性质 |
|---|---|---|
| `events` / `clips` / `worlds` / `livestage` / `onboarding` | ✅ 有 | 世界事实投影、切片、传记 |
| **`ifline`** | ❌ **没有** | **整段模型生成的付费正文**（玩家烧副本卡换来的东西） |
| **`reports`** | ❌ **没有** | 日报的独白（`kind: model_inference`）与摘要 |
| **`arena`** | ❌ **没有** | 战报里每条 `summary` |

三处已补齐，口径与既有五处逐字一致，各带一条用例（注入实测：逐个撤掉标识 → 各自变红）。

⚠️ **`reports` 那处要说清楚**：它本来就有 `provenanceLegend`（`public_fact` / `private_view` /
`model_inference`），看起来"已经标了来源"。但那两者**不是一回事**——来源图例标的是
「这条信息从哪来」（供玩家判断可信度），AI 标识标的是「这是 AI 生成内容」（合规义务，
面向监管与知情权）。两者并存，不互相顶替。把前者当成后者是这类漏标最容易发生的方式。

🔵 **同样核过、确认不该加的两处**（如实记，避免下一个人"顺手补齐"）：
`annotations` 的 `body` 是**玩家写的** OOC 注解，`interventions` 的托梦是**玩家写的**输入
——给玩家自己写的内容打 AI 标识是另一种错（把标识变成噪声，真正的 AI 内容反而不显眼）。
`memorial` / `chapters` 返回的是元数据与创作者撰写的道具文案，不是逐次生成的模型输出。

总规格第 6 条那句"已实现"已同步订正为「原写作已实现，实为只对一部分读取面成立，现已补齐」。

### 3.14 经济 feature 关闭时「一条付费路径都不可达」——核过是对的，补上行为面证据（**2026-07-28**）

**核的结论：没有缺陷。** 门控是**编译期强制**的——无 `billing`/`arena` 时 `ledger` 模块根本不存在，
任何调用点不 gate 就编译不过，而 default 构建是通过的。全仓只有 5 处函数体内的 feature 分叉，
**全是路由注册**（端点存在与否），没有一处是「端点还在、只跳过扣费」那种危险形态。
`POST /worlds`（建房携开房费）随经济 feature 门控、`GET /worlds`（大厅）恒在，理由写在代码里。

但编译器保证不了**路由是否真的不存在**：有人完全可以注册一个不碰账本、却暴露付费内容的端点，
那样编译照过。故补 `app::economy_gate_tests`——**只在默认构建里编译**，断言付费前缀 404、
建房 405（同路径方法差异，不是 404，混为一谈会让断言在错误的地方绿）、大厅仍在。

🔵 三处故障注入，**结果不一样，如实记**：
- 给默认构建注册一个不碰账本的 `/me/earnings` → **红**（编译器管不到、本用例管得到的那一类）；
- 把大厅列表也一并门掉（过度门控）→ **红**；
- 把 `POST /worlds` 错误注册进默认构建 → **编译不过**（`create_room` 自己要调 `ledger`）。
  这一种错编译器已经挡住了，本用例在这点上是**纵深而非唯一防线**——
  写清楚比笼统说「三处全红」诚实。

### 3.13 确定性契约「禁三样」只有一个模块有闸；顺带发现我自己造的一处盲区（**2026-07-28**）

#### ① 「禁三样」扩到全仓

「禁三样」（系统随机 / 浮点 RNG / map 迭代序驱动 RNG）此前只有 **`ifline` 一个模块**有源码级红线，
而这句话本身住在 `assembly/mod.rs`——**装配与采样才是确定性最要紧的地方，却没有任何闸**。

`assembly::tests::red_line_no_irreproducible_randomness_outside_the_exempt_list`：
按「排除表而非包含表」扫全仓生产码，新模块默认被覆盖。方向与 §3.9 那次相同，理由也相同——
漏扫一个模块，那里的不可复现随机不会报错，只会让黄金世界回归**偶发**变红，
而偶发红的第一反应通常是「重跑一下」。

豁免两处，各有理由：`auth/mod.rs`（会话 token 与短信验证码**必须**是密码学随机，
确定性 token 是安全漏洞）、`db.rs`（全仓唯一时间源 `now_ms`）。
红线同时反向断言「豁免项里确实含被禁 API」——否则说明扫描根本没读到东西。

⚠️ **不禁 `HashMap`**：契约禁的是「用 map 迭代序驱动 RNG」，不是拿 map 做查表；
源码级扫描分不出这两者，一律禁只会逼出一堆无意义改写并让这道门被无视。
迭代序那一支仍由「同种子同结果」的行为用例负责。

🔵 故障注入三处全红：装配层加 `thread_rng` → 红；某模块绕过 `db::now_ms` 直接
`SystemTime::now` → 红；豁免名单多加一个不需要的条目 → 红（扫描器失效那一支）。

#### ② 🔴 我自己在本 session 给一条既有红线造了盲区

`ifline_sources()`（三条红线共用的扫描面：不写世界线 / 不铸资产 / 无系统随机）
是**手工列举文件**的，它的注释里早写着：

> 若扫描面不跟着扩，新增文件就是红线的盲区……将来再拆文件时**必须同步加进这里**。

而 0052 新增 `ifline/sweep.rs` 时**我漏了这一步**，三条红线对那个新文件静默失效了一整批提交。
所幸 `sweep.rs` 确实没有违反其中任何一条——**但那是运气，不是保证**。

修法不是「这次记得加」，而是不再靠人记得：`ifline_source_files_are_all_scanned`
从**目录**核对扫描面，少一个文件就红（`include_str!` 必须是字面量，故清单仍在代码里，
但「清单是否完整」交给用例）。🔵 注入实测：加一个新文件、不进扫描面 → 红。

**这条值得单独记**：本 session 修的多是「靠约定维护的判定」，而这一处说明
**连红线自己的扫描面也是那样的判定**——写下「必须记得」的那个人，就是没做到的那个人。

### 3.12 红线巡检两条：未成年保护（**核过，无缺陷**）与事实不可删（**补上红线**）

按「查同类模式」继续扫两条真红线。两条的结论不同，都如实记。

#### ① 未成年保护 —— **核过，没有缺陷，不动**

`auth::is_declared_adult` 早已收归一处，但「实现只有一份」不等于「该拦的路径都拦了」。
逐个数了 `social` 的九个玩家端点：五个身份端点全挂了 `ensure_adult_social`，
**四个没挂**——`list_blocks` / `create_block` / `remove_block` / `create_report`。

那不是漏挂，是**有意的**，模块头写得很清楚：

> ⚠️ **拉黑与举报不设年龄门**：它们是保护工具，不是社交特权。把未成年的举报/拉黑能力
> 一并关掉，是把"保护未成年"做成了"让未成年无法自保"。

而且**正反两个方向都已经有用例**：`red_line_minor_rejected_by_server_with_zero_side_effect`
钉身份端点必须拒未成年，`minor_can_still_block_and_report` 钉保护工具必须对未成年开放。
后者尤其重要——它挡的是「有人发现这个不一致、为了整齐给拉黑也加上成年门」这种**看起来像修 bug 的破坏**。

**这一条不需要任何改动。** 记下来是为了下一个人不必再数一遍，也为了说明：
「N 处手工调用」这个形状不必然是缺陷，得看那 N 处的差异是不是有理由。

#### ② 事实不可删 —— **补上红线**

`red_line_world_events_has_one_ratchet_and_one_guarded_relax` 把 `UPDATE world_events`
逐条钉死了形状（正文零改写 / 单向棘轮 / 一条带守卫的放宽），但**删除这一支全仓没有任何红线**。
而「回滚公共事实」最硬、最彻底的形态恰恰是删除：**改一列还留着痕迹，删一行连痕迹都没有。**

扫过一遍：生产码里对事实表**一次删除都没有**（处置手段全是状态标记——`content_takedowns`
+ 收紧 `moderation`，被拦内容仍落库留痕供申诉）。所以这条不变式今天成立，
`red_line_facts_are_never_deleted` 只是把它从「大家都这么做」变成「不这么做就红」。

覆盖：`world_events` / `world_ticks` / `ifline_beats` / `world_contributions` /
`ledger_postings` / `ledger_journals` / `audit_logs` / `risk_events`。
不含删除本就合法的表（幂等键、通知 outbox、运行时开关等有生命周期的行）。

🔵 故障注入三处全红：下架路径加一句 `DELETE FROM world_events`（最像「合理需求」的那种）→ 红；
删账本分录 → 红；表名写错 → 红（「有事实表一次都没被扫到」那一支，防的是扫描器失效）。

### 3.11 资产单一写入（§0.2 平台红线）此前只有注释和行为用例（**钉住于 2026-07-28**）

全仓有十来处注释写着「铸卡的唯一入口是 `subplot::grant_card_tx`」「道具唯一写入路径是
`backpack::grant_item_tx`」，`ifline` 那边也有行为用例钉「if 线不得铸卡」。
**但那些用例证明的是「某一个模块没违反」，不是「没有别的模块违反」**——
下一个模块直接 `INSERT INTO subplot_cards` 就凭空造出了资产，一条用例都不会红。

`assets::tests::red_line_each_asset_table_has_exactly_one_minter`：每张资产表，生产码里
只有登记的模块可以 INSERT。登记如下（**逐条核过，本身没有发现违规**）：

| 表 | 合法铸造点 |
|---|---|
| `subplot_cards` | `subplot/mod.rs`（if 线只改状态 owned→consumed，不铸） |
| `items` / `backpacks` | `backpack/mod.rs`（付费售卖 `shop` 复用 `grant_item_tx`） |
| `ledger_postings` / `ledger_journals` | `ledger/mod.rs` |
| `world_contributions` | `progression/mod.rs` |
| `cloud_characters` | **两个，都是有意的**：`assets/mod.rs`（玩家发布）+ `onboarding/mod.rs`（新手礼包预制卡） |

只管 INSERT 不管 UPDATE 是刻意的：铸造是「无中生有」，状态流转（if 线消耗卡、memorial 归还携带道具）
各自带 CAS 与用例，塞进同一条断言只会让红线失去焦点。

🔵 故障注入：在 `livegate` 里加一句 `INSERT INTO subplot_cards` → 红；把表名写错 → 红
（「一个铸造点都没扫到」那一支，防的是扫描器失效）。

#### 🔴 上线时当场撞出 `testkit::production_sources` 的一个真缺陷

它只剥**文件内**的 `#[cfg(test)] mod X { .. }` 块，**不认**「声明在父模块、实现在另一个文件」的
`#[cfg(test)] mod NAME;`。于是 `runtime/golden.rs` / `runtime/simulation.rs` 这类**整文件测试夹具**
一直被当生产码扫——它们里面全是 `INSERT INTO cloud_characters` 之类的播种语句。

这不是只影响新红线：**先前四条红线都在用这个扫描器**。补全之后有一条读数当场变了——
§3.10 那个精确棘轮从 8 降到 7，掉的是 `runtime/golden.rs` 的一条回归快照查询。
不影响那条红线的结论（那条查询本来就带 `moderation`），但它说明**棘轮的数字也会因为扫描口径
变化而变**，改动时要先弄清是口径变了还是代码变了。这一段已写进那条红线的注释里。

⚠️ 顺带记一次我自己的错法：第一次查这件事我用的是 `grep -v tests`（按**路径**过滤），
`assembly/mod.rs` 里一个叫 `seed_subplot_card` 的测试播种函数当场被误报成第二个铸卡点
——本仓的内联测试模块不止叫 `tests`。这个坑本 session 已经踩过两次，两次都是同一个原因。

### 3.10 §15 第 2 层「读取面过滤」是手工维护的 N 处判定（**钉住于 2026-07-28**）

台账里「`events`/`reports`/`clips`/`arena` 全部读取面过滤」这句，2026-07-28 逐条核过
——**是准确的**，没有发现漏网的读取面（把 `world_events` 的每一条 SELECT 都过了一遍：
返回叙事投影的六处全部带 `moderation` 条件；其余是计数 / 取坐标 / 取 `actors_json`，不交付内容）。

但「核过一次」不等于「以后也对」：这道过滤是**在每个读取面手工写一遍 SQL 条件**维护的，
而下一个读取面漏写这一条，违规内容就直接可见、且没有任何征兆。这正是本文件反复登记的形状
（同一判定的 N 份手工拷贝），所以核完之后把它钉住：

`safety::tests::red_line_narrative_projections_never_leave_the_db_unfiltered` —— 扫全部生产源码，
任何从 `world_events` 取 `public_projection_json` / `private_projections_json` / `arbiter_note`
的查询，SQL 文本必须含 `moderation`。豁免只有 `admin_api/audit.rs`（人审工作台的职责就是看未过审内容）。

🔵 故障注入：clips 高光选取去掉过滤 → 红；把 clips 加进豁免名单（给红线开后门）→ 红。
扫描走 `testkit::production_sources()`（花括号配平剥测试模块），读的是**源码文件**，
故 feature 门控的 `arena` / `clips` 在 default 那遍也照样被扫到（注入实测确认）。

⚠️ 条数用**精确棘轮**（8）而不是宽松下限，理由与 `flags::KNOWN_FLAGS.len()` 同：
实测让扫描器漏掉一个目录，条数 8 → 6，宽松下限照样绿、精确值当场红。
**但它不是万能的**：同样是扫描器退化，退回「按 `\nmod tests {` 截断」实测**仍是 8 条、仍绿**
——那 8 条恰好都不在内联测试模块之后。本棘轮挡的是「条数变了」，不是「扫描器一定还健康」。

### 3.9 创作者模板的机审覆盖面：包含表 → 排除表（**修于 2026-07-27**）

§3.8 查骨架手读键时顺出来的，但性质不同、也更重：这不是「功能静默退化」，是**内容绕过机审**。

`assets::worlds::world_scan_text` 是创作者发布模板（`POST /assets/worlds`）**唯一**的机审入口
（`safety::moderate_and_queue` 的输入），也是运营再审（`takedown::recheck`）送的同一段文本。
它此前是**包含表**——逐个列出「要扫哪个字段」：

```
sourceWork.title · worldCharacters[].card · locations[].name
worldItems[].narrative · hiddenContentPool[].template · sideHookPool[].template · storylines[].summary
```

于是**漏掉一个字段 = 那个字段默认不过审**，且没有任何征兆。实测漏了这些，
每一条都是创作者可写的自由文本，且直达模型或玩家：

| 漏掉的字段 | 去哪（已逐条核到落点） |
|---|---|
| `mainlineNodes[].summary` | `runtime` → `OutlineNode.summary` → **导演 prompt**（`narrative/mod.rs` 把 outline 序列化进提示词） |
| `identityPool[].label` | `brief_with_identity` → `唐三（户部主事）` → 感知层 brief → **模型** |
| `realmTier.briefing` / `flavorNotes[]` | `parse_realm_costume` → `RoundInput.realm_costume` → **导演 prompt** |
| 内联奖励道具的 `narrative`（`hiddenContentPool[].rewardItem` / `payoutTable…worldlineTiers[].item`） | 玩家背包。目录里的 `worldItems[].narrative` 扫了，**内联的这份没扫**——与「内联奖励是绕过品阶封顶的后门」是同一条内联路径 |
| `forbiddenPredicates[].reason` / `payoutTable…worldlineTiers[].label` | 玩家可见文案 |

**改法是把方向反过来**：默认扫一切字符串叶子，只排除标识符 / 枚举 / 受限 DSL
（`NON_NARRATIVE_LEAVES`）。两个方向的失败代价不对称——漏扫 = 内容绕过机审，
多扫 = 送审文本长几个 id。NPC 卡仍走 `card_scan_text` 的语义口径（与角色卡送审逐字一致）。

🔵 故障注入用的是**改动前的真实实现**（`git show HEAD:…` 取回后原样替换）：
`主线摘要探针` 确实不在送审文本里。这条是实测缺陷，不是推断。

**同一形状的第二处：NPC 卡的装配期机审**（补于 2026-07-28）。
`assembly::npc_scan_text` 也是包含表——只拼四个字段（名字 / 核心矛盾 / 表层目标 / 长期议程），
而 `CharacterCardV2` 有约 50 个创作者可写的叙事文本字段（`plotSeeds` / `bottomLines` / `stakes` /
`hiddenNeed` / `outburstPattern` / `forbiddenPhrases`……），**整张卡都会进模型**
（`WorldCharacterEntry.card` → runtime 逐 tick 注入）。于是那道写着「未复核内容不进实例」的门
漏看了其中约 46 个字段，且原注释只描述了选哪四个、没给过理由。

🔴 更要紧的是：**同一张卡本来就有两份送审实现**——模板发布走 `world_scan_text` → `card_scan_text`
（全量），装配期走 `npc_scan_text`（四字段），而窄的那份恰好在安全路径上。现收归一处
（`npc_scan_text` 直接复用 `card_scan_text`），并加了一条用例钉住「两者逐字相等」。

🔴 序列化失败返回 `None` 而不是空串，调用方按**未通过**处理：空串会被 provider 判过 ⇒ fail-open，
等于让一张读不出来的卡直接进实例。

🔵 故障注入用的是**改动前的真实实现**（原样贴回）：`隐藏需求探针` 确实不在送审文本里。

⚠️ **与 `MUSE_MODERATION_HTTP_MAX_CHARS` 的交互**：送审文本变长，更容易撞上客户端截断。
该项**默认 0 = 不截断**（注释已写明理由：让厂商侧的长度拒绝变成一次 `Err` → fail-closed，
比悄悄送半截文本过审安全），故默认配置下本改动的方向是安全的。但若运营把它调成正数，
截断掉的是**字典序靠后**的那截（`serde_json::Map` 有序）——恰好包含 `worldCharacters`（NPC 卡）。
provider 那里已有 warn（「尾部未经审核」），但 warn 不是拦截。**配这个值前请知悉这一点。**

⚠️ **运营影响（须知悉）**：送审文本变宽了，因此**存量模板在下一次运营再审时可能不再过**——
那是正确方向（它们本就是在欠扫的口径下过的），但会表现为「上次过了这次没过」。
`takedown::recheck` 的「两次机审须看同一份内容」这条约束仍成立，只是基准从此刻起换了一版。

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
