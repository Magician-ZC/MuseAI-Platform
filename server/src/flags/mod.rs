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
//! 🔴 本批次**不迁移任何现有开关**
//! ════════════════════════════════════════════════════════════════════════════
//!
//! 一次把 9 个开关全搬进来是一次巨大的行为变更：每个模块的「前门拒绝 + 读取侧降级」
//! 语义各有细节（有的关掉是端点 404，有的是读取侧降级，有的是结算期跳过而非报错），
//! 批量改必然出错。本批次只交付**基础设施 + 一条参考接线**（`MUSE_ONBOARDING`，
//! 语义最简单：四个端点统一 404）。其余未接线开关的迁移清单见本文件末尾的
//! `MIGRATION_NOTES` 常量——它是代码内的待办清单，随迁移逐条划掉。
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
}

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
    },
    FlagDef {
        name: "MUSE_SUBPLOT_CARDS",
        default_enabled: false,
        owner: "subplot",
        desc: "副本卡（结算铸卡 + 同星合成）",
        wired: false,
    },
    FlagDef {
        name: "MUSE_LETHALITY_DEATHMATCH",
        default_enabled: false,
        owner: "worlds",
        desc: "生死状档（读取侧降级：关闭时既有生死场立即降为同意制）",
        wired: false,
    },
    FlagDef {
        name: "MUSE_ROOM_INVITATIONS",
        default_enabled: false,
        owner: "invitations",
        desc: "房间邀请（接受邀请只点亮引导入口，入场仍走 join 全部校验）",
        wired: false,
    },
    FlagDef {
        name: "MUSE_CONTAINER_ASSEMBLY",
        default_enabled: false,
        owner: "assembly",
        desc: "自定义房容器装配（副本卡的消费端，无独立端点）",
        wired: false,
    },
    FlagDef {
        name: "MUSE_MEMORIAL",
        default_enabled: false,
        owner: "memorial",
        desc: "传世卡 · 遗作馆（关闭时端点 404 且不发生任何封卷）",
        wired: false,
    },
    FlagDef {
        name: "MUSE_WORLD_SERIES_AUTOSCALE",
        default_enabled: false,
        owner: "worlds",
        desc: "世界系列自动扩容（1 号满员开 2 号）",
        wired: false,
    },
    FlagDef {
        name: "MUSE_WORLD_BE_BIOGRAPHY",
        default_enabled: false,
        owner: "progression",
        desc: "BE 结局传记（世界线崩塌后的封卷）",
        wired: false,
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
    },
    FlagDef {
        name: "MUSE_IFLINE_PARALLEL",
        default_enabled: false,
        owner: "ifline",
        desc: "if 线付费副本（世界结束后以终局为分叉点开单人平行线；烧副本卡换内容）。总规格 §7 人设保险第 3 级；\
               🔴 平行线不是改写——原世界线一个字节不动，且 if 线不产出任何可反哺原世界的资产",
        // 🔵 与 `MUSE_OOC_ANNOTATIONS` 同理：R3 新建件，没有需要保留的历史 env 语义，直接接线。
        wired: true,
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
    },
    FlagDef {
        name: "MUSE_SAFETY_LEXICON",
        // 🔴 唯一默认为「开」的开关：审核链。关掉它 = 放行敏感词，
        // 所以对它而言「安全的那一侧」是开着，fail-closed 返回 true 才是 fail-**safe**。
        default_enabled: true,
        owner: "safety",
        desc: "运行时敏感词库（审核链，默认开启；fail-safe 方向是「继续过滤」）",
        wired: false,
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
/// - `MUSE_SUBPLOT_CARDS`：**两个消费点语义不同**——端点侧关闭是 404，而结算铸卡侧关闭是
///   「跳过发卡而非报错」（`onboarding` 领礼包时也依赖这个「跳过」语义）。结算路径在
///   `runtime` 事务内，`is_enabled` 会查库，**必须确认它不在同一事务的锁持有区间内**，
///   否则 SQLite 单连接池会自锁。建议结算侧改用「进事务前解析一次、把 bool 传进事务」。
/// - `MUSE_LETHALITY_DEATHMATCH`：作用在**读取侧**（`effective_lethality`），关掉后既有生死场
///   立即降级为同意制。天然适合**按世界**作用域。但它同时被 `admin_api/worlds_ops.rs` 的建房
///   前门校验读到——建房时世界还不存在，**那一处只能用 global 作用域**（ctx 无 world_id）。
///   两处口径不同要写清楚，否则会出现「全局关但某世界开，却建不出那个世界」的困惑。
/// - `MUSE_ROOM_INVITATIONS`：四个端点统一 404，语义与 onboarding 同构，是**第二容易**迁的一个。
///   注意它的读取侧降级要求「已发出的邀请在关闭后也读不出」，ctx 用受邀人的 user_id。
/// - `MUSE_CONTAINER_ASSEMBLY`：**无独立端点**，消费点在建模板期（拒绝声明 `subplotCardRefs`）
///   与装配期（忽略容器字段走原路径）。装配期在 `assembly::assemble_instance` 内，同样要注意
///   事务边界。建模板期无 world_id/user_id 语义，实际只用 global。
/// - `MUSE_MEMORIAL`：端点 404 **且封卷本身不发生**。封卷在结算事务内，事务边界注意事项同副本卡。
///   另有 `MUSE_MEMORIAL_BOND_MIN` / `MUSE_MEMORIAL_PAGE_SIZE` 两个**参数化 env（非布尔）**，
///   本体系只管布尔开关，参数化配置是另一件事（§0.2），不要顺手塞进 `runtime_flags`。
/// - `MUSE_WORLD_SERIES_AUTOSCALE`：扩容判定在 join 的事务路径上，事务边界注意事项同上。
///   它已有**逐系列**的 `world_series.status` 急停阀，与本体系的 world 作用域**语义重叠**：
///   迁移时要明确「两道闸都开才扩容」，不要让 `runtime_flags` 变成第三道容易被忘记的闸。
/// - `MUSE_WORLD_BE_BIOGRAPHY`：封卷路径，事务边界注意事项同上。
/// - `MUSE_SAFETY_LEXICON`：🔴 **最后迁，或者干脆不迁**。它是审核链、默认开启，
///   且消费点在 `runtime` commit **事务内的闸**上——事务内查库风险最大，收益最小
///   （审核链本就该恒开，「按世界灰度关掉敏感词过滤」不是一个合理的运营动作）。
///   若一定要迁，只允许 global 作用域，且要有单独的红线用例断言它**永远不能被关到 false**
///   以外的路径上去。
pub const MIGRATION_NOTES: &str =
    "尚未接线的开关（wired=false）仍是纯 env；迁移清单与逐个注意事项见 flags::MIGRATION_NOTES 的文档注释";

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
