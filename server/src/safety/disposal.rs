//! 被处置内容在**读取面**上的解引用闸门（`admin_api::takedown` / migration 0044 的第四条腿）。
//!
//! ## 补的是哪个缺口
//!
//! 0044 把下架落在四个展示态列上，靠的是「读取面判 `= 'approved'`」这条既有不变式。
//! 但那条不变式**只覆盖位图**：
//!
//! | 主体 | 展示态列 | 读取面闸门 | 谁在守 |
//! |---|---|---|---|
//! | 立绘 | `avatar_moderation` | 仅 approved 才下发 `avatarUrl` | 迁移 0016 起就有 |
//! | 世界封面 | `cover_moderation` | `worlds::visible_cover_url` | 迁移 0027 起就有 |
//! | 世界事件 | `world_events.moderation` | 事件流 / 日报只取 approved | §15 第 2 层 |
//! | **卡名（`card_json`）** | `cloud_characters.moderation` | **从来没有** | —— |
//!
//! 于是下架一张卡能断掉「进新世界」与立绘下发，**断不掉它在存量世界里已经露出的名字**：
//! roster 仍在列它、别人的传记条目仍写着它。对一个被要求下架的内容来说，
//! 这是处置不彻底。本模块给这条唯一没有闸门的路径补上闸门。
//!
//! ## 🔴 闸门作用在「现在去读卡拿名字」，不作用在已落定的文本
//!
//! §0.3 公共事实不可回滚。`world_events` 正文里出现过的名字是**已经发生的世界事实**，
//! 一个字节都不许回溯改写；`world_biographies.summary_json` 是封卷时的快照，同理。
//! 本模块处理的只有一类东西：**运行时拿着 `card_json` 现去解引用出一个名字**的读取面。
//!
//! 判据很简单——「关掉这个闸门，这段文字还会不会变？」会变的（现读现解）才归本模块管；
//! 已经落库成一段文本的，不归。红线用例
//! `admin_api::takedown::tests::red_line_name_gate_leaves_world_events_byte_identical`
//! 对八张事实表逐字节快照，并额外断言「事件正文里那个名字仍逐字在」。
//!
//! ## 🔴 默认关闭（`MUSE_DISPOSAL_NAME_GATE`），且关闭态与现状逐字节一致
//!
//! 打开这个闸门会**改变运行中世界的显示**：玩家昨天还看得见的名字今天变成一段替代文本。
//! 那是产品决策（什么时候开、开了给玩家看什么），不是工程能自作主张的事。所以本模块只交付
//! **能力**：走 `flags` 体系登记、`default_enabled: false`、解析链 user > world > global > env > 默认。
//! 关闭时 [`NameGate::display_name`] 原样返回真名，读取面输出与本模块存在之前**逐字节一致**
//! （`red_line_disabled_gate_is_byte_identical_to_today`）。
//!
//! ## 哪些读取面接、哪些刻意不接
//!
//! 接（都是「把**别人**的角色名摆给人看」）：
//! - `worlds::world_detail` 的公开阵容 roster；
//! - `worlds::character_display_name`（同源冲突 409 文案里的那个名字）；
//! - ~~`memorial` 遗作馆四处~~ —— **2026-07-29 随该模块整块删除**（角色卡永不损失，见总规格 §12 重写）。
//!   闸门的解引用点因此由 8 处降为 4 处；口径一字未改，只是被保护的读取面少了四个。
//! - `social` / `invitations` 的对手方角色名（各自一个共享 helper）。
//!
//! 不接，且每一条都有理由：
//! - **引擎输入**（`assembly::load_active_cards` / `runtime` 的 `other_cards_brief` / `ifline::runner`）：
//!   改这里等于改运行中世界的叙事内容，是 0044 模块头列的选项 (c)，需产品拍板；且引擎输入变了
//!   会让黄金世界回归对不上。闸门是展示层的事。
//! - **已封卷的快照**（`world_biographies.summary_json`、`GET /worlds/{id}/biography`）：§0.3。
//! - **后台审核面**（`admin_api::audit` 的申诉列表 / 队列详情）：人审要看的正是真名与全文，
//!   对运营遮蔽等于让处置无法复核。闸门朝玩家，不朝审核台。
//! - **作者自查面**（`backpack::my_memberships` 是 `WHERE wm.user_id = $1` 的自己看自己）：
//!   替代文本存在的意义是「不把被处置的名字摆给**别人**看」。把作者自己那份也涂掉，只会让他
//!   看不懂自己的账，而他该知道的事已由 `GET /assets/characters/{id}/status` 的 `takedown` 告知。
//!
//! ## 替代文本长什么样
//!
//! `暂不可见的角色·3f9a`（前缀参数化，见 [`ENV_DISPLAY_NAME`]）。四条约束定死了这个形状：
//!
//! 1. **中性**。它出现在**别人**的传记与悼念名单里，读者不是这场处置的当事人，也不该被平台
//!    塞一份对第三方的公开指控。所以不写「已违规」「已封禁」——那是判决书，不是占位符；
//!    也不写「已删除」——那是假的，世界事实里它还在。「暂不可见」只陈述读者这一侧的事实。
//! 2. **稳定**。由主体 id 推出，不掺时间与随机数：同一个角色在两次刷新、两个页面里必须读成
//!    同一个人，否则替代文本自己就成了新的信息噪声。
//! 3. **可区分**。同一份 roster 上若有两张被处置的卡，不带判别位就会渲染成两行一模一样的
//!    「暂不可见的角色」——读者会读成同一个人来了两次。那是**另一个假信息**，比露出真名更难察觉。
//! 4. **不泄露新东西**。判别位是主体 id 的 16 位 FNV-1a，而这些响应体本来就明文带着
//!    `cloudCharacterId` / `characterId`。它没有引入任何调用方拿不到的信息。

