# 平台后端 API 清单（server）

> 全部端点 nest 在 `/api` 下（`server/src/app.rs`），共 **99 条路由声明 / 108 个方法-路径组合**。
> 鉴权列语义：**JWT** = 需 `Authorization: Bearer <accessToken>`（`AuthUser` 提取器）；
> **公开** = 无需 token；**admin+角色** = 需管理员 token 且 `require_role` 通过（`admin` 角色恒通过）。
> 校验于 2026-07-26。**改路由必须同步改本文件**——与 `docs/VALIDATION.md` §3 台账同级纪律。

## 0. 挂载与 feature 门控

| 模块 | feature | 说明 |
|---|---|---|
| auth / assets / worlds / events / interventions / consents / invitations / notifications / reports / backpack / chapters / progression / subplot / memorial / onboarding / admin_api | 默认 | 默认构建即含 |
| arena / livegate | `arena` | 赛事房与直播礼物网关 |
| billing | `billing` | 计费闭环 |
| shop | `billing` 或 `arena` | 依赖复式账本，与 ledger 同门控（`app.rs:61`） |

未启用 feature 时对应路由**不注册**，请求返回 404。

**运行时开关**（与 feature 门控正交，同样"未验证功能默认关闭"）：`invitations` 全部端点由
`MUSE_ROOM_INVITATIONS` 控制，**默认关闭 → 404**；`onboarding`（新手动线）全部端点由
`MUSE_ONBOARDING` 控制，**默认关闭 → 404**；`subplot`（副本卡）全部端点**与结算铸卡**由
`MUSE_SUBPLOT_CARDS` 控制，**默认关闭 → 端点 404 且结算一张不铸**；自定义房容器装配
（副本卡的消费端，无独立端点）由 `MUSE_CONTAINER_ASSEMBLY` 控制，**默认关闭 → 建模板期拒绝声明
`subplotCardRefs`，装配期忽略容器字段走原路径**；`memorial`（传世卡 · 遗作馆）全部端点
**与封卷本身**由 `MUSE_MEMORIAL` 控制，**默认关闭 → 端点 404 且不发生任何封卷**；世界的生死状档由
`MUSE_LETHALITY_DEATHMATCH` 控制，默认关闭 → 读取侧降级为同意制。

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

## 4. 玩家账户（me / backpack / progression / subplot / memorial / onboarding / reports / notifications）

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
| POST | `/api/admin/worlds` | operator | 官方建房 |
| GET | `/api/admin/worlds/{id}/diagnostics` | operator | 脱敏诊断（采样种子不外泄）。`budget` 含金额换算与用量比：`spentCny`/`dailyCnyBudget`/`usageRatio`（**0..1**，取 token 与 cny 两维较大者）/`spentTokensTodayEffective`（跨日已归零） |
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
| GET | `/api/admin/risk-events` | operator, reviewer, support | 风控事件 |
| GET | `/api/admin/data-requests` | support | 数据主体请求 |
| POST | `/api/admin/data-requests/{id}/run` | support | 执行数据请求 |

> 生产管理员账号：靠 `users.role='admin'`，由运维经受控迁移/CLI 提权。
> **注意（`admin_api/mod.rs:177` TODO）**：当前 `/api/auth/login` 恒发 `role='user'`，
> 接真实管理员登录需由 auth 侧读 `users.role` 后签发对应 role。

---

## 8. 本清单的生成与校验

```bash
# 路由与方法（应得 104 个方法-路径）
grep -rhoE '\.route\("[^"]+"' server/src | wc -l           # 95 条 route 声明
# admin 角色矩阵
grep -rn "require_role" server/src/admin_api/*.rs
```

改动路由后请重跑上面两条并同步本文件。
