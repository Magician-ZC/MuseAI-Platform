//! 运行时开关体系（`docs/VALIDATION.md` §0.1「未验证功能默认关闭」的基础设施层）。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 这个模块解决什么问题
//! ════════════════════════════════════════════════════════════════════════════
//!
//! §0.1 原文写着运行时开关体系「**列入 R1 开发**」，但它既不在总规格 §19 的 R1 清单里，
//! 也不在 VALIDATION §3 台账初版里——**两边都漏了**（§3.1 已补记）。本模块补齐它。
//!
//! 补齐的必要性不是形式上的。现有 9 个开关**全部是 env 进程级**：
//!
//! | env 开关 | 默认 | 归属模块 |
//! |---|---|---|
//! | `MUSE_ONBOARDING`            | 关 | `onboarding`（新手动线） |
//! | `MUSE_SUBPLOT_CARDS`         | 关 | `subplot`（副本卡） |
//! | `MUSE_LETHALITY_DEATHMATCH`  | 关 | `worlds`（生死状档） |
//! | `MUSE_ROOM_INVITATIONS`      | 关 | `invitations`（房间邀请） |
//! | `MUSE_CONTAINER_ASSEMBLY`    | 关 | `assembly`（自定义房装配） |
//! | `MUSE_MEMORIAL`              | 关 | `memorial`（传世卡·遗作馆） |
//! | `MUSE_WORLD_SERIES_AUTOSCALE`| 关 | `worlds`（世界系列扩容） |
//! | `MUSE_WORLD_BE_BIOGRAPHY`    | 关 | `progression`（BE 结局传记） |
//! | `MUSE_SAFETY_LEXICON`        | **开** | `safety`（运行时敏感词库·审核链） |
//!
//! 它们只有**全开/全关两态**：不能按世界灰度、不能按用户灰度、改一次要重启进程、
//! 运营在后台点不了。而 VALIDATION §2 的每个阶段都要求「开放范围」可控——
//! T0「邀请制 ≤100 人」、T2「3-6 人世界」、T3「订阅制灰度」。
//! **两态开关做不到分阶段开闸，于是整个 T0-T5 验证计划悬空。**
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 建这套体系的那一批**刻意不迁移任何现有开关**（历史，已完成）
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 一次把 9 个开关全搬进来是一次巨大的行为变更：每个模块的「前门拒绝 + 读取侧降级」
//! 语义各有细节（有的关掉是端点 404，有的是读取侧降级，有的是结算期跳过而非报错），
//! 批量改必然出错。所以那一批只交付**基础设施 + 一条参考接线**（`MUSE_ONBOARDING`，
//! 语义最简单：四个端点统一 404），其余留成本文件末尾 `MIGRATION_NOTES` 里的待办清单。
//!
//! ✅ **该清单已于 2026-07-27 走完**：可迁的 7 个逐个迁完（每个一次提交、各带回归与灰度用例、
//! 各做故障注入），`MUSE_SAFETY_LEXICON` 按原注的判断**有理由地保留为纯 env**。
//! 清单里的每条注意事项都留在原处并标注了「预判对没对」——三条被证伪
//! （两处「事务边界」其实不需要、一处「按人灰度」会造出半开的功能），
//! 留着比删掉有用：它说明**这类清单本身也会过期，动手前要按当前代码复核一遍**。
//!
//! 迁完之后这套体系的实际形态（每个开关的作用域**不是统一的**，各有理由）：
//!
//! | 作用域 | 开关 | 定这个档的理由 |
//! |---|---|---|
//! | user + global | `MUSE_ONBOARDING` · `MUSE_ROOM_INVITATIONS` | 动作发起人就是被灰度的人 |
//! | world + global | `MUSE_LETHALITY_DEATHMATCH`（读取侧） · `MUSE_WORLD_BE_BIOGRAPHY`（两侧） | 被灰度的是「这个世界」 |
//! | 两处不同 | `MUSE_SUBPLOT_CARDS` · `MUSE_MEMORIAL`（端点 user / 结算 world） · `MUSE_CONTAINER_ASSEMBLY`（装配 world / 建模板 global） | 一侧结构上拿不到另一侧的维度（建房/建模板时世界还不存在；结算是一个世界事件、多个卡主） |
//! | 只有 global | `MUSE_WORLD_SERIES_AUTOSCALE` | **主动放弃灰度**：逐系列的闸已经是 `world_series.status`，再加一档就是第三道容易被忘记的闸 |
//! | 不迁（纯 env） | `MUSE_SAFETY_LEXICON` | 审核链、默认开启、消费点在 tick 事务内的闸上：事务里查库风险最大、收益最小 |
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 解析链：**按用户 > 按世界 > 全局 > env > 代码内默认值**
//! ════════════════════════════════════════════════════════════════════════════
//!
//! ```text
//!   is_enabled(db, "MUSE_ONBOARDING", FlagCtx::user("usr_1"))
//!        │
//!        ├─① runtime_flags(flag, scope='user',   target_id='usr_1')  ── 命中且在窗口内 → 用它
//!        ├─② runtime_flags(flag, scope='world',  target_id=<ctx.world_id>) ── 同上
//!        ├─③ runtime_flags(flag, scope='global', target_id='')       ── 同上
//!        ├─④ env `MUSE_ONBOARDING`（**现有语义逐字保留**）
//!        └─⑤ KNOWN_FLAGS 里声明的默认值
//! ```
//!
//! 🔴 **env 是兜底，不是被替代**。`runtime_flags` 表为空时（迁移后的初始状态即为空），
//! 解析必然落到 ④/⑤，于是**所有现存模块行为逐字节不变**。开闸是显式写入数据，
//! 不是升级的副作用。回归保护用例：`tests::empty_db_reproduces_env_semantics_exactly`。
//!
//! 🔴 **窄的赢**。运营给一个世界开了灰度之后，还要能对其中一个捣乱的用户单独关掉；
//! 反过来（宽的赢）会让细粒度记录永远无效，这套体系就退化回两态开关了。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 🔴 fail-closed：异常一律按「安全值」处理
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 查库失败 / 记录损坏（scope 非法、enabled 非 0-1、时间窗反转/负数）→
//! **立即返回该开关声明的默认值，且不再继续回落到 env**。
//!
//! 「不再回落」是刻意的：若损坏记录被静默跳过，「配坏了」就变成了「按 env 开着」——
//! 一个本该报警的状态被降级成了正常状态。宁可整个开关退回默认值（对未验证功能就是关），
//! 也不要让损坏数据决定用户能看到什么。
//!
//! ⚠️ **「安全」永远指向不扩大用户可见范围的那一侧，不是字面的 `false`**。
//! `MUSE_SAFETY_LEXICON` 默认为**开**：它是审核链，关掉它等于放行敏感词。
//! 因此 fail-safe 值统一取 `FlagDef::default_enabled`——对未验证功能是 `false`（关），
//! 对审核链是 `true`（继续过滤）。红线用例 `red_line_only_safety_chain_defaults_on` 锁死这一点。
//!
//! ════════════════════════════════════════════════════════════════════════════
//! 缓存：整表快照 + 短 TTL + 写入侧立即失效
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 开关会被高频调用（每次请求、每个 tick），不能每次都查库。方案是两层：
//!
//! - **进程内整表快照**（`Snapshot`）：`runtime_flags` 是极小的表（全局记录 ≤ 开关数；
//!   世界/用户灰度在 T0「≤100 人」量级也就百行），整表装进 `HashMap` 后解析是纯内存操作。
//! - **短 TTL**（`MUSE_FLAGS_CACHE_TTL_MS`，默认 5000ms）：兜住**多进程**部署下
//!   别的实例改了开关这一情形，最坏 5 秒收敛。
//! - **写入侧立即失效**（`invalidate`）：admin 端点每次写完就清本进程快照，
//!   于是**运营点完开关，本进程下一次读取即生效**（不是等 TTL）。
//!
//! 「运营点了开关不能等半小时才生效」这条要求由后两者共同保证：单进程 0 延迟，
//! 多进程 ≤ TTL。要更强的跨进程实时性需要 pub/sub（Redis/PG NOTIFY），
//! 本批次不引入新依赖——TTL 可调到 1000ms 甚至 0（0 = 禁用缓存，每次查库）。
//!
//! 缓存按**连接池身份**分桶（`pool_key`），因为测试里每个用例各建一个
//! `sqlite::memory:` 池而进程只有一个——不分桶会串味。`#[cfg(test)]` 下 TTL 默认 0
//! （禁用缓存），只有专门的缓存用例显式打开它。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use sqlx::{AnyPool, Row};

