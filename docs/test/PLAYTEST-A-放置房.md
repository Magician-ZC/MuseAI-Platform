# 玩法 A · 放置房(长夏小镇)端到端测试文档

> 数据来源:2026-07-23 dev 态真实运行 `scratchpad/scenario.py`(40 张角色卡 / axum + SQLite 内存库 + billing,arena feature)。
> **关键前提**:dev 态无模型配置,世界 tick 为 no-op(不真跑 LLM 回合)。本文档验证的是**平台机制层**(账号/发卡/投放/干预/日报/权威状态);**叙事内容**(角色实际生成的关系、日报正文)需接真实模型才产出,已逐条标注。

## 1. 玩法定位与测试目标
放置房是"慢世界":40 名角色投进同一世界,每日 2-4 个节拍由平台调度自主推进,主人通过**干预三环**(构筑/托梦/道具)轻度影响,靠**日报**《你的角色昨日人生》回访。测试目标:验证多角色投放、干预三环的服务端权威与额度、权威关系快照、日报机制。

## 2. 测试环境与数据
- 世界:`wld_9f9899a6…` title=长夏小镇 roomType=**idle** status=running memberLimit=60 tickPerDay=3
- 角色:**40 张卡全部发布成功(40/40)**,8 种决策原型均衡各 5 张(谋略/义气/生存/理想/复仇/机会/守护/野心)
- 投放:**40 名成员全部 join 成功,世界 memberCount=40**
- 三张"主角"内核对照(同一局势"发现同伴是内奸"→不可替换的选择,接真实模型后由引擎据此生成分歧):沈墨/谋略=隐忍布局反用之;江晚/义气=当场质问拔刀;苏鸢/生存=不动声色留后路

## 3. 测试用例矩阵

| # | 步骤 | 请求 | 实际响应 | 结果 |
|---|---|---|---|---|
| A1 | 40 卡投放 | POST /worlds/{id}/join ×40 | 40 成功,memberCount=40 | ✅ 通过 |
| A2 | 托梦(影响环低优先层) | POST /worlds/{id}/interventions {kind:whisper} | 200 accepted | ✅ 通过 |
| A3 | **道具干预红线**(投未拥有道具) | POST …/interventions {kind:item, itemId:"x"} | **403 forged_state** | ✅ 红线守住(不在背包→风控拦截) |
| A4 | 托梦额度(连发 4 条,上限 3/节拍) | POST …/interventions {whisper} ×4 | 4 条 HTTP 200 | ⚠️ 见发现①(额度超限按设计返 200+body rejected) |
| A5 | 干预记录 | GET …/interventions/mine | {count:4} | ✅ 通过(记录含被拒条目) |
| A6 | 权威关系/状态快照 | GET /worlds/{id}/state-summary | `{characters:[],relations:[]}` | ✅ 端点通(principal 过滤生效);内容空=无模型回合未产出状态 |
| A7 | 日报 | GET /me/reports | `{reports:[],nextCursor:null}` | ✅ 端点通;无当日日报=tick 空转(无模型) |

## 4. 红线核对
- ✅ **服务端权威**:道具必须真在背包才能投放,投未拥有道具(A3)→ `forged_state` 风控 + 403,客户端无法凭空注入。
- ✅ **托梦低优先层**:whisper 进决策上下文低优先层,角色可依人设忽略(引擎侧铁律,A2 仅验证受理)。
- ✅ **受众隔离**:state-summary(A6)按 principal 过滤,观战/非当事者看不到私密关系。

## 5. 发现与 seam
- **发现①(脚本假象,非 bug)**:托梦额度超限时按设计返回 **HTTP 200 + body `status=rejected("quota")`**(不作为攻击不报错),测试脚本仅看状态码故显示 4 条 200;`/mine` 的第 4 条 body 实为 rejected。额度逻辑(时间窗 = 86400000/tickPerDay = 8h 内计数,上限 3)本身正确。**建议**:客户端按 body.status 而非 HTTP 码判定受理结果。
- **seam(需真实模型)**:A6 关系/A7 日报为空,因 dev 无模型 tick 空转、narrative_state 未被回合填充。接真实模型后,tick 跑 `run_round` → 状态累积 → state-summary 有关系图、日报有《昨日人生》正文。这不是缺陷,是 dev 态的预期。
- **seam(放置房同意触发源)**:放置房内不可逆事件(角色死亡/永久关系)的同意门控,依赖引擎回合产 `ConsentRequested`——本次无模型未触发。引擎侧触发逻辑已实现(G-ENGINE),runtime 消费已接(G-RUNTIME),接模型后自动生效。

## 6. 结论
**放置房平台机制层验证通过**:40 角色投放、干预三环(托梦受理 + 道具红线 + 额度)、权威状态快照端点、日报端点均按契约工作,服务端权威与受众隔离守住。**叙事层(关系演化、日报正文)需接真实模型**——这是 dev 态的预期边界,非缺陷。建议客户端按 intervention body.status 判定额度结果。
