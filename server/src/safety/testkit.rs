//! 测试脚手架（仅 test 构建）：空库 + 播种助手，供 S3 各模块集成测试复用。
//! 建池统一走 `crate::testkit`（默认 `sqlite::memory:`，`MUSE_TEST_DATABASE_URL` 可切 PG）。

use sqlx::AnyPool;

use crate::app::AppState;
use crate::config::ServerConfig;

pub async fn test_state() -> AppState {
    let pool = crate::testkit::test_pool().await;
    let cfg = ServerConfig::from_env();
    AppState::new(pool, cfg)
}

pub fn token(state: &AppState, user_id: &str) -> String {
    crate::auth::issue_access(&state.config.jwt_secret, user_id, "user", 3600).expect("issue token")
}

pub async fn seed_user(db: &AnyPool, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, nickname, age_declared, status, created_at, updated_at) VALUES ($1, '', 1, 'active', $2, $3)",
    )
    .bind(id)
    .bind(crate::db::now_ms())
    .bind(crate::db::now_ms())
    .execute(db)
    .await
    .expect("seed user");
}

#[allow(clippy::too_many_arguments)]
pub async fn seed_world(db: &AnyPool, id: &str, revision: i64, status: &str) {
    sqlx::query(
        "INSERT INTO worlds (id, template_id, template_version, engine_version, prompt_set_version, \
         model_route_version, room_type, title, status, visibility, member_limit, tick_per_day, \
         state_revision, narrative_state_json, created_at, updated_at) \
         VALUES ($1, 'tpl', 1, 'e1', 'p1', 'm1', 'idle', '测试世界', $2, 'official', 10, 3, $3, '{}', $4, $5)",
    )
    .bind(id)
    .bind(status)
    .bind(revision)
    .bind(crate::db::now_ms())
    .bind(crate::db::now_ms())
    .execute(db)
    .await
    .expect("seed world");
}

pub async fn seed_member(db: &AnyPool, id: &str, world_id: &str, user_id: &str, char_id: &str, status: &str) {
    sqlx::query(
        "INSERT INTO world_members (id, world_id, user_id, cloud_character_id, boundary_json, status, joined_at) \
         VALUES ($1, $2, $3, $4, '{}', $5, $6)",
    )
    .bind(id)
    .bind(world_id)
    .bind(user_id)
    .bind(char_id)
    .bind(status)
    .bind(crate::db::now_ms())
    .execute(db)
    .await
    .expect("seed member");
}

pub async fn seed_backpack(
    db: &AnyPool,
    id: &str,
    user_id: &str,
    item_id: &str,
    status: &str,
    carried_world_id: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO backpacks (id, user_id, item_id, acquired_world_id, status, carried_world_id, acquired_at) \
         VALUES ($1, $2, $3, 'w0', $4, $5, $6)",
    )
    .bind(id)
    .bind(user_id)
    .bind(item_id)
    .bind(status)
    .bind(carried_world_id)
    .bind(crate::db::now_ms())
    .execute(db)
    .await
    .expect("seed backpack");
}

pub async fn count(db: &AnyPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(db).await.expect("count query")
}
