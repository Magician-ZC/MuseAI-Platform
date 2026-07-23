# P4b 计费 + P6 赛事房 — 开发契约

> 基线:加固后 server 110 测试绿,muse-engine 136,前端 tsc 0。feature-gated 骨架(billing/arena)能编译。
> 合规边界不变:DevProvider + 真实接入位 + 注释;合规门(支付牌照/版号)是运营动作,代码只预留。
> 红线写进实现与测试:余额不可提现/不可转账、订单退款幂等账本双录、买过程不买结果、无免死道具、胜者奖励荣誉非强度、未成年人限充/限礼。

## Wave 1 — 后端(3 agent 并行,各独占 feature-gated 文件域)

| Agent | 域 | feature | migration | 核心 |
|---|---|---|---|---|
| **P4b** | server/src/billing/* | `billing` | 0006 | 充值订单/余额/退款/账本双录/幂等履约(DevPayment)、未成年拒充、无提现转账端点 |
| **P6a** | server/src/arena/* | `arena` | 0007 | 唯一胜者赛制状态机、主播控制台触发回合(复用 runtime::insert_tick)、透明战报(world_events+仲裁 rule_refs)、复活赛资格(非免死)、**淘汰不可逆→consents::create_consent 同意门控**(补上重审发现的同意触发源缺口,在 arena 层不改引擎) |
| **P6b** | server/src/livegate/*、clips/* | `arena` | 0008 | 礼物 webhook→SKU 映射→arena_env_events(专用系统环境通道,不走被 HC 禁用的玩家 item 干预)、同 SKU 聚合、高光切片(DevTts) |

**跨 agent 契约 arena_env_events**(P6a 在 0007 建表,P6b 写入):
`arena_env_events(id TEXT PK, world_id TEXT, applied_tick INTEGER NULL, kind TEXT, payload_json TEXT, aggregated_count INTEGER DEFAULT 1, created_at BIGINT)`。P6b 的 livegate 写 kind='gift_boon';P6a 读作战报/环境。

## 已知 seam(诚实标注,不在本期强行接)
- **礼物→引擎回合真实影响**:gift boon 记入 arena_env_events + 进战报,但注入 LLM 回合需 runtime RoundInput 扩展(HA 域)——标为 seam,与后续 runtime 迭代一起做。
- **复活/礼物付费扣费**:P6a 记 revive 资格、P6b 记 gift 账,实际扣费经 billing 集成(跨 feature)留 TODO。
- **placement-room 同意触发源**(死亡/永久关系):P4a 仍缺,本期只在 arena 淘汰处补;placement 与后续叙事迭代一起。

## Wave 2 — 前端/后台(Wave 1 落地后)
钱包充值 UI(client)、赛事房主播控制台 + 观战战报(client)、admin 经济模块(占位→真实)。

## 验证
每 agent:`cargo test --features <flag>` 自域绿 + `cargo check`(default 无 feature)仍编译。全部落地后主循环全栈复验(含 --features billing,arena)。

## ✅ Wave 1 后端完成(集成复验)
- **P4b 计费**(7 测试):充值/余额/退款、账本双录恒等式、订单退款幂等+状态机、未成年拒充、无提现/转账端点。migration 0006(仅索引)。
- **P6a 赛事房核心**(6 测试):唯一胜者赛制、host/tick 复用 runtime::schedule_tick、透明战报(public events+ruleRefs+env)、复活赛资格(非免死)、**淘汰不可逆→consents::create_consent 同意门控(补 P4a 同意触发源缺口)**、荣誉奖励。migration 0007(arena_matches/rewards/revive_grants/eliminations/env_events)。
- **P6b 礼物+切片**(12 测试):webhook→SKU→arena_env_events 专用通道(不走玩家干预)、同 SKU 聚合、高光切片(仅 public 事件+DevTts)。migration 0008。
- **集成复验:`cargo test --features billing,arena` 148 passed / 0 failed;default 110;migration 0001-0008 干净。**

## ✅ Wave 2 前端/后台完成
- **FE1**(client,421 测试绿):钱包(余额/充值/退款/不可提现红线)、赛事房主播控制台(赛制/触发回合/淘汰+同意门控/结算/复活)、观战透明战报(时间轴+判定依据+礼物日志)。PlatformShell 导航加"钱包"入口(赛事房按世界上下文进入)。
- **FE2**(admin,3 测试绿):经济模块真实只读聚合(充值/退款/余额/礼物/订单,双录恒等式自检,finance/admin 角色门控)+ echarts。

## ✅✅ P4b+P6 最终全栈复验(全绿)
- muse-engine **136** / server default **125** / server `--features billing,arena` **150 passed / 0 failed** / src-tauri 编译通过
- 前端 **421**(69 文件)+ tsc 0 错误 / admin build 产出 dist
- **server 二进制带 feature 冒烟**:计费端到端(充值 5000 分→余额 5000,DevPayment 履约+账本双录)、后台经济看板读真实聚合、赛事房端点已路由、**提现端点 404(红线)**
- migration 0001-0008 干净

## 交付边界(诚实)
- feature-gated(billing/arena 默认关);合规门(支付牌照/版号/拟人化互动评估)是运营动作,代码 DevProvider + 预留位。
- 已知 seam:礼物→LLM 回合真实注入(runtime RoundInput 扩展)、复活/礼物实际扣费(跨 feature 集成)、placement 房同意触发源(赛事房淘汰处已补,placement 待叙事迭代)、L2/L3 视觉呈现、创作者结算(另一套账)。
