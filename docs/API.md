# 平台后端 API 清单（server）

> 全部端点 nest 在 `/api` 下（`server/src/app.rs`）。
> ⚠️ **本文不复述路由总数**：这个数字被漏改过多次（一度停在 103，彼时实际已 129），
> 且并行开发时任何一个批次落地都会让它当场过期——而一个「看起来精确却是错的」计数，
> 比没有计数更糟。需要数字时以代码为准，口径见 §8 的 grep 命令。
> 鉴权列语义：**JWT** = 需 `Authorization: Bearer <accessToken>`（`AuthUser` 提取器）；
> **公开** = 无需 token；**admin+角色** = 需管理员 token 且 `require_role` 通过（`admin` 角色恒通过）。
> 校验于 2026-07-26。**改路由必须同步改本文件**——与 `docs/VALIDATION.md` §3 台账同级纪律。

## 0. 挂载与 feature 门控

| 模块 | feature | 说明 |
|---|---|---|
| auth / assets / worlds / events / interventions / consents / invitations / notifications / reports / backpack / chapters / progression / subplot / memorial / onboarding / annotations / admin_api | 默认 | 默认构建即含 |
| arena / livegate | `arena` | 赛事房与直播礼物网关 |
| billing | `billing` | 计费闭环 |
| shop | `billing` 或 `arena` | 依赖复式账本，与 ledger 同门控（`app.rs:61`） |

未启用 feature 时对应路由**不注册**，请求返回 404。

**运行时开关体系**（migration 0036，`server/src/flags/`）：开关不再只有 env 一种形态。
统一读取入口 `flags::is_enabled(db, name, ctx)`，解析链**按用户 > 按世界 > 全局 > env > 代码内默认值**
（窄的赢）。运营面见 §7 的 `/api/admin/flags`。三条要点：

- 🔴 **env 是兜底而非被替代**：`runtime_flags` 表为空（迁移不插种子数据）时解析必然落到 env 分支，
  下面列的全部现存开关**行为逐字不变**。开闸是显式写入数据，不是升级的副作用。
- 🔴 **fail-closed**：查库失败 / 记录损坏（作用域非法、`enabled` 非 0-1、时间窗反转）→
  返回该开关声明的默认值，**且不再回落 env**。「安全」指不扩大用户可见范围的那一侧——
  8 个未验证开关是**关**，`MUSE_SAFETY_LEXICON`（审核链）是**开**（继续过滤）。
- **已接线本体系的开关有两个**：`MUSE_ONBOARDING`（0036 批次的参考接线，支持按用户灰度 =
  VALIDATION §2 T0「邀请制 ≤100 人」的执行手段）与 `MUSE_OOC_ANNOTATIONS`（0037 R3 新建件，
  无历史 env 语义要保留，建成即接线，支持按世界灰度）。其余 8 个存量开关仍是纯 env，
  登记表里 `wired=false`，迁移清单见 `flags::MIGRATION_NOTES`。

**各模块运行时开关现状**（与 feature 门控正交，同样"未验证功能默认关闭"）：`invitations` 全部端点由
`MUSE_ROOM_INVITATIONS` 控制，**默认关闭 → 404**；`onboarding`（新手动线）全部端点由
`MUSE_ONBOARDING` 控制，**默认关闭 → 404**；`subplot`（副本卡）全部端点**与结算铸卡**由
`MUSE_SUBPLOT_CARDS` 控制，**默认关闭 → 端点 404 且结算一张不铸**；自定义房容器装配
（副本卡的消费端，无独立端点）由 `MUSE_CONTAINER_ASSEMBLY` 控制，**默认关闭 → 建模板期拒绝声明
`subplotCardRefs`，装配期忽略容器字段走原路径**；`memorial`（传世卡 · 遗作馆）全部端点
**与封卷本身**由 `MUSE_MEMORIAL` 控制，**默认关闭 → 端点 404 且不发生任何封卷**；世界的生死状档由
`MUSE_LETHALITY_DEATHMATCH` 控制，默认关闭 → 读取侧降级为同意制；`annotations`（OOC 注解权）
全部端点由 `MUSE_OOC_ANNOTATIONS` 控制，**默认关闭 → 六端点全 404 且一行都不落库**；
`livestage`（直播场：定档 + 延迟缓冲 + 弹幕，见 §6）全部端点由 `MUSE_LIVE_STAGE` 控制，
**默认关闭 → 八端点全 404 且零副作用**（不记观众足迹、不落弹幕、不建场次、不写审计）。

---

## 1. 账号与实名（auth）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| POST | `/api/auth/challenge` | 公开 | 请求短信验证码。dev 态（`MUSE_DEV=1`）验证码在响应 `devCode` 字段回显 |
| POST | `/api/auth/login` | 公开 | 验证码登录，签发 access + refresh token |
| POST | `/api/auth/refresh` | 公开 | 用 refresh token 换新 access token |
| POST | `/api/auth/logout` | JWT | 注销当前 refresh token |
| POST | `/api/auth/age-declaration` | JWT | 年龄声明（未成年保护红线入口：拒充、禁入生死状） |
| POST | `/api/identity/verification` | JWT | 实名认证。**当前存在 dev 口子**：请求方可直提 `verified`，上线前须接真实 Provider 闭环 |

## 2. 角色卡与世界资产（assets）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| POST | `/api/assets/characters` | JWT | 发布角色卡到云端（字段级选择权在客户端落实） |
| GET | `/api/assets/characters/mine` | JWT | 我的云端角色卡列表 |
| GET | `/api/assets/characters/{id}/status` | JWT | 审核状态查询。含 `rejectReason`（人审驳回理由）、`appeal`（发布期申诉行）、`takedown`（事后下架告知——只给状态与时间，**不含运营内部理由**）、`disposalAppeal`（处置申诉最近一条）。见 §7「已过审内容处置」/「处置申诉」|
| POST | `/api/assets/characters/{id}/appeal` | JWT | 审核驳回后申诉 |
| POST | `/api/assets/characters/{id}/disposal-appeal` | JWT owner | 对**过审后被处置**发起申诉（每次处置一次；不改 `moderation`）。与上一条分属两条路径，见 §7「处置申诉」|
| GET | `/api/assets/characters/{id}/manifest` | JWT | 角色卡清单（钉住版本） |
| POST | `/api/assets/characters/{id}/avatar` | JWT | 上传立绘 |
| POST | `/api/assets/characters/{id}/withdraw` | JWT | 停止后续投放（幂等） |
| DELETE | `/api/assets/characters/{id}` | JWT | 删除 |
| GET | `/api/assets/objects/{*key}` | 公开 | 对象回读（头像等）。**能力 URL**：键含 128 位随机 id，`is_safe_object_key` 防路径穿越 |
| POST | `/api/assets/worlds` | JWT | 创作者发布世界模板（超集冗余门 `MIN_REDUNDANCY_RATIO=3.0`）。机审文本 `world_scan_text` 覆盖骨架里**一切字符串叶子**，只排除标识符/枚举/受限 DSL（排除表而非包含表——漏扫 = 内容绕过机审，见 VALIDATION §3.9） |
| GET | `/api/assets/worlds/mine` | JWT | 我发布的世界。含 `viewCount`/`favoriteCount`（**仅属主可见**，migration 0029） |
| GET | `/api/assets/worlds/{id}/status` | JWT | 审核状态 + `viewCount`/`favoriteCount`（owner 隔离，非本人 404） |
| GET | `/api/assets/worlds/{id}/manifest` | JWT | 世界清单 |
| POST | `/api/assets/worlds/{id}/withdraw` | JWT | 下架 |
| POST | `/api/assets/worlds/{id}/view` | JWT | 记一次浏览。**防刷**：同用户同窗口只计一次（去重窗口 env `MUSE_VIEW_DEDUP_WINDOW_MS`，默认 24h），属主自刷返回 `counted:false`；匿名不计数（无法按人去重） |
| POST | `/api/assets/worlds/{id}/favorite` | JWT | 收藏（幂等）。属主收藏自己的发布物 → 409（防自刷） |
| DELETE | `/api/assets/worlds/{id}/favorite` | JWT | 取消收藏（幂等） |
| GET | `/api/assets/worlds/{id}/favorite` | JWT | 我是否已收藏（只回自己的状态，不回总数） |

> 计数实现（`migrations/0029_engagement_invitations.sql`）：append-only 去重登记表 + `COUNT(*)` 派生，
> **没有可变计数列 → 没有热点行**；同人同窗口的重复浏览由主键冲突丢弃（防刷判定在数据库唯一性上，
> 不在应用层读-判-写）。

## 3. 世界运行时（worlds / events / interventions / consents / invitations / chapters）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/api/worlds/{id}` | JWT | 世界详情 |
| POST | `/api/worlds/{id}/join` | JWT | 入世（含防自刷校验：同用户一卡） |
| POST | `/api/worlds/{id}/leave` | JWT | 离场 |
| GET | `/api/worlds/{id}/events` | JWT | 事件流分页拉取 |
| GET | `/api/worlds/{id}/stream` | JWT（query） | WebSocket 实时流。浏览器 WS 不能带头，token 走 `?token=` 或 `?access_token=`，支持 `?lastEventId=` 断线补偿 |
| GET | `/api/worlds/{id}/state-summary` | JWT | 状态面板聚合 |
| POST | `/api/worlds/{id}/interventions` | JWT | 干预投递（托梦 / 递道具） |
| GET | `/api/worlds/{id}/interventions/mine` | JWT | 我的干预记录。**2026-07-27 新增** `dreamQuota`：`base` / `bonus`（OOC 申诉补偿，**单列**——玩家有权知道多出来那条是申诉换来的）/ `effective` / `used` / `remaining`。🔴 此前玩家只能靠**被拒绝**发现没额度了，而有效额度 = 基础 + 补偿，补偿是复核后补发的，玩家**在构造上算不出这个数**。统计口径与 `create_intervention` 的判定**逐字相同**（`status IN (accepted, applied)`）——已被引擎消费的托梦**仍占额度**。⚠️ 没入场的人给 `applicable: false` 而**不是** `remaining: 0`：「还没进这个世界」与「额度用光了」是两件事 |
| GET | `/api/me/consents` | JWT | 待响应的同意征询 |
| POST | `/api/worlds/{id}/consents/{cid}/respond` | JWT | 同意响应（超时保守：不可逆事件默认不发生） |
| POST | `/api/worlds/{id}/chapters/start` | JWT | 章节房开章 |
| POST | `/api/worlds/{id}/chapters/finish` | JWT | 章节结算 |
| GET | `/api/worlds/{id}/offline-gains` | JWT | 离线收益回看 |
| POST | `/api/worlds/{id}/carry` | JWT | 带入道具 |
| POST | `/api/worlds/{id}/invitations` | JWT | 发出房间邀请 `{targetCharacterId}`（房主/active 成员）。**开关默认关闭** |
| GET | `/api/worlds/{id}/invitations` | JWT | 我在该世界**发出**的邀请（只出自己发的） |
| GET | `/api/me/invitations?status=` | JWT | 我**收到**的邀请（默认 pending） |
| POST | `/api/me/invitations/{iid}/respond` | JWT | 接受/拒绝 `{accept}`（幂等）。非收件人 404 |
| GET | `/api/worlds/{id}/biography` | JWT | **BE 结局传记**（世界崩塌后的封卷，migration 0035）。**开关默认关闭**时恒 404；正常终局的世界无传记 → 404；私有房仅房主/成员可读 |

### 房间邀请（invitations，migration 0029）

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_ROOM_INVITATIONS`，**默认关闭**（VALIDATION.md §0.1）。关闭时四个端点全 404，且**已存在的邀请也读不出、响应不了**（读取侧降级，可逆急停阀）。✅ 已接入运行时开关体系（`flags`）：解析链 `runtime_flags(user=动作发起人) → global → env → false`，支持**按人灰度**。🔴 **刻意不支持按世界灰度**——收件侧 `/me/invitations` 跨世界、结构上没有 world 可传，允许 world 作用域会产出一封发得出、答不了的邀请 |
| 有效期 | `MUSE_INVITE_TTL_MS`，默认 7 天（惰性过期） |
| 防骚扰 | 无自由文本字段（结构化邀请，不是私信）· 拒绝即终局（同邀请人不得重邀同角色进同世界）· 同 (世界,邀请人,被邀角色) 至多一条 pending（重复邀请幂等复用、不重复通知）· 每人每日发出上限 `MUSE_INVITE_DAILY_LIMIT`（默认 20，跨世界合计）· 通知 kind `room_invitation` 非 essential，可被用户通知偏好静默 |
| 社交防火墙（§14） | 被邀请者由**角色 id** 寻址，邀请人只以**角色面具名**示人；`invitee_user_id` 仅服务端内部使用，**任何响应体/通知 payload 都不下发**真人身份 |

> 🔴 **接受 ≠ 入场**：`respond{accept:true}` 只把邀请置 `accepted` 并回一个 `next` 指引，
> **不写 `world_members`**；真正入场仍须调用 `POST /api/worlds/{id}/join`，于是 join 的全部服务端权威校验
> （角色属本人/approved/未撤回 · 人数上限 · 一人一卡防自刷 · 同源唯一 · 星级历练准入 ·
> 生死契约二次签署 · **未成年禁入生死状**）一条不少地生效。该性质由源码断言测试
> `invitations::tests::module_never_writes_world_members` 锁死。
> 未成年保护另有**前门拒绝 + 接受时复查**双保险：生效档为生死状的世界，未声明成年者既收不到邀请、
> 也接受不了（拒绝文案统一为通用句，不得让端点变成年龄探测器）。

### 世界系列自动扩容（migration 0035；总规格 §5「世界系列自动扩容【新增】」）

**1 号实例满员自动开 2 号**的排队分房层：运营基建，建房参数复制 + 排队队列。无新增端点——
触发点在既有 `POST /api/worlds/{id}/join` 的满员分支，登记入口在既有 `POST /api/admin/worlds`。

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_WORLD_SERIES_AUTOSCALE`，**默认关闭**（VALIDATION.md §0.1）。前门拒绝（关阀时 admin 建房不允许登记系列）+ 读取侧降级（关阀时既有系列立即停止扩容与排队指路，再打开原样恢复）|
| 第二道闸（数据侧） | 世界须**显式登记**为系列源头（建房时带 `series` 段）。未登记的世界——全部历史世界 + 全部玩家自建房——永不扩容，行为零变化 |
| 触发条件 | 仅 `POST /worlds/{id}/join` 撞人数上限（`world_full`）那一刻。无定时/预扩容：没人敲门就不多开烧预算的世界 |
| 排队队列 | **先指路、再开号**：队列里已有可入实例（`status IN (open,running)` 且 active 成员数 < `member_limit`，按号数升序）→ 直接指向它；没有才开下一号 |
| 满员回执 | 仍是 409，错误码 `world_full`；有下一号时追加 `world_full\|next={worldId}`。老客户端按 `contains("world_full")` 匹配，完全透明 |
| 世界详情 | `GET /worlds/{id}` 增 `series` 段：`seriesId` / `instanceNo` / `instanceCount` / `maxInstances` / `status` / `nextOpenWorldId`(可选)。**纯读，绝不建房**；开关关闭或未登记 → 不下发该键 |
| 上限（§0.2 参数化） | 逐系列 `series.maxInstances`（建房时设，含 1 号）∧ 全局硬顶 env `MUSE_WORLD_SERIES_MAX_INSTANCES`（默认 10，clamp 1-200），**取小**。达上限即不再扩容（每个 running 实例都进调度器并各持日预算，膨胀必须可控）|
| 幂等键 | `world_series_instances(series_id, instance_no)` 复合主键。开新号 = 同一事务内 `INSERT worlds` + 号数登记；并发抢号者整笔回滚（世界一并消失，不留孤儿房），回头重查队列命中赢家 |
| 参数复制 | 复制源恒为**1 号实例**（不是"上一号"，避免误差沿队列累积）：模板 id / 钉住的模板版本 / 房型 / 可见性 / 主播 / 人数上限 / tick 节奏 / 时间线模式 / **生死契约档** / 日 token 与 cny 预算 / **钉住的 engine·prompt·model 三版本** 全部逐字段照抄 |
| 不复制的两样 | `assembled_json`（那是**采样结果**不是参数——§5 要求每个实例按自己的种子采样，"一个模板，千个平行世界"，身份分布本身还是第二重防刷）；标题（后缀 ` #N` 以便大厅区分）|
| 审计 | 每次开号落 `audit_logs(action='world.series_expanded')`，reason 记 `series/instanceNo/clonedFrom` |

> 🔴 **扩容只解决"去哪个实例"，不解决"能不能进"**：扩容路径对 `world_members` **零写入**，
> 绝不替玩家把卡投进新实例。玩家须对新实例重新调 `POST /worlds/{id}/join`，于是**同源唯一 ·
> 一人一卡防自刷 · 人数上限 · 星级历练准入 · 生死契约二次签署 · 未成年禁入生死状**一条不少地重跑。
> 理由是硬的：这些校验有一半是**按世界**判定的，在扩容路径上复制一份必然与 join 漂移，漂移即破口。
> 该性质由源码断言 `worlds::tests::series_autoscale::series_region_never_writes_world_members`
> （扩容区不得出现 `world_members` 写入，也不得出现任何资格判定符号）+ 集成用例
> `expansion_never_bypasses_join_checks` 双重锁死。

### BE 结局传记（migration 0035；总规格 §9「世界线崩塌」）