use crate::db::now_ms;

#[cfg(test)]
mod tests;

// ═══════════════════════════════════════════════════════════════════════════
// 开关登记表（白名单）
// ═══════════════════════════════════════════════════════════════════════════

/// 作用域字面量（落库值）。
pub const SCOPE_USER: &str = "user";
pub const SCOPE_WORLD: &str = "world";
pub const SCOPE_GLOBAL: &str = "global";

/// 合法作用域，**按解析优先级从窄到宽排列**。解析链直接遍历本数组，
/// 顺序即优先级——将来加作用域（如按世界模板、按渠道）只需在正确位置插入一项。
pub const SCOPES_BY_PRIORITY: &[&str] = &[SCOPE_USER, SCOPE_WORLD, SCOPE_GLOBAL];

/// 一个开关的静态声明。**开关名即 env 变量名**（不另造命名空间：运营后台看到的名字、
/// `.env` 里写的名字、报警里出现的名字是同一个，排查时不需要在两张词表间做翻译）。
#[derive(Debug, Clone, Copy)]
pub struct FlagDef {
    /// 开关名 = env 变量名。
    pub name: &'static str,
    /// 代码内默认值：env 未设置 **或** 设了但解析不了时的取值。
    /// 同时也是 **fail-safe 值**（查库失败 / 记录损坏时返回它）。
    pub default_enabled: bool,
    /// 归属模块（运营后台展示 + 迁移时定位代码）。
    pub owner: &'static str,
    /// 一句话说明（运营后台展示）。
    pub desc: &'static str,
    /// 是否已接入本体系（参考接线）。`false` = 该开关目前仍是纯 env，
    /// 本表登记它只为运营后台可见 + 作为迁移清单，**写记录对它暂时无效**。
    pub wired: bool,
    /// 🔴 **这个开关的消费点实际会解析哪几档作用域**（不是「合法的作用域有哪些」）。
    ///
    /// 为什么要有这一列：写入端点原先只校验「作用域名是不是合法的三个之一」，
    /// 于是给一个只读 global 的开关写一条 `world` 记录会**写得进去、且毫无效果**——
    /// 而那正是本模块自己的注释点名的「这套体系最难自查的失败模式」
    /// （见 `admin_api::flags::set_flag` 里「配了但毫无效果」那段）。
    /// 有了这一列，写入端点能直接 400 并告诉运营这个开关到底读哪几档。
    ///
    /// 🔴 **必须含 `SCOPE_GLOBAL`**：全局档是每个开关的兜底面，不给它就没法平台级开关。
    /// 由红线用例 `every_flag_declares_scopes_including_global` 钉死。
    ///
    /// ⚠️ 它描述的是**代码现状**，不是愿望：加一档之前先去改消费点，否则这一列会变成
    /// 另一个「写下时是对的、之后每批开发让它更不对」的清单。
    pub scopes: &'static [&'static str],
}

/// 三档全开（user > world > global）：消费点在三个位置都能拿到对应维度。
const SCOPES_ALL: &[&str] = &[SCOPE_USER, SCOPE_WORLD, SCOPE_GLOBAL];
/// 按人 + 全局：消费点只有「动作发起人」这一个维度。
const SCOPES_USER: &[&str] = &[SCOPE_USER, SCOPE_GLOBAL];
/// 按世界 + 全局：消费点只有「哪个世界」这一个维度。
const SCOPES_WORLD: &[&str] = &[SCOPE_WORLD, SCOPE_GLOBAL];
/// 只有全局：**刻意不给灰度维度**（各自的理由写在对应模块的开关函数文档里）。
const SCOPES_GLOBAL: &[&str] = &[SCOPE_GLOBAL];

