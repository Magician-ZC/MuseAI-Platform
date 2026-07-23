//! 服务配置：环境变量驱动，dev 态零配置可跑（SQLite 内存 + 内存队列 + Dev providers）。

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `sqlite::memory:`（默认，dev）/ `sqlite://muse.db` / `postgres://...`
    pub database_url: String,
    pub bind_addr: String,
    pub jwt_secret: String,
    /// access token 有效期（秒）
    pub access_ttl_secs: i64,
    pub refresh_ttl_secs: i64,
    /// dev 模式：短信验证码打日志、审核直通、支付模拟
    pub dev_mode: bool,
    /// 对象存储根目录（立绘/切片/导出包）
    pub object_store_dir: String,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        Self {
            database_url: env("MUSE_DATABASE_URL", "sqlite::memory:"),
            bind_addr: env("MUSE_BIND", "127.0.0.1:8787"),
            jwt_secret: env("MUSE_JWT_SECRET", "dev-secret-change-me"),
            access_ttl_secs: env("MUSE_ACCESS_TTL", "3600").parse().unwrap_or(3600),
            refresh_ttl_secs: env("MUSE_REFRESH_TTL", "2592000").parse().unwrap_or(2_592_000),
            dev_mode: env("MUSE_DEV", "1") == "1",
            object_store_dir: env("MUSE_OBJECT_DIR", "./muse-objects"),
        }
    }
}