世界线崩塌（关键角色永久退场等终局条件）→ ③归零 + ①减半 + ②已锁定保留 + **产出「BE 结局传记」**
（坏结局也是内容，封卷收藏）。**有输、有痛、有纪念、无冤案、无武器化。**

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_WORLD_BE_BIOGRAPHY`，**默认关闭**。产出侧不产出 + 读取面 `GET /worlds/{id}/biography` 恒 404。关阀期间崩塌的世界不产传记，**再打开也不追溯补写**（传记是封卷那一刻的快照，补写等于把"当时的事实"换成"今天重算的事实"）|
| 产出接点 | `progression::settle_idle_world_ending_tx`（runtime 终局结算调用的那一个）。与结算**同事务**：结算回滚则传记同滚 |
| 触发条件 | **仅崩塌**（`is_collapse_reason`，当前白名单 `key_character_exit`）。`mainline_complete` / `time_cap` / `starved` / `time_limit` 等正常终局**不产出** |
| 幂等键 | `world_biographies.world_id` 主键 + 事务内先查后写。重复/并发触发不重复产传记、不改写封卷时刻与内容 |
| 内容 | 世界元信息（标题/模板/星级/契约档/起止时刻）· 世界线摘要（拍数分档、末拍号、事件按类型计数、贡献与里程碑合计）· 崩塌原因 · 参与者足迹（角色 id、面具名、在场状态、入离场时刻、贡献分）· 崩塌折算系数 |
| 摘要长度（§0.2 参数化） | `MUSE_BE_BIO_MAX_FOOTPRINTS`（默认 200，clamp 1-2000）· `MUSE_BE_BIO_MAX_EVENT_KINDS`（默认 20，clamp 1-200）。截断时 `truncated=true` + `total` 如实给出 |
| 审计 | 封卷落 `audit_logs(action='world.be_biography_sealed')` |

> 🔴 **公共事实不可回滚（§0.3）**：传记是对既有事实的**只读汇总**。产出路径对
> `worlds`/`world_events`/`world_ticks`/`world_contributions`/`world_members` 只有 SELECT，
> 唯一写入是传记那一行 + 一条审计痕。由源码级断言（只读区内零 UPDATE/DELETE）+ 运行时断言
> （封卷前后五张表全量快照逐字节相等）双重锁死。
>
> 🔴 **无冤案：崩塌原因不许模型现编**。原因的唯一来源是 `runtime::terminal_reason()` 与
> `audit_logs(action='world.ended')` 的既有确定性数据（原始痕原样附在 `collapse.auditReason`，
> 任何人可回 `audit_logs` 对质）；责任文案取自代码内**固定字典**。摘要显式声明
> `collapse.modelGenerated=false` 与 `collapse.blameAssigned=false`——本传记**不做任何责任归属判定**，
> 「蓄意毁世界者进风控」仍是既有 `risk_events` 的事。只读区源码级不含任何模型/provider 调用。
>
> 🔴 **不复制叙事正文、不下发真人身份**：摘要只有计量与结构，正文的唯一事实源仍是 `world_events`
> （其受众投影隔离与机审门不变，绝不给正文开第二条不过闸的读路径）；足迹只记角色 id 与面具名，
> 不记 `user_id`（§14 恨隔面具原则——传记是角色的墓志铭，不是真人的花名册）。
>
> 与**传世卡**（§12，migration 0034，`memorial/`）是两件事：那是**角色**死后的封卷（遗作馆陈列），
> 这是**世界**崩塌后的封卷。两者各自独立建表、互不读写。

### 错峰调度（migration 0038；总规格 §17【拍板 16】成本工程杠杆①）

**无 HTTP 端点**——它是 `runtime::schedule_due_ticks` 内部的一条排期策略，
但它改变世界的推进节奏、并往逐拍成本台账加了三列，故在此登记口径。

| 项 | 取值 |
|---|---|
| 做什么 | 把**连载场 / 慢炖场**的 tick 优先排进供应商的夜间折扣时段（§17：5-7.5 折常见）。窗口内按「窗口占全天的比例」压缩有效间隔，**每天的拍数不变**，只是全部挤进便宜时段——不是节奏降档 |
| 开关 | `MUSE_OFFPEAK_SCHEDULING`，**默认关闭**（VALIDATION §0.1）。开关名同时是运行时开关体系（`flags/`，migration 0036）的开关名：登记进 `KNOWN_FLAGS` 后自动按 **world 作用域**灰度（user > world > global > env > 默认）；**未登记时退回 env 兜底**，语义与解析链第 ④ 层一致 |
| 🔴 直播场豁免 | `room_type='arena'`（赛事）**或** `tick_per_day >= MUSE_OFFPEAK_LIVE_TICK_PER_DAY`（默认 48，即每 30 分钟一拍以上）→ **永不延后**。§2 直播场的定义就是「一晚跑完一阶段 + 弹幕实时」 |
| 🔴 防饿死兜底 | 距上一拍超过 `interval + min(interval × MUSE_OFFPEAK_MAX_DEFER_PCT%, MUSE_OFFPEAK_MAX_DEFER_MS)` → **无视时段照跑**。默认 200% / 6h ⇒ 连载场（1 拍/时）最长静默 3h、慢炖场（4 拍/天）最长 12h。event 背靠背房无 interval 可依，直接用绝对预算（默认 6h）。**世界首拍绝不延后** |
| 优先级 | 折扣时段开启时，**被压得最久的世界先入队**（先入队 = 先被 worker 领走）。稳定排序，无人被延后时逐字保留原顺序 |
| 时区口径 | 窗口字面量按 `MUSE_OFFPEAK_TZ_OFFSET_MIN`（供应商时区相对 UTC 的分钟偏移，默认 0）**在解析期一次性折算成 UTC**，之后判定/落库/聚合全是 UTC——与 `dashboards::utc_day_start_ms` / `runtime::day_string` / `reports::day_bounds` 同一套日界，**全仓不存在第二套时区口径**（用例 `offpeak_utc_day_offset_matches_dashboard_day_boundary` 钉住） |
| 可观测 | `world_ticks` 新增 `off_peak`(0/1) · `price_ratio_pct`(名义档位，100=原价) · `defer_ms`(被压时长)。「省了多少」= `Σ cost_tokens × (100-price_ratio_pct)/100 × MUSE_TOKEN_CNY_CENTS_PER_1K`；「生效了多少」= `off_peak=1` 的拍数占比与 `Σ defer_ms` |
| 读出口 | `GET /api/admin/metrics/overview` → `cost.offPeak`（**唯一**读出口，不另开路由；窗口同 `cost.trend`，即 `?costDays=`）。另在 `cost.trend[].offPeakTokens` 给出逐日拆分。字段与单位见下表 |
| 失效方向 | 窗口一条都解析不出来 → **整个错峰退化为关闭**（配错的后果必须是「功能不生效」，不是「所有世界永远被延后」） |

**参数**（全部 env，§0.2 参数化，不写死）：

| env | 默认 | 含义 |
|---|---|---|
| `MUSE_OFFPEAK_SCHEDULING` | `0` | 🔴 总开关 |
| `MUSE_OFFPEAK_WINDOWS` | `16:30-00:30` | 折扣时段，`HH:MM-HH:MM` 逗号分隔，跨零点自动识别；重叠自动合并；起止相同视为非法丢弃。默认值 ≡ 北京时间 `00:30-08:30` |
| `MUSE_OFFPEAK_TZ_OFFSET_MIN` | `0` | 窗口字面量所处时区相对 UTC 的分钟偏移（北京时间填 `480`） |
| `MUSE_OFFPEAK_DISCOUNT_PCT` | `50` | 折扣时段的**名义**价格档位（只进记账，不参与调度判定） |
| `MUSE_OFFPEAK_MAX_DEFER_PCT` | `200` | 延后预算 = `interval × 该百分比` |
| `MUSE_OFFPEAK_MAX_DEFER_MS` | `21600000` | 延后预算绝对上限（与上一条取**较小者**，阈值越小越早触发兜底） |
| `MUSE_OFFPEAK_MIN_INTERVAL_MS` | `60000` | 窗口内压缩后的间隔地板（防窗口过窄压出突发风暴） |
| `MUSE_OFFPEAK_LIVE_TICK_PER_DAY` | `48` | 达到该节奏即按直播场豁免 |

> 🔴 **`price_ratio_pct` 是运营配置的名义档位，不是供应商账单的结算价**——用于估算错峰收益，
> 不得当对账依据。非错峰路径（开关关闭 / 直播场 / 手动端点 `arena host/tick`、`chapters/start`
> 排的拍）恒为 `100`，**即便那一拍碰巧落在夜间**：只归因给调度器真正做出的决策，宁可低估不可高估。
>
> ⚠️ **杠杆③ Batch API 本批次未实现**（约 5 折）。原因：Batch 是异步的（提交→轮询→取结果，
> 分钟到小时级），而一拍是 `run_round`（引擎内部**串行** director→decide→arbiter→writer→critic）
> → 同一事务 `commit_tick`。一拍要 5 次批往返、`CLAIM_STALE_MS=300000` 会把等批的 worker 判成崩溃
> 并重排、且中间态无持久化（批途中重启 = 世界卡在半通管线，比不做更糟）。真正能省的做法是
> **跨世界同环节合批**，需要 `crates/muse-engine` 把 `run_round` 改成可挂起/可恢复的分步状态机 +
> `ModelClient` 增加 `submit_batch`/`poll_batch`，server 侧新增中间态表与批次协调器。
> 完整可行性分析与改造路径见 `server/src/runtime/mod.rs` 的 `offpeak` 模块头。

**成本看板读出口 `cost.offPeak`**（`GET /api/admin/metrics/overview`，operator/finance；窗口 = `?costDays=`，默认 7、clamp [1,60]、UTC 日界、末桶即今天）：

| 字段 | 单位 | 含义 |
|---|---|---|
| `windowDays` | 天 | 观测窗口，恒等于 `cost.trendDays` |
| `ticks` / `tokens` | 拍 / token | 窗口内**全部**拍数与 token（各类占比的分母） |
| `offPeakTicks` / `offPeakTokens` | 拍 / token | 其中 `off_peak=1` 的部分 |
| `tickRatio` · `tokenRatio` · `savedRatio` | **0..1 小数** | 错峰拍占比 / 错峰 token 占比 / 折让占原价比。🔴 与 `successRate`、`usageRatio`、`openRate` 同一套约定，**不是百分数**；窗口内一拍都没有 → `null`（无数据 ≠ 真实的 0%），前端显示 `—` |
| `nominalCents` / `nominalCny` | 分 / 元 | 窗口内**按原价**估算的成本（与 `cost.trend[].cents` 同口径） |
| `savedCents` / `savedCny` | 分 / 元 | 估算折让 = `Σ 按档位汇总 tokens × (100-priceRatioPct)/100 × 单价`。**先按档位汇总再换算**（不逐拍取整），避免地板误差累积 |
| `effectiveCents` / `effectiveCny` | 分 / 元 | `nominal - saved`，恒 ≥ 0 |
| `deferredTicks` · `deferMsTotal` · `deferMsMax` | 拍 / 毫秒 | 被延后过的拍数与被压总时长 / 峰值 |
| `avgDeferMs` | 毫秒（浮点） | 平均延后时长，**分母只含 `deferredTicks`**；无被延后拍 → `null`，不除零 |
| `byRatio[]` | — | 按名义档位分桶（升序，折扣最深在前，原价 `100` 在末），`Σ byRatio[].ticks == ticks`。每项含 `priceRatioPct`（**百分数整数**，100=原价）· `priceRatio`（同一个数的 **0..1** 形态）· `ticks` · `tokens` · `savedCents` / `savedCny` |
| `notes[]` | — | 口径与局限自述（同 `cost.notes[]` 的范式） |

> 🔴 **最易错的一处单位**：`priceRatioPct` 是**百分数整数**（`50` = 5 折），`priceRatio` / `tickRatio` /
> `tokenRatio` / `savedRatio` 是 **0..1 小数**。把 `priceRatioPct` 当比率渲染会得到 5000%。
> 用例 `cost_offpeak_meter_keeps_pct_and_zero_to_one_ratios_apart`（`server/src/admin_api/tests.rs`）钉住这条。
>
> `cost.today` / `cost.trend` / `cost.byWorld` / `cost.total` **一律按原价计**（口径不变，0038 之前的消费者逐字不受影响），
> 错峰折让只在 `cost.offPeak` 体现，两处**不重复相减**。错峰默认关闭时三列恒为中性值 ⇒ `offPeakTicks=0`、
> 各比率为真实的 `0.0`、`savedCents=0`，看板显示空态而非报错。

## 4. 玩家账户（me / backpack / progression / subplot / memorial / onboarding / annotations / ifline / reports / notifications）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/api/me/backpack` | JWT | 背包（道具单一写入路径 `grant_item_tx` 的读侧） |
| GET | `/api/me/memberships` | JWT | 我在哪些世界里 |
| GET | `/api/me/progression` | JWT | 历练与卡位（migration 0019） |
| POST | `/api/me/card-slots/unlock` | JWT | 卡位解锁（默认 3，历练解锁至 6：500/1500/4000） |
| GET | `/api/me/reports?cursor=&cursorId=&date=` | JWT | 日报列表（`date=` 时为按日详情，不分页） |
| GET | `/api/me/reports/{id}` | JWT | 日报详情 |
| GET | `/api/me/notifications?cursor=&cursorId=` | JWT | 通知列表 |
| GET/PUT | `/api/me/notification-preferences` | JWT | 通知偏好读写 |

> **游标分页一律是复合游标 `(cursor, cursorId)`**（`/api/me/reports`、`/api/me/notifications`、
> `/admin/social/reports`）。响应回 `nextCursor`（末行 `created_at`）+ `nextCursorId`（末行 `id`），
> 下一页把**两个**都带上。只带 `cursor` 是**受支持的退化路径**（旧客户端零行为变化），
> 但同毫秒并列行横跨页边界时会静默丢行——批量写入（一次结算多件道具、一个 tick 整批事件、
> 一批同时排定的通知）共用同一个 `now_ms()`，并列是常态。理由与推导见
> `server/src/pagination.rs` 模块头注释与 `docs/VALIDATION.md` §3.3。

### 新手动线（onboarding，migration 0031；总规格 §13【拍板 21】）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/api/onboarding/presets` | JWT | 预制精品卡库（只给 id/名字/一句话卖点，**不下发卡全文**）。**开关默认关闭** |
| POST | `/api/me/onboarding/gift` | JWT | 领取新人大礼包 `{presetId?}`：发 1 张预制卡 + 建 1 个单人微本世界。幂等 |
| GET | `/api/me/onboarding` | JWT | 我的动线状态（领没领 / 投放没投放 / 已跑拍数 / 下一步）。T0 门槛的客户端读口径 |
| POST | `/api/me/onboarding/microworld/start` | JWT | 开演：微本世界 `open → running`（须已投放；幂等） |

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_ONBOARDING`，**默认关闭**（VALIDATION.md §0.1）。关闭时四个端点全 404，且**已领过的礼包也读不出、开不了演**（读取侧降级，可逆急停阀，范式同 `MUSE_ROOM_INVITATIONS`） |
| 每人一次 | `onboarding_grants.user_id` **PRIMARY KEY**（迁移 0031）——不是应用层读-判-写。领取事务把「发卡 + 建房 + 登记」放一起、登记行最后写，撞主键即整体回滚并返回与首次**逐字节相同**的回执。`Idempotency-Key` 是另一层（覆盖同一次点击的 HTTP 重试），两层都要 |
| 卡位 | 预制卡**占卡位**（`users.card_slots` 默认 3）。卡位满 → 409，撤回一张即可领取。不占位等于开了「白得一个养成容器」的侧路 |
| 微本参数（全部可 env 覆盖，VALIDATION.md §0.2） | `MUSE_ONBOARDING_MAX_TICKS`（默认 3，**兜底保证微本必然收束**）· `MUSE_ONBOARDING_MIN_TICKS`（默认 1，防秒结束地板）· `MUSE_ONBOARDING_TICK_PER_DAY`（默认 1440 ≈ 每分钟一拍）· `MUSE_ONBOARDING_TOKEN_BUDGET`（默认 8 万）· `MUSE_ONBOARDING_CNY_BUDGET_CENTS`（默认 20 分） |
| 微本世界形态 | 模板 `tpl_onboarding_micro`（`room_type='idle'`、1★、代码 `ensure_template` 幂等入库、骨架变更即升版）· `visibility='private'`（不进大厅）· `member_limit=1` · `lethality='sanctuary'`（庇护档，教学场死亡不可能） |
| 托梦「3 条」 | **不单独发放**：配额已是全局参数 `MUSE_DREAM_QUOTA_PER_STAGE`（默认 3，每卡每阶段），新卡自动享有 |
| 未实现 | 礼包的「1 张低星副本卡」待副本卡资产落地后补（见下方 TODO） |

> 🔴 **领取 ≠ 入场**：礼包只发卡 + 建房，**不写 `world_members`**；真正入场仍须调用
> `POST /api/worlds/{id}/join`，于是 join 的全部服务端权威校验（角色属本人/approved/未撤回 ·
> 人数上限 · 一人一卡防自刷 · **同源唯一** · 星级历练准入 · 生死契约签署 · 未成年门）一条不少地生效。
> 该性质由源码断言测试 `onboarding::tests::module_never_writes_world_members` 锁死（体例同 invitations）。
>
> 🔴 **单角色微本 ≠ 世界里只有一个角色**：`runtime` 的推进门 `active_cards.len() < 2` 把 NPC 也算在内，
> 故微本骨架**自带 2 个 NPC**（玩家 1 张卡 + NPC 2 = 3 张活跃卡）。骨架不带 NPC 的单人房会**永远**卡在
> `insufficient_members`。集成测试 `microworld_advances_at_least_one_tick_with_single_player` 钉住这条链路。
>
> **同源唯一取舍**：**一人一世界实例**（主因是「5 分钟速通 + 从头教学」的节奏隔离，回避撞车是顺带红利）；
> 另有两道正交保险——预制卡原创虚构无 `sourceWork` → `source_fingerprint` 落 NULL；落库显式 `pristine=0`。
>
> **TODO(副本卡)**：礼包规格里的「1 张低星副本卡」本批次未实现（副本卡资产表当时尚未落地，
> 刻意不造临时表）。副本卡项落地后，在领取事务里追加一次发放即可，开关与幂等键无需改动。
> **现已可接**：在领取事务内调 `subplot::grant_card_tx(&mut tx, &NewSubplotCard{ origin_kind:
> ORIGIN_GRANT, grant_key: format!("grant:starter:{user_id}"), star_rating: 1, .. })`，
> 幂等由 `subplot_cards(owner_id, grant_key)` 唯一键自带，无需 onboarding 侧再做去重。

### 副本卡（subplot，migration 0032；总规格 §10【拍板 1、6、7、11、17】）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/api/me/subplot-cards?status=owned\|consumed\|all` | JWT | 我的副本卡（星级 / 卡面 / 来源世界与模板 / 合成血缘）。默认只出在手的（`owned`）；响应带 `synthesisRule` 公示合成规则。**开关默认关闭** |
| POST | `/api/me/subplot-cards/synthesize` | JWT | 同星合成 `{cardIds:[...]}`：N×n★ → 1×(n+1)★。源卡销毁 + 新卡铸成同事务。`Idempotency-Key` 可选 |

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_SUBPLOT_CARDS`，**默认关闭**（VALIDATION.md §0.1）。关闭时两个端点全 404，**且世界线结算一张卡都不铸**（前门 + 产出侧双保险）；已铸出的卡不因关阀而丢失——那是资产，不是功能。范式同 `MUSE_LETHALITY_DEATHMATCH` |
| 产出（唯一来源） | 三层结算 ③ 世界线层：贡献分 → 查**公示产出表**（`worlds.assembled_json` 的 `/assembly/payoutTable/worldlineTiers[].subplotCard`）→ 确定发放。接点在 `progression::settle_worldline_tx`，与道具/历练**同事务、同一档** |
| 🔴 零 RNG | **无概率字段、无爆率、无开箱**（§10【拍板 17】+ §16「去抽卡化是定性防线的关键」）。同一贡献分恒得同一张卡，可 replay 复算。运营话术同步避开"开箱/抽卡/爆率"，统一用"结算产出 / 产出表" |
| 🔴 无交易 | 无提现红线下道具交易 = RMT 侧门（§10「玩家间交易暂不开」）。**没有任何转让/赠送/挂单端点**，`owner_id` 只在 INSERT 时写入。由路由白名单 + 源码断言 + 运行时 404 探测三重锁死 |
| 🔴 永不加战力 | 副本卡不进任何引擎决策路径（`runtime/mod.rs` 与 `muse-engine` 源码级零 `subplot` 引用，grep 级断言，口径同历练 mileage） |
| 星级封顶 | 卡 `starRating > 实例 starRating` → **整张剔除**（不降级、不替换），与装配层 `culled_over_tier` 同口径 |
| 幂等 | `subplot_cards(owner_id, grant_key)` DB 唯一键（**自带**，不寄生在调用方转变沿上）：结算 `settlement:{world_id}:{character_id}:worldline` · 合成 `synthesis:{升序源卡 id}` · 礼包 `grant:...` |
| 参数化（VALIDATION.md §0.2） | `MUSE_SUBPLOT_SYNTHESIS_N`（配方张数，默认 3，clamp [2,10]）· `MUSE_SUBPLOT_MAX_STAR`（星级上限，默认 5，clamp [1,10]）。规格里的「3×2★→1×3★」是初值不是承诺 |
| 数据侧默认关闭 | 模板产出表未声明 `subplotCard` → 该档不发卡。开闸靠运营录数据，不靠代码合并 |

> 🔴 **为何独立于 `items`/`backpacks`**：副本卡是**永久蓝图**（装进自定义房，房散卡还在），
> 而 `backpacks` 是可携带、可被引擎消费的道具持有关系，`items` 更是**引擎可见的世界物**
> （随 `carry` 进 `RoundInput`）。塞在一起就得给 carry/admission 全链路开一串「这个 tag 不参与」的
> 例外——例外即漏点。合成还需要销毁语义与血缘（`consumed_into` / `synthesized_from_json`），
> 道具的 `consumed`（用掉）与副本卡的 `consumed`（熔成另一张）不是同一件事。§18 衔接表亦明写新表。
>
> 合成的三重幂等：① 事务原子性（先铸后熔，任一步失败整笔回滚，绝不"熔了却没出卡"）·
> ② 源卡 `status='owned'` 条件 UPDATE 的 CAS（并发/重放抢不到即回滚）· ③ `grant_key` 唯一键。
> 销毁是**软删**（`status='consumed'` + 反向指针），回收口必须可溯（§0.2 全链审计）。

### 自定义房装配（容器世界，migration 0033；总规格 §10「自定义房闭环」）

副本卡的**消费端**：打官方世界 → 结算得卡 → 合成 → **装进自提取世界容器开房**。
技术方案见 `docs/build/spec-subplot-cards.md` §3/§4/§5（⚠️ 该文件 §1/§2/§6/§7/§8 业务假设已作废）。

**无新增端点**：容器形态是**模板骨架的一段声明**，随既有建模板路径入库
（`POST /api/assets/worlds` 创作者发布 / `POST /api/admin/world-templates` 运营建模板），
两处都经 `assembly::validate_skeleton_refs` 校验；装配在 `assembly::assemble_instance` 内完成。

`skeleton_json` 顶层新增字段（全部可选，未声明 = 普通模板，**行为与产物逐字节不变**）：

```jsonc
"subplotCardRefs": [ { "cardId": "scard_a", "cardVersion": 3, "weight": 1.0 } ],  // 版本钉住
"seams":   [ { "from": "core-hub", "to": "scard_a:loc-gate" } ],  // 跨卡缝合边（两端须是 anchors）
"anchors": [ "core-hub" ],                                        // 本骨架对外缝合口白名单（秘境不可入）
"nexus":   { "name": "十字驿站" }                                  // 枢纽地点名（缺省「交汇之地」）
```

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_CONTAINER_ASSEMBLY`，**默认关闭**（VALIDATION.md §0.1）。**前门拒绝**（关闭时建模板不许声明 `subplotCardRefs`）+ **读取侧降级**（装配期忽略 refs，走原路径、产物逐字节不变）双保险，可逆急停阀，范式同 `MUSE_LETHALITY_DEATHMATCH` |
| 命名空间 | 卡内 id 一律重写为 `{cardId}:{原id}`（定义位与引用位全集）；**容器本体不加前缀**——本体 id 必须在开关一开一关两种形态下逐字相同，否则章节发货幂等键 `hook_key={world_id}:{cid}:{poolItemId}` 会漂移成两个 key → 重复发货。归属映射 = 前缀（裸 id = 本体） |
| 卡内容来源 | 卡是**蓝图指针**：`subplot_cards.source_template_id@source_template_version` 指向的模板骨架即卡片段（0032 原话「内容蓝图指针，自定义房装配的解引用入口」）。蓝图未过审/已下架 → 停止后续建房 |
| 实例种子 | **仅容器形态**升为四段式：`H(world_id‖阵容指纹‖template_version‖卡集合指纹)`，卡集合指纹 = 排序去重的 `{cardId}@{cardVersion}` 以 `\n` 连接。普通模板恒走原三段式（测试向量与黄金世界回归逐字节锁死）。防「换一张卡组合刷同一世界」 |
| 缝合 | 卡内 `connections` 只许闭包在卡内；跨卡连接**只能**经 `seams` 显式声明且两端须在各自 `anchors` 白名单内、非秘境。合并后仍不连通 → 自动生成枢纽地点 `loc-nexus`（保留 id）接上各连通分量的代表锚点；枢纽与全部缝合端点进地点采样**必选种子** |
| 建房期拒绝 | 静态门（`validate_skeleton_refs` 第 6 段）：卡引用重复/空/含 `:`、本体 id 含 `:`、占用 `loc-nexus`、锚点悬空或指向秘境、缝合边悬空/自环/指向未引用的卡、权重非法。合并门（`compose_container_skeleton`）：卡内 id 含 `:`、卡内引用悬空、缝合口不在锚点白名单、cosmology 不相容、分量无合法缝合口。**一律 400 拒绝装配，不留到运行时静默退化** |
| 🔴 装配不消耗卡 | 副本卡是**永久蓝图**（§10【拍板 11】"装入自定义房，房散卡在"）。装配只在 `world_container_cards`（0033）INSERT 一行引用，**绝不 UPDATE/DELETE `subplot_cards`**（唯一销毁语义是合成，归 `subplot/` 独占）。一卡多房是正常形态；源码级断言 + DB 全链路用例双守 |
| 🔴 永不加战力 | 卡只贡献**内容**（剧情线/主线/内容池/结局/NPC/道具/地点），**绝不贡献规则维度**——`payoutTable`/`identityPool`/`assemblyRules`/`sampling`/`isSuperset`/准入策略一律只认容器本体（顺带杜绝「卡里再引用卡」的递归）。卡内道具 `powerTier` 合并时夹到 `min(容器星级, 卡星级)`（只降不升，`effectTags` 恒不变）；钩子奖励再过既有星级封顶与稀有预算 |
| 🔴 cosmology 相容 | 卡内道具逐件跑既有 `admission::check_admission`（零新机制）：`Rejected`/`Sealed` → 建房期拒绝；`Translated`（容器显式 `rejectedHandling=translate`）→ 降档放行 |
| 确定性 | 合并是纯函数（按模板序遍历、无 RNG）；卡权重乘进该卡各 storyline 的选取权重，**不新开 RNG 子流域**，故既有六个采样域的消费协议一字未动。同 (world_id, 阵容, template_version, 卡集合) 恒得同一份装配 |
| 审计段 | `assembled_json./assembly/sampling` 新增 `cardSetFingerprint`（哈希，不存明文）+ `selectedCards`，均 `skip_serializing_if` → 非容器实例逐字节不变。**仅服务端/审计可见**，绝不进 members_projection 或日报 |

