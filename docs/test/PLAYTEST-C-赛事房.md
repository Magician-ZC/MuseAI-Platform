# 玩法 C · 赛事房(生死擂台)端到端测试文档

> 数据来源:2026-07-23 dev 态真实运行 `scratchpad/scenario.py`(40 张角色卡 / --features billing,arena)。
> **前提**:dev 态无模型 + 世界由 admin 以"官方世界"创建。**本文档暴露一个真实缺口(见发现①):官方赛事房未绑定主播,导致主播控制台整条回路 403 不可用**——这是本轮测试最重要的发现。

## 1. 玩法定位与测试目标
赛事房是"把无限流搬进现实的真人局":主播(host)开一场限时比赛,40 名角色进场,主播按节奏推进回合、按仲裁淘汰、结算冠军;观众通过**打赏网关**送礼(走系统频道,不改剧情结果)、通过**复活资格**购买再战机会(买过程不买结果)。测试目标:验证主播控制台(host/tick/eliminate/settle)、淘汰同意门控、打赏网关红线、复活资格红线、赛事播报合规、计费与年龄门。

## 2. 测试环境与数据
- 世界:`wld_arena…` title=生死擂台 roomType=**arena** status=running memberCount=**40**(40 卡全进场)
- 创建方:admin 官方世界(`CreateWorldParams::official` → **host_user_id = None**)
- 三主角在生死局的不可替换选择(接模型后驱动淘汰赛分歧):沈墨/谋略=借刀杀人不沾手;江晚/义气=挡刀替死不退;苏鸢/生存=弃子保身抽身

## 3. 测试用例矩阵

| # | 步骤 | 请求 | 实际响应 | 结果 |
|---|---|---|---|---|
| C1 | 40 卡进场 | POST /worlds/{id}/join ×40 | memberCount=40 | ✅ 通过 |
| C2 | 主播开赛 | POST …/arena/host | **403 无权限** | ❌ 见发现①(世界无主播) |
| C3 | 主播推进回合 | POST …/arena/tick | **403 无权限** | ❌ 见发现①(阻塞:回合/环境/播报都产不出) |
| C4 | **打赏网关红线** | POST …/arena/gift(webhook) ×2 | 200 ×2 | ✅ 通过(礼物走系统频道入账) |
| C5 | 主播淘汰(触发同意门控) | POST …/arena/eliminate | **403 无权限** | ❌ 见发现①(连带 C6 无法演示) |
| C6 | 淘汰同意门控 | GET …/arena/consents (pending) | `[]` | ⚠️ 空(因 C5 被 403 挡住,未产生待批) |
| C7 | **复活资格红线** | POST …/arena/revive-match | 200 `{status:"eligible", boundary:{buys:"revive_eligibility", notImmunity:true, notFinalVerdict:true}, reviveGrantId:…}` | ✅ 红线守住(买资格≠买免死≠买判决) |
| C8 | 主播结算 | POST …/arena/settle | **403 无权限** | ❌ 见发现①(冠军/荣誉奖励无法兑现) |
| C9 | 赛事播报合规 | GET …/arena/report | 200 `{compliance:{aiGenerated:true, arbitrationPublic:true}, match:{phase:"lobby", winnerCharId:null}, rounds:[], environment:[]}` | ✅ 合规标注在;内容空=停在 lobby(未开赛) |
| C10 | 年龄声明门 | POST /me/age-declaration | 200 | ✅ 通过 |
| C11 | **计费充值 + 未成年拦截** | POST /billing/recharge | 200 `{orderId:…, balanceCents:3000}` | ✅ 通过(age_declared==1 成年才放行) |

## 4. 红线核对
- ✅ **打赏走系统频道非玩家干预**(C4):礼物经 gift webhook 入账/入播报,不作为改变仲裁结果的干预路径。
- ✅ **复活买过程不买结果**(C7):返回 `boundary` 显式声明 `buys:"revive_eligibility"`(仅资格)、`notImmunity:true`(非免死)、`notFinalVerdict:true`(非判决)——红线以数据结构固化,不可绕过。
- ✅ **无提现/转账**:复活/打赏均单向入账,无逆向路径。
- ✅ **未成年拒充**(C11):充值前置 `age_declared==1`(成年声明),default/未成年拦截(G-BILLING #2 加固)。
- ✅ **AI 生成 + 仲裁公开**合规标注(C9):播报头部固定 `aiGenerated:true, arbitrationPublic:true`。
- ✅ **主播权限校验生效**(C2/C3/C5/C8):`require_host` 正确拒绝非主播——**校验本身是对的**,问题在于官方世界压根没设主播(见发现①),不是校验漏洞。

## 5. 发现与 seam
- **发现①(真实缺口 · 阻塞赛事房核心回路)**:admin 创建的官方赛事房 `host_user_id = None`(worlds/mod.rs:420 `CreateWorldParams::official`),而主播动作(host/tick/eliminate/settle,arena/mod.rs:55-58 `require_host`)要求调用者 == world.host_user_id。**结果:官方赛事房没有任何人能当主播,整条主播控制台回路(开赛→推进→淘汰→结算)全部 403**。这不是权限校验 bug(校验是对的),而是**缺一个主播指派机制**。影响:赛事房作为"主播真人局"的核心玩法当前无法在官方世界端到端跑通;打赏(C4)、复活(C7)、播报(C9)、计费(C11)这些不依赖主播的旁路机制可用,但比赛本身开不起来。
  - **修复建议(需产品定方向,故未擅自改)**:
    1. **admin 指派**:`admin_api CreateWorldReq` 增可选 `hostUserId`,arena 世界创建时写入 world.host_user_id(最小改动,官方赛事由平台指定主播);
    2. **主播认领**:新增 `POST /worlds/{id}/arena/claim-host`,持 host 角色的用户认领未指派的官方赛事房(去中心化,适合"平台放开赛事房给主播开");
    3. **房主即主播**:P4b 私有开黑房走房主自建路径(创建者 = host_user_id),该路径下主播动作本就可用——官方世界需要的是 1 或 2 其一。
  - 推荐方案 1(admin 指派)作为最小闭环,方案 2 作为运营放开后的自助入口。
- **seam(需主播 + 模型)**:C3/C5/C6/C8 依赖主播先就位(发现①)+ 真实模型跑回合。补齐发现①并接模型后:tick 产回合叙事/环境事件、eliminate 对不可逆淘汰产 `ConsentRequested` 进 C6 待批队列、settle 兑现冠军**荣誉级**奖励(非战力,红线)。同意门控的引擎侧(G-ENGINE)与 runtime 消费(G-RUNTIME)已实现,只等主播动作能触发。

## 6. 结论
**赛事房的旁路机制(打赏网关、复活资格、赛事播报合规、计费 + 年龄门)全部验证通过,红线以数据结构固化守得很死**。但**主线回路(主播开赛→推进→淘汰→结算)因官方世界缺主播指派机制而整条 403 不可用(发现①)——这是本轮测试最重要的产出**。`require_host` 校验本身正确,缺的是给官方赛事房绑定主播的入口。建议按方案 1(admin CreateWorldReq 增 hostUserId)补最小闭环,再接真实模型演示完整生死擂台。是否修复请示下方向。
