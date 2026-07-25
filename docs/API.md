# 平台后端 API 清单（server）

> 全部端点 nest 在 `/api` 下（`server/src/app.rs:65`），共 **84 条路由声明 / 90 个方法-路径组合**。
> 鉴权列语义：**JWT** = 需 `Authorization: Bearer <accessToken>`（`AuthUser` 提取器）；
> **公开** = 无需 token；**admin+角色** = 需管理员 token 且 `require_role` 通过（`admin` 角色恒通过）。
> 校验于 2026-07-25。**改路由必须同步改本文件**——与 `docs/VALIDATION.md` §3 台账同级纪律。

## 0. 挂载与 feature 门控

| 模块 | feature | 说明 |
|---|---|---|
| auth / assets / worlds / events / interventions / consents / notifications / reports / backpack / chapters / progression / admin_api | 默认 | 默认构建即含 |
| arena / livegate | `arena` | 赛事房与直播礼物网关 |
| billing | `billing` | 计费闭环 |
| shop | `billing` 或 `arena` | 依赖复式账本，与 ledger 同门控（`app.rs:61`） |

未启用 feature 时对应路由**不注册**，请求返回 404。

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
| GET | `/api/assets/worlds/mine` | JWT | 我发布的世界 |
| GET | `/api/assets/worlds/{id}/status` | JWT | 审核状态 |
| GET | `/api/assets/worlds/{id}/manifest` | JWT | 世界清单 |
| POST | `/api/assets/worlds/{id}/withdraw` | JWT | 下架 |

## 3. 世界运行时（worlds / events / interventions / consents / chapters）

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

## 4. 玩家账户（me / backpack / progression / reports / notifications）

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
# 路由与方法（应得 90 个方法-路径）
grep -rhoE '\.route\("[^"]+"' server/src | wc -l           # 84 条 route 声明
# admin 角色矩阵
grep -rn "require_role" server/src/admin_api/*.rs
```

改动路由后请重跑上面两条并同步本文件。
