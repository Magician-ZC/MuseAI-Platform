# MuseAI 角色资产与自主叙事引擎 — P0/P1/P2 产品与开发文档

> 文档版本：v1.2
> 编写日期：2026-07-20（v1.2 修订：2026-07-23）
> 文档状态：待评审（产品篇供需求评审，开发篇供技术评审）
> v1.2 变更：统一阶段编号；将 G0 前的工作限定为 S0 验证脚手架；补齐金标评测、版本化存储、原子状态提交、模型路由前置、隐私与权利边界；修正“完全离线”、证据重复存储和 P2 直接产出云端 `WorldEvent` 等不可交付表述
> 前置文档：[`character-asset-and-autonomous-narrative-product-proposal.md`](./character-asset-and-autonomous-narrative-product-proposal.md)（下称「建议稿」）
> 本文档职责：定义本地轨道的产品与技术事实源。**只有 S0/G0 可立即启动**；P0–P2 是否立项由阶段门决定，不把待验证假设写成已承诺路线图

---

# 第一部分 · 产品文档

## 1. 背景与定位

### 1.1 原始需求

1. 从任意一本完整的书中扫描并索引**所有可识别角色**；对有足够跨章节证据的角色生成分层角色卡，角色具备强特点，放入其他故事中依然成立。
2. 为角色卡配置**额外知识储备**（名著、传记等），使其对话与思考向指定思维模式靠拢，但不失去原有人格。
3. 上传完整**故事大纲**，投入跨作品角色阵容，由角色推动剧情演化，生成独一无二的小说。

### 1.2 核心重定义：从「辨识度」到「行为不可替换性」

早期讨论曾把「读者能否一眼认出角色来自哪部原作」作为北极星。本文档采纳建议稿的纠偏，**放弃该定位**，理由有三：

1. **优化方向错误**：以「猜出原作」为目标，模型会滑向堆砌原作名词、口癖和标签的表面模仿，而非行为内核的复刻。口癖鲜明但选择趋同，恰恰是角色扮演产品最常见的失败形态。
2. **指标不可推广**：该指标只对「读者恰好读过原作」的角色有效，对原创角色、冷门作品角色完全失效，无法成为产品级验收标准。
3. **风险方向错误**：表达层的逐字模仿是版权风险最集中的区域，把它设为北极星等于把产品往风险最大的方向优化。

**新北极星**：

> 角色的不同，不体现在名字、口癖或原作标签上，而体现在——面对同一局势时，他们会做出彼此**不可替换的选择**，并把故事推向不同的未来。

原「辨识度盲测」降级保留：仅作为**导入角色的附加测试**（对熟悉原作的用户提供参考），不进入核心指标。

### 1.3 与建议稿的能力映射

用户视角保持三期节奏，与建议稿能力模块的对应关系如下：

| 本文档 | 内容 | 对应建议稿能力 |
|---|---|---|
| **P0 角色资产基座** | Character DNA V2 数据模型 + 兼容迁移 + 全书分布式角色提取 | 角色行为模型 + 长篇角色提取 |
| **P1 知识与思维系统** | 知识包 / 思维包 / 价值包 / 表达包 + 本地检索 | 外部知识与思维资源 |
| **P2 自主叙事引擎** | 大纲约束 + 角色独立决策 + 行动仲裁 + 章节草稿模式 | 自主叙事 + 可审阅章节生产 |

建议稿中的「长篇连续性自动审校与全书生产线」统一称为 **L3 长篇连续性**，**不在本三期承诺范围**，只在 P2 的用户留存、稿件保留率和单位章节成本成立后独立立项。

P0 内部拆分为两个可独立验收的里程碑：**P0.a**（数据模型与迁移）、**P0.b**（全书提取管线），P0.a 不依赖 P0.b，先行交付。

### 1.4 产品定位与目标用户

> MuseAI 是一个可以从作品中提取角色行为模型、为角色配置知识与思维资源，并让跨作品角色在大纲世界中自主推动剧情的**本地小说实验室**。

**首选 ICP**（沿用建议稿）：重视人物塑造的网文 / 长篇 / 互动小说创作者——已有大纲、不满意角色沦为剧情工具人、想实验「不同人格组合会碰撞出什么故事」的人。

P0–P2 以“已有大纲的长篇创作者”为唯一主 ICP，G0 招募、任务设计和付费验证均围绕此人群；互动玩家只作为兼容用户，不同时决定三种运行模式的优先级。

**次级用户**：同人与跨作品角色扮演用户、剧本杀 / 跑团设计者。

**商业模式边界**：P0–P2 先验证创作者是否愿意为角色质量、可控性和长篇效率付费。平台轨道是条件性产品选项，不是本地轨道的默认下一期；其开发前还须满足平台文档中的用户行为、运营与合规门。角色卡交易市场和可兑现资产继续冻结。

## 2. 北极星与指标体系

### 2.1 角色质量指标（P0 起生效）

| 指标 | 定义 | 验证方式 | 目标 |
|---|---|---|---|
| **角色互换排斥率**（北极星） | 把甲的行动/台词换给乙后，盲评认为明显不适配的比例 | 内建互换测试 + 人工盲评 | ≥ 80% |
| 决策一致性 | 不同压力场景下保持可解释的价值与策略偏好 | 内建压力测试 | 人工抽查通过 |
| 阵容区分度 | 多角色长跑后是否收敛成同一人格 | 决策特征相似度诊断 | 系统主动告警 |
| 主动剧情贡献 | 角色主动引发且改变后续剧情的事件数 | 行动因果链统计 | 每主要角色 5 场景 ≥ 1 |
| 不可替代性 | 删除该角色后故事是否发生结构性变化 | 删角色重新模拟对比 | 明显差异 |

### 2.2 提取质量指标（P0.b）

章节扫描覆盖率 100%；核心角色召回率 ≥ 95%；别名错误合并率 ≤ 2%；关键字段证据覆盖率 ≥ 80%；无证据事实幻觉率 ≤ 5%；任务失败可恢复、不整本重跑。质量指标在不少于 3 部结构不同、已建立人工金标的作品上分别计算，同时报告分子、分母和置信区间，不以“模型自己生成的清单”充当真值。

### 2.3 知识包指标（P1）

检索片段与场景相关率；使用知识时事实准确率；时间边界越界次数（目标 0）；同一知识包挂载不同角色后行为仍分化（人格不融合）；每轮使用来源 100% 可追踪。

### 2.4 故事与行为指标（P2）

