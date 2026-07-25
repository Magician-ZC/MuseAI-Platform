//! 运行时敏感词库（总规格 §15 五层漏斗 · **第 2 层**）：生成后硬匹配 + 就地打码为 `*`。
//!
//! ## 它补的洞
//! 世界运行时把模型生成文本经 `events::project_domain_events` 投影为面向用户的 `summary` 后，
//! 直接 INSERT `world_events`（`moderation` 曾被硬编码为 `'approved'`）→ 用户读取面/WS 推送/日报
//! 三处外泄，全程无任何审核。第 1 层（生成前 prompt 约束）拦不住越狱与模型抽风；第 3 层（语义
//! 分类）是网络调用，**不能**放进 tick 事务（见 `safety::mod` 中 `record_risk_tx` 的死锁注释）。
//! 第 2 层是纯本地词表匹配（≈0 成本、无 IO），因此可以也应该在事务内同步跑，与状态 CAS 同成同败。
//!
//! ## 归一化：复用 `inject.rs` 的同一条管线（不许另起炉灶）
//! 匹配前把文本过 `INVISIBLE`（去零宽/bidi/变体选择符）→ `fold_char`（全角 ASCII 折半角 + 西里尔/
//! 希腊同形字映射）→ 小写 → 去 `is_separator`（空白与装饰性标点），得到与 `compact_needle` **完全
//! 同口径**的紧凑串；词表条目同样过 `compact_needle`。若这里自己写一套归一化，`inject.rs` 已经挡住
//! 的零宽插入 / 同形字伪装 / 全角 / 标点打断会在新词表上原样复活。测试
//! `compact_scan_matches_inject_pipeline` 把两条管线的等价性钉死。
//!
//! ## 打码口径
//! 匹配发生在紧凑串上，但打码回写到**原文字节区间**：命中区间内的原始字符（含攻击者插入的零宽符、
//! 全角空格、装饰标点）一并替换为 `*`，避免出现「关键词被打码但绕过痕迹仍拼得回来」的半吊子结果。
//! 打码在**落库前**完成——落库的即是最终事实，不存在事后改写已公开事实（§0.3 公共事实不可回滚）。
//!
//! ## 词表是数据，不是逻辑（§0.2 产品规则参数化）
//! 内置表是 **dev 基线种子**，只保证管道可用与回归可测，不是生产完整词库；分类与严重度是表上的
//! 字段而非散落在 if 里。运营侧补充词经 `MUSE_SAFETY_LEXICON_EXTRA` 注入（逗号/分号/换行分隔），
//! 严重度按 `custom` 分类走低危（只记险、不淹没人审队列）。真实生产应把本表换成词库服务/运营配置表。
//!
//! ## 开关（`MUSE_SAFETY_LEXICON`，默认 **开启**）
//! 见 `enabled()` 的注释：这是误伤应急阀，不是灰度开关——内容安全是恒开审核链。

use std::sync::OnceLock;

use super::inject::{compact_needle, fold_char, is_separator, INVISIBLE};

/// 词条严重度：只影响**处置强度**（是否进人审队列），不影响是否打码——命中一律打码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// 低危：仅记 risk_events（运行时每 tick 每事件都可能命中，进人审队列会淹掉队列）。
    Low,
    /// 高危：记险 + 进人审队列（按 `MUSE_SAFETY_RUNTIME_AUDIT` 策略，见 safety::mod）。
    High,
}

/// 一条命中：分类 + 严重度 + 命中的**词表条目**（不是用户文本片段——留痕足够定位，
/// 又不把用户上下文整段复制进风控库）+ 本段文本内的命中次数。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub category: String,
    pub severity: Severity,
    pub term: String,
    pub count: usize,
}

// ==================== 词表（集中区，const &[&str]，编译期零成本；风格对齐 inject.rs） ====================

/// 一个分类：名称 + 严重度 + 词条集合。分类/严重度是数据，调整词表不动匹配逻辑。
struct CategorySpec {
    name: &'static str,
    severity: Severity,
    terms: &'static [&'static str],
}

/// 辱骂与人身攻击（低危）：在叙事里出现多为角色对骂，打码即可，不必逐条拉人审。
const ABUSE_TERMS: &[&str] = &[
    "傻逼", "煞笔", "傻叉", "脑残", "智障", "操你妈", "草泥马", "尼玛逼", "狗娘养的", "婊子",
    "贱货", "杂种", "滚你妈", "去你妈的",
];

