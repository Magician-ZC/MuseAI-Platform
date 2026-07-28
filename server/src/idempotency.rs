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
        "SELECT payload_hash, response_json FROM idempotency_keys WHERE key = $1 AND user_id = $2 AND endpoint = $3",
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
        "INSERT INTO idempotency_keys (key, user_id, endpoint, payload_hash, created_at) VALUES ($1, $2, $3, $4, $5)",
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
            sqlx::query("UPDATE idempotency_keys SET response_json = $1 WHERE key = $2")
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

#[cfg(test)]
mod tests {
    /// 🔴 **每个会动钱 / 动资产的 POST 端点都必须挂幂等守卫。**
    ///
    /// 漏一处的后果是重复扣费或重复发放——客户端重试、移动网络切换、用户连点，
    /// 每一样都会触发。2026-07-28 逐个核过，**当前没有缺口**；本条是防下一个。
    ///
    /// ⚠️ **本条存在的第二个理由：别再手搓扫描器。**
    /// 核这件事时我用 Python 临时写了四版取数，四版各有 bug，而且是**同几种 bug**：
    ///
    /// | bug | 犯了几次 |
    /// |---|---|
    /// | 按第一个 `#[cfg(test)]` 截断源码（中段有测试夹具，其后仍是生产码） | 2 |
    /// | 用 `.route("` 直接匹配（长路由是跨行写的） | 2 |
    /// | `[^)]*?post\(` 跨不过 `get(x).post(y)` 里的第一个 `)` | 1 |
    /// | 函数体取到「下一个 async fn 为止」（窗口混进别人的代码） | 1 |
    ///
    /// 每一次都是靠故障注入才发现的。仓里本来就有正确的取数
    /// （`testkit::production_sources`，花括号配平剥测试模块），本条用它。
    #[test]
    fn red_line_money_endpoints_all_have_an_idempotency_guard() {
        /// 会动钱或动资产的调用。
        const MUTATIONS: &[&str] =
            &["ledger::charge", "grant_item_tx", "grant_card_tx", "grant_mileage_tx"];

        let mut checked: Vec<String> = Vec::new();
        for (path, src) in crate::testkit::production_sources() {
            // 🔴 **不按 `.route(` 找 handler**，扫全仓的 `post(<标识符>)`。
            //
            // 第一版按路由段找，数出 9 个——漏了 `/worlds`（建房要收开房费）。
            // 因为那条路由的 handler 是个**变量**：
            // `let worlds_route = get(list_worlds).post(create_room);` → `.route("/worlds", worlds_route)`。
            // 路由段里根本没有 `post(create_room)`，按路由找**结构上就看不见它**。
            // 这正是本红线最该抓的那类端点（它动钱），却是第一版唯一漏掉的。
            for seg in src.split("post(").skip(1) {
                let handler: String =
                    seg.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if handler.is_empty() {
                    continue;
                }
                let Some(k) = src.find(&format!("async fn {handler}")) else { continue };
                // 花括号配平取函数体（「到下一个 async fn」会把别人的代码算进来）。
                let Some(rel) = src[k..].find('{') else { continue };
                let open = k + rel;
                let mut depth = 0usize;
                let mut end = open;
                for (i, c) in src[open..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = open + i + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let body = &src[k..end];
                if !MUTATIONS.iter().any(|m| body.contains(m)) {
                    continue;
                }
                let key = format!("{path}::{handler}");
                if checked.contains(&key) {
                    continue;
                }
                checked.push(key);
                assert!(
                    body.contains("idempotency::guard"),
                    "🔴 `{handler}`（{path}）会动钱/资产，却没有幂等守卫：\n\
                     客户端重试、移动网络切换、用户连点都会导致重复扣费或重复发放。\n\
                     其余动钱端点都挂了 `idempotency::guard`，别漏这一个。"
                );
            }
        }
        // 🔵 精确棘轮：变多 = 新增了动钱端点（确认它挂了守卫后把数字加一）；
        // 变少 = 端点没了，**或者扫描器坏了**（后者更危险，红线会静默失效）。
        assert_eq!(
            checked.len(),
            10,
            "动钱/资产的 POST 端点数变了（当前 {}）：{checked:?}",
            checked.len()
        );
    }
}