/// 全部已知开关。写入端点按本表校验开关名（未登记 → 400），解析按本表取默认值。
///
/// 🔴 **除 `MUSE_SAFETY_LEXICON`（审核链，关掉等于放行敏感词）外，全部 `default_enabled: false`。**
/// 这是 VALIDATION §0.1 的代码化，由红线用例 `red_line_only_safety_chain_defaults_on` 锁死：
/// 任何人把某个未验证功能的默认值改成 true，测试立刻红。
pub const KNOWN_FLAGS: &[FlagDef] = &[
    FlagDef {
        name: "MUSE_ONBOARDING",
        default_enabled: false,
        owner: "onboarding",
        desc: "新手动线（新人礼包 + 单人微本）。T0 被测对象",
        // 🔴 本批次唯一的参考接线。
        wired: true,
        // 解析档：四个端点都按动作发起人解析；微本世界是领礼包时才建的，判定发生在世界存在之前。
        scopes: SCOPES_USER,
    },
    FlagDef {
        name: "MUSE_SUBPLOT_CARDS",
        default_enabled: false,
        owner: "subplot",
        desc: "副本卡（结算铸卡 + 同星合成）",
        // 🔵 已接线。🔴 **两处 ctx 口径故意不同**：端点侧（读/合成）按 **user**，
        // 结算铸卡按 **world**——结算是一个世界事件、多个卡主，按人解析就得在事务里逐 owner
        // 查库，而那正是单连接池自锁的来源。口径表见 `subplot::subplot_cards_enabled`。
        wired: true,
        // 解析档：端点侧按 user、结算铸卡按 world。
        scopes: SCOPES_ALL,
    },
    FlagDef {
        name: "MUSE_LETHALITY_DEATHMATCH",
        default_enabled: false,
        owner: "worlds",
        desc: "生死状档（读取侧降级：关闭时既有生死场立即降为同意制）",
        // 🔵 已接线。🔴 **两处 ctx 口径故意不同**：读取侧（join 契约门 / 引擎回灌 / 列表详情投影 /
        // if 线）按 **world**；建房前门只能按 **global**——建房那一刻世界还不存在。
        // 于是「全局关、某世界单独开」时该世界的契约照常生效、而新的生死场建不出来，
        // 两者都对（回答的是两个问题）。口径表见 `worlds::deathmatch_enabled`。
        wired: true,
        // 解析档：读取侧按 world；建房前门只能 global（那时世界还不存在）。
        scopes: SCOPES_WORLD,
    },
    FlagDef {
        name: "MUSE_ROOM_INVITATIONS",
        default_enabled: false,
        owner: "invitations",
        desc: "房间邀请（接受邀请只点亮引导入口，入场仍走 join 全部校验）",
        // 🔵 已接线（范式抄 MUSE_ONBOARDING）。🔴 作用域只取 **user / global，刻意不含 world**：
        // 收件侧（`GET /me/invitations`）是跨世界的、结构上没有 world 可传，允许 world 作用域
        // 会让「给某个世界单独开闸」产出**一封谁都答不了的邀请**（发件侧开、收件侧关）。
        // 理由全文见 `invitations::invitations_enabled` 的「灰度粒度」一节。
        wired: true,
        // 解析档：🔴 刻意不含 world：收件侧跨世界，world 档会造出一封谁都答不了的邀请。
        scopes: SCOPES_USER,
    },
    FlagDef {
        name: "MUSE_CONTAINER_ASSEMBLY",
        default_enabled: false,
        owner: "assembly",
        desc: "自定义房容器装配（副本卡的消费端，无独立端点）",
        // 🔵 已接线。ctx 两处不同：装配期按 **world**（世界已存在，「先在一个房试」是自然单位），
        // 建模板前门只能按 **global**（建模板时没有世界，模板是世界的蓝图）。
        // 两侧不同在这里不会半开半关——全局关时模板根本声明不了 refs。
        wired: true,
        // 解析档：装配期按 world；建模板前门只能 global（那时没有世界）。
        scopes: SCOPES_WORLD,
    },
    FlagDef {
        name: "MUSE_MEMORIAL",
        default_enabled: false,
        owner: "memorial",
        desc: "传世卡 · 遗作馆（关闭时端点 404 且不发生任何封卷）",
        // 🔵 已接线。ctx 口径同副本卡：端点侧按 **user**，结算内自动封卷按 **world**
        // （经 `progression::SettlementFlags`）。口径表见 `memorial::memorial_enabled`。
        wired: true,
        // 解析档：端点侧按 user、结算内自动封卷按 world。
        scopes: SCOPES_ALL,
    },
    FlagDef {
        name: "MUSE_WORLD_SERIES_AUTOSCALE",
        default_enabled: false,
        owner: "worlds",
        desc: "世界系列自动扩容（1 号满员开 2 号）",
        // 🔵 已接线，但**只有 global 档**——这是这批里唯一主动放弃灰度粒度的一个：
        // ① 系列是一串世界实例，按世界灰度会让同一系列半开半关（1 号能指到 2 号、
        //    2 号却开不出 3 号）；② 逐系列的闸已经存在（`world_series.status`），
        // 再加一档就是第三道语义重叠、最容易被忘记的闸。🔴 两道闸都开才扩容。
        wired: true,
        // 解析档：🔴 主动放弃灰度：逐系列的闸已经是 world_series.status。
        scopes: SCOPES_GLOBAL,
    },
    FlagDef {
        name: "MUSE_WORLD_BE_BIOGRAPHY",
        default_enabled: false,
        owner: "progression",
        desc: "BE 结局传记（世界线崩塌后的封卷）",
        // 🔵 已接线。🔴 与副本卡/传世卡不同：**两侧 ctx 都是 world**，没有 user 那一档——
        // 传记是公共事实（§0.3）不是个人资产，按人灰度会出现「同一份封卷 A 看得见 B 看不见」。
        // 于是它没有那两个开关的「产出了但看不见」不对称。封卷侧经 `SettlementFlags`。
        wired: true,
        // 解析档：两侧都按 world：传记是公共事实，不是个人资产。
        scopes: SCOPES_WORLD,
    },
    FlagDef {
        name: "MUSE_OOC_ANNOTATIONS",
        default_enabled: false,
        owner: "annotations",
        desc: "OOC 注解权（单拍申诉 + 私人批注 + 复核补偿托梦配额）。总规格 §7 人设保险第 2 级；\
               同时是 VALIDATION §4.2「OOC 申诉率」SLO 的唯一数据源与 T1 门槛的测量手段",
        // 🔵 本模块**从一开始就经本体系解析**（不是先 env 再迁移）：它是 R3 新建件，
        // 没有需要保留的历史 env 语义，直接接线是最省事的一次。
        wired: true,
        // 解析档：解析函数收 user_id + world_id 两个可选维度。
        scopes: SCOPES_ALL,
    },
    FlagDef {
        name: "MUSE_IFLINE_PARALLEL",
        default_enabled: false,
        owner: "ifline",
        desc: "if 线付费副本（世界结束后以终局为分叉点开单人平行线；烧副本卡换内容）。总规格 §7 人设保险第 3 级；\
               🔴 平行线不是改写——原世界线一个字节不动，且 if 线不产出任何可反哺原世界的资产",
        // 🔵 与 `MUSE_OOC_ANNOTATIONS` 同理：R3 新建件，没有需要保留的历史 env 语义，直接接线。
        wired: true,
        // 解析档：同上。
        scopes: SCOPES_ALL,
    },
    FlagDef {
        name: "MUSE_SOCIAL_IDENTITY_UNLOCK",
        default_enabled: false,
        owner: "social",
        desc: "真人社交解锁（正向羁绊线达阈值后双向自愿互揭真身）+ 拉黑 / 举报队列。总规格 §14【拍板 22】恨隔面具原则；\
               🔴 敌对线永久匿名 · 青少年模式服务端拒绝 · 独有社交资产「我们的角色一起死过」是关系凭证不是数值",
        // 🔵 与 `MUSE_OOC_ANNOTATIONS` / `MUSE_IFLINE_PARALLEL` 同理：R3 新建件，
        // 没有需要保留的历史 env 语义，从建成之日起就经本体系解析。
        wired: true,
        // 解析档：同上。
        scopes: SCOPES_ALL,
    },
    FlagDef {
        name: "MUSE_LIVE_STAGE",
        default_enabled: false,
        owner: "livestage",
        desc: "直播场（定档场次 + 延迟缓冲 + 弹幕）。总规格 §2 场次节奏三档「直播场」+ §15 第 4 层\
               「直播场延迟 1-2 拍缓冲」；同时是 VALIDATION §2 T5 门槛「观众→玩家转化 ≥2%」的\
               唯一数据源（`live_viewers`）。🔴 延迟缓冲是内容安全机制不是体验设计——延迟拍数\
               `MUSE_LIVE_DELAY_TICKS` 是 T5 预案「审核成本失控 → 直播延迟拍数上调」的运营旋钮",
        // 🔵 与 OOC 注解权 / if 线 / 真人社交解锁同理：R3 新建件，没有需要保留的历史 env 语义，
        // 从建成之日起就经本体系解析。
        wired: true,
        // 解析档：同上。
        scopes: SCOPES_ALL,
    },
    FlagDef {
        name: "MUSE_OFFPEAK_SCHEDULING",
        default_enabled: false,
        owner: "runtime",
        desc: "错峰调度（成本工程杠杆①）：连载/慢炖场的 tick 优先排进折扣时段，窗口内压缩间隔以保住每日拍数；\
               🔴 直播场永不延后 · 🔴 防饿死兜底恒有限、首拍绝不延后",
        // 🔵 与上面几条不同：本开关**有历史 env 语义**（登记前一直走 `env_bool` 兜底）。
        // 登记后语义仍然连续——env 是解析链第 ④ 层，只是前面多了 user/world/global 三层，
        // 于是错峰从「全局一刀切」变成可按世界灰度。`runtime/mod.rs` 的
        // `offpeak::enabled_for_world` 早已写好「已登记走体系、未登记退 env」的分支，故那边一行不用改。
        wired: true,
        // 解析档：错峰是按世界排的（runtime::offpeak 只传 world）。
        scopes: SCOPES_WORLD,
    },
    FlagDef {
        name: "MUSE_SAFETY_SEMANTIC_RECHECK",
        // 🔴 与下面的 `MUSE_SAFETY_LEXICON` 同属审核链，默认值却相反——不是矛盾，是同一条原则：
        // **默认值指向「不改变现状」的那一侧**。词库闸**已经在线上跑着**，它的 default-on 保护的是
        // 「别让一次配置失误把既有过滤悄悄关掉」；第 3 层**从未生效过**，把一条从未跑过的链路
        // 默认开启，等于让「合并代码」直接改变线上行为并开始烧 token —— 那正是 §0.1 禁止的。
        // ⚠️ 等它接了真实 provider 并验证过，这个默认值应当重新评审为开。
        default_enabled: false,
        owner: "safety",
        desc: "语义分类异步复核（总规格 §15 运行时内容安全**第 3 层**）：tick 提交后、\
               **事务之外**对本拍投影跑 `ModerationProvider::check_text`，非 Approved 时把 \
               `world_events.moderation` 从 approved **收紧**（正文一个字节不改，§0.3）。\
               公开投影全量 + 私有投影确定性抽样；配合 §15 第 4 层直播场延迟缓冲作为拦截窗口。\
               🔴 **接通 ≠ 生效**：`ModerationProvider` 当前唯一实现是 Dev 桩，真实语义分类一次都没发生；\
               「当前是桩」随 `safety_recheck_runs.provider_stub` / 每条 risk_events / \
               `GET /admin/safety/recheck` 的 providerStub 一起走。不得据此表述为「五层漏斗已完整」",
        wired: true,
        // 解析档：按世界灰度是最自然的开闸单位；运营面读数走 global。
        scopes: SCOPES_WORLD,
    },
    FlagDef {
        name: "MUSE_SAFETY_RECHECK_SWEEP",
        // 🔴 默认关闭的理由比第 3 层本身**多一条**：轮询是这条链上唯一一处会「凭数据自发烧
        // token」的路径——它不需要有人推进世界就能发起送审。这类东西默认开着，
        // 等于让一次代码合并把成本曲线抬起来而没有人按过开关（§0.1 正是禁这个）。
        default_enabled: false,
        owner: "safety",
        desc: "第 3 层复核的**补偿轮询**（扫尾未复核拍）：按 `world_ticks ⋈ safety_recheck_runs` \
               对账，把「已落定、带 approved 事件、却没有终局复核行」的拍补投回复核队列。\
               闭合的是 `MemQueue` 不持久导致的投递丢失，**并且**覆盖持久队列覆盖不了的\
               「压根没入队」（开关当时关着 / tick 走 blocked·cas_conflict 分支没到入队那行）。\
               🔴 有真实覆盖上限：只回看 `MUSE_SAFETY_L3_SWEEP_LOOKBACK_MS`（默认 24h），\
               挂机超过这段的拍**永远补不回来**——`GET /admin/safety/recheck` 的 \
               `durability.justOutsideWindow` 把它量出来。🔴 单实例假设（多实例会重复补投）",
        // 🔵 新建件，无历史 env 语义，建成即接线。
        wired: true,
        // 解析档：它是一个跨世界的进程级循环，没有可解析的维度。
        scopes: SCOPES_GLOBAL,
    },
    FlagDef {
        name: "MUSE_SAFETY_LEXICON",
        // 🔴 唯一默认为「开」的开关：审核链。关掉它 = 放行敏感词，
        // 所以对它而言「安全的那一侧」是开着，fail-closed 返回 true 才是 fail-**safe**。
        default_enabled: true,
        owner: "safety",
        desc: "运行时敏感词库（审核链，默认开启；fail-safe 方向是「继续过滤」）。\
               🔴 只允许 **global** 作用域：按世界/按人关掉过滤不是运营动作，是内容安全事故。\
               迁进体系的首要收益不是灰度而是**留痕**——env 改一行就能关掉全平台过滤且无任何\
               审计记录，接进来后每次变更都落 audit_logs",
        // 🔵 已接线（2026-07-27）。闸收已解析好的 bool，由 `commit_tick` 在 `db.begin()`
        // 之前解析——事务里一次库都不查，`MIGRATION_NOTES` 原注的头号顾虑因此不成立。
        wired: true,
        // 解析档：🔴 审核链只允许平台级急停，按世界/按人关掉敏感词过滤不是合理运营动作。
        scopes: SCOPES_GLOBAL,
    },
    FlagDef {
        name: "MUSE_DISPOSAL_NAME_GATE",
        default_enabled: false,
        owner: "safety",
        desc: "被处置内容在读取面上的卡名解引用闸门（roster / 遗作馆 / 悼念名单 / 社交与邀请对手方名）。\
               🔴 与 `admin_api::takedown` 的处置能力**分属两件事**：处置能力是合规设施、恒开、不登记开关；\
               本开关只决定「已经露在存量世界里的名字要不要换成中性占位」——那会改变运行中世界的显示，\
               是产品决策，故默认关闭。🔴 闸门只作用于「现读现解 `card_json`」的展示面，\
               `world_events` / 已封卷传记快照一个字节不动（§0.3）",
        // 🔵 与 R3 那批新建件同理：新建件，没有需要保留的历史 env 语义，建成即接线。
        wired: true,
        // 解析档：闸门作用在「查看者看到什么」上，故按查看者解析。
        scopes: SCOPES_USER,
    },
];