/// 色情低俗（高危）：生成式服务的红线之一，命中即需人审复核链路是否被越狱。
const PORN_TERMS: &[&str] = &[
    "做爱", "性交", "口交", "肉棒", "淫穴", "淫水", "裸聊", "约炮", "嫖娼", "卖淫", "乱伦",
    "强奸", "轮奸",
];

/// 违禁品与危险物制作（高危）：毒品/枪爆，任何叙事包装都不豁免。
const CONTRABAND_TERMS: &[&str] = &[
    "冰毒", "海洛因", "甲基苯丙胺", "摇头丸", "可卡因", "制毒", "贩毒", "毒品交易", "枪支买卖",
    "军火买卖", "制造炸弹", "炸弹制作", "炸药配方", "雷管",
];

/// 自伤自杀方法引导（高危）：只收「方法/教程/相约」句式，不收单独的「自杀」二字——
/// 后者在悲剧叙事里是正常剧情词，收进来会大面积误伤。
const SELF_HARM_TERMS: &[&str] = &[
    "自杀方法", "自杀教程", "如何自杀", "怎么自杀", "割腕自杀", "烧炭自杀", "上吊自杀",
    "跳楼自杀", "服毒自杀", "相约自杀", "约死群",
];

/// 站外引流 / 诈骗 / 赌博（低危）：世界叙事里绝不该出现的招揽话术，是模型被注入的典型痕迹。
/// 用「加我微信」而非「加微信」——后者在现代都市题材里是正常剧情。
const SOLICIT_TERMS: &[&str] = &[
    "加我微信", "加我qq", "微信号是", "私聊我", "扫码进群", "刷单返利", "代开发票", "办证刻章",
    "博彩网站", "赌博网站", "开设赌场", "日赚过万", "出售账号",
];

const CATEGORIES: &[CategorySpec] = &[
    CategorySpec { name: "abuse", severity: Severity::Low, terms: ABUSE_TERMS },
    CategorySpec { name: "porn", severity: Severity::High, terms: PORN_TERMS },
    CategorySpec { name: "contraband", severity: Severity::High, terms: CONTRABAND_TERMS },
    CategorySpec { name: "self_harm", severity: Severity::High, terms: SELF_HARM_TERMS },
    CategorySpec { name: "solicit", severity: Severity::Low, terms: SOLICIT_TERMS },
];

/// 运营侧补充词表的环境变量（逗号/分号/换行分隔）。分类固定 `custom`、严重度固定低危：
/// 热配置词的准确度未经验证，让它直接灌人审队列风险更大。
const ENV_EXTRA_TERMS: &str = "MUSE_SAFETY_LEXICON_EXTRA";
/// 词库层开关环境变量。
const ENV_ENABLED: &str = "MUSE_SAFETY_LEXICON";
/// 词库层默认开关值。
const DEFAULT_LEXICON_ENABLED: bool = true;

/// 词库层是否启用（env 覆盖 + 默认常量，范式同 `runtime::token_cny_cents_per_1k`）。
///
/// **默认开启**，这是刻意的：VALIDATION.md §0.1「未验证功能默认关闭」约束的是**商业玩法功能**
/// （托梦配额、死亡规则、赛事…），内容安全审核链属于合规主体责任（总规格 §15/§16）与平台红线，
/// 是恒开设施；把它默认关闭等于「默认无审核上线」，方向正好反了。此开关的定位是**误伤应急阀**：
/// 运营发现词表大面积误伤时可临时关停、修词表、再开回来，而不是一个等待验证的灰度位。
pub fn enabled() -> bool {
    env_flag(ENV_ENABLED, DEFAULT_LEXICON_ENABLED)
}

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => parse_flag(&v, default),
        Err(_) => default,
    }
}

fn parse_flag(v: &str, default: bool) -> bool {
    match v.trim().to_ascii_lowercase().as_str() {
        "0" | "false" | "off" | "no" => false,
        "1" | "true" | "on" | "yes" => true,
        _ => default, // 配错不静默改变行为：回落默认（开启）
    }
}

// ==================== 编译后的匹配项 ====================

struct Needle {
    category: &'static str,
    severity: Severity,
    /// 展示/留痕用的原始词条
    term: String,
    /// 与 haystack 同口径的紧凑字符序列
    chars: Vec<char>,
}

/// 内置词表编译一次（进程级缓存）；词条为 `'static`，不随请求变化。
fn builtin_needles() -> &'static Vec<Needle> {
    static CELL: OnceLock<Vec<Needle>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut out = Vec::new();
        for cat in CATEGORIES {
            for t in cat.terms {
                let chars = compact_needle(t);
                if chars.is_empty() {
                    continue;
                }
                out.push(Needle {
                    category: cat.name,
                    severity: cat.severity,
                    term: (*t).to_string(),
                    chars,
                });
            }
        }
        out
    })
}

