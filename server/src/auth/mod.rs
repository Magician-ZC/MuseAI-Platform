//! 账号与鉴权（S1，agent-S1 填 handler；JWT 与 AuthUser 提取器为共享基础设施，已实现，勿改）。
//!
//! 待实现端点（平台规格 §9.1）：
//! POST /auth/challenge  {phone} → 发验证码（DevSms 打日志；写 sms_challenges，code 只存哈希，5 分钟过期，60s 限频）
//! POST /auth/login      {phone, code} → 校验+消费 challenge → upsert users → 返回 {accessToken, refreshToken, user}
//! POST /auth/refresh    {refreshToken} → 旋转 refresh（旧 token revoke）→ 新对
//! POST /auth/logout     revoke 当前用户全部 refresh
//! POST /auth/age-declaration {isAdult} → 写 users.age_declared（成年=1 / 未成年=2），支撑 §2.2 未成年默认保护（billing 保守拒充）
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
        .route("/auth/age-declaration", post(age_declaration))
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgeDeclarationReq {
    is_adult: bool,
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

/// 本模块的应答走 `json_response(serde_json::to_string(&resp).unwrap())`；那个 `unwrap`
/// 静态安全：`resp` 恒为 `json!` 产出的 `serde_json::Value`，其 `Serialize` 无失败分支
///（serde_json 仅有的两个报错来源——非字符串 map 键、NaN/Inf——`json!` 都构造不出来），
/// 写入目标是内存 `String` 而非 IO。故请求体再脏也炸不到这里。
fn json_response(body: String) -> Response {
    ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// 写入一条新的 refresh token 记录，返回明文供客户端保存。
async fn store_refresh(db: &sqlx::AnyPool, user_id: &str, ttl_secs: i64) -> Result<String, ApiError> {
    let token = random_token();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at, revoked, created_at) VALUES ($1, $2, $3, $4, 0, $5)",
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
    // 同手机号 60s 限频。
    //
    // 🔴 写作 `MAX(created_at)` 而不是 `ORDER BY created_at DESC LIMIT 1`：这里要的是一个**值**
    // （最近一次发码的时刻），不是某一**行**。`sms_challenges` 没有单调列、`id` 是 uuid v4，
    // 同毫秒的两行谁排前面在 PG 上是任意的——但对本判断毫无影响，因为它们的 `created_at` 相等。
    // 用聚合把这件事在**查询形状**上说清楚：无行可排 ⇒ 无排序稳定性问题可言。
    // （给排序补一个 `id` 次级键只会把"不稳定的任意"变成"稳定的任意"，是**假确定性**：
    //   uuid 大小与时间无关，选中的那行并不因此更"新"。见 docs/VALIDATION.md §3.3。）
    // 无行时 `MAX()` 返回 NULL → `None`，与原 `fetch_optional` 的 None 分支同义。
    let last: Option<i64> = sqlx::query_scalar("SELECT MAX(created_at) FROM sms_challenges WHERE phone = $1")
        .bind(&phone)
        .fetch_one(&state.db)
        .await?;
    if let Some(last_at) = last {
        if now - last_at < CHALLENGE_RATE_MS {
            return Err(ApiError::Conflict("请求过于频繁，请稍后再试".into()));
        }
    }

    let code = gen_code();
    let challenge_id = new_id("chal");
    let expires_at = now + CHALLENGE_TTL_MS;
    sqlx::query(
        "INSERT INTO sms_challenges (id, phone, code_hash, expires_at, consumed, created_at) VALUES ($1, $2, $3, $4, 0, $5)",
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
    // ⚠️ **已知的不确定排序，有意不补次级键**（登记于 docs/VALIDATION.md §3.3）。
    //
    // 这里要的是「这个号最新那条验证码」——选中哪一行**就是拿去校验 OTP hash 的那一行**，
    // 不是显示顺序。但 `sms_challenges` 里**没有任何单调列**可以表达"最新"：
    // `created_at` 毫秒可并列、`id` 是 uuid v4（大小与时间无关）。
    // 给它补 `id DESC` 只会把"不稳定的任意"换成"稳定的任意"——**假确定性**：
    // 选中的仍不是语义上更新的那条，只是每次都选同一条错的。故本行保持原状，把问题留在明处。
    //
    // 并列唯一的产生路径是 `challenge` 端 60s 限频的 TOCTOU（两个并发请求都读到"无近期记录"，
    // 各插一行）。那是**并发正确性**问题，不是排序问题，修法在写入侧（候选方案两条，见 §3.3）：
    //   ① `UNIQUE(phone, created_at)` + 冲突映射为 409 限频 —— 让同毫秒并列在**物理上不可能**，
    //      于是 `created_at DESC` 天然成为全序。代价：迁移在存量重复数据上会失败，
    //      且把一次竞态从"静默"变成"写入报错"，在 auth 关键路径上需评审。
    //   ② 给表加真正的单调序列列 —— 与 `world_events.sequence` 同一道题（`MAX+1` 读-改-写在
    //      PG READ COMMITTED 下不串行），代价与风险同 §3.3「相邻风险」那条。
    // 两条都是写入侧改造 + 迁移，且整条短信通道当前是 Dev 桩（`DevSms`，按 §0.3 本就不可上线），
    // 故不在"补排序键"这一批次内动它。
    let challenge: Option<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, code_hash, expires_at, consumed FROM sms_challenges WHERE phone = $1 ORDER BY created_at DESC LIMIT 1",
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
    sqlx::query("UPDATE sms_challenges SET consumed = 1 WHERE id = $1")
        .bind(&chal_id)
        .execute(&state.db)
        .await?;

    // upsert user（服务端权威：手机号唯一，不重复建号）
    let existing: Option<(String, String, i64, String)> =
        sqlx::query_as("SELECT id, nickname, age_declared, status FROM users WHERE phone = $1")
            .bind(&phone)
            .fetch_optional(&state.db)
            .await?;
    let user = if let Some((id, nickname, age, status)) = existing {
        if status == "banned" {
            return Err(ApiError::Forbidden);
        }
        sqlx::query("UPDATE users SET updated_at = $1 WHERE id = $2")
            .bind(now)
            .bind(&id)
            .execute(&state.db)
            .await?;
        UserView { id, phone: Some(phone.clone()), nickname, age_declared: age, status }
    } else {
        let id = new_id("user");
        sqlx::query(
            "INSERT INTO users (id, phone, nickname, age_declared, status, created_at, updated_at) VALUES ($1, $2, '', 0, 'active', $3, $4)",
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
        sqlx::query_as("SELECT id, user_id, expires_at, revoked FROM refresh_tokens WHERE token_hash = $1")
            .bind(&token_hash)
            .fetch_optional(&state.db)
            .await?;
    let (rt_id, user_id, expires_at, revoked) = row.ok_or(ApiError::Unauthorized)?;
    if revoked != 0 || expires_at < now {
        return Err(ApiError::Unauthorized);
    }
    sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE id = $1")
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
    sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE user_id = $1 AND revoked = 0")
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
        "INSERT INTO identity_verification_refs (id, user_id, provider, reference_id, status, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
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

/// POST /auth/age-declaration：用户自我年龄声明 → 写 users.age_declared（成年=1 / 未成年=2）。
/// 支撑 §2.2 未成年人默认保护：billing 充值仅放行"已声明成年"（==1）；未声明(0)/未成年(2)一律保守拒充。
/// 自我声明 ≠ 实名认证；真正的实名/防沉迷走 /identity/verification（仅存第三方状态）。
async fn age_declaration(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<AgeDeclarationReq>,
) -> Result<Response, ApiError> {
    // 成年声明=1，未成年声明=2（与 users.age_declared 语义一致；0 保留给"未声明"）。
    let age_value: i64 = if req.is_adult { 1 } else { 2 };
    let payload_hash = idempotency::hash_payload(&serde_json::to_vec(&req).unwrap_or_default());
    let key = idem_key(&headers);
    let guard =
        idempotency::guard(&state.db, &user.user_id, "POST /auth/age-declaration", key.as_deref(), &payload_hash).await?;
    if let Some(cached) = guard.cached_response {
        return Ok(json_response(cached));
    }
    let affected = sqlx::query("UPDATE users SET age_declared = $1, updated_at = $2 WHERE id = $3")
        .bind(age_value)
        .bind(now_ms())
        .bind(&user.user_id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound); // token 有效但用户行不存在（已注销）
    }
    let resp = serde_json::json!({ "ageDeclared": age_value });
    let body = serde_json::to_string(&resp).map_err(ApiError::internal)?;
    guard.store_response(&state.db, &body).await?;
    Ok(json_response(body))
}

// ═══════════════════════════════════════════════════════════════════════════
// 🔴 未成年人保护：全仓**唯一**的「已声明成年」判定（真红线 §0.4）
// ═══════════════════════════════════════════════════════════════════════════

/// `users.age_declared` 的「已声明成年」取值。**全仓唯一定义**。
///
/// 其余取值一律**不是**成年：`0` = 未声明、`2` = 未成年、将来若新增取值也默认不放行。
pub const AGE_DECLARED_ADULT: i64 = 1;

/// 这个用户**是否已声明成年**。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 为什么必须只有一份
/// ════════════════════════════════════════════════════════════════════════════
///
/// 这条判定此前**在 5 个模块里各写了一遍**（`worlds` 生死状入场门、`invitations`
/// 未成年不收生死状邀请、`social` 青少年模式限真人社交、`livestage` 弹幕成年门、
/// `ledger` 未成年创作者不分账）。五处**行为恰好一致**，但那是巧合维持的：
/// 三处硬编码字面量 `1`，两处各自定义了一份 `AGE_DECLARED_ADULT` 常量。
///
/// 它是**真红线 §0.4**。一条红线靠五份手抄保持一致，只要有一处被改成
/// `!= 2`（看起来更"直觉"：不是未成年就放行），**未声明年龄的账号立刻全部变成成年**——
/// 而那一处的用例只会验它自己那个模块，全仓测试照样绿。
///
/// ════════════════════════════════════════════════════════════════════════════
/// 🔴 fail-closed：年龄未知一律按未成年处理
/// ════════════════════════════════════════════════════════════════════════════
///
/// 查不到用户行、查库失败、取值不是 `AGE_DECLARED_ADULT` —— 一律返回 `false`。
/// **无法可靠判断年龄之前绝不放行**，这与 billing 的保守拒充是同一条原则。
///
/// ⚠️ 本函数只回**判据**，不决定后果：`worlds`/`social`/`livestage` 用它拒绝（403），
/// 而 `ledger` 用它把创作者分成全额留在平台（不是拒绝交易）。后果各归各的调用点，
/// 判据只能有一个。
///
/// 泛型 `Executor` 而不是「池版 + 事务版」两份：`ledger` 在事务里判、其余在池上判，
/// 写成两份就又是一处「靠注释保持同步」的重复判据（`memorial` 的死亡判据正是那样，
/// 且已因此被故障注入抓到过）。
pub async fn is_declared_adult<'e, E>(exec: E, user_id: &str) -> bool
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let row: Result<Option<(i64,)>, _> =
        sqlx::query_as("SELECT age_declared FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(exec)
            .await;
    // 🔴 查库失败也走这一支：`Err` → 不 matches → false。年龄未知按未成年处理。
    matches!(row, Ok(Some((AGE_DECLARED_ADULT,))))
}

#[cfg(test)]
pub(crate) mod tests;
