//! 账号与鉴权（S1，agent-S1 填 handler；JWT 与 AuthUser 提取器为共享基础设施，已实现，勿改）。
//!
//! 待实现端点（平台规格 §9.1）：
//! POST /auth/challenge  {phone} → 发验证码（DevSms 打日志；写 sms_challenges，code 只存哈希，5 分钟过期，60s 限频）
//! POST /auth/login      {phone, code} → 校验+消费 challenge → upsert users → 返回 {accessToken, refreshToken, user}
//! POST /auth/refresh    {refreshToken} → 旋转 refresh（旧 token revoke）→ 新对
//! POST /auth/logout     revoke 当前用户全部 refresh
//! POST /identity/verification {provider, referenceId, status} → 仅存第三方返回状态（不存原始证件）
//! 全部副作用端点支持 Idempotency-Key（idempotency 模块工具）。

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::db::{new_id, now_ms};
use crate::error::ApiError;
use crate::idempotency;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub role: String, // user / admin / operator / reviewer / support
    pub exp: i64,
}

pub fn issue_access(secret: &str, user_id: &str, role: &str, ttl_secs: i64) -> Result<String, ApiError> {
    let claims = Claims {
        sub: user_id.to_string(),
        role: role.to_string(),
        exp: crate::db::now_ms() / 1000 + ttl_secs,
    };
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(ApiError::internal)
}

pub fn verify_access(secret: &str, token: &str) -> Result<Claims, ApiError> {
    jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .map(|d| d.claims)
    .map_err(|_| ApiError::Unauthorized)
}

/// 已认证用户提取器：`Authorization: Bearer <jwt>`。所有需要登录的 handler 直接用参数注入。
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub role: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = header.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
        let claims = verify_access(&state.config.jwt_secret, token)?;
        Ok(AuthUser { user_id: claims.sub, role: claims.role })
    }
}

/// 管理员角色守卫（admin_api 用）。
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if matches!(user.role.as_str(), "admin" | "operator" | "reviewer" | "support" | "finance") {
            Ok(AdminUser(user))
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/challenge", post(challenge))
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/identity/verification", post(identity_verification))
}

