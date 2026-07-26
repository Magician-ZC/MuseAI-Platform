//! 数据库：sqlx AnyPool（SQLite dev / Postgres prod），可移植 SQL 子集。
//!
//! 约定：id/外键一律 TEXT；时间戳 BIGINT 毫秒；JSON 载荷 TEXT（serde 序列化）；
//! 布尔 INTEGER 0/1。禁止使用方言特性（JSONB/serial/NOW()），保证双库可跑。
//!
//! ## 参数占位符一律写 `$N`，不写 `?`
//!
//! sqlx 的 `Any` 驱动**原样透传 SQL 字符串**，不做 `?` → `$N` 的方言改写
//! （`sqlx-core/src/any/connection/executor.rs` 把 `query.sql()` 直接交给后端）。
//! 于是 `?` 只在 SQLite 上成立，到 Postgres 一律 42601 语法错。
//!
//! `$N` 则两个库都认：Postgres 原生位置参数；SQLite 把 `$1` 当具名参数，
//! **按首次出现顺序**派号。故约定是**严格顺序编号、且不复用编号**——
//! 一条语句里第 k 个占位符写 `$k`，这样与 `.bind()` 的调用顺序天然一一对应。
//! 该事实钉在 `testkit::tests::numbered_placeholders_are_portable_but_question_marks_are_not`。
//!
//! 拼接式查询（条件 `push_str` 片段、变长 `IN (...)` 列表）用 [`Placeholders`] 发号，
//! 别手写编号——走哪些分支要到运行时才知道。

use sqlx::any::AnyPoolOptions;
use sqlx::AnyPool;

pub async fn connect(database_url: &str) -> Result<AnyPool, sqlx::Error> {
    sqlx::any::install_default_drivers();
    // `:memory:` 每个连接是独立内存库；dev 态必须锁定单个永不回收的连接，
    // 否则跨请求看不到彼此数据（agent-S1 报告）。文件库 / PG 用连接池。
    let is_memory = database_url.contains(":memory:");
    let options = if is_memory {
        AnyPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
    } else {
        AnyPoolOptions::new().max_connections(10)
    };
    let pool = options.connect(database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// 顺序占位符发号器：`$1`、`$2`、……
///
/// 给**运行时才知道形状**的查询用：条件 `WHERE` 片段（`push_str`）、变长 `IN (...)` 列表。
/// 这类语句的编号没法写死在字面量里——走哪些分支、列表多长，编译期不知道。
///
/// 契约：**严格顺序、绝不复用编号**，且发号顺序必须与 `.bind()` 的调用顺序一致。
/// 这正是 `$N` 能同时满足两个库的前提（见本模块头注释）。
#[derive(Debug, Default)]
pub struct Placeholders(usize);

impl Placeholders {
    pub fn new() -> Self {
        Placeholders(0)
    }

    /// 取下一个占位符，如 `$3`。
    pub fn take(&mut self) -> String {
        self.0 += 1;
        format!("${}", self.0)
    }

    /// 取 `n` 个，连成 `IN (...)` 用的逗号列表，如 `$4,$5,$6`。
    pub fn list(&mut self, n: usize) -> String {
        (0..n).map(|_| self.take()).collect::<Vec<_>>().join(",")
    }

    /// 已发出的个数 = 到此为止应当 `.bind()` 的参数个数。
    pub fn count(&self) -> usize {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::Placeholders;

    /// 发号器的两条契约：从 1 起、严格递增不复用；`list` 与 `take` 共享同一序列。
    #[test]
    fn placeholders_are_sequential_and_never_reused() {
        let mut ph = Placeholders::new();
        assert_eq!(ph.take(), "$1");
        assert_eq!(ph.list(3), "$2,$3,$4");
        assert_eq!(ph.take(), "$5");
        assert_eq!(ph.count(), 5);
        assert_eq!(Placeholders::new().list(0), "");
    }
}