use sqlx::AnyPool;

use crate::flags::{is_enabled, FlagCtx};

/// 展示态列被下架时写入的哨兵值。
///
/// 🔴 **全仓唯一定义**（`admin_api::takedown` 从这里再导出）。写入侧与读取侧共用一个字面量是
/// 硬要求：两处各写一份，哪天有人改了其中一处，下架会**静默失效**——写进去的值不再等于
/// 读取面认得的值，而两边都不会报错。
pub const TAKEDOWN: &str = "takedown";

/// 展示态「已过审」。
pub const APPROVED: &str = "approved";

/// 本闸门的运营开关名（= env 变量名，见 `flags::FlagDef`）。默认关闭。
pub const FLAG_NAME_GATE: &str = "MUSE_DISPOSAL_NAME_GATE";

/// 替代文本前缀的参数名（§0.2 产品规则参数化：文案是运营旋钮，不写死在逻辑里）。
pub const ENV_DISPLAY_NAME: &str = "MUSE_DISPOSAL_DISPLAY_NAME";

/// 替代文本前缀默认值。
pub const DEFAULT_DISPLAY_NAME: &str = "暂不可见的角色";

/// 编译期一致性：登记表里的默认值必须与本模块的「默认关闭」口径一致。
/// 两处漂移在运行时几乎观察不到（只有 env 恰好没设、DB 恰好没记录时才暴露），故钉在编译期。
const _: () = assert!(!crate::flags::declared_default(FLAG_NAME_GATE));

fn display_name_prefix() -> String {
    std::env::var(ENV_DISPLAY_NAME)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_DISPLAY_NAME.to_string())
}

/// 主体 id → 4 位十六进制判别位（FNV-1a 64 折叠到 16 位）。
///
/// 纯函数、无随机、无时间：同一 id 恒得同一串（约束 2）。选 FNV 而不是密码学哈希是因为
/// 这里要的不是抗碰撞，是**稳定且便宜**；判别位碰撞的后果仅仅是两个占位符看起来像同一人，
/// 与不带判别位时的现状相同，不会更糟。
fn discriminator(subject_id: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in subject_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:04x}", (h ^ (h >> 32)) as u16)
}

/// 被处置主体的替代展示名，如 `暂不可见的角色·3f9a`。
pub fn placeholder_name(subject_id: &str) -> String {
    format!("{}·{}", display_name_prefix(), discriminator(subject_id))
}

/// 一次请求解析一次的闸门句柄。
///
/// 🔴 **必须在进入循环前解析一次、再逐行使用**。`flags::is_enabled` 会查库，逐行解析等于把
/// 一次 roster 渲染变成 N 次查库；更要命的是在事务内逐行查库，单连接池（测试 / SQLite dev）
/// 下会自锁 PoolTimedOut（同 `safety::record_risk_tx` 的注释）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NameGate {
    on: bool,
}

impl NameGate {
    /// 恒关闭的闸门。给「明确不接闸门」的调用点用（比后台面各写一遍 `if` 更难写错）。
    pub const OFF: NameGate = NameGate { on: false };