### 传世卡 · 遗作馆（memorial，migration 0034；总规格 §12【拍板 23】）

**死亡 = 传记封卷，不是资产清零。** 卡死后转「传世卡」：**只读、入遗作馆陈列、不可再入世界**；
道具归账户背包；与其有羁绊的**在世**角色获得「故人」印记。
**内核可复制，履历不可复制**——同内核开新卡 = 转世（双胞胎），不是复活：它没死过那一次。

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/api/memorial/characters?limit=&offset=` | JWT | 遗作馆陈列（按封卷时刻倒序）。**开关默认关闭** |
| GET | `/api/memorial/characters/{id}` | JWT | 传世卡详情：累计人生 = 历练 + 传记 + 足迹 + 谁还记得他。在世的卡 404 |
| GET | `/api/me/memorial/marks` | JWT | 我的角色获得的「故人」印记 |
| POST | `/api/me/characters/{id}/memorial` | JWT | **封卷**（本模块唯一写端点）。服务端核验死亡公共事实；幂等。`Idempotency-Key` 可选 |

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_MEMORIAL`，**默认关闭**（VALIDATION.md §0.1；死亡属 §2 中 T5 才验证的范围，T0-T2 明写「暂不验证：死亡」）。关闭时四个端点全 404，**且不发生任何封卷**。已封卷的卡不因关阀回到在世——封卷是单向状态转换，不是可开可关的功能，关阀只让它暂时不可见 |
| 🔴 不可再入世界 | 封卷是**原子双写**：`memorial_status='sealed'`（语义状态 + 幂等 CAS）**与** `withdrawn=1`。后者复用 `worlds::join_world` **既有**的 `withdrawn != 0` 门（→ `character_withdrawn`）——`worlds/mod.rs` **一行未改**。join 的资格查询是列名写死的 SELECT，新列它读不到，故只加新列等于没拦住 |
| 🔴 withdrawn 必须单向 | 上面那道门成立的前提是全仓不存在把 `cloud_characters.withdrawn` 置回 0 的 SQL（已 grep 核实，并由 `red_line_withdrawn_is_one_way_across_the_repo` 跨 8 个模块源码断言守死）。任何「取消下架」端点都会变成复活侧门 |
| 🔴 公共事实不可回滚（§0.3） | 封卷**不改写世界线**：不写 `world_events` / `narrative_state_json`，不改 `consent_requests`，不删 `world_members`（足迹是履历）。源码级断言 `red_line_never_rewrites_worldline` |
| 🔴 故人印记落独立表 | `memorial_marks`，**绝不进 `worlds.narrative_state_json`**——那一列每 tick 经 `build_seed_state` 原样回灌进引擎 `RoundInput.state`，写进去即把记账喂给决策（§0.1 平权红线，口径同 0025 贡献账本 / 0030 critic / 0032 副本卡）。引擎侧零 `memorial` 引用，grep 级断言 |
| 🔴 无隐藏数值（§12） | 传世卡与印记**不带任何加成/系数/强度**。卡的价值 = 历练 `mileage` + 传记 + 足迹 `world_members` + 羁绊 `memorial_marks`，全是**已存在的显性资产**。`memorial_marks` 只有「谁记得谁、在哪、何时」 |
| 死亡核验（服务端权威） | **两条证据缺一不可**：(a) `consent_requests` 有 `event_kind='death'` / `status='approved'` 且 subject 含本卡；(b) 该世界 `narrative.pendingConsents` **已不含**本卡的 death 条目（= 引擎已落定并清账）。**授权 ≠ 死亡**：引擎在下一拍才凭 `approved_consents` 落定，只看 (a) 会把活角色误封卷（捏造死亡）。查不到证据 → 409，绝不 fail-open |
| 幂等 | 两层：① `Idempotency-Key`（同一次点击的 HTTP 重试）；② **DB 状态 CAS**（`WHERE memorial_status='living'`）——抢到才归还道具、才打印记，重复封卷命中 0 行整段短路，`sealed:false` + 两个计数恒 0。印记另有 `memorial_marks(character_id, deceased_character_id)` 唯一键作第二道闸 |
| 道具归还口径 | §12 原文「道具归账户背包（**道具本为账户资产**）」= **解除携带**（`carried\|sealed → owned`，清 `carried_world_id` 与 S-5 降档覆盖），**不是** `grant_item_tx`。后者是 INSERT 类**发货**路径，对本就在账户里的道具再发一次会凭空多出一行——一次死亡把道具变成两件，违反 §0.2 资产守恒。本模块**绝不 INSERT `backpacks`**（`red_line_never_mints_items` 断言）；背包总行数封卷前后恒等 |
| 角色面具（§14） | 遗作馆与详情**只出角色维度事实**，不出 `owner_id`/昵称/任何真人身份。遗作馆是角色的墓园，不是玩家名录 |
| 参数化（§0.2） | `MUSE_MEMORIAL_BOND_MIN`（够得上「故人」的羁绊强度阈值，默认 0.0 = 有关系记录即算；强度 = `max(\|trust\|,\|affinity\|,\|fear\|,\|debt\|)`，**取绝对值**——§12 说的是羁绊不是友谊，宿敌同样成立）· `MUSE_MEMORIAL_PAGE_SIZE`（默认 20，clamp [1,100]） |
| 转世 | 同内核开新卡走既有 `POST /api/assets/characters`，本模块零特权：新卡新 id、零历练、空传记、无足迹、无印记，不进遗作馆。封卷置 `withdrawn=1` 顺带腾出卡位（`count_active_cards` 只数 `withdrawn=0`）。**同源唯一（0021）对转世卡照常生效**——同一提取源的 `pristine` 卡在同世界仍只允许一张，这是预期行为（「这个世界只有一个唐三」），未开任何后门 |

> 🔴 **遗作馆只读**：`/api/memorial/*` 下**只有 GET**，没有任何编辑/删除/复活传世卡的端点。
> 唯一的写端点 `POST /api/me/characters/{id}/memorial` 刻意放在 `/me/characters` 命名空间下——
> 它是**卡的状态转换**，不是对陈列品的编辑。由路由白名单（含逐条方法核对）+ 运行时探测双重锁死
> （`red_line_memorial_hall_is_read_only`）。
>
> **接线待办（自动封卷）**：本批次的封卷入口是**玩家主动认领**（服务端核验公共事实）。
> 自动封卷的正解是在「死亡落定」处直接调 `memorial::seal_character_tx`，但那一处在
> `runtime::commit_tick` 内；且平权红线要求 `runtime/mod.rs` 对资产模块零引用（同副本卡的处理：
> 结算铸卡挂在 `progression::settle_worldline_tx`，不挂 runtime）。故正确落点是**结算侧的薄接线层**。
> 该接线未做前，**生死状档（`Lethality::Deathmatch`）的死亡无法封卷**——该档入场即签、
> 引擎不产 `ConsentRequested`，证据 (a) 恒不成立。生死状档本身也默认关闭，故当前无实际缺口。
>
> **已知副作用（待 progression 侧裁定）**：`withdrawn=1` 会让传世卡退出
> `progression::total_mileage`（`SUM(mileage) WHERE withdrawn = 0`），于是**死亡会拉低"下一个卡位"
> 的解锁进度**。已解锁的卡位不受影响（`users.card_slots` 是只增的存量列，非派生量）。
> 这与 §12「死亡不是资产清零、履历是显性资产」有张力：更贴合规格的口径应是
> **总历练把传世卡也算进去**（`WHERE withdrawn = 0 OR memorial_status = 'sealed'`）。
> 该判断与改动均在 `progression/` 内，本批次未越界修改，留给该模块的负责人裁定。

### OOC 注解权（annotations，migration 0037；总规格 §7「人设保险（三级出口）」**第 2 级**）

> 规格原文：**事中·注解权**：单拍 OOC 申诉——世界事实不改，**私人传记可加内心批注**；
> 复核确认模型错误则补偿托梦配额。**事实归世界，解释权归玩家。**

```text
世界说：他在城门口退了一步。          ← world_events，公共事实，永不改写
玩家写：他不是怕，他在等那个人先走。  ← character_annotations，私人解释，只他自己看得见
```

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| POST | `/api/worlds/{id}/ooc-appeals` | JWT | 提 OOC 申诉（可附内心批注）。**幂等**：同一 `(worldId, tickNo, characterId)` 只受理一次，重复提交回既有那条 + `created:false`。**开关默认关闭** |
| GET | `/api/me/ooc-appeals?status=&limit=&offset=` | JWT | 我的申诉（含批注、复核结果、托梦补偿）。硬边界 `WHERE user_id = 本人` |
| PUT | `/api/me/ooc-appeals/{id}/annotation` | JWT | 加/改内心批注（可在申诉之后补写）；`body` 空串 = 清空。改别人的一律 **404**（不是 403） |
| GET | `/api/me/characters/{id}/annotations` | JWT | 我的角色传记批注（私人解释层）。卡非本人 → 404 |
| GET | `/api/admin/ooc-appeals?status=&limit=&offset=` | **reviewer** | 复核队列（默认 `status=pending`） |
| POST | `/api/admin/ooc-appeals/{id}/review` | **reviewer** | 复核。`decision` = `confirm_model_error`（确认模型错误 → 补偿托梦配额）\| `dismiss`；`reason` 必填 ≤500 字 |

请求体（提申诉）：

```jsonc
{
  "tickNo": 12,                     // 必须是已落定的拍（world_ticks.status='done'），否则 400
  "characterId": "cc_1",            // 必须是本人在该世界的卡（world_members 有行），否则 RiskBlocked
  "reasonCode": "ooc",              // ooc | unfair_ruling（正对 T1 门槛「OOC/裁决不公」两个词），未知值 400
  "reasonText": "他不会在城门口退这一步",   // 必填 ≤500 字
  "annotation": "他不是怕，他在等那个人先走。"  // 可选 ≤1000 字，只对本人可见
}
```

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_OOC_ANNOTATIONS`，**默认关闭**，经 `flags::is_enabled` 解析（解析链 user > world > global > env > 默认）。关闭时六个端点全 404 **且一行都不落库**。⚠️ **推荐灰度作用域是 user 或 global**：只按 world 灰度时玩家能提申诉但读不到 `/me/ooc-appeals`（那三个端点无 world 坐标）；world 作用域适合「临时关掉某个出问题世界的入口」这种收窄动作 |
| 🔴 世界事实不改（§0.3） | 申诉/批注/复核**不写** `worlds` · `world_events` · `world_ticks` · `world_members` · `world_contributions` · `consent_requests` · `interventions` · `backpacks` · `cloud_characters` · `world_biographies` 中的任何一行。红线用例 `red_line_appeal_and_review_leave_worldline_byte_identical` 对这十张表做**逐字节快照比对**（不是源码级近似），另有源码级 `red_line_never_rewrites_worldline` |
| 🔴 承认错误 ≠ 回滚事实 | `confirmed` 的含义是「我们承认这一拍演砸了」，**不是**「这一拍没发生过」。响应恒带 `worldFactChanged:false` / `worldlineChanged:false`，免得前端把「申诉成立」渲染成「这一拍被撤销」 |
| 🔴 批注为何无法冒充事实 | ① 独立表独立行（`character_annotations`，与 `world_events` 无外键/视图/UNION 读路径）；② 每行都有 `owner_id NOT NULL`，而世界事实表**没有 owner 列**——有主人的数据在形状上就不是「世界说的话」；③ 引擎零读取路径（`runtime`/`crates` 对三张新表零引用，grep 级断言），批注永不进 `RoundInput.state`；④ 读取面自带 `layer="annotation"` + `isWorldFact=false`，世界事实走 `/api/worlds/{id}/events`，两条管道各出各的 |
| 🔴 状态命名 | `pending` / **`confirmed`**（申诉成立）/ `dismissed`。刻意**不用 `upheld`**：`moderation_appeals` 里的 `upheld` 意思是「维持原判」= 申诉被驳回，与此处正好相反，同词反义是看板上最容易算反的坑 |
| 🔴 社交防火墙（§14） | 批注**只对本人可见**且**不出真人身份**：`owner_id` 只用于 SQL 过滤，从不出现在任何响应体里；申诉列表按 `user_id` 硬隔离；越权一律 404（403 等于承认该条存在）。复核回执只给 `reviewerAssigned` 布尔，不给复核人 id |
| 托梦补偿（§8 配额） | 确认模型错误 → `dream_quota_compensations` 落一条（`grants` 条数，`MUSE_OOC_COMPENSATION_WHISPERS` 默认 **1**、上限 10）。🔴 **不往 `interventions` 插行/改行**：那等于伪造玩家从未发过的托梦，或抹掉「已被引擎消费」这个已落定的事实。补偿只提供**加数**——有效配额 = `dream_quota_per_stage()` + `SUM(grants)`，`interventions` 的 `COUNT(*) ... status IN ('accepted','applied')` **一个字符不用改** |
| 🔵 接线待办 | 兑现补偿需 **`interventions` 的负责人改 2 行**：`let bonus = crate::annotations::dream_quota_bonus(&state.db, &world_id, &req.character_id).await;` + `if used >= dream_quota_per_stage() + bonus`。无表结构变化、无计数 SQL 变化、无拒绝语义变化。附带建议：`/worlds/{id}/interventions/mine` 把 quota 拆成 `base`/`bonus`/`total`。**在该行接上之前补偿已真实入账、可查、可审计**，只是尚未在托梦受理处兑现 |
| 幂等 | 两层：① `Idempotency-Key`（同一次点击的 HTTP 重试）；② **DB 唯一键** `(world_id, tick_no, character_id)`——换 key 再点也只读回既有那条。复核另有**状态 CAS**（`WHERE status='pending'`，重复 409）+ 补偿表 `appeal_id` 唯一索引双闸 |
| 审计 | 复核是运营改判，`audit_logs` 落 `ooc_appeal.confirmed` / `ooc_appeal.dismissed`，`subject = ooc_appeal:{id}`，reason 含 `状态\|world\|tick\|character\|compensation=N\|复核理由`。**与状态更新、补偿写入同一事务**——不存在「改判了但审计没落」的中间态 |
| 机审 | 批注走 `safety::moderate_and_queue`（Pending 自动进 `audit_queue` 人审）。**私密不豁免机审**：私密只决定「谁能看」，不决定「平台是否为它负责」。无论裁决都落库，读取面仅 `approved` 才给正文（否则 `body:null` + `withheld:true`），人审改判后自动恢复 |
| 复核队列可见性 | 走 `entry_ever_open`（入口曾对**任何人**开放过即可复核）而非全局解析——否则按世界灰度时「申诉进得来、复核进不去」，队列直接卡死 |
| **SLO 接线** | 本表是 `slo::ooc_appeal_block` 的唯一数据源，使 VALIDATION §4.2 八项里最后一项「唯一未解」的 **OOC 申诉率**转为可算，T1 门槛「OOC/裁决不公申诉 <10%/阶段」第一次具备判定手段。口径见下 |

**SLO「OOC 申诉率」口径**（`GET /api/admin/metrics/overview` → `narrativeSlo.metrics.oocAppealRate`）：

| 项 | 定义 |
|---|---|
| 分母 | 窗口内**演过戏**的世界（有 `world_ticks.status='done' AND cost_tokens>0` 的拍）× 其 `world_members` 行 = 「角色 × 阶段」对。阶段口径 = 一个 world 实例（与托梦配额一致）。NPC 不入 `world_members`，无需像基尼那样取交集 |
| 分子 | 窗口内新建申诉按 `(worldId, characterId)` **去重**后的对数（同一角色对多拍申诉 = 一个角色不满意，不是多个）。分子施加与分母**相同的两个 EXISTS**，故 分子 ≤ 分母 恒成立 |
| 三态 | `entry_not_open`（入口从未开放，value=null，显示 `—`）/ `no_data_in_window`（窗口内零样本，value=null，`—`）/ `ok`（真数，**可以是 0.0**）。🔴 三者不可混同：本功能默认关闭，若直接报 0% 会得到「一个看起来棒极了、实际上什么都没测的数」，而 T1 恰恰要拿它决定继续/调整/停止 |
| 辅助数 | `appealsTotal`（原始条数）· `byReasonCode`（ooc vs unfair_ruling，两类的改法完全不同）· `byStatus` · **`confirmedRate`**（坐实率 = confirmed / 已复核，一条没复核 → null）· 补偿发放量 |
| 🔴 申诉率 ≠ 坐实率 | `value` 是「多少人不满」（T1 门槛盯的），`confirmedRate` 是「其中多少确实是模型的错」。混成一个数会同时丢掉两个信号 |
| 门槛 | `MUSE_SLO_OOC_APPEAL_RATE_MAX`，默认 **0.10**（T1 原文数值，作为默认值而非常量语义——预注册纪律「开测前可改、开测后冻结」） |

> 🔴 **`moderation_appeals` 不可冒充**：那是**内容风控申诉**（只受理 rejected 的卡/头像、每主体终身一次），
> 与「角色演得不像 / 裁决不公」零关系。库里有内容风控申诉时 `oocAppealRate` 照样只读 `ooc_appeals`，
> 由 `slo::tests::ooc_appeal_rate_never_reads_moderation_appeals` 与
> `admin_api::tests::narrative_slo_marks_remaining_metrics_as_no_data_source` 双向锁死。

### if 线付费副本（ifline，migration 0039 立项 + **0041 推进**；总规格 §7「人设保险（三级出口）」**第 3 级**）

> 规格原文：**事后·if 线**：世界结束后花资源以某拍为分叉点开单人平行线副本（**不影响原世界线**）——
> 把遗憾变成付费内容。

三级出口至此完整：事前底线硬约束（engine/critic）· 事中注解权（0037）· **事后 if 线（本节）**。

```text
原世界线：他在城门口退了一步 → 城破 → 世界结束     ← worlds/world_events，永不改写
if 线：   从终局那一拍岔出去的、只属于你的一条平行线   ← ifline_worlds，独立实例、有主人、不进结算
```

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/api/worlds/{id}/ifline-fork-points` | JWT | **可用分叉点 + 限制说明**（诚实面：客户端不必猜）。含 `eligible` / `supportedForkPoints` / `unsupportedForkPoints`（带证据与补齐路径）/ `cost` |
| POST | `/api/worlds/{id}/iflines` | JWT | 开 if 线（**烧副本卡**）。**幂等**：同一 `(ownerId, originWorldId, characterId, forkPoint, forkTickNo)` 只开得出一条，重复提交回既有那条 + `created:false`。**开关默认关闭** |
| GET | `/api/me/iflines?status=&limit=&offset=` | JWT | 我的 if 线列表。硬边界 `WHERE owner_id = 本人` |
| GET | `/api/me/iflines/{id}` | JWT | 一条 if 线（含**冻结分叉快照**与剥离台账）。别人的一律 **404**（不是 403） |
| POST | `/api/me/iflines/{id}/beats` | JWT | **推进一拍**（0041）。并发闸 = `(ifline_id, beat_no)` 唯一键：同一拍只跑一次，抢占失败 **409 且一个 token 都不花**。到拍数上限则**强制收尾**（不调模型）。别人的一律 **404** 。**2026-07-27 起改为异步（migration 0050）**：回 **202 已受理**（不是「已推进」），模型调用在后台 worker 里跑，玩家端轮询 `GET /me/iflines/{id}` 的 `advance.{pending,lastError}` 与 `GET /me/iflines/{id}/beats`。🔴 「玩家拉动」不变——入队只由点击触发，没有调度器会碰 `ifline_worlds`。在飞期间重复点击 409（请求层 CAS 闸；`(ifline_id, beat_no)` 唯一键仍是第二层）。⚠️ 队列不持久：进程重启带走在飞任务时，`pending` 会挂到 `MUSE_IFLINE_ADVANCE_STALE_MS`（默认 10min）才自动解除——那是**让你能再点一次**。**自动补上丢掉的那次**由对账补投负责（`MUSE_IFLINE_ADVANCE_SWEEP`，默认关闭，migration 0052）：它只补投「玩家已经点过」的那一次，有补投次数封顶，到顶时把原因写进 `advance.lastError` 而不是静默放弃 |
| GET | `/api/me/iflines/{id}/beats?limit=&offset=` | JWT | 这条平行线的正文（按拍序）= **终局产物的全部形态**：可读的私人传记 + 结局名。恒带 `grantedAssets: []` |
| GET | `/api/admin/iflines?status=&limit=&offset=` | **operator** | 运营只读列表（`status` ∈ `sealed`/`running`/`ended`，默认 `sealed`）。不下发 `ownerId`；含 `beatCount` / `costTokensTotal` / `endingReason` |
| GET | `/api/admin/iflines/cost?since=&until=` | **operator** | 🔴 **if 线 token 开销读数**（0041）。时间窗 BIGINT 毫秒，缺省回看 30 天。响应的 `dashboardIntegration` 明写主看板**已并入**（`cost.ifline` + `cost.combined`）以及「`cost.total` 仍是世界线口径、不会改」 |