引擎确定性不变量要求 100% 通过（未授权私密字段不得进入其他角色上下文、非法状态补丁不得提交、锁定章节不得改写）；角色秘密在正文中是否被“合理揭露”另按叙事抽样评审。硬节点不是强迫模型伪装完成：无法同时满足时必须进入 `blocked` 并请求用户裁决。另观察关键事件因果完整度、生成段落保留率、平均修改幅度、第二组阵容创建率和 7 日续写率。

### 2.5 统一评测协议

- 固定评测集：至少 3 部公版、原创或已授权作品，覆盖第一/第三人称、多别名/少别名、线性/多线叙事；每部由两名标注者独立建立角色清单与证据金标，分歧经复核解决。
- 固定对照：V1 描述卡、V2 决策卡、同卡复制和删角色重跑；盲评时隐藏产品版本与角色来源。
- 固定记录：模型与提示词版本、随机种子（如适用）、输入/输出 Token、p50/p95 延迟、重试率、解析失败率、人工编辑时长。
- 指标分层：安全与状态隔离属于发布阻断门；质量指标属于样本统计；产品指标属于用户行为。三者不得互相替代。
- 阈值解释：80% 等目标是 G0 的初始 go/no-go 线，不是长期行业基准；样本不足时必须标注“不确定”，不得四舍五入为通过。

## 3. P0 · 角色资产基座

### 3.1 用户故事

- 作为创作者，我上传一本 50 万字的小说，系统扫描**全部章节**后给我一份分层的角色清单（核心/重要/功能/过场），我确认清单后批量生成角色卡。
- 作为创作者，我打开一张生成的角色卡，能看到每个关键设定**来自原文哪一章哪句话**，能区分「原文事实」和「模型推断」，并对低置信项逐条确认或修正。
- 作为创作者，我选两张角色卡运行「互换测试」，系统告诉我这两个角色在同一局势下的选择是否真的不同。
- 作为老用户，我升级后旧角色卡完好无损，可以一键升级为 V2 并逐步补全新字段。

### 3.2 范围分层

**必须做**：

- Character DNA V2 数据模型（十层结构，见开发篇 §9.1）与 `schemaVersion: 2`
- V1 → V2 无损迁移器；角色模板 / 故事运行状态 / 用户关系记忆三分离
- 全书提取管线：TXT / Markdown 输入，章节切分 → 角色发现 → 别名归并 → 证据账本 → 重要度分层 → DNA 合成 → 覆盖报告 → 人工确认
- 断点续跑：任务可暂停 / 恢复 / 单角色重试，不整本重跑
- 证据与置信度：关键字段挂原文位置、证据类型、置信度、矛盾标记、用户确认位
- 角色测试：互换测试 + 压力测试（内建，结果可对比）
- 导入导出兼容：MuseAI 包格式 + SillyTavern 导出（V2 高级字段入扩展区）

**暂不做（本期）**：EPUB 解析（P1 窗口评估）；角色卡协同编辑；提取过程的移动端操作（移动端只读查看结果）。

**后续版本**：跨作品角色关系网络图谱；角色卡版本 diff 视图。

**明确不做**：PDF / 扫描件 OCR；为「猜出原作」优化的字段；给每个过场人物生成完整卡（只入索引）；一次模型调用返回全部角色。

### 3.3 关键产品定义

**「所有角色」的定义**（采纳建议稿四级分层）：

| 层级 | 定义 | 输出 |
|---|---|---|
| 核心角色 | 持续推动主线并发生明显变化 | 完整 Character DNA |
| 重要配角 | 多次参与冲突或影响主要关系 | 较完整 Character DNA |
| 功能角色 | 有明确事件功能但证据有限 | 精简行为卡 |
| 过场人物 | 单次出现 / 群体 / 无稳定身份 | 仅索引记录，默认不入角色库 |

**「任意一本书」的定义**：指内容结构不限定题材、叙事人称或篇幅，不等于首版支持所有文件格式与受保护来源。P0.b 首版只接受可读取的 TXT / Markdown；DRM、EPUB、PDF、扫描件和 OCR 按范围说明处理。系统必须展示未扫描章节、解析失败和低置信区域，不能在覆盖报告不完整时宣称“已提取全书所有角色”。

**导入向导八阶段**：文件检查 → 章节扫描 → 角色发现 → 别名合并（用户确认）→ 重要度分层（用户勾选入库范围）→ DNA 生成（并发，可单个重试）→ 低置信确认 → 入库。每阶段可暂停退出，进度落盘。

### 3.4 验收标准

1. 旧 V1 角色卡在新版本中无损打开、可继续用于现有聊天 / 冒险 / 穿书流程。
2. 不少于 3 部结构不同的金标试点作品（其中至少 1 部 ≥ 30 万字）：全部章节被扫描，各作品分别报告核心角色召回与别名错误合并，汇总门槛为召回 ≥ 95%、错误合并 ≤ 2%。
3. 任一关键人格字段可回溯到原文证据或明确标注为推断；证据覆盖率 ≥ 80%。
4. 提取任务在第 40 章中断后，恢复执行不重复处理前 39 章。
5. 互换测试对「同书两主角」输出可读的差异报告；对「同一角色复制两份」正确报告无差异。
6. 生成的 V2 卡可导出 SillyTavern 格式并在其中正常加载（高级字段静默降级）。

### 3.5 前置门（G0）：两周核心假设验证

在任何生产数据迁移或 P0.b 全量开发前，先用**两周**验证建议稿 §10 的单一核心假设。为完成验证，允许先建 **S0 验证脚手架**：临时 V2 schema、最小决策循环、盲评与埋点；这些代码默认不进入生产存储，也不承诺兼容。

> 当角色拥有结构化的决策内核、关系规则和行动力，并在同一场景中分别决策时，用户会认为这些角色明显不同，且角色组合能产生大纲中没有预写的有效剧情。

做法：选 3 部公版作品，人工辅助制作 10–12 张 V2 卡，在现有 `role_play` 机制上搭最小决策原型，生成 5–10 个连续场景，邀请 10–20 名创作者测试。

**通过标准**（不达标则不进入 P0.a 产品化、P0.b 或 P2 完整开发）：互换排斥率 ≥ 80%；每主要角色 5 场景内主动引发 ≥ 1 个影响后续的事件；70% 测试者 24 小时后能复述至少两个角色的关键矛盾；≥ 5 人主动创建第二组阵容；≥ 3 人接受明确价格区间下的预约、预购或等价强意向行为。单纯口头“愿意付费”只作为弱信号。若差异只停留在口癖和语气、剧情选择基本一致，判定为未通过。

