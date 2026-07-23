//! 数据库：sqlx AnyPool（SQLite dev / Postgres prod），可移植 SQL 子集。
//!
//! 约定：id/外键一律 TEXT；时间戳 BIGINT 毫秒；JSON 载荷 TEXT（serde 序列化）；
//! 布尔 INTEGER 0/1。禁止使用方言特性（JSONB/serial/NOW()），保证双库可跑。

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
