# P4a 整体重审 — Triage 与加固计划

> 三份只读审查(R1 运行时核心 / R2 交互安全 / R3 章节房+前后台)综合。基线 server 86 测试绿。
> **总体结论**:P4a 安全主干健全——服务端权威、同意保守超时、事件按 principal 双层硬隔离、机审分层——**无 Critical、无真正跨用户隐私泄漏或资金逻辑错**。但存在一批 High 级的**联编/并发/成本/额度/注入**缺陷,必须在配置真实模型运营前修复。R1 的最关键发现:**86 测试全部在"无模型跳过"处提前返回,从未执行 run_round——测试绿掩盖了引擎集成未真正联编**。

## 分域问题清单

### HA — runtime + worlds + muse-engine 联编(最高优先,阻塞真实运行)
| 编号 | 级别 | 来源 | 位置 | 问题 → 修复 |
|---|---|---|---|---|
| E-1 | **High/BLOCKER** | R1 | runtime:431,418-427 | run_id 逐 tick 变 + 引擎 FS 状态从不 seed + RoundInput 无状态回灌 + skeleton 硬节点/禁止谓词不注入 → 真实模型第一 tick 即 `store.load` fail-closed 暂停。**修:run_id 稳定到 world 粒度;建房/首 tick 用 assembled_json/skeleton seed 引擎 FS + store.init;DB narrative_state_json ↔ 引擎 FS 单一事实源回灌;补真实 mock 模型路由的集成测试走完整 run_round/commit** |
| C-1 | High | R1 | runtime:287-299,131-145 | tick 无原子认领 + 每轮询无条件 re-enqueue pending → 长回合被多 worker 重复跑 run_round(~2× 重复计费)。修:认领 `UPDATE status='running' WHERE status='pending'` CAS,rows=0 跳过;re-enqueue 只补偿超时 pending |
| C-2 | High | R1 | runtime:505-508 | cas_conflict 让 tick 停 pending 非终态 → 无限 re-enqueue + 无限重跑。修:cas_conflict 标终态 |
| B-1 | High | R1 | runtime:488,engine | 计费用先验估算漏计输入 token。修:用 ModelClient 实测 input/output token 累计 |
| B-2 | Med | R1 | runtime:311-351,worlds:414 | cny 预算未强制 + 官方房默认 daily_token_budget=0 无上限。修:官方建房设非零预算 + cny 熔断 |
| C-4 | Med | R1 | worlds:302-319 | member_limit TOCTOU(唯一键按角色非人数)。修:唯一计数约束/FOR UPDATE/串行 |
| C-9 | Med | R1 | runtime:583 | worker Err 仅日志不终态化无退避。修:重试上限+退避+终态 |
| Q-3 | Med | R1 | runtime:527 | 消费面>使用面:长回合新 accepted whisper 被标 applied 但从未投递(静默丢失)。修:只消费本 tick 实际喂入的干预 id |

### HB — safety + assets(注入防线 + 双写)
| S-1 | **High(情境 Critical)** | R2 | safety:59-81 | 注入检测精确子串黑名单实测可绕过(零宽/全角/标点/多空格/同义/同形字/JSON 分段)且误伤反派/背景卡。修:Unicode NFKC 归一化+去零宽+折叠空白+同形字映射;在语义拼接文本(非序列化 JSON)检测;句式判别(第一人称祈使 vs 第三人称叙述)降误伤;黑名单降为辅助信号 |
| S-2 | High | R2/S1 | assets:156,176-192 | assets↔safety 双写 audit_queue+risk(命中卡 2 条 open+2 条 risk)。修:确立单一写入方——safety::moderate_and_queue 为唯一入队/记险方,assets 不再自行入队/记险 |

### HC — interventions + consents(额度 + 同意)
| Q-1 | High | R2 | interventions:171-180 | 额度复位唯一点是 commit_tick,no_model/单人/blocked/failed 不提交 → 用户满 3 条后永久 rejected("quota")。修:额度按时间窗/tick 序号分桶,与提交解耦 |
| Q-2 | Med | R1/R2 | interventions | item 干预被受理计额度但 runtime 从不消费(空操作)。修:受理前明确 unsupported 拒绝且不计额度(P5 接线前) |
| S-4 | Med | R2 | interventions:112-131 | 合法 sealed/consumed 物品被判 forged_state 污染风控。修:sealed/consumed 返回良性 BadRequest 不记险 |
| C-5 | Med | R2 | consents:225-250 | respond 读改写无事务丢更新(失败保守但审计失真)。修:事务+FOR UPDATE 或响应落独立表 UNIQUE(consent_id,subject) |