## 4. P1 · 知识与思维系统

### 4.1 用户故事

- 我上传自己有权使用的传记、访谈或笔记，系统提炼出一份可编辑的「思维包」（问题拆解、证据偏好、反例习惯），我把它挂给某个角色并设置影响强度；角色借用分析方法，但不被产品描述为“复制某个现实人物”。
- 我上传一本战争史，作为「知识包」挂给军师角色；对话中涉及战术时系统检索相关片段注入，且我能看到本轮实际引用了哪几段原文。
- 我一键停用某个知识包，对比启用前后同一场景的角色表现差异。

### 4.2 范围分层

**必须做**：

- 四类资产（采纳建议稿）：Knowledge Pack（知道什么）/ Mind Pack（如何分析）/ Value Pack（价值尺度）/ Expression Pack（语言组织）；同一资料可蒸馏出多个包，分别启用
- 包配置：来源与版权状态、生效模式、影响强度、绑定角色与故事、时间边界、冲突优先级、可引用范围
- 本地检索：切块 → 索引 → 关键词 + 轻量向量混合检索 → 重排 → 少量高相关片段注入 → 来源与使用记录
- 上下文优先级硬序（高到低）：产品硬约束 → 世界规则与大纲禁止项 → 角色不可变内核与底线 → 角色私有状态 → 本轮检索片段 → 表达与文风要求
- 诊断：知识串线检测（片段进错角色上下文）、人格覆盖告警（启用后决策偏移过大）、时间边界越界检测
- 数据与权利：导入前选择权利基础与允许用途；展示是否会发送给远程模型；支持仅索引不保留源文件副本、删除源与索引、内容哈希变化后失效重建

**暂不做（本期）**：跨包联合推理；知识包分享 / 导入市场。

**明确不做**：按书 / 按角色微调模型；把整本资料全文注入上下文；知识包让角色知晓故事世界内未发生的事件。

### 4.3 验收标准

1. 任一轮角色输出可展开查看「本轮使用了哪些知识片段、来自哪个包哪个位置」。
2. 同一思维包挂给两个不同角色，两者在同一场景下选择仍明显分化。
3. 停用知识包后重新生成，可并排对比差异。
4. 多角色并行时，A 角色私有知识包片段 0 次进入 B 角色上下文（自动化测试覆盖）。
5. 时间边界测试：知识包含有晚于角色时代的内容时，角色不引用（抽样验证）。
6. 删除知识源后，其切块、嵌入、缓存和使用入口均不可再访问；源文件内容变化后旧索引自动失效。

## 5. P2 · 自主叙事引擎

### 5.1 用户故事

- 我上传一份 8 个节点的大纲，把其中 3 个标为硬节点、2 个软节点、其余自由区域，再设定 1 条禁止结果；投入 4 个跨作品角色后进入观察模式，看角色们自己把故事走出来，随时暂停干预。
- 章节草稿模式下，系统跑完一章暂停等我审阅；我改掉一个角色的某次选择后从该场景分支重跑。
- 每章结束我能看到：角色状态变化、埋下的伏笔、对大纲的偏离度、本章 Token 花费。
- 我删掉阵容里的某个角色重跑同一章，剧情走向明显不同——这让我确信角色真的在推动故事。

### 5.2 范围分层

**必须做**：

- 大纲约束：节点只分硬 / 软 / 自由；“禁止结果”是独立的状态谓词，不与节点混为同一枚举。角色自主度拆解为剧情偏离、角色拒绝、秘密行动、不可逆后果四种权限
- 回合结构：导演设局 → 活跃角色**并发独立决策** → 行动仲裁 → 场景写作 → 一致性与信息边界审校 → 状态提交
- 角色决策协议：结构化输出（意图 / 行动候选 / 是否发言 / 影响对象 / 愿付代价 / 对他人行为预测）；角色 Agent **不得直接修改状态**。只有仲裁结果经 reducer 生成并校验 `StatePatch` 后才能原子提交
- 仲裁器边界（采纳建议稿）：只裁决可行性、冲突、信息可见性、结果与硬约束，**不重写角色意图**
- 状态五层：世界公共 / 角色私有 / 关系 / 叙事 / 创作；角色私有状态是信息差与戏剧性的来源，秘密不因同场而共享
- 三种运行模式：互动模式（延续现有穿书体验）/ 观察模式 / 章节草稿模式（每章暂停确认）
- 成本控制：每场景活跃角色 2–5 上限、非活跃角色状态摘要代理、决策与写作分级用模型、运行前展示预估调用次数与 Token 区间、单章预算上限
- 场景快照、分支与回滚

**暂不做（本期）**：无人值守连续生成整本小说；移动端发起叙事运行（移动端提供观察模式只读回放）。

**明确不做**：无限角色同场独立决策；「出版级成稿」承诺；导演代替角色写决定。

### 5.3 验收标准

1. 对可满足的确定性测试，硬节点完成率 100%、禁止谓词提交率 0；约束互相冲突或不可满足时进入 `blocked`，展示冲突规则并等待用户选择，不允许伪造完成。
2. 角色秘密在未被剧情合理揭露前，0 次出现在其他角色的决策输入中（自动化测试）。
3. 删除阵容中任一主要角色重跑同章，剧情结构可观察地不同。
4. 章节草稿模式：锁定的已确认章节不被后续生成改写；从任一场景快照可分支重跑。
5. 每次运行前显示成本预估，实际消耗与预估偏差在可解释范围内。
6. 阵容区分度诊断能在角色趋同时主动告警（用「同卡复制多份」做阳性对照）。

## 6. 全局非目标

- P0–P2 不实现角色市场、卡片交易、养成资产流通、账号、云端协作或平台经济；平台候选方向另文评审。
- 不做按书 / 按角色微调；不做每次生成的强化学习闭环。
- 不以「读者猜出原作」为优化目标；不逐字模仿原作者文风。
- 不承诺无人工编辑的出版级长篇。
- P0–P2 不依赖 MuseAI 自建云账号或服务器，资产与状态默认本地保存；但当前生成能力依赖用户配置的远程 API 或本地模型。**Local-first 不等于完全离线**：使用远程模型时，明确选中的文本会离开设备；只有配置本地模型且所有索引本地完成时才可称离线运行。

## 7. 风险与应对（精选）

