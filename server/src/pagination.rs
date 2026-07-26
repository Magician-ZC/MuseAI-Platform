//! 游标分页的复合键（keyset）工具。
//!
//! ## 为什么单列游标会**静默丢行**
//!
//! 仓库里的游标分页原本一律是「末行 `created_at` 当游标 + 下一页 `WHERE created_at < cursor`」。
//! 这个形状只在 `created_at` 唯一时才正确。而平台侧大量写入是**批量同毫秒**的
//! （一次结算发多件道具、一个 tick 的整批事件、一批同时排定的通知都共用同一个 `now_ms()`），
//! 于是并列是常态：
//!
//! - 若一组并列行**横跨页边界**，`created_at < cursor` 的**严格小于**会把这一页没取到的
//!   同值行整组跳过 —— 那些行**永远不会出现在任何一页**。这不是排序抖动，是**数据丢失**，
//!   且两个库上都会发生（SQLite 也一样，只是顺序稳定所以每次丢的是同一批，更难被发现）。
//! - 改成 `<=` 只会把丢行换成无限重复同一页，不是修复。
//!
//! ## 修法：把游标补成「排序键的全部列」
//!
//! 排序键补上唯一次级键 `id` 后是全序 `(created_at DESC, id DESC)`；游标也必须同样是二元组：
//!
//! ```sql
//! WHERE ... AND ($2 IS NULL OR created_at < $3 OR (created_at = $4 AND id < $5))
//! ORDER BY created_at DESC, id DESC LIMIT n
//! ```
//!
//! 边界那组并列行于是被 `id` 精确切开：上一页取到 `id` 为止，下一页从它**之后**接着取，
//! 不重不漏。
//!
//! ## 兼容旧客户端（只回传 `cursor`、不回传 `cursorId`）
//!
//! 第二段缺省时用**空串**当下界：所有 id 都是 `new_id()` 产的 `前缀_hex` 非空串，
//! `id < ''` 恒假 ⇒ 第三个析取项整体不成立，SQL 退化成原来的 `created_at < cursor`，
//! 逐字节等价于旧行为（旧客户端不会因为服务端升级而改变分页结果，只是仍享受不到不丢行）。
//!
//! 用空串而非 `NULL`：`id < NULL` 是 NULL 不是 FALSE，虽然在 `OR` 里同样不使该项为真，
//! 但要多一层 `$n IS NOT NULL` 判空才读得懂；且 PG 对纯 `$n` 比较的类型推断更啰嗦。
//! 空串是这张表的值域里天然的下确界，语义与实现都更直白。

/// 复合游标第二段的绑定值：`Some(id)` 原样透传，`None` → 空串（恒假下界，退化为单列游标）。
pub fn cursor_id_bound(cursor_id: Option<&str>) -> String {
    cursor_id.unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::cursor_id_bound;

    #[test]
    fn missing_cursor_id_degrades_to_an_always_false_lower_bound() {
        // 缺省 → 空串。SQL 里 `id < ''` 对任何 `new_id()` 产的 id 恒假 ⇒ 复合项不成立，
        // 整条 WHERE 退化为旧的单列游标语义（旧客户端零行为变化）。
        assert_eq!(cursor_id_bound(None), "");
        assert_eq!(cursor_id_bound(Some("ntf_abc")), "ntf_abc");
        // 空串游标与"没传"等价——不额外区分（也没有 id 会等于空串）。
        assert_eq!(cursor_id_bound(Some("")), "");
    }

    /// `new_id` 的形状保证了空串是严格下界：任何 id 都非空，故 `id < ''` 恒假。
    #[test]
    fn generated_ids_are_never_empty_so_the_empty_string_is_a_strict_lower_bound() {
        for prefix in ["ntf", "rep", "srp"] {
            let id = crate::db::new_id(prefix);
            assert!(!id.is_empty());
            assert!(id.as_str() > "", "id={id} 必须严格大于空串，否则退化下界会误放行");
        }
    }
}