/// 按名字查登记项。未登记 → None（调用方按 fail-closed 处理）。
pub fn find_flag(name: &str) -> Option<&'static FlagDef> {
    KNOWN_FLAGS.iter().find(|f| f.name == name)
}

/// `const fn` 版的默认值查询，供各模块做**编译期一致性断言**：
///
/// ```ignore
/// const _: () = assert!(
///     crate::flags::declared_default(ENV_ONBOARDING_ENABLED) == DEFAULT_ONBOARDING_ENABLED,
/// );
/// ```
///
/// 🔴 存在的意义：接线后一个开关的默认值出现在**两处**（本登记表 + 模块内原有常量）。
/// 两处不一致就是「默认关闭」这条 §0.1 约束有了两个事实源，而这种漂移在运行时几乎观察不到
/// （只有当 env 恰好没设、DB 恰好没记录时才暴露）。钉在编译期，改一处不改另一处直接编不过。
///
/// 未登记的名字返回 `false`（fail-closed 方向）。
pub const fn declared_default(name: &str) -> bool {
    // const 上下文里没有迭代器与 str::eq，手写按字节比较。
    const fn str_eq(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }
    let mut i = 0;
    while i < KNOWN_FLAGS.len() {
        if str_eq(KNOWN_FLAGS[i].name, name) {
            return KNOWN_FLAGS[i].default_enabled;
        }
        i += 1;
    }
    false
}