| 风险 | 应对 |
|---|---|
| 角色仍趋同为「通用 AI 人格」 | 强制价值排序 / 牺牲顺序 / 失败模式字段；互换与压力测试进验收门；阵容相似度诊断；重要场景强制有代价的选择 |
| 自由发挥导致大纲失控 | 四级约束 + 仲裁器只管边界；章节草稿模式每章确认；快照分支回滚 |
| 调用成本过高 | 活跃角色上限；分级模型；角色内核与检索结果缓存；预算预估与单章上限 |
| 长篇提取错误累积 | 先确认角色清单与归并结果再生成完整卡；证据账本可回滚；低置信合并必须人工确认 |
| 知识包覆盖人格 | 四类分离启用；不可变内核优先级高于外部资料；影响强度可调；启停对照测试 |
| 版权与合规 | 试点用公版 / 原创 / 已授权作品；导入记录来源与权利状态；明示「本地保存 ≠ 不发送给模型服务商」；生成内容保留标识与删除能力；对外发布前专项法律评估。依据：《著作权法》、《生成式人工智能服务管理暂行办法》、《人工智能生成合成内容标识办法》、《人工智能拟人化互动服务管理暂行办法》（详见建议稿 §13.6 链接） |

---

# 第二部分 · 开发文档

## 8. 架构基座与工程约定

### 8.1 可复用能力盘点（已核验）

| 现有能力 | 代码落点 | 在本项目中的用途 |
|---|---|---|
| 角色卡存储 / 导入导出 / SillyTavern 转换 | `src/stores/usePartnerStore.ts`、`convert_character_card_to_silly_tavern` | V2 在其上扩展，不另起炉灶 |
| 背景提取（两阶段：角色名 → 并发单卡） | `generate_background_stage_one` / `generate_background_character_card`（`sessions.rs`） | P0.b 管线的雏形；其 10 万字符入口限制（`sessions.rs:1081`）由新管线取代 |
| 长文分段处理 | `split_content_by_char_limit`（`sessions.rs:2415`，反向大纲按 5000 字切块） | 章节切分的基础工具 |
| 后台任务：启动 / 取消 / 重试收尾 | `start_reverse_outline_analysis` / `cancel_background_task` / `retry_and_finalize_reverse_outline` | P0.b 任务模型直接沿用此模式，补「断点恢复」 |
| 角色独立发言 | `role_play` 工具（`agent/mod.rs:952`，`role_play_context` + `resolve_role_play_character`） | P2 `role_decide` 的实现基础 |
| 多阶段叙事管线 | `book_travel.rs`（装配 / 入场导演 / 场景规划 / 场景写作 / 记忆整理 / 结局判断） | P2 在「场景规划」与「场景写作」之间插入决策与仲裁环节 |
| 长线状态 | `useBookTravelStore.ts`（场景 / 节拍 / 稳定与临时记忆 / 偏离度） | 扩展为五层状态模型 |
| 每模块独立采样参数 | `useSettingsStore.ts`（`agentConfigs` + prompt 全集） | 可复用 temperature/token 等参数；**当前 `AgentConfig` 没有 `modelId`，不能据此宣称已支持按环节分级模型** |
| 双端调用与流式 | `appInvoke` / `listenStream`（`runtime.ts`）、`mobile_server.rs` | 移动端只读能力按三处改动规则接入 |
| 并发控制 | `SettingsConcurrencyCard` + 现有批量提取并发 | DNA 批量合成复用 |

### 8.2 工程约定（沿用现有模式，新代码必须遵守）

1. **双端三处改动规则**：任何需要移动端的能力 = Rust command（`lib.rs` 注册）+ `mobile_server.rs` axum 路由 + `runtime.ts` `appInvoke` 分支。P0/P1 提取与知识管理为桌面 only，仅结果查看走移动端。
2. **严格 JSON 输出**：所有提取 / 决策 / 仲裁 Agent 一律要求严格 JSON，Rust 侧解析失败自动重试（沿用现有提取重试模式）；抽取类 Agent 默认 `temperature: 0`（README FAQ 中提取失败的既有教训）。
3. **UI 文案与错误信息**：简体中文。
4. **大文件治理**：`sessions.rs` 已 4551 行、`Background.tsx` 已 2782 行，本项目新增逻辑一律入新模块（§13），不再向这两个文件堆代码；`sessions.rs` 中仅保留薄的 command 壳。
5. **Prompt 注册表重构需单独评审**：它是横切迁移，不是 P0.a 的“顺手工作”。先用旧模式接入 S0；只有在给出迁移、回滚、设置页回归测试与独立估时后，才并入 P0.a。
6. **宿主无关**：`character_engine`、`knowledge`、`narrative` 不直接依赖 `AppHandle`。文件、事件、时钟、模型调用通过 trait 注入，便于测试并为未来抽 crate 留边界。
7. **版本与原子性**：持久化对象包含 `schemaVersion`、`revision`、`createdAt`、`updatedAt`；写入使用临时文件 + 原子替换并保留最近一份可恢复备份。Zustand store 必须配置 `version/migrate`，不能只靠 TypeScript 类型升级。
8. **模型输出不可信**：JSON schema 校验只是第一层；还要做字段白名单、长度/枚举/引用完整性校验、prompt 注入隔离和业务不变量校验。自动重试不得重复扣费或重复提交状态。

## 9. 数据模型

### 9.1 Character DNA V2（TypeScript 定义，落点 `src/utils/characterCardV2.ts`）

