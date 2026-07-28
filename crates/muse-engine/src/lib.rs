//! muse-engine：MuseAI 宿主无关核心引擎。
//!
//! 约束（规格 §8.2 / §16）：本 crate 不依赖 tauri / axum 任何类型；
//! 文件、时钟、事件、模型调用一律通过 [`host`] 与 [`model`] 的 trait 注入。
//! 桌面壳（src-tauri）与平台后端（server）共享同一套实现。

pub mod character;
pub mod error;
pub mod host;
pub mod knowledge;
pub mod model;
pub mod narrative;
pub mod replay;
pub mod store;
pub mod world;

pub use error::EngineError;

/// 引擎版本号，随不兼容的管线/状态变更递增；持久化对象与事件携带它用于版本钉住。
pub const ENGINE_VERSION: &str = "0.1.0";

#[cfg(test)]
mod determinism_red_line {
    /// 🔴 **确定性契约「禁三样」在引擎侧此前一道闸都没有。**
    ///
    /// server 那边有（`assembly::tests::red_line_no_irreproducible_randomness_*`），
    /// 但它走 `testkit::production_sources()`，而那个函数读的是
    /// `server/Cargo.toml` 所在的 `src` —— **本 crate 不在它的覆盖内**。
    ///
    /// 而采样、仲裁、结局选取全在这里。引擎里混进一个 `thread_rng`，
    /// 症状是黄金世界回归**偶发**变红，而偶发红的第一反应通常是「重跑一下」。
    ///
    /// 需要随机请走 `assembly::fnv1a_64` + `Rng`（SplitMix64）并登记域常量；
    /// 需要时间请走注入的 `HostClock`。
    ///
    /// ⚠️ **不禁 `HashMap`**：契约禁的是「用 map 迭代序驱动 RNG」，不是拿 map 查表；
    /// 源码级扫描分不出这两者，一律禁会逼出一堆无意义改写并让这道门被无视。
    /// 迭代序那一支由「同种子同结果」的行为用例负责。
    #[test]
    fn no_irreproducible_randomness_in_engine_sources() {
        /// 不可复现的随机 / 时钟 API。
        const BANNED: &[&str] =
            &["thread_rng", "rand::random", "gen_range", "shuffle(", "SystemTime::now", "Instant::now"];

        /// 豁免两处，各有理由：
        /// - `host.rs`：`SystemClock` 是**注入用的时钟实现本身**——引擎的设计正是
        ///   「时钟由宿主注入」，那个 impl 就是唯一允许读真实时间的地方（同 server 侧的 `db.rs`）。
        /// - `model.rs`：`Instant::now` 只用于**测一次调用耗时**，不进任何决策。
        const EXEMPT: &[&str] = &["host.rs", "model.rs"];

        let mut offenders: Vec<String> = Vec::new();
        let mut exempt_hits = 0usize;
        for (path, src) in production_sources() {
            // 剥注释：本仓多处注释里逐字写着 `thread_rng` 来说明「禁的是它」。
            let code: String = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            let hit: Vec<&str> = BANNED.iter().copied().filter(|b| code.contains(b)).collect();
            if hit.is_empty() {
                continue;
            }
            if EXEMPT.iter().any(|e| path.ends_with(e)) {
                exempt_hits += 1;
                continue;
            }
            offenders.push(format!("{path} → {hit:?}"));
        }
        assert!(
            offenders.is_empty(),
            "🔴 引擎生产码出现不可复现的随机/时钟 API：{offenders:?}\n\
             确定性产出是合规红线（同一种子同一结果，黄金世界回归与录制回放都以此为前提）。\n\
             · 需要随机 → `assembly::fnv1a_64` + `Rng`（SplitMix64）并登记域常量；\n\
             · 需要时间 → 走注入的 `HostClock`。\n\
             若确有必须不可复现的理由，加进 EXEMPT 并写清楚，不要只把断言改绿。"
        );
        // 扫描器失效 = 红线静默失效：确认豁免项**确实**含被禁 API。
        assert_eq!(
            exempt_hits,
            EXEMPT.len(),
            "🔴 豁免项里应当恰好每个都命中一次（当前 {exempt_hits}）——\
             要么某个不再需要豁免（请删掉），要么扫描器坏了"
        );
    }