/// 其余未接线开关的迁移清单（代码内待办，随迁移逐条划掉；同步见 `docs/VALIDATION.md` §3.1）。
///
/// ⚠️ 此处**刻意不写死开关个数**：数字散落在模块头、本注释、`flags/tests.rs` 与
/// `docs/VALIDATION.md` 四处，历次加开关都漏改过其中几处。计数的唯一权威是
/// `KNOWN_FLAGS.len()`（由 `red_line_only_safety_chain_defaults_on` 钉住），文字描述不复述它。
///
/// 迁移一个开关的通用步骤：
///   1. 把模块内 `xxx_enabled()` 改成 `async fn xxx_enabled(db, ctx)`，内部调 `flags::is_enabled`；
///   2. 该模块的 `ensure_enabled()` 一并改 async，所有 handler 补 `State(state)` 与 `.await`；
///   3. `KNOWN_FLAGS` 里把 `wired` 改 true；
///   4. **保留模块内原有的 env 常量与 RAII 测试夹具**——env 仍是兜底，原有用例不动即为回归保护。
///
/// 逐个的注意事项：
///
/// - ~~`MUSE_SUBPLOT_CARDS`~~ —— **已迁（2026-07-27）**。原注两条预判全中：两个消费点语义不同
///   （端点 404 / 结算跳过不报错，`onboarding` 领礼包也依赖那个「跳过」），且结算路径在事务内、
///   必须「进事务前解析一次、把 bool 传进去」。
///   🔵 多做的一件事：那个 bool 不是裸参数，而是 [`crate::progression::SettlementFlags`]——
///   结算事务里挂着的开关不止一个（还有传世卡自动封卷、BE 传记），一个一个加 bool，
///   `settle_idle_world_ending_tx` 迟早变成七个 bool 的函数。
///   🔵 「事务里解析会自锁」现在有可执行证据：
///   `subplot::tests::resolving_flags_inside_the_transaction_deadlocks_and_fails_closed`。
/// - ~~`MUSE_LETHALITY_DEATHMATCH`~~ —— **已迁（2026-07-27）**。原注预判的两处口径差异全部命中：
///   读取侧按 **world**，建房前门只能按 **global**（建房那一刻世界还不存在），
///   于是「全局关但某世界开，却建不出那个世界」确实会发生——现已写进
///   `worlds::deathmatch_enabled` 的 ctx 口径表并有用例钉住，不再是「困惑」而是有据可查的规则。
///   🔵 原注没预判到的一条：`effective_lethality` **不能改成 async**。它的调用点里有列表投影的
///   循环体与引擎回灌，将来还可能被搬进结算事务——一旦它自己会查库，任何一次「顺手挪进事务」
///   都会在单连接池上自锁，而那种死锁在只跑内存 SQLite 的用例里不一定复现。
///   故改成**收一个已解析好的 bool**：调用点必须先 `.await` 一次 `deathmatch_enabled` 才拿得到它，
///   事务边界问题因此在编译期就摆到眼前。这条对下面剩余几个开关同样适用。
/// - ~~`MUSE_ROOM_INVITATIONS`~~ —— **已迁**（2026-07-27）。当初判断的「第二容易」成立：
///   四个端点统一 404，无事务边界问题，改动就是 `ensure_enabled` 变 async + 四个调用点传
///   发起人的 user_id。🔵 但迁的时候多发现一条当初没写到的约束：这个开关**不能有 world 作用域**。
///   原注只说了「ctx 用受邀人的 user_id」，而发件侧（`/worlds/{id}/invitations`）路径里是有
///   world 的，照着写很自然会顺手把 world 也传上——那样运营给某个世界单独开闸就会产出
///   **一封谁都答不了的邀请**：发件侧命中 world 记录（开），收件侧（`/me/invitations` 跨世界、
///   结构上没有 world）落到 global（关）。理由全文见 `invitations::invitations_enabled`。
/// - ~~`MUSE_CONTAINER_ASSEMBLY`~~ —— **已迁（2026-07-27）**。原注对消费点的判断全对：
///   建模板期（拒绝声明 `subplotCardRefs`）+ 装配期（忽略容器字段走原路径），
///   且建模板期确实只能用 global（那时没有世界，模板是世界的蓝图）。
///   🔵 装配期则**可以**按 world，故取 world 档——原注只说了建模板期那一侧。
///   ⚠️ 原注说「装配期在 `assemble_instance` 内，同样要注意事务边界」——**实际不需要**：
///   `load_container_cards` 的唯一调用点在 C-7 那次 CAS 占位写入**之前**，事务还没开。
///   `validate_container_refs` 收 bool 保持同步，是为了让它继续是一个**纯校验函数**
///   （可被任意上下文复用），而不是因为事务边界。
/// - ~~`MUSE_MEMORIAL`~~ —— **已迁（2026-07-27）**。端点 404 且封卷本身不发生；封卷在结算事务内，
///   故与副本卡共用 [`crate::progression::SettlementFlags`]（往里加一个字段，没有新增 bool 参数）。
///   ⚠️ 原注那条提醒仍然有效且**依然没被违反**：`MUSE_MEMORIAL_BOND_MIN` /
///   `MUSE_MEMORIAL_PAGE_SIZE` 是**参数化 env（非布尔）**，本体系只管布尔开关，
///   参数化配置是另一件事（§0.2），迁移时一个都没往 `runtime_flags` 里塞。
/// - ~~`MUSE_WORLD_SERIES_AUTOSCALE`~~ —— **已迁（2026-07-27）**。原注对「语义重叠」的提醒
///   直接决定了结论：它**只接 global 档，不给 world 档**。逐系列的闸已经是
///   `world_series.status`，再加一档 world 作用域就是第三道容易被忘记的闸；而且系列是
///   **一串**世界实例，按世界灰度会让同一系列半开半关（1 号能指到 2 号、2 号却开不出 3 号）。
///   🔴 两道闸都开才扩容，缺一不可。
///   ⚠️ 原注另一句「扩容判定在 join 的事务路径上」**是不准确的**，已改正：
///   `ensure_next_series_instance` 由 `world_full_conflict` 调用，那是撞满员后的 **409 构造
///   路径**，join 的事务此时还没开。照着原注去做一次 bool 穿透是白费功夫。
/// - ~~`MUSE_WORLD_BE_BIOGRAPHY`~~ —— **已迁（2026-07-27）**。封卷路径的事务边界注意事项同上，
///   经 [`crate::progression::SettlementFlags`] 传入（第三个字段）。
///   🔵 它是这批里唯一**两侧 ctx 同档**（都按 world）的：传记是公共事实不是个人资产，
///   于是没有副本卡/传世卡那种「产出了但看不见」的不对称。
///   ⚠️ 「关阀期间崩塌的世界不产传记，再打开也不追溯补写」这条语义**一个字没变**——
///   传记是封卷那一刻的快照，补写会把「当时的事实」换成「今天重算的事实」。
/// - ~~`MUSE_SAFETY_LEXICON`~~ —— **已迁（2026-07-27）**，但**理由与别的开关都不同**。
///   原注判它「最后迁或干脆不迁」，依据：① 消费点在 commit 事务内的闸上，事务内查库风险最大；
///   ② 收益最小（「按世界灰度关掉敏感词过滤」不是合理的运营动作）。重新评估后仍然迁：
///   - ① **已被解掉**：闸收已解析好的 `bool`，`commit_tick` 在 `db.begin()` 之前解析，
///     事务里一次库都不查。这套做法是迁前面几个开关时才成型的，原注写下时还没有。
///   - ② **收益被低估**：原注只想到「灰度」。而对一个**审核链的急停阀**来说最重要的收益是
///     **留痕**——env 改一行就能关掉全平台的敏感词过滤，**没有任何审计记录**；
///     接进体系后每次变更都落 `audit_logs`。🔴「谁在什么时候关掉了内容过滤」必须查得到。
///   原注的两个前提逐字执行：只允许 global（`scopes: SCOPES_GLOBAL`，写入端点直接 400）+
///   单独红线用例 `lexicon::tests::red_line_lexicon_never_fails_open`。
pub const MIGRATION_NOTES: &str =
    "迁移清单已于 2026-07-27 走完：登记表里的存量 env 开关全部接入本体系（wired=true）。\
     本常量与上方文档注释保留为**已完成的施工记录**——每条注意事项都标注了当初的预判\
     对没对，其中三条被证伪。留着是因为它说明：这类清单本身也会过期，动手前先按当前代码复核";

