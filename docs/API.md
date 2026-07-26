# 平台后端 API 清单（server）

> 全部端点 nest 在 `/api` 下（`server/src/app.rs`），共 **103 条路由声明 / 112 个方法-路径组合**
> （0036 运行时开关 +3 条声明 / +4 个组合；口径以 §8 的 grep 为准）。
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
全部端点由 `MUSE_OOC_ANNOTATIONS` 控制，**默认关闭 → 六端点全 404 且一行都不落库**。

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
| GET | `/api/assets/characters/{id}/status` | JWT | 审核状态查询 |
| POST | `/api/assets/characters/{id}/appeal` | JWT | 审核驳回后申诉 |
| GET | `/api/assets/characters/{id}/manifest` | JWT | 角色卡清单（钉住版本） |
| POST | `/api/assets/characters/{id}/avatar` | JWT | 上传立绘 |
| POST | `/api/assets/characters/{id}/withdraw` | JWT | 停止后续投放（幂等） |
| DELETE | `/api/assets/characters/{id}` | JWT | 删除 |
| GET | `/api/assets/objects/{*key}` | 公开 | 对象回读（头像等）。**能力 URL**：键含 128 位随机 id，`is_safe_object_key` 防路径穿越 |
| POST | `/api/assets/worlds` | JWT | 创作者发布世界模板（超集冗余门 `MIN_REDUNDANCY_RATIO=3.0`） |
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
| GET | `/api/worlds/{id}/interventions/mine` | JWT | 我的干预记录 |
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
| 运营开关 | `MUSE_ROOM_INVITATIONS`，**默认关闭**（VALIDATION.md §0.1）。关闭时四个端点全 404，且**已存在的邀请也读不出、响应不了**（读取侧降级，可逆急停阀，范式同 `MUSE_LETHALITY_DEATHMATCH`） |
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

## 4. 玩家账户（me / backpack / progression / subplot / memorial / onboarding / annotations / reports / notifications）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/api/me/backpack` | JWT | 背包（道具单一写入路径 `grant_item_tx` 的读侧） |
| GET | `/api/me/memberships` | JWT | 我在哪些世界里 |
| GET | `/api/me/progression` | JWT | 历练与卡位（migration 0019） |
| POST | `/api/me/card-slots/unlock` | JWT | 卡位解锁（默认 3，历练解锁至 6：500/1500/4000） |
| GET | `/api/me/reports` | JWT | 日报列表 |
| GET | `/api/me/reports/{id}` | JWT | 日报详情 |
| GET | `/api/me/notifications` | JWT | 通知列表 |
| GET/PUT | `/api/me/notification-preferences` | JWT | 通知偏好读写 |

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

## 5. 计费与商城（billing / shop）

| 方法 | 路径 | 鉴权 | feature | 说明 |
|---|---|---|---|---|
| POST | `/api/billing/orders` | JWT | `billing` | 下单充值。**PaymentProvider 当前为 Dev 桩** |
| GET | `/api/billing/balance` | JWT | `billing` | 余额 |
| POST | `/api/billing/refunds` | JWT | `billing` | 退款 |
| GET | `/api/me/earnings` | JWT | `billing`\|`arena` | 创作者收益查询。**平台内权益，无提现（红线）** |
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
| POST | `/api/arena/{worldId}/gift` | JWT | 礼物投递。boon 记入 `arena_env_events`，**注入引擎回合待 `RoundInput` 扩展（已知 seam）** |
| GET | `/api/arena/{worldId}/clips` | JWT | 高光切片（TtsProvider/切片为 Dev 桩） |
| POST | `/api/livegate/webhook` | 验签 | 直播平台礼物回调。`MUSE_LIVEGATE_SECRET` 未配置时 **fail-closed** |

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
| POST | `/api/admin/audit-queue/{id}/approve`·`/reject` | reviewer | 审核裁定（回写主体 moderation） |
| GET | `/api/admin/appeals` | reviewer | 申诉列表 |
| POST | `/api/admin/appeals/{id}/resolve` | reviewer | 申诉复审（overturn/uphold，**唯一改判路径**） |
| GET | `/api/admin/worlds` | operator | 世界列表。含 `participantCount`、`successRate`（**0..1 小数**，无已终结 tick 时为 null）、`todayTokens`/`todayCostCents`/`todayCostCny` |
| POST | `/api/admin/worlds` | operator | 官方建房。可选 `cover`（一次建完带图）· 可选 `series: {maxInstances}`（**登记为世界系列 1 号实例**，migration 0035；开关未开时 400，见 §3「世界系列自动扩容」）|
| GET | `/api/admin/worlds/{id}/diagnostics` | operator | 脱敏诊断（采样种子不外泄）。`budget` 含金额换算与用量比：`spentCny`/`dailyCnyBudget`/`usageRatio`（**0..1**，取 token 与 cny 两维较大者）/`spentTokensTodayEffective`（跨日已归零）。另含 `series`（系列队列态；**不受 env 开关门控**——关阀时运营更需要看得见队列，开关状态另作 `autoscaleEnabled` 明示）与 `beBiography`（崩塌封卷元信息，正文另走玩家读取面）|
| POST | `/api/admin/worlds/{id}/pause`·`/resume` | operator | 暂停/恢复（需审计理由） |
| GET | `/api/admin/world-templates?sagaId=` | operator, reviewer | 模板列表。带 `sagaId` 时切换为**阶段列表**语义：只返回该世界系列，按 `stage_no` 升序（剧情顺序）且不分页 |
| POST | `/api/admin/world-templates` | operator | 建模板。可选 `sagaId` + `stageNo`（总规格 §3 Saga 归组），二者必须成对，`stageNo` ∈ 1-999；都不传 = 独立模板 |
| POST | `/api/admin/world-templates/{id}/star` | operator | 星级 curation（**3-5★ 唯一晋升路径**） |
| GET | `/api/admin/economy/overview` | finance | 经济只读聚合 |
| GET | `/api/admin/ledger/reconcile` | finance | 全账复式恒等 SUM=0 + 物化余额对账（只读，无提现） |
| GET | `/api/admin/metrics/overview?costDays=` | operator, finance | 数据看板。含 `cost` 对象：`today`（今日 token/分/元）、`trend[]`（近 N 日，默认 7，clamp [1,60]）、`byWorld[]`（每局 Top10 含 `tokensPerPlayer`）、`total`、`centsPer1kTokens`。**每玩家成本口径为人均等分**（`world_ticks` 是整拍口径、无 per-member 分解），局限见响应 `notes` |
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
| GET | `/api/admin/data-requests` | support | 数据主体请求 |
| POST | `/api/admin/data-requests/{id}/run` | support | 执行数据请求 |

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
grep -rhoE '\.route\("[^"]+"' server/src | wc -l           # 109 条 route 声明（0037 后）
# admin 角色矩阵
grep -rn "require_role" server/src/admin_api/*.rs
```

改动路由后请重跑上面两条并同步本文件。
