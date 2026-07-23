//! 幂等键工具（共享基础设施，已实现，agent 勿改）：
//! 副作用端点统一调用 `guard`：同 key 同载荷 → 返回缓存响应；同 key 异载荷 → 409。

use sqlx::AnyPool;

use crate::error::ApiError;

pub struct IdempotencyGuard {
    pub cached_response: Option<String>,
    key: Option<String>,
}

pub async fn guard(
    db: &AnyPool,
    user_id: &str,
    endpoint: &str,
    key: Option<&str>,
    payload_hash: &str,
) -> Result<IdempotencyGuard, ApiError> {
    let Some(key) = key else {
        return Ok(IdempotencyGuard { cached_response: None, key: None });
    };
    let existing: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT payload_hash, response_json FROM idempotency_keys WHERE key = ? AND user_id = ? AND endpoint = ?",
    )
    .bind(key)
    .bind(user_id)
    .bind(endpoint)
    .fetch_optional(db)
    .await?;

    if let Some((hash, response)) = existing {
        if hash != payload_hash {
            return Err(ApiError::IdempotencyMismatch);
        }
        return Ok(IdempotencyGuard { cached_response: response, key: Some(key.to_string()) });
    }

    sqlx::query(
        "INSERT INTO idempotency_keys (key, user_id, endpoint, payload_hash, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(key)
    .bind(user_id)
    .bind(endpoint)
    .bind(payload_hash)
    .bind(crate::db::now_ms())
    .execute(db)
    .await?;
    Ok(IdempotencyGuard { cached_response: None, key: Some(key.to_string()) })
}

impl IdempotencyGuard {
    /// handler 成功后回填响应
    pub async fn store_response(&self, db: &AnyPool, response_json: &str) -> Result<(), ApiError> {
        if let Some(key) = &self.key {
            sqlx::query("UPDATE idempotency_keys SET response_json = ? WHERE key = ?")
                .bind(response_json)
                .bind(key)
                .execute(db)
                .await?;
        }
        Ok(())
    }
}

pub fn hash_payload(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}