/// 解析运营补充词表（纯函数，便于单测；不读 env）。
fn parse_extra_terms(raw: &str) -> Vec<Needle> {
    raw.split(|c: char| matches!(c, ',' | ';' | '\n' | '\r' | '，' | '；'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|t| {
            let chars = compact_needle(t);
            if chars.is_empty() {
                return None;
            }
            Some(Needle {
                category: "custom",
                severity: Severity::Low,
                term: t.to_string(),
                chars,
            })
        })
        .collect()
}

/// 运营补充词表（每次读 env——通常未设置，近乎零成本；设置了也只在启动期变化）。
fn extra_needles() -> Vec<Needle> {
    match std::env::var(ENV_EXTRA_TERMS) {
        Ok(raw) => parse_extra_terms(&raw),
        Err(_) => Vec::new(),
    }
}

// ==================== 紧凑扫描（保留回原文的 char 下标映射） ====================

/// 原文 char 序列 + 归一化紧凑序列 + 紧凑字符→原文 char 下标的映射。
///
/// 与 `inject::Scan` 的差别：那边映射到 `normalize()` 后的串（够用于生成可读摘要），
/// 这边必须映射回**原文**，因为第 2 层要在原文上就地打码后原样落库。
struct CompactScan {
    raw: Vec<(usize, char)>,
    chars: Vec<char>,
    src: Vec<usize>,
}

impl CompactScan {
    fn new(text: &str) -> CompactScan {
        let raw: Vec<(usize, char)> = text.char_indices().collect();
        let mut chars = Vec::with_capacity(raw.len());
        let mut src = Vec::with_capacity(raw.len());
        for (i, (_, ch)) in raw.iter().enumerate() {
            // 与 inject::normalize + compact_needle 逐步对齐：
            // 去不可见 → 折叠全角/同形字 → 丢弃分隔符（含空白）→ 小写展开。
            if INVISIBLE.contains(ch) {
                continue;
            }
            let folded = fold_char(*ch);
            if is_separator(folded) {
                continue;
            }
            for lc in folded.to_lowercase() {
                chars.push(lc);
                src.push(i);
            }
        }
        CompactScan { raw, chars, src }
    }

    /// 全部（非重叠）命中的紧凑下标。
    fn find_all(&self, needle: &[char]) -> Vec<usize> {
        let mut out = Vec::new();
        if needle.is_empty() || needle.len() > self.chars.len() {
            return out;
        }
        let mut i = 0usize;
        let last = self.chars.len() - needle.len();
        while i <= last {
            if &self.chars[i..i + needle.len()] == needle {
                out.push(i);
                i += needle.len();
            } else {
                i += 1;
            }
        }
        out
    }
}

// ==================== 入口 ====================

/// 第 2 层硬匹配：命中即把**原文对应区间**整段替换为 `*`，返回（打码后文本, 命中清单）。
///
/// 纯函数、无 IO、不读 `MUSE_SAFETY_LEXICON`——开关由调用方（`safety::moderate_runtime_projection`）
/// 判定，这样单测不依赖进程环境变量、也不会与并行用例互相干扰。
/// 未命中时原样返回输入（零拷贝语义上的「不改写」），命中清单为空。
pub fn mask(text: &str) -> (String, Vec<Hit>) {
    let extra = extra_needles();
    if extra.is_empty() {
        return mask_with(text, builtin_needles());
    }
    let all: Vec<&Needle> = builtin_needles().iter().chain(extra.iter()).collect();
    mask_with_refs(text, &all)
}

/// 匹配核心（词表由调用方给定，便于单测注入自定义词条而不依赖进程环境变量）。
fn mask_with(text: &str, needles: &[Needle]) -> (String, Vec<Hit>) {
    let refs: Vec<&Needle> = needles.iter().collect();
    mask_with_refs(text, &refs)
}

fn mask_with_refs(text: &str, needles: &[&Needle]) -> (String, Vec<Hit>) {
    if text.is_empty() {
        return (String::new(), Vec::new());
    }
    let scan = CompactScan::new(text);
    if scan.chars.is_empty() {
        return (text.to_string(), Vec::new());
    }

    let mut masked = vec![false; scan.raw.len()];
    let mut hits: Vec<Hit> = Vec::new();

    for n in needles {
        let positions = scan.find_all(&n.chars);
        if positions.is_empty() {
            continue;
        }
        for p in &positions {
            // 打码回原文区间：覆盖命中首尾之间的**全部**原始字符，
            // 把攻击者插入的零宽符/全角空格/装饰标点一并抹掉。
            let a = scan.src[*p];
            let b = scan.src[p + n.chars.len() - 1];
            for m in masked.iter_mut().take(b + 1).skip(a) {
                *m = true;
            }
        }
        hits.push(Hit {
            category: n.category.to_string(),
            severity: n.severity,
            term: n.term.clone(),
            count: positions.len(),
        });
    }

    if hits.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let out: String = scan
        .raw
        .iter()
        .enumerate()
        .map(|(i, (_, ch))| if masked[i] { '*' } else { *ch })
        .collect();
    (out, hits)
}

/// 命中清单里的最高严重度（None = 未命中）。
pub fn max_severity(hits: &[Hit]) -> Option<Severity> {
    hits.iter().map(|h| h.severity).max()
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|h| h.term.clone()).collect()
    }

    // ---------- 基本命中与打码 ----------

    #[test]
    fn hits_are_masked_in_place() {
        let (out, hits) = mask("他冷笑一声：你这个傻逼，滚开。");
        assert_eq!(terms(&hits), vec!["傻逼"]);
        assert!(!out.contains("傻逼"), "命中词必须被打码：{out}");
        assert!(out.contains("**"), "应就地替换为 *：{out}");
        assert!(out.contains("他冷笑一声"), "未命中部分原样保留：{out}");
        assert_eq!(out.chars().count(), "他冷笑一声：你这个傻逼，滚开。".chars().count(), "打码不改变字符数");
    }

    #[test]
    fn clean_text_passes_through_unchanged() {
        let src = "两位大臣于烛下各怀心事，礼数周全，言语间暗藏机锋。";
        let (out, hits) = mask(src);
        assert!(hits.is_empty(), "正常叙事不应命中：{hits:?}");
        assert_eq!(out, src, "未命中必须原样放行");
    }

    #[test]
    fn narrative_suicide_word_alone_not_flagged() {
        // 「自杀」单词是正常悲剧剧情；只有方法/教程/相约句式才收。
        let (out, hits) = mask("他在信里写道，父亲那年选择了自杀，母亲从此不再提起。");
        assert!(hits.is_empty(), "单独的『自杀』不得误伤：{hits:?}");
        assert!(out.contains("自杀"));
        assert!(!mask("有人在群里发自杀方法").1.is_empty(), "方法引导必须命中");
    }

    #[test]
    fn category_and_severity_are_carried() {
        let (_, hits) = mask("他掏出一包冰毒。");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].category, "contraband");
        assert_eq!(hits[0].severity, Severity::High);
        assert_eq!(max_severity(&hits), Some(Severity::High));

        let (_, low) = mask("你这个傻逼");
        assert_eq!(low[0].severity, Severity::Low);
        assert_eq!(max_severity(&low), Some(Severity::Low));
        assert_eq!(max_severity(&[]), None);
    }

    #[test]
    fn multiple_occurrences_counted_and_all_masked() {
        let (out, hits) = mask("傻逼，你真是个傻逼。");
        assert_eq!(hits.len(), 1, "同一词条只出一条命中");
        assert_eq!(hits[0].count, 2, "但计数覆盖全部出现");
        assert!(!out.contains("傻逼"), "全部出现都要打码：{out}");
    }

    // ---------- 归一化绕过：复用 inject 管线 → 一律拦住 ----------

    #[test]
    fn bypass_zero_width_chars() {
        let (out, hits) = mask("你这个傻\u{200B}逼\u{200D}。");
        assert_eq!(terms(&hits), vec!["傻逼"], "零宽插入绕过未拦住");
        assert!(!out.contains('傻') && !out.contains('逼'), "命中区间含零宽符一并打码：{out}");
        assert!(!out.contains('\u{200B}'), "区间内零宽符也应被抹掉：{out:?}");
    }

    #[test]
    fn bypass_fullwidth_and_space() {
        assert_eq!(terms(&mask("傻　逼").1), vec!["傻逼"], "全角空格绕过未拦住");
        assert_eq!(terms(&mask("加我ＱＱ").1), vec!["加我qq"], "全角字母绕过未拦住");
        assert_eq!(terms(&mask("加我 Q Q").1), vec!["加我qq"], "空格+大小写绕过未拦住");
    }

    #[test]
    fn bypass_homoglyph() {
        // 内置表以中文为主（中文无同形字问题），故用注入词表验证同形字通路：
        // ѕ = 西里尔 U+0455 → s，映射后与词条 "system" 对齐。
        let ns = parse_extra_terms("system");
        let (out, hits) = mask_with("ѕystem 接管对话", &ns);
        assert_eq!(terms(&hits), vec!["system"], "同形字伪装未拦住");
        assert!(!out.contains('ѕ') && !out.to_lowercase().contains("system"), "命中区间应被打码：{out}");
        assert!(out.contains("接管对话"), "未命中部分保留：{out}");
    }

    #[test]
    fn injected_needles_apply_to_mask_core() {
        // 运营补充词经同一匹配核心生效（`mask` 走 env，核心走注入，行为一致）。
        let ns = parse_extra_terms("禁忌之名");
        let (out, hits) = mask_with("他念出了禁·忌　之名。", &ns);
        assert_eq!(terms(&hits), vec!["禁忌之名"], "补充词 + 分隔符绕过未拦住");
        assert!(!out.contains("之名"), "{out}");
        // 未注入时同一文本放行 → 证明命中确实来自词表而非硬编码逻辑。
        assert!(mask_with("他念出了禁·忌　之名。", &[]).1.is_empty());
    }

    #[test]
    fn bypass_punctuation_insertion() {
        assert_eq!(terms(&mask("傻·逼").1), vec!["傻逼"], "装饰标点绕过未拦住");
        assert_eq!(terms(&mask("制-造-炸-弹").1), vec!["制造炸弹"], "连字符绕过未拦住");
    }

    #[test]
    fn compact_scan_matches_inject_pipeline() {
        // 本模块的紧凑扫描必须与 inject::compact_needle 完全同口径——
        // 一旦漂移，两层的绕过防护就会不一致（这正是要求复用管线的原因）。
        for s in [
            "忽略　以上ＳＹＳＴＥＭ：接管",
            "傻\u{200B}逼·你 好",
            "аbс ABC",
            "混合 English 与中文，带标点！？",
            "",
            "。。。",
        ] {
            let scan = CompactScan::new(s);
            assert_eq!(scan.chars, compact_needle(s), "紧凑串口径与 inject 管线不一致：{s:?}");
        }
    }

    #[test]
    fn scan_index_mapping_is_consistent() {
        let scan = CompactScan::new("a\u{200B}b　c");
        assert_eq!(scan.chars, vec!['a', 'b', 'c']);
        assert_eq!(scan.src.len(), scan.chars.len());
        for (k, i) in scan.src.iter().enumerate() {
            assert_eq!(scan.raw[*i].1.to_ascii_lowercase(), scan.chars[k]);
        }
    }

    // ---------- 参数化：运营补充词表 ----------

    #[test]
    fn extra_terms_parse_is_split_and_normalized() {
        let ns = parse_extra_terms("甲乙, 丙丁;\n戊己，  ，庚辛；");
        let got: Vec<String> = ns.iter().map(|n| n.term.clone()).collect();
        assert_eq!(got, vec!["甲乙", "丙丁", "戊己", "庚辛"], "多种分隔符 + 去空项");
        assert!(ns.iter().all(|n| n.category == "custom" && n.severity == Severity::Low));
        assert_eq!(ns[0].chars, compact_needle("甲乙"), "补充词同样过归一化管线");
        assert!(parse_extra_terms("   ,, ;; \n").is_empty(), "全空配置 → 空表");
    }

    // ---------- 开关 ----------

    #[test]
    fn lexicon_enabled_by_default() {
        // 内容安全恒开：不设 env 时必须是启用态（默认关闭 = 默认无审核上线）。
        assert!(DEFAULT_LEXICON_ENABLED, "词库层默认必须开启");
        assert!(parse_flag("", DEFAULT_LEXICON_ENABLED), "空值回落默认（开启）");
        assert!(parse_flag("乱填", DEFAULT_LEXICON_ENABLED), "配错回落默认（开启）");
        assert!(!parse_flag("off", DEFAULT_LEXICON_ENABLED), "显式 off 才关闭");
        assert!(!parse_flag(" 0 ", DEFAULT_LEXICON_ENABLED));
        assert!(parse_flag("on", false), "显式 on 覆盖默认");
    }

    #[test]
    fn empty_and_separator_only_text_is_safe() {
        assert_eq!(mask(""), (String::new(), Vec::new()));
        let (out, hits) = mask("。，、 ");
        assert!(hits.is_empty());
        assert_eq!(out, "。，、 ");
    }
}