```typescript
export interface EvidenceRef {
  id: string;
  sourceId: string;
  chapterIndex: number;              // 全书章节位置
  locator: { start: number; end: number; heading?: string };
  quotePreview: string;              // UI 预览，≤200 字；非完整原文副本
  kind: 'description' | 'action' | 'otherView' | 'inference';
  confidence: 'high' | 'medium' | 'low';
  userConfirmed?: boolean;
  conflictsWith?: string[];          // 互相矛盾的证据 id
}

export interface DecisionRule {
  when: string;                      // 当……时
  then: string;                      // 通常会……
  because: string;                   // 因为……
  evidenceIds?: string[];
}

export interface CharacterCardV2 {
  schemaVersion: 2;
  id: string;
  lifecycle: 'draft' | 'reviewed' | 'ready';
  // A 基础身份层（V1 字段迁移目的地；含别名与指代）
  identity: {
    name: string;
    aliases: string[];
    narrativeRole?: string;          // 主角/对手/盟友/导师/催化者
    importance: 'core' | 'major' | 'functional';
    sourceWork?: { sourceId: string; title: string; version?: string };
    legacyV1Fields?: Record<string, unknown>;  // V1 原样保留区，禁止类型收窄丢数据
  };
  // B 戏剧内核层
  dramaticCore: {
    coreContradiction: string;
    surfaceGoal: string;
    hiddenNeed: string;
    deniedDesire?: string;
    coreFear: string;
    stakes: string;
    bottomLines: string[];
    selfDeception?: string;
  };
  // C 决策模型层
  decisionModel: {
    valuePriorities: string[];       // 冲突时从高到低
    riskAppetite: string;
    defaultStrategies: string[];     // 谈判/试探/欺骗/对抗/退让/牺牲/拖延
    escalationPath: string[];        // 克制 → 失控的阶段
    sacrificeOrder: string[];        // 资源/名誉/关系/身体/信念
    knownBiases: string[];
    decisionRules: DecisionRule[];
  };
  // D 感知与认知层
  perception: {
    firstNotices: string[];
    blindSpots: string[];
    attributionStyle: string;        // 判断他人动机的默认归因
    trustOrder: string[];            // 证据/权威/直觉/经验/情感
  };
  // E 情绪动力层
  emotionDynamics: {
    triggers: string[];
    maskingStyle: string;
    outburstPattern: string;
    recoveryConditions: string;
    pressureShift?: string;          // 长期压力下的性格变形
  };
  // F 关系语法层
  relationGrammar: {
    trustBuilding: string;
    trustRepair: string;
    modesByRelation: Record<string, string>;  // 盟友/爱人/权威/陌生人/敌人…
    attractedBy: string[];
    provokedBy: string[];
  };
  // G 表达与行为指纹层（只管「怎样表现」，不替代决策内核）
  expressionFingerprint: {
    sentenceRhythm: string;
    metaphorSources: string[];
    questioningStyle?: string;
    lyingStyle?: string;
    humorStyle?: string;
    sayVsThinkGap: string;           // 口头表达与内心真实的距离
    signatureGestures: string[];
    stateVariants?: Record<string, string>;   // 平静/危险/羞耻/愤怒下的表达差异
    forbiddenPhrases: string[];      // 禁用的通用 AI 式表达
  };
  // H 行动力与剧情种子层（自主推动剧情的关键）
  agency: {
    initiativeTriggers: string[];
    defaultPlans: string[];
    longTermAgenda: string;
    leverage: string[];              // 影响他人与局势的筹码
    plotSeeds: string[];             // 天然携带的冲突/秘密/承诺/未完成事项
    refusalRules: string[];          // 会拒绝哪些剧情安排
  };
  // I 成长弧层（模板侧只存弧线定义，运行状态另存）
  growthArc: {
    immutableCore: string[];
    mutableBeliefs: string[];
    breakPoints: string[];
    awakeningPoints: string[];
  };
  // J 跨世界适配层
  worldAdaptation: {
    identityMapping?: string;
    capabilityMapping?: string;
    mustPreserve: string[];
    localizable: string[];
    conflictFallback?: string;       // 与目标世界规则冲突时的降级策略
  };
  evidenceIndex: { storeKey: string; contentHash: string; count: number };
                                           // 证据全量外置，各层仅以 evidenceIds 引用
  revision: number;
  createdAt: number;
  updatedAt: number;
}
```

迁移产生的卡一律为 `draft`。`ready` 校验要求关键行为字段非空、所有 evidenceIds 可解析、低置信冲突已处理；不能用空字符串或模型补全把旧卡伪装成完整 V2。UI 对缺失字段显示“待补充”，并允许 V1 路径继续工作。

### 9.2 三分离存储

| 数据 | 内容 | 存储位置 |
|---|---|---|
| 角色模板 | CharacterCardV2 轻量本体 | `config/partner-store.json`（与 V1 共存，`schemaVersion` 区分） |
| 证据账本 | EvidenceRef 全量（可能很大） | `character-engine/evidence/<characterId>.json` |
| 来源记录 | 内容哈希、权利基础、允许用途、数据发送与保留策略 | `character-engine/sources/<sourceId>.json` |
| 用户关系记忆 | 现有羁绊 / 归档数据 | 现有位置不动 |
| 故事运行状态 | 五层状态（§9.4） | 随会话存 `agent-sessions/`，不写回模板 |

原则：**一次故事经历永远不污染原始角色资产**；用户显式「基于本次经历创建模板新版本」除外，不提供静默覆盖。模板、证据、运行状态均以版本和内容哈希相互引用，删除或迁移时执行引用完整性检查。

### 9.3 提取任务模型（P0.b，落点 `character-engine/extraction-tasks/<taskId>.json`）

```typescript
export interface ExtractionTask {
  schemaVersion: 1;
  taskId: string;
  workTitle: string;
  sourcePath: string;
  sourceFingerprint: { size: number; modifiedAt: number; contentHash: string };
  pipelineVersion: string;
  chapters: Array<{
    id: string;
    index: number;
    title: string;
    charRange: [number, number];
    status: 'pending' | 'running' | 'scanned' | 'failed' | 'cancelled';
    attempt: number;
    discoveryStoreKey?: string;       // 大结果分片存储，任务文件不无限增长
    error?: { code: string; message: string; retryable: boolean };
  }>;
  roster: Array<{                    // 归并后的角色清单
    key: string;                      // 首次确认后稳定，不以名称作为主键
    canonicalName: string;
    aliases: string[];
    tier: 'core' | 'major' | 'functional' | 'extra';
    mergedFrom: string[];
    userConfirmed: boolean;
    dnaStatus: 'pending' | 'generated' | 'failed' | 'skipped';
  }>;
  stage: 'preprocess' | 'scan' | 'merge' | 'tiering' | 'synthesis' | 'review' | 'done' | 'cancelled';
  revision: number;
  createdAt: number;
  updatedAt: number;
}
```

断点恢复不等于简单跳过非 `pending`：启动时先比对源文件哈希和管线版本；把上次崩溃遗留的 `running` 转为可重试；只有输出分片存在、哈希正确且 schema 可解析的 `scanned/generated` 才可跳过。任务状态按 revision 原子写入，取消、重试和重复事件必须幂等。

### 9.4 知识包与运行状态模型（P1 / P2）