// ═══════════════════════════════════════════════════════════════════════════
// 解析上下文
// ═══════════════════════════════════════════════════════════════════════════

/// 解析上下文：这次询问是「对谁」。字段全部可选——都不给就是纯全局解析。
#[derive(Debug, Clone, Copy, Default)]
pub struct FlagCtx<'a> {
    pub user_id: Option<&'a str>,
    pub world_id: Option<&'a str>,
}

impl<'a> FlagCtx<'a> {
    /// 纯全局（无用户、无世界）。
    pub fn global() -> Self {
        Self::default()
    }
    /// 按用户。
    pub fn user(user_id: &'a str) -> Self {
        Self { user_id: Some(user_id), world_id: None }
    }
    /// 按世界。
    pub fn world(world_id: &'a str) -> Self {
        Self { user_id: None, world_id: Some(world_id) }
    }
    pub fn with_user(mut self, user_id: &'a str) -> Self {
        self.user_id = Some(user_id);
        self
    }
    pub fn with_world(mut self, world_id: &'a str) -> Self {
        self.world_id = Some(world_id);
        self
    }

    /// 取某作用域对应的目标 id。global 恒为空串（与落库口径一致）。
    fn target_for(&self, scope: &str) -> Option<&str> {
        match scope {
            SCOPE_USER => self.user_id,
            SCOPE_WORLD => self.world_id,
            SCOPE_GLOBAL => Some(""),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 记录与解析结果
// ═══════════════════════════════════════════════════════════════════════════

/// `runtime_flags` 的一行（已做自校验）。
#[derive(Debug, Clone)]
pub struct FlagRecord {
    pub id: String,
    pub flag: String,
    pub scope: String,
    pub target_id: String,
    pub enabled: bool,
    pub starts_at: i64,
    pub ends_at: i64,
    pub updated_by: String,
    pub updated_at: i64,
    pub reason: String,
    pub created_at: i64,
    /// 自校验失败的原因；`Some(_)` 即触发 fail-closed。
    pub corrupt: Option<String>,
}

impl FlagRecord {
    /// 在给定时刻是否落在生效窗口内。`0` = 该端不限。
    ///
    /// 窗口是**左闭右开** `[starts_at, ends_at)`：右开使得「上一段的 ends_at = 下一段的 starts_at」
    /// 这种首尾相接的排期不会在交界那一毫秒同时命中两条记录。
    fn in_window(&self, now: i64) -> bool {
        if self.starts_at > 0 && now < self.starts_at {
            return false;
        }
        if self.ends_at > 0 && now >= self.ends_at {
            return false;
        }
        true
    }
}

/// 解析结果的来源（运营诊断用：回答「那天为什么突然开放了」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagSource {
    /// 命中 `runtime_flags` 记录。
    Db { scope: String, target_id: String, record_id: String },
    /// 回落 env（现有语义）。
    Env { raw: String },
    /// 回落代码内默认值（env 未设 / env 值解析不了）。
    Default,
    /// fail-closed：查库失败或记录损坏，返回声明的默认值。
    FailClosed { why: String },
}

impl FlagSource {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Db { .. } => "db",
            Self::Env { .. } => "env",
            Self::Default => "default",
            Self::FailClosed { .. } => "fail_closed",
        }
    }
}