    /// 按上下文解析一次。ctx 给得越窄越好（user + world），解析链是 user > world > global > env > 默认。
    pub async fn resolve(db: &AnyPool, ctx: FlagCtx<'_>) -> NameGate {
        NameGate { on: is_enabled(db, FLAG_NAME_GATE, ctx).await }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// 该主体的卡面信息是否应当被隐去。
    ///
    /// 只认哨兵值 `'takedown'`：`pending` / `rejected` 是**发布期**的审核态，那条路上的内容
    /// 从来没在读取面露过面，不归本闸门管（把它们也算进来会让「从未过审」与「过审后被下架」
    /// 在展示上混成一谈，正是 0044 刻意不复用 `'rejected'` 要避免的事）。
    pub fn hides(&self, moderation: Option<&str>) -> bool {
        self.on && moderation == Some(TAKEDOWN)
    }

    /// 展示名闸门：被处置 → 中性替代文本；其余一切情形（含闸门关闭）→ **原样返回 `real`**。
    ///
    /// 关闭态的逐字节一致性由这一行保证：`self.on == false` 时本函数是恒等函数。
    pub fn display_name(&self, subject_id: &str, moderation: Option<&str>, real: String) -> String {
        if self.hides(moderation) {
            placeholder_name(subject_id)
        } else {
            real
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 关闭态是恒等函数——这条是「开关默认关闭时行为逐字节不变」的最小单元证明。
    #[test]
    fn a_closed_gate_is_the_identity_function() {
        let gate = NameGate::OFF;
        for moderation in [Some(APPROVED), Some(TAKEDOWN), Some("rejected"), Some("pending"), None] {
            assert_eq!(gate.display_name("cc_1", moderation, "裴照".into()), "裴照");
            assert!(!gate.hides(moderation));
        }
    }

    /// 开启态**只**对哨兵值生效：发布期的 pending / rejected 不归本闸门管。
    #[test]
    fn an_open_gate_only_reacts_to_the_takedown_sentinel() {
        let gate = NameGate { on: true };
        assert_eq!(gate.display_name("cc_1", Some(APPROVED), "裴照".into()), "裴照");
        assert_eq!(gate.display_name("cc_1", Some("rejected"), "裴照".into()), "裴照");
        assert_eq!(gate.display_name("cc_1", Some("pending"), "裴照".into()), "裴照");
        assert_eq!(gate.display_name("cc_1", None, "裴照".into()), "裴照");
        assert_ne!(gate.display_name("cc_1", Some(TAKEDOWN), "裴照".into()), "裴照");
    }

    /// 替代文本的三条形状约束：稳定、可区分、不含真名。
    #[test]
    fn the_placeholder_is_stable_distinguishable_and_carries_no_real_name() {
        let gate = NameGate { on: true };
        let a1 = gate.display_name("cc_a", Some(TAKEDOWN), "裴照".into());
        let a2 = gate.display_name("cc_a", Some(TAKEDOWN), "裴照".into());
        let b = gate.display_name("cc_b", Some(TAKEDOWN), "另一个名字".into());

        // 稳定：同一主体两次调用逐字相同（无时间、无随机）。
        assert_eq!(a1, a2);
        // 可区分：同一份列表上的两张被处置的卡不得渲染成同一行。
        assert_ne!(a1, b, "🔴 两个被处置主体渲染成同一串 → 读者会读成同一个人");
        // 不含真名：替代文本的全部意义就在这一条。
        assert!(!a1.contains('裴') && !a1.contains('照'), "{a1}");
        assert!(a1.starts_with(DEFAULT_DISPLAY_NAME), "{a1}");
    }

    /// 🔴 中性：替代文本不得是一份对被处置者的公开指控，也不得声称内容已被删除。
    ///
    /// 它出现在**别人**的传记与悼念名单里，读者不是当事人。判语黑名单钉住这条，
    /// 免得将来有人把默认文案改成「该角色因违规已被封禁」这类判决书。
    #[test]
    fn red_line_the_placeholder_never_accuses_the_author() {
        let text = placeholder_name("cc_x");
        for bad in ["违规", "封禁", "禁封", "已删除", "被举报", "非法", "有害", "作者"] {
            assert!(
                !text.contains(bad),
                "🔴 替代文本出现判语/指控「{bad}」：{text}。\
                 它渲染在第三方的页面上，平台不得借占位符对被处置者做公开定性"
            );
        }
    }

    /// 前缀是参数（§0.2），且配成空串一律回落默认（不静默变成一个没有名字的占位符）。
    #[test]
    fn the_placeholder_prefix_is_parameterized_with_a_safe_default() {
        assert_eq!(DEFAULT_DISPLAY_NAME, "暂不可见的角色");
        // env 未设 / 设为空白 → 默认（此处不改进程 env，只校验纯函数分支）。
        assert!(placeholder_name("cc_x").starts_with(DEFAULT_DISPLAY_NAME));
        assert_eq!(discriminator("cc_x").len(), 4);
        assert!(discriminator("cc_x").chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// 🔴 本模块是**纯读**的：一个写语句都不许有。
    ///
    /// 闸门若能写库，「下架只改展示不改事实」这条边界就从设计约束退化成了口头承诺。
    #[test]
    fn red_line_the_gate_module_never_writes_anything() {
        let src = include_str!("disposal.rs");
        // 拆写 needle，免得断言自身把关键字带进源码里（同 `annotations` 的写法）。
        for verb in [
            concat!("UPD", "ATE "),
            concat!("INS", "ERT "),
            concat!("DEL", "ETE "),
            concat!("ALT", "ER "),
        ] {
            assert!(
                !src.contains(verb),
                "🔴 展示面闸门里出现了写语句 `{verb}`——闸门只允许改「怎么显示」，不允许改任何数据"
            );
        }
    }
}
