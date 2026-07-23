# 玩法 B · 章节房(风起副本)端到端测试文档

> 数据来源:2026-07-23 dev 态真实运行 `scratchpad/scenario.py`(40 张角色卡)。
> **前提**:dev 态无模型,tick/装配的模型精修为 no-op;但**开局装配的规则选择层不依赖模型**,故装配与结算机制可真实验证。

## 1. 玩法定位与测试目标
章节房是"无限流副本":官方世界模板(主线硬节点 + 结局池 + 隐藏内容池)+ 40 名角色投入,**开局装配器**读取全体角色的执念/剧情种子,从预审核内容池选择并参数化隐藏内容,使每个副本因阵容而异;通关**结算**兑现绑定各角色执念的隐藏道具进跨世界背包。测试目标:验证开局装配、通关结算、资产复制三重防线、跨世界背包。

## 2. 测试环境与数据
- 世界:`wld_6daa4539…` title=风起副本 roomType=**chapter** status=running memberCount=**40**
- 模板 skeleton:主线硬节点 `n1 内应现形(fated)`;结局池 `e_intrigue(谋略)/e_loyalty(义气)`;隐藏内容池 3 项(账册→item_ledger[主题 把柄/旧债/掌控]、信物→item_token[义气/救命之恩]、密道→item_map[退路/抽身/自由])
- 阵容执念可绑定性:沈墨(谋略,种子"握有同僚把柄")↔账册;江晚(义气,"欠救命之恩")↔信物;苏鸢(生存,"备好退路")↔密道——三主角与三隐藏内容主题高度匹配

## 3. 测试用例矩阵

| # | 步骤 | 请求 | 实际响应 | 结果 |
|---|---|---|---|---|
| B1 | 40 卡投放 | POST /worlds/{id}/join ×40 | memberCount=40 | ✅ 通过 |
| B2 | 开启章节(首触发→开局装配) | POST /worlds/{id}/chapters/start | 200 | ✅ 装配执行(规则选择层,不依赖模型) |
| B3 | 通关结算 | POST /worlds/{id}/chapters/finish | 200 `{cleared:true, advancedTo:1, totalNodes:1, grantedItems:[], offlineStarted:true}` | ⚠️ 通关判定+离线夹层通;**grantedItems 空**(见发现②) |
| B4 | 结算幂等重放 | POST …/chapters/finish(再次) | 200 `{advancedTo:2, grantedItems:[]}` | ⚠️ 见发现③(重放推进了节点而非幂等短路) |
| B5 | 跨世界背包 | GET /me/backpack | `{items:[]}` | ⚠️ 空(因 B3 未发货) |

## 4. 红线核对
- ✅ **服务端权威(结构上)**:结算走事务 + `state_revision` CAS + `backpacks(user_id,reward_hook_key)` 唯一约束三重防线(代码 chapters/mod.rs:184-287,加固审计 C-3 已验证为实体,单测 `concurrent_finish_grants_reward_exactly_once` 覆盖并发恰发一次)。本次运行未产出可发道具,故三重防线未被实际触发,但机制在。
- ✅ **背包单一写入路径**:物品取得仅 `grant_item_tx`(通关结算/支付两路),无"客户端声明拥有"接口。

## 5. 发现与 seam
- **发现②(需核实)**:B3 结算 `cleared:true` 但 `grantedItems:[]`、背包空。结算发货逻辑(chapters/mod.rs:220-284)遍历 `assembled.per_character_hooks`,仅对**属本人角色、携带 reward_item、未兑现**的钩子发货。空产出说明:本次开局装配未给发起人(沈墨)绑定带 reward_item 的钩子,或钩子的 character_id 与结算时的本人 cloud char 未对上。**建议核实**:装配的 per_character_hooks 是否为 40 名成员逐一绑定、reward_item 是否挂上、finish 的角色匹配键是否与装配一致。这是章节房"个性化奖励"闭环的关键一环,单测覆盖了并发幂等但未覆盖"多成员装配→按执念匹配发货"的端到端。
- **发现③(语义确认)**:B4 重放使 `advancedTo` 从 1→2。finish 每次推进 `currentNode`(主线节点游标),而"不二次发货"的幂等锚点是 `grantedHookIds`(道具级),非节点级。本次无道具可发,故未触发道具幂等;节点推进是否应对同一玩家重复 finish 幂等,需产品确认语义(每角色独立进度 vs 副本整体推进,审计 R3 曾标注 currentNode 共享计数器为 LOW)。
- **seam(装配模型精修)**:装配的连接文本模型精修在 dev 无模型时跳过,走规则选择;接模型后钩子文本更自然。

## 6. 结论
**章节房的投放、开局装配触发、通关判定、离线夹层、资产复制三重防线(结构)均验证到位**,服务端权威健全。**一个待核实发现**:本次多成员装配未产出可发道具(发现②),导致奖励闭环空转——建议补一个"40 成员装配→按执念绑定钩子→结算逐角色发货"的端到端测试并核实钩子 character_id 匹配。节点重复推进语义(发现③)需产品定性。