/// 一次完整解析的结果（含过程），供 admin 的 dry-run 端点直出。
#[derive(Debug, Clone)]
pub struct Resolution {
    pub flag: String,
    pub enabled: bool,
    pub source: FlagSource,
    /// 命中了 key 但**不在窗口内**因而被跳过的记录（`scope:target_id`），运营诊断用。
    pub skipped: Vec<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 对外入口
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **统一读取入口**。所有开关判定都应经此函数，不要在业务模块里再读 env。
///
/// 解析链见模块头。任何异常一律返回该开关的声明默认值（fail-closed）。
pub async fn is_enabled(db: &AnyPool, flag: &str, ctx: FlagCtx<'_>) -> bool {
    resolve(db, flag, ctx).await.enabled
}

/// 带过程的解析（`is_enabled` 的实现，另供 admin dry-run 端点直接使用）。
pub async fn resolve(db: &AnyPool, flag: &str, ctx: FlagCtx<'_>) -> Resolution {
    resolve_at(db, flag, ctx, now_ms()).await
}

/// 指定时刻解析（时间窗用例用；生产路径恒传 `now_ms()`）。
pub async fn resolve_at(db: &AnyPool, flag: &str, ctx: FlagCtx<'_>, now: i64) -> Resolution {
    // 未登记的开关名 → fail-closed 到 false。登记表是白名单：允许解析任意字符串等于
    // 允许打错一个字母就得到「关」而不自知，还不如显式报出来。
    let Some(def) = find_flag(flag) else {
        return Resolution {
            flag: flag.to_string(),
            enabled: false,
            source: FlagSource::FailClosed { why: format!("未登记的开关名「{flag}」") },
            skipped: Vec::new(),
        };
    };

    // ── ①②③ DB 记录 ──────────────────────────────────────────────────────
    let rows = match load_flag_rows(db, flag).await {
        Ok(rows) => rows,
        Err(e) => {
            // 🔴 查库失败 → fail-closed，**不回落 env**。见模块头。
            tracing::warn!(flag, error = %e, "runtime_flags 查询失败，按声明默认值 fail-closed");
            return Resolution {
                flag: flag.to_string(),
                enabled: def.default_enabled,
                source: FlagSource::FailClosed { why: format!("查库失败：{e}") },
                skipped: Vec::new(),
            };
        }
    };

    // 🔴 该开关名下**任意一行**损坏 → 整个开关 fail-closed。
    // 只跳过损坏行是不够的：一行 `scope='wrold'` 的记录按 key 查根本不会被命中，
    // 于是「运营配错了作用域」会表现为「配置毫无效果」而不是任何可见异常。
    if let Some(bad) = rows.iter().find(|r| r.corrupt.is_some()) {
        let why = bad.corrupt.clone().unwrap_or_default();
        tracing::warn!(flag, record = %bad.id, why = %why, "runtime_flags 记录损坏，fail-closed");
        return Resolution {
            flag: flag.to_string(),
            enabled: def.default_enabled,
            source: FlagSource::FailClosed { why: format!("记录 {} 损坏：{}", bad.id, why) },
            skipped: Vec::new(),
        };
    }

    let mut skipped = Vec::new();
    for scope in SCOPES_BY_PRIORITY {
        let Some(target) = ctx.target_for(scope) else { continue };
        let Some(rec) = rows.iter().find(|r| r.scope == *scope && r.target_id == target) else {
            continue;
        };
        if !rec.in_window(now) {
            // 窗口外 = 这条记录不参与解析，回落到更宽的作用域（见迁移 0036 注释）。
            skipped.push(format!("{}:{}", rec.scope, rec.target_id));
            continue;
        }
        return Resolution {
            flag: flag.to_string(),
            enabled: rec.enabled,
            source: FlagSource::Db {
                scope: rec.scope.clone(),
                target_id: rec.target_id.clone(),
                record_id: rec.id.clone(),
            },
            skipped,
        };
    }

    // ── ④ env 兜底（现有语义逐字保留） ────────────────────────────────────
    match std::env::var(def.name) {
        Ok(raw) => match parse_env_bool(&raw) {
            Some(v) => Resolution {
                flag: flag.to_string(),
                enabled: v,
                source: FlagSource::Env { raw },
                skipped,
            },
            // 配错不静默改变状态：回落默认（与各模块原有 `_ => DEFAULT_*` 分支逐字一致）。
            None => Resolution {
                flag: flag.to_string(),
                enabled: def.default_enabled,
                source: FlagSource::Default,
                skipped,
            },
        },
        // ── ⑤ 代码内默认值 ────────────────────────────────────────────────
        Err(_) => Resolution {
            flag: flag.to_string(),
            enabled: def.default_enabled,
            source: FlagSource::Default,
            skipped,
        },
    }
}

/// env 布尔解析。**与各模块原有实现逐字同构**（`onboarding::onboarding_enabled` 等）：
/// `1/true/on/yes` → 开，`0/false/off/no` → 关，其余（含空串）→ `None`（调用方回落默认）。
fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 读取与缓存
// ═══════════════════════════════════════════════════════════════════════════

const ENV_CACHE_TTL_MS: &str = "MUSE_FLAGS_CACHE_TTL_MS";

/// 缓存 TTL 默认值。
///
/// 生产 5 秒：足够把「每 tick 每请求」的读放量压到接近零，同时把**多进程**部署下
/// 「别的实例改了开关」的收敛时间钉在 5 秒内（本进程改的立即生效，见 `invalidate`）。
#[cfg(not(test))]
const DEFAULT_CACHE_TTL_MS: i64 = 5_000;
/// 测试默认 **0 = 禁用缓存**：用例各建各的 `sqlite::memory:` 池而进程只有一个，
/// 默认开缓存会让用例互相串味。缓存本身由专门的用例显式打开 TTL 后验证。
#[cfg(test)]
const DEFAULT_CACHE_TTL_MS: i64 = 0;

fn cache_ttl_ms() -> i64 {
    std::env::var(ENV_CACHE_TTL_MS)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(DEFAULT_CACHE_TTL_MS)
}

/// 整表快照。
struct Snapshot {
    loaded_at: i64,
    /// flag → 该开关名下的全部记录（含损坏的，损坏判定在解析处统一处理）。
    by_flag: HashMap<String, Vec<FlagRecord>>,
    /// 🔴 **持有池身份 Arc 的强引用，把这块内存钉住**。
    ///
    /// 缓存分桶键是 `Arc<AnyConnectOptions>` 的地址。若不持有强引用，一个池被 Drop 之后
    /// 这块内存会被释放，**下一个池可能分配到同一地址**，于是它会读到前一个池的快照——
    /// 生产只有一个池、看不出来，但测试里每个用例各建一个 `sqlite::memory:` 池，
    /// 会表现为随机失败的串味。持有强引用使地址在快照存活期间不可能被复用。
    /// 代价是每个曾出现过的池多留一个极小的结构体，可接受。
    _pool_identity: Arc<sqlx::any::AnyConnectOptions>,
}

type CacheMap = Mutex<HashMap<usize, Arc<Snapshot>>>;

fn cache() -> &'static CacheMap {
    static CACHE: OnceLock<CacheMap> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 连接池身份。`Pool::connect_options()` 返回池内部 `Arc` 的克隆（不是深拷贝），
/// 故其指针在同一个池上稳定、在不同池之间互异——正好当缓存分桶键。
/// 返回 Arc 本身而不只是地址：调用方要把它连同快照一起存起来钉住地址（见 `Snapshot`）。
fn pool_identity(db: &AnyPool) -> Arc<sqlx::any::AnyConnectOptions> {
    db.connect_options()
}

fn key_of(id: &Arc<sqlx::any::AnyConnectOptions>) -> usize {
    Arc::as_ptr(id) as *const () as usize
}

/// 🔴 使本进程该池的快照立即失效。**admin 的每个写操作都必须调它**，
/// 否则运营点完开关要等 TTL 才生效——「运营点了开关不能等半小时」这条要求就是靠它兑现的。
pub fn invalidate(db: &AnyPool) {
    if let Ok(mut m) = cache().lock() {
        m.remove(&key_of(&pool_identity(db)));
    }
}

/// 取某开关的全部记录（走缓存；TTL=0 时直查库且只查该 flag）。
async fn load_flag_rows(db: &AnyPool, flag: &str) -> Result<Vec<FlagRecord>, sqlx::Error> {
    let ttl = cache_ttl_ms();
    if ttl == 0 {
        return query_rows(db, Some(flag)).await;
    }

    let identity = pool_identity(db);
    let key = key_of(&identity);
    let now = now_ms();
    // 先看缓存（锁只在同步区间内持有，绝不跨 await）。
    if let Ok(m) = cache().lock() {
        if let Some(snap) = m.get(&key) {
            if now - snap.loaded_at < ttl {
                return Ok(snap.by_flag.get(flag).cloned().unwrap_or_default());
            }
        }
    }

    // 冷/过期：整表拉一次（表极小），失败则**向上抛**由解析处 fail-closed，不写缓存。
    let all = query_rows(db, None).await?;
    let mut by_flag: HashMap<String, Vec<FlagRecord>> = HashMap::new();
    for r in all {
        by_flag.entry(r.flag.clone()).or_default().push(r);
    }
    let out = by_flag.get(flag).cloned().unwrap_or_default();
    if let Ok(mut m) = cache().lock() {
        m.insert(key, Arc::new(Snapshot { loaded_at: now, by_flag, _pool_identity: identity }));
    }
    Ok(out)
}

/// 直查库。`flag=None` 拉整表（缓存填充），`Some(f)` 只拉一个开关（禁用缓存时）。
async fn query_rows(db: &AnyPool, flag: Option<&str>) -> Result<Vec<FlagRecord>, sqlx::Error> {
    let base = "SELECT id, flag, scope, target_id, enabled, starts_at, ends_at, \
                updated_by, updated_at, reason, created_at FROM runtime_flags";
    let rows = match flag {
        Some(f) => sqlx::query(&format!("{base} WHERE flag = $1")).bind(f).fetch_all(db).await?,
        None => sqlx::query(base).fetch_all(db).await?,
    };
    Ok(rows.iter().map(row_to_record).collect())
}

/// 行 → 记录（含自校验）。
///
/// 🔴 自校验就是 fail-closed 的入口。列读失败/取值非法一律标 `corrupt`，
/// 由解析处统一按声明默认值返回——**绝不猜测运营的本意**。
fn row_to_record(row: &sqlx::any::AnyRow) -> FlagRecord {
    let id: String = row.try_get("id").unwrap_or_default();
    let flag: String = row.try_get("flag").unwrap_or_default();
    let scope: String = row.try_get("scope").unwrap_or_default();
    let target_id: String = row.try_get("target_id").unwrap_or_default();
    let enabled_raw: i64 = row.try_get("enabled").unwrap_or(-1);
    let starts_at: i64 = row.try_get("starts_at").unwrap_or(-1);
    let ends_at: i64 = row.try_get("ends_at").unwrap_or(-1);

    let mut corrupt: Option<String> = None;
    if !SCOPES_BY_PRIORITY.contains(&scope.as_str()) {
        corrupt = Some(format!("作用域非法「{scope}」"));
    } else if scope == SCOPE_GLOBAL && !target_id.is_empty() {
        // global 记录带目标 id = 运营写错了作用域（本想配 world/user）。放行它会让这条
        // 配置永不命中，静默失效；报损坏才能让人当场发现。
        corrupt = Some(format!("global 作用域的 target_id 必须为空，实得「{target_id}」"));
    } else if scope != SCOPE_GLOBAL && target_id.is_empty() {
        corrupt = Some(format!("{scope} 作用域必须带 target_id"));
    } else if !(0..=1).contains(&enabled_raw) {
        corrupt = Some(format!("enabled 必须为 0/1，实得 {enabled_raw}"));
    } else if starts_at < 0 || ends_at < 0 {
        corrupt = Some(format!("时间窗不得为负：starts_at={starts_at} ends_at={ends_at}"));
    } else if starts_at > 0 && ends_at > 0 && starts_at >= ends_at {
        // 反转窗口（开始 ≥ 结束）永远不可能生效，几乎必然是填反了。
        corrupt = Some(format!("时间窗反转：starts_at={starts_at} >= ends_at={ends_at}"));
    }

    FlagRecord {
        id,
        flag,
        scope,
        target_id,
        enabled: enabled_raw == 1,
        starts_at,
        ends_at,
        updated_by: row.try_get("updated_by").unwrap_or_default(),
        updated_at: row.try_get("updated_at").unwrap_or_default(),
        reason: row.try_get("reason").unwrap_or_default(),
        created_at: row.try_get("created_at").unwrap_or_default(),
        corrupt,
    }
}

/// 列出记录（运营列表页；`flag=None` 全量）。按开关名 + 作用域优先级 + 目标排序，
/// 使运营看到的顺序与解析顺序一致。
pub async fn list_records(
    db: &AnyPool,
    flag: Option<&str>,
) -> Result<Vec<FlagRecord>, sqlx::Error> {
    let mut rows = query_rows(db, flag).await?;
    rows.sort_by(|a, b| {
        let rank = |s: &str| SCOPES_BY_PRIORITY.iter().position(|x| *x == s).unwrap_or(usize::MAX);
        a.flag
            .cmp(&b.flag)
            .then(rank(&a.scope).cmp(&rank(&b.scope)))
            .then(a.target_id.cmp(&b.target_id))
    });
    Ok(rows)
}

// ═══════════════════════════════════════════════════════════════════════════
// 写入（admin_api 调用）
// ═══════════════════════════════════════════════════════════════════════════

/// 写入参数。
#[derive(Debug, Clone)]
pub struct SetFlag<'a> {
    pub flag: &'a str,
    pub scope: &'a str,
    pub target_id: &'a str,
    pub enabled: bool,
    pub starts_at: i64,
    pub ends_at: i64,
    pub actor_id: &'a str,
    pub reason: &'a str,
}

