//! 世界线烙印用例。
//!
//! 钉三样东西，按重要性排：
//! ① **确定性** —— 派生、风化、指纹三个都是纯函数，同输入恒同输出（含顺序）。
//!    这不是洁癖：指纹要进实例种子，而种子是引擎确定性契约的输入。
//! ② **红线** —— 烙印让卡「不一样」，不让任何一张「更强」。检验方式见
//!    `swapping_imprints_between_two_cards_makes_neither_stronger`。
//! ③ **零烙印时逐字节不变** —— 没有烙印的世界（全新库）种子一个 bit 都不能变，
//!    否则黄金世界回归当场红，而那正是这套系统最不该付的代价。

use super::*;

fn facts(collapsed: bool, ticks: i64, cs: Vec<CharacterFacts>) -> WorldFacts {
    WorldFacts { world_id: "w1".into(), collapsed, total_ticks: ticks, characters: cs }
}

fn card(id: &str, stayed: bool, ms: i64, events: i64) -> CharacterFacts {
    CharacterFacts {
        character_id: id.into(),
        stayed_to_end: stayed,
        left_at_tick: None,
        milestone_score_milli: ms,
        event_count: events,
    }
}

fn codes(v: &[Imprint]) -> Vec<(&str, &str, &str)> {
    v.iter().map(|i| (i.character_id.as_str(), i.kind, i.code)).collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// 第 2 步：派生器
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn walking_to_the_end_and_leaving_midway_are_mutually_exclusive() {
    let out = derive_imprints(&facts(false, 40, vec![card("a", true, 0, 0), card("b", false, 0, 0)]));
    let a: Vec<_> = codes(&out).into_iter().filter(|(c, _, _)| *c == "a").collect();
    let b: Vec<_> = codes(&out).into_iter().filter(|(c, _, _)| *c == "b").collect();
    assert!(a.contains(&("a", KIND_CIRCUMSTANCE, "walked_to_the_end")));
    assert!(!a.iter().any(|(_, _, code)| *code == "left_midway"));
    assert!(b.contains(&("b", KIND_CIRCUMSTANCE, "left_midway")));
    assert!(!b.iter().any(|(_, _, code)| *code == "walked_to_the_end"));
}

#[test]
fn pushing_a_milestone_and_reaching_none_are_mutually_exclusive() {
    let out = derive_imprints(&facts(false, 10, vec![card("a", true, 1500, 0), card("b", true, 0, 0)]));
    let c = codes(&out);
    assert!(c.contains(&("a", KIND_CHOICE, "pushed_a_milestone")));
    assert!(!c.iter().any(|(cid, _, code)| *cid == "a" && *code == "no_milestone_reached"));
    // 🔵 未竟痕才是这一对里的重点：它记的是「什么没发生」——一个已经在这张卡面前的机会，
    //    它没走到。下一个世界里会被认出来，而它完全不构成优势。
    assert!(c.contains(&("b", KIND_UNFINISHED, "no_milestone_reached")));
}

#[test]
fn collapse_marks_every_participant_but_normal_ending_marks_none() {
    let collapsed = derive_imprints(&facts(true, 5, vec![card("a", true, 0, 0)]));
    assert!(codes(&collapsed).contains(&("a", KIND_CIRCUMSTANCE, "witnessed_collapse")));
    let normal = derive_imprints(&facts(false, 5, vec![card("a", true, 0, 0)]));
    assert!(!codes(&normal).iter().any(|(_, _, code)| *code == "witnessed_collapse"));
}

/// 🔴 见闻痕只记「有没有」，**不记多少**。
///
/// 把次数做成阈值就会变成「活跃度」，而活跃度是一种绩效评价——
/// 那正是 §8「结算不设勤奋度指标」明令排除的东西。
#[test]
fn leaving_a_trace_is_a_yes_or_no_never_a_score() {
    let few = derive_imprints(&facts(false, 9, vec![card("a", true, 0, 1)]));
    let many = derive_imprints(&facts(false, 9, vec![card("a", true, 0, 999)]));
    assert_eq!(codes(&few), codes(&many), "1 次与 999 次必须派生出完全相同的烙印");
    assert!(codes(&few).contains(&("a", KIND_WITNESS, "left_a_trace")));

    let none = derive_imprints(&facts(false, 9, vec![card("a", true, 0, 0)]));
    assert!(!codes(&none).iter().any(|(_, _, code)| *code == "left_a_trace"));
}

/// 🔴 纯函数：同一份事实恒得同一批烙印，**含顺序**。
///
/// 顺序不是审美问题——派生产物会进指纹、指纹会进实例种子。序变了，
/// 同一个世界重放两次就会抽出不同剧情，引擎的确定性契约当场破。
#[test]
fn derivation_is_a_pure_function_including_order() {
    let f = facts(true, 12, vec![card("z", false, 0, 3), card("a", true, 700, 1)]);
    let first = derive_imprints(&f);
    for _ in 0..5 {
        assert_eq!(derive_imprints(&f), first, "同输入必须恒同输出（含顺序）");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 第 3 步：指纹（进实例种子）
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 **零烙印恒返回空串。**
///
/// 这是黄金世界回归能继续绿的全部依据：空串在 `assembly::resolve_instance_seed` 里
/// 走与接线前**逐字节相同**的路径，没有烙印的世界种子一个 bit 都不变。
#[test]
fn empty_imprints_produce_an_empty_fingerprint() {
    assert_eq!(imprint_fingerprint(&[]), "");
}

/// 🔴 指纹与**输入顺序无关**：SQL 返回序、HashMap 迭代序都不确定，而这个串要进种子。
#[test]
fn fingerprint_is_order_independent_and_deduped() {
    let a = vec![
        ("c2".to_string(), "bond".to_string(), "x".to_string()),
        ("c1".to_string(), "choice".to_string(), "y".to_string()),
    ];
    let b = vec![
        ("c1".to_string(), "choice".to_string(), "y".to_string()),
        ("c2".to_string(), "bond".to_string(), "x".to_string()),
        // 重复项不得改变指纹（同卡同类同码本就只该有一条，重复只可能来自查询口径失误）。
        ("c2".to_string(), "bond".to_string(), "x".to_string()),
    ];
    assert_eq!(imprint_fingerprint(&a), imprint_fingerprint(&b));
}

/// 反向配对：**烙印不同 → 指纹必须不同**。否则整套「复刻内核也抽不到同样剧情」就是空的。
#[test]
fn different_imprints_produce_different_fingerprints() {
    let a = vec![("c1".to_string(), "choice".to_string(), "pushed_a_milestone".to_string())];
    let b = vec![("c1".to_string(), "unfinished".to_string(), "no_milestone_reached".to_string())];
    assert_ne!(imprint_fingerprint(&a), imprint_fingerprint(&b));
}

// ═══════════════════════════════════════════════════════════════════════════
// 第 4 步：风化
// ═══════════════════════════════════════════════════════════════════════════

fn seqs(n: i64) -> Vec<(i64, String, String)> {
    (1..=n).map(|i| (i, "circumstance".to_string(), format!("c{i}"))).collect()
}

#[test]
fn the_newest_imprints_stay_concrete_and_the_oldest_settle() {
    let w = weather(&seqs(30));
    // 最新那几条仍是具体事实。
    assert_eq!(w.last().unwrap().stage, WeatherStage::Fresh);
    // 最旧的沉底（不再单独占位）。
    assert_eq!(w.first().unwrap().stage, WeatherStage::Settled);
}

/// 🔴 **老卡不会更强，只会更模糊。**
///
/// 一张跑过 30 个世界的卡与一张跑过 6 个世界的卡，**占位（非 Settled 的条数）一样多**。
/// 这是「多不等于强」那条论证的可执行版本——没有它，烙印就是变相的成长数值。
#[test]
fn an_old_card_occupies_no_more_context_than_a_young_one() {
    let young = weather(&seqs(6));
    let old = weather(&seqs(60));
    let visible = |v: &[WeatheredImprint]| v.iter().filter(|x| x.stage != WeatherStage::Settled).count();
    assert!(visible(&old) <= imprint_capacity(), "老卡占位不得超过容量");
    assert!(
        visible(&old) >= visible(&young),
        "前置：老卡至少不比新卡少（容量内）"
    );
    assert_eq!(visible(&old), imprint_capacity(), "老卡恰好占满容量，不会更多");
}

/// 🔴 **确定性**：风化只看条数与顺序，不看时间。
///
/// 若按时间分档，同一张卡在两次 replay 里会得到不同的上下文，破坏引擎的确定性契约。
#[test]
fn weathering_depends_only_on_position_never_on_time() {
    let input = seqs(20);
    let first = weather(&input);
    for _ in 0..5 {
        assert_eq!(weather(&input), first);
    }
}

/// 🔴 平权红线的可执行检验：**两张卡的烙印互换，谁会变强？**
///
/// 答案必须是「谁都不会，只是变得不一样」。
/// 这里能检验的那一半是**结构性的**：互换之后两边的占位、分档分布完全对称——
/// 也就是说系统没有给任何一组烙印更多的上下文预算。
/// ⚠️ 另一半（「模型会不会因此表现得更好」）**测不了**，需要真实模型 + 对照组，
/// 见 `docs/build/spec-worldline-imprint.md` §4 风险 2。
#[test]
fn swapping_imprints_between_two_cards_makes_neither_stronger() {
    let a: Vec<(i64, String, String)> =
        (1..=8).map(|i| (i, "choice".into(), format!("a{i}"))).collect();
    let b: Vec<(i64, String, String)> =
        (1..=8).map(|i| (i, "bond".into(), format!("b{i}"))).collect();

    let stages = |v: &[(i64, String, String)]| -> Vec<WeatherStage> {
        weather(v).into_iter().map(|x| x.stage).collect()
    };
    assert_eq!(stages(&a), stages(&b), "同样条数的两组烙印，占位与分档必须完全一致");
}

#[test]
fn capacity_is_parameterized_not_hardcoded_at_the_call_site() {
    // 默认值来自常量（单一事实源），env 可覆盖（§0.2 产品规则参数化）。
    assert_eq!(imprint_capacity(), DEFAULT_IMPRINT_CAPACITY);
}

// ═══════════════════════════════════════════════════════════════════════════
// 接缝：烙印指纹 ↔ 实例种子（第 3 步）
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 这一组是整个第 3 步的**承重断言**。上面那些纯函数用例只证明「指纹算得对」，
// 而真正要保证的是两件事：
//   ① 零烙印时，种子**逐字节不变**（否则黄金世界回归当场红）；
//   ② 烙印不同时，种子**必须不同**（否则「复刻内核也抽不到同样剧情」是空话）。
// 单看任何一边的用例都会绿，漂开的恰恰是它们之间这条缝。

/// 🔴 零烙印 ⇒ 种子与接线前**逐字节相同**。
///
/// 这是这一层能作为「纯增量」上线的全部依据：全新库、全新卡没有烙印，
/// 黄金世界回归与全部既有采样向量一个 bit 都不受影响。
#[test]
fn no_imprints_means_the_seed_is_byte_identical_to_before() {
    let base = crate::assembly::testing::instance_seed_for_test("w1", "cidA\ncidB", 3);
    let resolved = crate::assembly::testing::resolve_seed_for_test("w1", "cidA\ncidB", 3, "");
    assert_eq!(resolved, base, "空烙印指纹必须走原路径，种子不得有任何变化");
}

/// 反向配对：**有烙印就必须换种子**，否则第 3 步等于没做。
#[test]
fn any_imprint_changes_the_seed() {
    let base = crate::assembly::testing::resolve_seed_for_test("w1", "cidA\ncidB", 3, "");
    let with_one =
        crate::assembly::testing::resolve_seed_for_test("w1", "cidA\ncidB", 3, "cidA:choice:x");
    assert_ne!(with_one, base, "有烙印必须换种子");
}

/// 🔴 **同内核、同世界、同阵容，只差经历 ⇒ 抽到的东西不同。**
///
/// 这一条是整套系统对外的那句承诺的可执行版本：
/// 「即使别人一字不差复刻了这张卡的内核，也不会触发同样的剧情」。
#[test]
fn two_cards_with_identical_cores_but_different_pasts_get_different_seeds() {
    let veteran = imprint_fingerprint(&[
        ("cidA".into(), "choice".into(), "pushed_a_milestone".into()),
        ("cidA".into(), "circumstance".into(), "witnessed_collapse".into()),
    ]);
    let rookie = imprint_fingerprint(&[]);
    let s_vet = crate::assembly::testing::resolve_seed_for_test("w1", "cidA", 1, &veteran);
    let s_new = crate::assembly::testing::resolve_seed_for_test("w1", "cidA", 1, &rookie);
    assert_ne!(s_vet, s_new, "经历不同 ⇒ 种子不同 ⇒ 抽到的剧情线/钩子/地点不同");
}

// ═══════════════════════════════════════════════════════════════════════════
// 世界记忆层（迁移 0055）
// ═══════════════════════════════════════════════════════════════════════════
//
// 🔴 这一组守的是**服务端与引擎的口径一致**：
// 引擎运行期间喂给角色的 `yourMemory`（`decide.rs`）与结算时定格进库的记忆（本模块）
// 用的是同一份 `pacingNotes` 和同一条过滤规则。两边漂开会产生最难查的一类偏差——
// **角色运行期间记得的事，结算之后没留下**（或反过来），而两边各自的用例都会是绿的。

#[test]
fn memories_are_filtered_by_the_same_prefix_rule_as_the_engine() {
    let notes: Vec<String> = [
        "A｜Success｜甲推开了门",
        "B｜Failure｜乙没拦住",
        "A｜PartialSuccess｜甲拿到一半",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let a = memories_of(&notes, "A");
    assert_eq!(a.len(), 2, "只该拿到自己的两条：{a:?}");
    assert!(a.iter().all(|n| n.starts_with("A｜")));
    assert!(!a.iter().any(|n| n.contains("乙没拦住")), "🔴 别人的记忆不得混进来");
}

/// 🔴 分隔符必须进前缀：id `A` 不得把 `AB` 的记忆算走。
///
/// 与引擎侧 `a_character_id_that_prefixes_another_does_not_steal_its_memory` 是**同一条判据**，
/// 两边都要有——因为它们是两份独立实现，而这正是本仓反复吃亏的「同一判定的 N 份拷贝」。
/// ⚠️ 真要收口，得把过滤规则抽成引擎的 pub 函数让 server 调；本轮不做，先用双边用例钉住。
#[test]
fn a_prefixing_id_does_not_steal_memories_on_the_server_side_either() {
    let notes: Vec<String> =
        ["A｜Success｜甲的事", "AB｜Success｜乙的事"].iter().map(|s| s.to_string()).collect();
    let a = memories_of(&notes, "A");
    assert_eq!(a, vec!["A｜Success｜甲的事".to_string()]);
}

#[test]
fn a_character_with_no_history_freezes_nothing() {
    let notes: Vec<String> = ["B｜Success｜与我无关".to_string()].to_vec();
    assert!(memories_of(&notes, "A").is_empty());
}

/// 记忆**原样保留**，不做二次加工。
///
/// 加工过的记忆不可对账：`world_events` 是不可回滚的公共事实，而记忆必须能被同一份
/// `pacingNotes` 复算出来——一旦在落库时改写措辞，这条对账链就断了。
#[test]
fn memories_are_stored_verbatim() {
    let raw = "A｜Success｜他在断桥前退了一步，那一步之后同伴死了";
    let got = memories_of(&[raw.to_string()], "A");
    assert_eq!(got, vec![raw.to_string()], "落库前不得改写一个字");
}

// ═══════════════════════════════════════════════════════════════════════════
// 生命层：记忆累积够多之后，卡身上开始有用户改不了的东西
// ═══════════════════════════════════════════════════════════════════════════

/// 🔴 平权红线的可执行检验：**两张卡的生命阶位互换，谁会变强？**
///
/// 答案必须是「谁都不会」。这里能检验的是**结构性**的那一半：
/// 阶位是记忆条数的纯函数，不含任何优待语义，也不进任何判定
/// （`life_snapshot` 的产物没有任何调用方把它喂进 `RoundInput` / 仲裁 / 结算）。
#[test]
fn life_stage_is_a_pure_function_of_how_long_a_card_has_lived() {
    assert_eq!(life_stage(0), LifeStage::Blank);
    assert_eq!(life_stage(29), LifeStage::Blank);
    assert_eq!(life_stage(30), LifeStage::Marked, "到阈值即进阶");
    assert_eq!(life_stage(119), LifeStage::Marked);
    assert_eq!(life_stage(120), LifeStage::Storied);
}

/// 🔴 **阶位封顶**：`Storied` 之上没有了。
///
/// 没有封顶，生命层就会变成一条无限增长的刻度——而任何无限刻度迟早会被人当成战力表。
/// 一张跑过 10 个世界的卡与一张跑过 1000 个世界的卡，**阶位相同**。
#[test]
fn the_ladder_has_a_top_so_it_never_becomes_a_power_scale() {
    assert_eq!(life_stage(120), LifeStage::Storied);
    assert_eq!(life_stage(100_000), LifeStage::Storied, "🔴 跑一万个世界也不会比 Storied 更高");
}

/// 阈值参数化（§0.2），且**调它不改变任何判定**——只改变展示。
#[test]
fn thresholds_are_parameterized_and_ordered() {
    // 默认值单一事实源。
    assert_eq!(env_i64(ENV_LIFE_MARKED_AT, DEFAULT_LIFE_MARKED_AT), DEFAULT_LIFE_MARKED_AT);
    // 🔴 storied 恒 > marked：即使有人把两个 env 配反，阶梯也不能反转
    //（`life_stage` 里 `.max(marked + 1)` 保证）。配错的代价该是「阶梯变窄」而不是「阶梯倒挂」。
    assert!(DEFAULT_LIFE_STORIED_AT > DEFAULT_LIFE_MARKED_AT);
}

/// 生命层**没有存储**：它是记忆的函数，不是另一份状态。
///
/// 这一条用「同一个记忆数恒得同一个阶位」来钉：只要它是纯函数，
/// 就不可能出现「记忆和阶位对不上」的第三种事实——而那种不一致没有任何办法自愈。
#[test]
fn life_stage_never_drifts_from_the_memories_it_is_derived_from() {
    for n in [0, 1, 29, 30, 31, 119, 120, 500] {
        let first = life_stage(n);
        for _ in 0..5 {
            assert_eq!(life_stage(n), first, "同一记忆数必须恒得同一阶位（n={n}）");
        }
    }
}
