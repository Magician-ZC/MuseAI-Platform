//! 全局测试建池 helper（仅 `#[cfg(test)]` 构建）。
//!
//! 生产跑 Postgres（`MUSE_DATABASE_URL=postgres://...`），而测试历来只跑 SQLite——
//! 「双数据库可移植 SQL 子集」（`db.rs` 头注释那条约定）因此长期零自动化验证。
//! 本模块把 11 处各自 `connect("sqlite::memory:")` 收敛成一个入口，由环境变量
//! `MUSE_TEST_DATABASE_URL` 决定连哪个库。
//!
//! 🔴 **默认值就是 `sqlite::memory:`**：不设该变量时，建池参数与收敛前逐字等价
//! （单连接 + 永不回收），本地 `cargo test` 的行为和耗时不变，也不依赖任何外部服务。
//! Postgres 是 CI 上**增量的第二遍**。
//!
//! ## Postgres 下的测试隔离
//!
//! `sqlite::memory:` 每个连接天然是一个独立空库，PG 没有这回事——并行跑的用例会共享
//! 同一个数据库互相污染。这里的方案是**每个池一个独立 schema**：
//!
//! 1. 用原子计数器取号 → schema 名 `{prefix}_{n}`（`MUSE_TEST_SCHEMA_PREFIX`，默认 `muse_test`）。
//!    ⚠️ 计数器而非随机数/时间戳：确定性契约（`docs/VALIDATION.md` §4「禁三样」——
//!    不用系统随机、不用浮点 RNG、不依赖 map 迭代序）要求同一份用例集每次跑出同一批 schema 名，
//!    这样「第 7 个池上挂了」才是可复现、可直接 `psql` 进去看的线索。
//! 2. `DROP SCHEMA IF EXISTS ... CASCADE` + `CREATE SCHEMA`——因为计数器每个进程从 0 起，
//!    schema 名跨轮复用，建之前先清掉上一轮的同名残留。故**清理发生在建池时而不是析构时**：
//!    不需要给 `AppState` 挂 `Drop`（异步析构在 Rust 里做不干净），残留 schema 数量上界 =
//!    历史最大并发用例数，恒定不增长。
//! 3. 池上挂 `after_connect`，每条连接一开就 `SET search_path`——迁移建的 `_sqlx_migrations`
//!    和 66 张业务表全部落进该 schema。
//!
//! ⚠️ 同一个 PG 库上**不要并行跑两个测试进程**（如 default 与 `--features billing,arena`
//! 同时开跑）：计数器是进程内的，两个进程会取到同一批号。CI 里两遍是串行的；确要并行请用
//! `MUSE_TEST_SCHEMA_PREFIX` 给它们各自的前缀。

use sqlx::any::AnyPoolOptions;
use sqlx::AnyPool;
use std::sync::atomic::{AtomicU64, Ordering};

static INIT: std::sync::Once = std::sync::Once::new();
/// schema 取号器。确定性：进程内从 0 单调递增，不掺随机数/时间戳。
static SCHEMA_SEQ: AtomicU64 = AtomicU64::new(0);

/// 本轮测试连哪个库。默认 `sqlite::memory:`。
pub fn test_database_url() -> String {
    std::env::var("MUSE_TEST_DATABASE_URL").unwrap_or_else(|_| "sqlite::memory:".to_string())
}

fn is_postgres(url: &str) -> bool {
    url.starts_with("postgres://") || url.starts_with("postgresql://")
}

/// 当前是否跑在 Postgres 那遍上。
///
/// 提供给「同一断言在两个库上都成立、但**准备数据**的方式必须不同」的场景（例如 SQLite
/// 允许往 INTEGER 列塞字符串而 PG 不允许）。
/// 🔴 **不可用来跳过用例或放宽断言**——那样等于把可移植性 bug 藏起来，正是本模块要堵的东西。
pub fn on_postgres() -> bool {
    is_postgres(&test_database_url())
}

/// 建一个跑完全部迁移的空库。SQLite → 进程内独立内存库；PG → 独立 schema。
///
/// **单连接**（两个库都是）。绝大多数用例都该用它。
pub async fn test_pool() -> AnyPool {
    INIT.call_once(sqlx::any::install_default_drivers);
    let url = test_database_url();
    if is_postgres(&url) {
        postgres_pool(&url, 1).await
    } else {
        sqlite_pool(&url).await
    }
}