    /// 🔴 **比源码扫描更强的一道：引擎的依赖表里不许有随机数库。**
    ///
    /// 上面那条红线是扫源码字符串，这一条是扫 `Cargo.toml`——
    /// 没有 `rand` 依赖，`thread_rng` 之类**根本引不进来**（写了也编译不过）。
    /// 故障注入时正是这样：往 `narrative` 里塞 `rand::thread_rng()`，**编译直接失败**。
    ///
    /// 两条一起才完整：依赖表这条挡「引进来」，源码那条挡「用标准库的时钟当决策输入」
    /// （`SystemTime::now` 不需要任何依赖）。
    ///
    /// ⚠️ 真要引入随机数库（比如为了非决策路径的抖动），那是**确定性契约的评审事项**，
    /// 不是加一行依赖就完事——本条会红，正是为了逼那次评审。
    #[test]
    fn engine_has_no_random_number_dependency() {
        let manifest = include_str!("../Cargo.toml");
        // 只看依赖段，避免误伤包名/描述里出现的字样。
        let deps = manifest
            .split("[dependencies]")
            .nth(1)
            .expect("Cargo.toml 应有 [dependencies] 段");
        for banned in ["rand", "fastrand", "oorandom", "nanorand", "getrandom"] {
            let declared = deps
                .lines()
                .take_while(|l| !l.trim_start().starts_with('['))
                .any(|l| {
                    let name = l.split(['=', ' ']).next().unwrap_or("").trim();
                    name == banned
                });
            assert!(
                !declared,
                "🔴 引擎依赖了随机数库 `{banned}`：确定性产出是合规红线\n\
                 （同一种子同一结果，黄金世界回归与录制回放都以此为前提）。\n\
                 需要随机请走 `assembly::fnv1a_64` + `Rng`（SplitMix64）并登记域常量。\n\
                 若确有非决策路径的需要，那是一次评审，不是加一行依赖。"
            );
        }
    }

