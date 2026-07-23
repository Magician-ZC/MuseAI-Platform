# 验收缺口补齐(代码层面)

> 补 AUDIT-P0-P6.md 的 A 类(2 小修)+ B 类(9 功能 backlog)。C 类(spec 授权退化 / 运营合规)不属代码层面,不碰。
> 基线:muse-engine 136 / server default 125 / server --features 150 / 前端 421+tsc0 / admin build。补齐后不得回归。

## Agent 分派(6,按文件域;跨层缺口两端定契约)

| Agent | 域 | 缺口 |
|---|---|---|
| **G-ENGINE** | crates/muse-engine | #4 时间边界确定性过滤+测试;#7 conflictsWith 证据级回写;#3a 引擎对不可逆行动产出 ConsentRequested + 门控不落定 |
| **G-RUNTIME** | server/src/{runtime,events} | #3b runtime 消费 ConsentRequested→create_consent+审批回灌;#6a 下发权威 relations/state 快照(按 principal 过滤) |
| **G-ASSETS** | server/src/{assets,admin_api} + migration 0009 | #11 发布可审计 manifest;#10a audit-queue 返回卡片全文+同作者历史 |
| **G-BILLING** | server/src/{billing(feature),auth} | #2 年龄声明入口 + 保守拒充(未成年 default 也拦) |
| **G-CLIENT** | src/ | #1 互换测试 DTO 修复(从 settings 取 profile+prompt,{request} 包裹);#8 压力测试 UI;#5 势力地图 L1;#6b 关系图谱消费权威 relations |
| **G-ADMIN** | admin/ | #9 后台前端 RBAC(按 dev-login 返回 role 收敛可见模块);#10b 审核台展示卡片全文 |

## 跨层契约

**#3 同意门控(G-ENGINE↔G-RUNTIME)**:
- G-ENGINE:①分类不可逆结果(角色死亡/永久退场/永久关系变更——由仲裁结果+行动语义判定);②对未获批的不可逆结果产出 `DomainEvent{type:ConsentRequested, fact:{eventKind, subjectCharacterIds, detail}, visibility:private→当事角色}`,并**不把该不可逆 StatePatch 落定**(在 state 记 `narrative.pendingConsents:[{subject,eventKind}]`);③`RoundInput` 增 `approved_consents:Vec<String>`(已批准的 subject),命中则本回合可落定该不可逆结果。保持 136 绿 + 新测试(pending→不落定;approved→落定)。
- G-RUNTIME:tick 消费 ConsentRequested 域事件→`consents::create_consent(permanent_exit/death, subjects)`;下一 tick 把 approved 的 consent subject 经 RoundInput.approved_consents 回灌引擎。

**#6 权威关系/状态(G-RUNTIME↔G-CLIENT)**:
- G-RUNTIME:新增 `GET /worlds/{id}/state-summary` → `{relations:[{from,to,trust,affinity,fear,debt}], characters:[{id,arcStage,activity}]}`,从 narrative_state 派生,**按 principal 过滤**(只出 viewer 可见的关系:from==自己角色 或 known_to 含之;公共 world 层)。
- G-CLIENT:WorldRoom 关系图谱/状态面板改用此端点的权威数据(替换事件共现启发式)。

**#10 审核卡片(G-ASSETS↔G-ADMIN)**:
- G-ASSETS:admin_api audit-queue 详情返回 `cardJson` 全文 + 同 owner 历史发布列表。
- G-ADMIN:审核台详情抽屉展示卡片全文 + 同作者历史。

## 验证
各 agent 自域测试绿 + 不回归;跨 feature 的用 `--features billing,arena`。全部落地后主循环全栈复验 + 二进制冒烟。