/// **短连接获取超时**的单连接池：只给「故意在事务里再借连接、量出自锁后果」的用例使用。
///
/// 🔴 为什么需要它：单连接池上，事务持有唯一连接期间任何再借连接的操作都会**挂满连接获取
/// 超时**（默认 30 秒）再 fail-closed，症状是「莫名变慢 + 悄悄拿到默认值」而不是显式报错。
/// 这个失败模式**必须有可执行的证据**——它是 `progression::SettlementFlags`
/// 与 `flags::MIGRATION_NOTES` 里「一律进事务前解析」那条约束的全部依据。
/// 但用默认 30 秒去演示它，等于给每次全量测试加 30 秒；把超时压到几百毫秒即可，
/// 现象一模一样（**取不到连接**这件事与等多久无关）。
///
/// ⚠️ 别拿它跑普通用例：超时压这么短，正常的慢查询也会变成假红。
pub async fn test_pool_short_acquire(acquire_timeout_ms: u64) -> AnyPool {
    INIT.call_once(sqlx::any::install_default_drivers);
    let url = test_database_url();
    let d = std::time::Duration::from_millis(acquire_timeout_ms.max(1));
    if is_postgres(&url) {
        postgres_pool_tuned(&url, 1, Some(d)).await
    } else {
        sqlite_pool_tuned(&url, Some(d)).await
    }
}

/// **多连接**测试池：只给需要「两个事务真的同时在跑」的并发用例使用。
///
/// 🔴 **不要改 [`test_pool`] 的 `max_connections`**。那里的 1 是刻意的（见 `postgres_pool`
/// 里的注释）：多连接下「A 未提交的写 B 看不见」会让大量既有用例以与被测点无关的理由失败，
/// 把真正的问题淹掉。所以并发能力是**按用例显式索取**的，不是全局放开的。
///
/// ⚠️ **SQLite 那遍拿不到真并发，这条必须清楚**：`sqlite::memory:` 每条连接是一个**独立空库**，
/// 多连接会直接把用例变成「各写各的库」。故 SQLite 分支原样回落到单连接池，
/// 并发用例在 SQLite 上退化为顺序执行——它仍然验证分配口径本身（连续、不重、不漏），
/// 但**不构成任何并发安全的证据**（SQLite 的单写者锁本就让 read-modify-write 事实上串行，
/// 旧口径在这里同样会绿 = 假绿）。并发结论只能来自
/// `MUSE_TEST_DATABASE_URL=postgres://...` 那一遍。
pub async fn test_pool_concurrent(max_connections: u32) -> AnyPool {
    INIT.call_once(sqlx::any::install_default_drivers);
    let url = test_database_url();
    if is_postgres(&url) {
        postgres_pool(&url, max_connections.max(1)).await
    } else {
        sqlite_pool(&url).await
    }
}

/// 单连接 + 永不回收：`:memory:` 每连接一个独立库，换连接 = 换成空库。
async fn sqlite_pool(url: &str) -> AnyPool {
    sqlite_pool_tuned(url, None).await
}

/// `acquire_timeout = None` 走 sqlx 默认（30s）；`Some(d)` 只给
/// [`test_pool_short_acquire`] 用。
async fn sqlite_pool_tuned(url: &str, acquire_timeout: Option<std::time::Duration>) -> AnyPool {
    let mut opts = AnyPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None);
    if let Some(d) = acquire_timeout {
        opts = opts.acquire_timeout(d);
    }
    let pool = opts
        .connect(url)
        .await
        .expect("connect sqlite memory");
    sqlx::migrate!("./migrations").run(&pool).await.expect("run migrations");
    pool
}

/// `max_connections`：默认 1（见下方注释）；只有 [`test_pool_concurrent`] 会传 >1。
async fn postgres_pool(url: &str, max_connections: u32) -> AnyPool {
    postgres_pool_tuned(url, max_connections, None).await
}