### HD — notifications + reports(通知 + 日报)
| C-6 | Med | R2 | notifications:47-56 | dedupe_key 无唯一索引 TOCTOU。修:唯一索引靠约束去重 |
| N-1 | Med | R2 | notifications:154-165 | 无 pending 恢复重扫,失败即孤儿+重启丢失。修:失败重推+启动/定时重扫 pending due |
| N-2 | Med | R2 | notifications:96-104 | 全局退订压制 consent 类(不可逆事件)通知。修:consent 类豁免或按类别分级 |
| N-3 | Med | R2 | reports:143-157 | private 兜底 `.or_else(first())` 潜在跨 principal 泄漏(当前不泄漏)。修:无匹配返回 None 不回退 |
| N-4 | Med | R2 | reports:89-95 | 日报未按角色隔离(一人多角色内容相同)。修:按 character 参与度过滤或文档明确用户级 |

### HE — chapters + assembly + backpack + admin_api(章节并发 + 准入 + 后台权限)
| C-3 | High | R3 | chapters:183-232 | 章节 finish 幂等在并发/崩溃失效 → 资产复制。修:finish 包事务+state_revision CAS;backpacks 加 (user_id,reward_hook_key) 唯一约束下沉幂等 |
| S-3 | Med | R3 | assembly:218-241 | 装配只拦 Rejected,Pending(含注入命中)仍嵌入并钉住。修:Pending 也跳过钩子/挂起装配至复核 |
| S-5 | Med | R3 | backpack:263-267 | 转译降档只进响应未持久化(潜在强度后门)。修:per-carry 降档值持久化到 backpacks 新列,仲裁读覆盖值 |
| S-6 | Med | R3 | auth:80-91 | AdminUser 五角色同权无最小权限。修:role→action 矩阵 |
| C-7 | Med | R3 | chapters:113-125 | 首次装配无并发保护重复装配。修:CAS 占位/仅当 assembly null 写 |
| L-* | Low | R3 | 多处 | delete 工单占位标记完成(合规风险,真实实现前保持 pending)、governance 激活非原子、create_world visibility/status 校验、carry 缺成员/世界态校验 |

## Migration 版本号分配(避免撞号)
HA→0002 / HC→0003 / HD→0004 / HE→0005。各 agent 加自己版本号的 migration 文件。HB 无 schema 变更。

## 加固后验证
每个 agent 跑 `cd server && cargo test` 自域绿 + 不回归他域。HA 必须新增配置 mock 模型路由、走完整 run_round→commit 的集成测试(补上 86 测试的最大盲区)。

## ✅ 加固完成(2026,全栈复验)
五个加固 agent(HA-HE)全部完成 + 主循环集成收尾(修 chapters/tests.rs E0716)。全栈复验:
- **muse-engine 136 passed**(引擎零改动,HA 全用 public API 在 server 侧联编)
- **server 126 passed**(基线 86 + 40 加固测试,含 HA 补的真实 run_round 集成测试)
- **src-tauri 编译通过**、**前端 tsc 0 错误**
- migration 0002(tick)/0003(consent_responses)/0004(notification dedupe)/0005(chapter) 无撞号

核心成果:引擎真正联编(run_id 稳定/DB↔FS 回灌/skeleton seed 硬节点)、真实 tick 首次被测试覆盖、tick 原子认领+终态化、实测 token 计费+非零预算、注入检测重做、机审双写去重、额度解耦、章节资产复制三重防线、后台最小权限矩阵、通知崩溃恢复、日报角色隔离。

## ⚠️ 重审发现的功能缺口(非加固 bug,待后续)
1. **同意机制触发源未接通**:consents 状态机/API/独立表/runtime 清理全就绪,但引擎 run_round/arbiter/continuity **从不产生 `ConsentRequested` 域事件**——不可逆行动(死亡/永久关系)的同意门控在引擎里没实现。需在 arbiter/continuity 检测不可逆行动→产生事件→门控落地直到同意→runtime create_consent。**与 P6 赛事房(淘汰赛制最需要不可逆同意门控)一起设计**。
2. **unicode-normalization 依赖**(HB 建议):当前手写 NFKC-lite 覆盖已知绕过面,生产启用真正 NFKC 可加此依赖(可选)。
3. 势力地图 L1 卡、L2/L3 视觉呈现、回访指标聚合等前端/运营增强。