    /// 递归收集本 crate 的**生产**源码，返回 `(相对路径, 剥掉测试模块后的源码)`。
    ///
    /// 🔴 花括号配平剥 `#[cfg(test)] mod X { .. }`，**不能按模块名或第一个 `#[cfg(test)]` 截断**
    /// ——两种错法 server 那边都踩过（见 `server/src/testkit.rs` 的长注释）：
    ///
    /// ⚠️ 一个如实记的否定结果：故障注入把本函数退化成「按第一个 `#[cfg(test)]` 截断」，
    /// **本 crate 上没红**——量了一下，正确剥后保留 60% 的字符，退化截断后 59%，
    /// 因为引擎的测试模块几乎都在文件末尾，中段夹具极少。也就是说那种退化在这里
    /// 确实只丢约 1%，我没有为此造一个检测不出来的断言。
    /// 保持配平写法是因为**将来**可能出现中段夹具（server 那边就有），不是因为现在有。
    ///
    /// 前者漏掉不叫 `tests` 的内联测试模块，后者会把中段测试夹具之后的**生产代码整段丢掉**。
    /// 🔴 **观众礼物（`ambient_events`）绝不许流出展示层。**
    ///
    /// 产品 2026-07-28 拍板（`docs/build/open-decisions.md` §5 选项 A）：
    /// **观众打赏买到的是一个被看见的机会，不是任何形式的优势。**
    /// 平台红线第一条是「不卖胜负与数值平权」，而这一项是全清单里唯一一个
    /// **「实现了再撤回」代价极高**的——它一旦被玩家感知为「打赏有用」，
    /// 撤回就等于承认平台卖过优势。
    ///
    /// # 判据方向：**排除表**，不是清单
    ///
    /// 若写成「以下这几处允许引用」，将来在仲裁里新加一处引用**不在清单里 → 静默放行**。
    /// 这里反过来：**除了登记的这几个文件，任何生产源码出现 `ambient_events` / `AmbientEvent`
    /// 都判红**，且登记表按**处数**记——往已登记文件里多加一处引用同样对不上。
    /// 于是新增引用一定会红，作者必须在「改回去」和「在这张表里写清为什么安全」之间选一个。
    ///
    /// # 为什么源码扫描在这里是够的
    ///
    /// 这个字段的危险形态是**被读**（仲裁读它、reducer 读它、不变量读它）。
    /// 读必然出现标识符。行为用例证明不了「所有可能的输入下都没被读」，源码扫描可以。
    /// 两者互补：另有 `ambient_only_changes_the_ambient_key_of_the_context`
    /// 从行为侧钉住「它只改上下文的那一格」。
    #[test]
    fn ambient_events_never_leave_the_presentation_layer() {
        // (相对 src/ 的路径, 允许出现 `ambient_events`/`AmbientEvent` 的处数, 为什么这几处安全)
        const ALLOWED: &[(&str, usize, &str)] = &[
            (
                "narrative/mod.rs",
                7,
                "字段声明那一行占 2（`pub ambient_events: Vec<AmbientEvent>` 两个词都命中）+ \
                 `Default` 里置空 1 + `AmbientEvent` 结构体声明 1 + 它的文档回指 1 + \
                 两处 `assemble_visible_context` 调用透传 2。\
                 全部是**定义与透传**，没有一处读取其内容",
            ),
            (
                "narrative/decide.rs",
                1,
                "函数签名里的类型 1。形参叫 `ambient`（不含 `ambient_events`），\
                 只用在唯一的展示层插入点上——那是它被允许抵达的终点",
            ),
        ];

        let mut offenders = Vec::new();
        for (rel, src) in production_sources() {
            let found = src.matches("ambient_events").count() + src.matches("AmbientEvent").count();
            let allowed =
                ALLOWED.iter().find(|(p, _, _)| *p == rel).map(|(_, n, _)| *n).unwrap_or(0);
            if found != allowed {
                offenders.push(format!("{rel}：找到 {found} 处，表里登记 {allowed} 处"));
            }
        }
        assert!(
            offenders.is_empty(),
            "🔴 观众礼物（ambient_events）出现在了不该出现的地方：\n  {}\n\n             它买到的是「被看见」，不是「影响力」（open-decisions §5 选项 A）。\n             仲裁 / 确定性不变量 / reducer / StatePatch / 同意门控 / 关系演化 / 里程碑强度\n             **一律不得引用它**，它也绝不写回 NarrativeState。\n             平台红线第一条是「不卖胜负与数值平权」，而这一项一旦被玩家感知为「打赏有用」，\n             撤回就等于承认平台卖过优势——所以这道门宁可拦错，不可放过。\n             确实只是透传 / 展示，就把它加进本用例的 ALLOWED 表并写清理由。",
            offenders.join("\n  ")
        );

        // 表本身别烂掉：登记了却已不存在的文件要删掉，否则它会悄悄放行一个同名新文件。
        let present: std::collections::BTreeSet<String> =
            production_sources().into_iter().map(|(rel, _)| rel).collect();
        for (rel, _, _) in ALLOWED {
            assert!(present.contains(*rel), "🔴 ALLOWED 里登记的 {rel} 已不在源码树里，请删掉这一行");
        }
    }

    fn production_sources() -> Vec<(String, String)> {
        fn strip_test_mods(src: &str) -> String {
            let mut out = String::with_capacity(src.len());
            let mut i = 0usize;
            while i < src.len() {
                let Some(rel) = src[i..].find("#[cfg(test)]") else {
                    out.push_str(&src[i..]);
                    break;
                };
                let marker = i + rel;
                let after = marker + "#[cfg(test)]".len();
                let head = src[after..].trim_start();
                let is_mod_block = head.starts_with("mod ")
                    && head[..head.find(['{', ';']).map(|k| k + 1).unwrap_or(head.len())]
                        .ends_with('{');
                if !is_mod_block {
                    out.push_str(&src[i..after]);
                    i = after;
                    continue;
                }
                out.push_str(&src[i..marker]);
                let Some(brace_rel) = src[after..].find('{') else { break };
                let open = after + brace_rel;
                let mut depth = 0usize;
                let mut end = src.len();
                for (k, c) in src[open..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = open + k + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                i = end;
            }
            out
        }

        fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
            let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
                .unwrap_or_else(|e| panic!("读目录 {dir:?}：{e}"))
                .map(|e| e.expect("目录项").path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(&path, root, out);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if name == "tests.rs" {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap_or_default();
                let rel = path
                    .strip_prefix(root)
                    .expect("相对路径")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, strip_test_mods(&src)));
            }
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        walk(&root, &root, &mut out);
        assert!(out.len() > 10, "🔴 源码遍历只收到 {} 个文件，扫描口径坏了", out.len());
        out
    }
}