async fn postgres_pool_tuned(
    url: &str,
    max_connections: u32,
    acquire_timeout: Option<std::time::Duration>,
) -> AnyPool {
    let n = SCHEMA_SEQ.fetch_add(1, Ordering::SeqCst);
    let prefix =
        std::env::var("MUSE_TEST_SCHEMA_PREFIX").unwrap_or_else(|_| "muse_test".to_string());
    // 前缀只允许 [A-Za-z0-9_]：schema 名无法参数化绑定，只能拼串。
    assert!(
        prefix.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "MUSE_TEST_SCHEMA_PREFIX 只允许字母/数字/下划线，实际为 {prefix:?}"
    );
    let schema = format!("{prefix}_{n}");

    // 建 schema 用一条临时连接，建完即关——不占用测试池的连接额度。
    let admin = AnyPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("connect postgres (admin)");
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop stale test schema");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create test schema");
    admin.close().await;

    // 连接数**默认**与 SQLite 那遍对齐（1）：不是性能取舍，是为了两遍的**可见性语义一致**——
    // 多连接时「A 连接未提交的写 B 连接看不见」会让用例以与 SQL 方言无关的理由失败，
    // 把真正的可移植性问题淹掉。
    // 例外只有 `test_pool_concurrent`：并发正确性（如 `world_event_seq` 发号）在单连接池上
    // **根本无法验证**——两个事务从来不会同时在跑。那类用例按需索取连接数，其余一律走 1。
    let hook_schema = schema.clone();
    let mut opts = AnyPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None);
    if let Some(d) = acquire_timeout {
        opts = opts.acquire_timeout(d);
    }
    let pool = opts
        .after_connect(move |conn, _meta| {
            let schema = hook_schema.clone();
            Box::pin(async move {
                sqlx::query(&format!("SET search_path TO {schema}")).execute(&mut *conn).await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .expect("connect postgres (test schema)");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .unwrap_or_else(|e| panic!("run migrations on postgres schema {schema}: {e}"));
    pool
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    /// 建池 helper 自身的契约：迁移在**本轮配置的库**上一条不落地跑完，且两个池互不可见。
    ///
    /// 这是 Postgres 那遍今天唯一**全绿**的部分，它锁住的东西不小：
    /// `migrations/0001-0041` 39 份 DDL（`0023`/`0028` 是有意空号）在 PG 上逐条通过，
    /// 即 `db.rs` 头注释那条「双库可移植 SQL 子集」在 **schema 层**成立。
    #[tokio::test]
    async fn migrations_apply_and_pools_are_isolated() {
        let expected = sqlx::migrate!("./migrations").migrations.len() as i64;
        let a = test_pool().await;
        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&a)
            .await
            .expect("read _sqlx_migrations");
        assert_eq!(applied, expected, "嵌入的迁移应全部落地");

        // 隔离：a 写的行，b 一行都看不到（SQLite 靠独立内存库，PG 靠独立 schema）。
        sqlx::query(
            "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind("u_isolation_probe")
        .bind("")
        .bind(1_i64)
        .bind("active")
        .bind(0_i64)
        .bind(0_i64)
        .execute(&a)
        .await
        .expect("seed into pool a");

        let b = test_pool().await;
        let leaked: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&b).await.expect("count b");
        assert_eq!(leaked, 0, "新池必须是空库；PG 下这条挂掉说明 schema 隔离失效");
    }

    /// 🔴 **本仓库当前不可移植的根因，钉在这里。**
    ///
    /// sqlx 的 `Any` 驱动**原样透传 SQL 字符串**，不做 `?` → `$N` 的方言改写
    /// （见 `sqlx-core-0.8.6/src/any/connection/executor.rs`：`self.backend.fetch_many(query.sql(), ..)`）。
    /// 于是全仓 900+ 条写成 `?` 的语句在 SQLite 上正常、到 PG 一律 42601 语法错——
    /// 也就是说 `MUSE_DATABASE_URL=postgres://...` 这条生产路径从未真正跑通过。
    ///
    /// 位置占位符 `$N` 则**两个库都认**：PG 原生；SQLite 把 `$1` 当具名参数，按首次出现
    /// 顺序分配序号，故只要**严格顺序编号且不复用**，`.bind()` 的位置绑定就对得上。
    /// 这就是迁移路径——但那是 900+ 处调用点的改写，不在本任务范围内。
    ///
    /// ⚠️ 这里按库分支断言的是**驱动层的既成事实**（两条都为真），不是拿分支跳过用例、
    /// 也不是放宽断言——把差异藏起来正是本模块要堵的东西。
    #[tokio::test]
    async fn numbered_placeholders_are_portable_but_question_marks_are_not() {
        let pool = test_pool().await;
        let insert = |ph: &str| {
            format!(
                "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) \
                 VALUES ({ph})"
            )
        };

        // `$N`：两个库都必须通过。
        sqlx::query(&insert("$1, $2, $3, $4, $5, $6"))
            .bind("u_numbered")
            .bind("")
            .bind(1_i64)
            .bind("active")
            .bind(0_i64)
            .bind(0_i64)
            .execute(&pool)
            .await
            .expect("$N 占位符必须在两个库上都可用——它是唯一的可移植写法");
        let got: String = sqlx::query("SELECT status FROM users WHERE id = $1")
            .bind("u_numbered")
            .fetch_one(&pool)
            .await
            .expect("$N 也必须能用在 WHERE 上")
            .get("status");
        assert_eq!(got, "active");

        // `?`：SQLite 认，PG 不认。
        let question = sqlx::query(&insert("?, ?, ?, ?, ?, ?"))
            .bind("u_question")
            .bind("")
            .bind(1_i64)
            .bind("active")
            .bind(0_i64)
            .bind(0_i64)
            .execute(&pool)
            .await;
        if on_postgres() {
            let err = question.expect_err(
                "若这条开始通过，说明 sqlx 已支持 ? → $N 改写，本注释与 CI 的非阻塞标记应一并撤掉",
            );
            assert!(
                err.to_string().contains("syntax error"),
                "PG 上 `?` 应报语法错，实际：{err}"
            );
        } else {
            question.expect("SQLite 上 `?` 是正常写法");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 源码级红线扫描的共用取数
// ═══════════════════════════════════════════════════════════════════════════

/// 递归收集 `server/src` 下的**生产**源码，返回 `(相对路径, 剥掉测试模块后的源码)`。
///
/// 🔴 **必须按花括号配平剥离 `#[cfg(test)] mod X { .. }`，不能按模块名截断。**
///
/// 本仓库的内联测试模块**不止叫 `tests`**：`app::cors_tests`、`assembly::sampling_tests` /
/// `container_tests` / `member_order_tests` 都是。按 `"\nmod tests {"` 截断的扫描器会把
/// 这些文件的测试代码**当成生产代码扫**，于是任何红线断言都可能被一段测试夹具里的字符串
/// 误伤——我写「`runtime_flags` 只许 `flags` 读」那条红线时就当场被 `assembly` 的
/// `container_tests` 误报了一次。
///
/// ⚠️ 也不能按「第一个 `#[cfg(test)]`」截断：本仓库有若干**测试专用夹具**
/// （`invitations::InvitationSwitch`、`assembly::ContainerSwitch` 等）定义在文件中段，
/// 其后仍有生产代码。截断会把那些生产代码整段漏掉，让扫描形同虚设。
///
/// 跳过 `tests.rs` / `testkit.rs` 整个文件；遍历前排序，断言的失败信息不随文件系统顺序抖动。
pub fn production_sources() -> Vec<(String, String)> {
    fn strip_test_mods(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let bytes = src.as_bytes();
        let mut i = 0usize;
        while i < src.len() {
            // 找下一个 `#[cfg(test)]`，其后（跳过空白/属性行）若是 `mod NAME {` 就整块跳过。
            let Some(rel) = src[i..].find("#[cfg(test)]") else {
                out.push_str(&src[i..]);
                break;
            };
            let marker = i + rel;
            let after = marker + "#[cfg(test)]".len();
            // `#[cfg(test)]` 之后到行首非空白的第一个 token
            let rest = &src[after..];
            let trimmed_at = rest.len() - rest.trim_start().len();
            let head = rest.trim_start();
            let is_mod_block = head.starts_with("mod ") && {
                // `mod NAME;` 是外置声明（对应文件已被整体跳过），不是块
                head[..head.find(['{', ';']).map(|k| k + 1).unwrap_or(head.len())].ends_with('{')
            };
            if !is_mod_block {
                out.push_str(&src[i..after]);
                i = after;
                continue;
            }
            out.push_str(&src[i..marker]);
            // 从 `{` 起做花括号配平
            let brace_at = after + trimmed_at + head.find('{').expect("上面已确认有 {");
            let mut depth = 0i32;
            let mut j = brace_at;
            while j < bytes.len() {
                match bytes[j] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            i = j;
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
            if name == "tests.rs" || name == "testkit.rs" {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {path:?}：{e}"));
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
    assert!(out.len() > 50, "🔴 源码遍历只收到 {} 个文件，扫描口径坏了", out.len());
    out
}