请求体（开 if 线）：

```jsonc
{
  "characterId": "cc_1",        // 必须是本人在该世界的**在世**卡（传世卡 400，见下）
  "forkPoint": "terminal",      // 可省；当前**只接受** terminal，其它值 400（绝不静默降级）
  "tickNo": 12,                 // 可省；给了就必须**等于**终局拍，否则 400 并写明唯一可用的拍号
  "premise": "如果他在城门口没有退那一步。",  // 可选 ≤1000 字，过机审
  "cardIds": ["sc_1"]           // 数量须恰好 = MUSE_IFLINE_CARD_COST（默认 1）；由玩家显式点名
}
```

#### 🔴 分叉点的状态从哪来，以及它的限制（本功能最容易做假的地方）

规格写「以某拍为分叉点」，但**仓库里没有任何一拍的状态快照**。核实证据：

| 候选数据源 | 实际内容 | 能否还原第 N 拍 |
|---|---|---|
| `world_ticks` | `base_revision` / `status` / `cost_tokens` / `attempts` / 错峰三列 | ❌ **没有一列存状态** |
| `worlds.narrative_state_json` | 单行，每拍被 `commit_tick` 的 CAS **覆盖** | ❌ 只有最终态，历史版本不留存 |
| `world_events` | 投影后的**展示文本**；引擎 `StatePatch` 在 `commit_tick` 里被丢弃、从不落库 | ❌ 事件流无法重放出中间态 |
| 引擎 FS（`store.rs`） | DB 那一列的每拍物化 | ❌ 同样只有当前态 |

因此本实现**只支持终局分叉**：`forkPoint='terminal'`，状态源 = 原世界 `narrative_state_json`
（世界已 `ended`，那份 JSON 就是最后一拍提交后的状态），**逐字节复制**后按 §14 剥离他人角色。
`forkTickNo` = `MAX(tick_no) WHERE status='done'`（**不是** `MAX(tick_no)`：没落定的拍不能当分叉点）。

- 请求中间拍 → **400**，报文点名「无法从第 N 拍分叉」+ 唯一可用的终局拍号，且**一张卡都不烧**；
- 请求 `forkPoint=tick` → **400**，不静默降级成 terminal；
- 每次读取恒下发 `forkPoint.stateFidelity`（当前唯一取值 `origin_terminal_state`）与 `isApproximate:false`。

> 🔴 **为什么不做「降级近似」**：用终局态冒充第 N 拍，会让玩家为一个假分叉付费。
> 宁可功能弱一点，也不给「看起来是那一拍、其实不是」的东西。
> 🔵 **补齐路径**：先加一张逐拍状态快照表（每拍多存一份完整 `NarrativeState`），再扩 `fork_point='tick'`。
> 表结构已留位（`fork_point` / `fork_tick_no` / `state_fidelity` 三列），不必改表。
> 用例佐证：`red_line_mid_tick_fork_is_rejected_without_touching_resources` ·
> `red_line_unsupported_fork_point_kind_is_rejected` · `fork_points_endpoint_declares_the_limitation`。

| 项 | 取值 |
|---|---|
| 运营开关 | `MUSE_IFLINE_PARALLEL`，**默认关闭**，经 `flags::is_enabled` 解析（解析链 user > world > global > env > 默认）。关闭时五个端点全 404 **且一行都不落库、一张卡都不烧**。⚠️ **推荐灰度作用域是 user 或 global**：只按 world 灰度时玩家能开 if 线但读不到 `/me/iflines`（那两个端点无 world 坐标）；运营列表走 `entry_ever_open`（入口曾对任何人开放过即可见），否则已烧掉玩家卡的 if 线运营查不到 |
| 🔴 不影响原世界线（§0.3） | 开 if 线**不写** `worlds` · `world_events` · `world_ticks` · `world_members` · `world_contributions` · `consent_requests` · `interventions` · `backpacks` · `cloud_characters` · `world_biographies` · `arena_rewards` 中的任何一行。用例 `red_line_opening_ifline_leaves_worldline_byte_identical` 对这十一张表做**逐字节快照比对**，另有源码级 `red_line_never_writes_worldline` |
| 🔴 **if 线不是一行 `worlds`** | 本批次最重要的结构决定。一行 `worlds` + `world_members` 会被 `runtime::commit_tick → end_world_tx → finalize_ending_tx` 自动带进 `progression::settle_idle_world_ending_tx`（发历练）/ `subplot::settle_subplot_card_tx`（铸卡）/ `arena_rewards`（荣誉）——历练是准入与卡位解锁的钥匙，于是「花钱开 if 线」立刻等于「花钱买数值」，踩穿 §0.1。放进独立表 `ifline_worlds` 后那条反哺路径**物理上不存在**（结算管线只认那两张表）。用例 `red_line_ifline_is_not_a_world_row` |
| 🔴 产出不反哺（§0.1） | `ifline_worlds` **没有任何数值列**。开 if 线后历练 / 背包 / 贡献账本 / 荣誉全部零变化，副本卡**总行数不增**（本模块不 INSERT `subplot_cards`）。用例 `red_line_ifline_grants_nothing_back_to_origin` |
| 🔴 读取面为何无法冒充世界线 | ① id 空间不同（`ifw_` 前缀）；② `owner_id NOT NULL` 而 `worlds` **没有 owner 列**——有主人的世界在形状上就不是「大家共处的那条世界线」；③ 读取管道分离（if 线只经 `/me/iflines**`，世界事实只经 `/worlds/{id}/events`）；④ 响应恒带 `layer="ifline"` / `isWorldFact=false` / `affectsOriginWorld=false` / `forkPoint.stateFidelity`；⑤ `runtime` 与 `crates/muse-engine` 对 `ifline_worlds` 零引用（grep 级断言） |
| 🔴 单人平行线（§14 社交防火墙） | 冻结前**剥离他人玩家角色**：`characters` 条目 + 涉及它的 `relations` 边 + 剩余边里的 `knownTo` 引用，三处一并清除（少清一处就是引用悬空）。判定依据 `world_members`（NPC 不在其中，故 **NPC 保留**——NPC 是世界的，不是谁的）。剥离台账落 `redaction_json` 并**对玩家可见**：不能既剥离了又不说剥离了什么。将来「经他人同意带入」应走 `consents` 同意流程，不是在本模块加开关。用例 `red_line_foreign_player_characters_are_redacted_from_snapshot` |
| 🔴 传世卡不得进 if 线（§12） | 主角卡须 `memorial_status='living'` 且 `withdrawn=0`。允许了就是**付费复活** = 付费改命，正是本项最该避免的形态。用例 `red_line_memorial_sealed_character_cannot_open_ifline` |
| 「花资源」= 烧副本卡（§10） | **不新造货币**（§0.5 无提现下多一种货币就多一条 RMT 侧门）。消耗 `MUSE_IFLINE_CARD_COST`（默认 **1**、上限 10）张在手副本卡，走副本卡**既有状态机**：`status='owned' → 'consumed'` 的 CAS，`consumed_into` 指向 if 线 id（反向血缘，与 `cost.subplotCardIds` 互为对账）。🔴 本模块**不 INSERT `subplot_cards`**——铸卡的唯一写入路径仍是 `subplot::grant_card_tx`（§0.2）。副作用是 if 线成为副本卡的**第二个回收口**（第一个是合成升级），对经济体净收缩 |
| 为何「烧」而非「占用」 | 「占用」需要一张绑定表，而 `subplot::synthesize` 的 CAS 只看 `status='owned'`，会把被占用的卡照熔不误 → 「卡熔了、if 线还开着」的白嫖漏洞，堵它必须改 `subplot/`。「烧」天然复用同一个状态机：卡一旦 `consumed`，合成端自动排除它，零跨模块接线 |
| 幂等 | 三层：① `Idempotency-Key`（同一次点击的 HTTP 重试）；② **DB 唯一键** `(owner_id, fork_key)`，`fork_key = {worldId}:{characterId}:{forkPoint}:{forkTickNo}`——换 key 再点也只读回既有那条；③ 副本卡 `status='owned'` 的 **CAS**。抢不到卡 → **整笔回滚**（if 线不留、已烧的卡不留），`409` |
| 审计 | `audit_logs` 落 `ifline.opened`，`subject = ifline:{id}`，reason 含 `origin\|forkPoint\|tick\|revision\|character\|cards\|redactedCharacters\|worldlineChanged=false`，**与建实例、烧卡同一事务** |
| 机审 | 分叉前提走 `safety::moderate_and_queue`（Pending 自动进 `audit_queue` 人审），**在开事务之前**调用（事务内做网络调用会把单连接池锁死）。无论裁决都落库，读取面仅 `approved` 才给正文（否则 `premise:null` + `premiseWithheld:true`） |
| **推进（跑拍，0041）** | 生命周期 `sealed`（已立项未推进）→ `running`（跑过拍）→ `ended`（已收尾）。**玩家拉动，不是调度器推动**：世界按调度器流逝，if 线由买它的人一拍一拍翻页——于是**没有任何调度器会碰 `ifline_worlds`**（`runtime::scheduler_loop` 只扫 `worlds`），付费内容也不会在他没看的时候自己烧完。一拍一行落 `ifline_beats`（**不是 `world_ticks`**），活态另存 `live_state_json` + `live_revision`（CAS 令牌） |
| 🔴 **终局绝不进结算管线**（本批次头号红线） | `progression::settle_*` / `subplot::settle_subplot_card_tx` / `arena_rewards` **一条都不进**。那三条全挂在 `commit_tick → end_world_tx → finalize_ending_tx` 这一条自动链路上，而该链路入口只有一行 `worlds` + 若干 `world_members`——if 线两者都不是，路径在物理上够不着它。推进走 `ifline::runner::commit_beat`，与 `runtime::commit_tick` 零交叉。🔴 **接线时最容易走错的一步**是为复用 `process_tick_inner` 而把 if 线塞回 `worlds`/`world_ticks`：tick 管线与结算管线是**连体的**（CAS 成功即评估终局、终局即结算），没有「跑但不结算」的开关可拨。用例：运行时 `red_line_ifline_ending_grants_nothing`（跑到终局后 `SUM(mileage)` / `subplot_cards` 行数 / `backpacks` / `arena_rewards` / `world_contributions` / `world_ticks` 行数**全部零变化**）+ 源码级 `red_line_runner_never_enters_settlement` |
| 🔴 终局产物 = **内容** | `ifline_beats.prose` 按拍序拼起来的私人传记 + `endingReason`/`endingLabel` 两个**字符串**。终局投影恒带 `isContentOnly:true` / `grantedAssets:[]`，审计 `audit_logs` 落 `ifline.ended`，reason 含 `grantedAssets=none\|settlementEntered=none\|worldlineChanged=false`。🔴 if 线里主角「死了」**不会封卷传世卡**（封卷是 `UPDATE cloud_characters`，属被禁写入）——既不能复活（0039 已挡传世卡入场），也不会杀死你在真实世界线的卡，两个方向都不通才叫平行线 |
| 🔴 成本记在哪 | if 线跑拍烧 token，但**不能写 `world_ticks`**（写进去就等于接回上面那条自动链路）。故：`ifline_beats.cost_tokens`（逐拍实测，共用 `runtime::TokenMeter`，与 `world_ticks.cost_tokens` **口径逐字一致**故可比）+ `ifline_worlds.cost_tokens_total`（实例累计，同事务累加，两处互为对账）+ 运营端点 `GET /api/admin/iflines/cost`。✅ **已并入主看板**（此处原写「尚未并入」，已过期）：`GET /api/admin/metrics/overview` 现有 `cost.ifline`（allTime / window）+ `cost.combined`（世界线 + if 线合计）。🔴 但 `cost.total` 的语义**一个字没改**，仍是世界线口径——把 if 线悄悄加进去会让所有历史对账在同一个字段名下变含义，而看板上看不出发生过这件事。平台总开销读 `cost.combined` |
| 🔴 SLO 归属：**不并入世界线 SLO** | 五项 SLO 度量的是**多人世界线**。基尼（单人样本恒为满分 → **稀释真实的多人不公平，让指标失去报警能力**）/ 无戏份率（单人线结构上不可能有人没戏份）/ 二次入世率（if 线没有「入世」这件事）/ 收尾率（if 线常由拍数上限强制收尾，与「叙事弧完成」不是同一件事）——**四项全部排除**。仅「状态-文本矛盾」同质，故逐拍存 `ifline_beats.critic_json` 供将来做**独立**读数，不并进世界线池子。工程上本就默认排除（`slo/` 取数口径是 `world_ticks.status='done' AND cost_tokens>0`），本批次是把这个默认变成**有意的决定并写下来**。本批次不动 `slo/`。用例 `ifline_beats_never_enter_worldline_slo_input` |
| 成本闸（§0.2 参数化） | `MUSE_IFLINE_MAX_BEATS`（默认 **12**、上限 60）：一张副本卡换一条 if 线，推进无上限则单条算力开销无界。到顶**强制收尾**（`endingReason='beat_cap'`，不调模型、不花 token），玩家拿到的是完整而非断掉的线。另有 `MUSE_IFLINE_BEAT_TOKENS`（单拍预算，默认 40000）、`MUSE_IFLINE_CAST_SIZE`（每拍上场人数，默认 4，clamp 2–5）。**这是成本闸，不是玩法数值，与胜负无关** |
| 🔴 推进时的 §14（纵深防御） | ① 组阵容时剔除原世界 `world_members` 里他人的 `cloud_character_id`（挡「装配格式变化把玩家写进来」）；② 每拍跑前把活态再过一遍 `freeze_snapshot`（挡「`StatePatch` 往 `characters` 塞新键」）；③ 实际上场角色逐拍落 `ifline_beats.cast_json` 可审。NPC 保留。用例 `red_line_foreign_players_never_enter_beat_cast` |
| 同意门为何不卡死 | NPC 走 `world_controlled` 自动放行；**主角走 `approved_consents`**——单人平行线里唯一可能被不可逆结果伤到的人就是主人自己，而**开这条线的动作本身就是同意**（烧卡 + 手写前提）。于是 if 线永不产 `ConsentRequested`，也就永不写 `consent_requests`（被禁写入表之一）。生死档沿用原世界**生效档** `worlds::effective_lethality`（分叉忠实于原世界契约） |
| 🔴 冻结态永不被覆盖 | `snapshot_json` 是**分叉点证据**（「这条线确实从那一拍那份状态岔出去」），推进写的是另一列 `live_state_json`。覆盖了 `stateFidelity` 就变成一句无法证伪的话。用例 `snapshot_stays_frozen_while_live_state_advances` |
| 确定性（推进） | 首次推进钉 `run_seed`（`fnv1a_64` 派生自不可变身份要素，十六进制文本落库，**此后永不改写**）；逐拍子流 `Rng(fnv1a_64(run_seed‖beat_no) ^ 0x5B)`（SplitMix64，域常量 `DOMAIN_IFLINE_CAST=0x5B`，已登记进 `assembly` 的域常量清单，**下一个可用是 0x5C**）；抽样对象**先排序成 Vec 再抽**（禁 map 迭代序驱动 RNG）。⇒ 同分叉态 + 同 `run_seed` + 同 `beat_no` → 同演员表。用例 `cast_selection_is_deterministic_and_seed_sensitive` · `run_seed_is_pinned_on_first_advance_and_never_changes` |
| 终局判定 | 引擎导出的 `muse_engine::narrative::is_terminal`（**与世界线同一把尺**：if 线是一条真的叙事线，不是降级模拟）→ `mainline_done` / `time_cap` / `starved`；另加本模块成本闸 `beat_cap`。恒走 `run_round`（**不走 `run_event_step`**：DES 依赖 `timeline.next_time` 这类世界级调度元数据，而 if 线没有世界时钟在推它） |
| ⚠️ 遗留 | 推进端点在**请求内同步调用模型**，长回合会是长连接请求。生产化应改为「入队 + 后台 worker + 轮询/推送」（`queue` 模块已具备）。本批次未做：if 线默认关闭、状态只标到 `Implemented`，加独立 worker 循环会显著放大改动面且需单独评审 |