/// upsert 一条记录（唯一键 = flag+scope+target_id，语义是**覆盖**）。返回记录 id。
///
/// 实现是先 UPDATE、`rows_affected==0` 再 INSERT。并发下两个 INSERT 会有一个撞唯一索引，
/// 此时**重试一次 UPDATE** 即收敛为「后写的赢」——与单线程下的覆盖语义一致。
/// 开关写入是低频运营动作，这点重试成本无关紧要。
///
/// ⚠️ **此处原注释称「`ON CONFLICT` 方言不可移植、`db.rs` 约定禁用」——那是错的**，已订正：
/// `db.rs` 禁的是方言特性（JSONB / serial / NOW()），`ON CONFLICT` 是 SQLite 3.24+ 与
/// Postgres 都支持的标准 UPSERT，仓库里 `ledger` / `shop` / `consents` / `runtime` 等多处在用。
/// 本函数的实现**本身没问题**（重试已覆盖竞态），保留即可；但别再把那句话当约定传下去——
/// 它已经让人为绕开一条不存在的禁令而写出「先 SELECT 再 INSERT」的竞态代码。
///
/// 调用方**必须**在成功后调 `invalidate(db)`（admin_api 已封在同一个 handler 里）。
pub async fn set_flag(db: &AnyPool, p: SetFlag<'_>) -> Result<String, sqlx::Error> {
    let now = now_ms();
    let updated = sqlx::query(
        "UPDATE runtime_flags SET enabled = $1, starts_at = $2, ends_at = $3, \
         updated_by = $4, updated_at = $5, reason = $6 \
         WHERE flag = $7 AND scope = $8 AND target_id = $9",
    )
    .bind(if p.enabled { 1_i64 } else { 0_i64 })
    .bind(p.starts_at)
    .bind(p.ends_at)
    .bind(p.actor_id)
    .bind(now)
    .bind(p.reason)
    .bind(p.flag)
    .bind(p.scope)
    .bind(p.target_id)
    .execute(db)
    .await?;

    if updated.rows_affected() > 0 {
        let id: String = sqlx::query_scalar(
            "SELECT id FROM runtime_flags WHERE flag = $1 AND scope = $2 AND target_id = $3",
        )
        .bind(p.flag)
        .bind(p.scope)
        .bind(p.target_id)
        .fetch_one(db)
        .await?;
        return Ok(id);
    }

    let id = crate::db::new_id("flg");
    let ins = sqlx::query(
        "INSERT INTO runtime_flags (id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(&id)
    .bind(p.flag)
    .bind(p.scope)
    .bind(p.target_id)
    .bind(if p.enabled { 1_i64 } else { 0_i64 })
    .bind(p.starts_at)
    .bind(p.ends_at)
    .bind(p.actor_id)
    .bind(now)
    .bind(p.reason)
    .bind(now)
    .execute(db)
    .await;

    match ins {
        Ok(_) => Ok(id),
        // 撞唯一索引 = 并发对手先建好了；改走 UPDATE，后写的赢。
        Err(_) => {
            sqlx::query(
                "UPDATE runtime_flags SET enabled = $1, starts_at = $2, ends_at = $3, \
                 updated_by = $4, updated_at = $5, reason = $6 \
                 WHERE flag = $7 AND scope = $8 AND target_id = $9",
            )
            .bind(if p.enabled { 1_i64 } else { 0_i64 })
            .bind(p.starts_at)
            .bind(p.ends_at)
            .bind(p.actor_id)
            .bind(now)
            .bind(p.reason)
            .bind(p.flag)
            .bind(p.scope)
            .bind(p.target_id)
            .execute(db)
            .await?;
            sqlx::query_scalar(
                "SELECT id FROM runtime_flags WHERE flag = $1 AND scope = $2 AND target_id = $3",
            )
            .bind(p.flag)
            .bind(p.scope)
            .bind(p.target_id)
            .fetch_one(db)
            .await
        }
    }
}

/// 按 id 读一条（删除前取快照，用于审计留痕「删掉的是什么」）。
pub async fn get_record(db: &AnyPool, id: &str) -> Result<Option<FlagRecord>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, flag, scope, target_id, enabled, starts_at, ends_at, \
         updated_by, updated_at, reason, created_at FROM runtime_flags WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(row_to_record))
}

/// 删除一条记录（= 该目标回落到更宽作用域 / env）。
pub async fn delete_record(db: &AnyPool, id: &str) -> Result<u64, sqlx::Error> {
    let r = sqlx::query("DELETE FROM runtime_flags WHERE id = $1").bind(id).execute(db).await?;
    Ok(r.rows_affected())
}