```typescript
export interface KnowledgePack {
  schemaVersion: 1;
  id: string;
  title: string;
  source: {
    path: string; author?: string; contentHash: string;
    rightsBasis: 'owned' | 'licensed' | 'public_domain' | 'personal_use' | 'unknown';
    allowedUses: Array<'extract' | 'retrieve' | 'generate' | 'send_to_remote_model' | 'publish'>;
    userAttestedAt?: number;
    retention: 'reference_original' | 'managed_copy' | 'index_only';
  };
  mode: 'knowledge' | 'mind' | 'value' | 'expression';
  distilled: {
    principles: string[];
    decisionHeuristics?: Array<{ when: string; prefer: string; avoid?: string }>;
    evidenceStandards?: string[];
    expressionRules?: string[];
  };
  timeBoundary?: string;             // 角色可知的时代边界
  chunkIndexStoreKey: string;        // 内部受控 key，不接受任意路径
  indexVersion: string;
  revision: number;
}

export interface KnowledgeBinding {  // 绑定独立存储，避免共享包被某故事配置污染
  id: string;
  packId: string;
  characterId: string;
  storyId?: string;
  influence: number;
  enabled: boolean;
  conflictPolicy: 'character_core_wins' | 'ask_user';
}

export interface NarrativeState {
  schemaVersion: 1;
  runId: string;
  revision: number;                                  // compare-and-swap / 原子提交
  world: Record<string, unknown>;                    // 公共状态
  characters: Record<string, {                       // 角色私有状态
    goals: string[];
    emotions: Array<{ name: string; intensity: number; cause?: string }>;
    resources: string[];
    secrets: string[]; misconceptions: string[]; plans: string[];
    arcStage: string;
  }>;
  relations: Array<{                                  // 方向性状态：A 信任 B 不等于 B 信任 A
    from: string; to: string;
    trust: number; affinity: number; fear: number; debt: number;
    knownTo: string[]; notes: string[];
  }>;
  narrative: {                                       // 叙事状态（导演与系统）
    outlineNodes: Array<{
      id: string; summary: string;
      constraint: 'hard' | 'soft' | 'free';
      status: 'pending' | 'done' | 'bypassed' | 'blocked';
    }>;
    forbiddenPredicates: Array<{ id: string; expression: string; reason: string }>;
    foreshadowing: string[]; pacingNotes: string[];
  };
  authoring: { lockedSceneIds: string[]; branchSnapshotIds: string[] };
}

export interface StatePatch {
  id: string;
  baseRevision: number;
  sourceDecisionIds: string[];
  operations: Array<{
    op: 'set' | 'append' | 'remove' | 'increment';
    path: string;                     // 只允许 schema 白名单路径
    value?: unknown;
    precondition?: unknown;
  }>;
}
```

`role_decide`、写手和 continuity critic 都不能直接写 `NarrativeState`。仲裁器输出事实结果，确定性 reducer 生成 `StatePatch`，校验 baseRevision、路径白名单、禁止谓词、引用完整性后一次性提交；失败则整回合不提交并保留可诊断日志。

`rightsBasis` 是产品中的用户声明，不是平台替用户作出的法律结论。`unknown` 或仅标记 `personal_use` 的资料默认只能形成本地草稿，不开放云端发布；未勾选 `send_to_remote_model` 时，组装器必须阻止远程 API 路径并解释可选方案。

## 10. P0 技术方案

### 10.1 P0.a 数据模型与迁移

- 新增 `src/utils/characterCardV2.ts`（类型 + 迁移器 + 校验）。迁移规则：V1 全部字段原样存入 `identity.legacyV1Fields` 并按映射表填充可对应的 V2 字段（如 `speakingStyle` → `expressionFingerprint`、`typicalReactions` → `decisionModel.decisionRules` 的种子），其余层留空待补。
- `usePartnerStore.ts`：读入时按 `schemaVersion` 分流；V1 卡照常工作；「升级为 V2」为显式用户动作。
- 角色测试命令（Rust 薄壳 + prompt）：
  - `run_character_swap_test(card_a, card_b, scenario) -> SwapTestReport`
  - `run_character_stress_test(card, scenarios[]) -> StressTestReport`
- SillyTavern 导出：V2 高级层序列化入 `extensions.museai_dna` 扩展区。
- Prompt 注册表重构仅在独立技术评审通过后同期完成；否则沿用现有字段，避免把角色迁移与设置系统迁移绑成一次高风险发布。
- 迁移前自动备份 V1 store；迁移只生成 V2 新版本，不覆盖源卡；启动时校验 schema，失败回退到上一个可读快照并提示用户。

### 10.2 P0.b 全书提取管线

管线各阶段与模型调用（全部严格 JSON、temperature 0、失败重试一次后标记 failed 待手动重试）：

| 阶段 | 实现 | 模型调用 |
|---|---|---|
| 1 预处理 | 编码检测、正则 + 目录启发式切章，兜底按 8000 字硬切 | 无 |
| 2 章节扫描 | 逐章并发（沿用现有并发配置），产出 mentions | 每章 1 次 |
| 3 别名归并 | 先规则归并（完全同名 / 包含关系），剩余交模型判定；结果全部进用户确认页 | 1–3 次（按候选量分批） |
| 4 证据账本 | 从各章 mentions 聚合行为 / 选择 / 情绪 / 关系 / 表达样本 | 无（纯聚合） |
| 5 重要度分层 | 出场频次 + 事件参与度 + 关系中心性打分 → 模型复核边界情况 | ≤ 1 次 |
| 6 DNA 合成 | 每角色一次：输入该角色全部证据（超长则先做证据摘要分片） | 每角色 1–2 次 |
| 7 矛盾审查 | 合成时要求模型区分「成长变化 / 叙述者不可靠 / 真实矛盾」并标注 | 合成内完成 |
| 8 覆盖报告 | 已扫描章节、角色数、未决别名、低置信字段清单 | 无 |

新增 Rust commands（注册入 `lib.rs`，实现入新模块 `character_engine/`）：

```text
start_character_extraction(request)         -> { taskId }     // 后台任务，使用专用、可重连的 task event；不复用聊天 token 流
get_character_extraction_task(taskId)       -> ExtractionTask
confirm_character_roster(taskId, roster)    -> ExtractionTask // 归并确认 + 入库范围勾选
start_character_dna_synthesis(taskId, keys) -> { runId }      // 并发合成，可单角色重试
cancel_character_extraction(taskId)         -> bool           // 复用 cancel_background_task 机制
```