### 真人社交解锁（social，migration 0040；总规格 §14【拍板 22】「社交：恨隔面具原则」）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/worlds/{id}/social/bonds` | AuthUser | 我在该世界的社交对端（**面具视图**）+ 资格自查 + 「我们的角色一起死过」凭证 + 解锁状态 |
| POST | `/worlds/{id}/social/unlock-requests` | AuthUser | 发起真人身份解锁 `{targetCharacterId}`；`Idempotency-Key` 可选 |
| GET | `/me/social/unlock-requests?status=` | AuthUser | 我**收到**的解锁请求（默认 `pending`，`all` 出全部） |
| POST | `/me/social/unlock-requests/{id}/respond` | AuthUser | 接受 / 拒绝 `{accept}`（幂等） |
| GET | `/me/social/identities` | AuthUser | 🔴 **全平台唯一下发真人身份的读路径**（只给 `userId` + 昵称） |
| GET/POST | `/me/social/blocks` | AuthUser | 我的黑名单 / 拉黑 `{characterId, worldId?, reason?}` |
| DELETE | `/me/social/blocks/{id}` | AuthUser | 解除拉黑 |
| POST | `/me/social/reports` | AuthUser | 举报 `{subjectKind, subjectId, category, detail?, worldId?}` |
| GET | `/api/admin/social/bond-distribution` | AdminUser(operator) | 🔴 **解锁资格正向分的分布读数**（2026-07-27）。补的是两个工程缺陷：① 审计快照此前只记 `thresholds.minBond`，不记被比较的 `bond` 本身——记了标尺没记读数，事后答不上「我为什么解锁不了」（那条关系的正向分没有任何地方留下过，叙事态是活的）；② `eligibility_json` 一直在写却**没有任何读取面**，没人看得到的审计等于没写。含直方图 + `wouldPassAt`（阈值挪到 X 会有多少条通过——调阈值时唯一想知道的事）。⚠️ 样本只含**发起过解锁请求**的关系对，够不上而没去点的人不在样本里，**分布天然偏高**；旧快照无 `bond` 字段者单列 `legacyWithoutBond`，**绝不按 0 计入直方图**。🔴 只回聚合，**不下发任何真人身份或逐条明细**（§14）；自查视图 `GET /worlds/{id}/social/bonds` 则**始终不给分**——正向分由双方的边共同决定，露给本人即泄露对方的感受 |
| GET | `/admin/social/reports?status=&category=&subjectKind=&cursor=&cursorId=` | AdminUser(reviewer/support) | 举报队列（复合游标，见 §4 说明；漏页 = 漏处置）。三个筛选值走白名单，**未知值 400 而非静默空列表**（空队列会被读成「没有积压」）。末页回 `nextCursor: null`（多取一行判定，口径同 `/admin/risk-events`）。行内含 `handledBy` / `resolution`——没有这两个字段，`status=actioned` 那一屏只看得到「有结论」，看不到结论是什么、谁下的 |
| GET | `/admin/social/reports/summary` | AdminUser(reviewer/support) | 队列形状（只读聚合）：`byStatus` / `byCategory` / `bySubjectKind`（白名单键恒出现，哪怕是 0）/ `oldestPendingCreatedAt` / `escalateAt` / `escalatedSubjectCount`。**不受分页与筛选影响**——界面拿列表页自己数出来的积压是「这一页里有几条」，在安全队列上会被读成「没什么要处理的」 |
| POST | `/admin/social/reports/{id}/resolve` | AdminUser(reviewer/support) | 处置 `{action: actioned\|dismissed, reason}`。🔴 **只改举报单状态**：封禁/下架/改判走各自既有路径（见下「处置边界」） |

**运行时开关 `MUSE_SOCIAL_IDENTITY_UNLOCK`（默认关闭，经 `flags::is_enabled` 解析）。**
关闭时上述端点**全部 404 且零副作用**（不落幂等键、不发通知、不改任何表），
由 `social::tests::red_line_disabled_by_default_all_endpoints_404_and_no_side_effect` 锁死。
运营面（`/admin/social/reports*`）的可见性判据是 `entry_ever_open`——**入口曾对任何人开放过**即放行，
否则同样 404；后台前端把这个 404 渲染成「功能未开启」空态而不是报错（见下「admin 前端」）。