// ---------------- 请求 / 响应类型（camelCase 与客户端一致） ----------------

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeReq {
    phone: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeResp {
    challenge_id: String,
    expires_at: i64,
    /// 仅 dev_mode 返回，便于联调与测试；生产环境验证码只经 DevSms/真实短信外发。
    #[serde(skip_serializing_if = "Option::is_none")]
    dev_code: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginReq {
    phone: String,
    code: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserView {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phone: Option<String>,
    nickname: String,
    age_declared: i64,
    status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenResp {
    access_token: String,
    refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<UserView>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshReq {
    refresh_token: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityReq {
    provider: String,
    reference_id: String,
    status: String,
}

// ---------------- 辅助 ----------------

const CHALLENGE_TTL_MS: i64 = 5 * 60 * 1000;
const CHALLENGE_RATE_MS: i64 = 60 * 1000;

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 32 字节随机不可预测的 refresh 明文（客户端持有），服务端只存其 sha256。
fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn gen_code() -> String {
    use rand::Rng;
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000))
}

fn idem_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn json_response(body: String) -> Response {
    ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// 写入一条新的 refresh token 记录，返回明文供客户端保存。
async fn store_refresh(db: &sqlx::AnyPool, user_id: &str, ttl_secs: i64) -> Result<String, ApiError> {
    let token = random_token();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at, revoked, created_at) VALUES (?, ?, ?, ?, 0, ?)",
    )
    .bind(new_id("rt"))
    .bind(user_id)
    .bind(sha256_hex(&token))
    .bind(now + ttl_secs * 1000)
    .bind(now)
    .execute(db)
    .await?;
    Ok(token)
}

// ---------------- handler ----------------

/// POST /auth/challenge：发验证码（DevSms 打日志；code 只存 sha256，5 分钟过期，同手机号 60s 限频）。
async fn challenge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChallengeReq>,
) -> Result<Response, ApiError> {
    let phone = req.phone.trim().to_string();
    if phone.is_empty() || phone.len() > 32 {
        return Err(ApiError::BadRequest("手机号无效".into()));
    }
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let key = idem_key(&headers);
    let guard = idempotency::guard(&state.db, &phone, "POST /auth/challenge", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }

    let now = now_ms();
    // 同手机号 60s 限频
    let last: Option<(i64,)> =
        sqlx::query_as("SELECT created_at FROM sms_challenges WHERE phone = ? ORDER BY created_at DESC LIMIT 1")
            .bind(&phone)
            .fetch_optional(&state.db)
            .await?;
    if let Some((last_at,)) = last {
        if now - last_at < CHALLENGE_RATE_MS {
            return Err(ApiError::Conflict("请求过于频繁，请稍后再试".into()));
        }
    }

    let code = gen_code();
    let challenge_id = new_id("chal");
    let expires_at = now + CHALLENGE_TTL_MS;
    sqlx::query(
        "INSERT INTO sms_challenges (id, phone, code_hash, expires_at, consumed, created_at) VALUES (?, ?, ?, ?, 0, ?)",
    )
    .bind(&challenge_id)
    .bind(&phone)
    .bind(sha256_hex(&code))
    .bind(expires_at)
    .bind(now)
    .execute(&state.db)
    .await?;
    // DevSms：验证码打日志（不外发）
    let _ = state.sms.send_code(&phone, &code).await;

    let resp = ChallengeResp {
        challenge_id,
        expires_at,
        dev_code: if state.config.dev_mode { Some(code) } else { None },
    };
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// POST /auth/login：校验+消费 challenge → upsert users → 签发 access+refresh。
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<Response, ApiError> {
    let phone = req.phone.trim().to_string();
    let code = req.code.trim().to_string();
    if phone.is_empty() || code.is_empty() {
        return Err(ApiError::BadRequest("手机号或验证码为空".into()));
    }
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let key = idem_key(&headers);
    let guard = idempotency::guard(&state.db, &phone, "POST /auth/login", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }

    let now = now_ms();
    let challenge: Option<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, code_hash, expires_at, consumed FROM sms_challenges WHERE phone = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&phone)
    .fetch_optional(&state.db)
    .await?;
    let (chal_id, code_hash, expires_at, consumed) =
        challenge.ok_or_else(|| ApiError::BadRequest("验证码不存在，请重新获取".into()))?;
    if consumed != 0 {
        return Err(ApiError::BadRequest("验证码已使用，请重新获取".into()));
    }
    if expires_at < now {
        return Err(ApiError::BadRequest("验证码已过期，请重新获取".into()));
    }
    if code_hash != sha256_hex(&code) {
        return Err(ApiError::BadRequest("验证码错误".into()));
    }
    sqlx::query("UPDATE sms_challenges SET consumed = 1 WHERE id = ?")
        .bind(&chal_id)
        .execute(&state.db)
        .await?;

    // upsert user（服务端权威：手机号唯一，不重复建号）
    let existing: Option<(String, String, i64, String)> =
        sqlx::query_as("SELECT id, nickname, age_declared, status FROM users WHERE phone = ?")
            .bind(&phone)
            .fetch_optional(&state.db)
            .await?;
    let user = if let Some((id, nickname, age, status)) = existing {
        if status == "banned" {
            return Err(ApiError::Forbidden);
        }
        sqlx::query("UPDATE users SET updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&id)
            .execute(&state.db)
            .await?;
        UserView { id, phone: Some(phone.clone()), nickname, age_declared: age, status }
    } else {
        let id = new_id("user");
        sqlx::query(
            "INSERT INTO users (id, phone, nickname, age_declared, status, created_at, updated_at) VALUES (?, ?, '', 0, 'active', ?, ?)",
        )
        .bind(&id)
        .bind(&phone)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
        UserView { id, phone: Some(phone.clone()), nickname: String::new(), age_declared: 0, status: "active".into() }
    };

    let access = issue_access(&state.config.jwt_secret, &user.id, "user", state.config.access_ttl_secs)?;
    let refresh = store_refresh(&state.db, &user.id, state.config.refresh_ttl_secs).await?;
    let resp = TokenResp { access_token: access, refresh_token: refresh, user: Some(user) };
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// POST /auth/refresh：校验 + 旋转（旧 refresh revoke，签发新对）。
async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RefreshReq>,
) -> Result<Response, ApiError> {
    let presented = req.refresh_token.trim();
    if presented.is_empty() {
        return Err(ApiError::Unauthorized);
    }
    let token_hash = sha256_hex(presented);
    let payload_hash = idempotency::hash_payload(token_hash.as_bytes());
    let key = idem_key(&headers);
    let guard = idempotency::guard(&state.db, &token_hash, "POST /auth/refresh", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }

    let now = now_ms();
    let row: Option<(String, String, i64, i64)> =
        sqlx::query_as("SELECT id, user_id, expires_at, revoked FROM refresh_tokens WHERE token_hash = ?")
            .bind(&token_hash)
            .fetch_optional(&state.db)
            .await?;
    let (rt_id, user_id, expires_at, revoked) = row.ok_or(ApiError::Unauthorized)?;
    if revoked != 0 || expires_at < now {
        return Err(ApiError::Unauthorized);
    }
    sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE id = ?")
        .bind(&rt_id)
        .execute(&state.db)
        .await?;
    let access = issue_access(&state.config.jwt_secret, &user_id, "user", state.config.access_ttl_secs)?;
    let new_refresh = store_refresh(&state.db, &user_id, state.config.refresh_ttl_secs).await?;
    let resp = TokenResp { access_token: access, refresh_token: new_refresh, user: None };
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// POST /auth/logout：revoke 当前用户全部未失效 refresh。
async fn logout(State(state): State<AppState>, user: AuthUser, headers: HeaderMap) -> Result<Response, ApiError> {
    let payload_hash = idempotency::hash_payload(user.user_id.as_bytes());
    let key = idem_key(&headers);
    let guard = idempotency::guard(&state.db, &user.user_id, "POST /auth/logout", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }
    sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE user_id = ? AND revoked = 0")
        .bind(&user.user_id)
        .execute(&state.db)
        .await?;
    let body = serde_json::to_string(&serde_json::json!({ "success": true })).unwrap();
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

/// POST /identity/verification：仅存 provider + referenceId + status（不存证件原文，§2.2/§14）。
async fn identity_verification(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<IdentityReq>,
) -> Result<Response, ApiError> {
    let provider = req.provider.trim();
    let reference_id = req.reference_id.trim();
    if provider.is_empty() || reference_id.is_empty() {
        return Err(ApiError::BadRequest("provider 与 referenceId 必填".into()));
    }
    if !matches!(req.status.as_str(), "pending" | "verified" | "failed") {
        return Err(ApiError::BadRequest("status 非法".into()));
    }
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let key = idem_key(&headers);
    let guard =
        idempotency::guard(&state.db, &user.user_id, "POST /identity/verification", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }
    let id = new_id("idv");
    sqlx::query(
        "INSERT INTO identity_verification_refs (id, user_id, provider, reference_id, status, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&user.user_id)
    .bind(provider)
    .bind(reference_id)
    .bind(&req.status)
    .bind(now_ms())
    .execute(&state.db)
    .await?;
    let resp = serde_json::json!({ "id": id, "status": req.status });
    let body = serde_json::to_string(&resp).unwrap();
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

#[cfg(test)]
pub(crate) mod tests;