前端：新增向导页（新组件，不改造臃肿的 `Background.tsx`，入口挂在背景设定页）；新增 `src/stores/useExtractionStore.ts` 管理任务状态（`createDiskStorage` 持久化）。

任务事件至少包含 `{taskId, revision, stage, itemId?, progress, error?}`；前端断线重连后先以 `get_character_extraction_task` 拉取快照，再订阅增量，按 revision 去重。`sourcePath` 只是用户选择来源；恢复前必须校验 fingerprint，源内容变化时禁止沿用旧结果并提供“基于新版本复制任务”。

### 10.3 P0 测试清单

- V1 → V2 迁移无损（含 customFields、SillyTavern 往返）
- 模板与运行状态互不污染（写运行状态后模板文件哈希不变）
- 章节切分边界（无目录 / 混合编码 / 超短章）
- 别名归并回滚；错误合并的手动拆分
- 任务中断恢复：不重复已完成章节
- 证据 id 引用完整性（无悬空 evidenceIds）
- 互换测试阳性 / 阴性对照（不同角色 vs 同卡复制）

## 11. P1 技术方案

### 11.1 检索架构（MVP 从简，预留演进）

- 切块：语义段落优先，兜底 800–1200 字滑窗；元数据（packId、位置、章节）
- 索引 MVP：本地 JSON + 关键词倒排 + 可选嵌入文件。当前模型配置没有独立 embedding 能力声明，必须先做 capability probe；无 embedding 接口时退化为纯关键词，并允许关闭远程“查询改写/重排”，形成真正不再发送额外文本的本地路径
- 检索流程：场景查询改写（1 次小模型调用）→ 关键词 + 向量混合召回 top-20 → 模型重排取 top-3~5 → 注入 → 写使用记录
- 演进路径：规模变大后评估 SQLite FTS5 + 向量索引，**不引入独立数据库服务**
- 索引生命周期：索引键包含 `sourceHash + chunkerVersion + embeddingModel + embeddingDimension`；任何一项变化即重建。删除包时级联删除切块、嵌入、查询缓存和使用日志中的正文，仅保留必要的审计元数据

### 11.2 新增模块与 commands

```text
src-tauri/src/knowledge/mod.rs      // 包管理、蒸馏
src-tauri/src/knowledge/index.rs    // 切块、索引、检索
src/stores/useKnowledgePackStore.ts

import_knowledge_source(path, meta)          -> { packDraftId, chunkStats }
distill_knowledge_pack(packDraftId, mode)    -> KnowledgePack        // mind/value/expression 蒸馏
search_knowledge(packIds, query, limit)      -> fragments[]          // 供叙事回合内部调用
get_knowledge_usage(runId)                   -> usageLog[]           // 本轮引用溯源
```

### 11.3 上下文组装

`assemble_system_prompt` 侧新增组装器，按 §4.2 优先级硬序拼装；每层设 Token 预算上限，超限从低优先级开始裁剪。知识片段注入位置在角色内核之后、文风要求之前，并附来源标注供审校 Agent 溯源。

### 11.4 P1 测试清单

- 片段只进绑定角色的上下文（多角色并发下的隔离测试）
- 时间边界过滤生效
- 停用包后组装结果不含其片段
- 使用记录与实际注入一致
- 蒸馏输出 JSON schema 校验

## 12. P2 技术方案

### 12.1 回合循环（插入现有 book_travel 管线）

```text
素材装配 → 大纲约束解析 → 导演设局(situation_director)
  → 活跃角色并发 role_decide（每角色独立上下文：公共场景 + 自己的 DNA + 私有状态 + 检索片段）
  → arbitrate_actions（规则优先，规则不能裁决的交模型）
  → 场景写作（现有 scene writer，输入 = 局势 + 各角色意图与仲裁结果）
  → deterministic_invariant_check（schema / 私密信息 / 禁止谓词 / 锁定内容，失败即阻断）
  → narrative_critic（人物一致性与因果质量；可建议修订，不直接改状态）
  → reducer 生成并校验 StatePatch → NarrativeState 原子提交
  → 下一场景 / 章节停止点
```

### 12.2 role_decide 协议

在 `role_play` 基础上新增 `role_decide`（独立 prompt 与 agentConfig；按环节模型路由需另做，见 §12.4）。输出只是**提案**，不是状态变更命令：

```json
{
  "intent": "string",
  "action": "string",
  "speak": { "willSpeak": true, "purpose": "string" },
  "targets": ["characterId"],
  "acceptableCosts": ["reputation", "relationship"],
  "predictions": [{ "characterId": "string", "expected": "string", "confidence": 0.6 }]
}
```

组装角色决策上下文时**只注入该角色可见信息**：公共状态 + 自身私有状态 + 与自己相关的关系状态。信息边界由组装器保证（白名单制），并由 continuity_check 复核。

### 12.3 仲裁器

规则层（无模型调用）：资源 / 能力约束校验、同目标行动冲突检测、硬节点与禁止谓词校验、信息可见性判定。模型层（0–1 次调用）：仅裁决规则无法判定的行动结果与意外后果。输出包含不可变的 `intent` 引用、可执行结果和规则依据，不改写角色意图原文；状态变化统一交 reducer。硬节点与角色底线冲突时，仲裁器可以调整事件实现或进入 `blocked`，不能悄悄替角色改主意。

### 12.4 成本控制实现

- 每场景调用数 ≈ 设局 1 + 活跃角色 N + 仲裁 0–1 + 写作 1 + 审校 1 ≈ **N + 3~4**；N 默认 3，上限 5
- 运行前预估：以最近同模型/同场景类型的实际 p50/p95 输入输出 Token、重试率和单价估算；`max_output_tokens` 只作为最坏上界，不作为典型费用
- 分级模型前置：为 `AgentConfig` 增加可选 `modelId`，实现模型删除/失效时回退、provider capability 校验、设置迁移和测试；完成前所有 Agent 仍使用全局 `selectedModelId`，不得在 UI 宣称已分级路由
- 缓存：角色内核序列化结果、知识检索结果按（角色, 场景签名）缓存
- 交互预算：记录 p50/p95 总回合延迟；支持取消、章节预算硬停和“以较少角色重试”，取消后不得提交迟到结果

### 12.5 P2 测试清单

- 私有状态隔离（A 的秘密不进 B 的决策输入）
- 硬节点 / 禁止结果仲裁（构造违规行动验证拦截）
- 并发决策的确定性排序（同输入同顺序）
- 场景失败时状态不部分提交
- StatePatch 基于旧 revision、越权路径、重复提交和违反禁止谓词时均被拒绝
- 章节锁定不被重写；快照分支正确性
- 预算上限触发时的优雅停止