| 项 | 取值 |
|---|---|
| 🔴 未成年保护 | **服务端拒绝**（`ensure_adult_social`，挂每个身份端点第一行、任何读写之前）：只有 `users.age_declared == 1` 放行，未声明(0)/未成年(2)/**用户行缺失**一律 403，口径与 `worlds::join_world` 生死状门逐字一致。**对端未成年同样拒绝**。⚠️ **拉黑/举报不设年龄门**——它们是保护工具，关掉等于让未成年无法自保 |
| 🔴 敌对线永久匿名 | 任一方向 `trust`/`affinity ≤ -MUSE_SOCIAL_HOSTILE_MAX` 或 `fear ≥ MUSE_SOCIAL_HOSTILE_FEAR` → **一票否决**，在任何补偿路径之前，且「一起死过」也不豁免 |
| 解锁门槛 | ①非敌对 ②共历世界数 ≥ `MUSE_SOCIAL_MIN_SHARED_WORLDS` ③两条正向路径任一成立：正向羁绊分 ≥ `MUSE_SOCIAL_UNLOCK_MIN_BOND`（`max(trust,affinity,debt)` 取非负，**两方向取较小者**——单方面好感不算羁绊线）／ 「我们的角色一起死过」（`MUSE_SOCIAL_DEATH_BOND_COUNTS`） |
| 双向自愿 | 发起 → 对方接受。**接受时用当下数据重算资格**（世界线会继续跑，昨天的盟友今天可能已翻脸），发起时的 `eligibility_json` 只作审计、不参与判定 |
| 🔴 「我们的角色一起死过」 | **关系凭证，不是数值**。由 `cloud_characters.memorial_status/memorial_world_id` + `world_members` **只读派生，无任何存储**；三档 `grade`：`both_fell`／`they_fell`／`i_fell`。对历练/卡位/背包/副本卡/贡献账本/结算/引擎决策**一律零影响**（运行时九表快照 + 源码级写入白名单双用例锁死） |
| 拒绝文案 | 未成年 / 被拉黑 / 敌对 / 不够格**全部共用同一句** `REFUSE_GENERIC`——区分原因即把端点变成「探测对方是否未成年 / 是否拉黑了我」的接口 |
| 拉黑实效 | **按 user 判定、按面具录入**（按角色判定会被换卡绕过）。落库同时**撤销**双方 `pending`/`accepted` 解锁 → `revoked`（已授予的身份可见性立即收回）；被拉黑者发不出解锁请求；🔴 **跨通道生效**——`invitations::create_invitation` 前门也调 `social::is_blocked_pair`，且**不看社交开关**（拉黑是保护态，急停不应让它失效，方向同 `MUSE_SAFETY_LEXICON` 的 fail-safe） |
| 终局态 | `declined`/`expired`/`revoked` 均为**终局**，唯一索引 `(world_id, requester_character_id, target_character_id)` 使同一条线只有一行 → **拒绝后不能再问一次**（真人身份是最敏感的一次授予，不给反复施压的空间）。解除拉黑**不恢复**已撤销的解锁 |
| 举报 | 进 `social_reports` 队列（`pending → actioned/dismissed`，CAS + 同事务 `audit_logs('social.report_resolved')`）。同一被举报人 pending 数**恰好**达 `MUSE_SOCIAL_REPORT_ESCALATE_AT` → 写一条 `risk_events(kind='social_report_threshold')` 升级到既有风控面。冷却窗口内重复提交幂等复用既有那条（**不建唯一索引**：唯一即"终身只能举报一次"，会让再犯无法被举报） |
| 🔴 处置边界 | `resolve` **只改举报单自身的状态 + 留痕**，不做任何实处置。封禁走 `POST /admin/users/{id}/ban`（`require_role(support)`）、内容驳回走 `POST /admin/audit-queue/{id}/reject`（`require_role(reviewer)`）、**已过审内容**的再审/下架走 `POST /admin/content/{kind}/{id}/recheck|takedown`（见 §7「已过审内容处置」）、改判走 `POST /admin/appeals/{id}/resolve`，各自带自己的权限与审计。把处置塞进举报接口等于给封禁开一条**绕过既有权限矩阵**的侧门。后台前端因此只做「跳转 + 回填」，不新开写路径 |
| admin 前端 | `admin/src/pages/SocialReports.tsx`（RBAC 模块 `social`，可见角色 reviewer/support/admin，与 `require_report_handler` 逐字对齐）。列表按状态/类别/主体种类筛 + 复合游标翻页 + 详情抽屉 + 复核处置（理由必填 ≤500 字）；处置动作跳 `/users?query=<被举报人>`、`/audit`、`/risk?kind=social_report_threshold` 三个既有入口，且**跳转按钮受前端 RBAC 收敛**（reviewer 看得到举报队列但进不去用户管理，就不该有一个能点的封禁按钮）。开关未开启（端点 404）→ 整页「功能未开启」空态，不报错 |
| 参数化（§0.2） | `MUSE_SOCIAL_UNLOCK_MIN_BOND`(0.6) / `_HOSTILE_MAX`(0.3) / `_HOSTILE_FEAR`(0.5) / `_MIN_SHARED_WORLDS`(1) / `_DEATH_BOND_COUNTS`(on) / `_UNLOCK_DAILY_LIMIT`(3) / `_UNLOCK_TTL_MS`(7d) / `_BLOCK_MAX`(500) / `_REPORT_DAILY_LIMIT`(20) / `_REPORT_COOLDOWN_MS`(24h) / `_REPORT_ESCALATE_AT`(3) / `_PAGE_SIZE`(20) |
| 幂等 | 发起/回应支持 `Idempotency-Key`；DB 侧 `INSERT ... ON CONFLICT(...) DO NOTHING` + 回读权威行（解锁请求与拉黑均如此，杜绝「先查后插」的并发竞态） |
| ⚠️ 运营须知 | 只按 `world` 作用域灰度时，玩家能在该世界发起解锁却读不到 `/me/social/**` 收件箱（后者无 world 坐标）。**推荐灰度作用域是 `user` 或 `global`**，`world` 只用于「临时关掉某个出问题世界的社交入口」这种收窄动作 |

## 5. 计费与商城（billing / shop）

| 方法 | 路径 | 鉴权 | feature | 说明 |
|---|---|---|---|---|
| POST | `/api/billing/orders` | JWT | `billing` | 下单充值。**PaymentProvider 当前为 Dev 桩** |
| GET | `/api/billing/balance` | JWT | `billing` | 余额 |
| POST | `/api/billing/refunds` | JWT | `billing` | 退款 |
| GET | `/api/me/earnings` | JWT | `billing`\|`arena` | 创作者收益查询。**平台内权益，无提现（红线）**。**2026-07-27 新增** `revenueShare`：`platformDefaultBps`（与 `ledger` 同源）+ `myTemplates[].{shareBps, isPlatformDefault}`。🔴 此前只给余额与流水，**不给分成比例**——而 `revenue_share_bps` 按**模板**可覆盖，于是创作者拿着自己模板挣的钱，看不到自己被按什么比例结算，连「这 700 分算得对不对」都无从验证。🔴 「随平台默认浮动」与「给我单独定死」必须分得开（`isPlatformDefault`），合并成一个数创作者就不知道自己的比例稳不稳。🔴 只列**本人拥有**的模板。⚠️ 回的是**当前**比例，不是历史流水各自结算时用的那个 |
| POST | `/api/me/cloud-growth` | JWT | `billing`\|`arena` | 云成长购买 |
| POST | `/api/shop/items/{sku}/purchase` | JWT | `billing`\|`arena` | 平台道具售卖。**永不加战力（红线）** |

## 6. 赛事房与直播（arena / livegate，feature `arena`）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| POST | `/api/arena/{world_id}/host/tick` | JWT + host | 主持人推进一拍 |
| GET | `/api/arena/{world_id}/report` | JWT | 战报 |
| GET | `/api/arena/{world_id}/replay` | JWT | 回放 |
| POST | `/api/arena/{world_id}/revive-match` | JWT | 复活。**实际扣费待接 billing（已知 seam）** |
| POST | `/api/arena/{world_id}/eliminate` | JWT + host | 淘汰（已补同意门控） |
| POST | `/api/arena/{world_id}/settle` | JWT + host | 结算 |
| GET | `/api/arena/gift-skus` | AuthUser | 🔴 **礼物目录**（2026-07-27）：`sku` / `label` / `priceCents` / `boon.{kind,effectTag}`。补的是「在卖一件价格与内容都查不到的东西」——`gift_sku_map` 此前**没有任何读取面**，而站内打赏会照 `priceCents × count` 真扣钱包，观众要**先付钱才知道多少钱**。🔴 只列 `enabled=1`，与扣费路径查表条件逐字相同（列出停用 SKU = 点了被拒而目录说可买）。🔴 §2.5 红线「买过程不买结果」写进响应——买到什么必须写在**付钱之前看得到的地方**。⚠️ 并如实说明礼物**现在不进引擎回合**（open-decisions §5 未拍板），此刻买到的是「被看见」与战报里的一条记录 |
| POST | `/api/arena/{worldId}/gift` | JWT | 礼物投递。boon 记入 `arena_env_events`，**注入引擎回合待 `RoundInput` 扩展（已知 seam）** |
| GET | `/api/arena/{worldId}/clips` | JWT | 高光切片（TtsProvider/切片为 Dev 桩） |
| POST | `/api/livegate/webhook` | 验签 | 直播平台礼物回调。`MUSE_LIVEGATE_SECRET` 未配置时 **fail-closed** |

### 直播场（livestage，migration 0042；总规格 §2 场次节奏三档「直播场」+ §15 第 4 层）

> ⚠️ **不在 `arena` feature 门控内**（模块挂在默认构建里，见 `app.rs`）：延迟缓冲是**内容安全机制**
> （§15 五层漏斗的第 4 层），把它编进可选 feature 等于让默认构建缺一层安全闸；且它只依赖
> `events`/`safety`/`flags`，与 `ledger` 无关。能力本身由**运行时开关** `MUSE_LIVE_STAGE` 控制。
>
> 与既有观战的分工：观战（`/worlds/{id}/events`、`WS /worlds/{id}/stream`、`arena` 战报/回放）
> 已实现且**一行未改**；本模块只补三件它没有的东西——**定档**、**延迟缓冲**、**弹幕**。

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/live/sessions?status=&cursor=&cursorId=&limit=` | AuthUser | 节目单。**只列 `announceAt <= now` 的场次**；复合游标 `(startsAt, id)` |
| GET | `/live/sessions/{id}` | AuthUser | 单场详情。含 `broadcast{delayTicks, publishedThroughTick, worldTickNow, pendingTicks}` |
| GET | `/live/sessions/{id}/feed?cursor=&cursorId=&limit=` | AuthUser | **播出面**（延迟缓冲后的公开事件）。复合游标 `(sequence, id)`；副作用：记一行观众足迹 |
| POST | `/live/sessions/{id}/danmaku` | AuthUser | 发弹幕 `{body}`。🔴 成年门 + 限频(429) + 审核链；仅 `approved` 才经 `WsHub` 外发 |
| GET | `/live/sessions/{id}/danmaku?anchorTick=&cursor=&cursorId=` | AuthUser | 弹幕列表（只出过审）。复合游标 `(createdAt, id)` |
| POST | `/admin/live/sessions` | AdminUser(operator) | 定档 `{worldId, startsAt, announceAt?, endsAt?, title?, delayTicks?, capacity?}` |
| POST | `/admin/live/sessions/{id}` | AdminUser(operator) | 状态迁移 `{status}` / **延迟拍数调整** `{delayTicks}` + `{reason}` |
| POST | `/admin/live/sessions/{id}/withhold` | AdminUser(reviewer) | 缓冲窗口内把一条**从本场播出面**撤下 `{eventId, reason?}` |

**运行时开关 `MUSE_LIVE_STAGE`（默认关闭，经 `flags::is_enabled` 解析）。**
关闭时上述 8 个端点**全部 404 且零副作用**（不记观众足迹、不落弹幕、不建场次、不写审计），
由 `livestage::tests::red_line_disabled_by_default_every_endpoint_404_with_zero_side_effect` 锁死。

| 项 | 取值 |
|---|---|
| ① 定档 | `live_sessions`：预告时刻 `announce_at` + 开播时刻 `starts_at` + 容量。状态机**单向**：`scheduled → live \| canceled`，`live → ended`，终局不可回头（CAS 落库，并发下只有一个请求能完成迁移）。节目单只列已到预告时刻的场次 |
| ② 延迟缓冲 | 🔴 **是内容安全机制不是体验设计**。播出边界 = `max(最新 done 拍 - delayTicks, publishedThroughTick)`；`scheduled`/`canceled` 一拍不播；`ended` 且过了 `MUSE_LIVE_DRAIN_GRACE_MS` 放尾拍（否则最后 N 拍永远卡在缓冲里） |
| 🔴 待播内容存在哪 | **存在它本来就在的地方**（`world_events`），**不建任何副本表**。一拍提交时事实已落定（§0.3），延迟的是**公开投影的播出时刻**，不是事实。建副本表会立刻产生两个事实源——那才是事实错乱 |
| 🔴 已播出不缩回 | `live_sessions.published_high_tick` 是单调下界：**上调 `delayTicks` 只勒住未来**，已在观众屏幕上滚过去的拍不会消失。用例 `raising_delay_ticks_never_retracts_already_published_ticks` |
| 🔴 世界内不延迟 | 延迟只作用于世界**外**的观众；成员的 `/worlds/{id}/events` 一拍不延——延后当事人等于让世界停摆。用例 `delay_buffer_holds_recent_ticks_but_never_delays_world_members` |
| 🔴 审核不过怎么处理 | **不外发 ≠ 回滚**。①自动：§15 第 2 层落库前打码置 `pending`，第 3 层异步复核在缓冲期内收紧 `moderation`——播出面只出 `approved`，与观战/回放同口径；②人工：`withhold` 写 `live_withholds` **独立表**，`world_events` **逐字节不动**，战报/回放/日报/成员读取面全不受影响 |
| `preemptive` 标注 | 撤下如实记 `preemptive`：`true`=播出前拦下（缓冲生效，观众从未看见）／`false`=播出后撤下（**收不回已经看见的**，不假装能撤回）。`withheldPreemptiveRate` < 1 即「延迟拍数配得不够」的直接证据 |
| ③ 弹幕 | 过 `safety::mask`（就地打码）+ `safety::moderate_and_queue`（**传原文**——`***` 会把注入句式抹平，等于用第 2 层蒙住第 3 层的眼睛；落库仍是打码版）。🔴 **词库命中 → 打码后仍不外发，置 `pending` 转人审**（弹幕是实时公开发言面，拦一条零代价）。非 `approved` 落库但读取面不出 |
| 🔴 弹幕不是世界事实 | **永不进 `world_events`**（源码级用例 `red_line_module_never_writes_world_events`），因而不进战报/回放/日报/`RoundInput`。观众的一句话不会变成世界里发生过的事，也不影响任何角色决策 |
| 🔴 弹幕锚定播出拍 | `anchor_tick` 由**服务端按播出水位线**计算，**不接受客户端传值**——否则观众可把弹幕锚到尚未播出的拍上，等于替世界剧透。这是「时间差不造成事实错乱」的关键一步，回放按它与画面对齐 |
| 🔴 未成年保护 | `ensure_adult_live` 挂发弹幕端点第一行（`ensure_enabled` 之后、任何读写之前，403 发生在**零副作用**位置）：只有 `users.age_declared == 1` 放行，未声明(0)/未成年(2)/用户行缺失一律 403，口径与 `social::ensure_adult_social` / `worlds::join_world` 生死状门逐字一致。⚠️ **只挡写不挡看**——未成年可观看直播（观战本就开放），年龄门挡的是新增的公开发言面 |
| 面具（§14） | 弹幕响应体只出 `displayName`（`观众xxxx` = `sha256(sessionId:userId)` 前 4 位），**无 `userId`、无昵称、无手机号**。同场稳定、跨场不可关联 |
| 限频 | `MUSE_LIVE_DANMAKU_RATE_PER_WINDOW` 条 / `MUSE_LIVE_DANMAKU_WINDOW_MS` 毫秒 → 超限 **429**（新增 `ApiError::TooManyRequests`；409 是"状态不允许"，429 是"太快了"，客户端的正确反应不同）。🔴 **被拒的弹幕照样计数**，否则可靠发违规内容白嫖额度 |
| ④ 转化度量 | `live_viewers` 是 VALIDATION §2 T5 门槛「观众→玩家转化 ≥2%」的**唯一数据源**（此前全仓无任何直播观看埋点）。聚合挂在 `GET /admin/metrics/overview` 的 **`liveStage`** 顶层键，口径见下表 |
| 参数化（§0.2） | `MUSE_LIVE_DELAY_TICKS`(2，规格 §15「1-2 拍」取上限) / `_ANNOUNCE_LEAD_MS`(1h) / `_SESSION_CAPACITY`(0=不限) / `_DRAIN_GRACE_MS`(5min) / `_DANMAKU_RATE_PER_WINDOW`(20) / `_DANMAKU_WINDOW_MS`(60s) / `_DANMAKU_MAX_LEN`(80) / `_CONVERSION_MIN`(0.02) |
| 🔴 与错峰调度解耦 | `runtime::offpeak` 的「直播场（`room_type='arena'` ∨ `tick_per_day ≥ MUSE_OFFPEAK_LIVE_TICK_PER_DAY`）**永不延后**」红线**一行未改**。豁免判据必须是世界自身的节奏属性，**不是「有没有定档记录」**——否则运营建一条定档就顺手改掉一个世界的调度行为。播出排期（`live_sessions`）与引擎拍排期（`schedule_due_ticks`）输入完全不相交，双向源码级用例 `red_line_offpeak_live_exemption_untouched` 钉住 |
| ⚠️ 运营须知 | 只按 `world` 作用域灰度时，观众能进那一场却在**节目单**里看不到它（节目单跨世界，无 world 坐标）。**推荐灰度作用域是 `user` 或 `global`**，`world` 只用于「临时关掉某个出问题世界的直播入口」这种收窄动作 |

**「直播场观众→玩家转化率」口径**（`GET /api/admin/metrics/overview` → `liveStage`，operator/finance；
窗口与 `narrativeSlo` 同一把尺 = `?sloDays=`，默认 30、UTC 日界；`?slo=0` 一并跳过）：

| 项 | 定义 |
|---|---|
| 分母 | 窗口内**首次观看**直播、且**当时还不是玩家**的人数，按 `userId` 去重（取 `MIN(first_seen_at)`）。🔴 `was_player` **在首次观看那一刻冻结**，绝不在统计时现算——现算的话，看完就入场的人会因为"现在是玩家了"被移出分母，分子分母一起缩水 |
| 分子 | 其中在**首次观看之后**且仍在窗口内入场的人数（`world_members.joined_at > 首次观看时刻`）。🔴 `>` 这个**严格**方向是要害：先入场后看直播不是转化，把它算进分子等于把留存记成拉新 |
| 恒等式 | 分子的过滤是分母的**子集条件**（同一张派生表上加 `EXISTS`），故 分子 ≤ 分母 恒成立，不会出现 >100% 的转化率 |
| 三态 | `entry_not_open`（`MUSE_LIVE_STAGE` 从未对任何人开放，value=null，显示 `—`）/ `no_data_in_window`（入口开着但窗口内零新观众，value=null，`—`）/ `ok`（真数，**可以是 0.0**）。🔴 三者不可混同：本功能默认关闭，若直接报 0% 会得到「一个看起来糟透了、实际上什么都没测的数」，而 T5 恰恰要拿它决定继续/调整/停止。入口判定走 `livestage::entry_ever_open`（覆盖按世界/按用户灰度，fail-safe 方向是「没开过」） |
| 门槛方向 | `thresholdMin` = `MUSE_LIVE_CONVERSION_MIN`，默认 **0.02**（T5 原文「≥2%」，作为默认值而非常量语义）。⚠️ 这是**下限**门槛（`belowThreshold` = 未达标），与基尼那种上限门槛方向相反，别把比较符抄串 |
| 辅助数 | `sessionsInWindow` · `danmakuTotal` / `danmakuBlocked`（弹幕审核负担，供 T5 门槛「内容审核成本 ≤ 生成成本的 5%」参考）· `withheldTotal` / `withheldPreemptive` / **`withheldPreemptiveRate`**（延迟缓冲的**有效性**度量：< 1 = 有内容已播出才被撤下 = 延迟拍数配得不够，即 T5 预案「上调延迟拍数」的判据；一条撤下都没有 → null，"没发生过"不是"0% 拦住了"） |
| 埋点位置 | 只在观众**真正拉取播出面**（`GET /live/sessions/{id}/feed`）时记一行，「打开了节目单」不算观看。幂等 upsert（唯一键 `(session_id, user_id)`），随后刷新 `last_seen_at` |

## 7. 运营后台（admin_api）

后台前端见 `admin/`（端口 1430）。全部端点需管理员 token；`require_role` 语义：`admin` 角色恒通过，
其余按下表。**写操作统一落 `audit_logs`。**

| 方法 | 路径 | 角色 | 说明 |
|---|---|---|---|
| POST | `/api/admin/dev-login` | 公开 | **仅 `MUSE_DEV=1`**，生产直接 403。密钥 `MUSE_ADMIN_DEV_SECRET`（默认 `muse-dev-admin`） |
| GET | `/api/admin/users` | support | 用户检索 |
| POST | `/api/admin/users/{id}/ban`·`/unban` | support | 封禁/解封 |
| GET | `/api/admin/audit-queue` | reviewer | 审核队列 |
| GET | `/api/admin/audit-queue/{id}` | reviewer | 审核详情 |
| POST | `/api/admin/audit-queue/{id}/approve`·`/reject` | reviewer | 审核裁定（回写主体 moderation）。`world_event` 主体走专用回写，见下「运行时世界事件的裁决回写」|
| POST | `/api/admin/audit-queue/{id}/reinstate` | **admin 专属** | 推翻**人审终判**、放行一条运行时世界事件。body `{reason}` 必填 1-500 字。只受理 `world_event` 主体（其余主体的改判走 `/admin/appeals/{id}/resolve` 或 `/admin/content/{kind}/{id}/restore`）|
| GET | `/api/admin/appeals` | reviewer | 申诉列表 |
| POST | `/api/admin/appeals/{id}/resolve` | reviewer | 申诉复审（overturn/uphold，**唯一改判路径**） |
| GET | `/api/admin/content/takedowns?state=&kind=&cursor=&limit=` | reviewer | 已过审内容处置台账（复合游标 `createdAt:id`）。`state` ∈ `restricted`/`removed`/`restored`/`all`；未知 `state`/`kind` → 400（空列表会被读成「这类内容没被处置过」）|
| GET | `/api/admin/content/{kind}/{id}` | reviewer | 单主体处置态 + `affectedRunningWorlds`（该主体仍在哪些运行中的世界里）|
| POST | `/api/admin/content/{kind}/{id}/recheck` | reviewer | **再审**：把已过审内容送回人审队列，**不改展示态**。四类主体均可（文本走 `queue_operator_recheck`、位图走 `queue_operator_recheck_image`，同为入队/记险入口③）；同主体已有 `open` 队列行时幂等复用（`created:false`）|
| POST | `/api/admin/content/{kind}/{id}/takedown` | reviewer；`permanent:true` 为 **admin 专属** | **下架**：展示态列置 `'takedown'`。默认 `restricted`（可恢复）；`permanent:true` → `removed`（不可恢复，位图主体连对象字节一并删除）|
| POST | `/api/admin/content/{kind}/{id}/restore` | reviewer | **恢复**：写回台账里的 `prevModeration`。仅 `restricted` 可恢复，`removed` 恒 409 |
| GET | `/api/admin/content/appeals?status=&kind=&cursor=&limit=` | reviewer | **处置申诉**队列（复合游标 `createdAt:id`）。`status` ∈ `pending`/`upheld`/`overturned`/`all`。每条附 `disposal` 段（处置台账全行，**含运营内部理由**——后台面才有）|
| POST | `/api/admin/content/appeals/{id}/resolve` | reviewer | 处置申诉裁决 `{decision: uphold\|overturn, reason}`。`overturn` **走 `restore` 那一段实现**（写回 `prevModeration` + 台账翻 `restored`），不直接写 `approved`。`reason` 是**写给作者的答复**，会回显 |

### 运行时世界事件的裁决回写（`world_event` 主体，migration 0047）

§15 第 2 层（词库高危命中）与第 3 层（语义复核）把运行时投影事件送进 `audit_queue`
（`subject_kind='world_event'`），但此前 `admin_api::audit` 对该主体**没有回写分支**——人审点
「通过」是一次静默空操作，事件永久停在 `pending`。第 3 层 fail-closed（provider 每抖动一次就收紧
一批并无条件入队）把这个缺口放大成了运营侧的实际风险。实现 `server/src/admin_api/audit.rs`，
后台在内容审核 → 审核队列的详情抽屉。

| 约束 | 口径 |
|---|---|
| 🔴 §0.3 正文零改写 | 回写只改 `world_events.moderation` 一列（**可见性**），事件正文（`public_projection_json` / `private_projections_json` / `arbiter_note`）一个字节不动。回执 `bodyRewritten:false` 自述，用例逐字节快照（含零宽符 / BOM）守死 |
| 🔴 写入路径盘点 | `world_events` 全仓只有 3 条 `UPDATE`：第 3 层机器棘轮、人审驳回收紧（两条**都从 `'approved'` 出发**）、以及**唯一一条放宽**。放宽形状 `SET moderation = 'approved' WHERE id = $1 AND moderation = $2 AND moderation IN ('pending','rejected')`——SET 写字面量（方向不由绑定值决定）、按主键点名一行、CAS 到读到的当前态、起点**白名单**写死在 SQL（不用 `<> 'approved'` 黑名单：NULL 下静默命中 0 行，且会随将来新增的哨兵值失效）。源码级红线用例 `red_line_world_events_has_one_ratchet_and_one_guarded_relax` 全仓扫描，多一条即红 |
| 🔴 定位坐标 | `subject_id` 存的是 `domain_event_id`，而引擎按 `patch-{base_revision}-ev-{seq}` 生成它——**确定性、不含世界维度**，两个世界在同一 revision 上逐字重名。故 0047 给 `audit_queue` 加 `subject_world_id`，回写先按 `(world_id, domain_event_id)` 定位主键再改。存量行该列为 NULL → 退化为全库定位，**命中多于一行即 409，绝不猜** |
| 🔴 权限两档 | 口径抄 0044（`restricted` reviewer 可逆 / `removed` admin 专属）。`approve` = 推翻**机器**收紧，`reviewer`；被人审驳回过的事件 `approve` 恒 409，只能走 `reinstate`（**admin 专属 + 理由必填**）。两档不共用一个按钮 |
| 判据 | 「机器收紧 vs 人工驳回」直接读 `audit_queue.status`，**不新增 provenance 列**：机器入队只写 `'open'`，只有人审裁决会写 `'approved'`/`'rejected'`。⚠️ NULL 安全：存量行 `subject_world_id IS NULL` 时判据**算作命中**（fail-closed，逼它走 admin 档），写成 `(subject_world_id IS NULL OR subject_world_id = $3)` |
| reject 方向 | 事件当前 `'approved'`（被另一条队列行放行过）→ 收紧为 `'rejected'`；当前已是 `pending`/`rejected` → **不改数据面**（机器入队前必定已收紧，两态在所有读取面上等价）。人审终判落在队列行 `status='rejected'` 上，它**使该事件从此不能再由 reviewer 档放行**——有实际效力，不是空操作。回执 `tightened` + 真实 `moderation` 如实说明 |
| reinstate 不抹判据 | 已是 `rejected` 的队列行，`reinstate` **不改它的 status**：那一行正是 tier-2 台阶的判据，覆盖它等于亲手拆掉台阶，也会抹掉「有人驳回过」这段处置历史 |
| 留痕 | `audit_logs`（`audit.approved` / `audit.rejected` / `audit.world_event_reinstate`）+ `risk_events`（`world_event_moderation`，经 `safety::record_risk_tx`，本模块不直写）。三段副作用（`world_events` / `audit_queue` / 留痕）**同一事务** |
| 人审不盲审 | `GET /admin/audit-queue/{id}` 对该主体附 `subjectEvent`（事件正文 + 世界/拍/序号 + `humanRejectedBefore`），理由与位图主体的 `subjectImageUrl` 逐字相同。定位不到时给 `subjectEvent.unresolved` 如实说明，队列行仍打得开 |
| 状态语言 | **Implemented**（§0.3 七档）。回写路径与两档权限已实现并被用例覆盖；尚未在真实运营流程上验证 |

### 已过审内容处置（migration 0044）

`audit-queue` 的 approve/reject 只作用于**仍在人审队列里**的条目；本组端点作用于**已经在线上**的内容
（举报队列指过来的那条路径）。实现 `server/src/admin_api/takedown.rs`，后台页面在
`admin/src/components/ContentDisposal.tsx`（内容审核 → 已过审内容处置，深链
`/audit?tab=disposal&kind=&subject=`）。

`kind` 白名单四项，各自落在既有的**展示态列**上——处置写入一个非 `'approved'` 值，
四个既有闸门（判「等于 approved」）随之自动关闭，无需给任何读取面新增过滤条件：

| kind | 表.列 | 关掉的既有闸门 |
|---|---|---|
| `character` | `cloud_characters.moderation` | `join` 409 `character_not_approved`；邀请接受同判；**卡名解引用闸门**（roster / 遗作馆 / 悼念名单 / 社交与邀请对手方名，见下「卡名读取面闸门」）|
| `character_avatar` | `cloud_characters.avatar_moderation` | `CharacterView` / world roster / backpack / 遗作馆 **四处**「仅 approved 才下发 avatarUrl」（遗作馆那处是后补的：0016 立规矩时它还不存在，此前无条件下发）|
| `world_cover` | `worlds.cover_moderation` | `worlds::visible_cover_url`（大厅 / 世界详情 / 后台世界列表唯一闸门）|
| `world_template` | `world_templates.moderation` | `create_room` 409 `template_not_approved`；assembly 蓝图解引用同判 |

| 约束 | 口径 |
|---|---|
| 🔴 边界 | 处置的是**展示面**，不是已发生的世界事实。`world_events` / `world_ticks` / `world_members` / `world_contributions` / `world_biographies` **一个字节不改**（§0.3 公共事实不可回滚），回执 `worldlineUntouched:true` 自述，用例逐字节快照守死 |
| 🔴 运行中的世界 | **不受影响**：入场闸只在入场时判一次，被下架的卡会继续参演存量世界。回执与后台界面直接列出 `affectedRunningWorlds`，中止请走既有 `POST /admin/worlds/{id}/pause`；本端点**不代做强制离场**（那要改世界线相关表，需红线评审）|
| 哨兵值 | `'takedown'`，**不复用 `'rejected'`**——后者是「发布时被驳回」的语义，复用会被 `POST /admin/appeals/{id}/resolve` 的改判路径悄悄翻转，且从此分不清「从未过审」与「过审后被下架」|
| 队列不得复活 | `POST /admin/audit-queue/{id}/approve` 的回写带 `<> 'takedown'` 守卫（位图两列可空，故写成 NULL 安全形式）：已下架主体不得经人审队列绕过 `restore` 的权限与可逆性台阶复活。**reject 方向不设守卫**（驳回是更强的处置，应当落地）|
| 再审可用范围 | **四类主体全覆盖**。文本主体送 `card_json` / `skeleton_json` 的机审拼接文本；位图主体送对象存储里的那份字节走 `check_image`，人审详情附 `subjectImageUrl` 供查看（否则只能盲审）。字节取不到 → 409 如实告知，不排一条打不开图的队。🔴 位图入队与 `audit::review` 的两条位图回写分支是**一对**，缺任一半即 migration 0027 警告过的「无法被改判的死队列项」|
| 留痕 | `audit_logs`（`content.takedown` / `content.takedown_permanent` / `content.restore` / `content.recheck`）+ `risk_events`（`content_disposal` / `content_recheck`，经 `safety` 入口，本模块不直写）。下架/恢复与留痕**同事务** |
| 作者告知 | `GET /api/assets/characters/{id}/status` 增 `takedown` 字段（`state`/`reversible`/`takenDownAt`/固定 `notice`）。🔴 **不回显运营内部处置理由**（口径同 `audit_logs.reason`；对比人审驳回理由走的是专为回显而设的 `audit_queue.reject_reason`）。已恢复 → 不下发 |
| 运营开关 | **处置能力无开关**（合规设施，定位同 `MUSE_SAFETY_LEXICON`——一个能被关掉的下架入口，在需要它的那一刻恰好可能是关的）。⚠️ **卡名读取面闸门另有开关且默认关闭**，两者不是一回事，见下 |
| 作者申诉 | `POST /api/assets/characters/{id}/disposal-appeal`（migration 0045），见下「处置申诉」|

### 卡名读取面闸门（`MUSE_DISPOSAL_NAME_GATE`，**默认关闭**）

上表 `character` 那一行的既有闸门只管「进新世界」。roster / 遗作馆 / 悼念名单 / 社交与邀请的对手方名
都是拿着 `card_json` **现读现解**出一个名字，从来不看审核态——于是下架一张卡断得掉入场与立绘，
断不掉它在**存量世界里已经露出的名字**。实现 `server/src/safety/disposal.rs`。

| 项 | 口径 |
|---|---|
| 🔴 边界 | 闸门只作用于「现在去读卡拿名字」的展示面。`world_events` 正文、`world_biographies.summary_json` 封卷快照是**已落定的事实**，一个字节不改（§0.3）。判据：关掉闸门这段文字会不会变——会变的才归它管 |
| 接了哪些 | world roster（`GET /worlds/{id}`）· 同源冲突 409 文案 · 遗作馆四处（馆列表 / 传世卡详情的 `name` 与整段 `identity` / 悼念名单 / 我的悼念）· `social` 与 `invitations` 的对手方角色名 |
| 刻意没接 | **引擎输入**（`assembly::load_active_cards`、`runtime` 的 `other_cards_brief`、`ifline`）——改它等于改运行中世界的叙事，是产品决策且会让黄金世界回归对不上；**已封卷快照**（`GET /worlds/{id}/biography`）——§0.3；**后台审核面**——人审要看的正是真名与全文；**作者自查面**（`GET /me/memberships` 是自己看自己）——替代文本是为了不把名字摆给**别人**看 |
| 🔴 为什么有开关 | 打开它会改变**运行中世界**对玩家的显示（昨天还在的名字今天变成占位）。那是产品决策（什么时候开、开了给玩家看什么），不是工程能自作主张的事。故按 §0.1 登记进 `flags` 体系、默认关闭；关闭时各读取面输出与本闸门存在之前**逐字节一致** |
| 替代文本 | `暂不可见的角色·<4 位十六进制>`，前缀参数化 `MUSE_DISPOSAL_DISPLAY_NAME`。判别位是主体 id 的 FNV-1a 折叠——**稳定**（无时间无随机）、**可区分**（同一列表上两张被处置的卡不塌成一个人）、**不泄露新信息**（这些响应体本就明文带着 `cloudCharacterId`）。刻意中性：它渲染在**第三方**页面上，平台不借占位符对被处置者做公开定性 |
| 只认哨兵值 | 仅 `'takedown'` 触发。发布期的 `pending` / `rejected` 不归它管——那条路上的内容从来没在读取面露过面，混起来会让「从未过审」与「过审后被下架」在展示上分不清 |

### 处置申诉（migration 0045）

| | 发布期申诉 `/api/admin/appeals`（0018） | **处置申诉** `/api/admin/content/appeals`（0045） |
|---|---|---|
| 受理 | `rejected`（发布时被驳回） | `restricted` / `removed`（过审后被处置） |
| 次数 | 每**主体**终身一次 | 每**次处置**一次（恢复后再被下架 = 新的一次，申诉权重开） |
| 改判动作 | 直接写 `moderation='approved'` | 走 `restore` 那一段实现：写回 `prevModeration` + 台账翻 `restored` + 审计 + 风控 |

**为什么不合表**：`content_takedowns` 每主体只留当前一行（重复下架 ON CONFLICT 覆盖、`id` 不变、
`created_at` 刷新），故「哪一次处置」的标识是 `(takedownId, disposalAt)` 这一对，与 0018 的主体键
不是一个形状。若把处置申诉塞进 0018 并把唯一索引扩成含可空的 `takedown_ref`，两个库的唯一索引
**都不认为两个 NULL 相等**，0018「终身一次」的保证会在扩索引那一刻无声失效。更要命的是 0018 的
`overturn` 直接写 `approved`——那正是 0044 给 `audit::review` 加守卫要防的洞（绕过恢复台阶，
且台账还留着一条自称仍在下架的记录）。

| 端点 | 鉴权 | 说明 |
|---|---|---|
| POST `/api/assets/characters/{id}/disposal-appeal` `{text, subjectKind?}` | JWT owner | `subjectKind` ∈ `character`（默认）/ `character_avatar`。非 owner → 404（不泄露存在性）；无生效中的处置 → 400；同一次处置重复提交 → 409。**提交不改任何 `moderation`** |
| GET `/api/assets/characters/{id}/status` | JWT owner | 增 `disposalAppeal`（最近一条）。与 `takedown` **并列**而非嵌套：处置被恢复后 `takedown` 不再下发，申诉结论必须留在原地 |

🔴 **两种"理由"不得互相顶替**：`content_takedowns.reason` 是运营内部处置备注，**作者侧一个字都不下发**
（口径同 `audit_logs.reason`）；`disposal_appeals.resolution_reason` 是复审人**写给作者**的答复，会回显。
后台队列的 `disposal` 段带前者，作者侧的 `disposalAppeal` 不带。

**`removed` 的救济边界**：可以申诉（作者有权提异议、运营有义务答复），但 `overturn` 恒 409——
永久移除不可逆（位图连对象字节一并删除），口径与 `restore` 逐字一致，不给不可逆开申诉侧门；
重新上线的路径是作者重新发布。若运营在裁决前已自行恢复，`overturn` 如实记 `overturned` 且不再动状态
（`restored:false` + `alreadyRestored:true`）——报 409 只会留下一条永远卡在 `pending` 的僵尸行。
| GET | `/api/admin/worlds` | operator | 世界列表。含 `participantCount`、`successRate`（**0..1 小数**，无已终结 tick 时为 null）、`todayTokens`/`todayCostCents`/`todayCostCny`。**2026-07-27 新增** `lastActivityAt` = `MAX(world_ticks.finished_at)`（最后一拍**跑完**的时刻）。🔴 刻意不用 `worlds.updated_at`：那一列任何一次写世界行都会动（暂停、改预算……），会把**运营自己的操作**记成世界活动，早就停摆的世界永远看起来很新鲜。从未跑完过任何一拍 → `null`，不是 0 |
| POST | `/api/admin/worlds` | operator | 官方建房。可选 `cover`（一次建完带图）· 可选 `series: {maxInstances}`（**登记为世界系列 1 号实例**，migration 0035；开关未开时 400，见 §3「世界系列自动扩容」）|
| GET | `/api/admin/worlds/{id}/diagnostics?limit=&sinceMs=` | operator | 脱敏诊断（采样种子不外泄）。`budget` 含金额换算与用量比：`spentCny`/`dailyCnyBudget`/`usageRatio`（**0..1**，取 token 与 cny 两维较大者）/`spentTokensTodayEffective`（跨日已归零）。另含 `series`（系列队列态；**不受 env 开关门控**——关阀时运营更需要看得见队列，开关状态另作 `autoscaleEnabled` 明示）与 `beBiography`（崩塌封卷元信息，正文另走玩家读取面）。**2026-07-27 补齐设计文档 §9.1 的四项空态**：`world.startedAt`（= `MIN(world_ticks.created_at)`，**首拍排期时刻**，不是建房时刻；没排过拍 → `null`，不回落 `createdAt`）· `riskEventDaily.{today,yesterday,delta}`（UTC 日界，🔴 **不给环比百分比**——昨日可能为 0，0 做分母不是「涨了无穷」是「没有可比基数」）· `promptSet.{version,createdAt,activatedBy,activatedAt}`（🔴 `prompt_versions` 上没有 `updated_by` 列，`activatedBy/At` 来自 `audit_logs` 的 `prompt.activate`；从未经端点激活过 → `null`，**不猜**）· 取数窗口 `?limit=`（默认 **10，与加参数之前逐字节一致**，上限 500）与 `?sinceMs=`（按 `created_at` 收窄）|
| POST | `/api/admin/worlds/{id}/pause`·`/resume` | operator | 暂停/恢复（需审计理由） |
| GET | `/api/admin/world-templates?sagaId=` | operator, reviewer | 模板列表。带 `sagaId` 时切换为**阶段列表**语义：只返回该世界系列，按 `stage_no` 升序（剧情顺序）且不分页 |
| POST | `/api/admin/world-templates` | operator | 建模板。可选 `sagaId` + `stageNo`（总规格 §3 Saga 归组），二者必须成对，`stageNo` ∈ 1-999；都不传 = 独立模板。骨架**任一已登记层**（`assembly::SKELETON_KEY_SETS`，32 条路径覆盖全部嵌套结构）出现无人读取的键（拼错 / 残留）→ 400，报**完整路径**（形如 `payoutTable.worldlineTiers[见证].itm`）并带编辑距离最近的「是不是想写 X？」。同一道校验也作用于创作者发布 `POST /assets/worlds` |
| POST | `/api/admin/world-templates/{id}/star` | operator | 星级 curation（**3-5★ 唯一晋升路径**） |
| GET | `/api/admin/sagas` | operator | 人工校准：阶段切分总览（每系列的阶段数 / 缺号 / 重号 / 未编号 / 审核态 / 星级跨度 / 世界数）。见下方「人工校准面」 |
| GET | `/api/admin/sagas/{sagaId}` | operator | 人工校准：单系列逐阶段结构（按 `stage_no` 升序 = 剧情顺序）+ 每阶段骨架形状指标。系列不存在 → 404 |
| GET | `/api/admin/identity-pools` | operator | 人工校准：声明了 `identityPool` 的模板目录（未声明者不列出） |
| GET | `/api/admin/world-templates/{id}/identity-pool?limit=` | operator | 人工校准：身份池声明 + 实际分配分布（`limit` 为扫描世界数，默认 100，clamp [1,500]） |
| GET | `/api/admin/realm-tiers` | operator | 人工校准：声明了 `realmTier`（境界档）的模板目录。另给 `undeclaredInSagaCount`（系列里缺戏服的阶段 = 校准缺口）/ `undeclaredStandaloneCount`（独立模板，只作对照） |
| GET | `/api/admin/world-templates/{id}/realm-tier?limit=` | operator | 人工校准：境界档声明 + 同系列各阶对照 + 实例钉住情况（`limit` 同上） |
| GET | `/api/admin/economy/overview` | finance | 经济只读聚合 |
| GET | `/api/admin/ledger/reconcile` | finance | 全账复式恒等 SUM=0 + 物化余额对账（只读，无提现） |
| GET | `/api/admin/metrics/overview?costDays=` | operator, finance | 数据看板。含 `cost` 对象：`today`（今日 token/分/元）、`trend[]`（近 N 日，默认 7，clamp [1,60]，每项另含 `offPeakTokens`）、`byWorld[]`（每局 Top10 含 `tokensPerPlayer`）、`total`、`centsPer1kTokens`、**`offPeak`**（错峰调度仪表：占比 / 估算折让 / 延后时长 / 档位分桶，字段与单位见 §3「错峰调度」小节）。**每玩家成本口径为人均等分**（`world_ticks` 是整拍口径、无 per-member 分解），局限见响应 `notes`。另含顶层键 **`liveStage`**（直播场观众→玩家转化率，T5 门槛；三态 `entry_not_open`/`no_data_in_window`/`ok`，口径见 §6「直播场」小节；`?slo=0` 一并跳过） |
| GET | `/api/admin/metrics/trends` | operator, finance | 按天趋势（UTC 日界） |
| GET | `/api/admin/prompts` | operator | Prompt 版本列表 |
| POST | `/api/admin/prompts` | **admin 专属** | 建 Prompt 版本 |
| POST | `/api/admin/prompts/{id}/activate`·`/canary` | **admin 专属** | 激活 / 灰度 |
| GET | `/api/admin/model-routes` | operator | 模型路由列表 |
| POST | `/api/admin/model-routes` | **admin 专属** | 建路由版本 |
| POST | `/api/admin/model-routes/{id}/activate` | **admin 专属** | 激活（**一键回滚 = 激活旧版本**） |
| GET | `/api/admin/flags?flag=` | operator | 运行时开关：登记表（`defaultEnabled`/`owner`/`wired`）+ 全部记录 + 每个开关的 `globalEffective`（大盘生效值及其来源） |
| GET | `/api/admin/flags/resolve?flag=&userId=&worldId=` | operator | **dry-run**：该开关对这个人/这个世界解析成什么、来自哪一层（`db`/`env`/`default`/`failClosed`），含被时间窗跳过的记录。复盘主力工具 |
| POST | `/api/admin/flags` | **admin 专属** | 设置一条（upsert，唯一键 `flag+scope+targetId`）。见下方「运行时开关」小节 |
| DELETE | `/api/admin/flags/{id}?reason=` | **admin 专属** | 删除一条 = 该目标**回落到更宽作用域 / env**（不是强制关闭）。回执 `fallsBackTo` 直接告知删完变成什么 |
| GET | `/api/admin/ooc-appeals?status=` | **reviewer** | OOC 申诉复核队列（migration 0037）。见 §4「OOC 注解权」 |
| POST | `/api/admin/ooc-appeals/{id}/review` | **reviewer** | OOC 申诉复核（改判类，与内容风控申诉同档）。确认模型错误 → 补偿托梦配额；落 `audit_logs` |
| GET | `/api/admin/risk-events` | operator, reviewer, support | 风控事件 |
| GET | `/api/admin/safety/recheck?since=&until=` | operator | §15 **第 3 层**语义分类异步复核的运行台账 + 成本读数（migration 0046；实现在 `safety::semantic`，由 `admin_api::router()` 聚合挂载）。字段见下方「内容安全第 3 层」小节。🔴 响应恒带 `providerStub` / `source` / `honesty[]` —— 当前 provider 是 Dev 桩，这条链路**接通了但拦不住任何东西** |
| GET | `/api/admin/data-requests` | support | 数据主体请求 |
| POST | `/api/admin/data-requests/{id}/run` | support | 执行数据请求 |

### 内容安全第 3 层：语义分类异步复核（`GET /api/admin/safety/recheck`）

总规格 §15 五层漏斗的第 3 层。开关 **`MUSE_SAFETY_SEMANTIC_RECHECK`，默认关闭**（按世界灰度）。

⚠️ **这条链一共三个开关，三个都得按**，任缺一个的表现都不一样：provider 配置
（不配 = 用 Dev 桩，跑但拦不住）· `MUSE_SAFETY_SEMANTIC_RECHECK`（不开 = 根本不跑）·
`MUSE_SAFETY_RECHECK_SWEEP`（不开 = 跑，但队列丢掉的拍没人捡，见下方「投递可靠性」行）。

🔴 **先看这条**：`ModerationProvider` 当前唯一实现是 Dev 桩（`providers::DevModeration`），
真实语义分类**一次都没有发生**。本层交付的是**管线**，不是防线；接真实服务商 = 换实现并把
`is_dev_stub()` 覆写为 `false`。**不得**据此表述为「五层漏斗已完整」或「内容安全已就绪」。
这个事实随数据走三处：`safety_recheck_runs.provider_stub` 列 · 每条 `risk_events.detail_json`
的 `providerStub` · 本端点的 `providerStub` / `source` / `honesty[]`。

| 维度 | 口径 |
|---|---|
| 触发 | `runtime::commit_tick` 在 **`tx.commit()` 之后**入队（`queue` topic 独立于 tick 队列，worker 也分池）。🔴 `check_text` 是网络调用，绝不进 tick 事务（单连接池死锁 + 事务时长被 RTT 绑架） |
| 处置 | 非 Approved → `UPDATE world_events SET moderation` 从 `approved` **收紧**。`SET` 只有这一列、`WHERE` 钉着 `approved` ⇒ **正文逐字节不变**（§0.3）、**单向棘轮**（不放宽、不覆盖更严裁决） |
| 抽样 | 公开投影全量（`MUSE_SAFETY_L3_PUBLIC_SAMPLE_BP`，默认 10000 万分比）；私有投影抽样（`..._PRIVATE_SAMPLE_BP`，默认 500）。**确定性**（`fnv1a_64` 种子 + SplitMix64，域常量 `0x5C`），重试拿到同一批样本 |
| 失败 | 先重试（`MUSE_SAFETY_L3_MAX_ATTEMPTS` / `..._BACKOFF_MS` / `..._TIMEOUT_MS`），预算耗尽 → 🔴 **fail-closed**：收紧为 `pending` + 记险 + **无条件**进人审队列。方向不参数化——与 `MUSE_SAFETY_LEXICON` 的 fail-safe（默认「继续过滤」）自洽 |
| 与第 4 层 | 播出面本就只出 `approved`，故收紧发生在直播水位线之前 ⇒ 观众根本看不到。`interceptedBeforeBroadcast` 是「`MUSE_LIVE_DELAY_TICKS` 够不够」的度量，口径对齐 `live_withholds.preemptive`。⚠️ 它**不等于**「没人看见」：延迟只作用于世界外，成员的读取面不延迟。已返回给客户端的字节收不回，平台也不另发撤回通知 |
| 留痕 | 一律走 `safety` 既有入口（`record_risk` + 第 2/3 层共用的那条 `audit_queue` 写入语句），本层不另开写入路径 |
| provider 耗时 | 响应块 `providerLatency`（migration 0049）。`totalProviderMs` 只累加 `check_text` 两端的时钟差（**超时/报错的调用照算**——剔掉会让「provider 开始抖动」在曲线上反而变好看）；`avgMsPerCall` 是纯 RTT，`maxAttemptLatencyMs` 是一次尝试全程（含 DB 与记账），两者之差 = 本层自身开销。🔴 `usableForAlerting` 在**桩下恒为 false**：恒 0 的「审核延迟」在看板上与「审核非常快」长得一模一样。`checks = 0` 时 `avgMsPerCall` 给 `null` 而不是 `0`（「没调用过」≠「调用极快」）。⚠️ 只覆盖运行时投影这条链，静态审核走 `moderate_and_queue` 不落本表——这也是 `GET /api/admin/worlds` 的 `moderationLatency` **数据源已有却仍不下发**的原因之一 |
| 投递可靠性 | 响应块 `durability`。队列是进程内内存队列、**不持久**；`MUSE_SAFETY_RECHECK_SWEEP`（**默认关**）按 `world_ticks ⋈ safety_recheck_runs` 对账，把「没有终局复核行」的拍补投回去。🔴 有硬上限：只回看 `MUSE_SAFETY_L3_SWEEP_LOOKBACK_MS`（默认 24h），`justOutsideWindow > 0` = 已有拍**永远补不回来**。数字**现算**，不读轮询自己的记账 ⇒ 轮询关着/挂了也照样有效 |
| 成本 | `cost.ratioAvailable` **两侧单价都显式配置时才为 `true`**：T5 门槛「内容审核成本 ≤ 生成成本的 5%」= 审核侧 `MUSE_MODERATION_HTTP_PRICE_CENTS_PER_1K_CALLS` × `moderationCallsInWindow`（含重试）÷ 生成侧 `MUSE_TOKEN_CNY_CENTS_PER_1K` × `generationTokensInWindow`。缺任一半 → `false` + `why` 明说缺哪半。🔴 生成侧**不回落代码内默认估算**（拿估算算门槛得到的是「估算的估算」，在看板上和真值长得一样）。比值一律万分比整数 `ratioBp`，禁浮点 |

✅ **原登记的缺口已闭合（migration 0047）**：此处曾长期写着「`admin_api::audit::writeback_target`
对 `world_event` 主体返回 `None`，人审点『通过』不会写回 `approved`」——那条已由 0047 补上：
`POST /admin/audit-queue/{id}/review` 现在能回写，`world_events` 上因此有了**全仓唯一一条放宽语句**
（`SET` 仍只有 `moderation` 一列、按主键点名、CAS、起点白名单 `IN ('pending','rejected')` 写死在
SQL 里），权限分 reviewer / admin 两档。三条写入路径由源码级红线用例
`red_line_world_events_has_one_ratchet_and_one_guarded_relax` 全仓盘点、逐条钉死形状。

### 人工校准面（无迁移；总规格 §79/§83「人工校准 → 仿真试跑 → 世界质量回归」的第一环）

端点读的都是**已有数据**（`world_templates.saga_id`/`stage_no` 来自迁移 0024，
身份分配来自 `worlds.assembled_json` 的 `/assembly/identityAssignments`，
境界档来自 `world_templates.skeleton_json.realmTier` → `assembled_json` 的 `/assembly/realmTier`），
**不新增迁移、不新增列**。实现在 `server/src/admin_api/calibration.rs`；后台页面在
`admin/src/pages/Calibration.tsx`（世界运营 → 更多模块 → 人工校准，或 `/worlds?view=calibration`）。

**三维不同构，别当成同一张表读**：阶段切分是**坐标**（连续性诊断），身份池是**各不相同的开局站位**
（分布诊断），境界档是**全员统一的一件戏服**（有没有 / 各阶是否在换 / 实例钉住没有）。
境界档没有「分布」——它零抽样、无配额（总规格 §6）。

🔴 **端点全只读，只可视化、不可编辑**：无写入、无副作用、不落 `audit_logs`（没改数据，
写审计只会制造噪声），因此**不挂运营开关**（同 dashboards：VALIDATION §0.1 约束的是写入面）。
每个响应恒带 `editable: false` + `editPath`（说明唯一写入路径仍是 `POST /api/admin/world-templates`），
后台页面把这两个字段直接渲染出来，而不是自己写死「只读」二字。

| 字段 | 口径 |
|---|---|
| `missingStageNos` | 缺号，口径为 `1..=maxStageNo` 内没有模板的阶段号——**从 1 起算**，故「缺开篇」也会被报出来。超过 50 个即截断并置 `missingStageNosTruncated` |
| `duplicateStageNos` / `unnumberedTemplateCount` | 同一 `stage_no` 挂了多个模板 / `saga_id` 非空却 `stage_no ≤ 0`（建模板端点拦得住，直写库与历史数据拦不住） |
| `contiguous` | 无缺号 ∧ 无重号 ∧ 无未编号。**只说明阶段坐标齐整，不代表内容质量合格** |
| `shape.parsed` | `false` = 该模板 `skeleton_json` 不是合法 JSON，此时各形状指标**字段缺席**（不是 0）——「骨架坏了」与「骨架里真的一个主线节点都没有」是两件事 |
| `shape.unknownTopLevelKeys` | 骨架顶层出现的、**无人读取**的键（拼错或残留；`__` 前缀的注释键豁免）。非空即意味着上面那排计数里有几项的 0 是假的——键拼错时 serde 静默取默认值，不报错。建模板期已由 `assembly::validate_skeleton_refs` 拦掉，**但存量模板建在那道闸之前**，这一栏是它们唯一的发现途径 |
| `shape.unknownNestedKeys` | 同上，但**嵌套层**（全部已登记路径），形如 `mainlineNodes[mn-1].constrait`（无 `id` 的元素报 `#序号`）。**比顶层更隐蔽**——`constraint` 拼错只让硬约束降级成 Soft、`expression` 拼错只让整条禁止谓词被丢弃，两种情况下上面那排计数都还是对的 |
| `fillRatio` | **0..1 小数**（渲染须 ×100）。分母 = `quota × worldsWithAssignments`（只算真的参与过分配的世界）。分母为 0 → `null`，显示 `—`，**不得当 0% 读** |
| `gini` | 声明池内各身份**原始分配人次**的集中度（复用 `slo::gini_coefficient`，与叙事注意力基尼同一实现）。**未按 quota 归一化**，配额不等的池须配合 `fillRatio` 一起读。身份 < 2 或一次分配都没发生 → `null` |
| `activeMembersWithoutIdentity` | 在场却没有站位的角色数，**含「装配之后才入场」的成员**——他们本就不在那次分配的名单里，不是分配失败 |
| `unknownIdentityIds` | 分配里出现、当前池里查不到的身份 id = 老实例钉着模板已删除的身份；叙事层对这些角色退化为只显示名字 |
| `truncated` / `worldsTruncated` | 扫描达上限。阶段总览截断时，**末尾那个可能被切断的系列已整组丢弃**（半个系列的连续性诊断是错的） |

🔴 **身份池的真实效力（响应的 `effect` 段原样下发，后台必须显式渲染，不得只画分布图）**：

| 层 | 状态（§0.3 七档） | 事实 |
|---|---|---|
| 分配层 `assignmentLayer` | `Implemented` | `assembly::assign_identities`（内核匹配 + `DOMAIN_IDENTITY` 种子），结果钉进 `assembled_json` |
| 叙事感知层 `narrativeLayer` | `Implemented` | `runtime::load_identity_display_names` 读回 → 他人 brief `唐三（户部主事）` + 本人 `self_identities` 进引擎上下文 |
| 数值层 `numericLayer` | `NeverByDesign` | 平权红线：不改判定 / 不改发奖 / 不开权限 / 不调难度 / 不改准入 |
| 校准闭环 `calibrationLoop` | `Implemented`（2026-07-27） | 读数在 `narrativeSlo.calibration.dimensions.identityShareBalance`（按身份 id 分组的「相对均分倍率」）。见下方「校准维度读数」小节。🔴 **读数建成 ≠ 闭环已验证** |

因此本页能回答「分配结果长什么样、是否失衡」，**不能**回答「这样分配是不是更好」——
后者要先把身份维接进 `slo/`，属独立工作。

#### 境界档（`skeletonJson.realmTier`，总规格 §6【拍板 3】戏服原则）

**它是什么**：阶段模板发给**全员的同一件戏服**——「进黑角域篇全员领斗王档」。
与身份池正相反：**身份各不相同（有池、有配额、有种子分配），境界人人一样（无池、无配额、零抽样）**。
所以境界档这一维**没有「分布」可看**，它的校准问题是「有没有 / 各阶是不是在换 / 实例钉住没有」。

Schema（全部字符串或字符串数组，**一个数字都没有**——§6「跨体系靠风味翻译，不靠数值换算」）：

```jsonc
"realmTier": {
  "id": "tier-douwang",              // 必填，跨阶段对账与审计的稳定键
  "label": "斗王档",                  // 公示档名；空则展示层回落 id
  "cosmology": "cultivation",        // ∈ admission::KNOWN_COSMOLOGIES；**留空 = 无战力体系题材**（都市/言情/历史），合法
  "genre": "xuanhuan",               // ∈ assembly::KNOWN_GENRES；空 = 未标注
  "conflictIntensity": "martial",    // civil 文斗 / martial 武斗 / lethal 生死；空 = 未标注
  "briefing": "本篇全员领斗王档戏服……",  // 入场导演的统一设定
  "flavorNotes": ["魂技译为斗气招式风味，内核不变"]
}
```

三项枚举写自由文本 → 建模板期 400（口径同 `gate.requiredCosmologies`）。
`conflictIntensity: "lethal"` **不是死亡开关**：世界是否致命由建房参数 `lethality` 与 §11 死亡规则
独立决定，两者互不读取。`genre: "history"` 触发响应里的 `stricterModerationHint`，
但**未接进任何审核链路**（状态 `Concept`，仅提示人工按更严标准复核）。

🔴 **境界档的真实效力（`effect` 段五层，后台必须显式渲染）**：

| 层 | 状态（§0.3 七档） | 事实 |
|---|---|---|
| 声明层 `declarationLayer` | `Implemented` | `assembly::RealmTier` schema + `validate_skeleton_refs` 第 6 段取值域校验 |
| 钉住层 `pinningLayer` | `Implemented` | 装配时原样钉进 `assembled_json./assembly/realmTier`（**零抽样、不占 RNG 域常量**，下一个可用域仍是 `0x5C`） |
| 叙事感知层 `narrativeLayer` | **`Integrated`**（2026-07-27 接通） | `runtime::parse_realm_costume` 读回 `briefing` + `flavorNotes` → `RoundInput.realm_costume` → 引擎 `call_director` 的入场导演设局 prompt（§6「入场导演统一设定」） |
| 数值层 `numericLayer` | `NeverByDesign` | §6 + §0.1 平权：`RealmTier` 全字段是字符串 / 字符串数组，`realm_tier_carries_no_numeric_field` 锁住；接进叙事层后同样只改描写，不进任何判定域 |
| 校准闭环 `calibrationLoop` | `Implemented`（2026-07-27） | 读数在 `narrativeSlo.calibration.dimensions.realmTierWorldQuality`（按钉住的戏服分桶的世界质量三指标）。🔴 是**跨世界对比**不是组内分布；读数建成 ≠ 闭环已验证 |

🔴 **七个字段里只有两个进模型上下文**：`briefing` 与 `flavorNotes` 织进入场导演 prompt；
`id`/`label` 只用于本页展示与审计，`cosmology`/`genre` 只是取值域标注，
**`conflictIntensity` 刻意不进**——它长得像生死开关，但世界是否致命由建房参数 `lethality`
与 §11 独立决定，让一个叙事标注去撬动生死判定属平权红线违规。
导演 prompt 里那段戏服恒附一句「只改描写、不得据此判定谁能赢」的免责话术，
它是红线的一部分而非修辞（守卫用例 `realm_tier_reaches_only_the_director_prompt` /
`realm_costume_only_reaches_director` / `realm_costume_never_reaches_state_or_events`）。

所以这一维现在到 **Integrated**（VALIDATION §0.3）：戏服真的会改变这一篇被怎么描写，
但**到此为止**——它不改判定 / 发奖 / 权限 / 难度 / 准入。
「换一件戏服，那批世界演得怎么样」自 2026-07-27 起有了读数（`calibrationLoop` = `Implemented`，
见下方「校准维度读数」小节），但那只是**能测了**，仍**不回答**「这件戏服配得对不对」——
读数不给综合评分、不给判语。**Integrated ≠ 已验证 ≠ 可上线；有读数 ≠ 闭环已成立。**

**未声明即零影响（逐字节）**：`Skeleton.realm_tier` 与 `AssembledInstance.realm_tier` 都是
`Option` + `skip_serializing_if`（同 `payoutTable` 范式），模板不写 `realmTier` 时
`assembled_json` **一个字节都不变**，黄金世界快照因此不受影响。

| 字段 | 口径 |
|---|---|
| `undeclaredInSagaCount` | 归属某个 Saga 却没声明境界档的模板数 = **校准缺口**（§6「阶段天然携带境界档」，每一阶都该有戏服） |
| `undeclaredStandaloneCount` | 独立模板（非 Saga 阶段）没戏服，**只作对照不是缺口** |
| `sagaStages.reusedTierIds` | 同一系列多个阶段发同一件戏服 →「你选阶段就是在选境界」在这几阶之间不成立 |
| `sagaStages.distinctCosmologies` | 多于一个值 = 同系列跨了体系；按 §6 跨体系应走风味翻译而非换档，值得复核 |
| `pinning.worldsWithRealmTier` | 少于 `worldsAssembled` 属正常：模板声明戏服**之前**装配的实例不会回溯补写 |
| `pinning.staleTierIds` | 实例钉着的档 id ≠ 模板当前声明（模板改版后老实例保持原样）。模板未声明时恒空 |
| `matchesTemplate` | `null` = 模板未声明 / 实例未钉住，「是否一致」这个问题不成立——**不得当 `false` 渲染** |
| `invalidEnumFields` / `blankTierId` | 填了官方枚举外的自由文本 / 缺 id。建模板端点拦得住，出现在此 = 历史数据或直写库 |

### 校准维度读数（无迁移；§79/§83 流水线补上「配得对不对可度量」这一环）

**问题**：`/admin/sagas`·`/admin/identity-pools`·`/admin/realm-tiers` 三个视图回答「配成了什么样」，
仿真试跑与世界质量回归回答「这一批世界演得怎么样」，但两者之间少一根线——
`narrativeSlo.metrics` 的八项一律按平台 / 按 `character_id` 聚合，与**运营调的那个旋钮**
（身份 id、境界档）无关，所以「这样配是不是更好」在指标结构上问不出来。

**读出口**：`GET /api/admin/metrics/overview` → **`narrativeSlo.calibration`**（operator/finance）。
它是 `narrativeSlo` 下与 `metrics` 并列的**兄弟键**（`metrics` 是 VALIDATION §4.2 八项表的命名空间，
混进去会让那张表名不副实）。窗口与 `?sloDays=` 同一把尺；**`?slo=0` 一并跳过**
（这一段要分页解析 `assembled_json`，是本端点最重的一块）。实现在 `server/src/slo/calibration.rs`。

**窗口 = cohort**：`worlds.created_at ∈ [start, end)`（窗口内**开出**的这一批世界），两维共用同一批，
只需一次 `worlds` 分页扫描。⚠️ 与 `metrics.attentionGini` 的窗口（「有贡献分更新的世界」）**不是同一批**，
两处数字**不可互相校验**。

| 维度 | 键 | 形状 | 读数 |
|---|---|---|---|
| 身份维（§5） | `dimensions.identityShareBalance` | **组内分布**（身份各不相同，一个世界里同时存在多个） | 每个身份的**相对均分倍率**：`(该成员贡献分 ÷ 本世界成员贡献分总和) × 本世界成员数`，**1.0 = 恰好拿到均分**。`meanRelativeShare` 是读数信封（均值 + `n`/`worlds`/`sd`/中位数/极值），`zeroScoreRate` 是带 Wilson 区间的比例读数，外加各身份**均值之间**的集中度 `meanShareGini`（两层样本量：`n`=身份桶数、`sampleN`=最弱那条腿的观察数），以及 `(unassigned)` 对照桶 |
| 戏服维（§6【拍板 3】） | `dimensions.realmTierWorldQuality` | **跨世界对比**（境界档全员统一，**没有组内分布**——组内基尼恒为 0，是个假指标） | 按钉住的 `realmTier.id` 分桶，各桶各自报 `slo::quality` 的世界质量三指标：完读率 / 阻断率（含独立的内容安全扣留率）/ 结局分布。`(none)` 是未钉戏服的**对照桶**，不丢弃 |

| 项 | 口径 |
|---|---|
| 归一化 | 身份维**先按世界归一化再跨世界求均值**：原始分跨世界求和测的是**世界寿命**不是身份失衡（跑得久的世界分自然多）。归一化后每个「世界 × 角色」观察等权，大小世界不互相主导 |
| 🔴 分母差异 | 身份维分母 = `world_members` **全集**，无贡献分行的成员按 **0 分**计入。`world_contributions` 是**挣到分才落行**的，`attentionGini` 的交集口径因此**看不见「一分没挣到」的人**——而「某个身份是不是系统性拿不到戏」恰恰要靠这些人才答得了。两个数不可互相校验 |
| 🔴 只读不回灌 | 本段**没有一条写语句**，产物只作 JSON 返回。理由与 `world_contributions` 独立建表同源（迁移 0025）：一旦按身份分组的戏份差进了引擎判定输入，「身份影响判定」就成立，直接违反 §0.1 平权红线。锁：`calibration_readings_never_write_anything` / `calibration_readings_never_touch_narrative_state` / `calibration_module_source_contains_no_write_statements` |
| 🔴 四态 | `entry_not_open`（**这一维从未被任何模板配置过**，块级，`value:null`，显示 `—`，且**不发任何计数**——发了会被当成 0 读）/ `no_data_in_window`（配置过但零样本，`value:null`，`—`，计数照发以便区分是「没世界」还是「有世界但没分配 / 都没挣到分」；读数级则指分母为 0）/ `insufficient_sample`（**有样本但 `n < minN`**，`value:null` 而 `pointEstimate`/`n`/`ci95` 照给，显示「样本不足（n=…）」，🔴 不许显示成 `0`，也不宜与 `—` 混同）/ `ok`（真数，**可以是 0**） |
| 🔴 读数信封 | 每个读数是一个对象：`value`（可据此调参的读数，样本不足时 `null`）/ `pointEstimate`（原始算术，永远给）/ `n` / `unit`（`n` 数的是什么：`world`/`tick`/`event`/`observation`）/ `minN` / `ci95` / `ciNote`。**`n` 与 `value` 同处一个对象是刻意的**——取值必须穿过信封，「拿到比例却不知道压在几个观察上」在结构上不可能发生。🔴 渲染时必须与 `n`、`status` 一起渲染；`status ≠ ok` 的读数**不得**渲染成数字。锁：`every_reading_carries_its_own_sample_size` |
| 🔴 最小样本量 | `MUSE_SLO_CALIBRATION_MIN_N`（默认 **30**）+ `MUSE_SLO_CALIBRATION_MIN_GROUPS`（默认 **2**，集中度类专用：1 个分组时基尼恒为 0，读起来是「很分散」而真相是「全压在一个上」，**符号反了**）。两者与依据一起回显在 `calibration.sampleFloor`（数会被复制走，文档不会）。30 的依据：`p̂=0.5` 时 95% Wilson 半宽 n=3 → ±0.37 / n=30 → ±0.17 / n=100 → ±0.10，且 n=30 时单个观察最多挪动比例 3.3 个百分点。**默认值不是物理常量**，同 `attentionGiniMax` |
| 🔴 不确定性 | 比例类读数带 **95% Wilson 区间**（`ci95.method="wilson"`、`level=0.95`）——不用正态近似是因为它在 `p̂` 贴边时区间会塌成一个点。基尼与均值类**不给区间**，理由与替代方案随数下发在 `ciNote`（bootstrap 违反确定性契约；jackknife 重采样的是配置出来的总体、答错问题；均值的观察按世界聚类、iid 区间会低估宽度）。🔴 **有区间不等于有判语**：不给显著性布尔，区间对象只许有 `low`/`high`/`method`/`level`。锁：`confidence_intervals_come_without_a_significance_verdict` |
| 🔴 不给综合分 | 校准是**多目标**的（公平 vs 戏剧性：把戏份摊平到各身份均值全是 1.0 就没有主角了）。两维在 `ok` 态都**没有**代表整维的标量 `value`，全树也不出现 `score`/`grade`/`verdict`/`recommendation` 一类判语字段。锁：`calibration_readings_expose_no_composite_score` |
| 口径复用 | 集中度走 `slo::gini_coefficient`（与叙事注意力基尼同一实现）、三指标走 `slo::quality`（与仿真试跑、世界质量回归**算同一个数**）。批量取事实与单世界取事实由 `bulk_world_facts_match_single_world_facts` 锁住不漂移 |
| cohort 偏差 | `completionRate` 分母含未收尾世界（`quality.rs` 口径），近期窗口天然偏低。本读数的用途是**横向对比**——同一窗口内各桶承受同样的截断偏差；各桶另给 `firstCreatedAt`/`lastCreatedAt`/`unfinished`，年龄分布差得远时的失真要读的人自己看得见。⚠️ 置信区间**只覆盖抽样噪声、不覆盖这个截断偏差**（系统偏差，样本量再大也不消失），该说明随 `completionRate.ciNote` 下发 |
| 各比率的 n 不同 | 戏服桶里三个比率的分母各是**世界 / 拍 / 事件**：`completionRate.n`=世界数、`blockedRate.n`=引擎拍数、`withheldRate.n`=事件数。「这一桶有 40 个世界」**不等于**「阻断率压在 40 个观察上」，混着读会把最不可信的那个数当成最可信的。桶级 `status` 只说世界数那一层 |
| 上限 | 一次最多展开 `MUSE_SLO_CALIBRATION_WORLD_CAP` 个世界（默认 **300**，比 `scanRowCap` 小两个量级是刻意的——那一路解析 `assembled_json`，管的是内存峰值不是行数）。超限 → `skipped_too_large`，**明说跳过而不给残缺数** |

> 🔴 **读数建成 ≠ 校准闭环已验证**（§0.3 七档）。两处 `effect.calibrationLoop` 因此从 `Missing`
> 改为 **`Implemented`** 而不是更高：闭环成立要等运营真的据此调过参、并在下一批世界上看到因果。
> 后台页面照旧只翻译后端下发的状态，不做推断、不做美化。

### 运行时开关（flags，migration 0036；VALIDATION.md §0.1 的 R1 补齐项）

`POST /api/admin/flags` 请求体：

```jsonc
{
  "flag": "MUSE_ONBOARDING",   // 必须在 flags::KNOWN_FLAGS 白名单内，否则 400
  "scope": "user",             // global | world | user
  "targetId": "usr_1",         // global 不接受；world/user 必填，且**写入期校验目标存在**
  "enabled": true,
  "startsAt": 0,               // 毫秒；0 = 立即。窗口左闭右开 [startsAt, endsAt)
  "endsAt": 0,                 // 毫秒；0 = 永不过期
  "reason": "T0 邀请制首批内测"  // 🔴 必填非空，空/纯空白 → 400
}
```

| 项 | 取值 |
|---|---|
| 解析优先级 | **user > world > global > env > 代码内默认值**（窄的赢）。窗口外的记录 = **不参与解析、回落更宽作用域**（不是强制关闭），使灰度可组合：窗口一过，受灰度用户自动跟随大盘 |
| RBAC | 读 `operator`（急停时更需要看得见），**写 admin 专属**——开关直接决定用户能看到什么，爆炸半径与 prompt 激活同档甚至更高，按最严的来 |
| 审计 | 每次 set/delete 落 `audit_logs`（`flag.set`/`flag.delete`），`subject = flag:scope:target`，reason 含**变更前后完整状态**（如 `on[0~0] -> off[0~0] \| 急停`）。记录行另存 `updatedBy`/`updatedAt`/`reason` 作为现状面 |
| 缓存 | 进程内整表快照 + TTL `MUSE_FLAGS_CACHE_TTL_MS`（默认 5000ms，`0` = 禁用直查库）。**写端点内立即 `invalidate`**：本进程点完即生效，多进程部署 ≤TTL 收敛 |
| 幂等 | upsert 语义（同 `flag+scope+targetId` 只有一行，后写的赢），并发下撞唯一索引会退回 UPDATE |
| 外键 | **不建**（同 `prompt_versions.canary_world_ids` 先例）。世界/用户删除**不级联删灰度记录**——级联会静默改变开放范围、事后无从复盘；打错 id 由**写入期存在性校验**挡掉 |

> 🔴 **默认关闭仍是默认**：迁移 0036 **不插任何种子数据**，`enabled` 列 `DEFAULT 0`，
> 登记表里除 `MUSE_SAFETY_LEXICON`（审核链）外 `defaultEnabled` 全为 `false`。
> 三条均有红线用例锁死（`flags::tests::red_line_*`）。

> 生产管理员账号：靠 `users.role='admin'`，由运维经受控迁移/CLI 提权。
> **注意（`admin_api/mod.rs:177` TODO）**：当前 `/api/auth/login` 恒发 `role='user'`，
> 接真实管理员登录需由 auth 侧读 `users.role` 后签发对应 role。

---

## 8. 本清单的生成与校验

```bash
# 路由与方法
grep -rhoE '\.route\("[^"]+"' server/src | wc -l           # 114 条 route 声明（0039 后）
# admin 角色矩阵
grep -rn "require_role" server/src/admin_api/*.rs
```

改动路由后请重跑上面两条并同步本文件。