## 13. 新增 / 改造文件总表

**新增**：

```text
src/utils/characterCardV2.ts          // V2 类型、迁移、校验
src/utils/characterEvaluation.ts      // 互换 / 压力测试组装与报告
src/utils/storyConstraints.ts         // 大纲四级约束解析
src/stores/useExtractionStore.ts
src/stores/useKnowledgePackStore.ts
src/stores/useCharacterRuntimeStore.ts
src-tauri/src/character_engine/mod.rs // P0 提取管线入口
src-tauri/src/character_engine/extraction.rs
src-tauri/src/knowledge/mod.rs
src-tauri/src/knowledge/index.rs
src-tauri/src/narrative/mod.rs        // P2 回合编排
src-tauri/src/narrative/arbiter.rs
src-tauri/src/narrative/state.rs      // reducer、StatePatch 校验与原子提交
src-tauri/src/narrative/continuity.rs
```

**改造**（最小化侵入）：

| 文件 | 改造 | 期 |
|---|---|---|
| `usePartnerStore.ts` | schemaVersion 分流、升级动作 | P0.a |
| `useSettingsStore.ts` | 新 Agent 采样配置；P2 前新增可选 modelId 路由；Prompt 注册表重构另行评审 | P0.a / P2 |
| `lib.rs` | 注册新 commands | 各期 |
| `Background.tsx` | 仅加新向导入口 | P0.b |
| `storyAgent.ts` | 决策协议组装（不再只处理台词） | P2 |
| `agent/mod.rs` | 新增 `role_decide` 工具分支 | P2 |
| `book_travel.rs` | 管线插入决策 / 仲裁 / 审校环节 | P2 |
| `useBookTravelStore.ts` | 五层状态与分支快照 | P2 |
| `mobile_server.rs` + `runtime.ts` | 结果只读与观察模式回放路由 | P0.b / P2 |

## 14. 里程碑与验收门

```text
S0 验证脚手架（临时 schema + 手工卡 + 最小决策原型，不迁移生产数据）
  └─ G0 核心假设验证（2 周，固定评测集 + 对照组 + 成本/延迟埋点）
     └─ 通过 → P0.a 数据模型与迁移
       └─ 验收门：§3.4 条 1、5、6
     P0.b 全书提取（依赖：P0.a）
       └─ 验收门：§3.4 条 2、3、4 + §2.2 指标
     P1 知识系统（依赖：P0.a；可与 P0.b 后半并行）
       └─ 验收门：§4.3 全部
     P2 自主叙事引擎（依赖：P0.a、G0；按 Agent 模型路由是成本能力前置；知识挂载依赖 P1）
       └─ 验收门：§5.3 全部
```

每期 Definition of Done 还必须同时满足：

- 自动化：TypeScript 测试、Rust 测试、构建、格式检查通过，并补足迁移、崩溃恢复、幂等、隐私隔离和损坏文件恢复用例。
- 可观测：每个模型调用记录 `runId/agent/promptVersion/modelId/token/latency/retry/error`，但不默认记录完整私密提示词；诊断包支持用户选择性导出并脱敏。
- 可恢复：升级前备份、失败回滚、取消无迟到提交、任务可从最后一致快照恢复。
- 产品：验收用真实用户任务和盲评，不以 prompt 样例、单次演示或测试全绿代替角色质量结论。
- 隐私与权利：上传前提示数据流向；删除链路、来源权利字段和导出能力完成；发布说明明确使用远程 API 时的数据边界。

周期口径：原“2–3 人 8–12 周 / 单人 3–5 个月”只保留为量级假设。G0 后必须依据真实调用次数、p95 延迟、失败重试、人工标注与迁移工作量重新拆估；未重估前不作为交付承诺。

## 15. 立项前开放问题

1. G0 试点的 3 部公版/原创/已授权作品、金标人员与 10–20 名长篇创作者从哪里获取？
2. 每场景活跃角色默认值定 3 还是 4？（成本与戏剧密度的权衡，G0 期间 A/B）
3. 人工编辑角色行为后是否提供「创建模板新版本」入口，还是永远只影响当前故事分支？（默认后者，待用户验证）
4. embedding 依赖用户模型服务：无 embedding 接口的服务商占比多少？决定纯关键词退化路径的投入度。
5. 移动端观察模式回放的优先级：P2 内做还是 P2 后做？
6. 角色 V2 的哪些字段必须证据支持，哪些允许创作者明确标记为“再创作设定”？若不划清，证据覆盖率会奖励保守空卡或诱导幻觉。
7. 用户删除原书/知识源时，是否同时删除证据预览、嵌入和已有故事中的引用快照？需要在实现前确定数据保留策略。

## 16. 路线图扩展（v1.1 新增）

P0–P2 之后可评估平台轨道，条件性阶段规格见 [`platform-world-p3-p6-product-dev-spec.md`](./platform-world-p3-p6-product-dev-spec.md)。概览：

| 期 | 内容 | 关键前置门 |
|---|---|---|
| PX | 礼宾式放置世界与日报验证，不建设平台 | P2 有可招募高意向用户；7 日回访假设成立 |
| P3 | 账号、版本化资产、审核/删除、事件投影、通知、响应式 Web/PWA 和最小后台 | PX 通过；外部测试所需合规路径明确 |
| P4a | 免费封闭 Alpha：官方精选放置世界 + 日报 + 固定额度干预 | P3 安全、恢复、受众隔离与运营能力通过 |
| P4b | 条件性付费 Beta：订阅/运行额度优先，支付与私密建房 | P4a 留存、成本与付费意愿成立 |
| P5/P6 | 条件性章节房 / 赛事房期权 | 分别完成用户、法律和合作阶段门后独立立项 |

对本文档的两点约束性影响：

1. P0–P2 保持引擎逻辑与 Tauri 壳解耦：新模块不得直接依赖 `AppHandle` 等 Tauri 类型，通过 trait 注入事件、文件、时钟与模型能力。是否在 P3 抽成 workspace crate 由平台阶段门决定。
2. P2 只产出宿主无关、版本化的 `DomainEvent` 与 `StatePatch`。平台层在 P3 将其包装为含 worldId、受众、审核和呈现信息的 `WorldEvent`；本地引擎不得提前依赖云端世界、钱包或账号 schema。
